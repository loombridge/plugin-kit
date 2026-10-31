//! A plug-in's editor: the window it draws its interface in, inside the
//! host's.
//!
//! A plug-in opts in by returning an [`Editor`] from
//! [`Plugin::editor`](crate::Plugin::editor); one without keeps working as
//! before, with no window. The kit's CLAP export serves the editor through
//! CLAP's `gui` extension, and the VST3 and Audio Unit builds made from the
//! CLAP build get it through the same extension. `lspk-ui` provides
//! editors drawn with egui, including a generic one for any plug-in's
//! parameters; this module depends on no UI toolkit, so the format layer
//! stays headless.
//!
//! # Life of an editor
//!
//! The kit asks for the editor once, as the instance starts, and keeps it
//! for the instance's life. A host then opens and closes its window as
//! many times as it likes, always on the main thread:
//!
//! 1. [`Editor::create`], with the window system and whether the window
//!    floats on its own or sits inside one of the host's;
//! 2. [`Editor::set_scale`], [`Editor::size`] or [`Editor::set_size`], and
//!    the resize questions, in any order and at any time from here on;
//! 3. [`Editor::set_parent`] for an embedded window, which is where the
//!    window opens;
//! 4. [`Editor::show`] and [`Editor::hide`], perhaps many times;
//! 5. [`Editor::destroy`], which closes the window and frees everything it
//!    held, ready for the next [`Editor::create`].
//!
//! # Sizes
//!
//! An [`EditorSize`] is in the window system's own units, as CLAP has it:
//! logical points on macOS ([`WindowApi::uses_logical_size`]), and
//! physical pixels on Windows and X11, where the host's scale factor
//! ([`Editor::set_scale`]) turns one into the other.
//!
//! # Talking back
//!
//! An open editor reaches the plug-in and the host through its
//! [`EditorContext`], from whichever thread its window runs on (the main
//! thread on macOS and Windows, a thread of its own on X11). Every
//! parameter change goes to the host as a gesture, so it records automation
//! and undo exactly as for a person moving a control in its own interface.
//!
//! # Commands
//!
//! An editor changes nothing itself: every change it makes is a registered
//! command, run on the main thread through the plug-in's own registry, the
//! path an agent's command takes ([`EditorContext::run_command`]). A
//! parameter edit is the kit's `params.set` command (the plug-in exports
//! through `lspk_plugin_agent::WithAgent`, which registers it), run inside
//! the gesture the editor holds open, so the host hears one begin, each
//! value and one end, as it always has. A value the editor sets is
//! therefore in [`EditorContext::values`] once the main thread has run the
//! command, not before.
//!
//! # The keyboard
//!
//! An editor also tells its host where it stands with the keyboard
//! ([`EditorContext::set_keyboard_focus`]) and passes on the keys nothing
//! in it used ([`EditorContext::key`]), so a host that plays notes from the
//! computer keyboard, as the standalone app does, keeps playing while the
//! editor has the focus. Plug-in formats have no such route, and their
//! adapters ignore both.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use lspk_commands::Registry;
use lspk_params::ParamId;
use serde_json::{Value, json};

use crate::host::{EditError, Host};
use crate::params::ParamValues;
use crate::{Plugin, PluginError, agent_commands};

/// The kit's command that sets parameters, which an editor's value changes
/// run: `lspk-plugin-agent` registers it, with the agent interface.
const SET_PARAMS: &str = "params.set";

/// A window system an editor can open in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowApi {
    /// macOS's AppKit: the parent is an `NSView`.
    AppKit,
    /// Windows' Win32: the parent is an `HWND`.
    Win32,
    /// X11, on Linux: the parent is an X11 window ID.
    X11,
}

impl WindowApi {
    /// The window system of the platform this was built for, if the kit
    /// supports one there.
    #[must_use]
    pub const fn native() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::AppKit)
        } else if cfg!(target_os = "windows") {
            Some(Self::Win32)
        } else if cfg!(all(unix, not(target_os = "macos"))) {
            Some(Self::X11)
        } else {
            None
        }
    }

    /// Whether sizes in this window system are logical points (AppKit)
    /// rather than physical pixels (Win32 and X11).
    #[must_use]
    pub const fn uses_logical_size(self) -> bool {
        matches!(self, Self::AppKit)
    }
}

