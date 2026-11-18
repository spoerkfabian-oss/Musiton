//! A VST3 plugin instance: setup, processing, parameters and state.

// The bindings' enum and char types differ between Windows and Linux, so some casts are
// needed on one platform and no-ops on the other.
#![allow(clippy::unnecessary_cast)]

use std::path::Path;
use std::sync::Arc;

use musiton_core::PPQ;
use musiton_engine::node::{AudioIo, Event, EventKind, ProcessContext};
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0,
    BusDirections_::{kInput, kOutput},
    BusInfo, Event as VstEvent,
    Event_::EventTypes_,
    IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentHandler, IComponentTrait,
    IConnectionPoint, IConnectionPointTrait, IEditController, IEditControllerTrait, IEventList,
    IMidiMapping, IMidiMappingTrait, IParameterChanges,
    MediaTypes_::{kAudio, kEvent},
    NoteOffEvent, NoteOnEvent, ParameterInfo,
    ParameterInfo_::ParameterFlags_,
    ProcessContext as VstContext,
    ProcessContext_::StatesAndFlags_,
    ProcessData,
    ProcessModes_::kRealtime,
    ProcessSetup,
    SpeakerArr::kStereo,
    SpeakerArrangement,
    SymbolicSampleSizes_::kSample32,
};
use vst3::Steinberg::{
    kResultOk, FUnknown, IBStream, IPluginBase, IPluginBaseTrait, IPluginFactoryTrait, TUID,
};
use vst3::{ComPtr, ComWrapper, Interface};

use crate::plugin::{Controller, ParamChange, Plugin, PluginParam, Processor};
use crate::vst3_host::{
    read_wide, ComponentHandler, Edit, EventList, HostApplication, MemoryStream, ParameterChanges,
};
use crate::vst3_module::Module;
use crate::{PluginError, PluginFormat};

/// Events a block can carry to the plugin.
const MAX_EVENTS: usize = 1024;

/// The class id written as 32 hex digits, back as a TUID.
pub(crate) fn tuid_from_hex(hex: &str) -> Option<TUID> {
    if hex.len() != 32 {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()? as std::ffi::c_char;
    }
    Some(out)
}

fn failed(what: &str) -> PluginError {
    PluginError::Failed(what.into())
}

/// The plugin's objects; shuts the plugin down when the last half lets go of it.
struct Core {
    // Fields drop in this order: connections, controller, processor, component, module.
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    controller: Option<ComPtr<IEditController>>,
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    /// Whether the controller is a separate object (initialized and terminated by us).
    separate: bool,
    /// Kept alive while the plugin may call it.
    _host: ComWrapper<HostApplication>,
    handler: ComWrapper<ComponentHandler>,
    /// The library; declared last so it unloads after every object above is released.
    _module: Arc<Module>,
}

// SAFETY: VST3 objects are reference-counted COM objects that may be released from any
// thread; the processing half is only used on the audio thread and the controller half
// only on the UI thread, as the VST3 threading model asks.
unsafe impl Send for Core {}
// SAFETY: as above.
unsafe impl Sync for Core {}

impl Drop for Core {
    fn drop(&mut self) {
        // SAFETY: plain calls on objects we own, in the order the SDK prescribes.
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            if let Some((a, b)) = &self.connection {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
                if self.separate {
                    if let Some(base) = c.cast::<IPluginBase>() {
                        base.terminate();
                    }
                }
            }
            self.component.terminate();
        }
        // Fields are dropped in declaration order after this: the plugin's objects are
        // released first and `_module` (declared last) unloads the library afterwards.
    }
}

