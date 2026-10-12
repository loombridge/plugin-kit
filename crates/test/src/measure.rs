//! Measurements of rendered audio, as plain numbers in stated units.
//!
//! | Function | Measures | Method |
//! |---|---|---|
//! | [`peak`], [`rms`] | Level, as a linear amplitude ([`db`] gives dBFS) | The largest absolute sample; the root of the mean square |
//! | [`noise_floor`] | The level of what should be silence, in dBFS | RMS with any DC offset removed |
//! | [`tone`] | One frequency's amplitude and phase | A single-bin DFT under a 7-term Blackman–Harris window, whose side lobes sit about 180 dB down |
//! | [`frequency_response`] | An effect's gain and phase at each frequency | Stepped sines: each frequency rendered on its own, the output's [`tone`] over the input's once it has settled |
//! | [`thd`] | Total harmonic distortion: the harmonics' level over the fundamental's | Each harmonic's [`tone`] |
//! | [`thd_n`] | THD plus noise: everything but the fundamental, over the fundamental | A least-squares fit of the fundamental (with DC), and the residual's RMS over the fit's |
//! | [`aliasing`] | Energy away from a note's harmonics: what aliasing folds back, plus noise | spectra under the same window; the energy outside bands around each harmonic below Nyquist over the energy inside them, following a sweep frame by frame |
//! | [`pitch`] | A periodic signal's fundamental, in hertz | YIN's cumulative mean normalised difference (de Cheveigné and Kawahara, 2002) for the period, refined over as many periods as the signal holds |
//! | [`bands`], [`centroid`] | The spectrum in octave bands, as each band's RMS in dBFS; the spectral centroid in hertz | Welch-averaged power spectra under a Hann window, summed per band (Parseval) |
//! | [`envelope`] | When a sound starts, peaks and dies away | RMS in 5 ms windows against the loudest window |
//!
//! Every function takes one channel's samples as `f32` and works in `f64`.

use std::f64::consts::{PI, TAU};

use realfft::RealFftPlanner;

/// A linear ratio or amplitude in decibels (dBFS for an amplitude against
/// full scale): −∞ for 0.
#[must_use]
pub fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// The largest absolute sample.
#[must_use]
pub fn peak(signal: &[f32]) -> f64 {
    signal.iter().fold(0.0f64, |m, s| m.max(f64::from(s.abs())))
}

/// The root mean square.
#[must_use]
pub fn rms(signal: &[f32]) -> f64 {
    if signal.is_empty() {
        return 0.0;
    }
    let sum: f64 = signal.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / signal.len() as f64).sqrt()
}

/// The level of `signal`, which should be silent (an idle instrument, an
/// effect fed silence), in dBFS: its RMS once any DC offset is removed.
/// −∞ for digital silence.
#[must_use]
pub fn noise_floor(signal: &[f32]) -> f64 {
    if signal.is_empty() {
        return f64::NEG_INFINITY;
    }
    let n = signal.len() as f64;
    let mean = signal.iter().map(|&s| f64::from(s)).sum::<f64>() / n;
    let sum: f64 = signal.iter().map(|&s| (f64::from(s) - mean).powi(2)).sum();
    db((sum / n).sqrt())
}

/// The 7-term Blackman–Harris window, `n` points long: side lobes about
/// 180 dB down, so a tone leaks nothing measurable beyond the 7 bins either
/// side of it that its main lobe covers.
fn window(n: usize) -> Vec<f64> {
    const A: [f64; 7] = [
        0.271_051_400_693_42,
        0.433_297_939_234_48,
        0.218_122_999_543_11,
        0.065_925_446_388_03,
        0.010_811_742_098_37,
        0.000_776_584_825_22,
        0.000_013_887_217_35,
    ];
    let m = (n.max(2) - 1) as f64;
    (0..n)
        .map(|i| {
            let x = TAU * i as f64 / m;
            A.iter()
                .enumerate()
                .map(|(k, a)| {
                    let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
                    sign * a * (k as f64 * x).cos()
                })
                .sum()
        })
        .collect()
}

