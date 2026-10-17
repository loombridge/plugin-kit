//! The commands every kit plug-in has: its parameters (`params.get`,
//! `params.set`) and its agent server (`agent.set_server`,
//! `agent.server_status`), registered beside the plug-in's own.
//!
//! Parameter changes reach the host exactly as a person's would: through
//! [`Host`]'s gestures, a begin, the value and an end, so the host's
//! automation, undo and saved project stay in step. Every change one call
//! makes ([`Edits`]) opens its parameters' gestures as it first touches each
//! and closes them all at the end, so the host takes the call as one step.
//!
//! A modular plug-in's parameters ([`lspk_params::ParamSpace`]) appear by
//! their real names and paths (`fx "1" / param "Drive"`), with their
//! current module's kind, range and value text; a slot's reserved
//! parameters its module doesn't use are left out. One `params.set` can
//! change a slot's type and then set its new module's parameters: each
//! value is read as the values before it in the call leave the slots.

use lspk_commands::{CommandSpec, Context, NoArgs, Permission, RegisterError, Registry};
use lspk_params::{ParamId, ParamKind, ParamView};
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

/// A parameter, by its ID, its name or its path.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ParamRef {
    /// Its ID, which never changes.
    Id(u32),
    /// Its name as the host shows it, such as `FX 1 Drive`, or its path,
    /// such as `fx "1" / param "Drive"`, in any case.
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
    /// Its name, as the host shows it.
    pub name: String,
    /// Its path, such as `osc "A" / param "Level"`.
    pub path: String,
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
    /// The parameter `view` describes, with `stored` the value kept for
    /// it.
    #[must_use]
    pub fn new(view: &ParamView, stored: f64) -> Self {
        let d = &view.descriptor;
        Self {
            id: view.id.0,
            name: view.name.clone(),
            path: view.path.to_string(),
            description: d.description.clone(),
            kind: d.kind.clone(),
            min: d.min(),
            max: d.max(),
            default: d.default,
            value: view.real(stored),
            text: view.format(stored),
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
    register_params(registry)?;
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

/// Adds the commands for the plug-in's parameters (`params.get`,
/// `params.set`) to `registry`: all a plug-in run headless has of the kit's.
///
/// # Errors
///
/// One of them is already registered.
pub fn register_params(registry: &mut Registry) -> Result<(), RegisterError> {
    registry.register(
        CommandSpec::new(GET, "Get Parameters")
            .description(
                "Reads the plug-in's parameters: each one's ID, name, path, what it does, its kind \
                 and range, its default, and its value now as a number in its own units and as \
                 the host shows it. Give `params` to read only some, by ID, name or path. A \
                 slot's parameters are its current module's.",
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
                 Values apply in order: set a slot's type first, then its new module's \
                 parameters, in the same call. Returns the parameters it set, with their new \
                 values.",
            )
            .permission(Permission::Edit)
            .context(Context::Global)
            .args::<SetArgs>()
            .result::<ParamsResult>(),
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

/// The values the parameters would have after `pending` changes (ID and
/// stored value, in order), for reading a call's later values as its
/// earlier ones leave the slots.
fn value_of<'a>(
    values: &'a ParamValues,
    pending: &'a [(ParamId, f64)],
) -> impl Fn(ParamId) -> Option<f64> + 'a {
    move |id| {
        pending
            .iter()
            .rev()
            .find(|(p, _)| *p == id)
            .map(|(_, v)| *v)
            .or_else(|| values.get(id))
    }
}

/// What parameter `id` is once `pending` changes are made.
fn view_after(values: &ParamValues, id: ParamId, pending: &[(ParamId, f64)]) -> Option<ParamView> {
    match values.space() {
        Some(space) => space.view(id, &value_of(values, pending)),
        None => values.view(id),
    }
}

/// The visible parameter `param` names once `pending` changes are made.
fn resolve_after(
    values: &ParamValues,
    param: &ParamRef,
    pending: &[(ParamId, f64)],
) -> Result<ParamView, String> {
    let found = match param {
        ParamRef::Id(id) => view_after(values, ParamId(*id), pending),
        ParamRef::Name(name) => match values.space() {
            Some(space) => space.find(name, &value_of(values, pending)),
            None => values.find(name),
        }
        .and_then(|id| view_after(values, id, pending)),
    }
    .filter(|view| !view.hidden);
    found.ok_or_else(|| {
        let names: Vec<String> = values
            .descriptors()
            .iter()
            .filter_map(|d| view_after(values, d.id, pending))
            .filter(|v| !v.hidden)
            .map(|v| format!("{} ({})", v.name, v.id.0))
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

/// The parameter `param` names, as it is now: by ID, by name as the host
/// shows it, or by path. A slot's reserved parameter its module doesn't use
/// names nothing.
///
/// # Errors
///
/// No visible parameter has that ID, name or path.
pub fn resolve(values: &ParamValues, param: &ParamRef) -> Result<ParamView, String> {
    resolve_after(values, param, &[])
}

/// The parameters `refs` name (every visible one if it's empty), with
/// their values.
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
            .filter_map(|(index, d)| {
                let view = values.view(d.id)?;
                (!view.hidden).then(|| ParamState::new(&view, values.value_at(index)))
            })
            .collect()
    } else {
        refs.iter()
            .map(|r| {
                let view = resolve(values, r)?;
                let stored = values.get(view.id).unwrap_or(view.descriptor.default);
                Ok(ParamState::new(&view, stored))
            })
            .collect::<Result<_, String>>()?
    };
    Ok(ParamsResult { params })
}

/// The values `args` would set, resolved and clamped, without setting them:
/// each parameter's ID and the value to keep for it, in order. Each value
/// is read as the ones before it leave the slots, so a slot's type comes
/// before its new module's parameters.
///
/// # Errors
///
/// A value names no parameter, gives both or neither of `value` and
/// `text`, or has text the parameter can't read.
pub fn plan(values: &ParamValues, args: &SetArgs) -> Result<Vec<(ParamId, f64)>, String> {
    let mut planned: Vec<(ParamId, f64)> = Vec::new();
    for new in &args.values {
        let view = resolve_after(values, &new.param, &planned)?;
        let d = &view.descriptor;
        let real = match (new.value, new.text.as_deref()) {
            (Some(value), None) if value.is_finite() => value,
            (Some(_), None) => return Err(format!("{}'s value isn't a number", view.name)),
            (None, Some(text)) => d
                .parse(text)
                .ok_or_else(|| format!("`{text}` isn't a value {} can take", view.name))?,
            _ => {
                return Err(format!(
                    "give {} either a `value` or a `text`, not both or neither",
                    view.name
                ));
            }
        };
        planned.push((view.id, view.stored(d.clamp(real))));
    }
    Ok(planned)
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
                    let view = values.view(id)?;
                    Some(ParamState::new(&view, values.get(id)?))
                })
                .collect();
            Ok(ParamsResult { params })
        }),
        _ => return None,
    };
    Some(result.and_then(|r| serde_json::to_value(r).map_err(|e| e.to_string())))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use lspk_params::{ModuleType, ParamDescriptor, ParamId, ParamSpace, ParamUnit};

    fn test_values() -> ParamValues {
        ParamValues::new(vec![
            ParamDescriptor::gain(ParamId(1), "Gain", 0.0, 0.0),
            ParamDescriptor::continuous(ParamId(2), "Mix", (0.0, 100.0), 50.0, ParamUnit::Percent),
            ParamDescriptor::toggle(ParamId(3), "Bypass", false),
            ParamDescriptor::choice(ParamId(4), "Mode", &["Clean", "Warm", "Hot"], 0),
        ])
        .expect("valid parameters")
    }

    fn modular_values() -> ParamValues {
        let drive = ModuleType::new(40, "drive", "Drive")
            .param(ParamDescriptor::continuous(
                ParamId(0),
                "Amount",
                (0.0, 100.0),
                25.0,
                ParamUnit::Percent,
            ))
            .param(ParamDescriptor::choice(
                ParamId(1),
                "Shape",
                &["Soft", "Hard"],
                0,
            ));
        let echo = ModuleType::new(41, "echo", "Echo").param(
            ParamDescriptor::continuous(
                ParamId(0),
                "Time",
                (1.0, 1000.0),
                250.0,
                ParamUnit::Milliseconds,
            )
            .logarithmic(),
        );
        let space = ParamSpace::builder()
            .global(ParamDescriptor::gain(ParamId(1), "Gain", 0.0, 0.0))
            .slots((10, "fx", "FX"), "Type", &["1"], vec![drive, echo])
            .build()
            .expect("valid space");
        let descriptors = space.descriptors();
        ParamValues::with_space(descriptors, Arc::new(space)).expect("valid modular parameters")
    }

    #[test]
    fn a_request_that_names_no_parameter_fails_with_a_helpful_error() {
        let values = test_values();

        // Unknown ID in resolve.
        let err = resolve(&values, &ParamRef::Id(999)).unwrap_err();
        assert_eq!(
            err,
            "no parameter is number 999; the parameters are Gain (1), Mix (2), Bypass (3) and Mode (4)"
        );

        // Unknown name in resolve.
        let err = resolve(&values, &ParamRef::Name("Nonexistent".into())).unwrap_err();
        assert_eq!(
            err,
            "no parameter is called `Nonexistent`; the parameters are Gain (1), Mix (2), Bypass (3) and Mode (4)"
        );

        // Unknown name in plan.
        let err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Nonexistent".into()),
                    value: Some(1.0),
                    text: None,
                }],
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            "no parameter is called `Nonexistent`; the parameters are Gain (1), Mix (2), Bypass (3) and Mode (4)"
        );

        // Unknown ID in plan.
        let err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Id(999),
                    value: Some(1.0),
                    text: None,
                }],
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            "no parameter is number 999; the parameters are Gain (1), Mix (2), Bypass (3) and Mode (4)"
        );

        // Unknown ID in get.
        let err = get(&values, &[ParamRef::Id(42)]).unwrap_err();
        assert_eq!(
            err,
            "no parameter is number 42; the parameters are Gain (1), Mix (2), Bypass (3) and Mode (4)"
        );

        // In a modular space, an unassigned or hidden reserved parameter names nothing.
        let modular = modular_values();
        let space = modular.space().unwrap();
        let slot = &space.slots()[0];
        // Reserved parameter 5 is unused because span is at most 2.
        let unused_reserved_id = slot.reserved_id(5).0;
        let err = resolve(&modular, &ParamRef::Id(unused_reserved_id)).unwrap_err();
        assert!(
            err.starts_with(&format!(
                "no parameter is number {unused_reserved_id}; the parameters are "
            )),
            "{err}"
        );
    }

    #[test]
    fn giving_both_or_neither_of_value_and_text_is_refused() {
        let values = test_values();

        // Both value and text.
        let err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Gain".into()),
                    value: Some(0.5),
                    text: Some("-6 dB".into()),
                }],
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            "give Gain either a `value` or a `text`, not both or neither"
        );

        // Neither value nor text.
        let err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Gain".into()),
                    value: None,
                    text: None,
                }],
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            "give Gain either a `value` or a `text`, not both or neither"
        );
    }

    #[test]
    fn text_the_parameter_cannot_read_is_refused() {
        let values = test_values();

        // Unparseable text for continuous/gain parameter.
        let err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Gain".into()),
                    value: None,
                    text: Some("loud".into()),
                }],
            },
        )
        .unwrap_err();
        assert_eq!(err, "`loud` isn't a value Gain can take");

        // Unparseable text for choice parameter.
        let err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Mode".into()),
                    value: None,
                    text: Some("Distorted".into()),
                }],
            },
        )
        .unwrap_err();
        assert_eq!(err, "`Distorted` isn't a value Mode can take");
    }

    #[test]
    fn values_outside_range_are_clamped() {
        let values = test_values();

        // Continuous parameter above max (0..100 %).
        let planned = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Mix".into()),
                    value: Some(150.0),
                    text: None,
                }],
            },
        )
        .expect("planned");
        assert_eq!(planned, vec![(ParamId(2), 100.0)]);

        // Continuous parameter below min.
        let planned = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Mix".into()),
                    value: Some(-20.0),
                    text: None,
                }],
            },
        )
        .expect("planned");
        assert_eq!(planned, vec![(ParamId(2), 0.0)]);

        // Gain parameter above max (0 dB = 1.0 linear).
        let planned = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Gain".into()),
                    value: Some(2.5),
                    text: None,
                }],
            },
        )
        .expect("planned");
        assert_eq!(planned, vec![(ParamId(1), 1.0)]);

        // Clamping also applies to parsed text values.
        let planned = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Mix".into()),
                    value: None,
                    text: Some("200 %".into()),
                }],
            },
        )
        .expect("planned");
        assert_eq!(planned, vec![(ParamId(2), 100.0)]);
    }

    #[test]
    fn non_finite_values_are_refused() {
        let values = test_values();

        let nan_err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Gain".into()),
                    value: Some(f64::NAN),
                    text: None,
                }],
            },
        )
        .unwrap_err();
        assert_eq!(nan_err, "Gain's value isn't a number");

        let inf_err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Gain".into()),
                    value: Some(f64::INFINITY),
                    text: None,
                }],
            },
        )
        .unwrap_err();
        assert_eq!(inf_err, "Gain's value isn't a number");

        let neg_inf_err = plan(
            &values,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("Gain".into()),
                    value: Some(f64::NEG_INFINITY),
                    text: None,
                }],
            },
        )
        .unwrap_err();
        assert_eq!(neg_inf_err, "Gain's value isn't a number");
    }

    #[test]
    fn a_slots_type_must_be_set_before_its_modules_parameters() {
        let modular = modular_values();

        // Slot 1 defaults to Drive, which has Amount and Shape.
        // Echo's Time parameter is hidden / non-existent initially.
        let err = plan(
            &modular,
            &SetArgs {
                values: vec![NewValue {
                    param: ParamRef::Name("fx \"1\" / param \"Time\"".into()),
                    value: None,
                    text: Some("500 ms".into()),
                }],
            },
        )
        .unwrap_err();
        assert!(
            err.starts_with(
                "no parameter is called `fx \"1\" / param \"Time\"`; the parameters are "
            ),
            "{err}"
        );

        // When the slot's type is set first in the same call, Echo's Time parameter resolves.
        let planned = plan(
            &modular,
            &SetArgs {
                values: vec![
                    NewValue {
                        param: ParamRef::Name("FX 1 Type".into()),
                        value: None,
                        text: Some("Echo".into()),
                    },
                    NewValue {
                        param: ParamRef::Name("fx \"1\" / param \"Time\"".into()),
                        value: None,
                        text: Some("500 ms".into()),
                    },
                ],
            },
        )
        .expect("planned");
        assert_eq!(planned.len(), 2);

        // Inverting the order fails: the slot's type must precede its module's parameters.
        let err = plan(
            &modular,
            &SetArgs {
                values: vec![
                    NewValue {
                        param: ParamRef::Name("fx \"1\" / param \"Time\"".into()),
                        value: None,
                        text: Some("500 ms".into()),
                    },
                    NewValue {
                        param: ParamRef::Name("FX 1 Type".into()),
                        value: None,
                        text: Some("Echo".into()),
                    },
                ],
            },
        )
        .unwrap_err();
        assert!(
            err.starts_with(
                "no parameter is called `fx \"1\" / param \"Time\"`; the parameters are "
            ),
            "{err}"
        );

        // Similarly, once the slot is set to Echo, Drive's parameters (Amount) can no longer be resolved.
        let err = plan(
            &modular,
            &SetArgs {
                values: vec![
                    NewValue {
                        param: ParamRef::Name("FX 1 Type".into()),
                        value: None,
                        text: Some("Echo".into()),
                    },
                    NewValue {
                        param: ParamRef::Name("fx \"1\" / param \"Amount\"".into()),
                        value: Some(50.0),
                        text: None,
                    },
                ],
            },
        )
        .unwrap_err();
        assert!(
            err.starts_with(
                "no parameter is called `fx \"1\" / param \"Amount\"`; the parameters are "
            ),
            "{err}"
        );
    }
}
