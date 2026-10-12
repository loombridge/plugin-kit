//! [`EguiEditor`]: a plug-in editor drawn with egui, in a `baseview` window
//! embedded in the host's.
//!
//! # The route into the host's window
//!
//! `baseview` makes the plug-in's window: an `NSView` inside the host's on
//! macOS, a child `HWND` on Windows, and an X11 child window on Linux, run
//! on a thread of its own there. It carries the hosts' known workarounds:
//! it accepts the first click on macOS (where a plug-in view otherwise
//! swallows it), hooks the keyboard on Windows so hosts that keep keys
//! from plug-in windows (Live among them) don't, and tells the host of
//! resizes the window makes itself. `egui-baseview` draws egui inside it
//! with OpenGL (`egui_glow`), and hands back to the host any key no control
//! wants, so the host's own shortcuts keep working; its keyboard
//! workaround takes the keyboard back whenever a control asks for it.
//!
//! OpenGL rather than wgpu: opening a wgpu device each time the editor
//! opens costs time and is where plug-in windows most often hang or fail in
//! hosts, and the OpenGL route is the one Rust plug-in editors have used in
//! hosts for longest. Nothing outside this module depends on the choice.
//!
//! # Sizes, scale and zoom
//!
//! A [`View`] lays out in points. The editor's size is kept in points at
//! 100 % zoom; its window is that size times the global zoom, and its
//! pixels that times the scale factor: the host's, where it gives one
//! (Windows and X11), or the platform's. The host sees sizes in its window
//! system's units ([`lspk_plugin::editor`]'s "Sizes").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use baseview::dpi::LogicalSize;
use baseview::host::{HostCallbacks, HostMainThreadCaller};
use baseview::{HandlerError, ParentWindowHandle, WindowSize};
use egui::{Key, Rect, Sense, Ui, Vec2, ViewportCommand, pos2, vec2};
use egui_baseview::{EguiWindow, EguiWindowSettings, Frame};
use lspk_plugin::{
    Editor, EditorContext, EditorSize, ParentWindow, PluginError, ResizeHints, WindowApi,
};
use raw_window_handle::{
    AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle,
    WindowHandle, XlibWindowHandle,
};

use crate::accessibility::Accessibility;
use crate::controls::end_open_gestures;
use crate::theme::{Theme, clamp_zoom, step_zoom};

/// How often an open window looks for changes it didn't make (the host's
/// automation, the plug-in's own edits), and redraws.
const POLL: Duration = Duration::from_millis(33);

/// The size of the grip in the corner of a resizable window, in points.
const GRIP: f32 = 14.0;

/// How many editor windows are open in this process now.
static OPEN_WINDOWS: AtomicUsize = AtomicUsize::new(0);

/// How many editor windows are open in this process now, across every
/// plug-in instance: each window's interface and renderer count from when
/// it opens until everything it held is freed.
#[must_use]
pub fn open_windows() -> usize {
    OPEN_WINDOWS.load(Ordering::Acquire)
}

/// An interface an [`EguiEditor`] draws: the generic one
/// ([`GenericView`](crate::GenericView)), or a plug-in's own.
///
/// A view is made afresh each time the window opens, on the thread the
/// window runs on, so it holds only what it draws; the parameters' values
/// live in the plug-in, and egui's memory holds the rest.
pub trait View: Send + 'static {
    /// Draws the interface in `ui`, which fills the window.
    fn ui(&mut self, ui: &mut Ui, editor: &mut EditorUi<'_>);
}

/// What a [`View`] draws with: the plug-in's parameters, the theme, and
/// the zoom.
pub struct EditorUi<'a> {
    /// The link to the plug-in and the host: the parameters' values, and
    /// where changes go.
    pub cx: &'a EditorContext,
    /// The theme.
    pub theme: &'a Theme,
    zoom: f32,
    new_zoom: Option<f32>,
}

