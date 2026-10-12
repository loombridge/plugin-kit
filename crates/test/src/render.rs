//! The render harness: play a [`Script`] (notes, parameter changes and
//! audio input, each at its frame) through a plug-in at chosen
//! [`Settings`] (sample rate and block size), and get its [`Output`].
//!
//! - **In this process**, through the kit's traits: [`render()`] for one
//!   render with a new instance, or an [`Instance`] to keep between
//!   renders, edit parameters as the plug-in would, and run single blocks.
//!   The plug-in's code runs in the test binary, so the allocation trap
//!   checks it.
//! - **From a built `.clap`**, through the CLAP test host:
//!   [`ClapTestHost::render`](crate::clap::ClapTestHost::render) takes the
//!   same script, so a test can show the two give the same audio.
//!
//! The harness calls `process` for every block, whatever status the plug-in
//! returned last, so a render's length never depends on when a plug-in
//! sleeps; [`Render::statuses`] records what it asked for.
//!
//! [`block_invariance`] renders the same script at several block sizes
//! ([`BLOCK_SIZES`]: 1, 32, 64 and 512 frames) and compares them.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lspk_plugin::host::Edit;
use lspk_plugin::{
    Buffers, Event, EventSink, Events, Host, HostBackend, Note, OutputEvent, ParamId, ParamValues,
    Plugin, ProcessConfig, ProcessContext, ProcessStatus, Processor,
};
use rtrb::Consumer;

use crate::clap::Requests;
use crate::cpu::Timings;

/// The block sizes [`block_invariance`] checks by default.
pub const BLOCK_SIZES: [usize; 4] = [1, 32, 64, 512];

/// How many events a block can send the host; more are dropped and counted
/// in [`Render::dropped_events`].
const OUTPUT_CAPACITY: usize = 4096;

/// The sample rate and block size to render at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Frames per second.
    pub sample_rate: f64,
    /// Frames per block; the last block of a render may be shorter.
    pub block: usize,
}

impl Settings {
    /// `block`-frame blocks at `sample_rate`.
    #[must_use]
    pub const fn new(sample_rate: f64, block: usize) -> Self {
        Self { sample_rate, block }
    }

    /// The same sample rate in `block`-frame blocks.
    #[must_use]
    pub const fn with_block(self, block: usize) -> Self {
        Self { block, ..self }
    }
}

/// What to play: a length, events at their frames, and audio input.
///
/// Event times are frames from the start of the render. Events at the same
/// frame keep the order they were added in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    frames: usize,
    events: Vec<Event>,
    input: Vec<Vec<f32>>,
}

impl Script {
    /// `frames` frames of silence with no events.
    #[must_use]
    pub fn new(frames: usize) -> Self {
        Self {
            frames,
            events: Vec::new(),
            input: Vec::new(),
        }
    }

    /// How many frames it lasts.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Its events, in time order, each timed from the start.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Its input channels.
    #[must_use]
    pub fn input(&self) -> &[Vec<f32>] {
        &self.input
    }

    /// With `channels` as the audio input: an effect's input channel `c`
    /// takes channel `c`, or channel 0 if there are fewer. Past the end of a
    /// channel, the input is silent.
    #[must_use]
    pub fn with_input(mut self, channels: Vec<Vec<f32>>) -> Self {
        self.input = channels;
        self
    }

    /// With `signal` on every input channel.
    #[must_use]
    pub fn with_mono_input(self, signal: Vec<f32>) -> Self {
        self.with_input(vec![signal])
    }

    /// With `event`, timed from the start of the render.
    #[must_use]
    pub fn event(mut self, event: Event) -> Self {
        let at = self.events.partition_point(|e| e.time() <= event.time());
        self.events.insert(at, event);
        self
    }

    /// With key `key` starting at `frame` on channel 0 at `velocity`
    /// (from 0 to 1).
    #[must_use]
    pub fn note_on(self, frame: u32, key: u8, velocity: f32) -> Self {
        self.event(Event::NoteOn {
            time: frame,
            note: Note::key(0, key),
            velocity,
        })
    }

