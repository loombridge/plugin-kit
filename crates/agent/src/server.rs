//! The MCP server's state on the main thread: sessions, the methods MCP
//! defines, calls waiting for the person or for the host, and the program
//! whose tools it serves.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use lspk_commands::Permission;
use schemars::JsonSchema;
use serde_json::{Value, json};

use crate::jsonrpc::{self, Message, code};
use crate::permission::{Mode, class_names};

/// The protocol versions this server speaks, newest first. A client asking
/// for another gets the newest.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The first version whose tool results carry `structuredContent`.
const STRUCTURED_FROM: &str = "2025-06-18";

/// The first version with `audio` content blocks. Before it, audio goes in
/// an embedded resource.
const AUDIO_FROM: &str = "2025-03-26";

/// The session ID a stdio server's one session has.
pub const STDIO_SESSION: &str = "stdio";

/// Who's on the other end of a session, as the client said when it
/// connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    /// Its name, such as `claude-code`: what the person allows.
    pub name: String,
    /// Its name for people, such as `Claude Code`, if it gave one.
    pub title: Option<String>,
    /// Its version, if it gave one.
    pub version: Option<String>,
}

impl ClientInfo {
    /// The name people see: the title if there is one.
    #[must_use]
    pub fn label(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            name: "agent".to_owned(),
            title: None,
            version: None,
        }
    }
}

/// How a session's requests show who sent them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Credential {
    /// No token: the client started the server (over stdio) or runs in its
    /// process. In the running program it counts as the server's token.
    #[default]
    Local,
    /// The agent server's token, which the program's settings show: a
    /// client asking to connect, which is asked about each time and is never
    /// autonomous, whatever name it gives.
    Server,
    /// The own token of the allowed agent with this name: the requests are
    /// that agent's, whatever name the client gives.
    Agent(String),
}

/// One client's connection.
#[derive(Clone, Debug, Default)]
pub struct Session {
    /// Who it is, as it said when it connected, or as its token says.
    pub client: ClientInfo,
    /// The protocol version agreed, once it's initialised.
    pub protocol: Option<String>,
    /// The names a program's tools gave new objects' IDs (such as
    /// `{"$new": name}`), so later calls in the session can refer to them.
    pub refs: BTreeMap<String, u64>,
    /// How its requests show who sent them. Every request on a session
    /// carries the credential it started with.
    pub credential: Credential,
    /// The allowed agent whose rights the session has (its approval, and
    /// autonomous mode if the person switched it on): the agent whose own
    /// token it carries, or the agent the person allowed when this session
    /// first asked. `None` for a session the person allowed for itself
    /// only, which asks first for every destructive and external command.
    pub agent: Option<String>,
}

/// What handling a message came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Send this reply, or nothing for a notification or response.
    Reply(Option<Value>),
    /// Ask the person, then call [`Server::resolve`] with the answer; the
    /// reply comes from there.
    Ask(Question),
    /// A tool call that waits: for the person's gesture (a drag) to end, so
    /// it lands after the drop as its own undo step, or for the host to do
    /// what it asked ([`ToolOutcome::Defer`]). [`Server::resume`] gives its
    /// outcome with this ticket.
    Wait(u64),
}

/// Something only the person can answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// Its ID, for [`Server::resolve`].
    pub id: u64,
    /// The agent asking, as people see its name.
    pub agent: String,
    /// What's asked.
    pub kind: QuestionKind,
    /// The question as the person reads it.
    text: String,
}

/// What the person is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    /// An agent the person hasn't allowed wants to connect.
    Connect {
        /// Its name, which the approval is kept under.
        client: String,
    },
    /// An agent wants to run commands that ask first.
    Confirm {
        /// Their most far-reaching class: destructive or external.
        class: Permission,
        /// Every class among them that asks first, least reach first, such
        /// as destructive and external for a command that does both.
        classes: Vec<Permission>,
        /// What would run, a line per command.
        summary: String,
    },
}

