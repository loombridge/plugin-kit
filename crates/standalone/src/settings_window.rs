//! The Settings dialog: the audio devices, the sample rate, the buffer
//! size and the MIDI inputs, and, where there's no menu bar, the state
//! files. It's a window of its own, opened from the menu (or the strip's
//! Settings button), drawn with egui like the app's window.
//!
//! On X11 every window runs on a thread of its own, so the dialog never
//! touches the app: it draws from a [`Panel`] the app's window fills each
//! frame, and leaves the person's choices there for the app's window to
//! take.

use std::sync::{Arc, Mutex, PoisonError};

use egui_baseview::baseview::HandlerError;
use egui_baseview::baseview::dpi::LogicalSize;
use egui_baseview::{EguiWindow, EguiWindowSettings, Frame};
use lspk_ui::Theme;
use lspk_ui::egui::{self, ComboBox, RichText, Ui};

use crate::audio::DeviceInfo;
use crate::menu::Command;
use crate::settings::Settings;

/// What the dialog shows, and what the person chose in it, shared by the
/// app's window and the dialog.
#[derive(Debug, Default)]
pub(crate) struct Panel {
    /// The devices the backend has.
    pub(crate) devices: Vec<DeviceInfo>,
    /// The settings now.
    pub(crate) settings: Settings,
    /// Whether the plug-in takes audio in.
    pub(crate) has_input: bool,
    /// The MIDI inputs, or why there are none.
    pub(crate) midi: String,
    /// Whether to offer the state files, where there's no menu bar.
    pub(crate) state_files: bool,
    /// What last went wrong.
    pub(crate) message: Option<String>,
    /// Settings the person chose, for the app to take.
    pub(crate) wanted: Option<Settings>,
    /// Commands the person chose, for the app to run.
    pub(crate) commands: Vec<Command>,
    /// Whether the dialog is open: from when its window opens until it
    /// closes, which baseview's `is_open` doesn't report on macOS.
    pub(crate) dialog_open: bool,
}

/// The panel, shared.
pub(crate) type SharedPanel = Arc<Mutex<Panel>>;

/// Locks `panel`, whatever a panic elsewhere left in it.
pub(crate) fn lock(panel: &SharedPanel) -> std::sync::MutexGuard<'_, Panel> {
    panel.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The dialog's rows, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Output,
    Input,
    SampleRate,
    BufferSize,
    Midi,
    StateFiles,
}

/// The rows for a plug-in that does or doesn't take audio in, with or
/// without the state files.
pub(crate) fn rows(has_input: bool, state_files: bool) -> Vec<Row> {
    let mut rows = vec![Row::Output];
    if has_input {
        rows.push(Row::Input);
    }
    rows.extend([Row::SampleRate, Row::BufferSize, Row::Midi]);
    if state_files {
        rows.push(Row::StateFiles);
    }
    rows
}

/// Opens the dialog for the plug-in named `app`, drawing from `panel`.
pub(crate) fn open(
    app: &str,
    panel: SharedPanel,
    theme: Theme,
) -> Result<egui_baseview::baseview::Window, String> {
    let settings = EguiWindowSettings::new()
        .with_title(format!("{app} Settings"))
        .with_size(LogicalSize::new(560.0, 300.0))
        .with_resizable(false);
    let window = EguiWindow::create(settings, Dialog { panel, theme })
        .map_err(|e| format!("the Settings dialog didn't open: {e}"))?;
    window
        .show()
        .map_err(|e| format!("the Settings dialog didn't show: {e}"))?;
    Ok(window)
}

struct Dialog {
    panel: SharedPanel,
    theme: Theme,
}

impl Drop for Dialog {
    fn drop(&mut self) {
        lock(&self.panel).dialog_open = false;
    }
}

