//! The Misc group: EQ shapes, ring modulators, sample and hold, the
//! multi-comb types and the room, two more low-pass characters, bass add,
//! vowels, band reject, comb-feedback filters and shriek.

use super::flange::FEEDBACK_FILTER_OCTAVES;
use super::lane::Lane;
use super::lines::Lines;
use super::{
    BAND, COMB_FEEDBACK_MAX, Io, K_BAND, K_BUTTERWORTH_2, K_PEAK, K_PEAK_MIN, LOW, MAX_LANES,
    RATIO_LIMIT, Topology, ZERO_DELAY_LADDER_MAX, g_at, ladder, limit, one_pole, ratio_of,
    saturated_k, soft_clip, svf, svf_k, svf12, tan,
};

/// The EQ shapes.
pub(super) const LOW6: u8 = 0;
pub(super) const LOW12: u8 = 1;
pub(super) const BAND12: u8 = 2;
pub(super) const HIGH6: u8 = 3;
pub(super) const HIGH12: u8 = 4;

/// The EQ gain law: (2 × character)⁴, so 0 is silence, 0.5 unity and 1
/// +24 dB.
#[inline(always)]
fn eq_gain<L: Lane>(character: L) -> L {
    let twice = character + character;
    let squared = twice * twice;
    squared * squared
}

/// Low, band and high EQ: the input plus the band's share at the gain law
/// less one, so the band is scaled by the gain. The 12 dB shelves' band is
/// all but the opposite 12 dB pass, so silent they're that pass alone; the
/// band EQ's is the band-pass. The 12 dB shapes use a
/// state-variable stage whose damping falls with resonance to 1/16; the
/// 6 dB shapes a one-pole stage. State: 0 and 1 the stage, 2 its previous
/// band output.
pub(super) struct Eq<const KIND: u8>;

impl<const KIND: u8> Topology<6> for Eq<KIND> {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let one = L::splat(1.0);
        let (x, g) = (io.x, io.g);
        let less_one = eq_gain(io.character) - one;
        let part = if KIND == LOW6 || KIND == HIGH6 {
            let low = one_pole(&mut state[0], x, g / (one + g));
            if KIND == LOW6 { low } else { x - low }
        } else {
            let k0 = if KIND == BAND12 {
                K_PEAK
            } else {
                K_BUTTERWORTH_2
            };
            let k = saturated_k(k0, svf_k(k0, K_PEAK_MIN, io.r), state[2], io.inv_knee);
            let [s0, s1, s2, ..] = state;
            let (band, low) = svf(s0, s1, x, g, k);
            *s2 = band;
            // The 12 dB shelves scale everything but the opposite pass:
            // silent, a low shelf is a 12 dB high-pass.
            match KIND {
                LOW12 => k.fma(band, low),
                HIGH12 => x - low,
                _ => k * band,
            }
        };
        less_one.fma(part, x)
    }
}

/// Turns a sine oscillator, kept as one minus its cosine (`cm`, so a
/// cleared state starts at phase 0) and its sine (`s`), on by the angle
/// whose half has tangent `g`; its new sine. Its amplitude is pulled back
/// to 1 each sample.
#[inline(always)]
fn rotate<L: Lane>(cm: &mut L, s: &mut L, g: L) -> L {
    let one = L::splat(1.0);
    let g2 = g * g;
    let inv = one / (one + g2);
    let cos = (one - g2) * inv;
    let sin = (g + g) * inv;
    let c = one - *cm;
    let c1 = c.fma(cos, -(*s * sin));
    let s1 = s.fma(cos, c * sin);
    let norm = (L::splat(3.0) - c1.fma(c1, s1 * s1)) * L::splat(0.5);
    *cm = one - c1 * norm;
    *s = s1 * norm;
    *s
}

/// A ring modulator: the input times a sine at the cutoff; doubled, times
/// the mean of that and a sine `character` octaves higher. State: 0 and 1
/// the first oscillator, 2 and 3 the second.
pub(super) struct Ring<const DOUBLE: bool>;

impl<const DOUBLE: bool> Topology<6> for Ring<DOUBLE> {
    const CHARACTER: bool = DOUBLE;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let [c0, s0, c1, s1, ..] = state;
        let first = rotate(c0, s0, io.g);
        if DOUBLE {
            let second = rotate(c1, s1, g_at(io.ratio, io.character));
            io.x * (first + second) * L::splat(0.5)
        } else {
            io.x * first
        }
    }
}

/// Sample and hold: the input taken once every period of the cutoff, from
/// the first sample, and held; the difference type outputs the input less
/// the held value. The period is worked out lane by lane from the pitch
/// exactly as the scalar path does, so the two hold at the same samples.
/// State: 0 the cycles left before the next hold, 1 the held value.
pub(super) struct SampleHold<const DIFF: bool>;

