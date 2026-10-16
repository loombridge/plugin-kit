//! The CLAP glue, through a CLAP host, with a probe plug-in built into this
//! test binary so the allocation trap sees both sides of every block.

#![cfg(feature = "clap")]

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::clap::EntryDescriptor;
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, NotePorts, ParamValues, Plugin, PluginError,
    PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor, Tail, features,
};
use lspk_test::clap::{ClapTestHost, HostEvent, PluginEvent, Requests, transport_event};

lspk_rt::install_trap!();

const AMOUNT: ParamId = ParamId(10);
const MODE: ParamId = ParamId(20);
const GAIN: ParamId = ParamId(30);
const LATENCY: u32 = 64;
const TAIL: u32 = 100;
const PROBE_ID: &str = "com.loombridge.test.probe";

/// The last parameter value the processor's flush took, as bits.
static FLUSHED: AtomicU64 = AtomicU64::new(0);

/// A plug-in that shows what it receives in its output: on channel 0, each
/// parameter event's value at its frame; on channel 1, each note-on's key.
/// A note-on also sets Mode to 2 from the audio thread, in a gesture, and
/// a main-thread callback sets Amount to 0.25.
struct Probe {
    host: Host,
    own: Vec<u8>,
}

impl Plugin for Probe {
    type Processor = ProbeProcessor;

    const INFO: PluginInfo = PluginInfo::new(PROBE_ID, "Probe")
        .vendor("Loombridge")
        .features(&[features::AUDIO_EFFECT, features::STEREO])
        .audio(AudioLayout::effect(2))
        .notes(NotePorts::INPUT);

    fn new(host: Host) -> Self {
        Self {
            host,
            own: b"probe".to_vec(),
        }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        vec![
            ParamDescriptor::continuous(AMOUNT, "Amount", (0.0, 1.0), 0.5, ParamUnit::None),
            ParamDescriptor::choice(MODE, "Mode", &["A", "B", "C"], 1),
            ParamDescriptor::gain(GAIN, "Gain", 6.0, 0.0),
        ]
    }

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<ProbeProcessor, PluginError> {
        assert_eq!(values.len(), 3);
        Ok(ProbeProcessor)
    }

    fn save_state(&self) -> Vec<u8> {
        self.own.clone()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        if data.starts_with(b"probe") {
            self.own = data.to_vec();
            Ok(())
        } else {
            Err(PluginError::new("not a probe's state"))
        }
    }

    fn latency(&self) -> u32 {
        LATENCY
    }

    fn on_main_thread(&mut self) {
        self.host.edit(AMOUNT, 0.25).expect("an edit");
    }
}

struct ProbeProcessor;

impl Processor for ProbeProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        audio.silence();
        let mut any = false;
        for event in events.iter() {
            any = true;
            let at = event.time() as usize;
            match event {
                Event::Param { value, .. } => audio.channel_mut(0)[at] = value as f32,
                Event::NoteOn { note, .. } => {
                    audio.channel_mut(1)[at] = f32::from(note.key.unwrap_or(0));
                    cx.begin_gesture(event.time(), MODE);
                    cx.set_param(event.time(), MODE, 2.0);
                    cx.end_gesture(event.time(), MODE);
                }
                _ => {}
            }
        }
        if any {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }

    fn flush(&mut self, events: &Events<'_>, _cx: &mut ProcessContext<'_>) {
        for event in events.iter() {
            if let Event::Param { value, .. } = event {
                FLUSHED.store(value.to_bits(), Relaxed);
            }
        }
    }

    fn tail(&self) -> Tail {
        Tail::Frames(TAIL)
    }
}

static PROBE: EntryDescriptor = lspk_plugin::clap_entry!(Probe);

const TRANSPORT_ID: &str = "com.loombridge.test.transport";

/// A plug-in that shows the host's transport in its output: channel 0's
/// first five samples are its tempo, beats in a bar, playing, beats and
/// seconds, each NaN when the host offers no transport.
struct TransportProbe;

impl Plugin for TransportProbe {
    type Processor = TransportProbeProcessor;