impl EditorUi<'_> {
    /// The global zoom now.
    #[must_use]
    pub fn zoom(&self) -> f32 {
        self.new_zoom.unwrap_or(self.zoom)
    }

    /// Sets the global zoom, from 50 % to 200 %; the window resizes to
    /// match.
    pub fn set_zoom(&mut self, zoom: f32) {
        self.new_zoom = Some(clamp_zoom(zoom));
    }
}

/// How an [`EguiEditor`] opens.
#[derive(Clone, Debug)]
pub struct EditorOptions {
    /// The size it first opens at, in points at 100 % zoom.
    pub size: Vec2,
    /// The smallest it can be made, in points at 100 % zoom.
    pub min_size: Vec2,
    /// Whether the host and the person may resize it. Zooming resizes it
    /// either way.
    pub resizable: bool,
    /// What it draws with, and the zoom it first opens at.
    pub theme: Theme,
    /// The title of a floating window, until the host suggests one.
    pub title: String,
}

impl EditorOptions {
    /// An editor that opens at `size`, in points, and can't be made
    /// smaller.
    #[must_use]
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            min_size: size,
            resizable: true,
            theme: Theme::default(),
            title: String::new(),
        }
    }
}

/// A plug-in editor drawn with egui: an [`Editor`] a plug-in returns from
/// [`Plugin::editor`](lspk_plugin::Plugin::editor).
///
/// It remembers its size and zoom between openings, for the instance's
/// life.
pub struct EguiEditor {
    options: EditorOptions,
    make_view: Arc<dyn Fn() -> Box<dyn View> + Send + Sync>,
    state: Arc<State>,
    session: Option<Session>,
}

/// One opening, from the host's create to its destroy.
struct Session {
    api: WindowApi,
    floating: bool,
    cx: EditorContext,
    title: String,
    window: Option<baseview::Window>,
}

/// What the main thread and the window's thread share: the size, the zoom
/// and the scale, each a float in an atomic.
struct State {
    width: AtomicU32,
    height: AtomicU32,
    zoom: AtomicU32,
    /// The host's scale factor, 0 until it gives one.
    host_scale: AtomicU32,
    /// The platform's scale factor, as the window last saw it.
    system_scale: AtomicU32,
    /// The window closed by itself; the host needs telling.
    closed: AtomicBool,
}

fn load(value: &AtomicU32) -> f32 {
    f32::from_bits(value.load(Ordering::Acquire))
}

fn store(value: &AtomicU32, to: f32) {
    value.store(to.to_bits(), Ordering::Release);
}

impl State {
    fn new(size: Vec2, zoom: f32) -> Self {
        Self {
            width: AtomicU32::new(size.x.to_bits()),
            height: AtomicU32::new(size.y.to_bits()),
            zoom: AtomicU32::new(clamp_zoom(zoom).to_bits()),
            host_scale: AtomicU32::new(0f32.to_bits()),
            system_scale: AtomicU32::new(1f32.to_bits()),
            closed: AtomicBool::new(false),
        }
    }

    fn points(&self) -> Vec2 {
        vec2(load(&self.width), load(&self.height))
    }

    fn set_points(&self, size: Vec2) {
        store(&self.width, size.x);
        store(&self.height, size.y);
    }

    fn zoom(&self) -> f32 {
        load(&self.zoom)
    }

    /// The scale factor in use: the host's if it gave one, else the
    /// platform's.
    fn scale(&self) -> f32 {
        let host = load(&self.host_scale);
        if host > 0.0 {
            host
        } else {
            load(&self.system_scale)
        }
    }

    /// How much larger than the platform's scale the host's is, which the
    /// window's egui zoom makes up.
    fn ratio(&self) -> f32 {
        let host = load(&self.host_scale);
        let system = load(&self.system_scale);
        if host > 0.0 && system > 0.0 {
            host / system
        } else {
            1.0
        }
    }