/// Loads class `id` (32 hex digits) from the bundle at `path`.
pub(crate) fn load(
    path: &Path,
    id: &str,
    sample_rate: f64,
    max_block: usize,
) -> Result<Plugin, PluginError> {
    let module = Module::shared(path)?;
    let factory = module
        .factory()
        .ok_or_else(|| failed("no factory"))?
        .clone();
    let cid = tuid_from_hex(id).ok_or_else(|| failed("bad plugin id"))?;
    let host = ComWrapper::new(HostApplication);
    let host_ptr = host
        .as_com_ref::<vst3::Steinberg::Vst::IHostApplication>()
        .ok_or_else(|| failed("host"))?
        .as_ptr()
        .cast::<FUnknown>();

    let create = |cid: &TUID, iid: &TUID| -> Option<*mut std::ffi::c_void> {
        let mut obj = std::ptr::null_mut();
        // SAFETY: ids are 16-byte arrays; `obj` receives an owned reference.
        let r = unsafe { factory.createInstance(cid.as_ptr(), iid.as_ptr(), &mut obj) };
        (r == kResultOk && !obj.is_null()).then_some(obj)
    };
    let iid = |g: vst3::com_scrape_types::Guid| -> TUID { g.map(|b| b as std::ffi::c_char) };

    let raw =
        create(&cid, &iid(IComponent::IID)).ok_or_else(|| failed("could not create the plugin"))?;
    // SAFETY: the factory returned an owned IComponent reference.
    let component = unsafe { ComPtr::from_raw(raw.cast::<IComponent>()) }
        .ok_or_else(|| failed("no component"))?;
    // SAFETY: initialize with our host context (kept alive in `Core`).
    if unsafe { component.initialize(host_ptr) } != kResultOk {
        return Err(failed("the plugin did not initialize"));
    }
    let processor = component
        .cast::<IAudioProcessor>()
        .ok_or_else(|| failed("not an audio processor"))?;

    // The controller: the component itself, or a separate object.
    let mut separate = false;
    let controller = match component.cast::<IEditController>() {
        Some(c) => Some(c),
        None => {
            let mut ccid: TUID = [0; 16];
            // SAFETY: out pointer to a TUID.
            let ok = unsafe { component.getControllerClassId(&mut ccid) } == kResultOk;
            let made = ok
                .then(|| create(&ccid, &iid(IEditController::IID)))
                .flatten()
                // SAFETY: owned IEditController reference from the factory.
                .and_then(|raw| unsafe { ComPtr::from_raw(raw.cast::<IEditController>()) });
            if let Some(c) = &made {
                separate = true;
                if let Some(base) = c.cast::<IPluginBase>() {
                    // SAFETY: initialize the separate controller with our host context.
                    unsafe { base.initialize(host_ptr) };
                }
            }
            made
        }
    };
    let handler = ComWrapper::new(ComponentHandler::default());
    let mut core = Core {
        component,
        processor,
        controller,
        separate,
        connection: None,
        _host: host,
        handler,
        _module: module,
    };
    if separate {
        if let (Some(a), Some(b)) = (
            core.component.cast::<IConnectionPoint>(),
            core.controller
                .as_ref()
                .and_then(|c| c.cast::<IConnectionPoint>()),
        ) {
            // SAFETY: both connection points belong to this plugin.
            unsafe {
                a.connect(b.as_ptr());
                b.connect(a.as_ptr());
            }
            core.connection = Some((a, b));
        }
    }
    if let Some(c) = &core.controller {
        if let Some(h) = core.handler.as_com_ref::<IComponentHandler>() {
            // SAFETY: the handler lives as long as `core`.
            unsafe { c.setComponentHandler(h.as_ptr()) };
        }
        // The controller starts from the processor's state.
        let stream = ComWrapper::new(MemoryStream::default());
        if let Some(s) = stream.as_com_ref::<IBStream>() {
            // SAFETY: valid stream object.
            unsafe {
                if core.component.getState(s.as_ptr()) == kResultOk {
                    use vst3::Steinberg::IBStreamTrait;
                    s.seek(0, 0, std::ptr::null_mut());
                    c.setComponentState(s.as_ptr());
                }
            }
        }
    }

    // Buses: stereo where the plugin accepts it.
    // SAFETY: plain queries.
    let (n_in, n_out) = unsafe {
        (
            core.component
                .getBusCount(kAudio as i32, kInput as i32)
                .max(0),
            core.component
                .getBusCount(kAudio as i32, kOutput as i32)
                .max(0),
        )
    };
    let mut ins: Vec<SpeakerArrangement> = vec![kStereo; n_in as usize];
    let mut outs: Vec<SpeakerArrangement> = vec![kStereo; n_out as usize];
    // SAFETY: arrays of the given lengths.
    unsafe {
        core.processor
            .setBusArrangements(ins.as_mut_ptr(), n_in, outs.as_mut_ptr(), n_out);
    }
    let channels = |dir: i32, n: i32| -> Vec<usize> {
        (0..n)
            .map(|i| {
                let mut arr: SpeakerArrangement = 0;
                // SAFETY: out pointer.
                let ok = unsafe { core.processor.getBusArrangement(dir, i, &mut arr) } == kResultOk;
                if ok {
                    arr.count_ones() as usize
                } else {
                    let mut info: BusInfo = zeroed();
                    // SAFETY: out pointer.
                    unsafe { core.component.getBusInfo(kAudio as i32, dir, i, &mut info) };
                    info.channelCount.max(0) as usize
                }
            })
            .collect()
    };
    let in_channels = channels(kInput as i32, n_in);
    let out_channels = channels(kOutput as i32, n_out);
    // SAFETY: plain calls; main buses on, the first event input on.
    unsafe {
        for i in 0..n_in {
            core.component
                .activateBus(kAudio as i32, kInput as i32, i, u8::from(i == 0));
        }
        for i in 0..n_out {
            core.component
                .activateBus(kAudio as i32, kOutput as i32, i, u8::from(i == 0));
        }
        if core.component.getBusCount(kEvent as i32, kInput as i32) > 0 {
            core.component
                .activateBus(kEvent as i32, kInput as i32, 0, 1);
        }
    }
    let mut setup = ProcessSetup {
        processMode: kRealtime as i32,
        symbolicSampleSize: kSample32 as i32,
        maxSamplesPerBlock: max_block as i32,
        sampleRate: sample_rate,
    };
    // SAFETY: plain calls in the order the SDK asks for.
    unsafe {
        if core.processor.setupProcessing(&mut setup) != kResultOk {
            return Err(failed("the plugin refused the sample rate or block size"));
        }
        if core.component.setActive(1) != kResultOk {
            return Err(failed("the plugin could not be activated"));
        }
        core.processor.setProcessing(1);
    }

    // Controllers (CC numbers, pitch bend) the plugin maps to parameters.
    let mut cc_params = [None; 130];
    if let Some(mapping) = core
        .controller
        .as_ref()
        .and_then(|c| c.cast::<IMidiMapping>())
    {
        for (cc, slot) in cc_params.iter_mut().enumerate() {
            let mut id = 0;
            // SAFETY: out pointer.
            if unsafe { mapping.getMidiControllerAssignment(0, 0, cc as i16, &mut id) } == kResultOk
            {
                *slot = Some(id);
            }
        }
    }

    let params = read_params(core.controller.as_ref());
    let core = Arc::new(core);
    let processor = Vst3Processor::new(
        core.clone(),
        &in_channels,
        &out_channels,
        max_block,
        sample_rate,
        cc_params,
    );
    let controller = Vst3Controller { core, params };
    // Musiton feeds and takes stereo on the main buses (plus a stereo sidechain input).
    let inputs = if in_channels.is_empty() {
        0
    } else {
        2 + 2 * usize::from(in_channels.len() > 1)
    };
    Ok(Plugin::new(
        PluginFormat::Vst3,
        Box::new(processor),
        Box::new(controller),
        (inputs, 2),
    ))
}

