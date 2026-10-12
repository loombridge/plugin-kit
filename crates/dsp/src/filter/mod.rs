//! Filters, per voice, written once for the scalar path and for SIMD lanes
//! across voices: 96 types in four groups.
//!
//! | Group | Types | Character control |
//! |---|---|---|
//! | Normal | state-variable low-pass and high-pass at 6, 12, 18 and 24 dB; band-pass, peak and notch at 12 and 24 dB; a transistor-ladder low-pass at 6, 12, 18 and 24 dB | saturation in the resonance path ([`VoiceParams::saturation`]) |
//! | Multi | thirteen dual types, two state-variable stages in series; eight morphing types | the second stage's cutoff; the morph |
//! | Flanges | combs, flangers and phasers (12, 24, 36 or 48 stages), each with positive or negative feedback and optionally a low-pass, high-pass or both in the feedback path; a flanger-phaser | the feedback filter's frequency, or the band's width |
//! | Misc | low, band and high EQ; ring modulators; sample and hold; combs, all-passes and a small room; a two-resonance and a zero-delay low-pass; bass add; three vowel types; band reject; comb-feedback low-pass and band-pass; shriek low-pass and band-pass | per type: see [`FilterType::character`] |
//!
//! The Normal group's table:
//!
//! | [`FilterType`] | Slopes (dB per octave) | Resonance | Self-oscillates |
//! |---|---|---|---|
//! | Low-pass, high-pass | 6, 12, 18, 24 | from 12 | from 12 |
//! | Band-pass | 12, 24 | yes | yes |
//! | Peak | 12, 24 | raises the peak, up to +24 dB | no |
//! | Notch | 12, 24 | narrows the notch | no |
//! | Ladder low-pass | 6, 12, 18, 24 | yes | yes |
//!
//! Every type's character control is [`VoiceParams::character`], except the
//! Normal group's, whose is [`VoiceParams::saturation`].
//!
//! # Design
//!
//! Every filter is built from trapezoidal-integrator ("topology-preserving
//! transform") stages solved without a unit delay in their feedback, as in
//! Zavalishin, *The Art of VA Filter Design* (2nd edition, 2018), and, for
//! the state-variable stage, the linear trapezoidal SVF in Simper's
//! technical paper *Linear Trap Integrated SVF* (2013). A stage's cutoff is
//! prewarped, so each filter's response is its analogue prototype's on the
//! warped frequency axis: the cutoff is exact at every sample rate, and the
//! tests compare every type with its prototype. Coefficients are worked out
//! for every sample from that sample's cutoff, so cutoff can be modulated
//! at audio rate without zipper noise, and these structures stay stable
//! however fast it moves.
//!
//! - **State-variable family.** 6 dB slopes are one-pole stages (resonance
//!   has nothing to act on); 12 dB is one SVF; 18 dB a one-pole stage then
//!   an SVF; 24 dB two SVFs, the first fixed and the second resonant. At
//!   resonance 0 every low-pass and high-pass is a Butterworth response, so
//!   it's 3 dB down at the cutoff. Band-pass is the SVF's band output
//!   (24 dB: two in series), unity at the centre at resonance 0; peak adds a
//!   band-pass to the dry signal; notch subtracts one.
//! - **Ladder.** Four one-pole low-pass stages with negative feedback from
//!   the last, solved in closed form each sample (Zavalishin, chapter 5;
//!   the circuit analysed by Stilson and Smith, *Analyzing the Moog VCF*,
//!   1996). The 6, 12, 18 and 24 dB slopes are taken after the first,
//!   second, third and fourth stage, with the feedback always from the
//!   fourth. The input is raised by half the feedback, so the pass band
//!   drops less as resonance rises than in the bare circuit.
//! - **Multi.** Dual types are two 12 dB state-variable stages in series
//!   (two one-pole stages for the 6 dB pair), the first at the cutoff and
//!   the second at the character's cutoff, sharing one resonance. Morphing
//!   types take a weighted sum of one stage's low, band and dry signals,
//!   which spans low-pass, band-pass, high-pass, peak and notch; their
//!   24 dB forms are two morphing stages in series, the first fixed.
//! - **Flanges.** A comb is a delay line of one period of the cutoff with
//!   feedback; a flanger's line is half a period, so its first notch, at
//!   Mix 50 %, sits at the cutoff. A phaser is a chain of first-order
//!   all-pass stages at the cutoff with feedback from its output, a sample
//!   late. The wet signal is the line's or the chain's output alone, so
//!   Mix 50 % makes the notches. Feedback filters are one-pole stages
//!   placed relative to the cutoff. Delays are read with cubic Hermite
//!   interpolation.
//! - **Misc.** Each is described on its [`FilterType`] variant.
//!
//! **Nonlinear stages.** Drive (a soft clipper before the filter) and the
//! resonance path's saturator. The saturator is evaluated at the previous
//! sample's resonant output and applied as a gain inside the zero-delay
//! solution, which keeps every sample's solve linear and the feedback never
//! above what the resonance setting asks; at small signals it does
//! nothing. Even with [`VoiceParams::saturation`] at 0 it limits signals far
//! above full scale, which is what holds self-oscillation at a steady level
//! and keeps every setting bounded. The other groups' resonant
//! state-variable stages have the same saturator.
//!
//! **Oversampling.** A bank runs at the rate it's made for. A voice engine
//! running at 2 or 4 times the host rate for quality makes its banks at
//! that rate, so drive and resonance saturation are oversampled with the
//! rest of the voice. The filters are stable and
//! accurate from 44.1 kHz to 768 kHz (192 kHz oversampled 4 times), tested.
//! Outside a voice engine, run a bank inside an
//! [`Oversampler`](crate::Oversampler).
//!
//! # Laws to be matched by measurement
//!
//! These are first laws, chosen to sound right and stay simple, which
//! measurement against the reference instrument will replace:
//!
//! - **Resonance.** [`FilterType::feedback`]: the state-variable damping
//!   falls linearly from its Butterworth value at 0 to slightly negative
//!   at 1 (self-oscillation from about 98 %); the ladder's feedback rises
//!   linearly from 0 to 4.1 (the circuit oscillates from 4). Peak and notch
//!   stop at a damping of 1/16: +24 dB of peak, and the narrowest notch.
//!   Comb and flanger feedback rises linearly to 0.98, phaser feedback to
//!   0.95.
//! - **Drive.** Gain into the soft clipper rises with drive, from 0 dB to
//!   +24 dB, and the clipped signal is blended in by the drive amount, so a
//!   little drive is a little saturation. Clean drive drops the clipper's
//!   input by 24 dB and raises its output by 24 dB.
//! - **Resonance saturation.** The saturator's knee falls from 3.0 (about
//!   +10 dBFS) at 0 to 3/32 at 1, halving every fifth of the range.
//! - **Mix** is a linear blend of the filtered and dry signals; **level** a
//!   linear output gain after the mix.
//! - **Stereo offset** is in semitones: the right channel's cutoff rises
//!   by it and the left's falls; a plug-in maps its control onto it.
//! - **Character**, per type, is on each variant of [`FilterType`]; the
//!   stage counts, delay times and formant tables there are first laws too.
//!
//! # Layout
//!
//! A [`FilterBank`] holds `voices` voices of one or two channels. Audio is
//! interleaved by frame, then channel, then voice, with each channel's
//! voices padded to [`FilterBank::stride`]; cutoff, as a pitch in
//! semitones (MIDI note numbers, A4 = 69 at the reference pitch), is one
//! value per voice per frame, shared by the voice's channels. A one-voice
//! scalar bank's audio is ordinary interleaved stereo. Per-voice settings
//! other than cutoff are smoothed for every sample (a 2 ms one-pole
//! glide), so they change without zipper noise and the output is the same
//! whatever the block sizes.
//!
//! Everything a bank needs is allocated when it's made, including a delay
//! ring of 1/8 s at its rate for every voice and channel;
//! [`FilterBank::process`] allocates nothing, locks nothing and never
//! blocks.

mod flange;
mod lane;
mod lines;
mod misc;
mod multi;

use lane::{Lane, TAN_LIMIT, exp2, saturation_gain, soft_clip, tan};
use lines::Lines;

use std::marker::PhantomData;

use crate::simd::{self, F32s, Kernel, Level};