/// A host's window, for an embedded editor to open inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentWindow {
    /// An AppKit `NSView`.
    AppKit(NonNull<c_void>),
    /// A Win32 `HWND`.
    Win32(NonNull<c_void>),
    /// An X11 window ID.
    X11(u64),
}

impl ParentWindow {
    /// The window system it belongs to.
    #[must_use]
    pub const fn api(self) -> WindowApi {
        match self {
            Self::AppKit(_) => WindowApi::AppKit,
            Self::Win32(_) => WindowApi::Win32,
            Self::X11(_) => WindowApi::X11,
        }
    }
}

/// An editor window's size, in its window system's units (see the
/// [module documentation](self#sizes)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EditorSize {
    /// Its width.
    pub width: u32,
    /// Its height.
    pub height: u32,
}

impl EditorSize {
    /// A size `width` by `height`.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// Where an editor's window stands with the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyboardFocus {
    /// The window has the keyboard focus.
    pub focused: bool,
    /// A control in it is taking the keys, such as a text field being
    /// edited.
    pub typing: bool,
}

/// A key a person pressed or released in an editor's window that nothing
/// in it used, with no Command, Control or Alt held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorKey {
    /// The character the key types on a US layout, in lower case: a letter
    /// from `a` to `z`, a digit, or one of `` ;',./[]-=` ``.
    pub key: char,
    /// Whether it went down (or up).
    pub pressed: bool,
}

/// How a host may resize an editor's window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeHints {
    /// It can be made wider and narrower.
    pub horizontal: bool,
    /// It can be made taller and shorter.
    pub vertical: bool,
    /// The width-to-height ratio it keeps, if it keeps one.
    pub aspect_ratio: Option<(u32, u32)>,
}

/// A plug-in's editor, as the host drives it. Every method runs on the
/// host's main thread; see the [module documentation](self) for the order
/// a host calls them in.
pub trait Editor: 'static {
    /// Whether it can open in window system `api`, floating in a window of
    /// its own (`floating`) or embedded in one of the host's.
    fn supports(&self, api: WindowApi, floating: bool) -> bool;

    /// Prepares to open in `api`, floating or embedded; nothing is visible
    /// yet. `cx` is how the window will reach the plug-in and the host
    /// once it's open.
    ///
    /// # Errors
    ///
    /// Why it can't open so.
    fn create(
        &mut self,
        api: WindowApi,
        floating: bool,
        cx: EditorContext,
    ) -> Result<(), PluginError>;

    /// Closes the window, if it's open, and frees everything it held.
    fn destroy(&mut self);

    /// Takes the host's scale factor, overriding what the platform says.
    /// Returns false if it works the scale out for itself, as it always
    /// does on macOS.
    fn set_scale(&mut self, scale: f64) -> bool;

    /// Its window's size now, or the size it will open at.
    fn size(&self) -> EditorSize;

    /// How the host may resize it; `None` if its size is fixed.
    fn resize_hints(&self) -> Option<ResizeHints>;

    /// The size nearest `size` it can take.
    fn adjust_size(&self, size: EditorSize) -> EditorSize;

    /// Takes the size `size`, which the host has already adjusted. Returns
    /// false if it can't.
    fn set_size(&mut self, size: EditorSize) -> bool;

    /// Opens its window inside `parent`, for an embedded editor.
    ///
    /// # Errors
    ///
    /// Why the window didn't open.
    fn set_parent(&mut self, parent: ParentWindow) -> Result<(), PluginError>;

    /// The title the host suggests for a floating window.
    fn set_title(&mut self, title: &str) {
        let _ = title;
    }

    /// Shows the window, opening a floating one first if need be.
    ///
    /// # Errors
    ///
    /// Why it couldn't be shown.
    fn show(&mut self) -> Result<(), PluginError>;

    /// Hides the window, keeping it open.
    ///
    /// # Errors
    ///
    /// Why it couldn't be hidden.
    fn hide(&mut self) -> Result<(), PluginError>;

    /// Called on the main thread after [`EditorContext::request_callback`].
    fn on_main_thread(&mut self) {}
}

