//! The wavetable oscillator: every voice and unison copy of one oscillator
//! in SIMD lanes, reading a shared [`Table`].
//!
//! | Item | What it is |
//! |---|---|
//! | [`Oscillator`] | The lanes' phases, keys and mipmap levels, and the block render: scalar, or across lanes at any SIMD level |
//! | [`Params`] | The oscillator's controls, shared by its voices: pitch offsets, frame, Smooth frames, phase and phase random, the phase mode, key tracking, level |
//! | [`Inputs`] | What a render reads per lane: pitch, its glide step, and audio-rate pitch and frame modulation |
//! | [`Onset`] | How a voice starts a note, which decides whether its phase restarts |
//! | [`PhaseStage`], [`WaveStage`] | Hooks for shapers: one bends the phase before the read (and may sum several weighted [`Reads`]), the other the value after it |
//!
//! # Lanes
//!
//! Each voice owns [`Layout::lanes_per_voice`] consecutive lanes (its
//! unison copies), voice *v*'s starting at lane `v × lanes_per_voice`, as
//! the kit's voice block lays them out. Every per-lane array is
//! [`Oscillator::lane_count`] long, a multiple of 16, so a kernel at any
//! level reads whole vectors. Outputs and audio-rate inputs are frame by
//! frame: `out[k × lane_count + l]` is lane `l` at frame `k`.
//!
//! # Mipmaps without clicks
//!
//! Each lane picks its mipmap level every [`SUB_BLOCK`] frames, from the
//! highest phase increment in that stretch and the next (past the end of
//! a render, the last stretch's rise carried on), so the level it leaves
//! stays clean while it fades out. When the level changes, the lane
//! crossfades from the old level to the new over [`SUB_BLOCK`] frames,
//! however the renders split them, along a curve whose first two
//! derivatives are 0 at both ends, so fast pitch modulation never switches
//! a band of harmonics on or off in one sample.

mod kernel;
mod scalar;

use lspk_dsp::simd::{self, F32s, Level as SimdLevel};

use crate::{SharedTable, Table};

/// How many frames a lane keeps its mipmap level for, and how long its
/// crossfade to a new level lasts: 0.67 ms at 48 kHz.
pub const SUB_BLOCK: usize = 32;

/// The most unison copies a voice has (engine-core section 4).
pub const MAX_UNISON: usize = 16;

/// The most voices an oscillator holds.
pub const MAX_VOICES: usize = 32;

/// Every per-lane array's length is a multiple of this: the widest SIMD
/// vector `lspk-dsp` dispatches.
pub const LANE_ALIGN: usize = 16;

/// The most mipmap levels a table has.
const MAX_LEVELS: usize = 32;

/// How a lane with no level yet is marked: its first stretch takes its
/// level without a crossfade.
const NO_LEVEL: u32 = u32::MAX;

/// How many voices, and how many lanes each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Voices to hold, from 1 to [`MAX_VOICES`].
    pub voices: usize,
    /// Lanes per voice: its unison copies, from 1 to [`MAX_UNISON`].
    pub lanes_per_voice: usize,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            voices: MAX_VOICES,
            lanes_per_voice: 1,
        }
    }
}

/// Where a note's phase starts (engine-core R19).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PhaseMode {
    /// Every note starts from [`Params::phase`], offset by
    /// [`Params::phase_random`], separately for each unison copy.
    #[default]
    NoteStart,
    /// A note carries on from the phase its lanes' previous note left.
    Continue,
    /// Each note starts each unison copy from its own stored phase
    /// ([`Oscillator::voice_phases_mut`]); Phase and Phase random don't
    /// apply (R20).
    PerVoice,
}

/// How a voice starts a note, as the voice manager reports it. With
/// [`Params::phase_legato`] off, a fresh voice and a polyphonic legato
/// join restart the phase and the rest carry on (engine-core R18); Phase
/// legato inverts the steal and the join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Onset {
    /// From silence: a free voice.
    Fresh,
    /// A sounding voice taken for a new note, legato off.
    Steal,
    /// Mono without legato: the sounding voice re-pitched and retriggered.
    /// Treated as a steal.
    Retrigger,
    /// Mono legato: the sounding voice re-pitched without a retrigger; its
    /// phase always carries on.
    Legato,
    /// Polyphonic legato: a new voice joining a held note.
    Join,
}

