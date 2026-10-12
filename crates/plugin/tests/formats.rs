//! The VST3 and Audio Unit exports, made from the CLAP export by
//! clap-wrapper, through a VST3 host and (on macOS) an Audio Unit host,
//! with a probe plug-in exported from this test binary: the plug-in's own
//! edits reach each host as that format's edit notifications, and its
//! state round-trips.
//!
//! Each test needs a process of its own, as cargo-nextest (which
//! `cargo xtask check` runs) gives it: clap-wrapper keeps its instances and
//! its idle timer in process-wide state, on the run loop of the thread that
//! activated the first instance.

#![cfg(feature = "vst3")]

use std::cell::RefCell;

use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, NotePorts, ParamValues, Plugin, PluginError,
    PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor, features,
};

const AMOUNT: ParamId = ParamId(10);
const MODE: ParamId = ParamId(20);
const RATE: f64 = 48_000.0;
const FRAMES: usize = 256;

thread_local! {
    /// The newest probe's host, so a test can edit as the plug-in would.
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    /// The plug-in's own state the newest probe loaded.
    static LOADED: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// The plug-in edits `id` to `value` from the main thread, as an agent's
/// command or its interface would.
fn edit(id: ParamId, value: f64) {
    HOST.with_borrow(|host| {
        host.as_ref()
            .expect("a probe")
            .edit(id, value)
            .expect("an edit");
    });
}

/// An instrument whose note-ons set Mode to C from the audio thread, in a
/// gesture, and which keeps some state of its own.
struct Probe;

impl Plugin for Probe {
    type Processor = ProbeProcessor;

    const INFO: PluginInfo = PluginInfo::new("com.loombridge.test.formats", "Format Probe")
        .vendor("Loombridge")
        .version("1.0.0")
        .features(&[features::INSTRUMENT, features::STEREO])
        .audio(AudioLayout::instrument(2))
        .notes(NotePorts::INPUT);

    fn new(host: Host) -> Self {
        HOST.set(Some(host));
        Self
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        vec![
            ParamDescriptor::continuous(AMOUNT, "Amount", (0.0, 1.0), 0.5, ParamUnit::None),
            ParamDescriptor::choice(MODE, "Mode", &["A", "B", "C"], 1),
        ]
    }

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        _values: &ParamValues,
    ) -> Result<ProbeProcessor, PluginError> {
        Ok(ProbeProcessor)
    }

    fn save_state(&self) -> Vec<u8> {
        b"probe state".to_vec()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        LOADED.set(data.to_vec());
        Ok(())
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
        for event in events.iter() {
            if let Event::NoteOn { time, .. } = event {
                cx.begin_gesture(time, MODE);
                cx.set_param(time, MODE, 2.0);
                cx.end_gesture(time, MODE);
            }
        }
        ProcessStatus::Continue
    }

    fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
}

lspk_plugin::export_clap!(Probe);

mod vst3 {
    use std::time::Duration;

    use lspk_test::vst3::{HandlerEvent, Vst3TestHost};

    use super::*;

    /// clap-wrapper's idle timer reaches the host only where the platform
    /// gives it a timer without an editor: macOS and Windows (see
    /// lspk_test::vst3).
    const IDLE_TIMER: bool = cfg!(any(target_os = "macos", windows));
    const WAIT: Duration = Duration::from_secs(5);

    fn host() -> Vst3TestHost {
        // A host on Windows calls the module's InitDll first, which starts
        // the wrapper's idle timer.
        #[cfg(windows)]
        // SAFETY: this binary's own module entry, as a host calls it.
        unsafe {
            super::InitDll();
        }
        // SAFETY: this binary's own VST3 factory, which lives as long as the
        // process.
        unsafe { Vst3TestHost::from_factory(super::GetPluginFactory()) }.expect("an instance")
    }

    /// The handler's events for parameter `id`, in order.
    fn edits_of(events: &[HandlerEvent], id: u32) -> Vec<HandlerEvent> {
        events
            .iter()
            .copied()
            .filter(|e| match e {
                HandlerEvent::Begin(p) | HandlerEvent::End(p) | HandlerEvent::Perform(p, _) => {
                    *p == id
                }
                _ => false,
            })
            .collect()
    }