/// What an editor's window asks of the host, from any thread. A format's
/// adapter implements it for its own host.
pub trait EditorHost: Send + Sync + 'static {
    /// Asks the host to call the plug-in back on the main thread soon,
    /// where the editor's parameter changes reach it.
    fn request_callback(&self);

    /// Asks the host to resize the window it holds the editor in to
    /// `size`. Returns whether the host agreed; it may still refuse later,
    /// and then sets the editor's size back.
    fn request_resize(&self, size: EditorSize) -> bool;

    /// Tells the host the editor's window has closed by itself (a floating
    /// window its close button closed, or a window the platform took away),
    /// so the host destroys the editor. Called on the main thread, never
    /// from inside a call into the editor.
    fn closed(&self) {}

    /// A key nothing in the editor's window used, from the window's thread.
    fn key(&self, key: EditorKey) {
        let _ = key;
    }

    /// Where the editor's window stands with the keyboard now, from the
    /// window's thread, each time it changes.
    fn keyboard_focus(&self, focus: KeyboardFocus) {
        let _ = focus;
    }
}

/// One change an editor makes, on its way to the main thread.
#[derive(Clone, Debug, PartialEq)]
enum Change {
    Begin(ParamId),
    End(ParamId),
    Command {
        id: String,
        args: Value,
        undo_label: String,
    },
}

/// An open editor's link to the plug-in and its host. It may be cloned and
/// sent to the editor's window thread; every method may be called from any
/// thread.
///
/// Every change reaches the plug-in as a command, and the host as gestures,
/// from the main thread, where the kit runs them ([`EditorChanges`]).
#[derive(Clone)]
pub struct EditorContext {
    inner: Arc<ContextInner>,
}

struct ContextInner {
    values: Arc<ParamValues>,
    changes: Sender<Change>,
    host: Box<dyn EditorHost>,
    closed: AtomicBool,
    /// Why the last command the editor ran failed, until taken.
    failure: Arc<Mutex<Option<String>>>,
}

impl EditorContext {
    /// A link to the parameters `values`, reaching the host through
    /// `host`, and the queue its changes arrive in on the main thread. For
    /// a format's adapter.
    #[must_use]
    pub fn new(values: Arc<ParamValues>, host: Box<dyn EditorHost>) -> (Self, EditorChanges) {
        let (changes, receiver) = channel();
        let failure = Arc::new(Mutex::new(None));
        let cx = Self {
            inner: Arc::new(ContextInner {
                values,
                changes,
                host,
                closed: AtomicBool::new(false),
                failure: failure.clone(),
            }),
        };
        (
            cx,
            EditorChanges {
                receiver,
                waiting: VecDeque::new(),
                registry: None,
                failure,
            },
        )
    }

    /// Every parameter's descriptor and value now, kept current from
    /// every direction: the host's automation, the plug-in's own edits,
    /// this editor's.
    #[must_use]
    pub fn values(&self) -> &ParamValues {
        &self.inner.values
    }

    /// Starts a gesture on parameter `id`, as a person grabbing its
    /// control does.
    ///
    /// # Errors
    ///
    /// There's no parameter `id`.
    pub fn begin_edit(&self, id: ParamId) -> Result<(), EditError> {
        self.check(id)?;
        self.send(Change::Begin(id));
        Ok(())
    }

    /// Sets parameter `id` to `value`, in its own units as `params.get`
    /// gives them (clamped to its range), during a gesture: the command
    /// `params.set`, run on the main thread, inside the gesture
    /// [`begin_edit`](Self::begin_edit) opened.
    ///
    /// # Errors
    ///
    /// There's no parameter `id`.
    pub fn set_param(&self, id: ParamId, value: f64) -> Result<(), EditError> {
        self.check(id)?;
        let name = self
            .inner
            .values
            .view(id)
            .map_or_else(String::new, |v| v.name);
        self.run_command(
            SET_PARAMS,
            json!({ "values": [{ "param": id.0, "value": value }] }),
            format!("Set {name}"),
        );
        Ok(())
    }

