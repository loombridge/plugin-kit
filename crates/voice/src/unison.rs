//! Unison: each voice's stacked, detuned copies, one SIMD lane per copy.
//!
//! [`Unison`] holds the controls (engine-core section 4) and works out what
//! each copy plays ([`UnisonCopy`]): its pitch offset from detune and stack, its
//! left and right gains from blend, level compensation and width, and its
//! spreads of frame, shaper amounts, start and span. [`UnisonLanes`] keeps
//! those values for every voice's lanes, laid out as the [`VoiceBlock`]
//! lays out its lanes (voice *v*'s copies at lanes `v × lanes_per_voice`
//! on), draws the per-note randomness when a note starts, and mixes an
//! oscillator's lanes down to one stereo pair per voice in SIMD.
//!
//! # The laws
//!
//! Copy *i* of *n* has an even position `x = 2i / (n − 1) − 1`, from −1
//! (the first copy) to +1 (the last); one copy sits at 0. Each law below
//! is a first law, documented here and to be fitted in R7's measurements
//! unless it says it was observed.
//!
//! - **Detune** (R33): the outermost copies sit `Range × detune^1.25`
//!   semitones either side of the note, where `detune` is the shown value
//!   (the square of the knob position, so `Range × n^2.5`): ±200 cents at
//!   the default Range of 2 and full detune, as observed. Each copy sits at
//!   its law's position times that.
//! - **Detune laws** (R34), as a position for each `x`, applied to `|x|`
//!   with its sign kept: **Linear** `x` (observed: equal steps); **Dense**
//!   `x(0.85 + 0.15x)`, gaps that widen steadily outward so no two pairs
//!   beat at the same rate; **Exponential** `(4^x − 1) / 3`, packed near
//!   the centre (seven copies at ±0.20 and ±0.51 of the outer, against ±0.18
//!   and ±0.50 measured on the earlier version); **Inverse** `x(2 − x)`,
//!   pushed outward (±0.56 and ±0.89, against ±0.56 and ±0.86);
//!   **Random**, a position drawn uniformly from −1 to +1 for each copy
//!   whenever a note starts.
//! - **Stack** (R36): each copy is transposed by an interval chosen by its
//!   rank, how many places it is from the centre (the centre copy, or the
//!   centre pair, is rank 0; the next pair out rank 1, and so on), cycling
//!   through the stack's intervals: Octave ×1 is 0 and +12, ×2 adds +24,
//!   ×3 +36; Octave and fifth ×1 is 0, +7 and +12, ×2 adds +19 and +24, ×3
//!   +31 and +36. Pairs share an interval, so each interval stays balanced
//!   in the stereo field and the centre keeps the note; a stack needs more
//!   ranks than two copies have to sound. **Centre −12** and **Centre −24**
//!   drop the rank-0 copy or pair by that many semitones.
//! - **Blend** (R35): with more than two copies, the outer copies' weight
//!   rises from 0 at 0 % to 1 at 75 %, and the centre copy's (or pair's)
//!   falls from 1 at 75 % to 0 at 100 %; at 75 % every copy has weight 1.
//!   With one or two copies every copy has weight 1.
//! - **Level compensation** (R32): every weight is divided by the square
//!   root of the sum of the weights squared, so the copies' total power
//!   (they're uncorrelated once detuned or started at random phases) stays
//!   that of one copy: 16 equal copies each play at −12 dB.
//! - **Width** (R37): copy *i* is panned to `width/100 × x`, so 0 % is mono,
//!   100 % spreads the copies from hard left (the first, lowest copy with
//!   the Linear law) to hard right, and a negative width mirrors them.
//! - **Pan law**: constant power, 0 dB in both channels at the centre
//!   (`left = √2·cos θ`, `right = √2·sin θ`, θ from 0 to π/2 across the
//!   field), so a single copy plays the oscillator unchanged in both
//!   channels, and a copy panned hard to one side is +3 dB there.
//! - **Spreads** (R38): copy *i*'s frame moves by `spread/100 × x` half
//!   tables (the oscillator scales it by half its frame count less one),
//!   each shaper amount by `spread/100 × x × 50` percentage points, and the
//!   span (for sample-based oscillators) by `spread/100 × x` of the sample.
//!   The start spread is random: each copy's start moves by `spread/100`
//!   times a value drawn from 0 to 1 whenever a note starts.
//!
//! Each copy's random start phase is the oscillator's own (engine-core
//! R17): every lane draws its own offset when its note starts.
//!
//! Copies never count against the voice limit (R39): the limit counts
//! notes, and a voice's copies are its lanes.

