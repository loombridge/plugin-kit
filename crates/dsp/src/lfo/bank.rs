//! An LFO slot for every voice, processed a block at a time, with a SIMD
//! path across voices.

use std::ops::{Add, Div, Mul, Sub};

use super::shape::{curve, path_point};
use super::{Block, Lane, Lfo, LfoType, LfoVoice, Mode, Transport, chaos, post_lanes};
use crate::simd::{self, F32s, Kernel, Level};

/// An LFO slot with a state for each of a fixed number of voices.
///
/// [`process`](Self::process) fills a block for every voice, frame by
/// frame: `out[k × voices + v]` is voice `v`'s value at sample `k`, the
/// layout a synth whose voices sit in SIMD lanes reads. With
/// [`Settings::shared`](super::Settings::shared) on, one state plays for all
/// voices and every voice reads the same value.
///
/// The SIMD path ([`process_at`](Self::process_at)) runs each voice's
/// position and segment bookkeeping one voice at a time, then the curve,
/// the path, the chaos integrator, the fade in and the smoothing across
/// voices in SIMD lanes. It gives the same output as
/// [`process_scalar`](Self::process_scalar), to the bit, and
/// [`process`](Self::process) picks whichever is quicker.
#[derive(Debug)]
pub struct LfoBank {
    /// The slot: its settings, shape and path.
    pub lfo: Lfo,
    voices: Vec<LfoVoice>,
    shared: LfoVoice,
    pending: Vec<Option<usize>>,
    shared_pending: Option<usize>,
    lanes: Lanes,
}

/// The per-voice values the SIMD stage reads and writes, one array per
/// value, padded to a whole number of the widest vectors.
#[derive(Debug)]
struct Lanes {
    u: Vec<f32>,
    g: Vec<f32>,
    capture: Vec<f32>,
    /// Drawn and sample-and-hold coefficients.
    y0: Vec<f32>,
    dy: Vec<f32>,
    e: Vec<f32>,
    /// XY path coefficients.
    c: [Vec<f32>; 6],
    /// Chaos state and step.
    s: [Vec<f32>; 3],
    h: Vec<f32>,
    /// Start value and smoothing state for X and Y.
    start: [Vec<f32>; 2],
    smoothed: [Vec<f32>; 2],
    out: [Vec<f32>; 2],
}

impl Lanes {
    fn new(voices: usize) -> Self {
        let width = voices.div_ceil(16) * 16;
        let zeros = || vec![0.0; width];
        Self {
            u: zeros(),
            g: vec![1.0; width],
            capture: zeros(),
            y0: zeros(),
            dy: zeros(),
            e: vec![1.0; width],
            c: std::array::from_fn(|_| zeros()),
            s: std::array::from_fn(|i| vec![chaos::LORENZ.start[i]; width]),
            h: zeros(),
            start: std::array::from_fn(|_| zeros()),
            smoothed: std::array::from_fn(|_| zeros()),
            out: std::array::from_fn(|_| zeros()),
        }
    }

    /// Takes voice `v`'s coefficients for `kind`.
    fn load(&mut self, kind: LfoType, v: usize, voice: &LfoVoice) {
        match kind {
            LfoType::Drawn => {
                self.y0[v] = voice.seg.y0;
                self.dy[v] = voice.seg.dy;
                self.e[v] = voice.seg.e;
            }
            LfoType::SampleAndHold => {
                self.y0[v] = voice.held;
                self.dy[v] = 0.0;
                self.e[v] = 1.0;
            }
            LfoType::Path => {
                for (lane, c) in self.c.iter_mut().zip(voice.path_seg.c) {
                    lane[v] = c;
                }
            }
            LfoType::Lorenz | LfoType::Rossler => self.h[v] = voice.chaos_h,
        }
    }
}

