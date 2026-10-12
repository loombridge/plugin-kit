//! {{Name}} as a standalone app, on a fake audio device: the input device
//! passes through, and the audio loop stays off the heap.

use lspk_standalone::fake::FakeBackend;
use lspk_standalone::settings::Settings;
use lspk_standalone::{Standalone, Status};
use {{crate_lib}}::{ {{Type}}, GAIN};

lspk_rt::install_trap!();

#[test]
fn the_input_device_passes_through_at_the_gain() {
    let fake = FakeBackend::new(vec![FakeBackend::device("interface", true)]);
    let mut app = Standalone::<{{Type}}>::new(Box::new(fake.clone()), Settings::default(), None)
        .expect("the app");
    assert!(Standalone::<{{Type}}>::has_input() && !Standalone::<{{Type}}>::takes_notes());
    assert!(app.has_editor());
    app.start();
    assert!(matches!(app.status(), Status::Running(info) if info.input_channels == 2));

    let input: Vec<f32> = (0..1024).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let mut output = vec![0.0f32; input.len()];
    assert!(lspk_rt::rt_test!({ fake.render_into(&input, &mut output) }));
    assert_eq!(output, input, "at its defaults the input passes unchanged");

    // Half, once the 20 ms glide has run.
    app.host().edit(GAIN, 0.5).expect("an edit");
    for _ in 0..4 {
        assert!(fake.render_into(&input, &mut output));
    }
    for (out, inp) in output.iter().zip(&input) {
        assert!((out - inp * 0.5).abs() < 1e-6);
    }
}
