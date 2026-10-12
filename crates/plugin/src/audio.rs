//! A block's audio: the main output's channels, which a processor renders
//! into in place.

use std::marker::PhantomData;
use std::ptr;

/// The most channels a port carries.
pub const MAX_CHANNELS: usize = 8;

/// One block of a processor's main output: each channel's frames, which
/// hold the input on entry for an effect and silence for an instrument.
pub struct Buffers<'a> {
    channels: [*mut f32; MAX_CHANNELS],
    count: usize,
    frames: usize,
    _borrow: PhantomData<&'a mut [f32]>,
}

impl<'a> Buffers<'a> {
    /// The channels in `channels`, at most [`MAX_CHANNELS`] of them, each
    /// as long as the shortest.
    pub fn new(channels: &'a mut [&mut [f32]]) -> Self {
        let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        let mut pointers = [ptr::null_mut(); MAX_CHANNELS];
        let count = channels.len().min(MAX_CHANNELS);
        for (pointer, channel) in pointers.iter_mut().zip(channels.iter_mut()) {
            *pointer = channel.as_mut_ptr();
        }
        Self {
            channels: pointers,
            count,
            frames,
            _borrow: PhantomData,
        }
    }

    /// The first `count` channels in `channels`, each `frames` long.
    ///
    /// # Safety
    ///
    /// Each of the first `count` pointers (at most [`MAX_CHANNELS`]) must
    /// point to `frames` initialised floats that nothing else reads or
    /// writes for `'a`, and no two may overlap.
    #[must_use]
    pub unsafe fn from_raw(
        channels: [*mut f32; MAX_CHANNELS],
        count: usize,
        frames: usize,
    ) -> Self {
        Self {
            channels,
            count: count.min(MAX_CHANNELS),
            frames,
            _borrow: PhantomData,
        }
    }

    /// How many channels there are.
    #[must_use]
    pub fn channels(&self) -> usize {
        self.count
    }

    /// How many frames each channel holds.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Channel `index`.
    ///
    /// # Panics
    ///
    /// If there's no channel `index`.
    #[must_use]
    pub fn channel(&self, index: usize) -> &[f32] {
        assert!(index < self.count, "no channel {index}");
        // SAFETY: `new` and `from_raw` make every pointer below `count`
        // point to `frames` floats this value has borrowed exclusively.
        unsafe { std::slice::from_raw_parts(self.channels[index], self.frames) }
    }

    /// Channel `index`, to write.
    ///
    /// # Panics
    ///
    /// If there's no channel `index`.
    pub fn channel_mut(&mut self, index: usize) -> &mut [f32] {
        assert!(index < self.count, "no channel {index}");
        // SAFETY: as for `channel`; `&mut self` makes this the only
        // reference to the channel while it lives.
        unsafe { std::slice::from_raw_parts_mut(self.channels[index], self.frames) }
    }

    /// The first two channels at once, if there are two.
    pub fn stereo_mut(&mut self) -> Option<(&mut [f32], &mut [f32])> {
        if self.count < 2 {
            return None;
        }
        // SAFETY: as for `channel_mut`; the two channels don't overlap.
        unsafe {
            Some((
                std::slice::from_raw_parts_mut(self.channels[0], self.frames),
                std::slice::from_raw_parts_mut(self.channels[1], self.frames),
            ))
        }
    }

    /// Whether every sample in every channel is 0.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        (0..self.count).all(|c| self.channel(c).iter().all(|s| *s == 0.0))
    }

    /// Sets every sample in every channel to 0.
    pub fn silence(&mut self) {
        for c in 0..self.count {
            self.channel_mut(c).fill(0.0);
        }
    }
}

impl std::fmt::Debug for Buffers<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Buffers")
            .field("channels", &self.count)
            .field("frames", &self.frames)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_read_and_written_in_place() {
        let mut left = [0.0f32; 4];
        let mut right = [1.0f32; 5];
        let mut channels = [&mut left[..], &mut right[..]];
        let mut buffers = Buffers::new(&mut channels);
        assert_eq!((buffers.channels(), buffers.frames()), (2, 4));
        assert!(!buffers.is_silent());
        let (l, r) = buffers.stereo_mut().expect("two channels");
        l[0] = 0.5;
        r[3] = 0.0;
        buffers.channel_mut(1).fill(0.25);
        assert_eq!(buffers.channel(0), [0.5, 0.0, 0.0, 0.0]);
        buffers.silence();
        assert!(buffers.is_silent());
        assert_eq!(right, [0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn a_mono_block_has_no_stereo_pair() {
        let mut mono = [0.0f32; 2];
        let mut channels = [&mut mono[..]];
        assert!(Buffers::new(&mut channels).stereo_mut().is_none());
    }
}
