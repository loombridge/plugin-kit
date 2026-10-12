//! The agent interface: a program driven by an AI agent over the Model
//! Context Protocol (MCP), with whatever model the person has.
//!
//! The kit holds the protocol and its transports; a program brings its own
//! tools. A [`Program`] says what its tools are and runs them, and a
//! [`Server`] does the rest: JSON-RPC, MCP's lifecycle, tools and resources
//! methods, sessions, the person's approval of agents and of commands that
//! ask first, and calls that wait for the person's gesture or for the host.
//!
//! | Piece | What it is |
//! |---|---|
//! | [`Server`] | The server's state on the main thread: sessions, the MCP methods, and calls waiting for the person or the host |
//! | [`Program`] | What a program implements: its tools, resources, and the person's approvals |
//! | [`permission`] | Who may run what: permission classes against where the server runs and what the person allowed |
//! | [`stdio`] | The stdio transport, which a command-line tool serves headless |
//! | [`http`] | The Streamable HTTP transport on `127.0.0.1`, with bearer tokens and host and origin checks |
//! | [`client`] | A scripted MCP client over either, or in this process, for scripts and tests |
//! | [`jsonrpc`] | JSON-RPC 2.0 messages |
//! | [`base64`] | Base64, which MCP carries audio, images and binary resources in |
//!
//! # Where commands run
//!
//! Every message is handled on the program's main thread, with the
//! program's [`Program::Host`] (its document, engine and settings) lent to
//! the server for that message, so an agent's command takes the person's
//! path and never touches the audio thread. The HTTP server's threads only
//! parse requests and hand them to the main thread's event loop.
//!
//! # Permissions
//!
//! Every command has a permission class. Read and edit commands run.
//! Destructive and external ones ask the person first
//! ([`Outcome::Ask`]), unless the person turned on autonomous mode for that
//! agent; headless, they're refused unless the server was started with
//! `--autonomous`. Commands that change what agents may do are always
//! refused. An agent's rights belong to its own token: a session carries a
//! [`Credential`], and only one carrying the agent's own token (or the one
//! the person allowed when it first asked) has that agent's approval and
//! autonomous mode, whatever name its client gives. See [`permission`].
//!
//! # Calls that wait
//!
//! While the person drags something that will be one undo step, tool
//! calls wait ([`Outcome::Wait`]), and so do the program's events; once the
//! drag ends, [`Server::resume`] runs them in the order they came. A tool
//! that needs the host to do something first, such as draw a frame to
//! capture, waits the same way until the host answers it
//! ([`ToolOutcome::Defer`], [`Server::finish_deferred`]).
//!
//! # Example
//!
//! ```
//! use lspk_agent::client::{Client, InProcess};
//! use lspk_agent::permission::Mode;
//! use lspk_agent::{Call, Program, Server, ServerInfo, ToolOutcome, tool_definition};
//! use serde_json::{Value, json};
//!
//! /// A program with one tool, which reads its host's gain.
//! struct Gain;
//!
//! #[derive(schemars::JsonSchema)]
//! struct NoArgs {}
//!
//! impl Program for Gain {
//!     type Host = f64;
//!     type Event = ();
//!     type Deferred = ();
//!
//!     fn info(&self) -> ServerInfo {
//!         ServerInfo {
//!             name: "gain".to_owned(),
//!             title: "Gain".to_owned(),
//!             version: "1.0.0".to_owned(),
//!             instructions: "Read the gain with get_gain.".to_owned(),
//!         }
//!     }
//!
//!     fn tools(&self) -> Vec<Value> {
//!         vec![tool_definition::<NoArgs>("get_gain", "Get gain", "Reads the gain in dB.", true)]
//!     }
//!
//!     fn call(&mut self, call: Call<'_, f64>, name: &str, _: Value) -> Result<ToolOutcome<()>, String> {
//!         match name {
//!             "get_gain" => Ok(ToolOutcome::Done(Ok(json!({ "db": *call.host })))),
//!             other => Err(format!("no tool is called `{other}`")),
//!         }
//!     }
//!
//!     fn allows(&self, _: &f64, _: &str) -> bool {
//!         false
//!     }
//!
//!     fn approve(&mut self, _: &mut f64, _: &str) -> bool {
//!         false
//!     }
//! }
//!
//! let server = Server::new(Mode::Headless { autonomous: false }, Gain);
//! let mut client = Client::new(InProcess::new(server, -6.0));
//! client.initialize("doc-test")?;
//! let gain = client.call("get_gain", json!({}))?.ok()?;
//! assert_eq!(gain["db"], -6.0);
//! # Ok::<(), String>(())
//! ```

pub mod base64;
pub mod client;
pub mod http;
pub mod jsonrpc;
pub mod permission;
mod server;
pub mod stdio;

pub use server::{
    Call, ClientInfo, Credential, Media, Outcome, PROTOCOL_VERSIONS, Program, Question,
    QuestionKind, STDIO_SESSION, Server, ServerInfo, Session, ToolOutcome, tool_definition,
};
