# {{Name}}

{{#instrument}}
{{Name}} is a polyphonic instrument, written in Rust for macOS (Apple Silicon), Windows and Linux. It runs in any host as a CLAP, VST3 or Audio Unit plug-in, as a standalone app, and inside [Beatlayer](https://github.com/loombridge/beatlayer). Every action a person can take in it, an agent can take too.
{{/instrument}}
{{#effect}}
{{Name}} is a stereo effect, written in Rust for macOS (Apple Silicon), Windows and Linux. It runs in any host as a CLAP, VST3 or Audio Unit plug-in, as a standalone app, and inside [Beatlayer](https://github.com/loombridge/beatlayer). Every action a person can take in it, an agent can take too.
{{/effect}}

It's built on the [plug-in kit](https://github.com/loombridge/plugin-kit), Loombridge's shared crates for audio plug-ins, and began as the kit's {{kind}} template (`cargo xtask new-plugin`).

## Crates

| Crate | What it holds |
|---|---|
| `{{crate}}` | The plug-in: its parameters, its sound, its exports (CLAP, VST3, Audio Unit v2, the standalone app), the generic editor and the agent interface, with its tests and benchmarks |
| `xtask` | `cargo xtask`: the plug-in kit's repository tasks, run in this repository |

## Develop

1. Install Rust with [rustup](https://rustup.rs). The repository pins the toolchain, so rustup fetches the right version on first build.
1. Install cargo-deny, which checks dependency licences and advisories, and cargo-nextest, which runs the tests in parallel:

   ```sh
   cargo install --locked cargo-deny
   cargo install --locked cargo-nextest@{{nextest}}
   ```

1. Run every check: format, clippy, tests (golden renders and measurements among them), crate layering, and the dependency policy.

   ```sh
   cargo xtask check
   ```

1. Build it in every format in `target/bundle`: a `.clap`, a `.vst3` and, on macOS, a `.component` (Audio Unit v2), each signed ad hoc on macOS so hosts load it, and the standalone app (`{{crate}}.app` on macOS).

   ```sh
   cargo xtask bundle {{crate}}
   ```

1. Run every format's validators on it: clap-validator, Steinberg's VST3 validator (built from the plug-in kit's vendored SDK), on macOS `auval`, and [pluginval](https://github.com/Tracktion/pluginval) where it's on the `PATH`. Install clap-validator first (`cargo install --locked --git https://github.com/free-audio/clap-validator clap-validator`).

   ```sh
   cargo xtask validate
   ```

1. Golden renders (`crates/{{crate}}/tests/golden/`) are null-tested by `cargo xtask check`. When a change to the sound is intended, remake them and say why in the pull request:

   ```sh
   cargo xtask golden --update
   ```

1. Its benchmarks live in `crates/{{crate}}/benches/perf.rs`. Time them on your machine, with the results written as JSON under `target/bench/`:

   ```sh
   cargo xtask bench
   ```

   CI counts each benchmark's instructions on Linux and fails a pull request that makes any of them more than 3 % slower than `bench/baseline.json`; [AGENTS.md](AGENTS.md) explains how to update the baseline.

1. Run it with no DAW at all: its standalone app plays through the default audio devices (choose others in its Settings dialog) and keeps its settings and state between runs. Without a window, `--run-for <seconds>` plays and reports what it played, and `--list-devices` lists the devices:

   ```sh
   cargo run --release --bin {{crate}}-standalone -- --run-for 2
   ```

{{Name}} depends on the plug-in kit by git revision. To work on both at once, see [CONTRIBUTING.md](CONTRIBUTING.md#working-on-the-plug-in-kit-and-{{crate}}-at-once).

## Licence

{{#permissive}}
Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions.
{{/permissive}}
{{#gpl}}
{{Name}} is licensed under [GPL-3.0-or-later](LICENSE).
{{/gpl}}
