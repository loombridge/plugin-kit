//! The agent interface inside a running plug-in, so an agent can drive any
//! kit plug-in in any host.
//!
//! Wrap a plug-in in [`WithAgent`] where it's exported, and it gains:
//!
//! - **Commands for its parameters**: `params.get` reads every parameter's
//!   descriptor and value, and `params.set` sets one or many as one step,
//!   beside the plug-in's own commands from
//!   [`Plugin::register_commands`]. Parameter changes reach the host
//!   exactly as a person's would, through [`Host`]'s gestures (begin, value,
//!   end), so the host's automation, undo and saved project stay in step.
//!   See [`commands`].
//! - **Its own agent server**: `lspk-agent`'s Streamable HTTP server on
//!   `127.0.0.1`, started from the plug-in's main thread and served from
//!   [`Plugin::on_main_thread`], off until the person switches it on
//!   ([`WithAgent::set_server`], or the `agent.set_server` command; the
//!   editor will show both). The setting is saved with the project. Each
//!   instance listens on a port of its own, chosen by the system, with a
//!   bearer token of its own, new each time its server starts. Every
//!   request must carry the token, name `127.0.0.1` or `localhost` with
//!   the port as its host, and carry no `Origin` header, as Beatlayer's
//!   server requires.
//! - **Discovery**: while its server runs, each instance has an entry in
//!   the per-user discovery file (`lspk_agent::discovery`), with its port,
//!   token, plug-in ID and name, the host's name and the name of its track
//!   where the host reports it (CLAP's `track-info`). The file is readable
//!   by the user alone, since it holds the tokens.
//! - **The tools every agent gets** (see the guide, [`tools::GUIDE`]):
//!   `list_commands`, `describe_command`, `run` (one command or a batch as
//!   one step, with a dry run), `query` (every parameter and the plug-in's
//!   [summary](Plugin::summary)), `checkpoint` (named snapshots, restored
//!   as one step), and `preview`, which renders a note, chord or phrase (or,
//!   for an effect, a test signal) through a separate offline instance set
//!   as this one is, and returns the audio with an analysis.
//!
//! ```ignore
//! lspk_plugin::export_clap!(lspk_plugin_agent::WithAgent<MyPlugin>);
//! ```
//!
//! The same tools run headless, with no host, over stdio: see
//! [`headless`].
//!
//! # Threads
//!
//! Every tool call runs on the host's main thread, in
//! [`Plugin::on_main_thread`]: the server's threads only read requests and
//! wake the main thread through [`Host::waker`]. Nothing here touches the
//! audio thread. A preview renders on a thread of its own, through an
//! instance of the plug-in made there, never the one the host plays.
//!
//! # Hosts that refuse a socket
//!
//! A sandboxed host (an out-of-process or sandboxed Audio Unit, say) may
//! refuse to let a plug-in listen on a port. The server then doesn't start,
//! and [`WithAgent::server_status`] says why. Under VST3 on Linux,
//! clap-wrapper calls the plug-in back on the main thread only while its
//! editor is open, so the server answers only then.

pub mod commands;
pub mod headless;
pub mod perception;
mod server;
mod state;
pub mod tools;

pub use commands::ServerStatus;
pub use lspk_agent::Question;
pub use state::with_server;

use lspk_commands::{RegisterError, Registry};
use lspk_params::ParamDescriptor;
use lspk_plugin::{Editor, Host, ParamValues, Plugin, PluginError, PluginInfo, ProcessConfig};
use serde_json::Value;

use crate::commands::{Edits, SERVER_STATUS, SET_SERVER, SetServerArgs};
use crate::server::Agent;

/// Plug-in `P` with the agent interface: export this instead of `P`.
///
/// It's `P` in every other way: the same ID, parameters, processor and
/// state (with the agent server's setting saved in front of `P`'s own).
pub struct WithAgent<P: Plugin> {
    plugin: P,
    agent: Agent<P>,
}

