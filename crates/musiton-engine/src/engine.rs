//! The engine: UI-side handle and audio-side processor.

use std::sync::Arc;
use std::time::Duration;

use musiton_core::{SamplePos, TempoMap, Tick, TimeSig};
use musiton_dsp::osc::SineOsc;
use musiton_dsp::smooth::LinearRamp;

use crate::command::{EngineCommand, Garbage, InputSource};
use crate::graph::CompiledGraph;
use crate::node::{Event, EventKind, ProcessContext};
use crate::EngineShared;

/// Test tone frequency in Hz.
pub const TEST_TONE_HZ: f64 = 440.0;
/// Test tone peak level (linear, 0.25 ≈ −12 dBFS).
pub const TEST_TONE_LEVEL: f32 = 0.25;
/// Fade time when the test tone is switched on/off.
const TONE_FADE_MS: f32 = 10.0;
/// Cross-fade time when a new graph replaces the running one.
const GRAPH_FADE_MS: f32 = 10.0;
/// Capacity of the UI → audio command ring.
const COMMAND_CAPACITY: usize = 1024;
/// Capacity of the audio → GC ring.
const GARBAGE_CAPACITY: usize = 256;
/// Most commands handled per block (keeps the callback time bounded).
const MAX_COMMANDS_PER_BLOCK: usize = 64;
/// Most live MIDI events handled per callback.
const MAX_LIVE_EVENTS: usize = 256;
/// Most input channels the engine reads.
pub const MAX_INPUT_CHANNELS: usize = 16;
/// Input frames allowed to queue up before old ones are skipped (keeps latency bounded).
const MAX_INPUT_BACKLOG: usize = 2048;
/// Level on input 1 that counts as "the measurement click arrived".
const PROBE_THRESHOLD: f32 = 0.1;
/// How often the GC thread wakes up.
const GC_INTERVAL: Duration = Duration::from_millis(20);

/// Creates a connected handle/engine pair and starts the GC thread.
///
/// `max_block` is the largest number of frames processed at once; longer device buffers
/// are split. Graphs sent to this engine must be built with the same `max_block`.
pub fn create(sample_rate: f32, max_block: usize) -> (EngineHandle, Engine) {
    let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (trash_tx, trash_rx) = rtrb::RingBuffer::<Garbage>::new(GARBAGE_CAPACITY);
    spawn_gc(trash_rx);
    let shared = EngineShared::new();
    let max_block = max_block.max(1);
    let handle = EngineHandle {
        commands: cmd_tx,
        shared: shared.clone(),
        sample_rate,
        max_block,
    };
    let engine = Engine {
        commands: cmd_rx,
        trash: trash_tx,
        shared,
        sample_rate,
        max_block,
        playing: false,
        pos: 0,
        loop_range: None,
        tempo: Box::new(TempoMap::default()),
        time_sig: TimeSig::default(),
        metronome: false,
        graph: None,
        old_graph: None,
        fade_len: ((GRAPH_FADE_MS * 0.001 * sample_rate) as usize).max(1),
        fade_pos: 0,
        tone: SineOsc::new(TEST_TONE_HZ, f64::from(sample_rate)),
        tone_gain: LinearRamp::with_time(0.0, TONE_FADE_MS, sample_rate),
        input: None,
        input_buf: vec![vec![0.0; max_block]; MAX_INPUT_CHANNELS],
        recording: false,
        punch: None,
        input_started: false,
        probe: None,
        midi_in: None,
        live: Vec::with_capacity(MAX_LIVE_EVENTS),
    };
    (handle, engine)
}

fn spawn_gc(mut trash: rtrb::Consumer<Garbage>) {
    let spawned = std::thread::Builder::new()
        .name("musiton-gc".into())
        .spawn(move || loop {
            while let Ok(garbage) = trash.pop() {
                drop(garbage);
            }
            if trash.is_abandoned() {
                break;
            }
            std::thread::sleep(GC_INTERVAL);
        });
    if let Err(e) = spawned {
        tracing::error!("could not start GC thread: {e}");
    }
}

/// UI-thread side: sends commands, reads status.
pub struct EngineHandle {
    commands: rtrb::Producer<EngineCommand>,
    shared: Arc<EngineShared>,
    sample_rate: f32,
    max_block: usize,
}

