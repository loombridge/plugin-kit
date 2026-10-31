//! The editor's interface running with no window: it draws only when
//! something changed, and draws the same picture a window would.

use std::sync::Arc;
use std::time::Duration;

use egui::{Pos2, Ui, vec2};
use lspk_params::{ParamDescriptor, ParamId};
use lspk_plugin::{EditorContext, ParamValues};
use lspk_ui::editor::{EditorUi, View};
use lspk_ui::offscreen::Offscreen;
use lspk_ui::{GenericView, Theme};

const GAIN: ParamId = ParamId(1);
const STEP: Duration = Duration::from_millis(33);

fn values() -> Arc<ParamValues> {
    Arc::new(
        ParamValues::new(vec![
            ParamDescriptor::gain(GAIN, "Gain", 12.0, 0.0),
            ParamDescriptor::toggle(ParamId(2), "On", true),
        ])
        .expect("valid parameters"),
    )
}

fn generic(values: &Arc<ParamValues>) -> Offscreen {
    Offscreen::new(
        Box::new(GenericView::new("Test")),
        EditorContext::detached(values.clone()),
        Theme::default(),
        vec2(240.0, 200.0),
        1.0,
        1.0,
    )
}

/// A view showing a moving value, as a meter does: it asks for a frame
/// every 33 ms.
struct Meter;

impl View for Meter {
    fn ui(&mut self, ui: &mut Ui, _editor: &mut EditorUi<'_>) {
        ui.label("meter");
        ui.ctx().request_repaint_after(STEP);
    }
}

#[test]
fn an_idle_editor_draws_nothing() {
    let mut editor = generic(&values());
    let drawn = editor.frames();
    assert!(drawn > 0, "it drew its first frames");
    for _ in 0..300 {
        assert!(!editor.advance(STEP));
    }
    assert_eq!(editor.frames(), drawn, "ten seconds with nothing moving");
}

#[test]
fn a_changed_value_draws_one_frame_and_input_draws_one() {
    let values = values();
    let mut editor = generic(&values);
    let drawn = editor.frames();
    values.set(GAIN, 0.5);
    assert!(editor.advance(STEP), "the host's automation moved a knob");
    assert!(!editor.advance(STEP), "and then it's idle again");
    editor.event(egui::Event::PointerMoved(Pos2::new(10.0, 10.0)));
    assert!(editor.advance(Duration::ZERO), "a mouse move is input");
    assert_eq!(editor.frames(), drawn + 2);
}

#[test]
fn a_value_that_doesnt_change_draws_nothing() {
    let values = values();
    let mut editor = generic(&values);
    let drawn = editor.frames();
    let now = values.get(GAIN).unwrap();
    values.set(GAIN, now);
    assert!(!editor.advance(STEP));
    assert_eq!(editor.frames(), drawn);
}

#[test]
fn a_view_that_shows_a_moving_value_draws_as_often_as_it_asks() {
    let mut editor = Offscreen::new(
        Box::new(Meter),
        EditorContext::detached(values()),
        Theme::default(),
        vec2(100.0, 50.0),
        1.0,
        1.0,
    );
    let drawn = editor.frames();
    for _ in 0..30 {
        editor.advance(STEP);
    }
    assert!(
        (28..=31).contains(&(editor.frames() - drawn)),
        "about one frame in each of 30 steps, not {}",
        editor.frames() - drawn
    );
}

#[test]
fn the_picture_is_the_window_in_pixels_and_has_the_controls_in_it() {
    let mut editor = Offscreen::new(
        Box::new(GenericView::new("Test")),
        EditorContext::detached(values()),
        Theme::default(),
        vec2(240.0, 200.0),
        1.0,
        2.0,
    );
    let image = editor.render();
    assert_eq!((image.width, image.height), (480, 400));
    assert_eq!(image.rgba.len(), 480 * 400 * 4);
    let background = Theme::default().colors.background;
    let corner = &image.rgba[..4];
    assert_eq!(
        corner,
        [background.r(), background.g(), background.b(), 255]
    );
    let painted = image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[..3] != corner[..3])
        .count();
    assert!(painted > 1_000, "the controls are drawn: {painted} pixels");
}