    /// How many of `api`'s units a point at 100 % zoom is: the zoom, times
    /// the scale where `api` counts pixels.
    fn factor(&self, api: WindowApi) -> f32 {
        let factor = if api.uses_logical_size() {
            self.zoom()
        } else {
            self.zoom() * self.scale()
        };
        factor.max(f32::EPSILON)
    }

    /// `points` at 100 % zoom in `api`'s units.
    fn size_in(&self, api: WindowApi, points: Vec2) -> EditorSize {
        let size = points * self.factor(api);
        EditorSize::new(size.x.round() as u32, size.y.round() as u32)
    }

    /// The window's size in `api`'s units.
    fn host_size(&self, api: WindowApi) -> EditorSize {
        self.size_in(api, self.points())
    }

    /// `size`, in `api`'s units, as points at 100 % zoom.
    fn to_points(&self, api: WindowApi, size: EditorSize) -> Vec2 {
        vec2(size.width as f32, size.height as f32) / self.factor(api)
    }
}

impl EguiEditor {
    /// An editor drawing a fresh view from `make_view` each time its
    /// window opens.
    #[must_use]
    pub fn new<V: View>(
        options: EditorOptions,
        make_view: impl Fn() -> V + Send + Sync + 'static,
    ) -> Self {
        let state = Arc::new(State::new(options.size, options.theme.zoom));
        Self {
            options,
            make_view: Arc::new(move || Box::new(make_view()) as Box<dyn View>),
            state,
            session: None,
        }
    }

    /// The window system it will open in: the host's choice once it has
    /// made one, else the platform's own.
    fn api(&self) -> Option<WindowApi> {
        self.session
            .as_ref()
            .map(|session| session.api)
            .or_else(WindowApi::native)
    }

    /// Opens the window, inside `parent` or floating, and shows it.
    fn open(&mut self, parent: Option<ParentWindow>) -> Result<(), PluginError> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| PluginError::new("the host hasn't created the editor"))?;
        if session.window.is_some() {
            return Ok(());
        }
        let points = self.state.points();
        let mut settings = EguiWindowSettings::new()
            .with_title(session.title.clone())
            .with_size(LogicalSize::new(f64::from(points.x), f64::from(points.y)))
            .with_zoom_factor(self.state.zoom() * self.state.ratio())
            // Always resizable for baseview, so zooming can resize it; the
            // host's resize hints and the grip say whether a person can.
            .with_resizable(true);
        if let Some(parent) = parent {
            settings.parent = Some(ParentWindowHandle::from_window(&Parent(parent)));
        }
        let app = WindowApp {
            view: (self.make_view)(),
            cx: session.cx.clone(),
            state: self.state.clone(),
            theme: self.options.theme.clone(),
            resizable: self.options.resizable,
            min_size: self.options.min_size,
            ctx: None,
            accessibility: None,
        };
        let host = baseview::host::Host::new()
            .with_callbacks(Callbacks {
                cx: session.cx.clone(),
                api: session.api,
                floating: session.floating,
                state: self.state.clone(),
            })
            .with_main_thread(MainThread(session.cx.clone()));
        self.state.closed.store(false, Ordering::Release);
        let window = EguiWindow::create_with_host(settings, app, Some(host)).map_err(|error| {
            PluginError::new(format!("the editor's window didn't open: {error}"))
        })?;
        window.show().map_err(|error| {
            PluginError::new(format!("the editor's window didn't show: {error}"))
        })?;
        session.window = Some(window);
        Ok(())
    }

    fn window(&self) -> Option<&baseview::Window> {
        self.session.as_ref()?.window.as_ref()
    }
}

impl Editor for EguiEditor {
    fn supports(&self, api: WindowApi, _floating: bool) -> bool {
        WindowApi::native() == Some(api)
    }

