//! Running a plugin in a separate process (the sandbox), so a crash inside the plugin
//! cannot take Musiton down.
//!
//! The child is Musiton itself started as `musiton --plugin-host …`. Audio and events go
//! through a shared memory file; control (parameters, state, editor) through the child's
//! stdin/stdout as length-prefixed JSON messages.
//!
//! The audio thread never waits for the child: each block it hands over its input and
//! takes the result of the previous block, so a sandboxed plugin adds exactly one block
//! (`max_block` frames) of latency, which delay compensation then evens out. If the child
//! is late or gone, the plugin's output is silent for that block.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use musiton_core::{TempoMap, TimeSig};
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};
use serde::{Deserialize, Serialize};

use crate::plugin::{Controller, Edit, ParamChange, Plugin, PluginParam, Processor};
use crate::{PluginError, PluginFormat};

/// Command-line switch that turns the executable into a sandbox host.
pub const HOST_ARG: &str = "--plugin-host";

/// Events per block that fit into the shared memory.
const MAX_EVENTS: usize = 512;
/// Audio channels exchanged per direction (main + sidechain in, stereo out).
const IN_CHANNELS: usize = 4;
const OUT_CHANNELS: usize = 2;
const MAGIC: u64 = 0x4D55_5349_544F_4E31; // "MUSITON1"
/// How long the UI waits for an answer from the child.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the UI refreshes the parameter values shown for a sandboxed plugin.
const VALUE_REFRESH: Duration = Duration::from_millis(250);

/// One event in the shared memory.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WireEvent {
    offset: u32,
    /// 0 note on, 1 note off, 2 controller, 3 all notes off, 4 parameter.
    kind: u32,
    a: u32,
    b: u32,
    value: f64,
}

/// The start of the shared memory; the audio buffers follow it.
#[repr(C)]
struct Header {
    magic: u64,
    max_block: u64,
    /// Parent: number of the block now in the buffers.
    request: AtomicU64,
    /// Child: number of the last block it finished.
    done: AtomicU64,
    frames: u64,
    playing: u64,
    sample_pos: u64,
    tick: f64,
    bpm: f64,
    sig_num: u64,
    sig_den: u64,
    n_events: u64,
    events: [WireEvent; MAX_EVENTS],
}

/// Byte offset of the audio buffers.
fn audio_offset() -> usize {
    std::mem::size_of::<Header>().div_ceil(64) * 64
}

fn shm_size(max_block: usize) -> usize {
    audio_offset() + (IN_CHANNELS + OUT_CHANNELS) * max_block * std::mem::size_of::<f32>()
}

/// A mapped shared memory file.
struct Shm {
    map: memmap2::MmapMut,
    max_block: usize,
    path: Option<PathBuf>,
}

// SAFETY: the mapping is plain memory; the protocol (request/done counters) decides which
// side writes which part when.
unsafe impl Send for Shm {}

