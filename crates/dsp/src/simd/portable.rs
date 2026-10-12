//! The portable path: four lanes in a plain array, on any CPU.

use super::{F32s, Level};

/// Four `f32` lanes in an array.
#[derive(Clone, Copy, Debug)]
pub(super) struct F32x4([f32; 4]);

impl F32x4 {
    #[inline(always)]
    fn map(self, f: impl Fn(f32) -> f32) -> Self {
        Self(self.0.map(f))
    }

    #[inline(always)]
    fn zip(self, other: Self, f: impl Fn(f32, f32) -> f32) -> Self {
        let (a, b) = (self.0, other.0);
        Self([f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2]), f(a[3], b[3])])
    }

    #[inline(always)]
    fn add_(self, other: Self) -> Self {
        self.zip(other, |a, b| a + b)
    }

    #[inline(always)]
    fn sub_(self, other: Self) -> Self {
        self.zip(other, |a, b| a - b)
    }

    #[inline(always)]
    fn mul_(self, other: Self) -> Self {
        self.zip(other, |a, b| a * b)
    }

    #[inline(always)]
    fn div_(self, other: Self) -> Self {
        self.zip(other, |a, b| a / b)
    }

    #[inline(always)]
    fn neg_(self) -> Self {
        self.map(|a| -a)
    }
}

lane_ops!(F32x4);

impl F32s for F32x4 {
    const LANES: usize = 4;
    const LEVEL: Level = Level::Portable;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        Self([value; 4])
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        let mut lanes = [0.0; 4];
        lanes.copy_from_slice(&from[..4]);
        Self(lanes)
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        to[..4].copy_from_slice(&self.0);
    }

    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        // Unfused: `f32::mul_add` falls back to a slow software routine on
        // CPUs without fused multiply-add.
        self.mul_(mul).add_(add)
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        self.zip(other, f32::min)
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        self.zip(other, f32::max)
    }

    #[inline(always)]
    fn abs(self) -> Self {
        self.map(f32::abs)
    }

    #[inline(always)]
    fn sqrt(self) -> Self {
        self.map(f32::sqrt)
    }

    #[inline(always)]
    fn sum(self) -> f32 {
        let [a, b, c, d] = self.0;
        (a + b) + (c + d)
    }
}