    fn create(
        &mut self,
        api: WindowApi,
        floating: bool,
        cx: EditorContext,
    ) -> Result<(), PluginError> {
        if !self.supports(api, floating) {
            return Err(PluginError::new(
                "the editor can't open in this window system",
            ));
        }
        self.destroy();
        self.session = Some(Session {
            api,
            floating,
            cx,
            title: self.options.title.clone(),
            window: None,
        });
        Ok(())
    }

    fn destroy(&mut self) {
        if let Some(window) = self.session.take().and_then(|session| session.window) {
            window.close();
        }
    }

    fn set_scale(&mut self, scale: f64) -> bool {
        if self.api().is_none_or(WindowApi::uses_logical_size)
            || !(scale.is_finite() && scale > 0.0)
        {
            return false;
        }
        store(&self.state.host_scale, scale.clamp(0.25, 8.0) as f32);
        if let Some(window) = self.window() {
            window.waker().request_redraw();
        }
        true
    }

    fn size(&self) -> EditorSize {
        self.api()
            .map_or_else(EditorSize::default, |api| self.state.host_size(api))
    }

    fn resize_hints(&self) -> Option<ResizeHints> {
        self.options.resizable.then_some(ResizeHints {
            horizontal: true,
            vertical: true,
            aspect_ratio: None,
        })
    }

    fn adjust_size(&self, size: EditorSize) -> EditorSize {
        let Some(api) = self.api() else {
            return size;
        };
        if !self.options.resizable {
            return self.state.host_size(api);
        }
        let points = self.state.to_points(api, size).max(self.options.min_size);
        self.state.size_in(api, points)
    }

    fn set_size(&mut self, size: EditorSize) -> bool {
        let Some(api) = self.api() else {
            return false;
        };
        if !self.options.resizable {
            return size == self.state.host_size(api);
        }
        let points = self.state.to_points(api, size).max(self.options.min_size);
        self.state.set_points(points);
        if let Some(window) = self.window() {
            let logical = points * self.state.zoom() * self.state.ratio();
            let resized =
                window.resize(LogicalSize::new(f64::from(logical.x), f64::from(logical.y)));
            return resized.is_ok();
        }
        true
    }

    fn set_parent(&mut self, parent: ParentWindow) -> Result<(), PluginError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| PluginError::new("the host hasn't created the editor"))?;
        if session.floating || parent.api() != session.api {
            return Err(PluginError::new(
                "the host's window isn't the kind it created the editor for",
            ));
        }
        self.open(Some(parent))
    }

    fn set_title(&mut self, title: &str) {
        if let Some(session) = &mut self.session {
            title.clone_into(&mut session.title);
        }
    }

    fn show(&mut self) -> Result<(), PluginError> {
        let floating = self
            .session
            .as_ref()
            .is_some_and(|session| session.floating);
        if floating && self.window().is_none() {
            return self.open(None);
        }
        let window = self
            .window()
            .ok_or_else(|| PluginError::new("the editor's window isn't open"))?;
        window
            .show()
            .map_err(|error| PluginError::new(format!("the editor's window didn't show: {error}")))
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        match self.window() {
            Some(window) => window.hide().map_err(|error| {
                PluginError::new(format!("the editor's window didn't hide: {error}"))
            }),
            None => Ok(()),
        }
    }

    fn on_main_thread(&mut self) {
        if let Some(window) = self.window() {
            window.host_main_thread_callback();
        }
        if self.state.closed.swap(false, Ordering::AcqRel)
            && let Some(session) = &mut self.session
        {
            if let Some(window) = session.window.take() {
                window.close();
            }
            session.cx.window_closed();
        }
    }
}