/// A filter's type: its response and slope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterType {
    /// State-variable low-pass, 6 dB per octave.
    Lp6,
    /// State-variable low-pass, 12 dB per octave.
    Lp12,
    /// State-variable low-pass, 18 dB per octave.
    Lp18,
    /// State-variable low-pass, 24 dB per octave.
    Lp24,
    /// State-variable high-pass, 6 dB per octave.
    Hp6,
    /// State-variable high-pass, 12 dB per octave.
    Hp12,
    /// State-variable high-pass, 18 dB per octave.
    Hp18,
    /// State-variable high-pass, 24 dB per octave.
    Hp24,
    /// State-variable band-pass, 12 dB per octave (6 each side).
    Bp12,
    /// State-variable band-pass, 24 dB per octave (12 each side).
    Bp24,
    /// Peak: the dry signal plus a 12 dB band-pass.
    Peak12,
    /// Peak: the dry signal plus a 24 dB band-pass.
    Peak24,
    /// Notch: one state-variable notch.
    Notch12,
    /// Notch: two state-variable notches in series.
    Notch24,
    /// Transistor-ladder low-pass, after the first stage.
    Ladder6,
    /// Transistor-ladder low-pass, after the second stage.
    Ladder12,
    /// Transistor-ladder low-pass, after the third stage.
    Ladder18,
    /// Transistor-ladder low-pass, after the fourth stage.
    Ladder24,
    /// Dual: a one-pole low-pass at the cutoff, then a one-pole high-pass
    /// at the second cutoff. The second cutoff is the cutoff moved by
    /// −4 to +4 octaves as character goes from 0 to 1, for every dual type.
    DualLh6,
    /// Dual: low-pass then high-pass, 12 dB each.
    DualLh12,
    /// Dual: low-pass then band-pass.
    DualLb12,
    /// Dual: low-pass then peak.
    DualLp12,
    /// Dual: low-pass then notch.
    DualLn12,
    /// Dual: high-pass then band-pass.
    DualHb12,
    /// Dual: high-pass then peak.
    DualHp12,
    /// Dual: high-pass then notch.
    DualHn12,
    /// Dual: band-pass then peak.
    DualBp12,
    /// Dual: band-pass then notch.
    DualBn12,
    /// Dual: two peaks.
    DualPp12,
    /// Dual: peak then notch.
    DualPn12,
    /// Dual: two notches.
    DualNn12,
    /// Morph: low-pass at character 0, band-pass at 0.5, high-pass at 1.
    MorphLbh12,
    /// Morph: low-pass, band-pass, high-pass, 24 dB.
    MorphLbh24,
    /// Morph: low-pass, peak, high-pass.
    MorphLph12,
    /// Morph: low-pass, peak, high-pass, 24 dB.
    MorphLph24,
    /// Morph: low-pass, notch, high-pass.
    MorphLnh12,
    /// Morph: low-pass, notch, high-pass, 24 dB.
    MorphLnh24,
    /// Morph: band-pass, peak, notch.
    MorphBpn12,
    /// Morph: band-pass, peak, notch, 24 dB.
    MorphBpn24,
    /// Comb: a delay of one period of the cutoff with positive feedback,
    /// so its peaks fall on the cutoff's harmonics.
    CombPos,
    /// Comb, negative feedback: peaks on the odd harmonics of half the
    /// cutoff.
    CombNeg,
    /// Comb, positive, with a one-pole low-pass in the feedback path. Its
    /// cutoff falls from 10 octaves above the comb's (open) at character 0
    /// to the comb's own at 1.
    CombLpPos,
    /// Comb, negative, with a low-pass in the feedback path.
    CombLpNeg,
    /// Comb, positive, with a one-pole high-pass in the feedback path. Its
    /// cutoff rises from 10 octaves below the comb's (open) at character 0
    /// to the comb's own at 1.
    CombHpPos,
    /// Comb, negative, with a high-pass in the feedback path.
    CombHpNeg,
    /// Comb, positive, with both feedback filters: a band around the
    /// comb's frequency that narrows as character rises.
    CombBandPos,
    /// Comb, negative, with a band in the feedback path.
    CombBandNeg,
    /// Flanger: a delay of half a period of the cutoff with positive
    /// feedback, so at Mix 50 % its first notch is at the cutoff. Feedback
    /// filters as the comb's.
    FlangerPos,
    /// Flanger, negative feedback.
    FlangerNeg,
    /// Flanger, positive, low-pass in the feedback path.
    FlangerLpPos,
    /// Flanger, negative, low-pass in the feedback path.
    FlangerLpNeg,
    /// Flanger, positive, high-pass in the feedback path.
    FlangerHpPos,
    /// Flanger, negative, high-pass in the feedback path.
    FlangerHpNeg,
    /// Flanger, positive, a band in the feedback path.
    FlangerBandPos,
    /// Flanger, negative, a band in the feedback path.
    FlangerBandNeg,
    /// Phaser: 12 first-order all-pass stages at the cutoff, positive
    /// feedback. Feedback filters as the comb's.
    Phaser12Pos,
    /// Phaser, 12 stages, negative feedback.
    Phaser12Neg,
    /// Phaser, 24 stages, positive.
    Phaser24Pos,
    /// Phaser, 24 stages, negative.
    Phaser24Neg,
    /// Phaser, 36 stages, positive.
    Phaser36Pos,
    /// Phaser, 36 stages, negative.
    Phaser36Neg,
    /// Phaser, 48 stages, positive.
    Phaser48Pos,
    /// Phaser, 48 stages, negative.
    Phaser48Neg,
    /// Phaser, 48 stages, positive, low-pass in the feedback path.
    Phaser48LpPos,
    /// Phaser, 48 stages, negative, low-pass in the feedback path.
    Phaser48LpNeg,
    /// Phaser, 48 stages, positive, high-pass in the feedback path.
    Phaser48HpPos,
    /// Phaser, 48 stages, negative, high-pass in the feedback path.
    Phaser48HpNeg,
    /// Phaser, 48 stages, positive, a band in the feedback path.
    Phaser48BandPos,
    /// Phaser, 48 stages, negative, a band in the feedback path.
    Phaser48BandNeg,
    /// Flanger-phaser: 12 all-pass stages whose feedback runs through the
    /// flanger's delay line and a band, positive.
    FlangePhaser12BandPos,
    /// Flanger-phaser, negative.
    FlangePhaser12BandNeg,
    /// Low shelf, 6 dB: the low band's gain is character's gain law,
    /// (2 × character)⁴: silent at 0 (a high-pass), unity at 0.5, +24 dB at
    /// 1. Resonance does nothing.
    LowEq6,
    /// Low shelf, 12 dB, from a 12 dB high-pass at character 0 to +24 dB of
    /// low shelf at 1; resonance raises a bump at the cutoff.
    LowEq12,
    /// Band (peaking) EQ, 12 dB: gain at the cutoff by the gain law, from a
    /// notch at character 0; resonance narrows the band.
    BandEq12,
    /// High shelf, 6 dB, from a low-pass at character 0.
    HighEq6,
    /// High shelf, 12 dB, from a 12 dB low-pass at character 0.
    HighEq12,
    /// Ring modulator: the input times a sine at the cutoff.
    Ring,
    /// Two ring modulators in parallel, the second's frequency up to an
    /// octave above the first's as character rises (spread).
    Ring2,
    /// Sample and hold: the input held for one period of the cutoff.
    SampleHold,
    /// Sample and hold, difference: the input less the held value.
    SampleHoldDiff,
    /// Four combs in parallel at the cutoff and at 2^(3/12), 2^(6/12) and
    /// 2^(9/12) of it, each with a damping low-pass in its feedback (as
    /// the comb's low-pass); resonance sets their feedback. Half dry, half
    /// combs, whatever the mix.
    Combs,
    /// Four damped all-pass delays in series at the combs' delays.
    Allpasses,
    /// A small room: four damped combs in parallel and two all-passes in
    /// series, its size in proportion to the cutoff's period; resonance
    /// sets the decay, character the damping.
    Room,
    /// Two-resonance low-pass: two 12 dB state-variable low-passes at the
    /// cutoff with a soft clipper between, the first resonant by
    /// resonance, the second by character (up to about +12 dB).
    TwoResonanceLp,
    /// Zero-delay low-pass: a linear four-pole ladder whose input is
    /// raised by its whole feedback, so its pass band stays at unity; it
    /// stops short of oscillation.
    ZeroDelayLp,
    /// Bass add: a 12 dB low-pass plus, by character (through), the input
    /// through a second-order all-pass at the cutoff, so the low band adds
    /// in phase: +6 dB of bass at character 1.
    BassAdd,
    /// Vowel, adult male formants: three band-passes at the first three
    /// formants of a, e, i, o and u, the cutoff's whole range moving
    /// through the five; character raises every formant by up to an
    /// octave, resonance narrows them.
    Vowel1,
    /// Vowel, adult female formants.
    Vowel2,
    /// Vowel, child formants.
    Vowel3,
    /// Band reject: two notches, either side of the cutoff, up to two
    /// octaves apart by character (width); resonance narrows each.
    BandReject,
    /// Comb-feedback low-pass: a 12 dB low-pass whose input adds back its
    /// output through a soft clipper and a delay line, positive. The
    /// comb's frequency is the cutoff moved by −2 to +2 octaves by
    /// character; resonance sets the feedback.
    CombFeedbackLp1,
    /// Comb-feedback band-pass, positive.
    CombFeedbackBp1,
    /// Comb-feedback low-pass, negative.
    CombFeedbackLp2,
    /// Comb-feedback band-pass, negative.
    CombFeedbackBp2,
    /// Shriek low-pass: a resonant 12 dB low-pass whose output comes back,
    /// a sample late, through a one-pole low-pass and a hard-driven
    /// clipper, subtracted from its input, once drive passes 50 %. The
    /// feedback low-pass is the cutoff moved by −2 to +2 octaves by
    /// character.
    ShriekLp,
    /// Shriek band-pass.
    ShriekBp,
}

/// Which group a type belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterGroup {
    /// State-variable and ladder low-, high-, band-pass, peak and notch.
    Normal,
    /// Dual and morphing types.
    Multi,
    /// Combs, flangers and phasers.
    Flanges,
    /// Everything else.
    Misc,
}

/// How a type's resonance control maps onto its structure.
#[derive(Clone, Copy)]
enum Law {
    /// Resonance does nothing.
    None,
    /// A state-variable damping from `k0` at 0, falling to `floor`.
    Svf(f32, f32),
    /// The ladder's feedback.
    Ladder,
    /// The zero-delay ladder's feedback.
    ZeroDelayLadder,
    /// A feedback gain rising linearly to this (signed) value.
    Gain(f32),
}

/// What a type is, in one place.
struct Info {
    name: &'static str,
    group: FilterGroup,
    slope: u32,
    law: Law,
    oscillates: bool,
    character: Option<&'static str>,
}

