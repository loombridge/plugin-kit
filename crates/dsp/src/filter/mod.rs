//! Zero-delay-feedback filters, per voice, written once for the scalar path
//! and for SIMD lanes across voices: a state-variable family and a
//! four-pole transistor ladder (the normal group), and an extended group of
//! morphing, DJ, all-pass, ladder-character, pole-zero, comb, expander and
//! K35 types.
//!
//! | [`FilterType`] | Slopes (dB per octave) | Resonance | Self-oscillates |
//! |---|---|---|---|
//! | Low-pass, high-pass | 6, 12, 18, 24 | from 12 | from 12 |
//! | Band-pass | 12, 24 | yes | yes |
//! | Peak | 12, 24 | raises the peak, up to +24 dB | no |
//! | Notch | 12, 24 | narrows the notch | no |
//! | Ladder low-pass | 6, 12, 18, 24 | yes | yes |
//!
//! The extended group, in menu order, with what each one's Character does
//! ([`FilterType::character`], [`VoiceParams::character`]):
//!
//! | [`FilterType`] | Structure | Character | Self-oscillates |
//! |---|---|---|---|
//! | Buzz morph | 12 dB state-variable stage, asymmetric shaper before it, low resonance knee | morph: low-pass, notch at 0.5, high-pass | yes |
//! | DJ sweep | 12 dB state-variable stage; the cutoff control sweeps low-pass, flat, high-pass | none | no |
//! | Diffuser | second-order all-pass stages at the cutoff in series | stages, 1 to 8; resonance sets their Q | no |
//! | Transistor ladder | four one-pole stages whose gain falls as they swing, feedback from the last | Smooth | yes |
//! | Diode ladder | four coupled stages, asymmetric shaper before them, bass thinning with resonance | Smooth | yes |
//! | Clean ladder | the diode ladder's coupled stages, pass band compensated, no shaper | Smooth | yes |
//! | Driven ladder | the transistor ladder after a soft clipper, low resonance knee | overdrive, 0 to +30 dB into the clipper | yes |
//! | Pole-zero | eight drawn points per state, four states morphed by X and Y ([`pole_zero`]) | Smooth (cutoff, X and Y) | no |
//! | Comb II | feedback comb reading two taps | second frequency, up to two octaves above | no |
//! | Expander multimode | four-pole ladder, its taps mixed | low-pass, notch at 0.5, high-pass | yes |
//! | Expander band-pass | four-pole ladder, its taps mixed | none | yes |
//! | K35 | Sallen–Key low-pass and high-pass, saturating feedback amplifier | LP/HP: low-pass below 0.5, high-pass from it | yes |
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
//!
//! - **Extended group.** Every type is built from the same trapezoidal
//!   stages, solved without a unit delay:
//!   - *Buzz morph*: one state-variable stage whose low-pass and high-pass
//!     outputs are weighted min(1, 2 (1 − c)) and min(1, 2 c), so it's a
//!     notch at c = 0.5.
//!   - *DJ sweep*: the cutoff pitch's distance from the control's middle
//!     (pitch 68.4) picks the side and how far: below it a low-pass from
//!     20 kHz down to 30 Hz, above it a high-pass from 10 Hz up to 10 kHz,
//!     flat within 4 % of each half of the middle and fading in over the
//!     next tenth.
//!   - *Diffuser*: all-pass stages x − 2 k band, the output crossfading
//!     between the taps after whole numbers of stages.
//!   - *Transistor ladder*: the normal ladder's closed-form solution with
//!     each stage's integrator gain scaled by 1 / √(1 + (state / 4)²), the
//!     stage's swing on the previous sample (the "cheap" gain-scaling
//!     method of solving a nonlinear zero-delay loop one linear solve per
//!     sample, after Huovilainen, *Non-linear digital implementation of the
//!     Moog ladder filter*, DAFx 2004, and Zavalishin, chapter 6). Its
//!     self-oscillation sags about 2 % in pitch at full level.
//!   - *Diode ladders*: ẏ₁ = ω (u − y₁ − a (y₁ − y₂)), ẏ₂ = ω (y₁ − y₂ −
//!     a (y₂ − y₃)), ẏ₃ = ω (y₂ − y₃ − a (y₃ − y₄)), ẏ₄ = ω (y₃ − y₄), with
//!     u = input − k y₄ and coupling a = 0.5: a linearised diode ladder,
//!     whose stages load each other (Zavalishin, chapter 5; Stinchcombe,
//!     *Analysis of the Moog transistor ladder and derivative filters*,
//!     2008). Each sample's four coupled equations are solved by
//!     elimination. The stage frequency is the cutoff over 1.0553, where
//!     the open loop turns half a cycle, so it oscillates at the cutoff,
//!     from k = 8.618.
//!   - *Pole-zero*: see [`pole_zero`].
//!   - *Comb II*: y = x + f · clip(½ (y[n − T₁] + y[n − T₂])), out = (1 − f) y,
//!     T₁ the cutoff's period (20 Hz at the longest), T₂ the second
//!     frequency's, read with four-point Lagrange interpolation.
//!   - *Expanders*: the normal ladder, its output Σ cᵢ yᵢ over the ladder's
//!     input y₀ and stages y₁ to y₄: low-pass (0, 0, 0, 0, 1), notch
//!     (1, −4, 8, −8, 4), high-pass (1, −4, 6, −4, 1), band-pass
//!     4 × (0, 0, 1, −2, 1), the four-pole responses of mixing a ladder's
//!     taps (Zavalishin, chapter 5).
//!   - *K35*: low-pass y = LP(LP(x) + K · HP(y)) and high-pass
//!     y = HP(HP(x) + K · LP(y)) from one-pole stages, which are
//!     1 / (s² + (2 − K) s + 1) and s² / (s² + (2 − K) s + 1), so they
//!     oscillate from K = 2 (after Pirkle's application notes on
//!     virtual-analogue Korg35 filters, 2013).
//!
//! **Nonlinear stages.** Drive (a soft clipper before the filter) and the
//! resonance path's saturator. The saturator is evaluated at the previous
//! sample's resonant output and applied as a gain inside the zero-delay
//! solution, which keeps every sample's solve linear and the feedback never
//! above what the resonance setting asks; at small signals it does
//! nothing. Even with [`VoiceParams::saturation`] at 0 it limits signals far
//! above full scale, which is what holds self-oscillation at a steady level
//! and keeps every setting bounded.
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
//! - **The extended group's resonance**: the buzz morph as the 12 dB
//!   state-variable types; the DJ sweep's damping from √2 down to 0.35
//!   (about +9 dB of peak); the diffuser's stages from Q 0.5 to 5; the
//!   transistor, driven and expander ladders as the normal ladder; the
//!   diode ladders' feedback from 0 to 1.025 times its edge; the K35's
//!   K from 0 to 2.04; Comb II's feedback from 0 to 0.98. The buzz morph,
//!   acid ladder, driven ladder and K35 run their resonance saturators 3,
//!   2, 4 and 3 times lower than the normal types'.
//! - **Smooth** is a one-pole lag on the cutoff pitch (and the pole-zero
//!   X and Y) with time constant 100 ms × c², starting at the first sample
//!   after a voice resets.
//! - **The extended group's Characters**: the diffuser's stages
//!   1 + 7 c; the driven ladder's clipper gain 0 to +30 dB, its output
//!   divided by the clipper's at full scale; Comb II's second frequency
//!   2^(2 c) times the first; the K35 crossfading from low-pass to
//!   high-pass between c = 0.45 and 0.55.
//! - **The acid shaper** is (clip(x + 0.3) − clip(0.3)) / clip′(0.3) with
//!   the soft clipper below: unity slope at zero, so small signals pass,
//!   and even harmonics as it saturates.
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
//! The pole-zero filter's X and Y can also be modulated at audio rate
//! ([`FilterBank::process_xy`]), as offsets laid out like the pitch.
//!
//! Everything a bank needs is allocated when it's made, including Comb II's
//! delay lines (50 ms per voice and channel at the bank's sample rate);
//! [`FilterBank::process`] allocates nothing, locks nothing and never
//! blocks.

