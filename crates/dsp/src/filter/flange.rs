//! The Flanges group: combs, flangers, phasers and the flanger-phaser,
//! each with positive or negative feedback and an optional filter in the
//! feedback path.

use super::lane::Lane;
use super::lines::Lines;
use super::{COMB_FEEDBACK_MAX, Io, PHASER_FEEDBACK_MAX, Topology, g_at, limit, one_pole};

/// What sits in the feedback path.
pub(super) const NONE: u8 = 0;
pub(super) const LP: u8 = 1;
pub(super) const HP: u8 = 2;
pub(super) const BAND: u8 = 3;

/// How far from the comb's frequency the feedback filters start, at
/// character 0, in octaves: far enough to be open.
pub(super) const FEEDBACK_FILTER_OCTAVES: f32 = 10.0;

/// The feedback path's filter: a one-pole low-pass falling from
/// [`FEEDBACK_FILTER_OCTAVES`] above the comb's frequency to it as
/// character rises, a one-pole high-pass rising from as far below, or
/// both. `lp` and `hp` are their states.
#[inline(always)]
pub(super) fn feedback_filter<const FB: u8, L: Lane>(
    lp: &mut L,
    hp: &mut L,
    x: L,
    ratio: L,
    character: L,
) -> L {
    let one = L::splat(1.0);
    let away = (one - character) * L::splat(FEEDBACK_FILTER_OCTAVES);
    let mut y = x;
    if FB == LP || FB == BAND {
        let g = g_at(ratio, away);
        y = one_pole(lp, y, g / (one + g));
    }
    if FB == HP || FB == BAND {
        let g = g_at(ratio, -away);
        y = y - one_pole(hp, y, g / (one + g));
    }
    y
}

/// The feedback gain at resonance `r`.
#[inline(always)]
fn gain<L: Lane>(r: L, top: f32, negative: bool) -> L {
    r * L::splat(if negative { -top } else { top })
}

/// A comb (one period of the cutoff) or flanger (half a period): a delay
/// line fed by the input and its own output through the feedback filter.
/// The wet signal is the line's output. State: 0 and 1 the feedback
/// filter.
pub(super) struct Comb<const FLANGER: bool, const FB: u8, const NEG: bool>;

impl<const FLANGER: bool, const FB: u8, const NEG: bool> Topology<6> for Comb<FLANGER, FB, NEG> {
    const CHARACTER: bool = FB != NONE;
    const LINES: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, lines: &mut Lines<'_>) -> L {
        let periods = if FLANGER { 0.5 } else { 1.0 };
        let delayed = lines.read(0, 1, io, |ratio, _| periods / ratio);
        let [lp, hp, ..] = state;
        let fed = feedback_filter::<FB, L>(lp, hp, delayed, io.ratio, io.character);
        let fed = limit(fed, io.inv_knee);
        lines.write(0, 1, gain(io.r, COMB_FEEDBACK_MAX, NEG).fma(fed, io.x));
        delayed
    }
}

/// `STAGES` first-order all-pass stages at the cutoff, run in place on
/// `v` with `stages` their states.
#[inline(always)]
fn all_passes<L: Lane>(stages: &mut [L], mut v: L, g: L) -> L {
    let gg = g / (L::splat(1.0) + g);
    for s in stages {
        let low = one_pole(s, v, gg);
        v = low + low - v;
    }
    v
}

/// A phaser: `STAGES` first-order all-pass stages at the cutoff, fed by
/// the input and their own output, a sample late, through the feedback
/// filter. The wet signal is the chain's output. State: 0 to `STAGES − 1`
/// the stages, then the feedback filter's two and the previous output.
pub(super) struct Phaser<const STAGES: usize, const FB: u8, const NEG: bool>;

impl<const STAGES: usize, const FB: u8, const NEG: bool, const N: usize> Topology<N>
    for Phaser<STAGES, FB, NEG>
{
    const CHARACTER: bool = FB != NONE;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; N], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let (stages, rest) = state.split_at_mut(STAGES);
        let [lp, hp, previous, ..] = rest else {
            unreachable!("a phaser's state holds its stages and three more")
        };
        let fed = feedback_filter::<FB, L>(lp, hp, *previous, io.ratio, io.character);
        let fed = limit(fed, io.inv_knee);
        let v = gain(io.r, PHASER_FEEDBACK_MAX, NEG).fma(fed, io.x);
        let v = all_passes(stages, v, io.g);
        *previous = v;
        v
    }
}

/// The flanger-phaser: 12 all-pass stages at the cutoff whose feedback
/// runs through the flanger's delay line (half a period of the cutoff) and
/// the band filter. State: 0 to 11 the stages, 12 and 13 the band.
pub(super) struct FlangePhaser<const NEG: bool>;

/// The flanger-phaser's stage count.
const FLANGE_PHASER_STAGES: usize = 12;

impl<const NEG: bool, const N: usize> Topology<N> for FlangePhaser<NEG> {
    const CHARACTER: bool = true;
    const LINES: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; N], io: &Io<L>, lines: &mut Lines<'_>) -> L {
        let delayed = lines.read(0, 1, io, |ratio, _| 0.5 / ratio);
        let (stages, rest) = state.split_at_mut(FLANGE_PHASER_STAGES);
        let [lp, hp, ..] = rest else {
            unreachable!("the flanger-phaser's state holds its stages and two more")
        };
        let fed = feedback_filter::<BAND, L>(lp, hp, delayed, io.ratio, io.character);
        let fed = limit(fed, io.inv_knee);
        let v = gain(io.r, PHASER_FEEDBACK_MAX, NEG).fma(fed, io.x);
        let v = all_passes(stages, v, io.g);
        lines.write(0, 1, v);
        v
    }
}