impl EngineHandle {
    /// Sends a command. Returns it back if the ring is full (engine not running).
    pub fn send(&mut self, cmd: EngineCommand) -> Result<(), EngineCommand> {
        self.commands
            .push(cmd)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Sends a command; logs and drops it if the ring is full.
    pub fn send_or_log(&mut self, cmd: EngineCommand) {
        if self.send(cmd).is_err() {
            tracing::warn!("engine command queue full, command dropped");
        }
    }

    /// Status shared with the audio thread.
    pub fn shared(&self) -> &Arc<EngineShared> {
        &self.shared
    }

    /// Sample rate the engine runs at.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Block size graphs for this engine must be built with.
    pub fn max_block(&self) -> usize {
        self.max_block
    }
}

/// Audio-thread side. Everything reachable from [`Engine::process`] is allocation-free.
pub struct Engine {
    commands: rtrb::Consumer<EngineCommand>,
    trash: rtrb::Producer<Garbage>,
    shared: Arc<EngineShared>,
    sample_rate: f32,
    max_block: usize,
    playing: bool,
    pos: u64,
    loop_range: Option<(Tick, Tick)>,
    tempo: Box<TempoMap>,
    time_sig: TimeSig,
    metronome: bool,
    graph: Option<Box<CompiledGraph>>,
    old_graph: Option<Box<CompiledGraph>>,
    fade_len: usize,
    fade_pos: usize,
    tone: SineOsc,
    tone_gain: LinearRamp,
    input: Option<Box<InputSource>>,
    /// Planar input of the current segment.
    input_buf: Vec<Vec<f32>>,
    recording: bool,
    punch: Option<(Tick, Tick)>,
    /// The current input has delivered samples at least once.
    input_started: bool,
    /// Frames since the latency-measurement click was played.
    probe: Option<u64>,
    midi_in: Option<Box<rtrb::Consumer<[u8; 3]>>>,
    /// Live MIDI for the next segment (cleared after it was handed to the graph).
    live: Vec<Event>,
}

impl Engine {
    /// Status shared with the UI thread.
    pub fn shared(&self) -> &Arc<EngineShared> {
        &self.shared
    }

    /// Fills `out` (interleaved, `channels` per frame) with the next audio.
    /// Graph channel 0 goes to device channel 0, channel 1 to channel 1 (mono graphs are
    /// duplicated); further device channels are silent. Any length is accepted.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        self.drain_commands();
        self.read_midi();
        let frames = out.len() / channels;
        self.skip_input_backlog(frames);
        let (body, tail) = out.split_at_mut(frames * channels);
        tail.fill(0.0);
        let mut done = 0;
        while done < frames {
            let mut n = (frames - done).min(self.max_block);
            let loop_samples = self.loop_samples();
            if let Some((_, end)) = loop_samples {
                n = n.min((end - self.pos) as usize);
            }
            let Some(seg) = body.get_mut(done * channels..(done + n) * channels) else {
                break;
            };
            self.render_segment(seg, channels, n);
            if self.playing {
                self.pos += n as u64;
                if let Some((start, end)) = loop_samples {
                    if self.pos >= end {
                        self.pos = start;
                    }
                }
            }
            done += n;
        }
        self.shared.publish_transport(self.playing, self.pos);
        self.shared.publish_recording(self.recording);
    }

    fn push_live(&mut self, kind: EventKind) {
        if self.live.len() < self.live.capacity() {
            self.live.push(Event { offset: 0, kind });
        }
    }

    /// Moves raw MIDI from the input ring into `live`.
    fn read_midi(&mut self) {
        let mut pending = [None; 32];
        let mut n = 0;
        if let Some(rx) = self.midi_in.as_mut() {
            while n < pending.len() {
                let Ok(bytes) = rx.pop() else {
                    break;
                };
                if let Some(slot) = pending.get_mut(n) {
                    *slot = Some(bytes);
                }
                n += 1;
            }
        }
        for bytes in pending.into_iter().flatten() {
            if let Some(kind) = EventKind::from_midi(bytes) {
                if let EventKind::Cc { ch, cc, val } = kind {
                    self.shared.set_last_cc(ch, cc, (val * 127.0).round() as u8);
                }
                self.push_live(kind);
            }
        }
    }

