//! The arithmetic the shapers are written in, once for one lane (`f32`) and
//! once for a vector of lanes ([`V`] around an [`F32s`]), so the scalar
//! path and every SIMD level run the same steps; and the fast maths they
//! need (sine, arctangent, `2^x`, the hyperbolic tangent), each with its
//! error bounded by a test.

use std::cell::Cell;
use std::ops::{Add, Div, Mul, Neg, Sub};

use lspk_dsp::simd::{self, F32s, U32s};

/// One lane or a vector of lanes of `f32`, with their phase words.
pub(crate) trait Lane:
    Copy
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
{
    /// The phase words that go with it.
    type Word: Copy;

    /// Every lane `value`.
    fn splat(value: f32) -> Self;
    /// The lanes from index `at` of `from`.
    fn load(from: &[f32], at: usize) -> Self;
    /// Writes the lanes to `to` from index `at`.
    fn store(self, to: &mut [f32], at: usize);
    /// `self × mul + add`, fused where the instruction set has it.
    #[must_use]
    fn mul_add(self, mul: Self, add: Self) -> Self;
    #[must_use]
    fn min(self, other: Self) -> Self;
    #[must_use]
    fn max(self, other: Self) -> Self;
    #[must_use]
    fn abs(self) -> Self;
    #[must_use]
    fn sqrt(self) -> Self;
    #[must_use]
    fn floor(self) -> Self;
    /// 1 where the sign bit is set (negative numbers and −0), else 0.
    #[must_use]
    fn sign_bit(self) -> Self;
    /// `2^self`, as [`simd::exp2`] computes it.
    #[must_use]
    fn exp2(self) -> Self;
    /// Whether any lane isn't 0.
    fn any(self) -> bool;
    /// For positive normal numbers: the exponent, unbiased, and the
    /// mantissa, 1 to 2.
    fn split(self) -> (Self, Self);

    /// Every lane's word `value`.
    fn word(value: u32) -> Self::Word;
    /// Word lanes added, wrapping.
    fn add_words(a: Self::Word, b: Self::Word) -> Self::Word;
    /// `b` where `mask` is 1, `a` where it's 0.
    fn pick_word(mask: Self, a: Self::Word, b: Self::Word) -> Self::Word;
    /// A phase word read as a signed fraction of a cycle, −½ to ½.
    fn signed(word: Self::Word) -> Self;
    /// Cycles `x` (any number within ±2²³) wrapped to a phase word.
    fn to_word(x: Self) -> Self::Word;
    /// `table` (`segments + 1` values spanning 0 to 1) read at `x`, linear
    /// between its values; `x` is clamped to 0 to 1.
    fn lookup(table: &[f32], segments: usize, x: Self) -> Self;

    /// The lanes from index `at` of per-lane `cells`, through `scratch` (at
    /// least `S::LANES` long): one sample's feedback between the stages,
    /// which both hold as shared cells.
    fn load_cells(cells: &[Cell<f32>], at: usize, scratch: &mut [f32]) -> Self;
    /// Writes the lanes to `cells` from index `at`, through `scratch`.
    fn store_cells(self, cells: &[Cell<f32>], at: usize, scratch: &mut [f32]);
}

/// Lanes of an [`F32s`], wrapped so they can be a [`Lane`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct V<S>(pub S);

macro_rules! v_op {
    ($trait:ident, $f:ident) => {
        impl<S: F32s> $trait for V<S> {
            type Output = Self;
            #[inline(always)]
            fn $f(self, other: Self) -> Self {
                V(self.0.$f(other.0))
            }
        }
    };
}
v_op!(Add, add);
v_op!(Sub, sub);
v_op!(Mul, mul);
v_op!(Div, div);

impl<S: F32s> Neg for V<S> {
    type Output = Self;
    #[inline(always)]
    fn neg(self) -> Self {
        V(-self.0)
    }
}

/// 2³².
const TWO_32: f32 = 4_294_967_296.0;

