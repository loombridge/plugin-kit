//! The app's window: one window, drawn with egui in a `baseview` window, as
//! the kit's editors are (`lspk-ui`).
//!
//! The plug-in's editor fills it, embedded through
//! [`Editor::set_parent`](lspk_plugin::Editor::set_parent) as a host embeds
//! one; a plug-in without an editor gets the kit's generic one, embedded
//! the same way. A thin strip along the bottom shows where the audio
//! stands, the output's level, and, for an instrument, the computer
//! keyboard's switch, octave and velocity. The devices, the MIDI inputs
//! and the state files are in the menu bar ([`menu`](crate::menu)) and the
//! Settings dialog ([`settings_window`](crate::settings_window)). The
//! window follows the editor's size ([`layout`](crate::layout)). Closing
//! it quits the app, saving the settings and the plug-in's state for the
//! next run.
//!
//! The computer keyboard plays while either the strip or the editor has
//! the focus ([`keyboard`](crate::keyboard)'s "When it plays"): the strip's
//! keys come to this window, and the editor passes on the keys it doesn't
//! use.
//!
//! Everything runs on the window's thread, which is the plug-in's main
//! thread: the main thread on macOS and Windows, and a thread of the
//! window's own on X11. The app is made there, when the window opens.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_baseview::baseview::dpi::LogicalSize;
use egui_baseview::baseview::{HandlerError, WindowContext};
use egui_baseview::{EguiWindow, EguiWindowSettings, Frame};
use lspk_plugin::{EditorKey, KeyboardFocus, ParentWindow, Plugin, WindowApi};
use lspk_ui::Theme;
use lspk_ui::egui::{self, Color32, RichText, Ui};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::keyboard::{ComputerKeyboard, KeyboardGate, WindowFocus};
use crate::layout::{self, Layout, MIN_WIDTH, STRIP_HEIGHT, StripItem};
use crate::menu::{Command, Platform};
use crate::midi::MidiInputs;
use crate::settings::{self, Settings};
use crate::settings_window::{self, Panel, SharedPanel, lock};
use crate::{CpalBackend, Standalone, Status};

/// How often the window polls the app and redraws.
const POLL: Duration = Duration::from_millis(33);
/// How often it looks for new MIDI inputs and audio devices, and saves the
/// plug-in's state if it has changed.
const SCAN: Duration = Duration::from_secs(2);
/// How long the strip shows what went wrong in place of the status.
const MESSAGE: Duration = Duration::from_secs(8);
/// The quietest level the meter shows, in dB.
const METER_FLOOR: f32 = -60.0;

/// Opens the window for plug-in `P` and runs until it's closed.
pub fn run<P: Plugin>() -> Result<(), String> {
    // The size until the editor gives its own, as the window opens.
    let settings = EguiWindowSettings::new()
        .with_title(P::INFO.name)
        .with_size(LogicalSize::new(640.0, 400.0 + STRIP_HEIGHT))
        .with_min_size(Some(LogicalSize::new(MIN_WIDTH, STRIP_HEIGHT)))
        .with_resizable(true);
    let window = EguiWindow::create(settings, MainWindow::<P> { inner: None })
        .map_err(|e| format!("the window didn't open: {e}"))?;
    window
        .run_until_closed()
        .map_err(|e| format!("the window failed: {e}"))
}

/// The window's app. It holds nothing until the window opens.
struct MainWindow<P: Plugin> {
    inner: Option<Inner<P>>,
}

// SAFETY: `inner` is `None` while the value moves to the window's thread,
// and is made in `build`, on that thread, which the value never leaves
// again; so nothing that isn't `Send` crosses a thread.
unsafe impl<P: Plugin> Send for MainWindow<P> {}

/// Everything the window shows and drives.
struct Inner<P: Plugin> {
    app: Standalone<P>,
    theme: Theme,
    midi: MidiInputs,
    keyboard: ComputerKeyboard,
    gate: KeyboardGate,
    scanned: Instant,
    layout: Layout,
    /// The window's size, in points, when last looked at.
    seen: (f64, f64),
    /// What the window's own menu bar takes from its height when it's
    /// resized, in points (on Windows, where baseview leaves it out).
    menu_height: f64,
    #[cfg(any(target_os = "macos", windows))]
    menu: Option<crate::menu::NativeMenu>,
    panel: SharedPanel,
    settings_window: Option<egui_baseview::baseview::Window>,
    /// The meter's level now, in dB, falling back after each peak.
    level: f32,
    /// What last went wrong, and when.
    message: Option<(String, Instant)>,
}

