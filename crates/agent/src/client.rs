//! A small MCP client for scripts and tests: it drives the agent interface
//! the way an agent client does, over stdio (a program's command-line
//! `mcp`), over Streamable HTTP (a running program's agent server), or in
//! this process.
//!
//! A program's agent interface is its end-to-end test harness too: a
//! scripted client calls the same tools an agent does.
//!
//! ```no_run
//! use lspk_agent::client::{Client, Stdio};
//! use serde_json::json;
//!
//! let mut command = std::process::Command::new("my-synth-cli");
//! command.arg("mcp");
//! let mut client = Client::new(Stdio::spawn(&mut command)?);
//! client.initialize("my-script")?;
//! let tools = client.tools()?;
//! println!("{} tools", tools.len());
//! # Ok::<(), String>(())
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio as Pipe};
use std::time::Duration;

use serde_json::{Value, json};

use crate::jsonrpc;
use crate::server::{Credential, Outcome, PROTOCOL_VERSIONS, Program, Question, Server};

/// How a client reaches the server.
pub trait Transport {
    /// Sends a message, and for a request returns the reply.
    ///
    /// # Errors
    ///
    /// A message if the server can't be reached or its reply can't be read.
    fn send(&mut self, message: &Value) -> Result<Option<Value>, String>;
}

/// A stdio server as a child process, over its standard input and output.
#[derive(Debug)]
pub struct Stdio {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Stdio {
    /// Starts `command` (such as a command-line tool's `mcp`) with piped
    /// standard input and output. Its standard error goes to this process's.
    ///
    /// # Errors
    ///
    /// A message if it can't start.
    pub fn spawn(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Pipe::piped())
            .stdout(Pipe::piped())
            .spawn()
            .map_err(|error| format!("couldn't start the server: {error}"))?;
        let stdin = child.stdin.take().ok_or("no standard input")?;
        let stdout = child.stdout.take().ok_or("no standard output")?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        })
    }

    /// Closes the server's input, which ends it, and waits for it to exit.
    ///
    /// # Errors
    ///
    /// A message if waiting fails.
    pub fn finish(mut self) -> Result<ExitStatus, String> {
        drop(self.stdin.take());
        self.child
            .wait()
            .map_err(|error| format!("couldn't wait for the server: {error}"))
    }
}

impl Drop for Stdio {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Transport for Stdio {
    fn send(&mut self, message: &Value) -> Result<Option<Value>, String> {
        let stdin = self.stdin.as_mut().ok_or("the server's input is closed")?;
        let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
        line.push(b'\n');
        stdin
            .write_all(&line)
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("couldn't write to the server: {error}"))?;
        if !jsonrpc::is_request(message) {
            return Ok(None);
        }
        let id = &message["id"];
        loop {
            let mut reply = String::new();
            let read = self
                .stdout
                .read_line(&mut reply)
                .map_err(|error| format!("couldn't read from the server: {error}"))?;
            if read == 0 {
                return Err("the server closed its output".to_owned());
            }
            let reply: Value = serde_json::from_str(reply.trim())
                .map_err(|error| format!("the server wrote something that isn't JSON: {error}"))?;
            if reply.get("id") == Some(id) {
                return Ok(Some(reply));
            }
        }
    }
}

/// An HTTP response, as [`Http::exchange`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawResponse {
    /// The status code.
    pub status: u16,
    /// The headers, with lower-case names.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl RawResponse {
    /// A header's value.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body as JSON, or `null`.
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// A running program's agent server, over Streamable HTTP.
#[derive(Debug)]
pub struct Http {
    addr: SocketAddr,
    token: String,
    session: Option<String>,
    protocol: Option<String>,
    stream: Option<BufReader<TcpStream>>,
    timeout: Duration,
}

impl Http {
    /// A client of the server at `addr`, sending `token` as its bearer
    /// token.
    #[must_use]
    pub fn new(addr: SocketAddr, token: impl Into<String>) -> Self {
        Self {
            addr,
            token: token.into(),
            session: None,
            protocol: None,
            stream: None,
            timeout: Duration::from_secs(60),
        }
    }

