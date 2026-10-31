//! LFOs: drawn shapes, an XY path, two chaos generators and sample and hold,
//! per voice or shared, free-running, restarted by notes or played once.
//!
//! An [`Lfo`] is one LFO slot: its [`Settings`], its drawn [`Shape`] and its
//! XY [`Path`], and the clock a free-running LFO follows. Each voice keeps
//! an [`LfoVoice`], the state the slot plays for that voice; a shared LFO
//! (sometimes called "mono") is one `LfoVoice` the synth uses for every voice.
//! [`LfoBank`] holds a slot with a voice state for each voice, processes
//! them all a block at a time, and has a SIMD path across voices.
//!
//! Each block, [`Lfo::prepare`] takes the host's [`Transport`] and returns a
//! [`Block`]: the rate, the clock and the smoothing for that block. Then
//! [`Lfo::render`] fills any stretch of the block for a voice, and
//! [`Lfo::trigger`] restarts a voice at a note-on between stretches, so a
//! note can start on any sample.
//!
//! # Guarantees
//!
//! - **Sample-accurate and block-size invariant.** An LFO's position is a
//!   function of samples counted since it was last anchored (a note-on, or
//!   a change of rate), never a running sum, so splitting a block anywhere,
//!   or processing in blocks of any size, gives the same output to the bit.
//!   When following the host's song position, the output is the same to
//!   within the rounding of the host's own position (about 1e-12).
//! - **Audio rate.** Every output is computed per sample, up to 1000 Hz.
//! - **Real-time safe.** Nothing here allocates, locks or blocks once made.
//! - **The SIMD path matches.** [`LfoBank`]'s SIMD path does the same
//!   operations as its scalar path (no fused multiply-add), so the two agree
//!   to the bit at every SIMD level.
//!
//! # Laws
//!
//! The rules cited as "engine-core Rn" are the plug-in kit's LFO
//! specification. Where the reference instrument's behaviour hasn't been
//! measured yet, this module adopts a first law, documented where it's
//! used: the curve family, sample and hold's distribution, the chaos
//! attractors' parameters and time scale, the smoothing law, the value of
//! the "fast" rate, swing's law and the global scale's two modes.

mod bank;
mod chaos;
mod shape;
#[cfg(test)]
mod tests;

use std::ops::{Add, Div, Mul, Sub};

pub use bank::LfoBank;
pub use shape::{CURVE_RANGE, MAX_POINTS, Path, PathPoint, Point, PointMod, Shape};

use shape::{PathSegment, Segment, curve, drawn_segment, path_point, path_segment};

/// How many LFO slots an instrument has; every one is always available to
/// the engine (engine-core R81).
pub const LFO_COUNT: usize = 10;

/// How many point-modulation buses each LFO has (engine-core R94).
pub const POINT_BUSES: usize = 16;

/// What an LFO generates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LfoType {
    /// The drawn shape.
    #[default]
    Drawn,
    /// The XY path, with X and Y outputs.
    Path,
    /// The Lorenz attractor.
    Lorenz,
    /// The Rössler attractor.
    Rossler,
    /// A new random value each cycle, held.
    SampleAndHold,
}

impl LfoType {
    /// Whether it's a generator (chaos or sample and hold) rather than a
    /// shape: generators ignore Direction and the loopback point.
    #[must_use]
    pub const fn is_generator(self) -> bool {
        matches!(self, Self::Lorenz | Self::Rossler | Self::SampleAndHold)
    }
}

/// How notes start an LFO (engine-core R83).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Follows its clock and ignores notes.
    Free,
    /// Starts again from Phase at each note.
    #[default]
    Restart,
    /// Plays one cycle from Phase at each note and stops, or loops from the
    /// loopback point.
    OneShot,
}

/// Which way the shape plays (engine-core R91).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Start to end.
    #[default]
    Forward,
    /// End to start.
    Reverse,
    /// Start to end, then end to start, each pass one cycle.
    PingPong,
}

/// A synced rate's feel (engine-core R85).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Feel {
    /// The division as written.
    #[default]
    Straight,
    /// Two thirds of the division.
    Triplet,
    /// One and a half times the division.
    Dotted,
}

impl Feel {
    /// What it multiplies the division's length by.
    #[must_use]
    pub const fn factor(self) -> f64 {
        match self {
            Self::Straight => 1.0,
            Self::Triplet => 2.0 / 3.0,
            Self::Dotted => 1.5,
        }
    }
}

/// A synced rate's division: 32, 16, 8, 4, 2 and 1 bars, then 1/2 to 1/256
/// of a whole note, then "fast" (engine-core R85).
///
/// Whole bars follow the host's time signature; the fractions are of a
/// whole note (four beats), whatever the signature. "Fast" is 1/512 of a
/// whole note, a first value until it's measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Division(u8);

impl Division {
    /// How many divisions there are.
    pub const COUNT: u8 = 15;
    /// 32 bars, the slowest.
    pub const BARS_32: Self = Self(0);
    /// One bar.
    pub const BAR: Self = Self(5);
    /// A quarter note, the init rate.
    pub const QUARTER: Self = Self(7);
    /// 1/256 of a whole note.
    pub const N256: Self = Self(13);
    /// "Fast", the quickest.
    pub const FAST: Self = Self(14);