use lspk_dsp::simd::{self, F32s, Kernel, Level};

use crate::block::LANE_ALIGN;
use crate::settings::MAX_VOICES;

/// The most copies a voice has (engine-core section 4).
pub const MAX_COPIES: usize = 16;

/// How the copies spread across the detune (engine-core R34).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DetuneLaw {
    /// Even steps in pitch (the default, as observed).
    #[default]
    Linear,
    /// Slightly uneven steps, widening outward, for a dense sound.
    Dense,
    /// Packed near the centre.
    Exponential,
    /// Pushed outward.
    Inverse,
    /// A random position for each copy, drawn whenever a note starts.
    Random,
}

impl DetuneLaw {
    /// Every law, in menu order.
    pub const ALL: [Self; 5] = [
        Self::Linear,
        Self::Dense,
        Self::Exponential,
        Self::Inverse,
        Self::Random,
    ];

    /// Where a copy at even position `x` (−1 to +1) sits under this law,
    /// as a fraction of the outer copies' detune. Random has no fixed
    /// position; it gives `x`, and [`Unison::copy`] uses the drawn one.
    #[must_use]
    pub fn position(self, x: f32) -> f32 {
        let p = x.abs().min(1.0);
        let shaped = match self {
            Self::Linear | Self::Random => p,
            Self::Dense => p * (0.85 + 0.15 * p),
            Self::Exponential => (4f32.powf(p) - 1.0) / 3.0,
            Self::Inverse => p * (2.0 - p),
        };
        shaped.copysign(x)
    }
}

/// Which intervals the copies are stacked at (engine-core R36): nine
/// entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Stack {
    /// Detune only.
    #[default]
    Off,
    /// The note and one octave up.
    Octave1,
    /// The note and up to two octaves up.
    Octave2,
    /// The note and up to three octaves up.
    Octave3,
    /// The note, a fifth and an octave up.
    Fifth1,
    /// The note, fifths and octaves up to two octaves up.
    Fifth2,
    /// The note, fifths and octaves up to three octaves up.
    Fifth3,
    /// The centre copy or pair an octave down.
    CentreDown12,
    /// The centre copy or pair two octaves down.
    CentreDown24,
}

impl Stack {
    /// Every entry, in menu order.
    pub const ALL: [Self; 9] = [
        Self::Off,
        Self::Octave1,
        Self::Octave2,
        Self::Octave3,
        Self::Fifth1,
        Self::Fifth2,
        Self::Fifth3,
        Self::CentreDown12,
        Self::CentreDown24,
    ];

    /// The interval, in semitones, for a copy of rank `rank` (0 for the
    /// centre copy or pair, 1 for the next pair out, and so on).
    #[must_use]
    pub fn interval(self, rank: usize) -> f32 {
        let cycle: &[f32] = match self {
            Self::Off => &[0.0],
            Self::Octave1 => &[0.0, 12.0],
            Self::Octave2 => &[0.0, 12.0, 24.0],
            Self::Octave3 => &[0.0, 12.0, 24.0, 36.0],
            Self::Fifth1 => &[0.0, 7.0, 12.0],
            Self::Fifth2 => &[0.0, 7.0, 12.0, 19.0, 24.0],
            Self::Fifth3 => &[0.0, 7.0, 12.0, 19.0, 24.0, 31.0, 36.0],
            Self::CentreDown12 => return if rank == 0 { -12.0 } else { 0.0 },
            Self::CentreDown24 => return if rank == 0 { -24.0 } else { 0.0 },
        };
        cycle[rank % cycle.len()]
    }
}

