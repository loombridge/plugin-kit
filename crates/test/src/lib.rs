//! The headless test harness every kit plug-in tests itself with.
//!
//! | Module | What it holds |
//! |---|---|
//! | [`render`](mod@render) | The render harness: a [`Script`] of notes, parameter changes and audio input, rendered by any kit [`Plugin`](lspk_plugin::Plugin) in this process ([`render()`], [`Instance`]) or by a built `.clap` through the test host ([`ClapTestHost::render`]), at any sample rate and block size, each block under the allocation trap; and the block-size invariance check |
//! | [`signal`] | Test audio made by code: sines, sweeps, noise, impulses. No test uses recorded or third-party audio |
//! | [`golden`] | Golden renders and null tests: references stored as 24-bit FLAC with a manifest, bit-exact on the platform that made them and below −96 dBFS elsewhere, remade only by `cargo xtask golden --update` |
//! | [`measure`] | Measurements as plain numbers: RMS and peak, noise floor, frequency response and phase, THD and THD+N, aliasing, pitch |
//! | [`cpu`] | Wall-clock timing of blocks, for the performance harness |
//! | [`clap`] | A CLAP host for tests, which loads a plug-in from its `.clap` file or its entry point and drives it block by block |
//! | [`vst3`](mod@vst3) | A VST3 host for tests, which makes a plug-in from its VST3 factory, drives it block by block, and records the edits it reports to the host |
//! | `auv2` (macOS) | An Audio Unit host for tests, which registers a plug-in's Audio Unit factory in the test process, renders it, and records the edits it reports to listeners |
//! | [`validate`] | The format validators CI runs on built plug-ins: clap-validator, Steinberg's VST3 validator, auval and pluginval |
//! | [`agent`] | The scripted MCP client for a plug-in's agent tests |
//!
//! The format hosts (`clap`, `vst3`, `auv2`) are the `hosts` feature, on by
//! default. Without it the crate is the render harness, the signals and the
//! measurements alone, which a plug-in's agent interface uses at run time
//! to hear itself (`lspk-plugin-agent`'s preview).
//!
//! # The allocation trap
//!
//! A test crate installs the trap once, with `lspk_rt::install_trap!();`.
//! Every block the harness renders then runs as a real-time section, and a
//! block that touches the heap fails the test. Without the trap, blocks
//! still run as real-time sections (denormals flushed), so a render is the
//! same either way. The trap sees only the test binary's allocator: render
//! a plug-in in this process, or from its entry point, to check the
//! plug-in's own code.
//!
//! # A plug-in's tests
//!
//! ```ignore
//! use lspk_test::render::{self, Script, Settings};
//! use lspk_test::{golden, measure, signal};
//!
//! lspk_rt::install_trap!();
//!
//! #[test]
//! fn golden_renders_null_against_their_references() {
//!     let script = Script::new(48_000).with_mono_input(signal::sine(1_000.0, 48_000.0, 48_000, 0.5));
//!     let render = render::render::<MyEffect>(&script, Settings::new(48_000.0, 512)).unwrap();
//!     let mut suite = golden::Suite::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"));
//!     suite.add("one-kilohertz", render.output);
//!     suite.assert();
//! }
//! ```

pub mod agent;
#[cfg(all(feature = "hosts", target_os = "macos"))]
pub mod auv2;
#[cfg(feature = "hosts")]
pub mod clap;
pub mod cpu;
pub mod golden;
pub mod measure;
pub mod render;
pub mod signal;
pub mod validate;
#[cfg(feature = "hosts")]
pub mod vst3;

#[cfg(feature = "hosts")]
pub use clap::ClapTestHost;
pub use render::{Instance, Output, Render, Script, Settings, render};