mod extended;
mod lane;
pub mod pole_zero;

use lane::{Lane, TAN_LIMIT, exp2, saturation_gain, soft_clip, tan};
use pole_zero::PoleZeroDesign;

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
    /// Buzz morph: a gritty state-variable stage morphing from low-pass
    /// through notch to high-pass. Character: the morph.
    BuzzMorph,
    /// DJ sweep: the cutoff control sweeps from low-pass through flat to
    /// high-pass. Character: none.
    DjSweep,
    /// Diffuser: second-order all-pass stages in series. Character: how
    /// many, 1 to 8.
    Diffuser,
    /// Transistor ladder with saturating stages. Character: Smooth.
    TransistorLadder,
    /// Diode ladder, acid style: coupled stages, an asymmetric shaper, and
    /// the bass thinning with resonance. Character: Smooth.
    DiodeLadder,
    /// Clean diode ladder: coupled stages, the pass band compensated, no
    /// shaper. Character: Smooth.
    CleanLadder,
    /// Driven ladder: a transistor ladder pushed into its stages.
    /// Character: overdrive.
    DrivenLadder,
    /// Pole-zero: eight drawn points per state, four states morphed by X
    /// and Y ([`pole_zero`]). Character: Smooth.
    PoleZero,
    /// Comb II: a feedback comb with a second tap. Character: the second
    /// frequency.
    Comb2,
    /// Expander multimode: a four-pole ladder whose taps mix into
    /// low-pass, notch and high-pass. Character: the blend.
    ExpanderMultimode,
    /// Expander band-pass: the four-pole ladder's band-pass mix. Character:
    /// none.
    ExpanderBandPass,
    /// K35: Sallen–Key low-pass and high-pass with a saturating feedback
    /// amplifier. Character: LP/HP.
    K35,
}

