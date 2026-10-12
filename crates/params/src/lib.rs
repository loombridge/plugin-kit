//! Parameters: what each of a device's parameters is, and how its value
//! moves.
//!
//! - [`descriptor`]: parameter descriptors, with their range, unit, taper,
//!   default, value text and parsing, and the rules every descriptor keeps
//!   (G1: every gain reaches −∞ dB; G2: every parameter can be automated).
//! - [`smooth`]: parameter smoothing, so a change never steps. It allocates
//!   nothing, locks nothing, and never blocks, so it's safe on an audio
//!   thread.

pub mod descriptor;
pub mod smooth;

pub use descriptor::{
    DEFAULT_FLOOR_DB, MINUS_INFINITY_DB, ParamDescriptor, ParamId, ParamKind, ParamUnit, Taper,
    db_to_gain, gain_to_db,
};
pub use smooth::LinearSmoother;
