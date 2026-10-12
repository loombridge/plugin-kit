//! Envelopes: delay, attack, hold, decay, sustain and release, with start
//! and end levels, bendable attack, decay and release curves, times in
//! milliseconds or tempo-synced, legato and steal restarts, and a global
//! time scale.
//!
//! [`Envelope`] is one voice's envelope. [`EnvelopeBank`] runs one envelope
//! per voice across SIMD lanes, writing every voice's level for each
//! sample; lane `i` of a bank gives exactly the same samples as an
//! [`Envelope`] given the same parameters and events, on every SIMD level.
//!
//! # Stages
//!
//! At a note the envelope holds its starting level for Delay, rises to full
//! (1.0) over Attack, stays there for Hold, falls to Sustain over Decay and
//! stays there until the note ends. It then falls from wherever it is to
//! its end level over Release, and goes [idle](Stage::Idle) there. A stage
//! of zero time is skipped. Levels are gains from 0 to 1; times are in
//! milliseconds, or in beats (quarter notes) at the host's tempo with
//! [`TimeUnit::Tempo`], and every time is multiplied by the global scale
//! ([`Timing::scale`], 10 % to 1000 %).
//!
//! Attack, decay and release bend by their curve controls (see [`curve`]:
//! 50 % straight, higher values fast first and slow into the target).
//!
//! # Notes
//!
//! A note starts the envelope with a [`Trigger`] that says how the voice
//! came to play it, which the voice allocator knows: a new voice starts at
//! the start level; a legato note leaves the envelope alone unless it is
//! [legato-inverted](EnvelopeParams::legato_inverted); a voice that was
//! already sounding (a mono retrigger, or a stolen voice) restarts as
//! [`EnvelopeParams::steal_restart`] says, from its current level or from
//! its start level. An attack that starts above its start level joins its
//! curve at that level, so it takes only the rest of the attack time.
//!
//! # Sample-accurate and block-size-invariant
//!
//! Events take effect at the next sample [`process`](Envelope::process)
//! writes, so a caller splits its block at each event. Stage changes land
//! on exact samples: a 1 ms attack at 48 kHz outputs its start level at
//! sample 0 and reaches full at sample 48. The samples don't depend on how
//! a run is split into blocks; they are the same bits whatever the split.
//!
//! # Cost
//!
//! Each sample costs two multiplies and two adds per voice, in SIMD lanes
//! across voices. Planning (a few `exp` calls in `f64`) runs only at
//! events, stage changes and every [16384 samples](curve) of a long stage.
//! A lane group whose levels aren't moving (sustain, hold, idle) only
//! stores its levels; when no voice has a timed stage, a block is a copy.
//!
//! Nothing here allocates, locks or blocks after construction, so every
//! method but `new` is safe on an audio thread.

pub mod curve;
mod lane;
pub mod law;

#[cfg(test)]
mod tests;

use lane::{Lane, Segment, UNTIMED};

use crate::simd::{self, F32s, Kernel, Level};

/// The smallest global time scale: 10 %.
pub const MIN_TIME_SCALE: f64 = 0.1;
/// The largest global time scale: 1000 %.
pub const MAX_TIME_SCALE: f64 = 10.0;

/// The unit an envelope's times are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeUnit {
    /// Milliseconds.
    Milliseconds,
    /// Beats (quarter notes) at [`Timing::tempo`]; see [`nearest_division`].
    Tempo,
}

/// Where an envelope restarts when its voice, already sounding, takes a new
/// note: a stolen voice, a mono retrigger or a legato-inverted envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StealRestart {
    /// From the level it has reached, without a jump.
    CurrentLevel,
    /// From its start level (silence for an amplifier envelope).
    StartLevel,
}

/// How a note reached the envelope's voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// A voice that wasn't sounding: the envelope starts at its start
    /// level.
    New,
    /// A voice that was sounding and takes a new note without legato: a
    /// stolen voice, or a mono note change with legato off. The envelope
    /// restarts as [`EnvelopeParams::steal_restart`] says.
    Retrigger,
    /// A note played legato over a held one. The envelope carries on,
    /// unless it is [legato-inverted](EnvelopeParams::legato_inverted), when
    /// it restarts as for [`Retrigger`](Self::Retrigger).
    Legato,
}

