//! The noise oscillator (engine-core section 6, R45 to R49): a mono or
//! stereo sample player with high-quality interpolation, and Reagent's own
//! colour noises.
//!
//! | Item | What it is |
//! |---|---|
//! | [`Colour`] | The four colours Reagent generates itself: White, Pink (−3 dB per octave), Brown (−6 dB per octave) and Crackle, a sparse stream of random clicks (R49) |
//! | [`Samples`] | The block a render reads: mono or stereo, interleaved, at its own rate; a colour's block is generated from a seed |
//! | [`Params`] | The player's controls, shared by every voice: Loop, Start, Start random, Rate, Fine, Key track, Stereo, Tone and Level |
//! | [`Inputs`] | What a render reads per voice: pitch, its glide step, and Start modulation (R47) |
//! | [`Noise`] | The voices' read positions and filter state, and the block render |
//!
//! # Playback
//!
//! A voice reads the block at `Rate` (50 % is the block's original rate,
//! R46) times Fine's detune, times the note's pitch when Key track is on.
//! Start moves the read position into the block and Start random spreads
//! each note's start over the block, so chords don't repeat the same noise
//! (R47). One-shot stops at the end of the block; Loop wraps (R48).
//!
//! # The colours
//!
//! The four colour blocks are generated in code from a seed: white is
//! uniform noise, pink a three-pole approximation of a −3 dB per octave
//! slope, brown white noise through a leaky integrator (−6 dB per octave)
//! and Crackle a stream of randomly spaced, decaying clicks. Every block
//! is scaled to the same RMS, so the colours sit at the same level. For
//! them, Stereo runs from one mono stream (0 %) to two uncorrelated ones
//! (100 %) and Tone tilts the result from low-passed to high-passed (R49).

use std::f32::consts::{FRAC_1_SQRT_2, PI, TAU};

use crate::osc::{MAX_VOICES, Onset};

/// A colour noise Reagent generates itself (R49).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Colour {
    /// Uniform noise: flat.
    White,
    /// −3 dB per octave.
    Pink,
    /// −6 dB per octave.
    Brown,
    /// Sparse random clicks.
    Crackle,
}

impl Colour {
    /// Every colour, in the order the Source control lists them.
    pub const ALL: [Self; 4] = [Self::White, Self::Pink, Self::Brown, Self::Crackle];
}

/// The rate the colour blocks are generated at: a block plays at its
/// original speed at any engine rate, because the player resamples it.
pub const COLOUR_RATE: f64 = 48_000.0;

/// How long a colour block is, in seconds.
pub const COLOUR_SECONDS: f32 = 1.0;

/// The RMS every colour block is scaled to.
const COLOUR_RMS: f64 = 0.25;

/// Crackle's clicks per second.
const CRACKLE_RATE: f64 = 25.0;

/// How long a Crackle click takes to decay to about a third, in seconds.
const CRACKLE_DECAY: f64 = 0.0015;

/// The cutoff Tone splits the spectrum at, in hertz.
const TONE_HZ: f64 = 800.0;

/// A block of samples: mono or stereo, interleaved, at its own rate.
#[derive(Debug)]
pub struct Samples {
    channels: usize,
    rate: f64,
    data: Box<[f32]>,
    /// The colour a generated block holds, so the player knows to apply
    /// Stereo and Tone to it (R49); `None` for a block from elsewhere.
    colour: Option<Colour>,
}

impl Samples {
    /// The block `data` holds: `channels` interleaved channels at `rate`
    /// frames per second. Allocates.
    ///
    /// # Panics
    ///
    /// If `channels` isn't 1 or 2, `data` is empty or not a whole number of
    /// frames, or `rate` isn't positive.
    #[must_use]
    pub fn new(channels: usize, rate: f64, data: Vec<f32>) -> Self {
        assert!(
            (1..=2).contains(&channels)
                && rate > 0.0
                && !data.is_empty()
                && data.len().is_multiple_of(channels),
            "{channels} channels, {rate} Hz, {} samples",
            data.len()
        );
        Self {
            channels,
            rate,
            data: data.into(),
            colour: None,
        }
    }