/// The host's window, as `raw-window-handle` gives it to baseview.
struct Parent(ParentWindow);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = match self.0 {
            ParentWindow::AppKit(view) => RawWindowHandle::AppKit(AppKitWindowHandle::new(view)),
            ParentWindow::Win32(hwnd) => {
                let hwnd = std::num::NonZeroIsize::new(hwnd.as_ptr() as isize)
                    .ok_or(HandleError::Unavailable)?;
                RawWindowHandle::Win32(Win32WindowHandle::new(hwnd))
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "an X11 window ID fits the platform's `c_ulong` wherever X11 runs"
            )]
            ParentWindow::X11(window) => {
                RawWindowHandle::Xlib(XlibWindowHandle::new(window as std::ffi::c_ulong))
            }
        };
        // SAFETY: the host keeps its window alive while the editor is open
        // inside it (CLAP's `set_parent` until `destroy`), and baseview only
        // uses the handle while the editor's window is open.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

/// What the window asks of the host, through the editor's context.
struct Callbacks {
    cx: EditorContext,
    api: WindowApi,
    floating: bool,
    state: Arc<State>,
}

impl HostCallbacks for Callbacks {
    fn request_resize(&mut self, new_size: WindowSize) -> Result<(), HandlerError> {
        // A floating window is the plug-in's own to resize.
        if self.floating {
            return Ok(());
        }
        let size = if self.api.uses_logical_size() {
            EditorSize::new(
                new_size.logical.width.round() as u32,
                new_size.logical.height.round() as u32,
            )
        } else {
            EditorSize::new(new_size.physical.width, new_size.physical.height)
        };
        if self.cx.request_resize(size) {
            Ok(())
        } else {
            Err(HandlerError::from_boxed(
                "the host refused the new size".into(),
            ))
        }
    }

    fn destroyed(&mut self) {
        self.state.closed.store(true, Ordering::Release);
        self.cx.request_callback();
    }
}

/// On X11, where the window runs on a thread of its own, how it asks for
/// the main thread: through the host's callback, which reaches
/// [`Editor::on_main_thread`].
struct MainThread(EditorContext);

impl HostMainThreadCaller for MainThread {
    fn call_main_thread(&mut self) {
        self.0.request_callback();
    }
}

/// The window's side: the view, drawn by egui each frame.
struct WindowApp {
    view: Box<dyn View>,
    cx: EditorContext,
    state: Arc<State>,
    theme: Theme,
    resizable: bool,
    min_size: Vec2,
    ctx: Option<egui::Context>,
    accessibility: Option<Accessibility>,
}

impl egui_baseview::App for WindowApp {
    fn build(&mut self, ctx: egui::Context, frame: &mut Frame) -> Result<(), HandlerError> {
        OPEN_WINDOWS.fetch_add(1, Ordering::AcqRel);
        prepare(&ctx, &self.theme);
        self.accessibility = Accessibility::connect(&ctx, frame.baseview_window());
        frame.set_clear_color(self.theme.colors.background.into());
        store(
            &self.state.system_scale,
            frame.baseview_window().scale_factor() as f32,
        );
        self.ctx = Some(ctx);
        Ok(())
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut Frame) {
        if let Some(accessibility) = &mut self.accessibility {
            accessibility.update();
        }
        store(
            &self.state.system_scale,
            frame.baseview_window().scale_factor() as f32,
        );
        let ctx = ui.ctx().clone();
        let zoom = draw(
            ui,
            self.view.as_mut(),
            &self.cx,
            &self.theme,
            self.state.zoom(),
        );
        store(&self.state.zoom, zoom);
        // The host's scale makes up the difference from the platform's.
        let want = zoom * self.state.ratio();
        if (ctx.zoom_factor() - want).abs() > 1e-4 {
            ctx.set_zoom_factor(want);
        }
        if self.resizable {
            resize_grip(ui, &self.theme, self.state.points(), self.min_size);
        }
        // Asked of the window, not egui: egui calls back only when a
        // repaint comes sooner than one already asked for, and
        // egui-baseview doesn't read the delay egui reports.
        frame.baseview_window().request_redraw_after(POLL);
    }

