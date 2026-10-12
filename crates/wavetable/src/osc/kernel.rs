//! The render across lanes, written once for every SIMD level.
//!
//! Per vector of lanes: the phase increments for the render from the
//! pitches (2^x in the lanes), then stretch by stretch of [`SUB_BLOCK`]
//! frames, each lane's mipmap level, then per frame the read: the phase's
//! index and fraction in the level (a 32 × 32-bit multiply), six spline
//! coefficients gathered per lane, and the quintic B-spline, all in the
//! lanes.

use lspk_dsp::simd::{F32s, Kernel, U32s, exp2};

use super::{Inputs, Lanes, Levels, MAX_EXPONENT, PhaseStage, Plan, SUB_BLOCK, WaveStage};
use crate::Table;

/// One render, handed to [`dispatch`](lspk_dsp::simd::dispatch_at).
pub(super) struct Run<'a, 'i, P, W> {
    pub table: &'a Table,
    pub levels: &'a Levels,
    pub plan: Plan,
    pub lanes: &'a mut Lanes,
    pub lane_count: usize,
    pub frames: usize,
    pub inputs: &'a Inputs<'i>,
    pub out: &'a mut [f32],
    pub phase_stage: &'a mut P,
    pub wave_stage: &'a mut W,
}

impl<P: PhaseStage, W: WaveStage> Kernel for Run<'_, '_, P, W> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(mut self) {
        if self.frames == 0 {
            return;
        }
        let span = self.inputs.lanes.div_ceil(S::LANES) * S::LANES;
        for chunk in (0..span).step_by(S::LANES) {
            self.chunk::<S>(chunk);
        }
    }
}

/// One mipmap level per lane: where each lane's frame 0 starts, the
/// distance between frames, and the samples in a cycle.
#[derive(Clone, Copy)]
struct LaneLevels<U> {
    offset: U,
    stride: U,
    size: U,
}

/// The frame each lane reads, and with Smooth frames the next and how far
/// towards it.
#[derive(Clone, Copy)]
struct Frames<S: F32s> {
    first: S::U32s,
    second: S::U32s,
    toward: S,
}

impl<P: PhaseStage, W: WaveStage> Run<'_, '_, P, W> {
    /// Each lane's phase increment per frame at its pitch `pitch`, in
    /// semitones.
    #[inline(always)]
    fn increment<S: F32s>(&self, pitch: S) -> S::U32s {
        let exponent = pitch.mul_add(S::splat(1.0 / 12.0), S::splat(self.plan.exp_offset));
        exp2(exponent.max(S::splat(0.0)).min(S::splat(MAX_EXPONENT))).to_i32()
    }