    #[test]
    fn parameters_are_listed_with_their_ranges() {
        let host = host();
        // clap-wrapper adds hidden parameters of its own after the
        // plug-in's, standing for MIDI controllers, since VST3 has no MIDI
        // input of its own.
        let params: Vec<_> = host
            .params()
            .into_iter()
            .filter(|p| !p.title.starts_with("MIDI/"))
            .collect();
        let summary: Vec<(u32, &str, i32)> = params
            .iter()
            .map(|p| (p.id, p.title.as_str(), p.steps))
            .collect();
        assert_eq!(summary, [(AMOUNT.0, "Amount", 0), (MODE.0, "Mode", 2)]);
        assert_eq!(params[0].default, 0.5);
        assert_eq!(params[1].default, 0.5);
    }

    #[test]
    fn audio_thread_gestures_reach_the_host_as_edits() {
        let mut host = host();
        host.activate(RATE, FRAMES as u32).expect("activation");
        let block = host.process(FRAMES, None, &[(0, 60, 1.0)], &[]);
        // The value reaches the host from the block, as automation.
        assert!(
            block.changes.contains(&(MODE.0, 0, 1.0)),
            "{:?}",
            block.changes
        );
        if IDLE_TIMER {
            assert!(host.idle_until(HandlerEvent::End(MODE.0), WAIT));
            assert_eq!(
                edits_of(&host.take_handler_events(), MODE.0),
                [
                    HandlerEvent::Begin(MODE.0),
                    HandlerEvent::Perform(MODE.0, 1.0),
                    HandlerEvent::End(MODE.0),
                ]
            );
        }
    }

    #[test]
    fn main_thread_edits_reach_the_host_as_edits_and_dirty_the_state() {
        let mut host = host();
        host.activate(RATE, FRAMES as u32).expect("activation");
        host.process(FRAMES, None, &[], &[]);
        host.take_handler_events();

        // While processing, the next block carries the edit.
        edit(AMOUNT, 0.25);
        let block = host.process(FRAMES, None, &[], &[]);
        assert!(
            block.changes.contains(&(AMOUNT.0, 0, 0.25)),
            "{:?}",
            block.changes
        );
        if IDLE_TIMER {
            assert!(host.idle_until(HandlerEvent::End(AMOUNT.0), WAIT));
            let events = host.take_handler_events();
            assert_eq!(
                edits_of(&events, AMOUNT.0),
                [
                    HandlerEvent::Begin(AMOUNT.0),
                    HandlerEvent::Perform(AMOUNT.0, 0.25),
                    HandlerEvent::End(AMOUNT.0),
                ]
            );
            assert!(events.contains(&HandlerEvent::Dirty(true)), "{events:?}");

            // While active but not processing, the wrapper flushes the
            // edit from its idle timer.
            host.set_processing(false).expect("processing stops");
            edit(AMOUNT, 0.75);
            assert!(host.idle_until(HandlerEvent::End(AMOUNT.0), WAIT));
            assert_eq!(
                edits_of(&host.take_handler_events(), AMOUNT.0),
                [
                    HandlerEvent::Begin(AMOUNT.0),
                    HandlerEvent::Perform(AMOUNT.0, 0.75),
                    HandlerEvent::End(AMOUNT.0),
                ]
            );
        }

        // While inactive, the edit waits for the first block after the host
        // activates it again.
        host.deactivate();
        edit(AMOUNT, 0.5);
        host.activate(RATE, FRAMES as u32).expect("activation");
        let block = host.process(FRAMES, None, &[], &[]);
        assert!(
            block.changes.contains(&(AMOUNT.0, 0, 0.5)),
            "{:?}",
            block.changes
        );
    }

