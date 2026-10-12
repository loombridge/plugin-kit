//! The tools every agent gets from a kit plug-in, as an `lspk-agent`
//! [`Program`] whose host is the plug-in itself.

use std::marker::PhantomData;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use lspk_agent::permission::{Decision, Mode, Rules, decide, permit};
use lspk_agent::{Call, Media, Program, ServerInfo, ToolOutcome, tool_definition};
use lspk_commands::{CommandSpec, Permission, Registry};
use lspk_params::ParamId;
use lspk_plugin::{Host, Plugin};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::commands::{self, Edits, ParamRef, parse, person_only};
use crate::perception::{self, Job, PreviewArgs};

/// The guide's address.
pub const GUIDE_URI: &str = "lspk://guide";
/// The guide every kit plug-in serves.
pub const GUIDE: &str = include_str!("../guide.md");

const RULES: Rules<'static> = Rules {
    person_only: &person_only,
    settings: "the plug-in's agent settings",
    headless: "the plug-in",
};

/// What a finished preview hands back: its call's ticket and its result.
type Finished = (u64, Result<(Value, Vec<Media>), String>);

/// One named snapshot of the plug-in's state.
#[derive(Clone, Debug)]
struct Checkpoint {
    name: String,
    values: Vec<(ParamId, f64)>,
    own: Vec<u8>,
    saved_at: u64,
}

/// The tools, and what they keep between calls: checkpoints, and previews
/// rendering on threads of their own.
pub(crate) struct Tools<P: Plugin> {
    host: Host,
    checkpoints: Vec<Checkpoint>,
    finished_tx: mpsc::Sender<Finished>,
    finished_rx: mpsc::Receiver<Finished>,
    workers: Vec<JoinHandle<()>>,
    _plugin: PhantomData<fn() -> P>,
}

impl<P: Plugin> std::fmt::Debug for Tools<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tools")
            .field("checkpoints", &self.checkpoints.len())
            .field("previews", &self.workers.len())
            .finish_non_exhaustive()
    }
}

impl<P: Plugin> Drop for Tools<P> {
    fn drop(&mut self) {
        // A preview runs the plug-in's code: let it finish before the
        // plug-in's library can go.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Arguments of `list_commands`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    /// Only commands in this area: the part of a command's ID before the
    /// dot, such as `params`.
    #[serde(default)]
    area: Option<String>,
    /// Only commands whose ID, label or description holds every one of
    /// these words, best matches first.
    #[serde(default)]
    search: Option<String>,
}

/// Arguments of `describe_command`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DescribeArgs {
    /// The command's ID, such as `params.set`.
    command: String,
}

/// Arguments of `run`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    /// The commands to run, in order, as one step the host records: all of
    /// them or, if one fails, none.
    #[schemars(length(min = 1))]
    commands: Vec<Step>,
    /// Check the commands without changing anything: that each exists, its
    /// arguments fit its schema, the parameters it names exist, and whether
    /// running it would ask the person first.
    #[serde(default)]
    dry_run: bool,
}

/// One command in a `run` call.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Step {
    /// The command's ID, such as `params.set`.
    command: String,
    /// Its arguments, as `describe_command` gives their schema.
    #[serde(default = "empty_object")]
    #[schemars(with = "Map<String, Value>")]
    args: Value,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// Arguments of `query`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    /// Only these parameters, by ID, name or path; leave it out for every one.
    #[serde(default)]
    params: Vec<ParamRef>,
}

/// What `checkpoint` does.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// Keep the plug-in's state now under `name`, replacing any checkpoint
    /// with that name.
    Save,
    /// Put the plug-in back as checkpoint `name` has it, as one step the
    /// host records.
    Restore,
    /// List the checkpoints.
    List,
    /// Forget checkpoint `name`.
    Delete,
}

/// Arguments of `checkpoint`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckpointArgs {
    /// What to do.
    action: Action,
    /// The checkpoint's name, for `save`, `restore` and `delete`.
    #[serde(default)]
    name: Option<String>,
}

macro_rules! tool_args {
    ($args:expr) => {
        match parse($args) {
            Ok(args) => args,
            Err(message) => return Ok(ToolOutcome::Done(Err(message))),
        }
    };
}

