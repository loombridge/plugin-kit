//! Shapers: the wavetable oscillator's two slots that reshape its wave,
//! plugged into its phase and wave stages (engine-core section 3).
//!
//! | Item | What it is |
//! |---|---|
//! | [`ShapedOscillator`] | An [`Oscillator`] with two shaper slots: renders them in series, oversampled where a mode needs it, every voice and unison copy in SIMD lanes |
//! | [`Shapers`] | The two [`Slot`]s (mode, amount, character), the [`RemapCurve`], the oversampling factor, swapping, and the [`preview`](Shapers::preview) an editor draws |
//! | [`Mode`] | Every mode this package builds, in the order the menu lists them |
//! | [`Source`] | The signal an inter-oscillator mode reads: another oscillator, the noise or sub oscillator, a filter, or the shaped oscillator itself |
//! | [`ShaperInputs`] | Audio-rate amount modulation, per slot, and the source signal each slot's mode reads |
//!
//! # Where each mode acts
//!
//! The **phase stage** bends where the table is read: Sync, the pinches
//! and leans, Pulse width, Flip, Mirror, the remaps, Quantise, Odd/even,
//! FM and phase distortion. The **wave stage** bends the value read:
//! Low-pass, High-pass, the fourteen distortions, AM and Ring.
//!
//! The inter-oscillator modes (FM, phase distortion, AM and Ring) read a
//! signal at audio rate instead of a map of the phase: the caller renders
//! the source — another main oscillator, the noise or sub oscillator, or
//! a filter — and hands it to [`ShapedOscillator::process`] per slot, one
//! value a host frame and lane (`ShaperInputs::sources`), as the source's
//! wave *before* its Level, so a source at Level 0 still modulates (R12).
//! FM and phase distortion bend the phase, AM and Ring the value; an
//! oversampled render reads the source between its host frames.
//!
//! The two slots run in series, slot 1 first (engine-core R22): slot 2
//! reshapes the wave slot 1 made. In the phase stage that means slot 2's
//! map is applied to the running phase first and slot 1's to the result
//! (the wave slot 1 shaped, read where slot 2 says); in the wave stage,
//! slot 1's curve or filter runs first. A memoryless distortion and a
//! phase map give the same wave in either order, so the only pairing that
//! differs from a literal series is a filter in slot 1 with a phase mode
//! in slot 2: the filter then still runs on the phase-shaped wave.
//!
//! # Aliasing
//!
//! Every phase mode tells the oscillator its steepest slope, and the
//! oscillator reads a mipmap level band-limited for that speed, so a mode
//! that reads the table faster than the note (Sync, a strong pinch) never
//! reads harmonics above the render rate's Nyquist. The phase modes and
//! the distortions render at [`Shapers::oversampling`] times the host rate
//! (4 by default), because even a level of the fundamental alone, read 9 or
//! 16 times faster at a high note, would fold at the host rate. The modes
//! with edges (Sync's restart, a pulse's end, Flip, Mirror's turn,
//! Quantise's steps) also crossfade between the two sides of each edge
//! over [`TRANSITION`] host samples. A distortion is given a read
//! band-limited to the host rate's Nyquist, and runs by the first-order
//! antiderivative method, which lowers what folds by a further 6 dB an
//! octave. Oversampled renders come back to the host rate through a
//! cascade of IIR half-band filters that keep everything below 0.45 of the
//! rate and stop what would fold into it by 100 dB, so aliasing is
//! measured below 20 kHz. Odd/even and the filters are linear in the wave
//! and run at the host rate.
//!
//! The inter-oscillator modes render oversampled too, and FM and phase
//! distortion add their modulation to the slope the range tells the level
//! to pick, from the source's peak over the stretch (FM) or its steepest
//! step (phase distortion), so the read stays band-limited while the
//! source moves the phase at audio rate.
//!
//! # Laws to be matched by measurement
//!
//! Every amount law here is a first law, chosen to sound right and to stay
//! band-limited, which measurement against the reference instrument (R7)
//! replaces. `a` is the amount from 0 to 1 (0 to 100 %).
//!
//! | Mode | Amount | Character |
//! |---|---|---|
//! | Sync | the internal oscillator runs at [`SYNC_MAX_RATIO`]^a times the note (1× to 16×, four octaves), restarting with each cycle | softness: the crossfade at the restart widens from the shortest to the whole cycle (0 hard, 1 soft) |
//! | Pinch in, Pinch out, Pinch | a hyperbolic circle map at twice the cycle's rate, strength c = ±[`PINCH_MAX`] a (Pinch: c = [`PINCH_MAX`] (2a − 1), neutral at 50 %); content flows towards the quarter points (in) or away (out), the steepest slope (1 + \|c\|) / (1 − \|c\|) | |
//! | Lean right, Lean left, Lean | a parabolic circle map fixing the cycle's start, strength β = ∓[`LEAN_MAX`] a (Lean: β = [`LEAN_MAX`] (1 − 2a), neutral at 50 %); every point of the cycle moves later (right) or earlier (left) | |
//! | Pulse width | the cycle squeezed into its first 1 − [`PULSE_MAX`] a, then the wave's start held | |
//! | Flip | the polarity inverted from 1 − a to the end of the cycle | |
//! | Mirror | the first half of the wave forward then backward, each half leaned right by β = [`LEAN_MAX`] a; always audible | |
//! | Remap drawn, mirrored, sine, ×4 | the phase blended from identity (0 %) to the curve (100 %): the drawn curve over the cycle, over each half (the second point-mirrored), the fixed curve u − sin(2πu) / 2π, the drawn curve over each quarter | |
//! | Quantise | [`QUANTISE_MIN_STEPS`] × 1024^(1 − a) steps a cycle: 2,048 at 0 % to 2 at 100 % | |
//! | Odd/even | odd harmonics only at 0 %, unchanged at 50 %, even only at 100 %, linear either side | |
//! | Low-pass | 12 dB an octave, cutoff harmonic 2^(11 (1 − a)) of the note (2,048 to 1) | resonance: damping from √2 (Butterworth) to 0.21 |
//! | High-pass | 12 dB an octave, cutoff harmonic 2^(12a − 2) of the note (¼ to 1,024) | resonance, as Low-pass |
//! | Distortions | drive from 0 to [`DRIVE_MAX_DB`] dB ([`FOLD_DRIVE_MAX_DB`] for the folds) into the mode's curve, listed below; Rectify blends to full-wave rectification instead | |
//! | FM | the carrier's frequency deviates by [`FM_MAX_HZ`] a⁴ times the source, in hertz, whatever the carrier's pitch (roughly ±134 Hz at 20 %, ±42 Hz at 15 %); Thru-zero passes through 0 Hz and phase runs backwards, Linear clamps there, Exponential scales the source exponentially about its own value | |
//! | Phase distortion | the read phase moves by [`PD_MAX_CYCLES`] a times the source, in cycles | |
//! | AM | the value blended from unmodulated to (1 + source) / 2, so at 100 % the envelope follows the source and the carrier stays | |
//! | Ring | the value blended from unmodulated to the source itself, so at 100 % the carrier is gone | |
//!
//! The other modes ignore character for now.

mod curve;
mod halfband;
mod lanes;
mod phase;
mod wave;

use lspk_dsp::simd::{self, F32s, Level as SimdLevel};

pub use curve::{CURVE_RESOLUTION, MAX_CURVE_STRETCH, RemapCurve};
pub use phase::TRANSITION;

use crate::osc::{Inputs, LaneReads, Onset, Oscillator, PhaseStage, Reads, SUB_BLOCK, WaveStage};
use crate::{CYCLE, Table};
use halfband::Decimator;
use lanes::{Lane, V, clamp};
use phase::{ReadList, SlotView};
use wave::Svf;

/// Shaper slots per oscillator (engine-core R21).
pub const SLOTS: usize = 2;

/// Sync's highest ratio of internal to played pitch, at 100 %.
pub const SYNC_MAX_RATIO: f32 = 16.0;

/// The pinch map's strength at 100 %: the steepest slope is 9.
pub const PINCH_MAX: f32 = 0.8;

