//! How a plug-in reaches its host: [`Host`] on the main thread, and
//! [`ProcessContext`] on the audio thread.

use std::cell::{OnceCell, RefCell};
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use lspk_params::ParamId;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::events::Note;
use crate::params::ParamValues;

/// What a format's adapter does when the plug-in asks something of the
/// host. Each call only asks; the host acts on it later. An adapter
/// implements it for its own host (CLAP's, the standalone app's,
/// Beatlayer's).
pub trait HostBackend {
    /// Asks the host to flush parameters soon (call process or the
    /// parameter flush), so the plug-in's edits reach it.
    fn request_flush(&self);
    /// Tells the host the plug-in's state has changed and needs saving.
    fn mark_dirty(&self);
    /// Asks the host to deactivate and reactivate the plug-in, so a change
    /// of latency, say, takes effect.
    fn request_restart(&self);
    /// Asks the host to start processing again if the plug-in sleeps.
    fn request_process(&self);
    /// Asks the host to call [`Plugin::on_main_thread`](crate::Plugin::on_main_thread)
    /// soon.
    fn request_callback(&self);
    /// Tells the host every parameter's value may have changed without an
    /// edit (after loading state), so it reads them again.
    fn values_changed(&self);
}

/// Why a parameter edit wasn't made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The plug-in is still starting: [`Plugin::new`](crate::Plugin::new)
    /// hasn't returned.
    NotReady,
    /// There's no parameter with this ID.
    UnknownParam(ParamId),
    /// Edits are arriving faster than the audio thread takes them; try
    /// again after the next block.
    QueueFull,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReady => f.write_str("the plug-in is still starting"),
            Self::UnknownParam(id) => write!(f, "there's no parameter {}", id.0),
            Self::QueueFull => f.write_str("too many edits at once; try again"),
        }
    }
}

impl std::error::Error for EditError {}

/// One parameter edit on its way from the main thread to the audio thread.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    /// A gesture starts.
    Begin(ParamId),
    /// A value, in the parameter's own units. `echo` says whether the host
    /// hears it too; a value from loaded state goes only to the processor.
    Value { id: ParamId, value: f64, echo: bool },
    /// A gesture ends.
    End(ParamId),
}

/// The plug-in's link to its host on the main thread: parameter edits, and
/// requests for the host.
///
/// A plug-in gets one in [`Plugin::new`](crate::Plugin::new) and keeps it
/// as long as it likes; once the instance is gone, every call does nothing.
#[derive(Clone)]
pub struct Host {
    inner: Rc<HostInner>,
}

/// What a [`Host`] and its clones share.
struct HostInner {
    backend: RefCell<Option<Box<dyn HostBackend>>>,
    values: OnceCell<Arc<ParamValues>>,
    edits: OnceCell<RefCell<Producer<Edit>>>,
}

impl Host {
    /// A link through `backend`, for an adapter to give a new plug-in.
    /// Edits work once the adapter calls [`Host::connect`].
    #[must_use]
    pub fn new(backend: Box<dyn HostBackend>) -> Self {
        Self {
            inner: Rc::new(HostInner {
                backend: RefCell::new(Some(backend)),
                values: OnceCell::new(),
                edits: OnceCell::new(),
            }),
        }
    }

    /// Connects the link to the instance's parameters, once the plug-in has
    /// described them, and returns the queue the adapter takes edits from
    /// on the audio thread (or on the main thread while it's inactive).
    /// It holds enough edits for every parameter several times over.
    ///
    /// # Panics
    ///
    /// If it's connected twice.
    #[must_use]
    pub fn connect(&self, values: Arc<ParamValues>) -> Consumer<Edit> {
        let (producer, consumer) = RingBuffer::new(Self::edit_capacity(&values));
        assert!(
            self.inner.values.set(values).is_ok()
                && self.inner.edits.set(RefCell::new(producer)).is_ok(),
            "a host link connects once"
        );
        consumer
    }

    /// How many edits the queue [`connect`](Self::connect) returns holds.
    #[must_use]
    pub fn edit_capacity(values: &ParamValues) -> usize {
        (values.len() * 4).max(1024)
    }

    /// Cuts the link from the host, as the instance ends: every call after
    /// does nothing.
    pub fn disconnect(&self) {
        self.inner.backend.borrow_mut().take();
    }

