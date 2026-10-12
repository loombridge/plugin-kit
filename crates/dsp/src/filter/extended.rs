//! The extended group's kernels: the buzz morph, DJ sweep, diffuser, the
//! four ladder characters, the pole-zero filter, Comb II, the expander
//! multimode and band-pass, and the K35.
//!
//! They run through a block loop of their own beside the normal group's,
//! with the same smoothing, drive, stereo offset, mix, level and delay
//! lines, plus what only these types need: the pole-zero X and Y as
//! smoothed settings with offsets at audio rate, and cutoff smoothing
//! (Smooth).
//!
//! See the [module documentation](super) for each type's structure and
//! laws.

use std::marker::PhantomData;

use super::lane::{Lane, TAN_LIMIT, exp2, saturation_gain, soft_clip, tan};
use super::pole_zero::{NEUTRAL_PITCH, Table};
use super::{
    CHARACTER, CLEAN_DB, DRIVE, DRIVE_GAIN, FilterBank, FilterType, INV_KNEE, Io, K_BUTTERWORTH_2,
    LADDER_MAX, LEVEL, Lines, MAX_STATES, MIX, OFFSET, PARAMS, RES, SMOOTHING_SECONDS, saturated_k,
    svf, svf_k,
};
use crate::simd::{self, F32s, Kernel, Level};

/// Indices of the pole-zero X and Y among the morph settings.
const MORPH_X: usize = 0;
const MORPH_Y: usize = 1;
/// How many there are.
pub(super) const MORPH: usize = 2;
/// How many state variables each slot has for these types.
pub(super) const X_STATES: usize = 20;
/// Where the cutoff smoothing keeps its state: whether it has started,
/// then the smoothed pitch, X and Y.
const LAG_STARTED: usize = 16;
const LAG_PITCH: usize = 17;
const LAG_X: usize = 18;
const LAG_Y: usize = 19;

/// Smooth's longest time constant, at Character 1, in seconds.
pub(super) const SMOOTH_SECONDS: f32 = 0.1;
/// Comb II's second frequency at Character 1, in octaves above the first.
pub(super) const COMB_OCTAVES: f32 = 2.0;
/// The diffuser's most stages, at Character 1.
pub(super) const DIFFUSER_STAGES: usize = 8;
/// A diffuser stage's damping at resonance 0 (Q 0.5) and 1 (Q 5).
pub(super) const DIFFUSER_K: [f32; 2] = [2.0, 0.2];
/// The DJ sweep's damping at resonance 1: about +9 dB of peak.
pub(super) const DJ_K_MIN: f32 = 0.35;
/// The DJ sweep's flat zone around the cutoff control's middle, as a
/// fraction of each half, and how far past it the filter fades in.
pub(super) const DJ_FLAT: f32 = 0.04;
pub(super) const DJ_FADE: f32 = 0.1;
/// The DJ sweep's cutoff pitches: the low-pass from 20 kHz down to 30 Hz,
/// the high-pass from 10 Hz up to 10 kHz (at A4 = 440 Hz).
pub(super) const DJ_LOW_PASS: [f32; 2] = [135.076, 22.651];
pub(super) const DJ_HIGH_PASS: [f32; 2] = [3.486, 123.514];
/// The transistor ladders' stage knee: each stage's gain falls as
/// 1 / √(1 + (state / knee)²).
pub(super) const TRANSISTOR_KNEE: f32 = 4.0;
/// The driven ladder's gain into its clipper at Character 1, in decibels.
pub(super) const DRIVEN_DB: f32 = 30.0;
/// How far below the normal types' the driven ladder's resonance knee
/// sits.
const DRIVEN_KNEE: f32 = 4.0;
/// The diode ladders' coupling: how much of each stage's current a stage
/// passes back to the one before it (0 is a transistor ladder, 1 an RC
/// ladder of equal parts).
pub(super) const DIODE_COUPLING: f32 = 0.5;
/// The diode ladder's oscillation frequency, in units of its stage
/// frequency, for that coupling: the stage frequency is the cutoff divided
/// by this, so it oscillates at the cutoff.
pub(super) const DIODE_W: f32 = 1.055_289_7;
/// The diode ladder's feedback at the edge of self-oscillation, for that
/// coupling.
pub(super) const DIODE_K: f32 = 8.617_769;
/// How far past the edge the diode ladders' feedback goes at resonance 1.
pub(super) const DIODE_OVERSHOOT: f32 = 1.025;
/// The K35's feedback at resonance 1; it oscillates from 2.
pub(super) const K35_MAX: f32 = 2.04;
/// Where the K35's LP/HP control crosses from low-pass to high-pass, and
/// over how much of its range it crossfades.
pub(super) const K35_SWITCH: [f32; 2] = [0.45, 0.55];
/// The asymmetric shaper's bias, for the buzz morph and acid ladder.
const ASYM_BIAS: f32 = 0.3;
/// The shaper's slope at zero: [`soft_clip`]'s at the bias.
const ASYM_SLOPE: f32 = {
    let x = ASYM_BIAS;
    let x2 = x * x;
    let den = 27.0 + 9.0 * x2;
    ((27.0 + 3.0 * x2) * den - x * (27.0 + x2) * 18.0 * x) / (den * den)
};