fn zeroed<T: Copy>() -> T {
    // SAFETY: only used for plain VST3 C structs (numbers, arrays, raw pointers), for
    // which all-zero is a valid value.
    unsafe { std::mem::zeroed() }
}

fn read_params(controller: Option<&ComPtr<IEditController>>) -> Vec<PluginParam> {
    let Some(c) = controller else {
        return Vec::new();
    };
    // SAFETY: plain query.
    let n = unsafe { c.getParameterCount() }.max(0);
    (0..n)
        .filter_map(|i| {
            let mut info: ParameterInfo = zeroed();
            // SAFETY: out pointer.
            if unsafe { c.getParameterInfo(i, &mut info) } != kResultOk {
                return None;
            }
            let flags = info.flags;
            if flags & ParameterFlags_::kIsHidden as i32 != 0 {
                return None;
            }
            Some(PluginParam {
                id: info.id,
                name: read_wide(&info.title),
                unit: read_wide(&info.units),
                default: info.defaultNormalizedValue,
                steps: info.stepCount,
                automatable: flags & ParameterFlags_::kCanAutomate as i32 != 0
                    && flags & ParameterFlags_::kIsReadOnly as i32 == 0,
            })
        })
        .collect()
}

/// The audio-thread half.
struct Vst3Processor {
    core: Arc<Core>,
    /// One buffer per channel per bus, and the pointer arrays the plugin reads.
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    in_ptrs: Vec<Vec<*mut f32>>,
    out_ptrs: Vec<Vec<*mut f32>>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    events: ComWrapper<EventList>,
    out_events: ComWrapper<EventList>,
    changes: ComWrapper<ParameterChanges>,
    out_changes: ComWrapper<ParameterChanges>,
    context: VstContext,
    sample_rate: f64,
    cc_params: [Option<u32>; 130],
    /// Sounding notes per channel (bit = key), to end them on "all notes off".
    held: [u128; 16],
}