impl Onset {
    /// Whether this onset restarts the phase, with Phase legato `inverted`.
    #[must_use]
    pub const fn restarts(self, inverted: bool) -> bool {
        match self {
            Self::Fresh => true,
            Self::Steal | Self::Retrigger => inverted,
            Self::Join => !inverted,
            Self::Legato => false,
        }
    }
}

/// The oscillator's controls, in real units, shared by every voice.
/// Engine-core sections 1 and 2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Octave, −4 to +4 (R1).
    pub octave: i32,
    /// Semitone, −12 to +12 (R1).
    pub semitone: i32,
    /// Fine, −100 to +100 cents (R1).
    pub fine: f32,
    /// Sweep (coarse), −64 to +64 semitones, continuous (R1).
    pub coarse: f32,
    /// Frame, from 1 to the table's frame count (R14); between frames
    /// plays the nearest with Smooth frames off.
    pub frame: f32,
    /// Smooth frames: interpolate continuously between neighbouring
    /// frames (R14).
    pub smooth: bool,
    /// Phase, 0 to 360 degrees: where a note starts in the frame (R16).
    pub phase: f32,
    /// Phase random, 0 to 100 %: a random offset added to Phase for each
    /// new note and unison copy (R17).
    pub phase_random: f32,
    /// Where a note's phase starts (R19).
    pub phase_mode: PhaseMode,
    /// Phase legato: inverts which onsets restart the phase (R18).
    pub phase_legato: bool,
    /// Key tracking; off plays MIDI note 0, still following pitch bend and
    /// modulation (R5, R6).
    pub key_track: bool,
    /// Level, 0 to 100 %, as gain n² (R7).
    pub level: f32,
    /// The frequency of A4, in hertz (R136).
    pub reference: f32,
}

impl Default for Params {
    /// The defaults of a new patch: Phase 180°, Phase random 100 %, Level
    /// 75 % (engine-core sections 1 and 2, as observed).
    fn default() -> Self {
        Self {
            octave: 0,
            semitone: 0,
            fine: 0.0,
            coarse: 0.0,
            frame: 1.0,
            smooth: false,
            phase: 180.0,
            phase_random: 100.0,
            phase_mode: PhaseMode::NoteStart,
            phase_legato: false,
            key_track: true,
            level: 75.0,
            reference: 440.0,
        }
    }
}

impl Params {
    /// The pitch offset Octave, Semitone, Fine and Sweep add, in semitones
    /// (R1).
    #[must_use]
    pub fn tune(&self) -> f32 {
        12.0 * self.octave as f32 + self.semitone as f32 + self.fine / 100.0 + self.coarse
    }

    /// The output gain Level gives: n² (R7).
    #[must_use]
    pub fn gain(&self) -> f32 {
        let n = (self.level / 100.0).clamp(0.0, 1.0);
        n * n
    }
}

/// What a render reads for each lane. Pitches are MIDI note numbers in
/// semitones (69 is A4 at [`Params::reference`]), as the kit's voice block
/// gives them, with glide, bend and expression; Octave, Semitone, Fine and
/// Sweep are added here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inputs<'a> {
    /// Lanes to render, from lane 0: at most [`Oscillator::lane_count`].
    /// Rounded up to a whole vector.
    pub lanes: usize,
    /// Each lane's pitch at the first frame.
    pub pitch: &'a [f32],
    /// How far each lane's pitch moves per frame (glide), if it moves.
    pub pitch_step: Option<&'a [f32]>,
    /// Pitch modulation at audio rate, in semitones, frame by frame
    /// (`frames × lane_count` values).
    pub pitch_mod: Option<&'a [f32]>,
    /// Frame modulation, in frames, frame by frame (`frames ×
    /// lane_count` values).
    pub frame_mod: Option<&'a [f32]>,
}