    #[test]
    fn state_round_trips_through_the_component() {
        let mut first = host();
        first.activate(RATE, FRAMES as u32).expect("activation");
        // The host's automation sets both parameters.
        first.process(FRAMES, None, &[], &[(AMOUNT.0, 0, 0.3), (MODE.0, 0, 0.0)]);
        let state = first.save_state().expect("a state");
        drop(first);

        let mut second = host();
        assert_eq!(second.value(AMOUNT.0), 0.5);
        second.load_state(&state).expect("the state loads");
        assert!((second.value(AMOUNT.0) - 0.3).abs() < 1e-9);
        assert_eq!(second.value(MODE.0), 0.0);
        assert_eq!(LOADED.with_borrow(Clone::clone), b"probe state");
        assert_eq!(second.save_state().expect("a state"), state);
    }
}

#[cfg(all(target_os = "macos", feature = "auv2"))]
mod auv2 {
    use std::time::Duration;

    use lspk_test::auv2::{AuEvent, AuTestHost};

    use super::*;

    /// The probe's Audio Unit codes, registered in this process only.
    const CODES: [&str; 3] = ["aumu", "Prbe", "Lspk"];
    const WAIT: Duration = Duration::from_secs(5);

    fn host() -> AuTestHost {
        AuTestHost::new(super::GetPluginFactoryAUV2, CODES).expect("an instance")
    }

    /// The listener's events for parameter `id`, in order, with repeated
    /// values dropped.
    fn edits_of(events: &[AuEvent], id: u32) -> Vec<AuEvent> {
        let mut edits: Vec<AuEvent> = events
            .iter()
            .copied()
            .filter(|e| match e {
                AuEvent::Begin(p) | AuEvent::End(p) | AuEvent::Value(p, _) => *p == id,
            })
            .collect();
        edits.dedup();
        edits
    }

    #[test]
    fn audio_thread_gestures_reach_listeners() {
        let mut host = host();
        assert_eq!(host.param_ids(), [AMOUNT.0, MODE.0]);
        host.initialize(FRAMES as u32).expect("initialisation");
        host.midi(0x90, 60, 100);
        host.render(FRAMES as u32).expect("a block");
        assert!(host.listen_until(AuEvent::End(MODE.0), WAIT));
        assert_eq!(
            edits_of(&host.take_events(), MODE.0),
            [
                AuEvent::Begin(MODE.0),
                AuEvent::Value(MODE.0, 2.0),
                AuEvent::End(MODE.0),
            ]
        );
        assert_eq!(host.value(MODE.0), Some(2.0));
    }

    #[test]
    fn main_thread_edits_reach_listeners() {
        let mut host = host();
        host.initialize(FRAMES as u32).expect("initialisation");
        host.render(FRAMES as u32).expect("a block");
        host.take_events();
        edit(AMOUNT, 0.25);
        host.render(FRAMES as u32).expect("a block");
        assert!(host.listen_until(AuEvent::End(AMOUNT.0), WAIT));
        assert_eq!(
            edits_of(&host.take_events(), AMOUNT.0),
            [
                AuEvent::Begin(AMOUNT.0),
                AuEvent::Value(AMOUNT.0, 0.25),
                AuEvent::End(AMOUNT.0),
            ]
        );
        assert_eq!(host.value(AMOUNT.0), Some(0.25));
    }

    #[test]
    fn state_round_trips_through_class_info() {
        let mut first = host();
        first.initialize(FRAMES as u32).expect("initialisation");
        first.set_value(AMOUNT.0, 0.3).expect("a value");
        first.set_value(MODE.0, 0.0).expect("a value");
        first.render(FRAMES as u32).expect("a block");
        let state = first.save_state().expect("a state");
        drop(first);

        let mut second = host();
        second.initialize(FRAMES as u32).expect("initialisation");
        assert_eq!(second.value(AMOUNT.0), Some(0.5));
        second.load_state(&state).expect("the state loads");
        let amount = second.value(AMOUNT.0).expect("a value");
        assert!((amount - 0.3).abs() < 1e-6, "{amount}");
        assert_eq!(second.value(MODE.0), Some(0.0));
        assert_eq!(LOADED.with_borrow(Clone::clone), b"probe state");
    }
}
