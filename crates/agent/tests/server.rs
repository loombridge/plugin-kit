//! The server, its transports and its client, with a toy program: a gain a
//! tool reads and sets, a reset that asks first, and a snapshot that waits
//! for the host.

use std::sync::Arc;
use std::time::Duration;

use lspk_agent::client::{Client, Http, InProcess, Transport};
use lspk_agent::http::{Endpoint, HttpConfig, HttpServer, Served, resume, serve};
use lspk_agent::permission::{Decision, Mode, Rules, decide, permit};
use lspk_agent::{
    Call, Credential, Media, Outcome, Program, QuestionKind, Server, ServerInfo, ToolOutcome,
    tool_definition,
};
use lspk_commands::Permission;
use serde_json::{Value, json};

/// What the toy program's commands run on.
#[derive(Debug, Default)]
struct Host {
    gain: f64,
    dragging: bool,
    allowed: Vec<String>,
    autonomous: Vec<String>,
    events: Vec<&'static str>,
}

#[derive(Debug, Default)]
struct Toy {
    snapshots: Vec<u64>,
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
#[allow(dead_code)]
struct SetArgs {
    db: f64,
}

#[derive(schemars::JsonSchema)]
struct NoArgs {}

impl Program for Toy {
    type Host = Host;
    type Event = &'static str;
    type Deferred = &'static str;

    fn info(&self) -> ServerInfo {
        ServerInfo {
            name: "toy".to_owned(),
            title: "Toy".to_owned(),
            version: "0.1.0".to_owned(),
            instructions: "Set the gain.".to_owned(),
        }
    }

    fn tools(&self) -> Vec<Value> {
        vec![
            tool_definition::<SetArgs>("set_gain", "Set gain", "Sets the gain in dB.", false),
            tool_definition::<NoArgs>("reset", "Reset", "Resets everything.", false),
            tool_definition::<NoArgs>("snapshot", "Snapshot", "Draws a picture.", true),
        ]
    }

    fn call(
        &mut self,
        call: Call<'_, Host>,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome<&'static str>, String> {
        let rules = Rules {
            person_only: &|id| id.starts_with("agent."),
            settings: "Toy's settings",
            headless: "toy mcp",
        };
        let agent = call.session.agent.clone();
        let host = &*call.host;
        let decide = |class| {
            decide(
                call.mode,
                &rules,
                || agent.as_ref().is_some_and(|a| host.autonomous.contains(a)),
                class,
                &[name],
            )
        };
        Ok(match name {
            "set_gain" => {
                if let Err(outcome) =
                    permit(&[Permission::Edit], call.confirmed, decide, String::new)
                {
                    return Ok(outcome);
                }
                call.host.gain = args["db"].as_f64().unwrap_or_default();
                ToolOutcome::Done(Ok(json!({ "db": call.host.gain })))
            }
            "reset" => {
                if let Err(outcome) =
                    permit(&[Permission::Destructive], call.confirmed, decide, || {
                        "Reset (destructive)".to_owned()
                    })
                {
                    return Ok(outcome);
                }
                call.host.gain = 0.0;
                ToolOutcome::Done(Ok(json!({ "db": 0.0 })))
            }
            "snapshot" => ToolOutcome::Defer("picture"),
            "sound" => ToolOutcome::WithMedia(
                json!({ "seconds": 1 }),
                vec![Media {
                    mime_type: "audio/wav",
                    bytes: b"RIFF".to_vec(),
                    uri: None,
                }],
            ),
            other => return Err(format!("no tool is called `{other}`")),
        })
    }

    fn read_resource(&mut self, _: &mut Host, uri: &str) -> Result<Vec<Value>, String> {
        (uri == "toy://guide")
            .then(|| vec![json!({ "uri": uri, "mimeType": "text/plain", "text": "Hi." })])
            .ok_or_else(|| format!("nothing at {uri}"))
    }

    fn gesture_under_way(&self, host: &Host) -> bool {
        host.dragging
    }

    fn on_event(&mut self, host: &mut Host, event: &'static str) {
        host.events.push(event);
    }

    fn defer(&mut self, ticket: u64, _: &&'static str) {
        self.snapshots.push(ticket);
    }

    fn allows(&self, host: &Host, agent: &str) -> bool {
        host.allowed.iter().any(|a| a == agent)
    }

    fn approve(&mut self, host: &mut Host, client: &str) -> bool {
        host.allowed.push(client.to_owned());
        true
    }
}

fn headless(autonomous: bool) -> Client<InProcess<Toy>> {
    let server = Server::new(Mode::Headless { autonomous }, Toy::default());
    Client::new(InProcess::new(server, Host::default()))
}

#[test]
fn a_headless_server_runs_tools_and_refuses_what_asks_first() {
    let mut client = headless(false);
    let init = client.initialize("test").unwrap();
    assert_eq!(init["serverInfo"]["name"], "toy");
    assert_eq!(init["instructions"], "Set the gain.");
    let tools = client.tools().unwrap();
    assert_eq!(tools.len(), 3);
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["db"]["type"],
        "number"
    );
    let set = client.call("set_gain", json!({ "db": -3.0 })).unwrap();
    assert_eq!(set.ok().unwrap()["db"], -3.0);
    let reset = client.call("reset", json!({})).unwrap();
    assert!(reset.is_error);
    assert!(
        reset.text.contains("`toy mcp` has no person to ask"),
        "{}",
        reset.text
    );
    assert_eq!(client.transport().host.gain, -3.0);
    assert!(
        client
            .call("nothing", json!({}))
            .unwrap_err()
            .contains("-32602")
    );
    assert_eq!(client.read_resource("toy://guide").unwrap(), "Hi.");
    assert!(
        client
            .read_resource("toy://else")
            .unwrap_err()
            .contains("-32002")
    );

