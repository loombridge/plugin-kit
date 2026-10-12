//! The audio devices a standalone app plays through, behind one trait, so
//! the app runs the same on a real device ([`CpalBackend`](crate::CpalBackend))
//! and on a fake one in tests ([`FakeBackend`](crate::FakeBackend)).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// The sample rates the app offers where a device takes a range.
pub const STANDARD_SAMPLE_RATES: [u32; 9] = [
    22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 384_000,
];

/// The buffer sizes, in frames, the app offers where a device takes a range.
pub const STANDARD_BUFFER_SIZES: [u32; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

/// The block a processor is made for when the device's buffer size is left
/// to the device: longer device buffers are split into blocks this long.
pub const DEFAULT_MAX_FRAMES: u32 = 1024;

/// One audio device, as a backend lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The backend's ID for it, which stays the same between runs where the
    /// platform allows.
    pub id: String,
    /// Its name, to show.
    pub name: String,
    /// The most channels it records.
    pub inputs: u16,
    /// The most channels it plays.
    pub outputs: u16,
    /// The sample rates it runs at.
    pub sample_rates: Vec<u32>,
    /// The rate it runs at now, if known.
    pub current_rate: Option<u32>,
    /// The fewest and most frames a buffer may hold, if it says.
    pub buffer_sizes: Option<(u32, u32)>,
    /// Whether it's the system's default output.
    pub is_default_output: bool,
    /// Whether it's the system's default input.
    pub is_default_input: bool,
}

impl DeviceInfo {
    /// The buffer sizes from [`STANDARD_BUFFER_SIZES`] it takes.
    #[must_use]
    pub fn standard_buffer_sizes(&self) -> Vec<u32> {
        STANDARD_BUFFER_SIZES
            .into_iter()
            .filter(|size| {
                self.buffer_sizes
                    .is_none_or(|(min, max)| (min..=max).contains(size))
            })
            .collect()
    }
}

/// What to open: which devices, at what rate and buffer size, with how many
/// channels. `None` leaves a choice to the system or the device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamRequest {
    /// The output device's ID; `None` for the system's default, which the
    /// stream follows when it changes where the platform allows.
    pub output: Option<String>,
    /// The input device's ID, for an effect; `None` for the default.
    pub input: Option<String>,
    /// Whether to open an input at all.
    pub wants_input: bool,
    /// The sample rate; `None` for the device's current rate.
    pub sample_rate: Option<u32>,
    /// The device buffer, in frames; `None` for the device's default.
    pub buffer_size: Option<u32>,
}

/// What a stream opened, as it runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamInfo {
    /// The output device's ID and name.
    pub output_id: String,
    /// The output device's name.
    pub output_name: String,
    /// The input device's name, if an input is open.
    pub input_name: Option<String>,
    /// Frames per second.
    pub sample_rate: u32,
    /// The device buffer it asked for, if it asked for one.
    pub buffer_size: Option<u32>,
    /// The most frames the callback is given at once, which a processor is
    /// made for.
    pub max_frames: u32,
    /// Output channels, interleaved in the callback's output.
    pub output_channels: u16,
    /// Input channels, interleaved in the callback's input; 0 without one.
    pub input_channels: u16,
    /// Whether the output follows the system's default device.
    pub follows_default: bool,
    /// Whether the output device is a fallback for the one asked for,
    /// which wasn't there.
    pub fallback: bool,
}

/// The code that makes sound, which a stream calls on its audio thread.
pub trait AudioCallback: Send + 'static {
    /// Fills `output`, `channels` interleaved, from `input`,
    /// `input_channels` interleaved and as many frames long (silence if
    /// the input fell behind). Never allocates, locks or blocks.
    fn render(&mut self, input: &[f32], input_channels: usize, output: &mut [f32], channels: usize);
}

/// Why a stream stopped, or couldn't open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Failure {
    /// Nothing's wrong.
    None = 0,
    /// The device went away.
    DeviceLost = 1,
    /// The stream can't go on as it is (its configuration changed), and
    /// must be opened again.
    Invalidated = 2,
    /// The platform reported some other error the stream can't recover from.
    Other = 3,
}

impl Failure {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::DeviceLost,
            2 => Self::Invalidated,
            3 => Self::Other,
            _ => Self::None,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "running",
            Self::DeviceLost => "the device went away",
            Self::Invalidated => "the device's configuration changed",
            Self::Other => "the device reported an error",
        })
    }
}

/// What a running stream reports, from atomics its audio and error
/// callbacks write: real-time safe on both sides.
#[derive(Debug, Default)]
pub struct StreamHealth {
    callbacks: AtomicU64,
    xruns: AtomicU64,
    failure: AtomicU8,
}

impl StreamHealth {
    /// A new report, shared between a stream and whoever watches it.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Counts one audio callback.
    pub fn callback(&self) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts a dropout the platform reported.
    pub fn xrun(&self) {
        self.xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Records why the stream can't go on; the first reason stays.
    pub fn fail(&self, failure: Failure) {
        let _ = self.failure.compare_exchange(
            Failure::None as u8,
            failure as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// How many audio callbacks have run.
    #[must_use]
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Relaxed)
    }

    /// How many dropouts the platform reported.
    #[must_use]
    pub fn xruns(&self) -> u64 {
        self.xruns.load(Ordering::Relaxed)
    }

    /// Why the stream can't go on, or [`Failure::None`].
    #[must_use]
    pub fn failure(&self) -> Failure {
        Failure::from_u8(self.failure.load(Ordering::Acquire))
    }
}

/// An open stream. It plays from the moment it opens until it's dropped.
pub trait AudioStream {
    /// What it opened.
    fn info(&self) -> &StreamInfo;
    /// How it's running.
    fn health(&self) -> &StreamHealth;
}

/// Makes a stream's callback once the backend knows what the stream will
/// run at.
pub type MakeCallback<'a> =
    &'a mut dyn FnMut(&StreamInfo) -> Result<Box<dyn AudioCallback>, String>;

/// One audio API on this machine.
pub trait AudioBackend {
    /// Its name, such as `CoreAudio`.
    fn name(&self) -> String;

    /// The devices it has now.
    ///
    /// # Errors
    ///
    /// Why it couldn't list them.
    fn devices(&self) -> Result<Vec<DeviceInfo>, String>;

    /// Opens a stream for `request` and starts it, with the callback `make`
    /// returns for what the stream will run at. A requested device that
    /// isn't there falls back to the default ([`StreamInfo::fallback`]).
    ///
    /// # Errors
    ///
    /// Why it couldn't open, or why `make` failed.
    fn open(
        &self,
        request: &StreamRequest,
        make: MakeCallback<'_>,
    ) -> Result<Box<dyn AudioStream>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_failure_stays() {
        let health = StreamHealth::new();
        assert_eq!(health.failure(), Failure::None);
        health.fail(Failure::Invalidated);
        health.fail(Failure::DeviceLost);
        assert_eq!(health.failure(), Failure::Invalidated);
        health.callback();
        health.xrun();
        assert_eq!((health.callbacks(), health.xruns()), (1, 1));
    }

    #[test]
    fn buffer_sizes_follow_the_devices_range() {
        let device = DeviceInfo {
            buffer_sizes: Some((100, 600)),
            ..DeviceInfo::default()
        };
        assert_eq!(device.standard_buffer_sizes(), [128, 256, 512]);
        assert_eq!(
            DeviceInfo::default().standard_buffer_sizes(),
            STANDARD_BUFFER_SIZES
        );
    }
}