/// The lean map's strength at 100 %: the steepest slope is about 5.
pub const LEAN_MAX: f32 = 1.8;

/// How much of the cycle Pulse width squeezes out at 100 %.
pub const PULSE_MAX: f32 = 0.95;

/// Quantise's steps a cycle at 0 %.
pub const QUANTISE_MAX_STEPS: f32 = 2048.0;

/// Quantise's steps a cycle at 100 %.
pub const QUANTISE_MIN_STEPS: f32 = 2.0;

/// The distortions' drive at 100 %, in dB.
pub const DRIVE_MAX_DB: f32 = 30.0;

/// Fold's and Sine fold's drive at 100 %, in dB: about eight folds a
/// half-cycle of a full-scale wave.
pub const FOLD_DRIVE_MAX_DB: f32 = 18.0;

/// FM's scale: the carrier's frequency deviates by this times the amount⁴
/// times the source, in hertz, whatever the carrier's pitch. A first law:
/// the reference's deviation is about ±134 Hz at 20 % and ±42 Hz at 15 %
/// (engine-core R28; R7's measurement replaces it).
pub const FM_MAX_HZ: f32 = 83_750.0;

/// Phase distortion's scale: the read phase moves by this many cycles at
/// 100 %, times the source.
pub const PD_MAX_CYCLES: f32 = 0.5;

/// How many frames, at the render rate, an oversampled render takes at a
/// time.
const RENDER_CHUNK: usize = 128;

/// The FM type a mode reads its source with (engine-core R28, R29).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FmType {
    /// Passes through 0 Hz: the phase runs backwards below it.
    ThruZero,
    /// Scales the source exponentially about its own value.
    Exponential,
    /// Clamps at 0 Hz.
    Linear,
}

impl FmType {
    /// Its name, as a menu shows it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ThruZero => "FM",
            Self::Exponential => "FM exp",
            Self::Linear => "FM lin",
        }
    }
}

/// The signal an inter-oscillator mode reads (engine-core R27): the
/// caller renders it and hands it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// The first of the two main oscillators other than the shaped one.
    Osc1,
    /// The second of them.
    Osc2,
    /// The noise oscillator.
    Noise,
    /// The sub oscillator.
    Sub,
    /// Filter 1.
    Filter1,
    /// Filter 2.
    Filter2,
    /// The shaped oscillator's own last value (phase distortion only).
    Own,
}

impl Source {
    /// Its name, as a menu shows it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Osc1 => "1",
            Self::Osc2 => "2",
            Self::Noise => "noise",
            Self::Sub => "sub",
            Self::Filter1 => "filter 1",
            Self::Filter2 => "filter 2",
            Self::Own => "self",
        }
    }
}

/// The inter-oscillator modulation a mode is: what it does with its
/// source (engine-core R27).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Modulated {
    /// Frequency modulation.
    Fm(FmType),
    /// The read phase moved by the source.
    PhaseDistortion,
    /// The value blended towards the source's envelope.
    Am,
    /// The value blended towards the source itself.
    Ring,
}

/// A shaper slot's mode (engine-core section 3), in the order the menu
/// lists them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Does nothing.
    #[default]
    Off,
    /// Hard to soft sync to an internal oscillator.
    Sync,
    /// Each half-cycle bent inward.
    PinchIn,
    /// Each half-cycle bent outward.
    PinchOut,
    /// Out below 50 %, in above.
    Pinch,
    /// The cycle pushed to the left, the rest held.
    PulseWidth,
    /// The whole cycle bent right.
    LeanRight,
    /// The whole cycle bent left.
    LeanLeft,
    /// Left below 50 %, right above.
    Lean,
    /// The polarity flips part-way through the cycle.
    Flip,
    /// The second half mirrors the first.
    Mirror,
    /// The phase through the drawn curve.
    RemapDrawn,
    /// The drawn curve over each half.
    RemapMirrored,
    /// A fixed sinusoidal curve.
    RemapSine,
    /// The drawn curve over each quarter.
    RemapQuad,
    /// The cycle's resolution reduced.
    Quantise,
    /// Odd harmonics only, through unchanged, to even only.
    OddEven,
    /// A low-pass filter on the wave.
    LowPass,
    /// A high-pass filter on the wave.
    HighPass,
    /// A valve's saturation.
    Valve,
    /// A cubic soft clipper.
    SoftClip,
    /// A hard clipper.
    HardClip,
    /// One diode's clipping, asymmetric.
    Diode1,
    /// Two diodes' clipping, symmetric.
    Diode2,
    /// Linear wavefolding.
    Fold,
    /// Sinusoidal wavefolding.
    SineFold,
    /// Squared towards zero.
    ZeroSquare,
    /// Asymmetric saturation.
    Asymmetric,
    /// Full-wave rectification.
    Rectify,
    /// A sine curve as the shaper.
    SineShaper,
    /// A pedal's clipping.
    Pedal,
    /// Tape saturation.
    Tape,
    /// Gentle saturation.
    SoftSaturate,
    /// Through-zero FM from the first other oscillator (R27, R28).
    Fm1,
    /// Thru-zero FM from the second other oscillator.
    Fm2,
    /// Thru-zero FM from the noise oscillator.
    FmNoise,
    /// Thru-zero FM from the sub oscillator.
    FmSub,
    /// Thru-zero FM from Filter 1.
    FmFilter1,
    /// Thru-zero FM from Filter 2.
    FmFilter2,
    /// Exponential FM from the first other oscillator.
    FmExp1,
    /// Exponential FM from the second other oscillator.
    FmExp2,
    /// Exponential FM from the noise oscillator.
    FmExpNoise,
    /// Exponential FM from the sub oscillator.
    FmExpSub,
    /// Exponential FM from Filter 1.
    FmExpFilter1,
    /// Exponential FM from Filter 2.
    FmExpFilter2,
    /// Linear FM from the first other oscillator.
    FmLin1,
    /// Linear FM from the second other oscillator.
    FmLin2,
    /// Linear FM from the noise oscillator.
    FmLinNoise,
    /// Linear FM from the sub oscillator.
    FmLinSub,
    /// Linear FM from Filter 1.
    FmLinFilter1,
    /// Linear FM from Filter 2.
    FmLinFilter2,
    /// Phase distortion from the first other oscillator.
    Pd1,
    /// Phase distortion from the second other oscillator.
    Pd2,
    /// Phase distortion from the noise oscillator.
    PdNoise,
    /// Phase distortion from the sub oscillator.
    PdSub,
    /// Phase distortion from Filter 1.
    PdFilter1,
    /// Phase distortion from Filter 2.
    PdFilter2,
    /// Phase distortion from the oscillator's own last value.
    PdSelf,
    /// AM from the first other oscillator.
    Am1,
    /// AM from the second other oscillator.
    Am2,
    /// AM from the noise oscillator.
    AmNoise,
    /// AM from the sub oscillator.
    AmSub,
    /// AM from Filter 1.
    AmFilter1,
    /// AM from Filter 2.
    AmFilter2,
    /// Ring modulation from the first other oscillator.
    Rm1,
    /// Ring modulation from the second other oscillator.
    Rm2,
    /// Ring modulation from the noise oscillator.
    RmNoise,
    /// Ring modulation from the sub oscillator.
    RmSub,
    /// Ring modulation from Filter 1.
    RmFilter1,
    /// Ring modulation from Filter 2.
    RmFilter2,
}

/// Which stage of the oscillator a mode acts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// None: the slot is off.
    Off,
    /// Before the read: it bends the phase.
    Phase,
    /// After the read: it bends the value.
    Wave,
}

