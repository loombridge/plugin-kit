//! {{Name}} as VST3 and (on macOS) Audio Unit hosts load it, from the entry
//! points this crate exports beside its CLAP entry. Each test needs a process
//! of its own, as cargo-nextest gives it.

use {{crate_lib}}::GAIN;

const FRAMES: usize = 256;

#[test]
fn the_vst3_export_passes_audio_and_keeps_its_state() {
    #[cfg(windows)]
    // SAFETY: the crate's own module entry, as a host calls it.
    unsafe {
        {{crate_lib}}::InitDll();
    }
    // SAFETY: the crate's own VST3 factory, which lives as long as the
    // process.
    let factory = unsafe { {{crate_lib}}::GetPluginFactory() };
    // SAFETY: as above.
    let mut host =
        unsafe { lspk_test::vst3::Vst3TestHost::from_factory(factory) }.expect("an instance");
    let params = host.params();
    assert_eq!(params.len(), 3);
    assert_eq!((params[0].id, params[0].title.as_str()), (GAIN.0, "Gain"));

    host.activate(48_000.0, FRAMES as u32).expect("activation");
    let input: Vec<f32> = (0..FRAMES).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    // At its defaults, the input passes unchanged.
    let block = host.process(FRAMES, Some(&input), &[], &[]);
    assert_eq!(block.outputs, [input.clone(), input.clone()]);

    // The bottom of the gain's travel is silence, once the glide has run.
    let mut block = host.process(FRAMES, Some(&input), &[], &[(GAIN.0, 0, 0.0)]);
    for _ in 0..4 {
        block = host.process(FRAMES, Some(&input), &[], &[]);
    }
    assert!(block.outputs.iter().flatten().all(|s| *s == 0.0));

    let state = host.save_state().expect("a state");
    drop(host);
    // SAFETY: as above.
    let mut other =
        unsafe { lspk_test::vst3::Vst3TestHost::from_factory(factory) }.expect("an instance");
    assert_ne!(other.value(GAIN.0), 0.0);
    other.load_state(&state).expect("the state loads");
    assert_eq!(other.value(GAIN.0), 0.0);
}

#[cfg(target_os = "macos")]
#[test]
fn the_audio_unit_export_keeps_its_state() {
    // Codes of the test's own, so an installed copy of the plug-in never
    // answers instead.
    const CODES: [&str; 3] = ["aufx", "Ttst", "Test"];
    let mut host = lspk_test::auv2::AuTestHost::new({{crate_lib}}::GetPluginFactoryAUV2, CODES)
        .expect("an instance");
    assert_eq!(host.param_ids().len(), 3);
    host.initialize(FRAMES as u32).expect("initialisation");
    host.set_value(GAIN.0, 0.0).expect("a value");
    // The plug-in takes the host's value in its next block.
    host.render(FRAMES as u32).expect("a block");
    let state = host.save_state().expect("a state");
    drop(host);
    let mut other = lspk_test::auv2::AuTestHost::new({{crate_lib}}::GetPluginFactoryAUV2, CODES)
        .expect("an instance");
    other.initialize(FRAMES as u32).expect("initialisation");
    assert_ne!(other.value(GAIN.0), Some(0.0));
    other.load_state(&state).expect("the state loads");
    assert_eq!(other.value(GAIN.0), Some(0.0));
}
