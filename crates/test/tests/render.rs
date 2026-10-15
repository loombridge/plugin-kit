//! The in-process render harness, with a probe plug-in that marks where
//! each event lands: events reach the processor at their frames whatever
//! the block size, the plug-in's own edits arrive at the start of the next
//! block and the host hears them, and a block that allocates fails.

use std::hint::black_box;

use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, Note, NotePorts, OutputEvent, ParamValues, Plugin,
    PluginError, PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor, Transport,
};
use lspk_test::render::{self, BLOCK_SIZES, Script, Settings, block_invariance};

lspk_rt::install_trap!();

const LEVEL: ParamId = ParamId(1);
const ALLOCATE: ParamId = ParamId(2);
const RATE: f64 = 48_000.0;

/// An instrument that writes each note's velocity on its frame and holds
/// the level parameter's value on every frame, so a render shows where each
/// event landed. A note's key above 100 makes it report the note ended.
struct Probe {
    host: Host,
}

impl Plugin for Probe {
    type Processor = ProbeProcessor;

    const INFO: PluginInfo = PluginInfo::new("com.loombridge.test.probe", "Probe")
        .audio(AudioLayout::instrument(2))
        .notes(NotePorts::INPUT);

    fn new(host: Host) -> Self {
        Self { host }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        vec![
            ParamDescriptor::continuous(LEVEL, "Level", (0.0, 1.0), 0.0, ParamUnit::None),
            ParamDescriptor::toggle(ALLOCATE, "Allocate", false),
        ]
    }

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<ProbeProcessor, PluginError> {
        Ok(ProbeProcessor {
            level: values.get(LEVEL).unwrap_or(0.0) as f32,
            allocate: false,
        })
    }
}

struct ProbeProcessor {
    level: f32,
    allocate: bool,
}

impl Processor for ProbeProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        let frames = audio.frames();
        let mut done = 0;
        let mut marks = [(usize::MAX, 0.0f32); 16];
        let mut count = 0;
        for event in events.iter() {
            let at = (event.time() as usize).min(frames);
            for frame in done..at {
                audio.channel_mut(1)[frame] = self.level;
            }
            done = at;
            match event {
                Event::NoteOn { note, velocity, .. } => {
                    if count < marks.len() {
                        marks[count] = (at, velocity);
                        count += 1;
                    }
                    if note.key.is_some_and(|k| k > 100) {
                        cx.note_end(at as u32, note);
                    }
                }
                Event::Param {
                    id: LEVEL, value, ..
                } => self.level = value as f32,
                Event::Param {
                    id: ALLOCATE,
                    value,
                    ..
                } => self.allocate = value > 0.5,
                _ => {}
            }
        }
        for frame in done..frames {
            audio.channel_mut(1)[frame] = self.level;
        }
        for &(at, velocity) in &marks[..count] {
            audio.channel_mut(0)[at] = velocity;
        }
        if self.allocate {
            black_box(Box::new(0u8));
        }
        ProcessStatus::Continue
    }

    fn flush(&mut self, events: &Events<'_>, _cx: &mut ProcessContext<'_>) {
        for event in events.iter() {
            if let Event::Param {
                id: LEVEL, value, ..
            } = event
            {
                self.level = value as f32;
            }
        }
    }
}

fn script() -> Script {
    Script::new(1_000)
        .note_on(0, 60, 0.25)
        .note_on(31, 61, 0.5)
        .param(64, LEVEL, 0.75)
        .note_on(511, 62, 0.75)
        .note_on(999, 103, 1.0)
}

#[test]
fn events_land_on_their_frames_at_every_block_size() {
    for block in BLOCK_SIZES.into_iter().chain([7, 1_000]) {
        let render =
            render::render::<Probe>(&script(), Settings::new(RATE, block)).expect("a render");
        let marks = render.output.channel(0);
        let hits: Vec<(usize, f32)> = marks
            .iter()
            .enumerate()
            .filter(|(_, s)| **s != 0.0)
            .map(|(i, s)| (i, *s))
            .collect();
        assert_eq!(
            hits,
            [(0, 0.25), (31, 0.5), (511, 0.75), (999, 1.0)],
            "{block}-frame blocks"
        );
        let level = render.output.channel(1);
        assert!(level[..64].iter().all(|s| *s == 0.0) && level[64..].iter().all(|s| *s == 0.75));
        assert_eq!(render.statuses.len(), 1_000usize.div_ceil(block));
        assert_eq!(render.timings.frames(), 1_000);
        // The voice the processor ended, timed from the start of the render.
        assert!(
            matches!(
                render.events[..],
                [OutputEvent::NoteEnd {
                    time: 999,
                    note: Note { key: Some(103), .. }
                }]
            ),
            "{:?}",
            render.events
        );
    }
}

#[test]
fn the_probe_is_block_size_invariant() {
    let invariance = block_invariance(&BLOCK_SIZES, |block| {
        render::render::<Probe>(&script(), Settings::new(RATE, block))
            .expect("a render")
            .output
    });
    assert!(invariance.is_exact(), "{invariance:?}");
    assert_eq!(invariance.differences.len(), 3);
}