impl<P: Plugin> Tools<P> {
    pub(crate) fn new(host: Host) -> Self {
        let (finished_tx, finished_rx) = mpsc::channel();
        Self {
            host,
            checkpoints: Vec::new(),
            finished_tx,
            finished_rx,
            workers: Vec::new(),
            _plugin: PhantomData,
        }
    }

    /// A preview that has finished, if one has.
    pub(crate) fn take_finished(&mut self) -> Option<Finished> {
        self.finished_rx.try_recv().ok()
    }

    /// What running commands of `class` comes to for an agent.
    fn decide(class: Permission, ids: &[&str]) -> Decision {
        // The plug-in has a person to ask, through its editor; autonomous
        // mode for its agents comes with that editor.
        decide(Mode::App, &RULES, || false, class, ids)
    }

    /// The live instance's values, as a snapshot.
    fn values(&self) -> Vec<(ParamId, f64)> {
        self.host.values().map_or_else(Vec::new, |values| {
            values
                .descriptors()
                .iter()
                .enumerate()
                .map(|(index, d)| (d.id, values.value_at(index)))
                .collect()
        })
    }

    fn list(plugin: &P, args: &Value) -> Result<Value, String> {
        let ListArgs { area, search } = parse(args)?;
        let registry = commands::registry(plugin).map_err(|e| e.to_string())?;
        if area.is_none() && search.is_none() {
            let mut areas = std::collections::BTreeMap::<&str, usize>::new();
            for spec in &registry {
                *areas.entry(spec.area()).or_default() += 1;
            }
            return Ok(json!({
                "commands": registry.len(),
                "areas": areas,
                "next": "Call list_commands with an `area` or `search` to list commands.",
            }));
        }
        let words: Vec<String> = search
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let mut found: Vec<(usize, &CommandSpec)> = registry
            .iter()
            .filter(|spec| area.as_deref().is_none_or(|area| spec.area() == area))
            .filter_map(|spec| {
                let (id, label) = (spec.id.to_lowercase(), spec.label.to_lowercase());
                let description = spec.description.to_lowercase();
                let mut score = 0;
                for word in &words {
                    if id.contains(word.as_str()) || label.contains(word.as_str()) {
                        score += 2;
                    } else if description.contains(word.as_str()) {
                        score += 1;
                    } else {
                        return None;
                    }
                }
                Some((score, spec))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
        Ok(json!({
            "commands": found
                .iter()
                .map(|(_, spec)| json!({
                    "id": spec.id,
                    "label": spec.label,
                    "permission": spec.permission,
                    "access": access(spec),
                    "summary": first_sentence(&spec.description),
                }))
                .collect::<Vec<_>>(),
        }))
    }

    fn describe(plugin: &P, args: &Value) -> Result<Value, String> {
        let DescribeArgs { command } = parse(args)?;
        let registry = commands::registry(plugin).map_err(|e| e.to_string())?;
        let spec = find(&registry, &command)?;
        Ok(json!({
            "id": spec.id,
            "label": spec.label,
            "description": spec.description,
            "permission": spec.permission,
            "access": access(spec),
            "args": spec.args,
            "result": spec.result,
        }))
    }

    fn run(&mut self, call: Call<'_, P>, args: &Value) -> ToolOutcome<Job> {
        let RunArgs { commands, dry_run } = match parse(args) {
            Ok(args) => args,
            Err(message) => return ToolOutcome::Done(Err(message)),
        };
        let plugin = call.host;
        let registry = match commands::registry(plugin) {
            Ok(registry) => registry,
            Err(error) => return ToolOutcome::Done(Err(error.to_string())),
        };
        // Every command exists and its arguments fit, before anything runs.
        let mut classes = Vec::new();
        for (index, step) in commands.iter().enumerate() {
            let checked = find(&registry, &step.command).and_then(|spec| {
                if let Some(schema) = &spec.args {
                    lspk_commands::validate(schema, &step.args)
                        .map_err(|e| format!("its arguments don't fit: {e}"))?;
                }
                if step.command == commands::SET
                    && let Some(values) = self.host.values()
                {
                    commands::plan(values, &parse(&step.args)?)?;
                }
                Ok(spec.permission.unwrap_or(Permission::External))
            });
            match checked {
                Ok(class) => classes.push(class),
                Err(message) => {
                    return ToolOutcome::Done(Err(format!(
                        "command {} (`{}`): {message}. Nothing changed.",
                        index + 1,
                        step.command
                    )));
                }
            }
        }
        let ids: Vec<&str> = commands.iter().map(|s| s.command.as_str()).collect();
        if dry_run {
            let class = classes
                .iter()
                .copied()
                .max()
                .unwrap_or(Permission::External);
            let decision = match Self::decide(class, &ids) {
                Decision::Run => "runs".to_owned(),
                Decision::Ask => "asks the person first".to_owned(),
                Decision::Refuse(message) => format!("refused: {message}"),
            };
            return ToolOutcome::Done(Ok(json!({
                "dry_run": true,
                "commands": commands
                    .iter()
                    .zip(&classes)
                    .map(|(s, c)| json!({ "command": s.command, "permission": c }))
                    .collect::<Vec<_>>(),
                "running_it": decision,
            })));
        }
        if let Err(outcome) = permit(
            &classes,
            call.confirmed,
            |class| Self::decide(class, &ids),
            || {
                commands
                    .iter()
                    .map(|s| {
                        let label = registry.get(&s.command).map_or("", |spec| &spec.label);
                        format!("{label} ({})", s.command)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        ) {
            return outcome;
        }
        // Run them as one step: on a failure, put back what the call
        // changed.
        let before_values = self.values();
        let before_own = plugin.save_state();
        let mut edits = Edits::new(&self.host);
        let mut results = Vec::new();
        for (index, step) in commands.iter().enumerate() {
            let result = commands::run_params(&self.host, &mut edits, &step.command, &step.args)
                .unwrap_or_else(|| {
                    plugin
                        .run_command(&step.command, &step.args)
                        .map_err(|e| e.to_string())
                });
            match result {
                Ok(value) => results.push(json!({ "command": step.command, "result": value })),
                Err(message) => {
                    for &(id, value) in &before_values {
                        let _ = edits.set(id, value);
                    }
                    if plugin.save_state() != before_own {
                        let _ = plugin.load_state(&before_own);
                        self.host.mark_dirty();
                    }
                    edits.finish();
                    return ToolOutcome::Done(Err(format!(
                        "command {} (`{}`) failed: {message}. Nothing changed.",
                        index + 1,
                        step.command
                    )));
                }
            }
        }
        edits.finish();
        ToolOutcome::Done(Ok(json!({ "results": results })))
    }

    fn query(&self, plugin: &P, args: &Value) -> Result<Value, String> {
        let QueryArgs { params } = parse(args)?;
        let values = self.host.values().ok_or("the plug-in is still starting")?;
        let states = commands::get(values, &params)?.params;
        let generated = commands::get(values, &[])?
            .params
            .iter()
            .map(|p| format!("{} {}", p.name, p.text))
            .collect::<Vec<_>>()
            .join("; ");
        let info = P::INFO;
        let kind = if info.notes.input && info.audio.inputs == 0 {
            "instrument"
        } else if info.audio.inputs > 0 {
            "audio effect"
        } else {
            "note effect"
        };
        let summary = match plugin.summary() {
            Some(own) => format!("{own}\n\nParameters: {generated}."),
            None => format!("{} ({kind}). Parameters: {generated}.", info.name),
        };
        Ok(json!({
            "plugin": {
                "id": info.id,
                "name": info.name,
                "vendor": info.vendor,
                "version": info.version,
                "description": info.description,
                "kind": kind,
            },
            "host": self.host.host_name(),
            "track": self.host.track_name(),
            "summary": summary,
            "params": states,
        }))
    }

    fn checkpoint(&mut self, plugin: &mut P, args: &Value) -> Result<Value, String> {
        let CheckpointArgs { action, name } = parse(args)?;
        let named = || {
            name.clone()
                .filter(|n| !n.trim().is_empty())
                .ok_or("give the checkpoint a `name`")
        };
        let list = |checkpoints: &[Checkpoint]| {
            checkpoints
                .iter()
                .map(|c| json!({ "name": c.name, "saved_at": c.saved_at }))
                .collect::<Vec<_>>()
        };
        match action {
            Action::Save => {
                let name = named()?;
                let checkpoint = Checkpoint {
                    name: name.clone(),
                    values: self.values(),
                    own: plugin.save_state(),
                    saved_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs()),
                };
                self.checkpoints.retain(|c| c.name != name);
                self.checkpoints.push(checkpoint);
                Ok(json!({ "saved": name, "checkpoints": list(&self.checkpoints) }))
            }
            Action::Restore => {
                let name = named()?;
                let checkpoint = self
                    .checkpoints
                    .iter()
                    .find(|c| c.name == name)
                    .cloned()
                    .ok_or_else(|| format!("no checkpoint is called `{name}`"))?;
                let mut edits = Edits::new(&self.host);
                let mut changed = 0;
                for &(id, value) in &checkpoint.values {
                    if self.host.param(id).is_some_and(|now| now != value) {
                        edits.set(id, value)?;
                        changed += 1;
                    }
                }
                edits.finish();
                if plugin.save_state() != checkpoint.own {
                    plugin
                        .load_state(&checkpoint.own)
                        .map_err(|e| e.to_string())?;
                    self.host.mark_dirty();
                }
                Ok(json!({ "restored": name, "params_changed": changed }))
            }
            Action::List => Ok(json!({ "checkpoints": list(&self.checkpoints) })),
            Action::Delete => {
                let name = named()?;
                let before = self.checkpoints.len();
                self.checkpoints.retain(|c| c.name != name);
                if self.checkpoints.len() == before {
                    return Err(format!("no checkpoint is called `{name}`"));
                }
                Ok(json!({ "deleted": name, "checkpoints": list(&self.checkpoints) }))
            }
        }
    }
}

/// Whether running a command would run, ask the person first, or be
/// refused.
fn access(spec: &CommandSpec) -> &'static str {
    let class = spec.permission.unwrap_or(Permission::External);
    match decide(Mode::App, &RULES, || false, class, &[spec.id.as_ref()]) {
        Decision::Run => "runs",
        Decision::Ask => "asks the person first",
        Decision::Refuse(_) => "refused to agents",
    }
}

fn find<'a>(registry: &'a Registry, id: &str) -> Result<&'a CommandSpec, String> {
    registry
        .get(id)
        .ok_or_else(|| format!("there's no command `{id}`; list_commands lists them"))
}

/// The first sentence of a description.
fn first_sentence(description: &str) -> &str {
    description
        .find(". ")
        .map_or(description, |end| &description[..=end])
}

impl<P: Plugin> Program for Tools<P> {
    type Host = P;
    type Event = ();
    type Deferred = Job;

