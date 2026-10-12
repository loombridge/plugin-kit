//! The x86-64 paths: SSE4.2 (four lanes), AVX2 with FMA (eight) and
//! AVX-512F (sixteen).
//!
//! Each `run_*` function is compiled with its level's features and calls
//! the kernel there, so the kernel's lane operations, all
//! `#[inline(always)]`, are compiled with those instructions too.
//!
//! The lane types are private to the module tree and reached only through
//! those functions, which [`dispatch_at`](super::dispatch_at) calls only
//! after checking the CPU has the features. So a value of a lane type
//! exists only on a CPU that can run its instructions, and that is the
//! safety argument for every `unsafe` block below that calls an intrinsic;
//! the loads and stores also slice their memory first, so they never read
//! or write past it.

#![allow(
    clippy::undocumented_unsafe_blocks,
    reason = "every block calls an intrinsic of its type's level; see the module's safety note"
)]

use std::arch::x86_64::{
    __m128, __m128i, __m256, __m256i, __m512, __m512i, _mm_add_epi32, _mm_add_ps, _mm_andnot_ps,
    _mm_blend_epi16, _mm_castps_si128, _mm_castsi128_ps, _mm_cvtepi32_ps, _mm_cvttps_epi32,
    _mm_div_ps, _mm_floor_ps, _mm_loadu_ps, _mm_loadu_si128, _mm_max_epu32, _mm_max_ps,
    _mm_min_epu32, _mm_min_ps, _mm_movehl_ps, _mm_mul_epu32, _mm_mul_ps, _mm_mullo_epi32,
    _mm_set1_epi32, _mm_set1_ps, _mm_shuffle_ps, _mm_slli_epi32, _mm_sqrt_ps, _mm_srli_epi32,
    _mm_srli_epi64, _mm_storeu_ps, _mm_storeu_si128, _mm_sub_epi32, _mm_sub_ps, _mm_xor_ps,
    _mm256_add_epi32, _mm256_add_ps, _mm256_andnot_ps, _mm256_blend_epi32, _mm256_castps_si256,
    _mm256_castps256_ps128, _mm256_castsi256_ps, _mm256_cvtepi32_ps, _mm256_cvttps_epi32,
    _mm256_div_ps, _mm256_extractf128_ps, _mm256_floor_ps, _mm256_fmadd_ps, _mm256_i32gather_ps,
    _mm256_loadu_ps, _mm256_loadu_si256, _mm256_max_epu32, _mm256_max_ps, _mm256_min_epu32,
    _mm256_min_ps, _mm256_mul_epu32, _mm256_mul_ps, _mm256_mullo_epi32, _mm256_set1_epi32,
    _mm256_set1_ps, _mm256_slli_epi32, _mm256_sqrt_ps, _mm256_srli_epi32, _mm256_srli_epi64,
    _mm256_storeu_ps, _mm256_storeu_si256, _mm256_sub_epi32, _mm256_sub_ps, _mm256_xor_ps,
    _mm512_abs_ps, _mm512_add_epi32, _mm512_add_ps, _mm512_castps_si512, _mm512_castsi512_ps,
    _mm512_cvtepi32_ps, _mm512_cvttps_epi32, _mm512_div_ps, _mm512_fmadd_ps, _mm512_i32gather_ps,
    _mm512_loadu_ps, _mm512_loadu_si512, _mm512_mask_blend_epi32, _mm512_max_epu32, _mm512_max_ps,
    _mm512_min_epu32, _mm512_min_ps, _mm512_mul_epu32, _mm512_mul_ps, _mm512_mullo_epi32,
    _mm512_reduce_add_ps, _mm512_roundscale_ps, _mm512_set1_epi32, _mm512_set1_ps,
    _mm512_sllv_epi32, _mm512_sqrt_ps, _mm512_srli_epi64, _mm512_srlv_epi32, _mm512_storeu_ps,
    _mm512_storeu_si512, _mm512_sub_epi32, _mm512_sub_ps, _mm512_xor_si512,
};

use super::{F32s, Kernel, Level, U32s, gather_lanes, gather_limit};

