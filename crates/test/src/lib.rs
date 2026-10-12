//! The headless test harness. It will hold renders, golden renders and null
//! tests, measurement tools, format validators and a scripted MCP client.
//!
//! - [`clap`]: a CLAP host for tests, which loads a plug-in from its built
//!   `.clap` file or from its entry point in the test binary, and drives it
//!   block by block, each block under the allocation trap when the test
//!   crate installs it.

pub mod clap;