impl Mode {
    /// Every mode, in menu order (70, as the reference's warp menu lists
    /// them; there is no Formant entry).
    pub const ALL: [Self; 70] = [
        Self::Off,
        Self::Sync,
        Self::PinchIn,
        Self::PinchOut,
        Self::Pinch,
        Self::PulseWidth,
        Self::LeanRight,
        Self::LeanLeft,
        Self::Lean,
        Self::Flip,
        Self::Mirror,
        Self::RemapDrawn,
        Self::RemapMirrored,
        Self::RemapSine,
        Self::RemapQuad,
        Self::Quantise,
        Self::OddEven,
        Self::LowPass,
        Self::HighPass,
        Self::Valve,
        Self::SoftClip,
        Self::HardClip,
        Self::Diode1,
        Self::Diode2,
        Self::Fold,
        Self::SineFold,
        Self::ZeroSquare,
        Self::Asymmetric,
        Self::Rectify,
        Self::SineShaper,
        Self::Pedal,
        Self::Tape,
        Self::SoftSaturate,
        Self::Fm1,
        Self::Fm2,
        Self::FmNoise,
        Self::FmSub,
        Self::FmFilter1,
        Self::FmFilter2,
        Self::FmExp1,
        Self::FmExp2,
        Self::FmExpNoise,
        Self::FmExpSub,
        Self::FmExpFilter1,
        Self::FmExpFilter2,
        Self::FmLin1,
        Self::FmLin2,
        Self::FmLinNoise,
        Self::FmLinSub,
        Self::FmLinFilter1,
        Self::FmLinFilter2,
        Self::Pd1,
        Self::Pd2,
        Self::PdNoise,
        Self::PdSub,
        Self::PdFilter1,
        Self::PdFilter2,
        Self::PdSelf,
        Self::Am1,
        Self::Am2,
        Self::AmNoise,
        Self::AmSub,
        Self::AmFilter1,
        Self::AmFilter2,
        Self::Rm1,
        Self::Rm2,
        Self::RmNoise,
        Self::RmSub,
        Self::RmFilter1,
        Self::RmFilter2,
    ];

    /// Its name, as a menu shows it: the FM modes take the type's name and
    /// the source's, as in "FM exp noise".
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Sync => "Sync",
            Self::PinchIn => "Pinch in",
            Self::PinchOut => "Pinch out",
            Self::Pinch => "Pinch",
            Self::PulseWidth => "Pulse width",
            Self::LeanRight => "Lean right",
            Self::LeanLeft => "Lean left",
            Self::Lean => "Lean",
            Self::Flip => "Flip",
            Self::Mirror => "Mirror",
            Self::RemapDrawn => "Remap drawn",
            Self::RemapMirrored => "Remap mirrored",
            Self::RemapSine => "Remap sine",
            Self::RemapQuad => "Remap ×4",
            Self::Quantise => "Quantise",
            Self::OddEven => "Odd/even",
            Self::LowPass => "Low-pass",
            Self::HighPass => "High-pass",
            Self::Valve => "Valve",
            Self::SoftClip => "Soft clip",
            Self::HardClip => "Hard clip",
            Self::Diode1 => "Diode 1",
            Self::Diode2 => "Diode 2",
            Self::Fold => "Fold",
            Self::SineFold => "Sine fold",
            Self::ZeroSquare => "Zero-square",
            Self::Asymmetric => "Asymmetric",
            Self::Rectify => "Rectify",
            Self::SineShaper => "Sine shaper",
            Self::Pedal => "Pedal",
            Self::Tape => "Tape",
            Self::SoftSaturate => "Soft saturate",
            Self::Fm1 => "FM 1",
            Self::Fm2 => "FM 2",
            Self::FmNoise => "FM noise",
            Self::FmSub => "FM sub",
            Self::FmFilter1 => "FM filter 1",
            Self::FmFilter2 => "FM filter 2",
            Self::FmExp1 => "FM exp 1",
            Self::FmExp2 => "FM exp 2",
            Self::FmExpNoise => "FM exp noise",
            Self::FmExpSub => "FM exp sub",
            Self::FmExpFilter1 => "FM exp filter 1",
            Self::FmExpFilter2 => "FM exp filter 2",
            Self::FmLin1 => "FM lin 1",
            Self::FmLin2 => "FM lin 2",
            Self::FmLinNoise => "FM lin noise",
            Self::FmLinSub => "FM lin sub",
            Self::FmLinFilter1 => "FM lin filter 1",
            Self::FmLinFilter2 => "FM lin filter 2",
            Self::Pd1 => "Phase distortion 1",
            Self::Pd2 => "Phase distortion 2",
            Self::PdNoise => "Phase distortion noise",
            Self::PdSub => "Phase distortion sub",
            Self::PdFilter1 => "Phase distortion filter 1",
            Self::PdFilter2 => "Phase distortion filter 2",
            Self::PdSelf => "Phase distortion self",
            Self::Am1 => "AM 1",
            Self::Am2 => "AM 2",
            Self::AmNoise => "AM noise",
            Self::AmSub => "AM sub",
            Self::AmFilter1 => "AM filter 1",
            Self::AmFilter2 => "AM filter 2",
            Self::Rm1 => "Ring 1",
            Self::Rm2 => "Ring 2",
            Self::RmNoise => "Ring noise",
            Self::RmSub => "Ring sub",
            Self::RmFilter1 => "Ring filter 1",
            Self::RmFilter2 => "Ring filter 2",
        }
    }

    /// The source it reads at audio rate, if it's an inter-oscillator mode
    /// (R27). The caller renders that signal and hands it in.
    #[must_use]
    pub const fn source(self) -> Option<Source> {
        use Source::{Filter1, Filter2, Noise, Osc1, Osc2, Sub};
        match self {
            Self::Fm1 | Self::FmExp1 | Self::FmLin1 | Self::Pd1 | Self::Am1 | Self::Rm1 => {
                Some(Osc1)
            }
            Self::Fm2 | Self::FmExp2 | Self::FmLin2 | Self::Pd2 | Self::Am2 | Self::Rm2 => {
                Some(Osc2)
            }
            Self::FmNoise
            | Self::FmExpNoise
            | Self::FmLinNoise
            | Self::PdNoise
            | Self::AmNoise
            | Self::RmNoise => Some(Noise),
            Self::FmSub
            | Self::FmExpSub
            | Self::FmLinSub
            | Self::PdSub
            | Self::AmSub
            | Self::RmSub => Some(Sub),
            Self::FmFilter1
            | Self::FmExpFilter1
            | Self::FmLinFilter1
            | Self::PdFilter1
            | Self::AmFilter1
            | Self::RmFilter1 => Some(Filter1),
            Self::FmFilter2
            | Self::FmExpFilter2
            | Self::FmLinFilter2
            | Self::PdFilter2
            | Self::AmFilter2
            | Self::RmFilter2 => Some(Filter2),
            Self::PdSelf => Some(Source::Own),
            _ => None,
        }
    }

    /// The FM type an FM mode modulates with (R28, R29), if it's one.
    #[must_use]
    pub const fn fm_type(self) -> Option<FmType> {
        match self {
            Self::Fm1
            | Self::Fm2
            | Self::FmNoise
            | Self::FmSub
            | Self::FmFilter1
            | Self::FmFilter2 => Some(FmType::ThruZero),
            Self::FmExp1
            | Self::FmExp2
            | Self::FmExpNoise
            | Self::FmExpSub
            | Self::FmExpFilter1
            | Self::FmExpFilter2 => Some(FmType::Exponential),
            Self::FmLin1
            | Self::FmLin2
            | Self::FmLinNoise
            | Self::FmLinSub
            | Self::FmLinFilter1
            | Self::FmLinFilter2 => Some(FmType::Linear),
            _ => None,
        }
    }

    /// What the mode does with its source, if it reads one (R27).
    pub(crate) const fn modulated(self) -> Option<Modulated> {
        match self.fm_type() {
            Some(kind) => Some(Modulated::Fm(kind)),
            None => match self {
                Self::Pd1
                | Self::Pd2
                | Self::PdNoise
                | Self::PdSub
                | Self::PdFilter1
                | Self::PdFilter2
                | Self::PdSelf => Some(Modulated::PhaseDistortion),
                Self::Am1
                | Self::Am2
                | Self::AmNoise
                | Self::AmSub
                | Self::AmFilter1
                | Self::AmFilter2 => Some(Modulated::Am),
                Self::Rm1
                | Self::Rm2
                | Self::RmNoise
                | Self::RmSub
                | Self::RmFilter1
                | Self::RmFilter2 => Some(Modulated::Ring),
                _ => None,
            },
        }
    }

    /// The stage it acts in.
    #[must_use]
    pub const fn stage(self) -> Stage {
        match self {
            Self::Off => Stage::Off,
            Self::Sync
            | Self::PinchIn
            | Self::PinchOut
            | Self::Pinch
            | Self::PulseWidth
            | Self::LeanRight
            | Self::LeanLeft
            | Self::Lean
            | Self::Flip
            | Self::Mirror
            | Self::RemapDrawn
            | Self::RemapMirrored
            | Self::RemapSine
            | Self::RemapQuad
            | Self::Quantise
            | Self::OddEven
            | Self::Fm1
            | Self::Fm2
            | Self::FmNoise
            | Self::FmSub
            | Self::FmFilter1
            | Self::FmFilter2
            | Self::FmExp1
            | Self::FmExp2
            | Self::FmExpNoise
            | Self::FmExpSub
            | Self::FmExpFilter1
            | Self::FmExpFilter2
            | Self::FmLin1
            | Self::FmLin2
            | Self::FmLinNoise
            | Self::FmLinSub
            | Self::FmLinFilter1
            | Self::FmLinFilter2
            | Self::Pd1
            | Self::Pd2
            | Self::PdNoise
            | Self::PdSub
            | Self::PdFilter1
            | Self::PdFilter2
            | Self::PdSelf => Stage::Phase,
            _ => Stage::Wave,
        }
    }

    /// Whether the oscillator renders it oversampled: the modes with edges
    /// and the distortions.
    #[must_use]
    pub const fn oversampled(self) -> bool {
        !matches!(
            self,
            Self::Off | Self::OddEven | Self::LowPass | Self::HighPass
        )
    }

    const fn filter(self) -> bool {
        matches!(self, Self::LowPass | Self::HighPass)
    }

    /// Whether it's a distortion: a memoryless curve applied to the value.
    #[must_use]
    pub const fn distortion(self) -> bool {
        matches!(self.stage(), Stage::Wave) && !self.filter() && self.source().is_none()
    }
}

