//! The voice block: every voice's per-note values in structure-of-arrays
//! form, one `f32` lane per voice copy, for SIMD kernels.

use lspk_plugin::Note;

use crate::settings::{MAX_VOICES, Settings};

/// Every lane array's length is a multiple of this: the widest SIMD vector
/// `lspk-dsp` dispatches (AVX-512's 16 lanes), so a kernel at any level
/// loads whole vectors without a remainder loop.
pub const LANE_ALIGN: usize = 16;

/// Where a voice is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VoiceState {
    /// Not playing; free for a new note.
    #[default]
    Free,
    /// Its key is down.
    Held,
    /// Its key is up, but the sustain pedal holds it.
    Sustained,
    /// Released: in its release until the synthesiser reports it silent
    /// ([`VoiceBlock::end`]).
    Released,
}

impl VoiceState {
    /// Whether its gate is open: held or sustained.
    #[must_use]
    pub const fn is_gated(self) -> bool {
        matches!(self, Self::Held | Self::Sustained)
    }
}

/// What a voice is playing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VoiceInfo {
    /// Where it is in its life.
    pub state: VoiceState,
    /// The note it plays, as the host named it: port, channel, key and the
    /// host's note ID, for [`OutputEvent::NoteEnd`](lspk_plugin::OutputEvent).
    pub note: Note,
    /// The key it plays, from 0 to 127.
    pub key: u8,
    /// When its note started, counted in notes: a newer note has a higher
    /// age.
    pub age: u64,
}

/// The values a host's controllers hold that aren't per note: pitch bend,
/// channel pressure and the controllers (in MPE, from the master channel;
/// otherwise from every channel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controllers {
    /// Pitch bend, from −1 to +1, as received (the pitch lanes carry it
    /// smoothed and scaled).
    pub pitch_bend: f32,
    /// Channel pressure, from 0 to 1.
    pub channel_pressure: f32,
    /// Whether the sustain pedal (controller 64) is down.
    pub sustain: bool,
    cc: [f32; 128],
}

impl Default for Controllers {
    fn default() -> Self {
        let mut cc = [0.0; 128];
        // Expression and volume start full, as MIDI's reset leaves them.
        cc[7] = 100.0 / 127.0;
        cc[11] = 1.0;
        Self {
            pitch_bend: 0.0,
            channel_pressure: 0.0,
            sustain: false,
            cc,
        }
    }
}

impl Controllers {
    /// Controller `number`'s value, from 0 to 1 (0 for numbers above 127).
    #[must_use]
    pub fn cc(&self, number: u8) -> f32 {
        self.cc.get(usize::from(number)).copied().unwrap_or(0.0)
    }

    pub(crate) fn set_cc(&mut self, number: u8, value: f32) {
        if let Some(slot) = self.cc.get_mut(usize::from(number)) {
            *slot = value;
        }
    }
}

/// The default value of each per-note expression lane.
pub(crate) const DEFAULT_PRESSURE: f32 = 0.0;
pub(crate) const DEFAULT_TIMBRE: f32 = 0.5;
pub(crate) const DEFAULT_VOLUME: f32 = 1.0;
pub(crate) const DEFAULT_PAN: f32 = 0.5;
pub(crate) const DEFAULT_VIBRATO: f32 = 0.0;
pub(crate) const DEFAULT_EXPRESSION: f32 = 1.0;

/// The lane arrays, each `lane_count` long.
#[derive(Debug)]
pub(crate) struct Lanes {
    pub(crate) pitch: Box<[f32]>,
    pub(crate) pitch_step: Box<[f32]>,
    pub(crate) velocity: Box<[f32]>,
    pub(crate) release_velocity: Box<[f32]>,
    pub(crate) gate: Box<[f32]>,
    pub(crate) active: Box<[f32]>,
    pub(crate) pressure: Box<[f32]>,
    pub(crate) timbre: Box<[f32]>,
    pub(crate) volume: Box<[f32]>,
    pub(crate) pan: Box<[f32]>,
    pub(crate) vibrato: Box<[f32]>,
    pub(crate) expression: Box<[f32]>,
}

