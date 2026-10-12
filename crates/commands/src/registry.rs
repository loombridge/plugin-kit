//! The registry: every command, by ID, and the checks that keep each one
//! complete.

use core::fmt;
use std::collections::BTreeMap;

use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::schema::{property, validate_in};
use crate::spec::{Binding, CommandSpec};

/// Every command a host offers, by ID.
///
/// Each crate that implements commands has a function that registers its
/// specs, and the host (an app, or a headless command-line tool) calls each one into
/// a single registry. Menus, shortcuts, key and MIDI mapping, the agent
/// API, and tests all look commands up here, so an action has one ID, one
/// schema, and one implementation however it's reached.
///
/// The registry holds what each command is, not how it runs: each
/// providing crate also exposes a function that runs its commands by ID
/// with JSON arguments, and
/// the host sends each ID to the crate that registered it.
///
/// ```
/// use lspk_commands::{CommandSpec, Context, NoArgs, Permission, Registry};
///
/// let mut registry = Registry::new();
/// registry.register(
///     CommandSpec::new("edit.undo", "Undo")
///         .description("Undoes the newest step in the undo history.")
///         .permission(Permission::Edit)
///         .context(Context::Global)
///         .args::<NoArgs>()
///         .result::<()>(),
/// )?;
/// assert!(registry.problems().is_empty());
/// assert_eq!(registry.get("edit.undo").unwrap().label, "Undo");
/// # Ok::<(), lspk_commands::RegisterError>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct Registry {
    commands: BTreeMap<String, CommandSpec>,
}

/// A command was registered twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterError {
    /// The ID registered twice.
    pub id: String,
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "command `{}` is already registered", self.id)
    }
}

impl std::error::Error for RegisterError {}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a command.
    ///
    /// A spec with parts missing is accepted; [`Registry::problems`] reports
    /// them.
    ///
    /// # Errors
    ///
    /// [`RegisterError`] if a command with the same ID is already
    /// registered. The registry is unchanged.
    pub fn register(&mut self, spec: CommandSpec) -> Result<(), RegisterError> {
        let id = spec.id.to_string();
        if self.commands.contains_key(&id) {
            return Err(RegisterError { id });
        }
        self.commands.insert(id, spec);
        Ok(())
    }

    /// The command with this ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CommandSpec> {
        self.commands.get(id)
    }

    /// Whether a command has this ID.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.commands.contains_key(id)
    }

    /// Every command, ordered by ID.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &CommandSpec> {
        self.commands.values()
    }

    /// How many commands there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Everything missing or wrong in the registered specs, in ID order: a
    /// command with no description, permission class, argument or result
    /// schema, or context; a malformed ID; a binding that fixes an argument
    /// the command doesn't take. Checking the bindings against the shortcut
    /// reference needs the shortcut table:
    /// [`Registry::binding_problems`].
    #[must_use]
    pub fn problems(&self) -> Vec<Problem> {
        self.commands.values().flat_map(spec_problems).collect()
    }

    /// Fails if [`Registry::problems`] finds anything.
    ///
    /// # Errors
    ///
    /// [`Problems`], listing each one.
    pub fn check(&self) -> Result<(), Problems> {
        let problems = self.problems();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Problems(problems))
        }
    }
}

impl Serialize for Registry {
    /// A list of command specs, ordered by ID: what the agent API's
    /// discovery tools serve.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.commands.values())
    }
}

impl<'a> IntoIterator for &'a Registry {
    type Item = &'a CommandSpec;
    type IntoIter = std::collections::btree_map::Values<'a, String, CommandSpec>;

    fn into_iter(self) -> Self::IntoIter {
        self.commands.values()
    }
}