    /// With key `key` released at `frame` on channel 0.
    #[must_use]
    pub fn note_off(self, frame: u32, key: u8) -> Self {
        self.event(Event::NoteOff {
            time: frame,
            note: Note::key(0, key),
            velocity: 0.0,
        })
    }

    /// With key `key` held from `frame` for `length` frames.
    #[must_use]
    pub fn note(self, frame: u32, length: u32, key: u8, velocity: f32) -> Self {
        self.note_on(frame, key, velocity)
            .note_off(frame.saturating_add(length), key)
    }

    /// With key `key` stopped at once at `frame`.
    #[must_use]
    pub fn choke(self, frame: u32, key: u8) -> Self {
        self.event(Event::NoteChoke {
            time: frame,
            note: Note::key(0, key),
        })
    }

    /// With parameter `id` set to `value`, in its own units, at `frame`, as
    /// the host's automation would.
    #[must_use]
    pub fn param(self, frame: u32, id: ParamId, value: f64) -> Self {
        self.event(Event::Param {
            time: frame,
            id,
            value,
        })
    }

    /// Input channel `channel` from `from`, `frames` frames long, into
    /// `out`; silent where the script has none.
    pub(crate) fn fill_input(&self, channel: usize, from: usize, out: &mut [f32]) {
        let source = self.input.get(channel).or_else(|| self.input.first());
        let available = source.map_or(&[][..], |s| s.get(from..).unwrap_or(&[]));
        let n = available.len().min(out.len());
        out[..n].copy_from_slice(&available[..n]);
        out[n..].fill(0.0);
    }

    /// The events in frames `from..to`, retimed from `from`.
    pub(crate) fn block_events(&self, from: usize, to: usize) -> impl Iterator<Item = Event> + '_ {
        let start = self.events.partition_point(|e| (e.time() as usize) < from);
        self.events[start..]
            .iter()
            .take_while(move |e| (e.time() as usize) < to)
            .map(move |e| retime(*e, e.time() - from as u32))
    }
}

/// `event` at `time` instead.
pub(crate) fn retime(event: Event, time: u32) -> Event {
    match event {
        Event::NoteOn { note, velocity, .. } => Event::NoteOn {
            time,
            note,
            velocity,
        },
        Event::NoteOff { note, velocity, .. } => Event::NoteOff {
            time,
            note,
            velocity,
        },
        Event::NoteChoke { note, .. } => Event::NoteChoke { time, note },
        Event::Param { id, value, .. } => Event::Param { time, id, value },
        other => other,
    }
}

/// Rendered audio: each channel's frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    /// Frames per second.
    pub sample_rate: f64,
    /// Each channel, all the same length.
    pub channels: Vec<Vec<f32>>,
}

impl Output {
    /// `channels` at `sample_rate`.
    #[must_use]
    pub fn new(sample_rate: f64, channels: Vec<Vec<f32>>) -> Self {
        Self {
            sample_rate,
            channels,
        }
    }

    /// How many frames it holds.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Channel `index`.
    ///
    /// # Panics
    ///
    /// If there's no channel `index`.
    #[must_use]
    pub fn channel(&self, index: usize) -> &[f32] {
        &self.channels[index]
    }

    /// Frames `from..to` (clamped to its length) of every channel.
    #[must_use]
    pub fn slice(&self, from: usize, to: usize) -> Self {
        let to = to.min(self.frames());
        let from = from.min(to);
        Self {
            sample_rate: self.sample_rate,
            channels: self.channels.iter().map(|c| c[from..to].to_vec()).collect(),
        }
    }