/// One shaper slot's controls (engine-core section 3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    /// Its mode; Off in a new patch (R21).
    pub mode: Mode,
    /// Its amount, 0 to 100 %; 0 in a new patch (R21).
    pub amount: f32,
    /// Its character, 0 to 1: Sync's softness, the filters' resonance.
    pub character: f32,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            amount: 0.0,
            character: 0.0,
        }
    }
}

/// The two slots' controls, the remap curve and the oversampling factor,
/// shared by every voice of an oscillator.
#[derive(Clone, Debug, PartialEq)]
pub struct Shapers {
    /// Slot 1 then slot 2, which run in that order (R22).
    pub slots: [Slot; SLOTS],
    /// The curve Remap drawn, mirrored and ×4 map through (R25).
    pub curve: RemapCurve,
    oversampling: usize,
}

impl Default for Shapers {
    fn default() -> Self {
        Self {
            slots: [Slot::default(); SLOTS],
            curve: RemapCurve::default(),
            oversampling: 4,
        }
    }
}

impl Shapers {
    /// Exchanges the two slots: their modes, amounts and characters (R21).
    pub fn swap(&mut self) {
        self.slots.swap(0, 1);
    }

    /// The factor the modes that need it render oversampled at: 1, 2, 4
    /// or 8; 4 by default.
    #[must_use]
    pub fn oversampling(&self) -> usize {
        self.oversampling
    }

    /// Sets the oversampling factor, rounded down to 1, 2, 4 or 8.
    pub fn set_oversampling(&mut self, factor: usize) {
        self.oversampling = match factor {
            0 | 1 => 1,
            2 | 3 => 2,
            4..=7 => 4,
            _ => 8,
        };
    }

    /// The factor a render runs at: [`oversampling`](Self::oversampling)
    /// if either slot's mode needs it, else 1.
    #[must_use]
    pub fn render_factor(&self) -> usize {
        if self.slots.iter().any(|s| s.mode.oversampled()) {
            self.oversampling
        } else {
            1
        }
    }

    /// Whether both slots are off.
    #[must_use]
    pub fn is_off(&self) -> bool {
        self.slots.iter().all(|s| s.mode == Mode::Off)
    }

    /// One cycle of frame `frame` (counting from 0) of `table` through the
    /// shapers, at their knob amounts, `out.len()` points from phase 0, for
    /// an editor's 2D view (inventory WT-015). The table's richest level is
    /// read, edges are drawn sharp, and the filters show their steady
    /// state. An inter-oscillator mode draws as though its source were
    /// silent: its cycle needs the source's own signal.
    /// Allocation-free.
    pub fn preview(&self, table: &Table, frame: usize, out: &mut [f32]) {
        let len = out.len();
        if len == 0 {
            return;
        }
        let wave = table.wave(0, frame.min(table.frame_count() - 1));
        let increment = 1.0 / len as f32;
        let half = 0.25 * increment;
        let views = self.views();
        let amount = |slot: usize| (self.slots[slot].amount / 100.0).clamp(0.0, 1.0);
        let inner = if self.slots[1].mode.stage() == Stage::Phase {
            phase::stretch(self.slots[1].mode, amount(1), self.curve.steepest())
        } else {
            1.0
        };
        for (i, point) in out.iter_mut().enumerate() {
            let word = (i as f64 / len as f64 * CYCLE) as u64 as u32;
            let mut list = ReadList::<f32>::start(word);
            if self.slots[1].mode.stage() == Stage::Phase {
                list = phase::apply(&views[1], amount(1), half, &list);
            }
            if self.slots[0].mode.stage() == Stage::Phase {
                list = phase::apply(&views[0], amount(0), half * inner, &list);
            }
            *point = (0..list.count)
                .map(|r| list.gain[r] * wave.read(list.word[r]))
                .sum();
        }
        for (slot, view) in self.slots.iter().zip(&views) {
            // An inter-oscillator mode reads a signal the preview doesn't
            // have: it draws as though its source were silent.
            if slot.mode.stage() != Stage::Wave || slot.mode.source().is_some() {
                continue;
            }
            let a = (slot.amount / 100.0).clamp(0.0, 1.0);
            if slot.mode.filter() {
                let harmonic = wave::cutoff_harmonic(slot.mode, a);
                let k = wave::damping(view.character);
                let mut state = Svf { ic1: 0.0, ic2: 0.0 };
                // Fifteen cycles to settle, then the one drawn.
                for _ in 0..15 {
                    for &x in out.iter() {
                        wave::filter(slot.mode, harmonic, increment, k, x, &mut state);
                    }
                }
                for point in out.iter_mut() {
                    *point = wave::filter(slot.mode, harmonic, increment, k, *point, &mut state);
                }
            } else {
                for point in out.iter_mut() {
                    *point = wave::distort(slot.mode, a, *point);
                }
            }
        }
    }

    fn views(&self) -> [SlotView<'_>; SLOTS] {
        std::array::from_fn(|i| SlotView {
            mode: self.slots[i].mode,
            character: self.slots[i].character,
            curve: self.curve.table(),
        })
    }
}

