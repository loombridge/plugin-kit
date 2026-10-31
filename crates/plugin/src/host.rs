//! How a plug-in reaches its host: [`Host`] on the main thread, and
//! [`ProcessContext`] — which carries the host's [`Transport`] — on the
//! audio thread.

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
    /// Tells the host parameters' names, value text, hidden flags and
    /// values may have changed, and the remote-controls pages with them,
    /// because a slot of the plug-in's [`ParamSpace`](lspk_params::ParamSpace)
    /// changed its module. By default, [`values_changed`](Self::values_changed).
    fn params_changed(&self) {
        self.values_changed();
    }
    /// A way to ask for [`Plugin::on_main_thread`](crate::Plugin::on_main_thread)
    /// from any thread, such as an agent server's, if the host offers one.
    /// It must do nothing once the instance has gone.
    fn waker(&self) -> Option<Waker> {
        None
    }
    /// The host's name, such as `Bitwig Studio`, if it gives one.
    fn host_name(&self) -> Option<String> {
        None
    }
    /// The name of the track the instance is on, if the host reports it
    /// (CLAP's `track-info`).
    fn track_name(&self) -> Option<String> {
        None
    }
}

/// Asks the host, from any thread, to call
/// [`Plugin::on_main_thread`](crate::Plugin::on_main_thread) soon: see
/// [`Host::waker`]. Once the instance has gone it does nothing.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

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
    /// Which choice each slot of the parameter space held when last looked
    /// at.
    selection: RefCell<Vec<usize>>,
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
                selection: RefCell::new(Vec::new()),
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
        if let Some(space) = values.space() {
            *self.inner.selection.borrow_mut() = space.selection(&|id| values.get(id));
        }
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

    /// The parameters' values now, shared, once the plug-in has described
    /// them: what an off-screen editor draws from.
    #[must_use]
    pub fn values_shared(&self) -> Option<Arc<ParamValues>> {
        self.inner.values.get().cloned()
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

    /// A way to ask for [`Plugin::on_main_thread`](crate::Plugin::on_main_thread)
    /// from any thread, if the host offers one: an agent server's threads
    /// wake the main thread with it when a request comes. It does nothing
    /// once the instance has gone.
    #[must_use]
    pub fn waker(&self) -> Option<Waker> {
        self.inner.backend.try_borrow().ok()?.as_deref()?.waker()
    }

    /// The host's name, such as `Bitwig Studio`, if it gives one.
    #[must_use]
    pub fn host_name(&self) -> Option<String> {
        self.inner
            .backend
            .try_borrow()
            .ok()?
            .as_deref()?
            .host_name()
    }

    /// The name of the track the instance is on, if the host reports it.
    /// [`Plugin::track_info_changed`](crate::Plugin::track_info_changed)
    /// says when it changes.
    #[must_use]
    pub fn track_name(&self) -> Option<String> {
        self.inner
            .backend
            .try_borrow()
            .ok()?
            .as_deref()?
            .track_name()
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
        // A loaded state holds the values of the modules its slots hold:
        // the slots take them as they are.
        let adopted = self.selection_changed();
        self.with_backend(|b| {
            if adopted.is_empty() {
                b.values_changed();
            } else {
                b.params_changed();
            }
        });
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

    /// Brings the slots of the plug-in's parameter space up to date after
    /// a slot's type parameter changed: each slot that now holds another
    /// module gives its reserved parameters the new module's defaults,
    /// passed to the processor without the host hearing them as edits, and
    /// the host is told what changed
    /// ([`HostBackend::params_changed`]). Returns whether any slot changed.
    ///
    /// The kit calls it after every edit of a type parameter, and an
    /// adapter calls it on the main thread after the host's automation
    /// changes one ([`ParamValues::is_selector`]).
    pub fn sync_slots(&self) -> bool {
        let changed = self.selection_changed();
        if changed.is_empty() {
            return false;
        }
        let (Some(values), Some(queue)) = (self.values(), self.inner.edits.get()) else {
            return false;
        };
        if let Some(space) = values.space() {
            let mut queue = queue.borrow_mut();
            for (slot, choice) in changed {
                for (id, value) in space.slot_defaults(slot, choice) {
                    values.set(id, value);
                    // A full queue drops the processor's copy; the values
                    // themselves are already set.
                    let _ = queue.push(Edit::Value {
                        id,
                        value,
                        echo: false,
                    });
                }
            }
        }
        self.with_backend(|b| {
            b.params_changed();
            b.request_process();
            b.mark_dirty();
        });
        true
    }

    /// Each slot that holds another choice than when last looked at, with
    /// the choice it holds now; remembers the choices now.
    fn selection_changed(&self) -> Vec<(usize, usize)> {
        let Some(space) = self.values().and_then(ParamValues::space) else {
            return Vec::new();
        };
        let values = self.values().expect("a space comes with values");
        let now = space.selection(&|id| values.get(id));
        let before = self.inner.selection.replace(now.clone());
        now.iter()
            .enumerate()
            .filter(|(slot, choice)| before.get(*slot) != Some(*choice))
            .map(|(slot, choice)| (slot, *choice))
            .collect()
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
        let mut selector = false;
        {
            let mut queue = queue.borrow_mut();
            if queue.slots() < edits.len() {
                return Err(EditError::QueueFull);
            }
            for edit in edits {
                if let Edit::Value { id, value, .. } = *edit {
                    values.set(id, value);
                    selector |= values.is_selector(id);
                }
                queue.push(*edit).map_err(|_| EditError::QueueFull)?;
            }
        }
        if selector {
            self.sync_slots();
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

/// The host's transport at the first sample of a block: what a synced LFO,
/// a tempo-synced envelope or a song-locked phase follows.
///
/// Each format maps its own transport into it (CLAP's transport event;
/// VST3's process context and the Audio Unit's host callbacks through
/// clap-wrapper) and gives it to the processor's [`ProcessContext`]. A
/// format that doesn't offer a field leaves its default; a host that offers
/// no transport at all passes `None`, and the plug-in follows a clock of
/// its own, as the standalone app does with its device's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    /// Beats (quarter notes) per minute.
    pub tempo: f64,
    /// Beats in a bar, from the host's time signature.
    pub beats_per_bar: f64,
    /// Whether the host is playing: a song-locked phase follows only a
    /// playing host.
    pub playing: bool,
    /// The song position in beats (quarter notes).
    pub beats: f64,
    /// The song position in seconds.
    pub seconds: f64,
}

impl Default for Transport {
    /// A stopped transport at the start of a song: 120 beats per minute in
    /// 4/4.
    fn default() -> Self {
        Self {
            tempo: 120.0,
            beats_per_bar: 4.0,
            playing: false,
            beats: 0.0,
            seconds: 0.0,
        }
    }
}

impl Transport {
    /// Moves the song position on by `frames` samples at `sample_rate`, as
    /// a host does between blocks.
    pub fn advance(&mut self, frames: usize, sample_rate: f64) {
        let seconds = frames as f64 / sample_rate.max(1.0);
        self.seconds += seconds;
        self.beats += seconds * self.tempo.max(0.0) / 60.0;
    }
}

/// What a processor can reach during a block: the parameters' values, the
/// host's transport, and the host itself, for the processor's own parameter
/// changes and finished notes.
pub struct ProcessContext<'a> {
    values: &'a ParamValues,
    sink: &'a mut dyn EventSink,
    transport: Option<&'a Transport>,
}

impl<'a> ProcessContext<'a> {
    /// A context over `values`, sending to `sink`, with no transport. For
    /// adapters; a host with one uses [`with_transport`](Self::with_transport).
    pub fn new(values: &'a ParamValues, sink: &'a mut dyn EventSink) -> Self {
        Self {
            values,
            sink,
            transport: None,
        }
    }

    /// A context that also reports `transport`, the host's at the block's
    /// first sample, or `None` for a host that offers none. For adapters.
    pub fn with_transport(
        values: &'a ParamValues,
        sink: &'a mut dyn EventSink,
        transport: Option<&'a Transport>,
    ) -> Self {
        Self {
            values,
            sink,
            transport,
        }
    }

    /// The host's transport at this block's first sample, or `None` when
    /// the host offers none. It's a plain value read on the audio thread:
    /// nothing locks to reach it.
    #[must_use]
    pub fn transport(&self) -> Option<&Transport> {
        self.transport
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
        changed: Cell<u32>,
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
        fn params_changed(&self) {
            self.0.changed.set(self.0.changed.get() + 1);
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

    /// A space with one slot, holding nothing or a module of two
    /// parameters.
    fn slotted() -> (Host, Consumer<Edit>, Rc<Calls>, lspk_params::Slot) {
        use lspk_params::{ModuleType, ParamSpace, ParamUnit};
        let space = ParamSpace::builder()
            .slots(
                (9, "fx", "FX"),
                "Type",
                &["1"],
                vec![
                    ModuleType::new(1, "off", "Off"),
                    ModuleType::new(2, "echo", "Echo")
                        .param(ParamDescriptor::continuous(
                            ParamId(0),
                            "Time",
                            (0.0, 100.0),
                            25.0,
                            ParamUnit::Milliseconds,
                        ))
                        .param(ParamDescriptor::toggle(ParamId(1), "Sync", true)),
                ],
            )
            .build()
            .expect("a space");
        let slot = space.slots()[0].clone();
        let values = ParamValues::with_space(space.descriptors(), Arc::new(space))
            .expect("valid parameters");
        let calls = Rc::new(Calls::default());
        let host = Host::new(Box::new(Recorder(calls.clone())));
        let edits = host.connect(Arc::new(values));
        (host, edits, calls, slot)
    }

    #[test]
    fn a_slots_new_module_starts_at_its_defaults_and_the_host_is_told() {
        let (host, mut edits, calls, slot) = slotted();
        host.edit(slot.selector, 1.0).expect("an edit");
        assert_eq!(calls.changed.get(), 1);
        let queued: Vec<Edit> = std::iter::from_fn(|| edits.pop().ok()).collect();
        assert_eq!(
            queued[3..],
            [
                Edit::Value {
                    id: slot.reserved_id(0),
                    value: 0.25,
                    echo: false
                },
                Edit::Value {
                    id: slot.reserved_id(1),
                    value: 1.0,
                    echo: false
                },
            ]
        );
        let values = host.values().expect("connected");
        assert_eq!(
            values.view(slot.reserved_id(1)).expect("Sync").name,
            "FX 1 Sync"
        );
        // Nothing else changed, so nothing more to tell.
        assert!(!host.sync_slots());
        assert_eq!(calls.changed.get(), 1);

        // Loaded state keeps its values, and the host rereads everything.
        values.set(slot.selector, 0.0);
        values.set(slot.reserved_id(0), 0.75);
        host.state_loaded(false).expect("inactive");
        assert_eq!(calls.changed.get(), 2);
        values.set(slot.selector, 1.0);
        host.state_loaded(false).expect("inactive");
        assert_eq!(values.get(slot.reserved_id(0)), Some(0.75));
        assert_eq!(calls.changed.get(), 3);
        assert!(values.view(slot.reserved_id(0)).is_some_and(|v| !v.hidden));
    }

    #[test]
    fn a_context_carries_the_hosts_transport() {
        let values = ParamValues::new(Vec::new()).expect("no parameters");
        let mut sink = Collect(Vec::new());
        let cx = ProcessContext::new(&values, &mut sink);
        assert_eq!(cx.transport(), None);
        let mut transport = Transport {
            tempo: 90.0,
            beats_per_bar: 3.0,
            playing: true,
            beats: 8.5,
            seconds: 5.5,
        };
        let cx = ProcessContext::with_transport(&values, &mut sink, Some(&transport));
        assert_eq!(cx.transport(), Some(&transport));
        // A host moves the song position on between blocks.
        transport.advance(24_000, 48_000.0);
        assert_eq!(transport.seconds, 6.0);
        assert_eq!(transport.beats, 9.25);
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