/// An asymmetric soft clipper with unity slope at zero, so small signals
/// pass unchanged and large ones gain even harmonics.
#[inline(always)]
fn asym<L: Lane>(x: L) -> L {
    let bias = L::splat(ASYM_BIAS);
    (soft_clip(x + bias) - soft_clip(bias)) * L::splat(1.0 / ASYM_SLOPE)
}

/// Each lane clamped to 0 to 1.
#[inline(always)]
fn unit<L: Lane>(x: L) -> L {
    x.max_(L::splat(0.0)).min_(L::splat(1.0))
}

/// Each lane's magnitude.
#[inline(always)]
fn abs<L: Lane>(x: L) -> L {
    x.max_(-x)
}

/// The prewarped integrator gain g = tan(π f / rate) of `pitch`.
#[inline(always)]
fn gain<L: Lane>(pitch: L, ref_over_rate: L) -> L {
    let octaves = (pitch - L::splat(69.0)) * L::splat(1.0 / 12.0);
    let ratio = (exp2(octaves) * ref_over_rate).min_(L::splat(TAN_LIMIT / std::f32::consts::PI));
    tan(ratio * L::splat(std::f32::consts::PI))
}

/// One sample's settings, smoothed.
pub(super) struct Settings<'a, L> {
    resonance: L,
    inv_knee: L,
    character: L,
    x: L,
    y: L,
    ref_over_rate: L,
    /// The reference pitch over the sample rate, and one over the rate, for
    /// the delay lines.
    reference_ratio: f32,
    inv_rate: f32,
    pole_zero: &'a Table,
}