/// Audio-rate modulation of each slot's amount, in percent, added to the
/// knob and the lane's offset: `frames × lane_count` values frame by frame,
/// as [`Inputs`]' modulation; and the signal each slot's inter-oscillator
/// mode reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShaperInputs<'a> {
    /// Slot 1's and slot 2's amount modulation, if any.
    pub amount_mod: [Option<&'a [f32]>; SLOTS],
    /// Each slot's source signal, if its mode reads one (R27): what the
    /// source makes, `frames × lane_count` values at the host rate, frame
    /// by frame and lane, *before* its Level, so a source at Level 0 still
    /// modulates (R12). Lane for lane: unison copy *i* of the carrier
    /// reads copy *i* of the source (R30). An oversampled render reads it
    /// between its host frames. A mode whose source is `None` reads
    /// nothing.
    pub sources: [Option<&'a [f32]>; SLOTS],
}

/// A wavetable oscillator with two shaper slots in series, every voice and
/// unison copy in SIMD lanes (engine-core section 3).
///
/// Construction allocates; nothing after it does, so every method that
/// takes `&mut self` is safe on an audio thread.
#[derive(Debug)]
pub struct ShapedOscillator {
    osc: Oscillator,
    shapers: Shapers,
    /// Each slot's per-lane amount offset, in percent (unison spread).
    offsets: [Box<[f32]>; SLOTS],
    /// Each slot's knob plus offset, per lane, for the render.
    base: [Box<[f32]>; SLOTS],
    /// Per stretch of [`SUB_BLOCK`] frames at the host rate and per lane:
    /// the steepest slope of both slots together, times the render factor;
    /// and slot 2's alone.
    stretch: Box<[f32]>,
    inner: Box<[f32]>,
    /// Scratch for the amount range of a stretch, per lane.
    low: Box<[f32]>,
    high: Box<[f32]>,
    /// Per slot, the filter's two states per lane.
    filter: Box<[f32]>,
    /// Per slot and lane: the FM offsets the render carries, in cycles.
    fm: Box<[f32]>,
    /// Per lane: the value the wave stage last read, for phase distortion
    /// from the oscillator's own signal.
    last: Box<[f32]>,
    /// Per stretch and lane: the slope the inter-oscillator modes add, in
    /// words a frame at the render rate.
    add: Box<[f32]>,
    /// One vector's worth for each stage, where it spreads lanes into the
    /// cells the other reads.
    scratch: [Box<[f32]>; SLOTS],
    decimator: Decimator,
    /// The factor the last render ran at.
    factor: usize,
    /// An oversampled render: its output, and its inputs at the render
    /// rate.
    work: Box<[f32]>,
    pitch_mod: Box<[f32]>,
    frame_mod: Box<[f32]>,
    pitch: Box<[f32]>,
    step: Box<[f32]>,
}

impl ShapedOscillator {
    /// `osc` with both slots off. Allocates; make it off the audio thread.
    #[must_use]
    pub fn new(osc: Oscillator) -> Self {
        let n = osc.lane_count();
        let lanes = || vec![0.0f32; n].into_boxed_slice();
        let stretches = (osc.max_frames().div_ceil(SUB_BLOCK) + 1) * n;
        let chunk = RENDER_CHUNK.min(osc.max_frames()) * n;
        Self {
            shapers: Shapers::default(),
            offsets: [lanes(), lanes()],
            base: [lanes(), lanes()],
            stretch: vec![1.0; stretches].into(),
            inner: vec![1.0; stretches].into(),
            low: lanes(),
            high: lanes(),
            filter: vec![0.0; SLOTS * 2 * n].into(),
            fm: vec![0.0; SLOTS * n].into(),
            last: vec![0.0; n].into(),
            add: vec![0.0; stretches].into(),
            scratch: [
                vec![0.0; crate::osc::LANE_ALIGN].into(),
                vec![0.0; crate::osc::LANE_ALIGN].into(),
            ],
            decimator: Decimator::new(n),
            factor: 1,
            work: vec![0.0; chunk].into(),
            pitch_mod: vec![0.0; chunk].into(),
            frame_mod: vec![0.0; chunk].into(),
            pitch: lanes(),
            step: lanes(),
            osc,
        }
    }

    /// The oscillator.
    #[must_use]
    pub fn osc(&self) -> &Oscillator {
        &self.osc
    }

    /// The oscillator, to change its controls or lanes.
    pub fn osc_mut(&mut self) -> &mut Oscillator {
        &mut self.osc
    }

    /// The shapers' controls.
    #[must_use]
    pub fn shapers(&self) -> &Shapers {
        &self.shapers
    }

    /// The shapers' controls, to change; they apply from the next render.
    pub fn shapers_mut(&mut self) -> &mut Shapers {
        &mut self.shapers
    }

    /// Slot `slot`'s amount offset for each lane, in percent, added to its
    /// knob: where the unison shaper spreads go (R31, R38; R1.7).
    ///
    /// # Panics
    ///
    /// If `slot` isn't 0 or 1.
    pub fn amount_offsets_mut(&mut self, slot: usize) -> &mut [f32] {
        &mut self.offsets[slot]
    }

    /// Sets lanes `lanes`' amount offsets for both slots from `offsets`
    /// (each indexed by lane, in percentage points), as the unison block's
    /// shaper spreads give them (`lspk_voice::UnisonLanes::shaper`): the
    /// shapers add them to the knob and clamp the sum to 0 to 100 % (R31,
    /// R38).
    ///
    /// # Panics
    ///
    /// If `lanes` runs past a slice.
    pub fn set_unison(&mut self, lanes: std::ops::Range<usize>, offsets: [&[f32]; SLOTS]) {
        for (to, from) in self.offsets.iter_mut().zip(offsets) {
            to[lanes.clone()].copy_from_slice(&from[lanes.clone()]);
        }
    }

    /// Voice `voice` starts playing `key`, as [`Oscillator::start`]; a
    /// fresh voice also clears its lanes' shaper state.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn start(&mut self, voice: usize, key: f32, onset: Onset) {
        self.osc.start(voice, key, onset);
        if onset == Onset::Fresh {
            let lanes = self.osc.lanes_of(voice);
            let n = self.osc.lane_count();
            for block in self.filter.chunks_exact_mut(n) {
                block[lanes.clone()].fill(0.0);
            }
            for block in self.fm.chunks_exact_mut(n) {
                block[lanes.clone()].fill(0.0);
            }
            self.last[lanes.clone()].fill(0.0);
            self.decimator.reset(lanes);
        }
    }

    /// Memory this instance holds itself, in bytes: everything but the
    /// shared table.
    #[must_use]
    pub fn bytes(&self) -> usize {
        let boxes = [
            &self.scratch[0],
            &self.scratch[1],
            &self.offsets[0],
            &self.offsets[1],
            &self.base[0],
            &self.base[1],
            &self.stretch,
            &self.inner,
            &self.low,
            &self.high,
            &self.filter,
            &self.fm,
            &self.last,
            &self.add,
            &self.work,
            &self.pitch_mod,
            &self.frame_mod,
            &self.pitch,
            &self.step,
        ];
        self.osc.bytes()
            + std::mem::size_of::<Self>()
            + boxes
                .iter()
                .map(|b| std::mem::size_of_val(&***b))
                .sum::<usize>()
            + self.decimator.bytes()
    }

    /// Renders `frames` frames of `inputs.lanes` lanes into `out`, as
    /// [`Oscillator::process`] does, through the shapers, with `mods`
    /// modulating their amounts: across lanes in SIMD at the level this CPU
    /// dispatches, or one lane at a time when the lanes would fill no more
    /// than a quarter of a vector.
    ///
    /// # Panics
    ///
    /// As [`Oscillator::process`], or if a modulation slice is shorter than
    /// `frames × lane_count`.
    pub fn process(
        &mut self,
        frames: usize,
        inputs: &Inputs<'_>,
        mods: &ShaperInputs<'_>,
        out: &mut [f32],
    ) {
        let level = simd::level();
        if inputs.lanes * 4 <= level.lanes() {
            self.render(None, frames, inputs, mods, out);
        } else {
            self.render(Some(level), frames, inputs, mods, out);
        }
    }

    /// [`process`](Self::process) at SIMD level `level`, or the portable
    /// level if this CPU doesn't have it.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_at(
        &mut self,
        level: SimdLevel,
        frames: usize,
        inputs: &Inputs<'_>,
        mods: &ShaperInputs<'_>,
        out: &mut [f32],
    ) {
        self.render(Some(level), frames, inputs, mods, out);
    }

    /// [`process`](Self::process) one lane at a time, without SIMD: the
    /// reference the vector paths are tested against.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_scalar(
        &mut self,
        frames: usize,
        inputs: &Inputs<'_>,
        mods: &ShaperInputs<'_>,
        out: &mut [f32],
    ) {
        self.render(None, frames, inputs, mods, out);
    }

    fn render(
        &mut self,
        level: Option<SimdLevel>,
        frames: usize,
        inputs: &Inputs<'_>,
        mods: &ShaperInputs<'_>,
        out: &mut [f32],
    ) {
        let n = self.osc.lane_count();
        for m in mods.amount_mod.iter().flatten() {
            assert!(
                m.len() >= frames * n,
                "amount modulation for {frames} frames of {n} lanes"
            );
        }
        for source in mods.sources.iter().flatten() {
            assert!(
                source.len() >= frames * n,
                "a source for {frames} frames of {n} lanes"
            );
        }
        if self.shapers.is_off() {
            match level {
                Some(level) => self.osc.process_at(level, frames, inputs, out),
                None => self.osc.process_scalar(frames, inputs, out),
            }
            return;
        }
        let factor = self.shapers.render_factor();
        if factor != self.factor {
            self.decimator.reset(0..n);
            self.factor = factor;
        }
        assert!(
            frames <= self.osc.max_frames(),
            "{frames} frames, at most {}",
            self.osc.max_frames()
        );
        self.prepare(frames, inputs.lanes, mods, factor);
        if factor == 1 {
            let n = self.osc.lane_count();
            let rate = self.osc.sample_rate();
            let [scratch_phase, scratch_wave] = &mut self.scratch;
            let (mut phase_run, mut wave_run) = runs(
                &self.shapers,
                &self.base,
                mods,
                &self.stretch,
                &self.inner,
                &self.add,
                &mut self.filter,
                &mut self.fm,
                &mut self.last,
                [&mut scratch_phase[..], &mut scratch_wave[..]],
                n,
                0,
                1,
                rate,
            );
            match level {
                Some(level) => {
                    self.osc
                        .process_with(level, frames, inputs, out, &mut phase_run, &mut wave_run)
                }
                None => {
                    self.osc.process_scalar_with(
                        frames,
                        inputs,
                        out,
                        &mut phase_run,
                        &mut wave_run,
                    );
                }
            }
            return;
        }
        self.render_oversampled(level, frames, inputs, mods, out, factor);
    }

    /// The render at `factor` times the rate, a chunk at a time, each
    /// decimated into `out`.
    fn render_oversampled(
        &mut self,
        level: Option<SimdLevel>,
        frames: usize,
        inputs: &Inputs<'_>,
        mods: &ShaperInputs<'_>,
        out: &mut [f32],
        factor: usize,
    ) {
        let n = self.osc.lane_count();
        let chunk = (RENDER_CHUNK.min(self.osc.max_frames()) / factor).max(1);
        let rate = self.osc.sample_rate();
        self.osc.set_sample_rate(rate * factor as f32);
        let mut start = 0;
        while start < frames {
            let len = chunk.min(frames - start);
            let render = len * factor;
            for l in 0..n {
                let step = inputs.pitch_step.map_or(0.0, |s| s[l]);
                self.pitch[l] = (start as f32).mul_add(step, inputs.pitch[l]);
                self.step[l] = step / factor as f32;
            }
            for (from, to) in [
                (inputs.pitch_mod, &mut self.pitch_mod),
                (inputs.frame_mod, &mut self.frame_mod),
            ] {
                if let Some(from) = from {
                    for k in 0..render {
                        let source = (start + k / factor) * n;
                        to[k * n..(k + 1) * n].copy_from_slice(&from[source..source + n]);
                    }
                }
            }
            let chunk_inputs = Inputs {
                lanes: inputs.lanes,
                pitch: &self.pitch,
                pitch_step: inputs.pitch_step.map(|_| &self.step[..]),
                pitch_mod: inputs.pitch_mod.map(|_| &self.pitch_mod[..render * n]),
                frame_mod: inputs.frame_mod.map(|_| &self.frame_mod[..render * n]),
            };
            let [scratch_phase, scratch_wave] = &mut self.scratch;
            let (mut phase_run, mut wave_run) = runs(
                &self.shapers,
                &self.base,
                mods,
                &self.stretch,
                &self.inner,
                &self.add,
                &mut self.filter,
                &mut self.fm,
                &mut self.last,
                [&mut scratch_phase[..], &mut scratch_wave[..]],
                n,
                start,
                factor,
                self.osc.sample_rate(),
            );
            let work = &mut self.work[..render * n];
            match level {
                Some(level) => self.osc.process_with(
                    level,
                    render,
                    &chunk_inputs,
                    work,
                    &mut phase_run,
                    &mut wave_run,
                ),
                None => self.osc.process_scalar_with(
                    render,
                    &chunk_inputs,
                    work,
                    &mut phase_run,
                    &mut wave_run,
                ),
            }
            self.decimator
                .process(level, factor, work, len, inputs.lanes);
            out[start * n..(start + len) * n].copy_from_slice(&work[..len * n]);
            start += len;
        }
        self.osc.set_sample_rate(rate);
    }

    /// Works out, for the render: each slot's knob plus offset per lane,
    /// and per stretch and lane the steepest slope the slots read with.
    fn prepare(&mut self, frames: usize, lanes: usize, mods: &ShaperInputs<'_>, factor: usize) {
        let n = self.osc.lane_count();
        let lanes = lanes.div_ceil(16).saturating_mul(16).min(n);
        for slot in 0..SLOTS {
            let knob = self.shapers.slots[slot].amount;
            for (base, offset) in self.base[slot].iter_mut().zip(&self.offsets[slot]) {
                *base = knob + offset;
            }
        }
        let curve = self.shapers.curve.steepest();
        // FM's hertz as words a frame at the render rate.
        let words_per_hertz = 4_294_967_296.0 / (self.osc.sample_rate() * factor as f32);
        let stretches = frames.div_ceil(SUB_BLOCK);
        for s in 0..stretches {
            let from = s * SUB_BLOCK;
            let to = (from + SUB_BLOCK).min(frames);
            // A distortion makes harmonics of whatever it's given, so it's
            // given a read band-limited to the host rate's Nyquist; a phase
            // mode reads up to the render rate's, and the decimator removes
            // what lies above the host's.
            let distorts = self.shapers.slots.iter().any(|s| s.mode.distortion());
            let total = &mut self.stretch[s * n..s * n + lanes];
            total.fill(if distorts { factor as f32 } else { 1.0 });
            self.inner[s * n..s * n + lanes].fill(1.0);
            self.add[s * n..s * n + lanes].fill(0.0);
            for slot in (0..SLOTS).rev() {
                let mode = self.shapers.slots[slot].mode;
                if mode.stage() != Stage::Phase {
                    continue;
                }
                let modulated = mode.modulated();
                let source = mods.sources[slot];
                let base = &self.base[slot][..lanes];
                self.low[..lanes].copy_from_slice(base);
                self.high[..lanes].copy_from_slice(base);
                if let Some(m) = mods.amount_mod[slot] {
                    // With modulation the knob alone is never the amount.
                    self.low[..lanes].fill(f32::INFINITY);
                    self.high[..lanes].fill(f32::NEG_INFINITY);
                    for k in from..to {
                        let row = &m[k * n..k * n + lanes];
                        for (((low, high), &b), &x) in self.low[..lanes]
                            .iter_mut()
                            .zip(&mut self.high[..lanes])
                            .zip(base)
                            .zip(row)
                        {
                            *low = low.min(b + x);
                            *high = high.max(b + x);
                        }
                    }
                }
                for l in 0..lanes {
                    let low = phase::stretch(mode, self.low[l] / 100.0, curve);
                    let high = phase::stretch(mode, self.high[l] / 100.0, curve);
                    let steepest = low.max(high);
                    self.stretch[s * n + l] *= steepest;
                    if slot == 1 {
                        self.inner[s * n + l] = steepest;
                    }
                    // An inter-oscillator mode moves the read on with its
                    // source (R27): the slope it adds, in words a frame, so
                    // the read stays band-limited.
                    let (Some(modulated), Some(source)) = (modulated, source) else {
                        continue;
                    };
                    // AM and Ring move the value, not the read: no slope.
                    if matches!(modulated, Modulated::Am | Modulated::Ring) {
                        continue;
                    }
                    let amount = self.low[l].abs().max(self.high[l].abs()).min(100.0) / 100.0;
                    let (peak, step) = source_span(source, n, from, to, l);
                    self.add[s * n + l] += match modulated {
                        Modulated::Am | Modulated::Ring => 0.0,
                        Modulated::PhaseDistortion => {
                            amount * PD_MAX_CYCLES * (step / factor as f32) * 4_294_967_296.0
                        }
                        Modulated::Fm(FmType::Exponential) => {
                            amount
                                * amount
                                * amount
                                * amount
                                * FM_MAX_HZ
                                * fm_exponential(peak)
                                * words_per_hertz
                        }
                        Modulated::Fm(_) => {
                            amount * amount * amount * amount * FM_MAX_HZ * peak * words_per_hertz
                        }
                    };
                }
            }
        }
    }
}