impl<P: Plugin> egui_baseview::App for MainWindow<P> {
    fn build(&mut self, ctx: egui::Context, frame: &mut Frame) -> Result<(), HandlerError> {
        let theme = Theme::default();
        lspk_ui::editor::prepare(&ctx, &theme);
        frame.set_clear_color(theme.colors.background.into());
        let folder = settings::folder(P::INFO.id);
        let saved = folder.as_deref().map(Settings::load).unwrap_or_default();
        let keyboard = ComputerKeyboard::new(
            saved.keyboard_octave,
            if saved.keyboard_velocity == 0 {
                100
            } else {
                saved.keyboard_velocity
            },
        );
        let mut app = Standalone::<P>::new(Box::new(CpalBackend::new()), saved, folder)
            .map_err(|e| HandlerError::from_boxed(e.to_string().into()))?;
        let mut messages = Vec::new();
        messages.extend(app.restore().err());
        app.start();
        let mut midi = MidiInputs::new(P::INFO.name, app.midi().clone());
        midi.scan();

        let window = frame.baseview_window();
        let mut menu_height = 0.0;
        #[cfg(any(target_os = "macos", windows))]
        let menu = {
            let before = window.size().logical.height;
            match crate::menu::NativeMenu::install(P::INFO.name, P::INFO.version, window) {
                Ok(menu) => {
                    if cfg!(windows) {
                        menu_height = (before - window.size().logical.height).max(0.0);
                    }
                    Some(menu)
                }
                Err(error) => {
                    messages.push(format!("The menu bar didn't open: {error}"));
                    None
                }
            }
        };
        #[cfg(windows)]
        clip_children(window);

        let layout = Layout {
            api: WindowApi::native().unwrap_or(WindowApi::X11),
            scale: window.scale_factor(),
        };
        match parent_window(window)
            .ok_or_else(|| "the window has no handle the editor can open in".to_owned())
            .and_then(|parent| app.embed_editor(parent, layout.scale))
        {
            Ok(size) => resize(window, layout.window_for(size), &mut menu_height),
            Err(error) => messages.push(format!("The editor didn't open: {error}")),
        }
        for message in &messages {
            eprintln!("{message}");
        }

        let panel = Panel {
            devices: app.devices().unwrap_or_default(),
            settings: app.settings().clone(),
            has_input: Standalone::<P>::has_input(),
            midi: midi_text(&midi),
            state_files: !Platform::NATIVE.has_menu_bar(),
            ..Panel::default()
        };
        let seen = size_of(window);
        self.inner = Some(Inner {
            app,
            theme,
            midi,
            keyboard,
            gate: KeyboardGate::default(),
            scanned: Instant::now(),
            layout,
            seen,
            menu_height,
            #[cfg(any(target_os = "macos", windows))]
            menu,
            panel: Arc::new(Mutex::new(panel)),
            settings_window: None,
            level: METER_FLOOR,
            message: messages.pop().map(|m| (m, Instant::now())),
        });
        Ok(())
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut Frame) {
        if let Some(inner) = &mut self.inner {
            inner.frame(ui, frame.baseview_window());
        }
        frame.baseview_window().request_redraw_after(POLL);
    }
}

impl<P: Plugin> Drop for Inner<P> {
    fn drop(&mut self) {
        self.settings_window = None;
        self.release_all();
        self.app.shutdown();
    }
}

