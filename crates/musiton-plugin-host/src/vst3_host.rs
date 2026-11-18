//! The objects Musiton hands to VST3 plugins: host application, component handler,
//! event and parameter lists, a memory stream and messages.

// The bindings' char type differs between platforms (casts are no-ops on some).
#![allow(clippy::unnecessary_cast)]

use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::sync::Mutex;

use vst3::Steinberg::Vst::{
    Event, IAttributeList, IAttributeListTrait, IComponentHandler, IComponentHandlerTrait,
    IEventList, IEventListTrait, IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait,
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, ParamID,
    ParamValue, String128, TChar,
};
use vst3::Steinberg::{
    int32, int64, kInvalidArgument, kNoInterface, kResultFalse, kResultOk, tresult, uint32,
    FIDString, IBStream, IBStreamTrait, TUID,
};
use vst3::{Class, ComWrapper, Interface};

/// Writes `text` into a UTF-16 string buffer (NUL-terminated, cut to fit).
pub(crate) fn write_wide(text: &str, dst: &mut [TChar]) {
    let mut n = 0;
    for (s, d) in text.encode_utf16().zip(dst.iter_mut()) {
        *d = s as TChar;
        n += 1;
    }
    let end = n.min(dst.len().saturating_sub(1));
    if let Some(d) = dst.get_mut(end) {
        *d = 0;
    }
}

/// Reads a NUL-terminated UTF-16 string buffer.
pub(crate) fn read_wide(src: &[TChar]) -> String {
    let len = src.iter().position(|c| *c == 0).unwrap_or(src.len());
    String::from_utf16_lossy(&src[..len].iter().map(|c| *c as u16).collect::<Vec<_>>())
}

/// Whether a class/interface id given by the plugin is the interface `I`.
fn is<I: Interface>(id: &TUID) -> bool {
    id.iter().zip(I::IID.iter()).all(|(a, b)| *a as u8 == *b)
}

/// The host application: name and factory for messages.
pub(crate) struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin passes a valid String128 buffer.
        match unsafe { name.as_mut() } {
            Some(buf) => {
                write_wide("Musiton", buf);
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        // SAFETY: the plugin passes valid pointers (checked for null).
        let (Some(cid), Some(obj)) = (unsafe { cid.as_ref() }, unsafe { obj.as_mut() }) else {
            return kInvalidArgument;
        };
        if is::<IMessage>(cid) {
            let msg = ComWrapper::new(Message::default());
            match msg.to_com_ptr::<IMessage>() {
                Some(p) => {
                    *obj = p.into_raw().cast();
                    kResultOk
                }
                None => kNoInterface,
            }
        } else if is::<IAttributeList>(cid) {
            let list = ComWrapper::new(AttributeList::default());
            match list.to_com_ptr::<IAttributeList>() {
                Some(p) => {
                    *obj = p.into_raw().cast();
                    kResultOk
                }
                None => kNoInterface,
            }
        } else {
            *obj = std::ptr::null_mut();
            kNoInterface
        }
    }
}

/// What the plugin's own editor changed (UI thread).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Edit {
    /// The user grabbed a control.
    Begin(u32),
    /// A parameter moved to this normalized value.
    Perform(u32, f64),
    /// The user let go of the control.
    End(u32),
    /// The plugin's latency or parameter list changed.
    Restart,
}

/// Receives edits from the plugin's controller.
#[derive(Default)]
pub(crate) struct ComponentHandler {
    pub(crate) edits: Mutex<Vec<Edit>>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl ComponentHandler {
    fn push(&self, e: Edit) -> tresult {
        if let Ok(mut edits) = self.edits.lock() {
            edits.push(e);
        }
        kResultOk
    }
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.push(Edit::Begin(id))
    }
    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.push(Edit::Perform(id, value))
    }
    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.push(Edit::End(id))
    }
    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        self.push(Edit::Restart)
    }
}

