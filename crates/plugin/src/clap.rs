//! The CLAP export, through `clack`.
//!
//! [`export_clap!`](crate::export_clap) in a plug-in's `cdylib` crate makes
//! it a CLAP plug-in with the `params`, `state`, `audio-ports`,
//! `note-ports`, `latency` and `tail` extensions, and `gui` for a plug-in
//! with an [`Editor`](crate::Editor). The glue between CLAP's
//! calls and the [`Plugin`] and [`Processor`] traits allocates nothing,
//! locks nothing and never blocks on the audio thread.
//!
//! What the host sees of each parameter: a continuous or gain parameter as
//! its control's travel from 0 to 1, a stepped one as its whole-number
//! value (a choice also as an enumeration), each automatable. The kit
//! converts to and from the parameter's own units.

use std::cell::RefCell;
use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, HostGui, PluginGui,
    PluginGuiImpl, Window as ClapWindow,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    HostParams, ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, ParamRescanFlags,
    PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{HostState, PluginState, PluginStateImpl};
use clack_extensions::tail::{PluginTail, PluginTailImpl, TailLength};
use clack_plugin::entry::{DefaultPluginFactory, SinglePluginEntry};
use clack_plugin::events::event_types::{
    NoteEndEvent, ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::io::{InputEvents, OutputEvents};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::events::{Match, Pckn};
use clack_plugin::extensions::PluginExtensions;
use clack_plugin::host::{HostAudioProcessorHandle, HostMainThreadHandle, HostSharedHandle};
use clack_plugin::plugin::{
    Plugin as ClackPlugin, PluginAudioProcessor, PluginDescriptor, PluginError as ClackError,
    PluginMainThread, PluginShared,
};
use clack_plugin::process::audio::ChannelPair;
use clack_plugin::process::{
    Audio, Events as ClackEvents, PluginAudioConfiguration, Process, ProcessStatus as ClackStatus,
};
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::{ClapId, Cookie};
use lspk_params::{ParamDescriptor, ParamId, ParamKind};
use rtrb::Consumer;

use crate::audio::{Buffers, MAX_CHANNELS};
use crate::editor::{
    Editor, EditorChanges, EditorContext, EditorHost, EditorSize, ParentWindow, WindowApi,
};
use crate::events::{Event, EventSource, Events, Note};
use crate::host::{Edit, EventSink, Host, HostBackend, OutputEvent, ProcessContext};
use crate::params::ParamValues;
use crate::{Plugin, ProcessConfig, ProcessStatus, Processor, Tail, state};

/// Exports `$plugin`, a type implementing [`Plugin`](crate::Plugin), as
/// this library's CLAP entry point (`clap_entry`). Use it once, in the
/// plug-in's `cdylib` crate.
#[macro_export]
macro_rules! export_clap {
    ($plugin:ty) => {
        $crate::clap::__export_entry!($crate::clap::ClapEntry<$plugin>);
    };
}

/// A CLAP entry point for `$plugin` as a value, without exporting it, for
/// hosting a plug-in inside the same binary, as tests do:
/// `static ENTRY: EntryDescriptor = lspk_plugin::clap_entry!(MyPlugin);`.
#[macro_export]
macro_rules! clap_entry {
    ($plugin:ty) => {
        $crate::clap::__entry!($crate::clap::ClapEntry<$plugin>)
    };
}

/// A CLAP entry point, as [`export_clap!`](crate::export_clap) exports and
/// [`clap_entry!`](crate::clap_entry) makes.
pub use clack_plugin::entry::EntryDescriptor;
#[doc(hidden)]
pub use clack_plugin::{clack_entry as __entry, clack_export_entry as __export_entry};

/// The CLAP entry point for plug-in `P`, which [`export_clap!`](crate::export_clap)
/// exports.
pub type ClapEntry<P> = SinglePluginEntry<ClapPlugin<P>>;

/// Plug-in `P` as clack sees it.
pub struct ClapPlugin<P>(PhantomData<fn() -> P>);

impl<P: Plugin> ClackPlugin for ClapPlugin<P> {
    type AudioProcessor<'a> = ClapProcessor<'a, P>;
    type Shared<'a> = ClapShared<'a, P>;
    type MainThread<'a> = ClapMainThread<'a, P>;

    fn declare_extensions(
        builder: &mut PluginExtensions<'_, Self>,
        shared: Option<&ClapShared<'_, P>>,
    ) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginLatency>()
            .register::<PluginTail>();
        if P::INFO.notes.input || P::INFO.notes.output {
            builder.register::<PluginNotePorts>();
        }
        if shared.is_some_and(|shared| shared.has_editor.load(Ordering::Acquire)) {
            builder.register::<PluginGui>();
        }
    }
}

