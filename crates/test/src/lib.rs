//! The headless test harness. It will hold renders, golden renders and null
//! tests, measurement tools, format validators and a scripted MCP client.
//!
//! - [`clap`]: a CLAP host for tests, which loads a plug-in from its built
//!   `.clap` file or from its entry point in the test binary, and drives it
//!   block by block, each block under the allocation trap when the test
//!   crate installs it.
//! - [`vst3`]: a VST3 host for tests, which makes a plug-in from its VST3
//!   factory and records the edits it reports to the host.
//! - [`auv2`] (macOS): an Audio Unit host for tests, which registers a
//!   plug-in's Audio Unit factory in the test process and records the edits
//!   it reports to listeners.

#[cfg(target_os = "macos")]
pub mod auv2;
pub mod clap;
pub mod vst3;