/// Runs `kernel` with SSE4.2 lanes.
///
/// # Safety
///
/// The CPU must have SSE4.2.
#[target_feature(enable = "sse4.2")]
pub(super) unsafe fn run_sse42<K: Kernel>(kernel: K) -> K::Output {
    kernel.run::<Sse42>()
}

/// Runs `kernel` with AVX2 lanes.
///
/// # Safety
///
/// The CPU must have AVX2 and FMA.
#[target_feature(enable = "avx2,fma")]
pub(super) unsafe fn run_avx2<K: Kernel>(kernel: K) -> K::Output {
    kernel.run::<Avx2>()
}

/// Runs `kernel` with AVX-512 lanes.
///
/// # Safety
///
/// The CPU must have AVX-512F, AVX2 and FMA.
#[target_feature(enable = "avx512f,avx2,fma")]
pub(super) unsafe fn run_avx512<K: Kernel>(kernel: K) -> K::Output {
    kernel.run::<Avx512>()
}

/// Four lanes in an SSE register.
#[derive(Clone, Copy, Debug)]
pub(super) struct Sse42(__m128);

impl Sse42 {
    #[inline(always)]
    fn add_(self, other: Self) -> Self {
        Self(unsafe { _mm_add_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn sub_(self, other: Self) -> Self {
        Self(unsafe { _mm_sub_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_(self, other: Self) -> Self {
        Self(unsafe { _mm_mul_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn div_(self, other: Self) -> Self {
        Self(unsafe { _mm_div_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn neg_(self) -> Self {
        Self(unsafe { _mm_xor_ps(self.0, _mm_set1_ps(-0.0)) })
    }
}

lane_ops!(Sse42);

impl F32s for Sse42 {
    const LANES: usize = 4;
    const LEVEL: Level = Level::Sse42;
    type U32s = Sse42U;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        Self(unsafe { _mm_set1_ps(value) })
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        let from = &from[..4];
        Self(unsafe { _mm_loadu_ps(from.as_ptr()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        let to = &mut to[..4];
        unsafe { _mm_storeu_ps(to.as_mut_ptr(), self.0) }
    }

    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        // SSE4.2 has no fused multiply-add.
        self.mul_(mul).add_(add)
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { _mm_min_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { _mm_max_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn abs(self) -> Self {
        Self(unsafe { _mm_andnot_ps(_mm_set1_ps(-0.0), self.0) })
    }

    #[inline(always)]
    fn sqrt(self) -> Self {
        Self(unsafe { _mm_sqrt_ps(self.0) })
    }

    #[inline(always)]
    fn sum(self) -> f32 {
        sum128(self.0)
    }

    #[inline(always)]
    fn floor(self) -> Self {
        Self(unsafe { _mm_floor_ps(self.0) })
    }

    #[inline(always)]
    fn to_bits(self) -> Sse42U {
        Sse42U(unsafe { _mm_castps_si128(self.0) })
    }

    #[inline(always)]
    fn from_bits(bits: Sse42U) -> Self {
        Self(unsafe { _mm_castsi128_ps(bits.0) })
    }

    #[inline(always)]
    fn to_i32(self) -> Sse42U {
        Sse42U(unsafe { _mm_cvttps_epi32(self.0) })
    }

    #[inline(always)]
    fn from_i32(lanes: Sse42U) -> Self {
        Self(unsafe { _mm_cvtepi32_ps(lanes.0) })
    }

    #[inline(always)]
    fn gather<const N: usize>(table: &[f32], index: Sse42U) -> [Self; N] {
        // SSE has no gather instruction.
        let limit = gather_limit::<N>(table);
        let mut at = [0; 4];
        index.store(&mut at);
        gather_lanes::<4, N>(table, at, limit).map(|row| Self::load(&row))
    }
}

/// A 128-bit register's four lanes added up: (0 + 2) + (1 + 3).
#[inline(always)]
fn sum128(v: __m128) -> f32 {
    unsafe {
        let pairs = _mm_add_ps(v, _mm_movehl_ps(v, v));
        let total = _mm_add_ps(pairs, _mm_shuffle_ps::<0b01>(pairs, pairs));
        std::arch::x86_64::_mm_cvtss_f32(total)
    }
}

/// Eight lanes in an AVX register.
#[derive(Clone, Copy, Debug)]
pub(super) struct Avx2(__m256);

impl Avx2 {
    #[inline(always)]
    fn add_(self, other: Self) -> Self {
        Self(unsafe { _mm256_add_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn sub_(self, other: Self) -> Self {
        Self(unsafe { _mm256_sub_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_(self, other: Self) -> Self {
        Self(unsafe { _mm256_mul_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn div_(self, other: Self) -> Self {
        Self(unsafe { _mm256_div_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn neg_(self) -> Self {
        Self(unsafe { _mm256_xor_ps(self.0, _mm256_set1_ps(-0.0)) })
    }
}

lane_ops!(Avx2);

impl F32s for Avx2 {
    const LANES: usize = 8;
    const LEVEL: Level = Level::Avx2;
    type U32s = Avx2U;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        Self(unsafe { _mm256_set1_ps(value) })
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        let from = &from[..8];
        Self(unsafe { _mm256_loadu_ps(from.as_ptr()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        let to = &mut to[..8];
        unsafe { _mm256_storeu_ps(to.as_mut_ptr(), self.0) }
    }

    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        Self(unsafe { _mm256_fmadd_ps(self.0, mul.0, add.0) })
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { _mm256_min_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { _mm256_max_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn abs(self) -> Self {
        Self(unsafe { _mm256_andnot_ps(_mm256_set1_ps(-0.0), self.0) })
    }

    #[inline(always)]
    fn sqrt(self) -> Self {
        Self(unsafe { _mm256_sqrt_ps(self.0) })
    }

    #[inline(always)]
    fn sum(self) -> f32 {
        let halves = unsafe {
            _mm_add_ps(
                _mm256_castps256_ps128(self.0),
                _mm256_extractf128_ps::<1>(self.0),
            )
        };
        sum128(halves)
    }

    #[inline(always)]
    fn floor(self) -> Self {
        Self(unsafe { _mm256_floor_ps(self.0) })
    }

    #[inline(always)]
    fn to_bits(self) -> Avx2U {
        Avx2U(unsafe { _mm256_castps_si256(self.0) })
    }

    #[inline(always)]
    fn from_bits(bits: Avx2U) -> Self {
        Self(unsafe { _mm256_castsi256_ps(bits.0) })
    }

    #[inline(always)]
    fn to_i32(self) -> Avx2U {
        Avx2U(unsafe { _mm256_cvttps_epi32(self.0) })
    }

    #[inline(always)]
    fn from_i32(lanes: Avx2U) -> Self {
        Self(unsafe { _mm256_cvtepi32_ps(lanes.0) })
    }

    #[inline(always)]
    fn gather<const N: usize>(table: &[f32], index: Avx2U) -> [Self; N] {
        let limit = gather_limit::<N>(table);
        let index = index.min(Avx2U::splat(limit));
        let base = table.as_ptr();
        // SAFETY: every index is at most `table.len() − N`, which fits an
        // `i32` (`gather_limit` checked both), so each lane's reads of
        // `base + j + index` stay in the table.
        std::array::from_fn(|j| Self(unsafe { _mm256_i32gather_ps::<4>(base.add(j), index.0) }))
    }
}

/// Sixteen lanes in an AVX-512 register.
#[derive(Clone, Copy, Debug)]
pub(super) struct Avx512(__m512);

impl Avx512 {
    #[inline(always)]
    fn add_(self, other: Self) -> Self {
        Self(unsafe { _mm512_add_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn sub_(self, other: Self) -> Self {
        Self(unsafe { _mm512_sub_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_(self, other: Self) -> Self {
        Self(unsafe { _mm512_mul_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn div_(self, other: Self) -> Self {
        Self(unsafe { _mm512_div_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn neg_(self) -> Self {
        // AVX-512F has no float XOR (that's AVX-512DQ), so flip the sign
        // bits as integers.
        Self(unsafe {
            _mm512_castsi512_ps(_mm512_xor_si512(
                _mm512_castps_si512(self.0),
                _mm512_set1_epi32(i32::MIN),
            ))
        })
    }
}

lane_ops!(Avx512);

impl F32s for Avx512 {
    const LANES: usize = 16;
    const LEVEL: Level = Level::Avx512;
    type U32s = Avx512U;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        Self(unsafe { _mm512_set1_ps(value) })
    }

    #[inline(always)]
    fn load(from: &[f32]) -> Self {
        let from = &from[..16];
        Self(unsafe { _mm512_loadu_ps(from.as_ptr()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [f32]) {
        let to = &mut to[..16];
        unsafe { _mm512_storeu_ps(to.as_mut_ptr(), self.0) }
    }

    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        Self(unsafe { _mm512_fmadd_ps(self.0, mul.0, add.0) })
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { _mm512_min_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { _mm512_max_ps(self.0, other.0) })
    }

    #[inline(always)]
    fn abs(self) -> Self {
        Self(unsafe { _mm512_abs_ps(self.0) })
    }

    #[inline(always)]
    fn sqrt(self) -> Self {
        Self(unsafe { _mm512_sqrt_ps(self.0) })
    }

    #[inline(always)]
    fn sum(self) -> f32 {
        unsafe { _mm512_reduce_add_ps(self.0) }
    }

    #[inline(always)]
    fn floor(self) -> Self {
        // Round towards negative infinity, without raising the inexact
        // exception.
        Self(unsafe { _mm512_roundscale_ps::<0x09>(self.0) })
    }

    #[inline(always)]
    fn to_bits(self) -> Avx512U {
        Avx512U(unsafe { _mm512_castps_si512(self.0) })
    }

    #[inline(always)]
    fn from_bits(bits: Avx512U) -> Self {
        Self(unsafe { _mm512_castsi512_ps(bits.0) })
    }

    #[inline(always)]
    fn to_i32(self) -> Avx512U {
        Avx512U(unsafe { _mm512_cvttps_epi32(self.0) })
    }

    #[inline(always)]
    fn from_i32(lanes: Avx512U) -> Self {
        Self(unsafe { _mm512_cvtepi32_ps(lanes.0) })
    }

    #[inline(always)]
    fn gather<const N: usize>(table: &[f32], index: Avx512U) -> [Self; N] {
        let limit = gather_limit::<N>(table);
        let index = index.min(Avx512U::splat(limit));
        let base = table.as_ptr();
        // SAFETY: as for AVX2: every index is at most `table.len() − N`,
        // which fits an `i32`.
        std::array::from_fn(|j| Self(unsafe { _mm512_i32gather_ps::<4>(index.0, base.add(j)) }))
    }
}

/// Four `u32` lanes in an SSE register.
#[derive(Clone, Copy, Debug)]
pub(super) struct Sse42U(__m128i);

impl U32s for Sse42U {
    const LANES: usize = 4;

    #[inline(always)]
    fn splat(value: u32) -> Self {
        Self(unsafe { _mm_set1_epi32(value as i32) })
    }

    #[inline(always)]
    fn load(from: &[u32]) -> Self {
        let from = &from[..4];
        Self(unsafe { _mm_loadu_si128(from.as_ptr().cast()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [u32]) {
        let to = &mut to[..4];
        unsafe { _mm_storeu_si128(to.as_mut_ptr().cast(), self.0) }
    }

    #[inline(always)]
    fn wrapping_add(self, other: Self) -> Self {
        Self(unsafe { _mm_add_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_sub(self, other: Self) -> Self {
        Self(unsafe { _mm_sub_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_mul(self, other: Self) -> Self {
        Self(unsafe { _mm_mullo_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_high(self, other: Self) -> Self {
        // The even lanes' 64-bit products, then the odd lanes'; keep each
        // product's high half.
        Self(unsafe {
            let even = _mm_srli_epi64::<32>(_mm_mul_epu32(self.0, other.0));
            let odd = _mm_mul_epu32(_mm_srli_epi64::<32>(self.0), _mm_srli_epi64::<32>(other.0));
            _mm_blend_epi16::<0b1100_1100>(even, odd)
        })
    }

    #[inline(always)]
    fn shl<const N: i32>(self) -> Self {
        Self(unsafe { _mm_slli_epi32::<N>(self.0) })
    }

    #[inline(always)]
    fn shr<const N: i32>(self) -> Self {
        Self(unsafe { _mm_srli_epi32::<N>(self.0) })
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { _mm_min_epu32(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { _mm_max_epu32(self.0, other.0) })
    }
}

/// Eight `u32` lanes in an AVX register.
#[derive(Clone, Copy, Debug)]
pub(super) struct Avx2U(__m256i);

impl U32s for Avx2U {
    const LANES: usize = 8;

    #[inline(always)]
    fn splat(value: u32) -> Self {
        Self(unsafe { _mm256_set1_epi32(value as i32) })
    }

    #[inline(always)]
    fn load(from: &[u32]) -> Self {
        let from = &from[..8];
        Self(unsafe { _mm256_loadu_si256(from.as_ptr().cast()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [u32]) {
        let to = &mut to[..8];
        unsafe { _mm256_storeu_si256(to.as_mut_ptr().cast(), self.0) }
    }

    #[inline(always)]
    fn wrapping_add(self, other: Self) -> Self {
        Self(unsafe { _mm256_add_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_sub(self, other: Self) -> Self {
        Self(unsafe { _mm256_sub_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_mul(self, other: Self) -> Self {
        Self(unsafe { _mm256_mullo_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_high(self, other: Self) -> Self {
        Self(unsafe {
            let even = _mm256_srli_epi64::<32>(_mm256_mul_epu32(self.0, other.0));
            let odd = _mm256_mul_epu32(
                _mm256_srli_epi64::<32>(self.0),
                _mm256_srli_epi64::<32>(other.0),
            );
            _mm256_blend_epi32::<0b1010_1010>(even, odd)
        })
    }

    #[inline(always)]
    fn shl<const N: i32>(self) -> Self {
        Self(unsafe { _mm256_slli_epi32::<N>(self.0) })
    }

    #[inline(always)]
    fn shr<const N: i32>(self) -> Self {
        Self(unsafe { _mm256_srli_epi32::<N>(self.0) })
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { _mm256_min_epu32(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { _mm256_max_epu32(self.0, other.0) })
    }
}

/// Sixteen `u32` lanes in an AVX-512 register.
#[derive(Clone, Copy, Debug)]
pub(super) struct Avx512U(__m512i);

impl U32s for Avx512U {
    const LANES: usize = 16;

    #[inline(always)]
    fn splat(value: u32) -> Self {
        Self(unsafe { _mm512_set1_epi32(value as i32) })
    }

    #[inline(always)]
    fn load(from: &[u32]) -> Self {
        let from = &from[..16];
        Self(unsafe { _mm512_loadu_si512(from.as_ptr().cast()) })
    }

    #[inline(always)]
    fn store(self, to: &mut [u32]) {
        let to = &mut to[..16];
        unsafe { _mm512_storeu_si512(to.as_mut_ptr().cast(), self.0) }
    }

    #[inline(always)]
    fn wrapping_add(self, other: Self) -> Self {
        Self(unsafe { _mm512_add_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_sub(self, other: Self) -> Self {
        Self(unsafe { _mm512_sub_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn wrapping_mul(self, other: Self) -> Self {
        Self(unsafe { _mm512_mullo_epi32(self.0, other.0) })
    }

    #[inline(always)]
    fn mul_high(self, other: Self) -> Self {
        Self(unsafe {
            let even = _mm512_srli_epi64::<32>(_mm512_mul_epu32(self.0, other.0));
            let odd = _mm512_mul_epu32(
                _mm512_srli_epi64::<32>(self.0),
                _mm512_srli_epi64::<32>(other.0),
            );
            _mm512_mask_blend_epi32(0b1010_1010_1010_1010, even, odd)
        })
    }

    #[inline(always)]
    fn shl<const N: i32>(self) -> Self {
        // AVX-512F's immediate shifts take a `u32` count; a variable shift
        // by a constant vector costs the same.
        Self(unsafe { _mm512_sllv_epi32(self.0, _mm512_set1_epi32(N)) })
    }

    #[inline(always)]
    fn shr<const N: i32>(self) -> Self {
        Self(unsafe { _mm512_srlv_epi32(self.0, _mm512_set1_epi32(N)) })
    }

    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(unsafe { _mm512_min_epu32(self.0, other.0) })
    }

    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(unsafe { _mm512_max_epu32(self.0, other.0) })
    }
}
