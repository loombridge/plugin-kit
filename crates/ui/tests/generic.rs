//! The generic interface, headless: every kind of control drawn from its
//! descriptor, driven by the mouse, the keyboard and a screen reader, each
//! change reaching the host as a gesture, and the accessibility tree
//! carrying every control's role, name and value. Every change a control
//! makes is a command the plug-in runs: a frame that changes a value before
//! the commands run fails the test.

use egui::accesskit::{self, Action, ActionData, ActionRequest, Role, Toggled, TreeId};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, pos2, vec2};
use lspk_commands::{CommandSpec, Context, NoArgs, Permission};
use lspk_params::{ParamDescriptor, ParamId, ParamUnit, db_to_gain};
use lspk_plugin::editor::{EditorChanges, EditorHost};
use lspk_plugin::host::{Edit, HostBackend};
use lspk_plugin::{
    AudioLayout, Buffers, EditorContext, EditorSize, Events, Host, ParamValues, Plugin,
    PluginError, PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor,
    RegisterError, Registry,
};
use lspk_plugin_agent::WithAgent;
use lspk_ui::controls::control_id;
use lspk_ui::editor::{EditorUi, View, draw, prepare};
use lspk_ui::{Action as Button, GenericView, Theme};
use rtrb::Consumer;
use serde_json::Value;
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

/// A plug-in with the test's parameters, exported with the agent
/// interface as the kit's plug-ins are, so it has the `params.set` command
/// an editor's value changes run.
struct Probe {
    host: Host,
    /// The undo labels the plug-in's interface ran commands under.
    labels: Vec<String>,
}

/// The probe's own command: halves the gain.
const HALVE: &str = "probe.halve";

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

impl Plugin for Probe {
    type Processor = Silent;

    const INFO: PluginInfo = PluginInfo::new("com.loombridge.test.generic", "Generic Probe")
        .audio(AudioLayout::effect(2));

    fn new(host: Host) -> Self {
        Self {
            host,
            labels: Vec::new(),
        }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        params()
    }

    fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
        registry.register(
            CommandSpec::new(HALVE, "Halve Gain")
                .description("Halves the gain.")
                .permission(Permission::Edit)
                .context(Context::Global)
                .args::<NoArgs>()
                .result::<()>(),
        )
    }

    fn run_command(&mut self, id: &str, _args: &Value) -> Result<Value, PluginError> {
        assert_eq!(id, HALVE);
        let gain = self.host.param(GAIN).expect("a parameter");
        self.host
            .edit(GAIN, gain / 2.0)
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

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        _values: &ParamValues,
    ) -> Result<Silent, PluginError> {
        Ok(Silent)
    }
}

/// An editor's window without a window: egui run by hand, its changes
/// run on the plug-in and passed to its host link after each frame, as the
/// CLAP export does when the host calls back.
struct Harness<V = GenericView> {
    ctx: egui::Context,
    cx: EditorContext,
    changes: EditorChanges,
    plugin: WithAgent<Probe>,
    host: Host,
    edits: Consumer<Edit>,
    heard: Vec<Edit>,
    view: V,
    theme: Theme,
    zoom: f32,
    time: f64,
    last: egui::FullOutput,
}

impl Harness {
    fn new() -> Self {
        Self::with_view(GenericView::new("Test"))
    }
}

impl<V: View> Harness<V> {
    fn with_view(view: V) -> Self {
        let values = Arc::new(ParamValues::new(params()).expect("valid parameters"));
        let (cx, changes) = EditorContext::new(values.clone(), Box::new(NoHost));
        let host = Host::new(Box::new(NoHost));
        let edits = host.connect(values);
        let plugin = WithAgent::<Probe>::new(host.clone());
        let theme = Theme::default();
        let ctx = egui::Context::default();
        prepare(&ctx, &theme);
        let mut harness = Self {
            ctx,
            cx,
            changes,
            plugin,
            host,
            edits,
            heard: Vec::new(),
            view,
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
        let before = self.stored();
        self.last = ctx.run_ui(input, |ui| {
            zoom = draw(ui, &mut self.view, &self.cx, &self.theme, zoom);
        });
        // No renderer takes the textures; say so, as egui asks.
        self.last.textures_delta.clear();
        self.zoom = zoom;
        // The interface only asks: a value that moved while it drew moved
        // without a command.
        assert_eq!(
            before,
            self.stored(),
            "a control changed a value outside a command"
        );
        // The host calls back, and the main thread runs what was asked.
        self.changes.apply(&self.host, &mut self.plugin);
        assert_eq!(self.cx.take_command_error(), None);
        self.heard
            .extend(std::iter::from_fn(|| self.edits.pop().ok()));
    }

    /// Every parameter's stored value now.
    fn stored(&self) -> Vec<f64> {
        let values = self.cx.values();
        (0..values.len()).map(|i| values.value_at(i)).collect()
    }

    /// What the host has heard since last asked.
    fn edits(&mut self) -> Vec<Edit> {
        std::mem::take(&mut self.heard)
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
    // The main thread runs it after the next frame.
    h.frame(vec![]);
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

/// A control that writes a value itself, the way every control did before
/// commands.
struct Bypass;

impl View for Bypass {
    fn ui(&mut self, _ui: &mut egui::Ui, editor: &mut EditorUi<'_>) {
        editor.cx.values().set(GAIN, 0.5);
    }
}

#[test]
#[should_panic(expected = "a control changed a value outside a command")]
fn a_control_that_writes_a_value_itself_fails_the_harness() {
    Harness::with_view(Bypass);
}

#[test]
fn an_edit_is_a_command_the_plug_in_runs_under_a_label() {
    let mut h = Harness::new();
    h.cx.edit(GAIN, 0.5).expect("a parameter");
    // Asked, not done: the value waits for the main thread.
    assert_eq!(h.value(GAIN), 1.0);
    h.frame(vec![]);
    assert_eq!(h.value(GAIN), 0.5);
    assert_eq!(gesture(&h.edits(), GAIN), [0.5]);
    // A command the plug-in doesn't have is refused, and says so.
    h.cx.run_command("params.nothing", serde_json::json!({}), "Nothing");
    h.changes.apply(&h.host, &mut h.plugin);
    assert!(
        h.cx.take_command_error()
            .is_some_and(|why| why.contains("params.nothing"))
    );
}

#[test]
fn a_button_in_the_row_of_actions_runs_its_command_under_its_label() {
    let view = GenericView::new("Test").with_actions(vec![Button::new(HALVE, "Halve Gain")]);
    let mut h = Harness::with_view(view);
    // The row sits under the bar, the first button at its left end.
    let button = pos2(h.theme.sizes.spacing + 30.0, 48.0);
    h.click(button);
    assert_eq!(h.value(GAIN), 0.5);
    assert_eq!(gesture(&h.edits(), GAIN), [0.5]);
    assert_eq!(h.plugin.plugin().labels, ["Halve Gain"]);
}
