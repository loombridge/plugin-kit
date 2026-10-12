//! Example Gain driven by an agent through its own agent server, over the
//! real HTTP transport, while the CLAP test host plays it: commands, a
//! parameter change the host sees as a gesture, a preview and its analysis,
//! checkpoints, token checks and discovery entries.
//!
//! One test, so this process's discovery entries are all its own.

use std::net::SocketAddr;

use lspk_agent::client::{Client, Http};
use lspk_agent::discovery::{Discovery, Entry};
use lspk_example_gain::{GAIN, ID, NAME, RESET};
use lspk_test::agent::{audio, check_tools, serving};
use lspk_test::clap::{ClapTestHost, PluginEvent};
use lspk_test::measure;
use serde_json::{Value, json};

const RATE: f64 = 48_000.0;
const FRAMES: usize = 256;

/// A new instance, active, on track `track`.
fn instance(track: &str) -> ClapTestHost {
    let mut host =
        ClapTestHost::from_entry(&lspk_example_gain::clap_entry, ID).expect("the plug-in");
    host.set_track_name(track);
    host.activate(RATE, FRAMES as u32).expect("activation");
    host
}

/// This process's entries in the discovery file.
fn entries() -> Vec<Entry> {
    let discovery = Discovery::user().expect("a per-user folder");
    discovery
        .entries()
        .expect("the discovery file")
        .into_iter()
        .filter(|e| e.pid == std::process::id() && e.id == ID)
        .collect()
}

/// Switches the instance's agent server on or off, as loading a project
/// saved that way does, and returns the entry that appeared, if one did.
fn switch(host: &mut ClapTestHost, on: bool) -> Option<Entry> {
    let before = entries();
    let state = host.save_state().expect("a state");
    let state = lspk_plugin_agent::with_server(&state, on).expect("a kit state");
    host.load_state(&state).expect("the state loads");
    entries()
        .into_iter()
        .find(|e| !before.iter().any(|b| b.instance == e.instance))
}

fn addr(entry: &Entry) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], entry.port))
}

/// Runs `calls` with a client of `entry`'s server while `host` serves.
fn with_client<T: Send + 'static>(
    host: &mut ClapTestHost,
    client: Client<Http>,
    calls: impl FnOnce(&mut Client<Http>) -> T + Send + 'static,
) -> (Client<Http>, T) {
    serving(host, move || {
        let mut client = client;
        let out = calls(&mut client);
        (client, out)
    })
}

/// A tool's result, which must succeed.
fn call(client: &mut Client<Http>, tool: &str, args: Value) -> Value {
    client
        .call(tool, args)
        .expect("a reply")
        .ok()
        .unwrap_or_else(|e| panic!("{tool} failed: {e}"))
}

/// The gestures and values for the gain in a block's events.
fn gain_events(events: &[PluginEvent]) -> Vec<PluginEvent> {
    events
        .iter()
        .copied()
        .filter(|e| match e {
            PluginEvent::GestureBegin { id, .. }
            | PluginEvent::Param { id, .. }
            | PluginEvent::GestureEnd { id, .. } => *id == GAIN.0,
            _ => false,
        })
        .collect()
}

