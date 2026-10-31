//! A plug-in driven headless over stdio, as a command-line tool's `mcp`
//! command serves it: its tools, the permission rules with no person to
//! ask, checkpoints, and a preview that waits for its render.

use lspk_agent::base64;
use lspk_commands::{CommandSpec, Context, NoArgs, Permission, RegisterError, Registry};
use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, NotePorts, ParamValues, Plugin, PluginError,
    PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor,
};
use lspk_plugin_agent::headless::Headless;
use lspk_test::agent::decode_wav;
use serde_json::{Value, json};

const LEVEL: ParamId = ParamId(1);
const WIPE: &str = "toy.wipe";
const COMMAND: &str = "toy-cli mcp";

/// An instrument: a sine at the note's pitch, scaled by the level, with the
/// right channel at half the left's. One command of its own, which asks
/// first (it's destructive).
struct Toy {
    host: Host,
}

impl Plugin for Toy {
    type Processor = ToyProcessor;

    const INFO: PluginInfo = PluginInfo::new("com.loombridge.test.toy", "Toy")
        .audio(AudioLayout::instrument(2))
        .notes(NotePorts::INPUT);

    fn new(host: Host) -> Self {
        Self { host }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        vec![ParamDescriptor::continuous(
            LEVEL,
            "Level",
            (0.0, 1.0),
            0.5,
            ParamUnit::None,
        )]
    }

    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<ToyProcessor, PluginError> {
        Ok(ToyProcessor {
            rate: config.sample_rate,
            level: values.get(LEVEL).unwrap_or(0.5),
            ..ToyProcessor::default()
        })
    }

    fn editor(&mut self) -> Option<Box<dyn lspk_plugin::Editor>> {
        Some(Box::new(lspk_ui::generic_editor("Toy", &self.params())))
    }

    fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
        registry.register(
            CommandSpec::new(WIPE, "Wipe")
                .description("Sets the level to zero. It can't be taken back.")
                .permission(Permission::Destructive)
                .context(Context::Global)
                .args::<NoArgs>()
                .result::<()>(),
        )
    }

    fn run_command(&mut self, id: &str, _args: &Value) -> Result<Value, PluginError> {
        match id {
            WIPE => {
                self.host
                    .edit(LEVEL, 0.0)
                    .map_err(|e| PluginError::new(e.to_string()))?;
                Ok(Value::Null)
            }
            other => Err(PluginError::new(format!("no command `{other}`"))),
        }
    }
}

#[derive(Default)]
struct ToyProcessor {
    rate: f64,
    level: f64,
    phase: f64,
    step: f64,
    gain: f64,
}

impl Processor for ToyProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        _cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        // Events land at the start of the block; enough for a test.
        for event in events.iter() {
            match event {
                Event::NoteOn { note, velocity, .. } => {
                    let key = f64::from(note.key.unwrap_or(69));
                    self.step = 440.0 * 2f64.powf((key - 69.0) / 12.0) / self.rate;
                    self.gain = f64::from(velocity);
                }
                Event::NoteOff { .. } => self.gain = 0.0,
                Event::Param {
                    id: LEVEL, value, ..
                } => self.level = value,
                _ => {}
            }
        }
        for frame in 0..audio.frames() {
            let sample = (self.phase * std::f64::consts::TAU).sin() * self.gain * self.level;
            self.phase = (self.phase + self.step).fract();
            audio.channel_mut(0)[frame] = sample as f32;
            audio.channel_mut(1)[frame] = (sample * 0.5) as f32;
        }
        ProcessStatus::Continue
    }

    fn flush(&mut self, events: &Events<'_>, _cx: &mut ProcessContext<'_>) {
        for event in events.iter() {
            if let Event::Param {
                id: LEVEL, value, ..
            } = event
            {
                self.level = value;
            }
        }
    }
}

/// What an agent sent and got back over one stdio session.
struct Talk {
    replies: Vec<Value>,
}

/// Serves `calls` (tool name and arguments) to a headless Toy after
/// `initialize`, the way a client writes them to the tool's standard input.
fn talk(headless: &mut Headless<Toy>, autonomous: bool, calls: &[(&str, Value)]) -> Talk {
    let mut input = vec![
        json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ];
    for (index, (name, arguments)) in calls.iter().enumerate() {
        input.push(
            json!({ "jsonrpc": "2.0", "id": index + 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments } }),
        );
    }
    let input: String = input.iter().map(|m| format!("{m}\n")).collect();
    let mut output = Vec::new();
    headless
        .serve(COMMAND, autonomous, input.as_bytes(), &mut output)
        .expect("the session ends when the input does");
    let replies = String::from_utf8(output)
        .expect("text")
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON reply"))
        .collect();
    Talk { replies }
}