impl<S: F32s> Lane for V<S> {
    type Word = S::U32s;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        V(S::splat(value))
    }
    #[inline(always)]
    fn load(from: &[f32], at: usize) -> Self {
        V(S::load(&from[at..]))
    }
    #[inline(always)]
    fn store(self, to: &mut [f32], at: usize) {
        self.0.store(&mut to[at..]);
    }
    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        V(self.0.mul_add(mul.0, add.0))
    }
    #[inline(always)]
    fn min(self, other: Self) -> Self {
        V(self.0.min(other.0))
    }
    #[inline(always)]
    fn max(self, other: Self) -> Self {
        V(self.0.max(other.0))
    }
    #[inline(always)]
    fn abs(self) -> Self {
        V(self.0.abs())
    }
    #[inline(always)]
    fn sqrt(self) -> Self {
        V(self.0.sqrt())
    }
    #[inline(always)]
    fn floor(self) -> Self {
        V(self.0.floor())
    }
    #[inline(always)]
    fn sign_bit(self) -> Self {
        V(S::from_i32(self.0.to_bits().shr::<31>()))
    }
    #[inline(always)]
    fn exp2(self) -> Self {
        V(simd::exp2(self.0))
    }
    #[inline(always)]
    fn any(self) -> bool {
        self.0.abs().sum() != 0.0
    }
    #[inline(always)]
    fn split(self) -> (Self, Self) {
        let biased = self.0.to_bits().shr::<23>();
        let scale = S::from_bits(<S::U32s>::splat(254).wrapping_sub(biased).shl::<23>());
        (V(S::from_i32(biased) - S::splat(127.0)), V(self.0 * scale))
    }
    #[inline(always)]
    fn word(value: u32) -> S::U32s {
        <S::U32s>::splat(value)
    }
    #[inline(always)]
    fn add_words(a: S::U32s, b: S::U32s) -> S::U32s {
        a.wrapping_add(b)
    }
    #[inline(always)]
    fn pick_word(mask: Self, a: S::U32s, b: S::U32s) -> S::U32s {
        // The mask as 0 or 1 in the integer lanes: a + (b − a) × mask.
        a.wrapping_add(b.wrapping_sub(a).wrapping_mul(mask.0.to_i32()))
    }
    #[inline(always)]
    fn signed(word: S::U32s) -> Self {
        V(S::from_i32(word) * S::splat(1.0 / TWO_32))
    }
    #[inline(always)]
    fn to_word(x: Self) -> S::U32s {
        let wrapped = x.0 - (x.0 + S::splat(0.5)).floor();
        (wrapped * S::splat(TWO_32)).to_i32()
    }
    #[inline(always)]
    fn lookup(table: &[f32], segments: usize, x: Self) -> Self {
        let at = clamp(x, 0.0, 1.0).0 * S::splat(segments as f32);
        let whole = at.floor().min(S::splat((segments - 1) as f32));
        let [a, b] = S::gather::<2>(table, whole.to_i32());
        V((b - a).mul_add(at - whole, a))
    }
    #[inline(always)]
    fn load_cells(cells: &[Cell<f32>], at: usize, scratch: &mut [f32]) -> Self {
        for (i, cell) in cells[at..at + S::LANES].iter().enumerate() {
            scratch[i] = cell.get();
        }
        V(S::load(scratch))
    }
    #[inline(always)]
    fn store_cells(self, cells: &[Cell<f32>], at: usize, scratch: &mut [f32]) {
        self.0.store(scratch);
        for (i, cell) in cells[at..at + S::LANES].iter().enumerate() {
            cell.set(scratch[i]);
        }
    }
}

impl Lane for f32 {
    type Word = u32;

