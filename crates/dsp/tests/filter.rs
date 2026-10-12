//! The filters, measured: the Normal group's responses against their
//! analogue prototypes (slopes, cutoff, resonance peak) and
//! self-oscillation; for every type in every group, stability across
//! every setting and sample rate, block-size invariance, SIMD against
//! scalar, cutoff sample by sample and no allocation; and the controls'
//! behaviour. The other groups' responses are in `filter_groups.rs`.
//!
//! Spec rules are cited as `engine-core Rn`.

use std::f64::consts::{PI, TAU};

use lspk_dsp::Oversampler;
use lspk_dsp::filter::{FilterBank, FilterGroup, FilterType, VoiceParams, hertz_of, pitch_of};
use lspk_dsp::simd::Level;
use lspk_test::measure::{self, Pitch};
use lspk_test::signal;

lspk_rt::install_trap!();

const RATE: f32 = 48_000.0;

/// A complex number, for the prototypes.
#[derive(Clone, Copy, Debug)]
struct C(f64, f64);

impl C {
    fn abs(self) -> f64 {
        self.0.hypot(self.1)
    }
}

impl std::ops::Add for C {
    type Output = C;
    fn add(self, o: C) -> C {
        C(self.0 + o.0, self.1 + o.1)
    }
}

impl std::ops::Sub for C {
    type Output = C;
    fn sub(self, o: C) -> C {
        C(self.0 - o.0, self.1 - o.1)
    }
}

