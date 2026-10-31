//! Perception features for matching one sound to another, and a distance
//! between two sounds' features that ranks similar sounds close.
//!
//! [`extract`] reads a render once and keeps what a listener tells sounds
//! apart by, in numbers of a fixed size:
//!
//! | Feature | Holds | Method |
//! |---|---|---|
//! | [`Features::spectrum`] | The spectrum over time: [`BANDS`] log-spaced bands from [`LOW_HZ`] to [`HIGH_HZ`], at [`FRAMES`] moments spread over the sound, in dB below its loudest cell | Hann-windowed power spectra of 4,096 samples, the mean power in each band |
//! | [`Features::envelope`] | How loud it is through the sound, [`FRAMES`] values in dB below its loudest | RMS in [`FRAMES`] equal windows |
//! | [`Features::pitch_hz`] | The fundamental of its steady part | [`measure::pitch`] |
//! | [`Features::amplitude_modulation`], [`Features::brightness_modulation`] | The rate and depth of a periodic wobble of the level (in dB) and of the spectral centroid (in octaves) | A 10 ms track, a moving average of half a second removed, scanned from 2 to 20 Hz with a single-bin DFT under a Hann window |
//! | [`Features::stereo`] | The width and the channels' correlation | Mid and side energies |
//! | [`Features::duration`], [`Features::level_db`] | How long it sounds and how loud it is overall | [`measure::envelope`] and RMS |
//!
//! The spectrum and the envelope are relative, so [`distance`] doesn't
//! count a plain change of gain as a different timbre: it has its own,
//! small term.
//!
//! Features are plain data (`serde`), so a dataset can store them beside
//! each render.

use std::f64::consts::TAU;
use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use crate::measure;
use crate::render::Output;

/// How many moments of the sound the spectrum and the envelope are read at.
pub const FRAMES: usize = 16;
/// How many bands the spectrum has.
pub const BANDS: usize = 16;
/// The lower edge of the lowest band, in hertz.
pub const LOW_HZ: f64 = 50.0;
/// The upper edge of the highest band, in hertz.
pub const HIGH_HZ: f64 = 16_000.0;
/// The lowest rate of modulation read, in hertz; slower movement is in the
/// spectrum's and the envelope's course.
pub const MIN_RATE_HZ: f64 = 2.0;
/// The highest rate of modulation read, in hertz; faster is timbre.
pub const MAX_RATE_HZ: f64 = 20.0;

/// The size of the spectrum frames, in samples.
const FFT: usize = 4_096;
/// The size of the frames the brightness track reads, in samples.
const TRACK_FFT: usize = 1_024;
/// How far below its loudest a cell of the spectrum counts, in dB.
const SPECTRUM_RANGE_DB: f64 = 80.0;
/// How far below its peak an envelope value counts, in dB.
const ENVELOPE_RANGE_DB: f64 = 60.0;
/// The hop of the modulation tracks, in seconds.
const TRACK_HOP: f64 = 0.01;
/// The window of the amplitude track, in seconds: long enough that the
/// ripple of an RMS over a few periods stays well under [`MIN_AMPLITUDE_DEPTH`].
const TRACK_WINDOW: f64 = 0.05;
/// The width of the moving average removed from a track, in seconds.
const TREND: f64 = 0.5;
/// The shortest sound a modulation is read from, in seconds.
const MIN_MODULATED: f64 = 0.75;
/// The smallest wobble of the level counted, in dB (peak from the mean).
const MIN_AMPLITUDE_DEPTH: f64 = 0.5;
/// The smallest wobble of the centroid counted, in octaves.
const MIN_BRIGHTNESS_DEPTH: f64 = 0.05;
/// How far below its peak a frame of the brightness track still has a
/// centroid worth reading, in dB.
const BRIGHTNESS_RANGE_DB: f64 = 40.0;

