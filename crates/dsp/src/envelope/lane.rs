//! One envelope's stages, shared by [`Envelope`](super::Envelope) and
//! [`EnvelopeBank`](super::EnvelopeBank). Planning runs here, in scalar
//! code, only at a note, a stage change or a parameter change; the samples
//! in between are drawn from a [`Segment`].

use super::curve::{self, MAX_SEGMENT};
use super::{EnvelopeParams, Stage, StealRestart, Timing, Trigger, unit};

/// [`Segment::left`] for a level that holds until something happens (sustain
/// and idle).
pub(super) const UNTIMED: u32 = u32::MAX;

/// What draws the next samples: `y = u × scale + base`, then
/// `u = u × r + d` (see [`curve`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Segment {
    pub u: f32,
    pub r: f32,
    pub d: f32,
    pub scale: f32,
    pub base: f32,
    /// Samples until the segment ends, or [`UNTIMED`].
    pub left: u32,
}

impl Segment {
    /// `level`, for `left` samples.
    pub const fn constant(level: f32, left: u32) -> Self {
        Self {
            u: 0.0,
            r: 1.0,
            d: 0.0,
            scale: 0.0,
            base: level,
            left,
        }
    }

    /// The level it outputs next.
    #[inline(always)]
    pub fn level(&self) -> f32 {
        self.u * self.scale + self.base
    }

    /// Whether it ends at some point, rather than holding until an event.
    #[inline(always)]
    pub fn is_timed(&self) -> bool {
        self.left != UNTIMED
    }
}

/// A stage's length and target, as the parameters set them. A change in
/// them is what makes a parameter change re-plan the stage.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Spec {
    total: u64,
    to: f32,
    k: f64,
}

/// One envelope's plan.
#[derive(Clone, Debug)]
pub(super) struct Lane {
    pub params: EnvelopeParams,
    pub stage: Stage,
    /// The current stage as the parameters set it.
    spec: Spec,
    /// The curve the current stage follows: from `from` to `to` over
    /// `curve_total` samples with bend `k`. A re-planned stage's curve is
    /// the rest of the stage, so `spec.total − curve_total` samples of the
    /// stage had passed when it started.
    from: f32,
    to: f32,
    k: f64,
    curve_total: u64,
    /// Samples into the curve at the start of the current segment.
    pos: u64,
    /// The current segment's length.
    seg_len: u32,
}

impl Lane {
    /// An idle envelope with `params`, and the segment it outputs.
    pub fn new(params: EnvelopeParams) -> (Self, Segment) {
        let end = unit(params.end);
        let lane = Self {
            params,
            stage: Stage::Idle,
            spec: Spec {
                total: 0,
                to: end,
                k: 0.0,
            },
            from: end,
            to: end,
            k: 0.0,
            curve_total: 0,
            pos: 0,
            seg_len: 0,
        };
        (lane, Segment::constant(end, UNTIMED))
    }

    /// `stage` as the parameters set it; `held` is the level a delay
    /// holds.
    fn spec(&self, stage: Stage, timing: &Timing, held: f32) -> Spec {
        let p = &self.params;
        let samples = |value| timing.samples(value, p.unit);
        let (total, to, k) = match stage {
            Stage::Idle => (0, unit(p.end), 0.0),
            Stage::Delay => (samples(p.delay), held, 0.0),
            Stage::Attack => (samples(p.attack), 1.0, curve::bend(p.attack_curve)),
            Stage::Hold => (samples(p.hold), 1.0, 0.0),
            Stage::Decay => (
                samples(p.decay),
                unit(p.sustain),
                curve::bend(p.decay_curve),
            ),
            Stage::Sustain => (0, unit(p.sustain), 0.0),
            Stage::Release => (
                samples(p.release),
                unit(p.end),
                curve::bend(p.release_curve),
            ),
        };
        Spec { total, to, k }
    }

