//! {{Name}} as a standalone app, on a fake audio device: a MIDI note plays
//! at its pitch, and the audio loop stays off the heap.

use lspk_standalone::fake::FakeBackend;
use lspk_standalone::settings::Settings;
use lspk_standalone::{Standalone, Status};
use {{crate_lib}}::{ {{Type}}, VOICE_PEAK};

lspk_rt::install_trap!();

#[test]
fn a_note_from_midi_plays_through_the_device() {
    let fake = FakeBackend::new(vec![FakeBackend::device("speakers", true)]);
    let mut app = Standalone::<{{Type}}>::new(Box::new(fake.clone()), Settings::default(), None)
        .expect("the app");
    assert!(Standalone::<{{Type}}>::takes_notes() && !Standalone::<{{Type}}>::has_input());
    assert!(app.has_editor());
    app.start();
    assert!(matches!(app.status(), Status::Running(info) if info.sample_rate == 48_000));

    let silence = fake.render(512).expect("a buffer");
    assert!(silence.iter().all(|s| *s == 0.0));

    // A4 for a tenth of a second: 44 cycles, at most a voice's peak.
    app.midi().send([0x90, 69, 127]);
    let input = vec![0.0f32; 4800 * 2];
    let mut output = vec![0.0f32; 4800 * 2];
    assert!(lspk_rt::rt_test!({ fake.render_into(&input, &mut output) }));
    let left: Vec<f32> = output.iter().step_by(2).copied().collect();
    let right: Vec<f32> = output.iter().skip(1).step_by(2).copied().collect();
    assert_eq!(left, right);
    let peak = left.iter().fold(0.0f32, |p, s| p.max(s.abs()));
    assert!(peak > 0.01 && peak <= VOICE_PEAK, "peak {peak}");
    let rising = left
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    assert!((43..=45).contains(&rising), "{rising} cycles");
}