/// The weights of [`distance`]'s terms. Each term is about 1 for a
/// difference a listener calls large: 12 dB of spectrum or envelope, an
/// octave of pitch or modulation rate.
const W_SPECTRUM: f64 = 1.0;
const W_ENVELOPE: f64 = 0.5;
const W_DURATION: f64 = 0.25;
const W_PITCH: f64 = 0.5;
const W_MODULATION: f64 = 0.25;
const W_STEREO: f64 = 0.25;
const W_LEVEL: f64 = 0.1;

/// A periodic wobble of a quantity.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Modulation {
    /// How fast it goes round, in hertz.
    pub rate_hz: f64,
    /// How far it swings either side of its mean: dB for the level,
    /// octaves for the centroid.
    pub depth: f64,
}

/// How wide a sound is.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stereo {
    /// The share of the energy that is side (left minus right) rather
    /// than mid, from 0 for mono to 1 for opposite channels.
    pub width: f64,
    /// The channels' correlation, from −1 to 1.
    pub correlation: f64,
}

/// What a sound is made of, to a listener: see the [module](self).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Features {
    /// How long it sounds, from where it comes within 40 dB of its
    /// loudest to where it has fallen for good 60 dB below, in seconds.
    pub duration: f64,
    /// The RMS over that time, in dBFS.
    pub level_db: f64,
    /// The spectrum over time, row by row: [`BANDS`] values for each of
    /// [`FRAMES`] moments, in dB at or below 0 (the loudest cell) down to
    /// −80.
    pub spectrum: Vec<f64>,
    /// The level at [`FRAMES`] moments, in dB at or below 0 (the loudest)
    /// down to −60.
    pub envelope: Vec<f64>,
    /// The fundamental of the steady part, in hertz, or `None` if it has
    /// no clear pitch.
    pub pitch_hz: Option<f64>,
    /// A wobble of the level, if it has one.
    pub amplitude_modulation: Option<Modulation>,
    /// A wobble of the spectral centroid, if it has one.
    pub brightness_modulation: Option<Modulation>,
    /// The stereo image; a mono render reads as the same in both channels.
    pub stereo: Stereo,
}

const MONO: Stereo = Stereo {
    width: 0.0,
    correlation: 1.0,
};

/// A Hann-windowed power spectrum of `size` samples around a point.
struct Frames {
    fft: Arc<dyn RealToComplex<f64>>,
    window: Vec<f64>,
}

impl Frames {
    fn new(size: usize) -> Self {
        Self {
            fft: RealFftPlanner::<f64>::new().plan_fft_forward(size),
            window: (0..size)
                .map(|i| 0.5 - 0.5 * (TAU * i as f64 / size as f64).cos())
                .collect(),
        }
    }

    /// The power in each bin from DC to Nyquist of the samples centred on
    /// `centre` (zero where the signal isn't).
    fn power(&self, signal: &[f32], centre: usize) -> Vec<f64> {
        let size = self.window.len();
        let start = centre as i64 - (size / 2) as i64;
        let mut input = self.fft.make_input_vec();
        for (i, (slot, w)) in input.iter_mut().zip(&self.window).enumerate() {
            let at = start + i as i64;
            *slot = usize::try_from(at)
                .ok()
                .and_then(|at| signal.get(at))
                .map_or(0.0, |&x| f64::from(x) * w);
        }
        let mut spectrum = self.fft.make_output_vec();
        // The input and output are the plan's own sizes, so this can't fail.
        let _ = self.fft.process(&mut input, &mut spectrum);
        spectrum.iter().map(|c| c.norm_sqr()).collect()
    }
}