    /// Every frame's channels in turn, as audio files store them.
    #[must_use]
    pub fn interleaved(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.frames() * self.channels.len());
        for frame in 0..self.frames() {
            out.extend(self.channels.iter().map(|c| c[frame]));
        }
        out
    }

    /// The channels' average.
    #[must_use]
    pub fn mono(&self) -> Vec<f32> {
        let count = self.channels.len().max(1) as f64;
        (0..self.frames())
            .map(|frame| {
                let sum: f64 = self.channels.iter().map(|c| f64::from(c[frame])).sum();
                (sum / count) as f32
            })
            .collect()
    }

    /// Whether every sample is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.channels.iter().flatten().all(|s| s.is_finite())
    }

    /// Whether every sample is 0.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.channels.iter().flatten().all(|s| *s == 0.0)
    }

    /// Whether `other` has the same shape and the same samples to the bit.
    #[must_use]
    pub fn same_bits(&self, other: &Self) -> bool {
        self.channels.len() == other.channels.len()
            && self.channels.iter().zip(&other.channels).all(|(a, b)| {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
            })
    }

    /// The largest difference between any sample and the same sample of
    /// `other`, as a linear amplitude; infinite if their shapes differ.
    #[must_use]
    pub fn max_difference(&self, other: &Self) -> f64 {
        if self.channels.len() != other.channels.len()
            || self.frames() != other.frames()
            || self.channels.iter().any(|c| c.len() != self.frames())
            || other.channels.iter().any(|c| c.len() != other.frames())
        {
            return f64::INFINITY;
        }
        self.channels
            .iter()
            .zip(&other.channels)
            .flat_map(|(a, b)| a.iter().zip(b))
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs())
            .fold(0.0, f64::max)
    }
}

/// What a render made.
#[derive(Clone, Debug)]
pub struct Render<E = OutputEvent> {
    /// The audio.
    pub output: Output,
    /// What the plug-in sent the host, each timed from the start of the
    /// render: parameter changes and gestures (the plug-in's own edits
    /// first, at the start of the block after they were made), and voices
    /// that ended.
    pub events: Vec<E>,
    /// What each block's `process` returned.
    pub statuses: Vec<ProcessStatus>,
    /// How long each block's `process` call took.
    pub timings: Timings,
    /// Events the plug-in sent beyond what one block can carry.
    pub dropped_events: usize,
}

impl<E> Render<E> {
    /// Whether the plug-in asked to sleep after the last block.
    #[must_use]
    pub fn sleeps(&self) -> bool {
        self.statuses.last() == Some(&ProcessStatus::Sleep)
    }
}

/// One block's results from [`Instance::process`].
#[derive(Clone, Debug)]
pub struct Block {
    /// Each output channel.
    pub outputs: Vec<Vec<f32>>,
    /// What `process` returned.
    pub status: ProcessStatus,
    /// What the plug-in sent the host, timed in the block.
    pub events: Vec<OutputEvent>,
    /// How long `process` took.
    pub duration: Duration,
}

/// Renders `script` with a new instance of `P`, activated at `settings`.
///
/// # Errors
///
/// The plug-in's parameters are invalid, or it refused to activate.
///
/// # Panics
///
/// With the allocation trap installed, if a block touches the heap.
pub fn render<P: Plugin>(script: &Script, settings: Settings) -> Result<Render, String> {
    let mut instance = Instance::<P>::new()?;
    instance.activate(settings)?;
    Ok(instance.render(script))
}

/// A plug-in instance driven in this process, as a host drives it.
pub struct Instance<P: Plugin> {
    // Field order matters: the processor goes before the plug-in that made
    // it, and the plug-in before its host link.
    active: Option<Active<P::Processor>>,
    plugin: P,
    host: Host,
    values: Arc<ParamValues>,
    edits: Consumer<Edit>,
    requests: Rc<Recorder>,
}

/// An active instance's processor and buffers.
struct Active<T> {
    processor: T,
    settings: Settings,
    buffers: Vec<Vec<f32>>,
    events: Vec<Event>,
    sink: Collect,
}

impl<P: Plugin> Instance<P> {
    /// Makes an instance, as a host does when it loads the plug-in.
    ///
    /// # Errors
    ///
    /// The plug-in's parameters break a rule or repeat an ID.
    pub fn new() -> Result<Self, String> {
        let requests = Rc::new(Recorder::default());
        let host = Host::new(Box::new(Backend(requests.clone())));
        let plugin = P::new(host.clone());
        let values = Arc::new(ParamValues::new(plugin.params()).map_err(|e| e.to_string())?);
        let edits = host.connect(values.clone());
        Ok(Self {
            active: None,
            plugin,
            host,
            values,
            edits,
            requests,
        })
    }