    /// Division `index`, 0 (32 bars) to 14 (fast).
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if index < Self::COUNT {
            Some(Self(index))
        } else {
            None
        }
    }

    /// Its index, 0 (32 bars) to 14 (fast).
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// The division at a rate knob's normalised position `n`: 15 positions
    /// at n = k / 14.
    #[must_use]
    pub fn from_normalised(n: f32) -> Self {
        Self((n.clamp(0.0, 1.0) * 14.0).round() as u8)
    }

    /// Its knob position, k / 14.
    #[must_use]
    pub fn normalised(self) -> f32 {
        f32::from(self.0) / 14.0
    }

    /// The value the reference instrument stores for it, 100 × n⁴ (for
    /// preset import).
    #[must_use]
    pub fn stored(self) -> f32 {
        100.0 * self.normalised().powi(4)
    }

    /// Its length in beats (quarter notes), given the bar's length in beats.
    #[must_use]
    pub fn beats(self, beats_per_bar: f64) -> f64 {
        match self.0 {
            k @ 0..=5 => f64::from(1u32 << (5 - k)) * beats_per_bar,
            k @ 6..=13 => 4.0 / f64::from(1u32 << (k - 5)),
            _ => 4.0 / 512.0,
        }
    }
}

/// An LFO's rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rate {
    /// Locked to the host's tempo.
    Synced {
        /// The cycle's length.
        division: Division,
        /// Straight, triplet or dotted.
        feel: Feel,
    },
    /// In hertz.
    Hz {
        /// Cycles per second: 0 to 100, or 0 to 1000 with `x10`.
        hz: f32,
        /// Rate ×10: the range goes up to 1000 Hz.
        x10: bool,
    },
}

impl Default for Rate {
    fn default() -> Self {
        Self::Synced {
            division: Division::QUARTER,
            feel: Feel::Straight,
        }
    }
}

impl Rate {
    /// The rate at a Hz knob's normalised position `n`: f = 100 Hz × n⁴,
    /// ten times that with ×10 (engine-core R86).
    #[must_use]
    pub fn hz_from_normalised(n: f32, x10: bool) -> Self {
        let scale = if x10 { 10.0 } else { 1.0 };
        Self::Hz {
            hz: 100.0 * n.clamp(0.0, 1.0).powi(4) * scale,
            x10,
        }
    }
}

/// A time for Fade in or Delay.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Span {
    /// None.
    #[default]
    Off,
    /// A division of the host's tempo, 1/256 to 32 bars ("fast" counts as
    /// 1/256).
    Synced(Division),
    /// Seconds, 0 to 32.
    Seconds(f32),
}

impl Span {
    /// The tempo-mode knob's position `index`: 0 is Off, then 14 divisions
    /// from 1/256 (1) to 32 bars (14).
    #[must_use]
    pub const fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Off,
            i if i <= 14 => Self::Synced(Division(14 - i)),
            _ => Self::Synced(Division::BARS_32),
        }
    }

    /// Its length in samples at `block`'s tempo and sample rate.
    fn samples(self, block: &Block) -> u64 {
        let samples = match self {
            Self::Off => 0.0,
            Self::Synced(d) => {
                let d = if d == Division::FAST {
                    Division::N256
                } else {
                    d
                };
                d.beats(block.beats_per_bar) / block.beats_per_sample.max(f64::MIN_POSITIVE)
            }
            Self::Seconds(s) => f64::from(s.clamp(0.0, 32.0)) * block.sample_rate,
        };
        samples.round().clamp(0.0, 1e15) as u64
    }
}

/// The instrument's global LFO scale, 10 % to 1000 % (engine-core R97).
///
/// A first law: in percent, the scale stretches every LFO's cycle (200 %
/// is twice as long), as the envelope scale stretches times; by rate, it
/// multiplies every LFO's rate (200 % is twice as fast).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    /// Stretches every cycle by this percentage.
    Percent(f32),
    /// Multiplies every rate by this percentage.
    Rate(f32),
}

impl Default for Scale {
    fn default() -> Self {
        Self::Percent(100.0)
    }
}

impl Scale {
    /// What it multiplies a rate by.
    #[must_use]
    pub fn factor(self) -> f64 {
        match self {
            Self::Percent(p) => 100.0 / f64::from(p.clamp(10.0, 1000.0)),
            Self::Rate(p) => f64::from(p.clamp(10.0, 1000.0)) / 100.0,
        }
    }
}