/// The features of `output`, or `None` if it's silent.
#[must_use]
pub fn extract(output: &Output) -> Option<Features> {
    let rate = output.sample_rate;
    let mono = output.mono();
    let envelope = measure::envelope(&mono, rate)?;
    let at = |seconds: f64| (seconds * rate) as usize;
    let (from, to) = (at(envelope.onset), at(envelope.end).min(mono.len()));
    let active = &mono[from.min(to)..to];
    let duration = envelope.end - envelope.onset;
    let moment =
        |i: usize| from + (((i as f64 + 0.5) / FRAMES as f64) * (to - from) as f64) as usize;

    let frames = Frames::new(FFT);
    let mut spectrum: Vec<f64> = (0..FRAMES)
        .flat_map(|i| band_powers(&frames.power(&mono, moment(i)), rate))
        .map(|p| 10.0 * p.max(1e-30).log10())
        .collect();
    let loudest = spectrum.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    for cell in &mut spectrum {
        *cell = (*cell - loudest).max(-SPECTRUM_RANGE_DB);
    }

    let window = ((to - from) / FRAMES).max(1);
    let mut levels: Vec<f64> = (0..FRAMES)
        .map(|i| {
            let start = (from + i * window).min(to);
            measure::db(measure::rms(&mono[start..(start + window).min(to)]))
        })
        .collect();
    let peak = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    for level in &mut levels {
        *level = (*level - peak).max(-ENVELOPE_RANGE_DB);
    }

    // The steady part, as the agent's perception reads it: from the end of
    // the attack, for up to a second.
    let steady = at(envelope.peak)..at(envelope.end.min(envelope.peak + 1.0)).min(mono.len());
    let pitch_hz = (steady.len() > 2_048)
        .then(|| measure::pitch(&mono[steady], rate))
        .flatten();

    let hop = ((TRACK_HOP * rate) as usize).max(1);
    let count = active.len() / hop;
    let track_rate = rate / hop as f64;
    let amplitude: Vec<f64> = (0..count)
        .map(|i| {
            let centre = from + i * hop + hop / 2;
            let half = (TRACK_WINDOW * rate / 2.0) as usize;
            measure::db(measure::rms(
                &mono[centre.saturating_sub(half)..(centre + half).min(to)],
            ))
        })
        .collect();
    let amplitude_modulation = modulation(&amplitude, track_rate, MIN_AMPLITUDE_DEPTH);
    let brightness_modulation = brightness(&mono, &amplitude, from, hop, rate)
        .and_then(|track| modulation(&track, track_rate, MIN_BRIGHTNESS_DEPTH));

    Some(Features {
        duration,
        level_db: measure::db(measure::rms(active)),
        spectrum,
        envelope: levels,
        pitch_hz,
        amplitude_modulation,
        brightness_modulation,
        stereo: stereo(output).unwrap_or(MONO),
    })
}

/// The mean power in each of [`BANDS`] log-spaced bands, from a power
/// spectrum of [`FFT`] samples. A band narrower than a bin reads the bin
/// nearest its middle; a band above Nyquist reads nothing.
fn band_powers(power: &[f64], rate: f64) -> Vec<f64> {
    let bin_hz = rate / FFT as f64;
    let ratio = (HIGH_HZ / LOW_HZ).powf(1.0 / BANDS as f64);
    (0..BANDS)
        .map(|b| {
            let (low, high) = (
                LOW_HZ * ratio.powi(b as i32),
                LOW_HZ * ratio.powi(b as i32 + 1),
            );
            if low >= rate / 2.0 {
                return 0.0;
            }
            let (first, last) = (
                (low / bin_hz).ceil() as usize,
                (high / bin_hz).ceil() as usize,
            );
            let last = last.min(power.len());
            if first < last {
                power[first..last].iter().sum::<f64>() / (last - first) as f64
            } else {
                power[(((low * high).sqrt() / bin_hz).round() as usize).min(power.len() - 1)]
            }
        })
        .collect()
}

/// The centroid, in octaves above 1 Hz, every [`TRACK_HOP`] where the
/// sound is within [`BRIGHTNESS_RANGE_DB`] of its loudest; the mean where it
/// isn't. `None` if nothing is loud enough.
fn brightness(
    mono: &[f32],
    amplitude: &[f64],
    from: usize,
    hop: usize,
    rate: f64,
) -> Option<Vec<f64>> {
    let loudest = amplitude.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let frames = Frames::new(TRACK_FFT);
    let bin_hz = rate / TRACK_FFT as f64;
    let mut track: Vec<Option<f64>> = amplitude
        .iter()
        .enumerate()
        .map(|(i, &level)| {
            if level < loudest - BRIGHTNESS_RANGE_DB {
                return None;
            }
            let power = frames.power(mono, from + i * hop + hop / 2);
            let total: f64 = power.iter().skip(1).sum();
            let weighted: f64 = power
                .iter()
                .enumerate()
                .skip(1)
                .map(|(k, p)| k as f64 * bin_hz * p)
                .sum();
            (total > 0.0).then(|| (weighted / total).log2())
        })
        .collect();
    let valid: Vec<f64> = track.iter().flatten().copied().collect();
    let mean = valid.iter().sum::<f64>() / valid.len().max(1) as f64;
    (!valid.is_empty()).then(|| track.iter_mut().map(|v| v.unwrap_or(mean)).collect())
}