/// One extended type's sample: the driven input and the cutoff pitch in;
/// the output out.
pub(super) trait Structure {
    /// Whether Character is Smooth: it lags the cutoff (and the pole-zero
    /// X and Y).
    const SMOOTH: bool = false;
    /// Whether it runs on the delay lines.
    const LINES: bool = false;
    fn tick<L: Lane>(
        lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L;
}

/// Runs a block of an extended type, scalar or at `level`.
pub(super) fn dispatch(
    bank: &mut FilterBank,
    level: Option<Level>,
    pitch: &[f32],
    xy: Option<(&[f32], &[f32])>,
    audio: &mut [f32],
    frames: usize,
) -> bool {
    fn go<T: Structure>(
        bank: &mut FilterBank,
        level: Option<Level>,
        pitch: &[f32],
        xy: Option<(&[f32], &[f32])>,
        audio: &mut [f32],
        frames: usize,
    ) -> bool {
        let run = Run::<T> {
            bank,
            pitch,
            xy,
            audio,
            frames,
            structure: PhantomData,
        };
        match level {
            None => {
                run.go::<f32>();
                true
            }
            Some(level) => simd::dispatch_at(level, run).is_ok(),
        }
    }
    let run = match bank.kind {
        FilterType::BuzzMorph => go::<BuzzMorph>,
        FilterType::DjSweep => go::<DjSweep>,
        FilterType::Diffuser => go::<Diffuser>,
        FilterType::TransistorLadder => go::<TransistorLadder<false>>,
        FilterType::DrivenLadder => go::<TransistorLadder<true>>,
        FilterType::DiodeLadder => go::<DiodeLadder<false>>,
        FilterType::CleanLadder => go::<DiodeLadder<true>>,
        FilterType::PoleZero => go::<PoleZero>,
        FilterType::Comb2 => go::<Comb2>,
        FilterType::ExpanderMultimode => go::<Expander<false>>,
        FilterType::ExpanderBandPass => go::<Expander<true>>,
        FilterType::K35 => go::<K35>,
        _ => unreachable!("not an extended type"),
    };
    run(bank, level, pitch, xy, audio, frames)
}

struct Run<'a, T> {
    bank: &'a mut FilterBank,
    pitch: &'a [f32],
    xy: Option<(&'a [f32], &'a [f32])>,
    audio: &'a mut [f32],
    frames: usize,
    structure: PhantomData<T>,
}

impl<T: Structure> Kernel for Run<'_, T> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        self.go::<S>();
    }
}