impl Question {
    /// The question as the person reads it, in a sentence or two.
    #[must_use]
    pub fn text(&self) -> String {
        self.text.clone()
    }
}

/// What a program says about itself when a client connects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    /// Its name for programs, such as `my-synth`. Audio with no resource of
    /// its own is named `<name>://audio`.
    pub name: String,
    /// Its name for people, such as `My Synth`.
    pub title: String,
    /// Its version.
    pub version: String,
    /// What `initialize` tells the agent, which clients show to the model:
    /// what the program is, and which tools and resources to start with.
    pub instructions: String,
}

/// Something a tool returns for the model beside its JSON: audio or an
/// image.
#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    /// Its media type, such as `audio/wav` or `image/png`.
    pub mime_type: &'static str,
    /// The file.
    pub bytes: Vec<u8>,
    /// The resource it can also be read at, if any.
    pub uri: Option<String>,
}

impl Media {
    /// Whether it's audio rather than an image.
    #[must_use]
    pub fn is_audio(&self) -> bool {
        self.mime_type.starts_with("audio/")
    }
}

/// How a tool call ended.
#[derive(Debug)]
pub enum ToolOutcome<D> {
    /// It finished: its result, or why it failed (which the agent reads).
    Done(Result<Value, String>),
    /// It finished with a result and something for the model to hear or
    /// see beside it: audio or an image.
    WithMedia(Value, Vec<Media>),
    /// It needs the host to do something first, such as draw a frame to
    /// capture: the call waits ([`Outcome::Wait`]),
    /// [`Program::defer`] hears what's asked with the call's ticket, and the
    /// host answers it with [`Server::finish_deferred`].
    Defer(D),
    /// It needs the person's confirmation first.
    Ask {
        /// The most far-reaching class among what it would run.
        class: Permission,
        /// Every class among them that asks first, least reach first.
        classes: Vec<Permission>,
        /// What it would do, for the person to read.
        summary: String,
    },
}

/// Everything a tool call works with besides the program itself.
#[derive(Debug)]
pub struct Call<'a, H> {
    /// Where the agent interface runs.
    pub mode: Mode,
    /// What the program's commands run on, which the host lent the server
    /// for this message.
    pub host: &'a mut H,
    /// The session the call came on.
    pub session: &'a mut Session,
    /// The person already allowed this call.
    pub confirmed: bool,
}

/// A program served over MCP: its tools and resources, and what the
/// server asks of it about the person.
///
/// The server handles the protocol, sessions, permissions questions and
/// waiting calls; the program answers for its own tools. A program's state
/// (its tools' shared state, such as checkpoints) lives in the value that
/// implements this; what its commands run on, the [`Host`](Program::Host),
/// is lent to the server with each message, so it stays the host's.
pub trait Program {
    /// What tool calls run on, such as the program's document and engine on
    /// the main thread.
    type Host;
    /// Something other than a tool call that waits for the person's
    /// gesture to end, in order with tool calls, such as an engine's report
    /// that a scheduled command fired. `()` if there's none.
    type Event: fmt::Debug;
    /// What a tool call that waits for the host asks of it
    /// ([`ToolOutcome::Defer`]). `()` if none does.
    type Deferred: fmt::Debug;

    /// What the program says about itself when a client connects.
    fn info(&self) -> ServerInfo;

    /// Every tool's definition, for `tools/list` (see [`tool_definition`]).
    fn tools(&self) -> Vec<Value>;