/// Where an envelope is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Not playing: holding the end level until a note.
    Idle,
    /// Holding the starting level.
    Delay,
    /// Rising to full.
    Attack,
    /// Holding full.
    Hold,
    /// Falling to the sustain level.
    Decay,
    /// Holding the sustain level until the note ends.
    Sustain,
    /// Falling to the end level.
    Release,
}

impl Stage {
    /// The stage after this one, when it ends by itself.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Delay => Self::Attack,
            Self::Attack => Self::Hold,
            Self::Hold => Self::Decay,
            Self::Decay | Self::Sustain => Self::Sustain,
            Self::Release | Self::Idle => Self::Idle,
        }
    }
}

/// An envelope's settings. Times are in [`unit`](Self::unit); levels and
/// curves run from 0 to 1, and values outside are clamped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvelopeParams {
    /// The unit of every time below.
    pub unit: TimeUnit,
    /// Time holding the starting level before the attack.
    pub delay: f32,
    /// Time rising to full.
    pub attack: f32,
    /// Time holding full.
    pub hold: f32,
    /// Time falling to the sustain level.
    pub decay: f32,
    /// The level held while the note lasts.
    pub sustain: f32,
    /// Time falling to the end level once the note ends.
    pub release: f32,
    /// The attack's curve: 0.5 straight (see [`curve`]).
    pub attack_curve: f32,
    /// The decay's curve.
    pub decay_curve: f32,
    /// The release's curve.
    pub release_curve: f32,
    /// The level a new note starts from.
    pub start: f32,
    /// The level the release ends at, held while idle.
    pub end: f32,
    /// Restart on every note, legato or not.
    pub legato_inverted: bool,
    /// Where a voice that was already sounding restarts.
    pub steal_restart: StealRestart,
}

impl EnvelopeParams {
    /// An amplifier envelope as an initialised patch has it: no delay,
    /// 1 ms attack, no hold, 1 s decay, full sustain, 15 ms release, curves
    /// 50 %, 66.6 % and 66.6 %, starting and ending at silence, restarting
    /// a stolen voice from its current level.
    pub const AMP: Self = Self {
        unit: TimeUnit::Milliseconds,
        delay: 0.0,
        attack: 1.0,
        hold: 0.0,
        decay: 1000.0,
        sustain: 1.0,
        release: 15.0,
        attack_curve: 0.5,
        decay_curve: 0.666,
        release_curve: 0.666,
        start: 0.0,
        end: 0.0,
        legato_inverted: false,
        steal_restart: StealRestart::CurrentLevel,
    };

    /// A modulation envelope as an initialised patch has it: as
    /// [`AMP`](Self::AMP) but with a 0.5 ms attack, restarting a stolen
    /// voice from its start level.
    pub const MODULATION: Self = Self {
        attack: 0.5,
        steal_restart: StealRestart::StartLevel,
        ..Self::AMP
    };
}

impl Default for EnvelopeParams {
    fn default() -> Self {
        Self::AMP
    }
}

/// The clock envelope times are measured against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// Samples per second.
    pub sample_rate: f64,
    /// Beats per minute, for [`TimeUnit::Tempo`].
    pub tempo: f64,
    /// The global time scale every time is multiplied by, from
    /// [`MIN_TIME_SCALE`] to [`MAX_TIME_SCALE`] (clamped).
    pub scale: f64,
}

impl Timing {
    /// `sample_rate`, at 120 beats per minute and a scale of 100 %.
    #[must_use]
    pub const fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            tempo: 120.0,
            scale: 1.0,
        }
    }

    /// The same, at `tempo` beats per minute.
    #[must_use]
    pub const fn with_tempo(self, tempo: f64) -> Self {
        Self { tempo, ..self }
    }

    /// The same, with every time multiplied by `scale`.
    #[must_use]
    pub const fn with_scale(self, scale: f64) -> Self {
        Self { scale, ..self }
    }

    /// `value` in `unit`, scaled, in whole samples.
    #[must_use]
    pub fn samples(&self, value: f32, unit: TimeUnit) -> u64 {
        let seconds = match unit {
            TimeUnit::Milliseconds => f64::from(value) / 1000.0,
            TimeUnit::Tempo => f64::from(value) * 60.0 / self.tempo.max(1.0),
        };
        let scale = self.scale.clamp(MIN_TIME_SCALE, MAX_TIME_SCALE);
        // `max` turns NaN into 0, and the cast saturates.
        (seconds.max(0.0) * scale * self.sample_rate.max(0.0)).round() as u64
    }
}

