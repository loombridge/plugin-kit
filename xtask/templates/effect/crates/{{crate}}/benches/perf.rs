//! {{Name}}'s benchmark scenarios: a second of stereo programme at 48 kHz,
//! with every parameter moving in 128-frame blocks, and clean in 32-frame
//! ones. The suite is named `effect`, so the IDs in `bench/baseline.json`
//! don't depend on the plug-in's name.

use std::process::ExitCode;

use lspk_test::bench::{Scenario, Suite};
use lspk_test::{Script, Settings, signal};
use {{crate_lib}}::{ {{Type}}, DRIVE, GAIN, WIDTH};

lspk_rt::install_trap!();

const RATE: u32 = 48_000;

fn main() -> ExitCode {
    let frames = RATE as usize;
    let tone = signal::sine(220.0, f64::from(RATE), frames, 0.5);
    let left = signal::mix(&[&tone, &signal::noise(1, frames, 0.1)]);
    let right = signal::mix(&[&tone, &signal::noise(2, frames, 0.1)]);
    let programme = Script::new(frames).with_input(vec![left, right]);
    let mut suite = Suite::new("effect");

    // Every parameter moving every 50 ms, so they're always gliding.
    let automated = (1..20).fold(programme.clone(), |script, i| {
        let on = i % 2 == 0;
        script
            .param(i * RATE / 20, GAIN, if on { 1.0 } else { 0.5 })
            .param(i * RATE / 20, DRIVE, if on { 80.0 } else { 20.0 })
            .param(i * RATE / 20, WIDTH, if on { 150.0 } else { 50.0 })
    });
    suite.scenario(Scenario::new::<{{Type}}>(
        "automation",
        automated,
        Settings::new(f64::from(RATE), 128),
    ));

    // Clean, in small blocks, where per-block overhead shows.
    suite.scenario(Scenario::new::<{{Type}}>(
        "small-blocks",
        programme,
        Settings::new(f64::from(RATE), 32),
    ));

    suite.main()
}
