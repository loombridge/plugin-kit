//! Micro-benchmarks for `lspk-wavetable`: loading a table (the WAV read,
//! the morphed frames and the mipmaps) and reading one per sample. `cargo
//! xtask bench` times them, and CI's instruction-count gate covers them.

use std::hint::black_box;
use std::process::ExitCode;

use lspk_test::bench::Suite;
use lspk_wavetable::{Interpolation, MAX_FRAMES, Table, Wavetable, increment, wav};

/// Frames in a block, as hosts commonly use.
const BLOCK: usize = 512;

/// A table of `frames` frames whose harmonics change from frame to frame,
/// as a WAV file's bytes.
fn wav_bytes(frames: usize, interpolation: Interpolation) -> Vec<u8> {
    let table = Wavetable::from_fn(frames, |frame, n| {
        let x = n as f32 / 2048.0 * std::f32::consts::TAU;
        let bright = 1.0 + frame as f32 / 8.0;
        (1..=24)
            .map(|h| (h as f32 * x).sin() / (h as f32).powf(2.0 / bright))
            .sum::<f32>()
            * 0.3
    })
    .expect("a valid table")
    .with_interpolation(interpolation);
    wav::write(&table, wav::SampleFormat::Float32)
}

fn main() -> ExitCode {
    let mut suite = Suite::new("lspk-wavetable");

    // A full table loading: 256 drawn frames read from a float WAV, then
    // every frame's mipmaps built.
    suite.micro("load/256-frames", 1, || {
        let bytes = wav_bytes(MAX_FRAMES, Interpolation::None);
        move || {
            let table = Table::build(wav::read(&bytes, None).expect("a valid file"));
            black_box(&table);
        }
    });

    // Four drawn frames morphed spectrally to 256 at load, then the
    // mipmaps.
    suite.micro("load/spectral-morph", 1, || {
        let bytes = wav_bytes(4, Interpolation::Spectral);
        move || {
            let table = Table::build(wav::read(&bytes, None).expect("a valid file"));
            black_box(&table);
        }
    });

    // A note reading its level, a block of samples per call: A4 at 48 kHz.
    suite.micro("read/block", BLOCK, || {
        let table = Table::build(wav::read(&wav_bytes(1, Interpolation::None), None).unwrap());
        let step = increment(440.0, 48_000.0);
        let level = table.level_for(step);
        let mut phase = 0u32;
        let mut out = vec![0.0f32; BLOCK];
        move || {
            let wave = table.wave(level, 0);
            for sample in &mut out {
                *sample = wave.read(phase);
                phase = phase.wrapping_add(step);
            }
            black_box(&out);
        }
    });

    suite.main()
}