/// The amplitude and phase of `frequency` in `signal`: for
/// `a·cos(2πft + φ)`, `(a, φ)`, with φ in radians from −π to π. A sine
/// starting at 0 has phase −π/2.
///
/// The window separates frequencies about eight DFT bins apart
/// (`8 · sample_rate / signal.len()` hertz), so give it several dozen
/// periods of the lowest frequency it must tell apart.
#[must_use]
pub fn tone(signal: &[f32], sample_rate: f64, frequency: f64) -> (f64, f64) {
    let window = self::window(signal.len());
    let w = TAU * frequency / sample_rate;
    let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
    for (n, (&x, &win)) in signal.iter().zip(&window).enumerate() {
        let x = f64::from(x) * win;
        let angle = w * n as f64;
        re += x * angle.cos();
        im -= x * angle.sin();
        sum += win;
    }
    if sum == 0.0 {
        return (0.0, 0.0);
    }
    (2.0 * re.hypot(im) / sum, im.atan2(re))
}

/// An effect's response at one frequency.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Response {
    /// The frequency, in hertz.
    pub frequency: f64,
    /// The output's level over the input's, in dB.
    pub gain_db: f64,
    /// The output's phase less the input's, in radians from −π to π.
    /// Latency shows here as a phase that falls with frequency.
    pub phase: f64,
}

/// `count` frequencies spaced evenly in octaves from `low` to `high`
/// hertz, both included.
#[must_use]
pub fn log_frequencies(low: f64, high: f64, count: usize) -> Vec<f64> {
    match count {
        0 => Vec::new(),
        1 => vec![low],
        _ => (0..count)
            .map(|i| low * (high / low).powf(i as f64 / (count - 1) as f64))
            .collect(),
    }
}

/// The input level [`frequency_response`] measures at: −6 dBFS.
pub const RESPONSE_LEVEL: f64 = 0.5;

/// The frequency response of an effect: at each of `frequencies`, a sine at
/// [`RESPONSE_LEVEL`] goes through `render` (which renders an input signal
/// with a fresh instance and returns one output channel) and the output's
/// [`tone`] is compared with the input's, once the first 50 ms (or 2,048
/// frames, if more) have let it settle. The measured stretch holds at least
/// 16,384 frames and 32 periods.
///
/// This suits effects that are close to linear and time-invariant (filters,
/// EQs, gains); a distortion's response depends on its level.
pub fn frequency_response(
    sample_rate: f64,
    frequencies: &[f64],
    mut render: impl FnMut(&[f32]) -> Vec<f32>,
) -> Vec<Response> {
    let settle = ((sample_rate * 0.05).ceil() as usize).max(2_048);
    frequencies
        .iter()
        .map(|&frequency| {
            let measured = ((32.0 * sample_rate / frequency).ceil() as usize).max(16_384);
            let input =
                crate::signal::sine(frequency, sample_rate, settle + measured, RESPONSE_LEVEL);
            let output = render(&input);
            let end = (settle + measured).min(output.len());
            let (x, phase_x) = tone(&input[settle..settle + measured], sample_rate, frequency);
            let (y, phase_y) = tone(
                output.get(settle..end).unwrap_or(&[]),
                sample_rate,
                frequency,
            );
            Response {
                frequency,
                gain_db: db(y / x),
                phase: wrap(phase_y - phase_x),
            }
        })
        .collect()
}

/// `angle` wrapped into −π to π.
fn wrap(angle: f64) -> f64 {
    let wrapped = (angle + PI).rem_euclid(TAU) - PI;
    if wrapped <= -PI {
        wrapped + TAU
    } else {
        wrapped
    }
}

/// Total harmonic distortion: the root sum of the squares of harmonics 2
/// to `harmonics` (those below Nyquist) over the fundamental's amplitude, as
/// a ratio ([`db`] gives dB). `fundamental` must be known to well under a
/// DFT bin; give the signal several dozen periods.
#[must_use]
pub fn thd(signal: &[f32], sample_rate: f64, fundamental: f64, harmonics: usize) -> f64 {
    let (first, _) = tone(signal, sample_rate, fundamental);
    if first == 0.0 {
        return f64::INFINITY;
    }
    let sum: f64 = (2..=harmonics)
        .map(|h| h as f64 * fundamental)
        .take_while(|f| *f < sample_rate / 2.0)
        .map(|f| tone(signal, sample_rate, f).0.powi(2))
        .sum();
    sum.sqrt() / first
}

