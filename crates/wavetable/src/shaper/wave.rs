//! The wave stage: the shaper modes that bend the value after the read,
//! the filters and the distortions.
//!
//! The distortions are memoryless curves, each the identity's slope at 0,
//! driven by up to [`DRIVE_MAX_DB`](super::DRIVE_MAX_DB). They make
//! harmonics above Nyquist, so the oscillator renders them oversampled,
//! its table read band-limited to the host rate's Nyquist, and decimates.
//!
//! The filters are a state-variable filter (Simper's trapezoidal SVF) per
//! lane, 12 dB an octave, whose cutoff is a harmonic of the note: it
//! follows the lane's phase increment, so the shaped cycle looks the same
//! at every pitch, as the 2D view draws it.

use super::Mode;
use super::lanes::{
    Lane, above, clamp, cos_cycles, exp, ln, ln_cosh, pick, sign, sin_cycles, tan, tanh,
};

/// A filter mode's cutoff, as a harmonic of the note, at amount `a`:
/// low-pass from harmonic 2,048 at 0 % down to the fundamental at 100 %;
/// high-pass from a quarter of the fundamental up to harmonic 1,024.
#[inline(always)]
pub(crate) fn cutoff_harmonic<L: Lane>(mode: Mode, a: L) -> L {
    let c = L::splat;
    match mode {
        Mode::LowPass => ((c(1.0) - a) * c(11.0)).exp2(),
        _ => a.mul_add(c(12.0), c(-2.0)).exp2(),
    }
}

/// The filters' damping (1 / Q) at character `character`: Butterworth
/// (√2) at 0, falling to 0.21 (a 13 dB peak) at 1.
pub(crate) fn damping(character: f32) -> f32 {
    std::f32::consts::SQRT_2 - 1.2 * character.clamp(0.0, 1.0)
}

/// One lane or vector of lanes of a filter's state.
#[derive(Clone, Copy)]
pub(crate) struct Svf<L> {
    pub ic1: L,
    pub ic2: L,
}

/// Filters `x` through `state`, returning the low-pass or high-pass
/// output: cutoff `harmonic` of a note whose phase moves `increment`
/// cycles a sample, damping `k`.
#[inline(always)]
pub(crate) fn filter<L: Lane>(
    mode: Mode,
    harmonic: L,
    increment: L,
    k: f32,
    x: L,
    state: &mut Svf<L>,
) -> L {
    let c = L::splat;
    let cutoff = (harmonic * increment).min(c(0.45));
    let g = tan(cutoff * c(std::f32::consts::PI));
    let k = c(k);
    let a1 = c(1.0) / g.mul_add(g + k, c(1.0));
    let a2 = g * a1;
    let a3 = g * a2;
    let v3 = x - state.ic2;
    let v1 = a1.mul_add(state.ic1, a2 * v3);
    let v2 = state.ic2 + a2.mul_add(state.ic1, a3 * v3);
    state.ic1 = c(2.0) * v1 - state.ic1;
    state.ic2 = c(2.0) * v2 - state.ic2;
    match mode {
        Mode::LowPass => v2,
        _ => x - k * v1 - v2,
    }
}

/// tanh of the valve's bias.
const VALVE_TANH_BIAS: f32 = 0.244_918_66;

/// The valve's bias, before the curve.
const VALVE_BIAS: f32 = 0.25;

/// ln cosh of the valve's bias.
const VALVE_LN_COSH_BIAS: f32 = 0.030_929_8;

/// `mode`'s drive gain at amount `a`: 0 to
/// [`DRIVE_MAX_DB`](super::DRIVE_MAX_DB), or to
/// [`FOLD_DRIVE_MAX_DB`](super::FOLD_DRIVE_MAX_DB) for the folds, whose
/// every 6 dB folds the wave twice as many times.
#[inline(always)]
pub(crate) fn drive<L: Lane>(mode: Mode, a: L) -> L {
    let most = match mode {
        Mode::Fold | Mode::SineFold => super::FOLD_DRIVE_MAX_DB,
        _ => super::DRIVE_MAX_DB,
    };
    (a * L::splat(most / 6.020_6)).exp2()
}

