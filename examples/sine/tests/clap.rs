//! Example Sine as a host loads it: from its built `.clap` file, and from
//! its entry point in this binary under the allocation trap.

use lspk_example_sine::{ID, LEVEL, NAME, VOICES, frequency};
use lspk_test::clap::{Block, ClapTestHost, HostEvent, PluginEvent, ProcessStatus};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;
const FRAMES: usize = 512;

/// The sine plug-in from the `.clap` file Cargo built, active.
fn built() -> ClapTestHost {
    let clap = lspk_test::clap::bundle_built_library("lspk_example_sine", NAME, ID);
    let mut host = ClapTestHost::load(&clap, ID).expect("the built plug-in");
    host.activate(RATE, FRAMES as u32).expect("activation");
    host
}

fn note_on(time: u32, key: u16) -> HostEvent {
    HostEvent::NoteOn {
        time,
        key,
        channel: 0,
        note_id: None,
        velocity: 1.0,
    }
}

fn note_off(time: u32, key: u16) -> HostEvent {
    HostEvent::NoteOff {
        time,
        key: Some(key),
        channel: Some(0),
        note_id: None,
    }
}

/// `blocks` blocks, with `events` in the first; every block's left channel
/// joined, and the last block.
fn render(host: &mut ClapTestHost, events: &[HostEvent], blocks: usize) -> (Vec<f32>, Block) {
    let mut left = Vec::new();
    let mut block = host.process(FRAMES, None, events);
    left.extend_from_slice(&block.outputs[0]);
    for _ in 1..blocks {
        block = host.process(FRAMES, None, &[]);
        left.extend_from_slice(&block.outputs[0]);
    }
    (left, block)
}

/// The pitch of `signal`, from the time between its first and last rising
/// zero crossings, each placed between samples by interpolation.
fn pitch(signal: &[f32]) -> f64 {
    let crossings: Vec<f64> = signal
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + f64::from(w[0] / (w[0] - w[1])))
        .collect();
    let (first, last) = (crossings[0], crossings[crossings.len() - 1]);
    (crossings.len() - 1) as f64 * RATE / (last - first)
}

#[test]
fn the_built_plugin_plays_each_key_at_its_pitch() {
    let mut host = built();
    assert_eq!(host.audio_ports(true), Vec::<u32>::new());
    assert_eq!(host.audio_ports(false), [2]);
    assert_eq!(host.note_ports(true), 1);
    let params = host.params();
    assert_eq!((params.len(), params[0].name.as_str()), (1, "Level"));
    assert_eq!(
        host.text(LEVEL.0, params[0].default).as_deref(),
        Some("-6.0 dB")
    );

    for key in [69u16, 60, 81] {
        let (left, block) = render(&mut host, &[note_on(0, key)], 48);
        // Past the attack and decay, about 0.1 s in.
        let measured = pitch(&left[4_800..]);
        let expected = frequency(key as u8);
        assert!(
            (measured - expected).abs() < 0.01,
            "key {key}: {measured} Hz, not {expected} Hz"
        );
        assert_eq!(
            block.outputs[0], block.outputs[1],
            "the same in both channels"
        );
        assert_eq!(block.status, ProcessStatus::Continue);
        let (_, block) = render(&mut host, &[note_off(0, key)], 20);
        assert_eq!(block.status, ProcessStatus::Sleep);
    }
}

#[test]
fn a_note_starts_on_its_frame_and_ends_after_its_release() {
    let mut host = built();
    let (left, _) = render(&mut host, &[note_on(100, 69)], 1);
    assert!(
        left[..=100].iter().all(|s| *s == 0.0),
        "silent until the note's frame"
    );
    assert!(left[101..].iter().any(|s| *s != 0.0));

    let (_, block) = render(&mut host, &[note_off(10, 69)], 1);
    assert_eq!(block.status, ProcessStatus::Continue, "still releasing");
    // A 0.1 s release ends within ten blocks; the host hears the voice end.
    let mut ended = Vec::new();
    let mut last = block;
    for _ in 0..10 {
        last = host.process(FRAMES, None, &[]);
        ended.extend(last.events.iter().copied());
    }
    assert!(
        matches!(ended[..], [PluginEvent::NoteEnd { key: Some(69), .. }]),
        "{ended:?}"
    );
    assert_eq!(last.status, ProcessStatus::Sleep);
    assert!(last.outputs.iter().flatten().all(|s| *s == 0.0));
}

#[test]
fn with_no_notes_it_sleeps_in_silence() {
    let mut host = built();
    let (left, block) = render(&mut host, &[], 1);
    assert_eq!(block.status, ProcessStatus::Sleep);
    assert!(left.iter().all(|s| *s == 0.0));
}

#[test]
fn level_scales_it_down_to_silence() {
    let mut host = built();
    let (loud, _) = render(&mut host, &[note_on(0, 69)], 8);
    let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let full = peak(&loud[3 * FRAMES..]);
    assert!(full > 0.0);
    let silent = HostEvent::Param {
        time: 0,
        id: LEVEL.0,
        value: 0.0,
    };
    let (quiet, _) = render(&mut host, &[silent], 4);
    assert!(quiet[3 * FRAMES..].iter().all(|s| *s == 0.0));
}

#[test]
fn a_ninth_note_takes_the_oldest_voice() {
    let mut host = built();
    let notes: Vec<HostEvent> = (0..=VOICES as u16)
        .map(|i| note_on(u32::from(i), 60 + i))
        .collect();
    let block = host.process(FRAMES, None, &notes);
    assert_eq!(
        block.events,
        [PluginEvent::NoteEnd {
            time: VOICES as u32,
            key: Some(60)
        }]
    );
}

#[test]
fn processing_stays_off_the_heap() {
    assert!(lspk_rt::trap::is_installed());
    // From the entry point in this binary, so the trap sees the plug-in's
    // allocations as well as the host's.
    let mut host =
        ClapTestHost::from_entry(&lspk_example_sine::clap_entry, ID).expect("the plug-in");
    host.activate(RATE, FRAMES as u32).expect("activation");
    for i in 0..64u16 {
        let key = 48 + i % 24;
        let events = [
            note_on(u32::from(i), key),
            HostEvent::Param {
                time: 200,
                id: LEVEL.0,
                value: f64::from(i % 4) / 4.0,
            },
            note_off(400, 48 + (i + 12) % 24),
            HostEvent::NoteChoke {
                time: 450,
                key: Some(key.saturating_sub(5)),
            },
        ];
        host.process(FRAMES, None, &events);
    }
}
