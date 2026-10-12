//! The controls a parameter is drawn with, from its descriptor: a knob for
//! a continuous value or a gain, a switch for a toggle, a chooser for a
//! choice, and a number box for a whole number. Each shows its name above
//! and its value as text, takes a typed value, works from the keyboard,
//! exposes its role, name and value through AccessKit, and sends every
//! change to the host as a gesture.
//!
//! How each control behaves:
//!
//! - **Mouse.** Dragging a knob or number box up or down moves it,
//!   [`DRAG_TRAVEL`] points from one end of its range to the other, and
//!   [`FINE`] times finer with Shift held, in one gesture from press to
//!   release. A double-click sets the default. Clicking a switch flips it;
//!   clicking a chooser opens its list. Clicking a value opens typed entry.
//! - **Keyboard.** Tab and Shift-Tab move between controls. On a knob or
//!   number box, the arrow keys step it (one step of a stepped parameter,
//!   or 1 % of the travel; 0.1 % with Shift), Page Up and Page Down step ten
//!   times as far, Home and End go to its ends, Delete or Backspace sets
//!   the default, and Enter, or typing a digit, opens typed entry, which
//!   Enter takes and Escape cancels. Space or Enter flips a switch; the
//!   arrow keys move a chooser through its choices, and Space opens its
//!   list. Escape leaves the control, giving the keyboard back to the host.
//! - **Screen readers.** A knob is a slider and a number box a spin
//!   button, each with its value as text and as a number, its range, and
//!   the increment, decrement and set-value actions; a switch is a switch
//!   with its state; a chooser is a combo box with its choice.

use std::f32::consts::PI;

use egui::accesskit::{Action, ActionData, Role, Toggled};
use egui::{
    Align2, Event, EventFilter, Id, Key, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    WidgetInfo, WidgetType, pos2, vec2,
};
use lspk_params::{ParamDescriptor, ParamId, ParamKind};
use lspk_plugin::EditorContext;

use crate::theme::Theme;

/// Points of vertical drag that move a knob or number box from one end of
/// its range to the other.
pub const DRAG_TRAVEL: f32 = 200.0;

/// How much finer a drag or a key step is with Shift held.
pub const FINE: f32 = 10.0;

/// Which control a parameter is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    /// A rotary knob, for a continuous value or a gain.
    Knob,
    /// An on-off switch, for a toggle.
    Switch,
    /// A list to choose from, for a choice.
    Chooser,
    /// A box holding a whole number.
    NumberBox,
}

impl ControlKind {
    /// The control for `param`.
    #[must_use]
    pub fn for_param(param: &ParamDescriptor) -> Self {
        match param.kind {
            ParamKind::Continuous { .. } | ParamKind::Gain { .. } => Self::Knob,
            ParamKind::Toggle => Self::Switch,
            ParamKind::Choice { .. } => Self::Chooser,
            ParamKind::Integer { .. } => Self::NumberBox,
        }
    }

    /// The role a screen reader announces it with.
    #[must_use]
    pub fn role(self) -> Role {
        match self {
            Self::Knob => Role::Slider,
            Self::Switch => Role::Switch,
            Self::Chooser => Role::ComboBox,
            Self::NumberBox => Role::SpinButton,
        }
    }
}

/// The egui ID of parameter `param`'s control.
#[must_use]
pub fn control_id(param: ParamId) -> Id {
    Id::new(("lspk-param", param.0))
}

/// Where the parameters with a gesture under way are kept, so a window
/// that closes mid-drag can end them.
fn gestures_id() -> Id {
    Id::new("lspk-open-gestures")
}

/// Ends every gesture a control started and hasn't ended, as a window
/// closes in the middle of a drag; the host would otherwise wait for an
/// end that never comes.
pub fn end_open_gestures(ctx: &egui::Context, cx: &EditorContext) {
    let open = ctx.data_mut(|data| data.remove_temp::<Vec<ParamId>>(gestures_id()));
    for id in open.unwrap_or_default() {
        let _ = cx.end_edit(id);
    }
}

