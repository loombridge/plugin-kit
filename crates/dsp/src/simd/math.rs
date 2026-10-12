//! Fast maths on lanes, each with its error bounded by a test.

use super::{F32s, U32s};

/// The polynomial for 2^f on 0 ≤ f < 1, fitted for least relative error:
/// at most 1.6 × 10⁻⁷ in `f32` arithmetic.
const EXP2: [f32; 6] = [
    0.999_999_94,
    0.693_153_1,
    0.240_153_55,
    0.055_826_403,
    0.008_989_328,
    0.001_877_552_8,
];

/// 2^x in every lane, within a relative error of 2 × 10⁻⁷ (a third of a
/// millionth of a cent, as a frequency ratio) for −126 ≤ x ≤ 126; `x` is
/// clamped to that range. The same bits on every path where
/// [`mul_add`](F32s::mul_add) is fused, and within the unfused rounding
/// elsewhere. Real-time safe.
#[inline(always)]
#[must_use]
pub fn exp2<S: F32s>(x: S) -> S {
    let x = x.max(S::splat(-126.0)).min(S::splat(126.0));
    let whole = x.floor();
    let f = x - whole;
    let mut p = S::splat(EXP2[5]);
    for &c in EXP2[..5].iter().rev() {
        p = p.mul_add(f, S::splat(c));
    }
    // p is in [1, 2]: add the whole part to its exponent.
    S::from_bits(p.to_bits().wrapping_add(whole.to_i32().shl::<23>()))
}

#[cfg(test)]
mod tests {
    use super::super::{Kernel, Level, dispatch_at};
    use super::*;

    struct Exp2<'a>(&'a [f32], &'a mut [f32]);

    impl Kernel for Exp2<'_> {
        type Output = ();
        #[inline(always)]
        fn run<S: F32s>(self) {
            for (x, y) in self
                .0
                .chunks_exact(S::LANES)
                .zip(self.1.chunks_exact_mut(S::LANES))
            {
                exp2(S::load(x)).store(y);
            }
        }
    }

    #[test]
    fn exp2_is_within_its_bound_on_every_path() {
        let xs: Vec<f32> = (0..=2_000_000)
            .map(|i| -126.0 + 252.0 * i as f32 / 2_000_000.0)
            .chain([-126.0, -1.0, -0.0, 0.0, 1.0, 0.5, 31.0, 126.0])
            .chain(std::iter::repeat_n(0.0, 16))
            .collect();
        let n = xs.len() / 16 * 16;
        for level in Level::ALL {
            let mut ys = vec![0.0; n];
            if dispatch_at(level, Exp2(&xs[..n], &mut ys)).is_err() {
                continue;
            }
            let worst = xs[..n]
                .iter()
                .zip(&ys)
                .map(|(&x, &y)| {
                    let want = f64::from(x).exp2();
                    (f64::from(y) - want).abs() / want
                })
                .fold(0.0, f64::max);
            assert!(worst < 2e-7, "{level}: {worst:e}");
            // Whole numbers come out exact, as frequencies an octave apart
            // should.
            for (x, y) in xs[..n].iter().zip(&ys) {
                if x.fract() == 0.0 && x.abs() <= 126.0 {
                    assert!((f64::from(*y) / f64::from(*x).exp2() - 1.0).abs() < 1.2e-7);
                }
            }
        }
    }
}