    /// Ends the gesture [`begin_edit`](Self::begin_edit) started.
    ///
    /// # Errors
    ///
    /// There's no parameter `id`.
    pub fn end_edit(&self, id: ParamId) -> Result<(), EditError> {
        self.check(id)?;
        self.send(Change::End(id));
        Ok(())
    }

    /// Sets parameter `id` to `value` in one gesture (begin, value, end),
    /// as a typed value, a key press or a double-click does: `params.set`
    /// outside any gesture of the editor's.
    ///
    /// # Errors
    ///
    /// There's no parameter `id`.
    pub fn edit(&self, id: ParamId, value: f64) -> Result<(), EditError> {
        self.set_param(id, value)
    }

    /// Runs the registered command `id` with `args`, on the main thread,
    /// through the plug-in's own registry: the path an agent's command
    /// takes, checked against the command's schema. `undo_label` names the
    /// step for the plug-in's undo history
    /// ([`Plugin::run_command_labelled`]). It returns at once; if the
    /// command fails, [`take_command_error`](Self::take_command_error) says
    /// why.
    pub fn run_command(&self, id: &str, args: Value, undo_label: impl Into<String>) {
        self.send(Change::Command {
            id: id.to_owned(),
            args,
            undo_label: undo_label.into(),
        });
    }

    /// Why the last command the editor ran failed, if one has since this
    /// was last called.
    #[must_use]
    pub fn take_command_error(&self) -> Option<String> {
        self.inner.failure.lock().ok()?.take()
    }

    /// Asks the host to resize the window holding the editor to `size`, in
    /// the window system's units. Returns whether it agreed.
    pub fn request_resize(&self, size: EditorSize) -> bool {
        !self.is_closed() && self.inner.host.request_resize(size)
    }

    /// Tells the host the window has closed by itself. Call it on the main
    /// thread, from [`Editor::on_main_thread`], not from inside the
    /// window's own event handling.
    pub fn window_closed(&self) {
        if !self.is_closed() {
            self.inner.host.closed();
        }
    }

    /// Passes on a key nothing in the window used.
    pub fn key(&self, key: EditorKey) {
        if !self.is_closed() {
            self.inner.host.key(key);
        }
    }

    /// Tells the host where the window stands with the keyboard; call it
    /// each time that changes, and with the default (no focus) as the
    /// window closes.
    pub fn set_keyboard_focus(&self, focus: KeyboardFocus) {
        if !self.is_closed() {
            self.inner.host.keyboard_focus(focus);
        }
    }

    /// Asks the host to call [`Editor::on_main_thread`] soon.
    pub fn request_callback(&self) {
        if !self.is_closed() {
            self.inner.host.request_callback();
        }
    }

    /// Cuts the link, as the instance ends: every call after reaches
    /// nothing. For a format's adapter.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
    }

    /// Whether [`close`](Self::close) has cut the link.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    fn check(&self, id: ParamId) -> Result<(), EditError> {
        match self.inner.values.index_of(id) {
            Some(_) => Ok(()),
            None => Err(EditError::UnknownParam(id)),
        }
    }

    fn send(&self, change: Change) {
        if self.is_closed() {
            return;
        }
        // The receiver lives as long as the instance; if it's gone, so is
        // the host, and there's no one to tell.
        if self.inner.changes.send(change).is_ok() {
            self.inner.host.request_callback();
        }
    }
}

impl fmt::Debug for EditorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EditorContext")
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

/// The main thread's end of an editor's changes, which a format's adapter
/// runs in its main-thread callback: commands through the plug-in's
/// registry, gestures through the plug-in's [`Host`].
pub struct EditorChanges {
    receiver: Receiver<Change>,
    /// Changes the host's queue had no room for yet, oldest first.
    waiting: VecDeque<Change>,
    /// The plug-in's commands, listed on the first command run: a
    /// plug-in's registry doesn't change while it runs.
    registry: Option<Registry>,
    failure: Arc<Mutex<Option<String>>>,
}