/// The unison controls, in real units (engine-core section 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unison {
    /// Copies per voice, 1 to [`MAX_COPIES`] (R32).
    pub copies: usize,
    /// Detune, 0 to 1, as shown: the square of the knob position (R33).
    pub detune: f32,
    /// Blend, 0 to 100 %: the outer copies against the centre (R35).
    pub blend: f32,
    /// How the copies spread across the detune (R34).
    pub law: DetuneLaw,
    /// The intervals the copies are stacked at (R36).
    pub stack: Stack,
    /// Width, −100 to +100 %: the stereo spread (R37).
    pub width: f32,
    /// Range, 0 to 48 semitones: the outer copies' detune at full Detune
    /// (R33).
    pub range: f32,
    /// Frame spread, −100 to +100 (R38).
    pub frame_spread: f32,
    /// Shaper 1 and 2 amount spreads, −100 to +100 (R31, R38).
    pub shaper_spread: [f32; 2],
    /// Start spread, 0 to 100 %, random per note (sample-based oscillators).
    pub start_spread: f32,
    /// Span, −100 to +100: a fixed start offset per copy (sample-based
    /// oscillators).
    pub span_spread: f32,
}

impl Default for Unison {
    /// A new patch's unison, as observed: one copy, Detune 0.25, Blend
    /// 75 %, Linear, Stack off, Width 100 %, Range 2 semitones, every
    /// spread 0.
    fn default() -> Self {
        Self {
            copies: 1,
            detune: 0.25,
            blend: 75.0,
            law: DetuneLaw::Linear,
            stack: Stack::Off,
            width: 100.0,
            range: 2.0,
            frame_spread: 0.0,
            shaper_spread: [0.0; 2],
            start_spread: 0.0,
            span_spread: 0.0,
        }
    }
}

/// What one copy plays.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UnisonCopy {
    /// Pitch offset from the note, in semitones: detune and stack.
    pub pitch: f32,
    /// Pan, from −1 (left) to +1 (right).
    pub pan: f32,
    /// Its weight after blend and level compensation.
    pub level: f32,
    /// Its gain into the left channel: level and the pan law.
    pub left: f32,
    /// Its gain into the right channel.
    pub right: f32,
    /// Frame offset, in half tables.
    pub frame: f32,
    /// Shaper 1 and 2 amount offsets, in percentage points.
    pub shaper: [f32; 2],
    /// Start offset, as a fraction of the sample.
    pub start: f32,
    /// Span offset, as a fraction of the sample.
    pub span: f32,
}

/// One copy's random draws for a note: its detune position (for the Random
/// law), from −1 to +1, and its start, from 0 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Draws {
    /// The Random law's position.
    pub position: f32,
    /// The start spread's share.
    pub start: f32,
}

impl Unison {
    /// The copy count, clamped to 1 to [`MAX_COPIES`].
    #[must_use]
    pub fn count(&self) -> usize {
        self.copies.clamp(1, MAX_COPIES)
    }

    /// Copy `i`'s even position, from −1 to +1 (0 for a single copy).
    #[must_use]
    pub fn even(&self, i: usize) -> f32 {
        let n = self.count();
        if n == 1 {
            0.0
        } else {
            2.0 * i as f32 / (n - 1) as f32 - 1.0
        }
    }

    /// Copy `i`'s rank: how many places it is from the centre, the centre
    /// copy (odd counts) or each of the centre pair (even counts) being 0.
    #[must_use]
    pub fn rank(&self, i: usize) -> usize {
        let n = self.count();
        if n % 2 == 1 {
            i.abs_diff((n - 1) / 2)
        } else if i < n / 2 {
            n / 2 - 1 - i
        } else {
            i - n / 2
        }
    }