impl<const DIFF: bool> Topology<6> for SampleHold<DIFF> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let mut pitch = [0.0f32; MAX_LANES];
        let mut x = [0.0f32; MAX_LANES];
        let mut left = [0.0f32; MAX_LANES];
        let mut held = [0.0f32; MAX_LANES];
        io.pitch.store(&mut pitch);
        io.x.store(&mut x);
        state[0].store(&mut left);
        state[1].store(&mut held);
        for lane in 0..L::LANES {
            if left[lane] <= 0.0 {
                held[lane] = x[lane];
                left[lane] += 1.0;
            }
            left[lane] -= ratio_of(pitch[lane], io.ref_over_rate);
        }
        state[0] = L::load(&left);
        state[1] = L::load(&held);
        if DIFF { io.x - state[1] } else { state[1] }
    }
}

/// The multi-comb types.
pub(super) const COMBS: u8 = 0;
pub(super) const ALLPASSES: u8 = 1;
pub(super) const ROOM: u8 = 2;

/// The four combs' frequencies, as multiples of the cutoff: a stack of
/// minor thirds.
const COMB_RATIOS: [f32; 4] = [1.0, 1.189_207_1, std::f32::consts::SQRT_2, 1.681_792_8];
/// The room's size, as a multiple of the cutoff's period: its longest comb.
const ROOM_SCALE: f32 = 8.0;
/// The room's comb delays, as fractions of its size.
const ROOM_COMBS: [f32; 4] = [1.0, 0.897, 0.811, 0.737];
/// The room's two all-pass delays, as fractions of its size.
const ROOM_ALLPASSES: [f32; 2] = [0.31, 0.11];
/// The room's all-pass gain.
const ROOM_DIFFUSION: f32 = 0.5;

/// Combs, all-passes and the room. Every delay's feedback runs through a
/// one-pole damping low-pass, the comb's feedback low-pass (character from
/// open to the cutoff); resonance sets the feedback, up to 0.98. Combs:
/// four combs in parallel at [`COMB_RATIOS`] of the cutoff, half dry and
/// half their mean whatever the mix. All-passes: four all-pass delays in
/// series at the same delays. Room: four combs in parallel at
/// [`ROOM_COMBS`] of [`ROOM_SCALE`] periods of the cutoff, then two
/// all-passes. State: 0 to 3 the damping filters.
pub(super) struct MultiComb<const KIND: u8>;

impl<const KIND: u8> Topology<6> for MultiComb<KIND> {
    const CHARACTER: bool = true;
    const LINES: bool = true;
    const IGNORES_MIX: bool = KIND == COMBS;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, lines: &mut Lines<'_>) -> L {
        let one = L::splat(1.0);
        let damping = g_at(
            io.ratio,
            (one - io.character) * L::splat(FEEDBACK_FILTER_OCTAVES),
        );
        let gg = damping / (one + damping);
        let feedback = io.r * L::splat(COMB_FEEDBACK_MAX);
        match KIND {
            COMBS => {
                // The longest comb is held to the line, keeping the others'
                // intervals.
                let longest = lines.longest(4);
                let mut sum = L::splat(0.0);
                for (i, ratio) in COMB_RATIOS.into_iter().enumerate() {
                    let delayed = lines.read(i, 4, io, |r, _| (1.0 / r).min(longest) / ratio);
                    let damped = limit(one_pole(&mut state[i], delayed, gg), io.inv_knee);
                    lines.write(i, 4, feedback.fma(damped, io.x));
                    sum = sum + delayed;
                }
                sum.fma(L::splat(0.125), io.x * L::splat(0.5))
            }
            ALLPASSES => {
                let longest = lines.longest(4);
                let mut v = io.x;
                for (i, ratio) in COMB_RATIOS.into_iter().enumerate() {
                    let delayed = lines.read(i, 4, io, |r, _| (1.0 / r).min(longest) / ratio);
                    let damped = limit(one_pole(&mut state[i], delayed, gg), io.inv_knee);
                    let w = feedback.fma(damped, v);
                    lines.write(i, 4, w);
                    v = damped - feedback * w;
                }
                v
            }
            _ => {
                // The room's size is held to the line, keeping its shape.
                let longest = lines.longest(8);
                let size = move |r: f32| (ROOM_SCALE / r).min(longest);
                let mut sum = L::splat(0.0);
                for (i, scale) in ROOM_COMBS.into_iter().enumerate() {
                    let delayed = lines.read(i, 8, io, |r, _| size(r) * scale);
                    let damped = limit(one_pole(&mut state[i], delayed, gg), io.inv_knee);
                    lines.write(i, 8, feedback.fma(damped, io.x));
                    sum = sum + delayed;
                }
                let mut v = sum * L::splat(0.25);
                let a = L::splat(ROOM_DIFFUSION);
                for (i, scale) in ROOM_ALLPASSES.into_iter().enumerate() {
                    let delayed = lines.read(4 + i, 8, io, |r, _| size(r) * scale);
                    let w = a.fma(delayed, v);
                    lines.write(4 + i, 8, w);
                    v = delayed - a * w;
                }
                v
            }
        }
    }
}

