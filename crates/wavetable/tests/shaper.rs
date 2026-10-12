//! The shapers: both slots in series, every mode in its stage, aliasing at
//! high notes, block-size invariance, the vector paths against the scalar
//! one, the unison hooks, the preview, and no allocation.

use std::f64::consts::TAU;

use lspk_dsp::lfo::{Point, Shape};
use lspk_dsp::simd::{self, Level as SimdLevel};
use lspk_test::measure::{Pitch, aliasing_below};
use lspk_wavetable::osc::{Inputs, Layout, Onset, Oscillator};
use lspk_wavetable::shaper::{Mode, RemapCurve, ShapedOscillator, ShaperInputs, Slot, Stage};
use lspk_wavetable::{Cache, FRAME_SIZE, SharedTable, Wavetable};

lspk_rt::install_trap!();

/// One frame of harmonics `amplitude(h)`, in sine phase.
fn frame(amplitude: impl Fn(usize) -> f64) -> Vec<f32> {
    let amplitudes: Vec<(usize, f64)> = (1..=FRAME_SIZE / 2)
        .map(|h| (h, amplitude(h)))
        .filter(|&(_, a)| a != 0.0)
        .collect();
    (0..FRAME_SIZE)
        .map(|n| {
            let x = n as f64 / FRAME_SIZE as f64 * TAU;
            amplitudes
                .iter()
                .map(|&(h, a)| a * (h as f64 * x).sin())
                .sum::<f64>() as f32
        })
        .collect()
}

fn saw() -> Vec<f32> {
    frame(|h| 0.5 / h as f64)
}

fn sine() -> Vec<f32> {
    frame(|h| if h == 1 { 0.8 } else { 0.0 })
}

fn share(frames: &[Vec<f32>]) -> SharedTable {
    Cache::global().share(Wavetable::new(frames).unwrap())
}

/// A shaped oscillator of `voices` voices with `copies` lanes each, at
/// full level and phase 0.
fn shaped(
    table: SharedTable,
    voices: usize,
    copies: usize,
    rate: f32,
    max: usize,
) -> ShapedOscillator {
    let mut osc = Oscillator::new(
        table,
        Layout {
            voices,
            lanes_per_voice: copies,
        },
        rate,
        max,
        7,
    );
    let p = osc.params_mut();
    p.phase = 0.0;
    p.phase_random = 0.0;
    p.level = 100.0;
    ShapedOscillator::new(osc)
}

fn start_all(osc: &mut ShapedOscillator, keys: &[f32]) {
    for (voice, &key) in keys.iter().enumerate() {
        osc.start(voice, key, Onset::Fresh);
    }
}

fn pitches(osc: &ShapedOscillator, keys: &[f32]) -> Vec<f32> {
    let mut pitch = vec![0.0; osc.osc().lane_count()];
    for (voice, &key) in keys.iter().enumerate() {
        for lane in osc.osc().lanes_of(voice) {
            pitch[lane] = key;
        }
    }
    pitch
}

/// Renders `frames` frames in blocks of `block`, pitch held, amounts
/// modulated by `modulation(frame, lane)` per slot if given, at `level` or
/// scalar.
fn render(
    osc: &mut ShapedOscillator,
    pitch: &[f32],
    frames: usize,
    block: usize,
    level: Option<SimdLevel>,
    modulation: [Option<&dyn Fn(usize, usize) -> f32>; 2],
) -> Vec<f32> {
    let n = osc.osc().lane_count();
    let lanes = osc.osc().layout().voices * osc.osc().layout().lanes_per_voice;
    let mut out = vec![0.0; frames * n];
    let mut mods = [vec![0.0; block * n], vec![0.0; block * n]];
    let mut done = 0;
    while done < frames {
        let len = (frames - done).min(block);
        for (buffer, f) in mods.iter_mut().zip(&modulation) {
            if let Some(f) = f {
                for k in 0..len {
                    for l in 0..n {
                        buffer[k * n + l] = f(done + k, l);
                    }
                }
            }
        }
        let inputs = Inputs {
            lanes,
            pitch,
            ..Inputs::default()
        };
        let shaper_inputs = ShaperInputs {
            amount_mod: [
                modulation[0].map(|_| &mods[0][..len * n]),
                modulation[1].map(|_| &mods[1][..len * n]),
            ],
        };
        let out_block = &mut out[done * n..(done + len) * n];
        match level {
            Some(level) => osc.process_at(level, len, &inputs, &shaper_inputs, out_block),
            None => osc.process_scalar(len, &inputs, &shaper_inputs, out_block),
        }
        done += len;
    }
    out
}

fn lane(out: &[f32], lane: usize, lane_count: usize) -> Vec<f32> {
    out.iter().skip(lane).step_by(lane_count).copied().collect()
}

