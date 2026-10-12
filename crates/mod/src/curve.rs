//! Curves: a route's source and aux curves, and the velocity and key
//! curves drawn as shapes.
//!
//! A curve maps a unipolar value (0 to 1) to another. A bipolar source is
//! mapped to 0 to 1 first ((x + 1) / 2), curved, and mapped back by the
//! route's polarity.
//!
//! - [`Curve::Bend`] is the curve control: at 50 % it is linear and costs
//!   nothing (engine-core R107); either side it bends by the envelope's
//!   curve family ([`lspk_dsp::envelope::curve::shape`]), above 50 % rising
//!   quickly first. This first law will be matched against measurements.
//! - [`Curve::Drawn`] is a custom curve drawn as an LFO [`Shape`], with
//!   Rise and Fall slew: the curve's output moves up by at most full scale
//!   over Rise seconds, and down over Fall seconds (0 is instant).
//!
//! A compiled route reads a curve from a table of [`LUT_SIZE`] + 1 points,
//! interpolated linearly: within 1e-4 of the bend law at its steepest
//! (the tests bound it), closer for gentler bends and a drawn shape's
//! smooth segments; a drawn step lands within one table step of its point.

use lspk_dsp::envelope::curve::shape as bend_shape;
use lspk_dsp::lfo::{Point, Shape};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The table steps a compiled curve has.
pub const LUT_SIZE: usize = 512;

/// A curve's position at which it's linear.
pub const LINEAR: f32 = 0.5;

/// A route's source or aux curve.
#[derive(Clone, Debug, PartialEq)]
pub enum Curve {
    /// The curve control, 0 to 1; 0.5 is linear.
    Bend(f32),
    /// A drawn curve with slew.
    Drawn(Box<DrawnCurve>),
}

impl Default for Curve {
    fn default() -> Self {
        Self::LINEAR
    }
}

/// A custom curve: a drawn shape, and Rise and Fall slew in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnCurve {
    /// The shape, 0 to 1 on both axes.
    pub shape: Shape,
    /// Seconds the output takes to rise full scale; 0 is instant.
    pub rise: f32,
    /// Seconds the output takes to fall full scale; 0 is instant.
    pub fall: f32,
}

impl Curve {
    /// The linear curve, which is bypassed.
    pub const LINEAR: Self = Self::Bend(LINEAR);

    /// A drawn curve through `shape`, without slew.
    #[must_use]
    pub fn drawn(shape: Shape) -> Self {
        Self::Drawn(Box::new(DrawnCurve {
            shape,
            rise: 0.0,
            fall: 0.0,
        }))
    }

    /// Whether it changes nothing (engine-core R107).
    #[must_use]
    pub fn is_linear(&self) -> bool {
        matches!(self, Self::Bend(c) if *c == LINEAR)
    }

    /// Its value at `x` (0 to 1), exactly, as an editor draws it.
    #[must_use]
    pub fn value(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Self::Bend(c) if *c == LINEAR => x,
            Self::Bend(c) => bend_shape(c.clamp(0.0, 1.0), f64::from(x)) as f32,
            Self::Drawn(d) => d.shape.value(x),
        }
    }

    /// Its Rise and Fall, in seconds; 0 for a bend.
    #[must_use]
    pub fn slew(&self) -> (f32, f32) {
        match self {
            Self::Bend(_) => (0.0, 0.0),
            Self::Drawn(d) => (d.rise.max(0.0), d.fall.max(0.0)),
        }
    }

    /// Appends its table, [`LUT_SIZE`] + 1 values, to `table`.
    pub(crate) fn tabulate(&self, table: &mut Vec<f32>) {
        table.extend((0..=LUT_SIZE).map(|i| self.value(i as f32 / LUT_SIZE as f32)));
    }
}

/// The diagonal shape, which maps every value to itself: the init velocity
/// and key curves (engine-core R99).
#[must_use]
pub fn diagonal() -> Shape {
    Shape::new(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)])
}

/// The velocity and key curves and their options (engine-core section 11).
#[derive(Clone, Debug, PartialEq)]
pub struct NoteCurves {
    /// Maps note-on velocity (0 to 1) to the velocity source.
    pub velocity: Shape,
    /// Maps the note number ÷ 127 to the key source.
    pub key: Shape,
    /// With a legato note, the curves' outputs move to the new note's
    /// value over the glide time (engine-core R100).
    pub glide: bool,
    /// Whether a legato note updates the velocity source (engine-core
    /// R101). Off until the reference's default is observed.
    pub velocity_legato: bool,
}

impl Default for NoteCurves {
    fn default() -> Self {
        Self {
            velocity: diagonal(),
            key: diagonal(),
            glide: false,
            velocity_legato: false,
        }
    }
}

/// Reads a value from a table [`Curve::tabulate`] made.
#[inline(always)]
pub(crate) fn lookup(table: &[f32], x: f32) -> f32 {
    let pos = x.clamp(0.0, 1.0) * LUT_SIZE as f32;
    let i = (pos as usize).min(LUT_SIZE - 1);
    let frac = pos - i as f32;
    let a = table[i];
    a + (table[i + 1] - a) * frac
}

/// A curve as commands write it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CurveSpec {
    /// The curve control, 0 to 1; 0.5 is linear.
    Bend(f32),
    /// A drawn curve: points in order of x, and Rise and Fall in seconds.
    Drawn {
        /// The points, each x and y from 0 to 1, and the bend of the
        /// segment after it from −1 to 1 (0 straight).
        points: Vec<CurvePoint>,
        /// Seconds to rise full scale; 0 is instant.
        #[serde(default)]
        rise: f32,
        /// Seconds to fall full scale; 0 is instant.
        #[serde(default)]
        fall: f32,
    },
}

/// One point of a drawn curve, as commands write it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CurvePoint {
    /// Where, 0 to 1.
    pub x: f32,
    /// The value there, 0 to 1.
    pub y: f32,
    /// The bend of the segment after it, −1 to 1; 0 is straight.
    #[serde(default)]
    pub curve: f32,
}

impl From<&CurveSpec> for Curve {
    fn from(spec: &CurveSpec) -> Self {
        match spec {
            CurveSpec::Bend(c) => Self::Bend(c.clamp(0.0, 1.0)),
            CurveSpec::Drawn { points, rise, fall } => {
                let points: Vec<Point> = points
                    .iter()
                    .map(|p| Point::new(p.x, p.y).curve(p.curve))
                    .collect();
                Self::Drawn(Box::new(DrawnCurve {
                    shape: Shape::new(&points),
                    rise: rise.max(0.0),
                    fall: fall.max(0.0),
                }))
            }
        }
    }
}

impl From<&Curve> for CurveSpec {
    fn from(curve: &Curve) -> Self {
        match curve {
            Curve::Bend(c) => Self::Bend(*c),
            Curve::Drawn(d) => Self::Drawn {
                points: d
                    .shape
                    .points()
                    .iter()
                    .map(|p| CurvePoint {
                        x: p.x,
                        y: p.y,
                        curve: p.curve,
                    })
                    .collect(),
                rise: d.rise,
                fall: d.fall,
            },
        }
    }
}
