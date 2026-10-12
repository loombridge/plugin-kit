//! A single `f32` as a one-lane [`F32s`], so the evaluator written for SIMD
//! lanes also evaluates one voice, or a global route, with exactly the same
//! operations.

use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};

use lspk_dsp::simd::{F32s, Level};

/// One lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct One(pub(crate) f32);

impl Add for One {
    type Output = Self;
    #[inline(always)]
    fn add(self, o: Self) -> Self {
        Self(self.0 + o.0)
    }
}

impl Sub for One {
    type Output = Self;
    #[inline(always)]
    fn sub(self, o: Self) -> Self {
        Self(self.0 - o.0)
    }
}

impl Mul for One {
    type Output = Self;
    #[inline(always)]
    fn mul(self, o: Self) -> Self {
        Self(self.0 * o.0)
    }
}

impl Div for One {
    type Output = Self;
    #[inline(always)]
    fn div(self, o: Self) -> Self {
        Self(self.0 / o.0)
    }
}

impl Neg for One {
    type Output = Self;
    #[inline(always)]
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl AddAssign for One {
    #[inline(always)]
    fn add_assign(&mut self, o: Self) {
        self.0 += o.0;
    }
}

impl SubAssign for One {
    #[inline(always)]
    fn sub_assign(&mut self, o: Self) {
        self.0 -= o.0;
    }
}

impl MulAssign for One {
    #[inline(always)]
    fn mul_assign(&mut self, o: Self) {
        self.0 *= o.0;
    }
}

impl F32s for One {
    const LANES: usize = 1;
    const LEVEL: Level = Level::Portable;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        Self(value)
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        Self(from[0])
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        to[0] = self.0;
    }

    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        Self(self.0 * mul.0 + add.0)
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }

    #[inline(always)]
    fn abs(self) -> Self {
        Self(self.0.abs())
    }

    #[inline(always)]
    fn sqrt(self) -> Self {
        Self(self.0.sqrt())
    }

    #[inline(always)]
    fn sum(self) -> f32 {
        self.0
    }
}