    /// A colour block of `seconds` seconds at `rate`, stereo and
    /// uncorrelated, generated from `seed` (R49). Allocates; make it off
    /// the audio thread.
    #[must_use]
    pub fn colour(colour: Colour, seconds: f32, rate: f64, seed: u64) -> Self {
        let frames = ((f64::from(seconds) * rate) as usize).max(2);
        let mut state = seed;
        let mut left = vec![0.0f32; frames];
        let mut right = vec![0.0f32; frames];
        match colour {
            Colour::White => {
                for (l, r) in left.iter_mut().zip(&mut right) {
                    *l = white(&mut state);
                    *r = white(&mut state);
                }
            }
            Colour::Pink => {
                pink(&mut left, &mut state);
                pink(&mut right, &mut state);
            }
            Colour::Brown => {
                brown(&mut left, rate, &mut state);
                brown(&mut right, rate, &mut state);
            }
            Colour::Crackle => {
                crackle(&mut left, rate, &mut state);
                crackle(&mut right, rate, &mut state);
            }
        }
        // One level for every colour, with no DC offset in any of them.
        let mut data = Vec::with_capacity(frames * 2);
        for (l, r) in left.iter().zip(&right) {
            data.push(*l);
            data.push(*r);
        }
        normalise(&mut data);
        Self {
            channels: 2,
            rate,
            data: data.into(),
            colour: Some(colour),
        }
    }

    /// The channels it holds.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The frames it holds.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.data.len() / self.channels
    }

    /// The rate it plays at its original speed, in frames per second.
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The samples, interleaved.
    #[must_use]
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// The colour it is, for a block [`colour`](Self::colour) generated.
    #[must_use]
    pub fn kind(&self) -> Option<Colour> {
        self.colour
    }

    /// The sample at frame `frame`, channel `channel`.
    #[inline]
    fn at(&self, frame: usize, channel: usize) -> f32 {
        self.data[frame * self.channels + channel.min(self.channels - 1)]
    }

    /// Frame `frame` read at `position` (fractional frames) with a
    /// four-point cubic, wrapping for a looping read and holding the ends
    /// for a one-shot.
    fn interpolate(&self, position: f64, channel: usize, wrap: bool) -> f32 {
        let frames = self.frames();
        if frames == 1 {
            return self.at(0, channel);
        }
        let first = position.floor();
        let t = (position - first) as f32;
        let first = first as isize;
        let point = |offset: isize| {
            let index = first + offset;
            let index = if wrap {
                index.rem_euclid(frames as isize)
            } else {
                index.clamp(0, frames as isize - 1)
            };
            self.at(index as usize, channel)
        };
        let (p0, p1, p2, p3) = (point(-1), point(0), point(1), point(2));
        0.5 * ((2.0 * p1)
            + (-p0 + p2) * t
            + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
            + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t)
    }
}

