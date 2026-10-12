//! The app's window, drawn with egui in a `baseview` window, as the kit's
//! editors are (`lspk-ui`).
//!
//! It holds the device settings, the state files, the MIDI inputs and the
//! computer keyboard, and a level meter. A plug-in with an editor shows it
//! in a window of its own beside this one, as a host shows a floating
//! editor; a plug-in without one gets its parameters drawn here, with the
//! kit's generic interface. Closing this window quits the app, saving the
//! settings and the plug-in's state for the next run.
//!
//! Everything runs on the window's thread, which is the plug-in's main
//! thread: the main thread on macOS and Windows, and a thread of the
//! window's own on X11. The app is made there, when the window opens.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_baseview::baseview::HandlerError;
use egui_baseview::baseview::dpi::LogicalSize;
use egui_baseview::{EguiWindow, EguiWindowSettings, Frame};
use lspk_plugin::Plugin;
use lspk_ui::egui::{self, Color32, ComboBox, Key, RichText, Ui};
use lspk_ui::{GenericView, Theme};

use crate::audio::DeviceInfo;
use crate::keyboard::ComputerKeyboard;
use crate::midi::MidiInputs;
use crate::settings::{self, Settings};
use crate::{CpalBackend, Standalone, Status};

/// How often the window polls the app and redraws.
const POLL: Duration = Duration::from_millis(33);
/// How often it looks for new MIDI inputs and audio devices, and saves the
/// plug-in's state if it has changed.
const SCAN: Duration = Duration::from_secs(2);
/// The quietest level the meter shows, in dB.
const METER_FLOOR: f32 = -60.0;

/// Opens the window for plug-in `P` and runs until it's closed.
pub fn run<P: Plugin>() -> Result<(), String> {
    let has_view_room = (640.0, 520.0);
    let settings = EguiWindowSettings::new()
        .with_title(P::INFO.name)
        .with_size(LogicalSize::new(has_view_room.0, has_view_room.1))
        .with_min_size(Some(LogicalSize::new(480.0, 360.0)))
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
    devices: Vec<DeviceInfo>,
    scanned: Instant,
    /// The plug-in's parameters, drawn here when it has no editor.
    view: GenericView,
    zoom: f32,
    /// The meter's level now, in dB, falling back after each peak.
    level: f32,
    /// What last went wrong with a file or the editor.
    message: Option<String>,
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
        let mut message = app.restore().err();
        app.start();
        if app.has_editor()
            && let Err(error) = app.open_editor()
        {
            message = Some(format!("The editor didn't open: {error}"));
        }
        if let Some(message) = &message {
            eprintln!("{message}");
        }
        let mut midi = MidiInputs::new(P::INFO.name, app.midi().clone());
        midi.scan();
        let devices = app.devices().unwrap_or_default();
        self.inner = Some(Inner {
            app,
            zoom: theme.zoom,
            theme,
            midi,
            keyboard,
            devices,
            scanned: Instant::now(),
            view: GenericView::new(P::INFO.name),
            level: METER_FLOOR,
            message,
        });
        Ok(())
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut Frame) {
        if let Some(inner) = &mut self.inner {
            inner.frame(ui);
        }
        frame.baseview_window().request_redraw_after(POLL);
    }
}

impl<P: Plugin> Drop for Inner<P> {
    fn drop(&mut self) {
        for message in self.keyboard.release_all() {
            self.app.midi().send(message);
        }
        self.app.shutdown();
    }
}

