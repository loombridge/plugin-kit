//! Real devices through `cpal`: Core Audio on macOS, WASAPI (shared) on
//! Windows, ALSA on Linux (PipeWire and PulseAudio through their ALSA
//! plug-ins).
//!
//! `cpal` 0.18 opens input and output as separate streams, so an effect's
//! input reaches the output's callback through a lock-free ring, which
//! keeps at most a few buffers of it. The output's callback runs the kit's
//! real-time set-up once per audio thread (`lspk_rt::AudioThreadSetup`,
//! with the device's audio workgroup on macOS), playing silence for the
//! buffer that did it, then renders every buffer as a real-time section
//! (`lspk_rt::realtime`).
//!
//! On macOS a stream at a rate other than the device's current one changes
//! the device's rate, which stays after the app quits, so the app asks for
//! a rate only when the person chose one.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    FromSample, SizedSample, SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange,
};
use lspk_rt::{AudioThreadSetup, RtThreadConfig};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::audio::{
    AudioBackend, AudioCallback, AudioStream, DEFAULT_MAX_FRAMES, DeviceInfo, Failure,
    MakeCallback, STANDARD_SAMPLE_RATES, StreamHealth, StreamInfo, StreamRequest,
};

/// The most frames one call to the app's callback holds; longer device
/// buffers are split.
const SCRATCH_FRAMES: usize = 8192;

/// The system's default audio API, through `cpal`.
pub struct CpalBackend {
    host: cpal::Host,
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CpalBackend {
    /// The platform's default audio API.
    #[must_use]
    pub fn new() -> Self {
        Self {
            host: cpal::default_host(),
        }
    }

    fn id_of(device: &cpal::Device) -> Option<String> {
        device.id().ok().map(|id| id.to_string())
    }

    fn name_of(device: &cpal::Device) -> String {
        device
            .description()
            .map_or_else(|_| device.to_string(), |d| d.name().to_owned())
    }

    fn find(&self, id: &str) -> Option<cpal::Device> {
        let id = cpal::DeviceId::from_str(id).ok()?;
        self.host.device_by_id(&id)
    }

    fn describe(
        device: &cpal::Device,
        default_output: Option<&str>,
        default_input: Option<&str>,
    ) -> Option<DeviceInfo> {
        let id = Self::id_of(device)?;
        let outputs: Vec<SupportedStreamConfigRange> = device
            .supported_output_configs()
            .map(Iterator::collect)
            .unwrap_or_default();
        let inputs: Vec<SupportedStreamConfigRange> = device
            .supported_input_configs()
            .map(Iterator::collect)
            .unwrap_or_default();
        let ranges = if outputs.is_empty() {
            &inputs
        } else {
            &outputs
        };
        let current_rate = if outputs.is_empty() {
            device.default_input_config().ok()
        } else {
            device.default_output_config().ok()
        }
        .map(|config| config.sample_rate());
        let mut sample_rates: Vec<u32> = STANDARD_SAMPLE_RATES
            .into_iter()
            .chain(current_rate)
            .filter(|rate| ranges.iter().any(|r| r.contains_rate(*rate)))
            .collect();
        sample_rates.sort_unstable();
        sample_rates.dedup();
        let buffer_sizes = ranges
            .iter()
            .filter_map(|range| match *range.buffer_size() {
                SupportedBufferSize::Range { min, max } => Some((min, max)),
                SupportedBufferSize::Unknown => None,
            })
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
        Some(DeviceInfo {
            is_default_output: default_output == Some(id.as_str()),
            is_default_input: default_input == Some(id.as_str()),
            name: Self::name_of(device),
            id,
            inputs: inputs.iter().map(|r| r.channels()).max().unwrap_or(0),
            outputs: outputs.iter().map(|r| r.channels()).max().unwrap_or(0),
            sample_rates,
            current_rate,
            buffer_sizes,
        })
    }

    /// The device `id` asks for, or the default if it's `None` or missing;
    /// with whether it's a fallback.
    fn pick(
        &self,
        id: Option<&str>,
        default: impl Fn() -> Option<cpal::Device>,
    ) -> Option<(cpal::Device, bool)> {
        match id {
            None => default().map(|d| (d, false)),
            Some(id) => match self.find(id) {
                Some(device) => Some((device, false)),
                None => default().map(|d| (d, true)),
            },
        }
    }
}

/// The supported range to open at `rate` with `channels`: the device's
/// current format if it fits, else the most precise one that does.
fn choose(
    ranges: &[SupportedStreamConfigRange],
    channels: u16,
    rate: u32,
    current: cpal::SampleFormat,
) -> Option<SupportedStreamConfig> {
    let rank = |format: cpal::SampleFormat| match format {
        f if f == current => 0,
        cpal::SampleFormat::F32 => 1,
        cpal::SampleFormat::I32 => 2,
        cpal::SampleFormat::I24 => 3,
        cpal::SampleFormat::I16 => 4,
        _ => 5,
    };
    ranges
        .iter()
        .filter(|r| {
            r.channels() == channels && r.contains_rate(rate) && supported(r.sample_format())
        })
        .min_by_key(|r| rank(r.sample_format()))
        .and_then(|r| r.try_with_sample_rate(rate))
}

/// Whether the app reads and writes `format`.
fn supported(format: cpal::SampleFormat) -> bool {
    matches!(
        format,
        cpal::SampleFormat::F32
            | cpal::SampleFormat::I16
            | cpal::SampleFormat::I24
            | cpal::SampleFormat::I32
            | cpal::SampleFormat::U16
    )
}

/// The fixed buffer for `request` within `config`'s range, or the default.
fn buffer(config: &SupportedStreamConfig, frames: Option<u32>) -> Result<cpal::BufferSize, String> {
    let Some(frames) = frames else {
        return Ok(cpal::BufferSize::Default);
    };
    if let SupportedBufferSize::Range { min, max } = *config.buffer_size()
        && !(min..=max).contains(&frames)
    {
        return Err(format!(
            "the device takes buffers of {min} to {max} frames, not {frames}"
        ));
    }
    Ok(cpal::BufferSize::Fixed(frames))
}

/// Records a running stream's error in `health`. `cpal` may call this on
/// the audio thread, so it only touches atomics.
fn record(health: &StreamHealth, error: &cpal::Error) {
    match error.kind() {
        cpal::ErrorKind::Xrun => health.xrun(),
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable => {
            health.fail(Failure::DeviceLost);
        }
        cpal::ErrorKind::StreamInvalidated => health.fail(Failure::Invalidated),
        _ => {}
    }
}

impl AudioBackend for CpalBackend {
    fn name(&self) -> String {
        self.host.id().to_string()
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        let default_output = self
            .host
            .default_output_device()
            .and_then(|d| Self::id_of(&d));
        let default_input = self
            .host
            .default_input_device()
            .and_then(|d| Self::id_of(&d));
        let devices = self.host.devices().map_err(|e| e.to_string())?;
        Ok(devices
            .filter_map(|d| Self::describe(&d, default_output.as_deref(), default_input.as_deref()))
            .collect())
    }