    /// Runs the tool called `name`.
    ///
    /// # Errors
    ///
    /// A message if no tool has that name, which is a protocol error rather
    /// than the tool's own failure.
    fn call(
        &mut self,
        call: Call<'_, Self::Host>,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome<Self::Deferred>, String>;

    /// The resources `resources/list` lists.
    fn resources(&self, _host: &Self::Host) -> Vec<Value> {
        Vec::new()
    }

    /// The resource templates `resources/templates/list` lists.
    fn resource_templates(&self) -> Vec<Value> {
        Vec::new()
    }

    /// The contents of the resource at `uri`, as `resources/read` returns
    /// them.
    ///
    /// # Errors
    ///
    /// Why there's none, which the client gets as a resource-not-found error.
    fn read_resource(&mut self, _host: &mut Self::Host, uri: &str) -> Result<Vec<Value>, String> {
        Err(format!("no resource is at `{uri}`"))
    }

    /// Whether the person's gesture (such as a drag that will be one undo
    /// step) is under way, so tool calls and events wait for it to end.
    fn gesture_under_way(&self, _host: &Self::Host) -> bool {
        false
    }

    /// Handles an event that waited, or didn't need to wait, for the
    /// person's gesture to end ([`Server::on_event`]).
    fn on_event(&mut self, _host: &mut Self::Host, _event: Self::Event) {}

    /// Hears that a tool call waits for the host ([`ToolOutcome::Defer`]),
    /// with the ticket [`Server::finish_deferred`] answers it with.
    fn defer(&mut self, _ticket: u64, _deferred: &Self::Deferred) {}

    /// Whether the person allows the agent called `agent`.
    fn allows(&self, host: &Self::Host, agent: &str) -> bool;

    /// Allows the agent called `client` for good, as the person just
    /// agreed, making it its own token. Returns whether it worked.
    fn approve(&mut self, host: &mut Self::Host, client: &str) -> bool;

    /// What the person is asked when an agent they haven't allowed
    /// connects: `agent` is its name for people, `client` its name.
    fn connect_question(&self, agent: &str, client: &str) -> String {
        format!(
            "\u{201c}{agent}\u{201d} ({client}) wants to control {} through the agent server. \
             Allow it?",
            self.info().title
        )
    }
}

/// A request waiting for the person's answer.
#[derive(Clone, Debug)]
struct Waiting {
    session: String,
    request: Value,
    kind: WaitingKind,
}

#[derive(Clone, Debug)]
enum WaitingKind {
    /// `initialize` from an agent not yet allowed; this is its result.
    Connect { result: Value },
    /// `tools/call`.
    Call { name: String, args: Value },
}

/// Something that waits for the person's gesture to end, in the order it
/// came.
#[derive(Clone, Debug)]
enum Held<E> {
    /// A tool call.
    Call {
        ticket: u64,
        session: String,
        request: Value,
        params: Value,
    },
    /// The program's event.
    Event(E),
}

/// A tool call waiting for the host.
#[derive(Clone, Debug)]
struct Deferred<D> {
    session: String,
    request: Value,
    asked: D,
}

/// The agent interface on the main thread: every session, the program
/// whose tools it serves, and calls waiting for the person or the host.
///
/// A transport hands each message to [`Server::handle`] with the session it
/// came on and the program's host, and sends back the reply. Commands run
/// on the host, the same one the program's interface drives, so an agent's
/// command takes the same path as a person's.
#[derive(Debug)]
pub struct Server<P: Program> {
    mode: Mode,
    program: P,
    sessions: BTreeMap<String, Session>,
    waiting: BTreeMap<u64, Waiting>,
    next_question: u64,
    /// What waits for the person's gesture to end, oldest first.
    held: VecDeque<Held<P::Event>>,
    next_ticket: u64,
    /// Calls waiting for the host, by ticket.
    deferred: BTreeMap<u64, Deferred<P::Deferred>>,
    /// Calls the host has answered, waiting to be handed back by
    /// [`Server::resume`].
    answered: VecDeque<(u64, Outcome)>,
}

impl<P: Program> Server<P> {
    /// The agent interface for `program`, running as `mode` says.
    #[must_use]
    pub fn new(mode: Mode, program: P) -> Self {
        Self {
            mode,
            program,
            sessions: BTreeMap::new(),
            waiting: BTreeMap::new(),
            next_question: 0,
            held: VecDeque::new(),
            next_ticket: 0,
            deferred: BTreeMap::new(),
            answered: VecDeque::new(),
        }
    }

