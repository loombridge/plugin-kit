//! The playable table: every frame, drawn and morphed, as band-limited
//! mipmaps laid out for the cache. Built off the audio thread; the audio
//! thread only reads it.

use crate::spectrum::{self, BINS, FrameKind, Spectrum};
use crate::{FRAME_SIZE, Interpolation, Wavetable};

/// How many times a level's samples outnumber twice its highest harmonic:
/// the oversampling that, with the quintic spline, keeps the reader's
/// interpolation images below the aliasing floor.
pub const OVERSAMPLING: usize = 2;

/// The fewest samples a level holds. The levels with few harmonics are
/// oversampled further, which costs little memory and keeps high notes
/// clean.
pub const MIN_LEVEL_SIZE: usize = 256;

/// Values stored around each cycle so a read never wraps: two before it,
/// three after it.
pub const GUARD: usize = 5;

/// One full cycle of phase, in the units [`Wave::read`] takes.
pub const CYCLE: f64 = 4_294_967_296.0;

/// One mipmap level: every frame band-limited to the same harmonic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    /// The highest harmonic it keeps.
    pub harmonics: usize,
    /// The samples in one cycle.
    pub size: usize,
    /// Where its first frame's samples start in the table's data.
    offset: usize,
}

impl Level {
    /// The distance between one frame's samples and the next frame's.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.size + GUARD
    }
}

/// The highest harmonic each level keeps, from all of a frame's 1,024
/// down to the fundamental alone. Levels step by 4/3 and 3/2 in turn
/// (1024, 768, 512, 384 … 4, 3, 2) and finally to 1, so a note above the
/// fundamental-only level never loses more than a third of its bandwidth to
/// the level below: finer than an octave.
fn level_harmonics() -> Vec<usize> {
    let mut levels = Vec::new();
    let mut top = FRAME_SIZE / 2;
    while top >= 2 {
        levels.push(top);
        // Below 4, three quarters isn't a whole harmonic.
        if top >= 4 {
            levels.push(top * 3 / 4);
        }
        top /= 2;
    }
    levels.push(1);
    levels
}

/// A playable wavetable: every frame of the table, the drawn ones and those
/// its interpolation made (engine-core R13), each as mipmap levels that
/// remove the harmonics a note's pitch would fold back above Nyquist.
///
/// [`build`](Self::build) does every FFT and allocation, off the audio
/// thread. Afterwards nothing changes: the audio thread reads it through
/// `&self`, and shares it between instances through the
/// [cache](crate::Cache).
///
/// # Layout
///
/// One block of samples, level after level; within a level, frame after
/// frame, each cycle stored with two values before it and three after it
/// copied from its other end ([`GUARD`]), so a 6-point read never wraps. A
/// note reads one level, so the frames it moves between sit side by side.
/// A level stores each cycle as quintic B-spline coefficients, which
/// [`Wave::read`] turns back into the band-limited cycle.
pub struct Table {
    source: Wavetable,
    frames: Vec<FrameKind>,
    levels: Vec<Level>,
    data: Box<[f32]>,
}

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Table")
            .field("drawn", &self.source.frame_count())
            .field("frames", &self.frames.len())
            .field("interpolation", &self.interpolation())
            .field("levels", &self.levels.len())
            .field("bytes", &self.bytes())
            .finish_non_exhaustive()
    }
}

impl Table {
    /// Builds the playable table from `source`: makes the morphed frames
    /// (engine-core R13), then each frame's mipmaps by FFT. Allocates and
    /// takes milliseconds; never call it on the audio thread.
    #[must_use]
    pub fn build(source: Wavetable) -> Self {
        let interpolation = source.interpolation();
        let frames = spectrum::layout(source.frame_count(), interpolation);
        let morphing = frames.len() > source.frame_count();
        let mut ffts = spectrum::Ffts::new();
        let keys = spectrum::keyframes(&mut ffts, source.frames(), interpolation, morphing);

        let mut levels = Vec::new();
        let mut offset = 0;
        for harmonics in level_harmonics() {
            let size = (2 * OVERSAMPLING * harmonics).max(MIN_LEVEL_SIZE);
            levels.push(Level {
                harmonics,
                size,
                offset,
            });
            offset += frames.len() * (size + GUARD);
        }
        let mut data = vec![0.0f32; offset].into_boxed_slice();

        let gains: Vec<_> = levels
            .iter()
            .map(|l| spectrum::spline_gains(l.size))
            .collect();
        let mut spectrum: Spectrum = Vec::with_capacity(BINS);
        let mut cycle = vec![0.0; levels.iter().map(|l| l.size).max().unwrap_or(0)];
        for (f, &kind) in frames.iter().enumerate() {
            spectrum::frame_spectrum(&keys, kind, &mut spectrum);
            for (level, gains) in levels.iter().zip(&gains) {
                let cycle = &mut cycle[..level.size];
                ffts.synthesise(&spectrum, FRAME_SIZE, level.harmonics, Some(gains), cycle);
                let start = level.offset + f * level.stride();
                let out = &mut data[start..start + level.stride()];
                let wrapped = cycle[level.size - 2..]
                    .iter()
                    .chain(cycle.iter())
                    .chain(&cycle[..GUARD - 2]);
                for (y, &x) in out.iter_mut().zip(wrapped) {
                    *y = x;
                }
            }
        }
        Self {
            source,
            frames,
            levels,
            data,
        }
    }

    /// The table it was built from, as drawn.
    #[must_use]
    pub fn source(&self) -> &Wavetable {
        &self.source
    }