/// Something missing or wrong in the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Problem {
    /// The ID isn't lower-case words joined by underscores, in at least two
    /// dot-separated parts.
    BadId {
        /// The command.
        command: String,
    },
    /// The label is empty or has spaces at either end.
    BadLabel {
        /// The command.
        command: String,
    },
    /// There's no description.
    NoDescription {
        /// The command.
        command: String,
    },
    /// The description isn't whole sentences: it doesn't start with a
    /// capital letter and end with a full stop.
    DescriptionNotSentences {
        /// The command.
        command: String,
    },
    /// There's no permission class.
    NoPermission {
        /// The command.
        command: String,
    },
    /// The command applies in no context.
    NoContext {
        /// The command.
        command: String,
    },
    /// There's no argument schema.
    NoArgsSchema {
        /// The command.
        command: String,
    },
    /// There's no result schema.
    NoResultSchema {
        /// The command.
        command: String,
    },
    /// The argument schema doesn't describe a JSON object, which is how
    /// agents send arguments.
    ArgsNotObject {
        /// The command.
        command: String,
    },
    /// The same shortcut is bound twice.
    BindingRepeated {
        /// The command.
        command: String,
        /// The shortcut.
        shortcut: String,
    },
    /// A binding fixes an argument the command doesn't take, or a value its
    /// schema rejects.
    BindingArg {
        /// The command.
        command: String,
        /// The shortcut.
        shortcut: String,
        /// What's wrong.
        message: String,
    },
    /// A binding toggles an argument that isn't a boolean.
    BindingToggle {
        /// The command.
        command: String,
        /// The shortcut.
        shortcut: String,
        /// The argument.
        arg: String,
    },
    /// A binding names a shortcut the shortcut reference doesn't have.
    UnknownShortcut {
        /// The command.
        command: String,
        /// The shortcut.
        shortcut: String,
    },
    /// A binding's shortcut applies in a context the command doesn't (and
    /// isn't global).
    ShortcutContext {
        /// The command.
        command: String,
        /// The shortcut.
        shortcut: String,
        /// The shortcut's context.
        context: String,
    },
    /// Two commands bind the same shortcut in the same context, so a key
    /// press couldn't tell which to run.
    SharedShortcut {
        /// The shortcut.
        shortcut: String,
        /// One command.
        first: String,
        /// The other.
        second: String,
        /// The context they share.
        context: String,
    },
    /// The registry's contexts and the shortcut reference's differ.
    ContextDrift {
        /// Contexts in the reference that the registry lacks.
        missing: Vec<String>,
        /// Contexts in the registry that the reference lacks.
        extra: Vec<String>,
    },
}