impl LfoBank {
    /// `lfo` for `voices` voices, each with its own random values.
    /// Allocates; make it off the audio thread.
    #[must_use]
    pub fn new(lfo: Lfo, voices: usize) -> Self {
        Self {
            voices: (0..voices).map(|v| lfo.voice(v)).collect(),
            shared: lfo.voice(usize::MAX - 1),
            pending: vec![None; voices],
            shared_pending: None,
            lanes: Lanes::new(voices),
            lfo,
        }
    }

    /// How many voices it has.
    #[must_use]
    pub fn voices(&self) -> usize {
        self.voices.len()
    }

    /// Voice `v`'s state, to set its point-modulation buses.
    ///
    /// # Panics
    ///
    /// If there's no voice `v`.
    pub fn voice_mut(&mut self, v: usize) -> &mut LfoVoice {
        &mut self.voices[v]
    }

    /// The shared state, played for every voice when the slot is shared.
    pub fn shared_mut(&mut self) -> &mut LfoVoice {
        &mut self.shared
    }

    /// A note-on for voice `v` just before sample `at` of the next block
    /// processed. One per voice per block; a later call replaces an
    /// earlier. When the slot is shared, any voice's note-on starts the
    /// shared state, the latest in the block winning. A voice past the last
    /// is ignored.
    pub fn trigger(&mut self, v: usize, at: usize) {
        if let Some(slot) = self.pending.get_mut(v) {
            *slot = Some(at);
            self.shared_pending = Some(self.shared_pending.map_or(at, |p| p.max(at)));
        }
    }

    /// Fills `out` (and `out_y`, the XY path's Y output; 0 for the other
    /// types) with `frames` samples for every voice, frame by frame, by
    /// whichever path is quicker. The chaos types always run across voices
    /// in SIMD lanes, at the level this CPU dispatches: the integrator's
    /// arithmetic dominates, and the lanes take about a third of the time on
    /// Apple Silicon's NEON and half the instructions with AVX2. The drawn
    /// shape, the path and sample and hold use the lanes only when they're
    /// at least 8 wide (AVX2 and AVX-512, where they take a fifth to a third
    /// fewer instructions); with 4 lanes the per-voice position and segment
    /// work dominates, and on NEON the lanes are no quicker.
    /// `transport` is the host's at the block's first sample.
    ///
    /// # Panics
    ///
    /// If `out` (or `out_y`) holds fewer than `frames × voices` values.
    pub fn process(
        &mut self,
        transport: &Transport,
        frames: usize,
        out: &mut [f32],
        out_y: Option<&mut [f32]>,
    ) {
        let level = simd::level();
        let chaos = matches!(self.lfo.settings.kind, LfoType::Lorenz | LfoType::Rossler);
        if chaos || level.lanes() >= 8 {
            self.process_at(level, transport, frames, out, out_y);
        } else {
            self.process_scalar(transport, frames, out, out_y);
        }
    }

    /// [`process`](Self::process) across voices in SIMD lanes at `level`, or
    /// the portable level if this CPU doesn't have it, whatever the type.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_at(
        &mut self,
        level: Level,
        transport: &Transport,
        frames: usize,
        out: &mut [f32],
        out_y: Option<&mut [f32]>,
    ) {
        let block = self.lfo.prepare(transport, frames);
        if self.lfo.settings.shared {
            self.process_shared(&block, out, out_y);
            return;
        }
        let run = Run {
            lfo: &self.lfo,
            block: &block,
            voices: &mut self.voices,
            pending: &mut self.pending,
            lanes: &mut self.lanes,
            out,
            out_y,
        };
        if let Err(run) = simd::dispatch_at(level, run) {
            // The portable level runs anywhere.
            let _ = simd::dispatch_at(Level::Portable, run);
        }
        self.shared_pending = None;
    }

    /// [`process`](Self::process) one voice at a time, without SIMD.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_scalar(
        &mut self,
        transport: &Transport,
        frames: usize,
        out: &mut [f32],
        mut out_y: Option<&mut [f32]>,
    ) {
        let block = self.lfo.prepare(transport, frames);
        if self.lfo.settings.shared {
            self.process_shared(&block, out, out_y);
            return;
        }
        let stride = self.voices.len();
        for (v, voice) in self.voices.iter_mut().enumerate() {
            let at = self.pending[v].take().map(|at| at.min(frames));
            render_parts(&self.lfo, &block, voice, at, |k, x, y| {
                out[k * stride + v] = x;
                if let Some(o) = out_y.as_deref_mut() {
                    o[k * stride + v] = y;
                }
            });
        }
        self.shared_pending = None;
    }

    /// The shared state, rendered once and copied to every voice.
    fn process_shared(&mut self, block: &Block, out: &mut [f32], mut out_y: Option<&mut [f32]>) {
        let stride = self.voices.len();
        let at = self.shared_pending.take().map(|at| at.min(block.frames));
        self.pending.fill(None);
        render_parts(&self.lfo, block, &mut self.shared, at, |k, x, y| {
            out[k * stride..(k + 1) * stride].fill(x);
            if let Some(o) = out_y.as_deref_mut() {
                o[k * stride..(k + 1) * stride].fill(y);
            }
        });
    }
}