/// Draws the control for the parameter at `index` in `cx`'s order, filling
/// `rect`: its name above, the control, and its value. Returns the
/// control's response.
///
/// # Panics
///
/// If there's no parameter at `index`.
pub fn param_control(
    ui: &mut Ui,
    cx: &EditorContext,
    theme: &Theme,
    index: usize,
    rect: Rect,
) -> Response {
    let values = cx.values();
    let descriptor = &values.descriptors()[index];
    let control = Control {
        cx,
        theme,
        d: descriptor,
        value: values.value_at(index),
        id: control_id(descriptor.id),
        rect,
        kind: ControlKind::for_param(descriptor),
    };
    control.show(ui)
}

/// A drag under way: where the value started on its travel, and how far
/// the pointer has moved, in travel.
#[derive(Clone, Copy, Debug, Default)]
struct Drag {
    start: f64,
    moved: f64,
}

/// Typed entry under way: its text, whether it opened this frame, and
/// whether its text was typed rather than the value's.
#[derive(Clone, Debug, Default)]
struct Entry {
    text: String,
    fresh: bool,
    typed: bool,
}

impl Entry {
    fn new(text: String, typed: bool) -> Self {
        Self {
            text,
            fresh: true,
            typed,
        }
    }
}

/// What a key press, a screen reader or a double-click asks a value
/// control to do.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    Steps(f64),
    To(f64),
    Default,
}

/// One control being drawn.
struct Control<'a> {
    cx: &'a EditorContext,
    theme: &'a Theme,
    d: &'a ParamDescriptor,
    value: f64,
    id: Id,
    rect: Rect,
    kind: ControlKind,
}