#[test]
fn an_agent_drives_the_gain_through_its_own_server() {
    let mut host = instance("Vocals");
    // Off until the person switches it on.
    assert!(entries().is_empty());
    let entry = switch(&mut host, true).expect("an entry in the discovery file");
    assert_eq!(entry.name, NAME);
    assert_eq!(entry.host.as_deref(), Some("plug-in kit tests"));
    assert_eq!(entry.track.as_deref(), Some("Vocals"));
    assert_eq!(entry.url, format!("http://127.0.0.1:{}/mcp", entry.port));
    assert_eq!(entry.token.len(), 64);

    // The track's name follows the host.
    host.set_track_name("Lead Vocals");
    assert_eq!(entries()[0].track.as_deref(), Some("Lead Vocals"));

    let client = Client::new(Http::new(addr(&entry), entry.token.clone()));
    let (client, ()) = with_client(&mut host, client, |client| {
        let info = client.initialize("gain-test").expect("initialised");
        assert_eq!(info["serverInfo"]["name"], ID);
        let tools = client.tools().expect("tools");
        check_tools(&tools).expect("well-described tools");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "list_commands",
                "describe_command",
                "run",
                "query",
                "checkpoint",
                "preview"
            ]
        );
        let guide = client.read_resource("lspk://guide").expect("the guide");
        assert!(guide.contains("Agent guide: Example Gain"), "{guide}");
        assert!(
            client.read_resource("lspk://plugin-guide").is_err(),
            "it has no guide of its own"
        );

        let areas = call(client, "list_commands", json!({}));
        assert_eq!(
            areas["areas"],
            json!({ "agent": 2, "gain": 1, "params": 2 })
        );
        let found = call(client, "list_commands", json!({ "search": "reset" }));
        assert_eq!(found["commands"][0]["id"], RESET);
        let agent = call(client, "list_commands", json!({ "area": "agent" }));
        assert_eq!(agent["commands"][0]["access"], "refused to agents");
        let set = call(
            client,
            "describe_command",
            json!({ "command": "params.set" }),
        );
        assert_eq!(set["permission"], "edit");
        assert!(set["args"]["properties"]["values"].is_object(), "{set}");

        let state = call(client, "query", json!({}));
        assert_eq!(state["track"], "Lead Vocals");
        assert_eq!(state["params"][0]["name"], "Gain");
        assert_eq!(state["params"][0]["text"], "0.00 dB");
        let summary = state["summary"].as_str().expect("a summary");
        assert!(summary.contains("scaled by +0.00 dB"), "{summary}");

        let set = call(
            client,
            "run",
            json!({ "commands": [ { "command": "params.set",
                "args": { "values": [ { "param": "gain", "text": "-6.0206 dB" } ] } } ] }),
        );
        let value = set["results"][0]["result"]["params"][0]["value"]
            .as_f64()
            .expect("a value");
        assert!((value - 0.5).abs() < 1e-4, "{set}");
    });

    // The host saw a person's change: a gesture around the value, and the
    // project marked dirty.
    let block = host.process(FRAMES, Some(&[0.5; FRAMES]), &[]);
    let events = gain_events(&block.events);
    assert!(
        matches!(
            events.as_slice(),
            [
                PluginEvent::GestureBegin { .. },
                PluginEvent::Param { .. },
                PluginEvent::GestureEnd { .. }
            ]
        ),
        "{events:?}"
    );
    assert!(host.take_requests().dirty);
    let half = host.value(GAIN.0).expect("the gain");
    assert_eq!(host.text(GAIN.0, half).as_deref(), Some("-6.0 dB"));

    let (client, ()) = with_client(&mut host, client, |client| {
        // A dry run changes nothing; a batch with a failing command changes
        // nothing; the server's own commands are refused to agents.
        let dry = call(
            client,
            "run",
            json!({ "dry_run": true, "commands": [ { "command": RESET } ] }),
        );
        assert_eq!(dry["running_it"], "runs");
        let failed = client
            .call(
                "run",
                json!({ "commands": [
                    { "command": RESET },
                    { "command": "params.set", "args": { "values": [ { "param": "Gain", "text": "loud" } ] } } ] }),
            )
            .expect("a reply");
        assert!(
            failed.is_error && failed.text.contains("Nothing changed"),
            "{failed:?}"
        );
        let refused = client
            .call(
                "run",
                json!({ "commands": [ { "command": "agent.set_server", "args": { "on": false } } ] }),
            )
            .expect("a reply");
        assert!(
            refused.is_error && refused.text.contains("only a person"),
            "{refused:?}"
        );
        let state = call(client, "query", json!({ "params": ["Gain"] }));
        assert_eq!(state["params"][0]["text"], "-6.0 dB");

        // Checkpoints: keep −6 dB, reset with the plug-in's own command,
        // and restore.
        call(
            client,
            "checkpoint",
            json!({ "action": "save", "name": "half" }),
        );
        call(
            client,
            "run",
            json!({ "commands": [ { "command": RESET } ] }),
        );
        let state = call(client, "query", json!({}));
        assert_eq!(state["params"][0]["text"], "0.00 dB");
        let restored = call(
            client,
            "checkpoint",
            json!({ "action": "restore", "name": "half" }),
        );
        assert_eq!(restored["params_changed"], 1);
        let list = call(client, "checkpoint", json!({ "action": "list" }));
        assert_eq!(list["checkpoints"][0]["name"], "half");
    });
    let block = host.process(FRAMES, Some(&[0.5; FRAMES]), &[]);
    // The reset and the restore each reached the host as a gesture.
    let events = gain_events(&block.events);
    let begins = events
        .iter()
        .filter(|e| matches!(e, PluginEvent::GestureBegin { .. }))
        .count();
    assert_eq!(begins, 2, "{events:?}");
    assert_eq!(host.value(GAIN.0), Some(half));

    let (client, preview) = with_client(&mut host, client, |client| {
        client
            .call(
                "preview",
                json!({ "input": { "kind": "sine", "frequency": 1000.0, "level_db": -6.0 }, "seconds": 1.0 }),
            )
            .expect("a reply")
    });
    // A 1 kHz sine at −6 dBFS through −6 dB: −12 dBFS, heard and analysed.
    let analysis = preview.clone().ok().expect("a preview");
    let output = audio(&preview).expect("audio");
    assert_eq!((output.sample_rate, output.channels.len()), (RATE, 2));
    let tail = output.slice(4_800, output.frames());
    let peak = measure::db(measure::peak(tail.channel(0)));
    assert!((peak + 12.04).abs() < 0.1, "{peak}");
    let pitch = analysis["pitch"]["hz"].as_f64().expect("a pitch");
    assert!((pitch - 1_000.0).abs() < 0.5, "{analysis}");
    assert!((analysis["loudness"]["peak_dbfs"].as_f64().expect("a peak") + 12.0).abs() < 0.2);
    let summary = analysis["summary"].as_str().expect("a summary");
    assert!(summary.contains("loudest octave band 1000 Hz"), "{summary}");
    // The live instance played on untouched.
    assert_eq!(host.value(GAIN.0), Some(half));

    token_checks(&mut host, &entry);

    // A second instance in the same process has its own port and token.
    let mut second = instance("Drums");
    let other = switch(&mut second, true).expect("a second entry");
    assert_ne!(other.port, entry.port);
    assert_ne!(other.token, entry.token);
    assert_eq!(other.track.as_deref(), Some("Drums"));
    assert_eq!(entries().len(), 2);
    let mut wrong = Client::new(Http::new(addr(&other), entry.token.clone()));
    let refused = serving(&mut second, move || wrong.initialize("wrong-token"));
    assert!(refused.is_err_and(|e| e.contains("401")));
    let client2 = Client::new(Http::new(addr(&other), other.token.clone()));
    let (_, state) = with_client(&mut second, client2, |client| {
        client.initialize("second").expect("initialised");
        call(client, "query", json!({}))
    });
    assert_eq!(state["track"], "Drums");
    // Its entry goes when the instance does.
    drop(second);
    assert_eq!(entries().len(), 1);

    // Switched off, the server stops and the entry goes.
    drop(client);
    assert!(switch(&mut host, false).is_none());
    assert!(entries().is_empty());
    assert!(std::net::TcpStream::connect(addr(&entry)).is_err());
}