/// The most table reads a [`PhaseStage`] asks for at one frame.
pub const MAX_READS: usize = 4;

/// What a [`PhaseStage`] has a vector of lanes read at one frame: the sum
/// of up to [`MAX_READS`] reads of the table, each at its own phase and
/// weighted by its own gain. Shapers use more than one read to crossfade
/// between two branches of a phase map (a sync reset, a pulse's edge) or
/// to mix a wave with itself half a cycle on.
#[derive(Clone, Copy, Debug)]
pub struct Reads<S: F32s> {
    /// Where each read is, per lane.
    pub phase: [S::U32s; MAX_READS],
    /// Each read's gain, per lane.
    pub gain: [S; MAX_READS],
    /// How many reads to sum, from 1 to [`MAX_READS`]; 0 means one read at
    /// `phase[0]` with no gain applied.
    pub count: usize,
}

impl<S: F32s> Reads<S> {
    /// One read at `phase`, unweighted.
    #[inline(always)]
    #[must_use]
    pub fn one(phase: S::U32s) -> Self {
        Self {
            phase: [phase; MAX_READS],
            gain: [S::splat(1.0); MAX_READS],
            count: 0,
        }
    }
}

/// [`Reads`] for one lane, for the scalar path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaneReads {
    /// Where each read is.
    pub phase: [u32; MAX_READS],
    /// Each read's gain.
    pub gain: [f32; MAX_READS],
    /// How many reads to sum; 0 means one read at `phase[0]`, unweighted.
    pub count: usize,
}

impl LaneReads {
    /// One read at `phase`, unweighted.
    #[must_use]
    pub const fn one(phase: u32) -> Self {
        Self {
            phase: [phase; MAX_READS],
            gain: [1.0; MAX_READS],
            count: 0,
        }
    }
}

/// A stage that bends each lane's phase before the table is read: where
/// R1.4's phase shapers (sync, bend, pulse width, mirror, remaps) plug in.
///
/// Mark the methods `#[inline(always)]`, so each SIMD level's kernel
/// compiles them with its own instructions.
pub trait PhaseStage {
    /// The phase to read lanes `lanes..lanes + S::LANES` at, at frame
    /// `frame`, from their running `phase` and its `increment` per frame
    /// ([`CYCLE`](crate::CYCLE) is a whole cycle).
    fn shape<S: F32s>(
        &mut self,
        lanes: usize,
        frame: usize,
        phase: S::U32s,
        increment: S::U32s,
    ) -> S::U32s;

    /// The same for lane `lane` alone, for the scalar path.
    fn shape_lane(&mut self, lane: usize, frame: usize, phase: u32, increment: u32) -> u32;

    /// The reads lanes `lanes..lanes + S::LANES` sum at frame `frame`: by
    /// default one, at [`shape`](Self::shape)'s phase.
    #[inline(always)]
    fn reads<S: F32s>(
        &mut self,
        lanes: usize,
        frame: usize,
        phase: S::U32s,
        increment: S::U32s,
    ) -> Reads<S> {
        Reads::one(self.shape::<S>(lanes, frame, phase, increment))
    }

    /// The same for lane `lane` alone, for the scalar path.
    #[inline(always)]
    fn reads_lane(&mut self, lane: usize, frame: usize, phase: u32, increment: u32) -> LaneReads {
        LaneReads::one(self.shape_lane(lane, frame, phase, increment))
    }

    /// How many times faster than its running phase lane `lane` reads the
    /// table at most over frames `frames` of this render: the stage's
    /// steepest slope. The lane picks its mipmap level for its increment
    /// times this, so a stage that reads faster than the note still reads
    /// a level whose harmonics stay below Nyquist. By default 1.
    #[inline(always)]
    fn stretch(&mut self, lane: usize, frames: std::ops::Range<usize>) -> f32 {
        let _ = (lane, frames);
        1.0
    }
}

