//! A loaded plugin, independent of its format: the UI-side handle ([`Plugin`]) and the
//! engine node that processes it ([`PluginNode`]).
//!
//! One plugin instance outlives graph rebuilds: every rebuild gets a new, light
//! [`PluginNode`] for the same instance. Of the nodes of one instance only the newest
//! processes (it claims the instance on its first block), so during the short cross-fade
//! between two graphs the instance is never processed twice per block.

use std::cell::UnsafeCell;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use musiton_core::ids::ParamId;
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

pub use crate::vst3_host::Edit;
use crate::{PluginError, PluginFormat};

/// One parameter of a plugin. Values are normalized (0..1).
#[derive(Debug, Clone, PartialEq)]
pub struct PluginParam {
    /// The plugin's id for it.
    pub id: u32,
    /// Display name.
    pub name: String,
    /// Unit ("dB", "%", …).
    pub unit: String,
    /// Default value (normalized).
    pub default: f64,
    /// Number of steps for stepped parameters (0 = continuous).
    pub steps: i32,
    /// Whether it may be automated.
    pub automatable: bool,
}

/// A parameter change for one block: (plugin id, frame offset, normalized value).
pub(crate) type ParamChange = (u32, i32, f64);

/// The audio-thread half of a plugin.
pub(crate) trait Processor: Send {
    /// Renders one block. `params` are changes to apply in this block.
    fn process(
        &mut self,
        ctx: &ProcessContext,
        io: &mut AudioIo,
        events: &[Event],
        params: &[ParamChange],
    );
    /// Stops every sounding note.
    fn reset(&mut self);
    /// For handing the processor back to its controller at shutdown.
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any>;
}

/// The UI-thread half of a plugin.
pub(crate) trait Controller {
    fn params(&self) -> &[PluginParam];
    fn param_value(&self, id: u32) -> f64;
    fn set_param(&self, id: u32, value: f64);
    fn param_text(&self, id: u32, value: f64) -> String;
    fn take_edits(&self) -> Vec<Edit>;
    fn state(&self) -> Result<Vec<u8>, PluginError>;
    fn set_state(&self, state: &[u8]) -> Result<(), PluginError>;
    fn latency(&self) -> u32;
    /// Shuts the plugin down; `processor` is the processing half (UI thread).
    fn shutdown(&self, processor: Box<dyn std::any::Any>);
    /// Whether the plugin has its own editor for this system's windows.
    fn has_editor(&self) -> bool;
    /// Puts the editor into the native window `parent`; returns its size and whether it
    /// can be resized.
    fn attach_editor(
        &self,
        parent: crate::window::Handle,
    ) -> Result<((i32, i32), bool), PluginError>;
    /// Takes the editor out of its window.
    fn detach_editor(&self);
    /// A size the editor asked for since the last call.
    fn editor_size_request(&self) -> Option<(i32, i32)>;
    /// The user resized the editor's window to this inner size.
    fn editor_resized(&self, width: i32, height: i32);
    /// For plugins whose editor lives elsewhere (the sandbox): opens it there.
    fn external_editor(&self, _title: &str) -> Option<Result<(), PluginError>> {
        None
    }
    /// Why the plugin stopped working, if it did (e.g. its sandbox process ended).
    fn failure_text(&self) -> Option<String> {
        None
    }
    /// Ends the plugin's own process abruptly, as a crash would (sandbox only; tests).
    fn kill_process(&self) {}
}

/// What the plugin's nodes share.
struct Shared {
    processor: UnsafeCell<Option<Box<dyn Processor>>>,
    /// Ticket of the node that processes now.
    owner: AtomicU64,
    next_ticket: AtomicU64,
    /// Edits made in the plugin's own editor, on their way to the processor.
    edits: UnsafeCell<rtrb::Consumer<(u32, f64)>>,
    latency: AtomicU32,
    /// Set while a node is inside `process` (guards against two threads at once).
    busy: AtomicBool,
    inputs: usize,
    outputs: usize,
}