impl Shm {
    fn create(path: &Path, max_block: usize) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(shm_size(max_block) as u64)?;
        // SAFETY: the file is ours and stays the right size while mapped.
        let map = unsafe { memmap2::MmapOptions::new().map_mut(&file)? };
        let mut shm = Self {
            map,
            max_block,
            path: Some(path.to_owned()),
        };
        let h = shm.header_mut();
        h.magic = MAGIC;
        h.max_block = max_block as u64;
        Ok(shm)
    }

    fn open(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        // SAFETY: the parent created the file with the full size before starting us.
        let map = unsafe { memmap2::MmapOptions::new().map_mut(&file)? };
        if map.len() < std::mem::size_of::<Header>() {
            return Err(std::io::Error::other("shared memory too small"));
        }
        let mut shm = Self {
            map,
            max_block: 0,
            path: None,
        };
        if shm.header().magic != MAGIC {
            return Err(std::io::Error::other("not a Musiton sandbox file"));
        }
        shm.max_block = shm.header().max_block as usize;
        if shm.map.len() < shm_size(shm.max_block) {
            return Err(std::io::Error::other("shared memory too small"));
        }
        Ok(shm)
    }

    fn header(&self) -> &Header {
        // SAFETY: the mapping is page-aligned and at least a `Header` long (checked).
        unsafe { &*(self.map.as_ptr() as *const Header) }
    }

    fn header_mut(&mut self) -> &mut Header {
        // SAFETY: as above; `&mut self` gives exclusive access on this side.
        unsafe { &mut *(self.map.as_mut_ptr() as *mut Header) }
    }

    /// Channel `ch` of the input (`out == false`) or output buffers.
    fn channel(&mut self, out: bool, ch: usize) -> &mut [f32] {
        let n = self.max_block;
        let index = if out { IN_CHANNELS + ch } else { ch };
        let start = audio_offset() + index * n * std::mem::size_of::<f32>();
        // SAFETY: inside the mapping (size checked), aligned to 4 bytes.
        unsafe { std::slice::from_raw_parts_mut(self.map.as_mut_ptr().add(start) as *mut f32, n) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A control message to the child.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Values,
    SetParam { id: u32, value: f64 },
    State,
    SetState { hex: String },
    OpenEditor { title: String },
    Quit,
}

/// A message from the child: a reply, or something that happened.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Message {
    Ready {
        inputs: usize,
        latency: u32,
        params: Vec<WireParam>,
        has_editor: bool,
    },
    Failed {
        error: String,
    },
    Values {
        values: Vec<(u32, f64, String)>,
    },
    State {
        hex: String,
    },
    Done {
        ok: bool,
        error: Option<String>,
    },
    Edit {
        id: u32,
        value: f64,
    },
    Restart {
        latency: u32,
    },
}

#[derive(Serialize, Deserialize)]
struct WireParam {
    id: u32,
    name: String,
    unit: String,
    default: f64,
    steps: i32,
    automatable: bool,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn send<T: Serialize>(out: &mut impl Write, msg: &T) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(&body)?;
    out.flush()
}

