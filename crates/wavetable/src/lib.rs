//! The wavetable model every kit synthesiser plays.
//!
//! | Item | What it is |
//! |---|---|
//! | [`Wavetable`] | A table as drawn: 1 to 256 frames of 2,048 samples, the [`Interpolation`] that fills the frames between them (none, crossfade or a spectral morph), and the factory flag |
//! | [`wav`] | WAV import and export, with the `clm ` chunk the wavetable community uses; plain WAVs split at a given frame size; 8 to 32-bit integer and 32 and 64-bit float input |
//! | [`Table`] | The playable table, built off the audio thread: the morphed frames made at load, and every frame as band-limited mipmaps laid out for the cache. The audio thread only reads it |
//! | [`Cache`] | The process-wide cache that shares one [`Table`] between every instance that loads it, never freed on the audio thread |
//!
//! # Playing a table
//!
//! A note picks its level once per block from its phase increment, then
//! reads it per sample. The level holds only harmonics below Nyquist at
//! that pitch, so the read doesn't alias at any note or sample rate:
//!
//! ```
//! use lspk_wavetable::{Cache, Wavetable, increment};
//!
//! // A saw, as one frame.
//! let saw = Wavetable::from_fn(1, |_, n| 1.0 - 2.0 * n as f32 / 2048.0).unwrap();
//! let table = Cache::global().share(saw);
//!
//! // On the audio thread: no allocation, no lock.
//! let step = increment(440.0, 48_000.0);
//! let wave = table.wave(table.level_for(step), 0);
//! let mut phase = 0u32;
//! let mut block = [0.0f32; 64];
//! for sample in &mut block {
//!     *sample = wave.read(phase);
//!     phase = phase.wrapping_add(step);
//! }
//! ```

mod cache;
mod model;
mod spectrum;
mod table;
pub mod wav;

pub use cache::{Cache, SharedTable};
pub use model::{Frame, Interpolation, Wavetable};
pub use spectrum::FrameKind;
pub use table::{CYCLE, GUARD, Level, MIN_LEVEL_SIZE, OVERSAMPLING, Table, Wave, increment};

/// The samples in one frame (engine-core R13).
pub const FRAME_SIZE: usize = 2048;

/// The most frames a table holds (engine-core R13).
pub const MAX_FRAMES: usize = 256;

/// Why a table couldn't be made or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The table has no frames.
    NoFrames,
    /// The table has more than [`MAX_FRAMES`] frames.
    TooManyFrames(usize),
    /// A frame isn't [`FRAME_SIZE`] samples, or a frame size to split a WAV
    /// at is below 2.
    FrameSize(usize),
    /// The bytes aren't a RIFF WAVE file.
    NotWav,
    /// The WAV file is damaged.
    Malformed(&'static str),
    /// The WAV file's sample format isn't one this reads.
    Unsupported {
        /// The `fmt ` chunk's format tag.
        format: u16,
        /// Its bits per sample.
        bits: u16,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFrames => write!(f, "the table has no frames"),
            Self::TooManyFrames(n) => {
                write!(f, "the table has {n} frames, more than {MAX_FRAMES}")
            }
            Self::FrameSize(n) => write!(f, "a frame of {n} samples isn't usable here"),
            Self::NotWav => write!(f, "not a WAV file"),
            Self::Malformed(why) => write!(f, "damaged WAV file: {why}"),
            Self::Unsupported { format, bits } => {
                write!(f, "unsupported WAV sample format {format} at {bits} bits")
            }
        }
    }
}

impl std::error::Error for Error {}
