//! Example Sine's benchmark scenarios, at the rate and block size the
//! kit's reference patches use: 48 kHz in 128-frame blocks.

use std::process::ExitCode;

use lspk_example_sine::{LEVEL, Sine, VOICES};
use lspk_test::bench::{Scenario, Suite};
use lspk_test::{Script, Settings};

lspk_rt::install_trap!();

const RATE: u32 = 48_000;

fn main() -> ExitCode {
    let settings = Settings::new(f64::from(RATE), 128);
    let mut suite = Suite::new("lspk-example-sine");

    // Every voice held for a second while the level moves, then released.
    let chord = (0..VOICES as u8)
        .fold(Script::new(RATE as usize * 6 / 5), |script, i| {
            script.note(0, RATE, 48 + 5 * i, 0.8)
        })
        .param(RATE / 4, LEVEL, 0.25)
        .param(RATE / 2, LEVEL, 0.5);
    suite.scenario(Scenario::new::<Sine>("chord", chord, settings).voices(VOICES));

    // One voice, for the cost of a voice alone.
    let single = Script::new(RATE as usize * 6 / 5).note(0, RATE, 69, 0.8);
    suite.scenario(Scenario::new::<Sine>("one-voice", single, settings).voices(1));

    suite.main()
}
