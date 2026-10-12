//! Example Modular through the CLAP test host, from its entry point in this
//! binary under the allocation trap: every parameter of every module
//! visible by name, a slot that changes its effect renaming its parameters
//! through a rescan, automation staying with the slot, the remote-controls
//! pages, the plug-in's own gestures, and state.

use lspk_example_modular::{ID, OSC, OSC_PITCH, edit_newest, effects, id, slot_param, slot_type};
use lspk_test::clap::{ClapTestHost, HostEvent, PluginEvent, Rescan};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;
const FRAMES: usize = 256;

fn instance() -> ClapTestHost {
    ClapTestHost::from_entry(&lspk_example_modular::clap_entry, ID).expect("the plug-in")
}

/// The visible parameters' names, in order.
fn visible(host: &mut ClapTestHost) -> Vec<String> {
    host.params()
        .into_iter()
        .filter(|p| !p.hidden)
        .map(|p| p.name)
        .collect()
}

/// The rescan a slot's change asks for: info, text and values, and never
/// a full rescan, which would need a restart.
const SLOT_CHANGED: Rescan = Rescan {
    values: true,
    text: true,
    info: true,
    all: false,
};

#[test]
fn every_parameter_of_every_module_is_visible_by_name() {
    let mut host = instance();
    let params = host.params();
    assert_eq!(params.len(), 1 + 2 * 3 + 1 + 2 + 2 * (1 + 4));
    assert!(params.iter().all(|p| p.automatable));
    assert_eq!(
        visible(&mut host),
        [
            "Volume",
            "Osc A Level",
            "Osc A Shape",
            "Osc A Pitch",
            "Osc B Level",
            "Osc B Shape",
            "Osc B Pitch",
            "Filter 1 Cutoff",
            "Env 1 Attack",
            "Env 1 Release",
            "FX 1 Type",
            "FX 2 Type",
        ]
    );
    let pitch = params
        .iter()
        .find(|p| p.id == id(OSC, 1, OSC_PITCH).0)
        .expect("Osc B Pitch");
    assert_eq!(pitch.id, 0x0101_0002);
    assert_eq!(pitch.module, "Osc B");
    assert!(pitch.stepped);
    assert_eq!(host.text(pitch.id, -12.0).as_deref(), Some("-12 st"));
    let rack = params
        .iter()
        .find(|p| p.id == slot_type(0).0)
        .expect("FX 1 Type");
    assert!(rack.is_enum && rack.stepped);
    assert_eq!((rack.min, rack.max), (0.0, 3.0));
    assert_eq!(host.text(rack.id, 2.0).as_deref(), Some("Delay"));
    // Each slot's four reserved parameters, hidden while it holds nothing,
    // and always the same shape to the host.
    let reserved: Vec<_> = params
        .iter()
        .filter(|p| p.module == "FX 1")
        .skip(1)
        .collect();
    assert_eq!(reserved.len(), 4);
    assert!(reserved.iter().all(|p| p.hidden && !p.stepped));
    assert!(reserved.iter().all(|p| (p.min, p.max) == (0.0, 1.0)));
    let pages: Vec<String> = host.remote_pages().into_iter().map(|p| p.name).collect();
    assert_eq!(
        pages,
        [
            "Osc A", "Osc B", "Filter 1", "Env 1", "FX 1", "FX 2", "Global"
        ]
    );
}

