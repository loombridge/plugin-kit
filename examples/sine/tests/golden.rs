//! Example Sine's golden renders, null-tested against the references in
//! `tests/golden/`: bit-exact on the platform that made them, below
//! −96 dBFS elsewhere. Remake them with `cargo xtask golden --update`, never
//! from here.
//!
//! The same script renders to the same bits at every block size, and from
//! the built `.clap` through the CLAP test host.

use lspk_example_sine::{ID, LEVEL, NAME, Sine, params};
use lspk_params::db_to_gain;
use lspk_test::golden;
use lspk_test::render::{self, BLOCK_SIZES, Script, Settings, block_invariance};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;

/// A second and a half: a C major chord built note by note, the level
/// dropped 12 dB, a high A held then choked, and a run of ten quick notes,
/// more than the eight voices, so the oldest give way.
fn phrase() -> Script {
    let mut script = Script::new(72_000)
        .note(0, 12_000, 60, 1.0)
        .note(6_000, 18_000, 64, 0.8)
        .note(12_000, 24_000, 67, 0.6)
        .param(30_000, LEVEL, db_to_gain(-12.0))
        .note_on(36_000, 81, 0.9)
        .choke(46_000, 81);
    for i in 0..10u32 {
        script = script.note(48_000 + i * 600, 9_000, 72 + (i * 5 % 12) as u8, 0.7);
    }
    script
}

#[test]
fn golden_renders_null_against_their_references() {
    let render = render::render::<Sine>(&phrase(), Settings::new(RATE, 512)).expect("a render");
    let mut suite = golden::Suite::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"));
    suite.add("phrase", render.output);
    suite.assert();
}

#[test]
fn the_golden_script_renders_the_same_at_every_block_size() {
    let invariance = block_invariance(&BLOCK_SIZES, |block| {
        render::render::<Sine>(&phrase(), Settings::new(RATE, block))
            .expect("a render")
            .output
    });
    assert!(invariance.is_exact(), "{invariance:?}");
}

#[test]
fn the_built_clap_renders_the_golden_script_to_the_same_bits() {
    let clap = lspk_test::clap::bundle_built_library("lspk_example_sine", NAME, ID);
    let mut host = lspk_test::ClapTestHost::load(&clap, ID).expect("the built plug-in");
    host.activate(RATE, 512).expect("activation");
    let built = host.render(&phrase(), 512, &params());
    let in_process = render::render::<Sine>(&phrase(), Settings::new(RATE, 512)).expect("a render");
    assert!(
        built.output.same_bits(&in_process.output),
        "the .clap differs by up to {} dBFS",
        lspk_test::measure::db(built.output.max_difference(&in_process.output))
    );
}