impl Talk {
    /// Call `index`'s reply (0 is the first call).
    fn reply(&self, index: usize) -> &Value {
        &self.replies[index + 1]
    }

    /// Call `index`'s structured result, which must have succeeded.
    fn ok(&self, index: usize) -> &Value {
        let result = &self.reply(index)["result"];
        assert_eq!(result["isError"], false, "{result}");
        &result["structuredContent"]
    }

    /// The text of call `index`, which must have failed.
    fn error(&self, index: usize) -> &str {
        let result = &self.reply(index)["result"];
        assert_eq!(result["isError"], true, "{result}");
        result["content"][0]["text"].as_str().expect("a message")
    }
}

fn headless() -> Headless<Toy> {
    Headless::new().expect("an instance")
}

#[test]
fn the_tools_are_the_kits_and_the_commands_leave_out_the_agent_server() {
    let mut toy = headless();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"lspk://guide"}}"#,
        "\n",
    );
    let mut output = Vec::new();
    toy.serve(COMMAND, false, input.as_bytes(), &mut output)
        .expect("served");
    let replies: Vec<Value> = String::from_utf8(output)
        .expect("text")
        .lines()
        .map(|l| serde_json::from_str(l).expect("JSON"))
        .collect();
    let tools = replies[1]["result"]["tools"].as_array().expect("tools");
    lspk_test::agent::check_tools(tools).expect("well-described tools");
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
    let guide = replies[2]["result"]["contents"][0]["text"]
        .as_str()
        .expect("the guide");
    assert!(guide.contains("Toy"), "{guide}");

    let talk = talk(
        &mut toy,
        false,
        &[
            ("list_commands", json!({ "area": "toy" })),
            ("list_commands", json!({ "area": "agent" })),
            ("describe_command", json!({ "command": "params.set" })),
        ],
    );
    let listed = &talk.ok(0)["commands"][0];
    assert_eq!(listed["id"], WIPE);
    assert_eq!(listed["access"], "refused to agents");
    assert_eq!(talk.ok(1)["commands"], json!([]));
    assert!(talk.ok(2)["args"].is_object());
}

#[test]
fn a_batch_is_all_or_nothing_and_a_dry_run_changes_nothing() {
    let mut toy = headless();
    let set = |value: f64| {
        json!({ "command": "params.set",
            "args": { "values": [ { "param": "Level", "value": value } ] } })
    };
    let talk = talk(
        &mut toy,
        false,
        &[
            ("run", json!({ "commands": [set(0.9)], "dry_run": true })),
            ("query", json!({ "params": ["Level"] })),
            (
                "run",
                json!({ "commands": [set(0.9), { "command": "nothing.here" }] }),
            ),
            ("query", json!({ "params": [1] })),
            ("run", json!({ "commands": [set(0.9), set(0.25)] })),
            ("query", json!({ "params": ["level"] })),
        ],
    );
    assert_eq!(talk.ok(0)["dry_run"], true);
    assert_eq!(talk.ok(1)["params"][0]["value"], 0.5);
    assert!(
        talk.error(2).contains("Nothing changed"),
        "{}",
        talk.error(2)
    );
    assert_eq!(talk.ok(3)["params"][0]["value"], 0.5);
    assert_eq!(talk.ok(4)["results"].as_array().map(Vec::len), Some(2));
    assert_eq!(talk.ok(5)["params"][0]["value"], 0.25);
    let summary = talk.ok(5)["summary"].as_str().expect("a summary");
    assert!(summary.contains("Level"), "{summary}");
}

#[test]
fn destructive_commands_are_refused_unless_autonomous() {
    let wipe = json!({ "commands": [ { "command": WIPE } ] });
    let calls = [("run", wipe), ("query", json!({ "params": ["Level"] }))];

    let refused = talk(&mut headless(), false, &calls);
    let message = refused.error(0);
    assert!(message.contains("`toy.wipe` is destructive"), "{message}");
    assert!(
        message.contains("`toy-cli mcp` has no person to ask"),
        "{message}"
    );
    assert!(message.contains("--autonomous"), "{message}");
    assert_eq!(refused.ok(1)["params"][0]["value"], 0.5);

    let allowed = talk(&mut headless(), true, &calls);
    allowed.ok(0);
    assert_eq!(allowed.ok(1)["params"][0]["value"], 0.0);
}