/// THD plus noise: the RMS of everything but the fundamental (and DC) over
/// the fundamental's RMS, as a ratio ([`db`] gives dB). The fundamental is
/// fitted by least squares at exactly `fundamental` hertz, so give its true
/// frequency, and only a steady stretch of the signal.
#[must_use]
pub fn thd_n(signal: &[f32], sample_rate: f64, fundamental: f64) -> f64 {
    let w = TAU * fundamental / sample_rate;
    // Normal equations for x ≈ a·cos + b·sin + c.
    let mut m = [[0.0f64; 3]; 3];
    let mut v = [0.0f64; 3];
    for (n, &x) in signal.iter().enumerate() {
        let (s, c) = (w * n as f64).sin_cos();
        let basis = [c, s, 1.0];
        for i in 0..3 {
            v[i] += basis[i] * f64::from(x);
            for j in 0..3 {
                m[i][j] += basis[i] * basis[j];
            }
        }
    }
    let Some([a, b, dc]) = solve3(m, v) else {
        return f64::INFINITY;
    };
    let mut residual = 0.0;
    let mut fitted = 0.0;
    for (n, &x) in signal.iter().enumerate() {
        let (s, c) = (w * n as f64).sin_cos();
        let tone = a * c + b * s;
        residual += (f64::from(x) - tone - dc).powi(2);
        fitted += tone * tone;
    }
    if fitted == 0.0 {
        return f64::INFINITY;
    }
    (residual / fitted).sqrt()
}

/// Solves `m · x = v` by Gaussian elimination with partial pivoting.
fn solve3(mut m: [[f64; 3]; 3], mut v: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let pivot = (col..3).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        v.swap(col, pivot);
        let pivot_row = m[col];
        for row in col + 1..3 {
            let factor = m[row][col] / pivot_row[col];
            for (value, pivot) in m[row].iter_mut().zip(pivot_row).skip(col) {
                *value -= factor * pivot;
            }
            v[row] -= factor * v[col];
        }
    }
    let mut x = [0.0; 3];
    for row in (0..3).rev() {
        let sum: f64 = (row + 1..3).map(|k| m[row][k] * x[k]).sum();
        x[row] = (v[row] - sum) / m[row][row];
    }
    Some(x)
}

/// The pitch of the note [`aliasing`] measures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pitch {
    /// A steady note at this frequency, in hertz.
    Steady(f64),
    /// An exponential sweep across the whole signal, as
    /// [`signal::sweep`](crate::signal::sweep) makes, from `start` to `end`
    /// hertz.
    Sweep {
        /// Its frequency at the first frame.
        start: f64,
        /// Its frequency after the last frame.
        end: f64,
    },
}

/// Energy inside and outside a note's harmonic bands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aliasing {
    /// Energy within the bands around the harmonics below Nyquist.
    pub harmonic: f64,
    /// Energy everywhere else above DC: aliases, inharmonic partials and
    /// noise.
    pub other: f64,
}

impl Aliasing {
    /// The energy outside the harmonics over the energy in them, in dB.
    #[must_use]
    pub fn ratio_db(&self) -> f64 {
        10.0 * (self.other / self.harmonic).log10()
    }
}

/// How many frames each spectrum [`aliasing`] takes covers.
pub const ALIASING_FRAME: usize = 8_192;

/// How energy in `signal` divides between the harmonics of a note at
/// `pitch` and everything else. A band-limited oscillator or an
/// oversampled distortion keeps its energy at the harmonics; aliasing
/// folds harmonics above Nyquist back to frequencies between them, so the
/// [`ratio_db`](Aliasing::ratio_db) is the aliasing (and noise) level
/// against the note.
///
/// Spectra of [`ALIASING_FRAME`] frames (or the whole signal, if shorter),
/// overlapping by half, each under the 7-term Blackman–Harris window. A harmonic's
/// band spans its frequency over the frame (for a sweep) and 9 bins either
/// side. Choose a note that doesn't divide the sample rate, or its aliases
/// land on its own harmonics.
#[must_use]
pub fn aliasing(signal: &[f32], sample_rate: f64, pitch: Pitch) -> Aliasing {
    aliasing_below(signal, sample_rate, pitch, sample_rate / 2.0)
}

