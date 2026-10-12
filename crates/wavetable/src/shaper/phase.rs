//! The phase stage: the shaper modes that bend where the table is read,
//! each a map from the running phase to the read phase, with the reads it
//! sums where the map has an edge.
//!
//! # Edges without aliasing
//!
//! A map that jumps (sync's reset, a quantise step) or turns a corner (a
//! pulse's end, mirror's turn) makes the wave jump or turn a corner too,
//! and that edge's harmonics run on far above Nyquist. At each edge the
//! stage reads both branches of the map, each carried on across the edge,
//! and crossfades between them over [`TRANSITION`] samples at the host
//! rate along a curve whose first two derivatives are 0 at both ends. The
//! edge's spectrum then falls away at 18 dB an octave beyond the
//! crossfade's bandwidth, and rendering oversampled puts what remains
//! above the band the decimator keeps. Away from the edges one read is
//! enough; a vector of lanes reads twice only while one of its lanes is
//! inside an edge.
//!
//! The smooth maps (pinch, lean, the sine remap) are periodic and
//! infinitely differentiable, so they need no crossfade: their steepest
//! slope ([`stretch`]) is all the oscillator needs to pick a mipmap level
//! whose harmonics stay below the render rate's Nyquist however fast the
//! map reads.

use super::lanes::{Lane, above, atan, clamp, cos_cycles, pick, sin_cycles, smoothstep};
use super::{
    LEAN_MAX, Mode, PINCH_MAX, PULSE_MAX, QUANTISE_MAX_STEPS, QUANTISE_MIN_STEPS, SYNC_MAX_RATIO,
};
use crate::osc::MAX_READS;

/// How many samples at the host rate a crossfade across an edge lasts,
/// whatever the oversampling: edges sound the same at every quality, and
/// the oversampling sets how far down what folds is.
pub const TRANSITION: f32 = 2.0;

/// Up to [`MAX_READS`] weighted reads, as a slot builds them.
#[derive(Clone, Copy)]
pub(crate) struct ReadList<L: Lane> {
    pub word: [L::Word; MAX_READS],
    pub gain: [L; MAX_READS],
    pub count: usize,
    /// Whether the gains aren't all 1.
    pub weighted: bool,
}

impl<L: Lane> ReadList<L> {
    /// One read at `word`, unweighted.
    #[inline(always)]
    pub fn start(word: L::Word) -> Self {
        Self {
            word: [word; MAX_READS],
            gain: [L::splat(1.0); MAX_READS],
            count: 1,
            weighted: false,
        }
    }

    #[inline(always)]
    fn empty(like: L::Word) -> Self {
        Self {
            count: 0,
            ..Self::start(like)
        }
    }

    #[inline(always)]
    fn push(&mut self, word: L::Word, gain: L) {
        if self.count < MAX_READS {
            self.word[self.count] = word;
            self.gain[self.count] = gain;
            self.count += 1;
        }
    }

    /// Two branches, `a` weighted `wa` and `b` weighted `1 − wa`, each also
    /// weighted `gain`: the heavier first, and the lighter only if some
    /// lane gives it any weight.
    #[inline(always)]
    fn push_pair(&mut self, a: L::Word, wa: L, b: L::Word, gain: L) {
        let one = L::splat(1.0);
        let wb = one - wa;
        let a_first = above(wa - L::splat(0.5));
        let first = L::pick_word(a_first, b, a);
        let second = L::pick_word(a_first, a, b);
        let first_weight = pick(a_first, wb, wa);
        let second_weight = one - first_weight;
        self.weighted = true;
        self.push(first, first_weight * gain);
        if second_weight.any() {
            self.push(second, second_weight * gain);
        }
    }
}

/// What a slot shapes with during a render: its mode, character and, for
/// the remaps, the curve's table and steepest slope.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotView<'a> {
    pub mode: Mode,
    pub character: f32,
    pub curve: &'a [f32],
}