impl Control<'_> {
    fn show(&self, ui: &mut Ui) -> Response {
        let (name_row, body, value_row) = self.rows();
        ui.painter().text(
            name_row.center(),
            Align2::CENTER_CENTER,
            &self.d.name,
            self.theme.small_font(),
            self.theme.colors.text_dim,
        );
        let response = match self.kind {
            ControlKind::Knob => self.knob(ui, body, value_row),
            ControlKind::NumberBox => self.number_box(ui, body),
            ControlKind::Switch => self.switch(ui, body),
            ControlKind::Chooser => self.chooser(ui, body),
        };
        self.describe(ui, &response);
        response
    }

    /// The name row, the control's own area, and the value row.
    fn rows(&self) -> (Rect, Rect, Rect) {
        let s = &self.theme.sizes;
        let r = self.rect;
        let name = Rect::from_min_size(r.min, vec2(r.width(), s.small_text + 6.0));
        let value = Rect::from_min_max(pos2(r.left(), r.bottom() - s.field_height), r.max);
        let body = Rect::from_min_max(pos2(r.left(), name.bottom()), pos2(r.right(), value.top()));
        (name, body, value)
    }

    /// A field-sized rectangle centred in `area`.
    fn field(&self, area: Rect) -> Rect {
        let s = &self.theme.sizes;
        Rect::from_center_size(
            area.center(),
            vec2(area.width() - 8.0, s.field_height.min(area.height())),
        )
    }

    fn text(&self) -> String {
        self.d.format(self.value)
    }

    /// A knob: an arc showing the value over its range, a pointer, and the
    /// value below.
    fn knob(&self, ui: &mut Ui, body: Rect, value_row: Rect) -> Response {
        let response = ui.interact(self.rect, self.id, Sense::click_and_drag());
        self.value_input(ui, &response);
        let entry_open = self.typed_entry(ui, &response, self.field(value_row));
        let c = &self.theme.colors;
        let s = &self.theme.sizes;
        let painter = ui.painter();
        let centre = body.center();
        let radius = (s.knob.min(body.height()).min(body.width()) / 2.0 - s.stroke).max(4.0);
        // The arc sweeps 300 degrees, from lower left to lower right.
        let (from, sweep) = (PI * 0.5 + PI / 6.0, PI * 5.0 / 3.0);
        let arc = |to: f32| -> Vec<egui::Pos2> {
            let steps = 40;
            (0..=steps)
                .map(|i| {
                    let a = from + sweep * to * i as f32 / steps as f32;
                    centre + vec2(a.cos(), a.sin()) * radius
                })
                .collect()
        };
        painter.add(Shape::line(arc(1.0), Stroke::new(s.stroke, c.control)));
        let n = self.d.normalize(self.value) as f32;
        if n > 0.0 {
            painter.add(Shape::line(arc(n), Stroke::new(s.stroke, c.accent)));
        }
        let a = from + sweep * n;
        painter.line_segment(
            [
                centre + vec2(a.cos(), a.sin()) * radius * 0.35,
                centre + vec2(a.cos(), a.sin()) * radius,
            ],
            Stroke::new(2.0, c.text),
        );
        if !entry_open {
            painter.text(
                value_row.center(),
                Align2::CENTER_CENTER,
                self.text(),
                self.theme.small_font(),
                c.text,
            );
        }
        self.focus_ring(ui, &response, body.shrink2(vec2(0.0, 1.0)));
        response
    }

    /// A number box: a field with the value centred and a fill showing
    /// where it sits in its range.
    fn number_box(&self, ui: &mut Ui, body: Rect) -> Response {
        let field = self.field(body);
        let response = ui.interact(field, self.id, Sense::click_and_drag());
        self.value_input(ui, &response);
        let entry_open = self.typed_entry(ui, &response, field);
        if !entry_open {
            let c = &self.theme.colors;
            let corner = self.theme.sizes.corner;
            let painter = ui.painter();
            painter.rect_filled(field, corner, c.field);
            let n = self.d.normalize(self.value) as f32;
            let fill = Rect::from_min_size(field.min, vec2(field.width() * n, field.height()));
            painter.rect_filled(fill, corner, c.control);
            painter.text(
                field.center(),
                Align2::CENTER_CENTER,
                self.text(),
                self.theme.small_font(),
                c.text,
            );
        }
        self.focus_ring(ui, &response, field);
        response
    }

    /// A switch: a button showing its state, lit while on.
    fn switch(&self, ui: &mut Ui, body: Rect) -> Response {
        let button = self.field(body);
        let response = ui.interact(button, self.id, Sense::click());
        if response.clicked() {
            response.request_focus();
        }
        let on = self.value >= 0.5;
        let pressed = response.has_focus()
            && ui.input(|i| i.key_pressed(Key::Space) || i.key_pressed(Key::Enter));
        if response.clicked() || pressed {
            self.edit(if on { 0.0 } else { 1.0 });
        }
        let c = &self.theme.colors;
        let (face, ink) = if on {
            (c.accent, c.accent_text)
        } else {
            (c.control, c.text)
        };
        ui.painter()
            .rect_filled(button, self.theme.sizes.corner, face);
        ui.painter().text(
            button.center(),
            Align2::CENTER_CENTER,
            self.text(),
            self.theme.small_font(),
            ink,
        );
        self.focus_ring(ui, &response, button);
        response
    }

    /// A chooser: a field with the choice and a ▼, opening the list of
    /// choices.
    fn chooser(&self, ui: &mut Ui, body: Rect) -> Response {
        let field = self.field(body);
        let response = ui.interact(field, self.id, Sense::click());
        if response.clicked() {
            response.request_focus();
        }
        let ParamKind::Choice { choices } = &self.d.kind else {
            return response;
        };
        if response.has_focus() {
            self.lock_arrows(ui);
            let steps = ui.input(|i| arrow_steps(&i.events));
            if steps != 0.0 {
                self.edit(self.d.clamp(self.value + steps));
            }
        }
        let c = &self.theme.colors;
        let painter = ui.painter();
        painter.rect_filled(field, self.theme.sizes.corner, c.field);
        painter.text(
            pos2(field.left() + 6.0, field.center().y),
            Align2::LEFT_CENTER,
            self.text(),
            self.theme.small_font(),
            c.text,
        );
        painter.text(
            pos2(field.right() - 6.0, field.center().y),
            Align2::RIGHT_CENTER,
            "▼",
            self.theme.small_font(),
            c.text_dim,
        );
        self.focus_ring(ui, &response, field);
        let open_by_key = response.has_focus() && ui.input(|i| i.key_pressed(Key::Space));
        let popup = egui::Popup::menu(&response)
            .open_memory(open_by_key.then_some(egui::SetOpenCommand::Toggle));
        popup.show(|ui| {
            for (index, choice) in choices.iter().enumerate() {
                let chosen = index as f64 == self.value;
                if ui.selectable_label(chosen, choice).clicked() {
                    self.edit(index as f64);
                    ui.close();
                }
            }
        });
        response
    }

    /// Typed entry over `field`: opened by Enter or a click on the value
    /// (holding the value's text, selected, so typing replaces it) or by
    /// typing a digit (holding what was typed), taken by Enter, cancelled
    /// by Escape or a click elsewhere. Returns whether it's open.
    fn typed_entry(&self, ui: &mut Ui, response: &Response, field: Rect) -> bool {
        let entry_id = self.id.with("entry");
        let field_id = entry_id.with("field");
        let Some(mut entry) = ui
            .data(|data| data.get_temp::<Entry>(entry_id))
            .or_else(|| self.open_entry(ui, response, field))
        else {
            return false;
        };
        let c = &self.theme.colors;
        ui.painter()
            .rect_filled(field, self.theme.sizes.corner, c.field);
        if entry.fresh {
            // The key that opened it has gone by; the field takes the
            // keyboard from the next frame, its text selected if it's the
            // value's, the cursor after it if it was typed.
            entry.fresh = false;
            ui.memory_mut(|memory| memory.request_focus(field_id));
            let mut state = egui::text_edit::TextEditState::default();
            let chars = entry.text.chars().count();
            let from = if entry.typed { chars } else { 0 };
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(from),
                    egui::text::CCursor::new(chars),
                )));
            state.store(ui.ctx(), field_id);
            ui.painter().text(
                field.center(),
                Align2::CENTER_CENTER,
                &entry.text,
                self.theme.small_font(),
                c.text,
            );
            ui.data_mut(|data| data.insert_temp(entry_id, entry));
            return true;
        }
        let edit = egui::TextEdit::singleline(&mut entry.text)
            .id(field_id)
            .font(self.theme.small_font())
            .horizontal_align(egui::Align::Center)
            .frame(egui::Frame::NONE)
            .desired_width(field.width());
        let output = ui.put(field, edit);
        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if output.lost_focus() || escape || !output.has_focus() {
            if enter
                && !escape
                && let Some(value) = self.d.parse(&entry.text)
            {
                self.edit(value);
            }
            ui.data_mut(|data| data.remove_temp::<Entry>(entry_id));
            response.request_focus();
            return false;
        }
        ui.data_mut(|data| data.insert_temp(entry_id, entry));
        true
    }

    /// Typed entry, if this frame opens it.
    fn open_entry(&self, ui: &Ui, response: &Response, field: Rect) -> Option<Entry> {
        let clicked_value = self.kind == ControlKind::Knob
            && response.clicked()
            && ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pos| field.contains(pos));
        if clicked_value {
            return Some(Entry::new(self.text(), false));
        }
        if !response.has_focus() {
            return None;
        }
        ui.input(|i| {
            i.events.iter().find_map(|event| match event {
                Event::Key {
                    key: Key::Enter,
                    pressed: true,
                    ..
                } => Some(Entry::new(self.text(), false)),
                Event::Text(text)
                    if text.starts_with(|c: char| c.is_ascii_digit() || "-.+".contains(c)) =>
                {
                    Some(Entry::new(text.clone(), true))
                }
                _ => None,
            })
        })
    }

    /// A drag, a double-click, the keyboard and a screen reader's actions
    /// on a knob or number box.
    fn value_input(&self, ui: &Ui, response: &Response) {
        let d = self.d;
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let drag_id = self.id.with("drag");
        if response.drag_started() {
            ui.data_mut(|data| {
                data.insert_temp(
                    drag_id,
                    Drag {
                        start: d.normalize(self.value),
                        moved: 0.0,
                    },
                );
                data.get_temp_mut_or_default::<Vec<ParamId>>(gestures_id())
                    .push(d.id);
            });
            let _ = self.cx.begin_edit(d.id);
        }
        if response.dragged() {
            let dy = response.drag_delta().y;
            let fine = ui.input(|i| i.modifiers.shift);
            let drag = ui.data_mut(|data| {
                let drag = data.get_temp_mut_or_default::<Drag>(drag_id);
                drag.moved -= f64::from(dy / DRAG_TRAVEL / if fine { FINE } else { 1.0 });
                *drag
            });
            let value = d.denormalize((drag.start + drag.moved).clamp(0.0, 1.0));
            if dy != 0.0 && value != self.value {
                let _ = self.cx.set_param(d.id, value);
            }
        }
        if response.drag_stopped() {
            ui.data_mut(|data| {
                data.remove::<Drag>(drag_id);
                data.get_temp_mut_or_default::<Vec<ParamId>>(gestures_id())
                    .retain(|id| *id != d.id);
            });
            let _ = self.cx.end_edit(d.id);
        }

        let mut change = None;
        if response.double_clicked() {
            change = Some(Change::Default);
        }
        let id = self.id;
        ui.input(|i| {
            let increments = i.num_accesskit_action_requests(id, Action::Increment) as f64;
            let decrements = i.num_accesskit_action_requests(id, Action::Decrement) as f64;
            if increments != decrements {
                change = Some(Change::Steps(increments - decrements));
            }
            for request in i.accesskit_action_requests(id, Action::SetValue) {
                match &request.data {
                    Some(ActionData::NumericValue(value)) => change = Some(Change::To(*value)),
                    Some(ActionData::Value(text)) => {
                        if let Some(value) = d.parse(text) {
                            change = Some(Change::To(value));
                        }
                    }
                    _ => {}
                }
            }
        });
        if response.has_focus() {
            self.lock_arrows(ui);
            ui.input(|i| {
                for event in &i.events {
                    let Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } = event
                    else {
                        continue;
                    };
                    let fine = if modifiers.shift {
                        1.0 / f64::from(FINE)
                    } else {
                        1.0
                    };
                    change = match key {
                        Key::ArrowUp | Key::ArrowRight => Some(Change::Steps(fine)),
                        Key::ArrowDown | Key::ArrowLeft => Some(Change::Steps(-fine)),
                        Key::PageUp => Some(Change::Steps(10.0)),
                        Key::PageDown => Some(Change::Steps(-10.0)),
                        Key::Home => Some(Change::To(d.min())),
                        Key::End => Some(Change::To(d.max())),
                        Key::Delete | Key::Backspace => Some(Change::Default),
                        _ => change,
                    };
                }
            });
        }
        match change {
            Some(Change::Default) => self.edit(d.default),
            Some(Change::To(value)) => self.edit(value),
            Some(Change::Steps(steps)) => self.edit(step(d, self.value, steps)),
            None => {}
        }
    }

    /// Keeps the arrow keys on the control while it has the keyboard,
    /// rather than moving the keyboard to the next control.
    fn lock_arrows(&self, ui: &Ui) {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                self.id,
                EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..EventFilter::default()
                },
            );
        });
    }

    /// Sets the value in one gesture, if it changes.
    fn edit(&self, value: f64) {
        if self.d.clamp(value) != self.value {
            let _ = self.cx.edit(self.d.id, value);
        }
    }

    /// The control's role, name and value, for egui and AccessKit.
    fn describe(&self, ui: &Ui, response: &Response) {
        let d = self.d;
        let text = self.text();
        let on = self.value >= 0.5;
        response.widget_info(|| match self.kind {
            ControlKind::Switch => WidgetInfo::selected(WidgetType::Checkbox, true, on, &d.name),
            ControlKind::Chooser => WidgetInfo::labeled(WidgetType::ComboBox, true, &d.name),
            ControlKind::Knob | ControlKind::NumberBox => {
                WidgetInfo::slider(true, self.value, &d.name)
            }
        });
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_role(self.kind.role());
            node.set_label(d.name.clone());
            node.set_value(text);
            if !d.description.is_empty() {
                node.set_description(d.description.clone());
            }
            match self.kind {
                ControlKind::Knob | ControlKind::NumberBox => {
                    node.set_numeric_value(self.value);
                    node.set_min_numeric_value(d.min());
                    node.set_max_numeric_value(d.max());
                    if d.is_stepped() {
                        node.set_numeric_value_step(1.0);
                    }
                    node.add_action(Action::Increment);
                    node.add_action(Action::Decrement);
                    node.add_action(Action::SetValue);
                }
                ControlKind::Switch => {
                    node.set_toggled(if on { Toggled::True } else { Toggled::False });
                    node.add_action(Action::Click);
                }
                ControlKind::Chooser => {
                    node.add_action(Action::Click);
                }
            }
        });
    }

    fn focus_ring(&self, ui: &Ui, response: &Response, rect: Rect) {
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                self.theme.sizes.corner,
                Stroke::new(1.5, self.theme.colors.focus),
                StrokeKind::Outside,
            );
        }
    }
}