    /// The plug-in, on the main thread.
    pub fn plugin(&self) -> &P {
        &self.plugin
    }

    /// The plug-in, to call on the main thread as a host would (its
    /// commands, say). Its parameter edits reach the processor at the
    /// start of the next block.
    pub fn plugin_mut(&mut self) -> &mut P {
        &mut self.plugin
    }

    /// The plug-in's link to this host, through which it edits its own
    /// parameters.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Every parameter's value now.
    pub fn values(&self) -> &ParamValues {
        &self.values
    }

    /// What the plug-in has asked of the host since the last call.
    pub fn take_requests(&mut self) -> Requests {
        self.requests.take()
    }

    /// Activates it at `settings`, for blocks of up to `settings.block`
    /// frames.
    ///
    /// # Errors
    ///
    /// The block size is 0, or the plug-in refused.
    pub fn activate(&mut self, settings: Settings) -> Result<(), String> {
        if settings.block == 0 {
            return Err("a block must hold at least one frame".to_owned());
        }
        self.deactivate();
        let max_frames = u32::try_from(settings.block).map_err(|e| e.to_string())?;
        let config = ProcessConfig::new(settings.sample_rate, max_frames);
        let processor = self
            .plugin
            .activate(&config, &self.values)
            .map_err(|e| e.to_string())?;
        let outputs = usize::from(P::INFO.audio.outputs).min(lspk_plugin::MAX_CHANNELS);
        self.active = Some(Active {
            processor,
            settings,
            buffers: vec![vec![0.0; settings.block]; outputs],
            events: Vec::with_capacity(Host::edit_capacity(&self.values) + 64),
            sink: Collect::with_capacity(OUTPUT_CAPACITY),
        });
        Ok(())
    }

    /// Deactivates it, handing the processor back to the plug-in.
    pub fn deactivate(&mut self) {
        if let Some(active) = self.active.take() {
            self.plugin.deactivate(active.processor);
        }
    }

    /// Whether it's active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// The settings it's active at.
    #[must_use]
    pub fn settings(&self) -> Option<Settings> {
        self.active.as_ref().map(|a| a.settings)
    }

