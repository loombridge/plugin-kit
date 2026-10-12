//! SIMD helpers: lanes of `f32` with an explicit path for each instruction
//! set, chosen once at run time.
//!
//! | [`Level`] | Lanes | Where |
//! |---|---|---|
//! | [`Level::Avx512`] | 16 | x86-64 with AVX-512F |
//! | [`Level::Avx2`] | 8 | x86-64 with AVX2 and FMA |
//! | [`Level::Sse42`] | 4 | x86-64 with SSE4.2 |
//! | [`Level::Neon`] | 4 | aarch64 (Apple Silicon, Arm Windows and Linux) |
//! | [`Level::Portable`] | 4 | anywhere: plain arrays the compiler may vectorise |
//!
//! A kernel is written once, generic over [`F32s`], and run with
//! [`dispatch`], which picks the best level this CPU has ([`level`]) and
//! runs the kernel compiled for it:
//!
//! ```
//! use lspk_dsp::simd::{self, F32s, Kernel};
//!
//! /// Doubles every sample.
//! struct Double<'a>(&'a mut [f32]);
//!
//! impl Kernel for Double<'_> {
//!     type Output = ();
//!     #[inline(always)]
//!     fn run<S: F32s>(self) {
//!         let mut chunks = self.0.chunks_exact_mut(S::LANES);
//!         for chunk in &mut chunks {
//!             (S::load(chunk) * S::splat(2.0)).store(chunk);
//!         }
//!         for sample in chunks.into_remainder() {
//!             *sample *= 2.0;
//!         }
//!     }
//! }
//!
//! let mut audio = [1.0; 37];
//! simd::dispatch(Double(&mut audio));
//! assert_eq!(audio, [2.0; 37]);
//! ```
//!
//! # Same results on every path
//!
//! For finite inputs, every path gives the same bits for `+`, `-`, `*`,
//! `/`, negation, [`abs`](F32s::abs), [`min`](F32s::min),
//! [`max`](F32s::max) and [`sqrt`](F32s::sqrt), as IEEE 754 requires each
//! to be correctly rounded. Two operations differ within a stated
//! tolerance, which the tests check on every path the machine has:
//!
//! - [`mul_add`](F32s::mul_add) is fused (one rounding) on NEON, AVX2 and
//!   AVX-512 and unfused (two) on SSE4.2 and the portable path, so results
//!   agree within `2 × f32::EPSILON × (|a × b| + |c|)`.
//! - [`sum`](F32s::sum) adds the lanes in a different order on each path,
//!   so results agree within `LANES × f32::EPSILON × Σ|x|`.
//!
//! Results for NaN inputs, and the sign of a zero from `min` or `max` of
//! zeros of different signs, are unspecified.
//!
//! # Integer lanes
//!
//! Each lane type has a companion of `u32` lanes, [`F32s::U32s`], for the
//! work audio code does in fixed point: wrapping phase accumulators, table
//! indices, bit patterns. Integer operations, the conversions between the
//! two and [`F32s::gather`] give the same bits on every path; so do
//! [`floor`](F32s::floor) and the [`exp2`] built on them, whose error the
//! tests bound.
//!
//! # Choosing the level
//!
//! [`level`] detects the CPU's features on its first call and remembers
//! the answer, so call it once off the audio thread (at activation, say);
//! after that it's one atomic load. The environment variable `LSPK_SIMD`
//! (`portable`, `neon`, `sse4.2`, `avx2` or `avx512`) caps the level, for
//! benchmarks that must count the same instructions on every machine and
//! for comparing paths; it never raises the level above what the CPU has.
//!
//! The lane types themselves are private: a kernel only ever meets one
//! through [`dispatch`], which runs it only on a CPU that has its
//! instructions.

use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};
use std::sync::atomic::{AtomicU8, Ordering};

/// The operator traits for a lane type, from its inherent `add`, `sub`,
/// `mul`, `div` and `neg` functions (named with a trailing `_`).
macro_rules! lane_ops {
    ($t:ty) => {
        impl std::ops::Add for $t {
            type Output = Self;
            #[inline(always)]
            fn add(self, other: Self) -> Self {
                self.add_(other)
            }
        }
        impl std::ops::Sub for $t {
            type Output = Self;
            #[inline(always)]
            fn sub(self, other: Self) -> Self {
                self.sub_(other)
            }
        }
        impl std::ops::Mul for $t {
            type Output = Self;
            #[inline(always)]
            fn mul(self, other: Self) -> Self {
                self.mul_(other)
            }
        }
        impl std::ops::Div for $t {
            type Output = Self;
            #[inline(always)]
            fn div(self, other: Self) -> Self {
                self.div_(other)
            }
        }
        impl std::ops::Neg for $t {
            type Output = Self;
            #[inline(always)]
            fn neg(self) -> Self {
                self.neg_()
            }
        }
        impl std::ops::AddAssign for $t {
            #[inline(always)]
            fn add_assign(&mut self, other: Self) {
                *self = self.add_(other);
            }
        }
        impl std::ops::SubAssign for $t {
            #[inline(always)]
            fn sub_assign(&mut self, other: Self) {
                *self = self.sub_(other);
            }
        }
        impl std::ops::MulAssign for $t {
            #[inline(always)]
            fn mul_assign(&mut self, other: Self) {
                *self = self.mul_(other);
            }
        }
    };
}