/// Which group of the type menu a type is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterGroup {
    /// The state-variable and ladder types at each slope.
    Normal,
    /// The later group: morphing, DJ, diffuser, ladder characters,
    /// pole-zero, Comb II, expander and K35.
    Extended,
}

/// What a type's Character control does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Character {
    /// Nothing.
    None,
    /// Saturation in the resonance path ([`VoiceParams::saturation`]),
    /// which tames resonance.
    Saturation,
    /// Morphs the response: low-pass at 0, notch at 0.5, high-pass at 1.
    Morph,
    /// How many stages: 1 at 0 to the most at 1.
    Stages,
    /// Smooth: lags the cutoff (for the pole-zero filter, X and Y too),
    /// from none at 0 to a 100 ms time constant at 1.
    Smooth,
    /// Overdrive into the filter's stages.
    Overdrive,
    /// A second frequency, from the cutoff's at 0 to two octaves above at 1.
    SecondFrequency,
    /// Low-pass below 0.5, high-pass from it up.
    LowHigh,
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
/// The reference pitch's default: A4 in hertz.
pub const DEFAULT_REFERENCE: f32 = 440.0;

impl FilterType {
    /// Every type, in the order of the table in the module documentation.
    pub const ALL: [Self; 30] = [
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
        Self::BuzzMorph,
        Self::DjSweep,
        Self::Diffuser,
        Self::TransistorLadder,
        Self::DiodeLadder,
        Self::CleanLadder,
        Self::DrivenLadder,
        Self::PoleZero,
        Self::Comb2,
        Self::ExpanderMultimode,
        Self::ExpanderBandPass,
        Self::K35,
    ];