/// Events for one block (filled and read on the audio thread; never reallocates).
pub(crate) struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

// SAFETY: only the audio thread touches the list (the host fills it, the plugin reads
// it during `process`).
unsafe impl Sync for EventList {}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    pub(crate) fn with_capacity(n: usize) -> Self {
        Self {
            events: UnsafeCell::new(Vec::with_capacity(n)),
        }
    }

    /// Empties the list.
    ///
    /// # Safety
    /// Audio thread only, not while the plugin is processing.
    pub(crate) unsafe fn clear(&self) {
        // SAFETY: exclusive use on the audio thread (see above).
        unsafe { (*self.events.get()).clear() }
    }

    /// Adds an event if there is room.
    ///
    /// # Safety
    /// As for [`EventList::clear`].
    pub(crate) unsafe fn push(&self, e: Event) {
        // SAFETY: exclusive use on the audio thread (see above).
        let v = unsafe { &mut *self.events.get() };
        if v.len() < v.capacity() {
            v.push(e);
        }
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        // SAFETY: read during `process` on the audio thread.
        unsafe { (*self.events.get()).len() as int32 }
    }
    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        // SAFETY: read during `process`; `e` is checked for null.
        let v = unsafe { &*self.events.get() };
        match (usize::try_from(index).ok().and_then(|i| v.get(i)), unsafe {
            e.as_mut()
        }) {
            (Some(src), Some(dst)) => {
                *dst = *src;
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }
    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        // SAFETY: the plugin adds output events during `process` on the audio thread.
        match unsafe { e.as_ref() } {
            Some(e) => {
                // SAFETY: as above.
                unsafe { self.push(*e) };
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
}

/// Points of one parameter for one block.
pub(crate) struct ParamQueue {
    id: Cell<ParamID>,
    points: UnsafeCell<Vec<(int32, ParamValue)>>,
}

// SAFETY: audio thread only (see `EventList`).
unsafe impl Sync for ParamQueue {}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }
    unsafe fn getPointCount(&self) -> int32 {
        // SAFETY: audio thread only.
        unsafe { (*self.points.get()).len() as int32 }
    }
    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        // SAFETY: audio thread only; out pointers checked for null.
        let v = unsafe { &*self.points.get() };
        match (
            usize::try_from(index).ok().and_then(|i| v.get(i)),
            unsafe { offset.as_mut() },
            unsafe { value.as_mut() },
        ) {
            (Some(&(o, x)), Some(po), Some(px)) => {
                *po = o;
                *px = x;
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }
    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        // SAFETY: audio thread only.
        let v = unsafe { &mut *self.points.get() };
        if v.len() == v.capacity() {
            return kResultFalse;
        }
        v.push((offset, value));
        // SAFETY: optional out pointer.
        if let Some(i) = unsafe { index.as_mut() } {
            *i = v.len() as int32 - 1;
        }
        kResultOk
    }
}

/// Parameter changes for one block: a fixed pool of queues.
pub(crate) struct ParameterChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    used: Cell<usize>,
}

// SAFETY: audio thread only (see `EventList`).
unsafe impl Sync for ParameterChanges {}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParameterChanges {
    /// Room for `params` parameters with `points` points each per block.
    pub(crate) fn new(params: usize, points: usize) -> Self {
        Self {
            queues: (0..params)
                .map(|_| {
                    ComWrapper::new(ParamQueue {
                        id: Cell::new(0),
                        points: UnsafeCell::new(Vec::with_capacity(points)),
                    })
                })
                .collect(),
            used: Cell::new(0),
        }
    }

    /// Removes all changes (audio thread, between blocks).
    pub(crate) fn clear(&self) {
        self.used.set(0);
    }