    fn resized(&mut self, size: WindowSize) {
        // egui-baseview gives the size in egui's points, which are the
        // editor's points at 100 % zoom.
        self.state
            .set_points(vec2(size.logical.width as f32, size.logical.height as f32));
    }
}

impl Drop for WindowApp {
    fn drop(&mut self) {
        if let Some(ctx) = self.ctx.take() {
            end_open_gestures(&ctx, &self.cx);
            OPEN_WINDOWS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Readies `ctx` for an editor: the theme, AccessKit on, and the zoom keys
/// left to the editor.
pub fn prepare(ctx: &egui::Context, theme: &Theme) {
    theme.apply(ctx);
    ctx.enable_accesskit();
    ctx.options_mut(|options| options.zoom_with_keyboard = false);
}

/// Draws `view` filling `ui`, with the zoom keys (Command or Ctrl with
/// plus, minus and 0) and the view's own zoom changes; returns the zoom
/// after them. The window applies it.
pub fn draw(ui: &mut Ui, view: &mut dyn View, cx: &EditorContext, theme: &Theme, zoom: f32) -> f32 {
    let mut zoom = zoom;
    ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::COMMAND, Key::Plus)
            || i.consume_key(egui::Modifiers::COMMAND, Key::Equals)
        {
            zoom = step_zoom(zoom, 1);
        }
        if i.consume_key(egui::Modifiers::COMMAND, Key::Minus) {
            zoom = step_zoom(zoom, -1);
        }
        if i.consume_key(egui::Modifiers::COMMAND, Key::Num0) {
            zoom = 1.0;
        }
    });
    let mut editor = EditorUi {
        cx,
        theme,
        zoom,
        new_zoom: None,
    };
    let frame = egui::Frame::NONE.fill(theme.colors.background);
    egui::CentralPanel::default()
        .frame(frame)
        .show(ui, |ui| view.ui(ui, &mut editor));
    editor.zoom()
}

/// The grip in the bottom-right corner: dragging it resizes the window,
/// no smaller than `min_size`.
fn resize_grip(ui: &mut Ui, theme: &Theme, size: Vec2, min_size: Vec2) {
    let screen = ui.ctx().content_rect();
    let rect = Rect::from_min_max(screen.max - vec2(GRIP, GRIP), screen.max);
    let response = ui.interact(rect, ui.id().with("lspk-resize-grip"), Sense::drag());
    let ink = if response.hovered() || response.dragged() {
        theme.colors.text
    } else {
        theme.colors.text_dim
    };
    for step in [4.0, 8.0, 12.0] {
        ui.painter().line_segment(
            [
                pos2(rect.right() - step, rect.bottom() - 2.0),
                pos2(rect.right() - 2.0, rect.bottom() - step),
            ],
            egui::Stroke::new(1.0, ink),
        );
    }
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    if response.dragged() {
        let delta = response.drag_delta();
        if delta != Vec2::ZERO {
            let to = (size + delta).max(min_size);
            ui.ctx().send_viewport_cmd(ViewportCommand::InnerSize(to));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_points_times_zoom_and_pixels_times_scale() {
        let state = State::new(vec2(400.0, 300.0), 1.5);
        assert_eq!(
            state.host_size(WindowApi::AppKit),
            EditorSize::new(600, 450)
        );
        assert_eq!(state.host_size(WindowApi::Win32), EditorSize::new(600, 450));
        store(&state.host_scale, 2.0);
        assert_eq!(
            state.host_size(WindowApi::AppKit),
            EditorSize::new(600, 450)
        );
        assert_eq!(state.host_size(WindowApi::X11), EditorSize::new(1200, 900));
        assert_eq!(
            state.to_points(WindowApi::X11, EditorSize::new(1200, 900)),
            vec2(400.0, 300.0)
        );
        assert_eq!(state.ratio(), 2.0);
        store(&state.system_scale, 2.0);
        assert_eq!(state.ratio(), 1.0);
    }
}
