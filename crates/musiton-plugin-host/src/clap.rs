//! CLAP plugins (through `clack-host`).

use std::any::Any;
use std::cell::RefCell;
use std::ffi::CString;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window};
use clack_extensions::latency::PluginLatency;
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::state::PluginState;
use clack_host::entry::PluginEntry;
use clack_host::events::event_types::{
    MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::events::{EventFlags, EventHeader, Match, Pckn, UnknownEvent};
use clack_host::prelude::*;
use clack_host::process::PluginAudioProcessor;
use clack_host::utils::{BeatTime, ClapId, SecondsTime};
use musiton_core::PPQ;
use musiton_engine::node::{AudioIo, Event, EventKind, ProcessContext};

use crate::plugin::{Controller, Edit, ParamChange, Plugin, PluginParam, Processor};
use crate::{PluginError, PluginFormat, PluginInfo};

/// Loads the CLAP file at `path`.
pub(crate) fn load(path: &Path) -> Result<PluginEntry, PluginError> {
    if !path.is_file() {
        return Err(PluginError::NoBinary(path.to_owned()));
    }
    // SAFETY: loading runs the plugin's initializers; the user installed the plugin, and
    // scanning happens in a separate process.
    unsafe { PluginEntry::load(path) }.map_err(|e| PluginError::Load {
        path: path.to_owned(),
        reason: e.to_string(),
    })
}

/// Lists the plugins of a `.clap` file.
pub(crate) fn scan(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    let entry = load(path)?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| PluginError::Failed("no plugin factory".into()))?;
    let text = |s: Option<&std::ffi::CStr>| s.map(|s| s.to_string_lossy().into_owned());
    Ok(factory
        .plugin_descriptors()
        .filter_map(|d| {
            let features: Vec<String> = d
                .features()
                .map(|f| f.to_string_lossy().into_owned())
                .collect();
            Some(PluginInfo {
                format: PluginFormat::Clap,
                path: path.to_owned(),
                id: text(d.id())?,
                name: text(d.name()).unwrap_or_default(),
                vendor: text(d.vendor()).unwrap_or_default(),
                instrument: features.iter().any(|f| f == "instrument"),
                category: features.join(" "),
                version: text(d.version()).unwrap_or_default(),
            })
        })
        .collect())
}

/// Host callbacks shared by all threads: requests are remembered and handled on the UI
/// thread.
pub(crate) struct ClapShared {
    restart: AtomicBool,
    callback: AtomicBool,
}

impl<'a> SharedHandler<'a> for ClapShared {
    fn request_restart(&self) {
        self.restart.store(true, Ordering::Relaxed);
    }
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.callback.store(true, Ordering::Relaxed);
    }
}

/// Musiton as a CLAP host.
pub(crate) struct ClapHost;

