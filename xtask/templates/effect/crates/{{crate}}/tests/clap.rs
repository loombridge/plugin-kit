//! {{Name}} as a host loads it: from its built `.clap` file, and from its
//! entry point in this binary under the allocation trap.

use lspk_test::clap::{Block, ClapTestHost, HostEvent, PluginEvent, ProcessStatus};
use serde_json::json;
use {{crate_lib}}::{DRIVE, GAIN, ID, NAME, RESET, WIDTH};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;
const FRAMES: usize = 512;

/// The plug-in from the `.clap` file Cargo built, active.
fn built() -> ClapTestHost {
    let clap = lspk_test::clap::bundle_built_library("{{crate_lib}}", NAME, ID);
    let mut host = ClapTestHost::load(&clap, ID).expect("the built plug-in");
    host.activate(RATE, FRAMES as u32).expect("activation");
    host
}

/// A block of stereo programme: a sine at `hz` on the left, and one a fifth
/// above it on the right.
fn programme(hz: f32) -> [Vec<f32>; 2] {
    let sine = |hz: f32| {
        (0..FRAMES)
            .map(|i| (i as f32 / RATE as f32 * hz * std::f32::consts::TAU).sin() * 0.5)
            .collect()
    };
    [sine(hz), sine(hz * 1.5)]
}

/// Three blocks of `input`, with `events` in the first; the last, once the
/// parameters' 20 ms glides have run.
fn settled(host: &mut ClapTestHost, input: &[Vec<f32>; 2], events: &[HostEvent]) -> Block {
    let channels = [input[0].as_slice(), input[1].as_slice()];
    host.process_channels(FRAMES, &channels, events);
    host.process_channels(FRAMES, &channels, &[]);
    host.process_channels(FRAMES, &channels, &[])
}

/// A change of parameter `id` to `value`, as the host keeps it (which
/// `ClapTestHost::parse` gives for a value's text).
fn param(id: u32, value: f64) -> HostEvent {
    HostEvent::Param { time: 0, id, value }
}

fn peak(signal: &[f32]) -> f32 {
    signal.iter().fold(0.0f32, |m, x| m.max(x.abs()))
}

#[test]
fn the_built_plugin_shows_its_parameters_and_passes_audio_unchanged() {
    let mut host = built();
    assert_eq!(host.audio_ports(true), [2]);
    assert_eq!(host.audio_ports(false), [2]);
    let params = host.params();
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Gain", "Drive", "Width"]);
    assert!(params.iter().all(|p| p.automatable));
    assert_eq!(
        host.text(GAIN.0, params[0].default).as_deref(),
        Some("0.00 dB")
    );

    let input = programme(440.0);
    let block = settled(&mut host, &input, &[]);
    assert_eq!(block.outputs, input, "at its defaults it changes nothing");
}

#[test]
fn width_at_zero_makes_it_mono() {
    let mut host = built();
    let mono = host.parse(WIDTH.0, "0 %").expect("a value");
    let block = settled(&mut host, &programme(440.0), &[param(WIDTH.0, mono)]);
    assert_eq!(block.outputs[0], block.outputs[1]);
}

#[test]
fn drive_bends_the_peaks_down() {
    let mut host = built();
    let input = programme(440.0);
    let full = host.parse(DRIVE.0, "100 %").expect("a value");
    let block = settled(&mut host, &input, &[param(DRIVE.0, full)]);
    assert!(
        peak(&block.outputs[0]) < 0.9 * peak(&input[0]),
        "{}",
        peak(&block.outputs[0])
    );
}

#[test]
fn silence_in_sleeps() {
    let mut host = built();
    let block = host.process(FRAMES, None, &[]);
    assert_eq!(block.status, ProcessStatus::Sleep);
    assert!(block.outputs.iter().flatten().all(|s| *s == 0.0));
}

#[test]
fn its_state_round_trips() {
    let mut first = built();
    let quarter = first.parse(GAIN.0, "-12 dB").expect("a value");
    let drive = first.parse(DRIVE.0, "40 %").expect("a value");
    first.flush(&[param(GAIN.0, quarter), param(DRIVE.0, drive)]);
    let state = first.save_state().expect("a state");
    let mut second = built();
    second.load_state(&state).expect("the state loads");
    assert_eq!(second.value(GAIN.0), Some(quarter));
    assert_eq!(second.value(DRIVE.0), Some(drive));
}

// The agent-commands extension: the host lists the plug-in's commands and
// runs its own, and hears the parameter changes as a person's gestures.
#[test]
fn a_host_runs_its_command_through_the_extension() {
    let mut host = ClapTestHost::from_entry(&{{crate_lib}}::clap_entry, ID).expect("the plug-in");
    let commands = host.commands().expect("the extension");
    let reset = commands
        .iter()
        .find(|c| c.id == RESET)
        .expect("its command");
    assert_eq!(reset.permission, Some(lspk_commands::Permission::Edit));
    assert!(reset.args.is_some() && reset.result.is_some());

    let drive = host.parse(DRIVE.0, "60 %").expect("a value");
    host.flush(&[param(DRIVE.0, drive)]);
    host.take_requests();
    assert_eq!(host.run_command(RESET, &json!({})), Ok(json!(null)));
    let clean = host.parse(DRIVE.0, "0 %").expect("a value");
    assert_eq!(host.value(DRIVE.0), Some(clean));
    let requests = host.take_requests();
    assert!(requests.flush && requests.dirty, "{requests:?}");
    let events = host.flush(&[]);
    assert!(
        events.contains(&PluginEvent::Param {
            time: 0,
            id: DRIVE.0,
            value: clean
        }),
        "{events:?}"
    );
}

#[test]
fn processing_stays_off_the_heap() {
    assert!(lspk_rt::trap::is_installed());
    // From the entry point in this binary, so the trap sees the plug-in's
    // allocations as well as the host's.
    let mut host = ClapTestHost::from_entry(&{{crate_lib}}::clap_entry, ID).expect("the plug-in");
    host.activate(RATE, FRAMES as u32).expect("activation");
    let input = programme(220.0);
    let channels = [input[0].as_slice(), input[1].as_slice()];
    for i in 0..64u32 {
        let events = [
            param(GAIN.0, f64::from(i % 4) / 2.0),
            HostEvent::Param {
                time: 100,
                id: DRIVE.0,
                value: f64::from(i % 5) / 4.0,
            },
            HostEvent::Param {
                time: 300,
                id: WIDTH.0,
                value: f64::from(i % 3) / 2.0,
            },
        ];
        host.process_channels(FRAMES, &channels, &events);
    }
}
