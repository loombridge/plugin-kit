//! The generic interface: any plug-in's parameters, each drawn with its
//! control from its descriptor, in a grid that reflows as the window
//! resizes, under a bar with the plug-in's name and the zoom, and a row of
//! buttons for the commands a plug-in names ([`Action`]).

use egui::{Align2, Rect, ScrollArea, Sense, Ui, Vec2, pos2, vec2};

use crate::controls::param_control;
use crate::editor::{EditorUi, View};
use crate::theme::{Theme, step_zoom};

/// The most columns the generic interface opens with.
const MAX_COLUMNS: usize = 6;

/// A button in the generic interface's row of actions: it runs the
/// registered command `command` with no arguments, naming the undo step
/// `label`, as an agent could.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// The command's ID.
    pub command: String,
    /// What the button says, and the undo step's name: the command's label.
    pub label: String,
}

impl Action {
    /// A button labelled `label` that runs `command`.
    #[must_use]
    pub fn new(command: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            label: label.into(),
        }
    }
}

/// The generic interface, for any plug-in, titled with its name.
#[derive(Clone, Debug)]
pub struct GenericView {
    title: String,
    actions: Vec<Action>,
}

impl GenericView {
    /// The generic interface, with `title` in its bar.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            actions: Vec::new(),
        }
    }

    /// Adds a row of buttons under the bar, one for each of `actions`.
    #[must_use]
    pub fn with_actions(mut self, actions: Vec<Action>) -> Self {
        self.actions = actions;
        self
    }
}

impl View for GenericView {
    fn ui(&mut self, ui: &mut Ui, editor: &mut EditorUi<'_>) {
        let bar = top_bar_height(editor.theme);
        let full = ui.max_rect();
        let bar_rect = Rect::from_min_size(full.min, vec2(full.width(), bar));
        top_bar(ui, editor, &self.title, bar_rect);
        let mut top = bar_rect.bottom();
        if !self.actions.is_empty() {
            let row = Rect::from_min_size(pos2(full.left(), top), vec2(full.width(), bar));
            action_row(ui, editor, &self.actions, row);
            top = row.bottom();
        }
        let body = Rect::from_min_max(pos2(full.left(), top), full.max);
        let mut grid = ui.new_child(egui::UiBuilder::new().max_rect(body));
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(&mut grid, |ui| param_grid(ui, editor));
    }
}

/// The height of the bar above the controls.
#[must_use]
pub fn top_bar_height(theme: &Theme) -> f32 {
    theme.sizes.field_height + theme.sizes.spacing
}

/// The size, in points at 100 % zoom, the generic interface opens at for
/// `count` parameters: up to [`MAX_COLUMNS`] cells across, as many rows
/// as they need, and a row of buttons more when `actions` is set.
#[must_use]
pub fn generic_size(count: usize, actions: bool, theme: &Theme) -> Vec2 {
    let s = &theme.sizes;
    let columns = count.clamp(1, MAX_COLUMNS);
    let rows = count.max(1).div_ceil(columns);
    let bars = if actions { 2.0 } else { 1.0 };
    vec2(
        columns as f32 * s.cell_width + (columns + 1) as f32 * s.spacing,
        bars * top_bar_height(theme) + rows as f32 * s.cell_height + (rows + 1) as f32 * s.spacing,
    )
}

/// The smallest the generic interface can be: one cell and the bar.
#[must_use]
pub fn generic_min_size(theme: &Theme) -> Vec2 {
    let mut size = generic_size(1, false, theme);
    // Room for the bar's name and zoom.
    size.x = size.x.max(180.0);
    size
}