    #[inline(always)]
    fn splat(value: f32) -> Self {
        value
    }
    #[inline(always)]
    fn load(from: &[f32], at: usize) -> Self {
        from[at]
    }
    #[inline(always)]
    fn store(self, to: &mut [f32], at: usize) {
        to[at] = self;
    }
    #[inline(always)]
    fn mul_add(self, mul: Self, add: Self) -> Self {
        // Unfused, as the portable and SSE4.2 paths.
        self * mul + add
    }
    #[inline(always)]
    fn min(self, other: Self) -> Self {
        if other < self { other } else { self }
    }
    #[inline(always)]
    fn max(self, other: Self) -> Self {
        if other > self { other } else { self }
    }
    #[inline(always)]
    fn abs(self) -> Self {
        f32::abs(self)
    }
    #[inline(always)]
    fn sqrt(self) -> Self {
        f32::sqrt(self)
    }
    #[inline(always)]
    fn floor(self) -> Self {
        f32::floor(self)
    }
    #[inline(always)]
    fn sign_bit(self) -> Self {
        (self.to_bits() >> 31) as f32
    }
    #[inline(always)]
    fn exp2(self) -> Self {
        exp2(self)
    }
    #[inline(always)]
    fn any(self) -> bool {
        self != 0.0
    }
    #[inline(always)]
    fn split(self) -> (Self, Self) {
        let biased = self.to_bits() >> 23;
        let scale = f32::from_bits(254u32.wrapping_sub(biased) << 23);
        (biased as i32 as f32 - 127.0, self * scale)
    }
    #[inline(always)]
    fn word(value: u32) -> u32 {
        value
    }
    #[inline(always)]
    fn add_words(a: u32, b: u32) -> u32 {
        a.wrapping_add(b)
    }
    #[inline(always)]
    fn pick_word(mask: Self, a: u32, b: u32) -> u32 {
        a.wrapping_add(b.wrapping_sub(a).wrapping_mul(mask as i32 as u32))
    }
    #[inline(always)]
    fn signed(word: u32) -> Self {
        word as i32 as f32 * (1.0 / TWO_32)
    }
    #[inline(always)]
    fn to_word(x: Self) -> u32 {
        let wrapped = x - (x + 0.5).floor();
        // As `cvttps2dq` does: out of range is i32::MIN, here only ½.
        let scaled = wrapped * TWO_32;
        if scaled >= 2_147_483_648.0 {
            1 << 31
        } else {
            scaled as i32 as u32
        }
    }
    #[inline(always)]
    fn lookup(table: &[f32], segments: usize, x: Self) -> Self {
        let at = x.clamp(0.0, 1.0) * segments as f32;
        let whole = at.floor().min((segments - 1) as f32);
        let i = whole as usize;
        Lane::mul_add(table[i + 1] - table[i], at - whole, table[i])
    }
    #[inline(always)]
    fn load_cells(cells: &[Cell<f32>], at: usize, _: &mut [f32]) -> Self {
        cells[at].get()
    }
    #[inline(always)]
    fn store_cells(self, cells: &[Cell<f32>], at: usize, _: &mut [f32]) {
        cells[at].set(self);
    }
}

/// `2^x` for one lane, by [`simd::exp2`]'s polynomial and steps.
fn exp2(x: f32) -> f32 {
    const C: [f32; 6] = [
        0.999_999_94,
        0.693_153_1,
        0.240_153_55,
        0.055_826_403,
        0.008_989_328,
        0.001_877_552_8,
    ];
    let x = x.clamp(-126.0, 126.0);
    let whole = x.floor();
    let f = x - whole;
    let p = C[..5].iter().rev().fold(C[5], |p, &c| p * f + c);
    f32::from_bits(p.to_bits().wrapping_add(((whole as i32) << 23) as u32))
}

/// `b` where `mask` is 1, `a` where it's 0.
#[inline(always)]
pub(crate) fn pick<L: Lane>(mask: L, a: L, b: L) -> L {
    (b - a).mul_add(mask, a)
}

/// 1 where `x > 0`, else 0 (+0 counts as above).
#[inline(always)]
pub(crate) fn above<L: Lane>(x: L) -> L {
    (-x).sign_bit()
}

/// −1, or 1 for positive numbers and +0.
#[inline(always)]
pub(crate) fn sign<L: Lane>(x: L) -> L {
    L::splat(1.0) - x.sign_bit() * L::splat(2.0)
}

/// `x` clamped to `lo..=hi`.
#[inline(always)]
pub(crate) fn clamp<L: Lane>(x: L, lo: f32, hi: f32) -> L {
    x.max(L::splat(lo)).min(L::splat(hi))
}

/// The C2 step from 0 to 1 as `t` goes from 0 to 1, clamped outside:
/// 6t⁵ − 15t⁴ + 10t³, whose first two derivatives are 0 at both ends.
#[inline(always)]
pub(crate) fn smoothstep<L: Lane>(t: L) -> L {
    let t = clamp(t, 0.0, 1.0);
    let c = L::splat;
    t * t * t * t.mul_add(t.mul_add(c(6.0), c(-15.0)), c(10.0))
}

/// The odd minimax polynomial for sin x on |x| ≤ π/2, in powers of
/// x = 2πa: error below 4 × 10⁻⁹ in exact arithmetic.
const SIN: [f32; 5] = [
    0.999_999_98,
    -0.166_666_48,
    0.008_332_899,
    -0.000_198_008_3,
    2.590_370_3e-6,
];