impl<P: Plugin> Inner<P> {
    fn frame(&mut self, ui: &mut Ui, window: &WindowContext) {
        self.app.poll();
        if self.scanned.elapsed() >= SCAN {
            self.scanned = Instant::now();
            self.midi.scan();
            let devices = self.app.devices();
            {
                let mut panel = lock(&self.panel);
                if let Ok(devices) = devices {
                    panel.devices = devices;
                }
                panel.midi = midi_text(&self.midi);
            }
            if let Err(error) = self.app.autosave() {
                self.tell(error);
            }
        }
        let mut commands = Vec::new();
        #[cfg(any(target_os = "macos", windows))]
        if let Some(menu) = &self.menu {
            commands.extend(menu.chosen());
        }
        let wanted = {
            let mut panel = lock(&self.panel);
            commands.append(&mut panel.commands);
            panel.wanted.take()
        };
        if let Some(wanted) = wanted {
            self.app.set_settings(wanted);
        }
        for command in commands {
            self.run(command, window);
        }
        self.follow_the_editor(window);
        if Standalone::<P>::takes_notes() {
            self.computer_keyboard(ui);
        } else {
            // Nothing plays them; don't let them pile up.
            let _ = self.app.take_editor_keys();
        }
        let peak = self.app.meter().take();
        let db = if peak > 0.0 {
            20.0 * peak.log10()
        } else {
            METER_FLOOR
        };
        // Falls about 20 dB a second at 30 frames a second.
        self.level = db.max(self.level - 0.7).max(METER_FLOOR);

        let theme = self.theme.clone();
        let strip = egui::Frame::NONE
            .fill(theme.colors.background)
            .stroke(egui::Stroke::new(1.0, theme.colors.separator))
            .inner_margin(egui::Margin::symmetric(theme.sizes.spacing as i8, 0));
        egui::Panel::bottom("lspk-standalone-strip")
            .exact_size(STRIP_HEIGHT as f32)
            .resizable(false)
            .frame(strip)
            .show(ui, |ui| self.strip(ui));
        // The editor's window covers the rest.
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme.colors.background))
            .show(ui, |_| {});
        {
            let mut panel = lock(&self.panel);
            panel.settings = self.app.settings().clone();
            panel.message = self.message.as_ref().map(|(m, _)| m.clone());
        }
    }

    fn tell(&mut self, message: String) {
        eprintln!("{message}");
        self.message = Some((message, Instant::now()));
    }

    fn run(&mut self, command: Command, window: &WindowContext) {
        match command {
            Command::OpenState => self.open_state_file(),
            Command::SaveState => self.save_state_file(),
            Command::Settings => self.open_settings(),
            Command::Quit => window.request_close(),
        }
    }

    /// Keeps the window the editor's size: it follows the editor's own
    /// resizes, and when the person resizes the window, the editor takes
    /// the room, as near as it can.
    fn follow_the_editor(&mut self, window: &WindowContext) {
        let scale = window.scale_factor();
        if (scale - self.layout.scale).abs() > 1e-6 {
            self.layout.scale = scale;
            self.app.set_editor_scale(scale);
        }
        if let Some(size) = self.app.take_editor_resize() {
            self.resize(window, self.layout.window_for(size));
            return;
        }
        let now = size_of(window);
        if now == self.seen {
            return;
        }
        self.seen = now;
        let Some(fitted) = self.app.fit_editor(self.layout.editor_room(now)) else {
            return;
        };
        let wanted = self.layout.window_for(fitted);
        if (wanted.0 - now.0).abs() > 0.5 || (wanted.1 - now.1).abs() > 0.5 {
            self.resize(window, wanted);
        }
    }

    fn resize(&mut self, window: &WindowContext, size: (f64, f64)) {
        resize(window, size, &mut self.menu_height);
        self.seen = size_of(window);
    }

    /// Plays the computer keyboard while the window has the focus and no
    /// text field has the keys: the strip's own keys, and those the editor
    /// passes on.
    fn computer_keyboard(&mut self, ui: &Ui) {
        let (focused, events) = ui.input(|i| (i.focused, i.events.clone()));
        let strip = KeyboardFocus {
            focused,
            typing: focused && ui.ctx().egui_wants_keyboard_input(),
        };
        let focus = WindowFocus {
            strip,
            editor: self.app.editor_focus(),
        };
        let on = !self.app.settings().keyboard_off;
        if self.gate.update(Instant::now(), on, focus) {
            self.release_all();
        }
        // macOS also hands this window the keys the editor leaves; the
        // editor passes those on itself, so they count only from there.
        let mut keys = if strip.focused {
            lspk_ui::editor::plain_keys(&events)
        } else {
            Vec::new()
        };
        keys.extend(self.app.take_editor_keys());
        if self.gate.is_open() {
            self.play(&keys);
        }
    }

    fn play(&mut self, keys: &[EditorKey]) {
        let before = (self.keyboard.octave(), self.keyboard.velocity());
        for key in keys {
            let message = if key.pressed {
                self.keyboard.press(key.key)
            } else {
                self.keyboard.release(key.key)
            };
            if let Some(message) = message {
                self.app.midi().send(message);
            }
        }
        if before != (self.keyboard.octave(), self.keyboard.velocity()) {
            self.keep_keyboard();
        }
    }

    fn release_all(&mut self) {
        for message in self.keyboard.release_all() {
            self.app.midi().send(message);
        }
    }

    /// Saves the keyboard's octave and velocity in the settings.
    fn keep_keyboard(&mut self) {
        let mut settings = self.app.settings().clone();
        settings.keyboard_octave = self.keyboard.octave();
        settings.keyboard_velocity = self.keyboard.velocity();
        self.app.set_settings(settings);
    }

    fn strip(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        let items = layout::strip(
            Standalone::<P>::takes_notes(),
            Platform::NATIVE.has_menu_bar(),
        );
        ui.horizontal_centered(|ui| {
            for item in items {
                match item {
                    StripItem::Status => {
                        let (text, colour) = self.status_text();
                        ui.add_sized(
                            [220.0, STRIP_HEIGHT as f32],
                            egui::Label::new(RichText::new(text).color(colour)).truncate(),
                        );
                    }
                    StripItem::Meter => meter(ui, self.level, &theme),
                    StripItem::Keyboard => {
                        ui.separator();
                        let mut on = !self.app.settings().keyboard_off;
                        if ui
                            .checkbox(&mut on, "Keys")
                            .on_hover_text(
                                "Play from the computer keyboard: A to ; play, \
                                 Z and X change octave, C and V velocity",
                            )
                            .changed()
                        {
                            let mut settings = self.app.settings().clone();
                            settings.keyboard_off = !on;
                            self.app.set_settings(settings);
                        }
                    }
                    StripItem::Octave => {
                        let text = note_name(self.keyboard.base_key());
                        match stepper(ui, &text, "Octave (Z and X)", &theme) {
                            -1 => self.step_keyboard('z'),
                            1 => self.step_keyboard('x'),
                            _ => {}
                        }
                    }
                    StripItem::Velocity => {
                        let text = format!("Vel {}", self.keyboard.velocity());
                        match stepper(ui, &text, "Velocity (C and V)", &theme) {
                            -1 => self.step_keyboard('c'),
                            1 => self.step_keyboard('v'),
                            _ => {}
                        }
                    }
                    StripItem::Settings => {
                        ui.separator();
                        if ui.button("Settings…").clicked() {
                            self.open_settings();
                        }
                    }
                }
            }
        });
    }

    /// Presses one of the keyboard's octave or velocity keys, as the strip's
    /// buttons do.
    fn step_keyboard(&mut self, key: char) {
        self.keyboard.press(key);
        self.keep_keyboard();
    }

    /// What the strip says of the audio, or what last went wrong.
    fn status_text(&mut self) -> (String, Color32) {
        if let Some((message, at)) = &self.message {
            if at.elapsed() < MESSAGE {
                return (message.clone(), self.theme.colors.focus);
            }
            self.message = None;
        }
        match self.app.status() {
            Status::Running(info) => (
                format!(
                    "{}{}, {} Hz",
                    info.output_name,
                    if info.fallback {
                        " (the chosen device isn't connected)"
                    } else {
                        ""
                    },
                    info.sample_rate
                ),
                self.theme.colors.text,
            ),
            Status::Waiting(why) => (format!("Not playing: {why}"), self.theme.colors.focus),
            Status::Stopped => ("Stopped".to_owned(), self.theme.colors.text_dim),
        }
    }

    fn open_settings(&mut self) {
        if lock(&self.panel).dialog_open
            && let Some(window) = &self.settings_window
        {
            let _ = window.show();
            return;
        }
        // A closed dialog's window goes first.
        self.settings_window = None;
        match settings_window::open(P::INFO.name, self.panel.clone(), self.theme.clone()) {
            Ok(window) => self.settings_window = Some(window),
            Err(error) => self.tell(error),
        }
    }

    fn save_state_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Plug-in state", &[settings::STATE_EXTENSION])
            .set_file_name(format!("{}.{}", P::INFO.name, settings::STATE_EXTENSION))
            .save_file()
        else {
            return;
        };
        let path = with_extension(path);
        if let Err(error) = self.app.save_state_file(&path) {
            self.tell(error);
        }
    }

    fn open_state_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Plug-in state", &[settings::STATE_EXTENSION])
            .pick_file()
        else {
            return;
        };
        if let Err(error) = self.app.open_state_file(&path) {
            self.tell(error);
        }
    }
}