/// The read list after `slot` shapes `list`, at `amount` (0 to 1), with
/// crossfades `half` cycles either side of each edge.
#[inline(always)]
pub(crate) fn apply<L: Lane>(
    slot: &SlotView<'_>,
    amount: L,
    half: L,
    list: &ReadList<L>,
) -> ReadList<L> {
    let mut out = ReadList::empty(list.word[0]);
    out.weighted = list.weighted;
    for i in 0..list.count {
        map(slot, amount, half, list.word[i], list.gain[i], &mut out);
    }
    out
}

/// The running phase `word` read as a fraction of a cycle, 0 to 1.
#[inline(always)]
fn unit<L: Lane>(word: L::Word) -> L {
    let t = L::signed(word);
    t - t.floor()
}

/// `x` wrapped to −½ to ½.
#[inline(always)]
fn wrap<L: Lane>(x: L) -> L {
    x - (x + L::splat(0.5)).floor()
}

/// A box of half-length `half_length` centred on 0, repeating every cycle,
/// its edges smoothed `half` cycles either side, at `v` cycles from its
/// centre (−½ to ½). Shrinks to nothing as the box does, and is 1
/// everywhere when the box fills the cycle.
#[inline(always)]
fn smooth_box<L: Lane>(v: L, half_length: L, half: L) -> L {
    let one = L::splat(1.0);
    let width = L::splat(2.0) * half;
    let step = |x: L| smoothstep((x + half) / width);
    let inside = step(v + half_length) - step(v - half_length) + step(v + half_length - one) + one
        - step(v - half_length + one);
    clamp(inside, 0.0, 1.0)
}

/// The lean map's offset at `x` cycles, for strength `beta` (|β| < 2): a
/// parabolic circle map that fixes the cycle's start and moves every other
/// point the same way (forward for β > 0), infinitely differentiable.
#[inline(always)]
fn lean_offset<L: Lane>(x: L, beta: L) -> L {
    let one = L::splat(1.0);
    let ratio = beta * (one - cos_cycles(x)) / (L::splat(2.0) - beta * sin_cycles(x));
    atan(ratio) * L::splat(std::f32::consts::FRAC_1_PI)
}