    /// Where the agent interface runs.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The program.
    #[must_use]
    pub fn program(&self) -> &P {
        &self.program
    }

    /// The program, to change.
    pub fn program_mut(&mut self) -> &mut P {
        &mut self.program
    }

    /// Starts a session with a new random ID, or the given one, and returns
    /// its ID. Its requests carry no token ([`Credential::Local`]).
    pub fn open_session(&mut self, id: Option<&str>) -> String {
        self.open_session_as(id, Credential::Local)
    }

    /// Starts a session whose requests carry `credential`, with a new
    /// random ID or the given one, and returns its ID.
    pub fn open_session_as(&mut self, id: Option<&str>, credential: Credential) -> String {
        let id = id.map_or_else(new_session_id, str::to_owned);
        self.sessions.entry(id.clone()).or_default().credential = credential;
        id
    }

    /// The credential the session's requests carry, if it's open.
    #[must_use]
    pub fn session_credential(&self, id: &str) -> Option<&Credential> {
        self.sessions.get(id).map(|session| &session.credential)
    }

    /// Whether a session with this ID is open.
    #[must_use]
    pub fn has_session(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }

    /// Ends a session. Calls it left waiting for the person still finish.
    pub fn close_session(&mut self, id: &str) {
        self.sessions.remove(id);
    }

    /// The open sessions, by ID, with who's on each.
    pub fn sessions(&self) -> impl Iterator<Item = (&str, &Session)> {
        self.sessions.iter().map(|(id, s)| (id.as_str(), s))
    }

    /// Ends every session with the rights of the agent called `client`, as
    /// forgetting its approval does.
    pub fn close_sessions_of(&mut self, client: &str) {
        self.sessions.retain(|_, s| {
            s.agent.as_deref() != Some(client)
                && s.credential != Credential::Agent(client.to_owned())
        });
    }

    /// Hands the program an event now, or, while the person's gesture is
    /// under way or calls wait for it to end, once it ends, in order with
    /// them.
    pub fn on_event(&mut self, host: &mut P::Host, event: P::Event) {
        if self.program.gesture_under_way(host) || !self.held.is_empty() {
            self.held.push_back(Held::Event(event));
        } else {
            self.program.on_event(host, event);
        }
    }

    /// The tickets of the calls waiting for the host, oldest first.
    #[must_use]
    pub fn deferred(&self) -> Vec<u64> {
        self.deferred.keys().copied().collect()
    }

    /// Answers a call that waited for the host: `finish` turns what it
    /// asked into its result and any audio or images, or why there's none.
    /// The waiting call's reply comes from [`Server::resume`], with the
    /// ticket. A ticket nothing waits on is ignored.
    pub fn finish_deferred(
        &mut self,
        ticket: u64,
        finish: impl FnOnce(P::Deferred) -> Result<(Value, Vec<Media>), String>,
    ) {
        let Some(Deferred {
            session,
            request,
            asked,
        }) = self.deferred.remove(&ticket)
        else {
            return;
        };
        let protocol = self.sessions.get(&session).and_then(|s| s.protocol.clone());
        let structured = protocol.as_deref().is_none_or(|v| v >= STRUCTURED_FROM);
        let name = self.program.info().name;
        let body = match finish(asked) {
            Ok((value, media)) => {
                tool_result(Ok(value), structured, &media, protocol.as_deref(), &name)
            }
            Err(message) => tool_result(Err(message), structured, &[], protocol.as_deref(), &name),
        };
        self.answered.push_back((
            ticket,
            Outcome::Reply(Some(jsonrpc::response(request, body))),
        ));
    }

    /// Whether tool calls or events wait for the person's gesture to end,
    /// or calls the host has answered wait to be handed back.
    #[must_use]
    pub fn holds_calls(&self) -> bool {
        !self.held.is_empty() || !self.answered.is_empty()
    }

