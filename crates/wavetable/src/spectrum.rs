//! Spectra of single cycles: resampling a cycle to a frame, and the morphs
//! that make the frames between drawn frames. Off the audio thread only.

use std::f32::consts::PI;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::{FRAME_SIZE, Frame, Interpolation};

/// Bins in a frame's spectrum: DC to Nyquist.
pub(crate) const BINS: usize = FRAME_SIZE / 2 + 1;

/// The precision of the FFTs: single, as the tables store, whose rounding
/// sits far below the aliasing floor.
pub(crate) type Real = f32;

/// A frame's spectrum, unnormalised: bin `k` of a forward FFT of
/// [`FRAME_SIZE`] samples.
pub(crate) type Spectrum = Vec<Complex<Real>>;

/// Forward and inverse real FFTs with their scratch space, planned once per
/// size.
pub(crate) struct Ffts {
    planner: RealFftPlanner<Real>,
    scratch: Vec<Complex<Real>>,
    bins: Vec<Complex<Real>>,
}

impl Ffts {
    pub(crate) fn new() -> Self {
        Self {
            planner: RealFftPlanner::new(),
            scratch: Vec::new(),
            bins: Vec::new(),
        }
    }

    pub(crate) fn forward(&mut self, size: usize) -> Arc<dyn RealToComplex<Real>> {
        self.planner.plan_fft_forward(size)
    }

    pub(crate) fn inverse(&mut self, size: usize) -> Arc<dyn ComplexToReal<Real>> {
        self.planner.plan_fft_inverse(size)
    }

    /// The spectrum of `signal`, `signal.len() / 2 + 1` bins.
    pub(crate) fn analyse(&mut self, signal: &[f32]) -> Spectrum {
        let fft = self.forward(signal.len());
        let mut input = signal.to_vec();
        let mut output = fft.make_output_vec();
        self.scratch
            .resize(fft.get_scratch_len(), Complex::default());
        fft.process_with_scratch(&mut input, &mut output, &mut self.scratch)
            .expect("the buffers are the plan's sizes");
        output
    }

    /// Writes into `out` (`size` samples) one cycle holding `spectrum`'s
    /// harmonics up to `harmonics`, where `spectrum` is unnormalised over a
    /// cycle of `source` samples. Each harmonic keeps its amplitude and
    /// phase, so `out` is the band-limited cycle sampled at `size` points.
    /// With `gains` ([`spline_gains`]), each bin is first scaled by its gain.
    pub(crate) fn synthesise(
        &mut self,
        spectrum: &[Complex<Real>],
        source: usize,
        harmonics: usize,
        gains: Option<&[Real]>,
        out: &mut [Real],
    ) {
        let size = out.len();
        let fft = self.inverse(size);
        let mut bins = std::mem::take(&mut self.bins);
        bins.clear();
        bins.resize(size / 2 + 1, Complex::default());
        let target_nyquist = size / 2;
        let source_nyquist = source.is_multiple_of(2).then_some(source / 2);
        let top = harmonics.min(spectrum.len() - 1).min(target_nyquist);
        for (k, bin) in bins.iter_mut().enumerate().take(top + 1) {
            let x = spectrum[k];
            *bin = if k == 0 {
                Complex::new(x.re, 0.0)
            } else if Some(k) == source_nyquist && size.is_multiple_of(2) && k == target_nyquist {
                // Nyquist in both: its real part, as it was.
                Complex::new(x.re, 0.0)
            } else if Some(k) == source_nyquist {
                // The source's Nyquist bin stands for both halves of a
                // cosine; elsewhere, one bin of a conjugate pair does.
                Complex::new(x.re / 2.0, 0.0)
            } else if size.is_multiple_of(2) && k == target_nyquist {
                // A whole harmonic landing on the target's Nyquist keeps
                // its cosine part.
                Complex::new(2.0 * x.re, 0.0)
            } else {
                x
            };
        }
        if let Some(gains) = gains {
            for (bin, gain) in bins.iter_mut().zip(gains) {
                *bin *= *gain;
            }
        }
        self.scratch
            .resize(fft.get_scratch_len(), Complex::default());
        fft.process_with_scratch(&mut bins, out, &mut self.scratch)
            .expect("the buffers are the plan's sizes and the edge bins are real");
        self.bins = bins;
        let scale = 1.0 / source as Real;
        for x in out.iter_mut() {
            *x *= scale;
        }
    }

    /// `cycle`, of any length from 2 samples, as a frame of
    /// [`FRAME_SIZE`] samples: harmonics above the frame's Nyquist are
    /// removed, and the rest keep their amplitude and phase.
    pub(crate) fn resample(&mut self, cycle: &[f32]) -> Frame {
        let mut frame = Box::new([0.0f32; FRAME_SIZE]);
        if cycle.len() == FRAME_SIZE {
            frame.copy_from_slice(cycle);
            return frame;
        }
        let spectrum = self.analyse(cycle);
        // A longer cycle's harmonic at the frame's own Nyquist can't keep
        // its phase there, so it goes too.
        let harmonics = if cycle.len() > FRAME_SIZE {
            FRAME_SIZE / 2 - 1
        } else {
            FRAME_SIZE / 2
        };
        self.synthesise(&spectrum, cycle.len(), harmonics, None, &mut frame[..]);
        frame
    }
}

