//! The NEON path: four lanes in a 128-bit Advanced SIMD register.
//!
//! Every aarch64 target Rust supports has NEON, so this path needs no
//! run-time check and its functions compile with it already.
//!
//! Rust still asks for an `unsafe` block around each intrinsic, as they're
//! declared with `#[target_feature(enable = "neon")]`; the ones here only
//! compute on registers, apart from the load and store, which slice their
//! memory first.

#![allow(
    clippy::undocumented_unsafe_blocks,
    reason = "every block calls a NEON intrinsic, which every aarch64 CPU has"
)]

use std::arch::aarch64::{
    float32x4_t, uint32x4_t, vabsq_f32, vaddq_f32, vaddq_u32, vaddvq_f32, vcombine_f32,
    vcvtq_f32_s32, vcvtq_s32_f32, vdivq_f32, vdupq_n_f32, vdupq_n_u32, vfmaq_f32, vget_low_u32,
    vld1_f32, vld1q_f32, vld1q_lane_f32, vld1q_u32, vmaxq_f32, vmaxq_u32, vminq_f32, vminq_u32,
    vmull_high_u32, vmull_u32, vmulq_f32, vmulq_u32, vnegq_f32, vreinterpretq_f32_f64,
    vreinterpretq_f32_u32, vreinterpretq_f64_f32, vreinterpretq_s32_u32, vreinterpretq_u32_f32,
    vreinterpretq_u32_s32, vreinterpretq_u32_u64, vrndmq_f32, vshlq_n_u32, vshrq_n_u32, vsqrtq_f32,
    vst1q_f32, vst1q_u32, vsubq_f32, vsubq_u32, vtrn1_f32, vtrn1q_f32, vtrn2_f32, vtrn2q_f32,
    vuzp2q_u32, vzip1q_f64, vzip2q_f64,
};

use super::{F32s, Kernel, Level, U32s, gather_limit};

/// Runs `kernel` with NEON lanes.
pub(super) fn run<K: Kernel>(kernel: K) -> K::Output {
    kernel.run::<F32x4>()
}

/// Four `f32` lanes in a NEON register.
#[derive(Clone, Copy, Debug)]
pub(super) struct F32x4(float32x4_t);

impl F32x4 {
    #[inline(always)]
    fn add_(self, other: Self) -> Self {
        Self(unsafe { vaddq_f32(self.0, other.0) })
    }