impl<P: Plugin> DefaultPluginFactory for ClapPlugin<P> {
    fn get_descriptor() -> PluginDescriptor {
        let info = P::INFO;
        PluginDescriptor::new(info.id, info.name)
            .with_vendor(info.vendor)
            .with_version(info.version)
            .with_url(info.url)
            .with_description(info.description)
            .with_features(info.features.iter().copied())
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<ClapShared<'_, P>, ClackError> {
        Ok(ClapShared {
            has_editor: AtomicBool::new(false),
            _lifetime: PhantomData,
            _plugin: PhantomData,
        })
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a ClapShared<'a, P>,
    ) -> Result<ClapMainThread<'a, P>, ClackError> {
        let backend = ClapBackend {
            params: host.get_extension::<HostParams>(),
            state: host.get_extension::<HostState>(),
            // SAFETY: the handle lives only in this instance's `Host`, which
            // `ClapMainThread` disconnects when it's dropped, before the
            // instance ends; no call reaches the handle after that.
            host: unsafe { host.with_arbitrary_lifetime() },
        };
        let gui = host.get_extension::<HostGui>();
        let shared_host = host.shared();
        let link = Host::new(Box::new(backend));
        let mut plugin = P::new(link.clone());
        let values = Arc::new(ParamValues::new(plugin.params())?);
        let edits = link.connect(values.clone());
        let editor = plugin.editor().map(|editor| {
            let (cx, changes) = EditorContext::new(
                values.clone(),
                Box::new(ClapEditorHost {
                    // SAFETY: the handle lives only in this instance's
                    // editor context, which `ClapMainThread` closes when
                    // it's dropped, after destroying the editor's window and
                    // before the instance ends; no call reaches the handle
                    // after that.
                    host: unsafe { shared_host.with_arbitrary_lifetime() },
                    gui,
                }),
            );
            ClapEditor {
                editor: RefCell::new(editor),
                cx,
                changes: RefCell::new(changes),
            }
        });
        shared.has_editor.store(editor.is_some(), Ordering::Release);
        Ok(ClapMainThread {
            values,
            plugin: RefCell::new(plugin),
            host: link,
            edits: RefCell::new(Some(edits)),
            editor,
            _shared: PhantomData,
        })
    }
}

/// What every thread shares: whether the plug-in has an editor, which
/// decides whether the host sees the `gui` extension.
pub struct ClapShared<'a, P> {
    has_editor: AtomicBool,
    _lifetime: PhantomData<&'a ()>,
    _plugin: PhantomData<fn() -> P>,
}

impl<'a, P: Plugin> PluginShared<'a> for ClapShared<'a, P> {}

/// The CLAP host as a [`HostBackend`].
struct ClapBackend {
    host: HostMainThreadHandle<'static>,
    params: Option<HostParams>,
    state: Option<HostState>,
}

impl HostBackend for ClapBackend {
    fn request_flush(&self) {
        if let Some(params) = &self.params {
            params.request_flush(&self.host);
        }
    }

    fn mark_dirty(&self) {
        if let Some(state) = &self.state {
            state.mark_dirty(&self.host);
        }
    }

    fn request_restart(&self) {
        self.host.request_restart();
    }

    fn request_process(&self) {
        self.host.request_process();
    }

    fn request_callback(&self) {
        self.host.request_callback();
    }

    fn values_changed(&self) {
        if let Some(params) = &self.params {
            params.rescan(&self.host, ParamRescanFlags::VALUES);
        }
    }
}

/// The CLAP host as an [`EditorHost`], reached from the editor's window
/// thread: requesting a callback and a resize are thread-safe in CLAP, and
/// the editor says it's closed on the main thread.
struct ClapEditorHost {
    host: HostSharedHandle<'static>,
    gui: Option<HostGui>,
}

impl EditorHost for ClapEditorHost {
    fn request_callback(&self) {
        self.host.request_callback();
    }

    fn closed(&self) {
        if let Some(gui) = self.gui {
            gui.closed(&self.host, true);
        }
    }

    fn request_resize(&self, size: EditorSize) -> bool {
        self.gui.is_some_and(|gui| {
            gui.request_resize(&self.host, size.width, size.height)
                .is_ok()
        })
    }
}

/// A plug-in's editor, its link to the plug-in, and the main thread's end
/// of that link.
struct ClapEditor {
    editor: RefCell<Box<dyn Editor>>,
    cx: EditorContext,
    changes: RefCell<EditorChanges>,
}

