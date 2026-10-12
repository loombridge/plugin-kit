//! Decimation by 2, 4 or 8 across lanes, for the shapers that render
//! oversampled: a cascade of polyphase IIR half-band filters.
//!
//! Each stage halves the rate with a half-band low-pass made of two chains
//! of first-order allpass sections in z⁻², one fed the even samples and
//! one the odd, whose outputs are averaged: the elliptic design of
//! Valenzuela and Constantinides (*Digital signal processing schemes for
//! efficient interpolation and decimation*, IEE Proceedings G, 1983). Its
//! phase isn't linear, but its delay is a few samples, where a
//! linear-phase filter as steep would delay the oscillator by tens.
//!
//! The last stage passes up to 0.45 of the output rate and stops from 0.55,
//! 100 dB down: what lies between folds above 0.45 of the rate, outside
//! the band the shapers' aliasing is measured in (below 20 kHz at 44.1 kHz
//! and up). Earlier stages only have to keep their images out of that
//! band, so they have wider transitions and fewer sections.
//!
//! Every stage uses separately rounded multiplies and adds, so the scalar
//! path and every SIMD level give the same bits.

use lspk_dsp::simd::{self, F32s, Kernel, Level};

/// The most allpass coefficients a stage has.
pub(crate) const MAX_COEFS: usize = 12;

/// The most stages: decimation by 8.
pub(crate) const MAX_STAGES: usize = 3;

/// The stopband attenuation every stage is designed for, in dB.
pub(crate) const ATTENUATION_DB: f64 = 100.0;

/// The last stage's half-width of transition, as a fraction of its input
/// rate: pass to 0.45 and stop from 0.55 of the output rate.
const LAST_TRANSITION: f64 = 0.025;

/// One stage's allpass coefficients, the first to the path fed the later
/// sample of each pair, then alternating.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Design {
    pub coefs: [f32; MAX_COEFS],
    pub count: usize,
}

/// The half-width of transition, as a fraction of its input rate, of the
/// stage `from_last` stages before the last (0 for the last): wide enough
/// that what it lets through folds outside 0.55 of the final rate.
pub(crate) fn transition(from_last: usize) -> f64 {
    if from_last == 0 {
        LAST_TRANSITION
    } else {
        // The stage's output rate is 2^from_last of the final rate; it must
        // pass to 0.55 of the final rate.
        0.25 - 0.275 / f64::from(1u32 << from_last)
    }
}

/// The elliptic half-band design with half-width of transition
/// `transition` (a fraction of the input rate, below ¼) and at least
/// `attenuation` dB of stopband, with as few coefficients as reach it (at
/// most [`MAX_COEFS`]).
pub(crate) fn design(transition: f64, attenuation: f64) -> Design {
    let (k, q) = transition_parameters(transition);
    let order = order_for(attenuation, q).min(2 * MAX_COEFS + 1);
    let count = (order - 1) / 2;
    let mut coefs = [0.0f32; MAX_COEFS];
    for (i, coef) in coefs.iter_mut().enumerate().take(count) {
        *coef = coefficient(i, k, q, order) as f32;
    }
    Design { coefs, count }
}

/// The selectivity `k` and nome `q` of the elliptic design for a
/// half-width of transition `transition`.
fn transition_parameters(transition: f64) -> (f64, f64) {
    let k = ((1.0 - 2.0 * transition) * std::f64::consts::FRAC_PI_4).tan();
    let k = k * k;
    let root = (1.0 - k * k).powf(0.25);
    let e = 0.5 * (1.0 - root) / (1.0 + root);
    let e4 = e.powi(4);
    let q = e * (1.0 + e4 * (2.0 + e4 * (15.0 + 150.0 * e4)));
    (k, q)
}

/// The odd filter order that reaches `attenuation` dB with nome `q`.
fn order_for(attenuation: f64, q: f64) -> usize {
    let power = 10f64.powf(-attenuation / 10.0);
    let a = power / (1.0 - power);
    let mut order = ((a * a / 16.0).ln() / q.ln()).ceil() as usize;
    if order.is_multiple_of(2) {
        order += 1;
    }
    order.max(3)
}

