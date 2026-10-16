//! The audio loop: a plug-in's processor run from a device's callback, with
//! the plug-in's parameter edits and the MIDI that arrived since the last
//! buffer as its events.
//!
//! Everything here runs on the audio thread and allocates nothing, locks
//! nothing and never blocks; the buffers it needs are made with it, on the
//! main thread, sized for the stream's longest block.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use lspk_plugin::host::Edit;
use lspk_plugin::{
    Buffers, Event, Events, MAX_CHANNELS, Note, ParamValues, ProcessConfig, ProcessContext,
    ProcessStatus, Processor, Transport,
};
use rtrb::Consumer;

use crate::audio::AudioCallback;

/// One MIDI 1.0 channel message, as it arrived: status and two data bytes.
pub type MidiMessage = [u8; 3];

/// The loudest sample a stream has played since it was last read, for a
/// level meter. Real-time safe on both sides.
#[derive(Debug, Default)]
pub struct Meter {
    peak: AtomicU32,
}

impl Meter {
    /// Records `peak`, an absolute sample value.
    fn record(&self, peak: f32) {
        // Non-negative floats order as their bits do.
        self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }

    /// The loudest absolute sample since the last call, and starts again.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

/// A plug-in's processor and everything its blocks need.
pub struct Engine<Pr: Processor> {
    processor: Pr,
    values: Arc<ParamValues>,
    edits: Consumer<Edit>,
    midi: Consumer<MidiMessage>,
    /// The block's events; never grows past the capacity it's made with.
    events: Vec<Event>,
    /// The processor's channels, each `max_frames` long.
    channels: Box<[Box<[f32]>]>,
    /// The plug-in's main input channels; 0 for an instrument.
    inputs: usize,
    sample_rate: f64,
    max_frames: usize,
    /// The app's own clock, the transport a host would give: it plays, at
    /// 120 beats per minute, from the start of the stream.
    transport: Transport,
    asleep: bool,
    meter: Arc<Meter>,
}

impl<Pr: Processor> Engine<Pr> {
    /// An engine running `processor` with the plug-in's `inputs` and
    /// `outputs` channels at `config`, taking edits and MIDI from the two
    /// queues.
    pub fn new(
        processor: Pr,
        values: Arc<ParamValues>,
        edits: Consumer<Edit>,
        midi: Consumer<MidiMessage>,
        (inputs, outputs): (u16, u16),
        config: ProcessConfig,
        meter: Arc<Meter>,
    ) -> Self {
        let max_frames = config.max_frames.max(1) as usize;
        let channels = usize::from(outputs).min(MAX_CHANNELS);
        let capacity = edits.buffer().capacity() + midi.buffer().capacity();
        Self {
            processor,
            values,
            edits,
            midi,
            events: Vec::with_capacity(capacity),
            channels: (0..channels)
                .map(|_| vec![0.0; max_frames].into_boxed_slice())
                .collect(),
            inputs: usize::from(inputs).min(channels),
            sample_rate: config.sample_rate,
            max_frames,
            transport: Transport {
                playing: true,
                ..Transport::default()
            },
            asleep: false,
            meter,
        }
    }

    /// Takes the engine apart, for the main thread to deactivate the
    /// processor and keep the queues for the next one.
    pub fn into_parts(self) -> (Pr, Consumer<Edit>, Consumer<MidiMessage>) {
        (self.processor, self.edits, self.midi)
    }

    /// Takes every waiting edit and MIDI message as events at frame 0.
    fn gather(&mut self) {
        self.events.clear();
        while let Ok(edit) = self.edits.pop() {
            // The host link has already recorded the value, and the
            // standalone app has no host to tell of the gesture.
            if let Edit::Value { id, value, .. } = edit
                && self.events.len() < self.events.capacity()
            {
                self.events.push(Event::Param { time: 0, id, value });
            }
        }
        while let Ok(message) = self.midi.pop() {
            if let Some(event) = midi_event(message)
                && self.events.len() < self.events.capacity()
            {
                self.events.push(event);
            }
        }
    }

