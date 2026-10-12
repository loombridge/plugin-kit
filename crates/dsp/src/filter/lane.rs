//! What a filter kernel computes with: one `f32` (the scalar path) or a
//! vector of lanes from [`simd`](crate::simd), behind one trait, so each
//! filter is written once and compiled for both. Also the fast
//! approximations the kernels use, each with a test bounding its error over
//! the range the filters use it in.

use std::ops::{Add, Div, Mul, Neg, Sub};

use crate::simd::F32s;

/// One `f32`, or lanes of them.
pub(crate) trait Lane:
    Copy
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
{
    /// How many values it holds.
    const LANES: usize;
    /// Every lane `value`.
    fn splat(value: f32) -> Self;
    /// The first [`LANES`](Self::LANES) values of `from`.
    fn load(from: &[f32]) -> Self;
    /// Writes its lanes to the first [`LANES`](Self::LANES) values of `to`.
    fn store(self, to: &mut [f32]);
    /// `self × mul + add`, fused where the instruction set has it.
    fn fma(self, mul: Self, add: Self) -> Self;
    /// The smaller of each pair.
    fn min_(self, other: Self) -> Self;
    /// The larger of each pair.
    fn max_(self, other: Self) -> Self;
    /// Each lane's square root.
    fn sqrt_(self) -> Self;
}

impl Lane for f32 {
    const LANES: usize = 1;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        value
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        from[0]
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        to[0] = self;
    }

    #[inline(always)]
    fn fma(self, mul: Self, add: Self) -> Self {
        // Unfused, as the portable lanes are: `f32::mul_add` is a slow
        // software routine on CPUs without fused multiply-add.
        self * mul + add
    }

    #[inline(always)]
    fn min_(self, other: Self) -> Self {
        self.min(other)
    }

    #[inline(always)]
    fn max_(self, other: Self) -> Self {
        self.max(other)
    }

    #[inline(always)]
    fn sqrt_(self) -> Self {
        self.sqrt()
    }
}

impl<S: F32s> Lane for S {
    const LANES: usize = S::LANES;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        S::splat(value)
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        S::load(from)
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        F32s::store(self, to);
    }

    #[inline(always)]
    fn fma(self, mul: Self, add: Self) -> Self {
        self.mul_add(mul, add)
    }

    #[inline(always)]
    fn min_(self, other: Self) -> Self {
        F32s::min(self, other)
    }

    #[inline(always)]
    fn max_(self, other: Self) -> Self {
        F32s::max(self, other)
    }

    #[inline(always)]
    fn sqrt_(self) -> Self {
        F32s::sqrt(self)
    }
}

/// The largest magnitude [`exp2`] takes; beyond it, it clamps.
pub(crate) const EXP2_RANGE: f32 = 12.0;

/// 2 to the power `x`, for `x` clamped to ±[`EXP2_RANGE`]: the Taylor
/// series of e^z (degree 7) at z = x ln 2 / 16, squared four times.
/// Relative error under 10⁻⁵ (0.02 cents), tested; most of it is the
/// squarings doubling `f32` rounding.
#[inline(always)]
pub(crate) fn exp2<L: Lane>(x: L) -> L {
    let x = x.max_(L::splat(-EXP2_RANGE)).min_(L::splat(EXP2_RANGE));
    let z = x * L::splat(std::f32::consts::LN_2 / 16.0);
    let mut p = L::splat(1.0 / 5040.0);
    p = p.fma(z, L::splat(1.0 / 720.0));
    p = p.fma(z, L::splat(1.0 / 120.0));
    p = p.fma(z, L::splat(1.0 / 24.0));
    p = p.fma(z, L::splat(1.0 / 6.0));
    p = p.fma(z, L::splat(0.5));
    p = p.fma(z, L::splat(1.0));
    p = p.fma(z, L::splat(1.0));
    p = p * p;
    p = p * p;
    p = p * p;
    p * p
}

/// The largest angle [`tan`] is used at: 0.49 π, so a cutoff stops just
/// short of Nyquist.
pub(crate) const TAN_LIMIT: f32 = 0.49 * std::f32::consts::PI;

