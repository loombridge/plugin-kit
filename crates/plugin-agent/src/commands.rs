//! The commands every kit plug-in has: its parameters (`params.get`,
//! `params.set`) and its agent server (`agent.set_server`,
//! `agent.server_status`), registered beside the plug-in's own.
//!
//! Parameter changes reach the host exactly as a person's would: through
//! [`Host`]'s gestures, a begin, the value and an end, so the host's
//! automation, undo and saved project stay in step. Every change one call
//! makes ([`Edits`]) opens its parameters' gestures as it first touches each
//! and closes them all at the end, so the host takes the call as one step.

use lspk_commands::{CommandSpec, Context, NoArgs, Permission, RegisterError, Registry};
use lspk_params::{ParamDescriptor, ParamId, ParamKind};
use lspk_plugin::{Host, ParamValues, Plugin};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reads parameters.
pub const GET: &str = "params.get";
/// Sets parameters.
pub const SET: &str = "params.set";
/// Switches the agent server on or off.
pub const SET_SERVER: &str = "agent.set_server";
/// Says whether the agent server is on, and how to reach it.
pub const SERVER_STATUS: &str = "agent.server_status";

/// Whether a command changes what agents may do, so only a person may run
/// it: the agent server's own commands.
#[must_use]
pub fn person_only(id: &str) -> bool {
    id.starts_with("agent.")
}

/// A parameter, by its ID or its name.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ParamRef {
    /// Its ID, which never changes.
    Id(u32),
    /// Its name, as the host shows it, in any case.
    Name(String),
}

/// Arguments of `params.get`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    /// The parameters to read, by ID or name; leave it out for every one.
    #[serde(default)]
    pub params: Vec<ParamRef>,
}

/// Arguments of `params.set`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArgs {
    /// The new values, applied together as one step.
    #[schemars(length(min = 1))]
    pub values: Vec<NewValue>,
}

/// One parameter's new value, as a number in its own units or as text.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewValue {
    /// The parameter, by ID or name.
    pub param: ParamRef,
    /// The value in the parameter's own units, as `params.get` gives it: a
    /// gain as a linear factor (1 is 0 dB), a toggle as 0 or 1, a choice as
    /// its index from 0. Clamped to its range.
    #[serde(default)]
    pub value: Option<f64>,
    /// The value as text, as the host shows it, such as `-6 dB`, `440 Hz`
    /// or a choice's name. Give `value` or `text`, not both.
    #[serde(default)]
    pub text: Option<String>,
}

/// One parameter as `params.get` gives it.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ParamState {
    /// Its ID, which never changes.
    pub id: u32,
    /// Its name.
    pub name: String,
    /// What it does.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What kind of value it takes, and its range.
    pub kind: ParamKind,
    /// Its lowest value, in its own units.
    pub min: f64,
    /// Its highest value, in its own units.
    pub max: f64,
    /// Its default, in its own units.
    pub default: f64,
    /// Its value now, in its own units.
    pub value: f64,
    /// Its value now, as the host shows it.
    pub text: String,
}

impl ParamState {
    /// `descriptor` at `value`.
    #[must_use]
    pub fn new(descriptor: &ParamDescriptor, value: f64) -> Self {
        Self {
            id: descriptor.id.0,
            name: descriptor.name.clone(),
            description: descriptor.description.clone(),
            kind: descriptor.kind.clone(),
            min: descriptor.min(),
            max: descriptor.max(),
            default: descriptor.default,
            value,
            text: descriptor.format(value),
        }
    }
}

/// What `params.get` and `params.set` return.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ParamsResult {
    /// The parameters, in the plug-in's order (for `params.set`, those it
    /// set, with their new values).
    pub params: Vec<ParamState>,
}

/// Arguments of `agent.set_server`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetServerArgs {
    /// Whether the agent server is on.
    pub on: bool,
}