#[test]
fn a_slot_takes_its_new_effects_names_ranges_and_text() {
    let mut host = instance();
    host.activate(RATE, FRAMES as u32).expect("activation");
    let ids_before: Vec<u32> = host.params().iter().map(|p| p.id).collect();

    // The host's automation puts a delay in slot 1, on the audio thread.
    let type_change = HostEvent::Param {
        time: 0,
        id: slot_type(0).0,
        value: effects::DELAY as f64,
    };
    host.process(FRAMES, None, &[type_change]);
    assert!(
        host.take_requests().callback,
        "the plug-in asks for the main thread"
    );
    host.on_main_thread();
    let (rescans, pages_changed) = host.take_rescans();
    assert_eq!(rescans, [SLOT_CHANGED]);
    assert!(pages_changed);

    let names: Vec<String> = visible(&mut host);
    assert!(
        names.ends_with(&[
            "FX 1 Type".into(),
            "FX 1 Time".into(),
            "FX 1 Feedback".into(),
            "FX 1 Mix".into(),
            "FX 1 Tone".into(),
            "FX 2 Type".into(),
        ]),
        "{names:?}"
    );
    // Its real range and text, and its default.
    let time = slot_param(0, 0).0;
    assert_eq!(host.text(time, 0.0).as_deref(), Some("1.00 ms"));
    assert_eq!(host.text(time, 1.0).as_deref(), Some("1.00 s"));
    let mix = host.value(slot_param(0, 2).0).expect("a value");
    assert_eq!(host.text(slot_param(0, 2).0, mix).as_deref(), Some("30 %"));
    let half_second = host.parse(time, "500 ms").expect("a value");
    assert!((host.text(time, half_second).unwrap_or_default()) == "500 ms");

    // Automation recorded on the slot's parameter reaches what it means
    // now.
    host.process(
        FRAMES,
        None,
        &[HostEvent::Param {
            time: 0,
            id: time,
            value: half_second,
        }],
    );
    assert_eq!(host.value(time), Some(half_second));

    // The remote-controls page follows the slot.
    let pages = host.remote_pages();
    let fx = pages
        .iter()
        .find(|p| p.name == "FX 1")
        .expect("FX 1's page");
    assert_eq!(fx.section, "FX");
    assert_eq!(
        fx.params,
        [
            Some(slot_type(0).0),
            Some(slot_param(0, 0).0),
            Some(slot_param(0, 1).0),
            Some(slot_param(0, 2).0),
            Some(slot_param(0, 3).0),
            None,
            None,
            None
        ]
    );

    // A tremolo uses two of the four, and hides the rest.
    host.process(
        FRAMES,
        None,
        &[HostEvent::Param {
            time: 0,
            id: slot_type(0).0,
            value: effects::TREMOLO as f64,
        }],
    );
    host.on_main_thread();
    assert_eq!(host.take_rescans().0, [SLOT_CHANGED]);
    let params = host.params();
    let slot: Vec<(&str, bool)> = params
        .iter()
        .filter(|p| p.module == "FX 1")
        .map(|p| (p.name.as_str(), p.hidden))
        .collect();
    assert_eq!(
        slot,
        [
            ("FX 1 Type", false),
            ("FX 1 Rate", false),
            ("FX 1 Depth", false),
            ("FX 1 Slot 3", true),
            ("FX 1 Slot 4", true),
        ]
    );
    assert_eq!(host.text(time, 1.0).as_deref(), Some("20.0 Hz"));
    // Every ID is as it was, and the other slot is untouched.
    let ids_after: Vec<u32> = params.iter().map(|p| p.id).collect();
    assert_eq!(ids_after, ids_before);
    assert!(
        params
            .iter()
            .filter(|p| p.module == "FX 2")
            .skip(1)
            .all(|p| p.hidden)
    );

    // A main-thread callback with nothing changed asks for nothing.
    host.on_main_thread();
    assert!(host.take_rescans().0.is_empty());
}

#[test]
fn the_plugins_own_type_change_reaches_the_host_and_its_state_round_trips() {
    let mut host = instance();
    host.activate(RATE, FRAMES as u32).expect("activation");
    // As the plug-in's interface or an agent would: a gesture, the slot's
    // parameters retold at once.
    edit_newest(slot_type(1), effects::DISTORTION as f64).expect("an edit");
    assert_eq!(host.take_rescans().0, [SLOT_CHANGED]);
    let block = host.process(FRAMES, None, &[]);
    let selector = slot_type(1).0;
    let events: Vec<PluginEvent> = block
        .events
        .iter()
        .copied()
        .filter(|e| !matches!(e, PluginEvent::Other))
        .collect();
    assert_eq!(
        events,
        [
            PluginEvent::GestureBegin {
                time: 0,
                id: selector
            },
            PluginEvent::Param {
                time: 0,
                id: selector,
                value: 1.0
            },
            PluginEvent::GestureEnd {
                time: 0,
                id: selector
            },
        ],
        "the new effect's defaults aren't edits the host records"
    );
    let shape = slot_param(1, 1).0;
    assert_eq!(host.text(shape, 1.0).as_deref(), Some("Hard"));
    let drive = host.value(slot_param(1, 0).0).expect("a value");
    assert!((drive - 0.5).abs() < 1e-12, "Drive starts at its default");

    // Some sound through it, under the allocation trap.
    let note = HostEvent::NoteOn {
        time: 0,
        key: 60,
        channel: 0,
        note_id: None,
        velocity: 1.0,
    };
    let mut peak = 0.0f32;
    host.process(FRAMES, None, &[note]);
    for _ in 0..8 {
        let block = host.process(FRAMES, None, &[]);
        peak = block.outputs[0].iter().fold(peak, |p, s| p.max(s.abs()));
        assert!(block.outputs[0].iter().all(|s| s.is_finite()));
    }
    assert!(peak > 0.01, "it plays");

    host.process(
        FRAMES,
        None,
        &[HostEvent::Param {
            time: 0,
            id: shape,
            value: 1.0,
        }],
    );
    let state = host.save_state().expect("a state");
    drop(host);

    let mut other = instance();
    assert!(
        other
            .params()
            .iter()
            .find(|p| p.id == shape)
            .is_some_and(|p| p.hidden)
    );
    other.load_state(&state).expect("the state loads");
    // The loaded slot keeps the state's values, and the host rereads names.
    assert_eq!(other.take_rescans().0, [SLOT_CHANGED]);
    assert_eq!(other.value(selector), Some(1.0));
    assert_eq!(other.value(shape), Some(1.0));
    let names = visible(&mut other);
    assert!(names.contains(&"FX 2 Drive".to_owned()), "{names:?}");
    assert_eq!(other.text(shape, 1.0).as_deref(), Some("Hard"));
}
