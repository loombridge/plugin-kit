//! The pole-zero filter's design: four states of eight points each, morphed
//! by X and Y, and an overall gain. This is the data an editor draws and a
//! preset stores; [`FilterBank::set_pole_zero`](super::FilterBank::set_pole_zero)
//! turns it into what the kernel runs.
//!
//! # Points
//!
//! Each point is one second-order section: a pair of poles at its
//! frequency and Q, and zeros set by its kind. A section is a linear
//! trapezoidal state-variable stage whose high-pass, band-pass and low-pass
//! outputs are mixed (Simper, *Linear Trap Integrated SVF*, 2013), so its
//! poles depend only on frequency and damping (1/Q), and its kind only on
//! the mix. With `s` normalised to the point's frequency, `k` its damping
//! and `A` = 10^(gain / 40):
//!
//! | [`PointKind`] | Response | Damping |
//! |---|---|---|
//! | Low-pass | 1 / (s² + k s + 1) | 1/Q |
//! | High-pass | s² / (s² + k s + 1) | 1/Q |
//! | Band-pass | k s / (s² + k s + 1), unity at the peak | 1/Q |
//! | Notch | (s² + 1) / (s² + k s + 1) | 1/Q |
//! | All-pass | (s² − k s + 1) / (s² + k s + 1) | 1/Q |
//! | Bypass | 1 | 1/Q |
//! | Bell | (s² + k A² s + 1) / (s² + k s + 1), A² at the centre | 1/(Q A) |
//! | Free | (s²/r² + k_z s/r + 1) / (M (s² + k s + 1)), zeros at r = zero frequency / frequency, k_z = 1/zero Q, M = max(1, 1/r²) so neither end rises above unity | 1/Q |
//!
//! The eight points run in series, point 1 first, then the overall gain.
//!
//! # Morphing
//!
//! States 1 to 4 sit at the corners of the X–Y square: state 1 at (0, 0),
//! state 2 at (1, 0), state 3 at (0, 1) and state 4 at (1, 1). For each point
//! the filter blends, bilinearly in X and Y, five numbers taken from the
//! four states: the frequency in octaves, the damping, and the weights of
//! the high-pass, band-pass (unity-peak) and low-pass outputs. Never the
//! filter coefficients themselves.
//!
//! - Frequencies blend in octaves, so a morph glides evenly in pitch.
//! - Damping blends linearly. Every state's damping is positive, so every
//!   blend of them is too: the poles stay in the left half-plane wherever
//!   X and Y are, and the filter is stable along any path.
//! - The output weights only move the zeros, which can't make the filter
//!   unstable; a point that's a low-pass in one state and a bypass in
//!   another becomes a shelf between them.
//!
//! The kernel recomputes every section for every sample from that sample's
//! X, Y and cutoff, and trapezoidal state-variable stages stay well behaved
//! when their frequency and damping move every sample, so X and Y can be
//! modulated at audio rate without zipper noise. Cutoff transposes the
//! whole design: at the cutoff control's middle (about 425 Hz, pitch
//! [`NEUTRAL_PITCH`]) every point sits at its drawn frequency.

use serde::{Deserialize, Serialize};

/// Points in each state.
pub const POINTS: usize = 8;
/// States, at the corners of the X–Y square.
pub const STATES: usize = 4;
/// The lowest and highest point frequency, in hertz.
pub const FREQUENCY_RANGE: (f32, f32) = (8.0, 22_050.0);
/// The lowest and highest Q.
pub const Q_RANGE: (f32, f32) = (0.1, 40.0);
/// A bell's gain range, in decibels: the editor's ±18 dB.
pub const POINT_GAIN_DB: f32 = 18.0;
/// The overall gain's range, in decibels.
pub const GAIN_DB: f32 = 24.0;
/// The cutoff pitch at which points sit at their drawn frequencies: the
/// cutoff control's middle, about 425 Hz.
pub const NEUTRAL_PITCH: f32 = 68.4;

/// What a point does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointKind {
    /// A resonant high-pass.
    HighPass,
    /// A resonant low-pass.
    LowPass,
    /// Nothing: the point passes its input.
    #[default]
    Bypass,
    /// An all-pass: flat, with a phase turn at the frequency.
    AllPass,
    /// A band-pass, unity at its peak.
    BandPass,
    /// A notch.
    Notch,
    /// A bell, raising or lowering the band around the frequency by its gain.
    Bell,
    /// Poles at the frequency and Q, zeros at their own frequency and Q.
    Free,
}