/// Allpass coefficient `index` of the design.
fn coefficient(index: usize, k: f64, q: f64, order: usize) -> f64 {
    let c = (index + 1) as f64;
    let order = order as f64;
    let pi = std::f64::consts::PI;
    // The theta functions' series, summed until their terms vanish.
    let mut numerator = 0.0;
    let mut i = 0.0f64;
    let mut sign = 1.0;
    loop {
        let term = q.powf(i * (i + 1.0)) * ((2.0 * i + 1.0) * c * pi / order).sin() * sign;
        numerator += term;
        if term.abs() < 1e-100 {
            break;
        }
        sign = -sign;
        i += 1.0;
    }
    let mut denominator = 0.0;
    let mut i = 1.0f64;
    let mut sign = -1.0;
    loop {
        let term = q.powf(i * i) * (2.0 * i * c * pi / order).cos() * sign;
        denominator += term;
        if term.abs() < 1e-100 {
            break;
        }
        sign = -sign;
        i += 1.0;
    }
    let w = numerator * q.powf(0.25) / (denominator + 0.5);
    let w2 = w * w;
    let x = ((1.0 - w2 * k) * (1.0 - w2 / k)).sqrt() / (1.0 + w2);
    (1.0 - x) / (1.0 + x)
}

/// The decimator for every lane: each stage's design and its sections'
/// state.
#[derive(Debug)]
pub(crate) struct Decimator {
    /// Indexed by how many stages before the last.
    designs: [Design; MAX_STAGES],
    lane_count: usize,
    /// Per stage (from the last), per coefficient, the last input and
    /// output of each lane: `[stage][coef][x or y][lane]`.
    state: Box<[f32]>,
}

impl Decimator {
    /// A decimator for `lane_count` lanes. Allocates.
    pub(crate) fn new(lane_count: usize) -> Self {
        Self {
            designs: std::array::from_fn(|j| design(transition(j), ATTENUATION_DB)),
            lane_count,
            state: vec![0.0; MAX_STAGES * MAX_COEFS * 2 * lane_count].into(),
        }
    }

    /// Memory it holds itself, in bytes.
    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of_val(&*self.state)
    }

    /// Clears lanes `lanes`' state.
    pub(crate) fn reset(&mut self, lanes: std::ops::Range<usize>) {
        let n = self.lane_count;
        for block in self.state.chunks_exact_mut(n) {
            block[lanes.clone()].fill(0.0);
        }
    }

    /// Decimates `buffer` in place by `factor` (1, 2, 4 or 8): it holds
    /// `frames × factor` frames of `lane_count` lanes, frame by frame, and
    /// afterwards its first `frames` frames are the result. Lanes from
    /// `lanes` on are left alone. At SIMD level `level`, or one lane at a
    /// time with `None`.
    pub(crate) fn process(
        &mut self,
        level: Option<Level>,
        factor: usize,
        buffer: &mut [f32],
        frames: usize,
        lanes: usize,
    ) {
        let stages = factor.trailing_zeros() as usize;
        let n = self.lane_count;
        let block = MAX_COEFS * 2 * n;
        for s in 0..stages {
            let from_last = stages - 1 - s;
            let out_frames = frames << from_last;
            let stage = Stage {
                design: &self.designs[from_last],
                state: &mut self.state[from_last * block..(from_last + 1) * block],
                buffer: &mut *buffer,
                frames: out_frames,
                lane_count: n,
                lanes,
            };
            match level {
                Some(level) => {
                    if let Err(stage) = simd::dispatch_at(level, stage) {
                        let _ = simd::dispatch_at(Level::Portable, stage);
                    }
                }
                None => stage.scalar(),
            }
        }
    }
}

