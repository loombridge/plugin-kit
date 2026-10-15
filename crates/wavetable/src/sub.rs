//! The sub oscillator (engine-core section 5, R40 to R44): six fixed
//! shapes, one per voice, at the note's pitch plus Octave and Sweep.
//!
//! | Item | What it is |
//! |---|---|
//! | [`Shape`] | The six waveforms: Sine, Rounded square, Triangle, Saw, Square and Pulse |
//! | [`Params`] | The sub's controls, shared by every voice: Octave, Sweep, Shape, Phase, Continue phase, Wrap phase, Key track and Level |
//! | [`Inputs`] | What a render reads per voice: pitch, its glide step, audio-rate pitch modulation and phase modulation |
//! | [`Sub`] | The voices' phases and keys, and the block render |
//!
//! # Phase
//!
//! A render works in cycles: `phase` is 0 at the start of a shape and 1 one
//! whole cycle on. Phase sets where a note starts (R41); with Continue
//! phase on, a note carries on from the phase its voice's last note left
//! instead. Phase modulation is added to the running phase (R42): with Wrap
//! phase on the sum wraps around a cycle; with it off the read position
//! stays at one whole cycle, so the shape holds its last value until the
//! modulation comes back.
//!
//! # Band limiting
//!
//! Saw, Square and Pulse keep their corners with a polyBLEP correction, so
//! a note near the top of the keyboard doesn't fold its harmonics back
//! down; Sine, Triangle and Rounded square are smooth by construction. The
//! sub is one voice per lane and there is no unison (R40).

use std::f32::consts::TAU;

use crate::osc::{MAX_VOICES, Onset};

/// The sub's six fixed shapes (R40).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Shape {
    /// A sine: the first harmonic alone.
    #[default]
    Sine,
    /// A square with raised-cosine corners.
    RoundedSquare,
    /// A triangle.
    Triangle,
    /// A rising saw.
    Saw,
    /// A square, half a cycle high.
    Square,
    /// A pulse, narrower than a square (R44).
    Pulse,
}

impl Shape {
    /// Every shape, in the order the Shape control lists them.
    pub const ALL: [Self; 6] = [
        Self::Sine,
        Self::RoundedSquare,
        Self::Triangle,
        Self::Saw,
        Self::Square,
        Self::Pulse,
    ];
}

/// How much of a cycle a rounded square spends turning at each corner.
const ROUNDING: f32 = 0.05;

/// Pulse's duty cycle: less than half a cycle, so the pulse is unequal
/// (R44). Its exact width is one of R7's measurements.
const PULSE_DUTY: f32 = 0.25;

/// The sub's controls, in real units, shared by every voice (section 5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Octave, −4 to +4 (R40).
    pub octave: i32,
    /// Sweep, −64 to +64 semitones, continuous (R40).
    pub sweep: f32,
    /// The shape it plays (R40).
    pub shape: Shape,
    /// Phase, 0 to 360 degrees: where a note starts (R41).
    pub phase: f32,
    /// Continue phase: a note carries on from the last note's phase (R41).
    pub continue_phase: bool,
    /// Wrap phase: phase modulated past 360° wraps (R42).
    pub wrap_phase: bool,
    /// Key tracking; off plays MIDI note 60, still following pitch bend and
    /// modulation (R5).
    pub key_track: bool,
    /// Level, 0 to 100 %, as gain n² (R43, R7).
    pub level: f32,
    /// The frequency of A4, in hertz (R136).
    pub reference: f32,
}

impl Default for Params {
    /// The defaults of a new patch: Sine, Phase 0°, Key track on, Level
    /// 75 % (section 5).
    fn default() -> Self {
        Self {
            octave: 0,
            sweep: 0.0,
            shape: Shape::Sine,
            phase: 0.0,
            continue_phase: false,
            wrap_phase: false,
            key_track: true,
            level: 75.0,
            reference: 440.0,
        }
    }
}

impl Params {
    /// The pitch offset Octave and Sweep add, in semitones (R40).
    #[must_use]
    pub fn tune(&self) -> f32 {
        12.0 * self.octave as f32 + self.sweep
    }

    /// The output gain Level gives: n² (R7).
    #[must_use]
    pub fn gain(&self) -> f32 {
        let n = (self.level / 100.0).clamp(0.0, 1.0);
        n * n
    }
}