fn receive<T: for<'de> Deserialize<'de>>(input: &mut impl Read) -> Option<T> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Starts plugin `id` of the file at `path` in a sandbox process (`exe`).
pub(crate) fn load(
    exe: &Path,
    format: PluginFormat,
    path: &Path,
    id: &str,
    sample_rate: f64,
    max_block: usize,
) -> Result<Plugin, PluginError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let failed = |e: String| PluginError::Failed(format!("sandbox: {e}"));
    let shm_path = std::env::temp_dir().join(format!(
        "musiton-sandbox-{}-{}.shm",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let shm = Shm::create(&shm_path, max_block).map_err(|e| failed(e.to_string()))?;
    let mut cmd = Command::new(exe);
    cmd.arg(HOST_ARG)
        .arg(&shm_path)
        .arg(format.name())
        .arg(path)
        .arg(id)
        .arg(sample_rate.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| failed(e.to_string()))?;
    let stdin = child.stdin.take().ok_or_else(|| failed("no pipe".into()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| failed("no pipe".into()))?;
    // A reader thread sorts the child's messages: replies and events.
    let (reply_tx, reply_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("sandbox-reader".into())
        .spawn(move || {
            while let Some(msg) = receive::<Message>(&mut stdout) {
                let to_events = matches!(msg, Message::Edit { .. } | Message::Restart { .. });
                let sent = if to_events {
                    event_tx.send(msg).is_ok()
                } else {
                    reply_tx.send(msg).is_ok()
                };
                if !sent {
                    break;
                }
            }
        })
        .map_err(|e| failed(e.to_string()))?;
    let first = reply_rx.recv_timeout(Duration::from_secs(30));
    let (inputs, latency, params, has_editor) = match first {
        Ok(Message::Ready {
            inputs,
            latency,
            params,
            has_editor,
        }) => (inputs, latency, params, has_editor),
        Ok(Message::Failed { error }) => {
            let _ = child.wait();
            return Err(PluginError::Failed(error));
        }
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed("the plugin process did not start".into()));
        }
    };
    let params: Vec<PluginParam> = params
        .into_iter()
        .map(|p| PluginParam {
            id: p.id,
            name: p.name,
            unit: p.unit,
            default: p.default,
            steps: p.steps,
            automatable: p.automatable,
        })
        .collect();
    let processor = SandboxProcessor::new(shm, max_block, sample_rate);
    let controller = SandboxController {
        child: RefCell::new(child),
        stdin: RefCell::new(stdin),
        replies: reply_rx,
        events: event_rx,
        params,
        values: RefCell::new(HashMap::new()),
        refreshed: RefCell::new(Instant::now() - VALUE_REFRESH),
        latency: std::cell::Cell::new(latency + max_block as u32),
        block: max_block as u32,
        has_editor,
    };
    Ok(Plugin::new(
        format,
        Box::new(processor),
        Box::new(controller),
        (inputs, OUT_CHANNELS),
    ))
}

/// The audio-thread half: hands blocks to the child, one block behind.
struct SandboxProcessor {
    shm: Shm,
    /// Output waiting to be played: `max_block` frames behind the input.
    fifo: [Vec<f32>; OUT_CHANNELS],
    /// Fill level and read position of the FIFO.
    len: usize,
    head: usize,
    /// Number of the block handed over last, if its result is still expected.
    awaiting: Option<u64>,
    prev_frames: usize,
    /// Events of blocks the child could not take yet.
    pending: Vec<WireEvent>,
    sample_rate: f64,
}

impl SandboxProcessor {
    fn new(shm: Shm, max_block: usize, sample_rate: f64) -> Self {
        let cap = 4 * max_block;
        let mut fifo = [vec![0.0; cap], vec![0.0; cap]];
        for f in &mut fifo {
            f.shrink_to_fit();
        }
        Self {
            shm,
            fifo,
            // Starts with one block of silence: that is the sandbox's latency.
            len: max_block,
            head: 0,
            awaiting: None,
            prev_frames: 0,
            pending: Vec::with_capacity(MAX_EVENTS),
            sample_rate,
        }
    }

    fn push_fifo(&mut self, frames: usize, from_child: bool) {
        let cap = self.fifo[0].len();
        for i in 0..frames {
            if self.len >= cap {
                break;
            }
            let at = (self.head + self.len) % cap;
            for ch in 0..OUT_CHANNELS {
                let v = if from_child {
                    self.shm.channel(true, ch).get(i).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
                if let Some(s) = self.fifo[ch].get_mut(at) {
                    *s = v;
                }
            }
            self.len += 1;
        }
    }

    fn stash(&mut self, e: WireEvent) {
        if self.pending.len() < self.pending.capacity() {
            self.pending.push(e);
        }
    }
}

impl Processor for SandboxProcessor {
    fn process(
        &mut self,
        ctx: &ProcessContext,
        io: &mut AudioIo,
        events: &[Event],
        params: &[ParamChange],
    ) {
        let frames = ctx.frames.min(self.shm.max_block);
        let ready = {
            let h = self.shm.header();
            h.done.load(Ordering::Acquire) == h.request.load(Ordering::Relaxed)
        };
        // 1. The previous block's result (or silence if the child was late).
        let got = ready && self.awaiting.is_some();
        self.push_fifo(self.prev_frames, got);
        self.awaiting = None;

        // 2. This block's input and events.
        for &(id, offset, value) in params {
            self.stash(WireEvent {
                offset: offset.max(0) as u32,
                kind: 4,
                a: id,
                b: 0,
                value,
            });
        }
        for e in events {
            let (kind, a, b, value) = match e.kind {
                EventKind::NoteOn { ch, key, vel } => {
                    (0, u32::from(ch), u32::from(key), f64::from(vel))
                }
                EventKind::NoteOff { ch, key } => (1, u32::from(ch), u32::from(key), 0.0),
                EventKind::Cc { ch, cc, val } => (2, u32::from(ch), u32::from(cc), f64::from(val)),
                EventKind::AllNotesOff => (3, 0, 0, 0.0),
                EventKind::Param { .. } => continue,
            };
            self.stash(WireEvent {
                offset: e.offset,
                kind,
                a,
                b,
                value,
            });
        }
        if ready {
            for ch in 0..IN_CHANNELS {
                let src = io.input(ch);
                let dst = self.shm.channel(false, ch);
                for (i, d) in dst.iter_mut().take(frames).enumerate() {
                    *d = src.get(i).copied().unwrap_or(0.0);
                }
            }
            let tick = ctx
                .tempo
                .sample_to_tick(ctx.sample_pos as f64, self.sample_rate);
            let n = self.pending.len().min(MAX_EVENTS);
            let h = self.shm.header_mut();
            for (dst, src) in h.events.iter_mut().zip(&self.pending) {
                *dst = *src;
                // Events held back from earlier blocks happen at the start of this one.
                dst.offset = dst.offset.min(frames.saturating_sub(1) as u32);
            }
            h.n_events = n as u64;
            h.frames = frames as u64;
            h.playing = u64::from(ctx.playing);
            h.sample_pos = ctx.sample_pos;
            h.tick = tick;
            h.bpm = ctx.tempo.bpm_at(tick);
            h.sig_num = u64::from(ctx.time_sig.num);
            h.sig_den = u64::from(ctx.time_sig.den);
            let next = h.request.load(Ordering::Relaxed) + 1;
            h.request.store(next, Ordering::Release);
            self.awaiting = Some(next);
            self.pending.clear();
        }
        self.prev_frames = frames;

        // 3. Play from the FIFO.
        let cap = self.fifo[0].len();
        for i in 0..frames {
            for ch in 0..io.num_outputs().min(OUT_CHANNELS) {
                let v = if self.len > 0 {
                    self.fifo[ch].get(self.head).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = v;
                }
            }
            if self.len > 0 {
                self.head = (self.head + 1) % cap;
                self.len -= 1;
            }
        }
    }

    fn reset(&mut self) {
        self.stash(WireEvent {
            kind: 3,
            ..WireEvent::default()
        });
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

/// The UI-thread half: talks to the child.
struct SandboxController {
    child: RefCell<Child>,
    stdin: RefCell<ChildStdin>,
    replies: mpsc::Receiver<Message>,
    events: mpsc::Receiver<Message>,
    params: Vec<PluginParam>,
    /// Latest values and texts from the child.
    values: RefCell<HashMap<u32, (f64, String)>>,
    refreshed: RefCell<Instant>,
    latency: std::cell::Cell<u32>,
    block: u32,
    has_editor: bool,
}

impl SandboxController {
    fn tell(&self, req: &Request) -> bool {
        send(&mut *self.stdin.borrow_mut(), req).is_ok()
    }

    fn ask(&self, req: &Request) -> Result<Message, PluginError> {
        if !self.tell(req) {
            return Err(PluginError::Failed(self.failure()));
        }
        self.replies
            .recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| PluginError::Failed(self.failure()))
    }

    fn failure(&self) -> String {
        match self.child.borrow_mut().try_wait() {
            Ok(Some(status)) => format!("the plugin process ended ({status})"),
            _ => "the plugin process does not answer".into(),
        }
    }

    fn refresh(&self) {
        if self.refreshed.borrow().elapsed() < VALUE_REFRESH {
            return;
        }
        *self.refreshed.borrow_mut() = Instant::now();
        if let Ok(Message::Values { values }) = self.ask(&Request::Values) {
            *self.values.borrow_mut() = values.into_iter().map(|(id, v, t)| (id, (v, t))).collect();
        }
    }
}

impl Controller for SandboxController {
    fn params(&self) -> &[PluginParam] {
        &self.params
    }

    fn param_value(&self, id: u32) -> f64 {
        self.values.borrow().get(&id).map_or(0.0, |(v, _)| *v)
    }

    fn set_param(&self, id: u32, value: f64) {
        self.tell(&Request::SetParam { id, value });
        if let Some(entry) = self.values.borrow_mut().get_mut(&id) {
            entry.0 = value;
        }
    }

    fn param_text(&self, id: u32, value: f64) -> String {
        self.values
            .borrow()
            .get(&id)
            .filter(|(v, _)| (*v - value).abs() < 1e-9)
            .map_or_else(|| format!("{value:.2}"), |(_, t)| t.clone())
    }

    fn take_edits(&self) -> Vec<Edit> {
        let mut out = Vec::new();
        while let Ok(msg) = self.events.try_recv() {
            match msg {
                Message::Edit { id, value } => {
                    if let Some(entry) = self.values.borrow_mut().get_mut(&id) {
                        entry.0 = value;
                    }
                    out.push(Edit::Perform(id, value));
                }
                Message::Restart { latency } => {
                    self.latency.set(latency + self.block);
                    out.push(Edit::Restart);
                }
                _ => {}
            }
        }
        self.refresh();
        out
    }

    fn state(&self) -> Result<Vec<u8>, PluginError> {
        match self.ask(&Request::State)? {
            Message::State { hex } => unhex(&hex)
                .ok_or_else(|| PluginError::Failed("bad state from the plugin process".into())),
            Message::Done { error, .. } => Err(PluginError::Failed(error.unwrap_or_default())),
            _ => Err(PluginError::Failed(
                "unexpected answer from the plugin process".into(),
            )),
        }
    }

    fn set_state(&self, state: &[u8]) -> Result<(), PluginError> {
        match self.ask(&Request::SetState { hex: hex(state) })? {
            Message::Done { ok: true, .. } => Ok(()),
            Message::Done { error, .. } => Err(PluginError::Failed(error.unwrap_or_default())),
            _ => Err(PluginError::Failed(
                "unexpected answer from the plugin process".into(),
            )),
        }
    }

    fn latency(&self) -> u32 {
        self.latency.get()
    }

    fn shutdown(&self, processor: Box<dyn std::any::Any>) {
        self.tell(&Request::Quit);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut child = self.child.borrow_mut();
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Unmaps and deletes the shared memory file.
        drop(processor);
    }

    fn has_editor(&self) -> bool {
        self.has_editor
    }

    fn external_editor(&self, title: &str) -> Option<Result<(), PluginError>> {
        Some(
            match self.ask(&Request::OpenEditor {
                title: title.to_owned(),
            }) {
                Ok(Message::Done { ok: true, .. }) => Ok(()),
                Ok(Message::Done { error, .. }) => {
                    Err(PluginError::Failed(error.unwrap_or_default()))
                }
                Ok(_) => Err(PluginError::Failed(
                    "unexpected answer from the plugin process".into(),
                )),
                Err(e) => Err(e),
            },
        )
    }

    fn attach_editor(
        &self,
        _parent: crate::window::Handle,
    ) -> Result<((i32, i32), bool), PluginError> {
        Err(PluginError::Failed(
            "the editor runs in the plugin process".into(),
        ))
    }

    fn detach_editor(&self) {}

    fn editor_size_request(&self) -> Option<(i32, i32)> {
        None
    }

    fn editor_resized(&self, _width: i32, _height: i32) {}

    fn failure_text(&self) -> Option<String> {
        matches!(self.child.borrow_mut().try_wait(), Ok(Some(_))).then(|| self.failure())
    }
}

/// Sandbox host mode: `args` are what follows `--plugin-host` on the command line.
/// Loads the plugin, then serves the parent until it says goodbye. Returns the exit code.
pub fn run_host(args: &[String]) -> i32 {
    let mut out = std::io::stdout().lock();
    let fail = |out: &mut std::io::StdoutLock, e: String| {
        let _ = send(out, &Message::Failed { error: e });
        3
    };
    let [shm_path, format, path, id, rate, ..] = args else {
        return fail(&mut out, "bad arguments".into());
    };
    let format = match format.as_str() {
        "VST3" => PluginFormat::Vst3,
        "CLAP" => PluginFormat::Clap,
        other => return fail(&mut out, format!("unknown format {other}")),
    };
    let rate: f64 = rate.parse().unwrap_or(48_000.0);
    let shm = match Shm::open(Path::new(shm_path)) {
        Ok(s) => s,
        Err(e) => return fail(&mut out, e.to_string()),
    };
    let max_block = shm.max_block;
    let mut plugin = match Plugin::load(format, Path::new(path), id, rate, max_block) {
        Ok(p) => p,
        Err(e) => return fail(&mut out, e.to_string()),
    };
    let params: Vec<WireParam> = plugin
        .params()
        .iter()
        .map(|p| WireParam {
            id: p.id,
            name: p.name.clone(),
            unit: p.unit.clone(),
            default: p.default,
            steps: p.steps,
            automatable: p.automatable,
        })
        .collect();
    let mut node = plugin.node();
    let inputs = node.num_inputs();
    let ready = Message::Ready {
        inputs,
        latency: plugin.latency(),
        params,
        has_editor: plugin.has_editor(),
    };
    if send(&mut out, &ready).is_err() {
        return 4;
    }

    // The audio thread of the sandbox.
    let quit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let audio = {
        let quit = quit.clone();
        std::thread::Builder::new()
            .name("sandbox-audio".into())
            .spawn(move || serve_audio(shm, node.as_mut(), rate, &quit))
    };

    // Control: requests arrive on a reader thread; this (main) thread owns the plugin's
    // controller and editor.
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("sandbox-control".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            while let Some(req) = receive::<Request>(&mut stdin) {
                if tx.send(req).is_err() {
                    break;
                }
            }
        });
    loop {
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Request::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(req) => {
                if let Some(reply) = answer(&mut plugin, req) {
                    if send(&mut out, &reply).is_err() {
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        for e in plugin.take_edits() {
            let msg = match e {
                Edit::Perform(id, value) => Message::Edit { id, value },
                Edit::Restart => Message::Restart {
                    latency: plugin.latency(),
                },
                Edit::Begin(_) | Edit::End(_) => continue,
            };
            let _ = send(&mut out, &msg);
        }
        plugin.poll_editor();
        crate::window::pump_messages();
    }
    quit.store(true, Ordering::Relaxed);
    if let Ok(handle) = audio {
        let _ = handle.join();
    }
    0
}

fn answer(plugin: &mut Plugin, req: Request) -> Option<Message> {
    let done = |r: Result<(), PluginError>| Message::Done {
        ok: r.is_ok(),
        error: r.err().map(|e| e.to_string()),
    };
    Some(match req {
        Request::Values => Message::Values {
            values: plugin
                .params()
                .iter()
                .map(|p| {
                    let v = plugin.param_value(p.id);
                    (p.id, v, plugin.param_text(p.id, v))
                })
                .collect(),
        },
        Request::SetParam { id, value } => {
            plugin.set_param(id, value);
            plugin.queue_param(id, value);
            return None;
        }
        Request::State => match plugin.state() {
            Ok(s) => Message::State { hex: hex(&s) },
            Err(e) => done(Err(e)),
        },
        Request::SetState { hex } => done(match unhex(&hex) {
            Some(bytes) => plugin.set_state(&bytes),
            None => Err(PluginError::Failed("bad state".into())),
        }),
        Request::OpenEditor { title } => done(plugin.open_editor(&title)),
        Request::Quit => return None,
    })
}

/// The sandbox's audio loop: waits for blocks from the parent and processes them.
fn serve_audio(
    mut shm: Shm,
    node: &mut dyn AudioNode,
    rate: f64,
    quit: &std::sync::atomic::AtomicBool,
) {
    let max = shm.max_block;
    let mut ins = vec![vec![0.0f32; max]; node.num_inputs()];
    let mut outs = vec![vec![0.0f32; max]; node.num_outputs()];
    let mut events: Vec<Event> = Vec::with_capacity(MAX_EVENTS);
    let mut tempo = TempoMap::constant(120.0);
    let mut last = shm.header().done.load(Ordering::Acquire);
    let mut idle = Instant::now();
    while !quit.load(Ordering::Relaxed) {
        let request = shm.header().request.load(Ordering::Acquire);
        if request == last {
            // Short spins first (low latency), then let the CPU rest.
            if idle.elapsed() < Duration::from_millis(2) {
                std::hint::spin_loop();
            } else if idle.elapsed() < Duration::from_millis(50) {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
            continue;
        }
        idle = Instant::now();
        let h = shm.header();
        let frames = (h.frames as usize).min(max);
        if (h.bpm - tempo.bpm_at(0.0)).abs() > 1e-9 && h.bpm > 0.0 {
            tempo = TempoMap::constant(h.bpm);
        }
        // Under the constant tempo, this sample position falls on the parent's tick.
        let sample_pos = tempo.tick_to_sample(h.tick, rate).max(0.0) as u64;
        let sig = TimeSig::new(h.sig_num.clamp(1, 32) as u8, h.sig_den.clamp(1, 32) as u8);
        let playing = h.playing != 0;
        events.clear();
        for w in h.events.iter().take((h.n_events as usize).min(MAX_EVENTS)) {
            let kind = match w.kind {
                0 => EventKind::NoteOn {
                    ch: w.a as u8,
                    key: w.b as u8,
                    vel: w.value as f32,
                },
                1 => EventKind::NoteOff {
                    ch: w.a as u8,
                    key: w.b as u8,
                },
                2 => EventKind::Cc {
                    ch: w.a as u8,
                    cc: w.b as u8,
                    val: w.value as f32,
                },
                3 => EventKind::AllNotesOff,
                4 => EventKind::Param {
                    id: musiton_core::ids::ParamId(u64::from(w.a)),
                    val: w.value as f32,
                },
                _ => continue,
            };
            events.push(Event {
                offset: w.offset,
                kind,
            });
        }
        for (ch, buf) in ins.iter_mut().enumerate() {
            let src = shm.channel(false, ch.min(IN_CHANNELS - 1));
            buf[..frames].copy_from_slice(&src[..frames]);
        }
        let ctx = ProcessContext {
            sample_rate: rate as f32,
            frames,
            playing,
            sample_pos,
            tempo: &tempo,
            time_sig: sig,
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
            live_midi: &[],
        };
        node.process(&ctx, &mut AudioIo::new(&ins, &mut outs, frames), &events);
        for (ch, buf) in outs.iter().enumerate().take(OUT_CHANNELS) {
            shm.channel(true, ch)[..frames].copy_from_slice(&buf[..frames]);
        }
        last = request;
        shm.header().done.store(request, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_bytes_survive_the_pipe() {
        let mut buf = Vec::new();
        send(
            &mut buf,
            &Request::SetState {
                hex: hex(&[0, 1, 254, 255]),
            },
        )
        .unwrap();
        let back: Request = receive(&mut buf.as_slice()).unwrap();
        let Request::SetState { hex } = back else {
            panic!("wrong message");
        };
        assert_eq!(unhex(&hex), Some(vec![0, 1, 254, 255]));
        assert_eq!(unhex("zz"), None);
    }
}