mod math;
#[cfg(target_arch = "aarch64")]
mod neon;
mod portable;
#[cfg(target_arch = "x86_64")]
mod x86;

/// An instruction set the lanes can run on, from the least to the most
/// capable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Plain arrays, on any CPU.
    Portable,
    /// Arm's Advanced SIMD, on every aarch64 CPU.
    Neon,
    /// SSE up to 4.2, on x86-64.
    Sse42,
    /// AVX2 with fused multiply-add, on x86-64.
    Avx2,
    /// AVX-512 Foundation, on x86-64.
    Avx512,
}

impl Level {
    /// Every level, least capable first.
    pub const ALL: [Self; 5] = [
        Self::Portable,
        Self::Neon,
        Self::Sse42,
        Self::Avx2,
        Self::Avx512,
    ];

    /// Its name, as `LSPK_SIMD` takes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::Neon => "neon",
            Self::Sse42 => "sse4.2",
            Self::Avx2 => "avx2",
            Self::Avx512 => "avx512",
        }
    }

    /// The level named `name`, as [`name`](Self::name) writes it.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.name() == name)
    }

    /// How many `f32` lanes its vectors hold.
    #[must_use]
    pub const fn lanes(self) -> usize {
        match self {
            Self::Portable | Self::Neon | Self::Sse42 => 4,
            Self::Avx2 => 8,
            Self::Avx512 => 16,
        }
    }

    /// Whether this CPU can run it.
    #[must_use]
    pub fn is_supported(self) -> bool {
        match self {
            Self::Portable => true,
            #[cfg(target_arch = "aarch64")]
            Self::Neon => std::arch::is_aarch64_feature_detected!("neon"),
            #[cfg(target_arch = "x86_64")]
            Self::Sse42 => std::arch::is_x86_feature_detected!("sse4.2"),
            #[cfg(target_arch = "x86_64")]
            Self::Avx2 => {
                std::arch::is_x86_feature_detected!("avx2")
                    && std::arch::is_x86_feature_detected!("fma")
            }
            #[cfg(target_arch = "x86_64")]
            Self::Avx512 => {
                std::arch::is_x86_feature_detected!("avx512f")
                    && std::arch::is_x86_feature_detected!("avx2")
                    && std::arch::is_x86_feature_detected!("fma")
            }
            #[allow(
                unreachable_patterns,
                reason = "which arms remain depends on the target"
            )]
            _ => false,
        }
    }

    /// The most capable level this CPU can run, at most `cap`.
    #[must_use]
    pub fn best(cap: Self) -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|level| *level <= cap && level.is_supported())
            .unwrap_or(Self::Portable)
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The environment variable that caps [`level`].
pub const LEVEL_ENV: &str = "LSPK_SIMD";

/// [`LEVEL`] before [`level`] has run.
const UNKNOWN: u8 = u8::MAX;

/// The level [`dispatch`] uses, once known.
static LEVEL: AtomicU8 = AtomicU8::new(UNKNOWN);

/// The level [`dispatch`] runs kernels at: the best this CPU has, capped by
/// `LSPK_SIMD` if it's set. The first call reads the environment and the
/// CPU's features, so make it off the audio thread.
#[must_use]
pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        UNKNOWN => {
            let cap = std::env::var(LEVEL_ENV)
                .ok()
                .and_then(|name| Level::parse(name.trim()))
                .unwrap_or(Level::Avx512);
            let level = Level::best(cap);
            LEVEL.store(level as u8, Ordering::Relaxed);
            level
        }
        known => Level::ALL[usize::from(known)],
    }
}

pub use math::exp2;