    /// The outer copies' detune, in semitones: `Range × detune^1.25`, or 0
    /// for a single copy (R33).
    #[must_use]
    pub fn spread(&self) -> f32 {
        if self.count() == 1 {
            0.0
        } else {
            self.range.clamp(0.0, 48.0) * self.detune.clamp(0.0, 1.0).powf(1.25)
        }
    }

    /// Copy `i`'s weight from Blend, before level compensation (R35).
    #[must_use]
    pub fn weight(&self, i: usize) -> f32 {
        if self.count() <= 2 {
            return 1.0;
        }
        let blend = self.blend.clamp(0.0, 100.0);
        if self.rank(i) == 0 {
            ((100.0 - blend) / 25.0).min(1.0)
        } else {
            (blend / 75.0).min(1.0)
        }
    }

    /// The level compensation every weight is multiplied by: one over the
    /// square root of the weights' squares summed (R32).
    #[must_use]
    pub fn compensation(&self) -> f32 {
        let power: f32 = (0..self.count()).map(|i| self.weight(i).powi(2)).sum();
        if power > 0.0 {
            power.sqrt().recip()
        } else {
            0.0
        }
    }

    /// What copy `i` plays, with its random `draws` for the note.
    #[must_use]
    pub fn copy(&self, i: usize, draws: Draws) -> UnisonCopy {
        self.copy_with(i, draws, self.compensation())
    }

    fn copy_with(&self, i: usize, draws: Draws, compensation: f32) -> UnisonCopy {
        let x = self.even(i);
        let position = match self.law {
            DetuneLaw::Random if self.count() > 1 => draws.position.clamp(-1.0, 1.0),
            law => law.position(x),
        };
        let pan = (self.width.clamp(-100.0, 100.0) / 100.0) * x;
        let level = self.weight(i) * compensation;
        let (left, right) = pan_gains(pan);
        let spread = |amount: f32| amount.clamp(-100.0, 100.0) / 100.0 * x;
        UnisonCopy {
            pitch: position * self.spread() + self.stack.interval(self.rank(i)),
            pan,
            level,
            left: left * level,
            right: right * level,
            frame: spread(self.frame_spread),
            shaper: self.shaper_spread.map(|s| spread(s) * 50.0),
            start: self.start_spread.clamp(0.0, 100.0) / 100.0 * draws.start.clamp(0.0, 1.0),
            span: spread(self.span_spread),
        }
    }
}

/// The pan law's gains at `pan` (−1 to +1): constant power, 0 dB in both
/// channels at the centre.
#[must_use]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    let (sin, cos) = theta.sin_cos();
    (
        std::f32::consts::SQRT_2 * cos,
        std::f32::consts::SQRT_2 * sin,
    )
}

/// Every voice's copies, as per-lane arrays laid out as the
/// [`VoiceBlock`](crate::VoiceBlock)'s lanes, and the mix of an
/// oscillator's lanes to a stereo pair per voice.
///
/// Each array is [`lane_count`](Self::lane_count) long, and lanes past a
/// voice's copies (when [`Unison::copies`] is below the layout's lanes per
/// voice) are silent: gain 0 and every offset 0. For the best packing,
/// build the layout with as many lanes per voice as there are copies.
///
/// Construction allocates; nothing after it does, so every other method is
/// safe on an audio thread.
#[derive(Debug)]
pub struct UnisonLanes {
    unison: Unison,
    voices: usize,
    lanes_per_voice: usize,
    lane_count: usize,
    voice_stride: usize,
    pitch: Box<[f32]>,
    frame: Box<[f32]>,
    shaper: [Box<[f32]>; 2],
    start: Box<[f32]>,
    span: Box<[f32]>,
    left: Box<[f32]>,
    right: Box<[f32]>,
    /// The gains again, [`MAX_COPIES`] per voice, zero past its copies: what
    /// the SIMD mix loads.
    left_padded: Box<[f32]>,
    right_padded: Box<[f32]>,
    draws: Box<[Draws]>,
    rng: u32,
}

