//! What an open editor costs the host: the interface with nothing moving,
//! which should cost nothing, and with one control animated, as a modulated
//! knob is. Each routine is one second of the editor's time, in the 33 ms
//! steps a window's watcher looks in; the idle one fails if it draws a
//! frame. `cargo xtask bench` times them, and CI's instruction-count gate
//! covers them.

use std::hint::black_box;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use egui::vec2;
use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::{EditorContext, ParamValues};
use lspk_test::bench::Suite;
use lspk_ui::offscreen::Offscreen;
use lspk_ui::theme::FontSource;
use lspk_ui::{GenericView, Theme, fonts};

/// How often a window looks, and so the steps of a second.
const STEP: Duration = Duration::from_millis(33);
const STEPS: usize = 30;
const GAIN: ParamId = ParamId(1);

/// The generic interface of eight parameters, drawn with the kit's own
/// typeface so the count doesn't depend on the platform's.
fn editor() -> (Offscreen, Arc<ParamValues>) {
    let mut params = vec![ParamDescriptor::gain(GAIN, "Gain", 12.0, 0.0)];
    for id in 2..=8 {
        params.push(ParamDescriptor::continuous(
            ParamId(id),
            &format!("Amount {id}"),
            (0.0, 100.0),
            50.0,
            ParamUnit::Percent,
        ));
    }
    let values = Arc::new(ParamValues::new(params).expect("valid parameters"));
    let mut theme = Theme::default();
    theme.fonts.ui = FontSource::Bytes(fonts::block_font());
    let offscreen = Offscreen::new(
        Box::new(GenericView::new("Benchmark")),
        EditorContext::detached(values.clone()),
        theme,
        vec2(560.0, 240.0),
        1.0,
        1.0,
    );
    (offscreen, values)
}

fn main() -> ExitCode {
    let mut suite = Suite::new("lspk-ui");

    // Nothing moves: no frame, and what looking costs.
    suite.micro("idle-second", STEPS, || {
        let (mut editor, _values) = editor();
        let drawn = editor.frames();
        for _ in 0..STEPS {
            editor.advance(STEP);
        }
        assert_eq!(editor.frames(), drawn, "an idle editor drew a frame");
        move || {
            for _ in 0..STEPS {
                black_box(editor.advance(STEP));
            }
        }
    });

    // One value changes at every step, as modulation moves a knob: every
    // step draws the interface.
    suite.micro("animated-second", STEPS, || {
        let (mut editor, values) = editor();
        let mut tick = 0u32;
        move || {
            for _ in 0..STEPS {
                tick += 1;
                values.set(GAIN, f64::from(tick % 2) * 0.5);
                black_box(editor.advance(STEP));
            }
        }
    });

    suite.main()
}