    /// The session the server gave at `initialize`.
    #[must_use]
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// How long to wait for each response; a minute unless set.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The headers every request carries: the host, the token, the session
    /// and protocol version once there are any, and what it accepts.
    #[must_use]
    pub fn standard_headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            ("Host".to_owned(), format!("127.0.0.1:{}", self.addr.port())),
            ("Authorization".to_owned(), format!("Bearer {}", self.token)),
            (
                "Accept".to_owned(),
                "application/json, text/event-stream".to_owned(),
            ),
        ];
        if let Some(session) = &self.session {
            headers.push(("Mcp-Session-Id".to_owned(), session.clone()));
        }
        if let Some(protocol) = &self.protocol {
            headers.push(("MCP-Protocol-Version".to_owned(), protocol.clone()));
        }
        headers
    }

    /// Sends one HTTP request with exactly these headers (plus the body's
    /// length) and reads the response, reusing the connection when the
    /// server keeps it open.
    ///
    /// # Errors
    ///
    /// A message if the server can't be reached or answers with something
    /// that isn't HTTP.
    pub fn exchange(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<RawResponse, String> {
        let mut request = format!("{method} {path} HTTP/1.1\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        if !body.is_empty() || method == "POST" {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        let mut bytes = request.into_bytes();
        bytes.extend_from_slice(body);

        // A kept-alive connection may have closed since: try a fresh one
        // once.
        for attempt in 0..2 {
            if self.stream.is_none() {
                let stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(5))
                    .map_err(|error| format!("couldn't connect to {}: {error}", self.addr))?;
                stream
                    .set_read_timeout(Some(self.timeout))
                    .map_err(|e| e.to_string())?;
                self.stream = Some(BufReader::new(stream));
            }
            let stream = self.stream.as_mut().expect("connected");
            let written = stream
                .get_mut()
                .write_all(&bytes)
                .and_then(|()| stream.get_mut().flush());
            let result = written
                .map_err(|e| e.to_string())
                .and_then(|()| read_response(stream));
            match result {
                Ok(response) => {
                    if response
                        .header("connection")
                        .is_some_and(|c| c.eq_ignore_ascii_case("close"))
                    {
                        self.stream = None;
                    }
                    return Ok(response);
                }
                Err(error) => {
                    self.stream = None;
                    if attempt == 1 {
                        return Err(error);
                    }
                }
            }
        }
        Err("unreachable".to_owned())
    }
}

fn read_response(stream: &mut BufReader<TcpStream>) -> Result<RawResponse, String> {
    let mut line = String::new();
    loop {
        line.clear();
        if stream.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("the server closed the connection".to_owned());
        }
        let status: u16 = line
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("not an HTTP status line: {line:?}"))?;
        let mut headers = Vec::new();
        loop {
            let mut header = String::new();
            if stream.read_line(&mut header).map_err(|e| e.to_string())? == 0 {
                return Err("the server closed the connection".to_owned());
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
            }
        }
        if status == 100 {
            continue;
        }
        let length: usize = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).map_err(|e| e.to_string())?;
        return Ok(RawResponse {
            status,
            headers,
            body,
        });
    }
}

impl Transport for Http {
    fn send(&mut self, message: &Value) -> Result<Option<Value>, String> {
        let body = serde_json::to_vec(message).map_err(|e| e.to_string())?;
        let headers = self.standard_headers();
        let response = self.exchange("POST", "/mcp", &headers, &body)?;
        if message.get("method").and_then(Value::as_str) == Some("initialize")
            && let Some(session) = response.header("mcp-session-id")
        {
            self.session = Some(session.to_owned());
        }
        match response.status {
            200 => {
                let reply = response.json();
                if let Some(version) = reply
                    .pointer("/result/protocolVersion")
                    .and_then(Value::as_str)
                {
                    self.protocol = Some(version.to_owned());
                }
                Ok(Some(reply))
            }
            202 => Ok(None),
            status => Err(format!(
                "HTTP {status}: {}",
                response
                    .json()
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("no message")
            )),
        }
    }
}