#[test]
fn the_plugins_own_edits_reach_the_next_block_and_the_host() {
    let mut instance = render::Instance::<Probe>::new().expect("an instance");
    instance.activate(Settings::new(RATE, 64)).expect("active");
    let first = instance.render(&Script::new(64));
    assert!(first.output.channel(1).iter().all(|s| *s == 0.0));

    // An edit, as an agent's command makes one through the plug-in.
    instance.plugin().host.edit(LEVEL, 0.5).expect("an edit");
    assert_eq!(instance.values().get(LEVEL), Some(0.5));
    let requests = instance.take_requests();
    assert!(requests.flush && requests.dirty);
    let second = instance.render(&Script::new(128));
    assert!(second.output.channel(1).iter().all(|s| *s == 0.5));
    assert_eq!(
        second.events,
        [
            OutputEvent::GestureBegin { time: 0, id: LEVEL },
            OutputEvent::Param {
                time: 0,
                id: LEVEL,
                value: 0.5
            },
            OutputEvent::GestureEnd { time: 0, id: LEVEL },
        ]
    );

    // A host's automation is recorded in the values; a flush applies
    // events while no block runs.
    instance.render(&Script::new(64).param(10, LEVEL, 0.25));
    assert_eq!(instance.values().get(LEVEL), Some(0.25));
    instance.flush(&[Event::Param {
        time: 0,
        id: LEVEL,
        value: 1.0,
    }]);
    let block = instance.process(16, &[], &[]);
    assert!(block.outputs[1].iter().all(|s| *s == 1.0));
    assert_eq!(block.status, ProcessStatus::Continue);
}

#[test]
#[should_panic(expected = "in a real-time section")]
fn a_block_that_allocates_fails_under_the_trap() {
    let script = Script::new(256).param(100, ALLOCATE, 1.0);
    let _ = render::render::<Probe>(&script, Settings::new(RATE, 64));
}

#[test]
fn an_effect_takes_its_input_and_settings_are_checked() {
    let mut instance = render::Instance::<Probe>::new().expect("an instance");
    assert!(instance.activate(Settings::new(RATE, 0)).is_err());
    assert!(!instance.is_active());
    instance
        .activate(Settings::new(44_100.0, 32))
        .expect("active");
    assert_eq!(instance.settings(), Some(Settings::new(44_100.0, 32)));
    // An instrument's buffers start silent, whatever input the script has.
    let render = instance.render(&Script::new(32).with_mono_input(vec![1.0; 32]));
    assert!(render.output.channel(0).iter().all(|s| *s == 0.0));
    assert_eq!(render.output.sample_rate, 44_100.0);
    instance.deactivate();
    assert!(!instance.is_active());
}

/// An instrument that lays the transport it sees into its first channel:
/// the tempo, then the song position in beats, each NaN when the host
/// offers no transport.
struct ClockProbe;

impl Plugin for ClockProbe {
    type Processor = ClockProbeProcessor;

    const INFO: PluginInfo =
        PluginInfo::new("com.loombridge.test.clock", "Clock").audio(AudioLayout::instrument(1));

    fn new(_host: Host) -> Self {
        Self
    }

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        _values: &ParamValues,
    ) -> Result<ClockProbeProcessor, PluginError> {
        Ok(ClockProbeProcessor)
    }
}

struct ClockProbeProcessor;

impl Processor for ClockProbeProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        _events: &Events<'_>,
        cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        audio.silence();
        let (tempo, beats) = match cx.transport() {
            Some(transport) => (transport.tempo, transport.beats),
            None => (f64::NAN, f64::NAN),
        };
        audio.channel_mut(0)[0] = tempo as f32;
        audio.channel_mut(0)[1] = beats as f32;
        ProcessStatus::Continue
    }

    fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
}

#[test]
fn the_harness_offers_the_transport_it_was_given() {
    let mut instance = render::Instance::<ClockProbe>::new().expect("an instance");
    instance.activate(Settings::new(RATE, 64)).expect("active");
    // A host with no transport offers none.
    let block = instance.process(64, &[], &[]);
    assert!(block.outputs[0][0].is_nan(), "{:?}", block.outputs[0]);
    // One that sets a transport offers it at every block's first sample,
    // and moves the song position on between blocks.
    instance.set_transport(Some(Transport {
        tempo: 90.0,
        playing: true,
        beats: 8.5,
        ..Transport::default()
    }));
    let block = instance.process(64, &[], &[]);
    assert_eq!(block.outputs[0][0], 90.0);
    assert_eq!(block.outputs[0][1], 8.5);
    let block = instance.process(64, &[], &[]);
    let beats = 8.5 + 64.0 / 48_000.0 * 90.0 / 60.0;
    assert!(
        (f64::from(block.outputs[0][1]) - beats).abs() < 1e-3,
        "{} against {beats}",
        block.outputs[0][1]
    );
}
