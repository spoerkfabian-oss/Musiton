//! Loading a VST3 module (shared library) and reading its class factory.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use vst3::ComPtr;
use vst3::Steinberg::{
    IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactoryTrait, PClassInfo,
    PClassInfo2, PFactoryInfo, TUID,
};

use crate::{c_text, PluginError, PluginFormat, PluginInfo};

/// Category of the classes that are audio processors (instruments and effects).
const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// The binary inside a `.vst3` bundle for this system, or the path itself when it is a
/// single file (old Windows plugins).
pub(crate) fn binary_path(bundle: &Path) -> Result<PathBuf, PluginError> {
    if bundle.is_file() {
        return Ok(bundle.to_owned());
    }
    let stem = bundle
        .file_stem()
        .ok_or_else(|| PluginError::NoBinary(bundle.to_owned()))?;
    let (arch, ext) = if cfg!(windows) {
        ("x86_64-win", "vst3")
    } else {
        ("x86_64-linux", "so")
    };
    let mut name = stem.to_owned();
    name.push(".");
    name.push(ext);
    let path = bundle.join("Contents").join(arch).join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(PluginError::NoBinary(bundle.to_owned()))
    }
}

/// A loaded VST3 module. Unloads (after calling its exit function) when dropped; every
/// object created from its factory must be released before that.
pub(crate) struct Module {
    factory: Option<ComPtr<IPluginFactory>>,
    library: Option<libloading::Library>,
}

// SAFETY: a VST3 factory may be used from any thread; Musiton creates instances only on
// the UI thread, and the library handle is only used to unload.
unsafe impl Send for Module {}

impl Module {
    /// The module of `bundle`, loaded once per process: every user shares it, and it
    /// exits and unloads when the last one lets go (loading or unloading one module
    /// twice at the same time crashes some plugins).
    pub(crate) fn shared(bundle: &Path) -> Result<Arc<Self>, PluginError> {
        static LOADED: Mutex<Vec<(PathBuf, Weak<Module>)>> = Mutex::new(Vec::new());
        let mut loaded = LOADED
            .lock()
            .map_err(|_| PluginError::Failed("module registry".into()))?;
        loaded.retain(|(_, m)| m.strong_count() > 0);
        if let Some(m) = loaded
            .iter()
            .find(|(p, _)| p == bundle)
            .and_then(|(_, m)| m.upgrade())
        {
            return Ok(m);
        }
        let m = Arc::new(Self::load(bundle)?);
        loaded.push((bundle.to_owned(), Arc::downgrade(&m)));
        Ok(m)
    }

    /// Loads the module of `bundle` and calls its entry function.
    fn load(bundle: &Path) -> Result<Self, PluginError> {
        let binary = binary_path(bundle)?;
        let fail = |reason: String| PluginError::Load {
            path: binary.clone(),
            reason,
        };
        // SAFETY: loading a plugin runs its initializers; the plugin is trusted to the
        // extent that the user installed it (and scanning happens in a separate process).
        let library =
            unsafe { libloading::Library::new(&binary) }.map_err(|e| fail(e.to_string()))?;
        Self::enter(&library).map_err(fail)?;
        type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
        // SAFETY: `GetPluginFactory` has this signature in every VST3 module.
        let get: libloading::Symbol<GetFactory> =
            unsafe { library.get(b"GetPluginFactory\0") }.map_err(|e| fail(e.to_string()))?;
        // SAFETY: returns an owned reference (or null) to the module's factory.
        let raw = unsafe { get() };
        // SAFETY: the pointer is a valid IPluginFactory or null; we take over its reference.
        let factory = unsafe { ComPtr::from_raw(raw) }
            .ok_or_else(|| fail("GetPluginFactory returned nothing".into()))?;
        Ok(Self {
            factory: Some(factory),
            library: Some(library),
        })
    }