/// The agent interface in this process, with its own host: for tests that
/// also drive the host between calls. Questions for the person go to
/// `answer`. Its session carries no token unless it's made with
/// [`InProcess::with_credential`], as one over HTTP carries one.
pub struct InProcess<P: Program> {
    /// The agent interface.
    pub server: Server<P>,
    /// What its commands run on.
    pub host: P::Host,
    /// Answers questions for the person: `true` allows.
    pub answer: Box<dyn FnMut(&Question) -> bool>,
    /// The questions asked so far, as the person read them.
    pub asked: Vec<String>,
    session: String,
}

impl<P: Program + std::fmt::Debug> std::fmt::Debug for InProcess<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InProcess")
            .field("server", &self.server)
            .field("asked", &self.asked)
            .finish_non_exhaustive()
    }
}

impl<P: Program> InProcess<P> {
    /// The agent interface with `host`, declining every question.
    #[must_use]
    pub fn new(server: Server<P>, host: P::Host) -> Self {
        Self::with_credential(server, host, Credential::Local)
    }

    /// The agent interface with `host`, its session's requests carrying
    /// `credential`, as an HTTP client's carry a token, declining every
    /// question.
    #[must_use]
    pub fn with_credential(mut server: Server<P>, host: P::Host, credential: Credential) -> Self {
        let session = server.open_session_as(None, credential);
        Self {
            server,
            host,
            answer: Box::new(|_| false),
            asked: Vec::new(),
            session,
        }
    }
}

// One thread can't wait for a gesture it would have to end itself: tests of
// waiting use `Server::handle` and `Server::resume`, or the HTTP server.
const WAITS: &str =
    "the call waits for the person's gesture to end, which this client can't wait for";

impl<P: Program> Transport for InProcess<P> {
    fn send(&mut self, message: &Value) -> Result<Option<Value>, String> {
        match self
            .server
            .handle(&mut self.host, &self.session, message.clone())
        {
            Outcome::Reply(reply) => Ok(reply),
            Outcome::Ask(question) => {
                self.asked.push(question.text());
                let allow = (self.answer)(&question);
                match self.server.resolve(&mut self.host, question.id, allow) {
                    Some(Outcome::Wait(_)) => Err(WAITS.to_owned()),
                    reply => Ok(reply.and_then(|outcome| match outcome {
                        Outcome::Reply(reply) => reply,
                        _ => None,
                    })),
                }
            }
            Outcome::Wait(_) => Err(WAITS.to_owned()),
        }
    }
}

/// What a tool call returned.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    /// Whether the tool failed.
    pub is_error: bool,
    /// Its text.
    pub text: String,
    /// Its structured result, when the server sent one.
    pub structured: Option<Value>,
    /// Every content block, the text's included: audio and images come
    /// after it.
    pub content: Vec<Value>,
}

impl ToolResult {
    /// The result as JSON: the structured result, or the text read as JSON.
    #[must_use]
    pub fn value(&self) -> Value {
        self.structured
            .clone()
            .or_else(|| serde_json::from_str(&self.text).ok())
            .unwrap_or_else(|| Value::String(self.text.clone()))
    }

    /// The first block of audio or an image (`type` `audio` or `image`,
    /// or an embedded resource's blob) with this media type, decoded: its
    /// bytes.
    #[must_use]
    pub fn attachment(&self, mime_type: &str) -> Option<Vec<u8>> {
        self.content.iter().find_map(|block| {
            let (data, mime) = match block["type"].as_str()? {
                "audio" | "image" => (block["data"].as_str()?, block["mimeType"].as_str()?),
                "resource" => (
                    block["resource"]["blob"].as_str()?,
                    block["resource"]["mimeType"].as_str()?,
                ),
                _ => return None,
            };
            (mime == mime_type)
                .then(|| crate::base64::decode(data))
                .flatten()
        })
    }