/// Renders `voice` over the whole block, triggering it before sample `at`.
fn render_parts(
    lfo: &Lfo,
    block: &Block,
    voice: &mut LfoVoice,
    at: Option<usize>,
    mut emit: impl FnMut(usize, f32, f32),
) {
    let split = at.unwrap_or(block.frames);
    lfo.render_with(block, voice, 0, split, &mut emit);
    if let Some(at) = at {
        lfo.trigger(block, voice, at);
        lfo.render_with(block, voice, at, block.frames - at, |i, x, y| {
            emit(at + i, x, y);
        });
    }
}

/// The SIMD path's kernel.
struct Run<'a> {
    lfo: &'a Lfo,
    block: &'a Block,
    voices: &'a mut [LfoVoice],
    pending: &'a mut [Option<usize>],
    lanes: &'a mut Lanes,
    out: &'a mut [f32],
    out_y: Option<&'a mut [f32]>,
}

impl Kernel for Run<'_> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        let Self {
            lfo,
            block,
            voices,
            pending,
            lanes,
            out,
            mut out_y,
        } = self;
        let kind = lfo.settings.kind;
        let chaos = matches!(kind, LfoType::Lorenz | LfoType::Rossler);
        let lorenz = kind == LfoType::Lorenz;
        let two = kind == LfoType::Path;
        let stride = voices.len();
        let width = stride.div_ceil(S::LANES) * S::LANES;

        for (v, voice) in voices.iter_mut().enumerate() {
            lfo.begin(block, voice, 0);
            lanes.load(kind, v, voice);
            for i in 0..2 {
                lanes.start[i][v] = voice.start[i];
                lanes.smoothed[i][v] = voice.smoothed[i];
            }
            for i in 0..3 {
                lanes.s[i][v] = voice.chaos[i];
            }
        }

        for k in 0..block.frames {
            // Each voice's position and bookkeeping, one at a time.
            let mut plain = true;
            for (v, voice) in voices.iter_mut().enumerate() {
                if pending[v] == Some(k) {
                    lfo.trigger(block, voice, k);
                    lfo.begin(block, voice, k);
                    if chaos && lfo.settings.mode != Mode::Free {
                        for i in 0..3 {
                            lanes.s[i][v] = voice.chaos[i];
                        }
                    }
                }
                let stage = lfo.stage(block, voice, k);
                lanes.u[v] = stage.u;
                lanes.g[v] = stage.g;
                lanes.capture[v] = if stage.capture { 1.0 } else { 0.0 };
                plain &= !stage.capture && stage.g == 1.0;
                if stage.changed {
                    lanes.load(kind, v, voice);
                }
            }
            // The arithmetic, across voices.
            for o in (0..width).step_by(S::LANES) {
                let (x, y) = match kind {
                    LfoType::Drawn | LfoType::SampleAndHold => (
                        curve(
                            ld::<S>(&lanes.u, o),
                            ld(&lanes.y0, o),
                            ld(&lanes.dy, o),
                            ld(&lanes.e, o),
                        ),
                        W(S::splat(0.0)),
                    ),
                    LfoType::Path => path_point(
                        ld::<S>(&lanes.u, o),
                        std::array::from_fn(|i| ld(&lanes.c[i], o)),
                    ),
                    LfoType::Lorenz | LfoType::Rossler => {
                        let s = chaos::advance(
                            lorenz,
                            std::array::from_fn(|i| ld::<S>(&lanes.s[i], o)),
                            ld(&lanes.h, o),
                            block.chaos_steps,
                        );
                        for (i, value) in s.iter().enumerate() {
                            st(*value, &mut lanes.s[i], o);
                        }
                        (chaos::output(lorenz, s[0]), W(S::splat(0.0)))
                    }
                };
                for (i, raw) in [x, y].into_iter().enumerate().take(if two { 2 } else { 1 }) {
                    // With no voice starting or fading, the selects would
                    // leave the start values as they are and pass the raw
                    // value through, so skip them.
                    let value = if plain {
                        match block.smooth {
                            Some(a) => {
                                let s = ld::<S>(&lanes.smoothed[i], o);
                                s + (raw - s) * W(S::splat(a))
                            }
                            None => raw,
                        }
                    } else {
                        let (value, start, _) = post_lanes(
                            raw,
                            ld(&lanes.g, o),
                            ld(&lanes.capture, o),
                            ld(&lanes.start[i], o),
                            ld(&lanes.smoothed[i], o),
                            block.smooth,
                        );
                        st(start, &mut lanes.start[i], o);
                        value
                    };
                    st(value, &mut lanes.out[i], o);
                    st(value, &mut lanes.smoothed[i], o);
                }
            }
            out[k * stride..(k + 1) * stride].copy_from_slice(&lanes.out[0][..stride]);
            if let Some(o) = out_y.as_deref_mut() {
                let o = &mut o[k * stride..(k + 1) * stride];
                if two {
                    o.copy_from_slice(&lanes.out[1][..stride]);
                } else {
                    o.fill(0.0);
                }
            }
        }

        for (v, voice) in voices.iter_mut().enumerate() {
            for i in 0..2 {
                voice.start[i] = lanes.start[i][v];
                voice.smoothed[i] = lanes.smoothed[i][v];
            }
            for i in 0..3 {
                voice.chaos[i] = lanes.s[i][v];
            }
            // A note-on at the block's end starts the voice for the next.
            if pending[v].is_some_and(|at| at >= block.frames) {
                lfo.trigger(block, voice, block.frames);
            }
        }
        pending.fill(None);
    }
}

/// SIMD lanes as a [`Lane`].
#[derive(Clone, Copy)]
struct W<S>(S);

#[inline(always)]
fn ld<S: F32s>(values: &[f32], at: usize) -> W<S> {
    W(S::load(&values[at..]))
}

#[inline(always)]
fn st<S: F32s>(value: W<S>, to: &mut [f32], at: usize) {
    value.0.store(&mut to[at..]);
}

impl<S: F32s> Add for W<S> {
    type Output = Self;
    #[inline(always)]
    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl<S: F32s> Sub for W<S> {
    type Output = Self;
    #[inline(always)]
    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl<S: F32s> Mul for W<S> {
    type Output = Self;
    #[inline(always)]
    fn mul(self, other: Self) -> Self {
        Self(self.0 * other.0)
    }
}

impl<S: F32s> Div for W<S> {
    type Output = Self;
    #[inline(always)]
    fn div(self, other: Self) -> Self {
        Self(self.0 / other.0)
    }
}

impl<S: F32s> Lane for W<S> {
    #[inline(always)]
    fn splat(value: f32) -> Self {
        Self(S::splat(value))
    }
    #[inline(always)]
    fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }
    #[inline(always)]
    fn max(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }
}