    /// Once the person's gesture has ended, runs what waited for it in the
    /// order it came, each call as its own undo step: events go to the
    /// program, and the next call that waited runs and gives its outcome
    /// with the ticket its [`Outcome::Wait`] had. Call it again until it
    /// gives `None`. While the gesture goes on, or a call it ran waits for
    /// the person's answer, it gives `None`. Calls the host has answered
    /// ([`Server::finish_deferred`]) come first, gesture or not.
    pub fn resume(&mut self, host: &mut P::Host) -> Option<(u64, Outcome)> {
        if let Some(answered) = self.answered.pop_front() {
            return Some(answered);
        }
        loop {
            if self.program.gesture_under_way(host)
                || self
                    .waiting
                    .values()
                    .any(|w| matches!(w.kind, WaitingKind::Call { .. }))
            {
                return None;
            }
            match self.held.pop_front()? {
                Held::Event(event) => self.program.on_event(host, event),
                Held::Call {
                    ticket,
                    session,
                    request,
                    params,
                } => {
                    let outcome = if self.sessions.contains_key(&session) {
                        self.call(host, &session, request, params, false)
                    } else {
                        Outcome::Reply(Some(jsonrpc::error_response(
                            request,
                            code::INVALID_REQUEST,
                            "the session ended while the call waited",
                        )))
                    };
                    return Some((ticket, outcome));
                }
            }
        }
    }

    /// The questions waiting for the person, oldest first.
    pub fn questions(&self) -> impl Iterator<Item = u64> + '_ {
        self.waiting.keys().copied()
    }

    /// Handles one message from `session`. A message on a session that
    /// isn't open opens it, as stdio's one session is.
    pub fn handle(&mut self, host: &mut P::Host, session: &str, message: Value) -> Outcome {
        let (id, method, params) = match Message::parse(message) {
            Err(error) => return Outcome::Reply(Some(error)),
            Ok(Message::Response) => return Outcome::Reply(None),
            Ok(Message::Notification { .. }) => {
                // `notifications/initialized` and `notifications/cancelled`
                // need nothing from this server.
                return Outcome::Reply(None);
            }
            Ok(Message::Request { id, method, params }) => (id, method, params),
        };
        let open = self.sessions.entry(session.to_owned()).or_default();
        if self.mode == Mode::App
            && method != "initialize"
            && open.protocol.is_some()
            && let Some(name) = open.agent.clone()
            && !self.program.allows(host, &name)
        {
            self.sessions.remove(session);
            return Outcome::Reply(Some(jsonrpc::error_response(
                id,
                code::NOT_ALLOWED,
                format!(
                    "the person withdrew {name}'s approval, so this session is over; connect \
                     again to ask"
                ),
            )));
        }
        match method.as_str() {
            "initialize" => self.initialize(host, session, id, &params),
            "ping" => reply(id, json!({})),
            "tools/list" => reply(id, json!({ "tools": self.program.tools() })),
            // While the person's hand is on the program, agents' calls
            // wait, in order, and land after the drop.
            "tools/call" if self.program.gesture_under_way(host) || !self.held.is_empty() => {
                self.next_ticket += 1;
                let ticket = self.next_ticket;
                self.held.push_back(Held::Call {
                    ticket,
                    session: session.to_owned(),
                    request: id,
                    params,
                });
                Outcome::Wait(ticket)
            }
            "tools/call" => self.call(host, session, id, params, false),
            "resources/list" => reply(id, json!({ "resources": self.program.resources(host) })),
            "resources/templates/list" => reply(
                id,
                json!({ "resourceTemplates": self.program.resource_templates() }),
            ),
            "resources/read" => {
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match self.program.read_resource(host, uri) {
                    Ok(contents) => reply(id, json!({ "contents": contents })),
                    Err(message) => {
                        Outcome::Reply(Some(jsonrpc::error_response(id, -32_002, message)))
                    }
                }
            }
            "prompts/list" => reply(id, json!({ "prompts": [] })),
            "logging/setLevel" => reply(id, json!({})),
            other => Outcome::Reply(Some(jsonrpc::error_response(
                id,
                code::METHOD_NOT_FOUND,
                format!("this server has no method `{other}`"),
            ))),
        }
    }

