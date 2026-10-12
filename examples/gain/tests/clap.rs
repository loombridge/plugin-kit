//! Example Gain as a host loads it: from its built `.clap` file, and from
//! its entry point in this binary under the allocation trap.

use lspk_example_gain::{GAIN, ID, NAME};
use lspk_test::clap::{ClapTestHost, HostEvent, ProcessStatus};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;
const FRAMES: usize = 256;

/// The gain plug-in from the `.clap` file Cargo built.
fn built() -> ClapTestHost {
    let clap = lspk_test::clap::bundle_built_library("lspk_example_gain", NAME, ID);
    ClapTestHost::load(&clap, ID).expect("the built plug-in")
}

/// Runs `blocks` blocks of `input` with `events` in the first, and returns
/// the last.
fn run(
    host: &mut ClapTestHost,
    input: &[f32],
    events: &[HostEvent],
    blocks: usize,
) -> lspk_test::clap::Block {
    let mut block = host.process(FRAMES, Some(input), events);
    for _ in 1..blocks {
        block = host.process(FRAMES, Some(input), &[]);
    }
    block
}

#[test]
fn the_built_plugin_applies_its_gain_down_to_silence() {
    let mut host = built();
    assert_eq!(host.audio_ports(true), [2]);
    assert_eq!(host.audio_ports(false), [2]);
    assert_eq!(host.note_ports(true), 0);
    let params = host.params();
    assert_eq!(params.len(), 1);
    assert_eq!((params[0].id, params[0].name.as_str()), (GAIN.0, "Gain"));
    assert_eq!(
        host.text(GAIN.0, params[0].default).as_deref(),
        Some("0.00 dB")
    );
    assert_eq!(host.latency(), Some(0));

    host.activate(RATE, FRAMES as u32).expect("activation");
    let input: Vec<f32> = (0..FRAMES).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    // At 0 dB the input passes unchanged.
    let block = run(&mut host, &input, &[], 1);
    assert_eq!(block.outputs, [input.clone(), input.clone()]);
    assert_eq!(block.status, ProcessStatus::ContinueIfNotQuiet);

    // −6.02 dB halves it, once the 20 ms glide has run.
    let half = host.parse(GAIN.0, "-6.0206 dB").expect("a value");
    let block = run(
        &mut host,
        &input,
        &[HostEvent::Param {
            time: 0,
            id: GAIN.0,
            value: half,
        }],
        5,
    );
    for (out, inp) in block.outputs[0].iter().zip(&input) {
        assert!((out - inp * 0.5).abs() < 1e-5, "{out} is not half of {inp}");
    }
    assert_eq!(block.outputs[0], block.outputs[1]);

    // The bottom of the travel is −∞ dB: silence.
    let block = run(
        &mut host,
        &input,
        &[HostEvent::Param {
            time: 0,
            id: GAIN.0,
            value: 0.0,
        }],
        5,
    );
    assert!(block.outputs.iter().flatten().all(|s| *s == 0.0));
    assert_eq!(host.text(GAIN.0, 0.0).as_deref(), Some("-inf dB"));
}

#[test]
fn a_change_glides_from_its_frame() {
    let mut host = built();
    host.activate(RATE, FRAMES as u32).expect("activation");
    let ones = vec![1.0f32; FRAMES];
    let block = run(
        &mut host,
        &ones,
        &[HostEvent::Param {
            time: 100,
            id: GAIN.0,
            value: 0.0,
        }],
        1,
    );
    let out = &block.outputs[0];
    // Unchanged before the event's frame, falling after it.
    assert!(out[..100].iter().all(|s| *s == 1.0));
    assert!(out[100] < 1.0 && out[255] < out[100] && out[255] > 0.0);
}

#[test]
fn silence_puts_it_to_sleep() {
    let mut host = built();
    host.activate(RATE, FRAMES as u32).expect("activation");
    let block = run(&mut host, &[0.0; FRAMES], &[], 1);
    assert_eq!(block.status, ProcessStatus::Sleep);
    assert!(block.outputs.iter().flatten().all(|s| *s == 0.0));
    let block = run(&mut host, &[0.25; FRAMES], &[], 1);
    assert_eq!(block.status, ProcessStatus::ContinueIfNotQuiet);
}

#[test]
fn its_state_round_trips() {
    let mut first = built();
    let quarter = first.parse(GAIN.0, "-12 dB").expect("a value");
    first.flush(&[HostEvent::Param {
        time: 0,
        id: GAIN.0,
        value: quarter,
    }]);
    let state = first.save_state().expect("a state");
    let mut second = built();
    second.load_state(&state).expect("the state loads");
    assert_eq!(second.value(GAIN.0), Some(quarter));
}

#[test]
fn processing_stays_off_the_heap() {
    assert!(lspk_rt::trap::is_installed());
    // From the entry point in this binary, so the trap sees the plug-in's
    // allocations as well as the host's.
    let mut host =
        ClapTestHost::from_entry(&lspk_example_gain::clap_entry, ID).expect("the plug-in");
    host.activate(RATE, FRAMES as u32).expect("activation");
    let input = [0.5f32; FRAMES];
    for i in 0..64u32 {
        let value = f64::from(i % 8) / 8.0;
        let events = [HostEvent::Param {
            time: i % FRAMES as u32,
            id: GAIN.0,
            value,
        }];
        host.process(FRAMES, Some(&input), &events);
    }
}
