//! The chaos generators: the Lorenz and Rössler attractors, integrated with
//! Heun's method (second-order Runge–Kutta) and held inside a box, so they
//! stay finite and bounded at every rate.
//!
//! These are first laws (engine-core R95; the laws are matched against the
//! reference in R7): the textbook parameters (Lorenz σ = 10, ρ = 28,
//! β = 8/3; Rössler a = b = 0.2, c = 5.7), time scaled so one LFO cycle is
//! about one turn of the attractor, and the X coordinate mapped to the LFO's
//! 0 to 1 range.

use super::Lane;

/// One attractor's constants.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Law {
    /// The attractor's time per LFO cycle: about one turn (the Lorenz
    /// attractor's Z peaks about 1.32 times per unit of time; the Rössler
    /// attractor's X crosses zero upwards about every 5.85).
    pub per_cycle: f64,
    /// The longest integration step it takes; longer steps are split.
    pub max_step: f64,
    /// Where it starts: a point on the attractor.
    pub start: [f32; 3],
    /// The box it's held in.
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    /// X maps to the output as `(x − out_lo) × out_scale`, clamped to 0 to 1.
    pub out_lo: f32,
    pub out_scale: f32,
}

pub(crate) const LORENZ: Law = Law {
    per_cycle: 1.0 / 1.32,
    max_step: 0.01,
    start: [-13.725_827, -6.877_337, 39.917_21],
    lo: [-40.0, -50.0, 0.0],
    hi: [40.0, 50.0, 80.0],
    // X stays within about ±19.
    out_lo: -20.0,
    out_scale: 1.0 / 40.0,
};

pub(crate) const ROSSLER: Law = Law {
    per_cycle: 5.85,
    max_step: 0.05,
    start: [-7.057_705, 0.774_365, 0.015_809],
    lo: [-25.0, -25.0, 0.0],
    hi: [25.0, 25.0, 60.0],
    // X stays within about −9.1 to 11.4.
    out_lo: -10.0,
    out_scale: 1.0 / 22.0,
};

/// Lorenz (`true`) or Rössler (`false`).
pub(crate) const fn law(lorenz: bool) -> &'static Law {
    if lorenz { &LORENZ } else { &ROSSLER }
}

/// The attractor's derivative at `s`.
#[inline(always)]
fn derivative<L: Lane>(lorenz: bool, s: [L; 3]) -> [L; 3] {
    let [x, y, z] = s;
    if lorenz {
        [
            L::splat(10.0) * (y - x),
            x * (L::splat(28.0) - z) - y,
            x * y - L::splat(8.0 / 3.0) * z,
        ]
    } else {
        [
            L::splat(0.0) - y - z,
            x + L::splat(0.2) * y,
            L::splat(0.2) + z * (x - L::splat(5.7)),
        ]
    }
}

/// `steps` Heun steps of `h` each from `s`, then the box. The scalar and
/// SIMD paths both run this, so they agree to the bit.
#[inline(always)]
pub(crate) fn advance<L: Lane>(lorenz: bool, mut s: [L; 3], h: L, steps: u32) -> [L; 3] {
    let law = law(lorenz);
    let half = h * L::splat(0.5);
    for _ in 0..steps {
        let k1 = derivative(lorenz, s);
        let s1 = [s[0] + h * k1[0], s[1] + h * k1[1], s[2] + h * k1[2]];
        let k2 = derivative(lorenz, s1);
        s = [
            s[0] + half * (k1[0] + k2[0]),
            s[1] + half * (k1[1] + k2[1]),
            s[2] + half * (k1[2] + k2[2]),
        ];
    }
    let held = |i: usize| s[i].max(L::splat(law.lo[i])).min(L::splat(law.hi[i]));
    [held(0), held(1), held(2)]
}

/// The output for state X `x`, 0 to 1.
#[inline(always)]
pub(crate) fn output<L: Lane>(lorenz: bool, x: L) -> L {
    let law = law(lorenz);
    ((x - L::splat(law.out_lo)) * L::splat(law.out_scale))
        .max(L::splat(0.0))
        .min(L::splat(1.0))
}

/// The state `cycles` LFO cycles on from the start point: where a restarted
/// generator begins for a Phase of `cycles` × 360°.
pub(crate) fn start_at(lorenz: bool, cycles: f64) -> [f32; 3] {
    let law = law(lorenz);
    let time = cycles * law.per_cycle;
    let steps = (time / law.max_step).ceil().max(0.0);
    if steps < 1.0 {
        return law.start;
    }
    let h = (time / steps) as f32;
    advance::<f32>(lorenz, law.start, h, steps as u32)
}