    /// The parameters' values now, once the plug-in has described them.
    #[must_use]
    pub fn values(&self) -> Option<&ParamValues> {
        self.inner.values.get().map(Arc::as_ref)
    }

    /// Parameter `id`'s value now.
    #[must_use]
    pub fn param(&self, id: ParamId) -> Option<f64> {
        self.values()?.get(id)
    }

    /// Starts a gesture on parameter `id`, as a person grabbing its control
    /// does. Follow it with [`set_param`](Self::set_param) for each new
    /// value and [`end_edit`](Self::end_edit) when they let go.
    ///
    /// # Errors
    ///
    /// The edit wasn't made: see [`EditError`].
    pub fn begin_edit(&self, id: ParamId) -> Result<(), EditError> {
        self.push(id, &[Edit::Begin(id)], false)
    }

    /// Sets parameter `id` to `value`, in its own units, clamped to its
    /// range: the processor gets it at the start of its next block, and
    /// the host hears it from the next process or flush.
    ///
    /// # Errors
    ///
    /// The edit wasn't made: see [`EditError`].
    pub fn set_param(&self, id: ParamId, value: f64) -> Result<(), EditError> {
        let value = self.clamped(id, value)?;
        self.push(
            id,
            &[Edit::Value {
                id,
                value,
                echo: true,
            }],
            true,
        )
    }

    /// Ends the gesture [`begin_edit`](Self::begin_edit) started.
    ///
    /// # Errors
    ///
    /// The edit wasn't made: see [`EditError`].
    pub fn end_edit(&self, id: ParamId) -> Result<(), EditError> {
        self.push(id, &[Edit::End(id)], false)
    }

    /// Sets parameter `id` to `value` in one gesture: begin, value, end,
    /// as an agent's command or a typed value does.
    ///
    /// # Errors
    ///
    /// The edit wasn't made: see [`EditError`].
    pub fn edit(&self, id: ParamId, value: f64) -> Result<(), EditError> {
        let value = self.clamped(id, value)?;
        let edits = [
            Edit::Begin(id),
            Edit::Value {
                id,
                value,
                echo: true,
            },
            Edit::End(id),
        ];
        self.push(id, &edits, true)
    }

    /// Asks the host to deactivate and reactivate the plug-in.
    pub fn request_restart(&self) {
        self.with_backend(|b| b.request_restart());
    }

    /// Asks the host to call [`Plugin::on_main_thread`](crate::Plugin::on_main_thread)
    /// soon.
    pub fn request_callback(&self) {
        self.with_backend(|b| b.request_callback());
    }

    /// Tells the host the plug-in's own state has changed. Parameter edits
    /// do this themselves.
    pub fn mark_dirty(&self) {
        self.with_backend(|b| b.mark_dirty());
    }

    /// After the adapter loads state into the values: tells the host to
    /// read them again, and, while the instance is `active`, passes them to
    /// the processor without the host hearing them as edits.
    ///
    /// # Errors
    ///
    /// The values couldn't all be queued for the processor.
    #[doc(hidden)]
    pub fn state_loaded(&self, active: bool) -> Result<(), EditError> {
        self.with_backend(|b| b.values_changed());
        if !active {
            return Ok(());
        }
        let values = self.values().ok_or(EditError::NotReady)?;
        let edits = self.inner.edits.get().ok_or(EditError::NotReady)?;
        let mut edits = edits.borrow_mut();
        if edits.slots() < values.len() {
            return Err(EditError::QueueFull);
        }
        for (index, descriptor) in values.descriptors().iter().enumerate() {
            let edit = Edit::Value {
                id: descriptor.id,
                value: values.value_at(index),
                echo: false,
            };
            edits.push(edit).map_err(|_| EditError::QueueFull)?;
        }
        drop(edits);
        self.with_backend(|b| b.request_process());
        Ok(())
    }

    fn clamped(&self, id: ParamId, value: f64) -> Result<f64, EditError> {
        let values = self.values().ok_or(EditError::NotReady)?;
        let descriptor = values.descriptor(id).ok_or(EditError::UnknownParam(id))?;
        Ok(descriptor.clamp(value))
    }