/// A uniform value in [−1, 1).
fn white(state: &mut u64) -> f32 {
    let x = next(state);
    (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

/// `out` filled with pink noise: the sum of three one-pole filters of the
/// white input, a published approximation of a −3 dB per octave slope.
fn pink(out: &mut [f32], state: &mut u64) {
    let (mut b0, mut b1, mut b2) = (0.0f32, 0.0f32, 0.0f32);
    for sample in out.iter_mut() {
        let w = white(state);
        b0 = 0.997_65 * b0 + w * 0.099_046;
        b1 = 0.963_00 * b1 + w * 0.296_516_4;
        b2 = 0.570_00 * b2 + w * 1.052_691_3;
        *sample = b0 + b1 + b2 + w * 0.184_8;
    }
}

/// `out` filled with brown noise: white noise through a leaky integrator,
/// whose leak keeps the very low frequencies from wandering, leaving the
/// audio band at −6 dB per octave.
fn brown(out: &mut [f32], rate: f64, state: &mut u64) {
    let leak = (-TAU as f64 * 1.0 / rate).exp() as f32;
    let mut low = 0.0f32;
    for sample in out.iter_mut() {
        low = low * leak + white(state);
        *sample = low;
    }
}

/// `out` filled with randomly spaced clicks: at each sample a click may
/// start, decaying away over [`CRACKLE_DECAY`].
fn crackle(out: &mut [f32], rate: f64, state: &mut u64) {
    let event = CRACKLE_RATE / rate;
    let decay = (-1.0 / (CRACKLE_DECAY * rate)).exp() as f32;
    let mut click = 0.0f32;
    for sample in out.iter_mut() {
        if next(state) as f64 / (u64::MAX as f64 + 1.0) > 1.0 - event {
            click = if next(state) >> 63 == 0 { 1.0 } else { -1.0 };
        }
        click *= decay;
        *sample = click;
    }
}

/// Removes the mean and scales `data` to [`COLOUR_RMS`].
fn normalise(data: &mut [f32]) {
    // One pass to measure the mean and the power, one to remove and scale.
    let mut sum = 0.0f64;
    let mut squares = 0.0f64;
    for &x in data.iter() {
        sum += f64::from(x);
        squares += f64::from(x) * f64::from(x);
    }
    let n = data.len() as f64;
    let mean = sum / n;
    let variance = (squares / n - mean * mean).max(0.0);
    let gain = if variance > 0.0 {
        (COLOUR_RMS / variance.sqrt()) as f32
    } else {
        0.0
    };
    let mean = mean as f32;
    for sample in data.iter_mut() {
        *sample = (*sample - mean) * gain;
    }
}

/// The next value of a SplitMix64 sequence.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
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

/// The noise player's controls, in real units, shared by every voice
/// (section 6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Loop: one-shot stops at the end of the block, Loop wraps (R48).
    pub looped: bool,
    /// Start, 0 to 100 % of the block (R47).
    pub start: f32,
    /// Start random, 0 to 100 %: how far each note's start is spread
    /// (R47).
    pub start_random: f32,
    /// Rate, 0 to 100 %, 50 % the block's original speed (R46).
    pub rate: f32,
    /// Fine, −1 to +1: a fine detune, in semitones.
    pub fine: f32,
    /// Key tracking: the pitch follows the note (R46).
    pub key_track: bool,
    /// Stereo, 0 to 100 %, for the colour noises only: one mono stream to
    /// two uncorrelated ones (R49).
    pub stereo: f32,
    /// Tone, 0 to 100 %, for the colour noises only: low-passed at 0,
    /// flat at 50, high-passed at 100 (R49).
    pub tone: f32,
    /// Level, 0 to 100 %, as gain n² (R7).
    pub level: f32,
    /// The frequency of A4, in hertz (R136).
    pub reference: f32,
}

impl Default for Params {
    /// The defaults of a new patch: Loop, Start 0 %, Start random 0 %,
    /// Rate 50 %, Key track off, Tone flat, Level 25 % (section 6).
    fn default() -> Self {
        Self {
            looped: true,
            start: 0.0,
            start_random: 0.0,
            rate: 50.0,
            fine: 0.0,
            key_track: false,
            stereo: 0.0,
            tone: 50.0,
            level: 25.0,
            reference: 440.0,
        }
    }
}

impl Params {
    /// The output gain Level gives: n² (R7).
    #[must_use]
    pub fn gain(&self) -> f32 {
        let n = (self.level / 100.0).clamp(0.0, 1.0);
        n * n
    }

    /// The playback speed Rate gives the block against its original rate:
    /// provisional, spanning a quarter to four times, corrected by R7's
    /// measurement.
    #[must_use]
    pub fn speed(&self) -> f32 {
        ((self.rate - 50.0) / 25.0).exp2()
    }
}

/// What a render reads for each voice. One lane per voice. Pitches are MIDI
/// note numbers in semitones, as the kit's voice block gives them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inputs<'a> {
    /// Voices to render, from lane 0: at most [`Noise::lane_count`].
    pub lanes: usize,
    /// Each voice's pitch at the first frame.
    pub pitch: &'a [f32],
    /// How far each voice's pitch moves per frame (glide), if it moves.
    pub pitch_step: Option<&'a [f32]>,
    /// Start modulation at audio rate, 0 to 100 % added to Start, frame by
    /// frame (`frames × lane_count` values); the control to modulate for a
    /// scratching effect (R47).
    pub start_mod: Option<&'a [f32]>,
}