/// What a render reads for each voice. One lane per voice: the sub has no
/// unison. Pitches are MIDI note numbers in semitones, as the kit's voice
/// block gives them, with glide, bend and expression; Octave and Sweep are
/// added here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inputs<'a> {
    /// Voices to render, from lane 0: at most [`Sub::lane_count`].
    pub lanes: usize,
    /// Each voice's pitch at the first frame.
    pub pitch: &'a [f32],
    /// How far each voice's pitch moves per frame (glide), if it moves.
    pub pitch_step: Option<&'a [f32]>,
    /// Pitch modulation at audio rate, in semitones, frame by frame
    /// (`frames × lane_count` values).
    pub pitch_mod: Option<&'a [f32]>,
    /// Phase modulation at audio rate, in cycles added to the running
    /// phase, frame by frame (`frames × lane_count` values); R42 decides
    /// what happens past a whole cycle.
    pub phase_mod: Option<&'a [f32]>,
}

/// One sub oscillator for every voice.
///
/// Construction allocates; nothing after it does, so every method that
/// takes `&mut self` but [`new`](Self::new) is safe on an audio thread.
#[derive(Debug)]
pub struct Sub {
    params: Params,
    sample_rate: f32,
    lane_count: usize,
    max_frames: usize,
    /// Each voice's phase in cycles, in [0, 1).
    phase: Box<[f32]>,
    /// Each voice's key, as a note number, for Key track off (R5).
    key: Box<[f32]>,
}

impl Sub {
    /// A sub playing for `voices` voices at `sample_rate`, rendering up to
    /// `max_frames` frames per call, with default [`Params`] and every
    /// voice's phase at the default's. Allocates; make it off the audio
    /// thread.
    ///
    /// # Panics
    ///
    /// If `voices` is outside 1 to [`MAX_VOICES`], or `max_frames` is 0.
    #[must_use]
    pub fn new(voices: usize, sample_rate: f32, max_frames: usize) -> Self {
        assert!(
            (1..=MAX_VOICES).contains(&voices) && max_frames > 0,
            "{voices} voices with {max_frames} frames"
        );
        let params = Params::default();
        Self {
            params,
            sample_rate,
            lane_count: voices,
            max_frames,
            phase: vec![phase_of(params.phase); voices].into(),
            key: vec![60.0; voices].into(),
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

    /// How many voices it holds.
    #[must_use]
    pub fn lane_count(&self) -> usize {
        self.lane_count
    }

    /// The most frames one render takes.
    #[must_use]
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Voice `voice`'s running phase, in cycles.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    #[must_use]
    pub fn phase(&self, voice: usize) -> f32 {
        self.phase[voice]
    }

    /// Voice `voice` starts playing `key` (a MIDI note number), as the
    /// voice manager reports `onset`: with Phase legato off, a fresh voice
    /// and a note joining a held one start at Phase, and a stolen,
    /// retriggered or legato voice carries on (R41); Continue phase keeps
    /// the phase across every onset instead.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn start(&mut self, voice: usize, key: f32, onset: Onset) {
        assert!(voice < self.lane_count, "no voice {voice}");
        self.key[voice] = key;
        if onset.restarts(false) && !self.params.continue_phase {
            self.phase[voice] = phase_of(self.params.phase);
        }
    }

    /// Puts every voice's phase back where it starts and forgets its key.
    pub fn reset(&mut self) {
        let start = phase_of(self.params.phase);
        for phase in &mut self.phase {
            *phase = start;
        }
        self.key.fill(60.0);
    }

    /// Memory this instance holds itself, in bytes.
    #[must_use]
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of_val(&*self.phase)
            + std::mem::size_of_val(&*self.key)
    }