/// The gains that turn a cycle of `size` samples into its quintic B-spline
/// coefficients, one per bin: dividing each harmonic by the sampled
/// spline's response (Unser, Aldroubi and Eden, 1991), and by the weights'
/// common 120, makes [`Wave::read`](crate::Wave::read)'s spline pass
/// through the cycle's samples.
pub(crate) fn spline_gains(size: usize) -> Vec<Real> {
    (0..=size / 2)
        .map(|k| {
            let w = 2.0 * PI * k as Real / size as Real;
            1.0 / (66.0 + 52.0 * w.cos() + 2.0 * (2.0 * w).cos())
        })
        .collect()
}

/// Where each frame of a built table comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameKind {
    /// Drawn frame `n` of the source, counting from 0.
    Drawn(usize),
    /// Made at load between drawn frames `from` and `from + 1`, `t` of the
    /// way from one to the other.
    Morphed {
        /// The drawn frame before it.
        from: usize,
        /// How far it is towards the next drawn frame, between 0 and 1.
        t: f32,
    },
}

/// How a table's frames map onto its drawn frames: one entry per frame of
/// the built table (engine-core R13). With interpolation and between 2 and
/// 255 drawn frames, the table is filled to [`MAX_FRAMES`](crate::MAX_FRAMES),
/// each drawn frame kept at an evenly spaced position and the frames
/// between them morphed; otherwise the drawn frames are the table.
pub(crate) fn layout(drawn: usize, interpolation: Interpolation) -> Vec<FrameKind> {
    let total = crate::MAX_FRAMES;
    if interpolation == Interpolation::None || drawn < 2 || drawn >= total {
        return (0..drawn).map(FrameKind::Drawn).collect();
    }
    let gaps = drawn - 1;
    let position = |i: usize| (i * (total - 1) + gaps / 2) / gaps;
    let mut kinds = Vec::with_capacity(total);
    for from in 0..gaps {
        let (start, end) = (position(from), position(from + 1));
        kinds.push(FrameKind::Drawn(from));
        for j in start + 1..end {
            let t = (j - start) as f32 / (end - start) as f32;
            kinds.push(FrameKind::Morphed { from, t });
        }
    }
    kinds.push(FrameKind::Drawn(gaps));
    kinds
}

/// A drawn frame's spectrum, and each bin's magnitude and phase when a
/// spectral morph needs them.
pub(crate) struct Key {
    bins: Spectrum,
    polar: Vec<(Real, Real)>,
}

/// The drawn frames' spectra, prepared for `interpolation`: shifted so each
/// fundamental starts at phase zero, or with every phase at zero, for those
/// modes when frames are morphed.
pub(crate) fn keyframes(
    ffts: &mut Ffts,
    frames: &[Frame],
    interpolation: Interpolation,
    morphing: bool,
) -> Vec<Key> {
    frames
        .iter()
        .map(|frame| {
            let mut bins = ffts.analyse(&frame[..]);
            let last = bins.len() - 1;
            if morphing {
                match interpolation {
                    Interpolation::SpectralZeroFundamental if bins[1].norm() > 1e-6 => {
                        let phase = bins[1].arg();
                        for (k, bin) in bins.iter_mut().enumerate().skip(1) {
                            *bin *= Complex::from_polar(1.0, -(k as Real) * phase);
                        }
                        // Nyquist stays real.
                        bins[last] = Complex::new(bins[last].re, 0.0);
                    }
                    Interpolation::SpectralZeroPhases => {
                        for bin in &mut bins[1..last] {
                            *bin = Complex::new(bin.norm(), 0.0);
                        }
                    }
                    _ => {}
                }
            }
            let spectral = morphing
                && !matches!(
                    interpolation,
                    Interpolation::None | Interpolation::Crossfade
                );
            let polar = if spectral {
                bins.iter().map(|b| (b.norm(), b.arg())).collect()
            } else {
                Vec::new()
            };
            Key { bins, polar }
        })
        .collect()
}

/// Writes into `out` the spectrum of the frame `kind` names.
pub(crate) fn frame_spectrum(keys: &[Key], kind: FrameKind, out: &mut Spectrum) {
    match kind {
        FrameKind::Drawn(n) => out.clone_from(&keys[n].bins),
        FrameKind::Morphed { from, t } => {
            let (a, b) = (&keys[from], &keys[from + 1]);
            out.clear();
            let last = a.bins.len() - 1;
            if a.polar.is_empty() {
                // A crossfade of samples is a crossfade of bins.
                out.extend(a.bins.iter().zip(&b.bins).map(|(&a, &b)| a + (b - a) * t));
                return;
            }
            out.extend(a.polar.iter().zip(&b.polar).enumerate().map(
                |(k, (&(a_norm, a_arg), &(b_norm, b_arg)))| {
                    if k == 0 || k == last {
                        // DC and Nyquist are real: crossfade them.
                        let (a, b) = (a.bins[k].re, b.bins[k].re);
                        return Complex::new(a + (b - a) * t, 0.0);
                    }
                    let magnitude = a_norm + (b_norm - a_norm) * t;
                    let mut turn = b_arg - a_arg;
                    if turn > PI {
                        turn -= 2.0 * PI;
                    } else if turn < -PI {
                        turn += 2.0 * PI;
                    }
                    Complex::from_polar(magnitude, a_arg + turn * t)
                },
            ));
        }
    }
}
