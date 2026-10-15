//! Micro-benchmarks for `lspk-wavetable`: loading a table (the WAV read,
//! the morphed frames and the mipmaps), reading one per sample, the
//! oscillator rendering one voice and 32, scalar and across lanes in SIMD,
//! the shapers a family at a time (including the inter-oscillator modes
//! reading a source), and unison: 8 voices of 7 copies and 32 of 16,
//! rendered and mixed to a stereo pair per voice, timed per voice-copy
//! sample, and the sub and noise oscillators.
//! `cargo xtask bench` times them, and CI's instruction-count gate covers
//! them (at the AVX2 level).

use std::hint::black_box;
use std::process::ExitCode;

use lspk_dsp::simd;
use lspk_test::bench::Suite;
use lspk_voice::{Unison, UnisonLanes};
use lspk_wavetable::noise::{
    COLOUR_RATE, COLOUR_SECONDS, Colour, Inputs as NoiseInputs, Noise, Samples,
};
use lspk_wavetable::osc::{Inputs, Layout, Onset, Oscillator};
use lspk_wavetable::shaper::{Mode, ShapedOscillator, ShaperInputs};
use lspk_wavetable::sub::{Inputs as SubInputs, Sub};
use lspk_wavetable::{Cache, Interpolation, MAX_FRAMES, Table, Wavetable, increment, wav};

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

    // The oscillator: one voice, 32 voices, and 32 voices with every lane's
    // frame moving at audio rate across a 16-frame table, a block per
    // call, scalar and at the SIMD level this CPU dispatches.
    for (name, voices, frame_mod) in [
        ("osc/one-voice", 1, false),
        ("osc/32-voices", 32, false),
        ("osc/32-voices-frame-mod", 32, true),
    ] {
        for simd in [false, true] {
            let id = format!("{name}/{}", if simd { "simd" } else { "scalar" });
            suite.micro(&id, BLOCK, move || oscillator(voices, frame_mod, simd));
        }
    }

    // Unison at the plan's reference block of 128 frames: every voice's
    // copies rendered and mixed to stereo, scalar and in SIMD. Items are
    // voice-copy samples, so the time per item is the CPU per voice-copy.
    for (voices, copies) in [(8, 7), (32, 16)] {
        for simd in [false, true] {
            let id = format!(
                "unison/{voices}x{copies}/{}",
                if simd { "simd" } else { "scalar" }
            );
            suite.micro(&id, UNISON_BLOCK * voices * copies, move || {
                unison(voices, copies, simd)
            });
        }
    }

    // The shapers, a family at a time: 32 voices across lanes, a block per
    // call, slot 1 in the family's mode at 60 % (and the two-slot case with
    // slot 2 too). Off measures what the shapers cost when they do nothing.
    for (name, modes, sourced) in [
        ("shaper/off", [Mode::Off, Mode::Off], false),
        ("shaper/smooth-pinch", [Mode::Pinch, Mode::Off], false),
        ("shaper/sync", [Mode::Sync, Mode::Off], false),
        (
            "shaper/edges-pulse-width",
            [Mode::PulseWidth, Mode::Off],
            false,
        ),
        ("shaper/quantise", [Mode::Quantise, Mode::Off], false),
        ("shaper/remap-drawn", [Mode::RemapDrawn, Mode::Off], false),
        ("shaper/odd-even", [Mode::OddEven, Mode::Off], false),
        ("shaper/filter-low-pass", [Mode::LowPass, Mode::Off], false),
        ("shaper/distortion-valve", [Mode::Valve, Mode::Off], false),
        (
            "shaper/distortion-hard-clip",
            [Mode::HardClip, Mode::Off],
            false,
        ),
        ("shaper/sync-then-valve", [Mode::Sync, Mode::Valve], false),
        ("shaper/fm", [Mode::Fm1, Mode::Off], true),
        ("shaper/fm-linear", [Mode::FmLin1, Mode::Off], true),
        ("shaper/fm-exponential", [Mode::FmExp1, Mode::Off], true),
        ("shaper/phase-distortion", [Mode::Pd1, Mode::Off], true),
        ("shaper/ring", [Mode::Rm1, Mode::Off], true),
        ("shaper/fm-then-ring", [Mode::Fm1, Mode::Rm1], true),
    ] {
        suite.micro(&format!("{name}/simd"), BLOCK, move || {
            shaped(32, modes, false, sourced)
        });
    }
    // One voice, scalar, with audio-rate amount modulation.
    suite.micro("shaper/distortion-valve-modulated/one-voice", BLOCK, || {
        shaped(1, [Mode::Valve, Mode::Off], true, false)
    });

    // The sub oscillator: 32 voices (one lane each) rendered across lanes.
    suite.micro("sub/32-voices", BLOCK, sub);

    // The noise oscillator: 32 voices of a generated colour block, both
    // channels. The block's own generation is timed too, as a load-time
    // cost.
    suite.micro("noise/32-voices", BLOCK, noise);
    suite.micro("noise/generate-colour", 1, || {
        move || {
            let block = Samples::colour(Colour::Pink, COLOUR_SECONDS, COLOUR_RATE, 1);
            black_box(&block);
        }
    });

    suite.main()
}