/// The main thread's side: the plug-in, and the edit queue while the
/// instance is inactive.
pub struct ClapMainThread<'a, P: Plugin> {
    values: Arc<ParamValues>,
    plugin: RefCell<P>,
    host: Host,
    /// The queue of the plug-in's edits, here while inactive and with the
    /// processor while active.
    edits: RefCell<Option<Consumer<Edit>>>,
    /// The plug-in's editor, if it has one.
    editor: Option<ClapEditor>,
    _shared: PhantomData<&'a ClapShared<'a, P>>,
}

impl<P: Plugin> Drop for ClapMainThread<'_, P> {
    fn drop(&mut self) {
        if let Some(editor) = &self.editor {
            // The window goes first, so nothing on its thread is still
            // using the context when it's closed.
            editor.editor.borrow_mut().destroy();
            editor.cx.close();
        }
        self.host.disconnect();
    }
}

impl<'a, P: Plugin> PluginMainThread<'a, ClapShared<'a, P>> for ClapMainThread<'a, P> {
    fn on_main_thread(&self) {
        if let Some(editor) = &self.editor {
            if let Ok(mut changes) = editor.changes.try_borrow_mut() {
                changes.apply(&self.host);
            }
            if let Ok(mut editor) = editor.editor.try_borrow_mut() {
                editor.on_main_thread();
            }
        }
        if let Ok(mut plugin) = self.plugin.try_borrow_mut() {
            plugin.on_main_thread();
        }
    }
}

/// The audio thread's side: the processor.
pub struct ClapProcessor<'a, P: Plugin> {
    values: Arc<ParamValues>,
    processor: Option<P::Processor>,
    edits: Option<Consumer<Edit>>,
    /// The block's own edits, as events at frame 0; never grows past the
    /// capacity it's made with.
    first: Vec<Event>,
    _shared: PhantomData<&'a ClapShared<'a, P>>,
}

impl<'a, P: Plugin> PluginAudioProcessor<'a, ClapShared<'a, P>, ClapMainThread<'a, P>>
    for ClapProcessor<'a, P>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        main_thread: &ClapMainThread<'a, P>,
        _shared: &'a ClapShared<'a, P>,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, ClackError> {
        let config = ProcessConfig {
            sample_rate: audio_config.sample_rate,
            min_frames: audio_config.min_frames_count,
            max_frames: audio_config.max_frames_count,
        };
        let values = main_thread.values.clone();
        let processor = main_thread
            .plugin
            .try_borrow_mut()
            .map_err(|_| ClackError::Message("the plug-in is busy"))?
            .activate(&config, &values)?;
        let edits = main_thread
            .edits
            .try_borrow_mut()
            .ok()
            .and_then(|mut edits| edits.take());
        Ok(Self {
            first: Vec::with_capacity(Host::edit_capacity(&values)),
            values,
            processor: Some(processor),
            edits,
            _shared: PhantomData,
        })
    }

    fn process(
        &mut self,
        _process: Process<'_>,
        mut audio: Audio<'_>,
        events: ClackEvents<'_>,
    ) -> Result<ClackStatus, ClackError> {
        let Some(processor) = self.processor.as_mut() else {
            return Ok(ClackStatus::Sleep);
        };
        drain(
            &mut self.edits,
            &mut self.first,
            &self.values,
            events.output,
        );
        record(events.input, &self.values);

        let frames = audio.frames_count() as usize;
        let layout = P::INFO.audio;
        let mut channels = [std::ptr::null_mut(); MAX_CHANNELS];
        let mut count = 0;
        if layout.outputs > 0
            && let Some(mut pair) = audio.port_pair(0)
            && let Ok(paired) = pair.channels()
            && let Some(mut paired) = paired.into_f32()
        {
            for channel in paired.iter_mut() {
                let output = match channel {
                    ChannelPair::InputOutput(input, output) => {
                        output.copy_from_slice(input);
                        output
                    }
                    ChannelPair::InPlace(output) => output,
                    ChannelPair::OutputOnly(output) => {
                        output.fill(0.0);
                        output
                    }
                    ChannelPair::InputOnly(_) => continue,
                };
                if layout.inputs == 0 {
                    output.fill(0.0);
                }
                if count < MAX_CHANNELS.min(usize::from(layout.outputs)) {
                    channels[count] = output.as_mut_ptr();
                    count += 1;
                }
            }
        }
        // SAFETY: each pointer is one of the host's output channels for this
        // call, `frames` long, distinct from the others, and nothing else
        // touches it until `process` returns.
        let mut buffers = unsafe { Buffers::from_raw(channels, count, frames) };
        let source = ClapEvents {
            input: events.input,
            values: &self.values,
        };
        let events_in = Events::new(&self.first, &source);
        let mut sink = ClapSink {
            output: events.output,
            values: &self.values,
        };
        let mut cx = ProcessContext::new(&self.values, &mut sink);
        let status = processor.process(&mut buffers, &events_in, &mut cx);
        Ok(match status {
            ProcessStatus::Continue => ClackStatus::Continue,
            ProcessStatus::ContinueIfNotQuiet => ClackStatus::ContinueIfNotQuiet,
            ProcessStatus::Tail => ClackStatus::Tail,
            ProcessStatus::Sleep => ClackStatus::Sleep,
        })
    }

    fn deactivate(mut self, main_thread: &ClapMainThread<'a, P>) {
        if let Some(processor) = self.processor.take()
            && let Ok(mut plugin) = main_thread.plugin.try_borrow_mut()
        {
            plugin.deactivate(processor);
        }
        *main_thread.edits.borrow_mut() = self.edits.take();
    }

    fn reset(&mut self) {
        if let Some(processor) = self.processor.as_mut() {
            processor.reset();
        }
    }
}