/// The damping at resonance 0 of the 12 dB low-pass and high-pass: a
/// Butterworth pair of poles.
const K_BUTTERWORTH_2: f32 = std::f32::consts::SQRT_2;
/// The damping of the 18 dB types' state-variable stage at resonance 0:
/// with the one-pole stage, a third-order Butterworth response.
const K_BUTTERWORTH_3: f32 = 1.0;
/// The 24 dB low-pass and high-pass stages' dampings at resonance 0: a
/// fourth-order Butterworth response, 2 sin(3π/8) for the fixed stage and
/// 2 sin(π/8) for the resonant one.
const K_BUTTERWORTH_4: [f32; 2] = [1.847_759, 0.765_366_9];
/// Band-pass damping at resonance 0: Q = 1.
const K_BAND: f32 = 1.0;
/// Peak and notch damping at resonance 0: the peak is flat there.
const K_PEAK: f32 = 2.0;
/// The least damping peak and notch reach, at resonance 1: +24 dB of peak.
const K_PEAK_MIN: f32 = 1.0 / 16.0;
/// How far past zero damping the self-oscillating state-variable types go
/// at resonance 1, as a fraction of their damping at resonance 0.
const SVF_OVERSHOOT: f32 = 0.02;
/// The ladder's feedback at resonance 1; it oscillates from 4.
const LADDER_MAX: f32 = 4.1;
/// How much of the ladder's feedback raises its input.
const LADDER_COMPENSATION: f32 = 0.5;
/// The zero-delay ladder's feedback at resonance 1, short of oscillation.
const ZERO_DELAY_LADDER_MAX: f32 = 3.9;
/// Comb and flanger feedback at resonance 1.
const COMB_FEEDBACK_MAX: f32 = 0.98;
/// Phaser feedback at resonance 1.
const PHASER_FEEDBACK_MAX: f32 = 0.95;
/// The resonance saturator's knee at saturation 0.
const SATURATION_KNEE: f32 = 3.0;
/// Octaves the knee falls across the saturation range.
const SATURATION_OCTAVES: f32 = 5.0;
/// Drive's gain into the clipper at full drive, in decibels.
const DRIVE_DB: f32 = 24.0;
/// What clean drive drops the clipper's input by, and raises its output
/// by, in decibels.
const CLEAN_DB: f32 = 24.0;
/// The smoothing glide's time constant, in seconds.
const SMOOTHING_SECONDS: f32 = 0.002;
/// The highest cutoff a stage runs at, as a fraction of the sample rate:
/// just short of Nyquist.
const RATIO_LIMIT: f32 = TAN_LIMIT / std::f32::consts::PI;
/// The reference pitch's default: A4 in hertz.
pub const DEFAULT_REFERENCE: f32 = 440.0;

/// The state-variable law for the plain low-pass and high-pass types'
/// resonance, at 12 dB.
const LH: Law = Law::Svf(K_BUTTERWORTH_2, f32::NEG_INFINITY);
/// The band-pass law.
const BAND_LAW: Law = Law::Svf(K_BAND, f32::NEG_INFINITY);
/// The peak and notch law.
const PEAK_LAW: Law = Law::Svf(K_PEAK, K_PEAK_MIN);

impl FilterType {
    /// The Normal group, in the order of its table.
    pub const NORMAL: [Self; 18] = [
        Self::Lp6,
        Self::Lp12,
        Self::Lp18,
        Self::Lp24,
        Self::Hp6,
        Self::Hp12,
        Self::Hp18,
        Self::Hp24,
        Self::Bp12,
        Self::Bp24,
        Self::Peak12,
        Self::Peak24,
        Self::Notch12,
        Self::Notch24,
        Self::Ladder6,
        Self::Ladder12,
        Self::Ladder18,
        Self::Ladder24,
    ];

    /// The Multi group.
    pub const MULTI: [Self; 21] = [
        Self::DualLh6,
        Self::DualLh12,
        Self::DualLb12,
        Self::DualLp12,
        Self::DualLn12,
        Self::DualHb12,
        Self::DualHp12,
        Self::DualHn12,
        Self::DualBp12,
        Self::DualBn12,
        Self::DualPp12,
        Self::DualPn12,
        Self::DualNn12,
        Self::MorphLbh12,
        Self::MorphLbh24,
        Self::MorphLph12,
        Self::MorphLph24,
        Self::MorphLnh12,
        Self::MorphLnh24,
        Self::MorphBpn12,
        Self::MorphBpn24,
    ];

    /// The Flanges group.
    pub const FLANGES: [Self; 32] = [
        Self::CombPos,
        Self::CombNeg,
        Self::CombLpPos,
        Self::CombLpNeg,
        Self::CombHpPos,
        Self::CombHpNeg,
        Self::CombBandPos,
        Self::CombBandNeg,
        Self::FlangerPos,
        Self::FlangerNeg,
        Self::FlangerLpPos,
        Self::FlangerLpNeg,
        Self::FlangerHpPos,
        Self::FlangerHpNeg,
        Self::FlangerBandPos,
        Self::FlangerBandNeg,
        Self::Phaser12Pos,
        Self::Phaser12Neg,
        Self::Phaser24Pos,
        Self::Phaser24Neg,
        Self::Phaser36Pos,
        Self::Phaser36Neg,
        Self::Phaser48Pos,
        Self::Phaser48Neg,
        Self::Phaser48LpPos,
        Self::Phaser48LpNeg,
        Self::Phaser48HpPos,
        Self::Phaser48HpNeg,
        Self::Phaser48BandPos,
        Self::Phaser48BandNeg,
        Self::FlangePhaser12BandPos,
        Self::FlangePhaser12BandNeg,
    ];

    /// The Misc group.
    pub const MISC: [Self; 25] = [
        Self::LowEq6,
        Self::LowEq12,
        Self::BandEq12,
        Self::HighEq6,
        Self::HighEq12,
        Self::Ring,
        Self::Ring2,
        Self::SampleHold,
        Self::SampleHoldDiff,
        Self::Combs,
        Self::Allpasses,
        Self::Room,
        Self::TwoResonanceLp,
        Self::ZeroDelayLp,
        Self::BassAdd,
        Self::Vowel1,
        Self::Vowel2,
        Self::Vowel3,
        Self::BandReject,
        Self::CombFeedbackLp1,
        Self::CombFeedbackBp1,
        Self::CombFeedbackLp2,
        Self::CombFeedbackBp2,
        Self::ShriekLp,
        Self::ShriekBp,
    ];

    /// Every type: the Normal, Multi, Flanges and Misc groups in turn.
    pub const ALL: [Self; 96] = {
        let mut all = [Self::Lp6; 96];
        let mut at = 0;
        let groups: [&[Self]; 4] = [&Self::NORMAL, &Self::MULTI, &Self::FLANGES, &Self::MISC];
        let mut g = 0;
        while g < groups.len() {
            let mut i = 0;
            while i < groups[g].len() {
                all[at] = groups[g][i];
                at += 1;
                i += 1;
            }
            g += 1;
        }
        all
    };