/// The source's peak and steepest step over frames `from..to` of lane
/// `lane`: what an inter-oscillator mode's slope bound is made of.
#[inline]
fn source_span(source: &[f32], n: usize, from: usize, to: usize, lane: usize) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut step = 0.0f32;
    let mut last = source[from * n + lane];
    for k in from..to {
        let x = source[k * n + lane];
        peak = peak.max(x.abs());
        step = step.max((x - last).abs());
        last = x;
    }
    (peak, step)
}

/// Exponential FM's scaling of the source's value: (2^s − 2^−s) / (2 ln
/// 2), the exponential about its own value, chosen so a small swing gives
/// the linear FM's deviation (a first law; measurement replaces it).
#[inline(always)]
fn fm_exponential<L: Lane>(s: L) -> L {
    (s.exp2() - (-s).exp2()) / L::splat(2.0 * std::f32::consts::LN_2)
}

/// The two stages for a render whose first frame is host frame `start`,
/// at `factor` times the rate.
#[allow(
    clippy::too_many_arguments,
    reason = "the oscillator's parts, kept apart for borrowing"
)]
fn runs<'a>(
    shapers: &'a Shapers,
    base: &'a [Box<[f32]>; SLOTS],
    mods: &ShaperInputs<'a>,
    stretch: &'a [f32],
    inner: &'a [f32],
    add: &'a [f32],
    filter: &'a mut [f32],
    fm: &'a mut [f32],
    last: &'a mut [f32],
    scratch: [&'a mut [f32]; SLOTS],
    n: usize,
    start: usize,
    factor: usize,
    rate: f32,
) -> (PhaseRun<'a>, WaveRun<'a>) {
    let views = shapers.views();
    let stages = shapers.slots.map(|s| s.mode.stage());
    let amounts = Amounts {
        base: [&base[0][..], &base[1][..]],
        mods: mods.amount_mod,
        n,
        start,
        factor,
    };
    // What the wave stage last read, for phase distortion from the
    // oscillator's own signal: cells, so both stages may hold it.
    let last: &'a [std::cell::Cell<f32>] = std::cell::Cell::from_mut(last).as_slice_of_cells();
    let modulated = shapers.slots.map(|s| s.mode.modulated().is_some());
    let track = shapers
        .slots
        .iter()
        .any(|s| s.mode.source() == Some(Source::Own));
    let [scratch_phase, scratch_wave] = scratch;
    let phase = PhaseRun {
        views,
        on: stages.map(|s| s == Stage::Phase),
        modulated,
        amounts,
        stretch,
        inner,
        add,
        sources: mods.sources,
        fm,
        last,
        scratch: scratch_phase,
        rate,
    };
    let wave = WaveRun {
        modes: shapers.slots.map(|s| s.mode),
        on: stages.map(|s| s == Stage::Wave),
        modulated,
        damping: shapers.slots.map(|s| wave::damping(s.character)),
        amounts,
        filter,
        sources: mods.sources,
        last,
        scratch: scratch_wave,
        track,
    };
    (phase, wave)
}

