//! Values that move over time, as exact functions of the frame clock, so a
//! value at a frame doesn't depend on how the frames were split into
//! blocks.

/// A value moving from `from` to `to` over `len` frames from `start`,
/// shaped by `curve` (0 is linear; see [`shape`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ramp {
    from: f32,
    to: f32,
    start: u64,
    len: u32,
    curve: f32,
}

impl Ramp {
    /// Settled at `value`.
    pub(crate) const fn at(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: 0,
            len: 0,
            curve: 0.0,
        }
    }

    /// From `from` to `to` over `len` frames starting at frame `start`.
    pub(crate) const fn new(from: f32, to: f32, start: u64, len: u32, curve: f32) -> Self {
        Self {
            from,
            to,
            start,
            len,
            curve,
        }
    }

    /// Its value at frame `t`.
    #[inline]
    pub(crate) fn value(&self, t: u64) -> f32 {
        if t >= self.start + u64::from(self.len) {
            self.to
        } else if t <= self.start {
            self.from
        } else {
            let x = (t - self.start) as f32 / self.len as f32;
            self.from + (self.to - self.from) * shape(x, self.curve)
        }
    }

    /// Where it ends.
    pub(crate) const fn target(&self) -> f32 {
        self.to
    }

    /// Moves linearly from its value at frame `t` to `to` over `len`
    /// frames.
    pub(crate) fn retarget(&mut self, to: f32, t: u64, len: u32) {
        *self = Self::new(self.value(t), to, t, len, 0.0);
    }
}

/// How far along a glide with `curve` is at `x`, from 0 to 1: `x` itself at
/// curve 0; above 0, `1 − (1 − x)^k`, which leaves quickly and slows near
/// the target; below 0, `x^k`, which leaves slowly and arrives quickly; with
/// `k = 2^(6 |curve|)`, so at ±1 (k = 64) a glide is almost a step: at +1 it
/// is 80 % there after 2.5 % of its time, at −1 it is 2.4 % there after
/// 94 % (engine-core R123).
///
/// Serum 2's curve family is still to be measured (R123's M); this one
/// matches what was observed of its ends and middle.
#[inline]
pub(crate) fn shape(x: f32, curve: f32) -> f32 {
    if curve == 0.0 {
        x
    } else {
        let k = (6.0 * curve.abs()).exp2();
        if curve > 0.0 {
            1.0 - (1.0 - x).powf(k)
        } else {
            x.powf(k)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ramp_is_a_function_of_the_frame() {
        let ramp = Ramp::new(0.0, 12.0, 100, 100, 0.0);
        assert_eq!(ramp.value(0), 0.0);
        assert_eq!(ramp.value(150), 6.0);
        assert_eq!(ramp.value(200), 12.0);
        assert_eq!(ramp.value(10_000), 12.0);
        let mut moved = ramp;
        moved.retarget(0.0, 150, 0);
        assert_eq!(moved.value(150), 0.0);
        assert_eq!(Ramp::at(3.0).value(7), 3.0);
    }

    // Spec: engine-core R123
    #[test]
    fn the_curve_family_has_the_observed_ends() {
        assert_eq!(shape(0.5, 0.0), 0.5);
        assert!(shape(0.025, 1.0) >= 0.79);
        assert!(shape(0.94, -1.0) < 0.025);
        for curve in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            assert_eq!(shape(0.0, curve), 0.0);
            assert_eq!(shape(1.0, curve), 1.0);
        }
        assert!(shape(0.5, 0.3) > 0.5 && shape(0.5, -0.3) < 0.5);
    }
}
