//! The command reference, generated from the registry.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use schemars::Schema;
use serde_json::{Map, Value};

use crate::permission::Permission;
use crate::registry::Registry;
use crate::shortcuts::ShortcutTable;
use crate::spec::{Binding, CommandSpec};

/// Renders the command reference as Markdown: a summary table, then each
/// command with its description, permission class, contexts, default keys,
/// arguments, and result, then every type the schemas use, with its JSON
/// Schema.
///
/// The output depends only on the registry and the shortcut table, so the
/// file can be checked for drift.
///
/// # Errors
///
/// If a binding names a shortcut the table doesn't have, or two schemas
/// define different types under the same name. Run
/// [`Registry::problems`] and [`Registry::binding_problems`] first for the
/// full list of problems.
pub fn markdown(registry: &Registry, shortcuts: &ShortcutTable) -> Result<String, String> {
    markdown_with_intro(registry, shortcuts, INTRO)
}

/// The reference's opening, as [`markdown`] writes it: its title, and what
/// a command has. A program with its own wording passes it to
/// [`markdown_with_intro`].
pub const INTRO: &str = "\
# Command reference

Every action a person or an agent can take is a command in the registry. Menus, shortcuts, key \
and MIDI mapping, the agent interface, and tests all run commands by the IDs below, so an action \
has one implementation however it's reached.

Each command has:

