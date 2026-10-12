//! The modulation engine: a 64-slot matrix of routes from sources to
//! destinations, with curves, bipolar and unipolar amounts, auxiliary
//! sources and output scalers, evaluated per voice and global,
//! sample-accurately, at audio rate only where a destination needs it.
//!
//! | Module | What it holds |
//! |---|---|
//! | [`source`] | The source registry a plug-in fills: envelopes, LFOs, the velocity and key curves, macros, MIDI and MPE, note-on randoms and alternates, audio |
//! | [`dest`] | The destination registry: parameters by ID, LFO point buses, macros and slot amounts that feed back |
//! | [`matrix`] | The matrix as data, its edits, and drag-to-modulate's rules |
//! | [`commands`] | The edits as commands a plug-in registers |
//! | [`curve`] | Source and aux curves, drawn curves with slew, the velocity and key curves |
//! | [`program`] | The compiled matrix |
//! | [`engine`] | The engine that runs it on the audio thread |
//! | [`mailbox`] | Handing compiled programs to the audio thread |
//!
//! Rules cited as "engine-core Rn" are the plug-in kit's engine
//! specification, section 11 (velocity and note curves) and 12
//! (modulation matrix).
//!
//! # A route
//!
//! A slot's value is `curve(source) × amount × output`, times
//! `curve(aux)` (or `1 − curve(aux)` inverted) when it has an aux source
//! (engine-core R103). A source is first taken to 0 to 1 (a bipolar
//! source as `(x + 1) / 2`), curved, and then a one-way (unipolar) route
//! uses it as it is, adding from the destination's base upward (downward
//! for a negative amount), and a two-way (bipolar) route uses `2u − 1`,
//! swinging around the base (engine-core R105). Amounts are fractions of
//! the destination's travel: a parameter's base is its position on its
//! control's travel, 0 to 1, so an amount of 1 sweeps the whole control.
//! Every route into a destination adds, and the sum is clamped to the
//! destination's range (engine-core R108).
//!
//! # Rates: the control-rate rule
//!
//! A route runs at its destination's rate. The plug-in declares each
//! destination's [`Rate`]:
//!
//! - **Audio** where the ear follows the value sample by sample, so a step
//!   or a lag would be heard: pitch, FM and phase-modulation depth,
//!   amplitude and pan. Every route into it is evaluated at every sample.
//! - **Control** where the value's effect is smooth by nature: filter
//!   cutoff and resonance, levels, shaper and warp amounts, frame
//!   position, LFO rates, envelope times, effect parameters, LFO point
//!   buses, macros and slot amounts. Its routes are evaluated once per
//!   [`CONTROL_PERIOD`] (32 samples, 0.67 ms at 48 kHz, 1.5 kHz) on a grid
//!   counted from the engine's start, and the output ramps linearly from
//!   one tick's value to the next across the period
//!   ([`ControlOut::ramp`]), so it never steps; the ramp lags the source
//!   by one period. Its difference from audio-rate evaluation is at most
//!   the route's change over two periods, and is a delay rather than a
//!   distortion: for a 10 Hz sine LFO sweeping the whole travel, under
//!   4.2 % of the travel, and for slower or smaller modulation
//!   proportionally less.
//! - **Adaptive** where a destination is usually smooth but can take
//!   audio-rate modulation, such as cutoff for filter FM: audio rate
//!   while any route into it reads an [`Audio`](SourceKind::Audio)
//!   source (an oscillator, the noise, a filter), control rate otherwise.
//!   [`Program::rate`] says which a compiled program chose, so the
//!   consumer reads the matching output.
//!
//! Held sources (macros, MIDI controllers, MPE, velocity, note-on values)
//! are read once per tick at control rate and at every sample at audio
//! rate; the plug-in smooths controllers that step if it needs to. Whether
//! each control-rate destination is inaudible against audio rate is
//! measured when the patch compiler sets the reference patches' budgets.
//!
//! # Sample accuracy and blocks
//!
//! The plug-in renders a block's signals (envelopes, LFOs, audio) first,
//! then calls [`Engine::process`] for ranges of the block, splitting at
//! each event: a note-on at frame `k` takes effect at frame `k`. A note
//! that starts a voice ticks its control-rate routes at once, so a new
//! note never waits for the grid. Results don't depend on how a block is
//! split, nor on the block size.
//!
//! # Only active routes cost anything
//!
//! [`Program::compile`] turns the matrix into a flat list of the routes in
//! use, off the audio thread: empty and bypassed slots, destinations no
//! route reaches and their buffers simply don't exist in it. A route
//! whose amount or output is 0 at run time is skipped, and so is every
//! voice that isn't playing. The engine takes a new program at a block's
//! start ([`Engine::begin_block`], through a [`mailbox`]), and an empty
//! matrix costs one branch per call.
//!
//! # Real-time safety
//!
//! Everything [`Engine`] does after [`Engine::new`] allocates nothing,
//! locks nothing and never blocks; old programs are freed on the thread
//! that sent the new ones.

pub mod commands;
pub mod curve;
pub mod dest;
pub mod engine;
mod lanes;
pub mod mailbox;
pub mod matrix;
pub mod program;
pub mod source;

#[cfg(test)]
mod tests;

pub use commands::{Edit, register};
pub use curve::{Curve, DrawnCurve, NoteCurves};
pub use dest::{DestId, DestinationInfo, Destinations, Feed, Rate, Target};
pub use engine::{AudioOut, Chunk, ControlOut, Engine, NoteOn, Output, Signals};
pub use mailbox::{ProgramReceiver, ProgramSender, mailbox};
pub use matrix::{
    Drag, EditError, Matrix, Polarity, SLOTS, Slot, SortBy, drag_polarity, limit_depth,
};
pub use program::{Config, Program, SkipReason, Skipped};
pub use source::{Scope, SourceId, SourceInfo, SourceKind, SourceRange, Sources, Standard};

/// The frames between control-rate ticks: 32, 0.67 ms at 48 kHz.
pub const CONTROL_PERIOD: usize = 32;

/// The widest SIMD vector, in `f32` lanes; per-voice buffers are padded to
/// a multiple of it.
pub(crate) const MAX_LANES: usize = 16;
