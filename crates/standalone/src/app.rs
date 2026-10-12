//! The standalone host: one plug-in instance on the main thread, its
//! processor in an audio device's callback, and the supervision that keeps
//! it playing through device changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use lspk_plugin::editor::{EditorChanges, EditorHost};
use lspk_plugin::host::Edit;
use lspk_plugin::{
    Editor, EditorContext, EditorKey, EditorSize, Host, HostBackend, KeyboardFocus, ParamValues,
    ParentWindow, Plugin, PluginError, ProcessConfig, state,
};
use rtrb::Consumer;

use crate::audio::{
    AudioBackend, AudioCallback, AudioStream, DeviceInfo, Failure, StreamInfo, StreamRequest,
};
use crate::engine::{Engine, EngineCallback, Meter, MidiMessage, Slot};
use crate::midi::{self, MidiSender};
use crate::settings::{self, Settings};

/// How long a stream may go without a callback before it's reopened.
const STALL: Duration = Duration::from_secs(2);
/// How long to wait between attempts to open a device.
const RETRY: Duration = Duration::from_secs(1);

/// What the plug-in has asked of its host since the last poll.
#[derive(Debug, Default)]
struct Requests {
    callback: AtomicBool,
    restart: AtomicBool,
    dirty: AtomicBool,
    editor_callback: AtomicBool,
    editor_closed: AtomicBool,
    /// The size the editor last asked for, width in the high half and
    /// height in the low; 0 for none.
    editor_resize: AtomicU64,
    editor_focused: AtomicBool,
    editor_typing: AtomicBool,
}

/// The standalone app as the host of the plug-in's editor, from whichever
/// thread its window runs on.
struct EditorBackend {
    requests: Arc<Requests>,
    keys: Sender<EditorKey>,
}

impl EditorHost for EditorBackend {
    fn request_callback(&self) {
        self.requests.editor_callback.store(true, Ordering::Release);
    }

    fn request_resize(&self, size: EditorSize) -> bool {
        // The app's window follows the editor, at its next frame.
        let packed = (u64::from(size.width) << 32) | u64::from(size.height);
        self.requests.editor_resize.store(packed, Ordering::Release);
        true
    }

    fn closed(&self) {
        self.requests.editor_closed.store(true, Ordering::Release);
    }

    fn key(&self, key: EditorKey) {
        // The receiver lives as long as the app.
        let _ = self.keys.send(key);
    }

    fn keyboard_focus(&self, focus: KeyboardFocus) {
        self.requests
            .editor_typing
            .store(focus.typing, Ordering::Release);
        self.requests
            .editor_focused
            .store(focus.focused, Ordering::Release);
    }
}

/// The standalone app as the plug-in's host.
struct Backend(Arc<Requests>);

impl HostBackend for Backend {
    fn request_flush(&self) {
        // The audio loop takes edits every buffer, and the main thread
        // while no stream runs.
    }

    fn mark_dirty(&self) {
        self.0.dirty.store(true, Ordering::Relaxed);
    }

    fn request_restart(&self) {
        self.0.restart.store(true, Ordering::Relaxed);
    }

    fn request_process(&self) {
        // The audio loop wakes a sleeping processor on any event.
    }

    fn request_callback(&self) {
        self.0.callback.store(true, Ordering::Relaxed);
    }

    fn values_changed(&self) {
        // The window reads the values every frame.
    }

    fn waker(&self) -> Option<lspk_plugin::host::Waker> {
        let requests = Arc::downgrade(&self.0);
        Some(Arc::new(move || {
            if let Some(requests) = requests.upgrade() {
                requests.callback.store(true, Ordering::Relaxed);
            }
        }))
    }

    fn host_name(&self) -> Option<String> {
        Some("Standalone".to_owned())
    }
}

/// Where the audio stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not started, or stopped.
    Stopped,
    /// Playing, as the stream reports.
    Running(StreamInfo),
    /// Not playing, and why; the app tries again every second.
    Waiting(String),
}