/// The agent server's state, as `agent.server_status` gives it to the
/// person.
#[derive(Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ServerStatus {
    /// The person has switched it on.
    pub on: bool,
    /// It's listening.
    pub running: bool,
    /// Where agents connect, while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The bearer token agents connect with, while it runs: new each time
    /// it starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The discovery file this instance's entry is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovery: Option<String>,
    /// Why it isn't running, or isn't in the discovery file, if something
    /// went wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl std::fmt::Debug for ServerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token.
        f.debug_struct("ServerStatus")
            .field("on", &self.on)
            .field("running", &self.running)
            .field("url", &self.url)
            .field("discovery", &self.discovery)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// Adds the kit's commands to `registry`.
///
/// # Errors
///
/// One of them is already registered.
pub fn register(registry: &mut Registry) -> Result<(), RegisterError> {
    registry.register(
        CommandSpec::new(GET, "Get Parameters")
            .description(
                "Reads the plug-in's parameters: each one's ID, name, what it does, its kind and \
                 range, its default, and its value now as a number in its own units and as the \
                 host shows it. Give `params` to read only some, by ID or name.",
            )
            .permission(Permission::Read)
            .context(Context::Global)
            .args::<GetArgs>()
            .result::<ParamsResult>(),
    )?;
    registry.register(
        CommandSpec::new(SET, "Set Parameters")
            .description(
                "Sets one or more of the plug-in's parameters together, as one step the host \
                 records like a person's: its automation, undo and saved project follow. Give \
                 each new value as a number in the parameter's own units or as text the host \
                 would show, such as `-6 dB`. Values are clamped to each parameter's range. \
                 Returns the parameters it set, with their new values.",
            )
            .permission(Permission::Edit)
            .context(Context::Global)
            .args::<SetArgs>()
            .result::<ParamsResult>(),
    )?;
    registry.register(
        CommandSpec::new(SET_SERVER, "Agent Server")
            .description(
                "Switches the plug-in's agent server on or off. While on, agents on this computer \
                 that have its token can control this instance; the setting is saved with the \
                 project. Only a person can run it.",
            )
            .permission(Permission::External)
            .context(Context::Global)
            .args::<SetServerArgs>()
            .result::<ServerStatus>(),
    )?;
    registry.register(
        CommandSpec::new(SERVER_STATUS, "Agent Server Status")
            .description(
                "Says whether the plug-in's agent server is on, where agents connect, its token, \
                 and its discovery file, for the person to give to an agent. Only a person can \
                 run it.",
            )
            .permission(Permission::Read)
            .context(Context::Global)
            .args::<NoArgs>()
            .result::<ServerStatus>(),
    )
}

/// Every command the plug-in offers: the kit's, then its own.
///
/// # Errors
///
/// One of the plug-in's commands repeats an ID.
pub fn registry<P: Plugin>(plugin: &P) -> Result<Registry, RegisterError> {
    let mut registry = Registry::new();
    register(&mut registry)?;
    plugin.register_commands(&mut registry)?;
    Ok(registry)
}

/// Arguments from JSON, with `null` as no arguments.
pub(crate) fn parse<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T, String> {
    let args = if args.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        args.clone()
    };
    serde_json::from_value(args).map_err(|e| format!("the arguments don't fit: {e}"))
}

/// The parameter `param` names.
///
/// # Errors
///
/// No parameter has that ID or name.
pub fn resolve<'a>(
    values: &'a ParamValues,
    param: &ParamRef,
) -> Result<&'a ParamDescriptor, String> {
    let found = match param {
        ParamRef::Id(id) => values.descriptor(ParamId(*id)),
        ParamRef::Name(name) => values
            .descriptors()
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(name.trim())),
    };
    found.ok_or_else(|| {
        let names: Vec<String> = values
            .descriptors()
            .iter()
            .map(|d| format!("{} ({})", d.name, d.id.0))
            .collect();
        format!(
            "no parameter is {}; the parameters are {}",
            match param {
                ParamRef::Id(id) => format!("number {id}"),
                ParamRef::Name(name) => format!("called `{name}`"),
            },
            lspk_agent::permission::and_list(&names)
        )
    })
}