/// Which lane array a write goes to.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Lane {
    Velocity,
    ReleaseVelocity,
    Gate,
    Active,
    Pressure,
    Timbre,
    Volume,
    Pan,
    Vibrato,
    Expression,
}

/// Every voice's values, laid out for SIMD.
///
/// Each voice owns [`lanes_per_voice`](Self::lanes_per_voice) consecutive
/// lanes (its unison copies; one until unison is on), voice *v*'s starting
/// at lane `v × lanes_per_voice`, and every per-note value is written to
/// all of a voice's lanes, so a kernel reads it with plain vector loads.
/// Every array is [`lane_count`](Self::lane_count) long, a multiple of
/// [`LANE_ALIGN`]. New notes take the lowest free voice, so the active
/// lanes stay at the bottom: a kernel processes `0..span(S::LANES)` and
/// multiplies by [`active`](Self::active) (or skips inactive vectors).
///
/// Per-note values are held for a whole render call, except pitch, which
/// moves by [`pitch_step`](Self::pitch_step) each frame.
#[derive(Debug)]
pub struct VoiceBlock {
    pub(crate) lanes: Lanes,
    pub(crate) info: [VoiceInfo; MAX_VOICES],
    pub(crate) capacity: usize,
    pub(crate) lanes_per_voice: usize,
    pub(crate) lane_count: usize,
    pub(crate) active_mask: u32,
    pub(crate) ending: u32,
    pub(crate) settings: Settings,
    pub(crate) controllers: Controllers,
}

/// A lane array accessor, with its documentation.
macro_rules! lane {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[must_use]
        #[inline]
        pub fn $name(&self) -> &[f32] {
            &self.lanes.$name
        }
    };
}

impl VoiceBlock {
    pub(crate) fn new(capacity: usize, lanes_per_voice: usize) -> Self {
        let lane_count = (capacity * lanes_per_voice).div_ceil(LANE_ALIGN) * LANE_ALIGN;
        let filled = |value: f32| vec![value; lane_count].into_boxed_slice();
        Self {
            lanes: Lanes {
                pitch: filled(0.0),
                pitch_step: filled(0.0),
                velocity: filled(0.0),
                release_velocity: filled(0.0),
                gate: filled(0.0),
                active: filled(0.0),
                pressure: filled(DEFAULT_PRESSURE),
                timbre: filled(DEFAULT_TIMBRE),
                volume: filled(DEFAULT_VOLUME),
                pan: filled(DEFAULT_PAN),
                vibrato: filled(DEFAULT_VIBRATO),
                expression: filled(DEFAULT_EXPRESSION),
            },
            info: [VoiceInfo::default(); MAX_VOICES],
            capacity,
            lanes_per_voice,
            lane_count,
            active_mask: 0,
            ending: 0,
            settings: Settings::default(),
            controllers: Controllers::default(),
        }
    }

    /// How many voices it holds.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many lanes each voice owns.
    #[must_use]
    pub const fn lanes_per_voice(&self) -> usize {
        self.lanes_per_voice
    }

    /// Every lane array's length: a multiple of [`LANE_ALIGN`].
    #[must_use]
    pub const fn lane_count(&self) -> usize {
        self.lane_count
    }

    /// The lanes a kernel with `lanes`-wide vectors must cover to reach
    /// every active voice: up to the highest active voice's last lane,
    /// rounded up to a multiple of `lanes`. `lanes` must divide
    /// [`LANE_ALIGN`].
    #[must_use]
    pub fn span(&self, lanes: usize) -> usize {
        let voices = (u32::BITS - self.active_mask.leading_zeros()) as usize;
        (voices * self.lanes_per_voice).div_ceil(lanes) * lanes
    }

    /// Which voices are active, one bit per voice (bit *v* for voice *v*).
    #[must_use]
    pub const fn active_mask(&self) -> u32 {
        self.active_mask
    }

    /// How many voices are active.
    #[must_use]
    pub const fn active_count(&self) -> usize {
        self.active_mask.count_ones() as usize
    }

    /// The active voices, lowest first.
    pub fn active_voices(&self) -> impl Iterator<Item = usize> + use<> {
        let mut mask = self.active_mask;
        std::iter::from_fn(move || {
            (mask != 0).then(|| {
                let voice = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                voice
            })
        })
    }