/// Takes every edit waiting in `edits`: tells the host of each (the
/// gestures, and each value it should hear), and puts each value in
/// `first`, as an event at frame 0 for the processor.
fn drain(
    edits: &mut Option<Consumer<Edit>>,
    first: &mut Vec<Event>,
    values: &ParamValues,
    output: &mut OutputEvents<'_>,
) {
    first.clear();
    let Some(edits) = edits.as_mut() else { return };
    while let Ok(edit) = edits.pop() {
        match edit {
            Edit::Begin(id) => {
                let _ = output.try_push(ParamGestureBeginEvent::new(0, ClapId::new(id.0)));
            }
            Edit::End(id) => {
                let _ = output.try_push(ParamGestureEndEvent::new(0, ClapId::new(id.0)));
            }
            Edit::Value { id, value, echo } => {
                if echo && let Some(descriptor) = values.descriptor(id) {
                    let event = ParamValueEvent::new(
                        0,
                        ClapId::new(id.0),
                        Pckn::match_all(),
                        to_host(descriptor, value),
                    );
                    let _ = output.try_push(event);
                }
                // `first` holds as many as the queue, so this never grows it.
                if first.len() < first.capacity() {
                    first.push(Event::Param { time: 0, id, value });
                }
            }
        }
    }
}

/// Records each of the host's parameter changes in `values`.
fn record(input: &InputEvents<'_>, values: &ParamValues) {
    for event in input {
        if let Some(CoreEventSpace::ParamValue(event)) = event.as_core_event()
            && let Some(id) = event.param_id()
            && let Some(descriptor) = values.descriptor(ParamId(id.get()))
        {
            values.set(descriptor.id, from_host(descriptor, event.value()));
        }
    }
}

impl<P: Plugin> PluginAudioProcessorParams for ClapProcessor<'_, P> {
    fn flush(&mut self, input: &InputEvents<'_>, output: &mut OutputEvents<'_>) {
        drain(&mut self.edits, &mut self.first, &self.values, output);
        record(input, &self.values);
        let Some(processor) = self.processor.as_mut() else {
            return;
        };
        let source = ClapEvents {
            input,
            values: &self.values,
        };
        let events = Events::new(&self.first, &source);
        let mut sink = ClapSink {
            output,
            values: &self.values,
        };
        let mut cx = ProcessContext::new(&self.values, &mut sink);
        processor.flush(&events, &mut cx);
    }
}

impl<P: Plugin> PluginTailImpl for ClapProcessor<'_, P> {
    fn get(&self) -> TailLength {
        let tail = self.processor.as_ref().map_or(Tail::None, Processor::tail);
        TailLength::from_raw(match tail {
            Tail::None => 0,
            // u32::MAX is CLAP's "infinite".
            Tail::Frames(frames) => frames.min(u32::MAX - 1),
            Tail::Infinite => u32::MAX,
        })
    }
}

/// The host's events, read as the kit's as the processor asks for them.
struct ClapEvents<'a> {
    input: &'a InputEvents<'a>,
    values: &'a ParamValues,
}