impl<T: Structure> Run<'_, T> {
    #[inline(always)]
    fn go<L: Lane>(self) {
        let bank = self.bank;
        let one = L::splat(1.0);
        let groups = bank.voices.div_ceil(L::LANES);
        let alpha = L::splat(1.0 - (-1.0 / (SMOOTHING_SECONDS * bank.rate)).exp());
        let clean = 10f32.powf(-CLEAN_DB / 20.0);
        let (clip_in, clip_out) = if bank.clean {
            (L::splat(clean), L::splat(1.0 / clean))
        } else {
            (one, one)
        };
        let smooth_samples = L::splat(SMOOTH_SECONDS * bank.rate);
        let reference_ratio = bank.reference / bank.rate;
        let inv_rate = 1.0 / bank.rate;
        let stride = bank.stride;
        let frame_stride = bank.channels * stride;
        for c in 0..bank.channels {
            let sign = L::splat(match (bank.channels, c) {
                (1, _) => 0.0,
                (_, 0) => -1.0,
                _ => 1.0,
            });
            for g in 0..groups {
                let voice = g * L::LANES;
                let slot = c * stride + voice;
                let targets: [L; PARAMS] =
                    std::array::from_fn(|i| L::load(&bank.targets[i][voice..]));
                let mut params: [L; PARAMS] =
                    std::array::from_fn(|i| L::load(&bank.smoothed[i][slot..]));
                let morph_targets: [L; MORPH] =
                    std::array::from_fn(|i| L::load(&bank.morph_targets[i][voice..]));
                let mut morph: [L; MORPH] =
                    std::array::from_fn(|i| L::load(&bank.morph_smoothed[i][slot..]));
                let mut state: [L; X_STATES] =
                    std::array::from_fn(|i| L::load(&bank.state[i][slot..]));
                let mut lines = Lines {
                    buf: &mut bank.delay,
                    len: bank.ring,
                    cursor: bank.cursor,
                    slot,
                };
                let ref_over_rate = L::splat(bank.reference / bank.rate);
                for f in 0..self.frames {
                    let at = f * frame_stride + slot;
                    let input = L::load(&self.audio[at..]);
                    let mut pitch = L::load(&self.pitch[f * stride + voice..]);
                    for (p, t) in params.iter_mut().zip(&targets) {
                        *p = (*t - *p).fma(alpha, *p);
                    }
                    for (p, t) in morph.iter_mut().zip(&morph_targets) {
                        *p = (*t - *p).fma(alpha, *p);
                    }
                    pitch = params[OFFSET].fma(sign, pitch);
                    let (mut x, mut y) = (morph[MORPH_X], morph[MORPH_Y]);
                    if let Some((xs, ys)) = self.xy {
                        x = x + L::load(&xs[f * stride + voice..]);
                        y = y + L::load(&ys[f * stride + voice..]);
                    }
                    (x, y) = (unit(x), unit(y));
                    if T::SMOOTH {
                        // A one-pole lag, time constant SMOOTH_SECONDS × c²,
                        // that starts at its first input.
                        let c = params[CHARACTER];
                        let a = one / (c * c).fma(smooth_samples, one);
                        let a = one - state[LAG_STARTED] * (one - a);
                        state[LAG_PITCH] = (pitch - state[LAG_PITCH]).fma(a, state[LAG_PITCH]);
                        state[LAG_X] = (x - state[LAG_X]).fma(a, state[LAG_X]);
                        state[LAG_Y] = (y - state[LAG_Y]).fma(a, state[LAG_Y]);
                        state[LAG_STARTED] = one;
                        (pitch, x, y) = (state[LAG_PITCH], state[LAG_X], state[LAG_Y]);
                    }
                    let u = input * params[DRIVE_GAIN];
                    let clipped = soft_clip(u * clip_in) * clip_out;
                    let driven = (clipped - u).fma(params[DRIVE], u);
                    let settings = Settings {
                        resonance: params[RES],
                        inv_knee: params[INV_KNEE],
                        character: params[CHARACTER],
                        x,
                        y,
                        ref_over_rate,
                        reference_ratio,
                        inv_rate,
                        pole_zero: &bank.pole_zero,
                    };
                    let wet = T::tick(&mut lines, &mut state, driven, pitch, &settings);
                    if T::LINES {
                        lines.advance();
                    }
                    let out = wet.fma(params[MIX], input * (one - params[MIX])) * params[LEVEL];
                    out.store(&mut self.audio[at..]);
                }
                for (i, value) in params.iter().enumerate() {
                    value.store(&mut bank.smoothed[i][slot..]);
                }
                for (i, value) in morph.iter().enumerate() {
                    value.store(&mut bank.morph_smoothed[i][slot..]);
                }
                for (i, value) in state.iter().enumerate() {
                    value.store(&mut bank.state[i][slot..]);
                }
            }
        }
        if T::LINES {
            bank.cursor = bank.cursor.wrapping_add(self.frames);
        }
    }
}

/// The buzz morph: a resonant state-variable stage with an asymmetric
/// shaper before it and a low knee in its resonance path, its output
/// morphing from low-pass (Character 0) through notch (0.5) to high-pass
/// (1). State: 0 and 1 the stage, 2 its previous band output.
struct BuzzMorph;

/// How far below the normal types' the buzz morph's resonance knee sits.
const BUZZ_KNEE: f32 = 3.0;

impl Structure for BuzzMorph {
    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let k0 = K_BUTTERWORTH_2;
        let k = saturated_k(
            k0,
            svf_k(k0, f32::NEG_INFINITY, p.resonance),
            state[2],
            p.inv_knee * L::splat(BUZZ_KNEE),
        );
        let u = asym(x);
        let g = gain(pitch, p.ref_over_rate);
        let [s0, s1, s2, ..] = state;
        let (band, low) = svf(s0, s1, u, g, k);
        *s2 = band;
        let high = u - k * band - low;
        let two = L::splat(2.0);
        let w_low = ((L::splat(1.0) - p.character) * two).min_(L::splat(1.0));
        let w_high = (p.character * two).min_(L::splat(1.0));
        w_low.fma(low, w_high * high)
    }
}

/// The DJ sweep: one state-variable stage whose cutoff control is the
/// sweep, low-pass below its middle and high-pass above, flat around it.
/// State: 0 and 1 the stage.
struct DjSweep;

