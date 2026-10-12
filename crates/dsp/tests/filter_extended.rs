//! The extended group, measured: each type's small-signal response against
//! its analogue prototype, what each Character does, self-oscillation, the
//! pole-zero filter's morphing (corners, blends, audio-rate X and Y without
//! zipper noise, stability along any path), and its design's data model.
//! The generic checks (stability across sample rates, block-size
//! invariance, SIMD against scalar, no allocation) run over every type in
//! filter.rs; this file adds the pole-zero filter's audio-rate path to them.
//!
//! Spec rules are cited as `engine-core Rn`.

use std::f64::consts::{PI, TAU};

use lspk_dsp::filter::pole_zero::{self, Point, PointKind, PoleZeroDesign, State};
use lspk_dsp::filter::{
    Character, FilterBank, FilterGroup, FilterType, VoiceParams, hertz_of, pitch_of,
};
use lspk_test::measure;
use lspk_test::signal;

lspk_rt::install_trap!();

const RATE: f32 = 48_000.0;

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

fn db(x: f64) -> f64 {
    20.0 * x.log10()
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

/// The kit's soft clipper, written out: x (27 + x²) / (27 + 9 x²) on x
/// clamped to ±3.
fn soft_clip(x: f64) -> f64 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// The extended types.
fn extended() -> impl Iterator<Item = FilterType> {
    FilterType::ALL
        .into_iter()
        .filter(|k| k.group() == FilterGroup::Extended)
}

/// The diode ladder's open loop, stage 1's input to stage 4's output, at
/// `s` in units of its stage frequency: four coupled stages solved as a
/// linear system.
fn diode_open_loop(s: C) -> C {
    let a = 0.5;
    // (sI − A) y = b u, A tridiagonal: −(1 + a) on the diagonal (−1 for the
    // last), 1 below, a above.
    let mut m = [[re(0.0); 4]; 4];
    for (i, row) in m.iter_mut().enumerate() {
        row[i] = s + re(if i == 3 { 1.0 } else { 1.0 + a });
        if i > 0 {
            row[i - 1] = re(-1.0);
        }
        if i < 3 {
            row[i + 1] = re(-a);
        }
    }
    let mut b = [re(1.0), re(0.0), re(0.0), re(0.0)];
    for col in 0..4 {
        for row in col + 1..4 {
            let f = m[row][col] / m[col][col];
            let pivot = m[col];
            for (to, from) in m[row].iter_mut().zip(pivot).skip(col) {
                *to = *to - f * from;
            }
            b[row] = b[row] - f * b[col];
        }
    }
    let mut y = [re(0.0); 4];
    for i in (0..4).rev() {
        let mut acc = b[i];
        for j in i + 1..4 {
            acc = acc - m[i][j] * y[j];
        }
        y[i] = acc / m[i][i];
    }
    y[3]
}

/// The diode ladder's oscillation frequency in stage units, and its
/// feedback at the edge, as the module documents them.
const DIODE_W: f64 = 1.055_289_7;
const DIODE_K: f64 = 8.617_769;

// The diode ladder's constants are its model's: the open loop turns half
// a cycle at the cutoff, where its gain is 1 / DIODE_K.
#[test]
fn the_diode_ladder_constants_fit_its_model() {
    let h = diode_open_loop(C(0.0, DIODE_W));
    assert!(h.1.abs() < 1e-6 * h.abs(), "{h:?}");
    assert!((-1.0 / h.0 / DIODE_K - 1.0).abs() < 1e-6, "{h:?}");
}

/// The analogue prototype of an extended `kind` with `params` at `s`,
/// normalised so the cutoff is 1, for a cutoff pitch `pitch` (the DJ sweep
/// maps it). Gives the prototype and the cutoff it's normalised to.
fn prototype(kind: FilterType, params: &VoiceParams, pitch: f32, s: C) -> C {
    let k = f64::from(kind.feedback(params.resonance));
    let c = f64::from(params.character);
    let one = re(1.0);
    let den = |k: f64| s * s + re(k) * s + one;
    let lp1 = one / (one + s);
    let ladder = |taps: [f64; 5], comp: f64| {
        let mut g = one;
        let mut sum = re(0.0);
        for t in taps {
            sum = sum + re(t) * g;
            g = g * lp1;
        }
        let g4 = lp1 * lp1 * lp1 * lp1;
        re(comp) * sum / (one + re(k) * g4)
    };
    match kind {
        FilterType::BuzzMorph => {
            let (wl, wh) = ((2.0 * (1.0 - c)).min(1.0), (2.0 * c).min(1.0));
            (re(wl) + re(wh) * s * s) / den(k)
        }
        FilterType::DjSweep => {
            let (_, low_side, wet) = dj(pitch);
            let filtered = if low_side {
                one / den(k)
            } else {
                s * s / den(k)
            };
            re(wet) * filtered + re(1.0 - wet)
        }
        FilterType::Diffuser => {
            let ap = (s * s - re(k) * s + one) / den(k);
            let n = c * 7.0;
            let mut out = re(0.0);
            let mut chain = one;
            for j in 0..8 {
                chain = chain * ap;
                out = out + re((1.0 - (n - j as f64).abs()).max(0.0)) * chain;
            }
            out
        }
        FilterType::TransistorLadder => ladder([0.0, 0.0, 0.0, 0.0, 1.0], 1.0 + 0.5 * k),
        FilterType::DrivenLadder => {
            let drive = 10f64.powf(c * 30.0 / 20.0);
            re(drive / soft_clip(drive)) * ladder([0.0, 0.0, 0.0, 0.0, 1.0], 1.0 + 0.5 * k)
        }
        FilterType::DiodeLadder | FilterType::CleanLadder => {
            let h = diode_open_loop(s * re(DIODE_W));
            let comp = if kind == FilterType::CleanLadder {
                1.0 + k
            } else {
                1.0
            };
            re(comp) * h / (one + re(k) * h)
        }
        FilterType::ExpanderMultimode => {
            let low = [0.0, 0.0, 0.0, 0.0, 1.0];
            let notch = [1.0, -4.0, 8.0, -8.0, 4.0];
            let high = [1.0, -4.0, 6.0, -4.0, 1.0];
            let (a, b) = ((2.0 * c).min(1.0), (2.0 * c - 1.0).max(0.0));
            let taps = std::array::from_fn(|i| {
                low[i] + a * (notch[i] - low[i]) + b * (high[i] - notch[i])
            });
            ladder(taps, 1.0 + 0.5 * k)
        }
        FilterType::ExpanderBandPass => ladder([0.0, 0.0, 4.0, -8.0, 4.0], 1.0 + 0.5 * k),
        FilterType::K35 => {
            let w = ((c - 0.45) / 0.1).clamp(0.0, 1.0);
            let d = s * s + re(2.0 - k) * s + one;
            (re(1.0 - w) + re(w) * s * s) / d
        }
        _ => unreachable!("{kind} has no prototype here"),
    }
}

/// The DJ sweep's map, as the module documents it: the stage's cutoff
/// pitch, whether it's the low-pass, and how much of it is mixed in.
fn dj(pitch: f32) -> (f32, bool, f64) {
    let d = ((pitch - 68.4) / 68.4).clamp(-1.0, 1.0);
    let u = ((d.abs() - 0.04) / 0.96).max(0.0);
    let wet = f64::from((u / 0.1).min(1.0));
    if d < 0.0 {
        (135.076 + u * (22.651 - 135.076), true, wet)
    } else {
        (3.486 + u * (123.514 - 3.486), false, wet)
    }
}

/// A one-voice mono scalar bank of `kind` with `params`, snapped.
fn bank(kind: FilterType, rate: f32, params: &VoiceParams) -> FilterBank {
    let mut bank = FilterBank::scalar(rate, 1, 1);
    bank.set_type(kind);
    bank.reset_voice(0, params);
    bank
}

/// The impulse response of `bank` at cutoff pitch `pitch`, from an impulse
/// small enough that the nonlinear stages do nothing.
fn impulse_response(mut bank: FilterBank, pitch: f32, frames: usize) -> Vec<f64> {
    const AMPLITUDE: f32 = 1e-4;
    let pitches = vec![pitch; frames];
    let mut audio = signal::impulse(frames, 0, AMPLITUDE);
    bank.process(&pitches, &mut audio);
    audio
        .iter()
        .map(|&y| f64::from(y) / f64::from(AMPLITUDE))
        .collect()
}

/// The settings each type's prototype test runs at.
fn settings(kind: FilterType) -> Vec<VoiceParams> {
    let at = |resonance: f32, character: f32| VoiceParams {
        resonance,
        character,
        ..VoiceParams::default()
    };
    let mut out = Vec::new();
    for resonance in [0.0, 0.5, 0.9] {
        let characters: &[f32] = match kind.character() {
            Character::Morph | Character::Stages => &[0.0, 0.2, 0.5, 0.8, 1.0],
            Character::LowHigh => &[0.0, 0.5, 1.0],
            // Smooth does nothing to a steady cutoff.
            Character::Smooth | Character::Overdrive => &[0.0, 1.0],
            _ => &[0.0],
        };
        for &c in characters {
            out.push(at(resonance, c));
        }
    }
    out
}

// Spec: engine-core R61 (the extended group's rows): each type's response
// at small signals is its analogue prototype's on the prewarped axis.
#[test]
fn each_type_matches_its_analogue_prototype() {
    eprintln!("type               worst error (relative)  at");
    for kind in extended().filter(|&k| k != FilterType::PoleZero && k != FilterType::Comb2) {
        let mut worst = (0.0f64, String::new());
        for (rate, cutoff) in [
            (48_000.0f32, 1_000.0f32),
            (192_000.0, 9_000.0),
            (44_100.0, 120.0),
        ] {
            let frequencies = measure::log_frequencies(20.0, 0.45 * f64::from(rate), 36);
            // The DJ sweep is tested across its control instead.
            let pitches: Vec<f32> = if kind == FilterType::DjSweep {
                vec![10.0, 40.0, 64.0, 66.0, 70.0, 90.0, 130.0]
            } else {
                vec![pitch_of(cutoff, 440.0)]
            };
            for params in settings(kind) {
                for &pitch in &pitches {
                    let fc = if kind == FilterType::DjSweep {
                        hertz_of(dj(pitch).0, 440.0)
                    } else {
                        hertz_of(pitch, 440.0)
                    };
                    let ir = impulse_response(bank(kind, rate, &params), pitch, 1 << 16);
                    for &hz in &frequencies {
                        let omega = (PI * hz / f64::from(rate)).tan()
                            / (PI * f64::from(fc.min(0.49 * rate)) / f64::from(rate)).tan();
                        let want = prototype(kind, &params, pitch, C(0.0, omega));
                        let got = dft(&ir, f64::from(rate), hz);
                        let error = (got - want).abs();
                        let relative = error / (want.abs() + 1.5e-2);
                        if relative > worst.0 {
                            worst = (
                                relative,
                                format!(
                                    "{rate} Hz, pitch {pitch}, resonance {}, character {}, {hz:.0} Hz",
                                    params.resonance, params.character
                                ),
                            );
                        }
                        assert!(
                            error <= 2e-3 * want.abs() + 3e-5,
                            "{kind} at {rate} Hz, pitch {pitch}, resonance {}, character {}, \
                             {hz:.0} Hz: {:.3} dB, prototype {:.3} dB",
                            params.resonance,
                            params.character,
                            db(got.abs()),
                            db(want.abs())
                        );
                    }
                }
            }
        }
        eprintln!("{:<18} {:>10.2e}  {}", kind.name(), worst.0, worst.1);
    }
}

// Spec: engine-core R61: Comb II's response, at whole-sample delays, is
// its difference equation's.
#[test]
fn comb_2_matches_its_difference_equation() {
    for (rate, period, character) in [
        (48_000.0f32, 100usize, 0.0f32),
        (48_000.0, 100, 0.5),
        (96_000.0, 480, 1.0),
    ] {
        let second = (period as f32 / (2.0 * character).exp2()).round() as usize;
        for resonance in [0.0, 0.5, 0.9] {
            let params = VoiceParams {
                resonance,
                character,
                ..VoiceParams::default()
            };
            let pitch = pitch_of(rate / period as f32, 440.0);
            let ir = impulse_response(bank(FilterType::Comb2, rate, &params), pitch, 1 << 16);
            let fb = f64::from(FilterType::Comb2.feedback(resonance));
            for hz in measure::log_frequencies(20.0, 0.45 * f64::from(rate), 300) {
                let w = TAU * hz / f64::from(rate);
                let z = |n: usize| C((w * n as f64).cos(), -(w * n as f64).sin());
                let want = re(1.0 - fb) / (re(1.0) - re(0.5 * fb) * (z(period) + z(second)));
                let got = dft(&ir, f64::from(rate), hz);
                assert!(
                    (got - want).abs() <= 2e-3 * want.abs() + 1e-4,
                    "{rate} Hz, period {period}, character {character}, resonance {resonance}, \
                     {hz:.0} Hz: {:.3} dB, expected {:.3} dB",
                    db(got.abs()),
                    db(want.abs())
                );
            }
        }
    }
}

// Comb II's peaks sit on the cutoff's harmonics between whole samples
// too, at unity, and its pitch stops at 20 Hz.
#[test]
fn comb_2_is_tuned_between_whole_samples() {
    let params = VoiceParams {
        resonance: 0.9,
        ..VoiceParams::default()
    };
    for hz in [110.0f64, 261.63, 1_234.5, 10.0] {
        let ir = impulse_response(
            bank(FilterType::Comb2, RATE, &params),
            pitch_of(hz as f32, 440.0),
            1 << 16,
        );
        let fundamental = hz.max(20.0);
        for harmonic in [1.0, 5.0] {
            let centre = fundamental * harmonic;
            let (mut at, mut peak) = (0.0, 0.0);
            for step in -300..=300 {
                let f = centre * (1.0 + f64::from(step) * 1e-4);
                let gain = dft(&ir, f64::from(RATE), f).abs();
                if gain > peak {
                    (at, peak) = (f, gain);
                }
            }
            eprintln!(
                "comb-2 at {hz} Hz: harmonic {harmonic} peaks at {at:.2} Hz, {:.2} dB",
                db(peak)
            );
            assert!((at / centre - 1.0).abs() < 5e-4, "{hz} × {harmonic}: {at}");
            // Interpolating between samples softens the peaks a little
            // high up: a quarter of a decibel at 6 kHz.
            assert!(db(peak).abs() < 0.3, "{hz} × {harmonic}: {}", db(peak));
        }
    }
}

// Each self-oscillating type rings on its own at resonance 1, at a steady
// level, at the cutoff.
#[test]
fn self_oscillating_types_ring_at_the_cutoff() {
    for kind in extended() {
        for (rate, cutoff) in [(48_000.0f32, 1_000.0f32), (96_000.0, 220.0)] {
            let params = VoiceParams {
                resonance: 1.0,
                ..VoiceParams::default()
            };
            let mut bank = bank(kind, rate, &params);
            let frames = (rate * 2.0) as usize;
            let pitch = vec![pitch_of(cutoff, 440.0); frames];
            let mut audio = signal::impulse(frames, 0, 0.1);
            bank.process(&pitch, &mut audio);
            let tail = audio.split_off(frames - (rate * 0.5) as usize);
            let rms = measure::rms(&tail);
            if kind.self_oscillates() {
                let found = measure::pitch(&tail, f64::from(rate)).expect("periodic");
                eprintln!(
                    "{kind} at {rate} Hz: {found:.2} Hz for {cutoff} Hz, {:.1} dBFS RMS",
                    db(rms)
                );
                assert!(rms > 0.02 && measure::peak(&tail) < 4.0, "{kind}: {rms}");
                // The transistor ladders' saturating stages slow as they
                // swing, so their pitch sags a little (about 2 % at full
                // level).
                let tolerance = match kind {
                    FilterType::TransistorLadder | FilterType::DrivenLadder => 0.03,
                    _ => 0.01,
                };
                assert!(
                    (found / f64::from(cutoff) - 1.0).abs() < tolerance,
                    "{kind}: {found}"
                );
            } else {
                assert!(rms < 1e-5, "{kind}: {rms}");
            }
        }
    }
}

/// `kind` with `params` over a sine at `hz` and `amplitude`, cutoff
/// `cutoff`: the output's steady half second.
fn sine_through(
    kind: FilterType,
    params: &VoiceParams,
    cutoff: f32,
    hz: f64,
    amplitude: f64,
) -> Vec<f32> {
    let mut bank = bank(kind, RATE, params);
    let frames = 48_000;
    let pitch = vec![pitch_of(cutoff, 440.0); frames];
    let mut audio = signal::sine(hz, f64::from(RATE), frames, amplitude);
    bank.process(&pitch, &mut audio);
    audio.split_off(24_000)
}

/// The gain in decibels of `kind` with `params` at `hz`, cutoff `cutoff`,
/// for a quiet sine.
fn gain_at(kind: FilterType, params: &VoiceParams, cutoff: f32, hz: f64) -> f64 {
    let out = sine_through(kind, params, cutoff, hz, 1e-3);
    db(measure::tone(&out, f64::from(RATE), hz).0 / 1e-3)
}

// Spec: engine-core R62 (the buzz morph's row): low-pass at 0, a notch at
// the cutoff at 0.5, high-pass at 1; loud signals gain even harmonics.
#[test]
fn the_buzz_morph_morphs_and_buzzes() {
    let at = |character| VoiceParams {
        resonance: 0.0,
        character,
        ..VoiceParams::default()
    };
    let k = FilterType::BuzzMorph;
    assert!(gain_at(k, &at(0.0), 1_000.0, 4_000.0) < -20.0);
    assert!(gain_at(k, &at(0.0), 1_000.0, 100.0).abs() < 0.1);
    assert!(gain_at(k, &at(0.5), 1_000.0, 1_000.0) < -40.0);
    assert!(gain_at(k, &at(1.0), 1_000.0, 250.0) < -20.0);
    assert!(gain_at(k, &at(1.0), 1_000.0, 10_000.0).abs() < 0.1);
    let out = sine_through(k, &at(0.0), 15_000.0, 441.0, 0.8);
    let (fundamental, _) = measure::tone(&out, f64::from(RATE), 441.0);
    let (second, _) = measure::tone(&out, f64::from(RATE), 882.0);
    eprintln!(
        "buzz morph, −2 dBFS: second harmonic {:.1} dB",
        db(second / fundamental)
    );
    assert!(db(second / fundamental) > -40.0);
}

// Spec: engine-core R61 (the DJ sweep's row): flat at the middle, low-pass
// below it, high-pass above.
#[test]
fn the_dj_sweep_is_flat_in_the_middle() {
    let params = VoiceParams::default();
    let input = signal::noise(2, 4_096, 0.5);
    for pitch in [68.4, 67.0, 70.0] {
        let mut b = bank(FilterType::DjSweep, RATE, &params);
        let mut audio = input.clone();
        b.process(&vec![pitch; 4_096], &mut audio);
        assert_eq!(audio, input, "{pitch}");
    }
    let open = |pitch: f32, hz: f64| {
        let mut b = bank(FilterType::DjSweep, RATE, &params);
        let frames = 48_000;
        let mut audio = signal::sine(hz, f64::from(RATE), frames, 1e-3);
        b.process(&vec![pitch; frames], &mut audio);
        db(measure::tone(&audio[24_000..], f64::from(RATE), hz).0 / 1e-3)
    };
    // Low end of the control: low-pass near 30 Hz; top: high-pass near
    // 10 kHz.
    assert!(open(0.0, 1_000.0) < -40.0);
    assert!(open(0.0, 20.0).abs() < 1.0);
    assert!(open(136.8, 1_000.0) < -40.0);
    assert!(open(136.8, 20_000.0).abs() < 2.0);
}

// Spec: engine-core R61 (the diffuser's row): flat magnitude whatever the
// stages, with more stages delaying the cutoff's band more.
#[test]
fn the_diffuser_is_flat_and_stages_add_delay() {
    let mut previous = 0.0;
    for stages in 0..8 {
        let params = VoiceParams {
            resonance: 0.5,
            character: stages as f32 / 7.0,
            ..VoiceParams::default()
        };
        let ir = impulse_response(
            bank(FilterType::Diffuser, RATE, &params),
            pitch_of(1_000.0, 440.0),
            1 << 15,
        );
        for hz in measure::log_frequencies(20.0, 20_000.0, 50) {
            let gain = db(dft(&ir, f64::from(RATE), hz).abs());
            assert!(gain.abs() < 0.01, "{stages}: {hz} Hz {gain}");
        }
        // Group delay at the cutoff, from the phase's slope.
        let phase = |hz: f64| {
            let r = dft(&ir, f64::from(RATE), hz);
            r.1.atan2(r.0)
        };
        let mut turn = phase(1_001.0) - phase(999.0);
        while turn > PI {
            turn -= TAU;
        }
        while turn < -PI {
            turn += TAU;
        }
        let delay = -turn / (TAU * 2.0) * 1e3;
        eprintln!(
            "diffuser, {} stages: {delay:.2} ms at the cutoff",
            stages + 1
        );
        assert!(delay > previous, "{stages}");
        previous = delay;
    }
}

// Spec: engine-core R61 (the ladder rows): Smooth lags the cutoff by a
// one-pole glide whose time constant is 100 ms × Character².
#[test]
fn smooth_lags_the_cutoff() {
    for kind in extended().filter(|k| k.character() == Character::Smooth) {
        let frames = 24_000;
        let step: Vec<f32> = (0..frames)
            .map(|f| if f < 2_000 { 50.0 } else { 90.0 })
            .collect();
        let input = signal::noise(6, frames, 0.3);
        let mut design = PoleZeroDesign::default();
        design.states[0].points[0] = Point::new(PointKind::LowPass, 2_000.0, 2.0);
        for character in [0.0f32, 0.5, 1.0] {
            let params = VoiceParams {
                resonance: 0.5,
                character,
                ..VoiceParams::default()
            };
            let mut smooth = bank(kind, RATE, &params);
            smooth.set_pole_zero(&design);
            let mut a = input.clone();
            smooth.process(&step, &mut a);
            // The same lag, applied to the pitch by hand, with Smooth off.
            let tau = 0.1 * f64::from(character * character) * f64::from(RATE);
            let coefficient = 1.0 / (1.0 + tau);
            let mut lagged = Vec::with_capacity(frames);
            let mut p = f64::from(step[0]);
            for &target in &step {
                p += (f64::from(target) - p) * coefficient;
                lagged.push(p as f32);
            }
            let mut plain = bank(
                kind,
                RATE,
                &VoiceParams {
                    character: 0.0,
                    ..params
                },
            );
            plain.set_pole_zero(&design);
            let mut b = input.clone();
            plain.process(&lagged, &mut b);
            let worst = a
                .iter()
                .zip(&b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max);
            assert!(worst < 2e-3, "{kind}, Smooth {character}: {worst}");
        }
    }
}

// Spec: engine-core R61 (the driven ladder's row): overdrive saturates.
#[test]
fn overdrive_adds_distortion() {
    let thd = |character| {
        let params = VoiceParams {
            resonance: 0.0,
            character,
            ..VoiceParams::default()
        };
        let out = sine_through(FilterType::DrivenLadder, &params, 15_000.0, 441.0, 0.25);
        db(measure::thd(&out, f64::from(RATE), 441.0, 9))
    };
    let (clean, driven) = (thd(0.0), thd(1.0));
    eprintln!("driven ladder, −12 dBFS: THD {clean:.1} dB at 0, {driven:.1} dB at 1");
    assert!(driven > clean + 20.0 && driven > -20.0, "{clean} {driven}");
}

// Spec: engine-core R61 (the K35's row): LP at 0 (its default), HP at 1.
#[test]
fn the_k35_switches_between_low_pass_and_high_pass() {
    let at = |character| VoiceParams {
        resonance: 0.0,
        character,
        ..VoiceParams::default()
    };
    assert_eq!(VoiceParams::default().character, 0.0);
    let k = FilterType::K35;
    assert!(gain_at(k, &at(0.0), 1_000.0, 100.0).abs() < 0.1);
    assert!(gain_at(k, &at(0.0), 1_000.0, 8_000.0) < -30.0);
    assert!(gain_at(k, &at(1.0), 1_000.0, 125.0) < -30.0);
    assert!(gain_at(k, &at(1.0), 1_000.0, 15_000.0).abs() < 0.2);
    // Below 0.45 and from 0.55 up, the control is one or the other.
    assert_eq!(
        sine_through(k, &at(0.3), 1_000.0, 500.0, 0.1),
        sine_through(k, &at(0.0), 1_000.0, 500.0, 0.1)
    );
}

// Spec: engine-core R61 (the expander rows): the band-pass is unity at the
// cutoff; the multimode's middle is a notch there.
#[test]
fn the_expanders_mix_their_taps() {
    let at = |character| VoiceParams {
        resonance: 0.0,
        character,
        ..VoiceParams::default()
    };
    assert!(gain_at(FilterType::ExpanderBandPass, &at(0.0), 1_000.0, 1_000.0).abs() < 0.05);
    assert!(gain_at(FilterType::ExpanderMultimode, &at(0.5), 1_000.0, 1_000.0) < -40.0);
    assert!(gain_at(FilterType::ExpanderMultimode, &at(1.0), 1_000.0, 250.0) < -40.0);
}

/// A design that does something in every state: all eight points active,
/// each kind somewhere.
fn busy_design() -> PoleZeroDesign {
    let kinds = PointKind::ALL;
    let mut design = PoleZeroDesign {
        gain_db: -3.0,
        ..PoleZeroDesign::default()
    };
    for (s, state) in design.states.iter_mut().enumerate() {
        for (p, point) in state.points.iter_mut().enumerate() {
            let hz = 80.0 * 1.9f32.powi(p as i32) * (1.0 + 0.3 * s as f32);
            *point = match kinds[(p + 3 * s) % kinds.len()] {
                PointKind::Bell => Point::bell(hz, 1.5, if s % 2 == 0 { 9.0 } else { -12.0 }),
                PointKind::Free => Point::free(hz, 3.0, hz * 1.5, 0.8),
                kind => Point::new(kind, hz, 0.7 + 0.5 * p as f32),
            };
        }
    }
    design
}

/// The pole-zero filter's digital response at `hz`: each point's prototype
/// on its own prewarped axis.
fn pole_zero_expected(
    design: &PoleZeroDesign,
    x: f32,
    y: f32,
    pitch: f32,
    rate: f64,
    hz: f64,
) -> C {
    let transpose = f64::from(pitch - pole_zero::NEUTRAL_PITCH) / 12.0;
    let mut acc = re(f64::from(design.gain()));
    for p in 0..pole_zero::POINTS {
        if design.is_bypassed(p) {
            continue;
        }
        let section = design.section(p, x, y);
        let fp = 425.012 * (f64::from(section.octaves) + transpose).exp2();
        let warped = fp * (PI * hz / rate).tan() / (PI * fp / rate).tan();
        let (r, i) = section.response(warped, transpose);
        acc = acc * C(r, i);
    }
    acc
}

// Spec: engine-core R61 (the pole-zero row): at each corner the filter is
// that state's points in series; between them, the documented blend.
#[test]
fn the_pole_zero_filter_matches_its_design() {
    let design = busy_design();
    for (rate, pitch) in [(48_000.0f32, pole_zero::NEUTRAL_PITCH), (96_000.0, 60.0)] {
        for (x, y) in [
            (0.0, 0.0),
            (1.0, 0.0),
            (0.0, 1.0),
            (1.0, 1.0),
            (0.5, 0.5),
            (0.2, 0.7),
        ] {
            let params = VoiceParams {
                morph_x: x,
                morph_y: y,
                ..VoiceParams::default()
            };
            let mut b = bank(FilterType::PoleZero, rate, &params);
            b.set_pole_zero(&design);
            let ir = impulse_response(b, pitch, 1 << 16);
            for hz in measure::log_frequencies(20.0, 0.4 * f64::from(rate), 60) {
                let want = pole_zero_expected(&design, x, y, pitch, f64::from(rate), hz);
                let got = dft(&ir, f64::from(rate), hz);
                assert!(
                    (got - want).abs() <= 2e-3 * want.abs() + 3e-5,
                    "({x}, {y}) at {rate} Hz, pitch {pitch}, {hz:.0} Hz: {:.3} dB, design {:.3} dB",
                    db(got.abs()),
                    db(want.abs())
                );
            }
        }
    }
    // At a corner, the response is that state's points alone.
    let mut corner = design;
    corner.states = [design.states[1]; 4];
    for hz in [50.0f32, 300.0, 2_000.0, 9_000.0] {
        let blended = design.magnitude_db(1.0, 0.0, hz);
        let alone = corner.magnitude_db(0.3, 0.8, hz);
        assert!((blended - alone).abs() < 1e-3, "{hz}: {blended} {alone}");
    }
}

// The kinds the editor offers, one point at a time, at the editor's
// display: −3 dB low-pass and high-pass at Q 0.707, a bell's gain, a
// notch's null, and a flat all-pass and bypass.
#[test]
fn each_point_kind_measures_as_drawn() {
    let measure_at = |point: Point, hz: f64| {
        let design = PoleZeroDesign::uniform(State {
            points: std::array::from_fn(|i| {
                if i == 2 {
                    point
                } else {
                    State::default().points[i]
                }
            }),
        });
        let mut b = bank(FilterType::PoleZero, RATE, &VoiceParams::default());
        b.set_pole_zero(&design);
        let ir = impulse_response(b, pole_zero::NEUTRAL_PITCH, 1 << 15);
        db(dft(&ir, f64::from(RATE), hz).abs())
    };
    let q = std::f32::consts::FRAC_1_SQRT_2;
    assert!((measure_at(Point::new(PointKind::LowPass, 1_000.0, q), 1_000.0) + 3.01).abs() < 0.02);
    assert!(measure_at(Point::new(PointKind::LowPass, 1_000.0, q), 8_000.0) < -35.0);
    assert!((measure_at(Point::new(PointKind::HighPass, 1_000.0, q), 1_000.0) + 3.01).abs() < 0.02);
    assert!(measure_at(Point::new(PointKind::BandPass, 1_000.0, 4.0), 1_000.0).abs() < 0.02);
    assert!(measure_at(Point::new(PointKind::Notch, 1_000.0, 4.0), 1_000.0) < -60.0);
    assert!((measure_at(Point::bell(1_000.0, 2.0, 18.0), 1_000.0) - 18.0).abs() < 0.02);
    assert!((measure_at(Point::bell(1_000.0, 2.0, -18.0), 1_000.0) + 18.0).abs() < 0.02);
    for kind in [PointKind::AllPass, PointKind::Bypass] {
        for hz in [50.0, 1_000.0, 15_000.0] {
            assert!(measure_at(Point::new(kind, 1_000.0, 4.0), hz).abs() < 0.01);
        }
    }
}

// Cutoff transposes the whole design: up an octave, every point moves up
// an octave.
#[test]
fn cutoff_transposes_the_pole_zero_design() {
    let design = PoleZeroDesign::uniform(State {
        points: std::array::from_fn(|i| {
            if i == 0 {
                Point::new(PointKind::Notch, 1_000.0, 2.0)
            } else {
                State::default().points[i]
            }
        }),
    });
    let gain = |pitch: f32, hz: f64| {
        let mut b = bank(FilterType::PoleZero, RATE, &VoiceParams::default());
        b.set_pole_zero(&design);
        let ir = impulse_response(b, pitch, 1 << 15);
        db(dft(&ir, f64::from(RATE), hz).abs())
    };
    assert!(gain(pole_zero::NEUTRAL_PITCH, 1_000.0) < -60.0);
    assert!(gain(pole_zero::NEUTRAL_PITCH + 12.0, 2_000.0) < -60.0);
    assert!(gain(pole_zero::NEUTRAL_PITCH + 12.0, 1_000.0) > -10.0);
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

/// A random design: every point random in kind, frequency, Q and gain.
fn random_design(rng: &mut Rng) -> PoleZeroDesign {
    let mut design = PoleZeroDesign {
        gain_db: 24.0 * (rng.unit() * 2.0 - 1.0),
        ..PoleZeroDesign::default()
    };
    for state in &mut design.states {
        for point in &mut state.points {
            let kind = PointKind::ALL[(rng.unit() * 8.0) as usize % 8];
            let hz = 8.0 * (22_050.0f32 / 8.0).powf(rng.unit());
            let q = 0.1 * 400f32.powf(rng.unit());
            *point = Point {
                kind,
                frequency: hz,
                q,
                gain_db: 36.0 * rng.unit() - 18.0,
                zero_frequency: 8.0 * (22_050.0f32 / 8.0).powf(rng.unit()),
                zero_q: 0.1 * 400f32.powf(rng.unit()),
            };
        }
    }
    design
}

// Morphing is safe: random designs, X and Y jumping at random every
// sample, cutoff swept at audio rate across beyond its range, at every
// sample rate, stay finite; and with the input stopped, the output dies
// away however X and Y keep moving, so no path pumps energy in.
#[test]
fn any_morph_path_is_stable() {
    const VOICES: usize = 8;
    let mut rng = Rng(19);
    for rate in [44_100.0f32, 48_000.0, 96_000.0, 192_000.0, 768_000.0] {
        for _ in 0..6 {
            let design = random_design(&mut rng);
            let mut b = FilterBank::new(rate, VOICES, 2);
            b.set_type(FilterType::PoleZero);
            b.set_pole_zero(&design);
            let stride = b.stride();
            for v in 0..VOICES {
                b.reset_voice(
                    v,
                    &VoiceParams {
                        character: v as f32 / 7.0,
                        drive: (v % 2) as f32,
                        ..VoiceParams::default()
                    },
                );
            }
            let frames = 4_096;
            let mut pitch = vec![0.0; frames * stride];
            let mut xs = vec![0.0; frames * stride];
            let mut ys = vec![0.0; frames * stride];
            let mut audio = vec![0.0; frames * 2 * stride];
            for f in 0..frames {
                let t = f as f32 / rate;
                for v in 0..VOICES {
                    let i = f * stride + v;
                    pitch[i] =
                        65.0 + 90.0 * (TAU as f32 * [3.0, 700.0][v % 2] * t + v as f32).sin();
                    xs[i] = rng.unit() * 1.4 - 0.2;
                    ys[i] = rng.unit() * 1.4 - 0.2;
                    for c in 0..2 {
                        audio[(f * 2 + c) * stride + v] = 4.0 * (rng.unit() * 2.0 - 1.0);
                    }
                }
            }
            b.process_xy(&pitch, &xs, &ys, &mut audio);
            assert!(audio.iter().all(|y| y.is_finite()), "{rate} Hz");
        }
    }
    // Decay: points from 50 Hz up with Q up to 10, so every state rings
    // out within the half second; the cutoff moving within an octave.
    for seed in 0..12 {
        let mut rng = Rng(100 + seed);
        let mut design = random_design(&mut rng);
        for state in &mut design.states {
            for point in &mut state.points {
                point.frequency = point.frequency.max(50.0);
                point.zero_frequency = point.zero_frequency.max(50.0);
                point.q = point.q.min(10.0);
                point.zero_q = point.zero_q.min(10.0);
            }
        }
        let mut b = FilterBank::new(RATE, 4, 1);
        b.set_type(FilterType::PoleZero);
        b.set_pole_zero(&design);
        let stride = b.stride();
        let frames = 36_000;
        let loud = 4_000;
        let mut pitch = vec![0.0; frames * stride];
        let mut xs = vec![0.0; frames * stride];
        let mut ys = vec![0.0; frames * stride];
        let mut audio = vec![0.0; frames * stride];
        for f in 0..frames {
            for v in 0..4 {
                let i = f * stride + v;
                pitch[i] = 68.4 + 12.0 * (f as f32 * 0.01 + v as f32).sin();
                xs[i] = rng.unit();
                ys[i] = rng.unit();
                if f < loud {
                    audio[i] = rng.unit() * 2.0 - 1.0;
                }
            }
        }
        b.process_xy(&pitch, &xs, &ys, &mut audio);
        let peak = |range: std::ops::Range<usize>| {
            audio[range.start * stride..range.end * stride]
                .iter()
                .fold(0.0f32, |m, y| m.max(y.abs()))
        };
        let (driven, after, end) = (
            peak(0..loud),
            peak(loud..loud + 2_000),
            peak(frames - 2_000..frames),
        );
        assert!(
            end < 1e-3 * after.max(driven),
            "seed {seed}: {driven} while driven, {after} after, {end} at the end"
        );
    }
}

// X and Y at audio rate move the response sample by sample, without the
// steps a block-rate update makes.
#[test]
fn audio_rate_morphing_has_no_zipper_noise() {
    // State 1 a +18 dB bell at 1 kHz, state 2 a −18 dB one: X sweeps the
    // level of a 1 kHz sine by 36 dB.
    let mut design = PoleZeroDesign::default();
    for (s, state) in design.states.iter_mut().enumerate() {
        state.points[0] = Point::bell(1_000.0, 1.0, if s % 2 == 0 { 18.0 } else { -18.0 });
    }
    let frames = 24_000;
    let input = signal::sine(1_000.0, f64::from(RATE), frames, 0.05);
    let pitch = vec![pole_zero::NEUTRAL_PITCH; frames];
    let zeros = vec![0.0; frames];
    let run = |xs: &[f32]| {
        let mut b = bank(FilterType::PoleZero, RATE, &VoiceParams::default());
        b.set_pole_zero(&design);
        let mut audio = input.clone();
        b.process_xy(&pitch, xs, &zeros, &mut audio);
        audio
    };
    // The largest second difference, against the sine's own.
    let roughness = |audio: &[f32]| {
        let peak = audio.iter().fold(0.0f32, |m, y| m.max(y.abs()));
        let d2 = audio
            .windows(3)
            .map(|w| (w[2] - 2.0 * w[1] + w[0]).abs())
            .fold(0.0f32, f32::max);
        f64::from(d2 / peak) / (2.0 * (PI * 1_000.0 / f64::from(RATE)).sin()).powi(2)
    };
    let smooth: Vec<f32> = (0..frames).map(|f| f as f32 / frames as f32).collect();
    let stepped: Vec<f32> = smooth
        .iter()
        .enumerate()
        .map(|(f, _)| (f / 512 * 512) as f32 / frames as f32)
        .collect();
    let (a, b) = (roughness(&run(&smooth)), roughness(&run(&stepped)));
    eprintln!(
        "X swept over half a second: second difference {a:.3} × a steady sine's, stepped every 512 samples {b:.3} ×"
    );
    assert!(a < 1.05, "{a}");
    assert!(b > a * 1.5, "{a} {b}");
}

// The audio-rate path keeps the bank's guarantees: the same output
// whatever the block sizes, SIMD within rounding of scalar, nothing
// allocated.
#[test]
fn the_audio_rate_path_is_block_invariant_simd_exact_and_off_the_heap() {
    const VOICES: usize = 5;
    let design = busy_design();
    let make = |simd: bool| {
        let mut b = if simd {
            FilterBank::new(RATE, VOICES, 2)
        } else {
            FilterBank::scalar(RATE, VOICES, 2)
        };
        b.set_type(FilterType::PoleZero);
        b.set_pole_zero(&design);
        for v in 0..VOICES {
            b.reset_voice(
                v,
                &VoiceParams {
                    character: v as f32 / 8.0,
                    morph_x: 0.2,
                    morph_y: 0.6,
                    ..VoiceParams::default()
                },
            );
        }
        b
    };
    let frames = 2_000;
    let inputs = |stride: usize| {
        let mut rng = Rng(5);
        let mut pitch = vec![0.0; frames * stride];
        let mut xs = vec![0.0; frames * stride];
        let mut ys = vec![0.0; frames * stride];
        let mut audio = vec![0.0; frames * 2 * stride];
        for f in 0..frames {
            for v in 0..VOICES {
                let i = f * stride + v;
                let t = f as f32 / 300.0 + v as f32;
                (pitch[i], xs[i], ys[i]) =
                    (60.0 + 20.0 * t.sin(), 0.5 * t.cos(), 0.3 * (2.0 * t).sin());
                for c in 0..2 {
                    audio[(f * 2 + c) * stride + v] = rng.unit() - 0.5;
                }
            }
        }
        (pitch, xs, ys, audio)
    };
    let run = |mut b: FilterBank, size: usize| {
        let stride = b.stride();
        let (pitch, xs, ys, mut audio) = inputs(stride);
        let mut at = 0;
        while at < frames {
            let n = size.min(frames - at);
            let (p, a) = (
                at * stride..(at + n) * stride,
                at * 2 * stride..(at + n) * 2 * stride,
            );
            b.process_xy(&pitch[p.clone()], &xs[p.clone()], &ys[p], &mut audio[a]);
            at += n;
        }
        audio
    };
    for simd in [false, true] {
        let whole = run(make(simd), usize::MAX);
        for size in [1, 7, 128] {
            assert!(
                whole == run(make(simd), size),
                "simd {simd}, blocks of {size}"
            );
        }
    }
    let (scalar, lanes) = (run(make(false), 256), run(make(true), 256));
    let l = make(true).stride();
    for f in 0..frames {
        for v in 0..VOICES {
            for c in 0..2 {
                let (a, b) = (scalar[(f * 2 + c) * VOICES + v], lanes[(f * 2 + c) * l + v]);
                assert!((a - b).abs() < 1e-4, "frame {f}, voice {v}: {a} {b}");
            }
        }
    }
    let mut b = make(true);
    let stride = b.stride();
    let (pitch, xs, ys, mut audio) = inputs(stride);
    lspk_rt::rt_test!({
        b.set_pole_zero(&design);
        b.process_xy(&pitch, &xs, &ys, &mut audio);
        b.set_type(FilterType::Comb2);
        b.process(&pitch, &mut audio);
        b.reset_voice(1, &VoiceParams::default());
    });
}

// The design round-trips through JSON as a preset stores it, a point left
// out takes its default, and the editor's magnitude matches the filter's.
#[test]
fn the_pole_zero_design_is_preset_data() {
    let design = busy_design();
    let json = serde_json::to_string(&design).expect("serialises");
    let back: PoleZeroDesign = serde_json::from_str(&json).expect("parses");
    assert_eq!(back, design);
    let partial: PoleZeroDesign = serde_json::from_str(r#"{"gain_db": 3.0}"#).expect("parses");
    assert_eq!(partial.gain_db, 3.0);
    assert_eq!(partial.states, PoleZeroDesign::default().states);
    let kind: PointKind = serde_json::from_str(r#""high-pass""#).expect("parses");
    assert_eq!(kind, PointKind::HighPass);
    assert_eq!(PointKind::default(), PointKind::Bypass);
    // The default design is flat: every point a bypass, gain 0 dB.
    let input = signal::noise(3, 1_024, 0.5);
    let mut audio = input.clone();
    let mut b = bank(FilterType::PoleZero, RATE, &VoiceParams::default());
    b.process(&vec![60.0; 1_024], &mut audio);
    assert_eq!(audio, input);
    // What the editor draws, at the cutoff's middle, is what the filter
    // does there (well below Nyquist, where prewarping is negligible).
    let mut b = bank(
        FilterType::PoleZero,
        192_000.0,
        &VoiceParams {
            morph_x: 0.3,
            morph_y: 0.9,
            ..VoiceParams::default()
        },
    );
    b.set_pole_zero(&design);
    let ir = impulse_response(b, pole_zero::NEUTRAL_PITCH, 1 << 17);
    for hz in [40.0f32, 200.0, 1_000.0, 3_000.0] {
        let drawn = design.magnitude_db(0.3, 0.9, hz);
        let measured = db(dft(&ir, 192_000.0, f64::from(hz)).abs()) as f32;
        assert!((drawn - measured).abs() < 0.05, "{hz}: {drawn} {measured}");
    }
}

// The type list: the extended group follows the normal group, in the
// spec's order, K35 last, each with its Character as the spec names it.
#[test]
fn the_extended_group_is_listed_in_order_with_its_characters() {
    let names: Vec<&str> = extended().map(FilterType::name).collect();
    assert_eq!(
        names,
        [
            "buzz-morph",
            "dj-sweep",
            "diffuser",
            "transistor-ladder",
            "diode-ladder",
            "clean-ladder",
            "driven-ladder",
            "pole-zero",
            "comb-2",
            "expander-mm",
            "expander-bp",
            "k35"
        ]
    );
    let characters: Vec<Character> = extended().map(FilterType::character).collect();
    use Character::*;
    assert_eq!(
        characters,
        [
            Morph,
            None,
            Stages,
            Smooth,
            Smooth,
            Smooth,
            Overdrive,
            Smooth,
            SecondFrequency,
            Morph,
            None,
            LowHigh
        ]
    );
}