/// `value` clamped to 0 to 1, with NaN as 0 and no negative zero.
fn unit(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0) + 0.0
    }
}

/// The note division nearest `beats` (in quarter notes), by ratio: a whole
/// number of bars of 4/4 from 1/256 of a bar to 32 bars, plain, dotted or
/// triplet. Zero stays zero. Switching an envelope to tempo converts each
/// time with this, and an editor snaps tempo times to it.
#[must_use]
pub fn nearest_division(beats: f32) -> f32 {
    if beats.is_nan() || beats <= 0.0 {
        return 0.0;
    }
    let mut best = 4.0 / 256.0 * (2.0 / 3.0);
    for exponent in -8..=5 {
        let plain = 4.0 * 2f32.powi(exponent);
        for candidate in [plain * (2.0 / 3.0), plain, plain * 1.5] {
            if candidate <= 128.0 && (candidate / beats).ln().abs() < (best / beats).ln().abs() {
                best = candidate;
            }
        }
    }
    best
}

/// The note division nearest `milliseconds` at `tempo` beats per minute,
/// in beats.
#[must_use]
pub fn division_for_milliseconds(milliseconds: f32, tempo: f64) -> f32 {
    nearest_division((f64::from(milliseconds) / 1000.0 * tempo / 60.0) as f32)
}

/// One voice's envelope.
#[derive(Clone, Debug)]
pub struct Envelope {
    lane: Lane,
    seg: Segment,
    timing: Timing,
}

impl Envelope {
    /// An idle envelope.
    #[must_use]
    pub fn new(params: EnvelopeParams, timing: Timing) -> Self {
        let (lane, seg) = Lane::new(params);
        Self { lane, seg, timing }
    }

    /// Its settings.
    #[must_use]
    pub fn params(&self) -> &EnvelopeParams {
        &self.lane.params
    }

    /// Changes its settings from the next sample. The current stage
    /// carries on unless its length or target changed; then it continues
    /// from the current level to the new target over what remains of its
    /// new length (ending at once if that has passed). A new sustain level
    /// applies at once while sustaining.
    pub fn set_params(&mut self, params: EnvelopeParams) {
        self.lane.params = params;
        self.lane.replan(&mut self.seg, &self.timing);
    }

    /// Changes the sample rate, tempo or global scale, from the next
    /// sample, re-planning the current stage as [`set_params`] does.
    ///
    /// [`set_params`]: Self::set_params
    pub fn set_timing(&mut self, timing: Timing) {
        self.timing = timing;
        self.lane.replan(&mut self.seg, &self.timing);
    }

    /// A note starts.
    pub fn trigger(&mut self, trigger: Trigger) {
        self.lane.trigger(&mut self.seg, &self.timing, trigger);
    }

    /// The note ends: the release starts from the current level.
    pub fn release(&mut self) {
        self.lane.release(&mut self.seg, &self.timing);
    }

    /// Goes idle at once.
    pub fn reset(&mut self) {
        self.lane.reset(&mut self.seg, &self.timing);
    }

    /// Where it is.
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.lane.stage
    }

    /// The level it outputs next.
    #[must_use]
    pub fn level(&self) -> f32 {
        self.seg.level()
    }

    /// Whether it's idle, so its voice can end.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.lane.stage == Stage::Idle
    }

    /// Writes its next `out.len()` levels.
    pub fn process(&mut self, mut out: &mut [f32]) {
        while !out.is_empty() {
            let timed = self.seg.is_timed();
            let run = if timed {
                out.len().min(self.seg.left as usize)
            } else {
                out.len()
            };
            let (now, rest) = out.split_at_mut(run);
            let Segment {
                mut u,
                r,
                d,
                scale,
                base,
                ..
            } = self.seg;
            for y in now {
                *y = u * scale + base;
                u = u * r + d;
            }
            self.seg.u = u;
            if timed {
                // `run` is at most `left`.
                self.seg.left -= run as u32;
                if self.seg.left == 0 {
                    self.lane.advance(&mut self.seg, &self.timing);
                }
            }
            out = rest;
        }
    }

    /// Its next level.
    pub fn next_level(&mut self) -> f32 {
        let mut out = [0.0];
        self.process(&mut out);
        out[0]
    }
}