/// Where a render's amounts come from.
#[derive(Clone, Copy)]
struct Amounts<'a> {
    base: [&'a [f32]; SLOTS],
    mods: [Option<&'a [f32]>; SLOTS],
    n: usize,
    /// The render's first frame, at the host rate.
    start: usize,
    factor: usize,
}

impl Amounts<'_> {
    /// The host frame render frame `frame` falls in.
    #[inline(always)]
    fn host_frame(&self, frame: usize) -> usize {
        self.start + frame / self.factor
    }

    /// Slot `slot`'s amount, 0 to 1, for lanes from `lane` at render frame
    /// `frame`.
    #[inline(always)]
    fn get<L: Lane>(&self, slot: usize, lane: usize, frame: usize) -> L {
        let mut amount = L::load(self.base[slot], lane);
        if let Some(m) = self.mods[slot] {
            amount = amount + L::load(m, self.host_frame(frame) * self.n + lane);
        }
        clamp(amount, 0.0, 100.0) * L::splat(0.01)
    }

    /// A source signal's value at render frame `frame`, lanes from `lane`:
    /// one value a host frame, linear between the two either side of it.
    #[inline(always)]
    fn source<L: Lane>(&self, source: &[f32], frame: usize, lane: usize) -> L {
        let at = self.host_frame(frame);
        let a = L::load(source, at * self.n + lane);
        if self.factor == 1 {
            return a;
        }
        let host = source.len() / self.n;
        let next = if at + 1 < host { at + 1 } else { host - 1 };
        let b = L::load(source, next * self.n + lane);
        let toward = (frame % self.factor) as f32 / self.factor as f32;
        (b - a).mul_add(L::splat(toward), a)
    }
}

/// The phase stage for one render.
struct PhaseRun<'a> {
    views: [SlotView<'a>; SLOTS],
    on: [bool; SLOTS],
    /// Slots whose mode is an inter-oscillator one.
    modulated: [bool; SLOTS],
    amounts: Amounts<'a>,
    stretch: &'a [f32],
    inner: &'a [f32],
    /// The slope the inter-oscillator modes add, in words a frame
    /// (stretch × lane).
    add: &'a [f32],
    /// The source each slot reads.
    sources: [Option<&'a [f32]>; SLOTS],
    /// The FM offsets, per slot and lane, in cycles.
    fm: &'a mut [f32],
    /// What the wave stage last read.
    last: &'a [std::cell::Cell<f32>],
    scratch: &'a mut [f32],
    /// The render's sample rate: FM's hertz as cycles a frame.
    rate: f32,
}

impl PhaseRun<'_> {
    /// The word offset inter-oscillator slot `slot` adds to the read at
    /// this frame and lane (R27): FM carries a running offset, phase
    /// distortion offsets directly.
    // Inlined into each SIMD level's kernel in an optimised build; kept out
    // of line in a debug one, whose kernel frame would otherwise hold every
    // inlined copy's locals at once and overflow a thread's stack.
    #[cfg_attr(not(debug_assertions), inline(always))]
    #[cfg_attr(debug_assertions, inline(never))]
    fn offset_of<L: Lane>(
        &mut self,
        slot: usize,
        lane: usize,
        frame: usize,
        increment: L,
    ) -> L::Word {
        let mode = self.views[slot].mode;
        let modulated = mode.modulated().expect("an inter-oscillator mode");
        // Without a source the mode reads nothing.
        let Some(source) = self.sources[slot] else {
            return L::word(0);
        };
        let a = self.amounts.get::<L>(slot, lane, frame);
        let s = if mode.source() == Some(Source::Own) {
            L::load_cells(self.last, lane, self.scratch)
        } else {
            self.amounts.source::<L>(source, frame, lane)
        };
        match modulated {
            Modulated::PhaseDistortion => L::to_word(a * L::splat(PD_MAX_CYCLES) * s),
            Modulated::Am | Modulated::Ring => L::word(0),
            Modulated::Fm(kind) => {
                let s = match kind {
                    FmType::Exponential => fm_exponential(s),
                    _ => s,
                };
                let cycles = a * a * a * a * L::splat(FM_MAX_HZ / self.rate) * s;
                // Linear FM clamps at 0 Hz: the read moves on at least not
                // backwards.
                let cycles = if kind == FmType::Linear {
                    cycles.max(-increment)
                } else {
                    cycles
                };
                let at = slot * self.amounts.n + lane;
                let carried = L::load(self.fm, at) + cycles;
                // Wrapped to ±½ cycle, where it means the same.
                let carried = carried - (carried + L::splat(0.5)).floor();
                carried.store(self.fm, at);
                L::to_word(carried)
            }
        }
    }

    // As `offset_of`.
    #[cfg_attr(not(debug_assertions), inline(always))]
    #[cfg_attr(debug_assertions, inline(never))]
    fn reads_of<L: Lane>(
        &mut self,
        lane: usize,
        frame: usize,
        word: L::Word,
        increment: L,
    ) -> ReadList<L> {
        // TRANSITION is in host samples; the increment is per render sample.
        let half = increment * L::splat(TRANSITION * 0.5 * self.amounts.factor as f32);
        let mut list = ReadList::start(word);
        // Slot 2's map first, then slot 1's on the result (R22).
        if self.on[1] {
            let a = self.amounts.get(1, lane, frame);
            list = phase::apply(&self.views[1], a, half, &list);
        }
        if self.on[0] {
            let a = self.amounts.get(0, lane, frame);
            let at = self.amounts.host_frame(frame) / SUB_BLOCK * self.amounts.n + lane;
            let half = half * L::load(self.inner, at);
            list = phase::apply(&self.views[0], a, half, &list);
        }
        // The inter-oscillator modes move the read on (R27); without a
        // source they read nothing.
        for slot in (0..SLOTS).rev() {
            if self.modulated[slot] && self.sources[slot].is_some() {
                let offset = self.offset_of::<L>(slot, lane, frame, increment);
                phase::shift(&mut list, offset);
            }
        }
        list
    }
}

