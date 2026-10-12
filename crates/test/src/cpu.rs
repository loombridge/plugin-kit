//! Wall-clock timing of blocks: how long each took, against how long the
//! audio it made lasts. The render harness times every block's `process`
//! call ([`Render::timings`](crate::Render::timings)); the performance
//! harness builds its benchmarks on these helpers.
//!
//! Timings vary with the machine and what else it runs, so a test never
//! asserts on them; benchmarks compare them on the same machine.

use std::time::{Duration, Instant};

/// How long blocks took to process.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timings {
    /// The sample rate the blocks ran at.
    pub sample_rate: f64,
    /// Each block's length in frames and how long it took.
    pub blocks: Vec<(usize, Duration)>,
}

impl Timings {
    /// No blocks yet, at `sample_rate`.
    #[must_use]
    pub fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate,
            blocks: Vec::new(),
        }
    }

    /// Records a block of `frames` frames that took `duration`.
    pub fn push(&mut self, frames: usize, duration: Duration) {
        self.blocks.push((frames, duration));
    }

    /// How many blocks there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Every block's time added up.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.blocks.iter().map(|(_, d)| *d).sum()
    }

    /// How many frames the blocks made.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.blocks.iter().map(|(f, _)| *f).sum()
    }

    /// The mean time per block.
    #[must_use]
    pub fn mean(&self) -> Duration {
        match u32::try_from(self.len()) {
            Ok(0) | Err(_) => Duration::ZERO,
            Ok(n) => self.total() / n,
        }
    }

    /// The time `fraction` of the way up the sorted block times: 0.5 is the
    /// median, 0.99 the 99th percentile, 1 the slowest.
    #[must_use]
    pub fn percentile(&self, fraction: f64) -> Duration {
        let mut times: Vec<Duration> = self.blocks.iter().map(|(_, d)| *d).collect();
        if times.is_empty() {
            return Duration::ZERO;
        }
        times.sort_unstable();
        let at = (fraction.clamp(0.0, 1.0) * (times.len() - 1) as f64).round() as usize;
        times[at]
    }

    /// The median block time.
    #[must_use]
    pub fn median(&self) -> Duration {
        self.percentile(0.5)
    }

    /// The slowest block's time.
    #[must_use]
    pub fn worst(&self) -> Duration {
        self.blocks
            .iter()
            .map(|(_, d)| *d)
            .max()
            .unwrap_or_default()
    }

    /// How long the audio lasts.
    #[must_use]
    pub fn audio(&self) -> Duration {
        if self.sample_rate > 0.0 {
            Duration::from_secs_f64(self.frames() as f64 / self.sample_rate)
        } else {
            Duration::ZERO
        }
    }

    /// Processing time over audio time: the share of one core the
    /// processor needs to keep up in real time (0.25 is a quarter).
    #[must_use]
    pub fn load(&self) -> f64 {
        let audio = self.audio().as_secs_f64();
        if audio > 0.0 {
            self.total().as_secs_f64() / audio
        } else {
            0.0
        }
    }

    /// The worst block's time over its own deadline (the time its audio
    /// lasts): above 1, that block would have dropped out.
    #[must_use]
    pub fn worst_deadline_fraction(&self) -> f64 {
        if self.sample_rate <= 0.0 {
            return 0.0;
        }
        self.blocks
            .iter()
            .filter(|(frames, _)| *frames > 0)
            .map(|(frames, d)| d.as_secs_f64() * self.sample_rate / *frames as f64)
            .fold(0.0, f64::max)
    }
}

/// Runs `f` and returns what it returned and how long it took.
pub fn time<R>(f: impl FnOnce() -> R) -> (R, Duration) {
    let start = Instant::now();
    let value = f();
    (value, start.elapsed())
}

/// Calls `block` `count` times, with each call's index, and times each
/// call as a block of `frames` frames at `sample_rate`.
pub fn time_blocks(
    count: usize,
    frames: usize,
    sample_rate: f64,
    mut block: impl FnMut(usize),
) -> Timings {
    let mut timings = Timings::new(sample_rate);
    timings.blocks.reserve(count);
    for index in 0..count {
        let ((), duration) = time(|| block(index));
        timings.push(frames, duration);
    }
    timings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn statistics_follow_the_blocks() {
        let mut timings = Timings::new(1_000.0);
        for d in [3, 1, 2, 10] {
            timings.push(100, ms(d));
        }
        assert_eq!(timings.len(), 4);
        assert_eq!(timings.total(), ms(16));
        assert_eq!(timings.mean(), ms(4));
        assert_eq!(timings.percentile(0.0), ms(1));
        assert_eq!(timings.median(), ms(3));
        assert_eq!(timings.worst(), ms(10));
        assert_eq!(timings.audio(), ms(400));
        assert!((timings.load() - 0.04).abs() < 1e-12);
        // The worst block, 10 ms, against its 100 ms of audio.
        assert!((timings.worst_deadline_fraction() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn no_blocks_is_all_zeros() {
        let timings = Timings::new(48_000.0);
        assert!(timings.is_empty());
        assert_eq!(timings.mean(), Duration::ZERO);
        assert_eq!(timings.median(), Duration::ZERO);
        assert_eq!(timings.load(), 0.0);
        assert_eq!(timings.worst_deadline_fraction(), 0.0);
    }

    #[test]
    fn time_blocks_calls_each_index_once() {
        let mut seen = Vec::new();
        let timings = time_blocks(3, 64, 48_000.0, |i| seen.push(i));
        assert_eq!(seen, [0, 1, 2]);
        assert_eq!(timings.frames(), 192);
        let (value, _) = time(|| 7);
        assert_eq!(value, 7);
    }
}