fn key_frequency(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

/// Slot `slot` of `osc` set to `mode` at `amount`.
fn set(osc: &mut ShapedOscillator, slot: usize, mode: Mode, amount: f32) {
    let s = &mut osc.shapers_mut().slots[slot];
    s.mode = mode;
    s.amount = amount;
}

/// One voice at `key` on `table` with slot 1 in `mode` at `amount`,
/// rendered for `frames` frames at 48 kHz.
fn one_note(table: &SharedTable, key: f32, slots: [(Mode, f32); 2], frames: usize) -> Vec<f32> {
    let mut osc = shaped(table.clone(), 1, 1, 48_000.0, 512);
    for (i, (mode, amount)) in slots.into_iter().enumerate() {
        set(&mut osc, i, mode, amount);
    }
    start_all(&mut osc, &[key]);
    let pitch = pitches(&osc, &[key]);
    let out = render(
        &mut osc,
        &pitch,
        frames,
        512,
        Some(simd::level()),
        [None, None],
    );
    lane(&out, 0, osc.osc().lane_count())
}

/// The magnitude of harmonics 1 to `count` of one cycle `cycle`.
fn harmonics(cycle: &[f32], count: usize) -> Vec<f64> {
    let n = cycle.len() as f64;
    (1..=count)
        .map(|h| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, &x) in cycle.iter().enumerate() {
                let a = TAU * h as f64 * i as f64 / n;
                re += f64::from(x) * a.cos();
                im += f64::from(x) * a.sin();
            }
            2.0 * (re * re + im * im).sqrt() / n
        })
        .collect()
}

/// The energy of harmonics `harmonics` of `f0` in `audio`, from a Hann-windowed
/// spectrum of its last 8,192 frames, per harmonic.
fn harmonic_levels(audio: &[f32], rate: f64, f0: f64, count: usize) -> Vec<f64> {
    const N: usize = 8_192;
    let fft = realfft::RealFftPlanner::<f64>::new().plan_fft_forward(N);
    let mut input: Vec<f64> = audio[audio.len() - N..]
        .iter()
        .enumerate()
        .map(|(i, &x)| f64::from(x) * (0.5 - 0.5 * (TAU * i as f64 / N as f64).cos()))
        .collect();
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    (1..=count)
        .map(|h| {
            let bin = (h as f64 * f0 * N as f64 / rate).round() as usize;
            (bin.saturating_sub(3)..=bin + 3)
                .map(|k| spectrum.get(k).map_or(0.0, |c| c.norm_sqr()))
                .sum::<f64>()
        })
        .collect()
}

fn db(x: f64) -> f64 {
    10.0 * x.log10()
}

#[test]
fn both_slots_start_off_and_swapping_exchanges_them() {
    // Spec: engine-core R21
    let mut osc = shaped(share(&[saw()]), 1, 1, 48_000.0, 64);
    assert_eq!(osc.shapers().slots, [Slot::default(); 2]);
    assert_eq!(osc.shapers().slots[0].mode, Mode::Off);
    assert_eq!(osc.shapers().slots[0].amount, 0.0);
    osc.shapers_mut().slots[0] = Slot {
        mode: Mode::Sync,
        amount: 30.0,
        character: 0.4,
    };
    osc.shapers_mut().slots[1] = Slot {
        mode: Mode::HardClip,
        amount: 70.0,
        character: 0.0,
    };
    osc.shapers_mut().swap();
    let slots = osc.shapers().slots;
    assert_eq!((slots[0].mode, slots[0].amount), (Mode::HardClip, 70.0));
    assert_eq!(
        (slots[1].mode, slots[1].amount, slots[1].character),
        (Mode::Sync, 30.0, 0.4)
    );
    // Every mode this package builds is in one stage or the other.
    assert_eq!(Mode::ALL.len(), 33);
    let waves = Mode::ALL
        .iter()
        .filter(|m| m.stage() == Stage::Wave)
        .count();
    assert_eq!(waves, 16, "two filters and fourteen distortions");
}