impl EventSource for ClapEvents<'_> {
    fn len(&self) -> usize {
        self.input.len() as usize
    }

    fn get(&self, index: usize) -> Option<Event> {
        let event = self.input.get(u32::try_from(index).ok()?)?;
        let time = event.header().time();
        Some(match event.as_core_event()? {
            CoreEventSpace::NoteOn(e) => Event::NoteOn {
                time,
                note: note(e.pckn())?,
                velocity: e.velocity() as f32,
            },
            CoreEventSpace::NoteOff(e) => Event::NoteOff {
                time,
                note: note(e.pckn())?,
                velocity: e.velocity() as f32,
            },
            CoreEventSpace::NoteChoke(e) => Event::NoteChoke {
                time,
                note: note(e.pckn())?,
            },
            CoreEventSpace::ParamValue(e) => {
                let descriptor = self.values.descriptor(ParamId(e.param_id()?.get()))?;
                Event::Param {
                    time,
                    id: descriptor.id,
                    value: from_host(descriptor, e.value()),
                }
            }
            CoreEventSpace::Midi(e) => midi(time, e.port_index(), e.data())?,
            _ => return None,
        })
    }
}

/// A CLAP note's port, channel, key and ID, each `None` for "every"; `None`
/// if one is out of range.
fn note(pckn: Pckn) -> Option<Note> {
    fn field<T: Copy, U: TryFrom<T>>(value: Match<T>) -> Option<Option<U>> {
        match value.into_specific() {
            None => Some(None),
            Some(v) => U::try_from(v).ok().map(Some),
        }
    }
    let channel = field::<u16, u8>(pckn.channel)?.filter(|c| *c < 16);
    let key = field::<u16, u8>(pckn.key)?.filter(|k| *k < 128);
    if pckn.channel.is_specific() && channel.is_none() || pckn.key.is_specific() && key.is_none() {
        return None;
    }
    Some(Note {
        port: field::<u16, u16>(pckn.port_index)?,
        channel,
        key,
        id: field::<u32, u32>(pckn.note_id)?,
    })
}

/// A MIDI 1.0 note-on or note-off as the kit's event; other messages are
/// passed over.
fn midi(time: u32, port: u16, [status, key, velocity]: [u8; 3]) -> Option<Event> {
    let note = Note {
        port: Some(port),
        channel: Some(status & 0x0f),
        key: Some(key & 0x7f),
        id: None,
    };
    let velocity = f32::from(velocity & 0x7f) / 127.0;
    match status & 0xf0 {
        0x90 if velocity > 0.0 => Some(Event::NoteOn {
            time,
            note,
            velocity,
        }),
        0x80 | 0x90 => Some(Event::NoteOff {
            time,
            note,
            velocity,
        }),
        _ => None,
    }
}

/// The host's output queue, as an [`EventSink`].
struct ClapSink<'a, 'b> {
    output: &'a mut OutputEvents<'b>,
    values: &'a ParamValues,
}

impl EventSink for ClapSink<'_, '_> {
    fn push(&mut self, event: OutputEvent) -> bool {
        let pushed = match event {
            OutputEvent::GestureBegin { time, id } => self
                .output
                .try_push(ParamGestureBeginEvent::new(time, ClapId::new(id.0))),
            OutputEvent::GestureEnd { time, id } => self
                .output
                .try_push(ParamGestureEndEvent::new(time, ClapId::new(id.0))),
            OutputEvent::Param { time, id, value } => {
                let Some(descriptor) = self.values.descriptor(id) else {
                    return false;
                };
                self.output.try_push(ParamValueEvent::new(
                    time,
                    ClapId::new(id.0),
                    Pckn::match_all(),
                    to_host(descriptor, value),
                ))
            }
            OutputEvent::NoteEnd { time, note } => {
                let pckn = Pckn::new(
                    note.port.map_or(Match::All, Match::Specific),
                    note.channel
                        .map_or(Match::All, |c| Match::Specific(u16::from(c))),
                    note.key
                        .map_or(Match::All, |k| Match::Specific(u16::from(k))),
                    note.id.map_or(Match::All, Match::Specific),
                );
                self.output.try_push(NoteEndEvent::new(time, pckn))
            }
        };
        pushed.is_ok()
    }
}

/// `value`, in the parameter's own units, as the host sees it.
fn to_host(descriptor: &ParamDescriptor, value: f64) -> f64 {
    if descriptor.is_stepped() {
        descriptor.clamp(value)
    } else {
        descriptor.normalize(value)
    }
}

/// The host's `value` in the parameter's own units.
fn from_host(descriptor: &ParamDescriptor, value: f64) -> f64 {
    if descriptor.is_stepped() {
        descriptor.clamp(value)
    } else {
        descriptor.denormalize(value)
    }
}