impl PointKind {
    /// Every kind, in the editor's order.
    pub const ALL: [Self; 8] = [
        Self::HighPass,
        Self::LowPass,
        Self::Bypass,
        Self::AllPass,
        Self::BandPass,
        Self::Notch,
        Self::Bell,
        Self::Free,
    ];
}

/// One point: a second-order section.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Point {
    /// What it does.
    pub kind: PointKind,
    /// Its frequency in hertz, at the cutoff control's middle.
    pub frequency: f32,
    /// Its Q.
    pub q: f32,
    /// A bell's gain in decibels (±18).
    pub gain_db: f32,
    /// A free point's zero frequency, in hertz.
    pub zero_frequency: f32,
    /// A free point's zero Q.
    pub zero_q: f32,
}

impl Default for Point {
    /// A bypass at 1 kHz, Q 0.707.
    fn default() -> Self {
        Self::new(PointKind::Bypass, 1_000.0, std::f32::consts::FRAC_1_SQRT_2)
    }
}

impl Point {
    /// A point of `kind` at `frequency` hertz and `q`; a bell's gain 0 dB, a
    /// free point's zeros at its poles.
    #[must_use]
    pub const fn new(kind: PointKind, frequency: f32, q: f32) -> Self {
        Self {
            kind,
            frequency,
            q,
            gain_db: 0.0,
            zero_frequency: frequency,
            zero_q: q,
        }
    }

    /// A bell at `frequency` hertz and `q`, raising the band by `gain_db`.
    #[must_use]
    pub const fn bell(frequency: f32, q: f32, gain_db: f32) -> Self {
        Self {
            gain_db,
            ..Self::new(PointKind::Bell, frequency, q)
        }
    }

    /// A free point: poles at `frequency` and `q`, zeros at
    /// `zero_frequency` and `zero_q`.
    #[must_use]
    pub const fn free(frequency: f32, q: f32, zero_frequency: f32, zero_q: f32) -> Self {
        Self {
            zero_frequency,
            zero_q,
            ..Self::new(PointKind::Free, frequency, q)
        }
    }

    /// The point with every value in its range; a value that isn't a
    /// number takes the default's.
    #[must_use]
    pub fn clamped(self) -> Self {
        let d = Self::default();
        let clamp = |v: f32, (lo, hi): (f32, f32), or: f32| {
            if v.is_nan() { or } else { v.clamp(lo, hi) }
        };
        Self {
            kind: self.kind,
            frequency: clamp(self.frequency, FREQUENCY_RANGE, d.frequency),
            q: clamp(self.q, Q_RANGE, d.q),
            gain_db: clamp(self.gain_db, (-POINT_GAIN_DB, POINT_GAIN_DB), 0.0),
            zero_frequency: clamp(self.zero_frequency, FREQUENCY_RANGE, d.frequency),
            zero_q: clamp(self.zero_q, Q_RANGE, d.q),
        }
    }

    /// What the filter blends between states: [`Section`].
    #[must_use]
    pub fn section(self) -> Section {
        let p = self.clamped();
        let k = 1.0 / p.q;
        let octaves = (p.frequency / NEUTRAL_HZ).log2();
        let (k, mix) = match p.kind {
            PointKind::HighPass => (k, [1.0, 0.0, 0.0]),
            PointKind::LowPass => (k, [0.0, 0.0, 1.0]),
            PointKind::Bypass => (k, [1.0, 1.0, 1.0]),
            PointKind::AllPass => (k, [1.0, -1.0, 1.0]),
            PointKind::BandPass => (k, [0.0, 1.0, 0.0]),
            PointKind::Notch => (k, [1.0, 0.0, 1.0]),
            PointKind::Bell => {
                let a = 10f32.powf(p.gain_db / 40.0);
                (k / a, [1.0, a * a, 1.0])
            }
            PointKind::Free => {
                // Scaled so neither the low nor the high end rises above
                // unity.
                let r = p.zero_frequency / p.frequency;
                let scale = 1.0 / (1.0 / (r * r)).max(1.0);
                (k, [scale / (r * r), scale / (p.zero_q * r * k), scale])
            }
        };
        Section {
            octaves,
            damping: k,
            high: mix[0],
            band: mix[1],
            low: mix[2],
        }
    }
}

/// The frequency of [`NEUTRAL_PITCH`] at A4 = 440 Hz.
const NEUTRAL_HZ: f32 = 425.012;

