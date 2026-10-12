//! A CLAP host for tests: load a plug-in, read its parameters and ports,
//! activate it, and run blocks of audio and events through it.
//!
//! Each process call runs as a real-time section under the allocation
//! trap when the test crate has installed it (`lspk_rt::install_trap!()`),
//! and fails the test if the host's side or the plug-in touches the heap.
//! The trap sees only the test binary's own allocator, so a plug-in loaded
//! from a `.clap` file is checked only on the host's side; load it from its
//! entry point ([`ClapTestHost::from_entry`]) to check the plug-in too.
//!
//! [`ClapTestHost::render`] plays a [`Script`] as the in-process harness
//! does ([`crate::render`](mod@crate::render)), so a test can render the same script both
//! ways and compare.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, PoisonError};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::latency::{HostLatency, HostLatencyImpl, PluginLatency};
use clack_extensions::note_ports::{NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamInfoBuffer,
    ParamInfoFlags, ParamRescanFlags, PluginParams,
};
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_extensions::tail::PluginTail;
use clack_extensions::track_info::{
    HostTrackInfo, HostTrackInfoImpl, PluginTrackInfo, TrackInfo, TrackInfoWriter,
};
use clack_host::entry::EntryDescriptor;
use clack_host::events::event_types::{
    MidiEvent, NoteChokeEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent,
};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{Match, Pckn};
use clack_host::prelude::*;
use lspk_params::ParamDescriptor;
use lspk_plugin::Event;

use crate::cpu::Timings;
use crate::render::{Output, Render, Script};

pub use crate::render::Requests;
pub use clack_host::process::ProcessStatus;

/// An event the test sends the plug-in, at its frame in the block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostEvent {
    /// A note starts.
    NoteOn {
        /// Its frame.
        time: u32,
        /// Its key.
        key: u16,
        /// Its channel.
        channel: u16,
        /// Its note ID, if any.
        note_id: Option<u32>,
        /// Its velocity, from 0 to 1.
        velocity: f64,
    },
    /// Notes are released; `None` fields match every value.
    NoteOff {
        /// Its frame.
        time: u32,
        /// The key, or every key.
        key: Option<u16>,
        /// The channel, or every channel.
        channel: Option<u16>,
        /// The note ID, or every note.
        note_id: Option<u32>,
    },
    /// Notes stop at once.
    NoteChoke {
        /// Its frame.
        time: u32,
        /// The key, or every key.
        key: Option<u16>,
    },
    /// A MIDI 1.0 message.
    Midi {
        /// Its frame.
        time: u32,
        /// Its three bytes.
        data: [u8; 3],
    },
    /// A parameter takes a value, as the host sees it.
    Param {
        /// Its frame.
        time: u32,
        /// The parameter's ID.
        id: u32,
        /// The value.
        value: f64,
    },
}

/// An event the plug-in sent the host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PluginEvent {
    /// A gesture starts.
    GestureBegin {
        /// Its frame.
        time: u32,
        /// The parameter's ID.
        id: u32,
    },
    /// A parameter took a value, as the host sees it.
    Param {
        /// Its frame.
        time: u32,
        /// The parameter's ID.
        id: u32,
        /// The value.
        value: f64,
    },
    /// A gesture ends.
    GestureEnd {
        /// Its frame.
        time: u32,
        /// The parameter's ID.
        id: u32,
    },
    /// A voice finished.
    NoteEnd {
        /// Its frame.
        time: u32,
        /// Its key, if it named one.
        key: Option<u16>,
    },
    /// Anything else.
    Other,
}

/// A parameter as the plug-in describes it to the host.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    /// Its ID.
    pub id: u32,
    /// Its name.
    pub name: String,
    /// Its lowest value, as the host sees it.
    pub min: f64,
    /// Its highest value.
    pub max: f64,
    /// Its default.
    pub default: f64,
    /// Whether it takes whole steps.
    pub stepped: bool,
    /// Whether it's a list of named choices.
    pub is_enum: bool,
    /// Whether automation can move it.
    pub automatable: bool,
}

/// One processed block.
#[derive(Clone, Debug)]
pub struct Block {
    /// Each output channel.
    pub outputs: Vec<Vec<f32>>,
    /// What the plug-in returned.
    pub status: ProcessStatus,
    /// What it sent the host.
    pub events: Vec<PluginEvent>,
}