/// A sub of 32 voices and a routine rendering a block of it.
fn sub() -> impl FnMut() {
    let mut sub = Sub::new(32, 48_000.0, BLOCK);
    let pitch: Vec<f32> = (0..32).map(|v| 33.0 + v as f32).collect();
    for (voice, &key) in pitch.iter().enumerate() {
        sub.start(voice, key, Onset::Fresh);
    }
    let mut out = vec![0.0f32; BLOCK * sub.lane_count()];
    move || {
        let inputs = SubInputs {
            lanes: 32,
            pitch: &pitch,
            ..SubInputs::default()
        };
        sub.process(BLOCK, &inputs, &mut out);
        black_box(&out);
    }
}

/// A noise player of 32 voices and a routine rendering a block of a
/// generated colour block.
fn noise() -> impl FnMut() {
    let source = Samples::colour(Colour::White, COLOUR_SECONDS, COLOUR_RATE, 1);
    let mut noise = Noise::new(32, 48_000.0, BLOCK, 1);
    let pitch: Vec<f32> = (0..32).map(|v| 33.0 + v as f32).collect();
    for voice in 0..32 {
        noise.start(voice, Onset::Fresh);
    }
    let mut out = vec![0.0f32; BLOCK * 2 * noise.lane_count()];
    move || {
        let inputs = NoiseInputs {
            lanes: 32,
            pitch: &pitch,
            ..NoiseInputs::default()
        };
        noise.process(BLOCK, &inputs, &source, &mut out);
        black_box(&out);
    }
}

/// A shaped oscillator of `voices` voices with slots in `modes` at 60 %, and
/// a routine rendering a block of it, its amounts modulated at audio rate
/// with `modulated` and, with `sourced`, a source signal per lane for slot 1.
fn shaped(voices: usize, modes: [Mode; 2], modulated: bool, sourced: bool) -> impl FnMut() {
    let table = Cache::global()
        .share(wav::read(&wav_bytes(16, Interpolation::None), None).expect("a valid file"));
    let osc = Oscillator::new(
        table,
        Layout {
            voices,
            lanes_per_voice: 1,
        },
        48_000.0,
        BLOCK,
        1,
    );
    let mut osc = ShapedOscillator::new(osc);
    osc.osc_mut().params_mut().frame = 4.5;
    for (slot, mode) in osc.shapers_mut().slots.iter_mut().zip(modes) {
        slot.mode = mode;
        slot.amount = 60.0;
    }
    let keys: Vec<f32> = (0..voices).map(|v| 36.0 + v as f32 * 1.5).collect();
    for (voice, &key) in keys.iter().enumerate() {
        osc.start(voice, key, Onset::Fresh);
    }
    let lanes = osc.osc().lane_count();
    let mut pitch = vec![0.0; lanes];
    pitch[..voices].copy_from_slice(&keys);
    let modulation: Vec<f32> = (0..BLOCK * lanes)
        .map(|i| 20.0 * ((i / lanes) as f32 * 0.01 + (i % lanes) as f32).sin())
        .collect();
    // The source: a sine a fifth above the middle of the notes played.
    let signal: Vec<f32> = (0..BLOCK * lanes)
        .map(|i| {
            let at = (i / lanes) as f32 + (i % lanes) as f32;
            (at * 0.02).sin()
        })
        .collect();
    let mut out = vec![0.0f32; BLOCK * lanes];
    move || {
        let inputs = Inputs {
            lanes: voices,
            pitch: &pitch,
            ..Inputs::default()
        };
        let mods = ShaperInputs {
            amount_mod: [modulated.then_some(&modulation[..]), None],
            // Both slots read it: the pairs include FM then Ring.
            sources: [
                sourced.then_some(&signal[..]),
                sourced.then_some(&signal[..]),
            ],
        };
        osc.process(BLOCK, &inputs, &mods, &mut out);
        black_box(&out);
    }
}