    #[inline(always)]
    fn sub_(self, other: Self) -> Self {
        Self(unsafe { vsubq_f32(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_(self, other: Self) -> Self {
        Self(unsafe { vmulq_f32(self.0, other.0) })
    }

    #[inline(always)]
    fn div_(self, other: Self) -> Self {
        Self(unsafe { vdivq_f32(self.0, other.0) })
    }

    #[inline(always)]
    fn neg_(self) -> Self {
        Self(unsafe { vnegq_f32(self.0) })
    }
}

lane_ops!(F32x4);

impl F32s for F32x4 {
    const LANES: usize = 4;
    const LEVEL: Level = Level::Neon;
    type U32s = U32x4;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        Self(unsafe { vdupq_n_f32(value) })
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        let from = &from[..4];
        // SAFETY: `from` holds the four floats read.
        Self(unsafe { vld1q_f32(from.as_ptr()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        let to = &mut to[..4];
        // SAFETY: `to` has room for the four floats written.
        unsafe { vst1q_f32(to.as_mut_ptr(), self.0) }
    }

    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        Self(unsafe { vfmaq_f32(add.0, self.0, mul.0) })
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { vminq_f32(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { vmaxq_f32(self.0, other.0) })
    }

    #[inline(always)]
    fn abs(self) -> Self {
        Self(unsafe { vabsq_f32(self.0) })
    }

    #[inline(always)]
    fn sqrt(self) -> Self {
        Self(unsafe { vsqrtq_f32(self.0) })
    }

    #[inline(always)]
    fn sum(self) -> f32 {
        unsafe { vaddvq_f32(self.0) }
    }

    #[inline(always)]
    fn floor(self) -> Self {
        Self(unsafe { vrndmq_f32(self.0) })
    }

    #[inline(always)]
    fn to_bits(self) -> U32x4 {
        U32x4(unsafe { vreinterpretq_u32_f32(self.0) })
    }

    #[inline(always)]
    fn from_bits(bits: U32x4) -> Self {
        Self(unsafe { vreinterpretq_f32_u32(bits.0) })
    }

    #[inline(always)]
    fn to_i32(self) -> U32x4 {
        U32x4(unsafe { vreinterpretq_u32_s32(vcvtq_s32_f32(self.0)) })
    }

    #[inline(always)]
    fn from_i32(lanes: U32x4) -> Self {
        Self(unsafe { vcvtq_f32_s32(vreinterpretq_s32_u32(lanes.0)) })
    }

    #[inline(always)]
    fn gather<const N: usize>(table: &[f32], index: U32x4) -> [Self; N] {
        let limit = gather_limit::<N>(table);
        let mut at = [0; 4];
        index.min(U32x4::splat(limit)).store(&mut at);
        // SAFETY: each index is at most `table.len() − N`, so the `N`
        // values from each lane's index are in the table, and every load
        // below reads only those.
        let p = at.map(|i| unsafe { table.as_ptr().add(i as usize) });
        let mut rows = [Self::splat(0.0); N];
        // Four rows at a time: each lane's four values in one load, then a
        // 4 × 4 transpose, far quicker than sixteen single-lane loads.
        let mut r = 0;
        while r + 4 <= N {
            unsafe {
                let [a, b, c, d] = p.map(|p| vld1q_f32(p.add(r)));
                let (ab0, ab1) = (vtrn1q_f32(a, b), vtrn2q_f32(a, b));
                let (cd0, cd1) = (vtrn1q_f32(c, d), vtrn2q_f32(c, d));
                let pair = |x: float32x4_t, y: float32x4_t, high: bool| {
                    let (x, y) = (vreinterpretq_f64_f32(x), vreinterpretq_f64_f32(y));
                    vreinterpretq_f32_f64(if high {
                        vzip2q_f64(x, y)
                    } else {
                        vzip1q_f64(x, y)
                    })
                };
                rows[r] = Self(pair(ab0, cd0, false));
                rows[r + 1] = Self(pair(ab1, cd1, false));
                rows[r + 2] = Self(pair(ab0, cd0, true));
                rows[r + 3] = Self(pair(ab1, cd1, true));
            }
            r += 4;
        }
        // Two rows: each lane's pair in one load.
        if r + 2 <= N {
            unsafe {
                let [a, b, c, d] = p.map(|p| vld1_f32(p.add(r)));
                rows[r] = Self(vcombine_f32(vtrn1_f32(a, b), vtrn1_f32(c, d)));
                rows[r + 1] = Self(vcombine_f32(vtrn2_f32(a, b), vtrn2_f32(c, d)));
            }
            r += 2;
        }
        // A last row, a lane at a time.
        while r < N {
            unsafe {
                let row = vld1q_lane_f32::<0>(p[0].add(r), vdupq_n_f32(0.0));
                let row = vld1q_lane_f32::<1>(p[1].add(r), row);
                let row = vld1q_lane_f32::<2>(p[2].add(r), row);
                rows[r] = Self(vld1q_lane_f32::<3>(p[3].add(r), row));
            }
            r += 1;
        }
        rows
    }
}

/// Four `u32` lanes in a NEON register.
#[derive(Clone, Copy, Debug)]
pub(super) struct U32x4(uint32x4_t);

impl U32s for U32x4 {
    const LANES: usize = 4;

    #[inline(always)]
    fn splat(value: u32) -> Self {
        Self(unsafe { vdupq_n_u32(value) })
    }

    #[inline(always)]
    fn load(from: &[u32]) -> Self {
        let from = &from[..4];
        // SAFETY: `from` holds the four values read.
        Self(unsafe { vld1q_u32(from.as_ptr()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [u32]) {
        let to = &mut to[..4];
        // SAFETY: `to` has room for the four values written.
        unsafe { vst1q_u32(to.as_mut_ptr(), self.0) }
    }

    #[inline(always)]
    fn wrapping_add(self, other: Self) -> Self {
        Self(unsafe { vaddq_u32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_sub(self, other: Self) -> Self {
        Self(unsafe { vsubq_u32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_mul(self, other: Self) -> Self {
        Self(unsafe { vmulq_u32(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_high(self, other: Self) -> Self {
        // Two widening multiplies, then the high halves of the four
        // products.
        Self(unsafe {
            let low = vmull_u32(vget_low_u32(self.0), vget_low_u32(other.0));
            let high = vmull_high_u32(self.0, other.0);
            vuzp2q_u32(vreinterpretq_u32_u64(low), vreinterpretq_u32_u64(high))
        })
    }

    #[inline(always)]
    fn shl<const N: i32>(self) -> Self {
        Self(unsafe { vshlq_n_u32::<N>(self.0) })
    }

    #[inline(always)]
    fn shr<const N: i32>(self) -> Self {
        Self(unsafe { vshrq_n_u32::<N>(self.0) })
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { vminq_u32(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { vmaxq_u32(self.0, other.0) })
    }
}