    const INFO: PluginInfo = PluginInfo::new(TRANSPORT_ID, "Transport Probe")
        .vendor("Loombridge")
        .features(&[features::AUDIO_EFFECT, features::STEREO])
        .audio(AudioLayout::effect(2));

    fn new(_host: Host) -> Self {
        Self
    }

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        _values: &ParamValues,
    ) -> Result<TransportProbeProcessor, PluginError> {
        Ok(TransportProbeProcessor)
    }
}

struct TransportProbeProcessor;

impl Processor for TransportProbeProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        _events: &Events<'_>,
        cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        audio.silence();
        let first = match cx.transport() {
            Some(transport) => [
                transport.tempo,
                transport.beats_per_bar,
                f64::from(transport.playing),
                transport.beats,
                transport.seconds,
            ],
            None => [f64::NAN; 5],
        };
        for (sample, value) in audio.channel_mut(0).iter_mut().zip(first) {
            *sample = value as f32;
        }
        ProcessStatus::Continue
    }

    fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
}

static TRANSPORT_PROBE: EntryDescriptor = lspk_plugin::clap_entry!(TransportProbe);

/// A plug-in whose parameters repeat an ID.
struct Broken;

impl Plugin for Broken {
    type Processor = ProbeProcessor;

    const INFO: PluginInfo = PluginInfo::new("com.loombridge.test.broken", "Broken");

    fn new(_host: Host) -> Self {
        Self
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        vec![
            ParamDescriptor::toggle(ParamId(1), "A", true),
            ParamDescriptor::toggle(ParamId(1), "B", true),
        ]
    }

    fn activate(
        &mut self,
        _: &ProcessConfig,
        _: &ParamValues,
    ) -> Result<ProbeProcessor, PluginError> {
        Ok(ProbeProcessor)
    }
}

static BROKEN: EntryDescriptor = lspk_plugin::clap_entry!(Broken);

const FRAMES: usize = 64;

fn probe() -> ClapTestHost {
    ClapTestHost::from_entry(&PROBE, PROBE_ID).expect("the probe")
}

fn active() -> ClapTestHost {
    let mut host = probe();
    host.activate(48_000.0, FRAMES as u32).expect("activation");
    host
}

fn param(time: u32, id: ParamId, value: f64) -> HostEvent {
    HostEvent::Param {
        time,
        id: id.0,
        value,
    }
}

fn gesture(time: u32, id: ParamId, value: f64) -> [PluginEvent; 3] {
    [
        PluginEvent::GestureBegin { time, id: id.0 },
        PluginEvent::Param {
            time,
            id: id.0,
            value,
        },
        PluginEvent::GestureEnd { time, id: id.0 },
    ]
}

#[test]
fn it_describes_its_ports_and_parameters_as_the_host_sees_them() {
    let mut host = probe();
    assert_eq!(host.audio_ports(true), [2]);
    assert_eq!(host.audio_ports(false), [2]);
    assert_eq!((host.note_ports(true), host.note_ports(false)), (1, 0));
    let params = host.params();
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Amount", "Mode", "Gain"]);
    assert!(params.iter().all(|p| p.automatable));
    // Continuous and gain parameters are the control's travel.
    let (amount, mode, gain) = (&params[0], &params[1], &params[2]);
    assert_eq!(
        (amount.min, amount.max, amount.default, amount.stepped),
        (0.0, 1.0, 0.5, false)
    );
    assert_eq!((gain.min, gain.max, gain.stepped), (0.0, 1.0, false));
    // A choice is its index, stepped, as an enumeration.
    assert_eq!((mode.min, mode.max, mode.default), (0.0, 2.0, 1.0));
    assert!(mode.stepped && mode.is_enum);
    // Values read in their own units, and parse back.
    assert_eq!(host.text(GAIN.0, 0.0).as_deref(), Some("-inf dB"));
    assert_eq!(host.text(GAIN.0, gain.default).as_deref(), Some("0.00 dB"));
    assert_eq!(host.text(MODE.0, 2.0).as_deref(), Some("C"));
    assert_eq!(host.parse(MODE.0, "b"), Some(1.0));
    let unity = host.parse(GAIN.0, "0 dB").expect("a value");
    assert!((unity - gain.default).abs() < 1e-9);
    assert_eq!(host.parse(GAIN.0, "-inf"), Some(0.0));
    assert_eq!(host.value(AMOUNT.0), Some(0.5));
    assert_eq!(host.value(99), None);
    assert_eq!(host.latency(), Some(LATENCY));
}