/// One point's section in the form the filter blends: its frequency in
/// octaves from [`NEUTRAL_PITCH`]'s, its damping (1/Q), and the weights of
/// its high-pass, unity-peak band-pass and low-pass outputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    /// Octaves above about 425 Hz.
    pub octaves: f32,
    /// Damping, 1/Q (for a bell, 1/(Q A)).
    pub damping: f32,
    /// The high-pass output's weight.
    pub high: f32,
    /// The unity-peak band-pass output's weight.
    pub band: f32,
    /// The low-pass output's weight.
    pub low: f32,
}

impl Section {
    fn values(self) -> [f32; 5] {
        [self.octaves, self.damping, self.high, self.band, self.low]
    }

    /// Its analogue response at `hz` with the cutoff `transpose` octaves
    /// from the middle, as (real, imaginary).
    #[must_use]
    pub fn response(self, hz: f64, transpose: f64) -> (f64, f64) {
        let f = f64::from(NEUTRAL_HZ) * (f64::from(self.octaves) + transpose).exp2();
        let w = hz / f;
        let k = f64::from(self.damping);
        // (h s² + b k s + l) / (s² + k s + 1) at s = jw.
        let (h, b, l) = (
            f64::from(self.high),
            f64::from(self.band),
            f64::from(self.low),
        );
        let (nr, ni) = (l - h * w * w, b * k * w);
        let (dr, di) = (1.0 - w * w, k * w);
        let d = dr * dr + di * di;
        ((nr * dr + ni * di) / d, (ni * dr - nr * di) / d)
    }
}

/// One state: eight points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Its points, run in series, point 1 first.
    pub points: [Point; POINTS],
}

impl Default for State {
    /// Every point a bypass, spread an octave apart from 62.5 Hz to 8 kHz.
    fn default() -> Self {
        Self {
            points: std::array::from_fn(|i| {
                Point::new(
                    PointKind::Bypass,
                    62.5 * (1 << i) as f32,
                    std::f32::consts::FRAC_1_SQRT_2,
                )
            }),
        }
    }
}

/// A pole-zero filter's whole design.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PoleZeroDesign {
    /// States 1 to 4.
    pub states: [State; STATES],
    /// The overall gain in decibels (±24), after the points.
    pub gain_db: f32,
}

/// Where state `state` (0 to 3, for states 1 to 4) sits in the X–Y square.
#[must_use]
pub const fn corner(state: usize) -> (f32, f32) {
    match state {
        0 => (0.0, 0.0),
        1 => (1.0, 0.0),
        2 => (0.0, 1.0),
        _ => (1.0, 1.0),
    }
}

impl PoleZeroDesign {
    /// A design with every state `state`, so X and Y do nothing.
    #[must_use]
    pub fn uniform(state: State) -> Self {
        Self {
            states: [state; STATES],
            gain_db: 0.0,
        }
    }

    /// The overall gain as a factor.
    #[must_use]
    pub fn gain(&self) -> f32 {
        let db = if self.gain_db.is_nan() {
            0.0
        } else {
            self.gain_db.clamp(-GAIN_DB, GAIN_DB)
        };
        10f32.powf(db / 20.0)
    }

    /// Point `point`'s section at `x` and `y` (each 0 to 1), as the filter
    /// blends it.
    #[must_use]
    pub fn section(&self, point: usize, x: f32, y: f32) -> Section {
        let [c00, dx, dy, dxy] = self.bilinear(point);
        let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
        let v: [f32; 5] = std::array::from_fn(|i| c00[i] + x * dx[i] + y * dy[i] + x * y * dxy[i]);
        Section {
            octaves: v[0],
            damping: v[1],
            high: v[2],
            band: v[3],
            low: v[4],
        }
    }

    /// Point `point`'s corner values as the bilinear form's terms: the value
    /// at (0, 0), then the X, Y and X × Y coefficients.
    fn bilinear(&self, point: usize) -> [[f32; 5]; 4] {
        let v: [[f32; 5]; STATES] =
            std::array::from_fn(|s| self.states[s].points[point].section().values());
        let mut out = [[0.0; 5]; 4];
        for i in 0..5 {
            out[0][i] = v[0][i];
            out[1][i] = v[1][i] - v[0][i];
            out[2][i] = v[2][i] - v[0][i];
            out[3][i] = v[3][i] - v[2][i] - v[1][i] + v[0][i];
        }
        out
    }