impl std::ops::Mul for C {
    type Output = C;
    fn mul(self, o: C) -> C {
        C(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
    }
}

impl std::ops::Div for C {
    type Output = C;
    fn div(self, o: C) -> C {
        let d = o.0 * o.0 + o.1 * o.1;
        C(
            (self.0 * o.0 + self.1 * o.1) / d,
            (self.1 * o.0 - self.0 * o.1) / d,
        )
    }
}

fn re(x: f64) -> C {
    C(x, 0.0)
}

/// The analogue prototype of `kind` at `resonance`, at `s` (normalised so
/// the cutoff is 1): the responses the module documentation describes,
/// written out independently here.
fn prototype(kind: FilterType, resonance: f32, s: C) -> C {
    let k = f64::from(kind.feedback(resonance));
    let one = re(1.0);
    let den = |k: f64| s * s + re(k) * s + one;
    let lp1 = one / (one + s);
    let hp1 = s / (one + s);
    let lp2 = |k: f64| one / den(k);
    let bp2 = |k: f64| s / den(k);
    let hp2 = |k: f64| s * s / den(k);
    // A fourth-order Butterworth's fixed stage: 2 sin(3π/8).
    let k1 = 2.0 * (3.0 * PI / 8.0).sin();
    match kind {
        FilterType::Lp6 => lp1,
        FilterType::Hp6 => hp1,
        FilterType::Lp12 => lp2(k),
        FilterType::Hp12 => hp2(k),
        FilterType::Lp18 => lp1 * lp2(k),
        FilterType::Hp18 => hp1 * hp2(k),
        FilterType::Lp24 => lp2(k1) * lp2(k),
        FilterType::Hp24 => hp2(k1) * hp2(k),
        FilterType::Bp12 => bp2(k),
        FilterType::Bp24 => bp2(1.0) * bp2(k),
        FilterType::Peak12 => one + re(1.0 - k / 2.0) * bp2(k),
        FilterType::Peak24 => one + re(1.0 - k / 2.0) * bp2(k) * bp2(1.0),
        FilterType::Notch12 => one - re(k) * bp2(k),
        FilterType::Notch24 => (one - re(k) * bp2(k)) * (one - re(k) * bp2(k)),
        FilterType::Ladder6
        | FilterType::Ladder12
        | FilterType::Ladder18
        | FilterType::Ladder24 => {
            let n = kind.slope() / 6;
            let mut g = one;
            for _ in 0..n {
                g = g * lp1;
            }
            let g4 = lp1 * lp1 * lp1 * lp1;
            re(1.0 + 0.5 * k) * g / (one + re(k) * g4)
        }
        _ => unreachable!("the Normal group's prototypes only"),
    }
}

/// The digital filter's prototype response at `hz`: the prototype on the
/// prewarped axis.
fn expected(kind: FilterType, resonance: f32, rate: f64, cutoff: f64, hz: f64) -> C {
    let omega = (PI * hz / rate).tan() / (PI * cutoff / rate).tan();
    prototype(kind, resonance, C(0.0, omega))
}

/// A one-voice mono scalar bank with `params`, snapped.
fn bank(kind: FilterType, rate: f32, params: &VoiceParams) -> FilterBank {
    let mut bank = FilterBank::scalar(rate, 1, 1);
    bank.set_type(kind);
    bank.reset_voice(0, params);
    bank
}

/// `kind`'s impulse response at `resonance`, cutoff `cutoff` hertz:
/// `frames` frames, from an impulse small enough that the saturator does
/// nothing.
fn impulse_response(
    kind: FilterType,
    resonance: f32,
    rate: f32,
    cutoff: f32,
    frames: usize,
) -> Vec<f64> {
    const AMPLITUDE: f32 = 1e-3;
    let params = VoiceParams {
        resonance,
        ..VoiceParams::default()
    };
    let mut bank = bank(kind, rate, &params);
    let pitch = vec![pitch_of(cutoff, 440.0); frames];
    let mut audio = signal::impulse(frames, 0, AMPLITUDE);
    bank.process(&pitch, &mut audio);
    audio
        .iter()
        .map(|&y| f64::from(y) / f64::from(AMPLITUDE))
        .collect()
}

/// The response at `hz` of the system with impulse response `ir`.
fn dft(ir: &[f64], rate: f64, hz: f64) -> C {
    let step = C((TAU * hz / rate).cos(), -(TAU * hz / rate).sin());
    let mut rot = re(1.0);
    let mut sum = re(0.0);
    for &x in ir {
        sum = sum + re(x) * rot;
        rot = rot * step;
    }
    sum
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

// Spec: engine-core R50, R61 (the normal state-variable and ladder rows).
#[test]
fn every_type_matches_its_analogue_prototype() {
    for (rate, cutoff) in [(48_000.0, 1_000.0), (192_000.0, 9_000.0), (44_100.0, 120.0)] {
        let frequencies = measure::log_frequencies(20.0, 0.45 * f64::from(rate), 36);
        for kind in FilterType::NORMAL {
            for resonance in [0.0, 0.5, 0.9] {
                let ir = impulse_response(kind, resonance, rate, cutoff, 1 << 15);
                for &hz in &frequencies {
                    let want = expected(kind, resonance, f64::from(rate), f64::from(cutoff), hz);
                    let got = dft(&ir, f64::from(rate), hz);
                    let error = (got - want).abs();
                    assert!(
                        error <= 2e-3 * want.abs() + 3e-5,
                        "{kind} at {rate} Hz, cutoff {cutoff}, resonance {resonance}, {hz:.0} Hz: \
                         {:.3} dB, prototype {:.3} dB",
                        db(got.abs()),
                        db(want.abs())
                    );
                }
            }
        }
    }
}

// Spec: engine-core R50, R61: slopes and cutoff accuracy, measured.
#[test]
fn slopes_and_cutoffs_are_as_the_names_say() {
    let rate = 48_000.0;
    let gain = |kind, cutoff: f32, hz: f64| {
        let ir = impulse_response(kind, 0.0, rate, cutoff, 1 << 15);
        db(dft(&ir, f64::from(rate), hz).abs())
    };
    // Slopes are per octave away from the pass band.
    eprintln!("type        at cutoff   slope (dB/oct)");
    for kind in FilterType::NORMAL {
        let n = f64::from(kind.slope());
        let (at_cutoff, slope) = match kind {
            // Low-pass slopes three to four octaves above a 100 Hz cutoff.
            FilterType::Lp6
            | FilterType::Lp12
            | FilterType::Lp18
            | FilterType::Lp24
            | FilterType::Ladder6
            | FilterType::Ladder12
            | FilterType::Ladder18
            | FilterType::Ladder24 => (
                gain(kind, 100.0, 100.0),
                gain(kind, 100.0, 1_600.0) - gain(kind, 100.0, 800.0),
            ),
            // High-pass slopes three to four octaves below 8 kHz.
            FilterType::Hp6 | FilterType::Hp12 | FilterType::Hp18 | FilterType::Hp24 => (
                gain(kind, 8_000.0, 8_000.0),
                gain(kind, 8_000.0, 500.0) - gain(kind, 8_000.0, 1_000.0),
            ),
            // Band-pass skirts: half the slope each side.
            FilterType::Bp12 | FilterType::Bp24 => (
                gain(kind, 1_000.0, 1_000.0),
                gain(kind, 1_000.0, 62.5) - gain(kind, 1_000.0, 125.0),
            ),
            _ => (gain(kind, 1_000.0, 1_000.0), 0.0),
        };
        eprintln!("{:<11} {at_cutoff:>7.2} dB {slope:>8.2}", kind.name());
        match kind {
            // Butterworth at resonance 0: 3 dB down at the cutoff.
            FilterType::Lp6
            | FilterType::Lp12
            | FilterType::Lp18
            | FilterType::Lp24
            | FilterType::Hp6
            | FilterType::Hp12
            | FilterType::Hp18
            | FilterType::Hp24 => {
                assert!((at_cutoff + 3.01).abs() < 0.02, "{kind}: {at_cutoff}");
                assert!((slope + n).abs() < 0.3, "{kind}: {slope}");
            }
            // Without feedback, each one-pole stage is 3 dB down there.
            FilterType::Ladder6
            | FilterType::Ladder12
            | FilterType::Ladder18
            | FilterType::Ladder24 => {
                assert!(
                    (at_cutoff + 3.01 * n / 6.0).abs() < 0.02,
                    "{kind}: {at_cutoff}"
                );
                assert!((slope + n).abs() < 0.3, "{kind}: {slope}");
            }
            // Unity at the centre.
            FilterType::Bp12 | FilterType::Bp24 => {
                assert!(at_cutoff.abs() < 0.02, "{kind}: {at_cutoff}");
                assert!((slope + n / 2.0).abs() < 0.3, "{kind}: {slope}");
            }
            // Flat at resonance 0.
            FilterType::Peak12 | FilterType::Peak24 => {
                assert!(at_cutoff.abs() < 0.02, "{kind}: {at_cutoff}");
            }
            // A notch: nothing at the centre.
            FilterType::Notch12 | FilterType::Notch24 => {
                assert!(at_cutoff < -60.0, "{kind}: {at_cutoff}");
            }
            _ => unreachable!("the Normal group only"),
        }
    }
}

// Spec: engine-core R50, R61: the resonant peak's height and frequency.
#[test]
fn resonant_peaks_have_the_prototypes_height_and_frequency() {
    let (rate, cutoff) = (48_000.0f32, 1_000.0f32);
    let grid = measure::log_frequencies(500.0, 2_000.0, 401);
    eprintln!("type        resonance  peak (dB)  prototype  at (Hz)");
    for kind in FilterType::NORMAL
        .into_iter()
        .filter(|k| k.has_resonance() && *k != FilterType::Notch12 && *k != FilterType::Notch24)
    {
        for resonance in [0.7, 0.9, 0.95] {
            let ir = impulse_response(kind, resonance, rate, cutoff, 1 << 15);
            let (mut peak, mut at, mut want_peak, mut want_at) = (0.0f64, 0.0, 0.0f64, 0.0);
            for &hz in &grid {
                let got = dft(&ir, f64::from(rate), hz).abs();
                let want = expected(kind, resonance, f64::from(rate), f64::from(cutoff), hz).abs();
                if got > peak {
                    (peak, at) = (got, hz);
                }
                if want > want_peak {
                    (want_peak, want_at) = (want, hz);
                }
            }
            eprintln!(
                "{:<11} {resonance:>9.2} {:>10.2} {:>10.2} {at:>8.1}",
                kind.name(),
                db(peak),
                db(want_peak)
            );
            assert!(
                (db(peak) - db(want_peak)).abs() < 0.05,
                "{kind} {resonance}"
            );
            assert!(
                (at / want_at - 1.0).abs() < 0.005,
                "{kind} {resonance}: {at} {want_at}"
            );
        }
    }
}

// The resonance law's fixed points.
#[test]
fn the_resonance_law_starts_at_butterworth_and_oscillates_near_the_top() {
    assert!((FilterType::Lp12.feedback(0.0) - std::f32::consts::SQRT_2).abs() < 1e-6);
    assert_eq!(FilterType::Lp18.feedback(0.0), 1.0);
    assert_eq!(FilterType::Ladder24.feedback(0.0), 0.0);
    for kind in FilterType::NORMAL {
        let top = kind.feedback(1.0);
        if !kind.has_resonance() {
            assert_eq!(top, 0.0);
        } else if kind.self_oscillates() {
            // Past the edge of stability at the top, inside it at 95 %.
            let ladder = kind.name().starts_with("ladder");
            assert!(if ladder { top > 4.0 } else { top < 0.0 }, "{kind}");
            let near = kind.feedback(0.95);
            assert!(if ladder { near < 4.0 } else { near > 0.0 }, "{kind}");
        } else {
            assert_eq!(top, 1.0 / 16.0, "{kind}");
        }
    }
    // Peak reaches +24 dB.
    let ir = impulse_response(FilterType::Peak12, 1.0, RATE, 1_000.0, 1 << 15);
    let peak = db(dft(&ir, f64::from(RATE), 1_000.0).abs());
    assert!((peak - 24.3).abs() < 0.1, "{peak}");
}

/// `kind` at resonance 1, kicked by an impulse then left with silence:
/// the last half second.
fn ringing(kind: FilterType, rate: f32, cutoff: f32) -> Vec<f32> {
    let params = VoiceParams {
        resonance: 1.0,
        ..VoiceParams::default()
    };
    let mut bank = bank(kind, rate, &params);
    let frames = (rate * 2.0) as usize;
    let pitch = vec![pitch_of(cutoff, 440.0); frames];
    let mut audio = signal::impulse(frames, 0, 0.1);
    bank.process(&pitch, &mut audio);
    audio.split_off(frames - (rate * 0.5) as usize)
}

// Self-oscillation where the type allows it, at a steady level and at
// the cutoff.
#[test]
fn types_that_self_oscillate_do_so_steadily_at_the_cutoff() {
    for kind in FilterType::NORMAL {
        for (rate, cutoff) in [(48_000.0, 1_000.0), (96_000.0, 220.0)] {
            let tail = ringing(kind, rate, cutoff);
            let rms = measure::rms(&tail);
            if kind.self_oscillates() {
                let pitch = measure::pitch(&tail, f64::from(rate)).expect("periodic");
                eprintln!(
                    "{kind} at {rate} Hz: {pitch:.2} Hz, {:.1} dBFS RMS",
                    db(rms)
                );
                assert!(rms > 0.05 && measure::peak(&tail) < 4.0, "{kind}: {rms}");
                assert!(
                    (pitch / f64::from(cutoff) - 1.0).abs() < 0.01,
                    "{kind}: {pitch}"
                );
            } else {
                assert!(rms < 1e-6, "{kind}: {rms}");
            }
        }
    }
}

/// A deterministic pseudo-random sequence.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

// Stable at every cutoff, resonance and setting, from 44.1 kHz to 192 kHz
// oversampled 4 times, with cutoff swept at audio rate and loud input.
// One test per group (the phasers apart), so they run in parallel.
#[test]
fn every_setting_is_stable_at_every_sample_rate() {
    stable(&FilterType::NORMAL);
}

#[test]
fn every_multi_setting_is_stable_at_every_sample_rate() {
    stable(&FilterType::MULTI);
}

#[test]
fn every_comb_and_flanger_setting_is_stable_at_every_sample_rate() {
    stable(&FilterType::FLANGES[..16]);
}

#[test]
fn every_phaser_setting_is_stable_at_every_sample_rate() {
    stable(&FilterType::FLANGES[16..]);
}

#[test]
fn every_misc_setting_is_stable_at_every_sample_rate() {
    stable(&FilterType::MISC);
}

/// Every type in `kinds` stays bounded at every cutoff, resonance and
/// setting, from 44.1 kHz to 192 kHz oversampled 4 times, with cutoff
/// swept at audio rate and loud input.
fn stable(kinds: &[FilterType]) {
    const VOICES: usize = 16;
    for rate in [
        44_100.0f32,
        48_000.0,
        88_200.0,
        96_000.0,
        176_400.0,
        192_000.0,
        768_000.0,
    ] {
        let frames = 4_096;
        for &kind in kinds {
            for clean in [false, true] {
                let mut bank = FilterBank::new(rate, VOICES, 2);
                bank.set_type(kind);
                bank.set_clean_drive(clean);
                let stride = bank.stride();
                for v in 0..VOICES {
                    let corner = |bit: usize| if v >> bit & 1 == 1 { 1.0 } else { 0.0 };
                    bank.reset_voice(
                        v,
                        &VoiceParams {
                            resonance: [0.0, 0.9, 0.99, 1.0][v % 4],
                            drive: corner(2),
                            saturation: corner(3),
                            character: [0.0, 0.5, 1.0, 0.8][(v / 4 + v) % 4],
                            stereo_offset: 24.0 * (corner(1) - 0.5),
                            ..VoiceParams::default()
                        },
                    );
                }
                let mut rng = Rng(rate as u64 + kind as u64);
                let mut pitch = vec![0.0; frames * stride];
                let mut audio = vec![0.0; frames * 2 * stride];
                for f in 0..frames {
                    let t = f as f32 / rate;
                    for v in 0..VOICES {
                        // Swept across beyond the whole range, some fast.
                        let speed = [3.0, 50.0, 700.0, 4_000.0][v / 4];
                        pitch[f * stride + v] =
                            65.0 + 90.0 * (TAU as f32 * speed * t + v as f32).sin();
                        // Loud noise, with silence in the last quarter.
                        let loud = if f < 3 * frames / 4 { 4.0 } else { 0.0 };
                        for c in 0..2 {
                            audio[(f * 2 + c) * stride + v] = loud * (rng.unit() * 2.0 - 1.0);
                        }
                    }
                }
                bank.process(&pitch, &mut audio);
                // The EQ types boost by up to 24 dB, on top of drive's.
                let eq = matches!(
                    kind,
                    FilterType::LowEq6
                        | FilterType::LowEq12
                        | FilterType::BandEq12
                        | FilterType::HighEq6
                        | FilterType::HighEq12
                );
                let bound = if eq { 16_000.0 } else { 1_000.0 };
                for (i, y) in audio.iter().enumerate() {
                    assert!(
                        y.is_finite() && y.abs() < bound,
                        "{kind} at {rate} Hz, clean {clean}, voice {}: {y}",
                        i % stride
                    );
                }
            }
        }
    }
}

/// A block of test input for a bank: noise in every voice and channel,
/// with each voice's pitch gliding.
fn test_input(bank: &FilterBank, frames: usize, seed: u64) -> (Vec<f32>, Vec<f32>) {
    let stride = bank.stride();
    let mut rng = Rng(seed);
    let pitch = (0..frames * stride)
        .map(|i| 50.0 + 60.0 * ((i / stride) as f32 / 900.0 + (i % stride) as f32).sin())
        .collect();
    let audio = (0..frames * bank.channels() * stride)
        .map(|_| rng.unit() - 0.5)
        .collect();
    (pitch, audio)
}

/// Varied settings for voice `v`.
fn varied(v: usize, step: usize) -> VoiceParams {
    let x = ((v * 7 + step * 3) % 11) as f32 / 10.0;
    VoiceParams {
        resonance: 0.9 * x,
        drive: 1.0 - x,
        saturation: x * x,
        character: 1.0 - x * x,
        stereo_offset: 6.0 * x - 3.0,
        mix: 0.5 + 0.5 * x,
        level: 0.5 + x,
    }
}

/// `bank` run over the input in blocks of `size`, with every voice's
/// settings changed at frames 700 and 1_900.
fn run_in_blocks(mut bank: FilterBank, pitch: &[f32], audio: &[f32], size: usize) -> Vec<f32> {
    let stride = bank.stride();
    let frame_len = bank.channels() * stride;
    let frames = pitch.len() / stride;
    let mut out = audio.to_vec();
    let mut at = 0;
    for (step, end) in [(1, 700), (2, 1_900), (3, frames)] {
        while at < end {
            let n = size.min(end - at);
            bank.process(
                &pitch[at * stride..(at + n) * stride],
                &mut out[at * frame_len..(at + n) * frame_len],
            );
            at += n;
        }
        for v in 0..bank.voices() {
            bank.set_params(v, &varied(v, step));
        }
    }
    out
}

// Spec: engine-core R135 (smoothing that doesn't depend on block size).
#[test]
fn output_is_the_same_whatever_the_block_sizes() {
    for simd in [false, true] {
        for kind in FilterType::ALL {
            let mut bank = if simd {
                FilterBank::new(RATE, 5, 2)
            } else {
                FilterBank::scalar(RATE, 5, 2)
            };
            bank.set_type(kind);
            for v in 0..5 {
                bank.reset_voice(v, &varied(v, 0));
            }
            let (pitch, audio) = test_input(&bank, 2_500, 7);
            let whole = run_in_blocks(bank.clone(), &pitch, &audio, usize::MAX);
            for size in [1, 3, 64, 512] {
                let blocks = run_in_blocks(bank.clone(), &pitch, &audio, size);
                assert!(whole == blocks, "{kind}, simd {simd}, blocks of {size}");
            }
        }
    }
}

// A voice engine processes only its sounding voices: the voices below the
// span come out as a whole bank's do, and those above keep their audio and
// state untouched.
#[test]
fn a_span_of_voices_processes_those_voices_alone() {
    const VOICES: usize = 32;
    for simd in [false, true] {
        let make = || {
            let mut bank = if simd {
                FilterBank::new(RATE, VOICES, 2)
            } else {
                FilterBank::scalar(RATE, VOICES, 2)
            };
            bank.set_type(FilterType::Ladder24);
            for v in 0..VOICES {
                bank.reset_voice(v, &varied(v, 0));
            }
            bank
        };
        let mut whole = make();
        let mut span = make();
        let (pitch, audio) = test_input(&whole, 600, 11);
        let stride = whole.stride();
        let mut all = audio.clone();
        whole.process(&pitch, &mut all);
        let mut some = audio.clone();
        span.process_voices(5, &pitch, &mut some);
        let lanes = if simd { 16 } else { 1 };
        let reached = 5usize.div_ceil(lanes) * lanes;
        for f in 0..600 {
            for c in 0..2 {
                for v in 0..VOICES {
                    let i = (f * 2 + c) * stride + v;
                    let expected = if v < reached { all[i] } else { audio[i] };
                    assert!(
                        some[i] == expected || v >= 5 && v < reached,
                        "simd {simd}, frame {f}, voice {v}"
                    );
                }
            }
        }
    }
}

// Every SIMD level this machine has gives the scalar path's results, to
// within fused multiply-add's rounding.
#[test]
fn simd_lanes_match_the_scalar_path() {
    const VOICES: usize = 13;
    for kind in FilterType::ALL {
        let mut scalar = FilterBank::scalar(RATE, VOICES, 2);
        let mut lanes = FilterBank::new(RATE, VOICES, 2);
        for bank in [&mut scalar, &mut lanes] {
            bank.set_type(kind);
            for v in 0..VOICES {
                bank.reset_voice(v, &varied(v, 1));
            }
        }
        let frames = 2_048;
        let (pitch, audio) = test_input(&lanes, frames, 3);
        let s = scalar.stride();
        let l = lanes.stride();
        // The scalar bank's layout is unpadded.
        let mut scalar_pitch = vec![0.0; frames * s];
        let mut scalar_audio = vec![0.0; frames * 2 * s];
        for f in 0..frames {
            for v in 0..VOICES {
                scalar_pitch[f * s + v] = pitch[f * l + v];
                for c in 0..2 {
                    scalar_audio[(f * 2 + c) * s + v] = audio[(f * 2 + c) * l + v];
                }
            }
        }
        scalar.process(&scalar_pitch, &mut scalar_audio);
        for level in Level::ALL {
            let mut bank = lanes.clone();
            let mut out = audio.clone();
            if !bank.process_at(level, &pitch, &mut out) {
                continue;
            }
            for f in 0..frames {
                for v in 0..VOICES {
                    for c in 0..2 {
                        let a = scalar_audio[(f * 2 + c) * s + v];
                        let b = out[(f * 2 + c) * l + v];
                        assert!(
                            (a - b).abs() < 1e-4,
                            "{kind} at {level}, frame {f}, voice {v}: {a} {b}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn processing_stays_off_the_heap() {
    for simd in [false, true] {
        for kind in FilterType::ALL {
            let mut bank = if simd {
                FilterBank::new(RATE, 8, 2)
            } else {
                FilterBank::scalar(RATE, 8, 2)
            };
            bank.set_type(kind);
            let (pitch, mut audio) = test_input(&bank, 256, 1);
            lspk_rt::rt_test!({
                bank.set_params(3, &varied(3, 2));
                bank.process(&pitch, &mut audio);
                bank.reset_voice(2, &VoiceParams::default());
                bank.set_type(FilterType::Lp24);
            });
        }
    }
}

// Cutoff is honoured sample by sample: a pitch change at one frame changes
// the output from exactly that frame.
#[test]
fn cutoff_follows_the_pitch_buffer_sample_by_sample() {
    for kind in FilterType::ALL {
        let mut a = bank(kind, RATE, &VoiceParams::default());
        let mut b = a.clone();
        let input = signal::noise(5, 512, 0.5);
        let steady = vec![60.0; 512];
        let mut stepped = steady.clone();
        stepped[300] = 90.0;
        let (mut x, mut y) = (input.clone(), input.clone());
        a.process(&steady, &mut x);
        b.process(&stepped, &mut y);
        assert_eq!(x[..300], y[..300], "{kind}");
        // Types built on delay lines or holds may answer later: the change
        // reaches the output once the line or the hold is read.
        if kind.group() == FilterGroup::Normal {
            assert_ne!(x[300], y[300], "{kind}");
        } else {
            assert_ne!(x[300..], y[300..], "{kind}");
        }
    }
}

// Spec: engine-core R135: settings glide rather than jump.
#[test]
fn settings_glide_without_zipper_noise() {
    let mut bank = bank(FilterType::Lp12, RATE, &VoiceParams::default());
    let frames = 4_800;
    let pitch = vec![100.0; frames];
    let mut audio = vec![1.0; frames];
    bank.process(&pitch[..2_400], &mut audio[..2_400]);
    bank.set_params(
        0,
        &VoiceParams {
            level: 0.0,
            ..VoiceParams::default()
        },
    );
    bank.process(&pitch[2_400..], &mut audio[2_400..]);
    let steps = audio.windows(2).map(|w| (w[1] - w[0]).abs());
    let largest = steps.skip(2_000).fold(0.0f32, f32::max);
    // A 2 ms glide moves at most 1/96 of the way each sample at 48 kHz.
    assert!(largest < 0.011, "{largest}");
    assert!(audio[2_400 + 960] < 1e-3, "{}", audio[2_400 + 960]);
}

// Spec: engine-core R51 and R58: cutoff as a pitch puts the control's
// middle at 425 Hz, and follows the reference pitch.
#[test]
fn cutoff_as_pitch_follows_the_reference() {
    assert!((hertz_of(0.0, 440.0) - 8.18).abs() < 0.005);
    assert!((hertz_of(136.8 * 0.5, 440.0) - 425.1).abs() < 0.1);
    for reference in [440.0f32, 432.0] {
        let mut bank = bank(
            FilterType::Lp12,
            RATE,
            &VoiceParams {
                resonance: 0.0,
                ..VoiceParams::default()
            },
        );
        bank.set_reference(reference);
        let frames = 1 << 15;
        let pitch = vec![68.4; frames];
        let mut audio = signal::impulse(frames, 0, 1e-3);
        bank.process(&pitch, &mut audio);
        let ir: Vec<f64> = audio.iter().map(|&y| f64::from(y) * 1e3).collect();
        let hz = f64::from(hertz_of(68.4, reference));
        let at = db(dft(&ir, f64::from(RATE), hz).abs());
        assert!((at + 3.01).abs() < 0.02, "{reference}: {at}");
    }
}

/// A stereo one-voice scalar bank's response at `hz` on each channel.
fn stereo_response(kind: FilterType, params: &VoiceParams, cutoff: f32, hz: f64) -> [f64; 2] {
    let mut bank = FilterBank::scalar(RATE, 1, 2);
    bank.set_type(kind);
    bank.reset_voice(0, params);
    let frames = 1 << 14;
    let pitch = vec![pitch_of(cutoff, 440.0); frames];
    let mut audio = vec![0.0; frames * 2];
    audio[0] = 1e-3;
    audio[1] = 1e-3;
    bank.process(&pitch, &mut audio);
    std::array::from_fn(|c| {
        let ir: Vec<f64> = audio
            .iter()
            .skip(c)
            .step_by(2)
            .map(|&y| f64::from(y) * 1e3)
            .collect();
        db(dft(&ir, f64::from(RATE), hz).abs())
    })
}

// Spec: engine-core R55: the stereo offset moves the channels' cutoffs
// apart.
#[test]
fn stereo_offset_moves_the_channels_cutoffs_apart() {
    let params = |offset| VoiceParams {
        resonance: 0.0,
        stereo_offset: offset,
        ..VoiceParams::default()
    };
    // No offset: both channels 3 dB down at the cutoff.
    let [l, r] = stereo_response(FilterType::Lp12, &params(0.0), 1_000.0, 1_000.0);
    assert!(
        (l + 3.01).abs() < 0.02 && (r + 3.01).abs() < 0.02,
        "{l} {r}"
    );
    // Up an octave: the right channel's cutoff at 2 kHz, the left's at 500 Hz.
    let [_, r] = stereo_response(FilterType::Lp12, &params(12.0), 1_000.0, 2_000.0);
    let [l, _] = stereo_response(FilterType::Lp12, &params(12.0), 1_000.0, 500.0);
    assert!(
        (l + 3.01).abs() < 0.02 && (r + 3.01).abs() < 0.02,
        "{l} {r}"
    );
    // Down: the reverse.
    let [l, _] = stereo_response(FilterType::Lp12, &params(-12.0), 1_000.0, 2_000.0);
    assert!((l + 3.01).abs() < 0.02, "{l}");
}

/// `kind` with `params` over a sine at `hz` and `amplitude`: the output's
/// steady half second.
fn sine_through(
    kind: FilterType,
    params: &VoiceParams,
    clean: bool,
    cutoff: f32,
    hz: f64,
    amplitude: f64,
) -> Vec<f32> {
    let mut bank = bank(kind, RATE, params);
    bank.set_clean_drive(clean);
    let frames = 48_000;
    let pitch = vec![pitch_of(cutoff, 440.0); frames];
    let mut audio = signal::sine(hz, f64::from(RATE), frames, amplitude);
    bank.process(&pitch, &mut audio);
    audio.split_off(24_000)
}

// Spec: engine-core R53: drive raises the gain into the filter and adds
// saturation; clean drive keeps the gain with less saturation.
#[test]
fn drive_raises_the_gain_and_clean_drive_saturates_less() {
    let open = hertz_of(150.0, 440.0);
    for clean in [false, true] {
        for drive in [0.0, 0.5, 1.0] {
            let params = VoiceParams {
                drive,
                resonance: 0.0,
                ..VoiceParams::default()
            };
            let out = sine_through(FilterType::Lp12, &params, clean, open, 441.0, 1e-3);
            let gain = db(measure::tone(&out, f64::from(RATE), 441.0).0 / 1e-3);
            assert!(
                (gain - 24.0 * f64::from(drive)).abs() < 0.1,
                "clean {clean}, drive {drive}: {gain}"
            );
        }
    }
    let params = VoiceParams {
        drive: 1.0,
        resonance: 0.0,
        ..VoiceParams::default()
    };
    let thd = |clean| {
        let out = sine_through(FilterType::Lp12, &params, clean, open, 441.0, 0.25);
        measure::thd(&out, f64::from(RATE), 441.0, 9)
    };
    let (normal, clean) = (thd(false), thd(true));
    eprintln!(
        "drive 100 %, −12 dBFS in: THD {:.1} dB, clean {:.1} dB",
        db(normal),
        db(clean)
    );
    assert!(
        db(normal) > -20.0 && db(clean) < db(normal) - 20.0,
        "{normal} {clean}"
    );
}

// Spec: engine-core R54: saturation in the resonance path tames resonance
// at high levels and leaves small signals alone.
#[test]
fn saturation_tames_resonance_at_high_levels() {
    for kind in [
        FilterType::Lp12,
        FilterType::Lp24,
        FilterType::Bp12,
        FilterType::Peak12,
        FilterType::Ladder24,
    ] {
        let level = |saturation, amplitude| {
            let params = VoiceParams {
                resonance: 0.9,
                saturation,
                ..VoiceParams::default()
            };
            let out = sine_through(kind, &params, false, 1_000.0, 1_000.0, amplitude);
            db(measure::tone(&out, f64::from(RATE), 1_000.0).0 / amplitude)
        };
        let quiet = (level(0.0, 1e-4), level(1.0, 1e-4));
        let loud = (level(0.0, 0.5), level(1.0, 0.5));
        eprintln!(
            "{kind} at resonance 90 %: gain at the cutoff, quiet {:.1} / {:.1} dB, loud {:.1} / {:.1} dB (saturation 0 / 1)",
            quiet.0, quiet.1, loud.0, loud.1
        );
        assert!((quiet.0 - quiet.1).abs() < 0.2, "{kind}: {quiet:?}");
        assert!(loud.1 < loud.0 - 6.0, "{kind}: {loud:?}");
    }
}

// Spec: engine-core R56, R57: mix blends filtered and dry; level is the
// output gain.
#[test]
fn mix_blends_and_level_scales() {
    let input = signal::noise(9, 2_048, 0.5);
    let pitch = vec![70.0; 2_048];
    let run = |mix: f32, level: f32| {
        let params = VoiceParams {
            mix,
            level,
            ..VoiceParams::default()
        };
        let mut bank = bank(FilterType::Lp24, RATE, &params);
        let mut audio = input.clone();
        bank.process(&pitch, &mut audio);
        audio
    };
    assert_eq!(run(0.0, 1.0), input);
    let (wet, half, loud) = (run(1.0, 1.0), run(0.5, 1.0), run(1.0, 2.0));
    for i in 0..input.len() {
        assert!((half[i] - 0.5 * (wet[i] + input[i])).abs() < 1e-6);
        assert_eq!(loud[i], 2.0 * wet[i]);
    }
}

// Spec: engine-core R132: in a bank run inside the oversampler, as a voice
// engine at High or Ultra quality runs it, drive's harmonics don't fold
// back.
#[test]
fn oversampling_keeps_drive_from_aliasing() {
    let hz = 4_987.0;
    let frames = 48_000;
    let input = signal::sine(hz, f64::from(RATE), frames, 0.8);
    let params = VoiceParams {
        drive: 1.0,
        resonance: 0.0,
        ..VoiceParams::default()
    };
    let mut ratios = Vec::new();
    for stages in [0u32, 2] {
        let factor = 1usize << stages;
        let rate = RATE * factor as f32;
        // Peak at resonance 0 is flat, so this measures drive alone.
        let mut bank = bank(FilterType::Peak12, rate, &params);
        let mut oversampler = Oversampler::new(stages, 1, 512);
        let pitch = vec![60.0; 512 * factor];
        let mut audio = input.clone();
        for block in audio.chunks_mut(512) {
            oversampler.process(0, block, |high| bank.process(&pitch[..high.len()], high));
        }
        let ratio =
            measure::aliasing(&audio[24_000..], f64::from(RATE), Pitch::Steady(hz)).ratio_db();
        eprintln!("drive 100 % at {factor}x: aliasing {ratio:.1} dB");
        ratios.push(ratio);
    }
    assert!(ratios[1] < ratios[0] - 20.0, "{ratios:?}");
}