impl UnisonLanes {
    /// Lanes for `voices` voices of `lanes_per_voice` copies each, at the
    /// default [`Unison`], with random draws seeded from `seed`. Allocates.
    ///
    /// # Panics
    ///
    /// If `voices` is outside 1 to [`MAX_VOICES`] or `lanes_per_voice`
    /// outside 1 to [`MAX_COPIES`].
    #[must_use]
    pub fn new(voices: usize, lanes_per_voice: usize, seed: u64) -> Self {
        assert!(
            (1..=MAX_VOICES).contains(&voices) && (1..=MAX_COPIES).contains(&lanes_per_voice),
            "{voices} voices of {lanes_per_voice} lanes"
        );
        let lane_count = (voices * lanes_per_voice).div_ceil(LANE_ALIGN) * LANE_ALIGN;
        let zeros = || vec![0.0; lane_count].into_boxed_slice();
        let mut state = seed;
        let mut lanes = Self {
            unison: Unison::default(),
            voices,
            lanes_per_voice,
            lane_count,
            voice_stride: voices.div_ceil(LANE_ALIGN) * LANE_ALIGN,
            pitch: zeros(),
            frame: zeros(),
            shaper: [zeros(), zeros()],
            start: zeros(),
            span: zeros(),
            left: zeros(),
            right: zeros(),
            left_padded: vec![0.0; voices * MAX_COPIES].into_boxed_slice(),
            right_padded: vec![0.0; voices * MAX_COPIES].into_boxed_slice(),
            draws: vec![Draws::default(); lane_count].into_boxed_slice(),
            rng: (splitmix(&mut state) as u32) | 1,
        };
        for voice in 0..voices {
            lanes.draw(voice);
        }
        lanes.write_all();
        lanes
    }

    /// The controls in force.
    #[must_use]
    pub fn unison(&self) -> &Unison {
        &self.unison
    }

    /// Changes the controls: every voice's lanes follow at once, keeping
    /// the random draws of the notes they play. Copies beyond the layout's
    /// lanes per voice are left out.
    pub fn set(&mut self, unison: Unison) {
        self.unison = unison;
        self.unison.copies = unison.count().min(self.lanes_per_voice);
        self.write_all();
    }