/// A plug-in instance in a test host.
pub struct ClapTestHost {
    // Field order matters: the processor and the instance go before the
    // entry they came from.
    processor: Option<StartedPluginAudioProcessor<TestHost>>,
    instance: PluginInstance<TestHost>,
    _entry: PluginEntry,
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    input_ports: AudioPorts,
    output_ports: AudioPorts,
    in_events: EventBuffer,
    out_events: EventBuffer,
    steady: u64,
    sample_rate: f64,
    max_frames: usize,
}

/// How many events a block may carry each way.
const EVENT_CAPACITY: usize = 4096;

impl ClapTestHost {
    /// Loads plug-in `id` from the CLAP file at `path`.
    ///
    /// # Errors
    ///
    /// The file didn't load, or holds no plug-in `id`.
    pub fn load(path: &Path, id: &str) -> Result<Self, String> {
        // SAFETY: loading a plug-in runs its code in this process; tests
        // load only plug-ins built from source in this workspace, and clack
        // checks the entry point and the CLAP version.
        let entry = unsafe { PluginEntry::load(path) }.map_err(|e| e.to_string())?;
        Self::new(entry, id)
    }

    /// Instantiates plug-in `id` from `entry`, a CLAP entry point in this
    /// binary, such as the `clap_entry` that `lspk_plugin::export_clap!`
    /// exports.
    ///
    /// # Errors
    ///
    /// The entry holds no plug-in `id`, or it failed to start.
    pub fn from_entry(entry: &'static EntryDescriptor, id: &str) -> Result<Self, String> {
        let path = CString::new(
            std::env::current_exe()
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        // SAFETY: the entry is a Rust static made by clack-plugin, which
        // upholds the CLAP contract.
        let entry =
            unsafe { PluginEntry::load_from_raw(entry, &path) }.map_err(|e| e.to_string())?;
        Self::new(entry, id)
    }

    fn new(entry: PluginEntry, id: &str) -> Result<Self, String> {
        let info = HostInfo::new("plug-in kit tests", "Loombridge", "", "0.0.0")
            .map_err(|e| e.to_string())?;
        let id = CString::new(id).map_err(|e| e.to_string())?;
        let instance = PluginInstance::<TestHost>::new(
            |()| Shared::default(),
            |_| MainThread::default(),
            &entry,
            &id,
            &info,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            processor: None,
            instance,
            _entry: entry,
            inputs: Vec::new(),
            outputs: Vec::new(),
            input_ports: AudioPorts::with_capacity(0, 0),
            output_ports: AudioPorts::with_capacity(0, 0),
            in_events: EventBuffer::with_capacity(EVENT_CAPACITY),
            out_events: EventBuffer::with_capacity(EVENT_CAPACITY),
            steady: 0,
            sample_rate: 0.0,
            max_frames: 0,
        })
    }

    /// Every parameter, in the plug-in's order.
    #[must_use]
    pub fn params(&mut self) -> Vec<ParamInfo> {
        let plugin = self.instance.plugin_handle();
        let Some(params) = plugin.get_extension::<PluginParams>() else {
            return Vec::new();
        };
        let mut buffer = ParamInfoBuffer::new();
        (0..params.count(&plugin))
            .filter_map(|index| {
                let info = params.get_info(&plugin, index, &mut buffer)?;
                Some(ParamInfo {
                    id: info.id.get(),
                    name: String::from_utf8_lossy(info.name).into_owned(),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
                    is_enum: info.flags.contains(ParamInfoFlags::IS_ENUM),
                    automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
                })
            })
            .collect()
    }

    /// Parameter `id`'s value, as the host sees it.
    #[must_use]
    pub fn value(&mut self, id: u32) -> Option<f64> {
        let plugin = self.instance.plugin_handle();
        let params = plugin.get_extension::<PluginParams>()?;
        params.get_value(&plugin, ClapId::new(id))
    }

    /// How the plug-in shows `value` of parameter `id`.
    #[must_use]
    pub fn text(&mut self, id: u32, value: f64) -> Option<String> {
        let plugin = self.instance.plugin_handle();
        let params = plugin.get_extension::<PluginParams>()?;
        let mut buffer = [0u8; 256];
        let text = params
            .value_to_text(&plugin, ClapId::new(id), value, &mut buffer)
            .ok()?;
        Some(String::from_utf8_lossy(text).into_owned())
    }