/// A phase increment as cycles a frame.
const PER_CYCLE: f32 = 1.0 / 4_294_967_296.0;

impl PhaseStage for PhaseRun<'_> {
    #[inline(always)]
    fn shape<S: F32s>(&mut self, _: usize, _: usize, phase: S::U32s, _: S::U32s) -> S::U32s {
        phase
    }

    #[inline(always)]
    fn shape_lane(&mut self, _: usize, _: usize, phase: u32, _: u32) -> u32 {
        phase
    }

    #[inline(always)]
    fn reads<S: F32s>(
        &mut self,
        lanes: usize,
        frame: usize,
        phase: S::U32s,
        increment: S::U32s,
    ) -> Reads<S> {
        if !(self.on[0] || self.on[1]) {
            return Reads::one(phase);
        }
        // Increments stay below 2³¹, so they read as positive integers.
        let increment = V(S::from_i32(increment) * S::splat(PER_CYCLE));
        let list = self.reads_of::<V<S>>(lanes, frame, phase, increment);
        Reads {
            phase: list.word,
            gain: list.gain.map(|g| g.0),
            count: if list.weighted { list.count } else { 0 },
        }
    }

    #[inline(always)]
    fn reads_lane(&mut self, lane: usize, frame: usize, phase: u32, increment: u32) -> LaneReads {
        if !(self.on[0] || self.on[1]) {
            return LaneReads::one(phase);
        }
        let increment = increment as i32 as f32 * PER_CYCLE;
        let list = self.reads_of::<f32>(lane, frame, phase, increment);
        LaneReads {
            phase: list.word,
            gain: list.gain,
            count: if list.weighted { list.count } else { 0 },
        }
    }

    #[inline(always)]
    fn stretch(&mut self, lane: usize, frames: std::ops::Range<usize>, increment: u32) -> f32 {
        let a = &self.amounts;
        let first = a.host_frame(frames.start) / SUB_BLOCK;
        let last = a.host_frame(frames.end.max(frames.start + 1) - 1) / SUB_BLOCK;
        let steepest = (first..=last)
            .map(|s| self.stretch[s * a.n + lane])
            .fold(1.0, f32::max);
        // What the inter-oscillator modes add to the increment: absolute,
        // so the ratio to the increment is taken here.
        let added = (first..=last)
            .map(|s| self.add[s * a.n + lane])
            .fold(0.0, f32::max);
        if added <= 0.0 || increment == 0 {
            return steepest;
        }
        steepest.max((increment as f32 + added) / increment as f32)
    }
}

/// The wave stage for one render.
struct WaveRun<'a> {
    modes: [Mode; SLOTS],
    on: [bool; SLOTS],
    /// Slots whose mode is an inter-oscillator one.
    modulated: [bool; SLOTS],
    damping: [f32; SLOTS],
    amounts: Amounts<'a>,
    filter: &'a mut [f32],
    /// The source each slot reads.
    sources: [Option<&'a [f32]>; SLOTS],
    /// Where the value read is kept, for phase distortion from it.
    last: &'a [std::cell::Cell<f32>],
    scratch: &'a mut [f32],
    /// Whether any slot reads that value.
    track: bool,
}

impl WaveRun<'_> {
    // As `PhaseRun::reads_of`.
    #[cfg_attr(not(debug_assertions), inline(always))]
    #[cfg_attr(debug_assertions, inline(never))]
    fn shape_of<L: Lane>(&mut self, lane: usize, frame: usize, increment: L, mut x: L) -> L {
        let n = self.amounts.n;
        if self.track {
            L::store_cells(x, self.last, lane, self.scratch);
        }
        for slot in 0..SLOTS {
            if !self.on[slot] {
                continue;
            }
            let mode = self.modes[slot];
            let a = self.amounts.get::<L>(slot, lane, frame);
            if mode.filter() {
                let at = slot * 2 * n + lane;
                let mut state = Svf {
                    ic1: L::load(self.filter, at),
                    ic2: L::load(self.filter, at + n),
                };
                let harmonic = wave::cutoff_harmonic(mode, a);
                x = wave::filter(mode, harmonic, increment, self.damping[slot], x, &mut state);
                state.ic1.store(self.filter, at);
                state.ic2.store(self.filter, at + n);
            } else if self.modulated[slot] {
                // AM and Ring read the source at audio rate (R27); without
                // a source they read nothing.
                let Some(source) = self.sources[slot] else {
                    continue;
                };
                let s = if mode.source() == Some(Source::Own) {
                    L::load_cells(self.last, lane, self.scratch)
                } else {
                    self.amounts.source::<L>(source, frame, lane)
                };
                match mode.modulated().expect("an inter-oscillator mode") {
                    Modulated::Am => {
                        x = x * (L::splat(1.0) - a * L::splat(0.5) * (L::splat(1.0) - s));
                    }
                    Modulated::Ring => x = x * (L::splat(1.0) - a + a * s),
                    Modulated::Fm(_) | Modulated::PhaseDistortion => {}
                }
            } else {
                let at = slot * 2 * n + lane;
                let mut state = Svf {
                    ic1: L::load(self.filter, at),
                    ic2: L::load(self.filter, at + n),
                };
                x = wave::distort_smooth(mode, a, x, &mut state);
                state.ic1.store(self.filter, at);
                state.ic2.store(self.filter, at + n);
            }
        }
        x
    }
}

impl WaveStage for WaveRun<'_> {
    #[inline(always)]
    fn shape<S: F32s>(&mut self, _: usize, _: usize, value: S) -> S {
        value
    }

    #[inline(always)]
    fn shape_lane(&mut self, _: usize, _: usize, value: f32) -> f32 {
        value
    }

    #[inline(always)]
    fn shape_at<S: F32s>(&mut self, lanes: usize, frame: usize, increment: S::U32s, value: S) -> S {
        if !(self.on[0] || self.on[1]) {
            return value;
        }
        let increment = V(S::from_i32(increment) * S::splat(PER_CYCLE));
        self.shape_of::<V<S>>(lanes, frame, increment, V(value)).0
    }

    #[inline(always)]
    fn shape_at_lane(&mut self, lane: usize, frame: usize, increment: u32, value: f32) -> f32 {
        if !(self.on[0] || self.on[1]) {
            return value;
        }
        let increment = increment as i32 as f32 * PER_CYCLE;
        self.shape_of::<f32>(lane, frame, increment, value)
    }
}