/// Lanes of `u32`, operated on together, the companion of an [`F32s`]
/// type with the same number of lanes. Arithmetic wraps, as a phase
/// accumulator wants.
pub trait U32s: Copy + Send + Sync + 'static {
    /// How many lanes it holds.
    const LANES: usize;

    /// Every lane `value`.
    fn splat(value: u32) -> Self;
    /// The first [`LANES`](Self::LANES) values of `from`.
    ///
    /// # Panics
    ///
    /// If `from` is shorter.
    fn load(from: &[u32]) -> Self;
    /// Writes its lanes to the first [`LANES`](Self::LANES) values of `to`.
    ///
    /// # Panics
    ///
    /// If `to` is shorter.
    fn store(self, to: &mut [u32]);
    /// Each lane pair added, wrapping.
    #[must_use]
    fn wrapping_add(self, other: Self) -> Self;
    /// Each lane pair subtracted, wrapping.
    #[must_use]
    fn wrapping_sub(self, other: Self) -> Self;
    /// The low 32 bits of each lane pair's product.
    #[must_use]
    fn wrapping_mul(self, other: Self) -> Self;
    /// The high 32 bits of each lane pair's 64-bit product: `a × b >> 32`.
    #[must_use]
    fn mul_high(self, other: Self) -> Self;
    /// Each lane shifted left by `N` bits, from 0 to 31.
    #[must_use]
    fn shl<const N: i32>(self) -> Self;
    /// Each lane shifted right by `N` bits, from 1 to 31, filling with
    /// zeros.
    #[must_use]
    fn shr<const N: i32>(self) -> Self;
    /// The smaller of each lane pair, as unsigned numbers.
    #[must_use]
    fn min(self, other: Self) -> Self;
    /// The larger of each lane pair, as unsigned numbers.
    #[must_use]
    fn max(self, other: Self) -> Self;
}

/// Lanes of `f32`, operated on together. Arithmetic works lane by lane.
pub trait F32s:
    Copy
    + Send
    + Sync
    + 'static
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
    + AddAssign
    + SubAssign
    + MulAssign
{
    /// How many lanes it holds.
    const LANES: usize;
    /// The level it runs at.
    const LEVEL: Level;
    /// Its companion integer lanes.
    type U32s: U32s;

    /// Every lane `value`.
    fn splat(value: f32) -> Self;
    /// The first [`LANES`](Self::LANES) values of `from`.
    ///
    /// # Panics
    ///
    /// If `from` is shorter.
    fn load(from: &[f32]) -> Self;
    /// Writes its lanes to the first [`LANES`](Self::LANES) values of `to`.
    ///
    /// # Panics
    ///
    /// If `to` is shorter.
    fn store(self, to: &mut [f32]);
    /// `self × mul + add`, fused where the instruction set has it.
    #[must_use]
    fn mul_add(self, mul: Self, add: Self) -> Self;
    /// The smaller of each lane pair.
    #[must_use]
    fn min(self, other: Self) -> Self;
    /// The larger of each lane pair.
    #[must_use]
    fn max(self, other: Self) -> Self;
    /// Each lane's magnitude.
    #[must_use]
    fn abs(self) -> Self;
    /// Each lane's square root.
    #[must_use]
    fn sqrt(self) -> Self;
    /// Every lane added up.
    #[must_use]
    fn sum(self) -> f32;
    /// Each lane rounded down to a whole number.
    #[must_use]
    fn floor(self) -> Self;
    /// Each lane's bits.
    #[must_use]
    fn to_bits(self) -> Self::U32s;
    /// Lanes with these bits.
    #[must_use]
    fn from_bits(bits: Self::U32s) -> Self;
    /// Each lane truncated towards zero, as an `i32`'s bits. Lanes outside
    /// `i32`'s range give unspecified values.
    #[must_use]
    fn to_i32(self) -> Self::U32s;
    /// Each integer lane, read as an `i32`, rounded to the nearest `f32`.
    #[must_use]
    fn from_i32(lanes: Self::U32s) -> Self;
    /// For each lane, the `N` values of `table` from that lane's index on:
    /// row `j` holds `table[index + j]`. An index past `table.len() − N` is
    /// clamped to it, so the read never leaves the table.
    ///
    /// # Panics
    ///
    /// If `table` holds fewer than `N` values, or more than `i32::MAX`.
    #[must_use]
    fn gather<const N: usize>(table: &[f32], index: Self::U32s) -> [Self; N];
}