/// [`aliasing`] counting only energy below `top` hertz: the aliasing in
/// the audible band, for a process whose decimation filter lets what folds
/// between `top` and Nyquist through, as an oversampled shaper's may.
#[must_use]
pub fn aliasing_below(signal: &[f32], sample_rate: f64, pitch: Pitch, top: f64) -> Aliasing {
    let frame = ALIASING_FRAME.min(signal.len());
    let mut result = Aliasing {
        harmonic: 0.0,
        other: 0.0,
    };
    if frame < 16 {
        return result;
    }
    let hop = frame / 2;
    let window = self::window(frame);
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(frame);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let bin = sample_rate / frame as f64;
    let nyquist = sample_rate / 2.0;
    let margin = 9.0 * bin;
    let frequency_at = |n: f64| match pitch {
        Pitch::Steady(f) => f,
        Pitch::Sweep { start, end } => crate::signal::sweep_frequency(start, end, signal.len(), n),
    };
    let mut from = 0;
    while from + frame <= signal.len() {
        for ((slot, &x), &w) in input
            .iter_mut()
            .zip(&signal[from..from + frame])
            .zip(&window)
        {
            *slot = f64::from(x) * w;
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            break;
        }
        let low = frequency_at(from as f64);
        let high = frequency_at((from + frame) as f64);
        let (low, high) = (low.min(high), low.max(high));
        for (k, value) in spectrum.iter().enumerate() {
            let f = k as f64 * bin;
            if f < margin || f >= top {
                continue;
            }
            let energy = value.norm_sqr();
            let h_low = ((f - margin) / high).ceil().max(1.0);
            let h_high = ((f + margin) / low).floor();
            let in_band = h_low <= h_high && h_low * low < nyquist;
            if in_band {
                result.harmonic += energy;
            } else {
                result.other += energy;
            }
        }
        from += hop;
    }
    result
}

/// The fundamental frequency of `signal`, in hertz, between 20 Hz and a
/// quarter of the sample rate; `None` if it isn't clearly periodic.
///
/// YIN finds the period from the first 4,096 frames (or half the signal,
/// if shorter); the period is then refined from the difference across as
/// many whole periods as half the signal holds, which divides the error by
/// that many. A steady sine of a second is measured to well under 0.01 Hz.
#[must_use]
pub fn pitch(signal: &[f32], sample_rate: f64) -> Option<f64> {
    let x: Vec<f64> = signal.iter().map(|&s| f64::from(s)).collect();
    let window = (x.len() / 2).min(4_096);
    // Up to a quarter of the sample rate.
    let min_lag = 4;
    let max_lag = ((sample_rate / 20.0).ceil() as usize).min(x.len().saturating_sub(window));
    if window < 32 || max_lag <= min_lag + 2 {
        return None;
    }
    let difference = |lag: usize, length: usize| -> f64 {
        x[..length]
            .iter()
            .zip(&x[lag..lag + length])
            .map(|(a, b)| (a - b).powi(2))
            .sum()
    };
    // YIN: the cumulative mean normalised difference, and the first dip
    // below the threshold (or the deepest, if none is).
    let mut normalised = vec![1.0; max_lag + 1];
    let mut running = 0.0;
    let mut differences = vec![0.0; max_lag + 1];
    for lag in 1..=max_lag {
        let d = difference(lag, window);
        differences[lag] = d;
        running += d;
        normalised[lag] = if running > 0.0 {
            d * lag as f64 / running
        } else {
            1.0
        };
    }
    const THRESHOLD: f64 = 0.1;
    let mut lag = (min_lag..max_lag).find(|&l| normalised[l] < THRESHOLD);
    if let Some(found) = lag.as_mut() {
        while *found + 1 < max_lag && normalised[*found + 1] < normalised[*found] {
            *found += 1;
        }
    }
    let lag = lag
        .or_else(|| (min_lag..max_lag).min_by(|&a, &b| normalised[a].total_cmp(&normalised[b])))?;
    if normalised[lag] > 0.5 {
        return None;
    }
    let period =
        lag as f64 + parabola(differences[lag - 1], differences[lag], differences[lag + 1]);
    // Refinement: the dip nearest a whole number of periods, four times as
    // many each round, as far out as half the signal reaches. Each round
    // divides the period's error by its number of periods.
    let most = ((x.len() / 2) as f64 / period).floor() as usize;
    let mut periods = 1usize;
    let mut period = period;
    while periods < most {
        let next = (periods * 4).min(most);
        let target = (period * next as f64).round() as usize;
        if target < 3 || target + 3 >= x.len() {
            break;
        }
        let length = x.len() - target - 3;
        let Some((best, _)) = (target - 2..=target + 2)
            .map(|l| (l, difference(l, length)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            break;
        };
        if best == target - 2 || best == target + 2 {
            break;
        }
        let refined = best as f64
            + parabola(
                difference(best - 1, length),
                difference(best, length),
                difference(best + 1, length),
            );
        period = refined / next as f64;
        periods = next;
    }
    Some(sample_rate / period)
}

/// The centres of the octave bands [`bands`] measures, in hertz.
pub const OCTAVE_BANDS: [f64; 10] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// How many frames each spectrum [`bands`] and [`centroid`] average covers.
const SPECTRUM_FRAME: usize = 4_096;

/// One octave band's level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// Its centre, in hertz; it spans half an octave either side.
    pub centre: f64,
    /// The RMS of what lies in it, in dBFS: a full-scale sine reads
    /// −3.01 dBFS in its band. −∞ for nothing.
    pub level_db: f64,
}