    /// A short name: `svf-lp12`, `ladder-24` and so on.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lp6 => "svf-lp6",
            Self::Lp12 => "svf-lp12",
            Self::Lp18 => "svf-lp18",
            Self::Lp24 => "svf-lp24",
            Self::Hp6 => "svf-hp6",
            Self::Hp12 => "svf-hp12",
            Self::Hp18 => "svf-hp18",
            Self::Hp24 => "svf-hp24",
            Self::Bp12 => "svf-bp12",
            Self::Bp24 => "svf-bp24",
            Self::Peak12 => "svf-peak12",
            Self::Peak24 => "svf-peak24",
            Self::Notch12 => "svf-notch12",
            Self::Notch24 => "svf-notch24",
            Self::Ladder6 => "ladder-6",
            Self::Ladder12 => "ladder-12",
            Self::Ladder18 => "ladder-18",
            Self::Ladder24 => "ladder-24",
            Self::BuzzMorph => "buzz-morph",
            Self::DjSweep => "dj-sweep",
            Self::Diffuser => "diffuser",
            Self::TransistorLadder => "transistor-ladder",
            Self::DiodeLadder => "diode-ladder",
            Self::CleanLadder => "clean-ladder",
            Self::DrivenLadder => "driven-ladder",
            Self::PoleZero => "pole-zero",
            Self::Comb2 => "comb-2",
            Self::ExpanderMultimode => "expander-mm",
            Self::ExpanderBandPass => "expander-bp",
            Self::K35 => "k35",
        }
    }

    /// Its slope in decibels per octave; 0 for the types that have none
    /// (the diffuser, pole-zero and Comb II).
    #[must_use]
    pub const fn slope(self) -> u32 {
        match self {
            Self::Lp6 | Self::Hp6 | Self::Ladder6 => 6,
            Self::Lp12
            | Self::Hp12
            | Self::Bp12
            | Self::Peak12
            | Self::Notch12
            | Self::Ladder12
            | Self::BuzzMorph
            | Self::DjSweep
            | Self::K35 => 12,
            Self::Lp18 | Self::Hp18 | Self::Ladder18 => 18,
            Self::Lp24
            | Self::Hp24
            | Self::Bp24
            | Self::Peak24
            | Self::Notch24
            | Self::Ladder24
            | Self::TransistorLadder
            | Self::DiodeLadder
            | Self::CleanLadder
            | Self::DrivenLadder
            | Self::ExpanderMultimode
            | Self::ExpanderBandPass => 24,
            Self::Diffuser | Self::PoleZero | Self::Comb2 => 0,
        }
    }

    /// Which group of the type menu it's in.
    #[must_use]
    pub const fn group(self) -> FilterGroup {
        match self {
            Self::Lp6
            | Self::Lp12
            | Self::Lp18
            | Self::Lp24
            | Self::Hp6
            | Self::Hp12
            | Self::Hp18
            | Self::Hp24
            | Self::Bp12
            | Self::Bp24
            | Self::Peak12
            | Self::Peak24
            | Self::Notch12
            | Self::Notch24
            | Self::Ladder6
            | Self::Ladder12
            | Self::Ladder18
            | Self::Ladder24 => FilterGroup::Normal,
            _ => FilterGroup::Extended,
        }
    }

    /// What its Character control ([`VoiceParams::character`], or
    /// [`VoiceParams::saturation`] for the normal group) does.
    #[must_use]
    pub const fn character(self) -> Character {
        match self {
            Self::BuzzMorph | Self::ExpanderMultimode => Character::Morph,
            Self::DjSweep | Self::ExpanderBandPass => Character::None,
            Self::Diffuser => Character::Stages,
            Self::TransistorLadder | Self::DiodeLadder | Self::CleanLadder | Self::PoleZero => {
                Character::Smooth
            }
            Self::DrivenLadder => Character::Overdrive,
            Self::Comb2 => Character::SecondFrequency,
            Self::K35 => Character::LowHigh,
            _ => Character::Saturation,
        }
    }

    /// Whether resonance does anything (not for the 6 dB state-variable
    /// types, which are one-pole stages, or the pole-zero filter, whose
    /// points carry their own Q).
    #[must_use]
    pub const fn has_resonance(self) -> bool {
        !matches!(self, Self::Lp6 | Self::Hp6 | Self::PoleZero)
    }

    /// Whether high resonance makes it oscillate on its own.
    #[must_use]
    pub const fn self_oscillates(self) -> bool {
        self.has_resonance()
            && !matches!(
                self,
                Self::Peak12
                    | Self::Peak24
                    | Self::Notch12
                    | Self::Notch24
                    | Self::DjSweep
                    | Self::Diffuser
                    | Self::Comb2
            )
    }

    /// The resonance law: for the state-variable types (and the buzz
    /// morph, DJ sweep and diffuser stages), the resonant stage's damping
    /// (`k`, 1/Q) at `resonance` from 0 to 1; for the ladders and
    /// expanders, the feedback gain; for the K35, its feedback amplifier's
    /// gain; for Comb II, the fraction fed back. Without saturation, this
    /// is the value the analogue prototype has. The 6 dB state-variable
    /// types and the pole-zero filter have none and return 0.
    #[must_use]
    pub fn feedback(self, resonance: f32) -> f32 {
        let r = resonance.clamp(0.0, 1.0);
        match self {
            Self::Lp6 | Self::Hp6 | Self::PoleZero => 0.0,
            Self::Ladder6
            | Self::Ladder12
            | Self::Ladder18
            | Self::Ladder24
            | Self::TransistorLadder
            | Self::DrivenLadder
            | Self::ExpanderMultimode
            | Self::ExpanderBandPass => ladder_k(r),
            Self::DiodeLadder | Self::CleanLadder => {
                r * extended::DIODE_K * extended::DIODE_OVERSHOOT
            }
            Self::BuzzMorph => svf_k(K_BUTTERWORTH_2, f32::NEG_INFINITY, r),
            Self::DjSweep => extended::dj_k(r),
            Self::Diffuser => extended::diffuser_k(r),
            Self::Comb2 => r * extended::COMB_MAX,
            Self::K35 => r * extended::K35_MAX,
            _ => svf_k(self.damping(), self.damping_floor(), r),
        }
    }

    /// The resonant stage's damping at resonance 0.
    const fn damping(self) -> f32 {
        match self {
            Self::Lp12 | Self::Hp12 => K_BUTTERWORTH_2,
            Self::Lp18 | Self::Hp18 => K_BUTTERWORTH_3,
            Self::Lp24 | Self::Hp24 => K_BUTTERWORTH_4[1],
            Self::Bp12 | Self::Bp24 => K_BAND,
            Self::Peak12 | Self::Peak24 | Self::Notch12 | Self::Notch24 => K_PEAK,
            _ => 0.0,
        }
    }

    /// The least damping it reaches: below zero for the self-oscillating
    /// types.
    const fn damping_floor(self) -> f32 {
        match self {
            Self::Peak12 | Self::Peak24 | Self::Notch12 | Self::Notch24 => K_PEAK_MIN,
            _ => f32::NEG_INFINITY,
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
    /// high levels.
    pub saturation: f32,
    /// Stereo cutoff offset in semitones: the right channel's cutoff rises
    /// by this and the left's falls. No effect on one channel.
    pub stereo_offset: f32,
    /// Filtered against dry, 0 (dry) to 1 (filtered).
    pub mix: f32,
    /// Output gain, linear, after the mix.
    pub level: f32,
    /// Character, 0 to 1, for the extended group: what it does depends on
    /// the type ([`FilterType::character`]). The normal group's Character
    /// is [`saturation`](Self::saturation).
    pub character: f32,
    /// The pole-zero filter's X, 0 to 1.
    pub morph_x: f32,
    /// The pole-zero filter's Y, 0 to 1.
    pub morph_y: f32,
}

impl Default for VoiceParams {
    /// Resonance 10 %, no drive or saturation, no offset, fully filtered,
    /// unity level, Character 0, X and Y at 0 (state 1).
    fn default() -> Self {
        Self {
            resonance: 0.1,
            drive: 0.0,
            saturation: 0.0,
            stereo_offset: 0.0,
            mix: 1.0,
            level: 1.0,
            character: 0.0,
            morph_x: 0.0,
            morph_y: 0.0,
        }
    }
}

/// The extended group's settings a bank smooths towards, for one voice.
fn extra_targets_of(params: &VoiceParams) -> [f32; extended::EXTRA] {
    let unit = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    let mut t = [0.0; extended::EXTRA];
    t[extended::CHARACTER] = unit(params.character);
    t[extended::MORPH_X] = unit(params.morph_x);
    t[extended::MORPH_Y] = unit(params.morph_y);
    t
}

/// Indices of the smoothed settings.
const RES: usize = 0;
const DRIVE: usize = 1;
const DRIVE_GAIN: usize = 2;
const INV_KNEE: usize = 3;
const OFFSET: usize = 4;
const MIX: usize = 5;
const LEVEL: usize = 6;
/// How many smoothed settings each slot has.
const PARAMS: usize = 7;
/// How many state variables each slot has.
const STATES: usize = 6;
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
    /// Per slot.
    state: [Vec<f32>; STATES],
    /// The extended group's smoothed settings: per voice, then per slot.
    extra_targets: [Vec<f32>; extended::EXTRA],
    extra_smoothed: [Vec<f32>; extended::EXTRA],
    /// The extended group's state, per slot.
    extra_state: [Vec<f32>; extended::X_STATES],
    /// Comb II's delay lines: `channels × delay_rows` rows of `stride`
    /// values, each voice's line a column.
    delay: Vec<f32>,
    /// Rows per channel, a power of two.
    delay_rows: usize,
    /// The row the next frame writes.
    delay_at: usize,
    /// The longest delay in samples.
    max_delay: f32,
    /// The pole-zero design, and the form the kernel runs.
    pole_zero_design: PoleZeroDesign,
    pole_zero: pole_zero::Table,
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
        let extra = extra_targets_of(&VoiceParams::default());
        let max_delay = (sample_rate * extended::MAX_DELAY_SECONDS).ceil();
        let delay_rows = (max_delay as usize + 4).next_power_of_two();
        let design = PoleZeroDesign::default();
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
            state: std::array::from_fn(|_| vec![0.0; channels * stride]),
            extra_targets: std::array::from_fn(|i| vec![extra[i]; stride]),
            extra_smoothed: std::array::from_fn(|i| vec![extra[i]; channels * stride]),
            extra_state: std::array::from_fn(|_| vec![0.0; channels * stride]),
            delay: vec![0.0; channels * delay_rows * stride],
            delay_rows,
            delay_at: 0,
            max_delay,
            pole_zero_design: design,
            pole_zero: pole_zero::Table::of(&design),
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

    /// Sets the type. A change clears every voice's filter state, as the
    /// types' states don't carry over.
    pub fn set_type(&mut self, kind: FilterType) {
        if kind != self.kind {
            self.kind = kind;
            self.clear_state();
        }
    }

    /// Sets the pole-zero filter's design, which every voice shares. It
    /// takes effect at once, without clearing state; allocates nothing.
    pub fn set_pole_zero(&mut self, design: &PoleZeroDesign) {
        self.pole_zero_design = *design;
        self.pole_zero = pole_zero::Table::of(design);
    }

    /// The pole-zero filter's design.
    #[must_use]
    pub fn pole_zero(&self) -> &PoleZeroDesign {
        &self.pole_zero_design
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

    /// Sets the sample rate. Filter states carry on. Comb II's delay lines
    /// were sized for the rate the bank was made at, so at a higher rate
    /// its lowest frequency rises in proportion.
    ///
    /// # Panics
    ///
    /// If `sample_rate` isn't positive.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        assert!(sample_rate > 0.0, "the sample rate must be positive");
        self.rate = sample_rate;
        self.max_delay = (sample_rate * extended::MAX_DELAY_SECONDS)
            .ceil()
            .min((self.delay_rows - 4) as f32);
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
        for (i, value) in extra_targets_of(params).into_iter().enumerate() {
            self.extra_targets[i][voice] = value;
        }
    }

    /// Starts voice `voice` afresh, as at a note-on: its filter state
    /// cleared and its settings at `params` at once, without gliding.
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
            for i in 0..extended::EXTRA {
                self.extra_smoothed[i][slot] = self.extra_targets[i][voice];
            }
            for s in &mut self.extra_state {
                s[slot] = 0.0;
            }
            if self.kind == FilterType::Comb2 {
                let rows = self.delay_rows;
                for row in c * rows..(c + 1) * rows {
                    self.delay[row * self.stride + voice] = 0.0;
                }
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
            for i in 0..extended::EXTRA {
                let (to, from) = (&mut self.extra_smoothed[i], &self.extra_targets[i]);
                to[c * self.stride..(c + 1) * self.stride].copy_from_slice(from);
            }
        }
    }

    /// Clears the filter state; Comb II's delay lines only when Comb II is
    /// the type, as no other type reads them.
    fn clear_state(&mut self) {
        for s in self.state.iter_mut().chain(&mut self.extra_state) {
            s.fill(0.0);
        }
        if self.kind == FilterType::Comb2 {
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
        self.dispatch(simd.then(simd::level), pitch, None, audio, frames);
    }

    /// As [`process`](Self::process), with the pole-zero filter's X and Y
    /// modulated at audio rate: `x` and `y` are laid out as `pitch`, and
    /// each value is added to its voice's [`VoiceParams::morph_x`] or
    /// [`VoiceParams::morph_y`], the sum clamped to 0 to 1. Other types
    /// ignore them.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process), or if `x` or `y` isn't as long as
    /// `pitch`.
    pub fn process_xy(&mut self, pitch: &[f32], x: &[f32], y: &[f32], audio: &mut [f32]) {
        assert!(
            x.len() == pitch.len() && y.len() == pitch.len(),
            "X and Y hold as many values as the pitch buffer"
        );
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
        self.dispatch(simd.then(simd::level), pitch, Some((x, y)), audio, frames);
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
        self.dispatch(Some(level), pitch, None, audio, frames)
    }

    /// Runs the block with this bank's type, scalar or at `level`. Each type
    /// is a kernel of its own, so each compiled copy holds one structure.
    fn dispatch(
        &mut self,
        level: Option<Level>,
        pitch: &[f32],
        xy: Option<(&[f32], &[f32])>,
        audio: &mut [f32],
        frames: usize,
    ) -> bool {
        fn go<T: Topology>(
            bank: &mut FilterBank,
            level: Option<Level>,
            pitch: &[f32],
            audio: &mut [f32],
            frames: usize,
        ) -> bool {
            let run = Run::<T> {
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
        let run = match self.kind {
            FilterType::Lp6 => go::<OnePole<false>>,
            FilterType::Hp6 => go::<OnePole<true>>,
            FilterType::Lp12 => go::<Svf12<LOW>>,
            FilterType::Hp12 => go::<Svf12<HIGH>>,
            FilterType::Bp12 => go::<Svf12<BAND>>,
            FilterType::Peak12 => go::<Svf12<PEAK>>,
            FilterType::Notch12 => go::<Svf12<NOTCH>>,
            FilterType::Lp18 => go::<Svf18<false>>,
            FilterType::Hp18 => go::<Svf18<true>>,
            FilterType::Lp24 => go::<Svf24<LOW>>,
            FilterType::Hp24 => go::<Svf24<HIGH>>,
            FilterType::Bp24 => go::<Svf24<BAND>>,
            FilterType::Peak24 => go::<Svf24<PEAK>>,
            FilterType::Notch24 => go::<Svf24<NOTCH>>,
            FilterType::Ladder6 => go::<Ladder<1>>,
            FilterType::Ladder12 => go::<Ladder<2>>,
            FilterType::Ladder18 => go::<Ladder<3>>,
            FilterType::Ladder24 => go::<Ladder<4>>,
            _ => return extended::dispatch(self, level, pitch, xy, audio, frames),
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
struct Run<'a, T> {
    bank: &'a mut FilterBank,
    pitch: &'a [f32],
    audio: &'a mut [f32],
    frames: usize,
    topology: PhantomData<T>,
}

impl<T: Topology> Kernel for Run<'_, T> {
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

impl<T: Topology> Run<'_, T> {
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
                let mut state: [L; STATES] =
                    std::array::from_fn(|i| L::load(&bank.state[i][slot..]));
                let block = Block {
                    pitch: self.pitch,
                    frames: self.frames,
                    stride: bank.stride,
                    frame_stride: bank.channels * bank.stride,
                    voice,
                    slot,
                };
                block.run::<L, T>(&shared, &targets, &mut params, &mut state, self.audio);
                for (i, value) in params.iter().enumerate() {
                    value.store(&mut bank.smoothed[i][slot..]);
                }
                for (i, value) in state.iter().enumerate() {
                    value.store(&mut bank.state[i][slot..]);
                }
            }
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
}

impl Block<'_> {
    /// Runs one group of lanes of one channel through the block.
    #[inline(always)]
    fn run<L: Lane, T: Topology>(
        &self,
        shared: &Shared<L>,
        targets: &[L; PARAMS],
        params: &mut [L; PARAMS],
        state: &mut [L; STATES],
        audio: &mut [f32],
    ) {
        let one = L::splat(1.0);
        for f in 0..self.frames {
            let pitch = L::load(&self.pitch[f * self.stride + self.voice..]);
            let at = f * self.frame_stride + self.slot;
            let x = L::load(&audio[at..]);
            for (p, t) in params.iter_mut().zip(targets) {
                *p = (*t - *p).fma(shared.alpha, *p);
            }
            // Cutoff: the pitch, offset for this channel, to the
            // prewarped integrator gain g = tan(π f / rate).
            let pitch = params[OFFSET].fma(shared.sign, pitch);
            let octaves = (pitch - L::splat(69.0)) * L::splat(1.0 / 12.0);
            let ratio = (exp2(octaves) * shared.ref_over_rate)
                .min_(L::splat(TAN_LIMIT / std::f32::consts::PI));
            let g = tan(ratio * L::splat(std::f32::consts::PI));
            // Drive: gain into the soft clipper, blended in by the amount.
            let u = x * params[DRIVE_GAIN];
            let clipped = soft_clip(u * shared.clip_in) * shared.clip_out;
            let driven = (clipped - u).fma(params[DRIVE], u);
            let y = T::tick(state, driven, g, params[RES], params[INV_KNEE]);
            let out = y.fma(params[MIX], x * (one - params[MIX])) * params[LEVEL];
            out.store(&mut audio[at..]);
        }
    }
}

/// One filter structure's sample: the input, the integrator gain, the
/// resonance (0 to 1) and the saturator's inverse knee in; the output out.
trait Topology {
    fn tick<L: Lane>(state: &mut [L; STATES], x: L, g: L, r: L, inv_knee: L) -> L;
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

/// The 6 dB types: a one-pole stage. State: 0.
struct OnePole<const HP: bool>;

impl<const HP: bool> Topology for OnePole<HP> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; STATES], x: L, g: L, _r: L, _inv_knee: L) -> L {
        let gg = g / (L::splat(1.0) + g);
        let low = one_pole(&mut state[0], x, gg);
        if HP { x - low } else { low }
    }
}

