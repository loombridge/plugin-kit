//! Example Sine as VST3 and (on macOS) Audio Unit hosts load it, from the
//! entry points this crate exports beside its CLAP entry. Each test needs a
//! process of its own, as cargo-nextest gives it (see lspk-plugin's
//! tests/formats.rs).

const FRAMES: usize = 256;

#[test]
fn the_vst3_export_plays_a_note() {
    #[cfg(windows)]
    // SAFETY: the crate's own module entry, as a host calls it.
    unsafe {
        lspk_example_sine::InitDll();
    }
    // SAFETY: the crate's own VST3 factory, which lives as long as the
    // process.
    let mut host = unsafe {
        lspk_test::vst3::Vst3TestHost::from_factory(lspk_example_sine::GetPluginFactory())
    }
    .expect("an instance");
    host.activate(48_000.0, FRAMES as u32).expect("activation");
    let silent = host.process(FRAMES, None, &[], &[]);
    assert!(silent.outputs.iter().flatten().all(|s| *s == 0.0));
    let block = host.process(FRAMES, None, &[(0, 69, 1.0)], &[]);
    assert_eq!(block.outputs.len(), 2);
    assert!(block.outputs[0].iter().any(|s| s.abs() > 0.01));
}

#[cfg(target_os = "macos")]
#[test]
fn the_audio_unit_export_plays_a_note() {
    let mut host = lspk_test::auv2::AuTestHost::new(
        lspk_example_sine::GetPluginFactoryAUV2,
        ["aumu", "Tsin", "Lspk"],
    )
    .expect("an instance");
    host.initialize(FRAMES as u32).expect("initialisation");
    host.midi(0x90, 69, 127);
    let block = host.render(FRAMES as u32).expect("a block");
    assert!(block[0].iter().any(|s| s.abs() > 0.01));
}