/// The power spectrum of `signal`, Welch-averaged over frames of
/// [`SPECTRUM_FRAME`] (or the whole signal, if shorter) overlapping by
/// half, under a Hann window, scaled so the bins above DC sum to the mean
/// square. Returns each bin's power and the bin width in hertz.
fn power_spectrum(signal: &[f32], sample_rate: f64) -> (Vec<f64>, f64) {
    let frame = SPECTRUM_FRAME.min(signal.len());
    if frame < 16 {
        return (Vec::new(), 0.0);
    }
    let hop = frame / 2;
    let window: Vec<f64> = (0..frame)
        .map(|i| 0.5 - 0.5 * (TAU * i as f64 / frame as f64).cos())
        .collect();
    let energy: f64 = window.iter().map(|w| w * w).sum();
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(frame);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut power = vec![0.0; spectrum.len()];
    let mut frames = 0usize;
    let mut from = 0;
    while from + frame <= signal.len() {
        for ((slot, &x), &w) in input
            .iter_mut()
            .zip(&signal[from..from + frame])
            .zip(&window)
        {
            *slot = f64::from(x) * w;
        }
        if fft.process(&mut input, &mut spectrum).is_err() {
            break;
        }
        for (p, value) in power.iter_mut().zip(&spectrum) {
            *p += value.norm_sqr();
        }
        frames += 1;
        from += hop;
    }
    // One-sided, so every bin but DC and Nyquist counts twice.
    let last = power.len() - 1;
    let scale = 1.0 / (frames.max(1) as f64 * frame as f64 * energy);
    for (k, p) in power.iter_mut().enumerate() {
        *p *= if k == 0 || (k == last && frame.is_multiple_of(2)) {
            scale
        } else {
            2.0 * scale
        };
    }
    (power, sample_rate / frame as f64)
}

/// The level of `signal` in each octave band of [`OCTAVE_BANDS`] below
/// Nyquist, as RMS in dBFS. The bands' powers sum to the signal's mean
/// square above DC, so a sine reads its own RMS in its band.
#[must_use]
pub fn bands(signal: &[f32], sample_rate: f64) -> Vec<Band> {
    let (power, bin) = power_spectrum(signal, sample_rate);
    OCTAVE_BANDS
        .iter()
        .filter(|&&centre| centre / 2f64.sqrt() < sample_rate / 2.0)
        .map(|&centre| {
            let (low, high) = (centre / 2f64.sqrt(), centre * 2f64.sqrt());
            let sum: f64 = power
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(k, _)| {
                    let f = *k as f64 * bin;
                    // The lowest band reaches down to DC, the highest up to
                    // Nyquist, so every bin above DC is in one band.
                    (f >= low || centre == OCTAVE_BANDS[0]) && (f < high || centre == 16_000.0)
                })
                .map(|(_, p)| p)
                .sum();
            Band {
                centre,
                level_db: 10.0 * sum.log10(),
            }
        })
        .collect()
}