// SAFETY: used only on the audio thread after creation; the raw pointers point into
// buffers owned by this struct.
unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    fn new(
        core: Arc<Core>,
        ins: &[usize],
        outs: &[usize],
        max_block: usize,
        sample_rate: f64,
        cc_params: [Option<u32>; 130],
    ) -> Self {
        let bufs = |chans: &[usize]| -> Vec<Vec<Vec<f32>>> {
            chans
                .iter()
                .map(|&n| vec![vec![0.0; max_block]; n])
                .collect()
        };
        let mut p = Self {
            core,
            in_bufs: bufs(ins),
            out_bufs: bufs(outs),
            in_ptrs: Vec::new(),
            out_ptrs: Vec::new(),
            in_buses: Vec::new(),
            out_buses: Vec::new(),
            events: ComWrapper::new(EventList::with_capacity(MAX_EVENTS)),
            out_events: ComWrapper::new(EventList::with_capacity(MAX_EVENTS)),
            changes: ComWrapper::new(ParameterChanges::new(256, 64)),
            out_changes: ComWrapper::new(ParameterChanges::new(256, 64)),
            context: zeroed(),
            sample_rate,
            cc_params,
            held: [0; 16],
        };
        p.in_ptrs = p
            .in_bufs
            .iter_mut()
            .map(|bus| bus.iter_mut().map(|c| c.as_mut_ptr()).collect())
            .collect();
        p.out_ptrs = p
            .out_bufs
            .iter_mut()
            .map(|bus| bus.iter_mut().map(|c| c.as_mut_ptr()).collect())
            .collect();
        let bus = |ptrs: &mut Vec<*mut f32>| AudioBusBuffers {
            numChannels: ptrs.len() as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: ptrs.as_mut_ptr(),
            },
        };
        p.in_buses = p.in_ptrs.iter_mut().map(bus).collect();
        p.out_buses = p.out_ptrs.iter_mut().map(bus).collect();
        p
    }

    fn note(&mut self, offset: i32, on: bool, ch: u8, key: u8, vel: f32) {
        let mut e: VstEvent = zeroed();
        e.sampleOffset = offset;
        if on {
            e.r#type = EventTypes_::kNoteOnEvent as u16;
            e.__field0.noteOn = NoteOnEvent {
                channel: i16::from(ch),
                pitch: i16::from(key),
                tuning: 0.0,
                velocity: vel,
                length: 0,
                noteId: -1,
            };
        } else {
            e.r#type = EventTypes_::kNoteOffEvent as u16;
            e.__field0.noteOff = NoteOffEvent {
                channel: i16::from(ch),
                pitch: i16::from(key),
                velocity: 0.0,
                noteId: -1,
                tuning: 0.0,
            };
        }
        if let Some(h) = self.held.get_mut(usize::from(ch & 15)) {
            let bit = 1u128 << (key & 127);
            if on {
                *h |= bit;
            } else {
                *h &= !bit;
            }
        }
        // SAFETY: audio thread, between process calls.
        unsafe { self.events.push(e) };
    }

    fn all_notes_off(&mut self, offset: i32) {
        for ch in 0..16u8 {
            let mut keys = self.held[usize::from(ch)];
            while keys != 0 {
                let key = keys.trailing_zeros() as u8;
                keys &= keys - 1;
                self.note(offset, false, ch, key, 0.0);
            }
        }
    }
}

