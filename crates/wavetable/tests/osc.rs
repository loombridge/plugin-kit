//! The wavetable oscillator: pitch, level, frames, phase and its modes,
//! aliasing at every key and frame, mipmap changes without clicks, the
//! vector paths against the scalar one, the shaper hooks, and memory.

use std::f64::consts::TAU;

use lspk_dsp::simd::{self, F32s, Level as SimdLevel, U32s};
use lspk_test::measure::{Pitch, aliasing};
use lspk_wavetable::osc::{
    Inputs, Layout, Onset, Oscillator, Params, PhaseMode, PhaseStage, WaveStage,
};
use lspk_wavetable::{CYCLE, Cache, FRAME_SIZE, MAX_FRAMES, SharedTable, Table, Wavetable};

lspk_rt::install_trap!();

/// The aliasing floor for a saw or a square, as `lspk-wavetable`'s own
/// test sets it: everything away from the harmonics this far below them.
const SAW_FLOOR_DB: f64 = -85.0;

/// The floor for the worst case, every harmonic at the same level.
const FLAT_FLOOR_DB: f64 = -65.0;

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

fn square() -> Vec<f32> {
    frame(|h| if h % 2 == 1 { 0.5 / h as f64 } else { 0.0 })
}

fn flat() -> Vec<f32> {
    frame(|_| 1.0 / 64.0)
}

fn sine(harmonic: usize) -> Vec<f32> {
    frame(|h| if h == harmonic { 1.0 } else { 0.0 })
}

fn share(frames: &[Vec<f32>]) -> SharedTable {
    Cache::global().share(Wavetable::new(frames).unwrap())
}

/// An oscillator of `voices` voices with `copies` lanes each, phases
/// starting at 0 unless a test says otherwise.
fn oscillator(
    table: SharedTable,
    voices: usize,
    copies: usize,
    rate: f32,
    max: usize,
) -> Oscillator {
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
    osc
}

/// Starts every voice of `osc` at the keys `keys` (one per voice).
fn start_all(osc: &mut Oscillator, keys: &[f32], onset: Onset) {
    for (voice, &key) in keys.iter().enumerate() {
        osc.start(voice, key, onset);
    }
}

/// Each lane's pitch, from its voice's key.
fn pitches(osc: &Oscillator, keys: &[f32]) -> Vec<f32> {
    let mut pitch = vec![0.0; osc.lane_count()];
    for (voice, &key) in keys.iter().enumerate() {
        for lane in osc.lanes_of(voice) {
            pitch[lane] = key;
        }
    }
    pitch
}

/// Renders `frames` frames in blocks of the oscillator's largest, with
/// `pitch` per lane held, at SIMD level `level` (or scalar with `None`).
fn render(
    osc: &mut Oscillator,
    pitch: &[f32],
    frames: usize,
    level: Option<SimdLevel>,
) -> Vec<f32> {
    let n = osc.lane_count();
    let lanes = osc.layout().voices * osc.layout().lanes_per_voice;
    let mut out = vec![0.0; frames * n];
    let mut done = 0;
    while done < frames {
        let len = (frames - done).min(osc.max_frames());
        let inputs = Inputs {
            lanes,
            pitch,
            ..Inputs::default()
        };
        let block = &mut out[done * n..(done + len) * n];
        match level {
            Some(level) => osc.process_at(level, len, &inputs, block),
            None => osc.process_scalar(len, &inputs, block),
        }
        done += len;
    }
    out
}

/// Lane `lane` of a frame-by-frame render.
fn lane(out: &[f32], lane: usize, lane_count: usize) -> Vec<f32> {
    out.iter().skip(lane).step_by(lane_count).copied().collect()
}