/// The spectral centroid of `signal`: the power-weighted mean frequency,
/// in hertz; `None` for silence.
#[must_use]
pub fn centroid(signal: &[f32], sample_rate: f64) -> Option<f64> {
    let (power, bin) = power_spectrum(signal, sample_rate);
    let total: f64 = power.iter().skip(1).sum();
    if total <= 0.0 {
        return None;
    }
    let weighted: f64 = power
        .iter()
        .enumerate()
        .skip(1)
        .map(|(k, p)| k as f64 * bin * p)
        .sum();
    Some(weighted / total)
}

/// When a sound starts, peaks and dies away, as [`envelope`] measures it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope {
    /// When it first comes within 40 dB of its loudest, in seconds.
    pub onset: f64,
    /// When it first comes within 1 dB of its loudest, in seconds: the end
    /// of its attack.
    pub peak: f64,
    /// Its loudest level: the RMS of its loudest window, in dBFS.
    pub peak_db: f64,
    /// When it has fallen for good to 60 dB below its loudest, in seconds;
    /// the signal's length if it never does.
    pub end: f64,
}

/// How long each window [`envelope`] measures is, in seconds.
pub const ENVELOPE_WINDOW: f64 = 0.005;

/// The envelope of `signal`, from its RMS in windows of
/// [`ENVELOPE_WINDOW`]: when it starts, peaks and has died away. `None`
/// for silence.
#[must_use]
pub fn envelope(signal: &[f32], sample_rate: f64) -> Option<Envelope> {
    let window = ((ENVELOPE_WINDOW * sample_rate).round() as usize).max(1);
    let levels: Vec<f64> = signal.chunks(window).map(|w| db(rms(w))).collect();
    let peak_db = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !peak_db.is_finite() {
        return None;
    }
    let loudest = levels.iter().position(|&l| l >= peak_db - 1.0)?;
    let seconds = |index: usize| (index * window) as f64 / sample_rate;
    let onset = levels.iter().position(|&l| l >= peak_db - 40.0)?;
    let end = levels
        .iter()
        .rposition(|&l| l >= peak_db - 60.0)
        .map_or(signal.len() as f64 / sample_rate, |last| {
            (seconds(last + 1)).min(signal.len() as f64 / sample_rate)
        });
    Some(Envelope {
        onset: seconds(onset),
        peak: seconds(loudest),
        peak_db,
        end,
    })
}