    fn open(
        &self,
        request: &StreamRequest,
        make: MakeCallback<'_>,
    ) -> Result<Box<dyn AudioStream>, String> {
        let (output, fallback) = self
            .pick(request.output.as_deref(), || {
                self.host.default_output_device()
            })
            .ok_or("there's no output device")?;
        let output_name = Self::name_of(&output);
        let output_id = Self::id_of(&output).unwrap_or_default();
        let current = output
            .default_output_config()
            .map_err(|e| format!("{output_name}: {e}"))?;
        let rate = request.sample_rate.unwrap_or(current.sample_rate());
        let ranges: Vec<SupportedStreamConfigRange> = output
            .supported_output_configs()
            .map_err(|e| format!("{output_name}: {e}"))?
            .collect();
        let config = choose(&ranges, current.channels(), rate, current.sample_format())
            .ok_or_else(|| format!("{output_name} can't play at {rate} Hz"))?;
        let buffer_size =
            buffer(&config, request.buffer_size).map_err(|e| format!("{output_name}: {e}"))?;
        let channels = config.channels();

        // The input, for an effect, at the output's rate; an input that
        // can't open leaves the output playing without one.
        let mut input = None;
        if request.wants_input
            && let Some((device, _)) = self.pick(request.input.as_deref(), || {
                self.host.default_input_device()
            })
            && let Ok(current) = device.default_input_config()
            && let Ok(ranges) = device.supported_input_configs()
        {
            let ranges: Vec<SupportedStreamConfigRange> = ranges.collect();
            if let Some(config) = choose(&ranges, current.channels(), rate, current.sample_format())
            {
                input = Some((device, config));
            }
        }

        let max_frames = request.buffer_size.unwrap_or(DEFAULT_MAX_FRAMES);
        let input_channels = input.as_ref().map_or(0, |(_, c)| c.channels());
        let info = StreamInfo {
            output_id: output_id.clone(),
            output_name,
            input_name: input.as_ref().map(|(d, _)| Self::name_of(d)),
            sample_rate: rate,
            buffer_size: request.buffer_size,
            max_frames,
            output_channels: channels,
            input_channels,
            follows_default: request.output.is_none() || fallback,
            fallback,
        };
        let callback = make(&info)?;
        let health = StreamHealth::new();

        // About 100 ms of input, or four of the largest buffers.
        let ring_frames = (rate as usize / 10).max(4 * max_frames as usize);
        let (ring_in, ring_out) = if input_channels > 0 {
            let (p, c) = RingBuffer::new(ring_frames * usize::from(input_channels));
            (Some(p), Some(c))
        } else {
            (None, None)
        };

        let workgroup = workgroup_of(&output_id, info.follows_default);
        let period_frames = request.buffer_size.unwrap_or(512);
        let (setup, _reports) = AudioThreadSetup::new(RtThreadConfig {
            period: Duration::from_secs_f64(f64::from(period_frames) / f64::from(rate)),
            promote: true,
            workgroup,
        });
        let render = Render {
            callback,
            setup,
            output: vec![0.0; SCRATCH_FRAMES * usize::from(channels)],
            input: vec![0.0; SCRATCH_FRAMES * usize::from(input_channels)],
            ring: ring_out,
            ring_frames,
            channels: usize::from(channels),
            input_channels: usize::from(input_channels),
            health: health.clone(),
        };
        let stream_config = cpal::StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size,
        };
        let output_stream = build_output(
            &output,
            stream_config,
            config.sample_format(),
            render,
            health.clone(),
        )
        .map_err(|e| format!("{}: {e}", info.output_name))?;
        let input_stream = match (input, ring_in) {
            (Some((device, config)), Some(ring)) => {
                let stream_config = cpal::StreamConfig {
                    channels: config.channels(),
                    sample_rate: rate,
                    buffer_size: cpal::BufferSize::Default,
                };
                build_input(
                    &device,
                    stream_config,
                    config.sample_format(),
                    ring,
                    health.clone(),
                )
                .ok()
            }
            _ => None,
        };
        output_stream.play().map_err(|e| e.to_string())?;
        if let Some(stream) = &input_stream {
            let _ = stream.play();
        }
        Ok(Box::new(CpalStream {
            _output: output_stream,
            _input: input_stream,
            info,
            health,
        }))
    }
}

