//! The Multi group: dual types, two stages in series with the second's
//! cutoff set by character, and morphing types, which blend three
//! responses of one stage as character goes from 0 to 1.

use super::lane::Lane;
use super::lines::Lines;
use super::{
    BAND, HIGH, Io, K_BAND, K_BUTTERWORTH_2, K_BUTTERWORTH_4, K_PEAK_MIN, LOW, NOTCH, PEAK,
    Topology, g_at, one_pole, saturated_k, svf, svf_k, svf12,
};

/// How far the dual types' second cutoff moves across the character
/// range, in octaves: from half this below the cutoff to half above.
pub(super) const DUAL_SPAN_OCTAVES: f32 = 8.0;

/// The dual types' second cutoff as octaves from the first.
#[inline(always)]
fn second<L: Lane>(character: L) -> L {
    character.fma(
        L::splat(DUAL_SPAN_OCTAVES),
        L::splat(-0.5 * DUAL_SPAN_OCTAVES),
    )
}

/// Two 12 dB state-variable stages in series, taking outputs `A` and `B`,
/// the second at the character's cutoff; each has the resonance law and
/// saturation of its 12 dB type, at one resonance. State: 0 to 2 the first
/// stage, 3 to 5 the second.
pub(super) struct Dual<const A: u8, const B: u8>;

impl<const A: u8, const B: u8> Topology<6> for Dual<A, B> {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let g2 = g_at(io.ratio, second(io.character));
        let [s0, s1, s2, s3, s4, s5] = state;
        let first = svf12::<A, L>(s0, s1, s2, io.x, io.g, io.r, io.inv_knee);
        svf12::<B, L>(s3, s4, s5, first, g2, io.r, io.inv_knee)
    }
}

/// The 6 dB dual: a one-pole low-pass at the cutoff, then a one-pole
/// high-pass at the character's cutoff. State: 0 and 1.
pub(super) struct DualOnePole;

impl Topology<6> for DualOnePole {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let one = L::splat(1.0);
        let low = one_pole(&mut state[0], io.x, io.g / (one + io.g));
        let g2 = g_at(io.ratio, second(io.character));
        low - one_pole(&mut state[1], low, g2 / (one + g2))
    }
}

/// The morphing sets: their three responses, in order.
pub(super) const LBH: u8 = 0;
pub(super) const LPH: u8 = 1;
pub(super) const LNH: u8 = 2;
pub(super) const BPN: u8 = 3;

/// A response of one state-variable stage, as coefficients of the input,
/// the low output and the band output, given the stage's damping `k` and
/// its resonance-0 damping `k0`.
#[inline(always)]
fn response<L: Lane>(kind: u8, k: L, k0: f32) -> [L; 3] {
    let (zero, one) = (L::splat(0.0), L::splat(1.0));
    match kind {
        LOW => [zero, one, zero],
        HIGH => [one, -one, -k],
        BAND => [zero, zero, L::splat(k0)],
        NOTCH => [one, zero, -k],
        _ => [one, zero, one - k * L::splat(1.0 / k0)],
    }
}

/// The three responses a set blends.
const fn responses(set: u8) -> [u8; 3] {
    match set {
        LBH => [LOW, BAND, HIGH],
        LPH => [LOW, PEAK, HIGH],
        LNH => [LOW, NOTCH, HIGH],
        _ => [BAND, PEAK, NOTCH],
    }
}

/// One morphing stage: the stage run, its responses blended by `weights`;
/// its output and band output.
#[inline(always)]
fn morph_stage<const SET: u8, L: Lane>(
    s0: &mut L,
    s1: &mut L,
    x: L,
    g: L,
    k: L,
    k0: f32,
    weights: [L; 3],
) -> (L, L) {
    let (band, low) = svf(s0, s1, x, g, k);
    let mut c = [L::splat(0.0); 3];
    for (w, kind) in weights.into_iter().zip(responses(SET)) {
        for (c, r) in c.iter_mut().zip(response(kind, k, k0)) {
            *c = w.fma(r, *c);
        }
    }
    (c[0].fma(x, c[1].fma(low, c[2] * band)), band)
}

/// Morphing types: the first response at character 0, the second at 0.5,
/// the third at 1, blended linearly between. The 24 dB forms run two
/// morphing stages in series, the first at a fixed damping, as the 24 dB
/// Normal types do. State: 0 and 1 the resonant stage, 2 its previous band
/// output, 3 and 4 the first stage of a 24 dB form.
pub(super) struct Morph<const SET: u8, const TWO: bool>;

impl<const SET: u8, const TWO: bool> Topology<6> for Morph<SET, TWO> {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let (zero, one) = (L::splat(0.0), L::splat(1.0));
        let twice = io.character + io.character;
        let a = twice.min_(one);
        let b = (twice - one).max_(zero);
        let weights = [one - a, a - b, b];
        // Dampings: the resonant stage's at resonance 0 and its floor, and
        // a 24 dB form's fixed first stage's.
        let (k0, floor, first) = match (SET, TWO) {
            (BPN, _) => (K_BAND, K_PEAK_MIN, K_BAND),
            (_, false) => (K_BUTTERWORTH_2, f32::NEG_INFINITY, 0.0),
            (_, true) => (K_BUTTERWORTH_4[1], f32::NEG_INFINITY, K_BUTTERWORTH_4[0]),
        };
        let k = saturated_k(k0, svf_k(k0, floor, io.r), state[2], io.inv_knee);
        let [s0, s1, s2, s3, s4, _] = state;
        let x = if TWO {
            morph_stage::<SET, L>(s3, s4, io.x, io.g, L::splat(first), first, weights).0
        } else {
            io.x
        };
        let (y, band) = morph_stage::<SET, L>(s0, s1, x, io.g, k, k0, weights);
        *s2 = band;
        y
    }
}
