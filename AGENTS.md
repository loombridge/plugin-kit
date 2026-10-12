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
- No plug-in binaries, SDK binaries or third-party audio in the repository.
- No `git add -A`.

## How work is done

One work package per branch (`wp/<id>-<slug>`) and per pull request, with acceptance evidence pasted in the PR.

Before a PR, `cargo xtask check` must pass on macOS locally, with its summary pasted in the PR, as must Windows clippy (`CARGO_FEATURE_PURE=1 cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings`). CI runs the check on macOS, Windows and Linux on every pull request and every push to `main`.

Plug-ins depend on the kit by git revision, so a change that spans the kit and a plug-in lands in the kit first; the plug-in's CONTRIBUTING.md explains working on both at once.

Commits: the project's own commits are authored as `Loombridge <code@loombridge.io>`. Set it once per checkout: `git config user.name Loombridge && git config user.email code@loombridge.io`. Outside contributors commit under their own names with the Developer Certificate of Origin ([CONTRIBUTING.md](CONTRIBUTING.md)).

Merging: a maintainer merges a pull request when `cargo xtask check` and CI pass, review finds no blocker, and the acceptance evidence is complete. Merge with a merge commit, never a squash, so every revision a plug-in pins stays reachable from `main`.

One-time setup: install Rust with rustup (the repository pins the toolchain in `rust-toolchain.toml`), then `cargo install --locked cargo-deny` and `cargo install --locked cargo-nextest@0.9.146`.