impl<P: Plugin> PluginMainThreadParams for ClapMainThread<'_, P> {
    fn count(&self) -> u32 {
        u32::try_from(self.values.len()).unwrap_or(u32::MAX)
    }

    fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter<'_>) {
        let Some(descriptor) = self.values.descriptors().get(param_index as usize) else {
            return;
        };
        let mut flags = ParamInfoFlags::IS_AUTOMATABLE;
        let (min, max) = if descriptor.is_stepped() {
            flags |= ParamInfoFlags::IS_STEPPED;
            if matches!(descriptor.kind, ParamKind::Choice { .. }) {
                flags |= ParamInfoFlags::IS_ENUM;
            }
            (descriptor.min(), descriptor.max())
        } else {
            (0.0, 1.0)
        };
        info.set(&ParamInfo {
            id: ClapId::new(descriptor.id.0),
            flags,
            cookie: Cookie::empty(),
            name: descriptor.name.as_bytes(),
            module: b"",
            min_value: min,
            max_value: max,
            default_value: to_host(descriptor, descriptor.default),
        });
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        let descriptor = self.values.descriptor(ParamId(param_id.get()))?;
        Some(to_host(descriptor, self.values.get(descriptor.id)?))
    }

    fn value_to_text(
        &self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter<'_>,
    ) -> std::fmt::Result {
        let descriptor = self
            .values
            .descriptor(ParamId(param_id.get()))
            .ok_or(std::fmt::Error)?;
        writer.write_str(&descriptor.format(from_host(descriptor, value)))
    }

    fn text_to_value(&self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let descriptor = self.values.descriptor(ParamId(param_id.get()))?;
        let value = descriptor.parse(text.to_str().ok()?)?;
        Some(to_host(descriptor, value))
    }

    fn flush(&self, input: &InputEvents<'_>, output: &mut OutputEvents<'_>) {
        // While active, the processor's flush takes the edits.
        if let Ok(mut edits) = self.edits.try_borrow_mut() {
            let mut none = Vec::new();
            drain(&mut edits, &mut none, &self.values, output);
        }
        record(input, &self.values);
    }
}

impl<P: Plugin> PluginStateImpl for ClapMainThread<'_, P> {
    fn save(&self, output: &mut OutputStream<'_>) -> Result<(), ClackError> {
        let own = self
            .plugin
            .try_borrow()
            .map_err(|_| ClackError::Message("the plug-in is busy"))?
            .save_state();
        output.write_all(&state::encode(&self.values, &own))?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream<'_>) -> Result<(), ClackError> {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes)?;
        let decoded = state::decode(&bytes)?;
        let mut plugin = self
            .plugin
            .try_borrow_mut()
            .map_err(|_| ClackError::Message("the plug-in is busy"))?;
        let before = state::encode(&self.values, &[]);
        state::apply(&decoded, &self.values);
        if let Err(error) = plugin.load_state(decoded.own) {
            // A state the plug-in refuses changes nothing.
            if let Ok(before) = state::decode(&before) {
                state::apply(&before, &self.values);
            }
            return Err(error.into());
        }
        drop(plugin);
        // While active, the processor takes the loaded values at the start
        // of its next block.
        let active = self.edits.try_borrow().is_ok_and(|edits| edits.is_none());
        self.host.state_loaded(active)?;
        Ok(())
    }
}

impl<P: Plugin> PluginLatencyImpl for ClapMainThread<'_, P> {
    fn get(&self) -> u32 {
        self.plugin
            .try_borrow()
            .map_or(0, |plugin| plugin.latency())
    }
}

impl<P: Plugin> PluginAudioPortsImpl for ClapMainThread<'_, P> {
    fn count(&self, is_input: bool) -> u32 {
        let layout = P::INFO.audio;
        u32::from(
            if is_input {
                layout.inputs
            } else {
                layout.outputs
            } > 0,
        )
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter<'_>) {
        let layout = P::INFO.audio;
        let channels = if is_input {
            layout.inputs
        } else {
            layout.outputs
        };
        if index != 0 || channels == 0 {
            return;
        }
        let channel_count = u32::from(channels.min(MAX_CHANNELS as u16));
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: b"Main",
            channel_count,
            flags: AudioPortFlags::IS_MAIN,
            port_type: AudioPortType::from_channel_count(channel_count),
            in_place_pair: (layout.inputs == layout.outputs).then(|| ClapId::new(0)),
        });
    }
}

impl<P: Plugin> PluginNotePortsImpl for ClapMainThread<'_, P> {
    fn count(&self, is_input: bool) -> u32 {
        let notes = P::INFO.notes;
        u32::from(if is_input { notes.input } else { notes.output })
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter<'_>) {
        if index != 0 || PluginNotePortsImpl::count(self, is_input) == 0 {
            return;
        }
        let supported_dialects = if is_input {
            NoteDialects::CLAP | NoteDialects::MIDI
        } else {
            NoteDialects::CLAP
        };
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"Notes",
            supported_dialects,
            preferred_dialect: Some(NoteDialect::Clap),
        });
    }
}