/// The output device's audio workgroup on macOS, which the audio thread
/// joins.
fn workgroup_of(id: &str, follows_default: bool) -> Option<lspk_rt::Workgroup> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if follows_default {
        return lspk_rt::Workgroup::of_default_output_device().ok();
    }
    let uid = cpal::DeviceId::from_str(id).ok()?;
    lspk_rt::Workgroup::of_core_audio_device(uid.id()).ok()
}

/// What the output stream's callback owns.
struct Render {
    callback: Box<dyn AudioCallback>,
    setup: AudioThreadSetup,
    output: Vec<f32>,
    input: Vec<f32>,
    ring: Option<Consumer<f32>>,
    /// The ring's size in frames; it keeps at most half.
    ring_frames: usize,
    channels: usize,
    input_channels: usize,
    health: Arc<StreamHealth>,
}

impl Render {
    fn render<T: SizedSample + FromSample<f32>>(&mut self, data: &mut [T]) {
        self.health.callback();
        if !self.setup.is_current_thread() {
            // Set-up never shares a deadline with audio.
            self.setup.enter();
            data.fill(T::EQUILIBRIUM);
            return;
        }
        lspk_rt::realtime(|| {
            let channels = self.channels.max(1);
            for chunk in data.chunks_mut(SCRATCH_FRAMES * channels) {
                let frames = chunk.len() / channels;
                let input = &mut self.input[..frames * self.input_channels];
                if let Some(ring) = &mut self.ring {
                    // Keep the input's delay bounded: drop what's more than
                    // half the ring ahead.
                    let keep = (self.ring_frames / 2 + frames) * self.input_channels;
                    while ring.slots() > keep {
                        let _ = ring.pop();
                    }
                    for sample in input.iter_mut() {
                        *sample = ring.pop().unwrap_or(0.0);
                    }
                }
                let output = &mut self.output[..frames * channels];
                self.callback
                    .render(input, self.input_channels, output, channels);
                for (target, source) in chunk.iter_mut().zip(output.iter()) {
                    *target = T::from_sample(*source);
                }
            }
        });
    }
}