    #[allow(clippy::too_many_lines)]
    const fn info(self) -> Info {
        use FilterGroup::{Flanges, Misc, Multi, Normal};
        const fn i(
            name: &'static str,
            group: FilterGroup,
            slope: u32,
            law: Law,
            oscillates: bool,
            character: Option<&'static str>,
        ) -> Info {
            Info {
                name,
                group,
                slope,
                law,
                oscillates,
                character,
            }
        }
        const SAT: Option<&str> = Some("resonance saturation");
        const SECOND: Option<&str> = Some("second cutoff");
        const MORPH: Option<&str> = Some("morph");
        const FB_LP: Option<&str> = Some("feedback low-pass");
        const FB_HP: Option<&str> = Some("feedback high-pass");
        const FB_BAND: Option<&str> = Some("feedback band width");
        const GAIN: Option<&str> = Some("gain");
        const DAMPING: Option<&str> = Some("damping");
        const COMB_HZ: Option<&str> = Some("comb frequency");
        const FB_CUT: Option<&str> = Some("feedback cutoff");
        let lp24 = Law::Svf(K_BUTTERWORTH_4[1], f32::NEG_INFINITY);
        let lp18 = Law::Svf(K_BUTTERWORTH_3, f32::NEG_INFINITY);
        const fn comb(negative: bool) -> Law {
            Law::Gain(if negative {
                -COMB_FEEDBACK_MAX
            } else {
                COMB_FEEDBACK_MAX
            })
        }
        const fn phaser(negative: bool) -> Law {
            Law::Gain(if negative {
                -PHASER_FEEDBACK_MAX
            } else {
                PHASER_FEEDBACK_MAX
            })
        }
        const VOWEL: Law = Law::Svf(misc::VOWEL_K, misc::VOWEL_K_MIN);
        match self {
            Self::Lp6 => i("svf-lp6", Normal, 6, Law::None, false, SAT),
            Self::Lp12 => i("svf-lp12", Normal, 12, LH, true, SAT),
            Self::Lp18 => i("svf-lp18", Normal, 18, lp18, true, SAT),
            Self::Lp24 => i("svf-lp24", Normal, 24, lp24, true, SAT),
            Self::Hp6 => i("svf-hp6", Normal, 6, Law::None, false, SAT),
            Self::Hp12 => i("svf-hp12", Normal, 12, LH, true, SAT),
            Self::Hp18 => i("svf-hp18", Normal, 18, lp18, true, SAT),
            Self::Hp24 => i("svf-hp24", Normal, 24, lp24, true, SAT),
            Self::Bp12 => i("svf-bp12", Normal, 12, BAND_LAW, true, SAT),
            Self::Bp24 => i("svf-bp24", Normal, 24, BAND_LAW, true, SAT),
            Self::Peak12 => i("svf-peak12", Normal, 12, PEAK_LAW, false, SAT),
            Self::Peak24 => i("svf-peak24", Normal, 24, PEAK_LAW, false, SAT),
            Self::Notch12 => i("svf-notch12", Normal, 12, PEAK_LAW, false, SAT),
            Self::Notch24 => i("svf-notch24", Normal, 24, PEAK_LAW, false, SAT),
            Self::Ladder6 => i("ladder-6", Normal, 6, Law::Ladder, true, SAT),
            Self::Ladder12 => i("ladder-12", Normal, 12, Law::Ladder, true, SAT),
            Self::Ladder18 => i("ladder-18", Normal, 18, Law::Ladder, true, SAT),
            Self::Ladder24 => i("ladder-24", Normal, 24, Law::Ladder, true, SAT),
            Self::DualLh6 => i("dual-lh6", Multi, 6, Law::None, false, SECOND),
            Self::DualLh12 => i("dual-lh12", Multi, 12, LH, true, SECOND),
            Self::DualLb12 => i("dual-lb12", Multi, 12, LH, true, SECOND),
            Self::DualLp12 => i("dual-lp12", Multi, 12, LH, true, SECOND),
            Self::DualLn12 => i("dual-ln12", Multi, 12, LH, true, SECOND),
            Self::DualHb12 => i("dual-hb12", Multi, 12, LH, true, SECOND),
            Self::DualHp12 => i("dual-hp12", Multi, 12, LH, true, SECOND),
            Self::DualHn12 => i("dual-hn12", Multi, 12, LH, true, SECOND),
            Self::DualBp12 => i("dual-bp12", Multi, 12, BAND_LAW, true, SECOND),
            Self::DualBn12 => i("dual-bn12", Multi, 12, BAND_LAW, true, SECOND),
            Self::DualPp12 => i("dual-pp12", Multi, 12, PEAK_LAW, false, SECOND),
            Self::DualPn12 => i("dual-pn12", Multi, 12, PEAK_LAW, false, SECOND),
            Self::DualNn12 => i("dual-nn12", Multi, 12, PEAK_LAW, false, SECOND),
            Self::MorphLbh12 => i("morph-lbh12", Multi, 12, LH, true, MORPH),
            Self::MorphLbh24 => i("morph-lbh24", Multi, 24, lp24, true, MORPH),
            Self::MorphLph12 => i("morph-lph12", Multi, 12, LH, true, MORPH),
            Self::MorphLph24 => i("morph-lph24", Multi, 24, lp24, true, MORPH),
            Self::MorphLnh12 => i("morph-lnh12", Multi, 12, LH, true, MORPH),
            Self::MorphLnh24 => i("morph-lnh24", Multi, 24, lp24, true, MORPH),
            Self::MorphBpn12 => i(
                "morph-bpn12",
                Multi,
                12,
                Law::Svf(K_BAND, K_PEAK_MIN),
                false,
                MORPH,
            ),
            Self::MorphBpn24 => i(
                "morph-bpn24",
                Multi,
                24,
                Law::Svf(K_BAND, K_PEAK_MIN),
                false,
                MORPH,
            ),
            Self::CombPos => i("comb+", Flanges, 0, comb(false), false, None),
            Self::CombNeg => i("comb-", Flanges, 0, comb(true), false, None),
            Self::CombLpPos => i("comb-lp+", Flanges, 0, comb(false), false, FB_LP),
            Self::CombLpNeg => i("comb-lp-", Flanges, 0, comb(true), false, FB_LP),
            Self::CombHpPos => i("comb-hp+", Flanges, 0, comb(false), false, FB_HP),
            Self::CombHpNeg => i("comb-hp-", Flanges, 0, comb(true), false, FB_HP),
            Self::CombBandPos => i("comb-band+", Flanges, 0, comb(false), false, FB_BAND),
            Self::CombBandNeg => i("comb-band-", Flanges, 0, comb(true), false, FB_BAND),
            Self::FlangerPos => i("flanger+", Flanges, 0, comb(false), false, None),
            Self::FlangerNeg => i("flanger-", Flanges, 0, comb(true), false, None),
            Self::FlangerLpPos => i("flanger-lp+", Flanges, 0, comb(false), false, FB_LP),
            Self::FlangerLpNeg => i("flanger-lp-", Flanges, 0, comb(true), false, FB_LP),
            Self::FlangerHpPos => i("flanger-hp+", Flanges, 0, comb(false), false, FB_HP),
            Self::FlangerHpNeg => i("flanger-hp-", Flanges, 0, comb(true), false, FB_HP),
            Self::FlangerBandPos => i("flanger-band+", Flanges, 0, comb(false), false, FB_BAND),
            Self::FlangerBandNeg => i("flanger-band-", Flanges, 0, comb(true), false, FB_BAND),
            Self::Phaser12Pos => i("phaser-12+", Flanges, 0, phaser(false), false, None),
            Self::Phaser12Neg => i("phaser-12-", Flanges, 0, phaser(true), false, None),
            Self::Phaser24Pos => i("phaser-24+", Flanges, 0, phaser(false), false, None),
            Self::Phaser24Neg => i("phaser-24-", Flanges, 0, phaser(true), false, None),
            Self::Phaser36Pos => i("phaser-36+", Flanges, 0, phaser(false), false, None),
            Self::Phaser36Neg => i("phaser-36-", Flanges, 0, phaser(true), false, None),
            Self::Phaser48Pos => i("phaser-48+", Flanges, 0, phaser(false), false, None),
            Self::Phaser48Neg => i("phaser-48-", Flanges, 0, phaser(true), false, None),
            Self::Phaser48LpPos => i("phaser-48-lp+", Flanges, 0, phaser(false), false, FB_LP),
            Self::Phaser48LpNeg => i("phaser-48-lp-", Flanges, 0, phaser(true), false, FB_LP),
            Self::Phaser48HpPos => i("phaser-48-hp+", Flanges, 0, phaser(false), false, FB_HP),
            Self::Phaser48HpNeg => i("phaser-48-hp-", Flanges, 0, phaser(true), false, FB_HP),
            Self::Phaser48BandPos => {
                i("phaser-48-band+", Flanges, 0, phaser(false), false, FB_BAND)
            }
            Self::Phaser48BandNeg => i("phaser-48-band-", Flanges, 0, phaser(true), false, FB_BAND),
            Self::FlangePhaser12BandPos => i(
                "flange-phaser-12-band+",
                Flanges,
                0,
                phaser(false),
                false,
                FB_BAND,
            ),
            Self::FlangePhaser12BandNeg => i(
                "flange-phaser-12-band-",
                Flanges,
                0,
                phaser(true),
                false,
                FB_BAND,
            ),
            Self::LowEq6 => i("low-eq6", Misc, 6, Law::None, false, GAIN),
            Self::LowEq12 => i(
                "low-eq12",
                Misc,
                12,
                Law::Svf(K_BUTTERWORTH_2, K_PEAK_MIN),
                false,
                GAIN,
            ),
            Self::BandEq12 => i("band-eq12", Misc, 12, PEAK_LAW, false, GAIN),
            Self::HighEq6 => i("high-eq6", Misc, 6, Law::None, false, GAIN),
            Self::HighEq12 => i(
                "high-eq12",
                Misc,
                12,
                Law::Svf(K_BUTTERWORTH_2, K_PEAK_MIN),
                false,
                GAIN,
            ),
            Self::Ring => i("ring", Misc, 0, Law::None, false, None),
            Self::Ring2 => i("ring-x2", Misc, 0, Law::None, false, Some("spread")),
            Self::SampleHold => i("sample-hold", Misc, 0, Law::None, false, None),
            Self::SampleHoldDiff => i("sample-hold-diff", Misc, 0, Law::None, false, None),
            Self::Combs => i("combs", Misc, 0, comb(false), false, DAMPING),
            Self::Allpasses => i("allpasses", Misc, 0, comb(false), false, DAMPING),
            Self::Room => i("room", Misc, 0, comb(false), false, DAMPING),
            Self::TwoResonanceLp => i(
                "two-resonance-lp",
                Misc,
                24,
                LH,
                true,
                Some("second resonance"),
            ),
            Self::ZeroDelayLp => i("zero-delay-lp", Misc, 24, Law::ZeroDelayLadder, false, None),
            Self::BassAdd => i(
                "bass-add",
                Misc,
                12,
                Law::Svf(K_BUTTERWORTH_2, K_PEAK_MIN),
                false,
                Some("through"),
            ),
            Self::Vowel1 => i("vowel-1", Misc, 0, VOWEL, false, Some("formant shift")),
            Self::Vowel2 => i("vowel-2", Misc, 0, VOWEL, false, Some("formant shift")),
            Self::Vowel3 => i("vowel-3", Misc, 0, VOWEL, false, Some("formant shift")),
            Self::BandReject => i("band-reject", Misc, 12, PEAK_LAW, false, Some("width")),
            Self::CombFeedbackLp1 => i("comb-feedback-lp1", Misc, 12, comb(false), false, COMB_HZ),
            Self::CombFeedbackBp1 => i("comb-feedback-bp1", Misc, 12, comb(false), false, COMB_HZ),
            Self::CombFeedbackLp2 => i("comb-feedback-lp2", Misc, 12, comb(true), false, COMB_HZ),
            Self::CombFeedbackBp2 => i("comb-feedback-bp2", Misc, 12, comb(true), false, COMB_HZ),
            Self::ShriekLp => i("shriek-lp", Misc, 12, LH, true, FB_CUT),
            Self::ShriekBp => i("shriek-bp", Misc, 12, BAND_LAW, true, FB_CUT),
        }
    }