    fn initialize(
        &mut self,
        host: &mut P::Host,
        session: &str,
        id: Value,
        params: &Value,
    ) -> Outcome {
        let asked = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == asked)
            .unwrap_or(&PROTOCOL_VERSIONS[0]);
        let info = params.get("clientInfo");
        let text = |key: &str| {
            info.and_then(|i| i.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let client = ClientInfo {
            name: text("name").unwrap_or_else(|| "agent".to_owned()),
            title: text("title"),
            version: text("version"),
        };
        let entry = self.sessions.entry(session.to_owned()).or_default();
        entry.client = client.clone();
        entry.protocol = Some((*version).to_owned());
        let credential = entry.credential.clone();
        let server = self.program.info();
        let result = json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "listChanged": false, "subscribe": false },
            },
            "serverInfo": {
                "name": server.name,
                "title": server.title,
                "version": server.version,
            },
            "instructions": server.instructions,
        });
        if self.mode == Mode::App {
            // An allowed agent's own token says who it is, whatever name
            // its client gives; any other client is asked about, every time
            // it connects.
            if let Credential::Agent(name) = credential {
                if !self.program.allows(host, &name) {
                    self.sessions.remove(session);
                    return Outcome::Reply(Some(jsonrpc::error_response(
                        id,
                        code::NOT_ALLOWED,
                        "that token belongs to no agent the person allows",
                    )));
                }
                let entry = self.sessions.entry(session.to_owned()).or_default();
                if entry.client.name != name {
                    entry.client = ClientInfo {
                        name: name.clone(),
                        title: None,
                        version: client.version,
                    };
                }
                entry.agent = Some(name);
                return reply(id, result);
            }
            let text = self.program.connect_question(client.label(), &client.name);
            return self.ask(
                session,
                id,
                WaitingKind::Connect { result },
                client.label().to_owned(),
                QuestionKind::Connect {
                    client: client.name,
                },
                text,
            );
        }
        reply(id, result)
    }

    fn call(
        &mut self,
        host: &mut P::Host,
        session: &str,
        id: Value,
        params: Value,
        confirmed: bool,
    ) -> Outcome {
        let Some(name) = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Outcome::Reply(Some(jsonrpc::error_response(
                id,
                code::INVALID_PARAMS,
                "`tools/call` needs the tool's `name`",
            )));
        };
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        let Some(open) = self.sessions.get_mut(session) else {
            return Outcome::Reply(Some(jsonrpc::error_response(
                id,
                code::INVALID_REQUEST,
                "the session is over",
            )));
        };
        let protocol = open.protocol.clone();
        let structured = protocol.as_deref().is_none_or(|v| v >= STRUCTURED_FROM);
        let agent = open.client.label().to_owned();
        let call = Call {
            mode: self.mode,
            host,
            session: open,
            confirmed,
        };
        let outcome = self.program.call(call, &name, args.clone());
        let server = self.program.info().name;
        match outcome {
            Err(message) => Outcome::Reply(Some(jsonrpc::error_response(
                id,
                code::INVALID_PARAMS,
                message,
            ))),
            Ok(ToolOutcome::Done(result)) => reply(
                id,
                tool_result(result, structured, &[], protocol.as_deref(), &server),
            ),
            Ok(ToolOutcome::WithMedia(result, media)) => reply(
                id,
                tool_result(Ok(result), structured, &media, protocol.as_deref(), &server),
            ),
            Ok(ToolOutcome::Defer(asked)) => {
                self.next_ticket += 1;
                let ticket = self.next_ticket;
                self.program.defer(ticket, &asked);
                self.deferred.insert(
                    ticket,
                    Deferred {
                        session: session.to_owned(),
                        request: id,
                        asked,
                    },
                );
                Outcome::Wait(ticket)
            }
            Ok(ToolOutcome::Ask {
                class,
                classes,
                summary,
            }) => {
                let text = confirm_text(&agent, class, &classes, &summary);
                self.ask(
                    session,
                    id,
                    WaitingKind::Call { name, args },
                    agent,
                    QuestionKind::Confirm {
                        class,
                        classes,
                        summary,
                    },
                    text,
                )
            }
        }
    }

    fn ask(
        &mut self,
        session: &str,
        request: Value,
        kind: WaitingKind,
        agent: String,
        question: QuestionKind,
        text: String,
    ) -> Outcome {
        self.next_question += 1;
        let id = self.next_question;
        self.waiting.insert(
            id,
            Waiting {
                session: session.to_owned(),
                request,
                kind,
            },
        );
        Outcome::Ask(Question {
            id,
            agent,
            kind: question,
            text,
        })
    }

    /// Answers a question: `allow` is the person's answer. Returns the reply
    /// to the request that asked, or `None` if no question has that ID.
    ///
    /// Allowing an agent to connect allows it for good
    /// ([`Program::approve`]), and the session gets its rights; if an agent
    /// with that name is already allowed, the session is allowed for itself
    /// only, with none of that agent's rights, since anyone can give a name.
    /// Allowing a call runs it.
    pub fn resolve(&mut self, host: &mut P::Host, question: u64, allow: bool) -> Option<Value> {
        let Waiting {
            session,
            request,
            kind,
        } = self.waiting.remove(&question)?;
        Some(match kind {
            WaitingKind::Connect { result } => {
                let name = self
                    .sessions
                    .get(&session)
                    .map(|s| s.client.name.clone())
                    .unwrap_or_default();
                let known = self.program.allows(host, &name);
                if allow && (known || self.program.approve(host, &name)) {
                    if let Some(open) = self.sessions.get_mut(&session) {
                        open.agent = (!known).then_some(name);
                    }
                    jsonrpc::response(request, result)
                } else {
                    self.sessions.remove(&session);
                    jsonrpc::error_response(
                        request,
                        code::NOT_ALLOWED,
                        format!(
                            "the person didn't allow {name} to control {}",
                            self.program.info().title
                        ),
                    )
                }
            }
            WaitingKind::Call { name, args } => {
                if allow {
                    match self.call(
                        host,
                        &session,
                        request.clone(),
                        json!({ "name": name, "arguments": args }),
                        true,
                    ) {
                        Outcome::Reply(Some(reply)) => reply,
                        // It was confirmed, so it never asks again, and it
                        // runs straight away.
                        Outcome::Reply(None) | Outcome::Ask(_) | Outcome::Wait(_) => {
                            jsonrpc::error_response(
                                request,
                                code::INTERNAL_ERROR,
                                "the call asked twice",
                            )
                        }
                    }
                } else {
                    let structured = self
                        .sessions
                        .get(&session)
                        .and_then(|s| s.protocol.as_deref())
                        .is_none_or(|v| v >= STRUCTURED_FROM);
                    jsonrpc::response(
                        request,
                        tool_result(
                            Err(format!(
                                "the person declined to let you run this ({name}). Nothing \
                                 changed. Ask them before trying it again."
                            )),
                            structured,
                            &[],
                            None,
                            "",
                        ),
                    )
                }
            }
        })
    }
}