// SAFETY: the processor and the edit consumer are only touched inside `process`/`reset` of
// a node that owns the instance and holds the `busy` flag, so never by two threads at
// once. The UI side never touches them.
unsafe impl Sync for Shared {}
// SAFETY: as above; the processor itself is `Send`.
unsafe impl Send for Shared {}

/// A loaded plugin (UI thread). Create engine nodes for it with [`Plugin::node`].
///
/// Drop it on the UI thread after every node of it is gone (plugins expect to be shut
/// down on the thread that created them).
pub struct Plugin {
    // Both are dropped by hand in `Drop`, under the lifecycle lock.
    shared: ManuallyDrop<Arc<Shared>>,
    controller: ManuallyDrop<Box<dyn Controller>>,
    edits_tx: rtrb::Producer<(u32, f64)>,
    format: PluginFormat,
    /// The open editor window.
    editor: Option<crate::window::Window>,
}

impl Plugin {
    pub(crate) fn new(
        format: PluginFormat,
        processor: Box<dyn Processor>,
        controller: Box<dyn Controller>,
        (inputs, outputs): (usize, usize),
    ) -> Self {
        let (tx, rx) = rtrb::RingBuffer::new(1024);
        let latency = controller.latency();
        Self {
            shared: ManuallyDrop::new(Arc::new(Shared {
                processor: UnsafeCell::new(Some(processor)),
                owner: AtomicU64::new(0),
                next_ticket: AtomicU64::new(1),
                edits: UnsafeCell::new(rx),
                latency: AtomicU32::new(latency),
                busy: AtomicBool::new(false),
                inputs,
                outputs,
            })),
            controller: ManuallyDrop::new(controller),
            edits_tx: tx,
            format,
            editor: None,
        }
    }

