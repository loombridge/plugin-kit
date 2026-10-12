//! DSP building blocks: band-limited and wavetable oscillators,
//! interpolators, zero-delay-feedback filters, envelopes, LFOs, oversampling,
//! FFT, resampling, SIMD helpers and waveshapers.
//!
//! So far:
//!
//! | Module | What it holds |
//! |---|---|
//! | [`smooth`] | Parameter smoothing, from `lspk-params` |
//! | [`envelope`] | Envelopes with delay, attack, hold, decay, sustain and release, bendable curves, tempo sync, legato and steal restarts, one voice or many in SIMD lanes |
//! | [`lfo`] | LFOs: drawn shapes, an XY path, chaos and sample and hold, per voice or shared, with a SIMD path across voices |
//! | [`oversample`] | Oversampling by 2, 4 or 8 for nonlinear processes |
//! | [`filter`] | Zero-delay-feedback filters per voice: a state-variable family and a transistor ladder, scalar or in SIMD lanes across voices |
//! | [`simd`] | Lanes of `f32` for NEON, SSE4.2, AVX2 and AVX-512, chosen at run time, with a portable fallback |
//!
//! Everything here that runs per sample allocates nothing, locks nothing and
//! never blocks, so it's safe on an audio thread. Each hot path has a
//! micro-benchmark in `benches/perf.rs`, which the instruction-count gate
//! covers.

pub mod envelope;
pub mod filter;
pub mod lfo;
pub mod oversample;
pub mod simd;
/// Parameter smoothing, from the plug-in kit's `lspk-params`.
pub use lspk_params::smooth;

pub use envelope::{Envelope, EnvelopeBank, EnvelopeParams};
pub use filter::{FilterBank, FilterType, VoiceParams};
pub use oversample::Oversampler;
pub use smooth::LinearSmoother;
