//! Zero-delay-feedback filters, per voice, written once for the scalar path
//! and for SIMD lanes across voices: a state-variable family and a
//! four-pole transistor ladder.
//!
//! | [`FilterType`] | Slopes (dB per octave) | Resonance | Self-oscillates |
//! |---|---|---|---|
//! | Low-pass, high-pass | 6, 12, 18, 24 | from 12 | from 12 |
//! | Band-pass | 12, 24 | yes | yes |
//! | Peak | 12, 24 | raises the peak, up to +24 dB | no |
//! | Notch | 12, 24 | narrows the notch | no |
//! | Ladder low-pass | 6, 12, 18, 24 | yes | yes |
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
//! Everything a bank needs is allocated when it's made; [`FilterBank::process`]
//! allocates nothing, locks nothing and never blocks.

mod lane;

use lane::{Lane, TAN_LIMIT, exp2, saturation_gain, soft_clip, tan};

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
    pub const ALL: [Self; 18] = [
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
        }
    }

    /// Its slope in decibels per octave.
    #[must_use]
    pub const fn slope(self) -> u32 {
        match self {
            Self::Lp6 | Self::Hp6 | Self::Ladder6 => 6,
            Self::Lp12
            | Self::Hp12
            | Self::Bp12
            | Self::Peak12
            | Self::Notch12
            | Self::Ladder12 => 12,
            Self::Lp18 | Self::Hp18 | Self::Ladder18 => 18,
            Self::Lp24
            | Self::Hp24
            | Self::Bp24
            | Self::Peak24
            | Self::Notch24
            | Self::Ladder24 => 24,
        }
    }

    /// Whether resonance does anything (not for the 6 dB state-variable
    /// types, which are one-pole stages).
    #[must_use]
    pub const fn has_resonance(self) -> bool {
        !matches!(self, Self::Lp6 | Self::Hp6)
    }

    /// Whether high resonance makes it oscillate on its own.
    #[must_use]
    pub const fn self_oscillates(self) -> bool {
        self.has_resonance()
            && !matches!(
                self,
                Self::Peak12 | Self::Peak24 | Self::Notch12 | Self::Notch24
            )
    }

    /// The resonance law: for the state-variable types, the resonant
    /// stage's damping (`k`, 1/Q) at `resonance` from 0 to 1; for the
    /// ladder, its feedback gain. Without saturation, this is the value
    /// the analogue prototype has. The 6 dB state-variable types have
    /// none and return 0.
    #[must_use]
    pub fn feedback(self, resonance: f32) -> f32 {
        let r = resonance.clamp(0.0, 1.0);
        match self {
            Self::Lp6 | Self::Hp6 => 0.0,
            Self::Ladder6 | Self::Ladder12 | Self::Ladder18 | Self::Ladder24 => ladder_k(r),
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
}

impl Default for VoiceParams {
    /// Resonance 10 %, no drive or saturation, no offset, fully filtered,
    /// unity level.
    fn default() -> Self {
        Self {
            resonance: 0.1,
            drive: 0.0,
            saturation: 0.0,
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

    /// Sets the sample rate. Filter states carry on.
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
        let run = Run {
            bank: self,
            pitch,
            audio,
            frames,
        };
        if run.bank.simd {
            simd::dispatch(run);
        } else {
            run.go::<f32>();
        }
    }

    /// As [`process`](Self::process), with SIMD lanes at `level`, for tests
    /// and benchmarks that compare paths. Gives `false`, and leaves `audio`
    /// alone, if this CPU can't run `level` or the bank is scalar.
    pub fn process_at(&mut self, level: Level, pitch: &[f32], audio: &mut [f32]) -> bool {
        if !self.simd {
            return false;
        }
        assert_eq!(pitch.len() % self.stride, 0);
        let frames = pitch.len() / self.stride;
        assert_eq!(audio.len(), frames * self.channels * self.stride);
        simd::dispatch_at(
            level,
            Run {
                bank: self,
                pitch,
                audio,
                frames,
            },
        )
        .is_ok()
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
struct Run<'a> {
    bank: &'a mut FilterBank,
    pitch: &'a [f32],
    audio: &'a mut [f32],
    frames: usize,
}

impl Kernel for Run<'_> {
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

impl Run<'_> {
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
                let audio = &mut *self.audio;
                let (p, s) = (&mut params, &mut state);
                match bank.kind {
                    FilterType::Lp6 => {
                        block.run::<L, OnePole<false>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Hp6 => {
                        block.run::<L, OnePole<true>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Lp12 => block.run::<L, Svf12<LOW>>(&shared, &targets, p, s, audio),
                    FilterType::Hp12 => block.run::<L, Svf12<HIGH>>(&shared, &targets, p, s, audio),
                    FilterType::Bp12 => block.run::<L, Svf12<BAND>>(&shared, &targets, p, s, audio),
                    FilterType::Peak12 => {
                        block.run::<L, Svf12<PEAK>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Notch12 => {
                        block.run::<L, Svf12<NOTCH>>(&shared, &targets, p, s, audio);
                    }
                    FilterType::Lp18 => {
                        block.run::<L, Svf18<false>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Hp18 => block.run::<L, Svf18<true>>(&shared, &targets, p, s, audio),
                    FilterType::Lp24 => block.run::<L, Svf24<LOW>>(&shared, &targets, p, s, audio),
                    FilterType::Hp24 => block.run::<L, Svf24<HIGH>>(&shared, &targets, p, s, audio),
                    FilterType::Bp24 => block.run::<L, Svf24<BAND>>(&shared, &targets, p, s, audio),
                    FilterType::Peak24 => {
                        block.run::<L, Svf24<PEAK>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Notch24 => {
                        block.run::<L, Svf24<NOTCH>>(&shared, &targets, p, s, audio);
                    }
                    FilterType::Ladder6 => {
                        block.run::<L, Ladder<1>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Ladder12 => {
                        block.run::<L, Ladder<2>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Ladder18 => {
                        block.run::<L, Ladder<3>>(&shared, &targets, p, s, audio)
                    }
                    FilterType::Ladder24 => {
                        block.run::<L, Ladder<4>>(&shared, &targets, p, s, audio)
                    }
                }
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