/// A stage that bends each lane's value after the read, before Level: where
/// R1.4's distortion and filter shapers plug in.
///
/// Mark the methods `#[inline(always)]`, as for [`PhaseStage`].
pub trait WaveStage {
    /// Lanes `lanes..lanes + S::LANES`'s value at frame `frame`, shaped.
    fn shape<S: F32s>(&mut self, lanes: usize, frame: usize, value: S) -> S;

    /// The same for lane `lane` alone, for the scalar path.
    fn shape_lane(&mut self, lane: usize, frame: usize, value: f32) -> f32;

    /// [`shape`](Self::shape), also told the lanes' phase `increment` per
    /// frame, for a stage that follows the note's pitch (a filter whose
    /// cutoff is a harmonic of the note). By default the increment is
    /// ignored.
    #[inline(always)]
    fn shape_at<S: F32s>(&mut self, lanes: usize, frame: usize, increment: S::U32s, value: S) -> S {
        let _ = increment;
        self.shape::<S>(lanes, frame, value)
    }

    /// The same for lane `lane` alone, for the scalar path.
    #[inline(always)]
    fn shape_at_lane(&mut self, lane: usize, frame: usize, increment: u32, value: f32) -> f32 {
        let _ = increment;
        self.shape_lane(lane, frame, value)
    }
}

/// A stage that changes nothing, for either hook.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bypass;

impl PhaseStage for Bypass {
    #[inline(always)]
    fn shape<S: F32s>(&mut self, _: usize, _: usize, phase: S::U32s, _: S::U32s) -> S::U32s {
        phase
    }

    #[inline(always)]
    fn shape_lane(&mut self, _: usize, _: usize, phase: u32, _: u32) -> u32 {
        phase
    }
}

impl WaveStage for Bypass {
    #[inline(always)]
    fn shape<S: F32s>(&mut self, _: usize, _: usize, value: S) -> S {
        value
    }

    #[inline(always)]
    fn shape_lane(&mut self, _: usize, _: usize, value: f32) -> f32 {
        value
    }
}

/// Each mipmap level's place in the table's data, and the highest phase
/// increment it plays without aliasing.
#[derive(Clone, Copy, Debug)]
struct Levels {
    count: usize,
    /// Level `i` plays increments up to `limit[i]`.
    limit: [u32; MAX_LEVELS],
    offset: [u32; MAX_LEVELS],
    size: [u32; MAX_LEVELS],
    stride: [u32; MAX_LEVELS],
}

impl Levels {
    fn of(table: &Table) -> Self {
        let mut levels = Self {
            count: table.levels().len().min(MAX_LEVELS),
            limit: [u32::MAX; MAX_LEVELS],
            offset: [0; MAX_LEVELS],
            size: [0; MAX_LEVELS],
            stride: [0; MAX_LEVELS],
        };
        for (i, level) in table.levels().iter().take(MAX_LEVELS).enumerate() {
            // Harmonic h of a note stays below Nyquist while
            // h × increment < 2³¹ (as `Table::level_for`).
            levels.limit[i] = ((1u64 << 31).div_ceil(level.harmonics as u64) - 1) as u32;
            levels.offset[i] = level.offset() as u32;
            levels.size[i] = level.size as u32;
            levels.stride[i] = level.stride() as u32;
        }
        // The fundamental-only level plays whatever is left.
        levels.limit[levels.count - 1] = u32::MAX;
        levels
    }

    /// The level for a lane whose increment reaches `increment`: the one
    /// with the most harmonics that all stay below Nyquist, as
    /// [`Table::level_for`] picks.
    #[inline]
    fn level_for(&self, increment: u32) -> u32 {
        self.limit[..self.count].partition_point(|&limit| limit < increment) as u32
    }