/// Resizes `window` to `size` points, below a menu bar of `menu_height`
/// points. On Windows, where baseview leaves the menu bar out of the
/// window's height and the bar wraps when the window narrows, it learns
/// the bar's height from what the window ends up with.
fn resize(window: &WindowContext, size: (f64, f64), menu_height: &mut f64) {
    let _ = window.resize(LogicalSize::new(size.0, size.1 + *menu_height));
    if cfg!(windows) {
        let short = size.1 - size_of(window).1;
        if short.abs() > 0.5 && short.abs() < 100.0 {
            *menu_height = (*menu_height + short).max(0.0);
            let _ = window.resize(LogicalSize::new(size.0, size.1 + *menu_height));
        }
    }
}

/// The window's size in points.
fn size_of(window: &WindowContext) -> (f64, f64) {
    let size = window.size().logical;
    (size.width, size.height)
}

/// The window, as the editor's parent.
#[allow(
    clippy::useless_conversion,
    reason = "an X11 window ID is a `c_ulong`, which is 32 bits on Windows"
)]
fn parent_window(window: &impl HasWindowHandle) -> Option<ParentWindow> {
    Some(match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::AppKit(handle) => ParentWindow::AppKit(handle.ns_view),
        RawWindowHandle::Win32(handle) => {
            ParentWindow::Win32(std::ptr::NonNull::new(handle.hwnd.get() as *mut _)?)
        }
        RawWindowHandle::Xlib(handle) => ParentWindow::X11(u64::from(handle.window)),
        RawWindowHandle::Xcb(handle) => ParentWindow::X11(u64::from(handle.window.get())),
        _ => return None,
    })
}