#[test]
fn the_slots_run_in_series_slot_1_first() {
    // Spec: engine-core R22
    let table = share(&[saw()]);
    let key = 57.0;
    // Hard clip then a low-pass: a smooth, filtered clip. The swapped order
    // clips the filtered wave, squarer and brighter.
    let clip_then_filter = one_note(
        &table,
        key,
        [(Mode::HardClip, 70.0), (Mode::LowPass, 80.0)],
        16_384,
    );
    let filter_then_clip = one_note(
        &table,
        key,
        [(Mode::LowPass, 80.0), (Mode::HardClip, 70.0)],
        16_384,
    );
    let f0 = key_frequency(f64::from(key));
    let upper = |audio: &[f32]| {
        harmonic_levels(audio, 48_000.0, f0, 40)[10..]
            .iter()
            .sum::<f64>()
    };
    let (smooth, square) = (upper(&clip_then_filter), upper(&filter_then_clip));
    println!(
        "harmonics 11 to 40: clip then filter {:.1} dB, filter then clip {:.1} dB",
        db(smooth),
        db(square)
    );
    assert!(db(square) > db(smooth) + 10.0);

    // Each mode alone sounds the same in either slot.
    for mode in [Mode::HardClip, Mode::LowPass, Mode::Sync, Mode::Mirror] {
        let first = one_note(&table, key, [(mode, 60.0), (Mode::Off, 0.0)], 4_096);
        let second = one_note(&table, key, [(Mode::Off, 0.0), (mode, 60.0)], 4_096);
        assert_eq!(first, second, "{}", mode.name());
    }

    // Two phase modes: slot 2's map is applied first, then slot 1's, so
    // the wave slot 1 made is reshaped by slot 2, as the preview draws it.
    let mut shapers = shaped(table.clone(), 1, 1, 48_000.0, 64);
    let s = shapers.shapers_mut();
    s.slots[0] = Slot {
        mode: Mode::PulseWidth,
        amount: 50.0,
        character: 0.0,
    };
    s.slots[1] = Slot {
        mode: Mode::LeanRight,
        amount: 60.0,
        character: 0.0,
    };
    let mut both = vec![0.0; 1024];
    s.preview(&table, 0, &mut both);
    let mut first_only = s.clone();
    first_only.slots[1].mode = Mode::Off;
    let mut pulse = vec![0.0; 1024];
    first_only.preview(&table, 0, &mut pulse);
    // The pulse shape ends where the lean puts the cycle's point 0.5, not
    // at 0.5: the leaned pulse holds its start value over a shorter stretch.
    let held = |cycle: &[f32]| cycle.iter().filter(|x| x.abs() < 1e-3).count();
    println!(
        "held points: pulse {}, leaned pulse {}",
        held(&pulse),
        held(&both)
    );
    assert!(held(&both) + 50 < held(&pulse));
}

#[test]
fn sync_raises_the_harmonics_and_keeps_the_note() {
    // Spec: engine-core R23
    let table = share(&[saw()]);
    let key = 48.5;
    let f0 = key_frequency(f64::from(key));
    let centroid = |audio: &[f32]| {
        let levels = harmonic_levels(audio, 48_000.0, f0, 60);
        let total: f64 = levels.iter().sum();
        levels
            .iter()
            .enumerate()
            .map(|(i, e)| (i + 1) as f64 * e)
            .sum::<f64>()
            / total
    };
    let mut last = 0.0;
    for amount in [0.0, 25.0, 50.0, 75.0] {
        let audio = one_note(
            &table,
            key,
            [(Mode::Sync, amount), (Mode::Off, 0.0)],
            16_384,
        );
        // Still periodic at the note: everything sits on its harmonics.
        let off = aliasing_below(&audio[8_192..], 48_000.0, Pitch::Steady(f0), 20_000.0).ratio_db();
        let c = centroid(&audio);
        println!(
            "sync {amount}%: away from the note's harmonics {off:.1} dB, centroid harmonic {c:.2}"
        );
        assert!(off < -80.0, "{amount}%: {off:.1} dB");
        assert!(c > last, "{amount}%: centroid {c} after {last}");
        last = c;
    }
    // Softness takes the edge off the restart: less energy high up (on a
    // sine, whose own harmonics don't mask the edge's).
    let sine_table = share(&[sine()]);
    let soft = |character: f32| {
        let mut osc = shaped(sine_table.clone(), 1, 1, 48_000.0, 512);
        osc.shapers_mut().slots[0] = Slot {
            mode: Mode::Sync,
            amount: 40.0,
            character,
        };
        start_all(&mut osc, &[key]);
        let pitch = pitches(&osc, &[key]);
        let out = render(
            &mut osc,
            &pitch,
            16_384,
            512,
            Some(simd::level()),
            [None, None],
        );
        let levels = harmonic_levels(&lane(&out, 0, osc.osc().lane_count()), 48_000.0, f0, 120);
        db(levels[40..].iter().sum::<f64>() / levels.iter().sum::<f64>())
    };
    let (hard, softer) = (soft(0.0), soft(1.0));
    println!("above harmonic 40: hard {hard:.1} dB, soft {softer:.1} dB");
    assert!(softer < hard - 10.0);
}