    /// The value, as the host sees it, that `text` names for parameter `id`.
    #[must_use]
    pub fn parse(&mut self, id: u32, text: &str) -> Option<f64> {
        let plugin = self.instance.plugin_handle();
        let params = plugin.get_extension::<PluginParams>()?;
        let text = CString::new(text).ok()?;
        params.text_to_value(&plugin, ClapId::new(id), &text)
    }

    /// Each audio port's channel count, inputs or outputs.
    #[must_use]
    pub fn audio_ports(&mut self, is_input: bool) -> Vec<u32> {
        let plugin = self.instance.plugin_handle();
        let Some(ports) = plugin.get_extension::<PluginAudioPorts>() else {
            return Vec::new();
        };
        let mut buffer = AudioPortInfoBuffer::new();
        (0..ports.count(&plugin, is_input))
            .filter_map(|i| Some(ports.get(&plugin, i, is_input, &mut buffer)?.channel_count))
            .collect()
    }

    /// How many note ports it has, inputs or outputs.
    #[must_use]
    pub fn note_ports(&mut self, is_input: bool) -> u32 {
        let plugin = self.instance.plugin_handle();
        let Some(ports) = plugin.get_extension::<PluginNotePorts>() else {
            return 0;
        };
        let mut buffer = NotePortInfoBuffer::new();
        (0..ports.count(&plugin, is_input))
            .filter(|i| ports.get(&plugin, *i, is_input, &mut buffer).is_some())
            .count() as u32
    }

    /// Its latency, in frames.
    #[must_use]
    pub fn latency(&mut self) -> Option<u32> {
        let plugin = self.instance.plugin_handle();
        let latency = plugin.get_extension::<PluginLatency>()?;
        Some(latency.get(&plugin))
    }

    /// Its tail, in frames (`u32::MAX` for an endless one), while active.
    #[must_use]
    pub fn tail(&mut self) -> Option<u32> {
        let tail = self
            .instance
            .plugin_handle()
            .get_extension::<PluginTail>()?;
        let processor = self.processor.as_mut()?;
        Some(tail.get(&processor.plugin_handle()).to_raw())
    }

    /// Saves its state.
    ///
    /// # Errors
    ///
    /// It has no state extension, or refused.
    pub fn save_state(&mut self) -> Result<Vec<u8>, String> {
        let plugin = self.instance.plugin_handle();
        let state = plugin
            .get_extension::<PluginState>()
            .ok_or("no state extension")?;
        let mut bytes = Vec::new();
        state
            .save(&plugin, &mut bytes)
            .map_err(|e| format!("{e:?}"))?;
        Ok(bytes)
    }

