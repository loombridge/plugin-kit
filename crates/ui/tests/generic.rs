//! The generic interface, headless: every kind of control drawn from its
//! descriptor, driven by the mouse, the keyboard and a screen reader, each
//! change reaching the host as a gesture, and the accessibility tree
//! carrying every control's role, name and value.

use egui::accesskit::{self, Action, ActionData, ActionRequest, Role, Toggled, TreeId};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};
use lspk_params::{ParamDescriptor, ParamId, ParamUnit, db_to_gain};
use lspk_plugin::editor::{EditorChanges, EditorHost};
use lspk_plugin::host::{Edit, HostBackend};
use lspk_plugin::{EditorContext, EditorSize, Host, ParamValues};
use lspk_ui::controls::control_id;
use lspk_ui::editor::{draw, prepare};
use lspk_ui::{GenericView, Theme};
use rtrb::Consumer;
use std::sync::Arc;

const GAIN: ParamId = ParamId(1);
const ON: ParamId = ParamId(2);
const MODE: ParamId = ParamId(3);
const VOICES: ParamId = ParamId(4);
const AMOUNT: ParamId = ParamId(5);

fn params() -> Vec<ParamDescriptor> {
    vec![
        ParamDescriptor::gain(GAIN, "Gain", 12.0, 0.0).describe("How loud."),
        ParamDescriptor::toggle(ON, "On", true),
        ParamDescriptor::choice(MODE, "Mode", &["Clean", "Warm", "Hot"], 0),
        ParamDescriptor::integer(VOICES, "Voices", (1, 16), 8, ParamUnit::None),
        ParamDescriptor::continuous(AMOUNT, "Amount", (0.0, 100.0), 50.0, ParamUnit::Percent),
    ]
}

struct NoHost;

impl EditorHost for NoHost {
    fn request_callback(&self) {}
    fn request_resize(&self, _size: EditorSize) -> bool {
        true
    }
}

impl HostBackend for NoHost {
    fn request_flush(&self) {}
    fn mark_dirty(&self) {}
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
    fn values_changed(&self) {}
}

/// An editor's window without a window: egui run by hand, its changes
/// passed to a plug-in's host link, as the CLAP export does.
struct Harness {
    ctx: egui::Context,
    cx: EditorContext,
    changes: EditorChanges,
    host: Host,
    edits: Consumer<Edit>,
    view: GenericView,
    theme: Theme,
    zoom: f32,
    time: f64,
    last: egui::FullOutput,
}

impl Harness {
    fn new() -> Self {
        let values = Arc::new(ParamValues::new(params()).expect("valid parameters"));
        let (cx, changes) = EditorContext::new(values.clone(), Box::new(NoHost));
        let host = Host::new(Box::new(NoHost));
        let edits = host.connect(values);
        let theme = Theme::default();
        let ctx = egui::Context::default();
        prepare(&ctx, &theme);
        let mut harness = Self {
            ctx,
            cx,
            changes,
            host,
            edits,
            view: GenericView::new("Test"),
            theme,
            zoom: 1.0,
            time: 0.0,
            last: egui::FullOutput::default(),
        };
        // Two frames, so every control has a place.
        harness.frame(vec![]);
        harness.frame(vec![]);
        harness
    }