    /// Starts `stage` at `level`, skipping stages that take no time.
    fn enter(&mut self, seg: &mut Segment, timing: &Timing, mut stage: Stage, mut level: f32) {
        loop {
            let spec = self.spec(stage, timing, level);
            self.stage = stage;
            self.spec = spec;
            if matches!(stage, Stage::Idle | Stage::Sustain) {
                (self.from, self.to) = (spec.to, spec.to);
                *seg = Segment::constant(spec.to, UNTIMED);
                return;
            }
            // An attack joins its curve where the curve meets the current
            // level, so a retriggered attack takes only the rest of its time.
            let (from, pos) = if stage == Stage::Attack {
                self.join_attack(level, spec)
            } else {
                (level, 0)
            };
            if pos >= spec.total {
                level = spec.to;
                stage = stage.next();
                continue;
            }
            self.from = from;
            self.to = spec.to;
            self.k = spec.k;
            self.curve_total = spec.total;
            self.pos = pos;
            self.start_segment(seg);
            return;
        }
    }

    /// Where an attack starting at `level` begins: the curve's start level
    /// and how far along it to start.
    fn join_attack(&self, level: f32, spec: Spec) -> (f32, u64) {
        let start = unit(self.params.start);
        if start >= 1.0 || level < start {
            // Below the curve, or a curve that doesn't rise: the whole
            // attack, from here.
            return (level, 0);
        }
        if level >= 1.0 {
            return (level, spec.total);
        }
        let x = curve::time_at(spec.k, f64::from((level - start) / (1.0 - start)));
        // At most `total`, which is a whole number of samples.
        (start, (x * spec.total as f64).round() as u64)
    }

    /// Starts the current curve's next segment, at `pos`.
    fn start_segment(&mut self, seg: &mut Segment) {
        // At most MAX_SEGMENT, so it fits.
        let len = (self.curve_total - self.pos).min(u64::from(MAX_SEGMENT)) as u32;
        self.seg_len = len;
        *seg = curve::segment(self.from, self.to, self.k, self.curve_total, self.pos, len);
    }

    /// Moves on when the current segment has ended (`seg.left == 0`).
    pub fn advance(&mut self, seg: &mut Segment, timing: &Timing) {
        self.pos += u64::from(self.seg_len);
        if self.pos < self.curve_total {
            self.start_segment(seg);
        } else {
            let to = self.to;
            self.enter(seg, timing, self.stage.next(), to);
        }
    }

    /// A note starts (see [`Trigger`]).
    pub fn trigger(&mut self, seg: &mut Segment, timing: &Timing, trigger: Trigger) {
        let start = unit(self.params.start);
        let sounding = self.stage != Stage::Idle;
        let level = match trigger {
            Trigger::Legato if sounding && !self.params.legato_inverted => return,
            Trigger::New => start,
            _ if !sounding => start,
            _ => match self.params.steal_restart {
                StealRestart::CurrentLevel => seg.level(),
                StealRestart::StartLevel => start,
            },
        };
        self.enter(seg, timing, Stage::Delay, level);
    }

    /// The note ends: release from the current level.
    pub fn release(&mut self, seg: &mut Segment, timing: &Timing) {
        if !matches!(self.stage, Stage::Idle | Stage::Release) {
            let level = seg.level();
            self.enter(seg, timing, Stage::Release, level);
        }
    }

    /// Goes idle at once, at the end level.
    pub fn reset(&mut self, seg: &mut Segment, timing: &Timing) {
        self.enter(seg, timing, Stage::Idle, 0.0);
    }

    /// Re-plans the current stage after its parameters or the timing
    /// changed. A stage whose length and target are unchanged carries on
    /// untouched; otherwise it continues from the current level to the new
    /// target over what remains of its new length, and ends at once if that
    /// has already passed. A sustain or idle level moves to its new value
    /// at once.
    pub fn replan(&mut self, seg: &mut Segment, timing: &Timing) {
        let spec = self.spec(self.stage, timing, self.to);
        if spec == self.spec {
            return;
        }
        if matches!(self.stage, Stage::Idle | Stage::Sustain) {
            self.spec = spec;
            (self.from, self.to) = (spec.to, spec.to);
            *seg = Segment::constant(spec.to, UNTIMED);
            return;
        }
        let elapsed =
            (self.spec.total - self.curve_total) + self.pos + u64::from(self.seg_len - seg.left);
        let level = seg.level();
        self.spec = spec;
        if elapsed >= spec.total {
            self.enter(seg, timing, self.stage.next(), spec.to);
            return;
        }
        self.from = level;
        self.to = spec.to;
        self.k = spec.k;
        self.curve_total = spec.total - elapsed;
        self.pos = 0;
        self.start_segment(seg);
    }
}
