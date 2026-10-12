//! The wavetable as drawn: its frames, how the frames between them are made,
//! and the factory flag.

use crate::{Error, FRAME_SIZE, MAX_FRAMES};

/// How a table fills the frames between its drawn frames when it loads
/// (engine-core R13). A table stores the mode, never the frames it makes.
///
/// The digit is the first flag of the `clm ` chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Interpolation {
    /// Only the drawn frames.
    #[default]
    None,
    /// Frames between drawn frames crossfade their samples.
    Crossfade,
    /// Frames between drawn frames morph each harmonic's magnitude and
    /// phase.
    Spectral,
    /// As [`Spectral`](Self::Spectral), with every frame first shifted in
    /// time so its fundamental starts at phase zero, which keeps the morph
    /// from cancelling where neighbouring frames are out of phase.
    SpectralZeroFundamental,
    /// As [`Spectral`](Self::Spectral), with every harmonic at phase zero:
    /// a morph of magnitudes alone.
    SpectralZeroPhases,
}

impl Interpolation {
    /// Every mode, in the order of their `clm ` digits.
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Crossfade,
        Self::Spectral,
        Self::SpectralZeroFundamental,
        Self::SpectralZeroPhases,
    ];

    /// Its digit in the `clm ` chunk's flags.
    #[must_use]
    pub const fn digit(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Crossfade => 1,
            Self::Spectral => 2,
            Self::SpectralZeroFundamental => 3,
            Self::SpectralZeroPhases => 4,
        }
    }

    /// The mode a `clm ` digit names; an unknown digit is
    /// [`None`](Self::None).
    #[must_use]
    pub const fn from_digit(digit: u8) -> Self {
        match digit {
            1 => Self::Crossfade,
            2 => Self::Spectral,
            3 => Self::SpectralZeroFundamental,
            4 => Self::SpectralZeroPhases,
            _ => Self::None,
        }
    }
}

/// One frame: a single cycle of [`FRAME_SIZE`] samples.
pub type Frame = Box<[f32; FRAME_SIZE]>;

/// A wavetable as drawn: 1 to [`MAX_FRAMES`] frames of [`FRAME_SIZE`]
/// samples, the interpolation that fills the frames between them, and the
/// factory flag (engine-core R13).
///
/// It is the editable source. [`Table::build`](crate::Table::build) makes
/// the playable table from it, off the audio thread; the audio thread reads
/// only that.
#[derive(Clone, Debug, PartialEq)]
pub struct Wavetable {
    frames: Vec<Frame>,
    interpolation: Interpolation,
    factory: bool,
}

impl Wavetable {
    /// A table of `frames`, each [`FRAME_SIZE`] samples long, with no
    /// interpolation and the factory flag clear.
    ///
    /// # Errors
    ///
    /// [`Error::NoFrames`], [`Error::TooManyFrames`], or
    /// [`Error::FrameSize`] if a frame isn't [`FRAME_SIZE`] samples.
    pub fn new<F: AsRef<[f32]>>(frames: &[F]) -> Result<Self, Error> {
        check_count(frames.len())?;
        let frames = frames
            .iter()
            .map(|f| {
                let f = f.as_ref();
                <[f32; FRAME_SIZE]>::try_from(f)
                    .map(Box::new)
                    .map_err(|_| Error::FrameSize(f.len()))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            frames,
            interpolation: Interpolation::None,
            factory: false,
        })
    }

    /// A table of `count` frames whose samples `f(frame, sample)` gives.
    ///
    /// # Errors
    ///
    /// [`Error::NoFrames`] or [`Error::TooManyFrames`].
    pub fn from_fn(count: usize, mut f: impl FnMut(usize, usize) -> f32) -> Result<Self, Error> {
        check_count(count)?;
        let frames = (0..count)
            .map(|frame| Box::new(std::array::from_fn(|sample| f(frame, sample))))
            .collect();
        Ok(Self {
            frames,
            interpolation: Interpolation::None,
            factory: false,
        })
    }

    /// The same table with `interpolation`.
    #[must_use]
    pub fn with_interpolation(mut self, interpolation: Interpolation) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// The drawn frames.
    #[must_use]
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// How many frames are drawn.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// How the frames between drawn frames are made.
    #[must_use]
    pub fn interpolation(&self) -> Interpolation {
        self.interpolation
    }

    /// Sets how the frames between drawn frames are made.
    pub fn set_interpolation(&mut self, interpolation: Interpolation) {
        self.interpolation = interpolation;
    }

    /// Whether the file it came from was marked as another synthesiser's
    /// factory content. It is kept for display only: export never sets it.
    #[must_use]
    pub fn factory(&self) -> bool {
        self.factory
    }

    pub(crate) fn set_factory(&mut self, factory: bool) {
        self.factory = factory;
    }
}

fn check_count(count: usize) -> Result<(), Error> {
    match count {
        0 => Err(Error::NoFrames),
        n if n > MAX_FRAMES => Err(Error::TooManyFrames(n)),
        _ => Ok(()),
    }
}