    /// Moves lane state `lane` on to a stretch whose increments reach
    /// `own`, and `peak` with the next stretch. A lane takes the level for
    /// `peak` and, when that's a change, fades to it from its last level
    /// over [`SUB_BLOCK`] frames, across renders. It takes the new level at
    /// once if it had none, or if the level it would fade from (or is
    /// fading from) has two or more levels too many harmonics for this
    /// stretch: after a jump in pitch the look ahead couldn't see, such as
    /// a new note on a sounding voice. A fade back to the level it came
    /// from carries on from the same mix.
    #[inline]
    fn advance(&self, lane: LaneLevel<'_>, own: u32, peak: u32) {
        let mut next = self.level_for(peak);
        if *lane.level != NO_LEVEL && next < *lane.level {
            // A level with more harmonics only once the pitch is a quarter
            // of a semitone clear of the change, so a pitch hovering there
            // doesn't fade back and forth (a level with fewer harmonics,
            // which a rising pitch needs, is never held back).
            next = self
                .level_for(peak.saturating_add(peak >> 6))
                .min(*lane.level);
        }
        let safe = self.level_for(own);
        if *lane.level == NO_LEVEL || *lane.level + 1 < safe {
            *lane.level = next;
            *lane.faded = FADED;
        }
        if *lane.faded < FADED && *lane.from + 1 < safe {
            *lane.faded = FADED;
        }
        if next != *lane.level {
            if *lane.faded < FADED && next == *lane.from {
                // Back the way it came, mid-fade: the same mix, with the
                // roles swapped (the fade curve is symmetric).
                *lane.faded = FADED - *lane.faded;
            } else {
                *lane.faded = 0.0;
            }
            *lane.from = *lane.level;
            *lane.level = next;
        }
        if *lane.faded >= FADED {
            *lane.from = *lane.level;
        }
    }
}

/// One lane's level state: the level it plays, the level it fades from,
/// and how many frames of the fade it has played.
struct LaneLevel<'a> {
    level: &'a mut u32,
    from: &'a mut u32,
    faded: &'a mut f32,
}

impl Lanes {
    /// Lane `l`'s level state.
    fn level_state(&mut self, l: usize) -> LaneLevel<'_> {
        LaneLevel {
            level: &mut self.level[l],
            from: &mut self.from[l],
            faded: &mut self.faded[l],
        }
    }

    /// Lane `l` had no level: its next stretch takes one without fading.
    fn forget_level(&mut self, l: usize) {
        self.level[l] = NO_LEVEL;
        self.faded[l] = FADED;
    }
}

/// How many frames a fade between levels lasts, as `faded` counts them.
const FADED: f32 = SUB_BLOCK as f32;

/// The values a render works from, fixed for the render.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// Octave, Semitone, Fine and Sweep, in semitones.
    tune: f32,
    /// log₂ of the increment is `pitch × 1/12 + offset`.
    exp_offset: f32,
    key_track: bool,
    gain: f32,
    /// Frame, counting from 0.
    frame: f32,
    last_frame: f32,
    smooth: bool,
}

/// The largest log₂ increment: just under 2³¹, Nyquist.
const MAX_EXPONENT: f32 = 30.9999;

/// The per-lane state, one array per value, each `lane_count` long.
#[derive(Debug)]
struct Lanes {
    phase: Box<[u32]>,
    key: Box<[f32]>,
    level: Box<[u32]>,
    /// The level each lane fades from, and how many frames of the fade it
    /// has played ([`FADED`] once done).
    from: Box<[u32]>,
    faded: Box<[f32]>,
    rng: Box<[u32]>,
    pitch_offset: Box<[f32]>,
    frame_offset: Box<[f32]>,
    /// One vector's or one lane's increments for a render, frame by frame,
    /// and one past it for the last stretch's look ahead.
    increments: Box<[u32]>,
}

/// One wavetable oscillator for every voice and unison copy, in SIMD lanes.
///
/// Construction allocates; nothing after it does, so every method that
/// takes `&mut self` but [`new`](Self::new) is safe on an audio thread
/// (dropping a [`SharedTable`] there never frees it).
#[derive(Debug)]
pub struct Oscillator {
    table: SharedTable,
    levels: Levels,
    params: Params,
    layout: Layout,
    lane_count: usize,
    max_frames: usize,
    sample_rate: f32,
    lanes: Lanes,
    voice_phases: [f32; MAX_UNISON],
}