impl Processor for Vst3Processor {
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
        // SAFETY: audio thread, the plugin is not processing right now.
        unsafe {
            self.events.clear();
            self.out_events.clear();
        }
        self.changes.clear();
        self.out_changes.clear();
        for &(id, offset, v) in params {
            self.changes.add(id, offset, v);
        }
        for e in events {
            let offset = e.offset as i32;
            match e.kind {
                EventKind::NoteOn { ch, key, vel } => self.note(offset, true, ch, key, vel),
                EventKind::NoteOff { ch, key } => self.note(offset, false, ch, key, 0.0),
                EventKind::AllNotesOff => self.all_notes_off(offset),
                EventKind::Cc { cc, val, .. } => {
                    if let Some(Some(id)) = self.cc_params.get(usize::from(cc)) {
                        self.changes.add(*id, offset, f64::from(val));
                    }
                }
                EventKind::Param { .. } => {}
            }
        }

        // Inputs: main bus = node inputs 0/1, second bus (sidechain) = inputs 2/3.
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

        // Transport.
        let c = &mut self.context;
        let tick = ctx
            .tempo
            .sample_to_tick(ctx.sample_pos as f64, self.sample_rate);
        c.state = (StatesAndFlags_::kTempoValid
            | StatesAndFlags_::kTimeSigValid
            | StatesAndFlags_::kProjectTimeMusicValid
            | if ctx.playing {
                StatesAndFlags_::kPlaying
            } else {
                0
            }) as u32;
        c.sampleRate = self.sample_rate;
        c.projectTimeSamples = ctx.sample_pos as i64;
        c.projectTimeMusic = tick / PPQ as f64;
        c.tempo = ctx.tempo.bpm_at(tick);
        c.timeSigNumerator = i32::from(ctx.time_sig.num);
        c.timeSigDenominator = i32::from(ctx.time_sig.den);

        let mut data = ProcessData {
            processMode: kRealtime as i32,
            symbolicSampleSize: kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.in_buses.len() as i32,
            numOutputs: self.out_buses.len() as i32,
            inputs: self.in_buses.as_mut_ptr(),
            outputs: self.out_buses.as_mut_ptr(),
            inputParameterChanges: self
                .changes
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            outputParameterChanges: self
                .out_changes
                .as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            inputEvents: self
                .events
                .as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            outputEvents: self
                .out_events
                .as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr()),
            processContext: &mut self.context,
        };
        // SAFETY: every pointer in `data` points into buffers owned by `self` that hold
        // at least `frames` samples per channel.
        let ok = unsafe { self.core.processor.process(&mut data) } == kResultOk;
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
        self.all_notes_off(0);
    }
}