impl Problem {
    /// The command the problem is in, if it's in one.
    #[must_use]
    pub fn command(&self) -> Option<&str> {
        match self {
            Self::BadId { command }
            | Self::BadLabel { command }
            | Self::NoDescription { command }
            | Self::DescriptionNotSentences { command }
            | Self::NoPermission { command }
            | Self::NoContext { command }
            | Self::NoArgsSchema { command }
            | Self::NoResultSchema { command }
            | Self::ArgsNotObject { command }
            | Self::BindingRepeated { command, .. }
            | Self::BindingArg { command, .. }
            | Self::BindingToggle { command, .. }
            | Self::UnknownShortcut { command, .. }
            | Self::ShortcutContext { command, .. } => Some(command),
            Self::SharedShortcut { .. } | Self::ContextDrift { .. } => None,
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadId { command } => write!(
                f,
                "{command}: IDs are lower-case words joined by underscores, in at least two \
                 dot-separated parts, such as `track.create`"
            ),
            Self::BadLabel { command } => {
                write!(f, "{command}: its label is empty or has spaces at an end")
            }
            Self::NoDescription { command } => write!(f, "{command}: has no description"),
            Self::DescriptionNotSentences { command } => write!(
                f,
                "{command}: its description should be whole sentences, starting with a capital \
                 letter and ending with a full stop"
            ),
            Self::NoPermission { command } => write!(f, "{command}: has no permission class"),
            Self::NoContext { command } => write!(f, "{command}: applies in no context"),
            Self::NoArgsSchema { command } => write!(f, "{command}: has no argument schema"),
            Self::NoResultSchema { command } => write!(f, "{command}: has no result schema"),
            Self::ArgsNotObject { command } => write!(
                f,
                "{command}: its argument schema doesn't describe a JSON object, which is how \
                 agents send arguments"
            ),
            Self::BindingRepeated { command, shortcut } => {
                write!(f, "{command}: binds {shortcut} twice")
            }
            Self::BindingArg {
                command,
                shortcut,
                message,
            } => write!(f, "{command}: {shortcut} fixes {message}"),
            Self::BindingToggle {
                command,
                shortcut,
                arg,
            } => write!(
                f,
                "{command}: {shortcut} toggles `{arg}`, which isn't a boolean argument"
            ),
            Self::UnknownShortcut { command, shortcut } => write!(
                f,
                "{command}: binds {shortcut}, which isn't in the shortcut reference"
            ),
            Self::ShortcutContext {
                command,
                shortcut,
                context,
            } => write!(
                f,
                "{command}: binds {shortcut}, whose context `{context}` isn't global or one of \
                 the command's contexts"
            ),
            Self::SharedShortcut {
                shortcut,
                first,
                second,
                context,
            } => write!(
                f,
                "{shortcut} runs both {first} and {second} in `{context}`"
            ),
            Self::ContextDrift { missing, extra } => write!(
                f,
                "the registry's contexts differ from the shortcut reference (missing from \
                 the registry: {}; not in the reference: {})",
                list_or_none(missing),
                list_or_none(extra)
            ),
        }
    }
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// Every problem [`Registry::check`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problems(pub Vec<Problem>);

impl fmt::Display for Problems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the command registry has {} problem(s):", self.0.len())?;
        for problem in &self.0 {
            write!(f, "\n  {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Problems {}

/// What's missing or wrong in one spec.
fn spec_problems(spec: &CommandSpec) -> Vec<Problem> {
    let command = || spec.id.to_string();
    let mut problems = Vec::new();
    if !valid_id(&spec.id) {
        problems.push(Problem::BadId { command: command() });
    }
    if spec.label.is_empty() || spec.label.trim() != spec.label {
        problems.push(Problem::BadLabel { command: command() });
    }
    let description = spec.description.trim();
    if description.is_empty() {
        problems.push(Problem::NoDescription { command: command() });
    } else if !(description.starts_with(|c: char| c.is_uppercase()) && description.ends_with('.')) {
        problems.push(Problem::DescriptionNotSentences { command: command() });
    }
    if spec.permission.is_none() {
        problems.push(Problem::NoPermission { command: command() });
    }
    if spec.contexts.is_empty() {
        problems.push(Problem::NoContext { command: command() });
    }
    match &spec.args {
        None => problems.push(Problem::NoArgsSchema { command: command() }),
        Some(args) if args.get("type").and_then(Value::as_str) != Some("object") => {
            problems.push(Problem::ArgsNotObject { command: command() });
        }
        Some(_) => {}
    }
    if spec.result.is_none() {
        problems.push(Problem::NoResultSchema { command: command() });
    }
    for (index, binding) in spec.bindings.iter().enumerate() {
        if spec.bindings[..index]
            .iter()
            .any(|earlier| earlier.shortcut == binding.shortcut && earlier.args == binding.args)
        {
            problems.push(Problem::BindingRepeated {
                command: command(),
                shortcut: binding.shortcut.to_string(),
            });
        }
        if let Some(args) = &spec.args {
            problems.extend(binding_arg_problems(spec, args, binding));
        }
    }
    problems
}

/// Checks the arguments a binding fixes and the one it toggles against the
/// command's argument schema.
fn binding_arg_problems(
    spec: &CommandSpec,
    args: &schemars::Schema,
    binding: &Binding,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    let shortcut = binding.shortcut.to_string();
    for (name, value) in &binding.args {
        let message = match property(args, name) {
            None => Some(format!("`{name}`, which isn't one of its arguments")),
            Some(schema) => validate_in(args, schema, value)
                .err()
                .map(|error| format!("`{name}` = {value}, which its schema rejects ({error})")),
        };
        if let Some(message) = message {
            problems.push(Problem::BindingArg {
                command: spec.id.to_string(),
                shortcut: shortcut.clone(),
                message,
            });
        }
    }
    if let Some(arg) = &binding.toggles {
        let boolean = property(args, arg).is_some_and(|schema| {
            validate_in(args, schema, &Value::Bool(true)).is_ok()
                && validate_in(args, schema, &Value::Bool(false)).is_ok()
                && validate_in(args, schema, &Value::from(0)).is_err()
        });
        if !boolean {
            problems.push(Problem::BindingToggle {
                command: spec.id.to_string(),
                shortcut,
                arg: arg.to_string(),
            });
        }
    }
    problems
}

/// Whether an ID is lower-case words joined by underscores, in at least
/// two dot-separated parts.
fn valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            part.starts_with(|c: char| c.is_ascii_lowercase())
                && !part.ends_with('_')
                && !part.contains("__")
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::Deserialize;

    use super::*;
    use crate::{Context, NoArgs, Permission};

    #[derive(Deserialize, JsonSchema)]
    #[expect(dead_code, reason = "only the schema is used")]
    struct SoloArgs {
        /// The track.
        track: u64,
        /// Whether it's soloed.
        solo: bool,
        /// How loud.
        kind: Kind,
    }

    #[derive(Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    enum Kind {
        Audio,
        Midi,
    }

    fn complete(id: &'static str) -> CommandSpec {
        CommandSpec::new(id, "Solo Track")
            .description("Solos a track, so only soloed tracks are heard.")
            .permission(Permission::Edit)
            .context(Context::TrackHeaders)
            .args::<SoloArgs>()
            .result::<()>()
    }

    fn problems_of(spec: CommandSpec) -> Vec<String> {
        let mut registry = Registry::new();
        registry.register(spec).unwrap();
        registry
            .problems()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_complete_command_has_no_problems() {
        assert_eq!(
            problems_of(complete("mixer.set_solo")),
            Vec::<String>::new()
        );
    }

    /// The registry reports a command that lacks a schema, a description,
    /// or a permission class, and every other required part.
    #[test]
    fn incomplete_commands_are_reported() {
        let bare = CommandSpec::new("mixer.set_solo", "Solo Track");
        assert_eq!(
            problems_of(bare),
            [
                "mixer.set_solo: has no description",
                "mixer.set_solo: has no permission class",
                "mixer.set_solo: applies in no context",
                "mixer.set_solo: has no argument schema",
                "mixer.set_solo: has no result schema",
            ]
        );

        let mut no_description = complete("mixer.set_solo");
        no_description.description = "  ".into();
        assert_eq!(
            problems_of(no_description),
            ["mixer.set_solo: has no description"]
        );
        let mut no_permission = complete("mixer.set_solo");
        no_permission.permission = None;
        assert_eq!(
            problems_of(no_permission),
            ["mixer.set_solo: has no permission class"]
        );
        let mut no_args = complete("mixer.set_solo");
        no_args.args = None;
        assert_eq!(
            problems_of(no_args),
            ["mixer.set_solo: has no argument schema"]
        );
        let mut no_result = complete("mixer.set_solo");
        no_result.result = None;
        assert_eq!(
            problems_of(no_result),
            ["mixer.set_solo: has no result schema"]
        );
    }

    #[test]
    fn ids_labels_and_descriptions_are_checked() {
        for id in [
            "solo",
            "Mixer.solo",
            "mixer.",
            "mixer..solo",
            "mixer.set__solo",
            "mixer.2x",
        ] {
            let spec = CommandSpec {
                id: id.into(),
                ..complete("a.b")
            };
            assert_eq!(problems_of(spec).len(), 1, "{id}");
        }
        for id in ["mixer.set_solo", "edit.undo", "view.show_track_2"] {
            let spec = CommandSpec {
                id: id.into(),
                ..complete("a.b")
            };
            assert!(problems_of(spec).is_empty(), "{id}");
        }
        let spec = CommandSpec {
            label: " Solo".into(),
            ..complete("mixer.set_solo")
        };
        assert_eq!(problems_of(spec).len(), 1);
        let spec = complete("mixer.set_solo").description("solos a track");
        assert_eq!(
            problems_of(spec),
            [
                "mixer.set_solo: its description should be whole sentences, starting with a capital \
              letter and ending with a full stop"
            ]
        );
    }

    #[test]
    fn arguments_must_be_an_object() {
        let spec = complete("mixer.set_solo").args::<u32>();
        assert_eq!(
            problems_of(spec),
            [
                "mixer.set_solo: its argument schema doesn't describe a JSON object, which is how \
              agents send arguments"
            ]
        );
        assert!(problems_of(complete("edit.undo").args::<NoArgs>()).is_empty());
    }

    #[test]
    fn bindings_must_fit_the_arguments() {
        let spec = complete("mixer.set_solo")
            .binding(Binding::new("S-TRACK-014").toggles("solo"))
            .binding(Binding::new("S-TRACK-001").arg("kind", "audio"));
        assert!(problems_of(spec).is_empty());

        let spec = complete("mixer.set_solo")
            .binding(Binding::new("S-TRACK-014").toggles("track"))
            .binding(Binding::new("S-TRACK-001").arg("kind", "video"))
            .binding(Binding::new("S-TRACK-002").arg("colour", "red"))
            .binding("S-TRACK-003")
            .binding("S-TRACK-003");
        let problems = problems_of(spec);
        assert_eq!(problems.len(), 4, "{problems:#?}");
        assert_eq!(
            problems[0],
            "mixer.set_solo: S-TRACK-014 toggles `track`, which isn't a boolean argument"
        );
        assert!(
            problems[1].starts_with(
                "mixer.set_solo: S-TRACK-001 fixes `kind` = \"video\", which its schema rejects"
            ),
            "{}",
            problems[1]
        );
        assert_eq!(
            problems[2],
            "mixer.set_solo: S-TRACK-002 fixes `colour`, which isn't one of its arguments"
        );
        assert_eq!(problems[3], "mixer.set_solo: binds S-TRACK-003 twice");
    }

    #[test]
    fn ids_register_once() {
        let mut registry = Registry::new();
        registry.register(complete("mixer.set_solo")).unwrap();
        assert_eq!(
            registry.register(complete("mixer.set_solo")),
            Err(RegisterError {
                id: "mixer.set_solo".into()
            })
        );
        assert_eq!(registry.len(), 1);
        registry.register(complete("edit.undo")).unwrap();
        let ids: Vec<_> = registry.iter().map(|spec| spec.id.as_ref()).collect();
        assert_eq!(ids, ["edit.undo", "mixer.set_solo"]);
    }

    #[test]
    fn registry_serialises_as_a_list() {
        let mut registry = Registry::new();
        registry
            .register(
                complete("mixer.set_solo").binding(Binding::new("S-TRACK-014").toggles("solo")),
            )
            .unwrap();
        let json = serde_json::to_value(&registry).unwrap();
        let spec = &json[0];
        assert_eq!(spec["id"], "mixer.set_solo");
        assert_eq!(spec["permission"], "edit");
        assert_eq!(spec["contexts"], serde_json::json!(["track-headers"]));
        assert_eq!(
            spec["bindings"],
            serde_json::json!([{ "shortcut": "S-TRACK-014", "toggles": "solo" }])
        );
        assert_eq!(spec["args"]["type"], "object");
        assert_eq!(spec["result"]["type"], "null");
    }
}