/// The 12 dB types: one resonant state-variable stage. State: 0 and 1 the
/// stage, 2 its previous band output.
struct Svf12<const OUT: u8>;

impl<const OUT: u8> Topology for Svf12<OUT> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; STATES], x: L, g: L, r: L, inv_knee: L) -> L {
        let (k0, floor) = match OUT {
            LOW | HIGH => (K_BUTTERWORTH_2, f32::NEG_INFINITY),
            BAND => (K_BAND, f32::NEG_INFINITY),
            _ => (K_PEAK, K_PEAK_MIN),
        };
        let k = saturated_k(k0, svf_k(k0, floor, r), state[2], inv_knee);
        let [s0, s1, s2, ..] = state;
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
}

/// The 18 dB types: a one-pole stage, then a resonant state-variable stage
/// of the same kind. State: 0 to 2 as [`Svf12`], 3 the one-pole stage.
struct Svf18<const HP: bool>;

impl<const HP: bool> Topology for Svf18<HP> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; STATES], x: L, g: L, r: L, inv_knee: L) -> L {
        let gg = g / (L::splat(1.0) + g);
        let first = one_pole(&mut state[3], x, gg);
        let first = if HP { x - first } else { first };
        let k0 = K_BUTTERWORTH_3;
        let k = saturated_k(k0, svf_k(k0, f32::NEG_INFINITY, r), state[2], inv_knee);
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

impl<const OUT: u8> Topology for Svf24<OUT> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; STATES], x: L, g: L, r: L, inv_knee: L) -> L {
        let (k0, floor, k_first) = match OUT {
            LOW | HIGH => (K_BUTTERWORTH_4[1], f32::NEG_INFINITY, K_BUTTERWORTH_4[0]),
            BAND => (K_BAND, f32::NEG_INFINITY, K_BAND),
            PEAK => (K_PEAK, K_PEAK_MIN, K_BAND),
            _ => (K_PEAK, K_PEAK_MIN, 0.0),
        };
        let k = saturated_k(k0, svf_k(k0, floor, r), state[2], inv_knee);
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

impl<const TAP: usize> Topology for Ladder<TAP> {
    #[inline(always)]
    fn tick<L: Lane>(state: &mut [L; STATES], x: L, g: L, r: L, inv_knee: L) -> L {
        let one = L::splat(1.0);
        let gain = saturation_gain(state[4], inv_knee);
        let k = ladder_k(r) * gain;
        let gg = g / (one + g);
        let h = one - gg;
        // Each stage's output is gg × its input + h × its state.
        let b = [h * state[0], h * state[1], h * state[2], h * state[3]];
        let sum = ((b[0] * gg + b[1]) * gg + b[2]).fma(gg, b[3]);
        let gg2 = gg * gg;
        let gg4 = gg2 * gg2;
        let input = x * k.fma(L::splat(LADDER_COMPENSATION), one);
        let y4 = gg4.fma(input, sum) / k.fma(gg4, one);
        let mut u = input - k * y4;
        let mut taps = [u; 4];
        for (i, tap) in taps.iter_mut().enumerate() {
            let y = gg.fma(u, b[i]);
            state[i] = y + y - state[i];
            *tap = y;
            u = y;
        }
        state[4] = taps[3];
        taps[TAP - 1]
    }
}