/// Shapes one read, `word` weighted `gain`, into `out`.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per mode, each short, kept together to read as a table"
)]
#[inline(always)]
fn map<L: Lane>(slot: &SlotView<'_>, a: L, half: L, word: L::Word, gain: L, out: &mut ReadList<L>) {
    let c = L::splat;
    let one = c(1.0);
    match slot.mode {
        Mode::Sync => {
            // The slave runs `ratio` times faster and restarts with the
            // cycle: after the restart it's at ratio × t, before it at
            // ratio × (t + 1). Softness widens the crossfade at the restart
            // up to the whole cycle.
            let ratio = (a * c(SYNC_MAX_RATIO.log2())).exp2();
            let t = L::signed(word);
            let after = L::to_word(ratio * t);
            let before = L::add_words(after, L::to_word(ratio));
            let half = half
                .max(c(0.5 * slot.character.clamp(0.0, 1.0)))
                .min(c(0.5));
            let weight = smoothstep((t + half) / (c(2.0) * half));
            out.push_pair(after, weight, before, gain);
        }
        Mode::PinchIn | Mode::PinchOut | Mode::Pinch => {
            let strength = pinch_strength(slot.mode, a);
            // A hyperbolic circle map at twice the cycle's rate: points
            // flow towards the quarter points (inward) or away from them.
            let x = unit::<L>(word) * c(2.0);
            let s = sin_cycles(x);
            let k = cos_cycles(x);
            let offset = -atan(strength * s / strength.mul_add(k, one))
                * c(0.5 * std::f32::consts::FRAC_1_PI);
            out.push(L::add_words(word, L::to_word(offset)), gain);
        }
        Mode::LeanRight | Mode::LeanLeft | Mode::Lean => {
            let beta = lean_strength(slot.mode, a);
            let offset = lean_offset(unit::<L>(word), beta);
            out.push(L::add_words(word, L::to_word(offset)), gain);
        }
        Mode::PulseWidth => {
            // The cycle squeezed into its first `length`, then the start
            // held: a box of the squeezed read and the hold.
            let length = one - a * c(PULSE_MAX);
            let centre = length * c(0.5);
            let v = wrap(unit::<L>(word) - centre);
            let squeezed = L::to_word((centre + v) / length);
            let weight = smooth_box(v, centre, half);
            out.push_pair(squeezed, weight, L::word(0), gain);
        }
        Mode::Flip => {
            // Inverted from 1 − a to the end of the cycle.
            let half_length = a * c(0.5);
            let v = wrap(unit::<L>(word) - (one - half_length));
            let inverted = smooth_box(v, half_length, half);
            out.weighted = true;
            out.push(word, gain * (one - c(2.0) * inverted));
        }
        Mode::Mirror => {
            // The first half of the wave, leaned, then the same backwards.
            let beta = -a * c(LEAN_MAX);
            let v = wrap(unit::<L>(word) - c(0.25));
            let forward_at = (c(0.25) + v) * c(2.0);
            let backward_at = c(1.5) - v * c(2.0);
            let forward = (forward_at + lean_offset(forward_at, beta)) * c(0.5);
            let backward = (backward_at + lean_offset(backward_at, beta)) * c(0.5);
            let weight = smooth_box(v, c(0.25), half);
            out.push_pair(L::to_word(forward), weight, L::to_word(backward), gain);
        }
        Mode::Quantise => quantise(a, half, word, gain, out),
        Mode::OddEven => {
            // Odd part (w(φ) − w(φ + ½)) / 2 and even part (w(φ) + w(φ +
            // ½)) / 2, the odd at full level to 50 % and the even from it.
            let odd = (c(2.0) * (one - a)).min(one);
            let even = (c(2.0) * a).min(one);
            out.weighted = true;
            out.push(word, gain * (odd + even) * c(0.5));
            out.push(
                L::add_words(word, L::word(1 << 31)),
                gain * (even - odd) * c(0.5),
            );
        }
        Mode::RemapDrawn => {
            let u = unit::<L>(word);
            let curve = L::lookup(slot.curve, super::CURVE_RESOLUTION, u);
            out.push(L::add_words(word, L::to_word(a * (curve - u))), gain);
        }
        Mode::RemapMirrored => {
            let u = unit::<L>(word);
            let second = above(u - c(0.5));
            let x = pick(second, u * c(2.0), c(2.0) - u * c(2.0));
            let y = remap(slot, a, x) * c(0.5);
            out.push(L::to_word(pick(second, y, one - y)), gain);
        }
        Mode::RemapQuad => {
            let u = unit::<L>(word) * c(4.0);
            let quarter = u.floor();
            let y = (quarter + remap(slot, a, u - quarter)) * c(0.25);
            out.push(L::to_word(y), gain);
        }
        Mode::RemapSine => {
            let offset = -a * sin_cycles(unit::<L>(word)) * c(1.0 / std::f32::consts::TAU);
            out.push(L::add_words(word, L::to_word(offset)), gain);
        }
        _ => out.push(word, gain),
    }
}

/// The drawn curve blended from identity by `a`, at `x` (0 to 1).
#[inline(always)]
fn remap<L: Lane>(slot: &SlotView<'_>, a: L, x: L) -> L {
    let curve = L::lookup(slot.curve, super::CURVE_RESOLUTION, x);
    (curve - x).mul_add(a, x)
}