impl<P: Plugin> std::fmt::Debug for WithAgent<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WithAgent")
            .field("plugin", &P::INFO.id)
            .field("server", &self.agent.status())
            .finish_non_exhaustive()
    }
}

impl<P: Plugin> WithAgent<P> {
    /// The plug-in.
    pub fn plugin(&self) -> &P {
        &self.plugin
    }

    /// The plug-in, to change.
    pub fn plugin_mut(&mut self) -> &mut P {
        &mut self.plugin
    }

    /// Switches the agent server on or off, as the person does in the
    /// plug-in's settings; the setting is saved with the project. Returns
    /// its state after.
    pub fn set_server(&mut self, on: bool) -> ServerStatus {
        if on != self.agent.wanted() {
            self.agent.host().mark_dirty();
        }
        self.agent.set_wanted(on);
        self.agent.status()
    }

    /// Whether the agent server is on, where agents connect and its token,
    /// for the person to give to an agent.
    #[must_use]
    pub fn server_status(&self) -> ServerStatus {
        self.agent.status()
    }

    /// Questions waiting for the person: agents' commands that ask first.
    /// The editor shows them and answers with [`WithAgent::answer`].
    #[must_use]
    pub fn questions(&self) -> Vec<Question> {
        self.agent.questions()
    }

    /// Answers question `id`; false if none has that ID.
    pub fn answer(&mut self, id: u64, allow: bool) -> bool {
        self.agent.answer(&mut self.plugin, id, allow)
    }
}

impl<P: Plugin> Plugin for WithAgent<P> {
    type Processor = P::Processor;

    const INFO: PluginInfo = P::INFO;

    fn new(host: Host) -> Self {
        Self {
            plugin: P::new(host.clone()),
            agent: Agent::new(host),
        }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        self.plugin.params()
    }

    fn param_space(&self) -> Option<lspk_params::ParamSpace> {
        self.plugin.param_space()
    }

    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<Self::Processor, PluginError> {
        self.plugin.activate(config, values)
    }

    fn deactivate(&mut self, processor: Self::Processor) {
        self.plugin.deactivate(processor);
    }

    fn save_state(&self) -> Vec<u8> {
        let settings = state::Settings {
            server: self.agent.wanted(),
        };
        state::encode(settings, &self.plugin.save_state())
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let (settings, own) = state::decode(data);
        self.plugin.load_state(own)?;
        self.agent.set_wanted(settings.server);
        Ok(())
    }

    fn latency(&self) -> u32 {
        self.plugin.latency()
    }

    fn on_main_thread(&mut self) {
        self.plugin.on_main_thread();
        self.agent.pump(&mut self.plugin);
    }

    fn track_info_changed(&mut self) {
        self.plugin.track_info_changed();
        self.agent.track_changed();
    }

    fn summary(&self) -> Option<String> {
        self.plugin.summary()
    }

    fn editor(&mut self) -> Option<Box<dyn Editor>> {
        self.plugin.editor()
    }

    fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
        commands::register(registry)?;
        self.plugin.register_commands(registry)
    }

    fn run_command(&mut self, id: &str, args: &Value) -> Result<Value, PluginError> {
        let status = |status: ServerStatus| {
            serde_json::to_value(status).map_err(|e| PluginError::new(e.to_string()))
        };
        match id {
            SET_SERVER => {
                let SetServerArgs { on } = commands::parse(args).map_err(PluginError::new)?;
                status(self.set_server(on))
            }
            SERVER_STATUS => status(self.server_status()),
            _ => {
                let host = self.agent.host().clone();
                let mut edits = Edits::new(&host);
                let result = commands::run_params(&host, &mut edits, id, args);
                edits.finish();
                match result {
                    Some(result) => result.map_err(PluginError::new),
                    None => self.plugin.run_command(id, args),
                }
            }
        }
    }
}
