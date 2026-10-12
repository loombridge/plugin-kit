# AGENTS.md

Shared context for any agent or person working in this repository. Keep it current and short; private notes belong in your own memory, not here.

## What this is

The plug-in kit is a set of Rust crates (prefix `lspk-`) that Loombridge's audio plug-ins are built from: real-time safety, parameters, commands and the agent interface, DSP, voices, modulation, effects, wavetables, sequencing, presets, UI widgets, the plug-in formats (CLAP natively, with VST3, Audio Unit, a standalone app and a native Beatlayer device made from it) and a headless test harness. Reagent, a wavetable synthesiser, is the first plug-in built on it. Anything a second plug-in would need belongs here, not in a plug-in's own repository.

## Read first

1. [README.md](README.md): what the kit holds and how to build it.
2. [CONTRIBUTING.md](CONTRIBUTING.md): how a change gets into `main`, and the certificate outside contributors give.

The plan isn't published yet; a maintainer gives you the work package and the context it needs.

## Lines not crossed

- The kit is MIT OR Apache-2.0, and every crate says `license = "MIT OR Apache-2.0"`. No copyleft dependency (`cargo deny` rejects GPL, LGPL and AGPL), and no code ported or derived from GPL projects such as Surge XT or Vital: that stays in a plug-in's own GPL crates.
- Every user-facing action is a registered command with a schema, a description and a permission class, so agents reach every feature through the agent interface.
- No allocation, lock or blocking call on an audio thread.
- No kit crate depends on a plug-in or on Beatlayer. The DSP, voice, modulation, effects, wavetable, sequencing, presets, parameters, real-time and commands crates never depend on `lspk-ui` or reach a UI toolkit. `cargo xtask layers` enforces this.
- Every feature has a headless test path.
- No plug-in binaries, SDK binaries or third-party audio in the repository. Third-party C and C++ sources the build needs are vendored, unchanged where possible, in a `vendor` folder, each component listed with its licence in `xtask/src/vendored.rs`; `cargo xtask vendored` checks them, as `cargo deny` checks crates. Nothing is downloaded at build time.
- No `git add -A`.

## How work is done

One work package per branch (`wp/<id>-<slug>`) and per pull request, with acceptance evidence pasted in the PR.

Before a PR, `cargo xtask check` must pass on macOS locally, with its summary pasted in the PR, as must Windows clippy (`CARGO_FEATURE_PURE=1 cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings`). CI runs the check on macOS, Windows and Linux on every pull request and every push to `main`.

Performance is checked on every pull request. Each crate keeps its benchmarks in `benches/perf.rs` (the format is `lspk_test::bench`): a plug-in's scenarios, and a micro-benchmark for each hot path, added in the same pull request as the code. CI counts every benchmark's instructions with Valgrind on Linux x86-64 (`cargo xtask bench --gate`) and fails a pull request that makes any of them more than 3 % slower than `bench/baseline.json`, or adds or removes one without updating it. When a slowdown is intended, or a speed-up should tighten the gate, update the baseline with `cargo xtask bench --update-baseline --reason "..."`, on x86-64 Linux or anywhere with `--from <file>` holding the counts the CI job prints, commit `bench/baseline.json`, and say why in the pull request. `cargo xtask bench` times every benchmark locally and writes JSON (machine, CPU, OS, commit, numbers) under `target/bench/`; nightly timing on fixed reference machines, and a dashboard of each benchmark's history, will collect those files once the machines are set up.

New plug-ins come from `cargo xtask new-plugin` (README.md, "Start a new plug-in"), from the templates in `xtask/templates` (`common`, `instrument`, `effect`; the placeholders and rules are in `xtask/src/new_plugin.rs`). A plug-in's `cargo xtask` is `lspk-xtask` run with its own `lspk_xtask::Repo`, so a task changed here changes for every plug-in when it moves to the new revision; keep tasks free of anything only the kit has. CI's `template (Linux x64)` job runs `cargo xtask template-check` on every pull request: it generates a GPL instrument and a permissive effect, patched to the checkout, and runs their check, validate, golden and benchmark gate. A change that moves the templates' sound, parameters or counts updates them in the same pull request: `cargo xtask template-check --update` remakes the templates' golden references and parameter ID lists (commit them, and say why); for the benchmark baselines, save the counts the template job prints for each plug-in, run `cargo xtask bench --update-baseline --reason "..." --from <file>` in that generated repository, and copy its `bench/baseline.json` to `xtask/templates/<kind>/bench/baseline.json`. The template's CI pins the same actions and tool versions as `.github/workflows/ci.yml`, which a test checks; change both together.

Plug-ins depend on the kit by git revision, so a change that spans the kit and a plug-in lands in the kit first; the plug-in's CONTRIBUTING.md explains working on both at once.

Commits: the project's own commits are authored as `Loombridge <code@loombridge.io>`. Set it once per checkout: `git config user.name Loombridge && git config user.email code@loombridge.io`. Outside contributors commit under their own names with the Developer Certificate of Origin ([CONTRIBUTING.md](CONTRIBUTING.md)).

Merging: a maintainer merges a pull request when `cargo xtask check` and CI pass, review finds no blocker, and the acceptance evidence is complete. Merge with a merge commit, never a squash, so every revision a plug-in pins stays reachable from `main`.

One-time setup: install Rust with rustup (the repository pins the toolchain in `rust-toolchain.toml`), then `cargo install --locked cargo-deny` and `cargo install --locked cargo-nextest@0.9.146`.
