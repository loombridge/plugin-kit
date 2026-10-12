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

/// Integer test data: `n` values across the whole `u32` range, with the
/// edges mixed in.
fn words(seed: u64, n: usize) -> Vec<u32> {
    let mut state = seed;
    (0..n)
        .map(|i| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            match i % 61 {
                7 => 0,
                19 => u32::MAX,
                29 => 1 << 31,
                _ => (state >> 32) as u32,
            }
        })
        .collect()
}

/// What [`IntOps`] makes: each operation's result for every lane.
#[derive(Debug, Default)]
struct IntResults {
    words: Vec<(&'static str, Vec<u32>)>,
    floats: Vec<(&'static str, Vec<f32>)>,
}

/// Runs every integer lane operation, the conversions and a gather.
struct IntOps<'a> {
    a: &'a [u32],
    b: &'a [u32],
    x: &'a [f32],
    table: &'a [f32],
}

impl Kernel for IntOps<'_> {
    type Output = IntResults;

    #[inline(always)]
    fn run<S: F32s>(self) -> IntResults {
        type U<S> = <S as F32s>::U32s;
        type Op<S> = (&'static str, fn(U<S>, U<S>) -> U<S>);
        let ops: [Op<S>; 9] = [
            ("add", U32s::wrapping_add),
            ("sub", U32s::wrapping_sub),
            ("mul", U32s::wrapping_mul),
            ("mul_high", U32s::mul_high),
            ("shl", |a, _| a.shl::<23>()),
            ("shr", |a, _| a.shr::<8>()),
            ("min", U32s::min),
            ("max", U32s::max),
            ("to_i32", |a, _| S::from_bits(a).to_i32()),
        ];
        let n = self.a.len();
        let mut results = IntResults::default();
        for (name, op) in ops {
            let mut out = vec![0; n];
            for at in (0..n).step_by(S::LANES) {
                op(U::<S>::load(&self.a[at..]), U::<S>::load(&self.b[at..])).store(&mut out[at..]);
            }
            results.words.push((name, out));
        }
        let mut bits = vec![0; n];
        let mut floor = vec![0.0; n];
        let mut from_i32 = vec![0.0; n];
        let mut gathered = vec![0.0; 3 * n];
        for at in (0..n).step_by(S::LANES) {
            let x = S::load(&self.x[at..]);
            x.to_bits().store(&mut bits[at..]);
            x.floor().store(&mut floor[at..]);
            S::from_i32(U::<S>::load(&self.a[at..])).store(&mut from_i32[at..]);
            let rows = S::gather::<3>(self.table, U::<S>::load(&self.b[at..]).shr::<20>());
            for (j, row) in rows.iter().enumerate() {
                row.store(&mut gathered[j * n + at..]);
            }
        }
        results.words.push(("to_bits", bits));
        results.floats.push(("floor", floor));
        results.floats.push(("from_i32", from_i32));
        results.floats.push(("gather", gathered));
        results
    }
}

#[test]
fn every_path_matches_the_portable_one_for_integer_lanes() {
    let n = 1_024;
    let (a, b) = (words(6, n), words(7, n));
    // Floats in i32's range for the conversions, and a table smaller than
    // the indices reach, so the gather clamps.
    let x = data(8, n);
    let table: Vec<f32> = (0..3_000).map(|i| i as f32 * 0.5).collect();
    let ops = || IntOps {
        a: &a,
        b: &b,
        x: &x,
        table: &table,
    };
    let reference = dispatch_at(Level::Portable, ops()).unwrap_or_else(|_| unreachable!());

    // The portable path against plain Rust.
    let words = |name| &reference.words.iter().find(|(n, _)| *n == name).unwrap().1;
    for i in 0..n {
        assert_eq!(words("add")[i], a[i].wrapping_add(b[i]));
        assert_eq!(words("mul")[i], a[i].wrapping_mul(b[i]));
        assert_eq!(
            words("mul_high")[i],
            ((u64::from(a[i]) * u64::from(b[i])) >> 32) as u32
        );
        assert_eq!(words("shr")[i], a[i] >> 8);
        assert_eq!(words("to_bits")[i], x[i].to_bits());
    }
    let gathered = &reference.floats[2].1;
    for i in 0..n {
        let at = ((b[i] >> 20) as usize).min(table.len() - 3);
        for j in 0..3 {
            assert_eq!(
                gathered[j * n + i],
                table[at + j],
                "gather lane {i} row {j}"
            );
        }
    }

    for level in Level::ALL {
        let Ok(results) = dispatch_at(level, ops()) else {
            continue;
        };
        for ((name, got), (_, want)) in results.words.iter().zip(&reference.words) {
            for (i, (got, want)) in got.iter().zip(want).enumerate() {
                // Out-of-range conversions are unspecified.
                if *name == "to_i32" && f32::from_bits(a[i]).abs() >= 2.0e9 {
                    continue;
                }
                if *name == "to_i32" && f32::from_bits(a[i]).is_nan() {
                    continue;
                }
                assert!(got == want, "{level} {name} at {i}: {got} against {want}");
            }
        }
        for ((name, got), (_, want)) in results.floats.iter().zip(&reference.floats) {
            for (i, (got, want)) in got.iter().zip(want).enumerate() {
                assert!(
                    got.to_bits() == want.to_bits() || got.is_nan() && want.is_nan(),
                    "{level} {name} at {i}: {got} against {want}"
                );
            }
        }
    }
}