impl Structure for DjSweep {
    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let one = L::splat(1.0);
        let middle = NEUTRAL_PITCH;
        let d = ((pitch - L::splat(middle)) * L::splat(1.0 / middle))
            .max_(-one)
            .min_(one);
        // 1 below the middle (low-pass), 0 from it up (high-pass).
        let low_side = unit(-d * L::splat(1e6));
        let u =
            ((abs(d) - L::splat(DJ_FLAT)) * L::splat(1.0 / (1.0 - DJ_FLAT))).max_(L::splat(0.0));
        let wet = (u * L::splat(1.0 / DJ_FADE)).min_(one);
        let lp = u.fma(
            L::splat(DJ_LOW_PASS[1] - DJ_LOW_PASS[0]),
            L::splat(DJ_LOW_PASS[0]),
        );
        let hp = u.fma(
            L::splat(DJ_HIGH_PASS[1] - DJ_HIGH_PASS[0]),
            L::splat(DJ_HIGH_PASS[0]),
        );
        let g = gain(low_side.fma(lp - hp, hp), p.ref_over_rate);
        let k = dj_k(p.resonance);
        let [s0, s1, ..] = state;
        let (band, low) = svf(s0, s1, x, g, k);
        let high = x - k * band - low;
        let filtered = low_side.fma(low - high, high);
        wet.fma(filtered - x, x)
    }
}

/// The DJ sweep's resonance law.
#[inline(always)]
pub(super) fn dj_k<L: Lane>(r: L) -> L {
    r.fma(
        L::splat(DJ_K_MIN - K_BUTTERWORTH_2),
        L::splat(K_BUTTERWORTH_2),
    )
}

/// The diffuser: up to eight second-order all-pass stages at the cutoff in
/// series, Character setting how many (1 to 8, crossfading between whole
/// numbers), resonance their Q. State: 2j and 2j + 1 stage j.
struct Diffuser;

impl Structure for Diffuser {
    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let g = gain(pitch, p.ref_over_rate);
        let k = diffuser_k(p.resonance);
        let k2 = k + k;
        let n = p.character * L::splat((DIFFUSER_STAGES - 1) as f32);
        let mut u = x;
        let mut out = L::splat(0.0);
        for j in 0..DIFFUSER_STAGES {
            let (mut a, mut b) = (state[2 * j], state[2 * j + 1]);
            let (band, _) = svf(&mut a, &mut b, u, g, k);
            (state[2 * j], state[2 * j + 1]) = (a, b);
            u = u - k2 * band;
            let w = (L::splat(1.0) - abs(n - L::splat(j as f32))).max_(L::splat(0.0));
            out = w.fma(u, out);
        }
        out
    }
}

/// The diffuser's resonance law: each stage's damping.
#[inline(always)]
pub(super) fn diffuser_k<L: Lane>(r: L) -> L {
    r.fma(
        L::splat(DIFFUSER_K[1] - DIFFUSER_K[0]),
        L::splat(DIFFUSER_K[0]),
    )
}

/// The transistor ladders: four one-pole stages with feedback from the
/// last, each stage's gain falling as its state grows, solved in closed
/// form. `DRIVEN` is the driven ladder: a soft clipper before it, whose
/// gain Character raises from 0 to +30 dB, scaled so full scale comes out
/// at full scale, and a lower resonance knee; otherwise Character is
/// Smooth. State: 0 to 3 the stages, 4 the previous fourth-stage output.
struct TransistorLadder<const DRIVEN: bool>;