    /// Renders one device buffer.
    fn render(
        &mut self,
        input: &[f32],
        input_channels: usize,
        output: &mut [f32],
        channels: usize,
    ) {
        let channels = channels.max(1);
        let total = output.len() / channels;
        self.gather();
        let mut peak = 0.0f32;
        let mut done = 0;
        while done < total {
            let frames = (total - done).min(self.max_frames);
            let mut quiet = true;
            for (c, channel) in self.channels.iter_mut().enumerate() {
                let channel = &mut channel[..frames];
                if c < self.inputs && input_channels > 0 {
                    let source = c % input_channels;
                    for (f, sample) in channel.iter_mut().enumerate() {
                        *sample = input
                            .get((done + f) * input_channels + source)
                            .copied()
                            .unwrap_or(0.0);
                    }
                    quiet &= channel.iter().all(|s| *s == 0.0);
                } else {
                    channel.fill(0.0);
                }
            }
            if self.asleep && quiet && self.events.is_empty() {
                // Asleep, and nothing to wake it: the output stays silent.
            } else {
                let mut pointers = [std::ptr::null_mut(); MAX_CHANNELS];
                for (pointer, channel) in pointers.iter_mut().zip(self.channels.iter_mut()) {
                    *pointer = channel.as_mut_ptr();
                }
                // SAFETY: each pointer is one of this engine's own channels,
                // at least `frames` long, distinct from the others, and
                // nothing else touches them until `process` returns.
                let mut buffers =
                    unsafe { Buffers::from_raw(pointers, self.channels.len(), frames) };
                let events = Events::from_slice(&self.events);
                let mut sink = ();
                let mut cx =
                    ProcessContext::with_transport(&self.values, &mut sink, Some(&self.transport));
                let status = self.processor.process(&mut buffers, &events, &mut cx);
                self.asleep = status == ProcessStatus::Sleep;
                self.events.clear();
            }
            // The clock runs whether or not the plug-in does.
            self.transport.advance(frames, self.sample_rate);
            let block = &mut output[done * channels..(done + frames) * channels];
            interleave(&self.channels, frames, block, channels);
            peak = block.iter().fold(peak, |peak, s| peak.max(s.abs()));
            done += frames;
        }
        self.meter.record(peak);
    }
}

/// Writes the plug-in's `frames` frames of `source` into `target`,
/// `channels` interleaved: device channel `d` plays plug-in channel `d`, a
/// mono plug-in plays on every device channel, and device channels beyond
/// the plug-in's are silent.
fn interleave(source: &[Box<[f32]>], frames: usize, target: &mut [f32], channels: usize) {
    for (f, frame) in target.chunks_exact_mut(channels).take(frames).enumerate() {
        for (d, sample) in frame.iter_mut().enumerate() {
            *sample = match source.len() {
                0 => 0.0,
                1 => source[0][f],
                n if d < n => source[d][f],
                _ => 0.0,
            };
        }
    }
}

/// `message` as the kit's event, at frame 0: notes on and off, the
/// channel-mode messages that silence a channel, and the other channel
/// messages (pitch bend, controllers, pressure) as [`Event::Midi`]. System
/// messages are passed over.
#[must_use]
pub fn midi_event(message: MidiMessage) -> Option<Event> {
    let [status, data1, data2] = message;
    let channel = status & 0x0f;
    let velocity = f32::from(data2 & 0x7f) / 127.0;
    let note = Note::key(channel, data1 & 0x7f);
    let whole_channel = Note {
        port: Some(0),
        channel: Some(channel),
        key: None,
        id: None,
    };
    match status & 0xf0 {
        0x90 if velocity > 0.0 => Some(Event::NoteOn {
            time: 0,
            note,
            velocity,
        }),
        0x80 | 0x90 => Some(Event::NoteOff {
            time: 0,
            note,
            velocity,
        }),
        // All sound off.
        0xb0 if data1 == 120 => Some(Event::NoteChoke {
            time: 0,
            note: whole_channel,
        }),
        // All notes off.
        0xb0 if data1 == 123 => Some(Event::NoteOff {
            time: 0,
            note: whole_channel,
            velocity: 0.0,
        }),
        0xa0..=0xe0 => Some(Event::Midi {
            time: 0,
            port: 0,
            data: [status, data1 & 0x7f, data2 & 0x7f],
        }),
        _ => None,
    }
}

const FREE: u8 = 0;
const BUSY: u8 = 1;
const TAKEN: u8 = 2;

/// A value the audio thread borrows for each buffer and the main thread
/// takes back once the stream has stopped, with no lock: the audio thread
/// only ever tries, and plays silence if it can't have it.
pub struct Slot<T> {
    inner: Arc<SlotInner<T>>,
}

struct SlotInner<T> {
    state: AtomicU8,
    value: UnsafeCell<Option<T>>,
}

// SAFETY: the value is reached only by whoever moves `state` from FREE to
// BUSY (the audio thread, for one call) or to TAKEN (the main thread, once),
// so no two threads ever reach it at once.
unsafe impl<T: Send> Sync for SlotInner<T> {}

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Send> Slot<T> {
    /// A slot holding `value`.
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(SlotInner {
                state: AtomicU8::new(FREE),
                value: UnsafeCell::new(Some(value)),
            }),
        }
    }

    /// Runs `f` on the value if no one else has it; real-time safe.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.inner
            .state
            .compare_exchange(FREE, BUSY, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        /// Frees the slot however `f` ends.
        struct Release<'a>(&'a AtomicU8);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.store(FREE, Ordering::Release);
            }
        }
        let _release = Release(&self.inner.state);
        // SAFETY: the state is BUSY, set by this call, so nothing else
        // reaches the value until `_release` drops.
        let value = unsafe { &mut *self.inner.value.get() }.as_mut()?;
        Some(f(value))
    }

    /// Takes the value for good, waiting for a buffer that has it to end;
    /// `None` if it's been taken. For the main thread, once the stream that
    /// borrows it has stopped.
    pub fn take(&self) -> Option<T> {
        loop {
            match self.inner.state.compare_exchange(
                FREE,
                TAKEN,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                // SAFETY: the state is TAKEN, set by this call, and never
                // leaves it, so nothing else reaches the value again.
                Ok(_) => return unsafe { &mut *self.inner.value.get() }.take(),
                Err(TAKEN) => return None,
                Err(_) => std::thread::yield_now(),
            }
        }
    }
}

