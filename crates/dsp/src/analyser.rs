//! A scope and a spectrum of the last samples played, for an interface to
//! draw. Allocation-free after [`Analyser::new`], and made to run on an
//! audio thread only while an editor watches.

use std::f32::consts::TAU;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// The last `size` samples of a signal, as a scope and as a spectrum.
///
/// [`push`](Self::push) every block while someone watches, then read
/// [`scope`](Self::scope) and [`spectrum`](Self::spectrum) when publishing.
pub struct Analyser {
    ring: Vec<f32>,
    /// Where the next sample goes, which is also the oldest sample.
    next: usize,
    /// A Hann window, which keeps a tone's energy in its own bins.
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    bins: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Analyser {
    /// An analyser of the last `size` samples; `size` is a power of two, at
    /// least 4. Allocates; make it off the audio thread.
    ///
    /// # Panics
    ///
    /// If `size` isn't a power of two of at least 4.
    #[must_use]
    pub fn new(size: usize) -> Self {
        assert!(
            size >= 4 && size.is_power_of_two(),
            "the size is a power of two"
        );
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        Self {
            ring: vec![0.0; size],
            next: 0,
            window: (0..size)
                .map(|i| 0.5 - 0.5 * (TAU * i as f32 / size as f32).cos())
                .collect(),
            input: fft.make_input_vec(),
            bins: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
        }
    }

    /// The number of samples held, which is also the scope's length.
    #[must_use]
    pub fn size(&self) -> usize {
        self.ring.len()
    }

    /// The number of bins [`spectrum`](Self::spectrum) fills: DC to Nyquist.
    #[must_use]
    pub fn bins(&self) -> usize {
        self.ring.len() / 2 + 1
    }

    /// Takes `samples`, the newest last.
    pub fn push(&mut self, samples: &[f32]) {
        // Only the last `size` samples can matter.
        let samples = &samples[samples.len().saturating_sub(self.ring.len())..];
        let room = self.ring.len() - self.next;
        let (head, tail) = samples.split_at(samples.len().min(room));
        self.ring[self.next..self.next + head.len()].copy_from_slice(head);
        self.ring[..tail.len()].copy_from_slice(tail);
        self.next = (self.next + samples.len()) % self.ring.len();
    }

    /// Fills `out` (at most [`size`](Self::size) long) with the last
    /// `out.len()` samples, the oldest first.
    ///
    /// # Panics
    ///
    /// If `out` is longer than [`size`](Self::size).
    pub fn scope(&self, out: &mut [f32]) {
        let n = self.ring.len();
        assert!(out.len() <= n, "the scope is at most the size");
        let from = (self.next + n - out.len()) % n;
        let head = (n - from).min(out.len());
        let (first, rest) = out.split_at_mut(head);
        first.copy_from_slice(&self.ring[from..from + head]);
        rest.copy_from_slice(&self.ring[..rest.len()]);
    }

    /// Fills `out` ([`bins`](Self::bins) long) with each bin's amplitude,
    /// DC to Nyquist, where a full-scale sine at a bin's centre reads 1.
    ///
    /// # Panics
    ///
    /// If `out` isn't [`bins`](Self::bins) long.
    pub fn spectrum(&mut self, out: &mut [f32]) {
        assert_eq!(out.len(), self.bins(), "one value per bin");
        let n = self.ring.len();
        let (old, new) = self.ring.split_at(self.next);
        for (i, (&s, &w)) in new.iter().chain(old).zip(&self.window).enumerate() {
            self.input[i] = s * w;
        }
        // The buffers are the plan's sizes, so it can't fail.
        let _ = self
            .fft
            .process_with_scratch(&mut self.input, &mut self.bins, &mut self.scratch);
        // The window's mean is a half, and a real tone's energy sits in
        // two bins, so a sine of amplitude 1 sums to n / 4 in its bin.
        let scale = 4.0 / n as f32;
        for (o, b) in out.iter_mut().zip(&self.bins) {
            *o = b.norm() * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(cycles: usize, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (TAU * cycles as f32 * i as f32 / n as f32).sin())
            .collect()
    }

    #[test]
    fn the_scope_holds_the_last_samples_oldest_first() {
        let mut a = Analyser::new(8);
        a.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        a.push(&[6.0, 7.0, 8.0, 9.0, 10.0]);
        let mut out = [0.0; 6];
        a.scope(&mut out);
        assert_eq!(out, [5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        a.push(&(0..20).map(|i| i as f32).collect::<Vec<_>>());
        a.scope(&mut out);
        assert_eq!(out, [14.0, 15.0, 16.0, 17.0, 18.0, 19.0]);
    }

    #[test]
    fn a_sine_peaks_in_its_own_bin_at_its_own_amplitude() {
        let mut a = Analyser::new(1024);
        a.push(&sine(40, 1024).iter().map(|s| s * 0.5).collect::<Vec<_>>());
        let mut out = vec![0.0; a.bins()];
        a.spectrum(&mut out);
        let (peak, level) = out
            .iter()
            .copied()
            .enumerate()
            .fold((0, 0.0), |m, (i, v)| if v > m.1 { (i, v) } else { m });
        assert_eq!(peak, 40);
        assert!((level - 0.5).abs() < 0.01, "read {level}");
    }

    #[test]
    fn the_spectrum_follows_the_ring_after_it_wraps() {
        let mut a = Analyser::new(256);
        let tone = sine(10, 256);
        a.push(&tone[..100]);
        a.push(&tone[100..]);
        a.push(&tone[..37]);
        a.push(&tone[37..]);
        let mut out = vec![0.0; a.bins()];
        a.spectrum(&mut out);
        assert!((out[10] - 1.0).abs() < 0.01, "read {}", out[10]);
    }
}