    let mut free = headless(true);
    free.initialize("test").unwrap();
    assert!(!free.call("reset", json!({})).unwrap().is_error);
}

#[test]
fn audio_goes_in_a_resource_for_old_clients() {
    let mut client = headless(false);
    client.initialize_with("old", "2024-11-05").unwrap();
    let sound = client.call("sound", json!({})).unwrap();
    assert!(sound.structured.is_none());
    assert_eq!(sound.content[1]["resource"]["uri"], "toy://audio");
    assert_eq!(sound.attachment("audio/wav").unwrap(), b"RIFF");
}

#[test]
fn the_person_allows_agents_and_confirms_what_asks_first() {
    let server = Server::new(Mode::App, Toy::default());
    let mut client = Client::new(InProcess::new(server, Host::default()));
    client.transport().answer = Box::new(|q| matches!(q.kind, QuestionKind::Connect { .. }));
    client.initialize("claude").unwrap();
    assert_eq!(client.transport().host.allowed, ["claude"]);
    let reset = client.call("reset", json!({})).unwrap();
    assert!(
        reset.is_error && reset.text.contains("declined"),
        "{}",
        reset.text
    );
    let asked = &client.transport().asked;
    assert!(asked[0].contains("wants to control Toy"), "{}", asked[0]);
    assert!(
        asked[1].contains("wants to run destructive command:"),
        "{}",
        asked[1]
    );

    client.transport().answer = Box::new(|_| true);
    client.transport().host.gain = -6.0;
    assert!(!client.call("reset", json!({})).unwrap().is_error);
    assert_eq!(client.transport().host.gain, 0.0);

    // Autonomous agents don't ask.
    client.transport().answer = Box::new(|_| false);
    client.transport().host.autonomous.push("claude".to_owned());
    assert!(!client.call("reset", json!({})).unwrap().is_error);

    // Forgetting an agent ends its sessions.
    client.transport().host.allowed.clear();
    assert!(
        client
            .tools()
            .unwrap_err()
            .contains("withdrew claude's approval")
    );
}

#[test]
fn a_token_speaks_for_its_agent() {
    let mut server = Server::new(Mode::App, Toy::default());
    let mut host = Host::default();
    let session = server.open_session_as(None, Credential::Agent("editor".to_owned()));
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "clientInfo": { "name": "someone" } } });
    let Outcome::Reply(Some(refused)) = server.handle(&mut host, &session, init.clone()) else {
        panic!("a token of no allowed agent is refused");
    };
    assert_eq!(refused["error"]["code"], -32_001);
    host.allowed.push("editor".to_owned());
    let session = server.open_session_as(None, Credential::Agent("editor".to_owned()));
    let Outcome::Reply(Some(_)) = server.handle(&mut host, &session, init) else {
        panic!("an allowed agent's token isn't asked about");
    };
    let (_, open) = server.sessions().find(|(id, _)| *id == session).unwrap();
    assert_eq!(open.client.name, "editor");
    assert_eq!(open.agent.as_deref(), Some("editor"));
}