    /// A short name: `svf-lp12`, `ladder-24`, `comb-lp+` and so on.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.info().name
    }

    /// Its group.
    #[must_use]
    pub const fn group(self) -> FilterGroup {
        self.info().group
    }

    /// Its slope in decibels per octave, or 0 for types with none (combs,
    /// phasers, ring modulators, sample and hold, the vowels and the
    /// multi-comb types).
    #[must_use]
    pub const fn slope(self) -> u32 {
        self.info().slope
    }

    /// What its character control does, or `None` if it does nothing. The
    /// Normal group's is [`VoiceParams::saturation`]; every other group's is
    /// [`VoiceParams::character`].
    #[must_use]
    pub const fn character(self) -> Option<&'static str> {
        self.info().character
    }

    /// Whether resonance does anything (not for the 6 dB state-variable
    /// types, which are one-pole stages, nor for several Misc types).
    #[must_use]
    pub const fn has_resonance(self) -> bool {
        !matches!(self.info().law, Law::None)
    }

    /// Whether high resonance makes it oscillate on its own.
    #[must_use]
    pub const fn self_oscillates(self) -> bool {
        self.info().oscillates
    }

    /// Whether it runs on delay lines.
    const fn uses_lines(self) -> bool {
        matches!(self.group(), FilterGroup::Flanges)
            && !matches!(
                self,
                Self::Phaser12Pos
                    | Self::Phaser12Neg
                    | Self::Phaser24Pos
                    | Self::Phaser24Neg
                    | Self::Phaser36Pos
                    | Self::Phaser36Neg
                    | Self::Phaser48Pos
                    | Self::Phaser48Neg
                    | Self::Phaser48LpPos
                    | Self::Phaser48LpNeg
                    | Self::Phaser48HpPos
                    | Self::Phaser48HpNeg
                    | Self::Phaser48BandPos
                    | Self::Phaser48BandNeg
            )
            || matches!(
                self,
                Self::Combs
                    | Self::Allpasses
                    | Self::Room
                    | Self::CombFeedbackLp1
                    | Self::CombFeedbackBp1
                    | Self::CombFeedbackLp2
                    | Self::CombFeedbackBp2
            )
    }

    /// The resonance law: for the state-variable types, the resonant
    /// stage's damping (`k`, 1/Q) at `resonance` from 0 to 1; for the
    /// ladders, their feedback gain; for the comb, flanger, phaser and
    /// multi-comb types, the feedback gain, negative for the negative
    /// types. Without saturation, this is the value the analogue prototype
    /// has. Types where resonance does nothing return 0.
    #[must_use]
    pub fn feedback(self, resonance: f32) -> f32 {
        let r = resonance.clamp(0.0, 1.0);
        match self.info().law {
            Law::None => 0.0,
            Law::Ladder => ladder_k(r),
            Law::ZeroDelayLadder => r * ZERO_DELAY_LADDER_MAX,
            Law::Svf(k0, floor) => svf_k(k0, floor, r),
            Law::Gain(top) => r * top,
        }
    }
}

impl std::fmt::Display for FilterType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The state-variable resonance law, for any lanes.
#[inline(always)]
fn svf_k<L: Lane>(k0: f32, floor: f32, r: L) -> L {
    r.fma(L::splat(-k0 * (1.0 + SVF_OVERSHOOT)), L::splat(k0))
        .max_(L::splat(floor))
}

/// The ladder's resonance law, for any lanes.
#[inline(always)]
fn ladder_k<L: Lane>(r: L) -> L {
    r * L::splat(LADDER_MAX)
}

/// One voice's settings, other than cutoff. A bank glides to new values
/// over a couple of milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceParams {
    /// Resonance, 0 to 1 ([`FilterType::feedback`] gives the law).
    pub resonance: f32,
    /// Drive, 0 to 1: gain into a soft clipper before the filter.
    pub drive: f32,
    /// Saturation in the resonance path, 0 to 1, which tames resonance at
    /// high levels: the Normal group's character control.
    pub saturation: f32,
    /// Character, 0 to 1: every other group's character control, whose job
    /// [`FilterType::character`] names.
    pub character: f32,
    /// Stereo cutoff offset in semitones: the right channel's cutoff rises
    /// by this and the left's falls. No effect on one channel.
    pub stereo_offset: f32,
    /// Filtered against dry, 0 (dry) to 1 (filtered).
    pub mix: f32,
    /// Output gain, linear, after the mix.
    pub level: f32,
}

impl Default for VoiceParams {
    /// Resonance 10 %, no drive, saturation or character, no offset, fully
    /// filtered, unity level.
    fn default() -> Self {
        Self {
            resonance: 0.1,
            drive: 0.0,
            saturation: 0.0,
            character: 0.0,
            stereo_offset: 0.0,
            mix: 1.0,
            level: 1.0,
        }
    }
}

/// Indices of the smoothed settings.
const RES: usize = 0;
const DRIVE: usize = 1;
const DRIVE_GAIN: usize = 2;
const INV_KNEE: usize = 3;
const OFFSET: usize = 4;
const MIX: usize = 5;
const LEVEL: usize = 6;
const CHARACTER: usize = 7;
/// How many smoothed settings each slot has.
const PARAMS: usize = 8;
/// The most state variables a type has: the 48-stage phasers'.
const MAX_STATES: usize = 52;
/// Every SIMD bank's stride is a multiple of the widest lanes.
const MAX_LANES: usize = 16;
/// The most channels a bank takes.
pub const MAX_CHANNELS: usize = 2;

/// The settings a bank smooths towards, for one voice, in the form the
/// kernel uses.
fn targets_of(params: &VoiceParams) -> [f32; PARAMS] {
    let drive = params.drive.clamp(0.0, 1.0);
    let saturation = params.saturation.clamp(0.0, 1.0);
    let mut t = [0.0; PARAMS];
    t[RES] = params.resonance.clamp(0.0, 1.0);
    t[DRIVE] = drive;
    t[DRIVE_GAIN] = 10f32.powf(drive * DRIVE_DB / 20.0);
    t[INV_KNEE] = (saturation * SATURATION_OCTAVES).exp2() / SATURATION_KNEE;
    t[OFFSET] = if params.stereo_offset.is_finite() {
        params.stereo_offset
    } else {
        0.0
    };
    t[MIX] = params.mix.clamp(0.0, 1.0);
    t[LEVEL] = if params.level.is_finite() {
        params.level.max(0.0)
    } else {
        0.0
    };
    t[CHARACTER] = params.character.clamp(0.0, 1.0);
    t
}

/// Filters for a number of voices, of one or two channels, with SIMD lanes
/// across voices or one voice at a time. See the module documentation for
/// the audio layout.
#[derive(Clone, Debug)]
pub struct FilterBank {
    simd: bool,
    voices: usize,
    channels: usize,
    stride: usize,
    kind: FilterType,
    clean: bool,
    rate: f32,
    reference: f32,
    /// Per voice (`stride` values).
    targets: [Vec<f32>; PARAMS],
    /// Per slot (`channels × stride` values).
    smoothed: [Vec<f32>; PARAMS],
    /// Per slot; [`MAX_STATES`] of them.
    state: Vec<Vec<f32>>,
    /// Every slot's delay ring, `ring` values each.
    delay: Vec<f32>,
    /// Each slot's delay ring length.
    ring: usize,
    /// Where the next frame is written in every ring.
    cursor: usize,
}

impl FilterBank {
    /// A bank of `voices` voices with `channels` channels (1 or 2) at
    /// `sample_rate`, processed with SIMD lanes across voices at the level
    /// [`simd::level`] chooses. Its stride is `voices` rounded up to a
    /// multiple of 16. Allocates, and calls [`simd::level`], so make it off
    /// the audio thread. Every voice starts with [`VoiceParams::default`]
    /// and type [`FilterType::Ladder12`].
    ///
    /// # Panics
    ///
    /// If `channels` isn't 1 or 2, `voices` is 0, or `sample_rate` isn't
    /// positive.
    #[must_use]
    pub fn new(sample_rate: f32, voices: usize, channels: usize) -> Self {
        let _ = simd::level();
        Self::make(true, sample_rate, voices, channels)
    }

    /// A bank like [`new`](Self::new)'s, processed one voice at a time
    /// without SIMD lanes. Its stride is `voices`, so a one-voice bank
    /// takes ordinary interleaved audio.
    ///
    /// # Panics
    ///
    /// As [`new`](Self::new).
    #[must_use]
    pub fn scalar(sample_rate: f32, voices: usize, channels: usize) -> Self {
        Self::make(false, sample_rate, voices, channels)
    }

    fn make(simd: bool, sample_rate: f32, voices: usize, channels: usize) -> Self {
        assert!(
            (1..=MAX_CHANNELS).contains(&channels),
            "a bank has 1 or 2 channels"
        );
        assert!(voices > 0, "a bank has at least one voice");
        assert!(sample_rate > 0.0, "the sample rate must be positive");
        let stride = if simd {
            voices.next_multiple_of(MAX_LANES)
        } else {
            voices
        };
        let targets = targets_of(&VoiceParams::default());
        let ring = lines::ring_len(sample_rate);
        let mut bank = Self {
            simd,
            voices,
            channels,
            stride,
            kind: FilterType::Ladder12,
            clean: false,
            rate: sample_rate,
            reference: DEFAULT_REFERENCE,
            targets: std::array::from_fn(|i| vec![targets[i]; stride]),
            smoothed: std::array::from_fn(|i| vec![targets[i]; channels * stride]),
            state: vec![vec![0.0; channels * stride]; MAX_STATES],
            delay: vec![0.0; channels * stride * ring],
            ring,
            cursor: 0,
        };
        bank.reset();
        bank
    }

    /// How many voices it holds.
    #[must_use]
    pub fn voices(&self) -> usize {
        self.voices
    }

    /// How many channels each voice has.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// How many values each channel of a frame takes in the audio, and each
    /// frame in the pitch buffer: the voices, padded.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Whether it processes with SIMD lanes.
    #[must_use]
    pub fn is_simd(&self) -> bool {
        self.simd
    }

    /// The type every voice uses.
    #[must_use]
    pub fn filter_type(&self) -> FilterType {
        self.kind
    }