impl<const DRIVEN: bool> Structure for TransistorLadder<DRIVEN> {
    const SMOOTH: bool = !DRIVEN;

    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let one = L::splat(1.0);
        let (x, inv_knee) = if DRIVEN {
            let drive = exp2(p.character * L::splat(DRIVEN_DB / 20.0 * std::f32::consts::LOG2_10));
            (
                soft_clip(x * drive) / soft_clip(drive),
                p.inv_knee * L::splat(DRIVEN_KNEE),
            )
        } else {
            (x, p.inv_knee)
        };
        let inv_stage = L::splat(1.0 / TRANSISTOR_KNEE);
        let k = p.resonance * L::splat(LADDER_MAX) * saturation_gain(state[4], inv_knee);
        let g = gain(pitch, p.ref_over_rate);
        let mut gg = [one; 4];
        let mut b = [one; 4];
        for i in 0..4 {
            let gi = g * saturation_gain(state[i], inv_stage);
            gg[i] = gi / (one + gi);
            b[i] = (one - gg[i]) * state[i];
        }
        let big_g = gg[0] * gg[1] * gg[2] * gg[3];
        let sum = ((b[0] * gg[1] + b[1]) * gg[2] + b[2]).fma(gg[3], b[3]);
        let input = x * k.fma(L::splat(super::LADDER_COMPENSATION), one);
        let y4 = big_g.fma(input, sum) / k.fma(big_g, one);
        let mut u = input - k * y4;
        for i in 0..4 {
            let y = gg[i].fma(u, b[i]);
            state[i] = y + y - state[i];
            u = y;
        }
        state[4] = u;
        u
    }
}

/// The diode ladders: four coupled stages, each passing part of its
/// neighbour's current back ([`DIODE_COUPLING`]), with feedback from the
/// last, solved in closed form by elimination. `CLEAN` is the clean ladder:
/// its pass band compensated for resonance and no shaper; otherwise the
/// acid ladder, with an asymmetric shaper before it, a low resonance knee,
/// and the bass thinning as resonance rises. Character is Smooth. State: 0
/// to 3 the stages, 4 the previous fourth-stage output.
struct DiodeLadder<const CLEAN: bool>;

/// How far below the normal types' the acid ladder's resonance knee sits.
const ACID_KNEE: f32 = 2.0;

impl<const CLEAN: bool> Structure for DiodeLadder<CLEAN> {
    const SMOOTH: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let one = L::splat(1.0);
        let inv_knee = if CLEAN {
            p.inv_knee
        } else {
            p.inv_knee * L::splat(ACID_KNEE)
        };
        let k =
            p.resonance * L::splat(DIODE_K * DIODE_OVERSHOOT) * saturation_gain(state[4], inv_knee);
        let input = if CLEAN { x * (one + k) } else { asym(x) };
        let g = gain(pitch, p.ref_over_rate) * L::splat(1.0 / DIODE_W);
        let e = g * L::splat(DIODE_COUPLING);
        let d = one + g + e;
        // Row by row, each stage as a1 × the next + b1 + d1 × y4.
        let r1 = one / d;
        let a1 = e * r1;
        let b1 = g.fma(input, state[0]) * r1;
        let d1 = -g * k * r1;
        let r2 = one / (d - g * a1);
        let a2 = e * r2;
        let b2 = g.fma(b1, state[1]) * r2;
        let d2 = g * d1 * r2;
        let r3 = one / (d - g * a2);
        let p3 = g.fma(d2, e) * r3;
        let b3 = g.fma(b2, state[2]) * r3;
        let y4 = g.fma(b3, state[3]) / (one + g - g * p3);
        let y3 = p3.fma(y4, b3);
        let y2 = a2.fma(y3, d2.fma(y4, b2));
        let y1 = a1.fma(y2, d1.fma(y4, b1));
        for (s, y) in state.iter_mut().zip([y1, y2, y3, y4]) {
            *s = y + y - *s;
        }
        state[4] = y4;
        y4
    }
}

/// The pole-zero filter: the design's active points in series, each a
/// state-variable stage whose frequency, damping and output mix are blended
/// from the four states at this sample's X and Y, then the overall gain.
/// Character is Smooth. State: 2j and 2j + 1 point j.
struct PoleZero;

