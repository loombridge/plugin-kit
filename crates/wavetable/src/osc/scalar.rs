//! The render one lane at a time, without SIMD: the reference the vector
//! paths are tested against.

use super::{Inputs, Lanes, Levels, MAX_EXPONENT, PhaseStage, Plan, SUB_BLOCK, WaveStage};
use crate::{Table, Wave};

/// 2^x as `lspk_dsp::simd::exp2` computes it on the portable path, for
/// 0 ≤ x < 31.
fn exp2(x: f32) -> f32 {
    const C: [f32; 6] = [
        0.999_999_94,
        0.693_153_1,
        0.240_153_55,
        0.055_826_403,
        0.008_989_328,
        0.001_877_552_8,
    ];
    let whole = x.floor();
    let f = x - whole;
    let p = C.iter().rev().skip(1).fold(C[5], |p, &c| p * f + c);
    f32::from_bits(p.to_bits().wrapping_add((whole as i32 as u32) << 23))
}

/// The increment for `pitch` semitones.
fn increment(plan: &Plan, pitch: f32) -> u32 {
    let exponent = pitch * (1.0 / 12.0) + plan.exp_offset;
    exp2(exponent.clamp(0.0, MAX_EXPONENT)) as i32 as u32
}

/// The frame a lane at frame position `position` reads, and with Smooth
/// frames the next and how far towards it.
#[derive(Clone, Copy)]
struct Pick {
    first: usize,
    second: usize,
    toward: f32,
}

fn pick(plan: &Plan, position: f32) -> Pick {
    let position = position.clamp(0.0, plan.last_frame);
    if plan.smooth {
        let floor = position.floor();
        Pick {
            first: floor as usize,
            second: (floor + 1.0).min(plan.last_frame) as usize,
            toward: position - floor,
        }
    } else {
        let nearest = (position + 0.5) as usize;
        Pick {
            first: nearest,
            second: nearest,
            toward: 0.0,
        }
    }
}

/// The frames `pick` names, at level `level`, ready to read.
#[derive(Clone, Copy)]
struct Waves<'a> {
    first: Wave<'a>,
    second: Wave<'a>,
    toward: f32,
}

impl<'a> Waves<'a> {
    fn new(table: &'a Table, level: usize, pick: Pick) -> Self {
        let first = table.wave(level, pick.first);
        Self {
            first,
            second: if pick.second == pick.first {
                first
            } else {
                table.wave(level, pick.second)
            },
            toward: pick.toward,
        }
    }

    fn read(&self, smooth: bool, phase: u32) -> f32 {
        let a = self.first.read(phase);
        if smooth {
            a + (self.second.read(phase) - a) * self.toward
        } else {
            a
        }
    }
}

/// Renders every lane of `inputs`, one at a time.
#[allow(
    clippy::too_many_arguments,
    reason = "the render's parts, kept apart for borrowing"
)]
pub(super) fn render<P: PhaseStage, W: WaveStage>(
    table: &Table,
    levels: &Levels,
    plan: Plan,
    lanes: &mut Lanes,
    stride: usize,
    frames: usize,
    inputs: &Inputs<'_>,
    out: &mut [f32],
    phase_stage: &mut P,
    wave_stage: &mut W,
) {
    if frames == 0 {
        return;
    }
    let n = frames;
    for l in 0..inputs.lanes {
        let varying = inputs.pitch_mod.is_some() || inputs.pitch_step.is_some_and(|s| s[l] != 0.0);
        let mut base = inputs.pitch[l] + lanes.pitch_offset[l] + plan.tune;
        if !plan.key_track {
            base -= lanes.key[l];
        }
        let fixed = increment(&plan, base);
        if varying {
            let step = inputs.pitch_step.map_or(0.0, |s| s[l]);
            let modulation = |k: usize| inputs.pitch_mod.map_or(0.0, |m| m[k * stride + l]);
            for k in 0..n {
                lanes.increments[k] = increment(&plan, k as f32 * step + base + modulation(k));
            }
            let pitch_at = |k: usize| k as f32 * step + base + modulation(k);
            let back = (n - 1).min(SUB_BLOCK);
            let last = pitch_at(n - 1);
            let rise = if back == 0 {
                step * SUB_BLOCK as f32
            } else {
                (last - pitch_at(n - 1 - back)) * (SUB_BLOCK as f32 / back as f32)
            };
            lanes.increments[n] = increment(&plan, last + rise.max(0.0));
        }
        let mut phase = lanes.phase[l];
        let frame_base = plan.frame + lanes.frame_offset[l];
        let mut start = 0;
        while start < n {
            let len = (n - start).min(SUB_BLOCK);
            let (own, peak) = if varying {
                let highest = |k: std::ops::Range<usize>| {
                    lanes.increments[k].iter().copied().max().unwrap_or(0)
                };
                let own = highest(start..start + len);
                let end = start + len + SUB_BLOCK;
                let mut peak = own.max(highest(start + len..end.min(n)));
                if end >= n {
                    peak = peak.max(lanes.increments[n]);
                }
                (own, peak)
            } else {
                (fixed, fixed)
            };
            let stretch = phase_stage.stretch(l, start..(start + len + SUB_BLOCK).min(n));
            levels.advance(
                lanes.level_state(l),
                super::kernel::stretched(own, stretch),
                super::kernel::stretched(peak, stretch),
            );
            let (before, next) = (lanes.from[l] as usize, lanes.level[l] as usize);
            let faded = lanes.faded[l];
            // Without frame modulation, the frames hold for the stretch.
            let fade = before != next;
            let held = inputs.frame_mod.is_none().then(|| pick(&plan, frame_base));
            let held_new = held.map(|at| Waves::new(table, next, at));
            let held_old = held.map(|at| Waves::new(table, before, at));
            for k in start..start + len {
                let increment = if varying { lanes.increments[k] } else { fixed };
                let reads = phase_stage.reads_lane(l, k, phase, increment);
                let at = held.unwrap_or_else(|| {
                    let modulation = inputs.frame_mod.map_or(0.0, |m| m[k * stride + l]);
                    pick(&plan, frame_base + modulation)
                });
                let new = held_new.unwrap_or_else(|| Waves::new(table, next, at));
                let old = fade.then(|| held_old.unwrap_or_else(|| Waves::new(table, before, at)));
                let x = ((faded + (k - start + 1) as f32) / super::FADED).min(1.0);
                let toward = x * x * x * (x * (x * 6.0 - 15.0) + 10.0);
                let sample = |read_at: u32| {
                    let value = new.read(plan.smooth, read_at);
                    match old {
                        Some(old) => {
                            let old = old.read(plan.smooth, read_at);
                            old + (value - old) * toward
                        }
                        None => value,
                    }
                };
                let mut value = sample(reads.phase[0]);
                if reads.count > 0 {
                    value *= reads.gain[0];
                    for i in 1..reads.count.min(super::MAX_READS) {
                        value += sample(reads.phase[i]) * reads.gain[i];
                    }
                }
                out[k * stride + l] = wave_stage.shape_at_lane(l, k, increment, value) * plan.gain;
                phase = phase.wrapping_add(increment);
            }
            if fade {
                lanes.faded[l] = (faded + len as f32).min(super::FADED);
            }
            start += len;
        }
        lanes.phase[l] = phase;
    }
}