    /// What voice `voice` is playing.
    ///
    /// # Panics
    ///
    /// If `voice` is [`MAX_VOICES`] or more.
    #[must_use]
    pub fn info(&self, voice: usize) -> &VoiceInfo {
        &self.info[voice]
    }

    /// The lanes voice `voice` owns.
    #[must_use]
    pub const fn lanes_of(&self, voice: usize) -> std::ops::Range<usize> {
        voice * self.lanes_per_voice..(voice + 1) * self.lanes_per_voice
    }

    /// Reports voice `voice` silent: it ends after the current render (or
    /// event) call, with a [`VoiceEventKind::End`](crate::VoiceEventKind)
    /// at that frame. Call it when a released voice's output and tails have
    /// fallen below the noise floor.
    pub fn end(&mut self, voice: usize) {
        if voice < self.capacity && self.active_mask & (1 << voice) != 0 {
            self.ending |= 1 << voice;
        }
    }

    /// How the voices are played.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// How the voices are played, to change.
    pub const fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// The controllers that aren't per note.
    #[must_use]
    pub const fn controllers(&self) -> &Controllers {
        &self.controllers
    }

    lane! {
        /// Pitch at the render call's first frame, as a MIDI note number in
        /// semitones (69 is A4): the key, with glide, pitch bend, MPE
        /// per-note bend and CLAP tuning.
        pitch
    }
    lane! {
        /// How much [`pitch`](Self::pitch) changes each frame through the
        /// render call: pitch is linear between control points
        /// ([`CONTROL_INTERVAL`](crate::CONTROL_INTERVAL) frames apart).
        pitch_step
    }
    lane! {
        /// Note-on velocity, from 0 to 1.
        velocity
    }
    lane! {
        /// Note-off velocity, from 0 to 1, once released.
        release_velocity
    }
    lane! {
        /// 1 while the note is held or sustained, 0 once released.
        gate
    }
    lane! {
        /// 1 for an active voice's lanes, 0 for the rest.
        active
    }
    lane! {
        /// Per-note pressure, from 0 to 1: polyphonic aftertouch, MPE
        /// channel pressure or CLAP's pressure expression.
        pressure
    }
    lane! {
        /// Per-note timbre, from 0 to 1 (0.5 by default): MPE's controller
        /// 74 or CLAP's brightness expression.
        timbre
    }
    lane! {
        /// CLAP's volume expression: a linear gain from 0 to 4, 1 by
        /// default.
        volume
    }
    lane! {
        /// CLAP's pan expression, from 0 (left) to 1 (right), 0.5 by default.
        pan
    }
    lane! {
        /// CLAP's vibrato expression, from 0 to 1, 0 by default.
        vibrato
    }
    lane! {
        /// CLAP's "expression" expression, from 0 to 1, 1 by default.
        expression
    }

    /// Writes `value` to every lane of `voice` in `lane`.
    pub(crate) fn write(&mut self, lane: Lane, voice: usize, value: f32) {
        let range = self.lanes_of(voice);
        let array = match lane {
            Lane::Velocity => &mut self.lanes.velocity,
            Lane::ReleaseVelocity => &mut self.lanes.release_velocity,
            Lane::Gate => &mut self.lanes.gate,
            Lane::Active => &mut self.lanes.active,
            Lane::Pressure => &mut self.lanes.pressure,
            Lane::Timbre => &mut self.lanes.timbre,
            Lane::Volume => &mut self.lanes.volume,
            Lane::Pan => &mut self.lanes.pan,
            Lane::Vibrato => &mut self.lanes.vibrato,
            Lane::Expression => &mut self.lanes.expression,
        };
        array[range].fill(value);
    }

    /// Writes a voice's pitch and pitch step.
    #[inline]
    pub(crate) fn write_pitch(&mut self, voice: usize, pitch: f32, step: f32) {
        let range = self.lanes_of(voice);
        self.lanes.pitch[range.clone()].fill(pitch);
        self.lanes.pitch_step[range].fill(step);
    }
}