    /// Renders lanes `c..c + S::LANES`.
    #[inline(always)]
    fn chunk<S: F32s>(&mut self, c: usize) {
        let n = self.frames;
        let stride = self.lane_count;
        let inputs = self.inputs;
        let plan = self.plan;

        let mut base = S::load(&inputs.pitch[c..])
            + S::load(&self.lanes.pitch_offset[c..])
            + S::splat(plan.tune);
        if !plan.key_track {
            // Note 0 in place of the key (R5), keeping bend and modulation
            // (R6).
            base -= S::load(&self.lanes.key[c..]);
        }

        // The increments: one for the render, or one per frame and one
        // for the look ahead past it.
        let moving = |s: &[f32]| s[c..c + S::LANES].iter().any(|&step| step != 0.0);
        let varying = inputs.pitch_mod.is_some() || inputs.pitch_step.is_some_and(moving);
        let fixed = self.increment::<S>(base);
        if varying {
            let step = inputs
                .pitch_step
                .map_or(S::splat(0.0), |s| S::load(&s[c..]));
            let modulation = |k: usize| inputs.pitch_mod.map(|m| S::load(&m[k * stride + c..]));
            for k in 0..n {
                let mut pitch = S::splat(k as f32).mul_add(step, base);
                if let Some(m) = modulation(k) {
                    pitch += m;
                }
                let increment = self.increment::<S>(pitch);
                increment.store(&mut self.lanes.increments[k * S::LANES..]);
            }
            // A stretch on: the last stretch's rise carried on (a fall is
            // ignored, which would make the level richer).
            let pitch_at = |k: usize| {
                let pitch = S::splat(k as f32).mul_add(step, base);
                modulation(k).map_or(pitch, |m| pitch + m)
            };
            let back = (n - 1).min(SUB_BLOCK);
            let last = pitch_at(n - 1);
            let rise = if back == 0 {
                step * S::splat(SUB_BLOCK as f32)
            } else {
                (last - pitch_at(n - 1 - back)) * S::splat(SUB_BLOCK as f32 / back as f32)
            };
            let increment = self.increment::<S>(last + rise.max(S::splat(0.0)));
            increment.store(&mut self.lanes.increments[n * S::LANES..]);
        }

        let mut phase = <S::U32s>::load(&self.lanes.phase[c..]);
        let frame_base = S::splat(plan.frame) + S::load(&self.lanes.frame_offset[c..]);
        let mut start = 0;
        while start < n {
            let len = (n - start).min(SUB_BLOCK);
            // The highest increment in this stretch, and in it and the
            // next (and past the render when the next reaches its end).
            let (own, peak) = if varying {
                let increment = |k: usize| <S::U32s>::load(&self.lanes.increments[k * S::LANES..]);
                let mut own = <S::U32s>::splat(0);
                for k in start..start + len {
                    own = own.max(increment(k));
                }
                let end = start + len + SUB_BLOCK;
                let mut peak = own;
                for k in start + len..end.min(n) {
                    peak = peak.max(increment(k));
                }
                if end >= n {
                    peak = peak.max(increment(n));
                }
                (own, peak)
            } else {
                (fixed, fixed)
            };
            let looked = start..(start + len + SUB_BLOCK).min(n);
            let (old, new, faded) = self.choose_levels::<S>(c, own, peak, looked);
            let stretch = Stretch {
                c,
                start,
                len,
                varying,
                fixed,
                frame_base,
                old,
                new,
                faded: faded.unwrap_or(S::splat(0.0)),
            };
            let fade = faded.is_some();
            phase = match (fade, plan.smooth, inputs.frame_mod.is_some()) {
                (false, false, false) => self.stretch::<S, false, false, false>(&stretch, phase),
                (false, false, true) => self.stretch::<S, false, false, true>(&stretch, phase),
                (false, true, false) => self.stretch::<S, false, true, false>(&stretch, phase),
                (false, true, true) => self.stretch::<S, false, true, true>(&stretch, phase),
                (true, false, false) => self.stretch::<S, true, false, false>(&stretch, phase),
                (true, false, true) => self.stretch::<S, true, false, true>(&stretch, phase),
                (true, true, false) => self.stretch::<S, true, true, false>(&stretch, phase),
                (true, true, true) => self.stretch::<S, true, true, true>(&stretch, phase),
            };
            if fade {
                for faded in &mut self.lanes.faded[c..c + S::LANES] {
                    *faded = (*faded + len as f32).min(super::FADED);
                }
            }
            start += len;
        }
        phase.store(&mut self.lanes.phase[c..]);
    }