impl HostHandlers for ClapHost {
    type Shared<'a> = ClapShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

/// Parameter ranges (CLAP values are plain; Musiton works with 0..1).
#[derive(Clone, Copy)]
struct Range {
    id: u32,
    min: f64,
    max: f64,
}

impl Range {
    fn plain(&self, normalized: f64) -> f64 {
        self.min + normalized.clamp(0.0, 1.0) * (self.max - self.min)
    }
    fn normalized(&self, plain: f64) -> f64 {
        if self.max > self.min {
            ((plain - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Events a block can carry to the plugin.
const MAX_EVENTS: usize = 512;

/// Loads plugin `id` from the CLAP file at `path`.
pub(crate) fn instantiate(
    path: &Path,
    id: &str,
    sample_rate: f64,
    max_block: usize,
) -> Result<Plugin, PluginError> {
    let entry = load(path)?;
    let failed = |what: &str| PluginError::Failed(what.into());
    let info = HostInfo::new(
        "Musiton",
        "Musiton",
        "https://github.com/spoerkfabian-oss/Musiton",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|_| failed("host info"))?;
    let cid = CString::new(id).map_err(|_| failed("bad plugin id"))?;
    let mut instance = PluginInstance::<ClapHost>::new(
        |_| ClapShared {
            restart: AtomicBool::new(false),
            callback: AtomicBool::new(false),
        },
        |_| (),
        &entry,
        &cid,
        &info,
    )
    .map_err(|e| failed(&e.to_string()))?;

    // Ports: Musiton feeds and takes stereo on the main ports.
    let (ins, outs) = {
        let handle = instance.plugin_handle();
        match handle.get_extension::<PluginAudioPorts>() {
            Some(ports) => {
                let mut buf = AudioPortInfoBuffer::new();
                let channels = |is_input: bool, buf: &mut AudioPortInfoBuffer| -> Vec<usize> {
                    (0..ports.count(&handle, is_input))
                        .map(|i| {
                            ports
                                .get(&handle, i, is_input, buf)
                                .map_or(2, |p| p.channel_count as usize)
                        })
                        .collect()
                };
                (channels(true, &mut buf), channels(false, &mut buf))
            }
            None => (Vec::new(), vec![2]),
        }
    };
    let ranges = read_ranges(&mut instance);
    let params = read_params(&mut instance);
    let processor = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate,
                min_frames_count: 1,
                max_frames_count: max_block as u32,
            },
        )
        .map_err(|e| failed(&e.to_string()))?;
    let mut sorted = ranges.clone();
    sorted.sort_by_key(|r| r.id);
    let processor = ClapProcessor {
        processor: processor.into(),
        in_bufs: ins.iter().map(|&n| vec![vec![0.0; max_block]; n]).collect(),
        out_bufs: outs
            .iter()
            .map(|&n| vec![vec![0.0; max_block]; n])
            .collect(),
        in_ports: AudioPorts::with_capacity(ins.iter().sum(), ins.len()),
        out_ports: AudioPorts::with_capacity(outs.iter().sum(), outs.len()),
        events: EventBuffer::with_capacity(MAX_EVENTS),
        out_events: EventBuffer::with_capacity(MAX_EVENTS),
        ranges: sorted,
        sample_rate,
        steady: 0,
    };
    let inputs = if ins.is_empty() {
        0
    } else {
        2 + 2 * usize::from(ins.len() > 1)
    };
    let controller = ClapController {
        instance: RefCell::new(instance),
        params,
        ranges,
        editor_size: std::cell::Cell::new(None),
        _entry: entry,
    };
    Ok(Plugin::new(
        PluginFormat::Clap,
        Box::new(processor),
        Box::new(controller),
        (inputs, 2),
    ))
}

fn read_ranges(instance: &mut PluginInstance<ClapHost>) -> Vec<Range> {
    let handle = instance.plugin_handle();
    let Some(ext) = handle.get_extension::<PluginParams>() else {
        return Vec::new();
    };
    let mut buf = ParamInfoBuffer::new();
    (0..ext.count(&handle))
        .filter_map(|i| {
            let p = ext.get_info(&handle, i, &mut buf)?;
            Some(Range {
                id: p.id.get(),
                min: p.min_value,
                max: p.max_value,
            })
        })
        .collect()
}

fn read_params(instance: &mut PluginInstance<ClapHost>) -> Vec<PluginParam> {
    let handle = instance.plugin_handle();
    let Some(ext) = handle.get_extension::<PluginParams>() else {
        return Vec::new();
    };
    let mut buf = ParamInfoBuffer::new();
    (0..ext.count(&handle))
        .filter_map(|i| {
            let p = ext.get_info(&handle, i, &mut buf)?;
            if p.flags.contains(ParamInfoFlags::IS_HIDDEN) {
                return None;
            }
            let range = Range {
                id: p.id.get(),
                min: p.min_value,
                max: p.max_value,
            };
            Some(PluginParam {
                id: p.id.get(),
                name: String::from_utf8_lossy(p.name)
                    .trim_end_matches('\0')
                    .to_owned(),
                unit: String::new(),
                default: range.normalized(p.default_value),
                steps: if p.flags.contains(ParamInfoFlags::IS_STEPPED) {
                    (p.max_value - p.min_value).round() as i32
                } else {
                    0
                },
                automatable: p.flags.contains(ParamInfoFlags::IS_AUTOMATABLE)
                    && !p.flags.contains(ParamInfoFlags::IS_READONLY),
            })
        })
        .collect()
}

/// The audio-thread half.
struct ClapProcessor {
    processor: PluginAudioProcessor<ClapHost>,
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    events: EventBuffer,
    out_events: EventBuffer,
    /// Sorted by id.
    ranges: Vec<Range>,
    sample_rate: f64,
    steady: u64,
}

impl ClapProcessor {
    fn push<E: AsRef<UnknownEvent>>(&mut self, e: &E) {
        // Stay well below the reserved size, so pushing never reallocates.
        if (self.events.len() as usize) < MAX_EVENTS / 2 {
            self.events.push(e);
        }
    }
}

impl Processor for ClapProcessor {
    fn process(
        &mut self,
        ctx: &ProcessContext,
        io: &mut AudioIo,
        events: &[Event],
        params: &[ParamChange],
    ) {
        let frames = ctx.frames.min(
            self.out_bufs
                .first()
                .and_then(|b| b.first())
                .map_or(0, Vec::len),
        );
        self.events.clear();
        self.out_events.clear();
        for &(id, offset, v) in params {
            if let Ok(i) = self.ranges.binary_search_by_key(&id, |r| r.id) {
                let plain = self.ranges[i].plain(v);
                if let Some(cid) = ClapId::from_raw(id) {
                    self.push(&ParamValueEvent::new(
                        offset.max(0) as u32,
                        cid,
                        Pckn::match_all(),
                        plain,
                    ));
                }
            }
        }
        for e in events {
            let t = e.offset;
            match e.kind {
                EventKind::NoteOn { ch, key, vel } => {
                    self.push(&NoteOnEvent::new(
                        t,
                        Pckn::new(0u16, u16::from(ch), u16::from(key), Match::All),
                        f64::from(vel),
                    ));
                    self.push(&MidiEvent::new(
                        t,
                        0,
                        [
                            0x90 | (ch & 15),
                            key & 127,
                            ((vel * 127.0).round() as u8).clamp(1, 127),
                        ],
                    ));
                }
                EventKind::NoteOff { ch, key } => {
                    self.push(&NoteOffEvent::new(
                        t,
                        Pckn::new(0u16, u16::from(ch), u16::from(key), Match::All),
                        0.0,
                    ));
                    self.push(&MidiEvent::new(t, 0, [0x80 | (ch & 15), key & 127, 0]));
                }
                EventKind::Cc { ch, cc, val } if cc == musiton_engine::node::PITCH_BEND => {
                    let v = (val.clamp(0.0, 1.0) * 16_383.0).round() as u16;
                    self.push(&MidiEvent::new(
                        t,
                        0,
                        [0xE0 | (ch & 15), (v & 127) as u8, (v >> 7) as u8],
                    ));
                }
                EventKind::Cc { ch, cc, val } => {
                    self.push(&MidiEvent::new(
                        t,
                        0,
                        [0xB0 | (ch & 15), cc & 127, (val * 127.0).round() as u8],
                    ));
                }
                EventKind::AllNotesOff => {
                    for ch in 0..16u8 {
                        self.push(&MidiEvent::new(t, 0, [0xB0 | ch, 123, 0]));
                    }
                }
                EventKind::Param { .. } => {}
            }
        }
        self.events.sort();

        for (b, bus) in self.in_bufs.iter_mut().enumerate() {
            for (c, buf) in bus.iter_mut().enumerate() {
                let src = io.input(b * 2 + c.min(1));
                let dst = &mut buf[..frames];
                if src.len() >= frames {
                    dst.copy_from_slice(&src[..frames]);
                } else {
                    dst.fill(0.0);
                }
            }
        }

        let tick = ctx
            .tempo
            .sample_to_tick(ctx.sample_pos as f64, self.sample_rate);
        let mut flags = TransportFlags::HAS_TEMPO
            | TransportFlags::HAS_BEATS_TIMELINE
            | TransportFlags::HAS_SECONDS_TIMELINE
            | TransportFlags::HAS_TIME_SIGNATURE;
        if ctx.playing {
            flags |= TransportFlags::IS_PLAYING;
        }
        let beats = tick / PPQ as f64;
        let per_bar = f64::from(ctx.time_sig.num) * 4.0 / f64::from(ctx.time_sig.den.max(1));
        let bar = (beats / per_bar).floor();
        let transport = TransportEvent {
            header: EventHeader::new_core(0, EventFlags::empty()),
            flags,
            song_pos_beats: BeatTime::from_float(beats),
            song_pos_seconds: SecondsTime::from_float(ctx.sample_pos as f64 / self.sample_rate),
            tempo: ctx.tempo.bpm_at(tick),
            tempo_inc: 0.0,
            loop_start_beats: BeatTime::from_int(0),
            loop_end_beats: BeatTime::from_int(0),
            loop_start_seconds: SecondsTime::from_int(0),
            loop_end_seconds: SecondsTime::from_int(0),
            bar_start: BeatTime::from_float(bar * per_bar),
            bar_number: bar as i32,
            time_signature_numerator: u16::from(ctx.time_sig.num),
            time_signature_denominator: u16::from(ctx.time_sig.den),
        };

        let Ok(started) = self.processor.ensure_processing_started() else {
            for ch in io.outputs() {
                ch.fill(0.0);
            }
            return;
        };
        let input_audio = self
            .in_ports
            .with_input_buffers(self.in_bufs.iter_mut().map(|bus| {
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        bus.iter_mut()
                            .map(|c| InputChannel::variable(&mut c[..frames])),
                    ),
                }
            }));
        let mut output_audio = self
            .out_ports
            .with_output_buffers(self.out_bufs.iter_mut().map(|bus| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    bus.iter_mut().map(|c| &mut c[..frames]),
                ),
            }));
        let ok = started
            .process(
                &input_audio,
                &mut output_audio,
                &InputEvents::from_buffer(&self.events),
                &mut OutputEvents::from_buffer(&mut self.out_events),
                Some(self.steady),
                Some(&transport),
            )
            .is_ok();
        self.steady += frames as u64;
        let main = self.out_bufs.first();
        for ch in 0..io.num_outputs() {
            let out = io.output(ch);
            let n = out.len().min(frames);
            match main.and_then(|bus| bus.get(ch.min(bus.len().saturating_sub(1)))) {
                Some(src) if ok => out[..n].copy_from_slice(&src[..n]),
                _ => out.fill(0.0),
            }
        }
    }