/// Lanes a bank is padded to: the widest SIMD level's.
const LANE_GROUP: usize = 16;

/// One envelope per voice, run across SIMD lanes.
///
/// [`process`](Self::process) writes frame-major: sample `f` of voice `v` at
/// `out[f * stride + v]`, where [`stride`](Self::stride) is the voice count
/// rounded up to a multiple of 16. That is the layout a voice engine that
/// keeps its voices in lanes reads.
#[derive(Clone, Debug)]
pub struct EnvelopeBank {
    voices: usize,
    stride: usize,
    lanes: Vec<Lane>,
    u: Vec<f32>,
    r: Vec<f32>,
    d: Vec<f32>,
    scale: Vec<f32>,
    base: Vec<f32>,
    left: Vec<u32>,
    timing: Timing,
}

impl EnvelopeBank {
    /// `voices` idle envelopes, each with `params`. Allocates; call it off
    /// the audio thread. It also settles the SIMD level, so the first
    /// [`process`](Self::process) needn't.
    #[must_use]
    pub fn new(voices: usize, params: EnvelopeParams, timing: Timing) -> Self {
        let stride = voices.div_ceil(LANE_GROUP).max(1) * LANE_GROUP;
        let (lane, seg) = Lane::new(params);
        let _ = simd::level();
        let mut bank = Self {
            voices,
            stride,
            lanes: vec![lane; voices],
            u: vec![0.0; stride],
            r: vec![1.0; stride],
            d: vec![0.0; stride],
            scale: vec![0.0; stride],
            base: vec![0.0; stride],
            left: vec![UNTIMED; stride],
            timing,
        };
        for voice in 0..voices {
            bank.store(voice, seg);
        }
        bank
    }

    /// How many voices it has.
    #[must_use]
    pub fn voices(&self) -> usize {
        self.voices
    }