/// The least damping the two-resonance low-pass's second stage reaches:
/// about +12 dB of peak.
const TWO_RESONANCE_FLOOR: f32 = 0.25;

/// Two 12 dB state-variable low-passes at the cutoff with a soft clipper
/// between: the first as the 12 dB low-pass, the second's damping falling
/// with character from Butterworth to [`TWO_RESONANCE_FLOOR`]. State: 0 to
/// 2 the first stage, 3 and 4 the second.
pub(super) struct TwoResonanceLp;

impl Topology<6> for TwoResonanceLp {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let [s0, s1, s2, s3, s4, _] = state;
        let first = svf12::<LOW, L>(s0, s1, s2, io.x, io.g, io.r, io.inv_knee);
        let k = svf_k(K_BUTTERWORTH_2, TWO_RESONANCE_FLOOR, io.character);
        svf(s3, s4, soft_clip(first), io.g, k).1
    }
}

/// A linear four-pole ladder, its input raised by its whole feedback so
/// the pass band stays at unity; feedback rises to 3.9. State: 0 to 3.
pub(super) struct ZeroDelayLp;

impl Topology<6> for ZeroDelayLp {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let k = io.r * L::splat(ZERO_DELAY_LADDER_MAX);
        let input = io.x * (L::splat(1.0) + k);
        ladder(state, input, io.g, k)[3]
    }
}

/// A 12 dB low-pass (damping falling to 1/16) plus character times the
/// input through the second-order all-pass of the same stage. State: 0 and
/// 1 the stage, 2 its previous band output.
pub(super) struct BassAdd;

impl Topology<6> for BassAdd {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let k0 = K_BUTTERWORTH_2;
        let k = saturated_k(k0, svf_k(k0, K_PEAK_MIN, io.r), state[2], io.inv_knee);
        let [s0, s1, s2, ..] = state;
        let (band, low) = svf(s0, s1, io.x, io.g, k);
        *s2 = band;
        let all_pass = io.x - (k + k) * band;
        io.character.fma(all_pass, low)
    }
}

/// The first three formants, in hertz, of a, e, i, o and u (as in father,
/// bed, beet, bought, boot), for adult men, adult women and children:
/// averages from Peterson and Barney, *Control Methods Used in a Study of
/// the Vowels* (1952).
const FORMANTS: [[[f32; 3]; 5]; 3] = [
    [
        [730.0, 1_090.0, 2_440.0],
        [530.0, 1_840.0, 2_480.0],
        [270.0, 2_290.0, 3_010.0],
        [570.0, 840.0, 2_410.0],
        [300.0, 870.0, 2_240.0],
    ],
    [
        [850.0, 1_220.0, 2_810.0],
        [610.0, 2_330.0, 2_990.0],
        [310.0, 2_790.0, 3_310.0],
        [590.0, 920.0, 2_710.0],
        [370.0, 950.0, 2_670.0],
    ],
    [
        [1_030.0, 1_370.0, 3_170.0],
        [690.0, 2_610.0, 3_570.0],
        [370.0, 3_200.0, 3_730.0],
        [680.0, 1_060.0, 3_180.0],
        [430.0, 1_170.0, 3_260.0],
    ],
];
/// The pitch range the cutoff moves through the five vowels across: the
/// whole cutoff control, 8.18 Hz to 22 kHz.
const VOWEL_PITCHES: f32 = 136.8;
/// Each formant's level: the first at unity, then −6 and −12 dB.
const FORMANT_LEVELS: [f32; 3] = [1.0, 0.5, 0.25];
/// The formant band-passes' damping at resonance 0 (Q of about 3).
pub(super) const VOWEL_K: f32 = 0.3;
/// Their least damping, at high resonance (Q 20).
pub(super) const VOWEL_K_MIN: f32 = 0.05;

/// Vowels: three band-passes at the formants of set `SET`, the cutoff's
/// pitch moving linearly (in hertz) from a to e, i, o and u; character
/// raises every formant by up to an octave. State: 0 to 5, two per
/// band-pass.
pub(super) struct Vowel<const SET: usize>;