/// One noise oscillator for every voice.
///
/// Construction allocates; nothing after it does, so every method that
/// takes `&mut self` but [`new`](Self::new) is safe on an audio thread.
#[derive(Debug)]
pub struct Noise {
    params: Params,
    sample_rate: f32,
    lane_count: usize,
    max_frames: usize,
    /// Each voice's read position as a fraction of the block, in [0, 1).
    position: Box<[f64]>,
    /// Whether a one-shot voice has read past the end of its block.
    finished: Box<[bool]>,
    rng: Box<[u32]>,
    /// Each voice's Tone filter state, one per channel.
    tone_l: Box<[f32]>,
    tone_r: Box<[f32]>,
}

impl Noise {
    /// A player for `voices` voices at `sample_rate`, rendering up to
    /// `max_frames` frames per call, with default [`Params`] and each
    /// voice's random source seeded from `seed`. Allocates; make it off
    /// the audio thread.
    ///
    /// # Panics
    ///
    /// If `voices` is outside 1 to [`MAX_VOICES`], or `max_frames` is 0.
    #[must_use]
    pub fn new(voices: usize, sample_rate: f32, max_frames: usize, seed: u64) -> Self {
        assert!(
            (1..=MAX_VOICES).contains(&voices) && max_frames > 0,
            "{voices} voices with {max_frames} frames"
        );
        let mut seed = seed;
        Self {
            params: Params::default(),
            sample_rate,
            lane_count: voices,
            max_frames,
            position: vec![0.0; voices].into(),
            finished: vec![false; voices].into(),
            rng: (0..voices).map(|_| (next(&mut seed) as u32) | 1).collect(),
            tone_l: vec![0.0; voices].into(),
            tone_r: vec![0.0; voices].into(),
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

    /// Voice `voice`'s read position as a fraction of its block.
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    #[must_use]
    pub fn position(&self, voice: usize) -> f64 {
        self.position[voice]
    }

    /// Voice `voice` starts reading, as the voice manager reports `onset`:
    /// a fresh note and a note joining a held one take their start
    /// position (R47), with Start random's random offset drawn per voice; a
    /// stolen, retriggered or legato note carries on, as the oscillators'
    /// and the sub's phases do (R18).
    ///
    /// # Panics
    ///
    /// If there's no voice `voice`.
    pub fn start(&mut self, voice: usize, onset: Onset) {
        assert!(voice < self.lane_count, "no voice {voice}");
        if !onset.restarts(false) {
            return;
        }
        let random = f64::from(unit(&mut self.rng[voice])) * f64::from(self.params.start_random);
        self.position[voice] = (f64::from(self.params.start) + random) / 100.0;
        self.position[voice] -= self.position[voice].floor();
        self.finished[voice] = false;
    }

    /// Puts every voice back at its start and clears the Tone filters.
    pub fn reset(&mut self) {
        let start = f64::from(self.params.start) / 100.0;
        for position in &mut self.position {
            *position = start;
        }
        self.finished.fill(false);
        self.tone_l.fill(0.0);
        self.tone_r.fill(0.0);
    }

    /// Memory this instance holds itself, in bytes.
    #[must_use]
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of_val(&*self.position)
            + std::mem::size_of_val(&*self.finished)
            + std::mem::size_of_val(&*self.rng)
            + std::mem::size_of_val(&*self.tone_l)
            + std::mem::size_of_val(&*self.tone_r)
    }

    /// Renders `frames` frames of `inputs.lanes` voices from `source` into
    /// `out`, frame by frame, both channels: `out[(k × 2 + c) × lane_count
    /// + v]`.
    ///
    /// # Panics
    ///
    /// If `frames` is above [`max_frames`](Self::max_frames), or a slice is
    /// shorter than it must be: `lane_count` values for the per-lane
    /// inputs, `frames × lane_count` for the modulation and `frames × 2 ×
    /// lane_count` for `out`.
    pub fn process(
        &mut self,
        frames: usize,
        inputs: &Inputs<'_>,
        source: &Samples,
        out: &mut [f32],
    ) {
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
        assert!(
            out.len() >= frames * 2 * n,
            "{frames} frames of {n} voices, two channels"
        );
        if let Some(modulation) = inputs.start_mod {
            assert!(
                modulation.len() >= frames * n,
                "{frames} frames of {n} voices"
            );
        }
        let p = self.params;
        let gain = p.gain();
        let colour = source.kind().is_some();
        let frames_in_source = source.frames() as f64;
        let speed = f64::from(p.speed()) * f64::from((p.fine / 12.0).exp2()) * source.rate()
            / f64::from(self.sample_rate)
            / frames_in_source;
        let stereo = if colour {
            p.stereo.clamp(0.0, 100.0) / 100.0
        } else {
            0.0
        };
        let (mono_gain, side_gain) = (
            (0.5 * PI * stereo).cos() * FRAC_1_SQRT_2,
            (0.5 * PI * stereo).sin(),
        );
        let tilt = if colour {
            p.tone.clamp(0.0, 100.0) / 100.0
        } else {
            0.5
        };
        let tone_k = 1.0 - (-TAU as f64 * TONE_HZ / f64::from(self.sample_rate)).exp() as f32;
        for v in 0..inputs.lanes {
            let mut position = self.position[v];
            let finished = &mut self.finished[v];
            let (mut tone_l, mut tone_r) = (self.tone_l[v], self.tone_r[v]);
            for k in 0..frames {
                let pitch = inputs.pitch[v] + k as f32 * inputs.pitch_step.map_or(0.0, |s| s[v]);
                // R5: with key tracking off the noise plays MIDI note 60.
                let key = if p.key_track {
                    (f64::from(pitch - 60.0) / 12.0).exp2()
                } else {
                    1.0
                };
                let start = inputs.start_mod.map_or(0.0, |m| f64::from(m[k * n + v]));
                let mut at = position + start / 100.0;
                at -= at.floor();
                let (l, r) = if *finished {
                    (0.0, 0.0)
                } else {
                    let read = at * frames_in_source;
                    (
                        source.interpolate(read, 0, p.looped),
                        source.interpolate(read, source.channels() - 1, p.looped),
                    )
                };
                let (mut l, mut r) = if colour {
                    let mono = (l + r) * FRAC_1_SQRT_2;
                    (
                        mono_gain * mono + side_gain * l,
                        mono_gain * mono + side_gain * r,
                    )
                } else {
                    (l, r)
                };
                if colour {
                    tone_l += tone_k * (l - tone_l);
                    tone_r += tone_k * (r - tone_r);
                    l = 2.0 * ((1.0 - tilt) * tone_l + tilt * (l - tone_l));
                    r = 2.0 * ((1.0 - tilt) * tone_r + tilt * (r - tone_r));
                }
                out[(k * 2) * n + v] = l * gain;
                out[(k * 2 + 1) * n + v] = r * gain;
                if !*finished {
                    position += speed * key;
                    if p.looped {
                        position -= position.floor();
                    } else if position >= 1.0 {
                        *finished = true;
                    }
                }
            }
            self.position[v] = position;
            self.tone_l[v] = tone_l;
            self.tone_r[v] = tone_r;
        }
    }
}