/// One stage over every lane, in place.
struct Stage<'a> {
    design: &'a Design,
    state: &'a mut [f32],
    buffer: &'a mut [f32],
    /// Output frames.
    frames: usize,
    lane_count: usize,
    lanes: usize,
}

impl Stage<'_> {
    fn scalar(self) {
        let n = self.lane_count;
        let count = self.design.count;
        let coefs = &self.design.coefs;
        for l in 0..self.lanes {
            let mut x = [0.0f32; MAX_COEFS];
            let mut y = [0.0f32; MAX_COEFS];
            for i in 0..count {
                x[i] = self.state[(2 * i) * n + l];
                y[i] = self.state[(2 * i + 1) * n + l];
            }
            for k in 0..self.frames {
                let early = self.buffer[2 * k * n + l];
                let late = self.buffer[(2 * k + 1) * n + l];
                let mut paths = [late, early];
                for i in 0..count {
                    let input = paths[i % 2];
                    let output = (input - y[i]) * coefs[i] + x[i];
                    x[i] = input;
                    y[i] = output;
                    paths[i % 2] = output;
                }
                self.buffer[k * n + l] = (paths[0] + paths[1]) * 0.5;
            }
            for i in 0..count {
                self.state[(2 * i) * n + l] = x[i];
                self.state[(2 * i + 1) * n + l] = y[i];
            }
        }
    }
}

