# plugin-kit

The plug-in kit is a set of Rust crates for building audio plug-ins: real-time safety, parameters, commands and the agent interface, DSP building blocks, voices, modulation, effects, wavetables, sequencing, presets, UI widgets, the plug-in formats and a headless test harness. A plug-in is written once against one `Plugin` trait and ships as CLAP natively, as VST3 and Audio Unit made from the CLAP build, as a standalone app and as a native [Beatlayer](https://github.com/loombridge/beatlayer) device. Every action a person can take in a plug-in built on the kit, an agent can take too.

[Reagent](https://github.com/loombridge/reagent), a wavetable synthesiser, is the first plug-in built on it.

The plug-in kit is in early development: `lspk-rt`, `lspk-params`, `lspk-commands`, `lspk-agent`, `lspk-plugin` (with its CLAP, VST3 and Audio Unit v2 exports and editor hosting), `lspk-plugin-agent` (the agent interface inside a running plug-in), `lspk-ui` (editors embedded in the host's window, and a generic interface for any plug-in's parameters) and `lspk-test` (the headless test harness, with CLAP, VST3 and Audio Unit hosts) hold working code, every other crate is still a stub, and there's no stable API yet. Until there is, plug-ins depend on it by git revision; nothing is published to crates.io.

## Crates

Every crate carries the prefix `lspk-`.

| Crate | What it will hold |
|---|---|
| `lspk-rt` | The real-time harness: thread priority and audio workgroups, denormal flushing, the allocation trap, a lock-free logger, the RealtimeSanitizer harness |
| `lspk-params` | Parameter descriptors, smoothing, sample-accurate parameter events, modulation offsets, gestures |
| `lspk-commands` | The command registry: every user-facing action with a schema, a model-facing description and a permission class |
| `lspk-agent` | The agent interface: MCP over stdio and over Streamable HTTP on loopback, tokens, permission prompts, the discovery file for running instances |
| `lspk-dsp` | DSP building blocks: oscillators, interpolators, filters, envelopes, LFOs, oversampling, FFT, resampling, SIMD helpers, waveshapers |
| `lspk-voice` | Voice allocation and stealing, mono, legato and glide, unison, MPE and per-note expression |
| `lspk-mod` | The modulation engine: sources, destinations, depth, curves, per-voice and global routes |
| `lspk-fx` | The effects library, each effect usable inside a plug-in and as a plug-in of its own |
| `lspk-wavetable` | The wavetable model, WAV import and export, mipmaps, spectral operations, resynthesis |
| `lspk-seq` | Arpeggiator and step and clip sequencing, synced to the host's tempo |
| `lspk-presets` | The preset format (documented, versioned JSON), the preset library, A/B compare, undo history |
| `lspk-ui` | The egui widget set, theming, zoom, AccessKit, host-window embedding through baseview |
| `lspk-plugin` | The format layer: the `Plugin` trait and its CLAP, VST3, Audio Unit, standalone and Beatlayer exports, and `com.loombridge.agent-commands`, the CLAP extension through which a host lists a plug-in's commands and serves them through its own agent interface |
| `lspk-plugin-agent` | The agent interface inside a running plug-in: its parameters and commands as tools, its own MCP server on loopback, discovery, checkpoints and previews it hears offline |
| `lspk-clap-wrapper` | free-audio's [clap-wrapper](https://github.com/free-audio/clap-wrapper) and the SDK sources it needs (VST3 SDK, AudioUnitSDK), vendored and built with `cc`, which make the VST3 and Audio Unit v2 exports from the CLAP one |
| `lspk-test` | The headless test harness: renders, golden renders and null tests, measurements, format validators, a scripted MCP client |

Two example plug-ins under `examples/`, never published, show the `Plugin` trait at its smallest and prove the format exports: `lspk-example-gain`, a stereo gain, and `lspk-example-sine`, an eight-voice sine instrument.

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

1. To try a plug-in in a host, build it in every format in `target/bundle`: a `.clap`, a `.vst3` and, on macOS, a `.component` (Audio Unit v2), each signed ad hoc on macOS so hosts load it.

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

1. To try a plug-in's editor without a DAW, open it in the kit's small CLAP host, which embeds it as a DAW's device window does (with no argument, it hosts a built-in test plug-in with every kind of control; `--check` opens, resizes and closes it many times and reports the result):

   ```sh
   cargo run -p lspk-ui --example editor_host -- target/bundle/lspk-example-gain.clap
   ```

Read [CONTRIBUTING.md](CONTRIBUTING.md) before you open a pull request.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option, so that any plug-in can build on the kit, whatever its own licence.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions.