/// The arrow keys pressed in `events`, up and right counting one step each
/// way, down and left one step back.
fn arrow_steps(events: &[Event]) -> f64 {
    events
        .iter()
        .map(|event| match event {
            Event::Key {
                key: Key::ArrowUp | Key::ArrowRight,
                pressed: true,
                ..
            } => 1.0,
            Event::Key {
                key: Key::ArrowDown | Key::ArrowLeft,
                pressed: true,
                ..
            } => -1.0,
            _ => 0.0,
        })
        .sum()
}

/// The value `steps` steps from `value`: whole steps of a stepped
/// parameter (at least one), or 1 % of the travel each.
#[must_use]
pub fn step(param: &ParamDescriptor, value: f64, steps: f64) -> f64 {
    if param.is_stepped() {
        let whole = if steps.abs() < 1.0 {
            steps.signum()
        } else {
            steps.round()
        };
        param.clamp(value + whole)
    } else {
        param.denormalize((param.normalize(value) + steps * 0.01).clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_parameter_has_its_control() {
        let knob = ParamDescriptor::gain(ParamId(1), "Gain", 6.0, 0.0);
        assert_eq!(ControlKind::for_param(&knob), ControlKind::Knob);
        let switch = ParamDescriptor::toggle(ParamId(2), "On", true);
        assert_eq!(ControlKind::for_param(&switch), ControlKind::Switch);
        let chooser = ParamDescriptor::choice(ParamId(3), "Mode", &["A", "B"], 0);
        assert_eq!(ControlKind::for_param(&chooser), ControlKind::Chooser);
        let number = ParamDescriptor::integer(
            ParamId(4),
            "Voices",
            (1, 16),
            8,
            lspk_params::ParamUnit::None,
        );
        assert_eq!(ControlKind::for_param(&number), ControlKind::NumberBox);
        assert_eq!(ControlKind::Knob.role(), Role::Slider);
        assert_eq!(ControlKind::NumberBox.role(), Role::SpinButton);
    }

    #[test]
    fn steps_are_whole_for_stepped_parameters_and_a_percent_otherwise() {
        let number = ParamDescriptor::integer(
            ParamId(4),
            "Voices",
            (1, 16),
            8,
            lspk_params::ParamUnit::None,
        );
        assert_eq!(step(&number, 8.0, 1.0), 9.0);
        assert_eq!(step(&number, 8.0, 0.1), 9.0);
        assert_eq!(step(&number, 8.0, -10.0), 1.0);
        let amount = ParamDescriptor::continuous(
            ParamId(5),
            "Amount",
            (0.0, 100.0),
            0.0,
            lspk_params::ParamUnit::Percent,
        );
        assert!((step(&amount, 50.0, 1.0) - 51.0).abs() < 1e-9);
        assert!((step(&amount, 50.0, -0.1) - 49.9).abs() < 1e-9);
        assert_eq!(step(&amount, 99.5, 10.0), 100.0);
    }
}