    /// Tells the processor to forget what it was playing.
    pub fn reset(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.processor.reset();
        }
    }

    /// Renders `script` in blocks of the activated size, from wherever the
    /// processor is now.
    ///
    /// # Panics
    ///
    /// If it isn't active or, with the allocation trap installed, a block
    /// touches the heap.
    pub fn render(&mut self, script: &Script) -> Render {
        let settings = self.settings().expect("an active instance");
        let channels = self.active.as_ref().map_or(0, |a| a.buffers.len());
        let mut output = vec![Vec::with_capacity(script.frames()); channels];
        let mut events = Vec::new();
        let mut statuses = Vec::new();
        let mut timings = Timings::new(settings.sample_rate);
        let mut dropped = 0;
        let mut from = 0;
        while from < script.frames() {
            let to = (from + settings.block).min(script.frames());
            let frames = to - from;
            let (status, duration) = self.run_block(
                frames,
                |channel, out| {
                    script.fill_input(channel, from, out);
                },
                script.block_events(from, to),
            );
            let active = self.active.as_mut().expect("an active instance");
            for (out, buffer) in output.iter_mut().zip(&active.buffers) {
                out.extend_from_slice(&buffer[..frames]);
            }
            let offset = from as u32;
            events.extend(
                active
                    .sink
                    .events
                    .drain(..)
                    .map(|e| retime_output(e, offset)),
            );
            dropped += std::mem::take(&mut active.sink.dropped);
            statuses.push(status);
            timings.push(frames, duration);
            from = to;
        }
        Render {
            output: Output::new(settings.sample_rate, output),
            events,
            statuses,
            timings,
            dropped_events: dropped,
        }
    }

    /// Runs one block of `frames` frames, with `input` (one slice per input
    /// channel, the first repeated on any channel beyond them; empty for
    /// silence) and `events`, timed in the block and in time order.
    ///
    /// # Panics
    ///
    /// If it isn't active, `frames` is more than it was activated for, or,
    /// with the allocation trap installed, the block touches the heap.
    pub fn process(&mut self, frames: usize, input: &[&[f32]], events: &[Event]) -> Block {
        let (status, duration) = self.run_block(
            frames,
            |channel, out| {
                let source = input.get(channel).or_else(|| input.first());
                let available = source.copied().unwrap_or(&[]);
                let n = available.len().min(out.len());
                out[..n].copy_from_slice(&available[..n]);
                out[n..].fill(0.0);
            },
            events.iter().copied(),
        );
        let active = self.active.as_mut().expect("an active instance");
        Block {
            outputs: active
                .buffers
                .iter()
                .map(|b| b[..frames].to_vec())
                .collect(),
            status,
            // Copied, so the sink keeps the capacity it never grows past.
            events: active.sink.events.clone(),
            duration,
        }
    }

    /// Passes parameter `events` to the processor while it isn't
    /// processing, as a host's parameter flush does, and returns what it
    /// sent the host.
    ///
    /// # Panics
    ///
    /// If it isn't active.
    pub fn flush(&mut self, events: &[Event]) -> Vec<OutputEvent> {
        let active = self.active.as_mut().expect("an active instance");
        active.sink.events.clear();
        active.events.clear();
        drain(&mut self.edits, &mut active.events, &mut active.sink);
        for event in events {
            record(&self.values, event);
            active.events.push(*event);
        }
        let list = Events::from_slice(&active.events);
        let mut cx = ProcessContext::new(&self.values, &mut active.sink);
        active.processor.flush(&list, &mut cx);
        active.sink.events.clone()
    }

    /// Runs one block: the plug-in's queued edits first, then `events`;
    /// each output buffer filled by `input` (an effect's) or silenced (an
    /// instrument's); `process` as a real-time section.
    fn run_block(
        &mut self,
        frames: usize,
        mut input: impl FnMut(usize, &mut [f32]),
        events: impl Iterator<Item = Event>,
    ) -> (ProcessStatus, Duration) {
        let active = self.active.as_mut().expect("an active instance");
        assert!(
            frames <= active.settings.block,
            "a block of {frames} frames, more than the {} it was activated for",
            active.settings.block
        );
        active.sink.events.clear();
        active.events.clear();
        drain(&mut self.edits, &mut active.events, &mut active.sink);
        for event in events {
            record(&self.values, &event);
            active.events.push(event);
        }
        let inputs = usize::from(P::INFO.audio.inputs);
        for (channel, buffer) in active.buffers.iter_mut().enumerate() {
            let buffer = &mut buffer[..frames];
            if channel < inputs {
                input(channel, buffer);
            } else {
                buffer.fill(0.0);
            }
        }
        let mut slices: Vec<&mut [f32]> = active
            .buffers
            .iter_mut()
            .map(|b| &mut b[..frames])
            .collect();
        let mut audio = Buffers::new(&mut slices);
        let processor = &mut active.processor;
        let sink = &mut active.sink;
        let values = &*self.values;
        let list = Events::from_slice(&active.events);
        let mut run = || {
            let mut cx = ProcessContext::new(values, sink);
            let start = Instant::now();
            let status = processor.process(&mut audio, &list, &mut cx);
            (status, start.elapsed())
        };
        if lspk_rt::trap::is_installed() {
            lspk_rt::rt_test!(run())
        } else {
            lspk_rt::realtime(run)
        }
    }
}

impl<P: Plugin> Drop for Instance<P> {
    fn drop(&mut self) {
        self.deactivate();
        self.host.disconnect();
    }
}

impl<P: Plugin + std::fmt::Debug> std::fmt::Debug for Instance<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("plugin", &self.plugin)
            .field("settings", &self.settings())
            .finish_non_exhaustive()
    }
}

/// Takes the plug-in's queued edits: each gesture and value the host should
/// hear goes to `sink`, at frame 0, and each value to `events`, as an event
/// at frame 0 for the processor.
fn drain(edits: &mut Consumer<Edit>, events: &mut Vec<Event>, sink: &mut Collect) {
    while let Ok(edit) = edits.pop() {
        match edit {
            Edit::Begin(id) => {
                sink.push(OutputEvent::GestureBegin { time: 0, id });
            }
            Edit::End(id) => {
                sink.push(OutputEvent::GestureEnd { time: 0, id });
            }
            Edit::Value { id, value, echo } => {
                if echo {
                    sink.push(OutputEvent::Param { time: 0, id, value });
                }
                events.push(Event::Param { time: 0, id, value });
            }
        }
    }
}

