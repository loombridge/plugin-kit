//! The laws an envelope's knobs follow, from a knob's normalised position
//! `n` (0 to 1) to the value the engine uses.
//!
//! - Delay, attack, hold, decay and release: `t = 32 s × n⁵`, so the middle
//!   of the knob is 1.00 s.
//! - Sustain, start and end levels: gain `n²`, so the middle is 0.25 (25 %,
//!   or −12 dB). An amplifier envelope shows its sustain in decibels
//!   ([`decibels`]); the others show it as a percentage of gain.

/// The longest stage time a knob reaches, in seconds.
pub const MAX_SECONDS: f32 = 32.0;

/// A time knob's value at `normalised`, in milliseconds.
#[must_use]
pub fn milliseconds(normalised: f32) -> f32 {
    1000.0 * MAX_SECONDS * super::unit(normalised).powi(5)
}

/// Where a time knob sits for `milliseconds` (clamped to 0 to 32 s).
#[must_use]
pub fn normalised_for_milliseconds(milliseconds: f32) -> f32 {
    super::unit(milliseconds / (1000.0 * MAX_SECONDS)).powf(0.2)
}

/// A level knob's gain at `normalised`.
#[must_use]
pub fn level(normalised: f32) -> f32 {
    let n = super::unit(normalised);
    n * n
}

/// Where a level knob sits for `gain` (clamped to 0 to 1).
#[must_use]
pub fn normalised_for_level(gain: f32) -> f32 {
    super::unit(gain).sqrt()
}

/// `gain` in decibels: −∞ for silence.
#[must_use]
pub fn decibels(gain: f32) -> f32 {
    20.0 * gain.max(0.0).log10()
}