/// A running stream and the engine it borrows.
struct Running<Pr: lspk_plugin::Processor> {
    stream: Box<dyn AudioStream>,
    slot: Slot<Engine<Pr>>,
    callbacks: u64,
    progressed: Instant,
    checked: Instant,
}

/// One plug-in instance, run as an app: its processor plays through an
/// [`AudioBackend`], its parameters and state are the main thread's, and
/// [`poll`](Self::poll), called often on the main thread, keeps the audio
/// going through device changes.
pub struct Standalone<P: Plugin> {
    plugin: P,
    host: Host,
    values: Arc<ParamValues>,
    requests: Arc<Requests>,
    /// The edit and MIDI queues while no stream runs.
    idle: Option<(Consumer<Edit>, Consumer<MidiMessage>)>,
    midi: MidiSender,
    backend: Box<dyn AudioBackend>,
    running: Option<Running<P::Processor>>,
    settings: Settings,
    folder: Option<PathBuf>,
    meter: Arc<Meter>,
    status: Status,
    retry_at: Option<Instant>,
    stall: Duration,
    retry: Duration,
    /// The plug-in's editor, if it has one (or the generic one, once
    /// embedded), and whether its window is open.
    editor: Option<Box<dyn Editor>>,
    has_own_editor: bool,
    editor_open: bool,
    /// The keys the editor passed on, in order.
    editor_keys: Receiver<EditorKey>,
    /// The link the editor, or the app's own parameter view, edits through.
    view: EditorContext,
    changes: EditorChanges,
    /// The state as last saved in the folder.
    saved: Option<Vec<u8>>,
}

impl<P: Plugin> Standalone<P> {
    /// A new instance of `P` playing through `backend` with `settings`,
    /// not yet started. With a `folder`, setting changes are saved there,
    /// and [`shutdown`](Self::shutdown) saves the plug-in's state.
    ///
    /// # Errors
    ///
    /// The plug-in's parameters break a rule.
    pub fn new(
        backend: Box<dyn AudioBackend>,
        settings: Settings,
        folder: Option<PathBuf>,
    ) -> Result<Self, PluginError> {
        let requests = Arc::new(Requests::default());
        let host = Host::new(Box::new(Backend(requests.clone())));
        let mut plugin = P::new(host.clone());
        let values = Arc::new(ParamValues::for_plugin(&plugin)?);
        let edits = host.connect(values.clone());
        let (midi, midi_in) = midi::queue();
        let editor = plugin.editor();
        let (keys, editor_keys) = channel();
        let (view, changes) = EditorContext::new(
            values.clone(),
            Box::new(EditorBackend {
                requests: requests.clone(),
                keys,
            }),
        );
        Ok(Self {
            plugin,
            host,
            values,
            requests,
            idle: Some((edits, midi_in)),
            midi,
            backend,
            running: None,
            settings,
            folder,
            meter: Arc::new(Meter::default()),
            status: Status::Stopped,
            retry_at: None,
            stall: STALL,
            retry: RETRY,
            has_own_editor: editor.is_some(),
            editor,
            editor_open: false,
            editor_keys,
            view,
            changes,
            saved: None,
        })
    }

    /// Changes how long a stream may go without a callback before it's
    /// reopened (2 s), and how long to wait between attempts to open a
    /// device (1 s). For tests.
    pub fn set_timing(&mut self, stall: Duration, retry: Duration) {
        self.stall = stall;
        self.retry = retry;
    }

    /// The plug-in.
    pub fn plugin(&mut self) -> &mut P {
        &mut self.plugin
    }

    /// The plug-in's link to this host, through which a window edits its
    /// parameters as gestures.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Every parameter's descriptor and value now.
    #[must_use]
    pub fn values(&self) -> &Arc<ParamValues> {
        &self.values
    }