    fn reset(&mut self) {
        if let PluginAudioProcessor::Started(p) = &mut self.processor {
            p.reset();
        }
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// The UI-thread half.
struct ClapController {
    instance: RefCell<PluginInstance<ClapHost>>,
    params: Vec<PluginParam>,
    ranges: Vec<Range>,
    /// Size of the open editor (`None` while closed).
    editor_size: std::cell::Cell<Option<(i32, i32)>>,
    /// Keeps the library loaded while the instance lives.
    _entry: PluginEntry,
}

impl ClapController {
    fn range(&self, id: u32) -> Option<Range> {
        self.ranges.iter().find(|r| r.id == id).copied()
    }
}

impl Controller for ClapController {
    fn params(&self) -> &[PluginParam] {
        &self.params
    }

    fn param_value(&self, id: u32) -> f64 {
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let (Some(ext), Some(cid), Some(range)) = (
            handle.get_extension::<PluginParams>(),
            ClapId::from_raw(id),
            self.range(id),
        ) else {
            return 0.0;
        };
        ext.get_value(&handle, cid)
            .map_or(0.0, |v| range.normalized(v))
    }

    fn set_param(&self, _id: u32, _value: f64) {
        // CLAP plugins learn about changes through the events of the next block.
    }

    fn param_text(&self, id: u32, value: f64) -> String {
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let (Some(ext), Some(cid), Some(range)) = (
            handle.get_extension::<PluginParams>(),
            ClapId::from_raw(id),
            self.range(id),
        ) else {
            return format!("{value:.2}");
        };
        let mut buf = [0u8; 128];
        match ext.value_to_text(&handle, cid, range.plain(value), &mut buf) {
            Ok(text) => {
                let end = text.iter().position(|b| *b == 0).unwrap_or(text.len());
                String::from_utf8_lossy(&text[..end]).into_owned()
            }
            Err(_) => format!("{:.2}", range.plain(value)),
        }
    }

    fn take_edits(&self) -> Vec<Edit> {
        let mut instance = self.instance.borrow_mut();
        let (restart, callback) = instance.access_shared_handler(|s: &ClapShared| {
            (
                s.restart.swap(false, Ordering::Relaxed),
                s.callback.swap(false, Ordering::Relaxed),
            )
        });
        // The plugin asked to be called on the main thread (this is the UI thread).
        if callback {
            instance.call_on_main_thread_callback();
        }
        if restart {
            vec![Edit::Restart]
        } else {
            Vec::new()
        }
    }

    fn state(&self) -> Result<Vec<u8>, PluginError> {
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let ext = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| PluginError::Failed("the plugin cannot save its state".into()))?;
        let mut out = Vec::new();
        ext.save(&handle, &mut out)
            .map_err(|_| PluginError::Failed("the plugin did not save its state".into()))?;
        Ok(out)
    }

    fn set_state(&self, state: &[u8]) -> Result<(), PluginError> {
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let ext = handle
            .get_extension::<PluginState>()
            .ok_or_else(|| PluginError::Failed("the plugin cannot load a state".into()))?;
        let mut reader = state;
        ext.load(&handle, &mut reader)
            .map_err(|_| PluginError::Failed("the plugin did not accept its saved state".into()))
    }

    fn latency(&self) -> u32 {
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        handle
            .get_extension::<PluginLatency>()
            .map_or(0, |l| l.get(&handle))
    }

    fn has_editor(&self) -> bool {
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        handle.get_extension::<PluginGui>().is_some_and(|g| {
            g.is_api_supported(
                &handle,
                GuiConfiguration {
                    api_type: GuiApiType::WIN32,
                    is_floating: false,
                },
            )
        })
    }

    fn attach_editor(
        &self,
        parent: crate::window::Handle,
    ) -> Result<((i32, i32), bool), PluginError> {
        self.detach_editor();
        let failed = |what: &str| PluginError::Failed(what.into());
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let gui = handle
            .get_extension::<PluginGui>()
            .ok_or_else(|| failed("the plugin has no editor"))?;
        gui.create(
            &handle,
            GuiConfiguration {
                api_type: GuiApiType::WIN32,
                is_floating: false,
            },
        )
        .map_err(|_| failed("the editor could not be created"))?;
        // SAFETY: `parent` is the live native window that holds the editor until
        // `detach_editor` is called.
        let attached = unsafe { gui.set_parent(&handle, Window::from_win32_hwnd(parent)) };
        if attached.is_err() {
            gui.destroy(&handle);
            return Err(failed("the editor could not be opened"));
        }
        let size = gui
            .get_size(&handle)
            .map_or((640, 480), |s| (s.width as i32, s.height as i32));
        let resizable = gui.can_resize(&handle);
        let _ = gui.show(&handle);
        self.editor_size.set(Some(size));
        Ok((size, resizable))
    }

    fn detach_editor(&self) {
        if self.editor_size.take().is_none() {
            return;
        }
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        if let Some(gui) = handle.get_extension::<PluginGui>() {
            gui.destroy(&handle);
        }
    }

    fn editor_size_request(&self) -> Option<(i32, i32)> {
        let known = self.editor_size.get()?;
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        let gui = handle.get_extension::<PluginGui>()?;
        let now = gui
            .get_size(&handle)
            .map(|s| (s.width as i32, s.height as i32))?;
        (now != known).then(|| {
            self.editor_size.set(Some(now));
            now
        })
    }

    fn editor_resized(&self, width: i32, height: i32) {
        if self.editor_size.get().is_none() {
            return;
        }
        let mut instance = self.instance.borrow_mut();
        let handle = instance.plugin_handle();
        if let Some(gui) = handle.get_extension::<PluginGui>() {
            if gui.can_resize(&handle) {
                let size = GuiSize {
                    width: width.max(1) as u32,
                    height: height.max(1) as u32,
                };
                if gui.set_size(&handle, size).is_ok() {
                    self.editor_size.set(Some((width, height)));
                }
            }
        }
    }

    fn shutdown(&self, processor: Box<dyn Any>) {
        if let Ok(p) = processor.downcast::<ClapProcessor>() {
            let p = *p;
            let mut instance = self.instance.borrow_mut();
            let stopped = match p.processor {
                PluginAudioProcessor::Started(s) => s.stop_processing(),
                PluginAudioProcessor::Stopped(s) => s,
            };
            instance.deactivate(stopped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn find(name: &str) -> Option<(std::path::PathBuf, String)> {
        let path = std::path::PathBuf::from(format!("/usr/lib/clap/{name}.clap"));
        let id = scan(&path).ok()?.first()?.id.clone();
        Some((path, id))
    }

    fn render(plugin: &Plugin, input: f32, events: &[Event], blocks: usize) -> f32 {
        let mut node = plugin.node();
        let tempo = TempoMap::constant(120.0);
        let ins = vec![vec![input; 256]; node.num_inputs()];
        let mut outs = vec![vec![0.0; 256]; node.num_outputs()];
        let mut peak = 0.0_f32;
        for b in 0..blocks {
            let ctx = ProcessContext {
                sample_rate: 48_000.0,
                frames: 256,
                playing: true,
                sample_pos: (b * 256) as u64,
                tempo: &tempo,
                time_sig: TimeSig::default(),
                metronome: false,
                inputs: &[],
                recording: false,
                punch: None,
                live_midi: &[],
            };
            let ev = if b == 0 { events } else { &[] };
            node.process(&ctx, &mut AudioIo::new(&ins, &mut outs, 256), ev);
            peak = outs[0].iter().fold(peak, |m, s| m.max(s.abs()));
        }
        peak
    }

    #[test]
    fn a_clap_synth_plays_and_keeps_its_state() {
        let Some((path, id)) = find("Kars") else {
            eprintln!("test plugin not installed; skipping");
            return;
        };
        let plugin = Plugin::load(PluginFormat::Clap, &path, &id, 48_000.0, 256).unwrap();
        assert!(!plugin.params().is_empty());
        let note = Event {
            offset: 0,
            kind: EventKind::NoteOn {
                ch: 0,
                key: 60,
                vel: 0.8,
            },
        };
        assert_eq!(render(&plugin, 0.0, &[], 10), 0.0);
        assert!(render(&plugin, 0.0, &[note], 40) > 0.01);
        let state = plugin.state().unwrap();
        let other = Plugin::load(PluginFormat::Clap, &path, &id, 48_000.0, 256).unwrap();
        other.set_state(&state).unwrap();
        assert_eq!(other.state().unwrap(), state);
        let p = &plugin.params()[0];
        assert!((0.0..=1.0).contains(&plugin.param_value(p.id)));
        assert!(!plugin.param_text(p.id, 0.5).is_empty());
    }

    #[test]
    fn a_clap_effect_processes_its_input() {
        let Some((path, id)) = find("MVerb") else {
            eprintln!("test plugin not installed; skipping");
            return;
        };
        let plugin = Plugin::load(PluginFormat::Clap, &path, &id, 48_000.0, 256).unwrap();
        assert!(plugin.node().num_inputs() >= 2);
        assert!(render(&plugin, 0.25, &[], 40) > 0.001);
    }
}
