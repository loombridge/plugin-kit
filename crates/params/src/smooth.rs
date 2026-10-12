//! Parameter smoothing: a value that moves to each new target along a
//! straight line over a fixed time, a frame at a time, so a parameter
//! change never steps.
//!
//! Its arithmetic runs frame by frame in the same order whatever the block
//! size, so a render is the same to the bit at any block size; and it lands
//! on its target exactly.

/// How long a native device's continuous parameter takes to reach a new
/// value by default: 20 ms, as the mixer's gains do.
pub const DEFAULT_SMOOTHING_SECONDS: f64 = 0.02;

/// A value that glides to each new target over a fixed number of frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearSmoother {
    value: f64,
    target: f64,
    step: f64,
    /// Frames until it reaches `target`.
    left: u32,
    /// Frames a move takes.
    frames: u32,
}

impl LinearSmoother {
    /// A value resting at `value` that takes `frames` frames to reach each
    /// new target; 0 frames jumps at once.
    #[must_use]
    pub fn new(value: f64, frames: u32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            left: 0,
            frames,
        }
    }

    /// The frames `seconds` lasts at `sample_rate`, rounded.
    #[must_use]
    pub fn frames_for(seconds: f64, sample_rate: f64) -> u32 {
        (seconds * sample_rate)
            .round()
            .clamp(0.0, f64::from(u32::MAX)) as u32
    }

    /// Starts moving to `target` from where the value is now. A value
    /// already heading for `target` carries on.
    pub fn set_target(&mut self, target: f64) {
        if target == self.target {
            return;
        }
        self.target = target;
        if self.frames == 0 || target == self.value {
            self.value = target;
            self.left = 0;
        } else {
            self.step = (target - self.value) / f64::from(self.frames);
            self.left = self.frames;
        }
    }

    /// Jumps to `value` at once.
    pub fn snap(&mut self, value: f64) {
        self.value = value;
        self.target = value;
        self.left = 0;
    }

    /// The value now.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Where it's heading.
    #[must_use]
    pub fn target(&self) -> f64 {
        self.target
    }

    /// Whether it has reached its target.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.left == 0
    }

    /// The value for the next frame.
    #[inline]
    pub fn step(&mut self) -> f64 {
        if self.left > 0 {
            self.left -= 1;
            self.value = if self.left == 0 {
                self.target
            } else {
                self.value + self.step
            };
        }
        self.value
    }

    /// Moves `frames` frames on at once, landing where as many calls of
    /// [`step`](Self::step) would.
    pub fn skip(&mut self, frames: usize) {
        let moving = (self.left as usize).min(frames);
        for _ in 0..moving {
            self.step();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: device-framework R4
    #[test]
    fn a_change_glides_in_a_straight_line_and_lands_exactly() {
        let mut s = LinearSmoother::new(0.0, 4);
        s.set_target(1.0);
        let path: Vec<f64> = (0..6).map(|_| s.step()).collect();
        assert_eq!(path, [0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
        assert!(s.is_settled());
        // A new target mid-glide starts from where it is.
        s.set_target(0.0);
        s.step();
        s.set_target(0.5);
        assert_eq!(s.step(), 0.75 - (0.75 - 0.5) / 4.0);
        let mut jump = LinearSmoother::new(3.0, 0);
        jump.set_target(-1.0);
        assert_eq!(jump.value(), -1.0);
        assert_eq!(LinearSmoother::frames_for(0.02, 48_000.0), 960);
    }

    // Spec: device-framework R4
    #[test]
    fn the_path_is_the_same_at_any_block_size() {
        let mut a = LinearSmoother::new(0.2, 960);
        let mut b = a;
        a.set_target(0.9);
        b.set_target(0.9);
        let whole: Vec<f64> = (0..1000).map(|_| a.step()).collect();
        let mut pieces = Vec::new();
        for size in [1usize, 7, 64, 500, 428] {
            pieces.extend((0..size).map(|_| b.step()));
        }
        assert_eq!(whole, pieces);
    }
}