    fn info(&self) -> ServerInfo {
        let info = P::INFO;
        ServerInfo {
            name: info.id.to_owned(),
            title: info.name.to_owned(),
            version: info.version.to_owned(),
            instructions: format!(
                "You're connected to one running instance of {}, a plug-in in a host, through \
                 its own agent server. Read the resource {GUIDE_URI} first. Start with `query` to \
                 see its parameters and a summary; change them with `run` and `params.set`; \
                 hear the result with `preview`; keep and restore versions with `checkpoint`.",
                info.name
            ),
        }
    }

    fn tools(&self) -> Vec<Value> {
        vec![
            tool_definition::<ListArgs>(
                "list_commands",
                "List commands",
                "Lists the plug-in's commands: every action it offers, which `run` runs. With no \
                 arguments it lists the areas (such as `params`) and how many commands each \
                 holds. With `area` or `search` it lists the matching commands: each one's ID, \
                 label, permission class, whether running it asks the person first, and the \
                 first sentence of its description.",
                true,
            ),
            tool_definition::<DescribeArgs>(
                "describe_command",
                "Describe a command",
                "Describes one command: what it does, its permission class and whether it asks \
                 the person first, and the JSON Schemas of its arguments and result. Read it \
                 before running a command for the first time.",
                true,
            ),
            tool_definition::<RunArgs>(
                "run",
                "Run commands",
                "Runs one command or several in order, as one step the host records like a \
                 person's edit, so its automation, undo and saved project follow. All or \
                 nothing: if one fails, nothing changes and the error names it. Set `dry_run` \
                 to check them without changing anything. To change parameters, run \
                 `params.set` with each new value as a number in its own units or as text such \
                 as `-6 dB`.",
                false,
            ),
            tool_definition::<QueryArgs>(
                "query",
                "Query the plug-in",
                "Reads the plug-in as it is now: what it is, the host and track it's on where \
                 the host says, a summary in prose, and every parameter with its range, default \
                 and value (as a number in its own units and as the host shows it).",
                true,
            ),
            tool_definition::<CheckpointArgs>(
                "checkpoint",
                "Checkpoints",
                "Keeps and restores named snapshots of the plug-in's state (every parameter and \
                 its own state), so you can try a variation and go back. `save` keeps the state \
                 now under `name`; `restore` puts it back as one step the host records; `list` \
                 shows them; `delete` forgets one. Checkpoints last while the plug-in runs and \
                 aren't saved with the project.",
                false,
            ),
            tool_definition::<PreviewArgs>(
                "preview",
                "Preview",
                "Hears the plug-in as it's set now: renders notes (a note, a chord or a short \
                 phrase, for an instrument) or a test signal (for an effect) through a separate \
                 copy of the plug-in, offline, so the host's playback is untouched, and returns \
                 the audio as a WAV file with an analysis: peak and RMS level, pitch, the \
                 spectrum in octave bands and its centroid, and the envelope's timing, as \
                 numbers and as a short summary.",
                true,
            ),
        ]
    }