/// The kit's name for CLAP's window API `api`, if it's one the kit knows.
fn window_api(api: GuiApiType<'_>) -> Option<WindowApi> {
    if api == GuiApiType::COCOA {
        Some(WindowApi::AppKit)
    } else if api == GuiApiType::WIN32 {
        Some(WindowApi::Win32)
    } else if api == GuiApiType::X11 {
        Some(WindowApi::X11)
    } else {
        None
    }
}

/// CLAP's name for the kit's window API `api`.
fn gui_api(api: WindowApi) -> GuiApiType<'static> {
    match api {
        WindowApi::AppKit => GuiApiType::COCOA,
        WindowApi::Win32 => GuiApiType::WIN32,
        WindowApi::X11 => GuiApiType::X11,
    }
}

/// The host's window as the kit's.
#[allow(
    clippy::useless_conversion,
    reason = "an X11 window ID is a `c_ulong`, which is 32 bits on Windows"
)]
fn parent_window(window: &ClapWindow<'_, '_>) -> Option<ParentWindow> {
    match window_api(window.api_type())? {
        WindowApi::AppKit => NonNull::new(window.as_cocoa_nsview()?).map(ParentWindow::AppKit),
        WindowApi::Win32 => NonNull::new(window.as_win32_hwnd()?).map(ParentWindow::Win32),
        WindowApi::X11 => Some(ParentWindow::X11(u64::from(window.as_x11_handle()?))),
    }
}

impl<P: Plugin> ClapMainThread<'_, P> {
    /// Runs `f` on the editor, if there is one and it isn't already busy
    /// (a host may call back into the plug-in from inside a request the
    /// editor makes).
    fn with_editor<T>(&self, f: impl FnOnce(&mut dyn Editor, &EditorContext) -> T) -> Option<T> {
        let editor = self.editor.as_ref()?;
        let mut inner = editor.editor.try_borrow_mut().ok()?;
        Some(f(inner.as_mut(), &editor.cx))
    }
}

/// `true` as `Ok`, and `false` as the error CLAP's `false` stands for.
fn refused(ok: bool, what: &'static str) -> Result<(), ClackError> {
    if ok {
        Ok(())
    } else {
        Err(ClackError::Message(what))
    }
}