impl Structure for PoleZero {
    const SMOOTH: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let table = p.pole_zero;
        let xy = p.x * p.y;
        let transpose = (pitch - L::splat(69.0)) * L::splat(1.0 / 12.0);
        let limit = L::splat(TAN_LIMIT / std::f32::consts::PI);
        let mut u = x;
        for (j, section) in table.sections.iter().take(table.active).enumerate() {
            let [octaves, k, high, band, low] = section.map(|[c, dx, dy, dxy]| {
                L::splat(dxy).fma(
                    xy,
                    L::splat(dy).fma(p.y, L::splat(dx).fma(p.x, L::splat(c))),
                )
            });
            let ratio = (exp2(transpose + octaves) * p.ref_over_rate).min_(limit);
            let g = tan(ratio * L::splat(std::f32::consts::PI));
            let (mut s0, mut s1) = (state[2 * j], state[2 * j + 1]);
            let (bp, lp) = svf(&mut s0, &mut s1, u, g, k);
            (state[2 * j], state[2 * j + 1]) = (s0, s1);
            let kb = k * bp;
            let hp = u - kb - lp;
            u = high.fma(hp, band.fma(kb, low * lp));
        }
        u * L::splat(table.gain)
    }
}

/// Comb II: a feedback comb whose loop reads two taps on the delay lines,
/// one period of the cutoff back and one period of a second frequency back
/// (character sets it, up to two octaves above), averaged, with a soft
/// clipper in the loop. The output is scaled by 1 − feedback, so the peaks
/// stay at unity.
struct Comb2;

impl Structure for Comb2 {
    const LINES: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(
        lines: &mut Lines<'_>,
        _state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        // The lines work each lane's delay out from its pitch.
        let io = Io {
            x,
            g: x,
            ratio: x,
            pitch,
            r: p.resonance,
            inv_knee: p.inv_knee,
            character: p.character,
            drive: x,
            ref_over_rate: p.reference_ratio,
            inv_rate: p.inv_rate,
        };
        let first = lines.read(0, 1, &io, |ratio, _| 1.0 / ratio);
        let second = lines.read(0, 1, &io, |ratio, c| {
            1.0 / (ratio * (c * COMB_OCTAVES).exp2())
        });
        let taps = (first + second) * L::splat(0.5);
        let knee = L::splat(1.0) / p.inv_knee;
        let feedback = p.resonance * L::splat(super::COMB_FEEDBACK_MAX);
        let y = feedback.fma(soft_clip(taps * p.inv_knee) * knee, x);
        lines.write(0, 1, y);
        y * (L::splat(1.0) - feedback)
    }
}

/// The expander types: four one-pole stages with feedback from the last,
/// as the normal ladder, their output a mix of the ladder's input and its
/// four stages. `BAND` is the band-pass, 4 (y2 − 2 y3 + y4); otherwise the
/// multimode, Character blending four-pole low-pass, notch and high-pass.
/// State: 0 to 3 the stages, 4 the previous fourth-stage output.
struct Expander<const BAND: bool>;

/// The expander multimode's tap weights (input, then stages 1 to 4): a
/// four-pole low-pass, notch and high-pass.
pub(super) const EXPANDER_LOW: [f32; 5] = [0.0, 0.0, 0.0, 0.0, 1.0];
pub(super) const EXPANDER_NOTCH: [f32; 5] = [1.0, -4.0, 8.0, -8.0, 4.0];
pub(super) const EXPANDER_HIGH: [f32; 5] = [1.0, -4.0, 6.0, -4.0, 1.0];

