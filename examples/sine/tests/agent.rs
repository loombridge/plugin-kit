//! Example Sine driven by an agent through its own agent server, over the
//! real HTTP transport, while the CLAP test host plays it: commands, a
//! parameter change the host sees as a gesture, previews of a note and a
//! chord with their analysis, and checkpoints.
//!
//! One test, so this process's discovery entries are all its own.

use std::net::SocketAddr;

use lspk_agent::client::{Client, Http};
use lspk_agent::discovery::{Discovery, Entry};
use lspk_example_sine::{ID, LEVEL};
use lspk_test::agent::{audio, check_tools, serving};
use lspk_test::clap::{ClapTestHost, PluginEvent};
use lspk_test::measure;
use serde_json::{Value, json};

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
fn an_agent_plays_the_sine_through_its_own_server() {
    let mut host =
        ClapTestHost::from_entry(&lspk_example_sine::clap_entry, ID).expect("the plug-in");
    host.activate(RATE, FRAMES as u32).expect("activation");
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
    // The test host reports no track for this instance.
    assert_eq!(entry.track, None);

    let addr = SocketAddr::from(([127, 0, 0, 1], entry.port));
    let token = entry.token.clone();
    let (mut client, ()) = serving(&mut host, move || {
        let mut client = Client::new(Http::new(addr, token));
        client.initialize("sine-test").expect("initialised");
        check_tools(&client.tools().expect("tools")).expect("well-described tools");
        // Its own guide, beside the kit's.
        let resources = client
            .request("resources/list", json!({}))
            .expect("the resources");
        assert_eq!(resources["resources"][1]["uri"], "lspk://plugin-guide");
        let guide = client
            .read_resource("lspk://plugin-guide")
            .expect("its guide");
        assert!(guide.contains("a pure sine"), "{guide}");
        let areas = call(&mut client, "list_commands", json!({}));
        assert_eq!(areas["areas"], json!({ "agent": 2, "params": 2 }));
        let state = call(&mut client, "query", json!({}));
        assert_eq!(state["plugin"]["kind"], "instrument");
        assert_eq!(state["params"][0]["text"], "-6.0 dB");
        let summary = state["summary"].as_str().expect("a summary");
        assert!(
            summary.contains("A polyphonic sine instrument"),
            "{summary}"
        );
        call(
            &mut client,
            "checkpoint",
            json!({ "action": "save", "name": "start" }),
        );
        let set = call(
            &mut client,
            "run",
            json!({ "commands": [ { "command": "params.set",
                "args": { "values": [ { "param": LEVEL.0, "value": 1.0 } ] } } ] }),
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

    // A note: A4 at full level and velocity 1, heard and analysed.
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
    let release = analysis["envelope"]["release_s"]
        .as_f64()
        .expect("a release");
    assert!(release > 0.05 && release < 0.15, "{analysis}");
    let output = audio(&note).expect("audio");
    assert_eq!(output.frames(), 72_000);
    // Sustain at 0.7 of the voice's 0.125 peak.
    let sustain = output.slice(9_600, 19_200);
    let level = measure::peak(sustain.channel(0));
    assert!((level - 0.7 * 0.125).abs() < 1e-3, "{level}");

    // A chord, and then back to where it started.
    let (_, (chord, restored)) = serving(&mut host, move || {
        let chord = call(
            &mut client,
            "preview",
            json!({ "notes": [ { "key": 60 }, { "key": 64 }, { "key": 67 } ], "seconds": 1.5 }),
        );
        let restored = call(
            &mut client,
            "checkpoint",
            json!({ "action": "restore", "name": "start" }),
        );
        (client, (chord, restored))
    });
    let summary = chord["summary"].as_str().expect("a summary");
    assert!(summary.contains("1.50 s at 48000 Hz, stereo"), "{summary}");
    assert!(chord["loudness"]["peak_dbfs"].as_f64().expect("a peak") < -6.0);
    assert_eq!(restored["params_changed"], 1);
    let block = host.process(FRAMES, None, &[]);
    assert_eq!(level_events(&block.events).len(), 3);
    let level = host.value(LEVEL.0).expect("the level");
    assert_eq!(host.text(LEVEL.0, level).as_deref(), Some("-6.0 dB"));
}