- an **ID** that never changes, which key maps, MIDI maps, agents, and logs use;
- a **label**: what a person sees in menus and key-mapping lists;
- a **description** written for a model: what it does, when to use it, and what it changes;
- a **permission class**, which decides whether an agent asks first;
- **contexts**: the parts of the interface where a person reaches it;
- **default keys**: shortcuts from the shortcut reference, by ID, with any arguments they fix. \
The UI supplies the other arguments from the selection in the shortcut's context, a text field, \
or a gesture;
- **arguments** and a **result** as JSON Schemas generated from the Rust types. An agent sends \
the arguments as a JSON object. The types they use are under [Types](#types).
";

/// [`markdown`], opening with `intro` (Markdown, ending in a newline)
/// instead of [`INTRO`]. Everything after it is generated as [`markdown`]
/// generates it.
///
/// # Errors
///
/// As [`markdown`].
pub fn markdown_with_intro(
    registry: &Registry,
    shortcuts: &ShortcutTable,
    intro: &str,
) -> Result<String, String> {
    let mut types = Types::default();
    for spec in registry {
        for schema in spec.args.iter().chain(&spec.result) {
            types.collect(schema)?;
        }
        if let Some(result) = &spec.result {
            types.add_root(result)?;
        }
    }

    let mut out = String::new();
    header(&mut out, registry, intro);
    summary(&mut out, registry, shortcuts)?;
    out.push_str("\n## Commands\n");
    let mut area = "";
    for spec in registry {
        if spec.area() != area {
            area = spec.area();
            let _ = write!(out, "\n### Area `{area}`\n");
        }
        command(&mut out, spec, shortcuts)?;
    }
    types.render(&mut out);
    Ok(out)
}

fn header(out: &mut String, registry: &Registry, intro: &str) {
    out.push_str(intro);
    let areas: std::collections::BTreeSet<&str> = registry.iter().map(CommandSpec::area).collect();
    let _ = write!(
        out,
        "\n{} commands in {} areas.\n\n## Permission classes\n\n\
         | Class | What a command in it may do | An agent |\n|---|---|---|\n",
        registry.len(),
        areas.len()
    );
    for permission in Permission::ALL {
        let agent = if permission.asks_first() {
            "Asks the user first, unless the user has turned on autonomous mode for it"
        } else {
            "Runs it"
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | {agent} |",
            permission.id(),
            permission.meaning()
        );
    }
}

fn summary(out: &mut String, registry: &Registry, shortcuts: &ShortcutTable) -> Result<(), String> {
    out.push_str(
        "\n## Summary\n\n\
         | Command | Label | Permission | Default keys (macOS) | Default keys (Windows) |\n\
         |---|---|---|---|---|\n",
    );
    for spec in registry {
        let mut mac = Vec::new();
        let mut win = Vec::new();
        for binding in &spec.bindings {
            let shortcut = shortcuts
                .get(&binding.shortcut)
                .ok_or_else(|| format!("{}: unknown shortcut {}", spec.id, binding.shortcut))?;
            mac.push(keys(&shortcut.mac));
            win.push(keys(&shortcut.win));
        }
        let _ = writeln!(
            out,
            "| [`{id}`](#{anchor}) | {label} | `{permission}` | {mac} | {win} |",
            id = spec.id,
            anchor = anchor(&format!("`{}`", spec.id)),
            label = cell(&spec.label),
            permission = spec.permission.map_or("none", Permission::id),
            mac = mac.join("; "),
            win = win.join("; "),
        );
    }
    Ok(())
}

fn command(out: &mut String, spec: &CommandSpec, shortcuts: &ShortcutTable) -> Result<(), String> {
    let contexts: Vec<String> = spec.contexts.iter().map(|c| format!("`{c}`")).collect();
    let _ = write!(
        out,
        "\n#### `{id}`\n\n**{label}**. Permission: `{permission}`. Contexts: {contexts}.\n\n\
         {description}\n",
        id = spec.id,
        label = spec.label,
        permission = spec.permission.map_or("none", Permission::id),
        contexts = contexts.join(", "),
        description = spec.description.trim(),
    );

    if spec.bindings.is_empty() {
        out.push_str("\nDefault keys: none.\n");
    } else {
        out.push_str(
            "\nDefault keys:\n\n\
             | Shortcut | macOS | Windows | Context | Action in the reference | Binding |\n\
             |---|---|---|---|---|---|\n",
        );
        for binding in &spec.bindings {
            let shortcut = shortcuts
                .get(&binding.shortcut)
                .ok_or_else(|| format!("{}: unknown shortcut {}", spec.id, binding.shortcut))?;
            let _ = writeln!(
                out,
                "| {} | {} | {} | `{}` | {} | {} |",
                shortcut.id,
                keys(&shortcut.mac),
                keys(&shortcut.win),
                shortcut.context,
                cell(&shortcut.action),
                binding_note(binding),
            );
        }
    }

    let empty = Schema::default();
    let args = spec.args.as_ref().unwrap_or(&empty);
    let rows = property_rows(args.as_value(), args.as_value());
    if rows.is_empty() {
        out.push_str("\nArguments: none (send `{}`).\n");
    } else {
        out.push_str(
            "\nArguments:\n\n| Name | Type | Required | Description |\n|---|---|---|---|\n",
        );
        out.push_str(&rows);
    }

    let result = spec.result.as_ref().map_or_else(
        || "not described".to_owned(),
        |result| result_summary(result.as_value()),
    );
    let _ = writeln!(out, "\nResult: {result}.");
    Ok(())
}

fn binding_note(binding: &Binding) -> String {
    let mut parts: Vec<String> = binding
        .args
        .iter()
        .map(|(name, value)| format!("fixes `{name}` = `{value}`"))
        .collect();
    if let Some(arg) = &binding.toggles {
        parts.push(format!("toggles `{arg}`"));
    }
    if parts.is_empty() {
        "runs it".to_owned()
    } else {
        parts.join("; ")
    }
}

/// A table row per property of an object schema: required ones in the
/// schema's order (the Rust field order), then optional ones by name.
fn property_rows(schema: &Value, root: &Value) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return String::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut names: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| properties.contains_key(*name))
        .collect();
    names.extend(
        properties
            .keys()
            .map(String::as_str)
            .filter(|name| !required.contains(name)),
    );
    let mut rows = String::new();
    for name in names {
        let property = &properties[name];
        let _ = writeln!(
            rows,
            "| `{name}` | {} | {} | {} |",
            type_summary(property, root),
            if required.contains(&name) {
                "yes"
            } else {
                "no"
            },
            cell(description(property).unwrap_or_default()),
        );
    }
    rows
}