    /// Renders `frames` frames of `inputs.lanes` voices into `out`, frame
    /// by frame (`out[k × lane_count + v]`).
    ///
    /// # Panics
    ///
    /// If `frames` is above [`max_frames`](Self::max_frames), or a slice is
    /// shorter than it must be: `lane_count` values for the per-lane
    /// inputs, `frames × lane_count` for the modulation and `out`.
    pub fn process(&mut self, frames: usize, inputs: &Inputs<'_>, out: &mut [f32]) {
        let n = self.lane_count;
        assert!(
            frames <= self.max_frames,
            "{frames} frames, at most {}",
            self.max_frames
        );
        assert!(inputs.lanes <= n, "{} voices, at most {n}", inputs.lanes);
        assert!(inputs.pitch.len() >= n, "pitch for {n} voices");
        assert!(
            inputs.pitch_step.is_none_or(|s| s.len() >= n),
            "pitch step for {n} voices"
        );
        for buffer in [inputs.pitch_mod, inputs.phase_mod, Some(&*out)]
            .into_iter()
            .flatten()
        {
            assert!(buffer.len() >= frames * n, "{frames} frames of {n} voices");
        }
        let p = self.params;
        let gain = p.gain();
        for v in 0..inputs.lanes {
            let varying =
                inputs.pitch_mod.is_some() || inputs.pitch_step.is_some_and(|s| s[v] != 0.0);
            let mut base = inputs.pitch[v] + p.tune();
            if !p.key_track {
                // R5: with key tracking off the sub plays MIDI note 60.
                base -= self.key[v] - 60.0;
            }
            let mut phase = self.phase[v];
            // A steady pitch converts once a render; a moving one per frame.
            let fixed = increment(base, self.sample_rate, p.reference);
            for k in 0..frames {
                let increment = if varying {
                    let at = base
                        + k as f32 * inputs.pitch_step.map_or(0.0, |s| s[v])
                        + inputs.pitch_mod.map_or(0.0, |m| m[k * n + v]);
                    increment(at, self.sample_rate, p.reference)
                } else {
                    fixed
                };
                let mut read = phase + inputs.phase_mod.map_or(0.0, |m| m[k * n + v]);
                read = if p.wrap_phase {
                    read.rem_euclid(1.0)
                } else {
                    read.clamp(0.0, 1.0)
                };
                out[k * n + v] = shape_value(p.shape, read, increment) * gain;
                phase += increment;
                phase -= phase.floor();
            }
            self.phase[v] = phase;
        }
    }
}

/// `degrees` as cycles, in [0, 1).
fn phase_of(degrees: f32) -> f32 {
    let cycles = degrees / 360.0;
    cycles - cycles.floor()
}

/// The increment, in cycles per sample, for `pitch` semitones.
fn increment(pitch: f32, sample_rate: f32, reference: f32) -> f32 {
    let frequency = f64::from(reference) * (f64::from(pitch - 69.0) / 12.0).exp2();
    (frequency / f64::from(sample_rate)) as f32
}

/// Shape `shape` at `phase` cycles for a lane rising `increment` cycles per
/// sample.
fn shape_value(shape: Shape, phase: f32, increment: f32) -> f32 {
    match shape {
        Shape::Sine => (TAU * phase).sin(),
        Shape::RoundedSquare => rounded_square(phase),
        Shape::Triangle => {
            // A triangle passing 0 at phase 0, peaking a quarter of a cycle
            // in, as the sine does.
            1.0 - 4.0 * ((phase + 0.25).fract() - 0.5).abs()
        }
        Shape::Saw => {
            // Rising through 0 at phase 0, its discontinuity half a cycle
            // on.
            let t = (phase + 0.5).fract();
            2.0 * t - 1.0 - poly_blep(t, increment)
        }
        Shape::Square => {
            let value = if phase < 0.5 { 1.0 } else { -1.0 };
            value + poly_blep(phase, increment) - poly_blep((phase + 0.5).fract(), increment)
        }
        Shape::Pulse => {
            let value = if phase < PULSE_DUTY { 1.0 } else { -1.0 };
            value + poly_blep(phase, increment)
                - poly_blep((phase + 1.0 - PULSE_DUTY).fract(), increment)
        }
    }
}

/// A square with raised-cosine corners [`ROUNDING`] wide, high through the
/// first half cycle and passing 0 at phase 0 like the sine. At phase 0 and
/// half a cycle the corner is half turned, so the shape is continuous.
fn rounded_square(phase: f32) -> f32 {
    let w = ROUNDING;
    let turn = |from: f32, to: f32, x: f32| {
        // 0 at `from`, 1 at `to`, with zero slope at both ends: a raised
        // cosine.
        let t = (x - from) / (to - from);
        0.5 - 0.5 * (std::f32::consts::PI * t.clamp(0.0, 1.0)).cos()
    };
    if phase < w {
        // Rising through the corner that spans phase 0.
        -1.0 + 2.0 * turn(0.0, 2.0 * w, phase + w)
    } else if phase < 0.5 - w {
        1.0
    } else if phase < 0.5 + w {
        1.0 - 2.0 * turn(0.5 - w, 0.5 + w, phase)
    } else if phase < 1.0 - w {
        -1.0
    } else {
        -1.0 + 2.0 * turn(1.0 - w, 1.0 + w, phase)
    }
}

/// The polyBLEP correction for a step at `t` = 0 of a wave whose phase
/// rises `dt` cycles per sample: the difference between a band-limited step
/// and the naive one, over the two samples around it. Add it for a step up,
/// subtract it for a step down.
fn poly_blep(t: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        0.0
    } else if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}