/// Records a host's parameter change in `values`, as a format's adapter
/// does before the processor sees it.
fn record(values: &ParamValues, event: &Event) {
    if let Event::Param { id, value, .. } = *event {
        values.set(id, value);
    }
}

/// `event` moved `offset` frames later.
fn retime_output(event: OutputEvent, offset: u32) -> OutputEvent {
    match event {
        OutputEvent::GestureBegin { time, id } => OutputEvent::GestureBegin {
            time: time.saturating_add(offset),
            id,
        },
        OutputEvent::Param { time, id, value } => OutputEvent::Param {
            time: time.saturating_add(offset),
            id,
            value,
        },
        OutputEvent::GestureEnd { time, id } => OutputEvent::GestureEnd {
            time: time.saturating_add(offset),
            id,
        },
        OutputEvent::NoteEnd { time, note } => OutputEvent::NoteEnd {
            time: time.saturating_add(offset),
            note,
        },
        other => other,
    }
}

/// What the processor sends, kept without growing past its capacity.
struct Collect {
    events: Vec<OutputEvent>,
    dropped: usize,
}

impl Collect {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
            dropped: 0,
        }
    }
}

impl EventSink for Collect {
    fn push(&mut self, event: OutputEvent) -> bool {
        if self.events.len() < self.events.capacity() {
            self.events.push(event);
            true
        } else {
            self.dropped += 1;
            false
        }
    }
}

/// What the plug-in asked of this host.
#[derive(Default)]
struct Recorder {
    flush: Cell<bool>,
    dirty: Cell<bool>,
    restart: Cell<bool>,
    process: Cell<bool>,
    callback: Cell<bool>,
}

impl Recorder {
    fn take(&self) -> Requests {
        Requests {
            flush: self.flush.take(),
            dirty: self.dirty.take(),
            restart: self.restart.take(),
            process: self.process.take(),
            callback: self.callback.take(),
        }
    }
}

/// This host as the plug-in's [`HostBackend`].
struct Backend(Rc<Recorder>);

impl HostBackend for Backend {
    fn request_flush(&self) {
        self.0.flush.set(true);
    }

    fn mark_dirty(&self) {
        self.0.dirty.set(true);
    }

    fn request_restart(&self) {
        self.0.restart.set(true);
    }

    fn request_process(&self) {
        self.0.process.set(true);
    }

    fn request_callback(&self) {
        self.0.callback.set(true);
    }

    fn values_changed(&self) {}
}

/// The same render at several block sizes, compared with the first.
#[derive(Clone, Debug, PartialEq)]
pub struct Invariance {
    /// The block size every other render is compared with.
    pub reference: usize,
    /// Each other block size and its largest difference from the
    /// reference, as a linear amplitude (infinite if the renders' shapes
    /// differ).
    pub differences: Vec<(usize, f64)>,
}

impl Invariance {
    /// Whether every render matched the reference to the bit.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.differences.iter().all(|(_, d)| *d == 0.0)
    }

    /// The largest difference, in dBFS (−∞ if every render matched).
    #[must_use]
    pub fn worst_dbfs(&self) -> f64 {
        crate::measure::db(self.differences.iter().map(|(_, d)| *d).fold(0.0, f64::max))
    }

    /// Ok if every difference is below `dbfs`, else which block sizes
    /// weren't.
    ///
    /// # Errors
    ///
    /// The block sizes whose renders differed by `dbfs` or more.
    pub fn below(&self, dbfs: f64) -> Result<(), String> {
        let failed: Vec<String> = self
            .differences
            .iter()
            .filter(|(_, d)| crate::measure::db(*d) >= dbfs)
            .map(|(size, d)| {
                format!(
                    "{size}-frame blocks differ from {}-frame blocks by {:.1} dBFS",
                    self.reference,
                    crate::measure::db(*d)
                )
            })
            .collect();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }
}