/// sin 2πt, for `t` in cycles (within ±2²³), within 2 × 10⁻⁷.
#[inline(always)]
pub(crate) fn sin_cycles<L: Lane>(t: L) -> L {
    let c = L::splat;
    let wrapped = t - (t + c(0.5)).floor();
    let a = wrapped.abs();
    // sin(π − x) = sin x: fold onto a quarter cycle.
    let a = a.min(c(0.5) - a);
    let x = a * c(std::f32::consts::TAU);
    let x2 = x * x;
    let mut p = c(SIN[4]);
    for &k in SIN[..4].iter().rev() {
        p = p.mul_add(x2, c(k));
    }
    (p * x) * sign(wrapped)
}

/// cos 2πt, for `t` in cycles, within 2 × 10⁻⁷.
#[inline(always)]
pub(crate) fn cos_cycles<L: Lane>(t: L) -> L {
    sin_cycles(t + L::splat(0.25))
}

/// The odd Taylor polynomial for atan x on |x| ≤ tan(π/8), to x¹⁵.
const ATAN: [f32; 8] = [
    1.0,
    -1.0 / 3.0,
    1.0 / 5.0,
    -1.0 / 7.0,
    1.0 / 9.0,
    -1.0 / 11.0,
    1.0 / 13.0,
    -1.0 / 15.0,
];

/// atan x for any `x` within ±10¹⁸, within 5 × 10⁻⁷: two half-angle steps
/// bring |x| below tan(π/8), then a polynomial.
#[inline(always)]
pub(crate) fn atan<L: Lane>(x: L) -> L {
    let one = L::splat(1.0);
    let half = |x: L| x / (one + x.mul_add(x, one).sqrt());
    let x = half(half(x));
    let x2 = x * x;
    let mut p = L::splat(ATAN[7]);
    for &k in ATAN[..7].iter().rev() {
        p = p.mul_add(x2, L::splat(k));
    }
    p * x * L::splat(4.0)
}

/// tanh x, within 3 × 10⁻⁷: 1 − 2 / (1 + e^2x), with x clamped to ±9.
#[inline(always)]
pub(crate) fn tanh<L: Lane>(x: L) -> L {
    let x = clamp(x, -9.0, 9.0);
    let e = (x * L::splat(2.0 * std::f32::consts::LOG2_E)).exp2();
    L::splat(1.0) - L::splat(2.0) / (e + L::splat(1.0))
}

/// e^x, within the error of [`Lane::exp2`].
#[inline(always)]
pub(crate) fn exp<L: Lane>(x: L) -> L {
    (x * L::splat(std::f32::consts::LOG2_E)).exp2()
}

/// ln x for positive normal `x`, within 2 × 10⁻⁷ absolute for x near 1
/// and 3 × 10⁻⁸ relative elsewhere: the exponent times ln 2, plus
/// 2 atanh((m − 1) / (m + 1)) of the mantissa as an odd series.
#[inline(always)]
pub(crate) fn ln<L: Lane>(x: L) -> L {
    let (exponent, mantissa) = x.split();
    let one = L::splat(1.0);
    let t = (mantissa - one) / (mantissa + one);
    let t2 = t * t;
    let mut p = L::splat(1.0 / 13.0);
    for k in [11.0f32, 9.0, 7.0, 5.0, 3.0, 1.0] {
        p = p.mul_add(t2, L::splat(1.0 / k));
    }
    exponent.mul_add(L::splat(std::f32::consts::LN_2), p * t * L::splat(2.0))
}

/// ln cosh x, without overflow: |x| + ln(1 + e^−2|x|) − ln 2.
#[inline(always)]
pub(crate) fn ln_cosh<L: Lane>(x: L) -> L {
    let a = x.abs();
    a + ln(L::splat(1.0) + exp(a * L::splat(-2.0))) - L::splat(std::f32::consts::LN_2)
}