impl<P: Plugin> PluginGuiImpl for ClapMainThread<'_, P> {
    fn is_api_supported(&self, configuration: GuiConfiguration<'_>) -> bool {
        window_api(configuration.api_type).is_some_and(|api| {
            self.with_editor(|editor, _| editor.supports(api, configuration.is_floating))
                .unwrap_or(false)
        })
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        let api = WindowApi::native()?;
        let embedded = self.with_editor(|editor, _| editor.supports(api, false))?;
        Some(GuiConfiguration {
            api_type: gui_api(api),
            is_floating: !embedded,
        })
    }

    fn create(&self, configuration: GuiConfiguration<'_>) -> Result<(), ClackError> {
        let api = window_api(configuration.api_type)
            .ok_or(ClackError::Message("the kit doesn't know this window API"))?;
        self.with_editor(|editor, cx| editor.create(api, configuration.is_floating, cx.clone()))
            .ok_or(ClackError::Message("the editor is busy"))??;
        Ok(())
    }

    fn destroy(&self) {
        self.with_editor(|editor, _| editor.destroy());
        // Gestures the window started reach the host before it closes.
        if let Some(editor) = &self.editor
            && let Ok(mut changes) = editor.changes.try_borrow_mut()
        {
            changes.apply(&self.host);
        }
    }

    fn set_scale(&self, scale: f64) -> Result<(), ClackError> {
        let applied = self.with_editor(|editor, _| editor.set_scale(scale));
        refused(applied == Some(true), "the editor finds its own scale")
    }

    fn get_size(&self) -> Option<GuiSize> {
        let size = self.with_editor(|editor, _| editor.size())?;
        Some(GuiSize {
            width: size.width,
            height: size.height,
        })
    }

    fn can_resize(&self) -> bool {
        self.with_editor(|editor, _| editor.resize_hints().is_some())
            .unwrap_or(false)
    }

    fn get_resize_hints(&self) -> Option<GuiResizeHints> {
        let hints = self.with_editor(|editor, _| editor.resize_hints())??;
        Some(GuiResizeHints {
            can_resize_horizontally: hints.horizontal,
            can_resize_vertically: hints.vertical,
            strategy: match hints.aspect_ratio {
                Some((width, height)) => AspectRatioStrategy::Preserve { width, height },
                None => AspectRatioStrategy::Disregard,
            },
        })
    }

    fn adjust_size(&self, size: GuiSize) -> Option<GuiSize> {
        let size = EditorSize::new(size.width, size.height);
        let adjusted = self.with_editor(|editor, _| editor.adjust_size(size))?;
        Some(GuiSize {
            width: adjusted.width,
            height: adjusted.height,
        })
    }

    fn set_size(&self, size: GuiSize) -> Result<(), ClackError> {
        let size = EditorSize::new(size.width, size.height);
        let applied = self.with_editor(|editor, _| editor.set_size(size));
        refused(applied == Some(true), "the editor can't take this size")
    }

    fn set_parent(&self, window: ClapWindow<'_, '_>) -> Result<(), ClackError> {
        let parent = parent_window(&window).ok_or(ClackError::Message(
            "the host's window isn't one the kit knows",
        ))?;
        self.with_editor(|editor, _| editor.set_parent(parent))
            .ok_or(ClackError::Message("the editor is busy"))??;
        Ok(())
    }

    fn set_transient(&self, _window: ClapWindow<'_, '_>) -> Result<(), ClackError> {
        Err(ClackError::Message(
            "a floating editor doesn't follow the host's window",
        ))
    }

    fn suggest_title(&self, title: &str) {
        self.with_editor(|editor, _| editor.set_title(title));
    }

    fn show(&self) -> Result<(), ClackError> {
        self.with_editor(|editor, _| editor.show())
            .ok_or(ClackError::Message("the editor is busy"))??;
        Ok(())
    }

    fn hide(&self) -> Result<(), ClackError> {
        self.with_editor(|editor, _| editor.hide())
            .ok_or(ClackError::Message("the editor is busy"))??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_values_are_travel_and_stepped_ones_are_whole() {
        let gain = ParamDescriptor::gain(ParamId(1), "Gain", 6.0, 0.0);
        assert_eq!(to_host(&gain, 0.0), 0.0);
        assert_eq!(from_host(&gain, 0.0), 0.0);
        assert_eq!(to_host(&gain, lspk_params::db_to_gain(6.0)), 1.0);
        let unity = to_host(&gain, 1.0);
        assert!((from_host(&gain, unity) - 1.0).abs() < 1e-12);
        let mode = ParamDescriptor::choice(ParamId(2), "Mode", &["A", "B", "C"], 0);
        assert_eq!(to_host(&mode, 2.0), 2.0);
        assert_eq!(from_host(&mode, 1.4), 1.0);
    }

    #[test]
    fn notes_carry_wildcards_and_refuse_out_of_range_fields() {
        let all = note(Pckn::match_all()).expect("a note");
        assert_eq!(all, Note::default());
        let one = note(Pckn::new(0u16, 3u16, 60u16, 9u32)).expect("a note");
        assert_eq!(
            one,
            Note {
                port: Some(0),
                channel: Some(3),
                key: Some(60),
                id: Some(9)
            }
        );
        assert_eq!(note(Pckn::new(0u16, 3u16, 200u16, Match::All)), None);
        assert_eq!(note(Pckn::new(0u16, 16u16, 60u16, Match::All)), None);
    }

    #[test]
    fn window_apis_match_clap_s() {
        for api in [WindowApi::AppKit, WindowApi::Win32, WindowApi::X11] {
            assert_eq!(window_api(gui_api(api)), Some(api));
            assert_eq!(gui_api(api).uses_logical_size(), api.uses_logical_size());
        }
        assert_eq!(window_api(GuiApiType::WAYLAND), None);
        let window = ClapWindow::from_x11_handle(42);
        assert_eq!(parent_window(&window), Some(ParentWindow::X11(42)));
    }

    #[test]
    fn midi_notes_become_note_events() {
        assert!(matches!(
            midi(5, 0, [0x91, 60, 127]),
            Some(Event::NoteOn { time: 5, velocity, note: Note { channel: Some(1), key: Some(60), .. } }) if velocity == 1.0
        ));
        assert!(matches!(
            midi(0, 0, [0x90, 60, 0]),
            Some(Event::NoteOff { .. })
        ));
        assert!(matches!(
            midi(0, 0, [0x80, 60, 64]),
            Some(Event::NoteOff { .. })
        ));
        assert_eq!(midi(0, 0, [0xb0, 7, 100]), None);
    }
}
