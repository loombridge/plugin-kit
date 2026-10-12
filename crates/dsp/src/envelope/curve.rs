//! The curve family that bends the attack, decay and release stages, and the
//! segments a stage is drawn with.
//!
//! A stage moves from one level to another over `n` samples. Its progress
//! `s(x)` at the fraction `x` of its time is
//!
//! ```text
//! s(x) = (1 − e^(−k·x)) / (1 − e^(−k))      (s(x) = x when k = 0)
//! ```
//!
//! where the bend `k` comes from the stage's curve control `c` (0 to 1):
//! `k = 12 × (2c − 1)`. At 50 % the stage is a straight line. Above 50 % it
//! moves quickly at first and slows into its target, as a capacitor charges
//! through a resistor; below 50 % it starts slowly and speeds up. Curves
//! either side of 50 % mirror each other: `s_c(x) = 1 − s_(1−c)(1 − x)`. At
//! the default 66.6 % a stage covers about 81 % of its distance in the first
//! 40 % of its time; at 100 %, about 99 %.
//!
//! This is a first family, smooth and cheap; it will be matched against
//! measurements later, which may change the mapping from `c` to `k` but not
//! the shape of the code.
//!
//! # Drawing it sample by sample
//!
//! Every stage is drawn with the same two steps per sample, so every lane
//! of a SIMD bank runs the same instructions whatever its stage:
//!
//! ```text
//! y = u × scale + base
//! u = u × r + d
//! ```
//!
//! `u` starts at 0 and `base` is the level at the start. A straight stage
//! counts `u` up by one (`r = 1`, `d = 1`), so `u` is a whole number and
//! exact. A bent one multiplies `1 + u` by `r = e^(−k/n)` each sample
//! (`d = r − 1`, exact in `f32`), so `u` holds only the distance moved and
//! rounding stays relative to it. A level that doesn't move has
//! `scale = 0`.
//!
//! A long stage is drawn as segments of at most [`MAX_SEGMENT`] samples.
//! Each segment's two ends come from the closed form in `f64`, and its
//! `scale` is fitted to the `r` that `f32` actually holds, so every segment
//! lands exactly on the curve and rounding can't build up across a stage
//! however long it lasts. A segment that bends by less than 10⁻³ is drawn
//! as the straight line between its ends, within 1.25 × 10⁻⁴ of its
//! distance of the curve.

use super::lane::Segment;

/// The bend at a curve of 0 or 100 %.
pub const MAX_BEND: f64 = 12.0;

/// A segment bending less than this is drawn as a straight line.
const CHORD_BELOW: f64 = 1e-3;

/// The longest segment, in samples: 16384, 341 ms at 48 kHz.
pub(super) const MAX_SEGMENT: u32 = 1 << 14;

/// The bend `k` for a curve control of `curve` (0 to 1, 0.5 straight).
#[must_use]
pub fn bend(curve: f32) -> f64 {
    MAX_BEND * (2.0 * f64::from(super::unit(curve)) - 1.0)
}

/// How far a stage with curve control `curve` has moved at `x` (0 to 1) of
/// its time, from 0 at the start to 1 at the end: the shape an editor
/// draws.
#[must_use]
pub fn shape(curve: f32, x: f64) -> f64 {
    progress(bend(curve), x.clamp(0.0, 1.0))
}

/// `s(x)` for bend `k`.
pub(super) fn progress(k: f64, x: f64) -> f64 {
    if k == 0.0 {
        x
    } else {
        (-k * x).exp_m1() / (-k).exp_m1()
    }
}

/// The `x` at which `s(x) = v`, for `v` from 0 to 1.
pub(super) fn time_at(k: f64, v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if k == 0.0 {
        v
    } else {
        // 1 − e^(−kx) = v (1 − e^(−k))
        (-(v * (-k).exp_m1()).ln_1p() / k).clamp(0.0, 1.0)
    }
}

/// The segment that draws `len` samples of a stage from `from` to `to` over
/// `total` samples with bend `k`, starting `pos` samples in.
pub(super) fn segment(from: f32, to: f32, k: f64, total: u64, pos: u64, len: u32) -> Segment {
    let (from, span) = (f64::from(from), f64::from(to) - f64::from(from));
    if span == 0.0 {
        return Segment::constant(from as f32, len);
    }
    // Sample counts are far below 2^53, so these are exact.
    let n = total as f64;
    let at = |p: u64| from + span * progress(k, p as f64 / n);
    let (start, end) = (at(pos), at(pos + u64::from(len)));
    let r = (-k / n).exp() as f32;
    if (k * f64::from(len) / n).abs() < CHORD_BELOW || r == 1.0 {
        return Segment {
            u: 0.0,
            r: 1.0,
            d: 1.0,
            scale: ((end - start) / f64::from(len)) as f32,
            base: start as f32,
            left: len,
        };
    }
    // 1 + u = r^j, so u reaches r^len − 1 at the segment's end. `len` is at
    // most `MAX_SEGMENT`, so it fits an `i32`.
    let moved = f64::from(r).powi(len as i32) - 1.0;
    Segment {
        u: 0.0,
        r,
        d: r - 1.0,
        scale: ((end - start) / moved) as f32,
        base: start as f32,
        left: len,
    }
}