impl<P: Plugin> Inner<P> {
    fn frame(&mut self, ui: &mut Ui) {
        self.app.poll();
        if self.scanned.elapsed() >= SCAN {
            self.scanned = Instant::now();
            self.midi.scan();
            if let Ok(devices) = self.app.devices() {
                self.devices = devices;
            }
            if let Err(error) = self.app.autosave() {
                self.message = Some(error);
            }
        }
        if Standalone::<P>::takes_notes() {
            self.computer_keyboard(ui);
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
        let frame = egui::Frame::NONE
            .fill(theme.colors.background)
            .inner_margin(theme.sizes.spacing);
        egui::Panel::top("lspk-standalone-settings")
            .frame(frame)
            .show(ui, |ui| self.settings_panel(ui));
        if self.app.has_editor() {
            egui::CentralPanel::default()
                .frame(frame)
                .show(ui, |ui| self.editor_panel(ui));
        } else {
            let view = &mut self.view;
            self.zoom = lspk_ui::editor::draw(ui, view, self.app.view(), &theme, self.zoom);
            let want = self.zoom;
            if (ui.ctx().zoom_factor() - want).abs() > 1e-4 {
                ui.ctx().set_zoom_factor(want);
            }
        }
    }

    /// Plays the computer keyboard's notes, while no text field has it.
    fn computer_keyboard(&mut self, ui: &Ui) {
        let (events, focused) = ui.input(|i| (i.events.clone(), i.focused));
        if !focused || ui.ctx().egui_wants_keyboard_input() {
            for message in self.keyboard.release_all() {
                self.app.midi().send(message);
            }
            return;
        }
        let (octave, velocity) = (self.keyboard.octave(), self.keyboard.velocity());
        for event in events {
            let egui::Event::Key {
                key,
                pressed,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if modifiers.command || modifiers.ctrl || modifiers.alt || repeat {
                continue;
            }
            let Some(c) = key_char(key) else { continue };
            let message = if pressed {
                self.keyboard.press(c)
            } else {
                self.keyboard.release(c)
            };
            if let Some(message) = message {
                self.app.midi().send(message);
            }
        }
        if (octave, velocity) != (self.keyboard.octave(), self.keyboard.velocity()) {
            let mut settings = self.app.settings().clone();
            settings.keyboard_octave = self.keyboard.octave();
            settings.keyboard_velocity = self.keyboard.velocity();
            self.app.set_settings(settings);
        }
    }

    fn settings_panel(&mut self, ui: &mut Ui) {
        let theme = self.theme.clone();
        let dim = theme.colors.text_dim;
        ui.horizontal(|ui| {
            ui.heading(P::INFO.name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Save state…").clicked() {
                    self.save_state_file();
                }
                if ui.button("Open state…").clicked() {
                    self.open_state_file();
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (text, colour) = match self.app.status() {
                Status::Running(info) => (
                    format!(
                        "Playing through {}{} at {} Hz",
                        info.output_name,
                        if info.fallback {
                            " (the chosen device isn't connected)"
                        } else {
                            ""
                        },
                        info.sample_rate
                    ),
                    theme.colors.text,
                ),
                Status::Waiting(why) => (format!("Not playing: {why}"), theme.colors.focus),
                Status::Stopped => ("Stopped".to_owned(), dim),
            };
            ui.label(RichText::new(text).color(colour));
        });
        meter(ui, self.level, &theme);
        ui.add_space(8.0);

        let current = self.app.settings().clone();
        let mut wanted = current.clone();
        egui::Grid::new("lspk-standalone-audio")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Output");
                device_choice(ui, "output", &mut wanted.output, &self.devices, true);
                ui.end_row();
                if Standalone::<P>::has_input() {
                    ui.label("Input");
                    device_choice(ui, "input", &mut wanted.input, &self.devices, false);
                    ui.end_row();
                }
                let device = self.output_device(&current);
                ui.label("Sample rate");
                let rates = device.map(|d| d.sample_rates.clone()).unwrap_or_default();
                let own = device.and_then(|d| d.current_rate).map_or_else(
                    || "The device's own".to_owned(),
                    |r| format!("The device's own ({r} Hz)"),
                );
                number_choice(ui, "rate", &mut wanted.sample_rate, &rates, &own, " Hz");
                ui.end_row();
                ui.label("Buffer size");
                let sizes = device
                    .map(DeviceInfo::standard_buffer_sizes)
                    .unwrap_or_default();
                number_choice(
                    ui,
                    "buffer",
                    &mut wanted.buffer_size,
                    &sizes,
                    "The device's own",
                    " frames",
                );
                ui.end_row();
                ui.label("MIDI in");
                let names = self.midi.names();
                let text = match (names.is_empty(), self.midi.error()) {
                    (_, Some(error)) => error.to_owned(),
                    (true, None) => "No MIDI inputs connected".to_owned(),
                    (false, None) => names.join(", "),
                };
                ui.label(RichText::new(text).color(dim));
                ui.end_row();
                if Standalone::<P>::takes_notes() {
                    ui.label("Computer keyboard");
                    ui.label(
                        RichText::new(format!(
                            "A to ; play from {}, Z and X change octave, C and V velocity ({})",
                            note_name(self.keyboard.base_key()),
                            self.keyboard.velocity()
                        ))
                        .color(dim),
                    );
                    ui.end_row();
                }
            });
        if wanted != current {
            self.app.set_settings(wanted);
        }
        if let Some(message) = &self.message {
            ui.add_space(6.0);
            ui.label(RichText::new(message).color(theme.colors.focus));
        }
    }

    fn editor_panel(&mut self, ui: &mut Ui) {
        if self.app.editor_is_open() {
            ui.label(
                RichText::new("The plug-in's editor is open in its own window.")
                    .color(self.theme.colors.text_dim),
            );
            if ui.button("Close editor").clicked() {
                self.app.close_editor();
            }
        } else if ui.button("Show editor").clicked() {
            self.message = self
                .app
                .open_editor()
                .err()
                .map(|e| format!("The editor didn't open: {e}"));
        }
    }

    /// The output device the settings choose, or the default.
    fn output_device(&self, settings: &Settings) -> Option<&DeviceInfo> {
        let chosen = settings
            .output
            .as_ref()
            .and_then(|id| self.devices.iter().find(|d| &d.id == id));
        chosen.or_else(|| self.devices.iter().find(|d| d.is_default_output))
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
        self.message = self.app.save_state_file(&path).err();
    }

    fn open_state_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Plug-in state", &[settings::STATE_EXTENSION])
            .pick_file()
        else {
            return;
        };
        self.message = self.app.open_state_file(&path).err();
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

/// A choice of device: the system's default, or one with outputs (or
/// inputs).
fn device_choice(
    ui: &mut Ui,
    id: &str,
    choice: &mut Option<String>,
    devices: &[DeviceInfo],
    output: bool,
) {
    let usable: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|d| if output { d.outputs > 0 } else { d.inputs > 0 })
        .collect();
    let default = usable
        .iter()
        .find(|d| {
            if output {
                d.is_default_output
            } else {
                d.is_default_input
            }
        })
        .map_or_else(
            || "The system's default".to_owned(),
            |d| format!("The system's default ({})", d.name),
        );
    let selected = match choice {
        None => default.clone(),
        Some(id) => usable
            .iter()
            .find(|d| &d.id == id)
            .map_or_else(|| format!("{id} (not connected)"), |d| d.name.clone()),
    };
    ComboBox::from_id_salt(id)
        .width(320.0)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(choice, None, default);
            for device in usable {
                ui.selectable_value(choice, Some(device.id.clone()), &device.name);
            }
        });
}