#[test]
fn a_checkpoint_puts_the_plug_in_back() {
    let set = json!({ "commands": [ { "command": "params.set",
        "args": { "values": [ { "param": "Level", "value": 1.0 } ] } } ] });
    let talk = talk(
        &mut headless(),
        false,
        &[
            ("checkpoint", json!({ "action": "save", "name": "start" })),
            ("run", set),
            (
                "checkpoint",
                json!({ "action": "restore", "name": "start" }),
            ),
            ("query", json!({ "params": ["Level"] })),
            ("checkpoint", json!({ "action": "restore", "name": "nope" })),
        ],
    );
    talk.ok(0);
    assert_eq!(talk.ok(2)["params_changed"], 1);
    assert_eq!(talk.ok(3)["params"][0]["value"], 0.5);
    assert!(talk.error(4).contains("no checkpoint is called `nope`"));
}

#[test]
fn a_preview_waits_for_its_render_and_returns_audio_and_an_analysis() {
    let talk = talk(
        &mut headless(),
        false,
        &[(
            "preview",
            json!({ "notes": [ { "key": 69, "length": 0.5, "velocity": 1.0 } ] }),
        )],
    );
    let analysis = talk.ok(0);
    assert_eq!(analysis["pitch"]["note"], "A4", "{analysis}");
    // The right channel is half the left's: a narrow image, fully correlated.
    assert_eq!(analysis["stereo"]["correlation"], 1.0, "{analysis}");
    assert_eq!(analysis["stereo"]["width"], 0.1, "{analysis}");
    let text = analysis["summary"].as_str().expect("a summary");
    assert!(text.contains("Stereo width 0.10"), "{text}");
    let blob = talk.reply(0)["result"]["content"]
        .as_array()
        .expect("content")
        .iter()
        .find_map(|block| block["data"].as_str())
        .expect("audio");
    let audio = decode_wav(&base64::decode(blob).expect("base64")).expect("a WAV file");
    assert!(audio.frames() > 0 && !audio.is_silent());
}

#[test]
fn the_interface_comes_back_as_an_image_of_the_values_the_commands_before_it_set() {
    let capture = |scale: Value| json!({ "command": "ui.capture", "args": { "scale": scale } });
    let set = json!({ "command": "params.set",
        "args": { "values": [ { "param": "Level", "value": 1.0 } ] } });
    let talk = talk(
        &mut headless(),
        false,
        &[
            ("describe_command", json!({ "command": "ui.capture" })),
            ("run", json!({ "commands": [capture(json!(1.0))] })),
            ("run", json!({ "commands": [set, capture(json!(1.0))] })),
            ("run", json!({ "commands": [capture(json!(2.0))] })),
            ("run", json!({ "commands": [capture(json!(9.0))] })),
        ],
    );
    assert_eq!(talk.ok(0)["permission"], "read");
    assert_eq!(talk.ok(0)["access"], "runs");
    assert!(talk.ok(0)["args"]["properties"]["scale"].is_object());
    let png = |index: usize| {
        let blob = talk.reply(index)["result"]["content"]
            .as_array()
            .expect("content")
            .iter()
            .find_map(|block| block["data"].as_str())
            .expect("an image");
        lspk_test::snapshot::decode_png(&base64::decode(blob).expect("base64")).expect("a PNG")
    };
    let (small, moved, big) = (png(1), png(2), png(3));
    assert_eq!((small.width, small.height), (moved.width, moved.height));
    assert_ne!(small.rgba, moved.rgba, "the Level knob moved");
    let size = &talk.ok(1)["results"][0]["result"];
    assert_eq!(
        (size["width"].as_u64(), size["height"].as_u64()),
        (Some(u64::from(small.width)), Some(u64::from(small.height)))
    );
    assert_eq!((big.width, big.height), (small.width * 2, small.height * 2));
    let background = &small.rgba[..4];
    assert!(
        small
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel != background),
        "the controls are drawn"
    );
    assert!(
        talk.error(4).contains("from 0.25 to 4"),
        "{}",
        talk.error(4)
    );
}

#[test]
fn edits_the_plug_in_makes_do_not_pile_up_with_no_processor() {
    // Each `params.set` queues three edits; the queue holds about a thousand.
    let set = |value: f64| {
        json!({ "commands": [ { "command": "params.set",
            "args": { "values": [ { "param": "Level", "value": value } ] } } ] })
    };
    let calls: Vec<(&str, Value)> = (0..600).map(|i| ("run", set(f64::from(i % 2)))).collect();
    let talk = talk(&mut headless(), false, &calls);
    for index in 0..calls.len() {
        talk.ok(index);
    }
}