/// Where a parabola through three equally spaced points has its vertex,
/// from the middle point, in samples: from −0.5 to 0.5.
fn parabola(before: f64, at: f64, after: f64) -> f64 {
    let denominator = before - 2.0 * at + after;
    if denominator.abs() < f64::EPSILON {
        0.0
    } else {
        (0.5 * (before - after) / denominator).clamp(-0.5, 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{mix, noise, scaled, sine, sweep};

    const RATE: f64 = 48_000.0;

    #[test]
    fn levels_of_a_sine() {
        let s = sine(1_000.0, RATE, 48_000, 0.5);
        assert!((peak(&s) - 0.5).abs() < 1e-6);
        assert!((rms(&s) - 0.5 / 2f64.sqrt()).abs() < 1e-6);
        assert!((db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(db(0.0), f64::NEG_INFINITY);
    }

    #[test]
    fn noise_floor_ignores_dc_and_reads_silence_as_minus_infinity() {
        assert_eq!(noise_floor(&[0.0; 64]), f64::NEG_INFINITY);
        assert_eq!(noise_floor(&[0.25; 64]), f64::NEG_INFINITY);
        // Uniform noise of peak a has RMS a/√3.
        let n = noise(1, 96_000, 0.01);
        let expected = db(0.01 / 3f64.sqrt());
        assert!(
            (noise_floor(&n) - expected).abs() < 0.1,
            "{}",
            noise_floor(&n)
        );
    }

    #[test]
    fn a_tone_has_its_amplitude_and_phase() {
        let s = sine(440.0, RATE, 48_000, 0.25);
        let (a, phase) = tone(&s, RATE, 440.0);
        assert!((a - 0.25).abs() < 1e-6, "{a}");
        assert!((phase + PI / 2.0).abs() < 1e-4, "{phase}");
        // A frequency that isn't there reads at the window's side lobes.
        assert!(db(tone(&s, RATE, 1_000.0).0) < -120.0);
    }

    #[test]
    fn a_one_pole_low_pass_has_its_textbook_response() {
        // y[n] = (1 − c)·x[n] + c·y[n − 1]; H(e^jw) = (1 − c) / (1 − c·e^−jw).
        let c = 0.9f64;
        let frequencies = log_frequencies(50.0, 10_000.0, 5);
        let response = frequency_response(RATE, &frequencies, |input| {
            let mut y = 0.0f64;
            input
                .iter()
                .map(|&x| {
                    y = (1.0 - c) * f64::from(x) + c * y;
                    y as f32
                })
                .collect()
        });
        for r in response {
            let w = TAU * r.frequency / RATE;
            let (re, im) = (1.0 - c * w.cos(), c * w.sin());
            let gain = db((1.0 - c) / re.hypot(im));
            let phase = -im.atan2(re);
            assert!((r.gain_db - gain).abs() < 0.01, "{r:?}: {gain} dB");
            assert!((r.phase - phase).abs() < 1e-3, "{r:?}: {phase} rad");
        }
    }

    #[test]
    fn a_delay_shows_as_falling_phase() {
        let response = frequency_response(RATE, &[1_000.0], |input| {
            let mut out = vec![0.0; 6];
            out.extend_from_slice(input);
            out
        });
        // Six frames at 1 kHz and 48 kHz: 45 degrees behind.
        assert!(response[0].gain_db.abs() < 1e-6);
        assert!(
            (response[0].phase + PI / 4.0).abs() < 1e-6,
            "{:?}",
            response[0]
        );
        let frequencies = log_frequencies(1.0, 8.0, 4);
        for (f, expected) in frequencies.iter().zip([1.0, 2.0, 4.0, 8.0]) {
            assert!((f - expected).abs() < 1e-12, "{frequencies:?}");
        }
        assert_eq!(log_frequencies(5.0, 8.0, 1), [5.0]);
    }

    #[test]
    fn distortion_reads_at_its_harmonics_level() {
        let fundamental = sine(1_000.0, RATE, 48_000, 0.5);
        // A second harmonic 40 dB down and a third 46 dB down: THD is the
        // root sum of squares, about −39 dB.
        let second = sine(2_000.0, RATE, 48_000, 0.005);
        let third = sine(3_000.0, RATE, 48_000, 0.0025);
        let signal = mix(&[&fundamental, &second, &third]);
        let expected = db((0.01f64.powi(2) + 0.005f64.powi(2)).sqrt());
        assert!((db(thd(&signal, RATE, 1_000.0, 10)) - expected).abs() < 0.01);
        assert!((db(thd_n(&signal, RATE, 1_000.0)) - expected).abs() < 0.05);
        // A clean sine sits at the f32 quantisation floor.
        assert!(db(thd_n(&fundamental, RATE, 1_000.0)) < -130.0);
        assert!(db(thd(&fundamental, RATE, 1_000.0, 10)) < -120.0);
        // Noise counts in THD+N but not in THD.
        let noisy = mix(&[&fundamental, &noise(5, 48_000, 0.005)]);
        assert!(db(thd_n(&noisy, RATE, 1_000.0)) > -45.0);
        assert!(db(thd(&noisy, RATE, 1_000.0, 10)) < -60.0);
    }

    #[test]
    fn aliasing_counts_energy_away_from_the_harmonics() {
        let note = 1_234.0;
        let harmonics: Vec<Vec<f32>> = (1..=5)
            .map(|h| sine(note * h as f64, RATE, 48_000, 0.4 / h as f64))
            .collect();
        let refs: Vec<&[f32]> = harmonics.iter().map(Vec::as_slice).collect();
        let clean = mix(&refs);
        let ratio = aliasing(&clean, RATE, Pitch::Steady(note)).ratio_db();
        assert!(ratio < -100.0, "{ratio}");
        // An alias 60 dB below the fundamental, between the harmonics.
        let alias = sine(note * 2.5, RATE, 48_000, 0.0004);
        let aliased = mix(&[&clean, &alias]);
        let ratio = aliasing(&aliased, RATE, Pitch::Steady(note)).ratio_db();
        let total: f64 = (1..=5).map(|h| (0.4 / h as f64).powi(2)).sum();
        let expected = 10.0 * (0.0004f64.powi(2) / total).log10();
        assert!(
            (ratio - expected).abs() < 0.5,
            "{ratio} dB, not {expected} dB"
        );
    }

    #[test]
    fn a_sweep_follows_its_harmonic_bands() {
        let frames = 96_000;
        let clean = sweep(200.0, 8_000.0, RATE, frames, 0.5);
        let pitch = Pitch::Sweep {
            start: 200.0,
            end: 8_000.0,
        };
        let ratio = aliasing(&clean, RATE, pitch).ratio_db();
        assert!(ratio < -90.0, "{ratio}");
        // A steady tone under the sweep is away from its harmonics most of
        // the time.
        let hum = sine(150.0, RATE, frames, 0.05);
        let ratio = aliasing(&mix(&[&clean, &hum]), RATE, pitch).ratio_db();
        assert!((ratio - db(0.1)).abs() < 1.0, "{ratio}");
    }

    #[test]
    fn pitch_of_sines_and_rich_waves() {
        for frequency in [27.5, 110.0, 440.0, 1_000.0, 3_520.0] {
            let s = sine(frequency, RATE, 48_000, 0.5);
            let measured = pitch(&s, RATE).expect("periodic");
            assert!(
                (measured - frequency).abs() < 0.01,
                "{measured} Hz, not {frequency} Hz"
            );
        }
        // A band-limited sawtooth: the fundamental, not a harmonic.
        let saw: Vec<Vec<f32>> = (1..=20)
            .map(|h| scaled(&sine(220.0 * h as f64, RATE, 48_000, 0.5), 1.0 / h as f64))
            .collect();
        let refs: Vec<&[f32]> = saw.iter().map(Vec::as_slice).collect();
        let measured = pitch(&mix(&refs), RATE).expect("periodic");
        assert!((measured - 220.0).abs() < 0.01, "{measured}");
        assert_eq!(pitch(&noise(9, 48_000, 0.5), RATE), None);
        assert_eq!(pitch(&[0.0; 10], RATE), None);
    }
    #[test]
    fn a_sine_reads_its_rms_in_its_band() {
        let tone = sine(1_000.0, 48_000.0, 48_000, 1.0);
        let bands = bands(&tone, 48_000.0);
        assert_eq!(bands.len(), 10);
        let loudest = bands
            .iter()
            .max_by(|a, b| a.level_db.total_cmp(&b.level_db))
            .expect("bands");
        assert_eq!(loudest.centre, 1_000.0);
        assert!((loudest.level_db + 3.01).abs() < 0.05, "{loudest:?}");
        let centroid = centroid(&tone, 48_000.0).expect("a centroid");
        assert!((centroid - 1_000.0).abs() < 5.0, "{centroid}");
        assert_eq!(super::centroid(&[0.0; 8_192], 48_000.0), None);
    }

    #[test]
    fn an_envelope_finds_the_start_peak_and_end() {
        let rate = 48_000.0;
        let mut signal = vec![0.0f32; 4_800];
        let mut tone = sine(440.0, rate, 24_000, 0.5);
        // A 50 ms rise, then a hold, then silence.
        for (i, s) in tone.iter_mut().take(2_400).enumerate() {
            *s *= i as f32 / 2_400.0;
        }
        signal.extend(tone);
        signal.extend(vec![0.0; 9_600]);
        let envelope = envelope(&signal, rate).expect("an envelope");
        assert!((envelope.onset - 0.1).abs() < 0.006, "{envelope:?}");
        assert!(
            envelope.peak > 0.135 && envelope.peak < 0.155,
            "{envelope:?}"
        );
        assert!(
            (envelope.peak_db - db(0.5 / 2f64.sqrt())).abs() < 0.5,
            "{envelope:?}"
        );
        assert!((envelope.end - 0.6).abs() < 0.006, "{envelope:?}");
        assert_eq!(super::envelope(&[0.0; 960], rate), None);
    }
}