    /// Loads plugin `id` from the file at `path` for `sample_rate` and blocks of at most
    /// `max_block` frames.
    pub fn load(
        format: PluginFormat,
        path: &std::path::Path,
        id: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Self, PluginError> {
        let _one_at_a_time = crate::lifecycle_lock();
        match format {
            PluginFormat::Vst3 => crate::vst3_plugin::load(path, id, sample_rate, max_block),
            PluginFormat::Clap => crate::clap::instantiate(path, id, sample_rate, max_block),
        }
    }

    /// Its format.
    pub fn format(&self) -> PluginFormat {
        self.format
    }

    /// A new engine node playing this instance (it takes over from older nodes).
    pub fn node(&self) -> Box<dyn AudioNode> {
        self.source().node()
    }

    /// Makes engine nodes of this instance on any thread (e.g. for an export).
    pub fn source(&self) -> NodeSource {
        NodeSource(Arc::clone(&self.shared))
    }

    /// Whether engine nodes of it still exist (in a graph or waiting to be freed).
    pub fn in_use(&self) -> bool {
        Arc::strong_count(&self.shared) > 1
    }

    /// Its parameters.
    pub fn params(&self) -> &[PluginParam] {
        self.controller.params()
    }

    /// Current value of parameter `id` (normalized).
    pub fn param_value(&self, id: u32) -> f64 {
        self.controller.param_value(id)
    }

    /// Shows a new value in the plugin's controller (the engine gets it separately).
    pub fn set_param(&self, id: u32, value: f64) {
        self.controller.set_param(id, value);
    }

    /// The plugin's text for a value of parameter `id` ("-6.0 dB").
    pub fn param_text(&self, id: u32, value: f64) -> String {
        self.controller.param_text(id, value)
    }

    /// Edits made in the plugin's own editor since the last call. Parameter changes are
    /// also passed on to the audio processing.
    pub fn take_edits(&mut self) -> Vec<Edit> {
        let edits = self.controller.take_edits();
        for e in &edits {
            match e {
                Edit::Perform(id, v) => {
                    let _ = self.edits_tx.push((*id, *v));
                }
                Edit::Restart => {
                    self.shared
                        .latency
                        .store(self.controller.latency(), Ordering::Relaxed);
                }
                Edit::Begin(_) | Edit::End(_) => {}
            }
        }
        edits
    }

    /// The plugin's complete state (to save with the project).
    pub fn state(&self) -> Result<Vec<u8>, PluginError> {
        self.controller.state()
    }

    /// Restores a state from [`Plugin::state`].
    pub fn set_state(&self, state: &[u8]) -> Result<(), PluginError> {
        self.controller.set_state(state)
    }

    /// Whether the plugin's own editor can be shown (Windows only).
    pub fn has_editor(&self) -> bool {
        crate::window::SUPPORTED && self.controller.has_editor()
    }

    /// Whether its editor window is open.
    pub fn editor_open(&self) -> bool {
        self.editor.is_some()
    }

    /// Opens the plugin's own editor in a window titled `title` (or brings it to the
    /// front). UI thread only.
    pub fn open_editor(&mut self, title: &str) -> Result<(), PluginError> {
        if let Some(result) = self.controller.external_editor(title) {
            return result;
        }
        if let Some(w) = &self.editor {
            w.show();
            return Ok(());
        }
        if !self.has_editor() {
            return Err(PluginError::Failed("this plugin has no editor here".into()));
        }
        let window = crate::window::Window::new(title, 640, 480, true)
            .ok_or_else(|| PluginError::Failed("the editor window could not be created".into()))?;
        let ((w, h), resizable) = self.controller.attach_editor(window.handle())?;
        // Recreate fixed-size editors without a sizing border.
        let window = if resizable {
            window
        } else {
            // The plugin leaves the window before it is destroyed (end of this block).
            let _old = window;
            self.controller.detach_editor();
            let fixed = crate::window::Window::new(title, w, h, false).ok_or_else(|| {
                PluginError::Failed("the editor window could not be created".into())
            })?;
            self.controller.attach_editor(fixed.handle())?;
            fixed
        };
        window.set_client_size(w, h);
        window.show();
        self.editor = Some(window);
        Ok(())
    }

    /// Closes the editor window, if open.
    pub fn close_editor(&mut self) {
        // The plugin leaves the window before it is destroyed (end of this block).
        if let Some(_window) = self.editor.take() {
            self.controller.detach_editor();
        }
    }

    /// Keeps the editor window in step with the plugin: closes it when the user closed
    /// it, and passes size changes both ways. Call once per frame (UI thread).
    pub fn poll_editor(&mut self) {
        let Some(w) = &self.editor else {
            return;
        };
        if w.closed() {
            self.close_editor();
            return;
        }
        if let Some((width, height)) = self.controller.editor_size_request() {
            w.set_client_size(width, height);
            // Our own resize shows up as a user resize; it is already applied.
            let _ = w.take_resized();
        } else if let Some((width, height)) = w.take_resized() {
            self.controller.editor_resized(width, height);
        }
    }

    /// Loads plugin `id` like [`Plugin::load`], but in a separate process started from
    /// the executable `exe` (which must handle [`crate::HOST_ARG`]).
    pub fn load_sandboxed(
        exe: &std::path::Path,
        format: PluginFormat,
        path: &std::path::Path,
        id: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Self, PluginError> {
        crate::sandbox::load(exe, format, path, id, sample_rate, max_block)
    }

    /// Why the plugin stopped working, if it did (a sandboxed plugin that crashed).
    pub fn failure(&self) -> Option<String> {
        self.controller.failure_text()
    }

    /// Kills a sandboxed plugin's process the way a crash would. For tests.
    #[doc(hidden)]
    pub fn simulate_crash(&self) {
        self.controller.kill_process();
    }

    /// Hands a parameter value to the processing directly (like an edit in the
    /// plugin's own editor).
    pub fn queue_param(&mut self, id: u32, value: f64) {
        let _ = self.edits_tx.push((id, value));
    }

    /// Processing delay in frames.
    pub fn latency(&self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.close_editor();
        let _one_at_a_time = crate::lifecycle_lock();
        // With no node left, the processing half comes back for an orderly shutdown.
        if let Some(shared) = Arc::get_mut(&mut self.shared) {
            if let Some(p) = shared.processor.get_mut().take() {
                self.controller.shutdown(p.into_any());
            }
        }
        // SAFETY: both fields are dropped exactly once, here, and never used afterwards.
        unsafe {
            ManuallyDrop::drop(&mut self.shared);
            ManuallyDrop::drop(&mut self.controller);
        }
    }
}

/// Creates engine nodes for a plugin instance; can be sent to other threads.
#[derive(Clone)]
pub struct NodeSource(Arc<Shared>);

impl NodeSource {
    /// A node that takes over the instance from older nodes.
    pub fn node(&self) -> Box<dyn AudioNode> {
        Box::new(PluginNode {
            shared: self.0.clone(),
            ticket: self.0.next_ticket.fetch_add(1, Ordering::Relaxed),
            pending: Vec::with_capacity(MAX_PARAM_CHANGES),
            changes: Vec::with_capacity(MAX_PARAM_CHANGES),
        })
    }
}

/// Parameter changes a node can take per block.
const MAX_PARAM_CHANGES: usize = 512;

/// Engine node of a [`Plugin`].
pub struct PluginNode {
    shared: Arc<Shared>,
    ticket: u64,
    /// Changes from `set_param` waiting for the next block.
    pending: Vec<ParamChange>,
    /// The changes of the current block.
    changes: Vec<ParamChange>,
}

impl PluginNode {
    fn silence(io: &mut AudioIo) {
        for ch in io.outputs() {
            ch.fill(0.0);
        }
    }
}

impl AudioNode for PluginNode {
    fn num_inputs(&self) -> usize {
        self.shared.inputs
    }

    fn num_outputs(&self) -> usize {
        self.shared.outputs
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        // The newest node owns the instance; older ones (still cross-fading) go silent.
        let owner = self.shared.owner.load(Ordering::Acquire);
        if owner > self.ticket {
            Self::silence(io);
            return;
        }
        if owner < self.ticket {
            self.shared.owner.store(self.ticket, Ordering::Release);
        }
        // Only one thread may be inside the plugin at a time; never wait for it.
        if self
            .shared
            .busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            Self::silence(io);
            return;
        }
        self.changes.clear();
        self.changes.append(&mut self.pending);
        // SAFETY: only the owning node touches the consumer, on the audio thread.
        let edits = unsafe { &mut *self.shared.edits.get() };
        while let Ok((id, v)) = edits.pop() {
            if self.changes.len() < self.changes.capacity() {
                self.changes.push((id, 0, v));
            }
        }
        for e in events {
            if let EventKind::Param { id, val } = e.kind {
                if self.changes.len() < self.changes.capacity() {
                    self.changes
                        .push((id.0 as u32, e.offset as i32, f64::from(val)));
                }
            }
        }
        // SAFETY: see `Shared`: only the owning node, only on the audio thread.
        match unsafe { &mut *self.shared.processor.get() } {
            Some(processor) => processor.process(ctx, io, events, &self.changes),
            None => Self::silence(io),
        }
        self.shared.busy.store(false, Ordering::Release);
    }

    fn reset(&mut self) {
        let owner = self.shared.owner.load(Ordering::Acquire) == self.ticket;
        if owner
            && self
                .shared
                .busy
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        {
            // SAFETY: as in `process`: owner and holder of the busy flag.
            if let Some(p) = unsafe { &mut *self.shared.processor.get() } {
                p.reset();
            }
            self.shared.busy.store(false, Ordering::Release);
        }
    }

    fn latency(&self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        if self.pending.len() < self.pending.capacity() {
            self.pending.push((id.0 as u32, 0, f64::from(value)));
        }
    }
}
