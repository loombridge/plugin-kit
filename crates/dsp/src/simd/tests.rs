//! Every path this machine has gives the portable path's results, within
//! the tolerances the module documents.

use super::*;

/// Test data: `n` values in about ±8, from a fixed seed, with a few
/// awkward magnitudes mixed in.
fn data(seed: u64, n: usize) -> Vec<f32> {
    let mut state = seed;
    (0..n)
        .map(|i| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let unit = (state >> 40) as f32 / (1u64 << 24) as f32;
            match i % 97 {
                13 => 1.0e-30,
                41 => -3.0e15,
                _ => unit * 16.0 - 8.0,
            }
        })
        .collect()
}

/// What [`AllOps`] makes: each operation's result for every lane, and each
/// chunk's sum with the exact sum and the magnitude it's held to.
#[derive(Debug, Default)]
struct Results {
    exact: Vec<(&'static str, Vec<f32>)>,
    mul_add: Vec<f32>,
    sums: Vec<f32>,
    lanes: usize,
}

/// Runs every lane operation over `a`, `b` and `c`.
struct AllOps<'a> {
    a: &'a [f32],
    b: &'a [f32],
    c: &'a [f32],
}

impl Kernel for AllOps<'_> {
    type Output = Results;

    #[inline(always)]
    fn run<S: F32s>(self) -> Results {
        type Op<S> = (&'static str, fn(S, S) -> S);
        let ops: [Op<S>; 9] = [
            ("add", |a, b| a + b),
            ("sub", |a, b| a - b),
            ("mul", |a, b| a * b),
            ("div", |a, b| a / b),
            ("neg", |a, _| -a),
            ("abs", |a, _| a.abs()),
            ("min", F32s::min),
            ("max", F32s::max),
            ("sqrt", |a, _| a.abs().sqrt()),
        ];
        let n = self.a.len();
        let mut results = Results {
            lanes: S::LANES,
            ..Results::default()
        };
        for (name, op) in ops {
            let mut out = vec![0.0; n];
            for at in (0..n).step_by(S::LANES) {
                let mut x = S::load(&self.a[at..]);
                // The assigning operators, through their own impls.
                x += S::splat(0.0);
                x -= S::splat(0.0);
                x *= S::splat(1.0);
                op(x, S::load(&self.b[at..])).store(&mut out[at..]);
            }
            results.exact.push((name, out));
        }
        results.mul_add = vec![0.0; n];
        for at in (0..n).step_by(S::LANES) {
            let (a, b, c) = (
                S::load(&self.a[at..]),
                S::load(&self.b[at..]),
                S::load(&self.c[at..]),
            );
            a.mul_add(b, c).store(&mut results.mul_add[at..]);
            results.sums.push(a.sum());
        }
        results
    }
}

fn close(got: f32, want: f64, tolerance: f64) -> bool {
    (f64::from(got) - want).abs() <= tolerance
}

#[test]
fn every_path_matches_the_portable_one() {
    let n = 1_024;
    let (a, b, c) = (data(1, n), data(2, n), data(3, n));
    let ops = || AllOps {
        a: &a,
        b: &b,
        c: &c,
    };
    let reference = dispatch_at(Level::Portable, ops()).unwrap_or_else(|_| unreachable!());
    let mut ran = Vec::new();
    for level in Level::ALL {
        let Ok(results) = dispatch_at(level, ops()) else {
            continue;
        };
        ran.push(level.name());
        assert_eq!(results.lanes, level.lanes(), "{level}");
        // Correctly rounded operations: the same bits.
        for ((name, got), (_, want)) in results.exact.iter().zip(&reference.exact) {
            for (i, (got, want)) in got.iter().zip(want).enumerate() {
                assert!(got == want, "{level} {name} at {i}: {got} against {want}");
            }
        }
        // mul_add, fused or not.
        for i in 0..n {
            let product = f64::from(a[i]) * f64::from(b[i]);
            let exact = product + f64::from(c[i]);
            let tolerance = 2.0 * f64::from(f32::EPSILON) * (product.abs() + f64::from(c[i]).abs());
            assert!(
                close(results.mul_add[i], exact, tolerance),
                "{level} mul_add at {i}: {} against {exact}",
                results.mul_add[i]
            );
        }
        // Sums, in each path's order.
        for (chunk, got) in a.chunks(level.lanes()).zip(&results.sums) {
            let exact: f64 = chunk.iter().map(|x| f64::from(*x)).sum();
            let magnitude: f64 = chunk.iter().map(|x| f64::from(x.abs())).sum();
            let tolerance = level.lanes() as f64 * f64::from(f32::EPSILON) * magnitude;
            assert!(
                close(*got, exact, tolerance),
                "{level} sum: {got} against {exact}"
            );
        }
    }
    eprintln!("paths checked on this machine: {}", ran.join(", "));
    assert!(ran.contains(&level().name()));
}

#[test]
fn the_helpers_agree_on_every_path() {
    let n = 1_027; // Not a multiple of any lane count, to test the tails.
    let (src, start) = (data(4, n), data(5, n));
    let mut want = start.clone();
    for (w, s) in want.iter_mut().zip(&src) {
        *w += s * 0.5;
    }
    let squares: f64 = src.iter().map(|x| f64::from(*x) * f64::from(*x)).sum();
    for level in Level::ALL.into_iter().filter(|l| l.is_supported()) {
        let mut dst = start.clone();
        let mixed = Mix {
            dst: &mut dst,
            src: &src,
            gain: 0.5,
        };
        assert!(dispatch_at(level, mixed).is_ok());
        for (i, (got, want)) in dst.iter().zip(&want).enumerate() {
            let tolerance = 2.0 * f64::from(f32::EPSILON) * f64::from(want.abs() + src[i].abs());
            assert!(
                close(*got, f64::from(*want), tolerance),
                "{level} mix at {i}"
            );
        }
        let got = dispatch_at(level, SumOfSquares(&src)).unwrap_or(f32::NAN);
        let tolerance = n as f64 * f64::from(f32::EPSILON) * squares;
        assert!(
            close(got, squares, tolerance),
            "{level}: {got} against {squares}"
        );
    }
    let mut dst = start.clone();
    mix(&mut dst, &src, 0.5);
    assert!(close(
        sum_of_squares(&src),
        squares,
        n as f64 * 1e-7 * squares
    ));
}

#[test]
fn levels_are_chosen_from_what_the_cpu_has() {
    assert!(level().is_supported());
    assert_eq!(Level::best(Level::Portable), Level::Portable);
    assert!(Level::best(Level::Avx512) >= level());
    #[cfg(target_arch = "aarch64")]
    assert_eq!(Level::best(Level::Avx512), Level::Neon);
    #[cfg(target_arch = "x86_64")]
    assert!(!Level::Neon.is_supported());
    #[cfg(target_arch = "aarch64")]
    assert!(!Level::Sse42.is_supported());
    for level in Level::ALL {
        assert_eq!(Level::parse(level.name()), Some(level));
        if !level.is_supported() {
            assert!(dispatch_at(level, SumOfSquares(&[1.0])).is_err());
        }
    }
    assert_eq!(Level::parse("mmx"), None);
}