/// An LFO slot's settings, with the init patch's values by default: a
/// drawn triangle, restarted by notes, per voice, forward, synced at a
/// quarter note, song lock off (its interface default isn't observed yet),
/// no fade in, delay or smoothing, phase 0 and an 8 by 8 grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// What it generates.
    pub kind: LfoType,
    /// How notes start it.
    pub mode: Mode,
    /// One LFO for all voices instead of one per voice (engine-core R82).
    pub shared: bool,
    /// Which way the shape plays.
    pub direction: Direction,
    /// Its rate.
    pub rate: Rate,
    /// Song lock (the host anchor): while the host plays, the LFO's phase
    /// is a function of its song position, whatever the mode, so two runs
    /// at the same position play the same phases; its rate may be synced or
    /// in hertz. Without it a restarted LFO runs from its own note, and a
    /// free one from its own clock (engine-core R84).
    pub song_lock: bool,
    /// Swing, on a synced rate, by the transport's swing amount
    /// (engine-core R87).
    pub swing: bool,
    /// Fade in from the start value to the shape, after Delay.
    pub fade_in: Span,
    /// How long the start value is held after a note-on.
    pub delay: Span,
    /// Smoothing, 0 to 100 (engine-core R89).
    pub smooth: f32,
    /// Where the shape starts, 0 to 360 degrees.
    pub phase: f32,
    /// Phase snaps to the vertical grid lines.
    pub phase_snap: bool,
    /// Vertical grid lines across the cycle.
    pub grid_x: u16,
    /// Horizontal grid lines, for an editor.
    pub grid_y: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            kind: LfoType::Drawn,
            mode: Mode::Restart,
            shared: false,
            direction: Direction::Forward,
            rate: Rate::default(),
            // The interface's own default isn't observed yet; off until it
            // is (engine-core R84).
            song_lock: false,
            swing: false,
            fade_in: Span::Off,
            delay: Span::Off,
            smooth: 0.0,
            phase: 0.0,
            phase_snap: false,
            grid_x: 8,
            grid_y: 8,
        }
    }
}

/// The host's transport at the first sample of a block, and the global
/// settings LFOs follow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    /// Samples per second.
    pub sample_rate: f64,
    /// Beats (quarter notes) per minute.
    pub tempo: f64,
    /// Beats in a bar, from the time signature.
    pub beats_per_bar: f64,
    /// Whether the host is playing; song lock only follows a playing host.
    pub playing: bool,
    /// The song position in beats.
    pub beats: f64,
    /// The song position in seconds.
    pub seconds: f64,
    /// The swing amount, 0 (straight) to 1, from the keyboard page.
    pub swing: f32,
    /// The global LFO scale.
    pub scale: Scale,
}

impl Transport {
    /// A stopped transport at the start of the song, 120 beats per minute
    /// in 4/4.
    #[must_use]
    pub fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            tempo: 120.0,
            beats_per_bar: 4.0,
            playing: false,
            beats: 0.0,
            seconds: 0.0,
            swing: 0.0,
            scale: Scale::default(),
        }
    }

    /// Moves the song position on by `frames` samples, as a host does
    /// between blocks.
    pub fn advance(&mut self, frames: usize) {
        let seconds = frames as f64 / self.sample_rate;
        self.seconds += seconds;
        self.beats += seconds * self.tempo / 60.0;
    }
}

/// How a free-running LFO's clock runs this block.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FreeClock {
    /// Locked to the song position: cycles = position × rate.
    Beats(f64),
    Seconds(f64),
    /// Counting samples: cycles = c0 + (n + k) × inc.
    Counting {
        c0: f64,
        n: u64,
    },
}

/// Where a locked LFO reads its phase from: the host's transport, so two
/// runs at the same song position play the same phases (engine-core R84).
#[derive(Clone, Copy, Debug, PartialEq)]
enum SongLock {
    /// Cycles per beat, from a synced rate.
    Beats(f64),
    /// Cycles per second, from a rate in hertz and the transport's own
    /// seconds.
    Seconds(f64),
}

/// One block's rate, clock and smoothing for an LFO slot, from
/// [`Lfo::prepare`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    frames: usize,
    /// Cycles per sample.
    inc: f64,
    /// Phase, in cycles.
    phase: f64,
    /// How a locked LFO — Song lock, with a playing host — follows the
    /// transport, whatever its mode.
    song_lock: Option<SongLock>,
    free: FreeClock,
    beats: f64,
    beats_per_sample: f64,
    seconds: f64,
    sample_rate: f64,
    beats_per_bar: f64,
    /// The first cycle's share of each pair, when swinging.
    swing: Option<f64>,
    /// The smoothing filter's coefficient, when smoothing.
    smooth: Option<f32>,
    /// The chaos integrator's step and steps per sample.
    chaos_h: f32,
    chaos_steps: u32,
}

impl Block {
    /// Samples in the block.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// The LFO's rate this block, in cycles per sample.
    #[must_use]
    pub const fn cycles_per_sample(&self) -> f64 {
        self.inc
    }

    fn beats_at(&self, k: usize) -> f64 {
        self.beats + k as f64 * self.beats_per_sample
    }

    fn seconds_at(&self, k: usize) -> f64 {
        self.seconds + k as f64 / self.sample_rate
    }
}

/// How a voice's position advances.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Anchor {
    /// cycles = c0 + n × inc, n counting samples.
    Samples { c0: f64, n: u64, inc: f64 },
    /// cycles = c0 + (beats − b0) × per_beat.
    Beats { c0: f64, b0: f64, per_beat: f64 },
    /// cycles = c0 + (seconds − s0) × per_second.
    Seconds { c0: f64, s0: f64, per_second: f64 },
}