/// The question for commands that ask first, as the person reads it.
fn confirm_text(agent: &str, class: Permission, classes: &[Permission], summary: &str) -> String {
    format!(
        "\u{201c}{agent}\u{201d} wants to run {} command{}:\n\n{summary}\n\nAllow it?",
        if classes.is_empty() {
            class.to_string()
        } else {
            class_names(classes)
        },
        if summary.lines().count() == 1 {
            ""
        } else {
            "s"
        }
    )
}

fn reply(id: Value, result: Value) -> Outcome {
    Outcome::Reply(Some(jsonrpc::response(id, result)))
}

/// A tool's definition, as `tools/list` gives it: its name, its name for
/// people, a description written for a model, and its arguments' schema,
/// from `T`. A `read_only` tool changes nothing.
#[must_use]
pub fn tool_definition<T: JsonSchema>(
    name: &str,
    title: &str,
    description: &str,
    read_only: bool,
) -> Value {
    let mut schema = lspk_commands::args_schema::<T>().to_value();
    if let Some(object) = schema.as_object_mut() {
        // The tool's description says what the arguments are for.
        object.remove("title");
        object.remove("description");
    }
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": schema,
        "annotations": {
            "title": title,
            "readOnlyHint": read_only,
            "destructiveHint": !read_only,
            "idempotentHint": read_only,
            "openWorldHint": false,
        },
    })
}

