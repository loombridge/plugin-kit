//! Micro-benchmarks for `lspk-dsp`'s hot paths. Add one here for each new
//! hot path as it lands; `cargo xtask bench` times them, and CI's
//! instruction-count gate covers them.

use std::hint::black_box;
use std::process::ExitCode;

use lspk_dsp::{FilterBank, FilterType, LinearSmoother, Oversampler, VoiceParams, simd};
use lspk_test::bench::Suite;
use lspk_test::signal;

/// Frames in a block, as hosts commonly use.
const BLOCK: usize = 512;

/// Voices in the filter benchmarks' banks.
const VOICES: usize = 16;

fn main() -> ExitCode {
    let mut suite = Suite::new("lspk-dsp");

    // A parameter gliding to a new value every block: 20 ms glides at
    // 48 kHz, a block of steps per call.
    suite.micro("smoother/glide", BLOCK, || {
        let mut smoother = LinearSmoother::new(0.0, 960);
        let mut target = 1.0;
        move || {
            smoother.set_target(target);
            target = 1.0 - target;
            let mut sum = 0.0;
            for _ in 0..BLOCK {
                sum += smoother.step();
            }
            black_box(sum);
        }
    });

    // A soft clipper oversampled by 2, 4 and 8: a block in, through the
    // filters up, the clipper, and the filters down.
    for (stages, name) in [
        (1, "oversampler/2x"),
        (2, "oversampler/4x"),
        (3, "oversampler/8x"),
    ] {
        suite.micro(name, BLOCK, move || {
            let mut oversampler = Oversampler::new(stages, 1, BLOCK);
            let input = signal::sine(1_000.0, 48_000.0, BLOCK, 0.9);
            let mut audio = input.clone();
            move || {
                audio.copy_from_slice(&input);
                oversampler.process(0, &mut audio, |high| {
                    for sample in high.iter_mut() {
                        *sample = 2.0 * *sample / (1.0 + (2.0 * *sample).abs());
                    }
                });
                black_box(&audio);
            }
        });
    }

    // Mixing one block into another at the SIMD level this CPU dispatches.
    suite.micro("simd/mix", BLOCK, || {
        let source = signal::noise(1, BLOCK, 0.5);
        let mut bus = vec![0.0; BLOCK];
        move || {
            simd::mix(&mut bus, &source, 0.25);
            black_box(&bus);
        }
    });

    // Every filter type over a block of 16 mono voices, each with its cutoff
    // swept at audio rate and its own resonance and drive: one voice at a
    // time (scalar), and in SIMD lanes across voices. Items are voice
    // samples, so the time per item is the cost of one voice's sample.
    for kind in FilterType::ALL {
        for simd in [false, true] {
            let name = format!("filter/{kind}/{}", if simd { "simd" } else { "scalar" });
            suite.micro(&name, BLOCK * VOICES, move || {
                let mut bank = if simd {
                    FilterBank::new(48_000.0, VOICES, 1)
                } else {
                    FilterBank::scalar(48_000.0, VOICES, 1)
                };
                bank.set_type(kind);
                let stride = bank.stride();
                for voice in 0..VOICES {
                    let x = voice as f32 / VOICES as f32;
                    bank.reset_voice(
                        voice,
                        &VoiceParams {
                            resonance: 0.9 * x,
                            drive: 0.5 * x,
                            saturation: x,
                            ..VoiceParams::default()
                        },
                    );
                }
                let pitch: Vec<f32> = (0..BLOCK * stride)
                    .map(|i| 70.0 + 30.0 * ((i / stride) as f32 * 0.01 + (i % stride) as f32).sin())
                    .collect();
                let noise = signal::noise(2, BLOCK * stride, 0.5);
                let mut audio = noise.clone();
                move || {
                    audio.copy_from_slice(&noise);
                    bank.process(&pitch, &mut audio);
                    black_box(&audio);
                }
            });
        }
    }

    suite.main()
}