/// The bar: the plug-in's name, and the zoom with its buttons.
fn top_bar(ui: &mut Ui, editor: &mut EditorUi<'_>, title: &str, rect: Rect) {
    let theme = editor.theme;
    let s = &theme.sizes;
    let painter = ui.painter();
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, theme.colors.separator),
    );
    painter.text(
        pos2(rect.left() + s.spacing, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        theme.font(),
        theme.colors.text,
    );
    let button = vec2(s.field_height, s.field_height);
    let right = rect.right() - s.spacing;
    let plus = Rect::from_center_size(pos2(right - button.x / 2.0, rect.center().y), button);
    let readout = Rect::from_center_size(
        plus.center() - vec2(button.x / 2.0 + 24.0, 0.0),
        vec2(44.0, s.field_height),
    );
    let minus = Rect::from_center_size(readout.center() - vec2(22.0 + button.x / 2.0, 0.0), button);
    let zoom = editor.zoom();
    if bar_button(ui, theme, minus, "−", "Zoom out") {
        editor.set_zoom(step_zoom(zoom, -1));
    }
    let percent = format!("{:.0} %", zoom * 100.0);
    let response = ui.interact(readout, ui.id().with("zoom-readout"), Sense::hover());
    ui.painter().text(
        readout.center(),
        Align2::CENTER_CENTER,
        &percent,
        theme.small_font(),
        theme.colors.text_dim,
    );
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Label);
        node.set_label("Zoom");
        node.set_value(percent.clone());
    });
    if bar_button(ui, theme, plus, "+", "Zoom in") {
        editor.set_zoom(step_zoom(zoom, 1));
    }
}

/// A button showing `glyph`, named `name` for screen readers; returns
/// whether it was pressed.
fn bar_button(ui: &mut Ui, theme: &Theme, rect: Rect, glyph: &str, name: &str) -> bool {
    let response = ui
        .put(
            rect,
            egui::Button::new(egui::RichText::new(glyph).font(theme.font()))
                .corner_radius(theme.sizes.corner)
                .fill(theme.colors.control),
        )
        .on_hover_text(name);
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label(name);
    });
    response.clicked()
}

/// The row of buttons: each runs its command on the main thread, through
/// the plug-in's registry.
fn action_row(ui: &mut Ui, editor: &mut EditorUi<'_>, actions: &[Action], rect: Rect) {
    let theme = editor.theme;
    let s = &theme.sizes;
    let mut left = rect.left() + s.spacing;
    for action in actions {
        let text =
            ui.painter()
                .layout_no_wrap(action.label.clone(), theme.font(), theme.colors.text);
        let size = vec2(text.size().x + s.spacing * 2.0, s.field_height);
        let button = Rect::from_min_size(pos2(left, rect.center().y - size.y / 2.0), size);
        if bar_button(ui, theme, button, &action.label, &action.label) {
            editor.cx.run_command(
                &action.command,
                serde_json::json!({}),
                action.label.as_str(),
            );
        }
        left = button.right() + s.spacing / 2.0;
    }
}

/// Every parameter's control, in a grid of as many columns as fit the
/// width; a slot's reserved parameters its current module doesn't use are
/// left out.
pub fn param_grid(ui: &mut Ui, editor: &mut EditorUi<'_>) {
    let theme = editor.theme;
    let s = &theme.sizes;
    let values = editor.cx.values();
    let shown: Vec<usize> = values
        .descriptors()
        .iter()
        .enumerate()
        .filter(|(_, d)| !values.view(d.id).is_some_and(|v| v.hidden))
        .map(|(index, _)| index)
        .collect();
    let count = shown.len();
    let width = ui.available_width();
    let columns = (((width - s.spacing) / (s.cell_width + s.spacing)).floor() as usize).max(1);
    let rows = count.div_ceil(columns);
    let height = rows as f32 * s.cell_height + (rows + 1) as f32 * s.spacing;
    let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    // Centre the grid in the width.
    let used = columns.min(count.max(1)) as f32 * (s.cell_width + s.spacing) + s.spacing;
    let left = area.left() + ((width - used) / 2.0).max(0.0);
    for (index, &param) in shown.iter().enumerate() {
        let (row, column) = (index / columns, index % columns);
        let min = pos2(
            left + s.spacing + column as f32 * (s.cell_width + s.spacing),
            area.top() + s.spacing + row as f32 * (s.cell_height + s.spacing),
        );
        let cell = Rect::from_min_size(min, vec2(s.cell_width, s.cell_height));
        param_control(ui, editor.cx, theme, param, cell);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generic_interface_grows_by_whole_cells() {
        let theme = Theme::dark();
        let one = generic_size(1, false, &theme);
        let six = generic_size(6, false, &theme);
        let seven = generic_size(7, false, &theme);
        assert_eq!(one.y, six.y);
        assert!(six.x > one.x);
        assert_eq!(seven.x, six.x);
        assert!(seven.y > six.y);
        assert_eq!(generic_size(0, false, &theme), one);
        assert!(generic_size(1, true, &theme).y > one.y);
        assert!(generic_min_size(&theme).x >= one.x);
    }
}
