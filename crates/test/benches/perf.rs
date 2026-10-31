//! Micro-benchmarks for `lspk-test`'s perception features, which a dataset
//! render reads once per sound: extracting them from two seconds of a
//! decaying, tremolo-modulated stereo sound at 48 kHz, and the distance
//! between two sounds' features. `cargo xtask bench` times them, and CI's
//! instruction-count gate covers them (at the AVX2 level).

use std::f64::consts::TAU;
use std::hint::black_box;
use std::process::ExitCode;

use lspk_test::Output;
use lspk_test::bench::Suite;
use lspk_test::features::{self, Features};

const RATE: f64 = 48_000.0;

/// Two seconds of a few partials dying away, with a 5 Hz tremolo and a
/// slightly different second channel.
fn sound() -> Output {
    let channel = |detune: f64| -> Vec<f32> {
        (0..96_000)
            .map(|i| {
                let t = f64::from(i) / RATE;
                let tones: f64 = (1..=8)
                    .map(|n| (TAU * 220.0 * detune * f64::from(n) * t).sin() / f64::from(n))
                    .sum();
                (0.3 * tones * (-2.0 * t).exp() * (1.0 + 0.4 * (TAU * 5.0 * t).sin())) as f32
            })
            .collect()
    };
    Output::new(RATE, vec![channel(1.0), channel(1.003)])
}

fn main() -> ExitCode {
    let mut suite = Suite::new("lspk-test");
    suite.micro("features/extract", 1, || {
        let output = sound();
        move || {
            black_box(features::extract(black_box(&output)));
        }
    });
    suite.micro("features/distance", 1, || {
        let features = |output: Output| -> Features { features::extract(&output).unwrap() };
        let (a, b) = (features(sound()), features(sound().slice(8_000, 96_000)));
        move || {
            black_box(features::distance(black_box(&a), black_box(&b)));
        }
    });
    suite.main()
}