fn result_summary(schema: &Value) -> String {
    if schema.get("type").and_then(Value::as_str) == Some("null") {
        return "nothing (`null`)".to_owned();
    }
    match schema.get("title").and_then(Value::as_str) {
        Some(title) => format!("[`{title}`](#{})", anchor(&format!("`{title}`"))),
        None => type_summary(schema, schema),
    }
}

/// A short description of a schema's type, linking named types.
fn type_summary(schema: &Value, root: &Value) -> String {
    let Some(object) = schema.as_object() else {
        return "anything".to_owned();
    };
    if let Some(name) = object
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(ref_name)
    {
        return format!("[`{name}`](#{})", anchor(&format!("`{name}`")));
    }
    if let Some(value) = object.get("const") {
        return format!("`{value}`");
    }
    if let Some(Value::Array(options)) = object.get("enum") {
        let options: Vec<String> = options.iter().map(|o| format!("`{o}`")).collect();
        return format!("one of {}", options.join(", "));
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(options)) = object.get(key) {
            let consts: Option<Vec<String>> = options
                .iter()
                .map(|o| o.get("const").map(|c| format!("`{c}`")))
                .collect();
            if let Some(consts) = consts {
                return format!("one of {}", consts.join(", "));
            }
            let options: Vec<String> = options.iter().map(|o| type_summary(o, root)).collect();
            return options.join(" or ");
        }
    }
    if let Some(Value::Array(kinds)) = object.get("type") {
        let kinds: Vec<&str> = kinds.iter().filter_map(Value::as_str).collect();
        return kinds.join(" or ");
    }
    let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "integer" | "number" => {
            let name = if kind == "integer" {
                "integer"
            } else {
                "number"
            };
            let minimum = object.get("minimum").and_then(Value::as_f64);
            let maximum = object.get("maximum").and_then(Value::as_f64);
            match (minimum, maximum) {
                (Some(min), Some(max)) => format!("{name} from {} to {}", num(min), num(max)),
                (Some(min), None) => format!("{name}, at least {}", num(min)),
                (None, Some(max)) => format!("{name}, at most {}", num(max)),
                (None, None) => name.to_owned(),
            }
        }
        "array" => {
            if let Some(Value::Array(parts)) = object.get("prefixItems") {
                let parts: Vec<String> = parts.iter().map(|p| type_summary(p, root)).collect();
                return format!("pair of {}", parts.join(" and "));
            }
            let items = object
                .get("items")
                .map_or_else(|| "anything".to_owned(), |items| type_summary(items, root));
            let minimum = object.get("minItems").and_then(Value::as_u64).unwrap_or(0);
            if minimum > 0 {
                format!("array of {items}, at least {minimum}")
            } else {
                format!("array of {items}")
            }
        }
        "object" => {
            if let Some(values) = object
                .get("patternProperties")
                .and_then(Value::as_object)
                .and_then(|p| p.values().next())
            {
                format!("object of {} by integer key", type_summary(values, root))
            } else if object.contains_key("properties") {
                fields(schema, root)
            } else {
                "object".to_owned()
            }
        }
        "string" => {
            let minimum = object.get("minLength").and_then(Value::as_u64);
            let maximum = object.get("maxLength").and_then(Value::as_u64);
            let mut text = match (minimum, maximum) {
                (Some(min), Some(max)) if min == max => format!("string of {min} characters"),
                _ => "string".to_owned(),
            };
            if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
                let _ = write!(text, " matching `{pattern}`");
            }
            text
        }
        "" => "see the schema".to_owned(),
        other => other.to_owned(),
    }
}