/// The block unison is benchmarked at: the plan's reference, 128 frames.
const UNISON_BLOCK: usize = 128;

/// `voices` voices of `copies` unison copies on a 16-frame table, with a
/// frame spread, and a routine rendering and mixing a block of them.
fn unison(voices: usize, copies: usize, simd: bool) -> impl FnMut() {
    let table = Cache::global()
        .share(wav::read(&wav_bytes(16, Interpolation::None), None).expect("a valid file"));
    let mut osc = Oscillator::new(
        table,
        Layout {
            voices,
            lanes_per_voice: copies,
        },
        48_000.0,
        UNISON_BLOCK,
        1,
    );
    osc.params_mut().frame = 8.0;
    let mut lanes = UnisonLanes::new(voices, copies, 1);
    lanes.set(Unison {
        copies,
        detune: 0.3,
        frame_spread: 20.0,
        ..Unison::default()
    });
    let keys: Vec<f32> = (0..voices).map(|v| 36.0 + v as f32 * 1.5).collect();
    let mut pitch = vec![0.0; osc.lane_count()];
    for (voice, &key) in keys.iter().enumerate() {
        lanes.start(voice);
        let range = lanes.lanes_of(voice);
        osc.set_unison(range.clone(), lanes.pitch(), lanes.frame());
        osc.start(voice, key, Onset::Fresh);
        pitch[range].fill(key);
    }
    let mut out = vec![0.0f32; UNISON_BLOCK * osc.lane_count()];
    let mut mix = vec![0.0f32; UNISON_BLOCK * 2 * lanes.voice_stride()];
    let level = simd::level();
    move || {
        let inputs = Inputs {
            lanes: voices * copies,
            pitch: &pitch,
            ..Inputs::default()
        };
        mix.fill(0.0);
        if simd {
            osc.process_at(level, UNISON_BLOCK, &inputs, &mut out);
            lanes.mix_at(level, UNISON_BLOCK, voices, &out, &mut mix);
        } else {
            osc.process_scalar(UNISON_BLOCK, &inputs, &mut out);
            lanes.mix_scalar(UNISON_BLOCK, voices, &out, &mut mix);
        }
        black_box(&mix);
    }
}

/// An oscillator of `voices` voices on a 16-frame table, and a routine
/// rendering a block of it.
fn oscillator(voices: usize, frame_mod: bool, simd: bool) -> impl FnMut() {
    let table = Cache::global()
        .share(wav::read(&wav_bytes(16, Interpolation::None), None).expect("a valid file"));
    let mut osc = Oscillator::new(
        table,
        Layout {
            voices,
            lanes_per_voice: 1,
        },
        48_000.0,
        BLOCK,
        1,
    );
    osc.params_mut().frame = 4.5;
    let keys: Vec<f32> = (0..voices).map(|v| 36.0 + v as f32 * 1.5).collect();
    for (voice, &key) in keys.iter().enumerate() {
        osc.start(voice, key, Onset::Fresh);
    }
    let lanes = osc.lane_count();
    let mut pitch = vec![0.0; lanes];
    pitch[..voices].copy_from_slice(&keys);
    let modulation: Vec<f32> = (0..BLOCK * lanes)
        .map(|i| 7.0 * (1.0 + ((i / lanes) as f32 * 0.01 + (i % lanes) as f32).sin()))
        .collect();
    let mut out = vec![0.0f32; BLOCK * lanes];
    let level = simd::level();
    move || {
        let inputs = Inputs {
            lanes: voices,
            pitch: &pitch,
            frame_mod: frame_mod.then_some(&modulation[..]),
            ..Inputs::default()
        };
        if simd {
            osc.process_at(level, BLOCK, &inputs, &mut out);
        } else {
            osc.process_scalar(BLOCK, &inputs, &mut out);
        }
        black_box(&out);
    }
}
