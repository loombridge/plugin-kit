//! {{Name}}'s golden renders, null-tested against the references in
//! `tests/golden/`: bit-exact on the platform that made them, below −96 dBFS
//! elsewhere. Remake them with `cargo xtask golden --update`, never from
//! here, and say why in the pull request.
//!
//! The same script renders to the same bits at every block size, and from
//! the built `.clap` through the CLAP test host.

use lspk_params::db_to_gain;
use lspk_test::render::{self, BLOCK_SIZES, Script, Settings, block_invariance};
use lspk_test::{golden, signal};
use {{crate_lib}}::{ {{Type}}, DRIVE, GAIN, ID, NAME, WIDTH, params};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;

/// A second of a sweep on the left and a sine on the right, with every
/// parameter moved: the drive up, the width narrowed then widened, the gain
/// down 6 dB and back.
fn programme() -> Script {
    let frames = 48_000;
    let left = signal::sweep(20.0, 20_000.0, RATE, frames, 0.5);
    let right = signal::sine(1_000.0, RATE, frames, 0.25);
    Script::new(frames)
        .with_input(vec![left, right])
        .param(6_000, DRIVE, 60.0)
        .param(12_000, WIDTH, 0.0)
        .param(20_000, WIDTH, 200.0)
        .param(28_000, GAIN, db_to_gain(-6.0))
        .param(36_000, DRIVE, 0.0)
        .param(40_000, GAIN, 1.0)
}

#[test]
fn golden_renders_null_against_their_references() {
    let render =
        render::render::<{{Type}}>(&programme(), Settings::new(RATE, 512)).expect("a render");
    let mut suite = golden::Suite::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"));
    suite.add("programme", render.output);
    suite.assert();
}

#[test]
fn the_golden_script_renders_the_same_at_every_block_size() {
    let invariance = block_invariance(&BLOCK_SIZES, |block| {
        render::render::<{{Type}}>(&programme(), Settings::new(RATE, block))
            .expect("a render")
            .output
    });
    assert!(invariance.is_exact(), "{invariance:?}");
}

#[test]
fn the_built_clap_renders_the_golden_script_to_the_same_bits() {
    let clap = lspk_test::clap::bundle_built_library("{{crate_lib}}", NAME, ID);
    let mut host = lspk_test::ClapTestHost::load(&clap, ID).expect("the built plug-in");
    host.activate(RATE, 512).expect("activation");
    let built = host.render(&programme(), 512, &params());
    let in_process =
        render::render::<{{Type}}>(&programme(), Settings::new(RATE, 512)).expect("a render");
    assert!(
        built.output.same_bits(&in_process.output),
        "the .clap differs by up to {} dBFS",
        lspk_test::measure::db(built.output.max_difference(&in_process.output))
    );
}
