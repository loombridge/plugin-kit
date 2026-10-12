# plugin-kit

The plug-in kit is a set of Rust crates for building audio plug-ins: real-time safety, parameters, commands and the agent interface, DSP building blocks, voices, modulation, effects, wavetables, sequencing, presets, UI widgets, the plug-in formats and a headless test harness. A plug-in is written once against one `Plugin` trait and ships as CLAP natively, as VST3 and Audio Unit made from the CLAP build, as a standalone app and as a native [Beatlayer](https://github.com/loombridge/beatlayer) device. Every action a person can take in a plug-in built on the kit, an agent can take too.

[Reagent](https://github.com/loombridge/reagent), a wavetable synthesiser, is the first plug-in built on it.

The plug-in kit is in early development: `lspk-rt`, `lspk-params`, `lspk-commands`, `lspk-agent`, `lspk-dsp` (oversampling, envelopes, LFOs, filters and the SIMD helpers so far), `lspk-wavetable` (the wavetable model, WAV files, mipmaps and the wavetable oscillator), `lspk-plugin` (with its CLAP, VST3 and Audio Unit v2 exports and editor hosting), `lspk-plugin-agent` (the agent interface inside a running plug-in), `lspk-ui` (editors embedded in the host's window, and a generic interface for any plug-in's parameters) and `lspk-test` (the headless test harness, with CLAP, VST3 and Audio Unit hosts, and the performance harness) hold working code, every other crate is still a stub, and there's no stable API yet. Until there is, plug-ins depend on it by git revision; nothing is published to crates.io.

## Crates

Every crate carries the prefix `lspk-`.

| Crate | What it will hold |
|---|---|
| `lspk-rt` | The real-time harness: thread priority and audio workgroups, denormal flushing, the allocation trap, a lock-free logger, the RealtimeSanitizer harness |
| `lspk-params` | Parameter descriptors, smoothing, the parameter space of modular plug-ins (stable hierarchical IDs, fixed modules, slots, paths, remote-controls pages), sample-accurate parameter events, modulation offsets, gestures |
| `lspk-commands` | The command registry: every user-facing action with a schema, a model-facing description and a permission class |
| `lspk-agent` | The agent interface: MCP over stdio and over Streamable HTTP on loopback, tokens, permission prompts, the discovery file for running instances |
| `lspk-dsp` | DSP building blocks: oscillators, interpolators, filters, envelopes, LFOs, oversampling, FFT, resampling, SIMD helpers, waveshapers |
| `lspk-voice` | Voice allocation and stealing, mono, legato and glide, unison, MPE and per-note expression |
| `lspk-mod` | The modulation engine: a 64-slot matrix of sources, destinations, curves and auxiliary sources, per-voice and global, sample-accurate, at audio rate only where a destination needs it; its edits as commands |
| `lspk-fx` | The effects library, each effect usable inside a plug-in and as a plug-in of its own |
| `lspk-wavetable` | The wavetable model, WAV import and export, mipmaps, the wavetable oscillator, spectral operations, resynthesis |
| `lspk-seq` | Arpeggiator and step and clip sequencing, synced to the host's tempo |
| `lspk-presets` | The preset format (documented, versioned JSON), the preset library, A/B compare, undo history |
| `lspk-ui` | The egui widget set, theming, zoom, AccessKit, host-window embedding through baseview |
| `lspk-plugin` | The format layer: the `Plugin` trait and its CLAP, VST3, Audio Unit, standalone and Beatlayer exports, and `com.loombridge.agent-commands`, the CLAP extension through which a host lists a plug-in's commands and serves them through its own agent interface |
| `lspk-plugin-agent` | The agent interface inside a running plug-in: its parameters and commands as tools, its own MCP server on loopback, discovery, checkpoints and previews it hears offline |
| `lspk-clap-wrapper` | free-audio's [clap-wrapper](https://github.com/free-audio/clap-wrapper) and the SDK sources it needs (VST3 SDK, AudioUnitSDK), vendored and built with `cc`, which make the VST3 and Audio Unit v2 exports from the CLAP one |
| `lspk-standalone` | The standalone app: any plug-in run as an application, with audio and MIDI device choice, the computer keyboard, settings kept between runs, and state files |
| `lspk-test` | The headless test harness: renders, golden renders and null tests, measurements, format validators, a scripted MCP client, benchmarks |
| `lspk-xtask` | `cargo xtask`: the repository tasks (check, bundle, validate, golden, bench) for the kit and for every plug-in built on it, and `new-plugin`, which generates a plug-in repository from the templates in `xtask/templates` |

Three example plug-ins under `examples/`, never published, show the `Plugin` trait and prove the format exports: `lspk-example-gain`, a stereo gain, `lspk-example-sine`, an eight-voice sine instrument, and `lspk-example-modular`, a small synthesiser of fixed modules and a two-slot effects rack, which proves the parameter space in every format. Gain and Sine also build as standalone apps (`src/main.rs`, one line), which `cargo xtask bundle` lays out beside their plug-in formats.

## Develop

1. Install Rust with [rustup](https://rustup.rs). The repository pins the toolchain, so rustup fetches the right version on first build.
1. Install cargo-deny, which checks dependency licences and advisories, and cargo-nextest, which runs the tests in parallel:

   ```sh
   cargo install --locked cargo-deny
   cargo install --locked cargo-nextest@0.9.146
   ```

1. Run every check: format, clippy, tests, crate layering, and the dependency policy.

   ```sh
   cargo xtask check
   ```

1. To try a plug-in in a host, build it in every format in `target/bundle`: a `.clap`, a `.vst3` and, on macOS, a `.component` (Audio Unit v2), each signed ad hoc on macOS so hosts load it, and the standalone app (`<crate>.app` on macOS, `<crate>.exe` on Windows, `<crate>` on Linux) for a crate with a `<crate>-standalone` binary.

   ```sh
   cargo xtask bundle lspk-example-gain lspk-example-sine
   clap-validator validate target/bundle/*.clap
   ```

1. To run every format's validators on them: clap-validator; Steinberg's VST3 validator, which xtask builds from the vendored VST3 SDK; on macOS `auval`, with each `.component` installed in `~/Library/Audio/Plug-Ins/Components` while it runs; and [pluginval](https://github.com/Tracktion/pluginval) at strictness 5 when it's on the `PATH` (a release you install yourself, as CI does on macOS; it isn't part of the repository). With no crates named, `cargo xtask validate` checks the example plug-ins, as CI does.

   ```sh
   cargo xtask validate lspk-example-gain lspk-example-sine
   ```

   A plug-in's Audio Unit type, subtype and manufacturer codes come from its info, as `lspk_plugin::bundle::AuCodes` describes; `cargo xtask bundle` prints them.

1. Golden renders are null-tested by `cargo xtask check`. When a change to a plug-in's sound is intended, remake its references and say why in the pull request:

   ```sh
   cargo xtask golden --update
   ```

1. Every crate's benchmarks live in its `benches/perf.rs`: plug-in scenarios (CPU per block and per voice, idle cost, instance creation, state loading) and micro-benchmarks of hot paths. Time them all on your machine, with the results written as JSON under `target/bench/`:

   ```sh
   cargo xtask bench
   ```

   CI counts each benchmark's instructions on Linux and fails a pull request that makes any of them more than 3 % slower than `bench/baseline.json`; [AGENTS.md](AGENTS.md) explains how to update the baseline when a slowdown is intended.

1. To try a plug-in's editor without a DAW, open it in the kit's small CLAP host, which embeds it as a DAW's device window does (with no argument, it hosts a built-in test plug-in with every kind of control; `--check` opens, resizes and closes it many times and reports the result):

   ```sh
   cargo run -p lspk-ui --example editor_host -- target/bundle/lspk-example-gain.clap
   ```

1. To run a plug-in with no DAW at all, open its standalone app. It plays through the default audio devices (choose others, the sample rate and the buffer size in its window), takes every MIDI input and, for an instrument, the computer keyboard, and keeps its settings and the plug-in's state between runs. Without a window, `--run-for <seconds>` plays and reports what it played (`--note <key>` holds a note), and `--list-devices` lists the devices:

   ```sh
   open target/bundle/lspk-example-sine.app
   cargo run --release -p lspk-example-sine --bin lspk-example-sine-standalone -- --run-for 2 --note 69
   ```

## Start a new plug-in

`cargo xtask new-plugin` generates a plug-in repository of its own, outside the kit, that passes every check from its first commit:

```sh
cargo xtask new-plugin bright-delay --kind effect
cargo xtask new-plugin glass-pad --kind instrument --licence gpl-3.0-or-later --id com.example.glass-pad
```

The repository goes beside the kit's checkout (`--dir` puts it elsewhere). It holds a workspace pinned to the kit's toolchain, its licence files (MIT OR Apache-2.0 by default, or GPL-3.0-or-later), and one plug-in crate: an eight-voice instrument or a stereo effect with a few parameters in a parameter space, exported as CLAP, VST3, Audio Unit v2 and a standalone app, with the generic editor and the agent interface. Its tests cover the formats, the agent interface, golden renders, measurements and the parameter ID list; its benchmark suite starts with a baseline, and its CI runs the check, the validators and the benchmark gate on all three platforms. Its own `cargo xtask` (check, bundle, validate, golden, bench) is a few lines that run this repository's tasks, from the `lspk-xtask` crate. README, CONTRIBUTING (with the Developer Certificate of Origin), AGENTS.md and CLAUDE.md come with it.

Every kit crate it uses comes from this repository at one git revision: the checkout's commit, which must be on `origin/main` (or pass `--rev`). Its CONTRIBUTING.md explains working on the plug-in and the kit at once through Cargo's `[patch]`. Every generated file is staged, so `git commit` makes its first commit. Nothing is created on GitHub.

`cargo xtask template-check` generates an instrument and an effect into a temporary folder, built against your checkout, and runs their checks, as CI does on every pull request.

Read [CONTRIBUTING.md](CONTRIBUTING.md) before you open a pull request.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option, so that any plug-in can build on the kit, whatever its own licence.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions.