impl Anchor {
    fn at(&self, block: &Block, k: usize) -> f64 {
        match *self {
            Self::Samples { c0, n, inc } => c0 + n as f64 * inc,
            Self::Beats { c0, b0, per_beat } => c0 + (block.beats_at(k) - b0) * per_beat,
            Self::Seconds { c0, s0, per_second } => c0 + (block.seconds_at(k) - s0) * per_second,
        }
    }

    /// A locked LFO's anchor from a synced rate: Phase where the song
    /// starts, so its phase is a function of the song position and not of
    /// when a note came (engine-core R84).
    fn locked_beats(phase: f64, per_beat: f64) -> Self {
        Self::Beats {
            c0: phase,
            b0: 0.0,
            per_beat,
        }
    }

    /// The same, from a rate in hertz and the transport's seconds.
    fn locked_seconds(phase: f64, per_second: f64) -> Self {
        Self::Seconds {
            c0: phase,
            s0: 0.0,
            per_second,
        }
    }
}

/// What a voice's sample-by-sample stage hands the lane arithmetic.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stage {
    /// Where the sample falls in its segment.
    pub u: f32,
    /// The fade-in gain, 0 to 1.
    pub g: f32,
    /// This sample is the first after a trigger: it sets the start value.
    pub capture: bool,
    /// The segment, the held value or the chaos step changed.
    pub changed: bool,
}

/// One voice's state for an LFO slot (or the one state of a shared LFO).
#[derive(Clone, Debug)]
pub struct LfoVoice {
    anchor: Anchor,
    rng: u64,
    pub(crate) held: f32,
    cycle: i64,
    pub(crate) chaos: [f32; 3],
    /// The chaos step this sample: the block's, or 0 once a one-shot ends.
    pub(crate) chaos_h: f32,
    pub(crate) seg: Segment,
    pub(crate) path_seg: PathSegment,
    xs: [f32; MAX_POINTS],
    ys: [f32; MAX_POINTS],
    buses: [f32; POINT_BUSES],
    /// The revision and type its modulated points were made for, if any.
    made_for: Option<(u64, LfoType)>,
    /// Samples since the last trigger, and Delay and Fade in in samples.
    t: u64,
    delay: u64,
    fade: u64,
    inv_fade: f32,
    capture: bool,
    pub(crate) start: [f32; 2],
    pub(crate) smoothed: [f32; 2],
}

impl LfoVoice {
    /// A voice state whose random values come from `seed`, not yet
    /// triggered: it runs from the start of the cycle, at full level.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            anchor: Anchor::Samples {
                c0: 0.0,
                n: 0,
                inc: 0.0,
            },
            rng: mix(seed),
            held: 0.0,
            cycle: i64::MIN,
            chaos: chaos::LORENZ.start,
            chaos_h: 0.0,
            seg: Segment::hold(f32::NAN, f32::NAN, 0.0),
            path_seg: PathSegment::default(),
            xs: [0.0; MAX_POINTS],
            ys: [0.0; MAX_POINTS],
            buses: [0.0; POINT_BUSES],
            made_for: None,
            t: u64::MAX,
            delay: 0,
            fade: 0,
            inv_fade: 0.0,
            capture: false,
            start: [0.0; 2],
            smoothed: [0.0; 2],
        }
    }

    /// Sets point-modulation bus `bus` (0 to 15) to `value`; the points it
    /// drives move from the next sample rendered (engine-core R94).
    pub fn set_bus(&mut self, bus: usize, value: f32) {
        if let Some(slot) = self.buses.get_mut(bus)
            && *slot != value
        {
            *slot = value;
            // Remake the points, keeping the type (no revision is ever
            // u64::MAX), so a chaos generator carries on.
            self.made_for = self.made_for.map(|(_, kind)| (u64::MAX, kind));
        }
    }

    /// The point-modulation buses' values.
    #[must_use]
    pub const fn buses(&self) -> &[f32; POINT_BUSES] {
        &self.buses
    }

    /// A new random value, 0 to 1 (xorshift64*). Sample and hold's first
    /// law: uniform.
    fn random(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let x = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A seed mixed into a random generator's state (splitmix64's finaliser),
/// never zero, as xorshift needs.
fn mix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)).max(1)
}

/// One LFO slot: its settings, shape, path and free-running clock.
#[derive(Clone, Debug)]
pub struct Lfo {
    /// Its settings; they take effect from the next [`prepare`](Self::prepare).
    pub settings: Settings,
    shape: Shape,
    path: Path,
    revision: u64,
    seed: u64,
    /// The free clock: anchor, samples counted, rate, and where the next
    /// block starts.
    clock_c0: f64,
    clock_n: u64,
    clock_inc: f64,
    clock_next: f64,
    clock_locked: bool,
}