    #[cfg(windows)]
    fn enter(library: &libloading::Library) -> Result<(), String> {
        type Init = unsafe extern "system" fn() -> bool;
        // SAFETY: `InitDll` is optional; when present it takes no arguments.
        if let Ok(init) = unsafe { library.get::<Init>(b"InitDll\0") } {
            // SAFETY: called once, right after loading.
            if !unsafe { init() } {
                return Err("InitDll failed".into());
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn enter(library: &libloading::Library) -> Result<(), String> {
        type Entry = unsafe extern "system" fn(*mut std::ffi::c_void) -> bool;
        // SAFETY: Linux VST3 modules export `ModuleEntry(void* handle)`.
        let entry = unsafe { library.get::<Entry>(b"ModuleEntry\0") }.map_err(|e| e.to_string())?;
        // SAFETY: called once, right after loading. Plugins only use the handle to find
        // their own files; null is accepted by the SDK's implementation.
        if unsafe { entry(std::ptr::null_mut()) } {
            Ok(())
        } else {
            Err("ModuleEntry failed".into())
        }
    }

    /// The module's class factory.
    pub(crate) fn factory(&self) -> Option<&ComPtr<IPluginFactory>> {
        self.factory.as_ref()
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // Everything created from the module is released before it exits and unloads.
        self.factory = None;
        let Some(library) = self.library.take() else {
            return;
        };
        let exit_name: &[u8] = if cfg!(windows) {
            b"ExitDll\0"
        } else {
            b"ModuleExit\0"
        };
        type Exit = unsafe extern "system" fn() -> bool;
        // SAFETY: the exit function takes no arguments and is called once, last.
        if let Ok(exit) = unsafe { library.get::<Exit>(exit_name) } {
            // SAFETY: see above.
            unsafe {
                exit();
            }
        }
        drop(library);
    }
}

/// 32 hex digits of a class id.
pub(crate) fn tuid_hex(id: &TUID) -> String {
    id.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

/// Lists the audio processor classes of a `.vst3` bundle.
pub(crate) fn scan(bundle: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    let module = Module::shared(bundle)?;
    let factory = module
        .factory()
        .ok_or_else(|| PluginError::Failed("no factory".into()))?;
    let mut factory_info: PFactoryInfo = zeroed_info();
    // SAFETY: valid out pointer to a plain struct.
    let factory_vendor = if unsafe { factory.getFactoryInfo(&mut factory_info) } == 0 {
        c_text(&factory_info.vendor)
    } else {
        String::new()
    };
    let factory2 = factory.cast::<IPluginFactory2>();
    // SAFETY: plain call.
    let count = unsafe { factory.countClasses() }.max(0);
    let mut out = Vec::new();
    for i in 0..count {
        let info = match &factory2 {
            Some(f2) => {
                let mut info: PClassInfo2 = zeroed_info();
                // SAFETY: valid index and out pointer.
                if unsafe { f2.getClassInfo2(i, &mut info) } != 0 {
                    continue;
                }
                (
                    info.cid,
                    c_text(&info.category),
                    c_text(&info.name),
                    c_text(&info.vendor),
                    c_text(&info.subCategories),
                    c_text(&info.version),
                )
            }
            None => {
                let mut info: PClassInfo = zeroed_info();
                // SAFETY: valid index and out pointer.
                if unsafe { factory.getClassInfo(i, &mut info) } != 0 {
                    continue;
                }
                (
                    info.cid,
                    c_text(&info.category),
                    c_text(&info.name),
                    String::new(),
                    String::new(),
                    String::new(),
                )
            }
        };
        let (cid, category, name, vendor, sub, version) = info;
        if category != AUDIO_MODULE_CLASS {
            continue;
        }
        out.push(PluginInfo {
            format: PluginFormat::Vst3,
            path: bundle.to_owned(),
            id: tuid_hex(&cid),
            name,
            vendor: if vendor.is_empty() {
                factory_vendor.clone()
            } else {
                vendor
            },
            instrument: sub.split('|').any(|s| s == "Instrument"),
            category: sub,
            version,
        });
    }
    // Interfaces the factory gave out are released before the module unloads.
    drop(factory2);
    Ok(out)
}

/// A zero-initialised plain C struct.
fn zeroed_info<T: Copy>() -> T {
    // SAFETY: only used for the VST3 info structs, which are plain data (integers and
    // char arrays) for which all-zero is valid.
    unsafe { std::mem::zeroed() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_are_written_as_hex() {
        let mut id: TUID = [0; 16];
        for (i, b) in id.iter_mut().enumerate() {
            *b = (i as i8).wrapping_mul(37);
        }
        let hex = tuid_hex(&id);
        assert_eq!(hex.len(), 32);
        assert_eq!(&hex[..4], "0025");
    }
}