fn key_frequency(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

/// Lane `lane`'s frequency, in hertz, from how far its phase moves in the
/// last of `frames` frames.
fn frequency(osc: &mut Oscillator, pitch: &[f32], lane: usize, frames: usize) -> f64 {
    render(osc, pitch, frames - 1, Some(simd::level()));
    let before = osc.phase(lane);
    render(osc, pitch, 1, Some(simd::level()));
    let moved = osc.phase(lane).wrapping_sub(before);
    f64::from(moved) * f64::from(osc.sample_rate()) / CYCLE
}

fn cents(got: f64, want: f64) -> f64 {
    1200.0 * (got / want).log2()
}

#[test]
fn pitch_is_the_note_plus_octave_semitone_fine_and_sweep() {
    // Spec: engine-core R1
    let mut osc = oscillator(share(&[sine(1)]), 1, 1, 48_000.0, 512);
    for (octave, semitone, fine, coarse, key) in [
        (0, 0, 0.0, 0.0, 69.0),
        (1, 0, 0.0, 0.0, 60.0),
        (-4, 12, 0.0, 0.0, 45.0),
        (0, -7, 37.0, 0.0, 69.0),
        (2, 3, -100.0, 12.5, 30.0),
        (0, 0, 0.0, -64.0, 100.0),
    ] {
        let p = osc.params_mut();
        (p.octave, p.semitone, p.fine, p.coarse) = (octave, semitone, fine, coarse);
        osc.start(0, key as f32, Onset::Fresh);
        let pitch = pitches(&osc, &[key as f32]);
        let got = frequency(&mut osc, &pitch, 0, 400);
        let want = key_frequency(
            key + 12.0 * f64::from(octave)
                + f64::from(semitone)
                + f64::from(fine) / 100.0
                + f64::from(coarse),
        );
        let off = cents(got, want);
        assert!(
            off.abs() < 0.01,
            "{octave} {semitone} {fine} {coarse} at {key}: {off} cents"
        );
    }

    // And the audio has that pitch: A4 an octave up, measured from the
    // signal.
    let p = osc.params_mut();
    (p.octave, p.semitone, p.fine, p.coarse) = (1, 0, 0.0, 0.0);
    let pitch = pitches(&osc, &[69.0]);
    let audio = lane(
        &render(&mut osc, &pitch, 48_000, Some(simd::level())),
        0,
        osc.lane_count(),
    );
    let measured = lspk_test::measure::pitch(&audio, 48_000.0).unwrap();
    assert!((measured - 880.0).abs() < 0.01, "{measured} Hz");
}

#[test]
fn reference_pitch_sets_a4() {
    // Spec: engine-core R136
    let mut osc = oscillator(share(&[sine(1)]), 1, 1, 48_000.0, 512);
    osc.params_mut().reference = 432.0;
    let pitch = pitches(&osc, &[69.0]);
    let got = frequency(&mut osc, &pitch, 0, 400);
    assert!(cents(got, 432.0).abs() < 0.01, "{got} Hz");
}

#[test]
fn without_key_tracking_it_plays_note_0_and_still_follows_bend() {
    // Spec: engine-core R5, R6
    let mut osc = oscillator(share(&[sine(1)]), 2, 1, 48_000.0, 512);
    osc.params_mut().key_track = false;
    start_all(&mut osc, &[48.0, 72.0], Onset::Fresh);
    // Voice 1 bent up two semitones.
    let pitch = pitches(&osc, &[48.0, 74.0]);
    let note0 = frequency(&mut osc, &pitch, 0, 4_000);
    assert!(cents(note0, 8.175_798_9).abs() < 0.01, "{note0} Hz");
    assert!((note0 - 8.18).abs() < 0.005);
    let bent = frequency(&mut osc, &pitch, 1, 4_000);
    assert!(cents(bent, key_frequency(2.0)).abs() < 0.01, "{bent} Hz");
}

#[test]
fn level_is_gain_n_squared() {
    // Spec: engine-core R7
    let peak_at = |level: f32| {
        let mut osc = oscillator(share(&[sine(1)]), 1, 1, 48_000.0, 512);
        osc.params_mut().level = level;
        let pitch = pitches(&osc, &[69.0]);
        let audio = lane(&render(&mut osc, &pitch, 4_800, None), 0, osc.lane_count());
        audio.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    };
    let full = peak_at(100.0);
    let db = |x: f32| 20.0 * f64::from(x / full).log10();
    assert!(
        (db(peak_at(75.0)) + 5.0).abs() < 0.01,
        "{}",
        db(peak_at(75.0))
    );
    assert!(
        (db(peak_at(25.0)) + 24.08).abs() < 0.01,
        "{}",
        db(peak_at(25.0))
    );
    assert_eq!(Params::default().level, 75.0);
}

/// A table whose frame `f` is a sine at harmonic `f + 1`.
fn harmonic_frames(count: usize) -> SharedTable {
    share(&(1..=count).map(sine).collect::<Vec<_>>())
}

/// Lane 0 of `osc` at A1, phase 0, against `want(phase in cycles)`.
fn assert_plays(osc: &mut Oscillator, want: impl Fn(f64) -> f64, what: &str) {
    osc.start(0, 33.0, Onset::Fresh);
    let pitch = pitches(osc, &[33.0]);
    let step = f64::from(lspk_wavetable::increment(key_frequency(33.0), 48_000.0)) / CYCLE;
    for level in [None, Some(simd::level())] {
        osc.start(0, 33.0, Onset::Fresh);
        let audio = lane(&render(osc, &pitch, 1_000, level), 0, osc.lane_count());
        for (k, &y) in audio.iter().enumerate() {
            let w = want(k as f64 * step);
            assert!(
                (f64::from(y) - w).abs() < 2e-3,
                "{what}, {level:?}, frame {k}: {y} against {w}"
            );
        }
    }
}

#[test]
fn frame_picks_the_nearest_frame_and_smooth_frames_interpolates() {
    // Spec: engine-core R14
    let mut osc = oscillator(harmonic_frames(4), 1, 1, 48_000.0, 512);
    let harmonic = |h: f64| move |x: f64| (h * x * TAU).sin();
    for (frame, h) in [
        (1.0, 1.0),
        (2.4, 2.0),
        (2.6, 3.0),
        (4.0, 4.0),
        (9.0, 4.0),
        (-3.0, 1.0),
    ] {
        osc.params_mut().frame = frame;
        assert_plays(&mut osc, harmonic(h), &format!("frame {frame}"));
    }
    osc.params_mut().smooth = true;
    osc.params_mut().frame = 2.25;
    assert_plays(
        &mut osc,
        |x| 0.75 * (2.0 * x * TAU).sin() + 0.25 * (3.0 * x * TAU).sin(),
        "smooth 2.25",
    );
    osc.params_mut().frame = 4.0;
    assert_plays(&mut osc, harmonic(4.0), "smooth at the last frame");
}

#[test]
fn a_full_table_plays_every_one_of_its_256_frames() {
    // Spec: engine-core R13, R14
    let frames: Vec<Vec<f32>> = (0..MAX_FRAMES)
        .map(|f| vec![f as f32 / 255.0; FRAME_SIZE])
        .collect();
    let mut osc = oscillator(share(&frames), 1, 1, 48_000.0, 64);
    for frame in [1.0, 2.0, 128.0, 255.6, 256.0] {
        osc.params_mut().frame = frame;
        osc.start(0, 60.0, Onset::Fresh);
        let pitch = pitches(&osc, &[60.0]);
        let audio = render(&mut osc, &pitch, 64, Some(simd::level()));
        let want = (frame.round() - 1.0) / 255.0;
        assert!(
            (audio[0] - want).abs() < 1e-5,
            "frame {frame}: {}",
            audio[0]
        );
    }
}

#[test]
fn phase_sets_where_a_note_starts_in_the_frame() {
    // Spec: engine-core R16
    let mut osc = oscillator(share(&[sine(1)]), 1, 1, 48_000.0, 64);
    assert_eq!(Params::default().phase, 180.0);
    for (degrees, first) in [(0.0, 0.0), (90.0, 1.0), (180.0, 0.0), (270.0, -1.0)] {
        osc.params_mut().phase = degrees;
        osc.start(0, 60.0, Onset::Fresh);
        assert_eq!(osc.phase(0), (f64::from(degrees) / 360.0 * CYCLE) as u32);
        let pitch = pitches(&osc, &[60.0]);
        let audio = render(&mut osc, &pitch, 8, Some(simd::level()));
        assert!((audio[0] - first).abs() < 1e-4, "{degrees}°: {}", audio[0]);
    }
}

/// Where each lane of a fresh oscillator starts, in cycles from Phase,
/// for `voices` voices of `copies` lanes, over four notes each.
fn start_offsets(params: Params, voices: usize, copies: usize) -> Vec<f64> {
    let mut osc = oscillator(share(&[sine(1)]), voices, copies, 48_000.0, 64);
    *osc.params_mut() = params;
    let keys = vec![60.0; voices];
    let from = f64::from(params.phase) / 360.0;
    let mut offsets = Vec::new();
    for _ in 0..4 {
        start_all(&mut osc, &keys, Onset::Fresh);
        offsets.extend(
            (0..voices * copies).map(|l| (f64::from(osc.phase(l)) / CYCLE - from).rem_euclid(1.0)),
        );
    }
    offsets
}

#[test]
fn phase_random_offsets_each_new_note_and_each_unison_copy() {
    // Spec: engine-core R17
    assert_eq!(Params::default().phase_random, 100.0);
    let params = |random| Params {
        phase: 45.0,
        phase_random: random,
        ..Params::default()
    };
    // Off: every lane starts at Phase.
    assert!(start_offsets(params(0.0), 4, 4).iter().all(|&o| o < 1e-6));
    // Full: fully random, each copy its own: spread over the whole cycle.
    let full = start_offsets(params(100.0), 32, 16);
    let mean = full.iter().sum::<f64>() / full.len() as f64;
    assert!((mean - 0.5).abs() < 0.03, "mean {mean}");
    for quarter in 0..4 {
        let share = full
            .iter()
            .filter(|&&o| (o * 4.0) as usize == quarter)
            .count() as f64
            / full.len() as f64;
        assert!((share - 0.25).abs() < 0.04, "quarter {quarter}: {share}");
    }
    let copies = &full[..16];
    for (i, a) in copies.iter().enumerate() {
        for b in &copies[i + 1..] {
            assert!((a - b).abs() > 1e-6, "two copies start together");
        }
    }
    // Half: within half a cycle of Phase.
    let half = start_offsets(params(50.0), 32, 4);
    assert!(half.iter().all(|&o| o < 0.5));
    assert!(half.iter().any(|&o| o > 0.4));
    // A second note on the same voice draws new offsets.
    let mut osc = oscillator(share(&[sine(1)]), 1, 1, 48_000.0, 64);
    osc.params_mut().phase_random = 100.0;
    osc.start(0, 60.0, Onset::Fresh);
    let first = osc.phase(0);
    osc.start(0, 60.0, Onset::Fresh);
    assert_ne!(osc.phase(0), first);
}

#[test]
fn steals_carry_on_joins_restart_and_phase_legato_inverts_both() {
    // Spec: engine-core R18
    let mut osc = oscillator(share(&[sine(1)]), 1, 1, 48_000.0, 64);
    osc.params_mut().phase = 90.0;
    let restart = phase_after(&mut osc, Onset::Fresh);
    assert_eq!(restart, (0.25 * CYCLE) as u32);
    for (inverted, onset, restarts) in [
        (false, Onset::Steal, false),
        (false, Onset::Retrigger, false),
        (false, Onset::Join, true),
        (false, Onset::Legato, false),
        (true, Onset::Steal, true),
        (true, Onset::Retrigger, true),
        (true, Onset::Join, false),
        (true, Onset::Legato, false),
    ] {
        osc.params_mut().phase_legato = inverted;
        let after = phase_after(&mut osc, onset);
        assert_eq!(
            after == restart,
            restarts,
            "{onset:?}, phase legato {inverted}"
        );
        assert_eq!(onset.restarts(inverted), restarts);
    }
}

/// Plays a few frames, then starts voice 0 with `onset`, and returns its
/// phase.
fn phase_after(osc: &mut Oscillator, onset: Onset) -> u32 {
    let pitch = pitches(osc, &[61.0]);
    render(osc, &pitch, 37, Some(simd::level()));
    osc.start(0, 62.0, onset);
    osc.phase(0)
}

#[test]
fn the_phase_modes_restart_carry_on_or_start_from_each_copy_s_stored_phase() {
    // Spec: engine-core R19
    assert_eq!(Params::default().phase_mode, PhaseMode::NoteStart);
    let mut osc = oscillator(share(&[sine(1)]), 2, 3, 48_000.0, 64);
    let pitch = pitches(&osc, &[60.0, 64.0]);
    osc.params_mut().phase = 90.0;

    // Note start: from Phase.
    start_all(&mut osc, &[60.0, 64.0], Onset::Fresh);
    render(&mut osc, &pitch, 50, Some(simd::level()));
    osc.start(0, 60.0, Onset::Fresh);
    assert!((0..3).all(|l| osc.phase(l) == (0.25 * CYCLE) as u32));

    // Continue: from where the lane's last note left it.
    osc.params_mut().phase_mode = PhaseMode::Continue;
    render(&mut osc, &pitch, 50, Some(simd::level()));
    let running: Vec<u32> = (0..3).map(|l| osc.phase(l)).collect();
    osc.start(0, 67.0, Onset::Fresh);
    assert_eq!((0..3).map(|l| osc.phase(l)).collect::<Vec<_>>(), running);

    // Per voice: each copy from its own stored phase, 0° until edited.
    osc.params_mut().phase_mode = PhaseMode::PerVoice;
    osc.start(1, 64.0, Onset::Fresh);
    assert!(osc.lanes_of(1).all(|l| osc.phase(l) == 0));
    osc.voice_phases_mut()[..3].copy_from_slice(&[0.0, 90.0, 270.0]);
    osc.start(1, 64.0, Onset::Fresh);
    let want = [0, (0.25 * CYCLE) as u32, (0.75 * CYCLE) as u32];
    for (lane, want) in osc.lanes_of(1).zip(want) {
        assert_eq!(osc.phase(lane), want);
    }

    // Choosing it while a note sounds captures that note's phases.
    render(&mut osc, &pitch, 50, Some(simd::level()));
    let sounding: Vec<u32> = osc.lanes_of(0).map(|l| osc.phase(l)).collect();
    osc.capture_phases(0);
    osc.start(1, 64.0, Onset::Fresh);
    for (lane, want) in osc.lanes_of(1).zip(sounding) {
        let error = osc.phase(lane).wrapping_sub(want) as i32;
        assert!(error.unsigned_abs() < 1 << 12, "lane {lane}: {error}");
    }
}

#[test]
fn per_voice_phases_ignore_phase_and_its_randomness() {
    // Spec: engine-core R20
    let mut osc = oscillator(share(&[sine(1)]), 1, 4, 48_000.0, 64);
    let p = osc.params_mut();
    p.phase_mode = PhaseMode::PerVoice;
    p.phase = 123.0;
    p.phase_random = 100.0;
    osc.voice_phases_mut()[..4].copy_from_slice(&[0.0, 90.0, 180.0, 270.0]);
    for _ in 0..3 {
        osc.start(0, 60.0, Onset::Fresh);
        for (l, quarter) in (0..4).zip(0..4u32) {
            assert_eq!(osc.phase(l), quarter * (0.25 * CYCLE) as u32);
        }
    }
}

/// The worst aliasing among keys `keys`, at `rate`, frame `frame` of
/// `table`, and its key. Sixteen keys render at once, one per lane.
fn worst_aliasing(
    table: &SharedTable,
    rate: f32,
    frame: f32,
    smooth: bool,
    keys: &[u8],
) -> (f64, u8) {
    let mut worst = (f64::NEG_INFINITY, 0);
    for batch in keys.chunks(16) {
        let mut osc = oscillator(table.clone(), 16, 1, rate, 512);
        let p = osc.params_mut();
        p.frame = frame;
        p.smooth = smooth;
        let keys: Vec<f32> = batch.iter().map(|&k| f32::from(k)).collect();
        start_all(&mut osc, &keys, Onset::Fresh);
        let pitch = pitches(&osc, &keys);
        let out = render(&mut osc, &pitch, 8_192, Some(simd::level()));
        for (l, &key) in batch.iter().enumerate() {
            let audio = lane(&out, l, osc.lane_count());
            let f = key_frequency(f64::from(key));
            let ratio = aliasing(&audio, f64::from(rate), Pitch::Steady(f)).ratio_db();
            if ratio > worst.0 {
                worst = (ratio, key);
            }
        }
    }
    worst
}

#[test]
fn no_aliasing_above_the_floor_at_any_key_frame_or_rate() {
    // Spec: engine-core R15
    // Frames 1 to 3 are a saw, a square and a flat spectrum.
    let table = share(&[saw(), square(), flat()]);
    let every_key: Vec<u8> = (0..=127).collect();
    let some_keys: Vec<u8> = (0..=127).step_by(5).chain([127]).collect();
    for (frame, smooth, floor) in [
        (1.0, false, SAW_FLOOR_DB),
        (2.0, false, SAW_FLOOR_DB),
        (1.5, true, SAW_FLOOR_DB),
        (3.0, false, FLAT_FLOOR_DB),
        (2.7, true, FLAT_FLOOR_DB),
    ] {
        for (rate, keys) in [
            (44_100.0, &every_key),
            (48_000.0, &every_key),
            (96_000.0, &some_keys),
            (192_000.0, &some_keys),
        ] {
            let (worst, key) = worst_aliasing(&table, rate, frame, smooth, keys);
            println!("frame {frame} smooth {smooth} at {rate} Hz: worst {worst:.1} dB, key {key}");
            assert!(
                worst < floor,
                "frame {frame} at {rate} Hz, key {key}: {worst:.1} dB"
            );
        }
    }
}

/// The energy of `signal` between `low` and `high` hertz against all of
/// it, in dB, from Hann-windowed spectra of 8,192 frames overlapping by
/// half.
fn band_db(signal: &[f32], rate: f64, low: f64, high: f64) -> f64 {
    const N: usize = 8_192;
    let fft = realfft::RealFftPlanner::<f64>::new().plan_fft_forward(N);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let (mut band, mut total) = (0.0, 0.0);
    for start in (0..=signal.len() - N).step_by(N / 2) {
        for (n, (slot, &x)) in input.iter_mut().zip(&signal[start..start + N]).enumerate() {
            let w = 0.5 - 0.5 * (TAU * n as f64 / N as f64).cos();
            *slot = f64::from(x) * w;
        }
        fft.process(&mut input, &mut spectrum).unwrap();
        for (k, bin) in spectrum.iter().enumerate() {
            let f = k as f64 * rate / N as f64;
            total += bin.norm_sqr();
            if (low..high).contains(&f) {
                band += bin.norm_sqr();
            }
        }
    }
    10.0 * (band / total).log10()
}

#[test]
fn mipmap_levels_change_without_clicks_under_fast_pitch_modulation() {
    // Spec: engine-core R15
    // A fundamental and harmonic 600, which the 768-harmonic level keeps
    // and the 512-harmonic level drops. Around 31.25 Hz at 48 kHz the
    // level changes; a 15 Hz vibrato of half a semitone crosses that
    // pitch 30 times a second, switching harmonic 600 (near 18.75 kHz) in
    // and out. Switched in one sample, each change is a click, broadband;
    // crossfaded, its sidebands stay close to it. Between 2 and 12 kHz
    // there is nothing else.
    let rate = 48_000.0;
    let table = share(&[frame(|h| match h {
        1 => 1.0,
        600 => 0.5,
        _ => 0.0,
    })]);
    let centre = (69.0 + 12.0 * (31.25f64 / 440.0).log2()) as f32;
    let frames = 96_000;
    let vibrato = |k: usize| 0.5 * (TAU * 15.0 * k as f64 / rate).sin();

    let mut osc = oscillator(table.clone(), 1, 1, rate as f32, 256);
    osc.start(0, centre, Onset::Fresh);
    let n = osc.lane_count();
    let pitch = pitches(&osc, &[centre]);
    let mut smooth = Vec::with_capacity(frames);
    let mut modulation = vec![0.0; 256 * n];
    let mut out = vec![0.0; 256 * n];
    for block in 0..frames / 256 {
        for k in 0..256 {
            modulation[k * n] = vibrato(block * 256 + k) as f32;
        }
        let inputs = Inputs {
            lanes: 1,
            pitch: &pitch,
            pitch_mod: Some(&modulation),
            ..Inputs::default()
        };
        osc.process(256, &inputs, &mut out);
        smooth.extend(out.iter().step_by(n));
    }

    // The same note, switching level in one sample.
    let mut abrupt = Vec::with_capacity(frames);
    let mut phase = 0u32;
    for k in 0..frames {
        let f = key_frequency(f64::from(centre) + vibrato(k));
        let step = lspk_wavetable::increment(f, rate);
        abrupt.push(table.wave(table.level_for(step), 0).read(phase));
        phase = phase.wrapping_add(step);
    }

    let clicks = band_db(&abrupt, rate, 2_000.0, 12_000.0);
    let faded = band_db(&smooth, rate, 2_000.0, 12_000.0);
    println!("between 2 and 12 kHz: switched {clicks:.1} dB, crossfaded {faded:.1} dB");
    assert!(
        clicks > -75.0,
        "the test should hear switching: {clicks:.1} dB"
    );
    assert!(faded < -95.0, "{faded:.1} dB");

    // Both kept harmonic 600 while the pitch allowed it.
    let upper = band_db(&smooth, rate, 17_000.0, 20_000.0);
    assert!(upper > -12.0, "{upper:.1} dB");
}

#[test]
fn a_rising_glide_never_reads_a_level_above_nyquist() {
    // Spec: engine-core R15
    // A glide from key 20 to 120 over half a second, through the levels
    // one after another: every frame's level is the one its own pitch
    // allows or one with fewer harmonics, so nothing folds.
    let table = share(&[flat()]);
    let rate = 48_000.0;
    let mut osc = oscillator(table.clone(), 1, 1, rate as f32, 512);
    osc.start(0, 20.0, Onset::Fresh);
    let n = osc.lane_count();
    let frames = 24_000;
    let step = vec![100.0 / frames as f32; n];
    let mut pitch = pitches(&osc, &[20.0]);
    let mut audio = Vec::new();
    let mut out = vec![0.0; 512 * n];
    for _ in 0..frames / 512 {
        let inputs = Inputs {
            lanes: 1,
            pitch: &pitch,
            pitch_step: Some(&step),
            ..Inputs::default()
        };
        osc.process(512, &inputs, &mut out);
        audio.extend(out.iter().step_by(n));
        pitch[0] += 512.0 * step[0];
    }
    // The ideal: each frame read at the level its own pitch allows.
    let mut ideal = Vec::new();
    let mut phase = 0u32;
    for k in 0..audio.len() {
        let f = key_frequency(20.0 + f64::from(step[0]) * k as f64);
        let inc = lspk_wavetable::increment(f, rate);
        ideal.push(table.wave(table.level_for(inc), 0).read(phase));
        phase = phase.wrapping_add(inc);
    }
    // Energy above the ideal's: the oscillator keeps no more harmonics
    // than the ideal does, so nothing above 2/3 Nyquist that the ideal
    // lacks. Compare the top band of each.
    let top = |x: &[f32]| band_db(x, rate, 23_000.0, 24_000.0);
    assert!(
        top(&audio) <= top(&ideal) + 0.5,
        "{} against {}",
        top(&audio),
        top(&ideal)
    );
}

/// A render of `osc` with pitch and frame modulation and Smooth frames,
/// at `level` or scalar.
fn busy_render(level: Option<SimdLevel>, smooth: bool) -> (Vec<f32>, usize) {
    let table = share(&[saw(), square(), flat(), sine(3)]);
    let mut osc = oscillator(table, 5, 3, 48_000.0, 300);
    let p = osc.params_mut();
    p.smooth = smooth;
    p.frame = 1.7;
    p.fine = 13.0;
    p.phase_random = 100.0;
    let keys = [20.0, 45.5, 69.0, 96.0, 118.0];
    start_all(&mut osc, &keys, Onset::Fresh);
    for (l, offset) in osc.pitch_offsets_mut().iter_mut().enumerate() {
        *offset = (l % 3) as f32 * 0.1;
    }
    for (l, offset) in osc.frame_offsets_mut().iter_mut().enumerate() {
        *offset = (l % 3) as f32 * 0.4;
    }
    let n = osc.lane_count();
    let pitch = pitches(&osc, &keys);
    let step: Vec<f32> = (0..n).map(|l| (l as f32 - 7.0) * 0.001).collect();
    let mut out = Vec::new();
    for block in 0..4 {
        let frames = [300, 17, 256, 1][block];
        let pitch_mod: Vec<f32> = (0..frames * n)
            .map(|i| 3.0 * ((i / n + block * 300) as f32 * 0.003 + (i % n) as f32).sin())
            .collect();
        let frame_mod: Vec<f32> = (0..frames * n)
            .map(|i| 1.5 + 1.5 * ((i / n) as f32 * 0.01 + (i % n) as f32).sin())
            .collect();
        let inputs = Inputs {
            lanes: 15,
            pitch: &pitch,
            pitch_step: Some(&step),
            pitch_mod: Some(&pitch_mod),
            frame_mod: Some(&frame_mod),
        };
        let mut block_out = vec![0.0; frames * n];
        match level {
            Some(level) => osc.process_at(level, frames, &inputs, &mut block_out),
            None => osc.process_scalar(frames, &inputs, &mut block_out),
        }
        out.extend(block_out);
    }
    (out, n)
}

#[test]
fn every_simd_level_matches_the_scalar_path() {
    // Spec: engine-core R1, R14, R15
    for smooth in [false, true] {
        let (reference, n) = busy_render(None, smooth);
        let mut ran = Vec::new();
        for level in SimdLevel::ALL.into_iter().filter(|l| l.is_supported()) {
            ran.push(level.name());
            let (got, _) = busy_render(Some(level), smooth);
            // The increments differ in their last bit where one path fuses
            // a multiply-add and the other doesn't, so the phases drift
            // apart by parts per million over a render.
            let mut squares = 0.0;
            for (i, (a, b)) in got.iter().zip(&reference).enumerate() {
                if i % n >= 15 {
                    continue;
                }
                squares += f64::from(a - b).powi(2);
                assert!(
                    (a - b).abs() < 1e-3,
                    "{level}, smooth {smooth}, frame {} lane {}: {a} against {b}",
                    i / n,
                    i % n
                );
            }
            let rms = (squares / (got.len() / n * 15) as f64).sqrt();
            assert!(rms < 5e-5, "{level}, smooth {smooth}: {rms:e} RMS");
        }
        println!("levels checked: {}", ran.join(", "));
    }
}

/// Adds half a cycle to every lane's phase.
struct HalfTurn;

impl PhaseStage for HalfTurn {
    #[inline(always)]
    fn shape<S: F32s>(&mut self, _: usize, _: usize, phase: S::U32s, _: S::U32s) -> S::U32s {
        phase.wrapping_add(<S::U32s>::splat(1 << 31))
    }

    fn shape_lane(&mut self, _: usize, _: usize, phase: u32, _: u32) -> u32 {
        phase.wrapping_add(1 << 31)
    }
}

/// Doubles lane 0 only, and counts its calls.
struct DoubleLaneZero(usize);

impl WaveStage for DoubleLaneZero {
    #[inline(always)]
    fn shape<S: F32s>(&mut self, lanes: usize, _: usize, value: S) -> S {
        self.0 += 1;
        let mut gain = [1.0f32; 16];
        if lanes == 0 {
            gain[0] = 2.0;
        }
        value * S::load(&gain)
    }

    fn shape_lane(&mut self, lane: usize, _: usize, value: f32) -> f32 {
        self.0 += 1;
        if lane == 0 { 2.0 * value } else { value }
    }
}

#[test]
fn the_shaper_hooks_bend_the_phase_before_the_read_and_the_value_after() {
    let mut plain = oscillator(share(&[sine(1)]), 2, 1, 48_000.0, 64);
    plain.params_mut().level = 50.0;
    let pitch = pitches(&plain, &[60.0, 60.0]);
    let inputs = Inputs {
        lanes: 2,
        pitch: &pitch,
        ..Inputs::default()
    };
    let n = plain.lane_count();
    let mut want = vec![0.0; 64 * n];
    plain.process(64, &inputs, &mut want);
    for level in [None, Some(simd::level())] {
        let mut osc = oscillator(share(&[sine(1)]), 2, 1, 48_000.0, 64);
        osc.params_mut().level = 50.0;
        let mut out = vec![0.0; 64 * n];
        let mut wave = DoubleLaneZero(0);
        match level {
            Some(level) => osc.process_with(level, 64, &inputs, &mut out, &mut HalfTurn, &mut wave),
            None => osc.process_scalar_with(64, &inputs, &mut out, &mut HalfTurn, &mut wave),
        }
        assert!(wave.0 > 0);
        for k in 0..64 {
            // Half a turn inverts a sine; the wave stage acts before Level.
            assert!(
                (out[k * n] + 2.0 * want[k * n]).abs() < 1e-4,
                "{level:?} frame {k}"
            );
            assert!(
                (out[k * n + 1] + want[k * n + 1]).abs() < 1e-4,
                "{level:?} frame {k}"
            );
        }
        // The stages don't move the running phase.
        assert_eq!(osc.phase(0), plain.phase(0));
    }
}

#[test]
fn rendering_stays_off_the_heap() {
    let table = share(&[saw(), flat()]);
    let mut osc = oscillator(table, 32, 4, 48_000.0, 256);
    let keys: Vec<f32> = (0..32).map(|v| 30.0 + v as f32 * 2.5).collect();
    start_all(&mut osc, &keys, Onset::Fresh);
    let pitch = pitches(&osc, &keys);
    let n = osc.lane_count();
    let modulation = vec![0.5; 256 * n];
    let mut out = vec![0.0; 256 * n];
    let _ = simd::level();
    lspk_rt::rt_test!({
        osc.params_mut().smooth = true;
        osc.start(3, 61.0, Onset::Steal);
        let inputs = Inputs {
            lanes: 128,
            pitch: &pitch,
            pitch_step: Some(&modulation[..n]),
            pitch_mod: Some(&modulation),
            frame_mod: Some(&modulation),
        };
        osc.process(256, &inputs, &mut out);
        osc.process_scalar(256, &inputs, &mut out);
    });
    assert!(out.iter().any(|&x| x != 0.0));
}

#[test]
fn instances_share_one_table_and_hold_little_of_their_own() {
    // A full table of 256 frames: what each instance would otherwise hold.
    let frames: Vec<Vec<f32>> = (0..MAX_FRAMES)
        .map(|f| frame(|h| if h <= 1 + f { 1.0 / h as f64 } else { 0.0 }))
        .collect();
    let source = Wavetable::new(&frames).unwrap();
    let cache = Cache::new();
    let instances: Vec<Oscillator> = (0..16)
        .map(|i| {
            Oscillator::new(
                cache.share(source.clone()),
                Layout {
                    voices: 32,
                    lanes_per_voice: 1,
                },
                48_000.0,
                1_024,
                i,
            )
        })
        .collect();
    assert_eq!(cache.len(), 1);
    assert!(
        instances
            .iter()
            .all(|o| o.table().ptr_eq(instances[0].table()))
    );
    let table = instances[0].table().bytes();
    let own = instances[0].bytes();
    let mb = |b: usize| b as f64 / 1e6;
    println!(
        "table {:.1} MB shared; per instance {:.1} KB; 1 instance {:.2} MB, 16 instances {:.2} MB",
        mb(table),
        own as f64 / 1e3,
        mb(table + own),
        mb(table + 16 * own)
    );
    // Sixteen instances cost less than a table and a half.
    assert!(table + 16 * own < table * 3 / 2);
    let _ = Table::build;
}