impl Lfo {
    /// A slot with `settings`, the init shape and path, drawing random
    /// values from `seed`.
    #[must_use]
    pub fn new(settings: Settings, seed: u64) -> Self {
        Self {
            settings,
            shape: Shape::default(),
            path: Path::default(),
            revision: 0,
            seed,
            clock_c0: 0.0,
            clock_n: 0,
            clock_inc: 0.0,
            clock_next: 0.0,
            clock_locked: false,
        }
    }

    /// Phase, in cycles, snapped to the grid when it's on.
    fn start_phase(&self) -> f64 {
        let s = &self.settings;
        let phase = f64::from(s.phase.clamp(0.0, 360.0)) / 360.0;
        if s.phase_snap && s.grid_x > 0 {
            let grid = f64::from(s.grid_x);
            return (phase * grid).round() / grid;
        }
        phase
    }

    /// Where `voice` is in its cycle, 0 up to 1, at the next sample to
    /// render: what an interface draws as the LFO's phase. It is the
    /// position before direction, swing and smoothing shape the value.
    /// `transport` is the transport at that sample, the last block's
    /// advanced by its frames.
    #[must_use]
    pub fn phase(&self, voice: &LfoVoice, transport: &Transport) -> f32 {
        let cycles = if self.settings.mode == Mode::Free {
            self.clock_next + self.start_phase()
        } else {
            match voice.anchor {
                Anchor::Samples { c0, n, inc } => c0 + n as f64 * inc,
                Anchor::Beats { c0, b0, per_beat } => c0 + (transport.beats - b0) * per_beat,
                Anchor::Seconds { c0, s0, per_second } => {
                    c0 + (transport.seconds - s0) * per_second
                }
            }
        };
        cycles.rem_euclid(1.0) as f32
    }