/// tan x for 0 ≤ x ≤ 1.45, within 2 × 10⁻⁶ relative: sin over cos.
#[inline(always)]
pub(crate) fn tan<L: Lane>(x: L) -> L {
    let t = x * L::splat(1.0 / std::f32::consts::TAU);
    sin_cycles(t) / cos_cycles(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lspk_dsp::simd::{Kernel, Level, dispatch_at};

    #[derive(Clone, Copy, Debug)]
    enum Func {
        Sin,
        Cos,
        Atan,
        Tanh,
        Tan,
        Ln,
        LnCosh,
    }

    fn apply<L: Lane>(f: Func, x: L) -> L {
        match f {
            Func::Sin => sin_cycles(x),
            Func::Cos => cos_cycles(x),
            Func::Atan => atan(x),
            Func::Tanh => tanh(x),
            Func::Tan => tan(x),
            Func::Ln => ln(x),
            Func::LnCosh => ln_cosh(x),
        }
    }

    struct Eval<'a>(Func, &'a [f32], &'a mut [f32]);

    impl Kernel for Eval<'_> {
        type Output = ();
        #[inline(always)]
        fn run<S: F32s>(self) {
            for (x, y) in self
                .1
                .chunks_exact(S::LANES)
                .zip(self.2.chunks_exact_mut(S::LANES))
            {
                apply(self.0, V(S::load(x))).0.store(y);
            }
        }
    }

    /// The worst error of `f` against `want` over `xs`, on every path the
    /// machine has and on one lane, relative to `scale(want)`.
    fn worst(f: Func, xs: &[f32], want: fn(f64) -> f64, scale: fn(f64) -> f64) -> f64 {
        let n = xs.len() / 16 * 16;
        let mut worst = 0.0f64;
        let mut check = |ys: &[f32]| {
            for (&x, &y) in xs[..n].iter().zip(ys) {
                let w = want(f64::from(x));
                worst = worst.max((f64::from(y) - w).abs() / scale(w));
            }
        };
        for level in Level::ALL {
            let mut ys = vec![0.0; n];
            if dispatch_at(level, Eval(f, &xs[..n], &mut ys)).is_ok() {
                check(&ys);
            }
        }
        let ys: Vec<f32> = xs[..n].iter().map(|&x| apply(f, x)).collect();
        check(&ys);
        worst
    }

    fn range(lo: f32, hi: f32) -> Vec<f32> {
        (0..=200_000)
            .map(|i| lo + (hi - lo) * i as f32 / 200_000.0)
            .collect()
    }

    #[test]
    fn the_fast_maths_stay_within_their_bounds_on_every_path() {
        use std::f64::consts::TAU;
        let absolute = |_: f64| 1.0;
        let sin = worst(Func::Sin, &range(-3.0, 3.0), |t| (TAU * t).sin(), absolute);
        let cos = worst(Func::Cos, &range(-3.0, 3.0), |t| (TAU * t).cos(), absolute);
        let atan = worst(Func::Atan, &range(-40.0, 40.0), f64::atan, absolute);
        let tanh = worst(Func::Tanh, &range(-12.0, 12.0), f64::tanh, absolute);
        let tan = worst(Func::Tan, &range(0.0, 1.45), f64::tan, f64::abs);
        let ln = worst(Func::Ln, &range(1e-3, 100.0), f64::ln, |w| w.abs().max(1.0));
        let ln_cosh = worst(
            Func::LnCosh,
            &range(-40.0, 40.0),
            |x| x.cosh().ln(),
            |w| w.abs().max(1.0),
        );
        println!("ln {ln:e}, ln cosh {ln_cosh:e}");
        assert!(
            ln < 3e-7 && ln_cosh < 3e-7,
            "ln {ln:e}, ln cosh {ln_cosh:e}"
        );
        println!("sin {sin:e}, cos {cos:e}, atan {atan:e}, tanh {tanh:e}, tan {tan:e} relative");
        assert!(sin < 2e-7 && cos < 2e-7, "sin {sin:e}, cos {cos:e}");
        assert!(atan < 5e-7, "atan {atan:e}");
        assert!(tanh < 3e-7, "tanh {tanh:e}");
        assert!(tan < 2e-6, "tan {tan:e}");
    }

    #[test]
    fn phase_words_round_trip() {
        for x in [-0.75f32, -0.5, -0.25, 0.0, 0.1, 0.25, 0.499, 0.75, 3.3] {
            let word = <f32 as Lane>::to_word(x);
            let back = <f32 as Lane>::signed(word);
            let want = x - (x + 0.5).floor();
            assert!(
                (back - want).abs() < 1e-6 || (back + want).abs() < 1e-6,
                "{x}: {back}"
            );
        }
    }
}