    /// Sets the type. A change clears every voice's filter state (and
    /// delay lines), as the types' states don't carry over, and moves
    /// character to its target at once.
    pub fn set_type(&mut self, kind: FilterType) {
        if kind != self.kind {
            self.kind = kind;
            self.clear_state();
            for c in 0..self.channels {
                let (to, from) = (&mut self.smoothed[CHARACTER], &self.targets[CHARACTER]);
                to[c * self.stride..(c + 1) * self.stride].copy_from_slice(from);
            }
        }
    }

    /// Sets clean drive: the clipper's input 24 dB lower and its output
    /// 24 dB higher, for less saturation at the same gain.
    pub fn set_clean_drive(&mut self, clean: bool) {
        self.clean = clean;
    }

    /// The sample rate it runs at.
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.rate
    }

    /// Sets the sample rate. Filter states carry on. The delay lines keep
    /// the length they were made with, so above the rate the bank was made
    /// for, the delay types' lowest frequencies rise in proportion.
    ///
    /// # Panics
    ///
    /// If `sample_rate` isn't positive.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        assert!(sample_rate > 0.0, "the sample rate must be positive");
        self.rate = sample_rate;
    }

    /// Sets the frequency of A4 (pitch 69), 440 Hz by default.
    pub fn set_reference(&mut self, hertz: f32) {
        if hertz > 0.0 && hertz.is_finite() {
            self.reference = hertz;
        }
    }

    /// Sets voice `voice`'s settings; it glides to them over a couple of
    /// milliseconds.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn set_params(&mut self, voice: usize, params: &VoiceParams) {
        assert!(voice < self.voices, "no voice {voice}");
        for (i, value) in targets_of(params).into_iter().enumerate() {
            self.targets[i][voice] = value;
        }
    }

    /// Starts voice `voice` afresh, as at a note-on: its filter state
    /// and delay lines cleared and its settings at `params` at once,
    /// without gliding.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn reset_voice(&mut self, voice: usize, params: &VoiceParams) {
        self.set_params(voice, params);
        for c in 0..self.channels {
            let slot = c * self.stride + voice;
            for i in 0..PARAMS {
                self.smoothed[i][slot] = self.targets[i][voice];
            }
            for s in &mut self.state {
                s[slot] = 0.0;
            }
            if self.kind.uses_lines() {
                self.delay[slot * self.ring..(slot + 1) * self.ring].fill(0.0);
            }
        }
    }

    /// Clears every voice's filter state and moves every setting to its
    /// target at once.
    pub fn reset(&mut self) {
        self.clear_state();
        for c in 0..self.channels {
            for i in 0..PARAMS {
                let (to, from) = (&mut self.smoothed[i], &self.targets[i]);
                to[c * self.stride..(c + 1) * self.stride].copy_from_slice(from);
            }
        }
    }

    fn clear_state(&mut self) {
        for s in &mut self.state {
            s.fill(0.0);
        }
        if self.kind.uses_lines() {
            self.delay.fill(0.0);
        }
    }

    /// Filters `audio` in place: `frames` frames of `channels × stride`
    /// values, where `frames` is `pitch.len() / stride`, with voice `v`'s
    /// cutoff at frame `f` the pitch `pitch[f × stride + v]` in semitones
    /// (69 is the reference pitch, A4). Padding voices in a group of lanes
    /// with real voices are processed with them, so their audio changes;
    /// their values mean nothing.
    ///
    /// # Panics
    ///
    /// If `pitch` isn't a whole number of frames, or `audio` holds a
    /// different number of frames.
    pub fn process(&mut self, pitch: &[f32], audio: &mut [f32]) {
        assert_eq!(
            pitch.len() % self.stride,
            0,
            "the pitch buffer holds a whole number of frames"
        );
        let frames = pitch.len() / self.stride;
        assert_eq!(
            audio.len(),
            frames * self.channels * self.stride,
            "the audio holds as many frames as the pitch buffer"
        );
        let simd = self.simd;
        self.dispatch(simd.then(simd::level), pitch, audio, frames);
    }

    /// As [`process`](Self::process), with SIMD lanes at `level`, for tests
    /// and benchmarks that compare paths. Gives `false`, and leaves `audio`
    /// alone, if this CPU can't run `level` or the bank is scalar.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_at(&mut self, level: Level, pitch: &[f32], audio: &mut [f32]) -> bool {
        if !self.simd {
            return false;
        }
        assert_eq!(pitch.len() % self.stride, 0);
        let frames = pitch.len() / self.stride;
        assert_eq!(audio.len(), frames * self.channels * self.stride);
        self.dispatch(Some(level), pitch, audio, frames)
    }

    /// Runs the block with this bank's type, scalar or at `level`. Each type
    /// is a kernel of its own, so each compiled copy holds one structure.
    #[allow(clippy::too_many_lines)]
    fn dispatch(
        &mut self,
        level: Option<Level>,
        pitch: &[f32],
        audio: &mut [f32],
        frames: usize,
    ) -> bool {
        use FilterType as F;
        use flange::{Comb, FlangePhaser, Phaser};
        use misc::{
            BandReject, BassAdd, CombFeedback, Eq, MultiComb, Ring, SampleHold, Shriek,
            TwoResonanceLp, Vowel, ZeroDelayLp,
        };
        use multi::{Dual, DualOnePole, Morph};
        fn go<const N: usize, T: Topology<N>>(
            bank: &mut FilterBank,
            level: Option<Level>,
            pitch: &[f32],
            audio: &mut [f32],
            frames: usize,
        ) -> bool {
            let run = Run::<N, T> {
                bank,
                pitch,
                audio,
                frames,
                topology: PhantomData,
            };
            match level {
                None => {
                    run.go::<f32>();
                    true
                }
                Some(level) => simd::dispatch_at(level, run).is_ok(),
            }
        }
        const S: usize = 6;
        const P: usize = MAX_STATES;
        let run = match self.kind {
            F::Lp6 => go::<S, OnePole<false>>,
            F::Hp6 => go::<S, OnePole<true>>,
            F::Lp12 => go::<S, Svf12<LOW>>,
            F::Hp12 => go::<S, Svf12<HIGH>>,
            F::Bp12 => go::<S, Svf12<BAND>>,
            F::Peak12 => go::<S, Svf12<PEAK>>,
            F::Notch12 => go::<S, Svf12<NOTCH>>,
            F::Lp18 => go::<S, Svf18<false>>,
            F::Hp18 => go::<S, Svf18<true>>,
            F::Lp24 => go::<S, Svf24<LOW>>,
            F::Hp24 => go::<S, Svf24<HIGH>>,
            F::Bp24 => go::<S, Svf24<BAND>>,
            F::Peak24 => go::<S, Svf24<PEAK>>,
            F::Notch24 => go::<S, Svf24<NOTCH>>,
            F::Ladder6 => go::<S, Ladder<1>>,
            F::Ladder12 => go::<S, Ladder<2>>,
            F::Ladder18 => go::<S, Ladder<3>>,
            F::Ladder24 => go::<S, Ladder<4>>,
            F::DualLh6 => go::<S, DualOnePole>,
            F::DualLh12 => go::<S, Dual<LOW, HIGH>>,
            F::DualLb12 => go::<S, Dual<LOW, BAND>>,
            F::DualLp12 => go::<S, Dual<LOW, PEAK>>,
            F::DualLn12 => go::<S, Dual<LOW, NOTCH>>,
            F::DualHb12 => go::<S, Dual<HIGH, BAND>>,
            F::DualHp12 => go::<S, Dual<HIGH, PEAK>>,
            F::DualHn12 => go::<S, Dual<HIGH, NOTCH>>,
            F::DualBp12 => go::<S, Dual<BAND, PEAK>>,
            F::DualBn12 => go::<S, Dual<BAND, NOTCH>>,
            F::DualPp12 => go::<S, Dual<PEAK, PEAK>>,
            F::DualPn12 => go::<S, Dual<PEAK, NOTCH>>,
            F::DualNn12 => go::<S, Dual<NOTCH, NOTCH>>,
            F::MorphLbh12 => go::<S, Morph<{ multi::LBH }, false>>,
            F::MorphLbh24 => go::<S, Morph<{ multi::LBH }, true>>,
            F::MorphLph12 => go::<S, Morph<{ multi::LPH }, false>>,
            F::MorphLph24 => go::<S, Morph<{ multi::LPH }, true>>,
            F::MorphLnh12 => go::<S, Morph<{ multi::LNH }, false>>,
            F::MorphLnh24 => go::<S, Morph<{ multi::LNH }, true>>,
            F::MorphBpn12 => go::<S, Morph<{ multi::BPN }, false>>,
            F::MorphBpn24 => go::<S, Morph<{ multi::BPN }, true>>,
            F::CombPos => go::<S, Comb<false, { flange::NONE }, false>>,
            F::CombNeg => go::<S, Comb<false, { flange::NONE }, true>>,
            F::CombLpPos => go::<S, Comb<false, { flange::LP }, false>>,
            F::CombLpNeg => go::<S, Comb<false, { flange::LP }, true>>,
            F::CombHpPos => go::<S, Comb<false, { flange::HP }, false>>,
            F::CombHpNeg => go::<S, Comb<false, { flange::HP }, true>>,
            F::CombBandPos => go::<S, Comb<false, { flange::BAND }, false>>,
            F::CombBandNeg => go::<S, Comb<false, { flange::BAND }, true>>,
            F::FlangerPos => go::<S, Comb<true, { flange::NONE }, false>>,
            F::FlangerNeg => go::<S, Comb<true, { flange::NONE }, true>>,
            F::FlangerLpPos => go::<S, Comb<true, { flange::LP }, false>>,
            F::FlangerLpNeg => go::<S, Comb<true, { flange::LP }, true>>,
            F::FlangerHpPos => go::<S, Comb<true, { flange::HP }, false>>,
            F::FlangerHpNeg => go::<S, Comb<true, { flange::HP }, true>>,
            F::FlangerBandPos => go::<S, Comb<true, { flange::BAND }, false>>,
            F::FlangerBandNeg => go::<S, Comb<true, { flange::BAND }, true>>,
            F::Phaser12Pos => go::<P, Phaser<12, { flange::NONE }, false>>,
            F::Phaser12Neg => go::<P, Phaser<12, { flange::NONE }, true>>,
            F::Phaser24Pos => go::<P, Phaser<24, { flange::NONE }, false>>,
            F::Phaser24Neg => go::<P, Phaser<24, { flange::NONE }, true>>,
            F::Phaser36Pos => go::<P, Phaser<36, { flange::NONE }, false>>,
            F::Phaser36Neg => go::<P, Phaser<36, { flange::NONE }, true>>,
            F::Phaser48Pos => go::<P, Phaser<48, { flange::NONE }, false>>,
            F::Phaser48Neg => go::<P, Phaser<48, { flange::NONE }, true>>,
            F::Phaser48LpPos => go::<P, Phaser<48, { flange::LP }, false>>,
            F::Phaser48LpNeg => go::<P, Phaser<48, { flange::LP }, true>>,
            F::Phaser48HpPos => go::<P, Phaser<48, { flange::HP }, false>>,
            F::Phaser48HpNeg => go::<P, Phaser<48, { flange::HP }, true>>,
            F::Phaser48BandPos => go::<P, Phaser<48, { flange::BAND }, false>>,
            F::Phaser48BandNeg => go::<P, Phaser<48, { flange::BAND }, true>>,
            F::FlangePhaser12BandPos => go::<P, FlangePhaser<false>>,
            F::FlangePhaser12BandNeg => go::<P, FlangePhaser<true>>,
            F::LowEq6 => go::<S, Eq<{ misc::LOW6 }>>,
            F::LowEq12 => go::<S, Eq<{ misc::LOW12 }>>,
            F::BandEq12 => go::<S, Eq<{ misc::BAND12 }>>,
            F::HighEq6 => go::<S, Eq<{ misc::HIGH6 }>>,
            F::HighEq12 => go::<S, Eq<{ misc::HIGH12 }>>,
            F::Ring => go::<S, Ring<false>>,
            F::Ring2 => go::<S, Ring<true>>,
            F::SampleHold => go::<S, SampleHold<false>>,
            F::SampleHoldDiff => go::<S, SampleHold<true>>,
            F::Combs => go::<S, MultiComb<{ misc::COMBS }>>,
            F::Allpasses => go::<S, MultiComb<{ misc::ALLPASSES }>>,
            F::Room => go::<S, MultiComb<{ misc::ROOM }>>,
            F::TwoResonanceLp => go::<S, TwoResonanceLp>,
            F::ZeroDelayLp => go::<S, ZeroDelayLp>,
            F::BassAdd => go::<S, BassAdd>,
            F::Vowel1 => go::<S, Vowel<0>>,
            F::Vowel2 => go::<S, Vowel<1>>,
            F::Vowel3 => go::<S, Vowel<2>>,
            F::BandReject => go::<S, BandReject>,
            F::CombFeedbackLp1 => go::<S, CombFeedback<false, false>>,
            F::CombFeedbackBp1 => go::<S, CombFeedback<true, false>>,
            F::CombFeedbackLp2 => go::<S, CombFeedback<false, true>>,
            F::CombFeedbackBp2 => go::<S, CombFeedback<true, true>>,
            F::ShriekLp => go::<S, Shriek<false>>,
            F::ShriekBp => go::<S, Shriek<true>>,
        };
        run(self, level, pitch, audio, frames)
    }
}

