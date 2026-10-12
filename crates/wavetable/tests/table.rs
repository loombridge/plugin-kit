//! The playable table: the morphed frames made at load, the mipmaps, and
//! the aliasing floor of a note reading the right level across the
//! keyboard at 44.1 to 192 kHz.

use std::f64::consts::TAU;

use lspk_test::measure::{Pitch, aliasing};
use lspk_wavetable::{
    FRAME_SIZE, FrameKind, Interpolation, MAX_FRAMES, Table, Wavetable, increment,
};

lspk_rt::install_trap!();

/// The sample rates a note is checked at.
const RATES: [f64; 4] = [44_100.0, 48_000.0, 96_000.0, 192_000.0];

/// The aliasing floor for a saw: everything away from the note's
/// harmonics stays this far below them, in dB, at every key.
const SAW_FLOOR_DB: f64 = -85.0;

/// The aliasing floor for the worst case, a frame with every harmonic at
/// the same level.
const FLAT_FLOOR_DB: f64 = -65.0;

/// One frame of every harmonic, harmonic `h` at `amplitude(h)`, in sine or
/// cosine phase.
fn frame(amplitude: impl Fn(usize) -> f64, sine: bool) -> Vec<f32> {
    (0..FRAME_SIZE)
        .map(|n| {
            let x = n as f64 / FRAME_SIZE as f64 * TAU;
            (1..=FRAME_SIZE / 2)
                .map(|h| {
                    let phase = h as f64 * x;
                    amplitude(h) * if sine { phase.sin() } else { phase.cos() }
                })
                .sum::<f64>() as f32
        })
        .collect()
}

fn saw() -> Wavetable {
    Wavetable::new(&[frame(|h| 0.5 / h as f64, true)]).unwrap()
}

fn flat() -> Wavetable {
    Wavetable::new(&[frame(|_| 1.0 / 64.0, false)]).unwrap()
}

fn sine(phase: f64) -> Vec<f32> {
    (0..FRAME_SIZE)
        .map(|n| (n as f64 / FRAME_SIZE as f64 * TAU + phase).sin() as f32)
        .collect()
}

/// The phase of sample `n` of a frame.
fn sample_phase(n: usize) -> u32 {
    (n << 21) as u32
}

/// Reads frame `frame` of `table` at the drawn samples' phases from the
/// level with every harmonic.
fn samples(table: &Table, frame: usize) -> Vec<f32> {
    let wave = table.wave(0, frame);
    (0..FRAME_SIZE)
        .map(|n| wave.read(sample_phase(n)))
        .collect()
}

/// `frames` samples of a note at `frequency` read from `level`, or the level
/// [`Table::level_for`] picks.
fn play(table: &Table, frequency: f64, rate: f64, frames: usize, level: Option<usize>) -> Vec<f32> {
    let step = increment(frequency, rate);
    let wave = table.wave(level.unwrap_or_else(|| table.level_for(step)), 0);
    let mut phase = 0u32;
    (0..frames)
        .map(|_| {
            let y = wave.read(phase);
            phase = phase.wrapping_add(step);
            y
        })
        .collect()
}