    /// Drops queued input beyond `frames + MAX_INPUT_BACKLOG` so input latency stays low.
    fn skip_input_backlog(&mut self, frames: usize) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        let ch = input.channels.max(1);
        let keep = (frames + MAX_INPUT_BACKLOG) * ch;
        let excess = input.samples.slots().saturating_sub(keep);
        let excess = excess - excess % ch;
        if excess > 0 {
            if let Ok(chunk) = input.samples.read_chunk(excess) {
                chunk.commit_all();
            }
        }
    }

    /// Reads `frames` input frames into `input_buf` (silence where the ring runs dry).
    fn read_input(&mut self, frames: usize) -> usize {
        let Some(input) = self.input.as_mut() else {
            return 0;
        };
        let ch = input.channels.clamp(1, MAX_INPUT_CHANNELS);
        let avail = (input.samples.slots() / input.channels.max(1)).min(frames);
        // Count input gaps only once the device has delivered data (not while it starts).
        if avail > 0 {
            self.input_started = true;
        }
        if avail < frames && self.input_started {
            self.shared.add_xrun();
        }
        if let Ok(chunk) = input.samples.read_chunk(avail * input.channels.max(1)) {
            let (a, b) = chunk.as_slices();
            for (k, s) in a.iter().chain(b).enumerate() {
                let (frame, c) = (k / input.channels.max(1), k % input.channels.max(1));
                if let Some(x) = self.input_buf.get_mut(c).and_then(|v| v.get_mut(frame)) {
                    *x = *s;
                }
            }
            chunk.commit_all();
        }
        for buf in self.input_buf.iter_mut().take(ch) {
            if let Some(rest) = buf.get_mut(avail..frames) {
                rest.fill(0.0);
            }
        }
        ch
    }

    /// Loop range in samples if the loop is active for the current position.
    fn loop_samples(&self) -> Option<(u64, u64)> {
        let (a, b) = self.loop_range?;
        if !self.playing {
            return None;
        }
        let sr = f64::from(self.sample_rate);
        let start = self.tempo.tick_to_sample(a.0 as f64, sr).round().max(0.0) as u64;
        let end = self.tempo.tick_to_sample(b.0 as f64, sr).round().max(0.0) as u64;
        (end > start && self.pos < end).then_some((start, end))
    }

    fn drain_commands(&mut self) {
        for _ in 0..MAX_COMMANDS_PER_BLOCK {
            let Ok(cmd) = self.commands.pop() else {
                break;
            };
            match cmd {
                EngineCommand::Play => self.playing = true,
                EngineCommand::Stop => self.playing = false,
                EngineCommand::Locate(SamplePos(p)) => self.pos = p,
                EngineCommand::SetLoop(range) => self.loop_range = range,
                EngineCommand::SetTempoMap(new) => {
                    let sr = f64::from(self.sample_rate);
                    let tick = self.tempo.sample_to_tick(self.pos as f64, sr);
                    self.pos = new.tick_to_sample(tick, sr).round().max(0.0) as u64;
                    let old = std::mem::replace(&mut self.tempo, new);
                    self.retire(old);
                }
                EngineCommand::SetTimeSig(sig) => self.time_sig = sig,
                EngineCommand::SetInput(new) => {
                    self.input_started = false;
                    if let Some(old) = std::mem::replace(&mut self.input, new) {
                        self.retire(old);
                    }
                }
                EngineCommand::SetRecording(on) => self.recording = on,
                EngineCommand::SetMidiInput(new) => {
                    if let Some(old) = std::mem::replace(&mut self.midi_in, new) {
                        self.retire(old);
                    }
                }
                EngineCommand::PreviewNote { on, key, vel } => {
                    let key = key.min(127);
                    self.push_live(if on {
                        EventKind::NoteOn { ch: 0, key, vel }
                    } else {
                        EventKind::NoteOff { ch: 0, key }
                    });
                }
                EngineCommand::SetPunch(range) => self.punch = range,
                EngineCommand::SetMetronome(on) => self.metronome = on,
                EngineCommand::SwapGraph(new) => {
                    if let Some(stale) = self.old_graph.take() {
                        self.retire(stale);
                    }
                    self.old_graph = self.graph.replace(new);
                    self.fade_pos = 0;
                }
                EngineCommand::NodeData { node, data } => {
                    let rest = match self.graph.as_mut() {
                        Some(g) => g.send_data(node, data),
                        None => Some(data),
                    };
                    if let Some(old) = rest {
                        if let Err(rtrb::PushError::Full(g)) = self.trash.push(old) {
                            std::mem::forget(g);
                        }
                    }
                }
                EngineCommand::SetParam { node, id, val } => {
                    if let Some(g) = self.graph.as_mut() {
                        g.set_param(node, id, val);
                    }
                }
            }
        }
    }

    /// Hands an object to the GC thread. If its queue is full the object is leaked
    /// rather than freed here (freeing on the audio thread is not allowed).
    fn retire<T: std::any::Any + Send>(&mut self, obj: Box<T>) {
        if let Err(rtrb::PushError::Full(g)) = self.trash.push(obj) {
            std::mem::forget(g);
        }
    }

    fn render_segment(&mut self, out: &mut [f32], channels: usize, frames: usize) {
        let in_ch = self.read_input(frames);
        let sr = f64::from(self.sample_rate);
        let punch = self.punch.map(|(a, b)| {
            let at = |t: Tick| self.tempo.tick_to_sample(t.0 as f64, sr).round().max(0.0) as u64;
            (at(a), at(b))
        });
        let ctx = ProcessContext {
            sample_rate: self.sample_rate,
            frames,
            playing: self.playing,
            sample_pos: self.pos,
            tempo: &self.tempo,
            time_sig: self.time_sig,
            metronome: self.metronome,
            inputs: self.input_buf.get(..in_ch).unwrap_or(&[]),
            recording: self.recording && self.playing,
            punch,
            live_midi: &self.live,
        };
        out.fill(0.0);
        let fade_start = self.fade_pos;
        let fade_len = self.fade_len;
        let fade_in = |i: usize| ((fade_start + i) as f32 / fade_len as f32).min(1.0);
        if let Some(g) = self.graph.as_mut() {
            g.process(&ctx);
            mix_graph(g, out, channels, frames, fade_in);
        }
        if let Some(old) = self.old_graph.as_mut() {
            old.process(&ctx);
            mix_graph(old, out, channels, frames, |i| 1.0 - fade_in(i));
        }
        if self.fade_pos < self.fade_len {
            self.fade_pos = (self.fade_pos + frames).min(self.fade_len);
            if self.fade_pos == self.fade_len {
                if let Some(done) = self.old_graph.take() {
                    self.retire(done);
                }
            }
        }
        self.live.clear();
        self.mix_test_tone(out, channels);
        self.run_probe(out, channels, frames, in_ch > 0);
    }

    /// Latency measurement: plays a short click, then counts frames until input 1 hears it.
    fn run_probe(&mut self, out: &mut [f32], channels: usize, frames: usize, has_input: bool) {
        if self.shared.take_probe_request() {
            self.probe = Some(0);
            for frame in out.chunks_exact_mut(channels).take(4) {
                frame.fill(0.5);
            }
        }
        let Some(start) = self.probe else {
            return;
        };
        let input = self
            .input_buf
            .first()
            .map_or(&[][..], |b| b.get(..frames).unwrap_or(&[]));
        if has_input {
            if let Some(i) = input.iter().position(|s| s.abs() > PROBE_THRESHOLD) {
                self.shared.set_probe_result((start + i as u64) as i64);
                self.probe = None;
                return;
            }
        }
        let elapsed = start + frames as u64;
        if elapsed > self.sample_rate as u64 {
            self.shared.set_probe_result(crate::PROBE_TIMEOUT);
            self.probe = None;
        } else {
            self.probe = Some(elapsed);
        }
    }

    fn mix_test_tone(&mut self, out: &mut [f32], channels: usize) {
        let target = if self.shared.test_tone() {
            TEST_TONE_LEVEL
        } else {
            0.0
        };
        self.tone_gain.set_target(target);
        if !self.tone_gain.is_ramping() && self.tone_gain.target() == 0.0 {
            self.tone.reset();
            return;
        }
        for frame in out.chunks_exact_mut(channels) {
            let s = self.tone.next_sample() * self.tone_gain.next_value();
            for x in frame {
                *x += s;
            }
        }
    }
}