    /// Adds a change of `id` to `value` at `offset` (dropped when full).
    pub(crate) fn add(&self, id: ParamID, offset: i32, value: f64) {
        let mut index = 0;
        let q = self.queue_for(id, &mut index);
        if let Some(q) = q {
            // SAFETY: audio thread only, between `process` calls.
            unsafe {
                q.addPoint(offset, value, &mut index);
            }
        }
    }

    fn queue_for(&self, id: ParamID, index: &mut int32) -> Option<&ComWrapper<ParamQueue>> {
        let used = self.used.get();
        if let Some(i) = self.queues[..used].iter().position(|q| q.id.get() == id) {
            *index = i as int32;
            return self.queues.get(i);
        }
        let q = self.queues.get(used)?;
        q.id.set(id);
        // SAFETY: audio thread only; the queue is not in use by the plugin right now.
        unsafe { (*q.points.get()).clear() };
        self.used.set(used + 1);
        *index = used as int32;
        Some(q)
    }
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used.get() as int32
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        usize::try_from(index)
            .ok()
            .filter(|i| *i < self.used.get())
            .and_then(|i| self.queues.get(i))
            .and_then(|q| q.as_com_ref::<IParamValueQueue>())
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        // SAFETY: pointers from the plugin, checked for null.
        let (Some(id), Some(index)) = (unsafe { id.as_ref() }, unsafe { index.as_mut() }) else {
            return std::ptr::null_mut();
        };
        self.queue_for(*id, index)
            .and_then(|q| q.as_com_ref::<IParamValueQueue>())
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

/// A growable in-memory stream for plugin state (UI thread).
#[derive(Default)]
pub(crate) struct MemoryStream {
    pub(crate) data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl MemoryStream {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: RefCell::new(bytes.to_vec()),
            pos: Cell::new(0),
        }
    }
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, n: int32, read: *mut int32) -> tresult {
        let data = self.data.borrow();
        let pos = self.pos.get().min(data.len());
        let n = usize::try_from(n).unwrap_or(0).min(data.len() - pos);
        if n > 0 && buffer.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin gives a buffer of at least `n` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr().add(pos), buffer.cast::<u8>(), n);
        }
        self.pos.set(pos + n);
        // SAFETY: optional out pointer.
        if let Some(r) = unsafe { read.as_mut() } {
            *r = n as int32;
        }
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, n: int32, written: *mut int32) -> tresult {
        let n = usize::try_from(n).unwrap_or(0);
        if n > 0 && buffer.is_null() {
            return kInvalidArgument;
        }
        let mut data = self.data.borrow_mut();
        let pos = self.pos.get();
        if data.len() < pos + n {
            data.resize(pos + n, 0);
        }
        // SAFETY: the plugin gives a buffer of `n` bytes.
        let src = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), n) };
        data[pos..pos + n].copy_from_slice(src);
        self.pos.set(pos + n);
        // SAFETY: optional out pointer.
        if let Some(w) = unsafe { written.as_mut() } {
            *w = n as int32;
        }
        kResultOk
    }
    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let base = match mode {
            0 => 0,
            1 => self.pos.get() as i64,
            2 => len,
            _ => return kInvalidArgument,
        };
        let new = (base + pos).max(0);
        self.pos.set(new as usize);
        // SAFETY: optional out pointer.
        if let Some(r) = unsafe { result.as_mut() } {
            *r = new;
        }
        kResultOk
    }
    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        // SAFETY: out pointer checked for null.
        match unsafe { pos.as_mut() } {
            Some(p) => {
                *p = self.pos.get() as i64;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }
}

/// A value in an attribute list.
enum Attr {
    Int(i64),
    Float(f64),
    Text(Vec<TChar>),
    Binary(Vec<u8>),
}

