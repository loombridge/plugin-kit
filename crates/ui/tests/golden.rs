//! Snapshots of the generic interface, drawn off screen with the kit's own
//! typeface so one reference holds on every platform, compared with the
//! pictures in `tests/golden/`. Remake them with `cargo xtask golden
//! --update`.

use std::sync::Arc;

use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::{Editor, EditorContext, ParamValues};
use lspk_test::snapshot::Suite;
use lspk_ui::theme::FontSource;
use lspk_ui::{EditorOptions, EguiEditor, GenericView, Theme, fonts};

const GAIN: ParamId = ParamId(1);
const ON: ParamId = ParamId(2);
const MODE: ParamId = ParamId(3);
const VOICES: ParamId = ParamId(4);
const AMOUNT: ParamId = ParamId(5);

fn values() -> Arc<ParamValues> {
    Arc::new(
        ParamValues::new(vec![
            ParamDescriptor::gain(GAIN, "Gain", 12.0, 0.0),
            ParamDescriptor::toggle(ON, "On", true),
            ParamDescriptor::choice(MODE, "Mode", &["Clean", "Warm", "Hot"], 0),
            ParamDescriptor::integer(VOICES, "Voices", (1, 16), 8, ParamUnit::None),
            ParamDescriptor::continuous(AMOUNT, "Amount", (0.0, 100.0), 50.0, ParamUnit::Percent),
        ])
        .expect("valid parameters"),
    )
}

fn editor(theme: Theme) -> EguiEditor {
    let mut options = EditorOptions::new(egui::vec2(300.0, 220.0));
    options.theme = theme;
    options.theme.fonts.ui = FontSource::Bytes(fonts::block_font());
    EguiEditor::new(options, || GenericView::new("Snapshot"))
}

#[test]
fn the_generic_interface_matches_its_references() {
    let values = values();
    let cx = EditorContext::detached(values.clone());
    let mut suite = Suite::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"));
    let capture = |theme: Theme, cx: &EditorContext, scale: f32| {
        editor(theme).capture(cx, scale).expect("the editor draws")
    };
    suite.add("generic-defaults", capture(Theme::dark(), &cx, 1.0));
    suite.add("generic-light", capture(Theme::light(), &cx, 1.0));
    suite.add("generic-sharp", capture(Theme::dark(), &cx, 2.0));
    values.set(GAIN, 0.25);
    values.set(ON, 0.0);
    values.set(MODE, 2.0);
    values.set(AMOUNT, 80.0);
    suite.add("generic-edited", capture(Theme::dark(), &cx, 1.0));
    suite.assert();
}