impl Oscillator {
    /// An oscillator playing `table` for `layout`'s voices at
    /// `sample_rate`, rendering up to `max_frames` frames per call, with
    /// default [`Params`] and random phases seeded from `seed`. Allocates;
    /// make it off the audio thread.
    ///
    /// # Panics
    ///
    /// If `layout` is outside [`MAX_VOICES`] and [`MAX_UNISON`], or
    /// `max_frames` is 0.
    #[must_use]
    pub fn new(
        table: SharedTable,
        layout: Layout,
        sample_rate: f32,
        max_frames: usize,
        seed: u64,
    ) -> Self {
        assert!(
            (1..=MAX_VOICES).contains(&layout.voices)
                && (1..=MAX_UNISON).contains(&layout.lanes_per_voice)
                && max_frames > 0,
            "layout {layout:?} with {max_frames} frames"
        );
        let lane_count = (layout.voices * layout.lanes_per_voice).div_ceil(LANE_ALIGN) * LANE_ALIGN;
        let mut seed = seed;
        let rng = (0..lane_count)
            .map(|_| (splitmix(&mut seed) as u32) | 1)
            .collect();
        let params = Params::default();
        let start = phase_word(params.phase);
        Self {
            levels: Levels::of(&table),
            table,
            params,
            layout,
            lane_count,
            max_frames,
            sample_rate,
            lanes: Lanes {
                phase: vec![start; lane_count].into(),
                key: vec![0.0; lane_count].into(),
                level: vec![NO_LEVEL; lane_count].into(),
                from: vec![NO_LEVEL; lane_count].into(),
                faded: vec![FADED; lane_count].into(),
                rng,
                pitch_offset: vec![0.0; lane_count].into(),
                frame_offset: vec![0.0; lane_count].into(),
                increments: vec![0; (max_frames + 1) * LANE_ALIGN].into(),
            },
            voice_phases: [0.0; MAX_UNISON],
        }
    }

    /// The table it plays.
    #[must_use]
    pub fn table(&self) -> &SharedTable {
        &self.table
    }

    /// Plays `table` from the next render. The lanes keep their phases and
    /// take their new levels without a crossfade.
    pub fn set_table(&mut self, table: SharedTable) {
        self.levels = Levels::of(&table);
        self.table = table;
        for l in 0..self.lane_count {
            self.lanes.forget_level(l);
        }
    }

    /// Its controls.
    #[must_use]
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Its controls, to change; they apply from the next render.
    pub fn params_mut(&mut self) -> &mut Params {
        &mut self.params
    }

    /// The sample rate it renders at.
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Renders at `sample_rate` from the next render.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    /// Its voices and lanes per voice.
    #[must_use]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Every per-lane array's length: a multiple of [`LANE_ALIGN`].
    #[must_use]
    pub fn lane_count(&self) -> usize {
        self.lane_count
    }

    /// The most frames one render takes.
    #[must_use]
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Each lane's fixed pitch offset in semitones, added to its pitch:
    /// where unison detune goes (R1.7).
    pub fn pitch_offsets_mut(&mut self) -> &mut [f32] {
        &mut self.lanes.pitch_offset
    }

    /// Each lane's fixed frame offset in frames, added to Frame: where
    /// unison frame spread goes (R1.7, R38).
    pub fn frame_offsets_mut(&mut self) -> &mut [f32] {
        &mut self.lanes.frame_offset
    }

    /// Sets lanes `lanes`' unison offsets: pitch from `pitch`, in
    /// semitones, and frame from `frame`, in half tables (scaled here by
    /// half the table's frame count less one), each read at the same lane
    /// index, as `lspk-voice`'s unison lanes give them (R33, R38). Set them
    /// again after [`set_table`](Self::set_table) changes the frame count.
    ///
    /// # Panics
    ///
    /// If `lanes` reaches past [`lane_count`](Self::lane_count) or either
    /// slice.
    pub fn set_unison(&mut self, lanes: std::ops::Range<usize>, pitch: &[f32], frame: &[f32]) {
        let half = (self.table.frame_count() as f32 - 1.0) / 2.0;
        self.lanes.pitch_offset[lanes.clone()].copy_from_slice(&pitch[lanes.clone()]);
        for (to, &from) in self.lanes.frame_offset[lanes.clone()]
            .iter_mut()
            .zip(&frame[lanes])
        {
            *to = from * half;
        }
    }

