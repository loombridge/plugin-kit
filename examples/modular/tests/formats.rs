//! Example Modular as VST3 and (on macOS) Audio Unit hosts load it, from
//! the entry points this crate exports beside its CLAP entry: a slot that
//! changes its effect renames its parameters through clap-wrapper's
//! `restartComponent` and Audio Unit property notifications, the
//! plug-in's own type change reaches each host as a gesture, and state
//! round-trips. Each test needs a process of its own, as cargo-nextest
//! gives it (see lspk-plugin's tests/formats.rs).

use std::time::Duration;

use lspk_example_modular::{edit_newest, effects, slot_param, slot_type};

const FRAMES: usize = 256;
const WAIT: Duration = Duration::from_secs(5);

/// VST3's `kParamValuesChanged | kParamTitlesChanged`.
const TITLES_AND_VALUES: i32 = 4 | 16;

/// clap-wrapper's VST3 idle timer, which calls the plug-in back on the main
/// thread and passes its gestures on, runs without an editor only on macOS
/// and Windows (see lspk_test::vst3).
const IDLE_TIMER: bool = cfg!(any(target_os = "macos", windows));

#[test]
fn the_vst3_export_renames_a_slots_parameters_and_keeps_its_state() {
    use lspk_test::vst3::{HandlerEvent, Vst3TestHost};

    #[cfg(windows)]
    // SAFETY: the crate's own module entry, as a host calls it.
    unsafe {
        lspk_example_modular::InitDll();
    }
    // SAFETY: the crate's own VST3 factory, which lives as long as the
    // process.
    let factory = unsafe { lspk_example_modular::GetPluginFactory() };
    // SAFETY: as above.
    let mut host = unsafe { Vst3TestHost::from_factory(factory) }.expect("an instance");
    let title = |host: &Vst3TestHost, id: u32| {
        host.params()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| (p.title, p.hidden))
    };
    assert_eq!(
        title(&host, 0x0100_0000),
        Some(("Osc A Level".to_owned(), false))
    );
    let time = slot_param(0, 0).0;
    assert_eq!(title(&host, time), Some(("FX 1 Slot 1".to_owned(), true)));

    host.activate(48_000.0, FRAMES as u32).expect("activation");
    let restarted = |events: &[HandlerEvent]| {
        events.iter().any(
            |e| matches!(e, HandlerEvent::Restart(flags) if flags & TITLES_AND_VALUES == TITLES_AND_VALUES),
        )
    };

    // The plug-in's own change (its interface's, an agent's) restarts the
    // component's titles and values at once, and reaches the host as an
    // edit: from the next block, and as a gesture from the idle timer.
    edit_newest(slot_type(1), effects::TREMOLO as f64).expect("an edit");
    let events = host.take_handler_events();
    assert!(restarted(&events), "restartComponent: {events:?}");
    let block = host.process(FRAMES, None, &[], &[]);
    assert!(
        block.changes.contains(&(slot_type(1).0, 0, 1.0)),
        "{:?}",
        block.changes
    );
    if IDLE_TIMER {
        assert!(host.idle_until(HandlerEvent::End(slot_type(1).0), WAIT));
        let events = host.take_handler_events();
        assert!(
            events.contains(&HandlerEvent::Begin(slot_type(1).0)),
            "{events:?}"
        );
    }
    assert_eq!(
        title(&host, slot_param(1, 1).0),
        Some(("FX 2 Depth".to_owned(), false))
    );
    assert_eq!(
        title(&host, slot_param(1, 2).0).map(|t| t.1),
        Some(true),
        "Tremolo leaves its third parameter hidden"
    );

    // The host's automation puts a delay in slot 1 (three steps, so the
    // delay is at two thirds), on the audio thread; the plug-in renames
    // the slot's parameters when the wrapper next calls it on the main
    // thread, from its idle timer.
    host.process(FRAMES, None, &[], &[(slot_type(0).0, 0, 2.0 / 3.0)]);
    if IDLE_TIMER {
        let start = std::time::Instant::now();
        let mut seen = Vec::new();
        while !restarted(&seen) && start.elapsed() < WAIT {
            host.idle(Duration::from_millis(20));
            seen.extend(host.take_handler_events());
        }
        assert!(restarted(&seen), "restartComponent: {seen:?}");
        assert_eq!(title(&host, time), Some(("FX 1 Time".to_owned(), false)));
        assert_eq!(
            title(&host, slot_param(0, 3).0),
            Some(("FX 1 Tone".to_owned(), false))
        );
        assert_eq!(host.text(time, 0.0).as_deref(), Some("1.00 ms"));
        assert_eq!(host.text(time, 1.0).as_deref(), Some("1.00 s"));
    }

    let state = host.save_state().expect("a state");
    drop(host);
    // SAFETY: as above.
    let mut other = unsafe { Vst3TestHost::from_factory(factory) }.expect("an instance");
    assert_eq!(title(&other, time).map(|t| t.1), Some(true));
    other.load_state(&state).expect("the state loads");
    assert_eq!(title(&other, time), Some(("FX 1 Time".to_owned(), false)));
    assert_eq!(
        title(&other, slot_param(1, 0).0),
        Some(("FX 2 Rate".to_owned(), false))
    );
    assert!((other.value(slot_type(0).0) - 2.0 / 3.0).abs() < 1e-9);
}

