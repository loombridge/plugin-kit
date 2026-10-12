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
    float32x4_t, vabsq_f32, vaddq_f32, vaddvq_f32, vdivq_f32, vdupq_n_f32, vfmaq_f32, vld1q_f32,
    vmaxq_f32, vminq_f32, vmulq_f32, vnegq_f32, vsqrtq_f32, vst1q_f32, vsubq_f32,
};

use super::{F32s, Kernel, Level};

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
}