impl<const BAND: bool> Structure for Expander<BAND> {
    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let one = L::splat(1.0);
        let k = p.resonance * L::splat(LADDER_MAX) * saturation_gain(state[4], p.inv_knee);
        let g = gain(pitch, p.ref_over_rate);
        let gg = g / (one + g);
        let h = one - gg;
        let b = [h * state[0], h * state[1], h * state[2], h * state[3]];
        let sum = ((b[0] * gg + b[1]) * gg + b[2]).fma(gg, b[3]);
        let gg2 = gg * gg;
        let gg4 = gg2 * gg2;
        let input = x * k.fma(L::splat(super::LADDER_COMPENSATION), one);
        let y4 = gg4.fma(input, sum) / k.fma(gg4, one);
        let mut u = input - k * y4;
        let mut taps = [u; 5];
        for i in 0..4 {
            let y = gg.fma(u, b[i]);
            state[i] = y + y - state[i];
            taps[i + 1] = y;
            u = y;
        }
        state[4] = u;
        if BAND {
            L::splat(4.0) * (taps[2] - taps[3] - taps[3] + taps[4])
        } else {
            // Low-pass to notch over the first half, notch to high-pass
            // over the second.
            let two = L::splat(2.0);
            let a = (p.character * two).min_(one);
            let c = (p.character * two - one).max_(L::splat(0.0));
            let mut out = L::splat(0.0);
            for i in 0..5 {
                let to_notch = L::splat(EXPANDER_NOTCH[i] - EXPANDER_LOW[i]);
                let to_high = L::splat(EXPANDER_HIGH[i] - EXPANDER_NOTCH[i]);
                let w = c.fma(to_high, a.fma(to_notch, L::splat(EXPANDER_LOW[i])));
                out = w.fma(taps[i], out);
            }
            out
        }
    }
}

/// The K35: a Sallen–Key low-pass (a one-pole low-pass, then a one-pole
/// low-pass whose input adds back a high-passed copy of its output) and
/// high-pass (the mirror image), both running, with a saturator on the
/// feedback amplifier; the LP/HP control picks one, crossfading over a
/// tenth of its range. State: 0 to 2 the low-pass's stages, 3 its previous
/// feedback; 4 to 6 the high-pass's, 7 its previous feedback.
struct K35;

/// How far below the normal types' the K35's feedback knee sits.
const K35_KNEE: f32 = 3.0;

impl Structure for K35 {
    #[inline(always)]
    fn tick<L: Lane>(
        _lines: &mut Lines<'_>,
        state: &mut [L; X_STATES],
        x: L,
        pitch: L,
        p: &Settings<'_, L>,
    ) -> L {
        let one = L::splat(1.0);
        let g = gain(pitch, p.ref_over_rate);
        let gg = g / (one + g);
        let h = one - gg;
        let inv_knee = p.inv_knee * L::splat(K35_KNEE);
        let base = p.resonance * L::splat(K35_MAX);
        let den_of = |k: L| one / (one - k * gg * h);

        // Low-pass.
        let k = base * saturation_gain(state[3], inv_knee);
        let y1 = gg.fma(x, h * state[0]);
        let y2 = (gg * y1 - k * gg * h * state[2] + h * state[1]) * den_of(k);
        let hp3 = h * (y2 - state[2]);
        state[0] = y1 + y1 - state[0];
        state[1] = y2 + y2 - state[1];
        let lp3 = y2 - hp3;
        state[2] = lp3 + lp3 - state[2];
        state[3] = k * hp3;
        let low = y2;

        // High-pass.
        let k = base * saturation_gain(state[7], inv_knee);
        let y1 = h * (x - state[4]);
        let y2 = h * (y1 + k * h * state[6] - state[5]) * den_of(k);
        let lp3 = gg.fma(y2, h * state[6]);
        let v = k.fma(lp3, y1);
        let lp1 = x - y1;
        state[4] = lp1 + lp1 - state[4];
        let lp2 = v - y2;
        state[5] = lp2 + lp2 - state[5];
        state[6] = lp3 + lp3 - state[6];
        state[7] = k * lp3;
        let high = y2;

        let w = unit(
            (p.character - L::splat(K35_SWITCH[0]))
                * L::splat(1.0 / (K35_SWITCH[1] - K35_SWITCH[0])),
        );
        w.fma(high - low, low)
    }
}

/// The extended types' state fits in the bank's.
const _: () = assert!(X_STATES <= MAX_STATES && 2 * DIFFUSER_STAGES <= LAG_STARTED);