impl egui_baseview::App for Dialog {
    fn build(&mut self, ctx: egui::Context, frame: &mut Frame) -> Result<(), HandlerError> {
        lock(&self.panel).dialog_open = true;
        lspk_ui::editor::prepare(&ctx, &self.theme);
        frame.set_clear_color(self.theme.colors.background.into());
        Ok(())
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut Frame) {
        let theme = self.theme.clone();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme.colors.background)
                    .inner_margin(theme.sizes.spacing * 2.0),
            )
            .show(ui, |ui| form(ui, &mut lock(&self.panel), &theme));
        // The app's window changes the panel; look again soon.
        frame
            .baseview_window()
            .request_redraw_after(std::time::Duration::from_millis(100));
    }
}

/// Draws the dialog's rows from `panel`, leaving the person's choices in
/// it.
pub(crate) fn form(ui: &mut Ui, panel: &mut Panel, theme: &Theme) {
    let dim = theme.colors.text_dim;
    let current = panel
        .wanted
        .clone()
        .unwrap_or_else(|| panel.settings.clone());
    let mut wanted = current.clone();
    egui::Grid::new("lspk-standalone-settings")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            for row in rows(panel.has_input, panel.state_files) {
                match row {
                    Row::Output => {
                        ui.label("Output");
                        device_choice(ui, "output", &mut wanted.output, &panel.devices, true);
                    }
                    Row::Input => {
                        ui.label("Input");
                        device_choice(ui, "input", &mut wanted.input, &panel.devices, false);
                    }
                    Row::SampleRate => {
                        let device = output_device(&panel.devices, &current);
                        ui.label("Sample rate");
                        let rates = device.map(|d| d.sample_rates.clone()).unwrap_or_default();
                        let own = device.and_then(|d| d.current_rate).map_or_else(
                            || "The device's own".to_owned(),
                            |r| format!("The device's own ({r} Hz)"),
                        );
                        number_choice(ui, "rate", &mut wanted.sample_rate, &rates, &own, " Hz");
                    }
                    Row::BufferSize => {
                        let device = output_device(&panel.devices, &current);
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
                    }
                    Row::Midi => {
                        ui.label("MIDI in");
                        ui.label(RichText::new(&panel.midi).color(dim));
                    }
                    Row::StateFiles => {
                        ui.label("State");
                        ui.horizontal(|ui| {
                            if ui.button("Open state…").clicked() {
                                panel.commands.push(Command::OpenState);
                            }
                            if ui.button("Save state…").clicked() {
                                panel.commands.push(Command::SaveState);
                            }
                        });
                    }
                }
                ui.end_row();
            }
        });
    if wanted != current {
        panel.wanted = Some(wanted);
    }
    if let Some(message) = &panel.message {
        ui.add_space(8.0);
        ui.label(RichText::new(message).color(theme.colors.focus));
    }
}

/// The output device `settings` choose, or the default.
fn output_device<'a>(devices: &'a [DeviceInfo], settings: &Settings) -> Option<&'a DeviceInfo> {
    let chosen = settings
        .output
        .as_ref()
        .and_then(|id| devices.iter().find(|d| &d.id == id));
    chosen.or_else(|| devices.iter().find(|d| d.is_default_output))
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
        .width(360.0)
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
        .width(360.0)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(choice, None, own);
            for n in options {
                ui.selectable_value(choice, Some(*n), format!("{n}{unit}"));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_offers_the_input_and_state_files_only_where_needed() {
        use Row::*;
        assert_eq!(rows(false, false), [Output, SampleRate, BufferSize, Midi]);
        assert_eq!(
            rows(true, true),
            [Output, Input, SampleRate, BufferSize, Midi, StateFiles]
        );
    }

    #[test]
    fn the_form_draws_headless_and_changes_nothing_by_itself() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut panel = Panel {
            has_input: true,
            state_files: true,
            midi: "No MIDI inputs connected".to_owned(),
            message: Some("couldn't open x".to_owned()),
            ..Panel::default()
        };
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            form(ui, &mut panel, &theme);
        });
        output.textures_delta.clear();
        assert!(panel.wanted.is_none());
        assert!(panel.commands.is_empty());
    }
}
