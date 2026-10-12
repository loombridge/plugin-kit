//! Golden renders of every shaper mode at 0, 50 and 100 % (engine-core
//! R21 to R26): one note of a saw, slot 1 in the mode, each amount a
//! channel of the mode's reference. `cargo xtask golden --update` remakes
//! them.

use std::f64::consts::TAU;

use lspk_test::golden::Suite;
use lspk_test::render::Output;
use lspk_wavetable::osc::{Inputs, Layout, Onset, Oscillator};
use lspk_wavetable::shaper::{Mode, ShapedOscillator, ShaperInputs};
use lspk_wavetable::{Cache, FRAME_SIZE, Wavetable};

/// Frames per render.
const FRAMES: usize = 1_024;

/// A saw of 1,024 harmonics, in sine phase.
fn saw() -> Vec<f32> {
    (0..FRAME_SIZE)
        .map(|n| {
            let x = n as f64 / FRAME_SIZE as f64 * TAU;
            (1..=FRAME_SIZE / 2)
                .map(|h| 0.5 / h as f64 * (h as f64 * x).sin())
                .sum::<f64>() as f32
        })
        .collect()
}

#[test]
fn every_mode_at_0_50_and_100() {
    // Spec: engine-core R21 to R26
    let table = Cache::global().share(Wavetable::new(&[saw()]).unwrap());
    let mut suite = Suite::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"));
    for mode in Mode::ALL {
        let channels = [0.0, 50.0, 100.0]
            .map(|amount| {
                let mut osc = Oscillator::new(
                    table.clone(),
                    Layout {
                        voices: 1,
                        lanes_per_voice: 1,
                    },
                    48_000.0,
                    512,
                    1,
                );
                let p = osc.params_mut();
                p.phase = 0.0;
                p.phase_random = 0.0;
                let mut osc = ShapedOscillator::new(osc);
                osc.shapers_mut().slots[0].mode = mode;
                osc.shapers_mut().slots[0].amount = amount;
                osc.shapers_mut().slots[0].character = 0.25;
                osc.start(0, 57.0, Onset::Fresh);
                let n = osc.osc().lane_count();
                let pitch = vec![57.0; n];
                let mut out = vec![0.0; 512 * n];
                let mut audio = Vec::with_capacity(FRAMES);
                while audio.len() < FRAMES {
                    let inputs = Inputs {
                        lanes: 1,
                        pitch: &pitch,
                        ..Inputs::default()
                    };
                    osc.process(512, &inputs, &ShaperInputs::default(), &mut out);
                    audio.extend(out.iter().step_by(n));
                }
                audio
            })
            .to_vec();
        let name = format!(
            "shaper-{}",
            mode.name()
                .to_lowercase()
                .replace([' ', '/', '-'], "-")
                .replace('×', "x")
        );
        suite.add(&name, Output::new(48_000.0, channels));
    }
    suite.assert();
}