    /// Loads a state.
    ///
    /// # Errors
    ///
    /// It has no state extension, or refused the state.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let plugin = self.instance.plugin_handle();
        let state = plugin
            .get_extension::<PluginState>()
            .ok_or("no state extension")?;
        state
            .load(&plugin, &mut &bytes[..])
            .map_err(|e| format!("{e:?}"))
    }

    /// Names the track the plug-in is on, as `track-info` reports it, and
    /// tells the plug-in it changed.
    pub fn set_track_name(&mut self, name: &str) {
        self.instance
            .access_handler(|m| m.track.set(Some(name.as_bytes().to_vec().leak())));
        let handle = self.instance.plugin_handle();
        if let Some(track_info) = handle.get_extension::<PluginTrackInfo>() {
            track_info.changed(&handle);
        }
    }

    /// Calls the plug-in back on the main thread, as after it asked.
    pub fn on_main_thread(&mut self) {
        self.instance.call_on_main_thread_callback();
    }

    /// What the plug-in has asked of the host since the last call.
    pub fn take_requests(&mut self) -> Requests {
        let (flush, restart, process, callback) = self.instance.access_shared_handler(|s| {
            (
                s.flush.swap(false, Relaxed),
                s.restart.swap(false, Relaxed),
                s.process.swap(false, Relaxed),
                s.callback.swap(false, Relaxed),
            )
        });
        let dirty = self.instance.access_handler(|m| m.dirty.replace(false));
        Requests {
            flush,
            dirty,
            restart,
            process,
            callback,
        }
    }

    /// Activates it at `sample_rate` for blocks of up to `max_frames`, with
    /// as many channels as its main ports have, and starts processing.
    ///
    /// # Errors
    ///
    /// It refused to activate or start.
    pub fn activate(&mut self, sample_rate: f64, max_frames: u32) -> Result<(), String> {
        let inputs = self.audio_ports(true).first().copied().unwrap_or(0) as usize;
        let outputs = self.audio_ports(false).first().copied().unwrap_or(0) as usize;
        let frames = max_frames as usize;
        self.sample_rate = sample_rate;
        self.max_frames = frames;
        self.inputs = vec![vec![0.0; frames]; inputs];
        self.outputs = vec![vec![0.0; frames]; outputs];
        self.input_ports = AudioPorts::with_capacity(inputs, 1);
        self.output_ports = AudioPorts::with_capacity(outputs, 1);
        let config = PluginAudioConfiguration {
            sample_rate,
            min_frames_count: 1,
            max_frames_count: max_frames,
        };
        let stopped = self
            .instance
            .activate(|_, _| (), config)
            .map_err(|e| e.to_string())?;
        let started = stopped
            .start_processing()
            .map_err(|_| "it refused to start processing".to_owned())?;
        self.processor = Some(started);
        Ok(())
    }

    /// Stops and deactivates it.
    pub fn deactivate(&mut self) {
        if let Some(processor) = self.processor.take() {
            self.instance.deactivate(processor.stop_processing());
        }
    }

    /// Whether it's active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.processor.is_some()
    }

    /// Runs one block of `frames` frames, with `input` (or silence) on
    /// every input channel and `events` (in time order).
    ///
    /// # Panics
    ///
    /// If it isn't active, the block is longer than it was activated for,
    /// processing fails, or, with the trap installed, the block touches the
    /// heap.
    pub fn process(&mut self, frames: usize, input: Option<&[f32]>, events: &[HostEvent]) -> Block {
        let inputs: &[&[f32]] = match &input {
            Some(input) => std::slice::from_ref(input),
            None => &[],
        };
        self.process_channels(frames, inputs, events)
    }

    /// Runs one block of `frames` frames, with `inputs` (one slice per
    /// input channel, the first repeated on any channel beyond them, and
    /// silence past a slice's end; empty for silence) and `events` (in time
    /// order).
    ///
    /// # Panics
    ///
    /// As for [`ClapTestHost::process`].
    pub fn process_channels(
        &mut self,
        frames: usize,
        inputs: &[&[f32]],
        events: &[HostEvent],
    ) -> Block {
        let (status, _) = self.run(frames, inputs, events);
        Block {
            outputs: self.outputs.iter().map(|c| c[..frames].to_vec()).collect(),
            status,
            events: self.out_events.iter().map(plugin_event).collect(),
        }
    }

    /// Plays `script` in blocks of `block` frames, from wherever the
    /// plug-in is now, at the sample rate it was activated at. `params` are
    /// the plug-in's parameters (its `Plugin::params`), to send the
    /// script's values, in their own units, as the host sees them.
    ///
    /// # Panics
    ///
    /// As for [`ClapTestHost::process`], or if `block` is 0.
    pub fn render(
        &mut self,
        script: &Script,
        block: usize,
        params: &[ParamDescriptor],
    ) -> Render<PluginEvent> {
        assert!(block > 0, "a block must hold at least one frame");
        let channels = self.outputs.len();
        let mut output = vec![Vec::with_capacity(script.frames()); channels];
        let mut events = Vec::new();
        let mut statuses = Vec::new();
        let mut timings = Timings::new(self.sample_rate);
        let mut host_events = Vec::new();
        let mut inputs = vec![Vec::new(); self.inputs.len()];
        let mut from = 0;
        while from < script.frames() {
            let to = (from + block).min(script.frames());
            let frames = to - from;
            host_events.clear();
            host_events.extend(
                script
                    .block_events(from, to)
                    .filter_map(|e| host_event(&e, params)),
            );
            for (channel, input) in inputs.iter_mut().enumerate() {
                input.resize(frames, 0.0);
                script.fill_input(channel, from, input);
            }
            let slices: Vec<&[f32]> = inputs.iter().map(Vec::as_slice).collect();
            let (status, duration) = self.run(frames, &slices, &host_events);
            for (out, channel) in output.iter_mut().zip(&self.outputs) {
                out.extend_from_slice(&channel[..frames]);
            }
            let offset = from as u32;
            events.extend(
                self.out_events
                    .iter()
                    .map(|e| retime_plugin_event(plugin_event(e), offset)),
            );
            statuses.push(status_of(status));
            timings.push(frames, duration);
            from = to;
        }
        Render {
            output: Output::new(self.sample_rate, output),
            events,
            statuses,
            timings,
            dropped_events: 0,
        }
    }

    /// Runs one block, leaving its output in `self.outputs` and what the
    /// plug-in sent in `self.out_events`.
    fn run(
        &mut self,
        frames: usize,
        inputs: &[&[f32]],
        events: &[HostEvent],
    ) -> (ProcessStatus, std::time::Duration) {
        assert!(
            frames <= self.max_frames,
            "a block of {frames} frames, more than the {} it was activated for",
            self.max_frames
        );
        self.in_events.clear();
        self.out_events.clear();
        for event in events {
            push(&mut self.in_events, event);
        }
        for (index, channel) in self.inputs.iter_mut().enumerate() {
            let source = inputs.get(index).or_else(|| inputs.first()).copied();
            let source = source.unwrap_or(&[]);
            let n = source.len().min(frames);
            channel[..n].copy_from_slice(&source[..n]);
            channel[n..frames].fill(0.0);
        }
        for channel in &mut self.outputs {
            channel[..frames].fill(0.0);
        }
        let processor = self.processor.as_mut().expect("an active plug-in");
        let ins = self
            .input_ports
            .with_input_buffers((!self.inputs.is_empty()).then(|| {
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        self.inputs
                            .iter_mut()
                            .map(|c| InputChannel::variable(&mut c[..frames])),
                    ),
                }
            }));
        let mut outs = self
            .output_ports
            .with_output_buffers((!self.outputs.is_empty()).then(|| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    self.outputs.iter_mut().map(|c| &mut c[..frames]),
                ),
            }));
        let in_events = InputEvents::from_buffer(&self.in_events);
        let mut out_events = OutputEvents::from_buffer(&mut self.out_events);
        let steady = self.steady;
        let mut run = || {
            let start = std::time::Instant::now();
            let result = processor.process(
                &ins,
                &mut outs,
                &in_events,
                &mut out_events,
                Some(steady),
                None,
            );
            (result, start.elapsed())
        };
        // A real-time section either way, so denormals are flushed and a
        // render is the same with the trap or without.
        let (result, duration) = if lspk_rt::trap::is_installed() {
            lspk_rt::rt_test!(run())
        } else {
            lspk_rt::realtime(run)
        };
        let status = result.expect("processing succeeds");
        self.steady += frames as u64;
        (status, duration)
    }

    /// Flushes parameters with `events`: through the audio thread's flush
    /// while active, the main thread's otherwise. Returns what the plug-in
    /// sent.
    pub fn flush(&mut self, events: &[HostEvent]) -> Vec<PluginEvent> {
        self.in_events.clear();
        self.out_events.clear();
        for event in events {
            push(&mut self.in_events, event);
        }
        let params = self
            .instance
            .plugin_handle()
            .get_extension::<PluginParams>();
        if let Some(params) = params {
            let input = InputEvents::from_buffer(&self.in_events);
            let mut output = OutputEvents::from_buffer(&mut self.out_events);
            if let Some(processor) = self.processor.as_mut() {
                params.flush_active(&mut processor.plugin_handle(), &input, &mut output);
            } else if let Some(mut plugin) = self.instance.inactive_plugin_handle() {
                params.flush(&mut plugin, &input, &mut output);
            }
        }
        self.out_events.iter().map(plugin_event).collect()
    }
}