    /// Where MIDI goes in: the MIDI inputs and the computer keyboard.
    #[must_use]
    pub fn midi(&self) -> &MidiSender {
        &self.midi
    }

    /// The output's level meter.
    #[must_use]
    pub fn meter(&self) -> &Arc<Meter> {
        &self.meter
    }

    /// The settings now.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Where the audio stands.
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// How many callbacks the running stream has had.
    #[must_use]
    pub fn callbacks(&self) -> u64 {
        self.running
            .as_ref()
            .map_or(0, |r| r.stream.health().callbacks())
    }

    /// The audio backend's name.
    #[must_use]
    pub fn backend_name(&self) -> String {
        self.backend.name()
    }

    /// The devices the backend has now.
    ///
    /// # Errors
    ///
    /// Why it couldn't list them.
    pub fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        self.backend.devices()
    }

    /// Whether the plug-in takes audio in.
    #[must_use]
    pub fn has_input() -> bool {
        P::INFO.audio.inputs > 0
    }

    /// Whether the plug-in takes notes.
    #[must_use]
    pub fn takes_notes() -> bool {
        P::INFO.notes.input
    }

    /// Whether the plug-in has an editor of its own. One without gets the
    /// kit's generic editor in the window.
    #[must_use]
    pub fn has_editor(&self) -> bool {
        self.has_own_editor
    }

    /// Whether the editor is open.
    #[must_use]
    pub fn editor_is_open(&self) -> bool {
        self.editor_open
    }

    /// Opens the plug-in's editor inside `parent`, the app's window, and
    /// shows it; a plug-in without an editor gets the kit's generic one.
    /// `scale` is the window's scale factor, which the editor takes where
    /// sizes are pixels (Windows and X11). Returns the editor's size.
    ///
    /// # Errors
    ///
    /// The editor can't open inside `parent`, or its window didn't.
    pub fn embed_editor(&mut self, parent: ParentWindow, scale: f64) -> Result<EditorSize, String> {
        let api = parent.api();
        if self.editor_open {
            self.close_editor();
        }
        let values = &self.values;
        let editor = self.editor.get_or_insert_with(|| {
            Box::new(lspk_ui::generic_editor(P::INFO.name, values.descriptors()))
        });
        if !editor.supports(api, false) {
            return Err("the editor can't open inside the app's window".to_owned());
        }
        self.requests.editor_closed.store(false, Ordering::Release);
        self.requests.editor_resize.store(0, Ordering::Release);
        editor
            .create(api, false, self.view.clone())
            .map_err(|e| e.to_string())?;
        if !api.uses_logical_size() {
            editor.set_scale(scale);
        }
        if let Err(error) = editor.set_parent(parent).and_then(|()| editor.show()) {
            editor.destroy();
            return Err(error.to_string());
        }
        self.editor_open = true;
        Ok(editor.size())
    }

    /// Closes the editor's window, if it's open.
    pub fn close_editor(&mut self) {
        if self.editor_open
            && let Some(editor) = &mut self.editor
        {
            editor.destroy();
        }
        self.editor_open = false;
        self.requests.editor_focused.store(false, Ordering::Release);
        self.requests.editor_typing.store(false, Ordering::Release);
    }

    /// The open editor's size, in its window system's units.
    #[must_use]
    pub fn editor_size(&self) -> Option<EditorSize> {
        self.editor
            .as_ref()
            .filter(|_| self.editor_open)
            .map(|editor| editor.size())
    }

    /// Whether the open editor can be resized.
    #[must_use]
    pub fn editor_resizable(&self) -> bool {
        self.editor
            .as_ref()
            .filter(|_| self.editor_open)
            .and_then(|editor| editor.resize_hints())
            .is_some_and(|hints| hints.horizontal || hints.vertical)
    }

    /// Resizes the open editor to the size nearest `size` it can take, as
    /// when the person resizes the window; returns the size it has after.
    pub fn fit_editor(&mut self, size: EditorSize) -> Option<EditorSize> {
        let editor = self.editor.as_mut().filter(|_| self.editor_open)?;
        if editor.resize_hints().is_some() {
            let adjusted = editor.adjust_size(size);
            if adjusted != editor.size() {
                editor.set_size(adjusted);
            }
        }
        Some(editor.size())
    }

    /// Tells the open editor the window's scale factor changed, where
    /// sizes are pixels (Windows and X11).
    pub fn set_editor_scale(&mut self, scale: f64) {
        if let Some(editor) = self.editor.as_mut().filter(|_| self.editor_open)
            && lspk_plugin::WindowApi::native().is_some_and(|api| !api.uses_logical_size())
        {
            editor.set_scale(scale);
        }
    }

    /// The size the editor last asked its window to take, if it has asked
    /// since this was last called.
    #[must_use]
    pub fn take_editor_resize(&self) -> Option<EditorSize> {
        let packed = self.requests.editor_resize.swap(0, Ordering::AcqRel);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "each half holds a 32-bit size"
        )]
        (packed != 0).then(|| EditorSize::new((packed >> 32) as u32, packed as u32))
    }

    /// Where the editor stands with the keyboard now.
    #[must_use]
    pub fn editor_focus(&self) -> KeyboardFocus {
        KeyboardFocus {
            focused: self.editor_open && self.requests.editor_focused.load(Ordering::Acquire),
            typing: self.requests.editor_typing.load(Ordering::Acquire),
        }
    }

    /// The keys the editor has passed on since this was last called, in
    /// order.
    pub fn take_editor_keys(&self) -> Vec<EditorKey> {
        self.editor_keys.try_iter().collect()
    }

    /// The link a window draws the plug-in's parameters through: every
    /// value now, and changes that reach the plug-in as gestures, as from
    /// the plug-in's own editor.
    #[must_use]
    pub fn view(&self) -> &EditorContext {
        &self.view
    }

    /// Opens the audio device and starts playing. If it can't, the status
    /// says why and [`poll`](Self::poll) tries again every second.
    pub fn start(&mut self) {
        if self.running.is_some() {
            return;
        }
        match self.open() {
            Ok(()) => self.retry_at = None,
            Err(why) => {
                self.status = Status::Waiting(why);
                self.retry_at = Some(Instant::now() + self.retry);
            }
        }
    }

    /// Stops playing and deactivates the processor.
    pub fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            drop(running.stream);
            self.reclaim(&running.slot);
        }
        self.status = Status::Stopped;
        self.retry_at = None;
    }

    /// Stops and starts again, with the settings as they are now.
    pub fn restart(&mut self) {
        self.stop();
        self.start();
    }

    fn open(&mut self) -> Result<(), String> {
        let (edits, midi) = self.idle.take().ok_or("the audio is already running")?;
        let request = StreamRequest {
            output: self.settings.output.clone(),
            input: self.settings.input.clone(),
            wants_input: Self::has_input(),
            sample_rate: self.settings.sample_rate,
            buffer_size: self.settings.buffer_size,
        };
        let layout = P::INFO.audio;
        let mut queues = Some((edits, midi));
        let mut lent = None;
        let plugin = &mut self.plugin;
        let values = &self.values;
        let meter = &self.meter;
        let mut make = |info: &StreamInfo| -> Result<Box<dyn AudioCallback>, String> {
            let config = ProcessConfig::new(f64::from(info.sample_rate), info.max_frames);
            let (edits, midi) = queues.take().ok_or("the stream asked for two callbacks")?;
            let processor = match plugin.activate(&config, values) {
                Ok(processor) => processor,
                Err(error) => {
                    queues = Some((edits, midi));
                    return Err(format!("the plug-in didn't start: {error}"));
                }
            };
            let engine = Engine::new(
                processor,
                values.clone(),
                edits,
                midi,
                (layout.inputs, layout.outputs),
                info.max_frames,
                meter.clone(),
            );
            let slot = Slot::new(engine);
            lent = Some(slot.clone());
            Ok(Box::new(EngineCallback::new(slot)))
        };
        let opened = self.backend.open(&request, &mut make);
        drop(make);
        match opened {
            Ok(stream) => {
                let slot = lent.ok_or("the stream opened without a callback")?;
                self.status = Status::Running(stream.info().clone());
                let now = Instant::now();
                self.running = Some(Running {
                    stream,
                    slot,
                    callbacks: 0,
                    progressed: now,
                    checked: now,
                });
                Ok(())
            }
            Err(why) => {
                match (lent, queues) {
                    (Some(slot), _) => self.reclaim(&slot),
                    (None, Some(queues)) => self.idle = Some(queues),
                    (None, None) => {}
                }
                Err(why)
            }
        }
    }

    /// Takes the engine back from a stream that has stopped: deactivates
    /// the processor and keeps the queues for the next.
    fn reclaim(&mut self, slot: &Slot<Engine<P::Processor>>) {
        if let Some(engine) = slot.take() {
            let (processor, edits, midi) = engine.into_parts();
            self.plugin.deactivate(processor);
            self.idle = Some((edits, midi));
        }
    }

    /// Does the main thread's work; call it every few tens of
    /// milliseconds. It answers the plug-in's requests, takes edits and
    /// MIDI while no stream runs, and keeps the audio going: a stream whose
    /// device went away, changed or stopped calling back is reopened (on
    /// the default device if the chosen one is gone, and back on the chosen
    /// one when it returns), and one that couldn't open is tried again.
    pub fn poll(&mut self) {
        self.changes.apply(&self.host);
        if self.requests.editor_closed.swap(false, Ordering::Acquire) {
            self.close_editor();
        }
        if self.requests.editor_callback.swap(false, Ordering::Acquire)
            && self.editor_open
            && let Some(editor) = &mut self.editor
        {
            editor.on_main_thread();
        }
        if self.requests.callback.swap(false, Ordering::Relaxed) {
            self.plugin.on_main_thread();
        }
        if self.requests.restart.swap(false, Ordering::Relaxed) {
            self.restart();
        }
        if let Some((edits, midi)) = &mut self.idle {
            // The values are already recorded; there's no processor to tell.
            while edits.pop().is_ok() {}
            while midi.pop().is_ok() {}
        }
        let now = Instant::now();
        if let Some(running) = &mut self.running {
            let health = running.stream.health();
            let failure = health.failure();
            let callbacks = health.callbacks();
            if callbacks != running.callbacks {
                running.callbacks = callbacks;
                running.progressed = now;
            }
            let why = if failure != Failure::None {
                Some(failure.to_string())
            } else if now.duration_since(running.progressed) > self.stall {
                Some("the device stopped playing".to_owned())
            } else {
                None
            };
            if let Some(why) = why {
                self.stop();
                self.start();
                if let Status::Waiting(reason) = &self.status {
                    self.status = Status::Waiting(format!("{why}; {reason}"));
                }
                return;
            }
            // Back to the chosen device once it returns.
            if now.duration_since(running.checked) >= self.retry {
                running.checked = now;
                if running.stream.info().fallback
                    && let Some(wanted) = &self.settings.output
                    && self
                        .backend
                        .devices()
                        .is_ok_and(|devices| devices.iter().any(|d| &d.id == wanted))
                {
                    self.restart();
                }
            }
        } else if self.retry_at.is_some_and(|at| now >= at) {
            self.start();
        }
    }

    /// Changes the settings, saves them, and reopens the audio with them.
    pub fn set_settings(&mut self, settings: Settings) {
        let audio_changed = settings.output != self.settings.output
            || settings.input != self.settings.input
            || settings.sample_rate != self.settings.sample_rate
            || settings.buffer_size != self.settings.buffer_size;
        self.settings = settings;
        self.save_settings();
        if audio_changed {
            self.restart();
        }
    }

    fn save_settings(&self) {
        if let Some(folder) = &self.folder {
            let _ = self.settings.save(folder);
        }
    }

    /// The plug-in's whole state: every parameter's value and its own, in
    /// the kit's state format, as a host saves it.
    #[must_use]
    pub fn save_state(&self) -> Vec<u8> {
        state::encode(&self.values, &self.plugin.save_state())
    }

    /// Loads a state [`save_state`](Self::save_state) made, here or in any
    /// host. A state the plug-in refuses changes nothing.
    ///
    /// # Errors
    ///
    /// `bytes` isn't a state this plug-in can load.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), PluginError> {
        let decoded = state::decode(bytes)?;
        let before = state::encode(&self.values, &[]);
        state::apply(&decoded, &self.values);
        if let Err(error) = self.plugin.load_state(decoded.own) {
            if let Ok(before) = state::decode(&before) {
                state::apply(&before, &self.values);
            }
            return Err(error);
        }
        self.host
            .state_loaded(self.running.is_some())
            .map_err(|e| PluginError::new(e.to_string()))
    }

    /// Saves the plug-in's state to the file `path`.
    ///
    /// # Errors
    ///
    /// The file couldn't be written.
    pub fn save_state_file(&self, path: &Path) -> Result<(), String> {
        settings::write_atomically(path, &self.save_state())
            .map_err(|e| format!("couldn't save {}: {e}", path.display()))
    }

    /// Loads the plug-in's state from the file `path`.
    ///
    /// # Errors
    ///
    /// The file couldn't be read, or isn't a state this plug-in can load.
    pub fn open_state_file(&mut self, path: &Path) -> Result<(), String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("couldn't open {}: {e}", path.display()))?;
        self.load_state(&bytes)
            .map_err(|e| format!("couldn't load {}: {e}", path.display()))
    }

    /// Loads the state the app saved when it last quit, if there is one.
    ///
    /// # Errors
    ///
    /// The saved state couldn't be loaded.
    pub fn restore(&mut self) -> Result<(), String> {
        let Some(path) = self.folder.as_ref().map(|f| f.join(settings::STATE_FILE)) else {
            return Ok(());
        };
        if path.exists() {
            self.open_state_file(&path)
        } else {
            Ok(())
        }
    }

    /// With a folder, saves the plug-in's state there if it has changed
    /// since it was last saved, so the next run finds it even if the app
    /// is quit in a way that never reaches [`shutdown`](Self::shutdown)
    /// (on macOS, the Dock's Quit ends the process at once). The settings
    /// are saved as they change.
    ///
    /// # Errors
    ///
    /// The state couldn't be written.
    pub fn autosave(&mut self) -> Result<(), String> {
        let Some(folder) = self.folder.clone() else {
            return Ok(());
        };
        let state = self.save_state();
        if self.saved.as_ref() == Some(&state) {
            return Ok(());
        }
        let path = folder.join(settings::STATE_FILE);
        settings::write_atomically(&path, &state)
            .map_err(|e| format!("couldn't save {}: {e}", path.display()))?;
        self.saved = Some(state);
        Ok(())
    }

    /// Stops the audio and, with a folder, saves the settings and the
    /// plug-in's state there for the next run.
    pub fn shutdown(&mut self) {
        self.close_editor();
        self.stop();
        if self.folder.is_some() {
            self.save_settings();
            let _ = self.autosave();
        }
    }
}

impl<P: Plugin> Drop for Standalone<P> {
    fn drop(&mut self) {
        // The window goes first, so nothing on its thread still uses the
        // link when it's closed.
        self.close_editor();
        self.view.close();
        self.stop();
        self.host.disconnect();
    }
}