    fn frame(&mut self, events: Vec<Event>) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 400.0))),
            time: Some(self.time),
            events,
            ..RawInput::default()
        };
        self.time += 0.5;
        let ctx = self.ctx.clone();
        let mut zoom = self.zoom;
        self.last = ctx.run_ui(input, |ui| {
            zoom = draw(ui, &mut self.view, &self.cx, &self.theme, zoom);
        });
        // No renderer takes the textures; say so, as egui asks.
        self.last.textures_delta.clear();
        self.zoom = zoom;
    }

    /// What the host has heard since last asked.
    fn edits(&mut self) -> Vec<Edit> {
        self.changes.apply(&self.host);
        std::iter::from_fn(|| self.edits.pop().ok()).collect()
    }

    fn rect(&self, id: ParamId) -> Rect {
        self.ctx
            .read_response(control_id(id))
            .expect("the control was drawn")
            .rect
    }

    fn value(&self, id: ParamId) -> f64 {
        self.cx.values().get(id).expect("a parameter")
    }

    fn click(&mut self, at: Pos2) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![press(at, true)]);
        self.frame(vec![press(at, false)]);
    }

    fn key(&mut self, key: Key) {
        self.frame(vec![key_event(key, Modifiers::NONE)]);
    }

    fn focus(&mut self, id: ParamId) {
        self.ctx
            .memory_mut(|memory| memory.request_focus(control_id(id)));
        self.frame(vec![]);
    }

    fn node(&self, id: ParamId) -> accesskit::Node {
        let update = self
            .last
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("AccessKit is on");
        let target = control_id(id).accesskit_id();
        update
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == target)
            .map(|(_, node)| node.clone())
            .expect("the control is in the accessibility tree")
    }
}

