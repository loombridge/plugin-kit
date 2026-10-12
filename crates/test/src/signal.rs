//! Test audio made by code. Every signal is the same on every run: the
//! oscillators use `f64` maths rounded once to `f32`, and the noise comes
//! from a seeded generator, which is the same on every platform. (The
//! platform's `sin` and `exp` may round an `f64`'s last bit differently,
//! which almost never reaches the `f32`.)

use std::f64::consts::TAU;

/// `frames` frames of a sine at `frequency` hertz and peak `amplitude`,
/// starting at phase 0.
#[must_use]
pub fn sine(frequency: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f32> {
    let step = frequency / sample_rate;
    (0..frames)
        .map(|n| (amplitude * ((n as f64 * step).fract() * TAU).sin()) as f32)
        .collect()
}

/// `frames` frames of an exponential (logarithmic) sine sweep from `start`
/// to `end` hertz at peak `amplitude`: equal time per octave. Its frequency
/// at frame `n` is [`sweep_frequency`].
#[must_use]
pub fn sweep(start: f64, end: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f32> {
    let length = frames as f64 / sample_rate;
    let rate = (end / start).ln() / length;
    (0..frames)
        .map(|n| {
            let t = n as f64 / sample_rate;
            // The phase, in cycles, is the integral of the frequency.
            let cycles = if rate.abs() < 1e-12 {
                start * t
            } else {
                start * ((rate * t).exp() - 1.0) / rate
            };
            (amplitude * (cycles.fract() * TAU).sin()) as f32
        })
        .collect()
}

/// The frequency of a [`sweep`] from `start` to `end` hertz over `frames`
/// frames, at frame `n`.
#[must_use]
pub fn sweep_frequency(start: f64, end: f64, frames: usize, n: f64) -> f64 {
    if frames == 0 {
        return start;
    }
    start * (end / start).powf(n / frames as f64)
}

/// `frames` frames of white noise, uniform between −`amplitude` and
/// `amplitude`, from `seed`. The same seed gives the same noise everywhere.
#[must_use]
pub fn noise(seed: u64, frames: usize, amplitude: f64) -> Vec<f32> {
    let mut random = Random::new(seed);
    (0..frames)
        .map(|_| (amplitude * random.bipolar()) as f32)
        .collect()
}

/// `frames` frames of silence with one sample of `amplitude` at frame
/// `at`.
#[must_use]
pub fn impulse(frames: usize, at: usize, amplitude: f32) -> Vec<f32> {
    let mut signal = vec![0.0; frames];
    if let Some(sample) = signal.get_mut(at) {
        *sample = amplitude;
    }
    signal
}

/// `frames` frames of silence.
#[must_use]
pub fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames]
}

/// Every sample of `signal` scaled by `gain`.
#[must_use]
pub fn scaled(signal: &[f32], gain: f64) -> Vec<f32> {
    signal
        .iter()
        .map(|&s| (f64::from(s) * gain) as f32)
        .collect()
}

/// The sample-by-sample sum of `signals`, as long as the longest.
#[must_use]
pub fn mix(signals: &[&[f32]]) -> Vec<f32> {
    let frames = signals.iter().map(|s| s.len()).max().unwrap_or(0);
    let mut out = vec![0.0f32; frames];
    for signal in signals {
        for (out, &s) in out.iter_mut().zip(signal.iter()) {
            *out += s;
        }
    }
    out
}

/// A small, seeded random number generator (SplitMix64), for test
/// signals and randomised event scripts that must repeat exactly.
#[derive(Clone, Debug)]
pub struct Random {
    state: u64,
}

impl Random {
    /// A generator from `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number from 0 (included) to 1 (not).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number from −1 to 1.
    pub fn bipolar(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }

    /// A whole number from 0 to `below` − 1 (0 if `below` is 0).
    pub fn below(&mut self, below: u64) -> u64 {
        if below == 0 {
            0
        } else {
            self.next_u64() % below
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_starts_at_zero_and_peaks_at_its_amplitude() {
        let s = sine(1_000.0, 48_000.0, 48, 0.5);
        assert_eq!(s[0], 0.0);
        assert!((s[12] - 0.5).abs() < 1e-6, "a quarter cycle in: {}", s[12]);
    }

    #[test]
    fn a_sweep_moves_from_its_start_to_its_end() {
        assert_eq!(sweep_frequency(20.0, 20_000.0, 100, 0.0), 20.0);
        assert!((sweep_frequency(20.0, 20_000.0, 100, 100.0) - 20_000.0).abs() < 1e-9);
        assert!((sweep_frequency(100.0, 400.0, 100, 50.0) - 200.0).abs() < 1e-9);
        let s = sweep(100.0, 100.0, 48_000.0, 480, 1.0);
        let t = sine(100.0, 48_000.0, 480, 1.0);
        for (a, b) in s.iter().zip(&t) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn noise_repeats_from_its_seed_and_stays_in_range() {
        assert_eq!(noise(7, 64, 0.5), noise(7, 64, 0.5));
        assert_ne!(noise(7, 64, 0.5), noise(8, 64, 0.5));
        assert!(noise(1, 10_000, 0.25).iter().all(|s| s.abs() <= 0.25));
    }

    #[test]
    fn helpers_place_scale_and_mix() {
        assert_eq!(impulse(4, 2, 1.0), [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(impulse(2, 5, 1.0), [0.0, 0.0]);
        assert_eq!(scaled(&[1.0, -0.5], 0.5), [0.5, -0.25]);
        assert_eq!(mix(&[&[1.0, 1.0], &[0.5]]), [1.5, 1.0]);
        assert_eq!(silence(3), [0.0; 3]);
        let mut random = Random::new(3);
        assert!((0..100).all(|_| random.below(5) < 5));
        assert_eq!(random.below(0), 0);
    }
}