impl Drop for ClapTestHost {
    fn drop(&mut self) {
        self.deactivate();
    }
}

/// A script's `event`, in the parameter's own units, as this host sends
/// it; `None` for a parameter `params` doesn't describe.
fn host_event(event: &Event, params: &[ParamDescriptor]) -> Option<HostEvent> {
    let key = |note: &lspk_plugin::Note| note.key.map(u16::from);
    let channel = |note: &lspk_plugin::Note| note.channel.map(u16::from);
    Some(match *event {
        Event::NoteOn {
            time,
            note,
            velocity,
        } => HostEvent::NoteOn {
            time,
            key: key(&note)?,
            channel: channel(&note).unwrap_or(0),
            note_id: note.id,
            velocity: f64::from(velocity),
        },
        Event::NoteOff { time, note, .. } => HostEvent::NoteOff {
            time,
            key: key(&note),
            channel: channel(&note),
            note_id: note.id,
        },
        Event::NoteChoke { time, note } => HostEvent::NoteChoke {
            time,
            key: key(&note),
        },
        Event::Param { time, id, value } => {
            let descriptor = params.iter().find(|d| d.id == id)?;
            HostEvent::Param {
                time,
                id: id.0,
                value: to_host(descriptor, value),
            }
        }
        _ => return None,
    })
}