    /// A note starts on voice `voice`: its copies draw their Random
    /// positions and start shares afresh.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn start(&mut self, voice: usize) {
        assert!(voice < self.voices, "no voice {voice}");
        self.draw(voice);
        self.write(voice, self.unison.compensation());
    }

    /// How many voices it holds.
    #[must_use]
    pub fn voices(&self) -> usize {
        self.voices
    }

    /// Lanes per voice.
    #[must_use]
    pub fn lanes_per_voice(&self) -> usize {
        self.lanes_per_voice
    }

    /// Every lane array's length, a multiple of [`LANE_ALIGN`]: the same as
    /// the voice block's and the oscillator's for the same layout.
    #[must_use]
    pub fn lane_count(&self) -> usize {
        self.lane_count
    }

    /// The distance between one frame's voices in a mix's output: the
    /// voices rounded up to a multiple of [`LANE_ALIGN`], as the kit's
    /// filter banks lay out their voices.
    #[must_use]
    pub fn voice_stride(&self) -> usize {
        self.voice_stride
    }

    /// The lanes voice `voice` owns.
    #[must_use]
    pub fn lanes_of(&self, voice: usize) -> std::ops::Range<usize> {
        voice * self.lanes_per_voice..(voice + 1) * self.lanes_per_voice
    }

    /// Each lane's pitch offset, in semitones: for the oscillator's pitch
    /// offsets.
    #[must_use]
    pub fn pitch(&self) -> &[f32] {
        &self.pitch
    }

    /// Each lane's frame offset, in half tables: times half the table's
    /// frame count less one, for the oscillator's frame offsets.
    #[must_use]
    pub fn frame(&self) -> &[f32] {
        &self.frame
    }

    /// Each lane's amount offset for shaper `slot` (0 or 1), in percentage
    /// points: the hook the shapers read, adding it to their amount.
    ///
    /// # Panics
    ///
    /// If `slot` is 2 or more.
    #[must_use]
    pub fn shaper(&self, slot: usize) -> &[f32] {
        &self.shaper[slot]
    }

    /// Each lane's start offset, as a fraction of the sample.
    #[must_use]
    pub fn start_offsets(&self) -> &[f32] {
        &self.start
    }

    /// Each lane's span offset, as a fraction of the sample.
    #[must_use]
    pub fn span(&self) -> &[f32] {
        &self.span
    }

    /// Each lane's gain into the left channel.
    #[must_use]
    pub fn left(&self) -> &[f32] {
        &self.left
    }

    /// Each lane's gain into the right channel.
    #[must_use]
    pub fn right(&self) -> &[f32] {
        &self.right
    }

    /// Adds `frames` frames of the first `voices` voices' lanes in `input`
    /// (frame by frame, `input[k × lane_count + l]`, as the oscillator
    /// renders them) to `out` as a stereo pair per voice, each lane at its
    /// left and right gains: `out[k × 2 × voice_stride + c × voice_stride +
    /// v]` for channel `c` (0 left, 1 right), the layout the kit's filter
    /// banks take. At the SIMD level this CPU dispatches.
    ///
    /// # Panics
    ///
    /// If `voices` is above [`voices`](Self::voices), or a buffer is
    /// shorter than `frames` frames.
    pub fn mix(&self, frames: usize, voices: usize, input: &[f32], out: &mut [f32]) {
        self.mix_at(simd::level(), frames, voices, input, out);
    }

    /// [`mix`](Self::mix) at SIMD level `level`, or the portable level if
    /// this CPU doesn't have it.
    ///
    /// # Panics
    ///
    /// As [`mix`](Self::mix).
    pub fn mix_at(
        &self,
        level: Level,
        frames: usize,
        voices: usize,
        input: &[f32],
        out: &mut [f32],
    ) {
        self.check(frames, voices, input, out);
        let run = Mix {
            lanes: self,
            frames,
            voices,
            input,
            out,
        };
        if let Err(run) = simd::dispatch_at(level, run) {
            let _ = simd::dispatch_at(Level::Portable, run);
        }
    }

    /// [`mix`](Self::mix) one lane at a time: the reference the SIMD path
    /// is tested against.
    ///
    /// # Panics
    ///
    /// As [`mix`](Self::mix).
    pub fn mix_scalar(&self, frames: usize, voices: usize, input: &[f32], out: &mut [f32]) {
        self.check(frames, voices, input, out);
        for v in 0..voices {
            self.mix_voice(frames, v, input, out);
        }
    }

    /// Voice `v`'s part of [`mix_scalar`](Self::mix_scalar).
    fn mix_voice(&self, frames: usize, v: usize, input: &[f32], out: &mut [f32]) {
        let stride = self.voice_stride;
        let lanes = self.lanes_of(v);
        let (gl, gr) = (&self.left[lanes.clone()], &self.right[lanes.clone()]);
        for k in 0..frames {
            let row = &input[k * self.lane_count + lanes.start..k * self.lane_count + lanes.end];
            let (mut left, mut right) = (0.0, 0.0);
            for ((&x, &l), &r) in row.iter().zip(gl).zip(gr) {
                left += x * l;
                right += x * r;
            }
            out[k * 2 * stride + v] += left;
            out[k * 2 * stride + stride + v] += right;
        }
    }

    fn check(&self, frames: usize, voices: usize, input: &[f32], out: &[f32]) {
        assert!(
            voices <= self.voices,
            "{voices} voices, at most {}",
            self.voices
        );
        assert!(
            input.len() >= frames * self.lane_count,
            "{frames} frames of {} lanes",
            self.lane_count
        );
        assert!(
            out.len() >= frames * 2 * self.voice_stride,
            "{frames} stereo frames of {} voices",
            self.voice_stride
        );
    }

    fn draw(&mut self, voice: usize) {
        for lane in self.lanes_of(voice) {
            let position = unit(&mut self.rng).mul_add(2.0, -1.0);
            let start = unit(&mut self.rng);
            self.draws[lane] = Draws { position, start };
        }
    }

    fn write_all(&mut self) {
        let compensation = self.unison.compensation();
        for voice in 0..self.voices {
            self.write(voice, compensation);
        }
    }

    fn write(&mut self, voice: usize, compensation: f32) {
        let count = self.unison.count();
        for (i, lane) in self.lanes_of(voice).enumerate() {
            let copy = if i < count {
                self.unison.copy_with(i, self.draws[lane], compensation)
            } else {
                UnisonCopy::default()
            };
            self.pitch[lane] = copy.pitch;
            self.frame[lane] = copy.frame;
            self.shaper[0][lane] = copy.shaper[0];
            self.shaper[1][lane] = copy.shaper[1];
            self.start[lane] = copy.start;
            self.span[lane] = copy.span;
            self.left[lane] = copy.left;
            self.right[lane] = copy.right;
            self.left_padded[voice * MAX_COPIES + i] = copy.left;
            self.right_padded[voice * MAX_COPIES + i] = copy.right;
        }
    }
}