/// Renders with `render` at each block size in `sizes` (try
/// [`BLOCK_SIZES`]) and compares every render with the first.
///
/// ```ignore
/// let script = Script::new(4_800).note(0, 2_400, 69, 1.0);
/// let invariance = block_invariance(&BLOCK_SIZES, |block| {
///     render::<MySynth>(&script, Settings::new(48_000.0, block)).unwrap().output
/// });
/// assert!(invariance.is_exact(), "{invariance:?}");
/// ```
pub fn block_invariance(sizes: &[usize], mut render: impl FnMut(usize) -> Output) -> Invariance {
    let Some((&reference, rest)) = sizes.split_first() else {
        return Invariance {
            reference: 0,
            differences: Vec::new(),
        };
    };
    let first = render(reference);
    let differences = rest
        .iter()
        .map(|&size| (size, first.max_difference(&render(size))))
        .collect();
    Invariance {
        reference,
        differences,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_events_stay_in_time_order_and_split_into_blocks() {
        let script = Script::new(100)
            .param(50, ParamId(1), 0.5)
            .note(10, 60, 60, 1.0)
            .param(50, ParamId(2), 0.25)
            .choke(99, 61);
        let times: Vec<u32> = script.events().iter().map(Event::time).collect();
        assert_eq!(times, [10, 50, 50, 70, 99]);
        // Same-frame events keep the order they were added in.
        assert!(matches!(
            script.events()[1],
            Event::Param { id: ParamId(1), .. }
        ));
        let block: Vec<Event> = script.block_events(50, 75).collect();
        let times: Vec<u32> = block.iter().map(Event::time).collect();
        assert_eq!(times, [0, 0, 20]);
        assert_eq!(script.block_events(100, 200).count(), 0);
    }

    #[test]
    fn input_repeats_its_first_channel_and_pads_with_silence() {
        let script = Script::new(4).with_mono_input(vec![1.0, 2.0, 3.0]);
        let mut out = [9.0; 3];
        script.fill_input(1, 1, &mut out);
        assert_eq!(out, [2.0, 3.0, 0.0]);
        script.fill_input(0, 10, &mut out);
        assert_eq!(out, [0.0; 3]);
        let mut out = [9.0; 2];
        Script::new(2).fill_input(0, 0, &mut out);
        assert_eq!(out, [0.0; 2]);
    }

    #[test]
    fn outputs_compare_by_shape_and_sample() {
        let a = Output::new(48_000.0, vec![vec![0.0, 0.5], vec![1.0, -1.0]]);
        let mut b = a.clone();
        assert!(a.same_bits(&b));
        assert_eq!(a.max_difference(&b), 0.0);
        b.channels[1][1] = -0.75;
        assert!(!a.same_bits(&b));
        assert_eq!(a.max_difference(&b), 0.25);
        assert_eq!(a.max_difference(&a.slice(0, 1)), f64::INFINITY);
        assert_eq!(a.interleaved(), [0.0, 1.0, 0.5, -1.0]);
        assert_eq!(a.mono(), [0.5, -0.25]);
        assert_eq!(a.slice(1, 9).channels, [vec![0.5], vec![-1.0]]);
        assert!(a.is_finite() && !a.is_silent());
    }

    #[test]
    fn invariance_reports_each_size_against_the_first() {
        let invariance = block_invariance(&[1, 2, 3], |size| {
            Output::new(1.0, vec![vec![if size == 3 { 0.5 } else { 0.0 }]])
        });
        assert_eq!(invariance.reference, 1);
        assert_eq!(invariance.differences, [(2, 0.0), (3, 0.5)]);
        assert!(!invariance.is_exact());
        assert!((invariance.worst_dbfs() + 6.0206).abs() < 1e-3);
        assert!(invariance.below(-3.0).is_ok());
        let error = invariance.below(-96.0).unwrap_err();
        assert!(error.contains("3-frame blocks"), "{error}");
        assert!(block_invariance(&[], |_| unreachable!()).is_exact());
    }
}