/// A distortion mode's curve at amount `a` (0 to 1), applied to `x`, as
/// the 2D preview draws it.
#[inline(always)]
pub(crate) fn distort<L: Lane>(mode: Mode, a: L, x: L) -> L {
    if mode == Mode::Rectify {
        // Blends towards full-wave rectification.
        return (x.abs() - x).mul_add(a, x);
    }
    curve(mode, x * drive(mode, a))
}

/// A distortion mode's curve, applied to `x` after the drive: slope 1 at 0
/// for every mode but Zero-square, whose slope there is 0. For Rectify,
/// the rectifier alone.
#[inline(always)]
pub(crate) fn curve<L: Lane>(mode: Mode, x: L) -> L {
    let c = L::splat;
    let one = c(1.0);
    match mode {
        Mode::Valve => {
            // A biased hyperbolic tangent: softer on one side, so even
            // harmonics as well as odd.
            (tanh(x + c(VALVE_BIAS)) - c(VALVE_TANH_BIAS))
                * c(1.0 / (1.0 - VALVE_TANH_BIAS * VALVE_TANH_BIAS))
        }
        Mode::SoftClip => {
            // A cubic with slope 1 at 0 and 0 at ±1.5, where it reaches ±1.
            let z = clamp(x, -1.5, 1.5);
            z - z * z * z * c(4.0 / 27.0)
        }
        Mode::HardClip => clamp(x, -1.0, 1.0),
        Mode::Diode1 => {
            // One diode: the positive half limits at 1 exponentially, the
            // negative half later and softer, at −2.
            let positive = x.max(c(0.0));
            let negative = x.min(c(0.0));
            (one - exp(-positive)) - c(2.0) * (one - exp(negative * c(0.5)))
        }
        // Two diodes back to back: both halves limit at ±1.
        Mode::Diode2 => sign(x) * (one - exp(-x.abs())),
        Mode::Fold => {
            // Folds back from ±1, again and again: a triangle of the input.
            let t = (x + one) * c(0.25);
            one - (c(4.0) * (t - t.floor()) - c(2.0)).abs()
        }
        Mode::SineFold => sin_cycles(x * c(0.25)),
        // Squared towards zero, keeping its sign: quiet parts drop away,
        // then ±1 clips.
        Mode::ZeroSquare => clamp(x * x.abs(), -1.0, 1.0),
        Mode::Asymmetric => {
            let positive = x.max(c(0.0));
            let negative = x.min(c(0.0));
            tanh(positive * c(2.0)) * c(0.5) + tanh(negative * c(0.5)) * c(2.0)
        }
        Mode::SineShaper => {
            // A sine's quarter cycle as the curve: flat at ±1.
            let z = clamp(x, -std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
            sin_cycles(z * c(1.0 / std::f32::consts::TAU))
        }
        Mode::Pedal => {
            // Hard clipping on the positive side, soft on the negative.
            let negative = x.min(c(0.0));
            x.max(c(0.0)).min(one) + negative / negative.mul_add(negative, one).sqrt()
        }
        Mode::Tape => x / x.mul_add(x, one).sqrt(),
        Mode::SoftSaturate => x / (one + x.abs()),
        Mode::Rectify => x.abs(),
        _ => x,
    }
}

/// [`curve`]'s integral from 0 to `x`, for the antiderivative method.
#[inline(always)]
pub(crate) fn integral<L: Lane>(mode: Mode, x: L) -> L {
    let c = L::splat;
    let one = c(1.0);
    // Past a clipping curve's knee at ±`knee`, the integral grows by the
    // distance beyond it.
    let beyond = |knee: L| x.abs() - knee.abs();
    match mode {
        Mode::Valve => {
            (ln_cosh(x + c(VALVE_BIAS)) - c(VALVE_LN_COSH_BIAS) - c(VALVE_TANH_BIAS) * x)
                * c(1.0 / (1.0 - VALVE_TANH_BIAS * VALVE_TANH_BIAS))
        }
        Mode::SoftClip => {
            let z = clamp(x, -1.5, 1.5);
            let z2 = z * z;
            z2 * c(0.5) - z2 * z2 * c(1.0 / 27.0) + beyond(z)
        }
        Mode::HardClip => {
            let z = clamp(x, -1.0, 1.0);
            z * z * c(0.5) + beyond(z)
        }
        Mode::Diode1 => {
            let positive = x.max(c(0.0));
            let negative = x.min(c(0.0));
            (positive + exp(-positive) - one)
                + (c(4.0) * exp(negative * c(0.5)) - c(2.0) * negative - c(4.0))
        }
        Mode::Diode2 => {
            let a = x.abs();
            a + exp(-a) - one
        }
        Mode::Fold => {
            let t = (x + one) * c(0.25);
            let s = c(2.0) * (t - t.floor()) - one;
            c(2.0) * s * (one - s.abs()) + c(0.5)
        }
        Mode::SineFold => (one - cos_cycles(x * c(0.25))) * c(2.0 / std::f32::consts::PI),
        Mode::ZeroSquare => {
            let z = clamp(x, -1.0, 1.0);
            let a = z.abs();
            a * a * a * c(1.0 / 3.0) + beyond(z)
        }
        Mode::Asymmetric => {
            let positive = x.max(c(0.0));
            let negative = x.min(c(0.0));
            ln_cosh(positive * c(2.0)) * c(0.25) + ln_cosh(negative * c(0.5)) * c(4.0)
        }
        Mode::SineShaper => {
            let z = clamp(x, -std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
            one - cos_cycles(z * c(1.0 / std::f32::consts::TAU)) + beyond(z)
        }
        Mode::Pedal => {
            let positive = x.max(c(0.0));
            let clipped = positive.min(one);
            let negative = x.min(c(0.0));
            clipped * clipped * c(0.5)
                + (positive - clipped)
                + negative.mul_add(negative, one).sqrt()
                - one
        }
        Mode::Tape => x.mul_add(x, one).sqrt() - one,
        Mode::SoftSaturate => {
            let a = x.abs();
            a - ln(one + a)
        }
        Mode::Rectify => x * x.abs() * c(0.5),
        _ => x * x * c(0.5),
    }
}

/// A distortion mode at amount `a` applied to `x` by the first-order
/// antiderivative method (Parker, Zavalishin and Le Bivic, *Reducing the
/// aliasing of nonlinear waveshaping using continuous-time convolution*,
/// DAFx 2016): the curve's mean over the straight line from the last input
/// to this one, the integral's difference over the input's, which turns
/// the curve's corners and jumps into gentler ones and so lowers what
/// folds by a further 6 dB an octave. Where the inputs are too close for
/// the difference to be accurate, the curve at their midpoint. `state`
/// holds the last input (after the drive) and its integral.
#[inline(always)]
pub(crate) fn distort_smooth<L: Lane>(mode: Mode, a: L, x: L, state: &mut Svf<L>) -> L {
    let c = L::splat;
    let driven = if mode == Mode::Rectify {
        x
    } else {
        x * drive(mode, a)
    };
    let area = integral(mode, driven);
    let last = state.ic1;
    let step = driven - last;
    // The integrals' rounding grows with their size, so the closeness
    // allowed does too.
    let scale = driven.abs().max(last.abs()).max(c(1.0));
    // At 1 % of their size, the integrals' rounding (which differs in its
    // last bits between paths that fuse multiply-adds and those that
    // don't) stays below −100 dB of the curve.
    let apart = above(step.abs() - scale * c(1e-2));
    let mean = (area - state.ic2) / pick(apart, c(1.0), step);
    let midpoint = curve(mode, (driven + last) * c(0.5));
    let y = pick(apart, midpoint, mean);
    state.ic1 = driven;
    state.ic2 = area;
    if mode == Mode::Rectify {
        // The rectifier blended with the input, both delayed half a sample
        // alike.
        let linear = (x + last) * c(0.5);
        (y - linear).mul_add(a, linear)
    } else {
        y
    }
}