    /// A fresh state for voice `index`, with its own random values.
    #[must_use]
    pub fn voice(&self, index: usize) -> LfoVoice {
        LfoVoice::new(
            self.seed
                .wrapping_add((index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        )
    }

    /// The drawn shape.
    #[must_use]
    pub const fn shape(&self) -> &Shape {
        &self.shape
    }

    /// The drawn shape, to edit; voices pick up the change at their next
    /// render.
    pub fn shape_mut(&mut self) -> &mut Shape {
        self.revision += 1;
        &mut self.shape
    }

    /// The XY path.
    #[must_use]
    pub const fn path(&self) -> &Path {
        &self.path
    }

    /// The XY path, to edit.
    pub fn path_mut(&mut self) -> &mut Path {
        self.revision += 1;
        &mut self.path
    }

    /// The block's rate, clock and smoothing; call once per block, before
    /// rendering any voice, with the transport at the block's first sample.
    pub fn prepare(&mut self, transport: &Transport, frames: usize) -> Block {
        let s = &self.settings;
        let sample_rate = transport.sample_rate.max(1.0);
        let beats_per_sample = transport.tempo.max(0.0) / 60.0 / sample_rate;
        let factor = transport.scale.factor();
        // Cycles per beat when synced, or cycles per second.
        let (per_beat, hz) = match s.rate {
            Rate::Synced { division, feel } => {
                let beats = division.beats(transport.beats_per_bar.max(0.25)) * feel.factor();
                (Some(factor / beats), None)
            }
            Rate::Hz { hz, x10 } => {
                let top = if x10 { 1000.0 } else { 100.0 };
                (None, Some(f64::from(hz.clamp(0.0, top)) * factor))
            }
        };
        let mut inc =
            per_beat.map_or_else(|| hz.unwrap_or(0.0) / sample_rate, |p| p * beats_per_sample);
        // Above half the sample rate a cycle would pass between samples.
        let limit = if inc > 0.5 { 0.5 / inc } else { 1.0 };
        inc *= limit;
        let per_beat = per_beat.map(|p| p * limit);
        let hz = hz.map(|h| h * limit);

        let phase = self.start_phase();

        let locked = s.song_lock && transport.playing;
        // A locked LFO is a function of the transport's position: its mode
        // only decides what restarts (Delay, Fade in) at a note.
        let song_lock = match (locked, per_beat, hz) {
            (true, Some(p), _) => Some(SongLock::Beats(p)),
            (true, None, Some(h)) => Some(SongLock::Seconds(h)),
            _ => None,
        };
        let free = match song_lock {
            Some(SongLock::Beats(p)) => FreeClock::Beats(p),
            Some(SongLock::Seconds(h)) => FreeClock::Seconds(h),
            None => {
                if self.clock_locked || inc != self.clock_inc {
                    self.clock_c0 = self.clock_next;
                    self.clock_n = 0;
                    self.clock_inc = inc;
                }
                FreeClock::Counting {
                    c0: self.clock_c0,
                    n: self.clock_n,
                }
            }
        };
        self.clock_locked = !matches!(free, FreeClock::Counting { .. });
        self.clock_next = match free {
            FreeClock::Beats(p) => (transport.beats + frames as f64 * beats_per_sample) * p,
            FreeClock::Seconds(h) => (transport.seconds + frames as f64 / sample_rate) * h,
            FreeClock::Counting { .. } => {
                self.clock_n += frames as u64;
                self.clock_c0 + self.clock_n as f64 * inc
            }
        };

        let swing = (s.swing && per_beat.is_some())
            .then(|| 0.5 + 0.25 * f64::from(transport.swing.clamp(0.0, 1.0)));
        // Smoothing's first law: a one-pole low-pass whose time constant is
        // (smooth / 100)² × a quarter cycle, so the smoothed shape looks the
        // same at every rate.
        let smooth = (s.smooth > 0.0).then(|| {
            let tau = 0.25 * (f64::from(s.smooth.min(100.0)) / 100.0).powi(2);
            (1.0 - (-inc / tau).exp()) as f32
        });
        let (chaos_h, chaos_steps) = match s.kind {
            LfoType::Lorenz | LfoType::Rossler => {
                let law = chaos::law(s.kind == LfoType::Lorenz);
                let dt = inc * law.per_cycle;
                let steps = (dt / law.max_step).ceil().clamp(1.0, 16.0);
                ((dt / steps) as f32, steps as u32)
            }
            _ => (0.0, 0),
        };

        Block {
            frames,
            inc,
            phase,
            song_lock,
            free,
            beats: transport.beats,
            beats_per_sample,
            seconds: transport.seconds,
            sample_rate,
            beats_per_bar: transport.beats_per_bar.max(0.25),
            swing,
            smooth,
            chaos_h,
            chaos_steps,
        }
    }

    /// Starts `voice` for a note-on in `block`: it restarts from Phase
    /// (unless the mode is Free, or Song lock grounds it in the transport,
    /// where its phase comes from the song position and not from the note),
    /// and its Delay and Fade in start (engine-core R83, R84, R88).
    pub fn trigger(&self, block: &Block, voice: &mut LfoVoice) {
        let s = &self.settings;
        if s.mode != Mode::Free {
            voice.anchor = match block.song_lock {
                Some(SongLock::Beats(per_beat)) => Anchor::locked_beats(block.phase, per_beat),
                Some(SongLock::Seconds(per_second)) => {
                    Anchor::locked_seconds(block.phase, per_second)
                }
                None => Anchor::Samples {
                    c0: block.phase,
                    n: 0,
                    inc: block.inc,
                },
            };
            voice.cycle = i64::MIN;
            if matches!(s.kind, LfoType::Lorenz | LfoType::Rossler) {
                voice.chaos = chaos::start_at(s.kind == LfoType::Lorenz, block.phase);
            }
        }
        voice.t = 0;
        voice.delay = s.delay.samples(block);
        voice.fade = s.fade_in.samples(block);
        voice.inv_fade = if voice.fade > 0 {
            (1.0 / voice.fade as f64) as f32
        } else {
            0.0
        };
        voice.capture = true;
    }

    /// Fills `out` (and `out_y`, for the XY path's Y output; 0 for the
    /// other types) with `voice`'s values for samples `from` to
    /// `from + out.len()` of `block`. Render a
    /// voice's samples in order; split a block anywhere, to trigger between
    /// the parts.
    pub fn render(
        &self,
        block: &Block,
        voice: &mut LfoVoice,
        from: usize,
        out: &mut [f32],
        mut out_y: Option<&mut [f32]>,
    ) {
        self.render_with(block, voice, from, out.len(), |i, x, y| {
            out[i] = x;
            if let Some(slot) = out_y.as_deref_mut().and_then(|o| o.get_mut(i)) {
                *slot = y;
            }
        });
    }

    /// [`render`](Self::render), handing each sample's X and Y to `emit`
    /// with its index in the stretch.
    pub(crate) fn render_with(
        &self,
        block: &Block,
        voice: &mut LfoVoice,
        from: usize,
        len: usize,
        mut emit: impl FnMut(usize, f32, f32),
    ) {
        self.begin(block, voice, from);
        let two = self.settings.kind == LfoType::Path;
        for i in 0..len {
            let stage = self.stage(block, voice, from + i);
            let (x, y) = self.raw(block, voice, stage);
            let (x, y) = post(block, voice, stage, x, y, two);
            emit(i, x, y);
        }
    }

    /// Readies `voice` for a stretch starting at sample `from`: moves its
    /// points if the shape, type or buses changed, and re-anchors it if the
    /// rate or the way it advances changed.
    pub(crate) fn begin(&self, block: &Block, voice: &mut LfoVoice, from: usize) {
        let kind = self.settings.kind;
        if voice.made_for != Some((self.revision, kind)) {
            if voice.made_for.is_none_or(|(_, k)| k != kind)
                && matches!(kind, LfoType::Lorenz | LfoType::Rossler)
            {
                voice.chaos = chaos::law(kind == LfoType::Lorenz).start;
            }
            match kind {
                LfoType::Path => self
                    .path
                    .modulated(&voice.buses, &mut voice.xs, &mut voice.ys),
                _ => self
                    .shape
                    .modulated(&voice.buses, &mut voice.xs, &mut voice.ys),
            }
            voice.seg = Segment::hold(f32::NAN, f32::NAN, 0.0);
            voice.path_seg = PathSegment {
                lo: f32::NAN,
                ..PathSegment::default()
            };
            voice.made_for = Some((self.revision, kind));
        }
        if self.settings.mode == Mode::Free {
            return;
        }
        let wanted = match block.song_lock {
            Some(SongLock::Beats(per_beat)) => match voice.anchor {
                Anchor::Beats {
                    c0,
                    b0: 0.0,
                    per_beat: p,
                } if p == per_beat && c0 == block.phase => None,
                // Grounded again: a rate or Phase change jumps the phase to
                // the position the song has reached (engine-core R84).
                _ => Some(Anchor::locked_beats(block.phase, per_beat)),
            },
            Some(SongLock::Seconds(per_second)) => match voice.anchor {
                Anchor::Seconds {
                    c0,
                    s0: 0.0,
                    per_second: p,
                } if p == per_second && c0 == block.phase => None,
                _ => Some(Anchor::locked_seconds(block.phase, per_second)),
            },
            None => match voice.anchor {
                Anchor::Samples { inc, .. } if inc == block.inc => None,
                _ => Some(Anchor::Samples {
                    c0: voice.anchor.at(block, from),
                    n: 0,
                    inc: block.inc,
                }),
            },
        };
        if let Some(anchor) = wanted {
            voice.anchor = anchor;
        }
    }

    /// The voice's position in cycles at sample `k`, before swing.
    #[inline(always)]
    fn cycles(&self, block: &Block, voice: &LfoVoice, k: usize) -> f64 {
        if self.settings.mode == Mode::Free {
            let clock = match block.free {
                FreeClock::Beats(p) => block.beats_at(k) * p,
                FreeClock::Seconds(h) => (block.seconds + k as f64 / block.sample_rate) * h,
                FreeClock::Counting { c0, n } => c0 + (n + k as u64) as f64 * block.inc,
            };
            clock + block.phase
        } else {
            voice.anchor.at(block, k)
        }
    }

    /// The voice's sample-by-sample stage at sample `k`: position, segment,
    /// generator events and fade-in gain. Both paths run it.
    #[inline(always)]
    pub(crate) fn stage(&self, block: &Block, voice: &mut LfoVoice, k: usize) -> Stage {
        let s = &self.settings;
        let mut c = self.cycles(block, voice, k);
        if let Anchor::Samples { n, .. } = &mut voice.anchor {
            *n += 1;
        }
        if let Some(first) = block.swing {
            c = swung(c, first);
        }
        let one_shot = s.mode == Mode::OneShot;
        let mut changed = false;
        let mut u = 0.0;
        match s.kind {
            LfoType::Drawn | LfoType::Path => {
                let loopback = match s.kind {
                    LfoType::Drawn => self.shape.loopback().map(|i| f64::from(voice.xs[i])),
                    _ => None,
                };
                let p = position(s.mode, s.direction, c, loopback) as f32;
                if s.kind == LfoType::Drawn {
                    if !voice.seg.contains(p) {
                        voice.seg = drawn_segment(
                            &self.shape,
                            &voice.xs[..self.shape.len()],
                            &voice.ys[..self.shape.len()],
                            p,
                        );
                        changed = true;
                    }
                    u = voice.seg.u(p);
                } else {
                    if !voice.path_seg.contains(p) {
                        voice.path_seg = path_segment(
                            &self.path,
                            &voice.xs[..self.path.len()],
                            &voice.ys[..self.path.len()],
                            p,
                        );
                        changed = true;
                    }
                    u = voice.path_seg.u(p);
                }
            }
            LfoType::SampleAndHold => {
                // One value per cycle; a one-shot keeps its first.
                let cycle = if one_shot { 0 } else { c.floor() as i64 };
                if cycle != voice.cycle {
                    voice.cycle = cycle;
                    voice.held = voice.random();
                    changed = true;
                }
            }
            LfoType::Lorenz | LfoType::Rossler => {
                let h = if one_shot && c >= 1.0 {
                    0.0
                } else {
                    block.chaos_h
                };
                changed = h != voice.chaos_h;
                voice.chaos_h = h;
            }
        }
        let g = if voice.t < voice.delay {
            0.0
        } else if voice.t - voice.delay >= voice.fade {
            1.0
        } else {
            (voice.t - voice.delay) as f32 * voice.inv_fade
        };
        voice.t = voice.t.saturating_add(1);
        let capture = std::mem::take(&mut voice.capture);
        Stage {
            u,
            g,
            capture,
            changed,
        }
    }

    /// The generator's raw X and Y for this sample (scalar path).
    #[inline(always)]
    fn raw(&self, block: &Block, voice: &mut LfoVoice, stage: Stage) -> (f32, f32) {
        match self.settings.kind {
            LfoType::Drawn => {
                let g = voice.seg;
                (curve(stage.u, g.y0, g.dy, g.e), 0.0)
            }
            LfoType::Path => path_point(stage.u, voice.path_seg.c),
            LfoType::SampleAndHold => (curve(0.0, voice.held, 0.0, 1.0), 0.0),
            kind @ (LfoType::Lorenz | LfoType::Rossler) => {
                let lorenz = kind == LfoType::Lorenz;
                voice.chaos = chaos::advance(lorenz, voice.chaos, voice.chaos_h, block.chaos_steps);
                (chaos::output(lorenz, voice.chaos[0]), 0.0)
            }
        }
    }
}

/// The start value, fade in and smoothing, after the generator: the scalar
/// path's version of [`post_lanes`], branching where the lanes select. The
/// results are equal, because `a × 1 + b × 0` is exactly `a`.
#[inline(always)]
fn post(
    block: &Block,
    voice: &mut LfoVoice,
    stage: Stage,
    x: f32,
    y: f32,
    two: bool,
) -> (f32, f32) {
    let mut out = [x, y];
    for (i, value) in out.iter_mut().enumerate().take(1 + usize::from(two)) {
        let raw = *value;
        if stage.capture {
            voice.start[i] = raw;
            voice.smoothed[i] = raw;
        }
        let faded = if stage.g == 1.0 {
            raw
        } else {
            raw * stage.g + voice.start[i] * (1.0 - stage.g)
        };
        *value = match block.smooth {
            Some(a) => voice.smoothed[i] + (faded - voice.smoothed[i]) * a,
            None => faded,
        };
        voice.smoothed[i] = *value;
    }
    (out[0], out[1])
}

/// One output's start value, fade in and smoothing: `capture` (1 or 0) takes
/// this sample as the start value and resets the smoothing to it; the
/// fade-in gain `g` blends from the start value to the raw value; then the
/// one-pole smoothing, if any. Returns the output, the start value and the
/// smoothing state. Selecting with `a × m + b × (1 − m)` gives `a` or `b`
/// exactly, so both paths agree to the bit.
#[inline(always)]
pub(crate) fn post_lanes<L: Lane>(
    raw: L,
    g: L,
    capture: L,
    start: L,
    smoothed: L,
    smooth: Option<f32>,
) -> (L, L, L) {
    let one = L::splat(1.0);
    let keep = one - capture;
    let start = raw * capture + start * keep;
    let smoothed = raw * capture + smoothed * keep;
    let faded = raw * g + start * (one - g);
    match smooth {
        Some(a) => {
            let s = smoothed + (faded - smoothed) * L::splat(a);
            (s, start, s)
        }
        None => (faded, start, faded),
    }
}

/// Swing over pairs of cycles: the first of each pair takes `first` of the
/// pair's time and the second the rest, so the pair still lasts two cycles.
/// A first law; R5 sets the amount.
fn swung(c: f64, first: f64) -> f64 {
    let pair = (c * 0.5).floor();
    let r = c - 2.0 * pair;
    let a = 2.0 * first;
    2.0 * pair
        + if r < a {
            r / a
        } else {
            1.0 + (r - a) / (2.0 - a)
        }
}

/// Where in the shape (0 to 1) position `c` cycles falls, by mode and
/// direction, with a One-shot loopback at `loopback` (engine-core R83,
/// R91).
#[inline(always)]
fn position(mode: Mode, direction: Direction, c: f64, loopback: Option<f64>) -> f64 {
    const EPS: f64 = 1e-9;
    if mode != Mode::OneShot {
        let cycle = c.floor();
        let q = c - cycle;
        return match direction {
            Direction::Forward => q,
            Direction::Reverse => 1.0 - q,
            Direction::PingPong if (cycle as i64) & 1 == 1 => 1.0 - q,
            Direction::PingPong => q,
        };
    }
    let c = c.max(0.0);
    match direction {
        Direction::Forward => match loopback {
            _ if c < 1.0 => c,
            Some(l) if 1.0 - l > EPS => l + (c - 1.0).rem_euclid(1.0 - l),
            _ => 1.0,
        },
        // In reverse, the loop runs from the loopback point back to the
        // start.
        Direction::Reverse => match loopback {
            _ if c < 1.0 => 1.0 - c,
            Some(l) if l > EPS => l - (c - 1.0).rem_euclid(l),
            _ => 0.0,
        },
        // Ping-pong goes there and back once, or bounces between the end
        // and the loopback point.
        Direction::PingPong => match loopback {
            _ if c < 1.0 => c,
            Some(l) if 1.0 - l > EPS => {
                let w = 1.0 - l;
                let d = (c - 1.0).rem_euclid(2.0 * w);
                if d < w { 1.0 - d } else { l + (d - w) }
            }
            _ if c < 2.0 => 2.0 - c,
            _ => 0.0,
        },
    }
}

/// Arithmetic on one `f32` or on SIMD lanes, so the scalar and SIMD paths
/// share their formulas.
pub(crate) trait Lane:
    Copy + Add<Output = Self> + Sub<Output = Self> + Mul<Output = Self> + Div<Output = Self>
{
    fn splat(value: f32) -> Self;
    #[must_use]
    fn min(self, other: Self) -> Self;
    #[must_use]
    fn max(self, other: Self) -> Self;
}

impl Lane for f32 {
    #[inline(always)]
    fn splat(value: f32) -> Self {
        value
    }
    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self::min(self, other)
    }
    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self::max(self, other)
    }
}