/// An engine in a [`Slot`], as a stream's callback.
pub struct EngineCallback<Pr: Processor> {
    slot: Slot<Engine<Pr>>,
}

impl<Pr: Processor> EngineCallback<Pr> {
    /// A callback rendering with the engine in `slot`.
    pub fn new(slot: Slot<Engine<Pr>>) -> Self {
        Self { slot }
    }
}

impl<Pr: Processor> AudioCallback for EngineCallback<Pr> {
    fn render(
        &mut self,
        input: &[f32],
        input_channels: usize,
        output: &mut [f32],
        channels: usize,
    ) {
        let rendered = self
            .slot
            .with(|engine| engine.render(input, input_channels, output, channels));
        if rendered.is_none() {
            output.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_becomes_note_events() {
        assert!(matches!(
            midi_event([0x91, 60, 127]),
            Some(Event::NoteOn { note, velocity, .. })
                if note == Note::key(1, 60) && velocity == 1.0
        ));
        assert!(matches!(
            midi_event([0x90, 60, 0]),
            Some(Event::NoteOff { .. })
        ));
        assert!(matches!(
            midi_event([0x80, 60, 64]),
            Some(Event::NoteOff { .. })
        ));
        assert!(matches!(
            midi_event([0xb2, 123, 0]),
            Some(Event::NoteOff { note, .. }) if note.key.is_none() && note.channel == Some(2)
        ));
        assert!(matches!(
            midi_event([0xb0, 120, 0]),
            Some(Event::NoteChoke { .. })
        ));
        assert_eq!(
            midi_event([0xb0, 7, 100]),
            Some(Event::Midi {
                time: 0,
                port: 0,
                data: [0xb0, 7, 100]
            })
        );
        assert!(matches!(
            midi_event([0xe0, 0, 64]),
            Some(Event::Midi { .. })
        ));
        assert_eq!(midi_event([0xf8, 0, 0]), None);
    }

    #[test]
    fn a_mono_plugin_plays_on_every_channel_and_extra_channels_are_silent() {
        let mono = [vec![0.5f32, 0.25].into_boxed_slice()];
        let mut out = [9.0f32; 6];
        interleave(&mono, 2, &mut out, 3);
        assert_eq!(out, [0.5, 0.5, 0.5, 0.25, 0.25, 0.25]);
        let stereo = [
            vec![0.1f32].into_boxed_slice(),
            vec![0.2f32].into_boxed_slice(),
        ];
        let mut out = [9.0f32; 3];
        interleave(&stereo, 1, &mut out, 3);
        assert_eq!(out, [0.1, 0.2, 0.0]);
    }

    #[test]
    fn a_slot_is_lent_until_taken() {
        let slot = Slot::new(5);
        assert_eq!(slot.with(|v| *v + 1), Some(6));
        let lent = slot.clone();
        assert_eq!(slot.take(), Some(5));
        assert_eq!(lent.with(|v| *v), None);
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn the_meter_keeps_the_peak_until_read() {
        let meter = Meter::default();
        meter.record(0.25);
        meter.record(0.5);
        meter.record(0.125);
        assert_eq!(meter.take(), 0.5);
        assert_eq!(meter.take(), 0.0);
    }

    /// A processor that lays the transport it sees into its first channel:
    /// the tempo, the song position in beats and seconds, and whether the
    /// host plays.
    struct ClockProbe;

    impl Processor for ClockProbe {
        fn process(
            &mut self,
            audio: &mut Buffers<'_>,
            _events: &Events<'_>,
            cx: &mut ProcessContext<'_>,
        ) -> ProcessStatus {
            audio.silence();
            let fields = match cx.transport() {
                Some(transport) => [
                    transport.tempo,
                    transport.beats,
                    transport.seconds,
                    f64::from(transport.playing),
                ],
                None => [f64::NAN; 4],
            };
            for (sample, value) in audio.channel_mut(0).iter_mut().zip(fields) {
                *sample = value as f32;
            }
            ProcessStatus::Continue
        }

        fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
    }

    #[test]
    fn the_apps_clock_serves_as_the_transport() {
        let values = Arc::new(ParamValues::new(Vec::new()).expect("no parameters"));
        let (_edit_tx, edits) = rtrb::RingBuffer::new(8);
        let (_midi_tx, midi) = rtrb::RingBuffer::new(8);
        let mut engine = Engine::new(
            ClockProbe,
            values,
            edits,
            midi,
            (0, 2),
            ProcessConfig::new(48_000.0, 64),
            Arc::new(Meter::default()),
        );
        // Two blocks of 64 frames, interleaved stereo.
        let mut out = vec![0.0f32; 2 * 128];
        engine.render(&[], 0, &mut out, 2);
        // The app plays its own clock: 120 beats per minute from the start.
        assert_eq!(out[0], 120.0);
        assert_eq!(out[2], 0.0);
        assert_eq!(out[4], 0.0);
        assert_eq!(out[6], 1.0);
        // By the second block the clock has moved on by 64 samples.
        assert_eq!(out[128], 120.0);
        assert!((f64::from(out[130]) - 64.0 / 48_000.0 * 2.0).abs() < 1e-5);
        assert!((f64::from(out[132]) - 64.0 / 48_000.0).abs() < 1e-6);
    }
}