    /// How the frames between drawn frames were made.
    #[must_use]
    pub fn interpolation(&self) -> Interpolation {
        self.source.interpolation()
    }

    /// How many frames it plays: the drawn frames and the morphed ones.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Where each frame comes from, drawn or morphed.
    #[must_use]
    pub fn frames(&self) -> &[FrameKind] {
        &self.frames
    }

    /// The mipmap levels, from every harmonic down to the fundamental.
    #[must_use]
    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    /// Memory the samples take, in bytes.
    #[must_use]
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(&*self.data)
    }

    /// The level a note should read when its phase advances `increment` per
    /// sample ([`CYCLE`] is a whole cycle): the one with the most harmonics
    /// that all stay below Nyquist. A note above Nyquist gets the
    /// fundamental alone. Real-time safe.
    #[must_use]
    pub fn level_for(&self, increment: u32) -> usize {
        // Harmonic h of the note sits at h × increment / CYCLE of the
        // sample rate, below Nyquist while h × increment < CYCLE / 2.
        let increment = u64::from(increment);
        self.levels
            .iter()
            .position(|l| l.harmonics as u64 * increment < 1 << 31)
            .unwrap_or(self.levels.len() - 1)
    }

    /// Frame `frame` at level `level`, to read from. Real-time safe.
    ///
    /// # Panics
    ///
    /// If `level` or `frame` is out of range.
    #[must_use]
    #[inline]
    pub fn wave(&self, level: usize, frame: usize) -> Wave<'_> {
        let level = self.levels[level];
        assert!(frame < self.frames.len(), "frame {frame} out of range");
        let start = level.offset + frame * level.stride();
        Wave {
            coefficients: &self.data[start..start + level.stride()],
            size: level.size as u64,
        }
    }
}

/// How far the phase of a note at `frequency` hertz advances per sample at
/// `sample_rate`, where [`CYCLE`] is one whole cycle.
#[must_use]
pub fn increment(frequency: f64, sample_rate: f64) -> u32 {
    (frequency / sample_rate * CYCLE)
        .round()
        .clamp(0.0, CYCLE - 1.0) as u32
}

/// One frame of one level: a single band-limited cycle, read at any phase.
#[derive(Clone, Copy, Debug)]
pub struct Wave<'a> {
    /// The cycle's spline coefficients with their guard values: two
    /// before, three after.
    coefficients: &'a [f32],
    size: u64,
}

impl Wave<'_> {
    /// The values in one cycle.
    #[must_use]
    pub fn size(&self) -> usize {
        self.size as usize
    }

    /// The cycle at `phase` (0 to [`CYCLE`], wrapping), by quintic B-spline
    /// interpolation: six values per read, a curve through every sample of
    /// the band-limited cycle whose images sit far below its harmonics.
    /// Real-time safe: no allocation, no branch on the data.
    #[must_use]
    #[inline]
    pub fn read(&self, phase: u32) -> f32 {
        let position = u64::from(phase) * self.size;
        let index = (position >> 32) as usize;
        let t = (position as u32) as f32 * (1.0 / CYCLE as f32);
        let p: &[f32; 6] = self.coefficients[index..index + 6]
            .try_into()
            .expect("six values");
        quintic(p, t)
    }
}

/// The quintic B-spline through coefficients `p` (the second before to the
/// third after the read), `t` of the way from the third to the fourth. The
/// weights are the uniform quintic B-spline's polynomial pieces, times 120,
/// a factor the table's coefficients already divide out.
#[inline]
fn quintic(p: &[f32; 6], t: f32) -> f32 {
    let [m2, m1, p0, p1, p2, p3] = *p;
    let (outer, inner) = (m2 + p2, m1 + p1);
    let (outer_slope, inner_slope) = (p2 - m2, p1 - m1);
    let d0 = outer + 26.0 * inner + 66.0 * p0;
    let d1 = 5.0 * outer_slope + 50.0 * inner_slope;
    let d2 = 10.0 * outer + 20.0 * inner - 60.0 * p0;
    let d3 = 10.0 * outer_slope - 20.0 * inner_slope;
    let d4 = 5.0 * outer - 20.0 * inner + 30.0 * p0;
    let d5 = p3 - m2 + 5.0 * (m1 - p2) + 10.0 * (p1 - p0);
    ((((d5 * t + d4) * t + d3) * t + d2) * t + d1) * t + d0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_step_finer_than_an_octave_down_to_the_fundamental() {
        let levels = level_harmonics();
        assert_eq!(levels.first(), Some(&1024));
        assert_eq!(levels.last(), Some(&1));
        for pair in levels.windows(2) {
            assert!(pair[1] < pair[0] && pair[0] <= 2 * pair[1], "{pair:?}");
        }
    }

    #[test]
    fn the_spline_weights_sum_to_120_and_are_continuous() {
        // Constant coefficients give 120 times the constant at every t.
        for t in [0.0, 0.25, 0.5, 0.99] {
            assert!((quintic(&[1.0; 6], t) - 120.0).abs() < 1e-3, "{t}");
        }
        // Reading at t = 1 equals reading the next window at t = 0.
        let values = [0.3, -0.7, 0.5, 0.9, -0.2, 0.4, 0.8];
        let left = quintic(values[..6].try_into().unwrap(), 1.0);
        let right = quintic(values[1..].try_into().unwrap(), 0.0);
        assert!((left - right).abs() < 1e-5, "{left} {right}");
    }
}