#[cfg(target_os = "macos")]
#[test]
fn the_audio_unit_export_renames_a_slots_parameters_and_keeps_its_state() {
    use lspk_test::auv2::{AuEvent, AuTestHost};

    /// `kAudioUnitProperty_ParameterList`.
    const PARAMETER_LIST: u32 = 3;
    let factory = lspk_example_modular::GetPluginFactoryAUV2;
    let mut host = AuTestHost::new(factory, CODES).expect("an instance");
    assert_eq!(host.param_ids().len(), 1 + 2 * 3 + 1 + 2 + 2 * (1 + 4));
    let osc = host.param(0x0100_0000).expect("Osc A Level");
    assert_eq!(osc.name, "Osc A Level");
    let time = slot_param(0, 0).0;
    let param = host.param(time).expect("FX 1's first slot");
    assert!(!param.writable, "hidden while the slot holds nothing");

    host.initialize(FRAMES as u32).expect("initialisation");
    host.set_value(slot_type(0).0, effects::DELAY as f32)
        .expect("a value");
    // The plug-in takes the host's value in its next block, then asks for
    // the main thread, where it tells the host.
    host.render(FRAMES as u32).expect("a block");
    assert!(host.wait_for_property(PARAMETER_LIST, WAIT));
    let param = host.param(time).expect("FX 1 Time");
    assert_eq!(param.name, "FX 1 Time");
    assert!(param.writable);
    assert_eq!((param.min, param.max), (0.0, 1.0));
    assert_eq!(host.text(time, 0.0).as_deref(), Some("1.00 ms"));
    assert_eq!(host.text(time, 1.0).as_deref(), Some("1.00 s"));

    // The plug-in's own change reaches the host as a gesture.
    edit_newest(slot_type(1), effects::DISTORTION as f64).expect("an edit");
    host.render(FRAMES as u32).expect("a block");
    assert!(host.listen_until(AuEvent::End(slot_type(1).0), WAIT));
    assert!(host.take_events().contains(&AuEvent::Begin(slot_type(1).0)));
    assert_eq!(
        host.param(slot_param(1, 1).0).map(|p| p.name).as_deref(),
        Some("FX 2 Shape")
    );
    assert_eq!(host.text(slot_param(1, 1).0, 1.0).as_deref(), Some("Hard"));

    let state = host.save_state().expect("a state");
    drop(host);
    let mut other = AuTestHost::new(factory, CODES).expect("an instance");
    other.initialize(FRAMES as u32).expect("initialisation");
    other.load_state(&state).expect("the state loads");
    assert_eq!(other.value(slot_type(0).0), Some(effects::DELAY as f32));
    assert_eq!(
        other.param(time).map(|p| p.name).as_deref(),
        Some("FX 1 Time")
    );
    assert_eq!(
        other.param(slot_param(1, 0).0).map(|p| p.name).as_deref(),
        Some("FX 2 Drive")
    );
}

/// Codes of the test's own, so an installed copy of the plug-in never
/// answers instead.
#[cfg(target_os = "macos")]
const CODES: [&str; 3] = ["aumu", "Tmod", "Lspk"];