/// [`UnisonLanes::mix`]'s kernel.
struct Mix<'a> {
    lanes: &'a UnisonLanes,
    frames: usize,
    voices: usize,
    input: &'a [f32],
    out: &'a mut [f32],
}

impl Kernel for Mix<'_> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        let lanes = self.lanes;
        let (n, count, stride) = (lanes.lanes_per_voice, lanes.lane_count, lanes.voice_stride);
        if n == 1 {
            // A lane per voice: the lanes are the voices.
            let (gl, gr) = (&*lanes.left, &*lanes.right);
            let span = self.voices.div_ceil(S::LANES) * S::LANES;
            for k in 0..self.frames {
                let row = &self.input[k * count..(k + 1) * count];
                let (left, right) =
                    self.out[k * 2 * stride..(k + 1) * 2 * stride].split_at_mut(stride);
                for v in (0..span).step_by(S::LANES) {
                    let x = S::load(&row[v..]);
                    x.mul_add(S::load(&gl[v..]), S::load(&left[v..]))
                        .store(&mut left[v..]);
                    x.mul_add(S::load(&gr[v..]), S::load(&right[v..]))
                        .store(&mut right[v..]);
                }
            }
            return;
        }
        // Voice by voice, its gains held in registers: the `m` vectors from
        // the voice's first lane, against gains padded with zeros past its
        // copies, summed across once per frame. A voice whose vectors
        // would read past the row goes lane by lane.
        let m = n.div_ceil(S::LANES);
        for v in 0..self.voices {
            let base = v * n;
            if base + m * S::LANES > count {
                lanes.mix_voice(self.frames, v, self.input, self.out);
                continue;
            }
            let mut gl = [S::splat(0.0); 4];
            let mut gr = [S::splat(0.0); 4];
            for j in 0..m {
                gl[j] = S::load(&lanes.left_padded[v * MAX_COPIES + j * S::LANES..]);
                gr[j] = S::load(&lanes.right_padded[v * MAX_COPIES + j * S::LANES..]);
            }
            for k in 0..self.frames {
                let row = &self.input[k * count + base..];
                let x = S::load(row);
                let (mut l, mut r) = (x * gl[0], x * gr[0]);
                for j in 1..m {
                    let x = S::load(&row[j * S::LANES..]);
                    l = x.mul_add(gl[j], l);
                    r = x.mul_add(gr[j], r);
                }
                let at = k * 2 * stride + v;
                self.out[at] += l.sum();
                self.out[at + stride] += r.sum();
            }
        }
    }
}

/// A uniform value in [0, 1) from an xorshift state.
fn unit(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// The next value of a SplitMix64 sequence.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
