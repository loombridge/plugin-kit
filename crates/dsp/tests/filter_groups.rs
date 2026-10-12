//! The Multi, Flanges and Misc groups, measured: each linear type's
//! response against its prototype, and the behaviour of the others (ring
//! modulation's sidebands, sample and hold's steps, the vowels' formants,
//! the room's decay, shriek's threshold). Stability, block-size
//! invariance, SIMD against scalar and allocation are tested for every
//! type in `filter.rs`.
//!
//! Spec rules are cited as `engine-core Rn`.

use std::f64::consts::{PI, TAU};

use lspk_dsp::filter::{FilterBank, FilterGroup, FilterType, VoiceParams, hertz_of, pitch_of};
use lspk_test::measure;
use lspk_test::signal;

const RATE: f32 = 48_000.0;
/// A sample rate at which pitch 69 (440 Hz) is exactly 1/128 of the rate,
/// so a comb's delay is exactly 128 samples and a flanger's 64.
const WHOLE: f32 = 56_320.0;

/// A complex number, for the prototypes.
#[derive(Clone, Copy, Debug)]
struct C(f64, f64);

impl C {
    fn abs(self) -> f64 {
        self.0.hypot(self.1)
    }
    /// e^(−jθ).
    fn turn(theta: f64) -> C {
        C(theta.cos(), -theta.sin())
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

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// The prewarped `s` of a stage with cutoff `cutoff` at `hz`.
fn s_at(rate: f64, cutoff: f64, hz: f64) -> C {
    let cutoff = cutoff.min(0.49 * rate);
    C(0.0, (PI * hz / rate).tan() / (PI * cutoff / rate).tan())
}

/// A state-variable stage's denominator.
fn den(s: C, k: f64) -> C {
    s * s + re(k) * s + re(1.0)
}

/// The state-variable outputs the 12 dB types take, written out
/// independently: low, high, band (unity at the centre at resonance 0),
/// peak and notch, with each kind's resonance law.
fn svf_output(kind: char, resonance: f32, s: C) -> C {
    let law = match kind {
        'l' | 'h' => FilterType::Lp12,
        'b' => FilterType::Bp12,
        _ => FilterType::Peak12,
    };
    let k = f64::from(law.feedback(resonance));
    let d = den(s, k);
    match kind {
        'l' => re(1.0) / d,
        'h' => s * s / d,
        'b' => s / d,
        'p' => re(1.0) + re(1.0 - k / 2.0) * s / d,
        _ => re(1.0) - re(k) * s / d,
    }
}

/// A one-voice mono scalar bank of `kind` with `params`, snapped.
fn bank(kind: FilterType, rate: f32, params: &VoiceParams) -> FilterBank {
    let mut bank = FilterBank::scalar(rate, 1, 1);
    bank.set_type(kind);
    bank.reset_voice(0, params);
    bank
}

/// `kind`'s impulse response with `params` at a steady `pitch`, from an
/// impulse small enough that the saturators and clippers do nothing.
fn impulse_response(
    kind: FilterType,
    params: &VoiceParams,
    rate: f32,
    pitch: f32,
    frames: usize,
) -> Vec<f64> {
    const AMPLITUDE: f32 = 1e-3;
    let mut bank = bank(kind, rate, params);
    let pitch = vec![pitch; frames];
    let mut audio = signal::impulse(frames, 0, AMPLITUDE);
    bank.process(&pitch, &mut audio);
    audio
        .iter()
        .map(|&y| f64::from(y) / f64::from(AMPLITUDE))
        .collect()
}

/// The response at `hz` of the system with impulse response `ir`.
fn dft(ir: &[f64], rate: f64, hz: f64) -> C {
    let step = C::turn(TAU * hz / rate);
    let mut rot = re(1.0);
    let mut sum = re(0.0);
    for &x in ir {
        sum = sum + re(x) * rot;
        rot = rot * step;
    }
    sum
}

/// Checks `kind`'s measured response against `prototype(hz)` at every
/// frequency in `frequencies`.
fn check(
    kind: FilterType,
    params: &VoiceParams,
    rate: f32,
    pitch: f32,
    frequencies: &[f64],
    tolerance: f64,
    prototype: impl Fn(f64) -> C,
) {
    // Long enough for the slowest ring (the 48-stage phasers' at 20 Hz)
    // to die away.
    let ir = impulse_response(kind, params, rate, pitch, 1 << 17);
    for &hz in frequencies {
        let want = prototype(hz);
        let got = dft(&ir, f64::from(rate), hz);
        assert!(
            (got - want).abs() <= tolerance * want.abs() + 3e-5,
            "{kind} with {params:?}, {hz:.0} Hz: {:.3} dB, prototype {:.3} dB",
            db(got.abs()),
            db(want.abs())
        );
    }
}

fn params(resonance: f32, character: f32) -> VoiceParams {
    VoiceParams {
        resonance,
        character,
        ..VoiceParams::default()
    }
}

// Spec: engine-core R61, R63: dual types are two state-variable stages,
// the second at the character's cutoff, sharing one resonance.
#[test]
fn dual_types_are_two_stages_at_the_cutoff_and_the_characters_cutoff() {
    let cutoff = 1_000.0f64;
    let frequencies = measure::log_frequencies(20.0, 0.45 * f64::from(RATE), 36);
    for kind in FilterType::MULTI
        .into_iter()
        .filter(|k| k.name().starts_with("dual"))
    {
        let letters: Vec<char> = kind.name()["dual-".len()..].chars().take(2).collect();
        for resonance in [0.0, 0.5, 0.9] {
            for character in [0.25, 0.5, 0.8] {
                // engine-core R63: the second cutoff, −4 to +4 octaves.
                let second = cutoff * 2f64.powf(8.0 * f64::from(character) - 4.0);
                check(
                    kind,
                    &params(resonance, character),
                    RATE,
                    pitch_of(cutoff as f32, 440.0),
                    &frequencies,
                    2e-3,
                    |hz| {
                        let (s1, s2) = (
                            s_at(f64::from(RATE), cutoff, hz),
                            s_at(f64::from(RATE), second, hz),
                        );
                        if kind == FilterType::DualLh6 {
                            re(1.0) / (re(1.0) + s1) * s2 / (re(1.0) + s2)
                        } else {
                            svf_output(letters[0], resonance, s1)
                                * svf_output(letters[1], resonance, s2)
                        }
                    },
                );
            }
        }
    }
}

/// A morphing stage's prototype: its three responses blended by
/// `character`, at damping `k` with resonance-0 damping `k0`.
fn morph_stage(set: &str, character: f64, k: f64, k0: f64, s: C) -> C {
    let d = den(s, k);
    let response = |kind: char| match kind {
        'l' => re(1.0) / d,
        'h' => s * s / d,
        'b' => re(k0) * s / d,
        'n' => re(1.0) - re(k) * s / d,
        _ => re(1.0) + re(1.0 - k / k0) * s / d,
    };
    let kinds: Vec<char> = set.chars().collect();
    let a = (2.0 * character).min(1.0);
    let b = (2.0 * character - 1.0).max(0.0);
    re(1.0 - a) * response(kinds[0]) + re(a - b) * response(kinds[1]) + re(b) * response(kinds[2])
}

// Spec: engine-core R61, R62: morphing types blend continuously across
// their three responses as character goes from 0 to 1.
#[test]
fn morph_types_blend_their_three_responses() {
    let cutoff = 1_000.0f64;
    let frequencies = measure::log_frequencies(20.0, 0.45 * f64::from(RATE), 36);
    let k4 = [2.0 * (3.0 * PI / 8.0).sin(), 2.0 * (PI / 8.0).sin()];
    for kind in FilterType::MULTI
        .into_iter()
        .filter(|k| k.name().starts_with("morph"))
    {
        let set = &kind.name()["morph-".len().."morph-".len() + 3];
        let two = kind.slope() == 24;
        for resonance in [0.0, 0.6] {
            for character in [0.0, 0.2, 0.5, 0.7, 1.0] {
                let k = f64::from(kind.feedback(resonance));
                let (k0, first) = match (set, two) {
                    ("bpn", _) => (1.0, 1.0),
                    (_, false) => (std::f64::consts::SQRT_2, 0.0),
                    (_, true) => (k4[1], k4[0]),
                };
                let c = f64::from(character);
                check(
                    kind,
                    &params(resonance, character),
                    RATE,
                    pitch_of(cutoff as f32, 440.0),
                    &frequencies,
                    2e-3,
                    |hz| {
                        let s = s_at(f64::from(RATE), cutoff, hz);
                        let second = morph_stage(set, c, k, k0, s);
                        if two {
                            morph_stage(set, c, first, first, s) * second
                        } else {
                            second
                        }
                    },
                );
            }
        }
    }
    // At the ends and the middle, the pure responses: low-pass at 0,
    // band-pass at 0.5, high-pass at 1.
    let gain = |character: f32, hz: f64| {
        let ir = impulse_response(
            FilterType::MorphLbh12,
            &params(0.0, character),
            RATE,
            pitch_of(1_000.0, 440.0),
            1 << 15,
        );
        db(dft(&ir, f64::from(RATE), hz).abs())
    };
    assert!(gain(0.0, 50.0).abs() < 0.01 && gain(0.0, 16_000.0) < -40.0);
    assert!(gain(0.5, 1_000.0).abs() < 0.01 && gain(0.5, 50.0) < -20.0);
    assert!(gain(1.0, 16_000.0).abs() < 0.01 && gain(1.0, 50.0) < -40.0);
}

/// A one-pole stage's prototype, low-pass or high-pass, at `cutoff`.
fn one_pole(high: bool, rate: f64, cutoff: f64, hz: f64) -> C {
    let s = s_at(rate, cutoff, hz);
    if high {
        s / (re(1.0) + s)
    } else {
        re(1.0) / (re(1.0) + s)
    }
}

/// The feedback filter's prototype for `name` (`lp`, `hp`, `band` or
/// none) around the comb's frequency `f`, at `character`.
fn feedback_filter(name: &str, character: f64, rate: f64, f: f64, hz: f64) -> C {
    let away = 2f64.powf(10.0 * (1.0 - character));
    let lp = one_pole(false, rate, f * away, hz);
    let hp = one_pole(true, rate, f / away, hz);
    if name.contains("-lp") {
        lp
    } else if name.contains("-hp") {
        hp
    } else if name.contains("-band") {
        lp * hp
    } else {
        re(1.0)
    }
}

// Spec: engine-core R61, R64: combs and flangers are delay lines of one
// and half a period of the cutoff, with signed feedback through their
// feedback filter, whose frequency or width character sets.
#[test]
fn combs_and_flangers_match_their_feedback_prototypes() {
    let rate = f64::from(WHOLE);
    let frequencies = measure::log_frequencies(20.0, 0.45 * rate, 41);
    for kind in FilterType::FLANGES
        .into_iter()
        .filter(|k| k.name().starts_with("comb") || k.name().starts_with("flanger"))
    {
        let delay = if kind.name().starts_with("comb") {
            128.0
        } else {
            64.0
        };
        for resonance in [0.0, 0.5, 0.9] {
            for character in [0.3, 0.8] {
                let a = f64::from(kind.feedback(resonance));
                check(
                    kind,
                    &params(resonance, character),
                    WHOLE,
                    69.0,
                    &frequencies,
                    2e-3,
                    |hz| {
                        let z = C::turn(TAU * hz / rate * delay);
                        let f = feedback_filter(kind.name(), f64::from(character), rate, 440.0, hz);
                        z / (re(1.0) - re(a) * f * z)
                    },
                );
            }
        }
    }
    // At Mix 50 % and no feedback, a comb's peaks are on the cutoff's
    // harmonics and a flanger's first notch is at the cutoff.
    let half = VoiceParams {
        resonance: 0.0,
        mix: 0.5,
        ..VoiceParams::default()
    };
    let comb = impulse_response(FilterType::CombPos, &half, WHOLE, 69.0, 4_096);
    let flanger = impulse_response(FilterType::FlangerPos, &half, WHOLE, 69.0, 4_096);
    for harmonic in 1..5 {
        let hz = 440.0 * f64::from(harmonic);
        assert!((dft(&comb, rate, hz).abs() - 1.0).abs() < 1e-6, "{hz}");
    }
    assert!(dft(&flanger, rate, 440.0).abs() < 1e-6);
}

/// A first-order all-pass chain of `stages` stages at `cutoff`.
fn all_passes(stages: i32, rate: f64, cutoff: f64, hz: f64) -> C {
    let s = s_at(rate, cutoff, hz);
    let a = (re(1.0) - s) / (re(1.0) + s);
    let mut chain = re(1.0);
    for _ in 0..stages {
        chain = chain * a;
    }
    chain
}

// Spec: engine-core R61, R64: phasers are chains of first-order all-pass
// stages at the cutoff (the count in the name) with signed feedback, a
// sample late, through the feedback filter; the flanger-phaser's feedback
// runs through the flanger's delay line too.
#[test]
fn phasers_match_their_all_pass_prototypes() {
    let rate = f64::from(WHOLE);
    let frequencies = measure::log_frequencies(20.0, 0.45 * rate, 41);
    for kind in FilterType::FLANGES
        .into_iter()
        .filter(|k| k.name().starts_with("phaser") || k.name().starts_with("flange-phaser"))
    {
        let stages: i32 = kind
            .name()
            .split('-')
            .find_map(|part| part.trim_end_matches(['+', '-']).parse().ok())
            .expect("a stage count");
        let flange = kind.name().starts_with("flange");
        for resonance in [0.0, 0.5, 0.8] {
            for character in [0.3, 0.8] {
                let a = f64::from(kind.feedback(resonance));
                check(
                    kind,
                    &params(resonance, character),
                    WHOLE,
                    69.0,
                    &frequencies,
                    2e-3,
                    |hz| {
                        let chain = all_passes(stages, rate, 440.0, hz);
                        let f = feedback_filter(kind.name(), f64::from(character), rate, 440.0, hz);
                        let late = C::turn(TAU * hz / rate * if flange { 64.0 } else { 1.0 });
                        chain / (re(1.0) - re(a) * f * late * chain)
                    },
                );
            }
        }
    }
    // At Mix 50 % without feedback, a phaser of n stages has n / 2 notches.
    let half = VoiceParams {
        resonance: 0.0,
        mix: 0.5,
        ..VoiceParams::default()
    };
    for (kind, notches) in [(FilterType::Phaser12Pos, 6), (FilterType::Phaser48Pos, 24)] {
        let ir = impulse_response(kind, &half, RATE, pitch_of(1_000.0, 440.0), 1 << 15);
        let grid = measure::log_frequencies(10.0, 23_990.0, 20_000);
        let gains: Vec<f64> = grid
            .iter()
            .map(|&hz| dft(&ir, f64::from(RATE), hz).abs())
            .collect();
        let found = gains
            .windows(3)
            .filter(|w| w[1] < w[0] && w[1] < w[2] && w[1] < 0.05)
            .count();
        assert_eq!(found, notches, "{kind}");
    }
}

// Spec: engine-core R61: the EQ types scale their band by the gain law,
// from silence (so Low EQ 12 is a high-pass) through unity to +24 dB;
// resonance does nothing at 6 dB.
#[test]
fn eq_types_scale_their_band_by_the_gain_law() {
    let cutoff = 1_000.0f64;
    let frequencies = measure::log_frequencies(20.0, 0.45 * f64::from(RATE), 36);
    for kind in [
        FilterType::LowEq6,
        FilterType::LowEq12,
        FilterType::BandEq12,
        FilterType::HighEq6,
        FilterType::HighEq12,
    ] {
        for resonance in [0.0, 0.7] {
            for character in [0.0, 0.3, 0.5, 0.8, 1.0] {
                let gain = (2.0 * f64::from(character)).powi(4);
                let k = f64::from(kind.feedback(resonance));
                check(
                    kind,
                    &params(resonance, character),
                    RATE,
                    pitch_of(cutoff as f32, 440.0),
                    &frequencies,
                    2e-3,
                    |hz| {
                        let s = s_at(f64::from(RATE), cutoff, hz);
                        let d = den(s, k);
                        let band = match kind {
                            FilterType::LowEq6 => re(1.0) / (re(1.0) + s),
                            FilterType::HighEq6 => s / (re(1.0) + s),
                            FilterType::LowEq12 => re(1.0) - s * s / d,
                            FilterType::HighEq12 => re(1.0) - re(1.0) / d,
                            _ => re(k) * s / d,
                        };
                        re(1.0) + re(gain - 1.0) * band
                    },
                );
            }
        }
    }
    assert!(!FilterType::LowEq6.has_resonance() && !FilterType::HighEq6.has_resonance());
    // Low EQ 12 at character 0 is a high-pass; at 1, +24 dB of shelf.
    let low = |character, hz| {
        let ir = impulse_response(
            FilterType::LowEq12,
            &params(0.0, character),
            RATE,
            pitch_of(1_000.0, 440.0),
            1 << 15,
        );
        db(dft(&ir, f64::from(RATE), hz).abs())
    };
    eprintln!(
        "low EQ 12 at character 0: {:.1} dB at 31.25 Hz, {:.3} dB at 16 kHz",
        low(0.0, 31.25),
        low(0.0, 16_000.0)
    );
    assert!(low(0.0, 31.25) < -55.0 && low(0.0, 16_000.0).abs() < 0.01);
    assert!(
        (low(1.0, 31.25) - 24.08).abs() < 0.05,
        "{}",
        low(1.0, 31.25)
    );
}

// Spec: engine-core R61: the two-resonance low-pass's second resonance
// is character; the zero-delay low-pass keeps its pass band at unity; bass
// add blends in the all-passed input by character; band reject's notches
// spread by character.
#[test]
fn the_other_linear_misc_types_match_their_prototypes() {
    let cutoff = 1_000.0f64;
    let frequencies = measure::log_frequencies(20.0, 0.45 * f64::from(RATE), 36);
    for kind in [
        FilterType::TwoResonanceLp,
        FilterType::ZeroDelayLp,
        FilterType::BassAdd,
        FilterType::BandReject,
    ] {
        for resonance in [0.0, 0.5, 0.9] {
            for character in [0.0, 0.5, 1.0] {
                let k = f64::from(kind.feedback(resonance));
                let c = f64::from(character);
                check(
                    kind,
                    &params(resonance, character),
                    RATE,
                    pitch_of(cutoff as f32, 440.0),
                    &frequencies,
                    2e-3,
                    |hz| {
                        let rate = f64::from(RATE);
                        let s = s_at(rate, cutoff, hz);
                        match kind {
                            FilterType::TwoResonanceLp => {
                                let r2 = std::f64::consts::SQRT_2;
                                let k2 = (r2 - r2 * 1.02 * c).max(0.25);
                                re(1.0) / den(s, k) / den(s, k2)
                            }
                            FilterType::ZeroDelayLp => {
                                let g = re(1.0) / (re(1.0) + s);
                                let g4 = g * g * g * g;
                                re(1.0 + k) * g4 / (re(1.0) + re(k) * g4)
                            }
                            FilterType::BassAdd => {
                                let d = den(s, k);
                                re(1.0) / d + re(c) * (s * s - re(k) * s + re(1.0)) / d
                            }
                            _ => {
                                let notch = |s: C| re(1.0) - re(k) * s / den(s, k);
                                let below = s_at(rate, cutoff * 2f64.powf(-c), hz);
                                let above = s_at(rate, cutoff * 2f64.powf(c), hz);
                                notch(below) * notch(above)
                            }
                        }
                    },
                );
            }
        }
    }
    // Bass add at full through: +6 dB of bass, unity at the top.
    let ir = impulse_response(
        FilterType::BassAdd,
        &params(0.0, 1.0),
        RATE,
        pitch_of(1_000.0, 440.0),
        1 << 15,
    );
    assert!((db(dft(&ir, f64::from(RATE), 30.0).abs()) - 6.02).abs() < 0.01);
    assert!(db(dft(&ir, f64::from(RATE), 20_000.0).abs()).abs() < 0.05);
    // The zero-delay low-pass stops short of oscillating.
    assert!(!FilterType::ZeroDelayLp.self_oscillates());
    assert!(FilterType::ZeroDelayLp.feedback(1.0) < 4.0);
}

// Spec: engine-core R61: comb-feedback types put a comb in the filter's
// feedback path, at the frequency character sets, positive for 1 and
// negative for 2.
#[test]
fn comb_feedback_types_match_their_prototypes_at_small_signals() {
    let rate = f64::from(WHOLE);
    let frequencies = measure::log_frequencies(20.0, 0.45 * rate, 41);
    for kind in [
        FilterType::CombFeedbackLp1,
        FilterType::CombFeedbackBp1,
        FilterType::CombFeedbackLp2,
        FilterType::CombFeedbackBp2,
    ] {
        for resonance in [0.0, 0.5, 0.9] {
            // Character 0.5 puts the comb at the cutoff: 128 samples.
            let a = f64::from(kind.feedback(resonance));
            check(
                kind,
                &params(resonance, 0.5),
                WHOLE,
                69.0,
                &frequencies,
                2e-3,
                |hz| {
                    let s = s_at(rate, 440.0, hz);
                    let filter = if kind.name().contains("-bp") {
                        s / den(s, 1.0)
                    } else {
                        re(1.0) / den(s, std::f64::consts::SQRT_2)
                    };
                    let z = C::turn(TAU * hz / rate * 128.0);
                    filter / (re(1.0) - re(a) * z * filter)
                },
            );
        }
    }
    // Character moves the comb: two octaves down at 0, so its peaks are a
    // quarter of the cutoff apart.
    let ir = impulse_response(
        FilterType::CombFeedbackLp1,
        &params(0.9, 0.0),
        WHOLE,
        69.0,
        1 << 15,
    );
    let at = |hz| dft(&ir, rate, hz).abs();
    assert!(at(110.0) > 4.0 * at(165.0), "{} {}", at(110.0), at(165.0));
}

/// `kind` with `params` over a sine at `hz`, amplitude `amplitude`, at
/// cutoff `cutoff`: the output's last second.
fn sine_through(
    kind: FilterType,
    params: &VoiceParams,
    cutoff: f32,
    hz: f64,
    amplitude: f64,
) -> Vec<f32> {
    let mut bank = bank(kind, RATE, params);
    let frames = 96_000;
    let pitch = vec![pitch_of(cutoff, 440.0); frames];
    let mut audio = signal::sine(hz, f64::from(RATE), frames, amplitude);
    bank.process(&pitch, &mut audio);
    audio.split_off(48_000)
}

// Spec: engine-core R61: ring modulation at the cutoff; the doubled form
// adds a second carrier up to an octave higher (spread).
#[test]
fn ring_modulators_make_sidebands_at_the_carriers() {
    let tone = |out: &[f32], hz: f64| measure::tone(out, f64::from(RATE), hz).0;
    let out = sine_through(FilterType::Ring, &params(0.0, 0.0), 300.0, 1_000.0, 0.5);
    for hz in [700.0, 1_300.0] {
        assert!(
            (tone(&out, hz) - 0.25).abs() < 1e-3,
            "{hz}: {}",
            tone(&out, hz)
        );
    }
    assert!(tone(&out, 1_000.0) < 1e-4 && tone(&out, 300.0) < 1e-4);
    // Spread 0.5: the second carrier half an octave up.
    let second = 300.0 * std::f64::consts::SQRT_2;
    let out = sine_through(FilterType::Ring2, &params(0.0, 0.5), 300.0, 1_000.0, 0.5);
    for hz in [700.0, 1_300.0, 1_000.0 - second, 1_000.0 + second] {
        assert!(
            (tone(&out, hz) - 0.125).abs() < 1e-3,
            "{hz}: {}",
            tone(&out, hz)
        );
    }
    // Spread 0: the doubled form is the single one.
    let one = sine_through(FilterType::Ring, &params(0.0, 0.0), 300.0, 1_000.0, 0.5);
    let two = sine_through(FilterType::Ring2, &params(0.0, 0.0), 300.0, 1_000.0, 0.5);
    for (a, b) in one.iter().zip(&two) {
        assert!((a - b).abs() < 1e-5);
    }
}

// Spec: engine-core R61: sample and hold takes the input once a period of
// the cutoff; the difference form outputs the input less the held value.
#[test]
fn sample_and_hold_holds_for_a_period_of_the_cutoff() {
    let frames = 4_800;
    let input: Vec<f32> = (0..frames).map(|i| i as f32 / frames as f32).collect();
    let cutoff = 1_000.0;
    let run = |kind| {
        let mut bank = bank(kind, RATE, &params(0.0, 0.0));
        let mut audio = input.clone();
        bank.process(&vec![pitch_of(cutoff, 440.0); frames], &mut audio);
        audio
    };
    let held = run(FilterType::SampleHold);
    // Steps every 48 samples, from the first, holding the input there.
    let mut changes = Vec::new();
    for i in 1..frames {
        if held[i] != held[i - 1] {
            changes.push(i);
        }
    }
    assert_eq!(held[0], input[0]);
    assert_eq!(changes.len(), frames / 48 - 1, "{changes:?}");
    for &i in &changes {
        assert!(i % 48 == 0 || i % 48 == 47 || i % 48 == 1, "{i}");
        assert_eq!(held[i], input[i]);
    }
    let difference = run(FilterType::SampleHoldDiff);
    for i in 0..frames {
        assert!((difference[i] - (input[i] - held[i])).abs() < 1e-6);
    }
}

// Spec: engine-core R56, R61: the combs ignore mix; the all-passes pass
// every frequency at unity; the room decays faster with less resonance,
// and its damping darkens it.
#[test]
fn the_multi_comb_types_and_the_room() {
    let input = signal::noise(4, 9_600, 0.5);
    let pitch = vec![pitch_of(200.0, 440.0); 9_600];
    let run = |kind, params: &VoiceParams, input: &[f32]| {
        let mut bank = bank(kind, RATE, params);
        let mut audio = input.to_vec();
        bank.process(&pitch[..input.len()], &mut audio);
        audio
    };
    // engine-core R56: mix has no effect on the combs.
    for mix in [0.0, 0.3] {
        let p = VoiceParams {
            mix,
            ..params(0.8, 0.2)
        };
        assert_eq!(
            run(FilterType::Combs, &p, &input),
            run(FilterType::Combs, &params(0.8, 0.2), &input)
        );
    }
    // The all-passes without damping pass every frequency at unity, up to
    // the interpolator's loss near the top.
    let ir = impulse_response(
        FilterType::Allpasses,
        &params(0.6, 0.0),
        RATE,
        pitch_of(200.0, 440.0),
        1 << 16,
    );
    for hz in measure::log_frequencies(20.0, 6_000.0, 25) {
        let gain = db(dft(&ir, f64::from(RATE), hz).abs());
        assert!(gain.abs() < 0.5, "{hz:.0} Hz: {gain:.2} dB");
    }
    // The room: an impulse's tail lasts longer with more resonance, and
    // damping takes its top off.
    let tail = |resonance, character| {
        let mut impulse = signal::impulse(9_600, 0, 1.0);
        impulse = run(FilterType::Room, &params(resonance, character), &impulse);
        impulse.split_off(4_800)
    };
    let (short, long) = (measure::rms(&tail(0.3, 0.0)), measure::rms(&tail(0.9, 0.0)));
    eprintln!(
        "room tail 100 to 200 ms: {:.1} dB at resonance 30 %, {:.1} dB at 90 %",
        db(short),
        db(long)
    );
    assert!(long > 10.0 * short, "{short} {long}");
    let bright = measure::centroid(&tail(0.9, 0.0), f64::from(RATE)).expect("a tail");
    let dark = measure::centroid(&tail(0.9, 0.8), f64::from(RATE)).expect("a tail");
    assert!(dark < 0.5 * bright, "{bright} {dark}");
}

// Spec: engine-core R61: the vowels' band-passes sit at the formants of the
// vowel the cutoff picks; character raises them.
#[test]
fn vowels_put_their_peaks_at_the_formants() {
    // Pitch 68.4 is the middle of the cutoff's range: the third vowel, i.
    let formants = [
        (FilterType::Vowel1, [270.0, 2_290.0, 3_010.0]),
        (FilterType::Vowel2, [310.0, 2_790.0, 3_310.0]),
        (FilterType::Vowel3, [370.0, 3_200.0, 3_730.0]),
    ];
    for (kind, hz) in formants {
        for (character, scale) in [(0.0, 1.0), (1.0, 2.0)] {
            let ir = impulse_response(kind, &params(0.8, character), RATE, 68.4, 1 << 15);
            for &f in &hz {
                let f = f * scale;
                // The formant is a local peak within a few per cent.
                let grid = measure::log_frequencies(f * 0.9, f * 1.1, 201);
                let (at, _) = grid.iter().fold((0.0, 0.0f64), |best, &x| {
                    let g = dft(&ir, f64::from(RATE), x).abs();
                    if g > best.1 { (x, g) } else { best }
                });
                assert!((at / f - 1.0).abs() < 0.03, "{kind} {character}: {f} {at}");
            }
        }
    }
    // The first vowel, a, at the bottom of the range.
    let ir = impulse_response(FilterType::Vowel1, &params(0.8, 0.0), RATE, 0.0, 1 << 15);
    let at = |hz| dft(&ir, f64::from(RATE), hz).abs();
    assert!(at(730.0) > 3.0 * at(450.0));
}

// Spec: engine-core R61: shriek is the plain resonant type until drive
// passes 50 %, then its feedback adds its own sound.
#[test]
fn shriek_comes_in_above_half_drive() {
    for (kind, plain) in [
        (FilterType::ShriekLp, FilterType::Lp12),
        (FilterType::ShriekBp, FilterType::Bp12),
    ] {
        let input = signal::noise(3, 9_600, 0.3);
        let pitch = vec![pitch_of(800.0, 440.0); 9_600];
        let run = |kind, drive| {
            let p = VoiceParams {
                drive,
                ..params(0.5, 0.6)
            };
            let mut bank = bank(kind, RATE, &p);
            let mut audio = input.clone();
            bank.process(&pitch, &mut audio);
            audio
        };
        for drive in [0.0, 0.3, 0.5] {
            assert_eq!(
                run(kind, drive),
                run(plain, drive),
                "{kind} at drive {drive}"
            );
        }
        let (shriek, base) = (run(kind, 1.0), run(plain, 1.0));
        let difference: Vec<f32> = shriek.iter().zip(&base).map(|(a, b)| a - b).collect();
        let ratio = measure::rms(&difference) / measure::rms(&base);
        eprintln!(
            "{kind} at full drive: differs from {plain} by {:.1} dB",
            db(ratio)
        );
        assert!(ratio > 0.3, "{kind}: {ratio}");
    }
}

// Spec: engine-core R61: every type's character control does its job, and
// types with none ignore it.
#[test]
fn character_moves_the_types_that_have_one_and_no_others() {
    for kind in FilterType::ALL
        .into_iter()
        .filter(|k| k.group() != FilterGroup::Normal)
    {
        let input = signal::noise(8, 4_096, 0.3);
        let pitch: Vec<f32> = (0..4_096)
            .map(|i| 60.0 + 10.0 * (i as f32 * 0.003).sin())
            .collect();
        let run = |character| {
            // Drive above half, for shriek's feedback.
            let p = VoiceParams {
                drive: 0.8,
                ..params(0.7, character)
            };
            let mut bank = bank(kind, RATE, &p);
            let mut audio = input.clone();
            bank.process(&pitch, &mut audio);
            audio
        };
        let (a, b) = (run(0.2), run(0.9));
        if kind.character().is_some() {
            assert_ne!(a, b, "{kind}");
        } else {
            assert_eq!(a, b, "{kind}");
        }
    }
    assert_eq!(FilterType::ALL.len(), 96);
    assert_eq!(FilterType::Lp12.character(), Some("resonance saturation"));
    assert!(hertz_of(69.0, 440.0) == 440.0);
}