#[test]
fn calls_and_events_wait_for_the_gesture_and_the_host() {
    let mut server = Server::new(Mode::Headless { autonomous: false }, Toy::default());
    let mut host = Host::default();
    let session = server.open_session(None);
    let call = |id: u64, name: &str, args: Value| {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args } })
    };
    host.dragging = true;
    let Outcome::Wait(first) = server.handle(
        &mut host,
        &session,
        call(1, "set_gain", json!({ "db": -1.0 })),
    ) else {
        panic!("a call during a gesture waits");
    };
    server.on_event(&mut host, "fired");
    assert!(server.holds_calls());
    assert!(server.resume(&mut host).is_none());
    host.dragging = false;
    let (ticket, Outcome::Reply(Some(reply))) = server.resume(&mut host).unwrap() else {
        panic!("the call runs once the gesture ends");
    };
    assert_eq!(
        (ticket, reply["result"]["isError"].clone()),
        (first, json!(false))
    );
    assert!(server.resume(&mut host).is_none());
    assert_eq!(host.events, ["fired"]);

    let Outcome::Wait(ticket) = server.handle(&mut host, &session, call(2, "snapshot", json!({})))
    else {
        panic!("a deferred call waits");
    };
    assert_eq!(server.program().snapshots, [ticket]);
    assert_eq!(server.deferred(), [ticket]);
    server.finish_deferred(ticket, |asked| {
        Ok((
            json!({ "of": asked, "list": [1, 2] }),
            vec![Media {
                mime_type: "image/png",
                bytes: vec![1, 2, 3],
                uri: None,
            }],
        ))
    });
    let (answered, Outcome::Reply(Some(reply))) = server.resume(&mut host).unwrap() else {
        panic!("the host's answer comes back");
    };
    assert_eq!(answered, ticket);
    assert_eq!(reply["result"]["structuredContent"]["of"], "picture");
    // The text block is compact JSON holding the same value as `structuredContent`.
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains(['\n', ' ']), "compact: {text}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text).unwrap(),
        reply["result"]["structuredContent"]
    );
    assert_eq!(reply["result"]["content"][1]["type"], "image");
    assert!(!server.holds_calls());
}

#[test]
fn stdio_serves_one_session() {
    let mut server = Server::new(Mode::Headless { autonomous: false }, Toy::default());
    let mut host = Host::default();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        "not json\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"set_gain","arguments":{"db":-2}}}"#,
        "\n",
    );
    let mut output = Vec::new();
    lspk_agent::stdio::serve(&mut server, &mut host, input.as_bytes(), &mut output).unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[1]["error"]["code"], -32_700);
    assert_eq!(host.gain, -2.0);
}

fn endpoint() -> Endpoint {
    Endpoint {
        path: "/mcp".to_owned(),
        realm: "toy".to_owned(),
        token_location: "Toy's settings".to_owned(),
    }
}

/// Serves the HTTP server's jobs on this thread while `client` talks to it
/// on another, answering every question with `allow`.
fn exchange<T: Send + 'static>(
    http: &HttpServer,
    server: &mut Server<Toy>,
    host: &mut Host,
    allow: bool,
    client: impl FnOnce() -> T + Send + 'static,
) -> T {
    let talking = std::thread::spawn(client);
    let mut parked = Vec::new();
    while !talking.is_finished() {
        if let Some(job) = http.wait_job(Duration::from_millis(10)) {
            match serve(server, host, job) {
                Served::Done => {}
                Served::Waiting(pending) => pending.finish(server, host, allow),
                Served::Parked(waiting) => parked.push(waiting),
            }
        }
        while resume(server, host, &mut parked).is_some() {}
    }
    talking.join().unwrap()
}