/// Attributes of a message between a plugin's processor and controller.
#[derive(Default)]
pub(crate) struct AttributeList {
    values: Mutex<HashMap<String, Attr>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

fn key(id: FIDString) -> Option<String> {
    // SAFETY: attribute ids are NUL-terminated C strings from the plugin.
    (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned())
}

impl AttributeList {
    fn set(&self, id: FIDString, v: Attr) -> tresult {
        match (key(id), self.values.lock()) {
            (Some(k), Ok(mut m)) => {
                m.insert(k, v);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: FIDString, value: int64) -> tresult {
        self.set(id, Attr::Int(value))
    }
    unsafe fn getInt(&self, id: FIDString, value: *mut int64) -> tresult {
        let m = self.values.lock();
        // SAFETY: out pointer checked for null.
        match (key(id), m, unsafe { value.as_mut() }) {
            (Some(k), Ok(m), Some(out)) => match m.get(&k) {
                Some(Attr::Int(v)) => {
                    *out = *v;
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }
    unsafe fn setFloat(&self, id: FIDString, value: f64) -> tresult {
        self.set(id, Attr::Float(value))
    }
    unsafe fn getFloat(&self, id: FIDString, value: *mut f64) -> tresult {
        let m = self.values.lock();
        // SAFETY: out pointer checked for null.
        match (key(id), m, unsafe { value.as_mut() }) {
            (Some(k), Ok(m), Some(out)) => match m.get(&k) {
                Some(Attr::Float(v)) => {
                    *out = *v;
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }
    unsafe fn setString(&self, id: FIDString, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut text = Vec::new();
        // SAFETY: a NUL-terminated UTF-16 string from the plugin.
        unsafe {
            let mut p = string;
            while *p != 0 {
                text.push(*p);
                p = p.add(1);
            }
        }
        text.push(0);
        self.set(id, Attr::Text(text))
    }
    unsafe fn getString(&self, id: FIDString, string: *mut TChar, size: uint32) -> tresult {
        let m = self.values.lock();
        match (key(id), m) {
            (Some(k), Ok(m)) => match m.get(&k) {
                Some(Attr::Text(t)) if !string.is_null() => {
                    let room = size as usize / std::mem::size_of::<TChar>();
                    let n = t.len().min(room);
                    // SAFETY: the plugin's buffer holds `size` bytes.
                    unsafe { std::ptr::copy_nonoverlapping(t.as_ptr(), string, n) };
                    if n > 0 && n == room {
                        // SAFETY: as above; keep it terminated.
                        unsafe { *string.add(n - 1) = 0 };
                    }
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }
    unsafe fn setBinary(&self, id: FIDString, data: *const c_void, size: uint32) -> tresult {
        if data.is_null() && size > 0 {
            return kInvalidArgument;
        }
        // SAFETY: the plugin gives `size` readable bytes.
        let bytes = if size == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) }.to_vec()
        };
        self.set(id, Attr::Binary(bytes))
    }
    unsafe fn getBinary(
        &self,
        id: FIDString,
        data: *mut *const c_void,
        size: *mut uint32,
    ) -> tresult {
        let m = self.values.lock();
        // SAFETY: out pointers checked for null.
        match (key(id), m, unsafe { data.as_mut() }, unsafe {
            size.as_mut()
        }) {
            (Some(k), Ok(m), Some(d), Some(s)) => match m.get(&k) {
                Some(Attr::Binary(b)) => {
                    // The bytes live as long as the list (the plugin owns a reference).
                    *d = b.as_ptr().cast();
                    *s = b.len() as uint32;
                    kResultOk
                }
                _ => kResultFalse,
            },
            _ => kInvalidArgument,
        }
    }
}

/// A message: an id and an attribute list.
pub(crate) struct Message {
    id: Mutex<Option<std::ffi::CString>>,
    attributes: ComWrapper<AttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: Mutex::new(None),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id
            .lock()
            .ok()
            .and_then(|id| id.as_ref().map(|s| s.as_ptr()))
            .unwrap_or(std::ptr::null())
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        if let Ok(mut slot) = self.id.lock() {
            // SAFETY: NUL-terminated C string from the plugin (or null).
            *slot = (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_owned());
        }
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}