/// Audio or an image as a content block for a client of `protocol`:
/// `image` blocks in every version; `audio` blocks from 2025-03-26, and an
/// embedded resource before it, at its own URI or `<server>://audio`.
fn media_block(media: &Media, protocol: Option<&str>, server: &str) -> Value {
    let data = crate::base64::encode(&media.bytes);
    if media.is_audio() && protocol.is_some_and(|v| v < AUDIO_FROM) {
        return json!({
            "type": "resource",
            "resource": {
                "uri": media.uri.clone().unwrap_or_else(|| format!("{server}://audio")),
                "mimeType": media.mime_type,
                "blob": data,
            },
        });
    }
    json!({
        "type": if media.is_audio() { "audio" } else { "image" },
        "data": data,
        "mimeType": media.mime_type,
    })
}

/// A tool's result as MCP carries it: the JSON as text for every client,
/// and as structured content for clients that read it, then any audio or
/// images, as blocks a client of `protocol` reads.
fn tool_result(
    result: Result<Value, String>,
    structured: bool,
    media: &[Media],
    protocol: Option<&str>,
    server: &str,
) -> Value {
    match result {
        Ok(value) => {
            let text = value.to_string();
            let mut content = vec![json!({ "type": "text", "text": text })];
            content.extend(media.iter().map(|m| media_block(m, protocol, server)));
            let mut result = json!({
                "content": content,
                "isError": false,
            });
            if structured && value.is_object() {
                result["structuredContent"] = value;
            }
            result
        }
        Err(message) => json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true,
        }),
    }
}

/// A new random session ID: 16 bytes from the operating system's secure
/// random source, as hexadecimal digits.
fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // A session ID only has to be unique; the token is the secret.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = nanos.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_before_2025_03_26_is_an_embedded_resource() {
        let audio = Media {
            mime_type: "audio/wav",
            bytes: b"RIFF".to_vec(),
            uri: None,
        };
        let old = media_block(&audio, Some("2024-11-05"), "demo");
        assert_eq!(old["type"], "resource");
        assert_eq!(old["resource"]["uri"], "demo://audio");
        assert_eq!(old["resource"]["blob"], "UklGRg==");
        let new = media_block(&audio, Some("2025-03-26"), "demo");
        assert_eq!(new["type"], "audio");
        let image = Media {
            mime_type: "image/png",
            ..audio
        };
        assert_eq!(
            media_block(&image, Some("2024-11-05"), "demo")["type"],
            "image"
        );
    }

    #[test]
    fn results_are_text_and_structured_content() {
        let result = tool_result(Ok(json!({ "a": 1 })), true, &[], None, "demo");
        assert_eq!(result["structuredContent"]["a"], 1);
        assert_eq!(result["isError"], false);
        let plain = tool_result(Ok(json!({ "a": 1 })), false, &[], None, "demo");
        assert!(plain.get("structuredContent").is_none());
        let failed = tool_result(Err("no".to_owned()), true, &[], None, "demo");
        assert_eq!(failed["isError"], true);
        assert_eq!(failed["content"][0]["text"], "no");
    }

    #[test]
    fn session_ids_are_random_hex() {
        let a = new_session_id();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, new_session_id());
    }
}