#[test]
fn events_arrive_on_their_frame_in_the_parameters_own_units() {
    let mut host = active();
    let block = host.process(
        FRAMES,
        None,
        &[
            param(5, AMOUNT, 0.75),
            HostEvent::NoteOn {
                time: 9,
                key: 60,
                channel: 0,
                note_id: None,
                velocity: 1.0,
            },
            param(20, MODE, 1.6),
            param(30, GAIN, 1.0),
            HostEvent::Midi {
                time: 40,
                data: [0x90, 64, 100],
            },
        ],
    );
    let (values, keys) = (&block.outputs[0], &block.outputs[1]);
    assert_eq!(values[5], 0.75);
    assert_eq!(keys[9], 60.0);
    // A stepped value is whole; a gain's travel at the top is +6 dB.
    assert_eq!(values[20], 2.0);
    assert!((f64::from(values[30]) - lspk_params::db_to_gain(6.0)).abs() < 1e-6);
    assert_eq!(keys[40], 64.0);
    assert_eq!(block.status, lspk_test::clap::ProcessStatus::Continue);
    assert_eq!(host.value(AMOUNT.0), Some(0.75));
    assert_eq!(host.value(GAIN.0), Some(1.0));
}

#[test]
fn the_processors_own_change_reaches_the_host_in_a_gesture() {
    let mut host = active();
    let block = host.process(
        FRAMES,
        None,
        &[HostEvent::NoteOn {
            time: 9,
            key: 60,
            channel: 0,
            note_id: None,
            velocity: 1.0,
        }],
    );
    assert_eq!(block.events, gesture(9, MODE, 2.0));
    assert_eq!(host.value(MODE.0), Some(2.0));
}

#[test]
fn a_main_thread_edit_reaches_the_processor_and_the_host() {
    let mut host = active();
    host.take_requests();
    host.on_main_thread();
    let requests = host.take_requests();
    assert!(requests.flush && requests.dirty, "{requests:?}");
    assert_eq!(host.value(AMOUNT.0), Some(0.25));
    let block = host.process(FRAMES, None, &[]);
    assert_eq!(block.events, gesture(0, AMOUNT, 0.25));
    assert_eq!(block.outputs[0][0], 0.25);
    // Once delivered, it isn't sent again.
    let block = host.process(FRAMES, None, &[]);
    assert!(block.events.is_empty());
    assert_eq!(block.status, lspk_test::clap::ProcessStatus::Sleep);
}

#[test]
fn while_inactive_edits_go_out_on_the_main_threads_flush() {
    let mut host = probe();
    host.on_main_thread();
    assert_eq!(host.flush(&[]), gesture(0, AMOUNT, 0.25));
    assert!(host.flush(&[param(0, AMOUNT, 0.9)]).is_empty());
    assert_eq!(host.value(AMOUNT.0), Some(0.9));
    // An edit made while inactive and not yet flushed goes out once active.
    host.on_main_thread();
    host.activate(48_000.0, FRAMES as u32).expect("activation");
    let block = host.process(FRAMES, None, &[]);
    assert_eq!(block.events, gesture(0, AMOUNT, 0.25));
}

#[test]
fn while_active_a_flush_reaches_the_processor() {
    let mut host = active();
    assert!(host.flush(&[param(0, AMOUNT, 0.6)]).is_empty());
    assert_eq!(f64::from_bits(FLUSHED.load(Relaxed)), 0.6);
    assert_eq!(host.value(AMOUNT.0), Some(0.6));
}