    /// The result, or the tool's error message.
    ///
    /// # Errors
    ///
    /// The tool's message if it failed.
    pub fn ok(self) -> Result<Value, String> {
        if self.is_error {
            Err(self.text)
        } else {
            Ok(self.value())
        }
    }
}

/// An MCP client over a [`Transport`].
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
    next_id: u64,
}

impl<T: Transport> Client<T> {
    /// A client that hasn't initialised yet.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 0,
        }
    }

    /// The transport, to reach what's behind it.
    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }

    /// The transport, giving up the client.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Initialises the session as the client called `name`, and says it's
    /// ready. Returns the server's `initialize` result.
    ///
    /// # Errors
    ///
    /// The server's error, such as the person refusing the client.
    pub fn initialize(&mut self, name: &str) -> Result<Value, String> {
        self.initialize_with(name, PROTOCOL_VERSIONS[0])
    }

    /// Initialises the session as [`Client::initialize`] does, asking for
    /// protocol version `protocol`, such as `2024-11-05`.
    ///
    /// # Errors
    ///
    /// The server's error, such as the person refusing the client.
    pub fn initialize_with(&mut self, name: &str, protocol: &str) -> Result<Value, String> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": protocol,
                "capabilities": {},
                "clientInfo": { "name": name, "version": "1.0.0" },
            }),
        )?;
        self.notify("notifications/initialized", Value::Null)?;
        Ok(result)
    }

    /// Sends a request and returns its result.
    ///
    /// # Errors
    ///
    /// The server's error message, or the transport's.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let reply = self
            .transport
            .send(&jsonrpc::request(self.next_id, method, params))?
            .ok_or("no reply to a request")?;
        if let Some(error) = reply.get("error") {
            return Err(format!(
                "{} ({})",
                error["message"].as_str().unwrap_or("an error"),
                error["code"]
            ));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Sends a notification.
    ///
    /// # Errors
    ///
    /// The transport's error.
    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.transport
            .send(&jsonrpc::notification(method, params))
            .map(|_| ())
    }

    /// The server's tools.
    ///
    /// # Errors
    ///
    /// As for [`Client::request`].
    pub fn tools(&mut self) -> Result<Vec<Value>, String> {
        let result = self.request("tools/list", json!({}))?;
        Ok(result["tools"].as_array().cloned().unwrap_or_default())
    }

    /// Calls a tool.
    ///
    /// # Errors
    ///
    /// A protocol error, such as an unknown tool. The tool's own failure
    /// comes back as a [`ToolResult`] with `is_error` set.
    pub fn call(&mut self, tool: &str, args: Value) -> Result<ToolResult, String> {
        let result = self.request("tools/call", json!({ "name": tool, "arguments": args }))?;
        Ok(ToolResult {
            is_error: result["isError"].as_bool().unwrap_or(false),
            text: result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            structured: result.get("structuredContent").cloned(),
            content: result["content"].as_array().cloned().unwrap_or_default(),
        })
    }

    /// Reads a binary resource, such as an audio preview: its media type
    /// and bytes.
    ///
    /// # Errors
    ///
    /// As for [`Client::request`], or if it has no binary contents.
    pub fn read_blob(&mut self, uri: &str) -> Result<(String, Vec<u8>), String> {
        let result = self.request("resources/read", json!({ "uri": uri }))?;
        let contents = &result["contents"][0];
        let blob = contents["blob"]
            .as_str()
            .and_then(crate::base64::decode)
            .ok_or_else(|| "the resource has no binary contents".to_owned())?;
        Ok((
            contents["mimeType"].as_str().unwrap_or_default().to_owned(),
            blob,
        ))
    }

    /// Reads a resource's text.
    ///
    /// # Errors
    ///
    /// As for [`Client::request`].
    pub fn read_resource(&mut self, uri: &str) -> Result<String, String> {
        let result = self.request("resources/read", json!({ "uri": uri }))?;
        result["contents"][0]["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "the resource has no text".to_owned())
    }
}