/// The UI-thread half.
struct Vst3Controller {
    core: Arc<Core>,
    params: Vec<PluginParam>,
}

impl Vst3Controller {
    fn stream_from(&self, f: impl FnOnce(*mut IBStream) -> i32) -> Option<Vec<u8>> {
        let stream = ComWrapper::new(MemoryStream::default());
        let s = stream.as_com_ref::<IBStream>()?;
        (f(s.as_ptr()) == kResultOk).then(|| stream.data.borrow().clone())
    }
}

impl Controller for Vst3Controller {
    fn params(&self) -> &[PluginParam] {
        &self.params
    }

    fn param_value(&self, id: u32) -> f64 {
        self.core
            .controller
            .as_ref()
            // SAFETY: plain query.
            .map_or(0.0, |c| unsafe { c.getParamNormalized(id) })
    }

    fn set_param(&self, id: u32, value: f64) {
        if let Some(c) = &self.core.controller {
            // SAFETY: plain call on the UI thread.
            unsafe { c.setParamNormalized(id, value) };
        }
    }

    fn param_text(&self, id: u32, value: f64) -> String {
        let Some(c) = &self.core.controller else {
            return format!("{value:.2}");
        };
        let mut s = [0; 128];
        // SAFETY: out buffer of the right size.
        if unsafe { c.getParamStringByValue(id, value, &mut s) } == kResultOk {
            read_wide(&s)
        } else {
            format!("{value:.2}")
        }
    }

    fn take_edits(&self) -> Vec<Edit> {
        self.core
            .handler
            .edits
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default()
    }

    fn state(&self) -> Result<Vec<u8>, PluginError> {
        // SAFETY: valid stream pointers.
        let component = self
            .stream_from(|s| unsafe { self.core.component.getState(s) })
            .ok_or_else(|| failed("the plugin did not save its state"))?;
        let controller = self
            .core
            .controller
            .as_ref()
            .filter(|_| self.core.separate)
            // SAFETY: valid stream pointer.
            .and_then(|c| self.stream_from(|s| unsafe { c.getState(s) }))
            .unwrap_or_default();
        let mut out = (component.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&component);
        out.extend_from_slice(&controller);
        Ok(out)
    }

    fn set_state(&self, state: &[u8]) -> Result<(), PluginError> {
        let bad = || failed("the saved plugin state is damaged");
        let len = u32::from_le_bytes(
            state
                .get(..4)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        ) as usize;
        let component = state.get(4..4 + len).ok_or_else(bad)?;
        let controller = state.get(4 + len..).unwrap_or_default();
        let load = |bytes: &[u8], f: &dyn Fn(*mut IBStream) -> i32| -> bool {
            let stream = ComWrapper::new(MemoryStream::from_bytes(bytes));
            stream
                .as_com_ref::<IBStream>()
                .is_some_and(|s| f(s.as_ptr()) == kResultOk)
        };
        // SAFETY: valid stream pointers.
        if !load(component, &|s| unsafe { self.core.component.setState(s) }) {
            return Err(failed("the plugin did not accept its saved state"));
        }
        if let Some(c) = &self.core.controller {
            // SAFETY: valid stream pointers.
            load(component, &|s| unsafe { c.setComponentState(s) });
            if self.core.separate && !controller.is_empty() {
                // SAFETY: as above.
                load(controller, &|s| unsafe { c.setState(s) });
            }
        }
        Ok(())
    }

    fn latency(&self) -> u32 {
        // SAFETY: plain query.
        unsafe { self.core.processor.getLatencySamples() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn ctx<'a>(tempo: &'a TempoMap, frames: usize, pos: u64) -> ProcessContext<'a> {
        ProcessContext {
            sample_rate: 48_000.0,
            frames,
            playing: true,
            sample_pos: pos,
            tempo,
            time_sig: TimeSig::default(),
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
            live_midi: &[],
        }
    }

