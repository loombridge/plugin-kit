//! Command specs: everything the registry knows about one command.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::context::Context;
use crate::permission::Permission;
use crate::schema;

/// One command in the registry: its ID, how a person reaches it, what it
/// takes and returns, and what it may touch.
///
/// Build one with [`CommandSpec::new`] and the builder methods, then add it
/// to a [`Registry`](crate::Registry). A spec can be registered with parts
/// missing; [`Registry::problems`](crate::Registry::problems) lists them,
/// and the registry's tests fail until every command is complete.
///
/// ```
/// use lspk_commands::{Binding, CommandSpec, Context, NoArgs, Permission};
///
/// let insert = CommandSpec::new("track.insert_audio", "Insert Audio Track")
///     .description("Adds an empty audio track after the selected track. Undo removes it.")
///     .permission(Permission::Edit)
///     .context(Context::Global)
///     .binding("S-TRACK-001")
///     .args::<NoArgs>()
///     .result::<()>();
/// assert_eq!(insert.area(), "track");
/// assert_eq!(insert.permission, Some(Permission::Edit));
/// ```
#[derive(Clone, Debug, Serialize)]
pub struct CommandSpec {
    /// The command's stable ID, such as `track.create`.
    ///
    /// Key maps, MIDI maps, agents, and logs refer to it, so it never
    /// changes and is never reused. It's lower-case words joined by
    /// underscores, in dot-separated parts, area first.
    pub id: Cow<'static, str>,
    /// What a person sees in a menu, a key-mapping list, or a command
    /// search, such as `Insert Track` or `Undo`.
    pub label: Cow<'static, str>,
    /// What the command does, when to use it, and what it changes, written
    /// for a model in whole sentences. Empty until set.
    pub description: Cow<'static, str>,
    /// What the command may touch, once set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<Permission>,
    /// Where a person reaches the command.
    pub contexts: Vec<Context>,
    /// The shortcuts that run it by default.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<Binding>,
    /// The JSON Schema of its arguments, once set. Always an object schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Schema>,
    /// The JSON Schema of its result, once set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Schema>,
}

impl CommandSpec {
    /// A spec with an ID and a label, and nothing else yet.
    #[must_use]
    pub fn new(id: impl Into<Cow<'static, str>>, label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: Cow::Borrowed(""),
            permission: None,
            contexts: Vec::new(),
            bindings: Vec::new(),
            args: None,
            result: None,
        }
    }

    /// Sets the description, written for a model: what the command does,
    /// when to use it, and what it changes.
    #[must_use]
    pub fn description(mut self, description: impl Into<Cow<'static, str>>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the permission class.
    #[must_use]
    pub fn permission(mut self, permission: Permission) -> Self {
        self.permission = Some(permission);
        self
    }

    /// Adds a context the command applies in.
    #[must_use]
    pub fn context(mut self, context: Context) -> Self {
        if !self.contexts.contains(&context) {
            self.contexts.push(context);
        }
        self
    }

    /// Adds several contexts.
    #[must_use]
    pub fn contexts(self, contexts: impl IntoIterator<Item = Context>) -> Self {
        contexts.into_iter().fold(self, Self::context)
    }

    /// Adds a default binding: a shortcut from the shortcut reference, by
    /// its ID, optionally with arguments it fixes or a switch it toggles.
    #[must_use]
    pub fn binding(mut self, binding: impl Into<Binding>) -> Self {
        self.bindings.push(binding.into());
        self
    }

    /// Sets the argument schema to `T`'s: what deserialising `T` accepts.
    /// Use [`NoArgs`](crate::NoArgs) for a command that takes none.
    #[must_use]
    pub fn args<T: JsonSchema>(self) -> Self {
        self.args_schema(schema::args_schema::<T>())
    }

    /// Sets the argument schema directly, such as one variant cut out of a
    /// command enum's schema by [`tagged_variant`](crate::tagged_variant).
    #[must_use]
    pub fn args_schema(mut self, schema: Schema) -> Self {
        self.args = Some(schema);
        self
    }

    /// Sets the result schema to `T`'s: what serialising `T` writes. Use
    /// `()` for a command that returns nothing.
    #[must_use]
    pub fn result<T: JsonSchema>(self) -> Self {
        self.result_schema(schema::result_schema::<T>())
    }

    /// Sets the result schema directly.
    #[must_use]
    pub fn result_schema(mut self, schema: Schema) -> Self {
        self.result = Some(schema);
        self
    }

    /// The area the command belongs to: the first part of its ID, such as
    /// `track`. The reference and the agent API group commands by area.
    #[must_use]
    pub fn area(&self) -> &str {
        self.id.split('.').next().unwrap_or_default()
    }
}

/// A default binding: a shortcut from the shortcut reference that runs a
/// command.
///
/// The shortcut is named by its ID in the shortcut reference, so
/// the keys come from the reference and can't drift from it. A command with
/// no shortcut in the reference has no binding.
///
/// A binding supplies the arguments it fixes (Cmd-T fixes an audio track's
/// `kind`). The UI supplies the rest when the shortcut is pressed: the
/// selected objects in the shortcut's context, text typed into a field, or
/// the result of a gesture. A binding that toggles a switch sets it to the
/// opposite of its current value.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Binding {
    /// The shortcut's ID in the shortcut reference, such as `S-EDIT-006`.
    pub shortcut: Cow<'static, str>,
    /// The arguments the binding fixes.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
    /// The boolean argument the binding toggles, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toggles: Option<Cow<'static, str>>,
}

impl Binding {
    /// A binding to the shortcut with this ID.
    #[must_use]
    pub fn new(shortcut: impl Into<Cow<'static, str>>) -> Self {
        Self {
            shortcut: shortcut.into(),
            args: Map::new(),
            toggles: None,
        }
    }

    /// Fixes an argument's value.
    #[must_use]
    pub fn arg(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.args.insert(name.into(), value.into());
        self
    }

    /// Makes the binding toggle a boolean argument: the UI passes the
    /// opposite of the switch's current value.
    #[must_use]
    pub fn toggles(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        self.toggles = Some(name.into());
        self
    }
}

impl From<&'static str> for Binding {
    fn from(shortcut: &'static str) -> Self {
        Self::new(shortcut)
    }
}