#[test]
fn the_paired_modes_are_neutral_at_half_and_move_apart_either_side() {
    // Spec: engine-core R24
    let table = share(&[saw()]);
    let key = 60.5;
    let render_at = |slots| one_note(&table, key, slots, 4_096);
    let off = (Mode::Off, 0.0);
    for (both, up, down) in [
        (Mode::Pinch, Mode::PinchIn, Mode::PinchOut),
        (Mode::Lean, Mode::LeanRight, Mode::LeanLeft),
    ] {
        // Neutral at 50 %, as the one-way modes are at 0 %.
        assert_eq!(
            render_at([(both, 50.0), off]),
            render_at([(up, 0.0), off]),
            "{}",
            both.name()
        );
        // Each side is one of the one-way modes at twice the distance.
        assert_eq!(
            render_at([(both, 100.0), off]),
            render_at([(up, 100.0), off]),
            "{}",
            both.name()
        );
        assert_eq!(
            render_at([(both, 0.0), off]),
            render_at([(down, 100.0), off]),
            "{}",
            both.name()
        );
        assert_ne!(render_at([(up, 50.0), off]), render_at([(down, 50.0), off]));
    }
    // Odd/even at 50 % is the plain oscillator.
    let plain = render_at([off, off]);
    let half = render_at([(Mode::OddEven, 50.0), off]);
    for (a, b) in plain.iter().zip(&half) {
        assert!((a - b).abs() < 1e-5, "{a} against {b}");
    }
}

#[test]
fn odd_even_keeps_only_the_odd_or_the_even_harmonics() {
    // Spec: engine-core R24
    let table = share(&[saw()]);
    let mut shapers = shaped(table.clone(), 1, 1, 48_000.0, 64).shapers().clone();
    shapers.slots[0].mode = Mode::OddEven;
    let mut cycle = vec![0.0; 2048];
    for (amount, keep_odd) in [(0.0, true), (100.0, false)] {
        shapers.slots[0].amount = amount;
        shapers.preview(&table, 0, &mut cycle);
        let h = harmonics(&cycle, 12);
        let (odd, even): (Vec<_>, Vec<_>) = h.iter().enumerate().partition(|(i, _)| i % 2 == 0);
        let (kept, dropped) = if keep_odd { (odd, even) } else { (even, odd) };
        let dropped = dropped.iter().map(|(_, v)| **v).fold(0.0, f64::max);
        let kept = kept.iter().map(|(_, v)| **v).fold(f64::INFINITY, f64::min);
        println!("odd/even {amount}%: weakest kept {kept:.2e}, strongest dropped {dropped:.2e}");
        assert!(dropped < 1e-4 && kept > 1e-2);
    }
    // A live render too: at 0 % the second harmonic is gone.
    let key = 55.5;
    let audio = one_note(
        &table,
        key,
        [(Mode::OddEven, 0.0), (Mode::Off, 0.0)],
        16_384,
    );
    let levels = harmonic_levels(&audio, 48_000.0, key_frequency(f64::from(key)), 4);
    assert!(
        db(levels[1] / levels[0]) < -60.0,
        "{:.1} dB",
        db(levels[1] / levels[0])
    );
}

#[test]
fn the_remaps_blend_from_identity_to_the_curve() {
    // Spec: engine-core R25
    let table = share(&[saw()]);
    let mut shapers = shaped(table.clone(), 1, 1, 48_000.0, 64).shapers().clone();
    let len = 512;
    let wave = table.wave(0, 0);
    let read = |x: f64| wave.read((x.rem_euclid(1.0) * lspk_wavetable::CYCLE) as u64 as u32);
    let mut plain = vec![0.0; len];
    shapers.preview(&table, 0, &mut plain);
    // A curve that bends the phase: slow, then fast.
    let shape = Shape::new(&[
        Point::new(0.0, 0.0),
        Point::new(0.5, 0.2),
        Point::new(1.0, 1.0),
    ]);
    shapers.curve = RemapCurve::new(&shape);
    let curve = |x: f64| shapers.curve.value(x as f32) as f64;
    let mut cycle = vec![0.0; len];
    for mode in [
        Mode::RemapDrawn,
        Mode::RemapMirrored,
        Mode::RemapSine,
        Mode::RemapQuad,
    ] {
        shapers.slots[0].mode = mode;
        shapers.slots[0].amount = 0.0;
        shapers.preview(&table, 0, &mut cycle);
        for (a, b) in cycle.iter().zip(&plain) {
            assert!((a - b).abs() < 1e-5, "{} at 0 %", mode.name());
        }
        shapers.slots[0].amount = 100.0;
        shapers.preview(&table, 0, &mut cycle);
        let want = |u: f64| match mode {
            Mode::RemapDrawn => curve(u),
            Mode::RemapMirrored if u < 0.5 => 0.5 * curve(2.0 * u),
            Mode::RemapMirrored => 1.0 - 0.5 * curve(2.0 - 2.0 * u),
            Mode::RemapSine => u - (TAU * u).sin() / TAU,
            _ => ((4.0 * u).floor() + curve(4.0 * u - (4.0 * u).floor())) / 4.0,
        };
        let worst = (0..len)
            .map(|i| (cycle[i] - read(want(i as f64 / len as f64))).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "{}: {worst}", mode.name());
    }
}