/// tan(w) for w from 0 to [`TAN_LIMIT`], as the ratio of the Taylor series
/// of sine (degree 11) and cosine (degree 12). Relative error under
/// 5 × 10⁻⁶ across that range, tested; near the top, most of it is `f32`
/// rounding in the cosine, which is small there.
#[inline(always)]
pub(crate) fn tan<L: Lane>(w: L) -> L {
    let w2 = w * w;
    let mut s = L::splat(-1.0 / 39_916_800.0);
    s = s.fma(w2, L::splat(1.0 / 362_880.0));
    s = s.fma(w2, L::splat(-1.0 / 5040.0));
    s = s.fma(w2, L::splat(1.0 / 120.0));
    s = s.fma(w2, L::splat(-1.0 / 6.0));
    s = s.fma(w2, L::splat(1.0));
    let mut c = L::splat(1.0 / 479_001_600.0);
    c = c.fma(w2, L::splat(-1.0 / 3_628_800.0));
    c = c.fma(w2, L::splat(1.0 / 40_320.0));
    c = c.fma(w2, L::splat(-1.0 / 720.0));
    c = c.fma(w2, L::splat(1.0 / 24.0));
    c = c.fma(w2, L::splat(-0.5));
    c = c.fma(w2, L::splat(1.0));
    s * w / c
}

/// A soft clipper shaped like tanh: the (3, 2) Padé approximant
/// x (27 + x²) / (27 + 9 x²) on x clamped to ±3, where it reaches ±1 with
/// zero slope. Within 0.025 of tanh everywhere, tested; odd, smooth, and
/// monotonic to within `f32` rounding.
#[inline(always)]
pub(crate) fn soft_clip<L: Lane>(x: L) -> L {
    let x = x.max_(L::splat(-3.0)).min_(L::splat(3.0));
    let x2 = x * x;
    x * (x2 + L::splat(27.0)) / x2.fma(L::splat(9.0), L::splat(27.0))
}

/// The gain the resonance path's saturator x / √(1 + (x / a)²) has at `x`,
/// given `inv_a` = 1 / a: 1 for small signals, falling towards a / |x| as
/// the signal grows past `a`.
#[inline(always)]
pub(crate) fn saturation_gain<L: Lane>(x: L, inv_a: L) -> L {
    let r = x * inv_a;
    L::splat(1.0) / r.fma(r, L::splat(1.0)).sqrt_()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp2_is_within_its_stated_error() {
        let mut worst = 0.0f64;
        for i in 0..=240_000 {
            let x = -EXP2_RANGE + i as f32 * (2.0 * EXP2_RANGE / 240_000.0);
            let exact = 2f64.powf(f64::from(x));
            let error = ((f64::from(exp2(x)) - exact) / exact).abs();
            worst = worst.max(error);
        }
        assert!(worst < 1e-5, "relative error {worst:e}");
        // It clamps beyond its range.
        assert_eq!(exp2(40.0f32), exp2(EXP2_RANGE));
    }

    #[test]
    fn tan_is_within_its_stated_error() {
        let mut worst = 0.0f64;
        for i in 1..=200_000 {
            let w = TAN_LIMIT * i as f32 / 200_000.0;
            let exact = f64::from(w).tan();
            let error = ((f64::from(tan(w)) - exact) / exact).abs();
            worst = worst.max(error);
        }
        assert!(worst < 5e-6, "relative error {worst:e}");
    }

    #[test]
    fn the_soft_clipper_stays_near_tanh_and_rises_monotonically() {
        let mut previous = -2.0f32;
        for i in 0..=20_000 {
            let x = -10.0 + i as f32 * 0.001;
            let y = soft_clip(x);
            assert!(
                (f64::from(y) - f64::from(x).tanh()).abs() < 0.025,
                "{x}: {y}"
            );
            assert!(y >= previous - 1e-6, "{x}");
            previous = y;
        }
        assert_eq!(soft_clip(3.0f32), 1.0);
        assert_eq!(soft_clip(-50.0f32), -1.0);
    }

    #[test]
    fn the_resonance_saturator_is_unity_for_small_signals_and_bounded_for_large() {
        assert!((saturation_gain(1e-3f32, 1.0 / 3.0) - 1.0).abs() < 1e-7);
        for x in [1.0f32, 10.0, 1e3, 1e6] {
            let y = x * saturation_gain(x, 1.0 / 3.0);
            assert!(y < 3.0 && y > 0.0, "{x}: {y}");
        }
    }
}