impl EditorChanges {
    /// Runs every change waiting: the editor's commands as `plugin` runs
    /// its own, and its gestures as gestures `host` passes on. Changes the
    /// plug-in has no room for yet stay, in order, for the next call;
    /// returns how many it ran.
    pub fn apply(&mut self, host: &Host, plugin: &mut impl Plugin) -> usize {
        self.waiting.extend(self.receiver.try_iter());
        let mut passed = 0;
        while let Some(change) = self.waiting.front().cloned() {
            let result = match change {
                Change::Begin(id) => host.begin_edit(id),
                Change::End(id) => host.end_edit(id),
                Change::Command {
                    id,
                    args,
                    undo_label,
                } => {
                    if let Err(why) = self.run(plugin, &id, &args, &undo_label)
                        && let Ok(mut failure) = self.failure.lock()
                    {
                        *failure = Some(why);
                    }
                    Ok(())
                }
            };
            match result {
                Err(EditError::QueueFull | EditError::NotReady) => break,
                // An unknown parameter was refused when it was sent.
                Ok(()) | Err(EditError::UnknownParam(_)) => {
                    self.waiting.pop_front();
                    passed += 1;
                }
            }
        }
        passed
    }

    /// Whether changes are waiting.
    #[must_use]
    pub fn is_empty(&mut self) -> bool {
        self.waiting.extend(self.receiver.try_iter());
        self.waiting.is_empty()
    }

    /// Runs command `id` as an agent's runs: it must be in the plug-in's
    /// registry, and `args` must fit its schema.
    fn run(
        &mut self,
        plugin: &mut impl Plugin,
        id: &str,
        args: &Value,
        undo_label: &str,
    ) -> Result<(), String> {
        if self.registry.is_none() {
            let mut registry = Registry::new();
            plugin
                .register_commands(&mut registry)
                .map_err(|e| e.to_string())?;
            self.registry = Some(registry);
        }
        let registry = self.registry.as_ref().expect("listed above");
        agent_commands::run_checked(registry, id, args, |id, args| {
            plugin
                .run_command_labelled(id, args, undo_label)
                .map_err(|e| e.message().to_owned())
        })
        .map(drop)
    }
}

impl fmt::Debug for EditorChanges {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EditorChanges")
            .field("waiting", &self.waiting.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Edit, HostBackend};
    use crate::{
        AudioLayout, Buffers, Events, PluginInfo, ProcessConfig, ProcessContext, ProcessStatus,
        Processor,
    };
    use lspk_commands::{CommandSpec, Context, Permission, RegisterError};
    use lspk_params::ParamDescriptor;
    use std::sync::atomic::AtomicU32;

    #[derive(Default)]
    struct Calls {
        callbacks: AtomicU32,
        resizes: AtomicU32,
        keys: std::sync::Mutex<Vec<EditorKey>>,
        focus: std::sync::Mutex<Vec<KeyboardFocus>>,
    }

    struct Recorder(Arc<Calls>);

    impl EditorHost for Recorder {
        fn request_callback(&self) {
            self.0.callbacks.fetch_add(1, Ordering::Relaxed);
        }

        fn request_resize(&self, _size: EditorSize) -> bool {
            self.0.resizes.fetch_add(1, Ordering::Relaxed);
            true
        }

        fn key(&self, key: EditorKey) {
            self.0.keys.lock().expect("not poisoned").push(key);
        }

        fn keyboard_focus(&self, focus: KeyboardFocus) {
            self.0.focus.lock().expect("not poisoned").push(focus);
        }
    }

    struct Quiet;

    impl HostBackend for Quiet {
        fn request_flush(&self) {}
        fn mark_dirty(&self) {}
        fn request_restart(&self) {}
        fn request_process(&self) {}
        fn request_callback(&self) {}
        fn values_changed(&self) {}
    }

    fn values() -> Arc<ParamValues> {
        Arc::new(
            ParamValues::new(vec![
                ParamDescriptor::gain(ParamId(1), "Gain", 6.0, 0.0),
                ParamDescriptor::toggle(ParamId(2), "On", true),
            ])
            .expect("valid parameters"),
        )
    }