/// [`F32s::gather`] one lane at a time, for the paths without a gather
/// instruction: `rows[j][i]` is `table[min(index[i], limit) + j]`.
#[inline(always)]
fn gather_lanes<const L: usize, const N: usize>(
    table: &[f32],
    index: [u32; L],
    limit: u32,
) -> [[f32; L]; N] {
    debug_assert!(limit as usize + N <= table.len());
    let mut rows = [[0.0; L]; N];
    for (i, &at) in index.iter().enumerate() {
        let at = at.min(limit) as usize;
        // SAFETY: `at` is at most `limit`, which `gather_limit` made
        // `table.len() − N`, so the `N` values from `at` are in the table.
        let values = unsafe { &*table.as_ptr().add(at).cast::<[f32; N]>() };
        for (row, &value) in rows.iter_mut().zip(values) {
            row[i] = value;
        }
    }
    rows
}

/// The largest index [`F32s::gather`] reads `N` values from in `table`,
/// checking the table's size.
#[inline(always)]
fn gather_limit<const N: usize>(table: &[f32]) -> u32 {
    assert!(
        table.len() >= N && i32::try_from(table.len()).is_ok(),
        "a gather of {N} from {} values",
        table.len()
    );
    (table.len() - N) as u32
}

/// A computation written once for any lane type, run by [`dispatch`].
///
/// Mark `run` `#[inline(always)]`, and keep the lane operations inside it,
/// so each level's copy is compiled with that level's instructions.
pub trait Kernel {
    /// What it returns.
    type Output;
    /// Runs it with lanes `S`.
    fn run<S: F32s>(self) -> Self::Output;
}

/// Runs `kernel` at [`level`].
pub fn dispatch<K: Kernel>(kernel: K) -> K::Output {
    match dispatch_at(level(), kernel) {
        Ok(output) => output,
        // `level` only returns levels this CPU has.
        Err(kernel) => kernel.run::<portable::F32x4>(),
    }
}

/// Runs `kernel` at `level`, or gives it back if this CPU can't run that
/// level. For tests and benchmarks that compare paths.
///
/// # Errors
///
/// The CPU doesn't have `level`'s instructions; the kernel comes back.
pub fn dispatch_at<K: Kernel>(level: Level, kernel: K) -> Result<K::Output, K> {
    if !level.is_supported() {
        return Err(kernel);
    }
    match level {
        Level::Portable => Ok(kernel.run::<portable::F32x4>()),
        #[cfg(target_arch = "aarch64")]
        Level::Neon => Ok(neon::run(kernel)),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the CPU has SSE4.2, checked above.
        Level::Sse42 => Ok(unsafe { x86::run_sse42(kernel) }),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the CPU has AVX2 and FMA, checked above.
        Level::Avx2 => Ok(unsafe { x86::run_avx2(kernel) }),
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the CPU has AVX-512F, AVX2 and FMA, checked above.
        Level::Avx512 => Ok(unsafe { x86::run_avx512(kernel) }),
        #[allow(
            unreachable_patterns,
            reason = "which arms remain depends on the target"
        )]
        _ => Err(kernel),
    }
}

/// Adds `src × gain` to `dst`, over as many samples as the shorter holds.
pub fn mix(dst: &mut [f32], src: &[f32], gain: f32) {
    dispatch(Mix { dst, src, gain });
}

/// Every sample of `samples` squared and added up, as a meter's RMS needs.
#[must_use]
pub fn sum_of_squares(samples: &[f32]) -> f32 {
    dispatch(SumOfSquares(samples))
}

/// [`mix`]'s kernel.
struct Mix<'a> {
    dst: &'a mut [f32],
    src: &'a [f32],
    gain: f32,
}

impl Kernel for Mix<'_> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        let n = self.dst.len().min(self.src.len());
        let (dst, src) = (&mut self.dst[..n], &self.src[..n]);
        let gain = S::splat(self.gain);
        let mut out = dst.chunks_exact_mut(S::LANES);
        let mut input = src.chunks_exact(S::LANES);
        for (out, input) in (&mut out).zip(&mut input) {
            S::load(input).mul_add(gain, S::load(out)).store(out);
        }
        for (out, input) in out.into_remainder().iter_mut().zip(input.remainder()) {
            *out += input * self.gain;
        }
    }
}

/// [`sum_of_squares`]'s kernel.
struct SumOfSquares<'a>(&'a [f32]);

impl Kernel for SumOfSquares<'_> {
    type Output = f32;

    #[inline(always)]
    fn run<S: F32s>(self) -> f32 {
        let mut total = S::splat(0.0);
        let mut chunks = self.0.chunks_exact(S::LANES);
        for chunk in &mut chunks {
            let x = S::load(chunk);
            total = x.mul_add(x, total);
        }
        total.sum() + chunks.remainder().iter().map(|x| x * x).sum::<f32>()
    }
}

#[cfg(test)]
mod tests;