/// Quantise: `steps` steps a cycle, each holding the wave's value at its
/// start, crossfaded into the next over at most half a step; as the steps
/// grow shorter than a crossfade, both reads move onto the running phase
/// (half a step behind, where a step's centre is), so the steps fade into
/// the plain wave instead of folding.
#[inline(always)]
fn quantise<L: Lane>(a: L, half: L, word: L::Word, gain: L, out: &mut ReadList<L>) {
    let c = L::splat;
    let one = c(1.0);
    let steps = ((one - a) * c((QUANTISE_MAX_STEPS / QUANTISE_MIN_STEPS).log2())).exp2()
        * c(QUANTISE_MIN_STEPS);
    let step = one / steps;
    // `half` is TRANSITION / 2 host samples: a step shorter than
    // TRANSITION samples starts to fade into the plain wave, and one of
    // half that is gone.
    let toward = clamp(half * c(2.0) * steps - one, 0.0, 1.0);
    let half = half.min(step * c(0.499));
    let t = L::signed(word);
    let u = t - t.floor();
    let x = u * steps;
    // The last step: the cycle ends inside it unless `steps` is whole.
    let last = (steps - c(1e-4)).floor();
    let k = clamp((x + c(0.5)).floor(), 1.0, f32::MAX).min(last);
    let grid = (x - k) * step;
    // The nearer edge: a grid line, or the cycle's end.
    let at_end = above(grid.abs() - t.abs());
    let distance = pick(at_end, grid, t);
    let left = pick(at_end, (k - one) * step, last * step - one);
    let right = pick(at_end, k * step, c(0.0));
    let target = pick(at_end, u, t) - step * c(0.5);
    let left = (target - left).mul_add(toward, left);
    let right = (target - right).mul_add(toward, right);
    let weight = smoothstep((distance + half) / (c(2.0) * half));
    out.push_pair(L::to_word(right), weight, L::to_word(left), gain);
}

/// The pinch map's strength, −[`PINCH_MAX`] (out) to +[`PINCH_MAX`] (in).
#[inline(always)]
pub(crate) fn pinch_strength<L: Lane>(mode: Mode, a: L) -> L {
    let c = L::splat;
    match mode {
        Mode::PinchIn => a * c(PINCH_MAX),
        Mode::PinchOut => -a * c(PINCH_MAX),
        _ => (a * c(2.0) - c(1.0)) * c(PINCH_MAX),
    }
}

/// The lean map's strength: negative leans right (the cycle's content
/// later), positive left.
#[inline(always)]
pub(crate) fn lean_strength<L: Lane>(mode: Mode, a: L) -> L {
    let c = L::splat;
    match mode {
        Mode::LeanRight => -a * c(LEAN_MAX),
        Mode::LeanLeft => a * c(LEAN_MAX),
        _ => (c(1.0) - a * c(2.0)) * c(LEAN_MAX),
    }
}

/// The lean map's steepest slope at strength `beta`.
fn lean_steepest(beta: f32) -> f32 {
    let b = beta.abs();
    (b * b + 2.0 + b * (b * b + 4.0).sqrt()) * 0.5
}

/// The steepest slope `mode` reads the table with at `amount` (0 to 1),
/// with the remap curve's steepest slope `curve`: how much faster than
/// the running phase it reads at most. Monotonic in the amount or, for the
/// modes neutral at 50 %, in its distance from 50 %, so the largest over a
/// range of amounts is at one end of it.
#[must_use]
pub(crate) fn stretch(mode: Mode, amount: f32, curve: f32) -> f32 {
    let a = amount.clamp(0.0, 1.0);
    match mode {
        Mode::Sync => SYNC_MAX_RATIO.powf(a),
        Mode::PinchIn | Mode::PinchOut | Mode::Pinch => {
            let s = pinch_strength(mode, a).abs();
            (1.0 + s) / (1.0 - s)
        }
        Mode::LeanRight | Mode::LeanLeft | Mode::Lean => lean_steepest(lean_strength(mode, a)),
        Mode::Mirror => lean_steepest(a * LEAN_MAX),
        Mode::PulseWidth => 1.0 / (1.0 - a * PULSE_MAX),
        Mode::RemapDrawn | Mode::RemapMirrored | Mode::RemapQuad => 1.0 + a * (curve - 1.0),
        Mode::RemapSine => 1.0 + a,
        _ => 1.0,
    }
}