/// Keeps the window's own drawing off the editor's window inside it.
#[cfg(windows)]
fn clip_children(window: &impl HasWindowHandle) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_CLIPCHILDREN,
    };
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    // SAFETY: `hwnd` is the app's own window, open while this runs.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style | WS_CLIPCHILDREN as isize);
    }
}

/// The MIDI inputs, or why there are none.
fn midi_text(midi: &MidiInputs) -> String {
    let names = midi.names();
    match (names.is_empty(), midi.error()) {
        (_, Some(error)) => error.to_owned(),
        (true, None) => "No MIDI inputs connected".to_owned(),
        (false, None) => names.join(", "),
    }
}

/// `path`, with the state extension if it has none.
fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension(settings::STATE_EXTENSION)
    }
}

/// `text` between a minus and a plus button; returns −1 or +1 for a click
/// on either, else 0.
fn stepper(ui: &mut Ui, text: &str, hover: &str, theme: &Theme) -> i8 {
    let mut step = 0;
    if ui.small_button("−").on_hover_text(hover).clicked() {
        step = -1;
    }
    ui.label(RichText::new(text).color(theme.colors.text))
        .on_hover_text(hover);
    if ui.small_button("+").on_hover_text(hover).clicked() {
        step = 1;
    }
    step
}

/// A level meter from [`METER_FLOOR`] to 0 dB.
fn meter(ui: &mut Ui, db: f32, theme: &Theme) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(100.0, 6.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme.colors.field);
    let fill = ((db - METER_FLOOR) / -METER_FLOOR).clamp(0.0, 1.0);
    let mut lit = rect;
    lit.set_width(rect.width() * fill);
    let colour = if db > -1.0 {
        Color32::from_rgb(0xe0, 0x5a, 0x4a)
    } else {
        theme.colors.accent
    };
    painter.rect_filled(lit, 2.0, colour);
}

/// A key's name, such as `C3` for middle C (60), as Live numbers octaves.
fn note_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = i16::from(key / 12) - 2;
    format!("{}{octave}", NAMES[usize::from(key % 12)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_named_with_middle_c_as_c3() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(0), "C-2");
        assert_eq!(note_name(69), "A3");
        assert_eq!(note_name(127), "G8");
    }
}