fn build_output(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    render: Render,
    health: Arc<StreamHealth>,
) -> Result<cpal::Stream, cpal::Error> {
    fn typed<T: SizedSample + FromSample<f32> + Send + 'static>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        mut render: Render,
        health: Arc<StreamHealth>,
    ) -> Result<cpal::Stream, cpal::Error> {
        device.build_output_stream::<T, _, _>(
            config,
            move |data, _| render.render(data),
            move |error| record(&health, &error),
            None,
        )
    }
    match format {
        cpal::SampleFormat::I16 => typed::<i16>(device, config, render, health),
        cpal::SampleFormat::I24 => typed::<cpal::I24>(device, config, render, health),
        cpal::SampleFormat::I32 => typed::<i32>(device, config, render, health),
        cpal::SampleFormat::U16 => typed::<u16>(device, config, render, health),
        _ => typed::<f32>(device, config, render, health),
    }
}

fn build_input(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    ring: Producer<f32>,
    health: Arc<StreamHealth>,
) -> Result<cpal::Stream, cpal::Error> {
    fn typed<T: SizedSample + Send + 'static>(
        device: &cpal::Device,
        config: cpal::StreamConfig,
        mut ring: Producer<f32>,
        health: Arc<StreamHealth>,
    ) -> Result<cpal::Stream, cpal::Error>
    where
        f32: FromSample<T>,
    {
        device.build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                lspk_rt::realtime(|| {
                    for sample in data {
                        // A full ring drops the newest; the output's side
                        // keeps the delay bounded.
                        let _ = ring.push(sample.to_sample::<f32>());
                    }
                });
            },
            move |error| record(&health, &error),
            None,
        )
    }
    match format {
        cpal::SampleFormat::I16 => typed::<i16>(device, config, ring, health),
        cpal::SampleFormat::I24 => typed::<cpal::I24>(device, config, ring, health),
        cpal::SampleFormat::I32 => typed::<i32>(device, config, ring, health),
        cpal::SampleFormat::U16 => typed::<u16>(device, config, ring, health),
        _ => typed::<f32>(device, config, ring, health),
    }
}

/// A running pair of `cpal` streams; dropping it stops both.
struct CpalStream {
    _output: cpal::Stream,
    _input: Option<cpal::Stream>,
    info: StreamInfo,
    health: Arc<StreamHealth>,
}

impl AudioStream for CpalStream {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn health(&self) -> &StreamHealth {
        &self.health
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(
        channels: u16,
        min: u32,
        max: u32,
        format: cpal::SampleFormat,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(
            channels,
            min,
            max,
            SupportedBufferSize::Range { min: 16, max: 4096 },
            format,
        )
    }

    #[test]
    fn the_current_format_wins_when_it_fits() {
        let ranges = [
            range(2, 44_100, 96_000, cpal::SampleFormat::I16),
            range(2, 44_100, 96_000, cpal::SampleFormat::F32),
        ];
        let chosen = choose(&ranges, 2, 48_000, cpal::SampleFormat::I16).expect("a range");
        assert_eq!(chosen.sample_format(), cpal::SampleFormat::I16);
        let chosen = choose(&ranges, 2, 48_000, cpal::SampleFormat::U8).expect("a range");
        assert_eq!(chosen.sample_format(), cpal::SampleFormat::F32);
        assert!(choose(&ranges, 1, 48_000, cpal::SampleFormat::F32).is_none());
        assert!(choose(&ranges, 2, 192_000, cpal::SampleFormat::F32).is_none());
    }

    #[test]
    fn a_buffer_outside_the_range_is_refused() {
        let config = range(2, 48_000, 48_000, cpal::SampleFormat::F32).with_sample_rate(48_000);
        assert_eq!(buffer(&config, None), Ok(cpal::BufferSize::Default));
        assert_eq!(buffer(&config, Some(256)), Ok(cpal::BufferSize::Fixed(256)));
        assert!(buffer(&config, Some(8)).is_err());
    }
}