impl Kernel for Stage<'_> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        let n = self.lane_count;
        let count = self.design.count;
        let coefs: [S; MAX_COEFS] = std::array::from_fn(|i| S::splat(self.design.coefs[i]));
        let span = self.lanes.div_ceil(S::LANES) * S::LANES;
        for c in (0..span).step_by(S::LANES) {
            let mut x = [S::splat(0.0); MAX_COEFS];
            let mut y = [S::splat(0.0); MAX_COEFS];
            for i in 0..count {
                x[i] = S::load(&self.state[(2 * i) * n + c..]);
                y[i] = S::load(&self.state[(2 * i + 1) * n + c..]);
            }
            for k in 0..self.frames {
                let early = S::load(&self.buffer[2 * k * n + c..]);
                let late = S::load(&self.buffer[(2 * k + 1) * n + c..]);
                let mut paths = [late, early];
                for i in 0..count {
                    let input = paths[i % 2];
                    let output = (input - y[i]) * coefs[i] + x[i];
                    x[i] = input;
                    y[i] = output;
                    paths[i % 2] = output;
                }
                ((paths[0] + paths[1]) * S::splat(0.5)).store(&mut self.buffer[k * n + c..]);
            }
            for i in 0..count {
                x[i].store(&mut self.state[(2 * i) * n + c..]);
                y[i].store(&mut self.state[(2 * i + 1) * n + c..]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// The half-band filter's gain at `f` (a fraction of its input rate):
    /// ½ (A₀(z²) + z⁻¹ A₁(z²)).
    fn gain(design: &Design, f: f64) -> f64 {
        let z2 = num_complex(-2.0 * TAU * f);
        let z1 = num_complex(-TAU * f);
        let mut a = [(1.0, 0.0), (1.0, 0.0)];
        for (i, &coef) in design.coefs[..design.count].iter().enumerate() {
            let c = f64::from(coef);
            // (c + z⁻²) / (1 + c z⁻²)
            let num = (c + z2.0, z2.1);
            let den = (1.0 + c * z2.0, c * z2.1);
            a[i % 2] = mul(a[i % 2], div(num, den));
        }
        let sum = add(a[0], mul(z1, a[1]));
        0.5 * (sum.0 * sum.0 + sum.1 * sum.1).sqrt()
    }

    fn num_complex(angle: f64) -> (f64, f64) {
        (angle.cos(), angle.sin())
    }
    fn add(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
        (a.0 + b.0, a.1 + b.1)
    }
    fn mul(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
        (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
    }
    fn div(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
        let d = b.0 * b.0 + b.1 * b.1;
        ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
    }

    #[test]
    fn each_stage_passes_its_band_and_stops_its_images() {
        for from_last in 0..MAX_STAGES {
            let tb = transition(from_last);
            let d = design(tb, ATTENUATION_DB);
            let pass_edge = 0.25 - tb;
            let stop_edge = 0.25 + tb;
            let mut ripple = 0.0f64;
            let mut stop = f64::NEG_INFINITY;
            for i in 0..=2000 {
                let f = 0.5 * f64::from(i) / 2000.0;
                let g = gain(&d, f);
                if f <= pass_edge {
                    ripple = ripple.max((20.0 * g.log10()).abs());
                } else if f >= stop_edge {
                    stop = stop.max(20.0 * g.log10());
                }
            }
            println!(
                "stage {from_last} before the last: {} coefficients, pass to {pass_edge:.4}, ripple {ripple:.2e} dB, stopband {stop:.1} dB",
                d.count
            );
            assert!(ripple < 1e-3, "{ripple} dB");
            assert!(stop < -ATTENUATION_DB + 0.5, "{stop:.1} dB");
        }
    }

    /// Decimates a sine of `f` (a fraction of the final rate) by `factor`
    /// and gives the output's RMS against the input's.
    fn decimated_gain(factor: usize, f: f64, level: Option<Level>) -> f64 {
        let lanes = 3;
        let n = 16;
        let mut decimator = Decimator::new(n);
        let frames = 64;
        let mut energy = 0.0;
        let mut count = 0;
        for block in 0..64 {
            let mut buffer = vec![0.0f32; frames * factor * n];
            for k in 0..frames * factor {
                let t = (block * frames * factor + k) as f64 / factor as f64;
                for l in 0..lanes {
                    buffer[k * n + l] = (TAU * f * t + l as f64).sin() as f32;
                }
            }
            decimator.process(level, factor, &mut buffer, frames, lanes);
            if block >= 16 {
                for k in 0..frames {
                    energy += f64::from(buffer[k * n]).powi(2);
                    count += 1;
                }
            }
        }
        (2.0 * energy / f64::from(count)).sqrt()
    }

    #[test]
    fn decimating_keeps_the_band_and_removes_what_would_fold_into_it() {
        for factor in [2, 4, 8] {
            for level in [None, Some(simd::level())] {
                let pass = decimated_gain(factor, 0.1, level);
                assert!((pass - 1.0).abs() < 1e-3, "×{factor} {level:?}: {pass}");
                // Tones that would fold to 0.1 of the rate.
                for image in [0.9, factor as f64 / 2.0 - 0.1] {
                    let got = 20.0 * decimated_gain(factor, image, level).log10();
                    assert!(got < -95.0, "×{factor} {level:?}: {image} at {got:.1} dB");
                }
                for stop in [0.6, 0.72, 0.9, 1.3, 1.45] {
                    if stop < factor as f64 / 2.0 {
                        let got = 20.0 * decimated_gain(factor, stop, level).log10();
                        println!("×{factor}: {stop} at {got:.1} dB");
                        assert!(got < -95.0, "×{factor} {level:?}: {stop} at {got:.1} dB");
                    }
                }
            }
        }
    }

    #[test]
    fn every_path_gives_the_same_bits() {
        let n = 16;
        let lanes = 13;
        let factor = 8;
        let frames = 40;
        let input: Vec<f32> = (0..frames * factor * n)
            .map(|i| ((i * 7919) % 1000) as f32 / 500.0 - 1.0)
            .collect();
        let mut reference = input.clone();
        Decimator::new(n).process(None, factor, &mut reference, frames, lanes);
        for level in Level::ALL.into_iter().filter(|l| l.is_supported()) {
            let mut got = input.clone();
            Decimator::new(n).process(Some(level), factor, &mut got, frames, lanes);
            for k in 0..frames {
                for l in 0..lanes {
                    assert_eq!(
                        got[k * n + l].to_bits(),
                        reference[k * n + l].to_bits(),
                        "{level}: frame {k} lane {l}"
                    );
                }
            }
        }
    }
}