#[test]
fn state_round_trips_and_reaches_an_active_processor() {
    let mut first = probe();
    first.flush(&[
        param(0, AMOUNT, 0.125),
        param(0, MODE, 0.0),
        param(0, GAIN, 1.0),
    ]);
    let saved = first.save_state().expect("a state");
    drop(first);

    let mut second = active();
    second.take_requests();
    second.load_state(&saved).expect("the state loads");
    assert_eq!(second.value(AMOUNT.0), Some(0.125));
    assert_eq!(second.value(MODE.0), Some(0.0));
    assert_eq!(second.value(GAIN.0), Some(1.0));
    let requests = second.take_requests();
    assert!(requests.process, "{requests:?}");
    // The processor hears the loaded values; the host doesn't hear them
    // as edits.
    let block = second.process(FRAMES, None, &[]);
    assert!(block.events.is_empty());
    // Gain, the last parameter, at +6 dB.
    assert!((f64::from(block.outputs[0][0]) - lspk_params::db_to_gain(6.0)).abs() < 1e-6);
    assert_eq!(second.save_state().expect("a state"), saved);

    assert!(second.load_state(b"").is_err());
    assert!(second.load_state(b"not a state").is_err());
    let mut foreign = saved.clone();
    let own = foreign.len() - b"probe".len();
    foreign[own..].copy_from_slice(b"xxxxx");
    assert!(second.load_state(&foreign).is_err());
    // A refused state changes nothing.
    assert_eq!(second.value(AMOUNT.0), Some(0.125));
}

#[test]
fn the_hosts_transport_reaches_the_processor() {
    let mut host = ClapTestHost::from_entry(&TRANSPORT_PROBE, TRANSPORT_ID).expect("the probe");
    host.activate(48_000.0, FRAMES as u32).expect("activation");
    // A free-running host offers no transport.
    let block = host.process(FRAMES, None, &[]);
    assert!(block.outputs[0][0].is_nan(), "{:?}", block.outputs[0]);

    // A playing host at 90 BPM, 8.5 beats in, reaches the processor at the
    // block's first sample.
    host.set_transport(Some(transport_event(90.0, 8.5, 5.5, true)));
    let block = host.process(FRAMES, None, &[]);
    let out = &block.outputs[0];
    assert!((out[0] - 90.0).abs() < 1e-4, "tempo {}", out[0]);
    assert!((out[1] - 4.0).abs() < 1e-4, "beats in a bar {}", out[1]);
    assert_eq!(out[2], 1.0, "playing");
    assert!((out[3] - 8.5).abs() < 1e-4, "beats {}", out[3]);
    assert!((out[4] - 5.5).abs() < 1e-4, "seconds {}", out[4]);

    // The host moves the song position on between blocks, as a host does.
    let block = host.process(FRAMES, None, &[]);
    let beats = 8.5 + FRAMES as f64 / 48_000.0 * 90.0 / 60.0;
    assert!((f64::from(block.outputs[0][3]) - beats).abs() < 1e-4);

    // A stopped host says so.
    host.set_transport(Some(transport_event(120.0, 0.0, 0.0, false)));
    let block = host.process(FRAMES, None, &[]);
    assert_eq!(block.outputs[0][2], 0.0, "stopped");
}

#[test]
fn it_reports_its_latency_and_tail() {
    let mut host = probe();
    assert_eq!(host.latency(), Some(LATENCY));
    assert_eq!(host.tail(), None);
    host.activate(48_000.0, FRAMES as u32).expect("activation");
    assert_eq!(host.tail(), Some(TAIL));
}

#[test]
fn a_plugin_with_broken_parameters_does_not_start() {
    assert!(ClapTestHost::from_entry(&BROKEN, "com.loombridge.test.broken").is_err());
}

#[test]
fn processing_stays_off_the_heap() {
    assert!(lspk_rt::trap::is_installed());
    let mut host = active();
    let input = [0.5f32; FRAMES];
    for block in 0..32u32 {
        // Main-thread edits, host events and the processor's own changes,
        // every block; each process call is a real-time section.
        host.on_main_thread();
        host.process(
            FRAMES,
            Some(&input),
            &[
                param(1, AMOUNT, f64::from(block % 4) / 4.0),
                HostEvent::NoteOn {
                    time: 2,
                    key: 60,
                    channel: 0,
                    note_id: Some(block),
                    velocity: 0.5,
                },
                HostEvent::NoteOff {
                    time: 3,
                    key: Some(60),
                    channel: Some(0),
                    note_id: None,
                },
            ],
        );
    }
    let _: Requests = host.take_requests();
}