#[test]
fn http_checks_every_request_and_serves_sessions() {
    let config = HttpConfig {
        port: 0,
        token: "server-token".to_owned(),
    };
    assert!(
        HttpServer::start(
            &HttpConfig {
                port: 0,
                token: String::new()
            },
            endpoint(),
            Arc::new(|| {})
        )
        .is_err()
    );
    let http = HttpServer::start(&config, endpoint(), Arc::new(|| {})).unwrap();
    http.set_agent_tokens([
        ("editor".to_owned(), "editor-token".to_owned()),
        ("empty".to_owned(), String::new()),
    ]);
    let addr = http.local_addr();
    assert_eq!(http.url(), format!("http://{addr}/mcp"));
    let mut server = Server::new(Mode::App, Toy::default());
    let mut host = Host::default();

    let statuses = exchange(&http, &mut server, &mut host, true, move || {
        let mut raw = Http::new(addr, "server-token");
        let mut headers = raw.standard_headers();
        let status = |raw: &mut Http, headers: &[(String, String)], path: &str| {
            raw.exchange("POST", path, headers, b"{}").unwrap()
        };
        let wrong_path = status(&mut raw, &headers, "/other");
        headers.push(("Origin".to_owned(), "https://example.com".to_owned()));
        let origin = status(&mut raw, &headers, "/mcp");
        headers.pop();
        headers[0].1 = "evil.example:80".to_owned();
        let host_header = status(&mut raw, &headers, "/mcp");
        let mut bad_token = Http::new(addr, "nope");
        let bad_headers = bad_token.standard_headers();
        let no_token = status(&mut bad_token, &bad_headers, "/mcp");
        let get = raw
            .exchange(
                "GET",
                "/mcp",
                &Http::new(addr, "server-token").standard_headers(),
                b"",
            )
            .unwrap();
        vec![
            (
                wrong_path.status,
                wrong_path.json()["error"]["message"].clone(),
            ),
            (origin.status, Value::Null),
            (host_header.status, Value::Null),
            (no_token.status, json!(no_token.header("www-authenticate"))),
            (get.status, json!(get.header("allow"))),
        ]
    });
    assert_eq!(statuses[0], (404, json!("the agent API is at /mcp")));
    assert_eq!(statuses[1].0, 403);
    assert_eq!(statuses[2].0, 403);
    assert_eq!(statuses[3], (401, json!("Bearer realm=\"toy\"")));
    assert_eq!(statuses[4], (405, json!("POST, DELETE")));

    // The server's token is asked about; allowed, the agent can work.
    let gain = exchange(&http, &mut server, &mut host, true, move || {
        let mut client = Client::new(Http::new(addr, "server-token"));
        client.initialize("claude").unwrap();
        let gain = client
            .call("set_gain", json!({ "db": -4.0 }))
            .unwrap()
            .ok()
            .unwrap();
        let session = client.transport().session().unwrap().to_owned();
        let mut other = Http::new(addr, "editor-token");
        let mut headers = other.standard_headers();
        headers.push(("Mcp-Session-Id".to_owned(), session));
        let stolen = other
            .exchange(
                "POST",
                "/mcp",
                &headers,
                br#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#,
            )
            .unwrap();
        (gain, stolen.status)
    });
    assert_eq!(gain, (json!({ "db": -4.0 }), 404));
    assert_eq!(host.allowed, ["claude"]);
    assert_eq!(host.gain, -4.0);

    // An agent's own token needs no question.
    let reply = exchange(&http, &mut server, &mut host, false, move || {
        let mut client = Client::new(Http::new(addr, "editor-token"));
        client.initialize("whoever")
    });
    assert!(reply.unwrap_err().contains("no agent the person allows"));
    host.allowed.push("editor".to_owned());
    let reply = exchange(&http, &mut server, &mut host, false, move || {
        let mut client = Client::new(Http::new(addr, "editor-token"));
        client.initialize("whoever")?;
        client
            .transport()
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
    });
    assert_eq!(reply, Ok(None));
    http.stop();
}

#[test]
fn rules_and_decisions_are_the_programs() {
    let rules = Rules {
        person_only: &|_| false,
        settings: "",
        headless: "toy mcp",
    };
    assert_eq!(
        decide(Mode::App, &rules, || false, Permission::External, &["a.b"]),
        Decision::Ask
    );
}