impl<const SET: usize> Topology<6> for Vowel<SET> {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let (zero, one) = (L::splat(0.0), L::splat(1.0));
        let table = &FORMANTS[SET];
        let position = io.pitch * L::splat(4.0 / VOWEL_PITCHES);
        let scale = super::exp2(io.character) * L::splat(io.inv_rate);
        let k = svf_k(VOWEL_K, VOWEL_K_MIN, io.r);
        let mut out = zero;
        let pairs = state.as_chunks_mut::<2>().0;
        for (j, (pair, level)) in pairs.iter_mut().zip(FORMANT_LEVELS).enumerate() {
            let mut hz = L::splat(table[0][j]);
            for i in 0..4 {
                let along = (position - L::splat(i as f32)).max_(zero).min_(one);
                hz = along.fma(L::splat(table[i + 1][j] - table[i][j]), hz);
            }
            let ratio = (hz * scale).min_(L::splat(RATIO_LIMIT));
            let g = tan(ratio * L::splat(std::f32::consts::PI));
            let [s0, s1] = pair;
            let band = svf(s0, s1, io.x, g, k).0;
            out = (k * band).fma(L::splat(level), out);
        }
        out
    }
}

/// The band reject's widest spacing of its two notches, in octaves.
pub(super) const BAND_REJECT_OCTAVES: f32 = 2.0;

/// Two state-variable notches in series, half the width below and above
/// the cutoff, each with the notch types' damping. State: 0 to 3.
pub(super) struct BandReject;

impl Topology<6> for BandReject {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let half = io.character * L::splat(0.5 * BAND_REJECT_OCTAVES);
        let k = svf_k(K_PEAK, K_PEAK_MIN, io.r);
        let [s0, s1, s2, s3, ..] = state;
        let below = io.x - k * svf(s0, s1, io.x, g_at(io.ratio, -half), k).0;
        below - k * svf(s2, s3, below, g_at(io.ratio, half), k).0
    }
}

/// How far the comb-feedback types' comb, and shriek's feedback low-pass,
/// move across the character range, in octaves: from half this below the
/// cutoff to half above.
pub(super) const CHARACTER_SPAN_OCTAVES: f32 = 4.0;

/// Octaves from the cutoff that character sets, for comb feedback and
/// shriek.
#[inline(always)]
fn around<L: Lane>(character: L) -> L {
    character.fma(
        L::splat(CHARACTER_SPAN_OCTAVES),
        L::splat(-0.5 * CHARACTER_SPAN_OCTAVES),
    )
}

/// A fixed 12 dB state-variable low-pass (Butterworth) or band-pass
/// (Q 1, unity at the centre) whose input adds back its output through a
/// soft clipper and a delay line of one period of the comb's frequency;
/// resonance sets the feedback, up to ±0.98. State: 0 and 1.
pub(super) struct CombFeedback<const BP: bool, const NEG: bool>;

impl<const BP: bool, const NEG: bool> Topology<6> for CombFeedback<BP, NEG> {
    const CHARACTER: bool = true;
    const LINES: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, lines: &mut Lines<'_>) -> L {
        let delayed = lines.read(0, 1, io, |ratio, c| 1.0 / (ratio * super::exp2(around(c))));
        let top = if NEG {
            -COMB_FEEDBACK_MAX
        } else {
            COMB_FEEDBACK_MAX
        };
        let input = (io.r * L::splat(top)).fma(soft_clip(delayed), io.x);
        let k = if BP { K_BAND } else { K_BUTTERWORTH_2 };
        let [s0, s1, ..] = state;
        let (band, low) = svf(s0, s1, input, io.g, L::splat(k));
        let y = if BP { band * L::splat(K_BAND) } else { low };
        lines.write(0, 1, y);
        y
    }
}

/// Shriek's feedback at full drive.
const SHRIEK_MAX: f32 = 1.0;
/// The gain into shriek's feedback clipper.
const SHRIEK_GAIN: f32 = 4.0;

/// A resonant 12 dB state-variable low-pass or band-pass, as the Normal
/// types, whose output, a sample late, through a one-pole low-pass and a
/// soft clipper driven 12 dB hard, is taken from its input in proportion
/// to drive above 50 %; at or below 50 % it's the plain type. State: 0 to
/// 2 the stage, 3 the previous output, 4 the feedback low-pass.
pub(super) struct Shriek<const BP: bool>;

impl<const BP: bool> Topology<6> for Shriek<BP> {
    const CHARACTER: bool = true;

    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let one = L::splat(1.0);
        let amount = (io.drive + io.drive - one).max_(L::splat(0.0)) * L::splat(SHRIEK_MAX);
        let g = g_at(io.ratio, around(io.character));
        let [s0, s1, s2, previous, lp, _] = state;
        let fed = one_pole(lp, *previous, g / (one + g));
        let input = io.x - amount * soft_clip(fed * L::splat(SHRIEK_GAIN));
        let y = if BP {
            svf12::<BAND, L>(s0, s1, s2, input, io.g, io.r, io.inv_knee)
        } else {
            svf12::<LOW, L>(s0, s1, s2, input, io.g, io.r, io.inv_knee)
        };
        *previous = y;
        y
    }
}
