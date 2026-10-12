//! Example Gain's benchmark scenarios: a second of stereo programme at
//! 48 kHz, with automation in 128-frame blocks and without it in 32-frame
//! ones.

use std::process::ExitCode;

use lspk_example_gain::{GAIN, Gain};
use lspk_test::bench::{Scenario, Suite};
use lspk_test::{Script, Settings, signal};

lspk_rt::install_trap!();

const RATE: u32 = 48_000;

fn main() -> ExitCode {
    let frames = RATE as usize;
    let tone = signal::sine(220.0, f64::from(RATE), frames, 0.5);
    let left = signal::mix(&[&tone, &signal::noise(1, frames, 0.1)]);
    let right = signal::mix(&[&tone, &signal::noise(2, frames, 0.1)]);
    let programme = Script::new(frames).with_input(vec![left, right]);
    let mut suite = Suite::new("lspk-example-gain");

    // The gain moving every 50 ms, so it's always gliding.
    let automated = (1..20).fold(programme.clone(), |script, i| {
        script.param(i * RATE / 20, GAIN, if i % 2 == 0 { 1.0 } else { 0.5 })
    });
    suite.scenario(Scenario::new::<Gain>(
        "automation",
        automated,
        Settings::new(f64::from(RATE), 128),
    ));

    // A steady gain in small blocks, where per-block overhead shows.
    suite.scenario(Scenario::new::<Gain>(
        "small-blocks",
        programme,
        Settings::new(f64::from(RATE), 32),
    ));

    suite.main()
}