/// `value`, in the parameter's own units, as a CLAP host sees it: a stepped
/// parameter's whole-number value, or a continuous one's travel from 0 to
/// 1, as the kit's CLAP export maps it.
#[must_use]
pub fn to_host(descriptor: &ParamDescriptor, value: f64) -> f64 {
    if descriptor.is_stepped() {
        descriptor.clamp(value)
    } else {
        descriptor.normalize(value)
    }
}

/// A host's `value` for a parameter, in the parameter's own units.
#[must_use]
pub fn from_host(descriptor: &ParamDescriptor, value: f64) -> f64 {
    if descriptor.is_stepped() {
        descriptor.clamp(value)
    } else {
        descriptor.denormalize(value)
    }
}

fn status_of(status: ProcessStatus) -> lspk_plugin::ProcessStatus {
    match status {
        ProcessStatus::Continue => lspk_plugin::ProcessStatus::Continue,
        ProcessStatus::ContinueIfNotQuiet => lspk_plugin::ProcessStatus::ContinueIfNotQuiet,
        ProcessStatus::Tail => lspk_plugin::ProcessStatus::Tail,
        ProcessStatus::Sleep => lspk_plugin::ProcessStatus::Sleep,
    }
}

/// `event` moved `offset` frames later.
fn retime_plugin_event(event: PluginEvent, offset: u32) -> PluginEvent {
    let later = |time: u32| time.saturating_add(offset);
    match event {
        PluginEvent::GestureBegin { time, id } => PluginEvent::GestureBegin {
            time: later(time),
            id,
        },
        PluginEvent::Param { time, id, value } => PluginEvent::Param {
            time: later(time),
            id,
            value,
        },
        PluginEvent::GestureEnd { time, id } => PluginEvent::GestureEnd {
            time: later(time),
            id,
        },
        PluginEvent::NoteEnd { time, key } => PluginEvent::NoteEnd {
            time: later(time),
            key,
        },
        PluginEvent::Other => PluginEvent::Other,
    }
}

fn pckn(key: Option<u16>, channel: Option<u16>, note_id: Option<u32>) -> Pckn {
    Pckn::new(
        0u16,
        channel.map_or(Match::All, Match::Specific),
        key.map_or(Match::All, Match::Specific),
        note_id.map_or(Match::All, Match::Specific),
    )
}

fn push(buffer: &mut EventBuffer, event: &HostEvent) {
    match *event {
        HostEvent::NoteOn {
            time,
            key,
            channel,
            note_id,
            velocity,
        } => buffer.push(&NoteOnEvent::new(
            time,
            pckn(Some(key), Some(channel), note_id),
            velocity,
        )),
        HostEvent::NoteOff {
            time,
            key,
            channel,
            note_id,
        } => buffer.push(&NoteOffEvent::new(time, pckn(key, channel, note_id), 0.0)),
        HostEvent::NoteChoke { time, key } => {
            buffer.push(&NoteChokeEvent::new(time, pckn(key, None, None)));
        }
        HostEvent::Midi { time, data } => buffer.push(&MidiEvent::new(time, 0, data)),
        HostEvent::Param { time, id, value } => buffer.push(&ParamValueEvent::new(
            time,
            ClapId::new(id),
            Pckn::match_all(),
            value,
        )),
    }
}