/// The parameters `refs` name (every one if it's empty), with their values.
///
/// # Errors
///
/// One of them names no parameter.
pub fn get(values: &ParamValues, refs: &[ParamRef]) -> Result<ParamsResult, String> {
    let params = if refs.is_empty() {
        values
            .descriptors()
            .iter()
            .enumerate()
            .map(|(index, d)| ParamState::new(d, values.value_at(index)))
            .collect()
    } else {
        refs.iter()
            .map(|r| {
                let d = resolve(values, r)?;
                Ok(ParamState::new(d, values.get(d.id).unwrap_or(d.default)))
            })
            .collect::<Result<_, String>>()?
    };
    Ok(ParamsResult { params })
}

/// The values `args` would set, resolved and clamped, without setting them.
///
/// # Errors
///
/// A value names no parameter, gives both or neither of `value` and
/// `text`, or has text the parameter can't read.
pub fn plan(values: &ParamValues, args: &SetArgs) -> Result<Vec<(ParamId, f64)>, String> {
    args.values
        .iter()
        .map(|new| {
            let d = resolve(values, &new.param)?;
            let value = match (new.value, new.text.as_deref()) {
                (Some(value), None) if value.is_finite() => value,
                (Some(_), None) => return Err(format!("{}'s value isn't a number", d.name)),
                (None, Some(text)) => d
                    .parse(text)
                    .ok_or_else(|| format!("`{text}` isn't a value {} can take", d.name))?,
                _ => {
                    return Err(format!(
                        "give {} either a `value` or a `text`, not both or neither",
                        d.name
                    ));
                }
            };
            Ok((d.id, d.clamp(value)))
        })
        .collect()
}

/// The parameter changes of one call, which the host takes as one step:
/// each parameter's gesture begins the first time the call changes it, and
/// every gesture ends when the call does ([`Edits::finish`], or on drop).
#[derive(Debug)]
pub struct Edits<'a> {
    host: &'a Host,
    open: Vec<ParamId>,
}

impl<'a> Edits<'a> {
    /// No changes yet.
    #[must_use]
    pub fn new(host: &'a Host) -> Self {
        Self {
            host,
            open: Vec::new(),
        }
    }

    /// Sets parameter `id` to `value`, unless it has that value already.
    ///
    /// # Errors
    ///
    /// The host link refused it: see [`lspk_plugin::EditError`].
    pub fn set(&mut self, id: ParamId, value: f64) -> Result<(), String> {
        if self.host.param(id) == Some(value) && !self.open.contains(&id) {
            return Ok(());
        }
        if !self.open.contains(&id) {
            self.host.begin_edit(id).map_err(|e| e.to_string())?;
            self.open.push(id);
        }
        self.host.set_param(id, value).map_err(|e| e.to_string())
    }

    /// Ends every gesture the call began.
    pub fn finish(mut self) {
        self.end();
    }

    fn end(&mut self) {
        for id in self.open.drain(..) {
            let _ = self.host.end_edit(id);
        }
    }
}

impl Drop for Edits<'_> {
    fn drop(&mut self) {
        self.end();
    }
}

/// Runs `id` if it's one of the parameter commands, with `edits` for its
/// changes; `None` if it isn't.
pub(crate) fn run_params(
    host: &Host,
    edits: &mut Edits<'_>,
    id: &str,
    args: &Value,
) -> Option<Result<Value, String>> {
    let values = host.values()?;
    let result = match id {
        GET => parse::<GetArgs>(args).and_then(|args| get(values, &args.params)),
        SET => parse::<SetArgs>(args).and_then(|args| {
            let planned = plan(values, &args)?;
            for &(id, value) in &planned {
                edits.set(id, value)?;
            }
            let params = planned
                .iter()
                .filter_map(|&(id, _)| {
                    let d = values.descriptor(id)?;
                    Some(ParamState::new(d, values.get(id)?))
                })
                .collect();
            Ok(ParamsResult { params })
        }),
        _ => return None,
    };
    Some(result.and_then(|r| serde_json::to_value(r).map_err(|e| e.to_string())))
}