#[test]
fn quantise_steps_follow_the_pitch() {
    // Spec: engine-core R26
    let table = share(&[sine()]);
    let mut shapers = shaped(table.clone(), 1, 1, 48_000.0, 64).shapers().clone();
    shapers.slots[0].mode = Mode::Quantise;
    // 2 × 1024^(1 − a) = 8 steps.
    let amount = 80.0;
    shapers.slots[0].amount = amount;
    let mut cycle = vec![0.0; 4096];
    shapers.preview(&table, 0, &mut cycle);
    // Runs of a held value, wrapping round the cycle.
    let mut runs = 0;
    let mut length = 0;
    for i in 0..cycle.len() {
        let next = cycle[(i + 1) % cycle.len()];
        if (next - cycle[i]).abs() < 1e-6 {
            length += 1;
        } else {
            if length > 100 {
                runs += 1;
            }
            length = 0;
        }
    }
    assert_eq!(runs, 8);
    // The steps' images sit at the same harmonics whatever the note, as a
    // fixed-rate reducer's wouldn't.
    let image = |key: f32| {
        let audio = one_note(
            &table,
            key,
            [(Mode::Quantise, amount), (Mode::Off, 0.0)],
            16_384,
        );
        let levels = harmonic_levels(&audio, 48_000.0, key_frequency(f64::from(key)), 9);
        db(levels[6] / levels[0])
    };
    let (low, high) = (image(45.5), image(57.5));
    println!("harmonic 7 against 1: {low:.2} dB at key 45.5, {high:.2} dB at key 57.5");
    assert!((low - high).abs() < 0.5 && low > -30.0);
}

#[test]
fn the_filters_follow_the_note() {
    // A filter shaper's cutoff is a harmonic of the note: the same harmonic
    // is cut by the same amount at every pitch.
    let table = share(&[saw()]);
    for mode in [Mode::LowPass, Mode::HighPass] {
        let ratio = |key: f32| {
            let amount = if mode == Mode::LowPass { 85.0 } else { 30.0 };
            let audio = one_note(&table, key, [(mode, amount), (Mode::Off, 0.0)], 16_384);
            let f0 = key_frequency(f64::from(key));
            let plain = one_note(&table, key, [(Mode::Off, 0.0), (Mode::Off, 0.0)], 16_384);
            let (a, b) = (
                harmonic_levels(&audio, 48_000.0, f0, 8),
                harmonic_levels(&plain, 48_000.0, f0, 8),
            );
            (db(a[0] / b[0]), db(a[7] / b[7]))
        };
        let (low, high) = (ratio(43.5), ratio(55.5));
        eprintln!(
            "{}: fundamental and 8th {low:.2?} at 43.5, {high:.2?} at 55.5",
            mode.name()
        );
        assert!((low.0 - high.0).abs() < 0.3 && (low.1 - high.1).abs() < 0.3);
    }
}

#[test]
fn every_mode_renders_bounded_audio_at_0_50_and_100() {
    // Spec: engine-core R21 to R26
    let table = share(&[saw()]);
    let key = 64.5;
    for mode in Mode::ALL {
        for amount in [0.0, 50.0, 100.0] {
            let audio = one_note(&table, key, [(mode, amount), (Mode::Off, 0.0)], 4_096);
            let peak = audio.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(
                audio.iter().all(|x| x.is_finite()),
                "{} {amount}%",
                mode.name()
            );
            assert!(peak < 4.0, "{} {amount}%: peak {peak}", mode.name());
        }
    }
}

/// The worst in-band aliasing among `keys`, slot 1 in `mode` at `amount`, on
/// a saw at 48 kHz at the default oversampling, and its key. Lanes with
/// next to nothing in band (Sync's internal oscillator above it, Quantise
/// at two steps of a saw) are skipped.
fn worst_aliasing(mode: Mode, amount: f32, keys: &[f32]) -> (f64, f32) {
    let rate = 48_000.0;
    let mut osc = shaped(share(&[saw()]), keys.len(), 1, rate, 512);
    set(&mut osc, 0, mode, amount);
    start_all(&mut osc, keys);
    let pitch = pitches(&osc, keys);
    let out = render(
        &mut osc,
        &pitch,
        12_288,
        512,
        Some(simd::level()),
        [None, None],
    );
    let n = osc.osc().lane_count();
    let mut worst = (f64::NEG_INFINITY, 0.0);
    for (l, &key) in keys.iter().enumerate() {
        let audio = lane(&out, l, n);
        let f = key_frequency(f64::from(key));
        let steady = &audio[4096..];
        if steady.iter().map(|x| x * x).sum::<f32>() / (steady.len() as f32) < 1e-6 {
            continue;
        }
        let band = aliasing_below(steady, f64::from(rate), Pitch::Steady(f), 20_000.0);
        let all = aliasing_below(steady, f64::from(rate), Pitch::Steady(f), 24_000.0);
        if band.harmonic + band.other < 1e-3 * (all.harmonic + all.other) {
            continue;
        }
        if band.ratio_db() > worst.0 {
            worst = (band.ratio_db(), key);
        }
    }
    worst
}