    fn call(
        &mut self,
        call: Call<'_, P>,
        name: &str,
        args: Value,
    ) -> Result<ToolOutcome<Job>, String> {
        let done = |result: Result<Value, String>| Ok(ToolOutcome::Done(result));
        match name {
            "list_commands" => done(Self::list(call.host, &args)),
            "describe_command" => done(Self::describe(call.host, &args)),
            "run" => Ok(self.run(call, &args)),
            "query" => done(self.query(call.host, &args)),
            "checkpoint" => done(self.checkpoint(call.host, &args)),
            "preview" => {
                let preview: PreviewArgs = tool_args!(&args);
                let own = call.host.save_state();
                Ok(match Job::new::<P>(&preview, self.values(), own) {
                    Ok(job) => ToolOutcome::Defer(job),
                    Err(message) => ToolOutcome::Done(Err(message)),
                })
            }
            other => Err(format!("no tool is called `{other}`")),
        }
    }

    fn defer(&mut self, ticket: u64, job: &Job) {
        self.workers.retain(|w| !w.is_finished());
        let job = job.clone();
        let finished = self.finished_tx.clone();
        let wake = self.host.waker();
        let spawned = std::thread::Builder::new()
            .name("agent-preview".to_owned())
            .spawn(move || {
                let result = perception::render::<P>(&job);
                let _ = finished.send((ticket, result));
                if let Some(wake) = wake {
                    wake();
                }
            });
        match spawned {
            Ok(worker) => self.workers.push(worker),
            Err(error) => {
                let _ = self
                    .finished_tx
                    .send((ticket, Err(format!("the preview couldn't start: {error}"))));
            }
        }
    }

    fn resources(&self, _host: &P) -> Vec<Value> {
        vec![json!({
            "uri": GUIDE_URI,
            "name": "guide",
            "title": "Agent guide",
            "description": "How to work with a plug-in built on the plug-in kit: its tools, \
                            parameters, previews and checkpoints.",
            "mimeType": "text/markdown",
        })]
    }

    fn read_resource(&mut self, _host: &mut P, uri: &str) -> Result<Vec<Value>, String> {
        if uri != GUIDE_URI {
            return Err(format!("no resource is at `{uri}`"));
        }
        Ok(vec![json!({
            "uri": GUIDE_URI,
            "mimeType": "text/markdown",
            "text": GUIDE.replace("{name}", P::INFO.name),
        })])
    }

    fn allows(&self, _host: &P, _agent: &str) -> bool {
        // The instance's token is the person's approval.
        true
    }

    fn approve(&mut self, _host: &mut P, _client: &str) -> bool {
        true
    }
}
