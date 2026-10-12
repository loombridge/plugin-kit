# plugin-kit

The plug-in kit is a set of Rust crates for building audio plug-ins: real-time safety, parameters, commands and the agent interface, DSP building blocks, voices, modulation, effects, wavetables, sequencing, presets, UI widgets, the plug-in formats and a headless test harness. A plug-in is written once against one `Plugin` trait and ships as CLAP natively, as VST3 and Audio Unit made from the CLAP build, as a standalone app and as a native [Beatlayer](https://github.com/loombridge/beatlayer) device. Every action a person can take in a plug-in built on the kit, an agent can take too.

[Reagent](https://github.com/loombridge/reagent), a wavetable synthesiser, is the first plug-in built on it.

The plug-in kit is in early development: every crate is a stub, and there's no stable API yet. Until there is, plug-ins depend on it by git revision; nothing is published to crates.io.

## Crates

Every crate carries the prefix `lspk-`.

| Crate | What it will hold |
|---|---|
| `lspk-rt` | The real-time harness: thread priority and audio workgroups, denormal flushing, the allocation trap, a lock-free logger, the RealtimeSanitizer harness |
| `lspk-params` | Parameter descriptors, smoothing, sample-accurate parameter events, modulation offsets, gestures |
| `lspk-commands` | The command registry: every user-facing action with a schema, a model-facing description and a permission class |
| `lspk-agent` | The agent interface: MCP over stdio and over Streamable HTTP on loopback, tokens, permission prompts, checkpoints, perception tools |
| `lspk-dsp` | DSP building blocks: oscillators, interpolators, filters, envelopes, LFOs, oversampling, FFT, resampling, SIMD helpers, waveshapers |
| `lspk-voice` | Voice allocation and stealing, mono, legato and glide, unison, MPE and per-note expression |
| `lspk-mod` | The modulation engine: sources, destinations, depth, curves, per-voice and global routes |
| `lspk-fx` | The effects library, each effect usable inside a plug-in and as a plug-in of its own |
| `lspk-wavetable` | The wavetable model, WAV import and export, mipmaps, spectral operations, resynthesis |
| `lspk-seq` | Arpeggiator and step and clip sequencing, synced to the host's tempo |
| `lspk-presets` | The preset format (documented, versioned JSON), the preset library, A/B compare, undo history |
| `lspk-ui` | The egui widget set, theming, zoom, AccessKit, host-window embedding through baseview |
| `lspk-plugin` | The format layer: the `Plugin` trait and its CLAP, VST3, Audio Unit, standalone and Beatlayer exports |
| `lspk-test` | The headless test harness: renders, golden renders and null tests, measurements, format validators, a scripted MCP client |

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

Read [CONTRIBUTING.md](CONTRIBUTING.md) before you open a pull request.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT licence](LICENSE-MIT), at your option, so that any plug-in can build on the kit, whatever its own licence.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 licence, shall be dual licensed as above, without any additional terms or conditions.