    /// Moves each lane's level on to a stretch whose increments reach
    /// `own`, and `peak` with the next stretch: the levels it fades from
    /// and plays, and if any lane is fading, how far each is into its fade.
    #[inline(always)]
    fn choose_levels<S: F32s>(
        &mut self,
        c: usize,
        own: S::U32s,
        peak: S::U32s,
        looked: std::ops::Range<usize>,
    ) -> (LaneLevels<S::U32s>, LaneLevels<S::U32s>, Option<S>) {
        let mut peaks = [0u32; 16];
        let mut owns = [0u32; 16];
        peak.store(&mut peaks);
        own.store(&mut owns);
        let mut old = [[0u32; 16]; 3];
        let mut new = [[0u32; 16]; 3];
        let mut fade = false;
        let levels = self.levels;
        for (i, (&peak, &own)) in peaks.iter().zip(&owns).enumerate().take(S::LANES) {
            // A stage that reads faster than the note picks the level for
            // the speed it reads at.
            let stretch = self.phase_stage.stretch(c + i, looked.clone());
            let lane = self.lanes.level_state(c + i);
            levels.advance(lane, stretched(own, stretch), stretched(peak, stretch));
            let (from, level) = (self.lanes.from[c + i], self.lanes.level[c + i]);
            fade |= from != level;
            for (arrays, level) in [(&mut old, from as usize), (&mut new, level as usize)] {
                arrays[0][i] = levels.offset[level];
                arrays[1][i] = levels.stride[level];
                arrays[2][i] = levels.size[level];
            }
        }
        let lanes = |a: &[[u32; 16]; 3]| LaneLevels {
            offset: <S::U32s>::load(&a[0]),
            stride: <S::U32s>::load(&a[1]),
            size: <S::U32s>::load(&a[2]),
        };
        let faded = fade.then(|| S::load(&self.lanes.faded[c..]));
        (lanes(&old), lanes(&new), faded)
    }

    /// The frames lanes at frame position `position` read.
    #[inline(always)]
    fn frames<S: F32s, const SMOOTH: bool>(&self, position: S) -> Frames<S> {
        let last = S::splat(self.plan.last_frame);
        let position = position.max(S::splat(0.0)).min(last);
        if SMOOTH {
            let floor = position.floor();
            let first = floor.to_i32();
            let second = (floor + S::splat(1.0)).min(last).to_i32();
            Frames {
                first,
                second,
                toward: position - floor,
            }
        } else {
            // The nearest whole frame.
            let first = (position + S::splat(0.5)).to_i32();
            Frames {
                first,
                second: first,
                toward: S::splat(0.0),
            }
        }
    }

    /// One stretch of frames, its levels fixed: crossfading from the old
    /// level with `FADE`, interpolating frames with `SMOOTH`, and moving
    /// the frame per sample with `FRAME_MOD`. Returns the phase after it.
    #[inline(always)]
    fn stretch<S: F32s, const FADE: bool, const SMOOTH: bool, const FRAME_MOD: bool>(
        &mut self,
        s: &Stretch<S>,
        mut phase: S::U32s,
    ) -> S::U32s {
        let data = self.table.data();
        let stride = self.lane_count;
        let gain = S::splat(self.plan.gain);
        let fixed_frames = self.frames::<S, SMOOTH>(s.frame_base);
        let per_frame = S::splat(1.0 / super::FADED);
        for k in s.start..s.start + s.len {
            let increment = if s.varying {
                <S::U32s>::load(&self.lanes.increments[k * S::LANES..])
            } else {
                s.fixed
            };
            let reads = self.phase_stage.reads::<S>(s.c, k, phase, increment);
            let frames = if FRAME_MOD {
                let modulation = self
                    .inputs
                    .frame_mod
                    .map_or(S::splat(0.0), |m| S::load(&m[k * stride + s.c..]));
                self.frames::<S, SMOOTH>(s.frame_base + modulation)
            } else {
                fixed_frames
            };
            let toward = if FADE {
                let played = s.faded + S::splat((k - s.start + 1) as f32);
                fade::<S>((played * per_frame).min(S::splat(1.0)))
            } else {
                S::splat(1.0)
            };
            let sample = |at: S::U32s| {
                let value = read::<S, SMOOTH>(data, &s.new, &frames, at);
                if FADE {
                    let before = read::<S, SMOOTH>(data, &s.old, &frames, at);
                    (value - before).mul_add(toward, before)
                } else {
                    value
                }
            };
            let mut value = sample(reads.phase[0]);
            if reads.count > 0 {
                value *= reads.gain[0];
                for i in 1..reads.count.min(super::MAX_READS) {
                    value = sample(reads.phase[i]).mul_add(reads.gain[i], value);
                }
            }
            let value = self.wave_stage.shape_at::<S>(s.c, k, increment, value) * gain;
            value.store(&mut self.out[k * stride + s.c..]);
            phase = phase.wrapping_add(increment);
        }
        phase
    }
}

