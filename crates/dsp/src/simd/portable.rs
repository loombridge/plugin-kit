//! The portable path: four lanes in a plain array, on any CPU.

use super::{F32s, Level, U32s, gather_lanes, gather_limit};

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
    type U32s = U32x4;

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

    #[inline(always)]
    fn floor(self) -> Self {
        self.map(f32::floor)
    }

    #[inline(always)]
    fn to_bits(self) -> U32x4 {
        U32x4(self.0.map(f32::to_bits))
    }

    #[inline(always)]
    fn from_bits(bits: U32x4) -> Self {
        Self(bits.0.map(f32::from_bits))
    }

    #[inline(always)]
    fn to_i32(self) -> U32x4 {
        // `as` saturates where the vector paths give unspecified values;
        // within range all agree.
        U32x4(self.0.map(|x| x as i32 as u32))
    }

    #[inline(always)]
    fn from_i32(lanes: U32x4) -> Self {
        Self(lanes.0.map(|x| x as i32 as f32))
    }

    #[inline(always)]
    fn gather<const N: usize>(table: &[f32], index: U32x4) -> [Self; N] {
        let limit = gather_limit::<N>(table);
        gather_lanes::<4, N>(table, index.0, limit).map(Self)
    }
}

/// Four `u32` lanes in an array.
#[derive(Clone, Copy, Debug)]
pub(super) struct U32x4([u32; 4]);

impl U32x4 {
    #[inline(always)]
    fn zip(self, other: Self, f: impl Fn(u32, u32) -> u32) -> Self {
        let (a, b) = (self.0, other.0);
        Self([f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2]), f(a[3], b[3])])
    }
}

impl U32s for U32x4 {
    const LANES: usize = 4;

    #[inline(always)]
    fn splat(value: u32) -> Self {
        Self([value; 4])
    }

    #[inline(always)]
    fn load(from: &[u32]) -> Self {
        let mut lanes = [0; 4];
        lanes.copy_from_slice(&from[..4]);
        Self(lanes)
    }

    #[inline(always)]
    fn store(self, to: &mut [u32]) {
        to[..4].copy_from_slice(&self.0);
    }

    #[inline(always)]
    fn wrapping_add(self, other: Self) -> Self {
        self.zip(other, u32::wrapping_add)
    }

    #[inline(always)]
    fn wrapping_sub(self, other: Self) -> Self {
        self.zip(other, u32::wrapping_sub)
    }

    #[inline(always)]
    fn wrapping_mul(self, other: Self) -> Self {
        self.zip(other, u32::wrapping_mul)
    }

    #[inline(always)]
    fn mul_high(self, other: Self) -> Self {
        self.zip(other, |a, b| ((u64::from(a) * u64::from(b)) >> 32) as u32)
    }

    #[inline(always)]
    fn shl<const N: i32>(self) -> Self {
        Self(self.0.map(|x| x << N))
    }

    #[inline(always)]
    fn shr<const N: i32>(self) -> Self {
        Self(self.0.map(|x| x >> N))
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        self.zip(other, u32::min)
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        self.zip(other, u32::max)
    }
}
