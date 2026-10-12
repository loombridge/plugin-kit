//! {{Name}} driven by an agent through its own agent server, over the real
//! HTTP transport, while the CLAP test host plays it: its tools, its own
//! command, a parameter change the host sees as a gesture, a note previewed
//! and analysed, and checkpoints.
//!
//! One test, so this process's discovery entries are all its own.

use std::net::SocketAddr;

use lspk_agent::client::{Client, Http};
use lspk_agent::discovery::{Discovery, Entry};
use lspk_test::agent::{audio, check_tools, serving};
use lspk_test::clap::{ClapTestHost, PluginEvent};
use serde_json::{Value, json};
use {{crate_lib}}::{ID, LEVEL, RESET};

const RATE: f64 = 48_000.0;
const FRAMES: usize = 256;

/// A tool's result, which must succeed.
fn call(client: &mut Client<Http>, tool: &str, args: Value) -> Value {
    client
        .call(tool, args)
        .expect("a reply")
        .ok()
        .unwrap_or_else(|e| panic!("{tool} failed: {e}"))
}

/// The level's gestures and values in a block's events.
fn level_events(events: &[PluginEvent]) -> Vec<PluginEvent> {
    events
        .iter()
        .copied()
        .filter(|e| match e {
            PluginEvent::GestureBegin { id, .. }
            | PluginEvent::Param { id, .. }
            | PluginEvent::GestureEnd { id, .. } => *id == LEVEL.0,
            _ => false,
        })
        .collect()
}

#[test]
fn an_agent_plays_it_through_its_own_server() {
    let mut host = ClapTestHost::from_entry(&{{crate_lib}}::clap_entry, ID).expect("the plug-in");
    host.activate(RATE, FRAMES as u32).expect("activation");
    // Off until the person switches it on, as loading a project saved that
    // way does here.
    let state = host.save_state().expect("a state");
    let on = lspk_plugin_agent::with_server(&state, true).expect("a kit state");
    host.load_state(&on).expect("the state loads");
    let entry: Entry = Discovery::user()
        .expect("a per-user folder")
        .entries()
        .expect("the discovery file")
        .into_iter()
        .find(|e| e.pid == std::process::id() && e.id == ID)
        .expect("an entry");

    let addr = SocketAddr::from(([127, 0, 0, 1], entry.port));
    let token = entry.token.clone();
    let (mut client, ()) = serving(&mut host, move || {
        let mut client = Client::new(Http::new(addr, token));
        client.initialize("{{crate}}-test").expect("initialised");
        check_tools(&client.tools().expect("tools")).expect("well-described tools");
        let found = call(&mut client, "list_commands", json!({ "search": "reset" }));
        assert_eq!(found["commands"][0]["id"], RESET);
        let state = call(&mut client, "query", json!({}));
        assert_eq!(state["plugin"]["kind"], "instrument");
        assert_eq!(state["params"][0]["text"], "-6.0 dB");
        let summary = state["summary"].as_str().expect("a summary");
        assert!(summary.contains("A polyphonic instrument"), "{summary}");
        call(
            &mut client,
            "checkpoint",
            json!({ "action": "save", "name": "start" }),
        );
        let set = call(
            &mut client,
            "run",
            json!({ "commands": [ { "command": "params.set",
                "args": { "values": [ { "param": "Level", "value": 1.0 } ] } } ] }),
        );
        assert_eq!(set["results"][0]["result"]["params"][0]["text"], "0.00 dB");
        (client, ())
    });

    // The host saw the change as a person's: a gesture around the value.
    let block = host.process(FRAMES, None, &[]);
    let events = level_events(&block.events);
    assert!(
        matches!(
            events.as_slice(),
            [
                PluginEvent::GestureBegin { .. },
                PluginEvent::Param { value, .. },
                PluginEvent::GestureEnd { .. }
            ] if *value == 1.0
        ),
        "{events:?}"
    );
    assert!(host.take_requests().dirty);

    // A note: A4 at full level and velocity, heard and analysed.
    let (back, note) = serving(&mut host, move || {
        let preview = client
            .call(
                "preview",
                json!({ "notes": [ { "key": 69, "length": 0.5, "velocity": 1.0 } ] }),
            )
            .expect("a reply");
        (client, preview)
    });
    client = back;
    let analysis = note.clone().ok().expect("a preview");
    assert_eq!(analysis["pitch"]["note"], "A4", "{analysis}");
    assert!((analysis["pitch"]["hz"].as_f64().expect("a pitch") - 440.0).abs() < 0.1);
    assert!(audio(&note).expect("audio").frames() > 0);

    // Its own command puts the level back to its default, and a checkpoint
    // restores what was saved.
    let (_, (reset, restored)) = serving(&mut host, move || {
        call(
            &mut client,
            "run",
            json!({ "commands": [ { "command": RESET } ] }),
        );
        let reset = call(&mut client, "query", json!({}));
        let restored = call(
            &mut client,
            "checkpoint",
            json!({ "action": "restore", "name": "start" }),
        );
        (client, (reset, restored))
    });
    assert_eq!(reset["params"][0]["text"], "-6.0 dB");
    assert_eq!(restored["params_changed"], 0);
    let block = host.process(FRAMES, None, &[]);
    assert!(!level_events(&block.events).is_empty());
}