/// The pitch, in semitones with A4 = 69 at `reference`, of `hertz`.
#[must_use]
pub fn pitch_of(hertz: f32, reference: f32) -> f32 {
    69.0 + 12.0 * (hertz / reference).log2()
}

/// The frequency of `pitch`, in semitones with A4 = 69 at `reference`.
#[must_use]
pub fn hertz_of(pitch: f32, reference: f32) -> f32 {
    reference * ((pitch - 69.0) / 12.0).exp2()
}

/// One call of [`FilterBank::process`], as a SIMD kernel.
struct Run<'a, const N: usize, T> {
    bank: &'a mut FilterBank,
    pitch: &'a [f32],
    audio: &'a mut [f32],
    frames: usize,
    topology: PhantomData<T>,
}

impl<const N: usize, T: Topology<N>> Kernel for Run<'_, N, T> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        self.go::<S>();
    }
}

/// What every sample of a block shares.
struct Shared<L> {
    alpha: L,
    ref_over_rate: L,
    sign: L,
    clip_in: L,
    clip_out: L,
}

impl<const N: usize, T: Topology<N>> Run<'_, N, T> {
    #[inline(always)]
    fn go<L: Lane>(self) {
        let bank = self.bank;
        let groups = bank.voices.div_ceil(L::LANES);
        let alpha = 1.0 - (-1.0 / (SMOOTHING_SECONDS * bank.rate)).exp();
        let clean = 10f32.powf(-CLEAN_DB / 20.0);
        for c in 0..bank.channels {
            let sign = match (bank.channels, c) {
                (1, _) => 0.0,
                (_, 0) => -1.0,
                _ => 1.0,
            };
            let shared = Shared {
                alpha: L::splat(alpha),
                ref_over_rate: L::splat(bank.reference / bank.rate),
                sign: L::splat(sign),
                clip_in: L::splat(if bank.clean { clean } else { 1.0 }),
                clip_out: L::splat(if bank.clean { 1.0 / clean } else { 1.0 }),
            };
            for g in 0..groups {
                let voice = g * L::LANES;
                let slot = c * bank.stride + voice;
                let targets: [L; PARAMS] =
                    std::array::from_fn(|i| L::load(&bank.targets[i][voice..]));
                let mut params: [L; PARAMS] =
                    std::array::from_fn(|i| L::load(&bank.smoothed[i][slot..]));
                let mut state: [L; N] = std::array::from_fn(|i| L::load(&bank.state[i][slot..]));
                let block = Block {
                    pitch: self.pitch,
                    frames: self.frames,
                    stride: bank.stride,
                    frame_stride: bank.channels * bank.stride,
                    voice,
                    slot,
                    inv_rate: 1.0 / bank.rate,
                    ref_over_rate: bank.reference / bank.rate,
                };
                let mut lines = Lines {
                    buf: &mut bank.delay,
                    len: bank.ring,
                    cursor: bank.cursor,
                    slot,
                };
                block.run::<L, N, T>(
                    &shared,
                    &targets,
                    &mut params,
                    &mut state,
                    &mut lines,
                    self.audio,
                );
                for (i, value) in params.iter().enumerate() {
                    value.store(&mut bank.smoothed[i][slot..]);
                }
                for (i, value) in state.iter().enumerate() {
                    value.store(&mut bank.state[i][slot..]);
                }
            }
        }
        if T::LINES {
            bank.cursor = bank.cursor.wrapping_add(self.frames);
        }
    }
}

/// Where one group of lanes sits in a block's buffers.
struct Block<'a> {
    pitch: &'a [f32],
    frames: usize,
    stride: usize,
    frame_stride: usize,
    voice: usize,
    slot: usize,
    inv_rate: f32,
    ref_over_rate: f32,
}

/// The cutoff, as a fraction of the sample rate, of a pitch: held just
/// below Nyquist.
#[inline(always)]
fn ratio_of<L: Lane>(pitch: L, ref_over_rate: L) -> L {
    let octaves = (pitch - L::splat(69.0)) * L::splat(1.0 / 12.0);
    (exp2(octaves) * ref_over_rate).min_(L::splat(RATIO_LIMIT))
}

/// The integrator gain of a stage at `ratio` moved by `octaves`, held
/// below Nyquist.
#[inline(always)]
fn g_at<L: Lane>(ratio: L, octaves: L) -> L {
    let ratio = (ratio * exp2(octaves)).min_(L::splat(RATIO_LIMIT));
    tan(ratio * L::splat(std::f32::consts::PI))
}

impl Block<'_> {
    /// Runs one group of lanes of one channel through the block.
    #[inline(always)]
    fn run<L: Lane, const N: usize, T: Topology<N>>(
        &self,
        shared: &Shared<L>,
        targets: &[L; PARAMS],
        params: &mut [L; PARAMS],
        state: &mut [L; N],
        lines: &mut Lines<'_>,
        audio: &mut [f32],
    ) {
        let one = L::splat(1.0);
        for f in 0..self.frames {
            let pitch = L::load(&self.pitch[f * self.stride + self.voice..]);
            let at = f * self.frame_stride + self.slot;
            let x = L::load(&audio[at..]);
            for (i, (p, t)) in params.iter_mut().zip(targets).enumerate() {
                if i != CHARACTER || T::CHARACTER {
                    *p = (*t - *p).fma(shared.alpha, *p);
                }
            }
            // Cutoff: the pitch, offset for this channel, to the
            // prewarped integrator gain g = tan(π f / rate).
            let pitch = params[OFFSET].fma(shared.sign, pitch);
            let ratio = ratio_of(pitch, shared.ref_over_rate);
            let g = tan(ratio * L::splat(std::f32::consts::PI));
            // Drive: gain into the soft clipper, blended in by the amount.
            let u = x * params[DRIVE_GAIN];
            let clipped = soft_clip(u * shared.clip_in) * shared.clip_out;
            let driven = (clipped - u).fma(params[DRIVE], u);
            let io = Io {
                x: driven,
                g,
                ratio,
                pitch,
                r: params[RES],
                inv_knee: params[INV_KNEE],
                character: params[CHARACTER],
                drive: params[DRIVE],
                ref_over_rate: self.ref_over_rate,
                inv_rate: self.inv_rate,
            };
            let y = T::tick(state, &io, lines);
            if T::LINES {
                lines.advance();
            }
            let out = if T::IGNORES_MIX {
                y * params[LEVEL]
            } else {
                y.fma(params[MIX], x * (one - params[MIX])) * params[LEVEL]
            };
            out.store(&mut audio[at..]);
        }
    }
}