    /// Queues `edits` for parameter `id`, all or none; records any value,
    /// then asks the host for a flush (and marks the state dirty, for a
    /// value).
    fn push(&self, id: ParamId, edits: &[Edit], dirty: bool) -> Result<(), EditError> {
        let values = self.values().ok_or(EditError::NotReady)?;
        if values.index_of(id).is_none() {
            return Err(EditError::UnknownParam(id));
        }
        let queue = self.inner.edits.get().ok_or(EditError::NotReady)?;
        {
            let mut queue = queue.borrow_mut();
            if queue.slots() < edits.len() {
                return Err(EditError::QueueFull);
            }
            for edit in edits {
                if let Edit::Value { id, value, .. } = *edit {
                    values.set(id, value);
                }
                queue.push(*edit).map_err(|_| EditError::QueueFull)?;
            }
        }
        self.with_backend(|b| {
            b.request_flush();
            if dirty {
                b.mark_dirty();
            }
        });
        Ok(())
    }

    fn with_backend(&self, f: impl FnOnce(&dyn HostBackend)) {
        // A backend call may come back into the plug-in (a host that
        // flushes at once, say); it never reaches this link again, but if
        // it did, the request is dropped rather than panicking.
        if let Ok(backend) = self.inner.backend.try_borrow()
            && let Some(backend) = backend.as_deref()
        {
            f(backend);
        }
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("connected", &self.inner.values.get().is_some())
            .finish_non_exhaustive()
    }
}

/// What a processor sends the host from the audio thread.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum OutputEvent {
    /// A gesture on a parameter starts.
    GestureBegin {
        /// Its frame in the block.
        time: u32,
        /// Which parameter.
        id: ParamId,
    },
    /// A parameter takes a new value, in its own units.
    Param {
        /// Its frame in the block.
        time: u32,
        /// Which parameter.
        id: ParamId,
        /// Its new value.
        value: f64,
    },
    /// A gesture on a parameter ends.
    GestureEnd {
        /// Its frame in the block.
        time: u32,
        /// Which parameter.
        id: ParamId,
    },
    /// A voice has finished sounding, so a host that modulates per note
    /// can let it go.
    NoteEnd {
        /// Its frame in the block.
        time: u32,
        /// Which note.
        note: Note,
    },
}

/// Where a format's adapter puts what a processor sends the host.
pub trait EventSink {
    /// Sends `event`; false if the host's queue is full.
    fn push(&mut self, event: OutputEvent) -> bool;
}

/// A sink that drops everything, for a processor run with no host.
impl EventSink for () {
    fn push(&mut self, _event: OutputEvent) -> bool {
        true
    }
}

/// What a processor can reach during a block: the parameters' values, and
/// the host, for the processor's own parameter changes and finished notes.
pub struct ProcessContext<'a> {
    values: &'a ParamValues,
    sink: &'a mut dyn EventSink,
}

impl<'a> ProcessContext<'a> {
    /// A context over `values`, sending to `sink`. For adapters.
    pub fn new(values: &'a ParamValues, sink: &'a mut dyn EventSink) -> Self {
        Self { values, sink }
    }

    /// The parameters' values now.
    #[must_use]
    pub fn values(&self) -> &ParamValues {
        self.values
    }

    /// Starts a gesture on parameter `id` at frame `time`.
    pub fn begin_gesture(&mut self, time: u32, id: ParamId) {
        self.sink.push(OutputEvent::GestureBegin { time, id });
    }

    /// Sets parameter `id` to `value` at frame `time`, clamped to its range,
    /// and tells the host; returns the value it took. The processor
    /// applies it itself: no event comes back. Wrap it in
    /// [`begin_gesture`](Self::begin_gesture) and
    /// [`end_gesture`](Self::end_gesture), as a person's change would be.
    pub fn set_param(&mut self, time: u32, id: ParamId, value: f64) -> Option<f64> {
        let value = self.values.set(id, value)?;
        self.sink.push(OutputEvent::Param { time, id, value });
        Some(value)
    }

    /// Ends a gesture on parameter `id` at frame `time`.
    pub fn end_gesture(&mut self, time: u32, id: ParamId) {
        self.sink.push(OutputEvent::GestureEnd { time, id });
    }

    /// Tells the host the voice playing `note` finished at frame `time`.
    pub fn note_end(&mut self, time: u32, note: Note) {
        self.sink.push(OutputEvent::NoteEnd { time, note });
    }
}