fn press(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn key_event(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// The values a list of edits sends, and whether it's one whole gesture
/// on `id`.
fn gesture(edits: &[Edit], id: ParamId) -> Vec<f64> {
    assert!(
        matches!(edits.first(), Some(Edit::Begin(first)) if *first == id),
        "starts a gesture on {id:?}: {edits:?}"
    );
    assert!(
        matches!(edits.last(), Some(Edit::End(last)) if *last == id),
        "ends a gesture on {id:?}: {edits:?}"
    );
    edits[1..edits.len() - 1]
        .iter()
        .map(|edit| match edit {
            Edit::Value {
                id: edited,
                value,
                echo: true,
            } if *edited == id => *value,
            other => panic!("only values inside the gesture: {other:?}"),
        })
        .collect()
}

#[test]
fn a_knob_drag_is_one_gesture() {
    let mut h = Harness::new();
    let knob = h.rect(GAIN).center();
    let start = lspk_params::ParamDescriptor::gain(GAIN, "Gain", 12.0, 0.0).normalize(1.0);
    h.frame(vec![Event::PointerMoved(knob)]);
    h.frame(vec![press(knob, true)]);
    for dy in [10.0, 30.0, 50.0] {
        h.frame(vec![Event::PointerMoved(knob - vec2(0.0, dy))]);
    }
    h.frame(vec![press(knob - vec2(0.0, 50.0), false)]);
    let edits = h.edits();
    let values = gesture(&edits, GAIN);
    assert!(values.len() >= 2, "{edits:?}");
    let expected = params()[0].denormalize(start + 50.0 / 200.0);
    assert!((h.value(GAIN) - expected).abs() < 1e-9);
    assert_eq!(values.last().copied(), Some(h.value(GAIN)));
    assert!(values.windows(2).all(|pair| pair[1] > pair[0]));
}

#[test]
fn a_double_click_sets_the_default() {
    let mut h = Harness::new();
    h.cx.edit(GAIN, 0.5).expect("a parameter");
    h.edits();
    let knob = h.rect(GAIN).center();
    h.frame(vec![Event::PointerMoved(knob)]);
    h.time = 10.0;
    h.frame(vec![press(knob, true), press(knob, false)]);
    h.time = 10.1;
    h.frame(vec![press(knob, true), press(knob, false)]);
    assert_eq!(h.value(GAIN), 1.0);
    assert_eq!(gesture(&h.edits(), GAIN), [1.0]);
}

#[test]
fn the_keyboard_steps_and_resets_a_knob() {
    let mut h = Harness::new();
    h.focus(AMOUNT);
    h.key(Key::ArrowUp);
    assert!((h.value(AMOUNT) - 51.0).abs() < 1e-9);
    assert_eq!(gesture(&h.edits(), AMOUNT).len(), 1);
    h.frame(vec![key_event(Key::ArrowDown, Modifiers::SHIFT)]);
    assert!((h.value(AMOUNT) - 50.9).abs() < 1e-9);
    h.key(Key::PageUp);
    assert!((h.value(AMOUNT) - 60.9).abs() < 1e-9);
    h.key(Key::End);
    assert_eq!(h.value(AMOUNT), 100.0);
    h.key(Key::Home);
    assert_eq!(h.value(AMOUNT), 0.0);
    h.key(Key::Delete);
    assert_eq!(h.value(AMOUNT), 50.0);
    // Every change was a whole gesture of its own.
    let edits = h.edits();
    assert_eq!(edits.len(), 5 * 3, "{edits:?}");
    for change in edits.chunks(3) {
        assert_eq!(gesture(change, AMOUNT).len(), 1);
    }
}

#[test]
fn a_typed_value_is_taken_by_enter_and_dropped_by_escape() {
    let mut h = Harness::new();
    h.focus(GAIN);
    h.key(Key::Enter);
    h.frame(vec![]);
    // Typed entry holds the value's text, selected, so typing replaces it.
    h.frame(vec![Event::Text("-6".into())]);
    h.key(Key::Enter);
    assert!((h.value(GAIN) - db_to_gain(-6.0)).abs() < 1e-9);
    assert_eq!(gesture(&h.edits(), GAIN).len(), 1);
    // The keyboard is back on the knob.
    h.frame(vec![]);
    assert!(h.ctx.memory(|memory| memory.has_focus(control_id(GAIN))));

    h.key(Key::Enter);
    h.frame(vec![]);
    h.frame(vec![Event::Text("3".into())]);
    h.key(Key::Escape);
    assert!((h.value(GAIN) - db_to_gain(-6.0)).abs() < 1e-9);
    assert!(h.edits().is_empty());

    // Typing a digit opens it with the digit; the rest follows.
    h.frame(vec![]);
    h.frame(vec![Event::Text("-".into())]);
    h.frame(vec![]);
    h.frame(vec![Event::Text("12".into())]);
    h.key(Key::Enter);
    assert!((h.value(GAIN) - db_to_gain(-12.0)).abs() < 1e-9);
    assert_eq!(gesture(&h.edits(), GAIN).len(), 1);
}

#[test]
fn a_switch_flips_by_click_and_by_key() {
    let mut h = Harness::new();
    let switch = h.rect(ON).center();
    h.click(switch);
    assert_eq!(h.value(ON), 0.0);
    assert_eq!(gesture(&h.edits(), ON), [0.0]);
    h.key(Key::Space);
    assert_eq!(h.value(ON), 1.0);
    assert_eq!(gesture(&h.edits(), ON), [1.0]);
}

#[test]
fn a_chooser_and_a_number_box_step_by_key() {
    let mut h = Harness::new();
    h.focus(MODE);
    h.key(Key::ArrowDown);
    assert_eq!(h.value(MODE), 0.0, "already at the first choice");
    h.key(Key::ArrowUp);
    h.key(Key::ArrowRight);
    assert_eq!(h.value(MODE), 2.0);
    assert_eq!(h.edits().len(), 6);
    h.focus(VOICES);
    h.key(Key::ArrowUp);
    h.key(Key::PageDown);
    assert_eq!(h.value(VOICES), 1.0);
    let edits = h.edits();
    assert_eq!(gesture(&edits[..3], VOICES), [9.0]);
    assert_eq!(gesture(&edits[3..], VOICES), [1.0]);
}

#[test]
fn tab_moves_between_every_control() {
    let mut h = Harness::new();
    let mut visited = Vec::new();
    // The parameters' controls, and the zoom buttons.
    for _ in 0..params().len() + 2 {
        h.key(Key::Tab);
        let focused = h.ctx.memory(|memory| memory.focused());
        visited.push(focused.expect("a control has the keyboard"));
    }
    for param in params() {
        assert!(visited.contains(&control_id(param.id)), "{}", param.name);
    }
    visited.sort_by_key(|id| id.value());
    visited.dedup();
    assert_eq!(visited.len(), params().len() + 2);
}

#[test]
fn every_control_has_its_role_name_and_value() {
    let h = Harness::new();
    let gain = h.node(GAIN);
    assert_eq!(gain.role(), Role::Slider);
    assert_eq!(gain.label(), Some("Gain"));
    assert_eq!(gain.value(), Some("0.00 dB"));
    assert_eq!(gain.numeric_value(), Some(1.0));
    assert_eq!(gain.description(), Some("How loud."));
    assert!(gain.supports_action(Action::Increment));
    assert!(gain.supports_action(Action::SetValue));
    let on = h.node(ON);
    assert_eq!(on.role(), Role::Switch);
    assert_eq!(on.toggled(), Some(Toggled::True));
    assert_eq!(on.value(), Some("On"));
    let mode = h.node(MODE);
    assert_eq!(mode.role(), Role::ComboBox);
    assert_eq!(mode.label(), Some("Mode"));
    assert_eq!(mode.value(), Some("Clean"));
    let voices = h.node(VOICES);
    assert_eq!(voices.role(), Role::SpinButton);
    assert_eq!(voices.value(), Some("8"));
    assert_eq!(voices.min_numeric_value(), Some(1.0));
    assert_eq!(voices.max_numeric_value(), Some(16.0));
    assert_eq!(voices.numeric_value_step(), Some(1.0));
}

#[test]
fn a_screen_reader_steps_and_sets_values() {
    let mut h = Harness::new();
    let request = |id: ParamId, action, data| {
        Event::AccessKitActionRequest(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: control_id(id).accesskit_id(),
            data,
        })
    };
    h.frame(vec![request(AMOUNT, Action::Increment, None)]);
    assert!((h.value(AMOUNT) - 51.0).abs() < 1e-9);
    h.frame(vec![request(
        AMOUNT,
        Action::SetValue,
        Some(ActionData::NumericValue(20.0)),
    )]);
    assert_eq!(h.value(AMOUNT), 20.0);
    h.frame(vec![request(
        GAIN,
        Action::SetValue,
        Some(ActionData::Value("-inf dB".into())),
    )]);
    assert_eq!(h.value(GAIN), 0.0);
    let edits = h.edits();
    assert_eq!(gesture(&edits[..3], AMOUNT).len(), 1);
    assert_eq!(gesture(&edits[3..6], AMOUNT), [20.0]);
    assert_eq!(gesture(&edits[6..], GAIN), [0.0]);
    // The tree follows.
    h.frame(vec![]);
    assert_eq!(h.node(GAIN).value(), Some("-inf dB"));
}

#[test]
fn the_host_s_changes_show_without_an_edit() {
    let mut h = Harness::new();
    h.cx.values().set(MODE, 1.0);
    h.frame(vec![]);
    assert_eq!(h.node(MODE).value(), Some("Warm"));
    assert!(h.edits().is_empty());
}

#[test]
fn the_zoom_keys_and_buttons_stay_between_half_and_double() {
    let mut h = Harness::new();
    h.frame(vec![key_event(Key::Equals, Modifiers::COMMAND)]);
    assert_eq!(h.zoom, 1.1);
    for _ in 0..20 {
        h.frame(vec![key_event(Key::Minus, Modifiers::COMMAND)]);
    }
    assert_eq!(h.zoom, 0.5);
    h.frame(vec![key_event(Key::Num0, Modifiers::COMMAND)]);
    assert_eq!(h.zoom, 1.0);
    // The bar's zoom-in button, at its right end.
    let screen = h.ctx.content_rect();
    let plus = pos2(screen.right() - 22.0, 16.0);
    h.click(plus);
    assert_eq!(h.zoom, 1.1);
    assert!(h.edits().is_empty());
}