/// What a filter structure's sample takes.
struct Io<L> {
    /// The input, after drive.
    x: L,
    /// The cutoff's integrator gain, tan(π f / rate).
    g: L,
    /// The cutoff as a fraction of the sample rate.
    ratio: L,
    /// The cutoff as a pitch, offset for this channel.
    pitch: L,
    /// Resonance, 0 to 1.
    r: L,
    /// The resonance saturator's inverse knee.
    inv_knee: L,
    /// Character, 0 to 1 (smoothed only for the types that use it).
    character: L,
    /// Drive, 0 to 1.
    drive: L,
    /// The reference pitch over the sample rate.
    ref_over_rate: f32,
    /// One over the sample rate.
    inv_rate: f32,
}

/// One filter structure's sample: the state (`N` variables), the sample's
/// inputs and the delay lines in; the output out.
trait Topology<const N: usize> {
    /// Whether it reads the character control.
    const CHARACTER: bool = false;
    /// Whether it runs on the delay lines.
    const LINES: bool = false;
    /// Whether its output ignores the mix control.
    const IGNORES_MIX: bool = false;
    fn tick<L: Lane>(state: &mut [L; N], io: &Io<L>, lines: &mut Lines<'_>) -> L;
}

/// Which output of a state-variable structure a type takes.
const LOW: u8 = 0;
const HIGH: u8 = 1;
const BAND: u8 = 2;
const PEAK: u8 = 3;
const NOTCH: u8 = 4;

/// A one-pole trapezoidal low-pass stage: its output, given the integrator
/// gain's `g / (1 + g)`, with its state updated.
#[inline(always)]
fn one_pole<L: Lane>(s: &mut L, x: L, gg: L) -> L {
    let v = (x - *s) * gg;
    let low = v + *s;
    *s = low + v;
    low
}

/// One linear trapezoidal state-variable stage (Simper, 2013) with damping
/// `k`: its band and low outputs, with its two states updated. The high
/// output is `x - k band - low`.
#[inline(always)]
fn svf<L: Lane>(ic1: &mut L, ic2: &mut L, x: L, g: L, k: L) -> (L, L) {
    let one = L::splat(1.0);
    let a1 = one / g.fma(g + k, one);
    let a2 = g * a1;
    let a3 = g * a2;
    let v3 = x - *ic2;
    let band = a1.fma(*ic1, a2 * v3);
    let low = a2.fma(*ic1, a3.fma(v3, *ic2));
    *ic1 = band + band - *ic1;
    *ic2 = low + low - *ic2;
    (band, low)
}

/// The damping a resonant stage runs at: the law's value `k`, pulled back
/// towards the resonance-0 damping `k0` by the saturator's gain at the
/// previous band output.
#[inline(always)]
fn saturated_k<L: Lane>(k0: f32, k: L, previous_band: L, inv_knee: L) -> L {
    let gain = saturation_gain(previous_band, inv_knee);
    let k0 = L::splat(k0);
    (k - k0).fma(gain, k0)
}

/// `x` through the resonance path's saturator: unchanged when small,
/// approaching the knee (1 / `inv_knee`) when large. The feedback paths
/// of the delay and phaser types run through it, so that however fast
/// their delays or cutoffs move, nothing grows without bound.
#[inline(always)]
fn limit<L: Lane>(x: L, inv_knee: L) -> L {
    x * saturation_gain(x, inv_knee)
}

/// One resonant 12 dB state-variable stage taking output `OUT`, with the
/// resonance law and saturation of the 12 dB types: `s0` and `s1` the
/// stage, `s2` its previous band output.
#[inline(always)]
fn svf12<const OUT: u8, L: Lane>(
    s0: &mut L,
    s1: &mut L,
    s2: &mut L,
    x: L,
    g: L,
    r: L,
    inv_knee: L,
) -> L {
    let (k0, floor) = match OUT {
        LOW | HIGH => (K_BUTTERWORTH_2, f32::NEG_INFINITY),
        BAND => (K_BAND, f32::NEG_INFINITY),
        _ => (K_PEAK, K_PEAK_MIN),
    };
    let k = saturated_k(k0, svf_k(k0, floor, r), *s2, inv_knee);
    let (band, low) = svf(s0, s1, x, g, k);
    *s2 = band;
    match OUT {
        LOW => low,
        HIGH => x - k * band - low,
        BAND => band * L::splat(k0),
        PEAK => (L::splat(1.0) - k * L::splat(1.0 / k0)).fma(band, x),
        _ => x - k * band,
    }
}

/// The 6 dB types: a one-pole stage. State: 0.
struct OnePole<const HP: bool>;

impl<const HP: bool> Topology<6> for OnePole<HP> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let gg = io.g / (L::splat(1.0) + io.g);
        let low = one_pole(&mut state[0], io.x, gg);
        if HP { io.x - low } else { low }
    }
}

/// The 12 dB types: one resonant state-variable stage. State: 0 and 1 the
/// stage, 2 its previous band output.
struct Svf12<const OUT: u8>;

impl<const OUT: u8> Topology<6> for Svf12<OUT> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let [s0, s1, s2, ..] = state;
        svf12::<OUT, L>(s0, s1, s2, io.x, io.g, io.r, io.inv_knee)
    }
}

/// The 18 dB types: a one-pole stage, then a resonant state-variable stage
/// of the same kind. State: 0 to 2 as [`Svf12`], 3 the one-pole stage.
struct Svf18<const HP: bool>;

impl<const HP: bool> Topology<6> for Svf18<HP> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let (x, g) = (io.x, io.g);
        let gg = g / (L::splat(1.0) + g);
        let first = one_pole(&mut state[3], x, gg);
        let first = if HP { x - first } else { first };
        let k0 = K_BUTTERWORTH_3;
        let k = saturated_k(
            k0,
            svf_k(k0, f32::NEG_INFINITY, io.r),
            state[2],
            io.inv_knee,
        );
        let [s0, s1, s2, ..] = state;
        let (band, low) = svf(s0, s1, first, g, k);
        *s2 = band;
        if HP { first - k * band - low } else { low }
    }
}

/// The 24 dB types: two state-variable stages in series, the second
/// resonant (for notch, both narrow together). State: 0 to 2 the second
/// stage as [`Svf12`], 3 and 4 the first.
struct Svf24<const OUT: u8>;

impl<const OUT: u8> Topology<6> for Svf24<OUT> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let (x, g) = (io.x, io.g);
        let (k0, floor, k_first) = match OUT {
            LOW | HIGH => (K_BUTTERWORTH_4[1], f32::NEG_INFINITY, K_BUTTERWORTH_4[0]),
            BAND => (K_BAND, f32::NEG_INFINITY, K_BAND),
            PEAK => (K_PEAK, K_PEAK_MIN, K_BAND),
            _ => (K_PEAK, K_PEAK_MIN, 0.0),
        };
        let k = saturated_k(k0, svf_k(k0, floor, io.r), state[2], io.inv_knee);
        let [s0, s1, s2, s3, s4, _] = state;
        // The first stage: fixed, except a notch's, which narrows with the
        // second.
        let k1 = if OUT == NOTCH { k } else { L::splat(k_first) };
        let (band1, low1) = svf(s3, s4, x, g, k1);
        let first = match OUT {
            LOW => low1,
            HIGH => x - k1 * band1 - low1,
            BAND | PEAK => band1 * L::splat(k_first),
            _ => x - k1 * band1,
        };
        let (band, low) = svf(s0, s1, first, g, k);
        *s2 = band;
        match OUT {
            LOW => low,
            HIGH => first - k * band - low,
            BAND => band * L::splat(k0),
            PEAK => (L::splat(1.0) - k * L::splat(1.0 / k0)).fma(band, x),
            _ => first - k * band,
        }
    }
}

/// The ladder: four one-pole stages with feedback from the last, solved in
/// closed form, the output taken after stage `TAP`. State: 0 to 3 the
/// stages, 4 the previous fourth-stage output.
struct Ladder<const TAP: usize>;

impl<const TAP: usize> Topology<6> for Ladder<TAP> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; 6], io: &Io<L>, _: &mut Lines<'_>) -> L {
        let one = L::splat(1.0);
        let gain = saturation_gain(state[4], io.inv_knee);
        let k = ladder_k(io.r) * gain;
        let input = io.x * k.fma(L::splat(LADDER_COMPENSATION), one);
        let taps = ladder(state, input, io.g, k);
        state[4] = taps[3];
        taps[TAP - 1]
    }
}

/// Four one-pole stages (states 0 to 3) with feedback `k` from the last,
/// solved in closed form: each stage's output.
#[inline(always)]
fn ladder<L: Lane>(state: &mut [L; 6], input: L, g: L, k: L) -> [L; 4] {
    let one = L::splat(1.0);
    let gg = g / (one + g);
    let h = one - gg;
    // Each stage's output is gg × its input + h × its state.
    let b = [h * state[0], h * state[1], h * state[2], h * state[3]];
    let sum = ((b[0] * gg + b[1]) * gg + b[2]).fma(gg, b[3]);
    let gg2 = gg * gg;
    let gg4 = gg2 * gg2;
    let y4 = gg4.fma(input, sum) / k.fma(gg4, one);
    let mut u = input - k * y4;
    let mut taps = [u; 4];
    for (i, tap) in taps.iter_mut().enumerate() {
        let y = gg.fma(u, b[i]);
        state[i] = y + y - state[i];
        *tap = y;
        u = y;
    }
    taps
}