fn key_frequency(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// The worst aliasing of any key 0 to 127 at `rate`, in dB, and its key.
fn worst_aliasing(table: &Table, rate: f64) -> (f64, u8) {
    (0..=127u8)
        .map(|key| {
            let f = key_frequency(key);
            let audio = play(table, f, rate, 8_192, None);
            (aliasing(&audio, rate, Pitch::Steady(f)).ratio_db(), key)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap()
}

#[test]
fn a_table_stores_its_drawn_frames_and_morph_type_and_fills_the_rest_at_load() {
    // Spec: engine-core R13
    let drawn = Wavetable::new(&[sine(0.0), sine(1.0), sine(2.0)])
        .unwrap()
        .with_interpolation(Interpolation::Crossfade);
    assert_eq!(drawn.frame_count(), 3);
    let table = Table::build(drawn.clone());
    assert_eq!(table.frame_count(), MAX_FRAMES);
    assert_eq!(table.source(), &drawn);
    assert_eq!(table.frames()[0], FrameKind::Drawn(0));
    assert_eq!(table.frames()[128], FrameKind::Drawn(1));
    assert_eq!(table.frames()[255], FrameKind::Drawn(2));
    assert_eq!(table.frames()[64], FrameKind::Morphed { from: 0, t: 0.5 });
    let drawn_count = table
        .frames()
        .iter()
        .filter(|k| matches!(k, FrameKind::Drawn(_)))
        .count();
    assert_eq!(drawn_count, 3);

    // Without interpolation, or with one frame or a full table, the drawn
    // frames are the table.
    for (frames, interpolation) in [
        (3, Interpolation::None),
        (1, Interpolation::Spectral),
        (MAX_FRAMES, Interpolation::Crossfade),
    ] {
        let source = Wavetable::from_fn(frames, |f, n| ((f + n) % 7) as f32 / 7.0)
            .unwrap()
            .with_interpolation(interpolation);
        assert_eq!(Table::build(source).frame_count(), frames);
    }
}

#[test]
fn the_full_level_reads_back_the_drawn_samples() {
    // Spec: engine-core R13
    let source = Wavetable::new(&[frame(|h| 0.5 / h as f64, true), sine(0.3)]).unwrap();
    let table = Table::build(source.clone());
    for f in 0..2 {
        for (got, want) in samples(&table, f).iter().zip(source.frames()[f].iter()) {
            assert!((got - want).abs() < 2e-5, "frame {f}: {got} against {want}");
        }
    }
}

#[test]
fn a_crossfade_mixes_samples_and_a_spectral_morph_keeps_each_harmonic_whole() {
    // Spec: engine-core R13
    // From a sine to the same sine a quarter-turn on: halfway, a crossfade
    // is their mix, at 0.707, while a spectral morph turns the phase and
    // keeps the level at 1.
    let pair = || Wavetable::new(&[sine(0.0), sine(TAU / 4.0)]).unwrap();
    let peak = |table: &Table, frame| {
        samples(table, frame)
            .iter()
            .fold(0.0f32, |m, x| m.max(x.abs()))
    };

    let crossfade = Table::build(pair().with_interpolation(Interpolation::Crossfade));
    assert_eq!(
        crossfade.frames()[128],
        FrameKind::Morphed {
            from: 0,
            t: 128.0 / 255.0
        }
    );
    let mixed = samples(&crossfade, 128);
    let t = 128.0 / 255.0;
    for (n, &y) in mixed.iter().enumerate() {
        let want = (1.0 - t) * sine(0.0)[n] + t * sine(TAU / 4.0)[n];
        assert!((y - want).abs() < 2e-5, "sample {n}");
    }
    assert!((peak(&crossfade, 128) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);

    let spectral = Table::build(pair().with_interpolation(Interpolation::Spectral));
    assert!((peak(&spectral, 128) - 1.0).abs() < 1e-3);
    // The drawn frames are as drawn.
    for (got, want) in samples(&spectral, 255).iter().zip(sine(TAU / 4.0)) {
        assert!((got - want).abs() < 2e-5);
    }
}

#[test]
fn the_zero_phase_morphs_align_phases_before_morphing() {
    // Spec: engine-core R13
    // A sine and its inverse: a plain spectral morph passes through
    // silence or a full turn; aligning the fundamentals first, or zeroing
    // every phase, makes every frame the same full-level wave.
    let pair = || Wavetable::new(&[sine(0.0), sine(TAU / 2.0)]).unwrap();
    for interpolation in [
        Interpolation::SpectralZeroFundamental,
        Interpolation::SpectralZeroPhases,
    ] {
        let table = Table::build(pair().with_interpolation(interpolation));
        let first = samples(&table, 0);
        for f in [1, 100, 200, 255] {
            for (a, b) in samples(&table, f).iter().zip(&first) {
                assert!((a - b).abs() < 1e-4, "{interpolation:?}, frame {f}");
            }
        }
        let peak = first.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - 1.0).abs() < 1e-3, "{interpolation:?}");
    }
}

#[test]
fn the_level_for_a_note_keeps_every_harmonic_below_nyquist_and_most_of_the_band() {
    let table = Table::build(saw());
    for rate in RATES {
        for key in 0..=127u8 {
            let f = key_frequency(key);
            let level = table.levels()[table.level_for(increment(f, rate))];
            let top = level.harmonics as f64 * f;
            assert!(
                top < rate / 2.0,
                "{rate} Hz, key {key}: harmonic at {top} Hz"
            );
            // Levels step by at most 3/2, so a note keeps at least two
            // thirds of the band, unless it already has every harmonic or
            // only its fundamental fits.
            assert!(
                level.harmonics == FRAME_SIZE / 2
                    || level.harmonics == 1
                    || top >= rate / 3.0 * 0.999,
                "{rate} Hz, key {key}: only up to {top} Hz"
            );
        }
    }
}

#[test]
fn a_saw_read_at_the_right_level_stays_below_the_aliasing_floor_across_the_keyboard() {
    // Spec: engine-core R15
    let table = Table::build(saw());
    for rate in RATES {
        let (worst, key) = worst_aliasing(&table, rate);
        println!("saw at {rate} Hz: worst {worst:.1} dB, key {key}");
        assert!(worst < SAW_FLOOR_DB, "{rate} Hz, key {key}: {worst:.1} dB");
    }
}

#[test]
fn a_flat_spectrum_stays_below_the_worst_case_floor_across_the_keyboard() {
    // Spec: engine-core R15
    let table = Table::build(flat());
    for rate in RATES {
        let (worst, key) = worst_aliasing(&table, rate);
        println!("flat at {rate} Hz: worst {worst:.1} dB, key {key}");
        assert!(worst < FLAT_FLOOR_DB, "{rate} Hz, key {key}: {worst:.1} dB");
    }
}

#[test]
fn the_measurement_sees_aliasing_when_a_note_reads_the_wrong_level() {
    // Spec: engine-core R15
    // A5 at 44.1 kHz from the full level folds its upper harmonics back.
    let table = Table::build(saw());
    let f = key_frequency(81);
    let audio = play(&table, f, 44_100.0, 8_192, Some(0));
    let ratio = aliasing(&audio, 44_100.0, Pitch::Steady(f)).ratio_db();
    assert!(ratio > -60.0, "{ratio:.1} dB");
}

#[test]
fn reading_a_table_stays_off_the_heap() {
    let table = Table::build(saw());
    let mut block = vec![0.0f32; 256];
    lspk_rt::rt_test!({
        let step = increment(261.63, 48_000.0);
        let wave = table.wave(table.level_for(step), 0);
        let mut phase = 0u32;
        for sample in &mut block {
            *sample = wave.read(phase);
            phase = phase.wrapping_add(step);
        }
    });
    assert!(block.iter().any(|&x| x != 0.0));
}