    /// Whether point `point` is a bypass in every state, so it does nothing
    /// wherever X and Y are.
    #[must_use]
    pub fn is_bypassed(&self, point: usize) -> bool {
        self.states
            .iter()
            .all(|s| s.points[point].kind == PointKind::Bypass)
    }

    /// The analogue response's magnitude in decibels at `hz`, with the
    /// morph at `x` and `y` and the cutoff at the control's middle: what an
    /// editor draws.
    #[must_use]
    pub fn magnitude_db(&self, x: f32, y: f32, hz: f32) -> f32 {
        let (re, im) = self.response(x, y, f64::from(hz), 0.0);
        (20.0 * re.hypot(im).log10()) as f32
    }

    /// The analogue response at `hz` as (real, imaginary), with the morph at
    /// `x` and `y` and the cutoff `transpose` octaves from the middle.
    #[must_use]
    pub fn response(&self, x: f32, y: f32, hz: f64, transpose: f64) -> (f64, f64) {
        let mut acc = (f64::from(self.gain()), 0.0);
        for p in 0..POINTS {
            if self.is_bypassed(p) {
                continue;
            }
            let (r, i) = self.section(p, x, y).response(hz, transpose);
            acc = (acc.0 * r - acc.1 * i, acc.0 * i + acc.1 * r);
        }
        acc
    }
}

/// A design in the form the kernel runs: the points that do something,
/// each as its bilinear terms, and the gain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Table {
    /// How many of `sections` are in use.
    pub(crate) active: usize,
    /// Per active point, per blended value (octaves, damping, high, band,
    /// low): the value at (0, 0), then the X, Y and X × Y coefficients.
    pub(crate) sections: [[[f32; 4]; 5]; POINTS],
    /// The overall gain.
    pub(crate) gain: f32,
}

impl Table {
    pub(crate) fn of(design: &PoleZeroDesign) -> Self {
        let mut table = Self {
            active: 0,
            sections: [[[0.0; 4]; 5]; POINTS],
            gain: design.gain(),
        };
        for p in 0..POINTS {
            if design.is_bypassed(p) {
                continue;
            }
            let terms = design.bilinear(p);
            for (v, out) in table.sections[table.active].iter_mut().enumerate() {
                *out = [terms[0][v], terms[1][v], terms[2][v], terms[3][v]];
            }
            table.active += 1;
        }
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_neutral_frequency_is_the_neutral_pitchs() {
        let hz = 440.0 * ((f64::from(NEUTRAL_PITCH) - 69.0) / 12.0).exp2();
        assert!((hz - f64::from(NEUTRAL_HZ)).abs() < 1e-3, "{hz}");
    }

    #[test]
    fn kinds_have_their_responses_at_the_frequency() {
        let at = |p: Point| {
            let (r, i) = p.section().response(f64::from(p.frequency), 0.0);
            20.0 * r.hypot(i).log10()
        };
        let q = std::f32::consts::FRAC_1_SQRT_2;
        assert!((at(Point::new(PointKind::LowPass, 1e3, q)) + 3.01).abs() < 0.01);
        assert!((at(Point::new(PointKind::HighPass, 1e3, q)) + 3.01).abs() < 0.01);
        assert!(at(Point::new(PointKind::BandPass, 1e3, 4.0)).abs() < 1e-6);
        assert!(at(Point::new(PointKind::Notch, 1e3, 4.0)) < -100.0);
        assert!(at(Point::new(PointKind::AllPass, 1e3, 4.0)).abs() < 1e-6);
        assert!(at(Point::new(PointKind::Bypass, 1e3, 4.0)).abs() < 1e-6);
        assert!((at(Point::bell(1e3, 2.0, 12.0)) - 12.0).abs() < 1e-4);
        assert!((at(Point::bell(1e3, 2.0, -18.0)) + 18.0).abs() < 1e-4);
        // A free point with its zeros on its poles is flat.
        assert!(at(Point::free(1e3, 2.0, 1e3, 2.0)).abs() < 1e-5);
    }

    #[test]
    fn values_out_of_range_are_clamped() {
        let p = Point {
            frequency: 1e6,
            q: 0.0,
            gain_db: f32::NAN,
            ..Point::default()
        }
        .clamped();
        assert_eq!((p.frequency, p.q, p.gain_db), (22_050.0, 0.1, 0.0));
        let design = PoleZeroDesign {
            gain_db: 99.0,
            ..PoleZeroDesign::default()
        };
        assert!((design.gain() - 10f32.powf(24.0 / 20.0)).abs() < 1e-4);
    }
}