    /// A plug-in with the one command an editor's value changes run, as
    /// `lspk-plugin-agent` registers it, and one of its own; it keeps the
    /// labels it was run under.
    struct Settable {
        host: Host,
        labels: Vec<String>,
    }

    struct Silent;

    impl Processor for Silent {
        fn process(
            &mut self,
            _audio: &mut Buffers<'_>,
            _events: &Events<'_>,
            _cx: &mut ProcessContext<'_>,
        ) -> ProcessStatus {
            ProcessStatus::Continue
        }

        fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
    }

    impl Plugin for Settable {
        type Processor = Silent;

        const INFO: PluginInfo = PluginInfo::new("com.loombridge.test.settable", "Settable")
            .audio(AudioLayout::effect(2));

        fn new(host: Host) -> Self {
            Self {
                host,
                labels: Vec::new(),
            }
        }

        fn activate(
            &mut self,
            _config: &ProcessConfig,
            _values: &ParamValues,
        ) -> Result<Silent, PluginError> {
            Ok(Silent)
        }

        fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
            for id in [SET_PARAMS, "test.fails"] {
                registry.register(
                    CommandSpec::new(id, id)
                        .description("A test command.")
                        .permission(Permission::Edit)
                        .context(Context::Global),
                )?;
            }
            Ok(())
        }

        fn run_command(&mut self, id: &str, args: &Value) -> Result<Value, PluginError> {
            if id != SET_PARAMS {
                return Err(PluginError::new("it failed"));
            }
            let set = &args["values"][0];
            let param = ParamId(set["param"].as_u64().expect("an ID") as u32);
            let value = set["value"].as_f64().expect("a value");
            // As `lspk-plugin-agent`'s edits: a gesture around the value.
            self.host
                .begin_edit(param)
                .map_err(|e| PluginError::new(e.to_string()))?;
            self.host
                .set_param(param, value)
                .map_err(|e| PluginError::new(e.to_string()))?;
            self.host
                .end_edit(param)
                .map_err(|e| PluginError::new(e.to_string()))?;
            Ok(Value::Null)
        }