/// Adds the graph's stereo (or mono) output, scaled by `gain(frame)`, to `out`.
fn mix_graph(
    g: &CompiledGraph,
    out: &mut [f32],
    channels: usize,
    frames: usize,
    gain: impl Fn(usize) -> f32,
) {
    let left = g.output(0, frames);
    let right = if g.num_outputs() > 1 {
        g.output(1, frames)
    } else {
        left
    };
    for (i, frame) in out.chunks_exact_mut(channels).enumerate() {
        let k = gain(i);
        let l = left.get(i).copied().unwrap_or(0.0) * k;
        let r = right.get(i).copied().unwrap_or(0.0) * k;
        match frame {
            [mono] => *mono += 0.5 * (l + r),
            [a, b, ..] => {
                *a += l;
                *b += r;
            }
            [] => {}
        }
    }
}

/// Renders `frames` frames offline in callbacks of `callback_frames` frames.
/// Used by tests and export; runs exactly the same code as the device callback.
pub fn render_offline(
    engine: &mut Engine,
    frames: usize,
    channels: usize,
    callback_frames: usize,
) -> Vec<f32> {
    let mut out = vec![0.0; frames * channels];
    for buf in out.chunks_mut(callback_frames.max(1) * channels.max(1)) {
        engine.process(buf, channels);
    }
    out
}
