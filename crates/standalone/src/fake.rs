//! A fake audio backend: devices that exist only in memory, and a stream
//! whose callback runs when a test asks for a buffer, on the test's own
//! thread. It's the standalone app's headless test path: the same audio
//! loop and the same device handling as on real hardware, with no sound
//! card.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::audio::{
    AudioBackend, AudioCallback, AudioStream, DEFAULT_MAX_FRAMES, DeviceInfo, Failure,
    MakeCallback, StreamHealth, StreamInfo, StreamRequest,
};

/// A fake backend and its devices. Clones share them, so a test keeps one
/// to drive the stream the app opened through another.
#[derive(Clone)]
pub struct FakeBackend {
    state: Rc<RefCell<State>>,
}

struct State {
    devices: Vec<DeviceInfo>,
    running: Option<Running>,
    next: u64,
    opened: u64,
}

struct Running {
    serial: u64,
    callback: Box<dyn AudioCallback>,
    info: StreamInfo,
    health: Arc<StreamHealth>,
}

impl FakeBackend {
    /// A backend with `devices`.
    #[must_use]
    pub fn new(devices: Vec<DeviceInfo>) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                devices,
                running: None,
                next: 1,
                opened: 0,
            })),
        }
    }

    /// A stereo device with ID `id`, playing and recording at 44.1, 48 and
    /// 96 kHz, now at 48 kHz.
    #[must_use]
    pub fn device(id: &str, default: bool) -> DeviceInfo {
        DeviceInfo {
            id: id.to_owned(),
            name: format!("Fake {id}"),
            inputs: 2,
            outputs: 2,
            sample_rates: vec![44_100, 48_000, 96_000],
            current_rate: Some(48_000),
            buffer_sizes: Some((32, 4096)),
            is_default_output: default,
            is_default_input: default,
        }
    }

    /// Replaces the devices, as plugging one in or out does. A stream on a
    /// device that's gone fails as a lost device.
    pub fn set_devices(&self, devices: Vec<DeviceInfo>) {
        let mut state = self.state.borrow_mut();
        if let Some(running) = &state.running
            && !devices.iter().any(|d| d.id == running.info.output_id)
        {
            running.health.fail(Failure::DeviceLost);
        }
        state.devices = devices;
    }

    /// Makes the running stream fail with `failure`, as a device does when
    /// its configuration changes underneath it.
    pub fn fail(&self, failure: Failure) {
        if let Some(running) = &self.state.borrow().running {
            running.health.fail(failure);
        }
    }

    /// How many streams have been opened.
    #[must_use]
    pub fn opened(&self) -> u64 {
        self.state.borrow().opened
    }

    /// What the running stream opened, if one is running.
    #[must_use]
    pub fn running(&self) -> Option<StreamInfo> {
        self.state.borrow().running.as_ref().map(|r| r.info.clone())
    }

    /// Runs the stream's callback for one buffer as long as `output`, which
    /// holds the stream's output channels interleaved, with `input`, its
    /// input channels interleaved. Returns false, with `output` silent, if
    /// no stream is running or it has failed. Allocates nothing, so a test
    /// can run it in a real-time section.
    pub fn render_into(&self, input: &[f32], output: &mut [f32]) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(running) = state.running.as_mut() else {
            output.fill(0.0);
            return false;
        };
        if running.health.failure() != Failure::None {
            output.fill(0.0);
            return false;
        }
        running.health.callback();
        let inputs = usize::from(running.info.input_channels);
        let outputs = usize::from(running.info.output_channels);
        running.callback.render(input, inputs, output, outputs);
        true
    }

    /// Runs the stream's callback for `frames` frames of silent input and
    /// returns its output, interleaved; `None` if no stream is running.
    #[must_use]
    pub fn render(&self, frames: usize) -> Option<Vec<f32>> {
        let info = self.running()?;
        let input = vec![0.0; frames * usize::from(info.input_channels)];
        let mut output = vec![0.0; frames * usize::from(info.output_channels)];
        self.render_into(&input, &mut output).then_some(output)
    }
}

impl AudioBackend for FakeBackend {
    fn name(&self) -> String {
        "Fake".to_owned()
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        Ok(self.state.borrow().devices.clone())
    }

    fn open(
        &self,
        request: &StreamRequest,
        make: MakeCallback<'_>,
    ) -> Result<Box<dyn AudioStream>, String> {
        let devices = self.devices()?;
        let default_output = devices.iter().find(|d| d.is_default_output);
        let (output, fallback) = match &request.output {
            None => (default_output, false),
            Some(id) => match devices.iter().find(|d| &d.id == id) {
                Some(device) => (Some(device), false),
                None => (default_output, true),
            },
        };
        let output = output.ok_or("there's no output device")?;
        let input = if request.wants_input {
            let default_input = devices.iter().find(|d| d.is_default_input);
            match &request.input {
                None => default_input,
                Some(id) => devices.iter().find(|d| &d.id == id).or(default_input),
            }
        } else {
            None
        };
        let sample_rate = request
            .sample_rate
            .or(output.current_rate)
            .unwrap_or(48_000);
        if !output.sample_rates.contains(&sample_rate) {
            return Err(format!("{} can't run at {sample_rate} Hz", output.name));
        }
        let info = StreamInfo {
            output_id: output.id.clone(),
            output_name: output.name.clone(),
            input_name: input.map(|d| d.name.clone()),
            sample_rate,
            buffer_size: request.buffer_size,
            max_frames: request.buffer_size.unwrap_or(DEFAULT_MAX_FRAMES),
            output_channels: output.outputs.min(2),
            input_channels: input.map_or(0, |d| d.inputs.min(2)),
            follows_default: request.output.is_none() || fallback,
            fallback,
        };
        let callback = make(&info)?;
        let health = StreamHealth::new();
        let mut state = self.state.borrow_mut();
        let serial = state.next;
        state.next += 1;
        state.opened += 1;
        state.running = Some(Running {
            serial,
            callback,
            info: info.clone(),
            health: health.clone(),
        });
        Ok(Box::new(FakeStream {
            state: self.state.clone(),
            serial,
            info,
            health,
        }))
    }
}

/// A fake stream: it runs until it's dropped.
struct FakeStream {
    state: Rc<RefCell<State>>,
    serial: u64,
    info: StreamInfo,
    health: Arc<StreamHealth>,
}

impl AudioStream for FakeStream {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn health(&self) -> &StreamHealth {
        &self.health
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        let mut state = self.state.borrow_mut();
        if state
            .running
            .as_ref()
            .is_some_and(|r| r.serial == self.serial)
        {
            state.running = None;
        }
    }
}