    /// The stored start phase of each unison copy in [`PhaseMode::PerVoice`],
    /// in degrees; all 0 at first (R19).
    pub fn voice_phases_mut(&mut self) -> &mut [f32; MAX_UNISON] {
        &mut self.voice_phases
    }

    /// Lane `lane`'s running phase, where [`CYCLE`](crate::CYCLE) is a
    /// whole cycle.
    ///
    /// # Panics
    ///
    /// If there's no lane `lane`.
    #[must_use]
    pub fn phase(&self, lane: usize) -> u32 {
        self.lanes.phase[lane]
    }

    /// Stores voice `voice`'s current phases as the per-voice start phases,
    /// as choosing [`PhaseMode::PerVoice`] while a note sounds does (R19).
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn capture_phases(&mut self, voice: usize) {
        let lanes = self.lanes_of(voice);
        for (stored, &phase) in self.voice_phases.iter_mut().zip(&self.lanes.phase[lanes]) {
            *stored = phase as f32 / crate::CYCLE as f32 * 360.0;
        }
    }

    /// The lanes voice `voice` owns.
    #[must_use]
    pub fn lanes_of(&self, voice: usize) -> std::ops::Range<usize> {
        let n = self.layout.lanes_per_voice;
        voice * n..(voice + 1) * n
    }

