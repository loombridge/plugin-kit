//! The same tools, served headless: a plug-in instance with no host and no
//! window, driven over stdio by a command-line tool's `mcp` command.
//!
//! [`Headless`] makes the instance a host would (its parameters, its link
//! to the host) and serves [`Tools`](crate::tools::Tools) on it, so an agent
//! has the tools [`WithAgent`](crate::WithAgent) gives it in a host:
//! `list_commands`, `describe_command`, `run`, `query`, `checkpoint` and
//! `preview`, and the guide. There is no person to ask, so the
//! commands that would ask first (destructive and external ones) are
//! refused, and the refusal says why, unless the server is autonomous (the
//! tool's `--autonomous`).
//!
//! The instance is never activated: nothing plays it. A `preview` renders a
//! separate instance made from this one's parameters and state, as it does
//! in a host.
//!
//! ```ignore
//! let mut instance = lspk_plugin_agent::headless::Headless::<MyPlugin>::new()?;
//! instance.serve("my-synth-cli mcp", autonomous, stdin().lock(), stdout().lock())?;
//! ```

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use lspk_agent::permission::Mode;
use lspk_agent::{Server, stdio};
use lspk_plugin::{Host, HostBackend, ParamValues, Plugin};

use crate::tools::Tools;

/// A host that is nobody: every request is dropped, since nothing plays the
/// instance and no project holds it.
struct NoHost;

impl HostBackend for NoHost {
    fn request_flush(&self) {}
    fn mark_dirty(&self) {}
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
    fn values_changed(&self) {}
}

/// An instance of plug-in `P` with no host, for an agent to drive.
pub struct Headless<P: Plugin> {
    // Field order matters: the plug-in goes before its host link.
    plugin: P,
    host: Host,
    /// Empties the queue of edits the plug-in makes to its own parameters,
    /// which only a processor would take: nothing plays this instance.
    drain: Box<dyn FnMut()>,
}

impl<P: Plugin> std::fmt::Debug for Headless<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Headless")
            .field("plugin", &P::INFO.id)
            .finish_non_exhaustive()
    }
}

impl<P: Plugin> Headless<P> {
    /// Makes the instance, as a host does when it loads the plug-in.
    ///
    /// # Errors
    ///
    /// The plug-in's parameters break a rule or repeat an ID.
    pub fn new() -> Result<Self, String> {
        let host = Host::new(Box::new(NoHost));
        let plugin = P::new(host.clone());
        let values = Arc::new(ParamValues::for_plugin(&plugin).map_err(|e| e.to_string())?);
        let mut edits = host.connect(values);
        Ok(Self {
            plugin,
            host,
            drain: Box::new(move || while edits.pop().is_ok() {}),
        })
    }

    /// The plug-in, to set up before an agent connects: load a preset,
    /// say.
    pub fn plugin_mut(&mut self) -> &mut P {
        &mut self.plugin
    }

    /// The agent server over this instance, for a client in this process
    /// (such as [`lspk_test::agent::in_process`] with [`Headless::into_plugin`]).
    /// `command` is how a refusal names the tool, such as `my-synth-cli mcp`.
    #[must_use]
    pub fn server(&self, command: &str, autonomous: bool) -> Server<Tools<P>> {
        let mode = Mode::Headless { autonomous };
        Server::new(mode, Tools::new(self.host.clone(), mode, command))
    }

    /// Serves the agent interface on `input` and `output` until `input`
    /// ends: JSON-RPC messages, one per line, as [`stdio::serve`] does.
    ///
    /// # Errors
    ///
    /// An I/O error reading `input` or writing `output`.
    pub fn serve(
        &mut self,
        command: &str,
        autonomous: bool,
        input: impl BufRead,
        output: impl Write,
    ) -> io::Result<()> {
        let mut server = self.server(command, autonomous);
        let drain = &mut self.drain;
        stdio::serve_until(&mut server, &mut self.plugin, input, output, |_| {
            drain();
            false
        })
    }

    /// The plug-in, handed over to serve it in this process.
    #[must_use]
    pub fn into_plugin(self) -> P {
        self.plugin
    }
}