        fn run_command_labelled(
            &mut self,
            id: &str,
            args: &Value,
            undo_label: &str,
        ) -> Result<Value, PluginError> {
            self.labels.push(undo_label.to_owned());
            self.run_command(id, args)
        }
    }

    /// An editor's context, the plug-in it edits and the queue it reaches
    /// the host through.
    fn rig() -> (
        EditorContext,
        EditorChanges,
        Host,
        Settable,
        rtrb::Consumer<Edit>,
        Arc<Calls>,
    ) {
        let values = values();
        let calls = Arc::new(Calls::default());
        let (cx, changes) = EditorContext::new(values.clone(), Box::new(Recorder(calls.clone())));
        let host = Host::new(Box::new(Quiet));
        let edits = host.connect(values);
        let plugin = Settable::new(host.clone());
        (cx, changes, host, plugin, edits, calls)
    }

    fn queued(edits: &mut rtrb::Consumer<Edit>) -> Vec<Edit> {
        std::iter::from_fn(|| edits.pop().ok()).collect()
    }

    #[test]
    fn an_editors_changes_reach_the_host_as_gestures() {
        let (cx, mut changes, host, mut plugin, mut edits, calls) = rig();

        // A drag, from another thread, as on X11.
        let drag = cx.clone();
        std::thread::spawn(move || {
            drag.begin_edit(ParamId(1)).expect("a parameter");
            assert_eq!(drag.set_param(ParamId(1), 0.5), Ok(()));
            assert_eq!(drag.set_param(ParamId(1), 99.0), Ok(()));
            drag.end_edit(ParamId(1)).expect("a parameter");
        })
        .join()
        .expect("the window thread");
        // Nothing changes until the main thread runs the commands.
        assert_eq!(cx.values().get(ParamId(1)), Some(1.0));
        assert_eq!(cx.edit(ParamId(2), 0.0), Ok(()));
        assert_eq!(
            cx.edit(ParamId(9), 0.0),
            Err(EditError::UnknownParam(ParamId(9)))
        );
        assert!(calls.callbacks.load(Ordering::Relaxed) >= 5);

        assert_eq!(changes.apply(&host, &mut plugin), 5);
        assert!(changes.is_empty());
        assert_eq!(cx.take_command_error(), None);
        let max = lspk_params::db_to_gain(6.0);
        assert_eq!(cx.values().get(ParamId(1)), Some(max));
        // The drag's gesture held, each value inside it; the lone edit its
        // own gesture.
        assert_eq!(
            queued(&mut edits),
            [
                Edit::Begin(ParamId(1)),
                Edit::Value {
                    id: ParamId(1),
                    value: 0.5,
                    echo: true
                },
                Edit::Value {
                    id: ParamId(1),
                    value: max,
                    echo: true
                },
                Edit::End(ParamId(1)),
                Edit::Begin(ParamId(2)),
                Edit::Value {
                    id: ParamId(2),
                    value: 0.0,
                    echo: true
                },
                Edit::End(ParamId(2)),
            ]
        );
        assert_eq!(plugin.labels, ["Set Gain", "Set Gain", "Set On"]);
    }

    #[test]
    fn a_command_runs_through_the_registry_and_says_why_it_failed() {
        let (cx, mut changes, host, mut plugin, _edits, _calls) = rig();
        cx.run_command("test.fails", serde_json::json!({}), "Fail");
        cx.run_command("test.nothing", serde_json::json!({}), "Nothing");
        assert_eq!(changes.apply(&host, &mut plugin), 2);
        assert_eq!(
            cx.take_command_error().as_deref(),
            Some("the plug-in has no command `test.nothing`"),
            "a command outside the registry never reaches the plug-in"
        );
        assert_eq!(cx.take_command_error(), None);
        // The failing command ran, and its label with it.
        assert_eq!(plugin.labels, ["Fail"]);
    }

    #[test]
    fn changes_wait_while_the_queue_is_full() {
        let values = values();
        let (cx, mut changes) =
            EditorContext::new(values.clone(), Box::new(Recorder(Arc::default())));
        let host = Host::new(Box::new(Quiet));
        let mut plugin = Settable::new(host.clone());
        // Not connected yet: nothing passes, nothing is lost.
        cx.begin_edit(ParamId(2)).expect("a parameter");
        assert_eq!(changes.apply(&host, &mut plugin), 0);
        let _edits = host.connect(values);
        assert_eq!(changes.apply(&host, &mut plugin), 1);
    }

    #[test]
    fn a_closed_link_reaches_nothing() {
        let calls = Arc::new(Calls::default());
        let (cx, mut changes) = EditorContext::new(values(), Box::new(Recorder(calls.clone())));
        cx.close();
        assert!(!cx.request_resize(EditorSize::new(10, 10)));
        cx.edit(ParamId(1), 0.5).expect("a parameter");
        cx.request_callback();
        assert!(changes.is_empty());
        assert_eq!(calls.callbacks.load(Ordering::Relaxed), 0);
        assert_eq!(calls.resizes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn keys_and_focus_reach_the_host_until_the_link_closes() {
        let calls = Arc::new(Calls::default());
        let (cx, _changes) = EditorContext::new(values(), Box::new(Recorder(calls.clone())));
        let typing = KeyboardFocus {
            focused: true,
            typing: true,
        };
        let a = EditorKey {
            key: 'a',
            pressed: true,
        };
        cx.set_keyboard_focus(typing);
        cx.key(a);
        cx.close();
        cx.key(a);
        cx.set_keyboard_focus(KeyboardFocus::default());
        assert_eq!(*calls.keys.lock().expect("not poisoned"), [a]);
        assert_eq!(*calls.focus.lock().expect("not poisoned"), [typing]);
    }

    #[test]
    fn only_appkit_sizes_are_logical() {
        assert!(WindowApi::AppKit.uses_logical_size());
        assert!(!WindowApi::Win32.uses_logical_size());
        assert!(!WindowApi::X11.uses_logical_size());
        assert!(WindowApi::native().is_some());
        let parent = ParentWindow::X11(7);
        assert_eq!(parent.api(), WindowApi::X11);
    }
}