impl fmt::Debug for ProcessContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessContext").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lspk_params::ParamDescriptor;
    use std::cell::Cell;

    #[derive(Default)]
    struct Calls {
        flush: Cell<u32>,
        dirty: Cell<u32>,
        process: Cell<u32>,
        rescan: Cell<u32>,
    }

    struct Recorder(Rc<Calls>);

    impl HostBackend for Recorder {
        fn request_flush(&self) {
            self.0.flush.set(self.0.flush.get() + 1);
        }
        fn mark_dirty(&self) {
            self.0.dirty.set(self.0.dirty.get() + 1);
        }
        fn request_restart(&self) {}
        fn request_process(&self) {
            self.0.process.set(self.0.process.get() + 1);
        }
        fn request_callback(&self) {}
        fn values_changed(&self) {
            self.0.rescan.set(self.0.rescan.get() + 1);
        }
    }

    fn connected() -> (Host, Consumer<Edit>, Rc<Calls>) {
        let calls = Rc::new(Calls::default());
        let host = Host::new(Box::new(Recorder(calls.clone())));
        assert_eq!(host.edit(ParamId(1), 0.5), Err(EditError::NotReady));
        let values = ParamValues::new(vec![ParamDescriptor::continuous(
            ParamId(1),
            "Amount",
            (0.0, 1.0),
            0.0,
            lspk_params::ParamUnit::None,
        )])
        .expect("valid parameters");
        let edits = host.connect(Arc::new(values));
        (host, edits, calls)
    }

    #[test]
    fn an_edit_is_a_whole_gesture_recorded_queued_and_flushed() {
        let (host, mut edits, calls) = connected();
        host.edit(ParamId(1), 2.0).expect("an edit");
        assert_eq!(host.param(ParamId(1)), Some(1.0));
        let queued: Vec<Edit> = std::iter::from_fn(|| edits.pop().ok()).collect();
        assert_eq!(
            queued,
            [
                Edit::Begin(ParamId(1)),
                Edit::Value {
                    id: ParamId(1),
                    value: 1.0,
                    echo: true
                },
                Edit::End(ParamId(1)),
            ]
        );
        assert_eq!((calls.flush.get(), calls.dirty.get()), (1, 1));
        assert_eq!(
            host.edit(ParamId(9), 0.0),
            Err(EditError::UnknownParam(ParamId(9)))
        );
    }

    #[test]
    fn a_full_queue_refuses_the_whole_edit() {
        let (host, mut edits, _) = connected();
        let capacity = Host::edit_capacity(host.values().expect("connected"));
        for _ in 0..capacity {
            host.set_param(ParamId(1), 0.25).expect("room");
        }
        assert_eq!(host.edit(ParamId(1), 0.75), Err(EditError::QueueFull));
        assert_eq!(host.param(ParamId(1)), Some(0.25));
        assert!(edits.pop().is_ok());
    }

    #[test]
    fn loaded_values_reach_the_processor_but_not_the_host() {
        let (host, mut edits, calls) = connected();
        host.values().expect("connected").set(ParamId(1), 0.75);
        host.state_loaded(false).expect("inactive");
        assert!(edits.pop().is_err());
        host.state_loaded(true).expect("room");
        assert_eq!(
            edits.pop(),
            Ok(Edit::Value {
                id: ParamId(1),
                value: 0.75,
                echo: false
            })
        );
        assert_eq!((calls.process.get(), calls.flush.get()), (1, 0));
        assert_eq!(calls.rescan.get(), 2);
    }

    #[test]
    fn a_disconnected_link_does_nothing() {
        let (host, _edits, calls) = connected();
        host.disconnect();
        host.edit(ParamId(1), 0.5).expect("still recorded");
        host.request_restart();
        assert_eq!(calls.flush.get(), 0);
    }

    struct Collect(Vec<OutputEvent>);

    impl EventSink for Collect {
        fn push(&mut self, event: OutputEvent) -> bool {
            self.0.push(event);
            true
        }
    }

    #[test]
    fn a_processor_change_is_recorded_and_sent() {
        let values = ParamValues::new(vec![ParamDescriptor::toggle(ParamId(3), "On", false)])
            .expect("valid parameters");
        let mut sink = Collect(Vec::new());
        let mut cx = ProcessContext::new(&values, &mut sink);
        cx.begin_gesture(4, ParamId(3));
        assert_eq!(cx.set_param(4, ParamId(3), 0.9), Some(1.0));
        assert_eq!(cx.set_param(4, ParamId(8), 0.9), None);
        cx.end_gesture(4, ParamId(3));
        assert_eq!(cx.values().get(ParamId(3)), Some(1.0));
        assert_eq!(sink.0.len(), 3);
    }
}