/// The strongest wobble in `track` (sampled `rate` times a second) from
/// [`MIN_RATE_HZ`] to [`MAX_RATE_HZ`], if its peak from the mean is at least
/// `min_depth`.
fn modulation(track: &[f64], rate: f64, min_depth: f64) -> Option<Modulation> {
    let n = track.len();
    if (n as f64) < MIN_MODULATED * rate {
        return None;
    }
    let half = (TREND * rate / 2.0) as usize;
    let weights: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * (TAU * (i as f64 + 0.5) / n as f64).cos())
        .collect();
    let steady: Vec<f64> = (0..n)
        .map(|i| {
            let around = &track[i.saturating_sub(half)..(i + half + 1).min(n)];
            (track[i] - around.iter().sum::<f64>() / around.len() as f64) * weights[i]
        })
        .collect();
    let scale = 2.0 / weights.iter().sum::<f64>();
    let strength = |hz: f64| {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, x) in steady.iter().enumerate() {
            let phase = TAU * hz * i as f64 / rate;
            re += x * phase.cos();
            im -= x * phase.sin();
        }
        scale * re.hypot(im)
    };
    let steps = ((MAX_RATE_HZ - MIN_RATE_HZ) / 0.25) as usize;
    let (rate_hz, depth) = (0..=steps)
        .map(|s| MIN_RATE_HZ + s as f64 * 0.25)
        .map(|hz| (hz, strength(hz)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    (depth >= min_depth).then_some(Modulation { rate_hz, depth })
}

/// How wide `output`'s image is, or `None` for one channel or silence.
#[must_use]
pub fn stereo(output: &Output) -> Option<Stereo> {
    let [left, right, ..] = output.channels.as_slice() else {
        return None;
    };
    let (mut mid, mut side, mut ll, mut rr, mut lr) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (&l, &r) in left.iter().zip(right) {
        let (l, r) = (f64::from(l), f64::from(r));
        mid += (l + r) * (l + r) / 4.0;
        side += (l - r) * (l - r) / 4.0;
        (ll, rr, lr) = (ll + l * l, rr + r * r, lr + l * r);
    }
    let total = mid + side;
    let norm = (ll * rr).sqrt();
    // A silent channel correlates with nothing.
    (total > 0.0).then(|| Stereo {
        width: side / total,
        correlation: if norm > 0.0 { lr / norm } else { 0.0 },
    })
}

/// How different two sounds are: 0 for the same features, larger the
/// further apart they sound. A weighted sum of a term for each feature,
/// each about 1 for a large difference, with no upper bound; it ranks
/// sounds by their closeness to one reference, and its scale is for
/// comparing distances to the same reference, not an absolute score.
#[must_use]
pub fn distance(a: &Features, b: &Features) -> f64 {
    W_SPECTRUM * rms_difference(&a.spectrum, &b.spectrum) / 12.0
        + W_ENVELOPE * rms_difference(&a.envelope, &b.envelope) / 12.0
        + W_DURATION * (a.duration / b.duration).log2().abs().min(2.0)
        + W_PITCH
            * match (a.pitch_hz, b.pitch_hz) {
                (Some(x), Some(y)) => (x / y).log2().abs().min(1.0),
                (None, None) => 0.0,
                _ => 1.0,
            }
        + W_MODULATION
            * (modulation_distance(a.amplitude_modulation, b.amplitude_modulation, 6.0)
                + modulation_distance(a.brightness_modulation, b.brightness_modulation, 1.0))
        + W_STEREO
            * ((a.stereo.width - b.stereo.width).abs()
                + (a.stereo.correlation - b.stereo.correlation).abs() / 2.0)
        + W_LEVEL * ((a.level_db - b.level_db).abs() / 12.0).min(1.0)
}

fn rms_difference(a: &[f64], b: &[f64]) -> f64 {
    let sum: f64 = a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum();
    (sum / a.len().max(1) as f64).sqrt()
}

/// From 0 to 1: a wobble against none counts for its depth over
/// `full_depth`; two wobbles, for half their rates' octaves apart and
/// half their depths' difference over `full_depth`.
fn modulation_distance(a: Option<Modulation>, b: Option<Modulation>, full_depth: f64) -> f64 {
    match (a, b) {
        (None, None) => 0.0,
        (Some(m), None) | (None, Some(m)) => (m.depth / full_depth).min(1.0),
        (Some(a), Some(b)) => {
            0.5 * (a.rate_hz / b.rate_hz).log2().abs().min(1.0)
                + 0.5 * ((a.depth - b.depth).abs() / full_depth).min(1.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{noise, scaled, sine};

    const RATE: f64 = 48_000.0;

    fn seconds(s: f64) -> usize {
        (s * RATE) as usize
    }

    /// `f(t)` for every frame of `length` seconds.
    fn made(length: f64, f: impl Fn(f64) -> f64) -> Vec<f32> {
        (0..seconds(length))
            .map(|i| f(i as f64 / RATE) as f32)
            .collect()
    }

    /// A sawtooth-like sound: `harmonics` partials falling as 1/n, dying
    /// away at `decay` per second.
    fn saw(hz: f64, harmonics: usize, decay: f64) -> Vec<f32> {
        made(1.5, |t| {
            let sum: f64 = (1..=harmonics)
                .map(|n| (TAU * hz * n as f64 * t).sin() / n as f64)
                .sum();
            0.3 * sum * (-decay * t).exp()
        })
    }

    fn mono(signal: Vec<f32>) -> Features {
        extract(&Output::new(RATE, vec![signal])).unwrap()
    }

    #[test]
    fn silence_has_no_features() {
        assert!(extract(&Output::new(RATE, vec![vec![0.0; 4_800]])).is_none());
    }

    #[test]
    fn a_sound_is_nearest_itself_and_the_distance_is_symmetric() {
        let a = mono(saw(220.0, 20, 3.0));
        let b = mono(saw(330.0, 6, 1.0));
        assert_eq!(distance(&a, &a), 0.0);
        assert!((distance(&a, &b) - distance(&b, &a)).abs() < 1e-12);
        assert!(distance(&a, &b) > 0.1);
    }

    #[test]
    fn similar_sounds_rank_closer_than_dissimilar_ones() {
        let reference = mono(saw(220.0, 20, 3.0));
        let similar = [
            // A little duller and quieter, a little slower to die away.
            mono(saw(220.0, 16, 2.6)),
            mono(scaled(&saw(220.0, 20, 3.0), 0.5)),
            // A fifth of a semitone sharp.
            mono(saw(222.5, 20, 3.0)),
        ];
        let dissimilar = [
            mono(sine(1_760.0, RATE, seconds(1.5), 0.3)),
            mono(
                noise(3, seconds(1.5), 0.3)
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| x * (-3.0 * i as f64 / RATE).exp() as f32)
                    .collect(),
            ),
            // A dark, steady, low sine.
            mono(sine(110.0, RATE, seconds(1.5), 0.3)),
        ];
        let worst_similar = similar
            .iter()
            .map(|s| distance(&reference, s))
            .fold(0.0, f64::max);
        let best_dissimilar = dissimilar
            .iter()
            .map(|s| distance(&reference, s))
            .fold(f64::INFINITY, f64::min);
        assert!(
            worst_similar < best_dissimilar,
            "{worst_similar} against {best_dissimilar}"
        );
    }

    #[test]
    fn nearby_pitches_rank_closer_than_far_ones() {
        let tone = |hz: f64| mono(sine(hz, RATE, seconds(1.0), 0.3));
        let (a, near, far) = (tone(440.0), tone(466.0), tone(1_760.0));
        assert_eq!(a.pitch_hz.map(f64::round), Some(440.0));
        assert!(distance(&a, &near) < distance(&a, &far));
    }

    #[test]
    fn a_tremolo_is_read_in_rate_and_depth() {
        let tremolo = mono(made(2.0, |t| {
            0.3 * (1.0 + 0.5 * (TAU * 5.0 * t).sin()) * (TAU * 440.0 * t).sin()
        }));
        let m = tremolo.amplitude_modulation.expect("a tremolo");
        // Swings between 20·log10(1.5) and 20·log10(0.5) dB about its mean.
        assert!((m.rate_hz - 5.0).abs() < 0.75, "{m:?}");
        assert!((2.0..5.0).contains(&m.depth), "{m:?}");
        let steady = mono(sine(440.0, RATE, seconds(2.0), 0.3));
        assert!(steady.amplitude_modulation.is_none());
        assert!(steady.brightness_modulation.is_none());
        // The tremolo is nearer its steady twin than a faster, deeper one.
        let faster = mono(made(2.0, |t| {
            0.3 * (1.0 + 0.9 * (TAU * 12.0 * t).sin()) * (TAU * 440.0 * t).sin()
        }));
        assert!(distance(&tremolo, &steady) < distance(&steady, &faster));
        assert!(distance(&tremolo, &faster) > 0.0);
    }

    #[test]
    fn a_wobbling_spectrum_is_read_as_brightness_modulation() {
        let wobble = mono(made(2.0, |t| {
            let s = (TAU * 4.0 * t).sin();
            0.2 * ((1.0 + s) * (TAU * 500.0 * t).sin() + (1.0 - s) * (TAU * 4_000.0 * t).sin())
        }));
        let m = wobble.brightness_modulation.expect("a wobbling centroid");
        assert!((m.rate_hz - 4.0).abs() < 0.75, "{m:?}");
        assert!(m.depth > 0.5, "{m:?}");
    }

    #[test]
    fn stereo_width_is_read_and_a_mono_render_matches_identical_channels() {
        let s = saw(220.0, 10, 2.0);
        // The same in the middle, a tone of its own in each side: a wide
        // sound that still adds up to `s` in mono.
        let side = sine(330.0, RATE, s.len(), 0.3);
        let left: Vec<f32> = s.iter().zip(&side).map(|(m, x)| m + x).collect();
        let right: Vec<f32> = s.iter().zip(&side).map(|(m, x)| m - x).collect();
        let one = extract(&Output::new(RATE, vec![s.clone()])).unwrap();
        let same = extract(&Output::new(RATE, vec![s.clone(), s.clone()])).unwrap();
        let wide = extract(&Output::new(RATE, vec![left, right])).unwrap();
        assert_eq!(one.stereo, same.stereo);
        assert_eq!(same.stereo.width, 0.0);
        assert!(
            (0.05..0.95).contains(&wide.stereo.width),
            "{:?}",
            wide.stereo
        );
        assert!(wide.stereo.correlation < 0.99, "{:?}", wide.stereo);
        assert!(distance(&one, &same) < 1e-9);
        assert!(distance(&one, &wide) > 0.01);
    }

    #[test]
    fn features_round_trip_through_json() {
        let features = mono(saw(220.0, 8, 2.0));
        let json = serde_json::to_string(&features).unwrap();
        // JSON's decimal text may differ from the float in its last digit.
        let back = serde_json::from_str::<Features>(&json).unwrap();
        assert!(distance(&back, &features) < 1e-9);
        assert_eq!(
            back.pitch_hz.map(f64::round),
            features.pitch_hz.map(f64::round)
        );
        assert_eq!(features.spectrum.len(), FRAMES * BANDS);
        assert_eq!(features.envelope.len(), FRAMES);
    }
}