/// Every request must carry the instance's token, name this machine as its
/// host, and come from no web page.
fn token_checks(host: &mut ClapTestHost, entry: &Entry) {
    let (address, token) = (addr(entry), entry.token.clone());
    let statuses = serving(host, move || {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let mut http = Http::new(address, token.clone());
        let mut send = |headers: Vec<(String, String)>| {
            http.exchange("POST", "/mcp", &headers, body)
                .expect("a response")
                .status
        };
        let host = ("Host".to_owned(), format!("127.0.0.1:{}", address.port()));
        let bearer = |t: &str| ("Authorization".to_owned(), format!("Bearer {t}"));
        [
            send(vec![host.clone()]),
            send(vec![host.clone(), bearer("not-the-token")]),
            send(vec![host.clone(), bearer(&token[..63])]),
            send(vec![
                (
                    "Host".to_owned(),
                    format!("evil.example:{}", address.port()),
                ),
                bearer(&token),
            ]),
            send(vec![
                host.clone(),
                bearer(&token),
                ("Origin".to_owned(), "https://evil.example".to_owned()),
            ]),
            // With the token, from no web page, the request reaches the
            // server, which wants a session first.
            send(vec![host, bearer(&token)]),
        ]
    });
    assert_eq!(statuses, [401, 401, 401, 403, 403, 400]);
}