    /// Renders `blocks` blocks of 256 frames; `events` go into the first block.
    fn render(plugin: &Plugin, input: f32, events: &[Event], blocks: usize) -> Vec<f32> {
        let mut node = plugin.node();
        node.prepare(48_000.0, 256);
        let tempo = TempoMap::constant(120.0);
        let ins = vec![vec![input; 256]; node.num_inputs()];
        let mut outs = vec![vec![0.0; 256]; node.num_outputs()];
        let mut left = Vec::new();
        for b in 0..blocks {
            let mut io = AudioIo::new(&ins, &mut outs, 256);
            let ev = if b == 0 { events } else { &[] };
            node.process(&ctx(&tempo, 256, (b * 256) as u64), &mut io, ev);
            left.extend_from_slice(&outs[0]);
        }
        left
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    fn find(name: &str) -> Option<(std::path::PathBuf, String)> {
        let path = std::path::PathBuf::from(format!("/usr/lib/vst3/{name}.vst3"));
        let id = crate::scan_file(&path).ok()?.first()?.id.clone();
        Some((path, id))
    }

    #[test]
    fn a_vst3_synth_plays_notes_and_keeps_its_state() {
        let Some((path, id)) = find("Kars") else {
            eprintln!("test plugin not installed; skipping");
            return;
        };
        let plugin = load(&path, &id, 48_000.0, 256).unwrap();
        assert_eq!(plugin.node().num_outputs(), 2);
        assert!(!plugin.params().is_empty());
        let note = Event {
            offset: 0,
            kind: EventKind::NoteOn {
                ch: 0,
                key: 60,
                vel: 0.8,
            },
        };
        let silent = render(&plugin, 0.0, &[], 20);
        assert_eq!(peak(&silent), 0.0);
        let played = render(&plugin, 0.0, &[note], 40);
        assert!(peak(&played) > 0.01, "peak {}", peak(&played));

        // State round trip, also into a second instance.
        let p = plugin.params()[0].clone();
        let target = if plugin.param_value(p.id) > 0.5 {
            0.1
        } else {
            0.9
        };
        plugin.set_param(p.id, target);
        let state = plugin.state().unwrap();
        let other = load(&path, &id, 48_000.0, 256).unwrap();
        other.set_state(&state).unwrap();
        assert_eq!(other.state().unwrap(), state);
        assert!(!plugin.param_text(p.id, 0.5).is_empty());
        assert!(other.set_state(&[1, 2]).is_err());
    }

    #[test]
    fn a_vst3_effect_processes_its_input_and_newest_node_wins() {
        let Some((path, id)) = find("MVerb") else {
            eprintln!("test plugin not installed; skipping");
            return;
        };
        let plugin = load(&path, &id, 48_000.0, 256).unwrap();
        assert!(plugin.node().num_inputs() >= 2);
        let wet = render(&plugin, 0.25, &[], 40);
        assert!(peak(&wet) > 0.001, "peak {}", peak(&wet));

        // Two nodes of one instance: the older one goes silent once the newer one ran.
        let tempo = TempoMap::constant(120.0);
        let mut old = plugin.node();
        let mut new = plugin.node();
        let ins = vec![vec![0.25; 256]; 2];
        let mut outs = vec![vec![0.0; 256]; 2];
        new.process(
            &ctx(&tempo, 256, 0),
            &mut AudioIo::new(&ins, &mut outs, 256),
            &[],
        );
        old.process(
            &ctx(&tempo, 256, 0),
            &mut AudioIo::new(&ins, &mut outs, 256),
            &[],
        );
        assert_eq!(peak(&outs[0]), 0.0);
        assert!(tuid_from_hex(&id).is_some());
    }
}