fn num(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn description(schema: &Value) -> Option<&str> {
    schema.get("description").and_then(Value::as_str)
}

fn ref_name(reference: &str) -> Option<&str> {
    reference.strip_prefix("#/$defs/")
}

/// Every named type the schemas use, by name.
#[derive(Default)]
struct Types {
    defs: BTreeMap<String, Value>,
}

impl Types {
    /// Adds a schema's definitions.
    fn collect(&mut self, schema: &Schema) -> Result<(), String> {
        if let Some(Value::Object(defs)) = schema.get("$defs") {
            for (name, def) in defs {
                self.add(name, def.clone())?;
            }
        }
        Ok(())
    }

    /// Adds a root schema as a named type, under its title, if it has one
    /// and isn't `null`.
    fn add_root(&mut self, schema: &Schema) -> Result<(), String> {
        let Some(title) = schema.get("title").and_then(Value::as_str) else {
            return Ok(());
        };
        if schema.get("type").and_then(Value::as_str) == Some("null") {
            return Ok(());
        }
        let mut def: Map<String, Value> = schema.as_object().cloned().unwrap_or_default();
        for key in ["$schema", "$defs", "title"] {
            def.remove(key);
        }
        self.add(title, Value::Object(def))
    }

    fn add(&mut self, name: &str, def: Value) -> Result<(), String> {
        match self.defs.get(name) {
            Some(existing) if *existing != def => Err(format!(
                "two different types are both named `{name}` in the command schemas; rename one"
            )),
            Some(_) => Ok(()),
            None => {
                self.defs.insert(name.to_owned(), def);
                Ok(())
            }
        }
    }

    fn render(&self, out: &mut String) {
        out.push_str(
            "\n## Types\n\nThe types the arguments and results use, with their JSON Schemas. A \
             `$ref` such as `#/$defs/Ticks` names a type in this list.\n",
        );
        for (name, def) in &self.defs {
            let _ = write!(out, "\n### `{name}`\n");
            if let Some(text) = description(def) {
                let _ = write!(out, "\n{text}\n");
            }
            let shape = shape(def);
            if !shape.is_empty() {
                out.push('\n');
                out.push_str(&shape);
            }
            let json = serde_json::to_string_pretty(def).unwrap_or_default();
            let _ = write!(
                out,
                "\n<details><summary>JSON Schema</summary>\n\n```json\n{json}\n```\n\n</details>\n"
            );
        }
    }
}

/// What a type is made of: its fields, its variants, or its type.
fn shape(def: &Value) -> String {
    let rows = property_rows(def, def);
    if !rows.is_empty() {
        return format!("| Field | Type | Required | Description |\n|---|---|---|---|\n{rows}");
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(variants)) = def.get(key) {
            let mut out = String::from("One of:\n\n");
            for variant in variants {
                let what = match variant.get("const") {
                    Some(value) => format!("`{value}`"),
                    None => variant_fields(variant),
                };
                match description(variant) {
                    Some(text) => {
                        let _ = writeln!(out, "- {what}: {text}");
                    }
                    None => {
                        let _ = writeln!(out, "- {what}");
                    }
                }
            }
            return out;
        }
    }
    format!("Type: {}.\n", type_summary(def, def))
}

/// One variant of an enum, such as an object with `kind` = `"constant"`,
/// `start` (Ticks), `bpm` (Bpm).
fn variant_fields(variant: &Value) -> String {
    if variant.get("properties").is_some() {
        format!("an {}", fields(variant, variant))
    } else {
        type_summary(variant, variant)
    }
}

/// An object schema's fields in a line: tags (constant fields) first, then
/// required fields in order, then the rest.
fn fields(schema: &Value, root: &Value) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return "object".to_owned();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
    names.sort_by_key(|name| {
        let tag = properties[*name].get("const").is_none();
        let position = required
            .iter()
            .position(|r| r == name)
            .unwrap_or(usize::MAX);
        (tag, position)
    });
    let fields: Vec<String> = names
        .into_iter()
        .map(|name| {
            let field = &properties[name];
            match field.get("const") {
                Some(value) => format!("`{name}` = `{value}`"),
                None => format!("`{name}` ({})", type_summary(field, root)),
            }
        })
        .collect();
    format!("object with {}", fields.join(", "))
}