/// The crossfade's weight `x` of the way through it: 6x⁵ − 15x⁴ + 10x³,
/// whose first two derivatives are 0 at both ends, so the fade's own
/// spectrum falls away at 24 dB an octave.
#[inline(always)]
fn fade<S: F32s>(x: S) -> S {
    let c = S::splat;
    x * x * x * x.mul_add(x.mul_add(c(6.0), c(-15.0)), c(10.0))
}

/// `increment` times a stage's `stretch`, saturating.
#[inline(always)]
pub(super) fn stretched(increment: u32, stretch: f32) -> u32 {
    if stretch == 1.0 {
        increment
    } else {
        (increment as f32 * stretch.max(0.0)) as u32
    }
}

/// What a stretch renders with.
struct Stretch<S: F32s> {
    c: usize,
    start: usize,
    len: usize,
    varying: bool,
    fixed: S::U32s,
    frame_base: S,
    old: LaneLevels<S::U32s>,
    new: LaneLevels<S::U32s>,
    /// How many frames of its fade each lane had played before the stretch.
    faded: S,
}

/// The lanes' values at `phase` in `frames` of `level`.
#[inline(always)]
fn read<S: F32s, const SMOOTH: bool>(
    data: &[f32],
    level: &LaneLevels<S::U32s>,
    frames: &Frames<S>,
    phase: S::U32s,
) -> S {
    // The phase's place in a cycle of `size` values: the whole part
    // indexes the coefficients, the fraction (24 bits) interpolates.
    let index = phase.mul_high(level.size);
    let t = S::from_i32(phase.wrapping_mul(level.size).shr::<8>()) * S::splat(1.0 / 16_777_216.0);
    let row = |frame: S::U32s| {
        level
            .offset
            .wrapping_add(frame.wrapping_mul(level.stride))
            .wrapping_add(index)
    };
    let first = quintic(S::gather::<6>(data, row(frames.first)), t);
    if SMOOTH {
        let second = quintic(S::gather::<6>(data, row(frames.second)), t);
        (second - first).mul_add(frames.toward, first)
    } else {
        first
    }
}

/// The quintic B-spline through coefficients `p` (the second before to the
/// third after the read), `t` of the way from the third to the fourth: as
/// [`Wave::read`](crate::Wave::read), in lanes.
#[inline(always)]
fn quintic<S: F32s>(p: [S; 6], t: S) -> S {
    let [m2, m1, p0, p1, p2, p3] = p;
    let (outer, inner) = (m2 + p2, m1 + p1);
    let (outer_slope, inner_slope) = (p2 - m2, p1 - m1);
    let c = S::splat;
    let d0 = p0.mul_add(c(66.0), inner.mul_add(c(26.0), outer));
    let d1 = inner_slope.mul_add(c(50.0), outer_slope * c(5.0));
    let d2 = p0.mul_add(c(-60.0), outer.mul_add(c(10.0), inner * c(20.0)));
    let d3 = inner_slope.mul_add(c(-20.0), outer_slope * c(10.0));
    let d4 = p0.mul_add(c(30.0), inner.mul_add(c(-20.0), outer * c(5.0)));
    let d5 = (p1 - p0).mul_add(c(10.0), (m1 - p2).mul_add(c(5.0), p3 - m2));
    let y = d5.mul_add(t, d4);
    let y = y.mul_add(t, d3);
    let y = y.mul_add(t, d2);
    let y = y.mul_add(t, d1);
    y.mul_add(t, d0)
}