fn plugin_event(event: &UnknownEvent) -> PluginEvent {
    let time = event.header().time();
    match event.as_core_event() {
        Some(CoreEventSpace::ParamGestureBegin(e)) => PluginEvent::GestureBegin {
            time,
            id: e.param_id().map_or(u32::MAX, |id| id.get()),
        },
        Some(CoreEventSpace::ParamGestureEnd(e)) => PluginEvent::GestureEnd {
            time,
            id: e.param_id().map_or(u32::MAX, |id| id.get()),
        },
        Some(CoreEventSpace::ParamValue(e)) => PluginEvent::Param {
            time,
            id: e.param_id().map_or(u32::MAX, |id| id.get()),
            value: e.value(),
        },
        Some(CoreEventSpace::NoteEnd(e)) => PluginEvent::NoteEnd {
            time,
            key: e.pckn().key.into_specific(),
        },
        _ => PluginEvent::Other,
    }
}

/// The plug-in, as a CLAP file made from a plug-in crate's `cdylib` that
/// Cargo built beside the running test binary: `<name>.clap`, in a folder
/// of this process's own under the build folder. It's written once per
/// process, since a loaded library can't be replaced on Windows.
///
/// # Panics
///
/// If the library isn't there or the file can't be written.
#[must_use]
pub fn bundle_built_library(lib_name: &str, name: &str, id: &str) -> PathBuf {
    static WRITTEN: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());
    let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(clap) = written.get(lib_name) {
        return clap.clone();
    }
    let library = built_library(lib_name);
    let target = library
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|n| n == "deps"))
        .and_then(Path::parent)
        .unwrap_or_else(|| library.parent().unwrap_or(Path::new(".")));
    let dir = target
        .join("clap-tests")
        .join(std::process::id().to_string());
    let clap = lspk_plugin::bundle::write_clap(&library, &dir, name, id).expect("the CLAP file");
    written.insert(lib_name.to_owned(), clap.clone());
    clap
}

/// The newest library `lib_name` Cargo built beside the running test
/// binary.
fn built_library(lib_name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's path");
    let deps = exe.parent().expect("the test binary's folder");
    let prefix = format!("{}{lib_name}", std::env::consts::DLL_PREFIX);
    let suffix = std::env::consts::DLL_SUFFIX;
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = [deps, deps.parent().unwrap_or(deps)]
        .iter()
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(suffix))
        })
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .collect();
    found.sort();
    found
        .pop()
        .map(|(_, path)| path)
        .unwrap_or_else(|| panic!("no {prefix}*{suffix} beside the tests: is its crate a cdylib?"))
}

/// The test host.
struct TestHost;

impl HostHandlers for TestHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<'_, Self>, _shared: &Shared) {
        builder
            .register::<HostParams>()
            .register::<HostState>()
            .register::<HostLatency>()
            .register::<HostTrackInfo>();
    }
}

/// Requests from any thread.
#[derive(Default)]
struct Shared {
    flush: AtomicBool,
    restart: AtomicBool,
    process: AtomicBool,
    callback: AtomicBool,
}

impl SharedHandler<'_> for Shared {
    fn request_restart(&self) {
        self.restart.store(true, Relaxed);
    }

    fn request_process(&self) {
        self.process.store(true, Relaxed);
    }

    fn request_callback(&self) {
        self.callback.store(true, Relaxed);
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {
        self.flush.store(true, Relaxed);
    }
}

/// Requests on the main thread.
#[derive(Default)]
struct MainThread {
    dirty: Cell<bool>,
    /// The track name `track-info` reports, if any. Leaked, as clack's
    /// writer wants the name to live as long as the host's handler.
    track: Cell<Option<&'static [u8]>>,
}

impl MainThreadHandler<'_> for MainThread {}

impl HostParamsImplMainThread for MainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {}

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for MainThread {
    fn mark_dirty(&self) {
        self.dirty.set(true);
    }
}

impl HostLatencyImpl for MainThread {
    fn changed(&self) {}
}

impl HostTrackInfoImpl for MainThread {
    fn get<'a>(&'a self, writer: &mut TrackInfoWriter<'_, 'a>) {
        if let Some(track) = self.track.get() {
            writer.set(&TrackInfo::new().with_name(Some(track)));
        }
    }
}
