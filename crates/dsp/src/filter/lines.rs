//! Delay lines for the types built on them (combs, flangers, the
//! flanger-phaser, the multi-comb types and the room): one ring per slot,
//! split into equal sub-lines for the types that need several, read at a
//! fractional delay with four-point cubic Hermite interpolation.
//!
//! Every slot writes one sample per frame, so one write cursor serves the
//! whole bank. Reads and writes go lane by lane, as each lane's delay
//! differs; the arithmetic around them stays in lanes.

use super::lane::Lane;
use super::{Io, MAX_LANES, ratio_of};

/// The longest delay a bank's rings hold, in seconds at the rate it's made
/// for: a comb down to 8 Hz.
pub(super) const MAX_DELAY_SECONDS: f32 = 0.125;

/// The shortest delay read, in samples: the interpolator reads one sample
/// newer than the delay, which must already have been written.
const MIN_DELAY: f32 = 2.0;

/// Each slot's ring length at `rate`: a power of two, so positions wrap by
/// masking.
pub(super) fn ring_len(rate: f32) -> usize {
    ((rate * MAX_DELAY_SECONDS) as usize + 8).next_power_of_two()
}

/// One group of lanes' view of the bank's rings during a block.
pub(super) struct Lines<'a> {
    /// Every slot's ring, `len` values each.
    pub(super) buf: &'a mut [f32],
    /// Each slot's ring length.
    pub(super) len: usize,
    /// Where this frame's sample is written; delays count back from it.
    pub(super) cursor: usize,
    /// The group's first slot.
    pub(super) slot: usize,
}

impl Lines<'_> {
    /// Moves on to the next frame.
    #[inline(always)]
    pub(super) fn advance(&mut self) {
        self.cursor = self.cursor.wrapping_add(1);
    }

    /// Sub-line `line` of `count`'s value `delay(ratio, character)` samples
    /// back, per lane, where `ratio` is the lane's cutoff over the sample
    /// rate; the delay is held between 2 samples and the sub-line's length.
    /// The delay is worked out lane by lane from the pitch, exactly as the
    /// scalar path works it out, as a long delay turns the smallest
    /// rounding difference into an audible one.
    #[inline(always)]
    pub(super) fn read<L: Lane>(
        &self,
        line: usize,
        count: usize,
        io: &Io<L>,
        delay: impl Fn(f32, f32) -> f32,
    ) -> L {
        let sub = self.len / count;
        let mask = sub - 1;
        let longest = self.longest(count);
        let mut pitch = [0.0f32; MAX_LANES];
        let mut character = [0.0f32; MAX_LANES];
        io.pitch.store(&mut pitch);
        io.character.store(&mut character);
        let mut out = [0.0f32; MAX_LANES];
        for (lane, o) in out.iter_mut().take(L::LANES).enumerate() {
            let base = (self.slot + lane) * self.len + line * sub;
            let ring = &self.buf[base..base + sub];
            let ratio = ratio_of(pitch[lane], io.ref_over_rate);
            let d = delay(ratio, character[lane]).clamp(MIN_DELAY, longest);
            let whole = d as usize;
            let t = d - whole as f32;
            let at = |back: usize| ring[self.cursor.wrapping_sub(back) & mask];
            *o = hermite(at(whole - 1), at(whole), at(whole + 1), at(whole + 2), t);
        }
        L::load(&out)
    }

    /// The longest delay sub-lines of `count` read, in samples.
    #[inline(always)]
    pub(super) fn longest(&self, count: usize) -> f32 {
        (self.len / count - 3) as f32
    }

    /// Writes this frame's `value` to sub-line `line` of `count`, per lane.
    #[inline(always)]
    pub(super) fn write<L: Lane>(&mut self, line: usize, count: usize, value: L) {
        let sub = self.len / count;
        let mask = sub - 1;
        let mut v = [0.0f32; MAX_LANES];
        value.store(&mut v);
        for (lane, &v) in v.iter().take(L::LANES).enumerate() {
            let base = (self.slot + lane) * self.len + line * sub;
            self.buf[base + (self.cursor & mask)] = v;
        }
    }
}

/// Four-point cubic Hermite (Catmull–Rom) interpolation between `p0` and
/// the older `p1`, `t` of the way, with `newer` and `older` the samples
/// either side.
#[inline(always)]
fn hermite(newer: f32, p0: f32, p1: f32, older: f32, t: f32) -> f32 {
    let c1 = 0.5 * (p1 - newer);
    let c2 = newer - 2.5 * p0 + 2.0 * p1 - 0.5 * older;
    let c3 = 0.5 * (older - newer) + 1.5 * (p0 - p1);
    ((c3 * t + c2) * t + c1) * t + p0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermite_is_exact_at_whole_samples_and_for_cubics() {
        assert_eq!(hermite(1.0, 2.0, 3.0, 4.0, 0.0), 2.0);
        // Catmull–Rom reproduces straight lines and parabolas exactly.
        let f = |x: f32| 0.5 * x * x - x + 2.0;
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let y = hermite(f(-1.0), f(0.0), f(1.0), f(2.0), t);
            assert!((y - f(t)).abs() < 1e-5, "{t}: {y}");
        }
    }

    #[test]
    fn a_line_reads_back_what_was_written_that_many_frames_ago() {
        let len = 64;
        let io = Io {
            x: 0.0f32,
            g: 0.0,
            ratio: 0.0,
            pitch: 69.0,
            r: 0.0,
            inv_knee: 0.0,
            character: 0.0,
            drive: 0.0,
            ref_over_rate: 0.01,
            inv_rate: 0.0,
        };
        let mut buf = vec![0.0f32; len * 2];
        let mut lines = Lines {
            buf: &mut buf,
            len,
            cursor: 5,
            slot: 1,
        };
        for frame in 0..40 {
            if frame >= 10 {
                // Two sub-lines of 32: whole delays read exactly.
                assert_eq!(lines.read(1, 2, &io, |_, _| 7.0), (frame - 7) as f32);
            }
            lines.write(1, 2, frame as f32);
            lines.advance();
        }
        // The first slot's ring was never touched.
        assert!(buf[..len].iter().all(|&x| x == 0.0));
        assert_eq!(ring_len(48_000.0), 8_192);
    }
}