    /// Voice `voice` starts playing `key` (a MIDI note number), as `onset`
    /// says: its lanes restart their phases or carry on (R16 to R20), and
    /// take the level for their new pitch.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn start(&mut self, voice: usize, key: f32, onset: Onset) {
        assert!(voice < self.layout.voices, "no voice {voice}");
        let lanes = self.lanes_of(voice);
        let restart = onset.restarts(self.params.phase_legato);
        for (copy, lane) in lanes.enumerate() {
            self.lanes.key[lane] = key;
            if onset == Onset::Fresh {
                self.lanes.forget_level(lane);
            }
            if !restart {
                continue;
            }
            match self.params.phase_mode {
                PhaseMode::Continue => {}
                PhaseMode::PerVoice => {
                    self.lanes.phase[lane] = phase_word(self.voice_phases[copy]);
                }
                PhaseMode::NoteStart => {
                    let random = unit(&mut self.lanes.rng[lane])
                        * (self.params.phase_random / 100.0).clamp(0.0, 1.0);
                    let cycles = self.params.phase / 360.0 + random;
                    self.lanes.phase[lane] = phase_word(cycles * 360.0);
                }
            }
        }
    }

    /// Memory this instance holds itself, in bytes: everything but the
    /// shared table.
    #[must_use]
    pub fn bytes(&self) -> usize {
        let l = &self.lanes;
        std::mem::size_of::<Self>()
            + std::mem::size_of_val(&*l.phase)
            + std::mem::size_of_val(&*l.key)
            + std::mem::size_of_val(&*l.level)
            + std::mem::size_of_val(&*l.from)
            + std::mem::size_of_val(&*l.faded)
            + std::mem::size_of_val(&*l.rng)
            + std::mem::size_of_val(&*l.pitch_offset)
            + std::mem::size_of_val(&*l.frame_offset)
            + std::mem::size_of_val(&*l.increments)
    }

    /// Renders `frames` frames of `inputs.lanes` lanes into `out`, frame by
    /// frame (`out[k × lane_count + l]`), by whichever path is quicker:
    /// across lanes in SIMD at the level this CPU dispatches, or one lane
    /// at a time when the lanes would fill no more than a quarter of a
    /// vector (one lane on NEON, where a vector of four is slower for a
    /// single voice than the scalar path).
    ///
    /// # Panics
    ///
    /// If `frames` is above [`max_frames`](Self::max_frames), or a slice is
    /// shorter than it must be: `lane_count` values for the per-lane
    /// inputs, `frames × lane_count` for the modulation and `out`.
    pub fn process(&mut self, frames: usize, inputs: &Inputs<'_>, out: &mut [f32]) {
        let level = simd::level();
        if inputs.lanes * 4 <= level.lanes() {
            self.process_scalar(frames, inputs, out);
        } else {
            self.process_at(level, frames, inputs, out);
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
        out: &mut [f32],
    ) {
        self.process_with(level, frames, inputs, out, &mut Bypass, &mut Bypass);
    }

    /// [`process_at`](Self::process_at), with `phase_stage` bending the
    /// phase before each read and `wave_stage` the value after it.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_with<P: PhaseStage, W: WaveStage>(
        &mut self,
        level: SimdLevel,
        frames: usize,
        inputs: &Inputs<'_>,
        out: &mut [f32],
        phase_stage: &mut P,
        wave_stage: &mut W,
    ) {
        self.check(frames, inputs, out);
        let run = kernel::Run {
            table: &self.table,
            levels: &self.levels,
            plan: self.plan(),
            lanes: &mut self.lanes,
            lane_count: self.lane_count,
            frames,
            inputs,
            out,
            phase_stage,
            wave_stage,
        };
        if let Err(run) = simd::dispatch_at(level, run) {
            // The portable level runs anywhere.
            let _ = simd::dispatch_at(SimdLevel::Portable, run);
        }
    }

    /// [`process`](Self::process) one lane at a time, without SIMD: the
    /// reference the vector paths are tested against, reading through
    /// [`Wave::read`](crate::Wave::read).
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_scalar(&mut self, frames: usize, inputs: &Inputs<'_>, out: &mut [f32]) {
        self.process_scalar_with(frames, inputs, out, &mut Bypass, &mut Bypass);
    }

    /// [`process_scalar`](Self::process_scalar) with shaper stages, as
    /// [`process_with`](Self::process_with).
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_scalar_with<P: PhaseStage, W: WaveStage>(
        &mut self,
        frames: usize,
        inputs: &Inputs<'_>,
        out: &mut [f32],
        phase_stage: &mut P,
        wave_stage: &mut W,
    ) {
        self.check(frames, inputs, out);
        scalar::render(
            &self.table,
            &self.levels,
            self.plan(),
            &mut self.lanes,
            self.lane_count,
            frames,
            inputs,
            out,
            phase_stage,
            wave_stage,
        );
    }

    fn check(&self, frames: usize, inputs: &Inputs<'_>, out: &mut [f32]) {
        let n = self.lane_count;
        assert!(
            frames <= self.max_frames,
            "{frames} frames, at most {}",
            self.max_frames
        );
        assert!(inputs.lanes <= n, "{} lanes, at most {n}", inputs.lanes);
        assert!(inputs.pitch.len() >= n, "pitch for {n} lanes");
        assert!(
            inputs.pitch_step.is_none_or(|s| s.len() >= n),
            "pitch step for {n} lanes"
        );
        for buffer in [inputs.pitch_mod, inputs.frame_mod, Some(&*out)]
            .into_iter()
            .flatten()
        {
            assert!(buffer.len() >= frames * n, "{frames} frames of {n} lanes");
        }
    }

    fn plan(&self) -> Plan {
        let p = &self.params;
        let frames = self.table.frame_count() as f32;
        Plan {
            tune: p.tune(),
            exp_offset: (f64::from(p.reference) / f64::from(self.sample_rate)).log2() as f32 + 32.0
                - 69.0 / 12.0,
            key_track: p.key_track,
            gain: p.gain(),
            frame: (p.frame - 1.0).clamp(0.0, frames - 1.0),
            last_frame: frames - 1.0,
            smooth: p.smooth,
        }
    }
}

/// `degrees` (wrapped to a cycle) as a phase word.
fn phase_word(degrees: f32) -> u32 {
    let cycles = f64::from(degrees) / 360.0;
    ((cycles - cycles.floor()) * crate::CYCLE) as u64 as u32
}

/// A uniform value in [0, 1) from a lane's xorshift state.
fn unit(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// The next value of a SplitMix64 sequence, to seed each lane apart.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