/// A choice of number from `options`, or `None` for the device's own.
fn number_choice(
    ui: &mut Ui,
    id: &str,
    choice: &mut Option<u32>,
    options: &[u32],
    own: &str,
    unit: &str,
) {
    let selected = choice.map_or_else(|| own.to_owned(), |n| format!("{n}{unit}"));
    ComboBox::from_id_salt(id)
        .width(320.0)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(choice, None, own);
            for n in options {
                ui.selectable_value(choice, Some(*n), format!("{n}{unit}"));
            }
        });
}

/// A level meter from [`METER_FLOOR`] to 0 dB.
fn meter(ui: &mut Ui, db: f32, theme: &Theme) {
    let width = ui.available_width().min(320.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
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

/// The character a key types, for the computer keyboard.
fn key_char(key: Key) -> Option<char> {
    Some(match key {
        Key::A => 'a',
        Key::C => 'c',
        Key::D => 'd',
        Key::E => 'e',
        Key::F => 'f',
        Key::G => 'g',
        Key::H => 'h',
        Key::J => 'j',
        Key::K => 'k',
        Key::L => 'l',
        Key::O => 'o',
        Key::P => 'p',
        Key::S => 's',
        Key::T => 't',
        Key::U => 'u',
        Key::V => 'v',
        Key::W => 'w',
        Key::X => 'x',
        Key::Y => 'y',
        Key::Z => 'z',
        Key::Semicolon => ';',
        _ => return None,
    })
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

    #[test]
    fn letter_keys_type_their_letters() {
        assert_eq!(key_char(Key::A), Some('a'));
        assert_eq!(key_char(Key::Semicolon), Some(';'));
        assert_eq!(key_char(Key::Q), None);
    }
}