#[test]
fn aliasing_stays_low_at_high_notes() {
    // Spec: engine-core R15, R21 to R26
    // Every key from 60.5 to 96.5 in steps of 3 (277 Hz to 2.2 kHz), at 0,
    // 50 and 100 %, measured below 20 kHz. Each family's bound sits a few
    // dB above what it measures; the oscillator alone is at −88.6 dB here.
    let keys: Vec<f32> = (0..13).map(|i| 60.5 + i as f32 * 3.0).collect();
    let bound = |mode: Mode, amount: f32| -> f64 {
        match mode {
            Mode::PinchIn | Mode::PinchOut | Mode::Pinch
                if amount != 50.0 || mode != Mode::Pinch =>
            {
                if amount == 0.0 && mode != Mode::Pinch {
                    -85.0
                } else {
                    -60.0
                }
            }
            Mode::PulseWidth if amount == 100.0 => -45.0,
            Mode::Sync | Mode::PulseWidth | Mode::Flip | Mode::Mirror | Mode::Quantise => -78.0,
            Mode::LowPass | Mode::HighPass | Mode::OddEven => -72.0,
            m if m.distortion() && amount == 100.0 => -52.0,
            m if m.distortion() && amount == 50.0 => {
                if m == Mode::Fold {
                    -60.0
                } else {
                    -55.0
                }
            }
            _ => -85.0,
        }
    };
    let mut failures = Vec::new();
    for mode in Mode::ALL.into_iter().skip(1) {
        let mut line = format!("{:>15}", mode.name());
        for amount in [0.0, 50.0, 100.0] {
            let (worst, key) = worst_aliasing(mode, amount, &keys);
            line += &format!("  {amount:>3}%: {worst:6.1} dB (key {key})");
            if worst > bound(mode, amount) {
                failures.push(format!(
                    "{} {amount}%: {worst:.1} dB at key {key}",
                    mode.name()
                ));
            }
        }
        println!("{line}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A busy render: two slots, audio-rate amount modulation, unison offsets,
/// five voices of three lanes, in blocks of `blocks`.
fn busy_render(
    slots: [(Mode, f32); 2],
    blocks: &[usize],
    level: Option<SimdLevel>,
    modulated: bool,
) -> (Vec<f32>, usize) {
    let table = share(&[saw(), sine()]);
    let mut osc = shaped(table, 5, 3, 48_000.0, 512);
    for (i, (mode, amount)) in slots.into_iter().enumerate() {
        set(&mut osc, i, mode, amount);
    }
    osc.shapers_mut().slots[0].character = 0.3;
    osc.osc_mut().params_mut().frame = 1.4;
    osc.osc_mut().params_mut().smooth = true;
    let keys = [36.0, 52.5, 69.0, 84.0, 98.0];
    start_all(&mut osc, &keys);
    for slot in 0..2 {
        for (l, offset) in osc.amount_offsets_mut(slot).iter_mut().enumerate() {
            *offset = (l % 3) as f32 * 12.0 - 12.0;
        }
    }
    let pitch = pitches(&osc, &keys);
    let n = osc.osc().lane_count();
    let wobble = |k: usize, l: usize| 20.0 * ((k as f32) * 0.003 + l as f32).sin();
    let mut out = Vec::new();
    let mut done = 0;
    for &len in blocks {
        let mods: Vec<f32> = (0..len * n).map(|i| wobble(done + i / n, i % n)).collect();
        let inputs = Inputs {
            lanes: 15,
            pitch: &pitch,
            ..Inputs::default()
        };
        let shaper_inputs = ShaperInputs {
            amount_mod: [
                modulated.then_some(&mods[..]),
                modulated.then_some(&mods[..]),
            ],
        };
        let mut block = vec![0.0; len * n];
        match level {
            Some(level) => osc.process_at(level, len, &inputs, &shaper_inputs, &mut block),
            None => osc.process_scalar(len, &inputs, &shaper_inputs, &mut block),
        }
        out.extend(block);
        done += len;
    }
    (out, n)
}

/// One mode of each family, and pairs across stages.
const BUSY: [[(Mode, f32); 2]; 9] = [
    [(Mode::Sync, 40.0), (Mode::Off, 0.0)],
    [(Mode::Pinch, 70.0), (Mode::Lean, 20.0)],
    [(Mode::PulseWidth, 50.0), (Mode::Quantise, 70.0)],
    [(Mode::Flip, 40.0), (Mode::Mirror, 30.0)],
    [(Mode::RemapQuad, 60.0), (Mode::OddEven, 30.0)],
    [(Mode::LowPass, 60.0), (Mode::HighPass, 30.0)],
    [(Mode::Valve, 50.0), (Mode::Fold, 40.0)],
    [(Mode::HardClip, 70.0), (Mode::Sync, 50.0)],
    [(Mode::Rectify, 60.0), (Mode::SoftSaturate, 80.0)],
];

#[test]
fn every_simd_level_matches_the_scalar_path() {
    let blocks = [300, 17, 256, 1, 100];
    for slots in BUSY {
        let (reference, n) = busy_render(slots, &blocks, None, true);
        for level in SimdLevel::ALL.into_iter().filter(|l| l.is_supported()) {
            let (got, _) = busy_render(slots, &blocks, Some(level), true);
            let mut squares = 0.0;
            let mut count = 0;
            let mut lanes = [0.0f64; 15];
            for (i, (a, b)) in got.iter().zip(&reference).enumerate() {
                if i % n >= 15 {
                    continue;
                }
                lanes[i % n] += f64::from(a - b).powi(2);
                squares += f64::from(a - b).powi(2);
                count += 1;
                // A driven clip in front of Sync steepens the drift further.
                assert!(
                    (a - b).abs() < 1e-2,
                    "{slots:?} {level}: frame {} lane {}: {a} against {b}",
                    i / n,
                    i % n
                );
            }
            // The increments differ in their last bit where one path fuses a
            // multiply-add and the other doesn't, so the phases drift apart
            // by parts per million over a render (as the oscillator's own
            // test allows), and Sync multiplies the drift by its ratio.
            let rms = (squares / f64::from(count)).sqrt();
            assert!(
                rms < 1e-3,
                "{slots:?} {level}: {rms:e} RMS, per lane {lanes:?}"
            );
        }
    }
}

#[test]
fn block_size_doesnt_change_the_render() {
    let whole = [512, 512];
    let pieces = [1, 37, 128, 64, 3, 279, 300, 200, 12];
    for slots in BUSY {
        // Fixed amounts: the same to the bit, however the blocks split.
        let (a, n) = busy_render(slots, &whole, Some(simd::level()), false);
        let (b, _) = busy_render(slots, &pieces, Some(simd::level()), false);
        assert_eq!(a.len(), b.len());
        let mut worst = 0.0f32;
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            if i % n < 15 {
                worst = worst.max((x - y).abs());
            }
        }
        assert!(worst < 1e-6, "{slots:?}: {worst}");
        // Audio-rate amounts: the mipmap level follows the amount a stretch
        // at a time, so splits may pick a level a stretch apart; the
        // difference stays tiny.
        let (a, _) = busy_render(slots, &whole, Some(simd::level()), true);
        let (b, _) = busy_render(slots, &pieces, Some(simd::level()), true);
        let rms = (a
            .iter()
            .zip(&b)
            .map(|(x, y)| f64::from(x - y).powi(2))
            .sum::<f64>()
            / a.len() as f64)
            .sqrt();
        let stretches = slots.iter().any(|(m, _)| {
            m.stage() == Stage::Phase && !matches!(m, Mode::Flip | Mode::Quantise | Mode::OddEven)
        });
        println!("{slots:?} modulated: {rms:.1e} RMS");
        assert!(
            rms < if stretches { 5e-3 } else { 1e-5 },
            "{slots:?} modulated: {rms:e} RMS"
        );
    }
}

#[test]
fn unison_offsets_and_modulation_move_each_lane_s_amount() {
    // Spec: engine-core R31, R38
    let table = share(&[saw()]);
    let render_with = |knob: f32, offsets: Option<[f32; 2]>, modulation: f32| {
        let mut osc = shaped(table.clone(), 1, 2, 48_000.0, 256);
        set(&mut osc, 0, Mode::Sync, knob);
        set(&mut osc, 1, Mode::SoftClip, knob);
        start_all(&mut osc, &[62.0]);
        if let Some(offsets) = offsets {
            let n = osc.osc().lane_count();
            let mut lanes = vec![0.0; n];
            lanes[..2].copy_from_slice(&offsets);
            osc.set_unison(0..2, [&lanes, &lanes]);
        }
        let pitch = pitches(&osc, &[62.0]);
        let f = move |_: usize, _: usize| modulation;
        let out = render(
            &mut osc,
            &pitch,
            1_024,
            256,
            Some(simd::level()),
            [Some(&f), Some(&f)],
        );
        let n = osc.osc().lane_count();
        (lane(&out, 0, n), lane(&out, 1, n))
    };
    // A lane offset, modulation and the knob all add up.
    let same = |a: &[f32], b: &[f32], what: &str| {
        let worst = a
            .iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "{what}: {worst}");
    };
    let (spread0, spread1) = render_with(30.0, Some([-20.0, 25.0]), 0.0);
    same(&spread0, &render_with(10.0, None, 0.0).0, "offset −20");
    same(&spread1, &render_with(55.0, None, 0.0).0, "offset +25");
    same(&render_with(30.0, None, 25.0).0, &spread1, "modulation +25");
    // The sum is clamped to the amount's range.
    same(
        &render_with(90.0, Some([40.0, 40.0]), 0.0).0,
        &render_with(100.0, None, 0.0).0,
        "clamped high",
    );
    same(
        &render_with(10.0, None, -50.0).0,
        &render_with(0.0, None, 0.0).0,
        "clamped low",
    );
}

#[test]
fn the_preview_draws_one_cycle_through_both_slots() {
    // Inventory WT-015.
    let table = share(&[saw()]);
    let mut shapers = shaped(table.clone(), 1, 1, 48_000.0, 64).shapers().clone();
    let mut plain = vec![0.0; 1024];
    shapers.preview(&table, 0, &mut plain);
    let wave = table.wave(0, 0);
    for (i, &x) in plain.iter().enumerate() {
        let want = wave.read((i as f64 / 1024.0 * lspk_wavetable::CYCLE) as u32);
        assert!((x - want).abs() < 1e-6);
    }
    // A distortion: its curve applied to the shaped cycle.
    shapers.slots[1] = Slot {
        mode: Mode::HardClip,
        amount: 50.0,
        character: 0.0,
    };
    let mut clipped = vec![0.0; 1024];
    shapers.preview(&table, 0, &mut clipped);
    let gain = 10f32.powf(15.0 / 20.0);
    for (c, p) in clipped.iter().zip(&plain) {
        assert!(
            (c - (p * gain).clamp(-1.0, 1.0)).abs() < 2e-3,
            "{c} against {p}"
        );
    }
    // A low-pass leaves the low harmonics and cuts the high ones.
    shapers.slots[1] = Slot {
        mode: Mode::LowPass,
        amount: 70.0,
        character: 0.0,
    };
    let mut filtered = vec![0.0; 1024];
    shapers.preview(&table, 0, &mut filtered);
    let (a, b) = (harmonics(&filtered, 64), harmonics(&plain, 64));
    assert!(
        (a[0] / b[0] - 1.0).abs() < 0.05,
        "{} against {}",
        a[0],
        b[0]
    );
    assert!(a[63] / b[63] < 0.05);
    // A live note's steady cycle matches the preview of a phase mode.
    let sine_table = share(&[sine()]);
    shapers.slots[1] = Slot::default();
    shapers.slots[0] = Slot {
        mode: Mode::Mirror,
        amount: 40.0,
        character: 0.0,
    };
    let mut preview = vec![0.0; 1024];
    shapers.preview(&sine_table, 0, &mut preview);
    let key = 33.0; // 55 Hz: 872.7 frames a cycle
    let audio = one_note(
        &sine_table,
        key,
        [(Mode::Mirror, 40.0), (Mode::Off, 0.0)],
        48_000,
    );
    let f0 = key_frequency(f64::from(key));
    let (mut error, mut energy) = (0.0f64, 0.0f64);
    for (k, &x) in audio.iter().enumerate().skip(40_000) {
        let at = (k as f64 * f0 / 48_000.0).fract() * 1024.0;
        let (i, t) = (at as usize, at.fract() as f32);
        let want = preview[i] + (preview[(i + 1) % 1024] - preview[i]) * t;
        error += f64::from(x - want).powi(2);
        energy += f64::from(x).powi(2);
    }
    println!(
        "live note against the preview: {:.1} dB",
        db(error / energy)
    );
    assert!(db(error / energy) < -20.0);
}

#[test]
fn rendering_stays_off_the_heap() {
    let table = share(&[saw(), sine()]);
    let mut osc = shaped(table, 32, 4, 48_000.0, 256);
    let keys: Vec<f32> = (0..32).map(|v| 30.0 + v as f32 * 2.5).collect();
    start_all(&mut osc, &keys);
    let pitch = pitches(&osc, &keys);
    let n = osc.osc().lane_count();
    let mods = vec![5.0; 256 * n];
    let pitch_mod = vec![0.1; 256 * n];
    let mut out = vec![0.0; 256 * n];
    let _ = simd::level();
    for slots in BUSY {
        for (i, (mode, amount)) in slots.into_iter().enumerate() {
            set(&mut osc, i, mode, amount);
        }
        let inputs = Inputs {
            lanes: 128,
            pitch: &pitch,
            pitch_mod: Some(&pitch_mod),
            ..Inputs::default()
        };
        let shaper_inputs = ShaperInputs {
            amount_mod: [Some(&mods), Some(&mods)],
        };
        lspk_rt::rt_test!({
            osc.process(256, &inputs, &shaper_inputs, &mut out);
            osc.process_scalar(100, &inputs, &shaper_inputs, &mut out);
            osc.shapers_mut().swap();
            osc.start(3, 60.0, Onset::Fresh);
        });
    }
    assert!(out.iter().any(|&x| x != 0.0));
}