/// Keys as the shortcut tables show them.
fn keys(bindings: &[String]) -> String {
    if bindings.is_empty() {
        "none".to_owned()
    } else {
        bindings
            .iter()
            .map(|b| format!("`{b}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Text made safe for a table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// The anchor GitHub gives a heading with this text.
fn anchor(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::shortcuts::Shortcut;
    use crate::{CommandSpec, Context, NoArgs};

    #[test]
    fn anchors_follow_github() {
        assert_eq!(anchor("`mixer.set_volume`"), "mixerset_volume");
        assert_eq!(anchor("`ChangeSet`"), "changeset");
        assert_eq!(anchor("Area `clip`"), "area-clip");
    }

    #[test]
    fn types_are_summarised() {
        let root = json!({});
        for (schema, summary) in [
            (
                json!({ "$ref": "#/$defs/TrackId" }),
                "[`TrackId`](#trackid)",
            ),
            (
                json!({ "anyOf": [{ "$ref": "#/$defs/TrackId" }, { "type": "null" }] }),
                "[`TrackId`](#trackid) or null",
            ),
            (
                json!({ "type": "number", "minimum": -1.0, "maximum": 1.0 }),
                "number from -1 to 1",
            ),
            (
                json!({ "type": "integer", "minimum": 0 }),
                "integer, at least 0",
            ),
            (
                json!({ "type": "array", "items": { "$ref": "#/$defs/ClipId" } }),
                "array of [`ClipId`](#clipid)",
            ),
            (
                json!({ "oneOf": [{ "const": "audio" }, { "const": "midi" }] }),
                "one of `\"audio\"`, `\"midi\"`",
            ),
            (json!({ "type": "boolean" }), "boolean"),
        ] {
            assert_eq!(type_summary(&schema, &root), summary, "{schema}");
        }
    }

    #[test]
    fn a_reference_lists_commands_keys_and_types() {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        #[expect(dead_code, reason = "only the schema is used")]
        struct Args {
            /// Where it goes.
            place: Place,
            /// Whether it's on.
            on: Option<bool>,
        }
        /// A place.
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        #[expect(dead_code, reason = "only the schema is used")]
        struct Place(u32);

        let mut registry = Registry::new();
        registry
            .register(
                CommandSpec::new("edit.undo", "Undo")
                    .description("Undoes the newest step.")
                    .permission(Permission::Edit)
                    .context(Context::Global)
                    .binding("S-EDIT-006")
                    .args::<NoArgs>()
                    .result::<()>(),
            )
            .unwrap();
        registry
            .register(
                CommandSpec::new("place.set", "Set Place")
                    .description("Sets the place.")
                    .permission(Permission::Edit)
                    .context(Context::Arrangement)
                    .args::<Args>()
                    .result::<Place>(),
            )
            .unwrap();
        let table = ShortcutTable::new(
            ["global".to_owned()],
            [Shortcut {
                id: "S-EDIT-006".to_owned(),
                action: "Undo".to_owned(),
                context: "global".to_owned(),
                mac: vec!["Cmd+Z".to_owned()],
                win: vec!["Ctrl+Z".to_owned()],
                milestone: "M1".to_owned(),
            }],
        );
        let text = markdown(&registry, &table).unwrap();
        for expected in [
            "2 commands in 2 areas.",
            "| [`edit.undo`](#editundo) | Undo | `edit` | `Cmd+Z` | `Ctrl+Z` |",
            "| S-EDIT-006 | `Cmd+Z` | `Ctrl+Z` | `global` | Undo | runs it |",
            "Arguments: none (send `{}`).",
            "Result: nothing (`null`).",
            "| `place` | [`Place`](#place) | yes | Where it goes. |",
            "| `on` | boolean or null | no | Whether it's on. |",
            "Result: [`Place`](#place).",
            "### `Place`\n\nA place.\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }

        let unknown = ShortcutTable::new(["global".to_owned()], []);
        assert!(markdown(&registry, &unknown).is_err());
    }
}
