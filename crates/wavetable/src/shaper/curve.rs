//! The remap curve the Remap shapers map the phase through: a drawn shape
//! (the kit's LFO shape model, so the editor that draws an LFO draws this
//! too), rendered to a table the audio thread reads.

use lspk_dsp::lfo::{Point, Shape};

/// How many segments the rendered table has: 1,025 values from x = 0 to 1.
pub const CURVE_RESOLUTION: usize = 1024;

/// The steepest slope a curve is read with when the oscillator picks its
/// mipmap level: a near-vertical segment beyond it aliases rather than
/// dulling the whole cycle.
pub const MAX_CURVE_STRETCH: f32 = 16.0;

/// The remap curve: a drawn shape from x = 0 to 1, y = 0 to 1 (engine-core
/// R25), identity by default, and the table it's played from.
///
/// Changing the shape re-renders the table in place, so it never allocates
/// and can be done on the audio thread.
#[derive(Clone, Debug, PartialEq)]
pub struct RemapCurve {
    shape: Shape,
    table: [f32; CURVE_RESOLUTION + 1],
    steepest: f32,
}

impl Default for RemapCurve {
    /// The identity, y = x (engine-core section 3, Remap curve default).
    fn default() -> Self {
        Self::new(&Shape::new(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)]))
    }
}

impl RemapCurve {
    /// The curve `shape` draws.
    #[must_use]
    pub fn new(shape: &Shape) -> Self {
        let mut curve = Self {
            shape: shape.clone(),
            table: [0.0; CURVE_RESOLUTION + 1],
            steepest: 1.0,
        };
        curve.render();
        curve
    }

    /// The drawn shape.
    #[must_use]
    pub fn shape(&self) -> &Shape {
        &self.shape
    }

    /// Draws `shape` instead. Allocation-free.
    pub fn set_shape(&mut self, shape: &Shape) {
        self.shape.clone_from(shape);
        self.render();
    }

    /// The curve's value at `x` (0 to 1), as the oscillator reads it:
    /// linear between the table's values.
    #[must_use]
    pub fn value(&self, x: f32) -> f32 {
        let at = x.clamp(0.0, 1.0) * CURVE_RESOLUTION as f32;
        let i = (at as usize).min(CURVE_RESOLUTION - 1);
        let t = at - i as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * t
    }

    /// The rendered table, `CURVE_RESOLUTION + 1` values.
    pub(crate) fn table(&self) -> &[f32] {
        &self.table
    }

    /// The steepest slope of the rendered curve, at least 1 and at most
    /// [`MAX_CURVE_STRETCH`].
    pub(crate) fn steepest(&self) -> f32 {
        self.steepest
    }

    fn render(&mut self) {
        for (i, value) in self.table.iter_mut().enumerate() {
            *value = self.shape.value(i as f32 / CURVE_RESOLUTION as f32);
        }
        let steepest = self
            .table
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0f32, f32::max)
            * CURVE_RESOLUTION as f32;
        self.steepest = steepest.clamp(1.0, MAX_CURVE_STRETCH);
    }
}