    /// The distance between one sample's levels and the next's in
    /// [`process`](Self::process)'s output.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.stride
    }

    fn load(&self, voice: usize) -> Segment {
        Segment {
            u: self.u[voice],
            r: self.r[voice],
            d: self.d[voice],
            scale: self.scale[voice],
            base: self.base[voice],
            left: self.left[voice],
        }
    }

    fn store(&mut self, voice: usize, seg: Segment) {
        self.u[voice] = seg.u;
        self.r[voice] = seg.r;
        self.d[voice] = seg.d;
        self.scale[voice] = seg.scale;
        self.base[voice] = seg.base;
        self.left[voice] = seg.left;
    }

    /// Runs `f` on `voice`'s plan and segment.
    fn with(&mut self, voice: usize, f: impl FnOnce(&mut Lane, &mut Segment, &Timing)) {
        let mut seg = self.load(voice);
        f(&mut self.lanes[voice], &mut seg, &self.timing);
        self.store(voice, seg);
    }

    /// `voice`'s settings.
    ///
    /// # Panics
    ///
    /// If there's no such voice, as for every method taking one.
    #[must_use]
    pub fn params(&self, voice: usize) -> &EnvelopeParams {
        &self.lanes[voice].params
    }

    /// Changes `voice`'s settings, as [`Envelope::set_params`] does.
    pub fn set_params(&mut self, voice: usize, params: EnvelopeParams) {
        self.with(voice, |lane, seg, timing| {
            lane.params = params;
            lane.replan(seg, timing);
        });
    }

    /// Changes every voice's settings.
    pub fn set_all_params(&mut self, params: EnvelopeParams) {
        for voice in 0..self.voices {
            self.set_params(voice, params);
        }
    }

    /// Changes the sample rate, tempo or global scale for every voice, as
    /// [`Envelope::set_timing`] does.
    pub fn set_timing(&mut self, timing: Timing) {
        self.timing = timing;
        for voice in 0..self.voices {
            self.with(voice, Lane::replan);
        }
    }

    /// A note starts on `voice`.
    pub fn trigger(&mut self, voice: usize, trigger: Trigger) {
        self.with(voice, |lane, seg, timing| {
            lane.trigger(seg, timing, trigger)
        });
    }

    /// `voice`'s note ends.
    pub fn release(&mut self, voice: usize) {
        self.with(voice, Lane::release);
    }

    /// `voice` goes idle at once.
    pub fn reset(&mut self, voice: usize) {
        self.with(voice, Lane::reset);
    }

    /// `voice` takes `from`'s envelope exactly as it stands, so the two
    /// move together from here: for paraphonic legato, where a new note
    /// joins the held note's envelope.
    pub fn join(&mut self, voice: usize, from: usize) {
        self.lanes[voice] = self.lanes[from].clone();
        let seg = self.load(from);
        self.store(voice, seg);
    }

    /// Where `voice` is.
    #[must_use]
    pub fn stage(&self, voice: usize) -> Stage {
        self.lanes[voice].stage
    }

    /// The level `voice` outputs next.
    #[must_use]
    pub fn level(&self, voice: usize) -> f32 {
        self.load(voice).level()
    }

    /// Whether `voice` is idle.
    #[must_use]
    pub fn is_idle(&self, voice: usize) -> bool {
        self.lanes[voice].stage == Stage::Idle
    }

    /// Writes the next `out.len() / stride` samples of every voice,
    /// frame-major (see the type's documentation); lanes past the last
    /// voice are written as 0.
    ///
    /// # Panics
    ///
    /// If `out.len()` isn't a multiple of [`stride`](Self::stride).
    pub fn process(&mut self, out: &mut [f32]) {
        // `level` only returns levels this CPU has.
        let _ = self.process_at(simd::level(), out);
    }

    /// [`process`](Self::process) at a given SIMD level, for comparing
    /// levels. Returns `false`, having done nothing, if this CPU lacks it.
    ///
    /// # Panics
    ///
    /// If `out.len()` isn't a multiple of [`stride`](Self::stride).
    #[must_use]
    pub fn process_at(&mut self, level: Level, out: &mut [f32]) -> bool {
        assert_eq!(
            out.len() % self.stride,
            0,
            "the output holds whole frames of every lane"
        );
        if !level.is_supported() {
            return false;
        }
        let frames = out.len() / self.stride;
        let mut done = 0;
        while done < frames {
            let run = self.left[..self.voices]
                .iter()
                .filter(|&&left| left != UNTIMED)
                .fold(frames - done, |run, &left| run.min(left as usize));
            let rows = &mut out[done * self.stride..(done + run) * self.stride];
            let kernel = Run {
                out: rows,
                stride: self.stride,
                u: &mut self.u,
                r: &self.r,
                d: &self.d,
                scale: &self.scale,
                base: &self.base,
            };
            if simd::dispatch_at(level, kernel).is_err() {
                return false;
            }
            for voice in 0..self.voices {
                if self.left[voice] != UNTIMED {
                    // `run` is at most every timed lane's `left`.
                    self.left[voice] -= run as u32;
                    if self.left[voice] == 0 {
                        self.with(voice, Lane::advance);
                    }
                }
            }
            done += run;
        }
        true
    }
}

/// Draws one run, in which no lane's segment ends before the last frame.
struct Run<'a> {
    out: &'a mut [f32],
    stride: usize,
    u: &'a mut [f32],
    r: &'a [f32],
    d: &'a [f32],
    scale: &'a [f32],
    base: &'a [f32],
}

impl Kernel for Run<'_> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        let Self {
            out,
            stride,
            u: us,
            r,
            d,
            scale,
            base,
        } = self;
        for group in (0..stride).step_by(S::LANES) {
            let lanes = group..group + S::LANES;
            let base_s = S::load(&base[lanes.clone()]);
            if scale[lanes.clone()].iter().all(|&s| s == 0.0) {
                // Nothing moving: the levels are the bases.
                for frame in out.chunks_exact_mut(stride) {
                    base_s.store(&mut frame[lanes.clone()]);
                }
                continue;
            }
            let (r, d, scale) = (
                S::load(&r[lanes.clone()]),
                S::load(&d[lanes.clone()]),
                S::load(&scale[lanes.clone()]),
            );
            let mut u = S::load(&us[lanes.clone()]);
            for frame in out.chunks_exact_mut(stride) {
                // Not `mul_add`, so every level gives the scalar path's bits.
                (u * scale + base_s).store(&mut frame[lanes.clone()]);
                u = u * r + d;
            }
            u.store(&mut us[lanes]);
        }
    }
}
