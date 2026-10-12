//! JSON Schemas for commands: generated from Rust types, cut out of a
//! command enum's schema, and checked against values.
//!
//! Every schema here is self-contained: the definitions it refers to sit
//! under `$defs` in the same schema, so the agent API can hand one command's
//! argument schema to a client as it is.

use std::collections::BTreeSet;

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The arguments of a command that takes none: an empty object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

/// The JSON Schema of `T` as a command's arguments: what `T`'s
/// deserialiser accepts.
#[must_use]
pub fn args_schema<T: JsonSchema>() -> Schema {
    root_schema::<T>(SchemaSettings::default().for_deserialize())
}

/// The JSON Schema of `T` as a command's result: what `T`'s serialiser
/// writes.
#[must_use]
pub fn result_schema<T: JsonSchema>() -> Schema {
    root_schema::<T>(SchemaSettings::default().for_serialize())
}

fn root_schema<T: JsonSchema>(settings: SchemaSettings) -> Schema {
    let mut schema = settings.into_generator().into_root_schema_for::<T>();
    if let Some(object) = schema.as_object_mut() {
        object.values_mut().for_each(unwrap_descriptions);
        if let Some(Value::String(text)) = object.get_mut("description") {
            *text = unwrap_lines(text);
        }
    }
    schema
}

/// Joins the lines of each paragraph in every `description`, so the text
/// an agent reads doesn't carry the line breaks of the doc comments it came
/// from. Blank lines between paragraphs stay.
fn unwrap_descriptions(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                match value {
                    Value::String(text) if key == "description" => *text = unwrap_lines(text),
                    _ => unwrap_descriptions(value),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(unwrap_descriptions),
        _ => {}
    }
}

fn unwrap_lines(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| {
            paragraph
                .split('\n')
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One variant's schema, cut out of the schema of an enum that serde tags
/// internally (`#[serde(tag = "...")]`), with the tag itself removed.
///
/// A provider whose commands are variants of one enum, as the document's
/// are, gets each command's argument schema this way, so the schema always
/// matches what deserialising the enum accepts. The result keeps the
/// variant's description and the definitions it uses, and nothing else.
///
/// Returns `None` if no variant has `tag` set to `value`.
#[must_use]
pub fn tagged_variant(enum_schema: &Schema, tag: &str, value: &str) -> Option<Schema> {
    let variant = enum_schema
        .get("oneOf")?
        .as_array()?
        .iter()
        .find(|variant| {
            variant
                .pointer(&format!("/properties/{tag}/const"))
                .is_some_and(|tagged| tagged == value)
        })?;
    let mut object = variant.as_object()?.clone();
    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        properties.remove(tag);
    }
    if let Some(Value::Array(required)) = object.get_mut("required") {
        required.retain(|name| name != tag);
    }
    if let Some(dialect) = enum_schema.get("$schema") {
        object.insert("$schema".to_owned(), dialect.clone());
    }
    if let Some(Value::Object(defs)) = enum_schema.get("$defs") {
        let used = used_defs(&Value::Object(object.clone()), defs);
        let kept: Map<String, Value> = defs
            .iter()
            .filter(|(name, _)| used.contains(name.as_str()))
            .map(|(name, def)| (name.clone(), def.clone()))
            .collect();
        if !kept.is_empty() {
            object.insert("$defs".to_owned(), Value::Object(kept));
        }
    }
    Some(Schema::from(object))
}

/// The names of the definitions `schema` refers to, directly or through
/// other definitions.
fn used_defs<'a>(schema: &Value, defs: &'a Map<String, Value>) -> BTreeSet<&'a str> {
    let mut used = BTreeSet::new();
    let mut pending = Vec::new();
    collect_refs(schema, &mut pending);
    while let Some(name) = pending.pop() {
        if let Some((name, def)) = defs.get_key_value(&name)
            && used.insert(name.as_str())
        {
            collect_refs(def, &mut pending);
        }
    }
    used
}

fn collect_refs(schema: &Value, out: &mut Vec<String>) {
    match schema {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "$ref"
                    && let Some(name) = value.as_str().and_then(def_name)
                {
                    out.push(name);
                } else {
                    collect_refs(value, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
        _ => {}
    }
}

/// The definition a local reference such as `#/$defs/Clip` names.
fn def_name(reference: &str) -> Option<String> {
    let name = reference.strip_prefix("#/$defs/")?;
    Some(name.replace("~1", "/").replace("~0", "~"))
}

/// The schema of one property of an object schema, if it has that property.
#[must_use]
pub fn property<'a>(schema: &'a Schema, name: &str) -> Option<&'a Value> {
    schema.get("properties")?.get(name)
}

/// Checks a value against a schema, and says where it fails.
///
/// This covers the keywords the registry's schemas use: `$ref` to local
/// definitions, `type`, `const`, `enum`, `oneOf`, `anyOf`, `allOf`,
/// `properties`, `required`, `additionalProperties`, `patternProperties`
/// for integer keys, `items`, `prefixItems`, the item counts, and the
/// numeric and length limits. It doesn't check `pattern` or `format`.
/// Deserialising remains the final word on what a command accepts; this is
/// for tests, and for checking the arguments a binding fixes.
///
/// # Errors
///
/// A message naming the first place the value fails, such as
/// `clip.data.midi.notes[0].key: 200 is above the maximum 127`.
pub fn validate(schema: &Schema, value: &Value) -> Result<(), String> {
    let root = schema.as_value();
    check(root, root, value, "value")
}

/// Checks `value` against `schema`, whose `$ref`s resolve in `root`.
///
/// # Errors
///
/// As for [`validate`].
pub fn validate_in(root: &Schema, schema: &Value, value: &Value) -> Result<(), String> {
    check(root.as_value(), schema, value, "value")
}

fn check(root: &Value, schema: &Value, value: &Value, at: &str) -> Result<(), String> {
    let object = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(format!("{at}: nothing is allowed here")),
        Value::Object(object) => object,
        _ => return Err(format!("{at}: the schema here isn't an object")),
    };

    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        let target = def_name(reference)
            .and_then(|name| root.get("$defs")?.get(name))
            .ok_or_else(|| format!("{at}: unresolved reference {reference}"))?;
        check(root, target, value, at)?;
    }
    if let Some(types) = object.get("type") {
        let allowed: Vec<&str> = match types {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !allowed.iter().any(|name| has_type(value, name)) {
            return Err(format!(
                "{at}: expected {}, found {value}",
                allowed.join(" or ")
            ));
        }
    }
    if let Some(expected) = object.get("const")
        && value != expected
    {
        return Err(format!("{at}: expected {expected}, found {value}"));
    }
    if let Some(Value::Array(options)) = object.get("enum")
        && !options.contains(value)
    {
        return Err(format!(
            "{at}: {value} isn't one of {}",
            Value::Array(options.clone())
        ));
    }
    if let Some(Value::Array(all)) = object.get("allOf") {
        for part in all {
            check(root, part, value, at)?;
        }
    }
    if let Some(Value::Array(any)) = object.get("anyOf")
        && !any.iter().any(|part| check(root, part, value, at).is_ok())
    {
        return Err(first_failure(
            root,
            any,
            value,
            at,
            "matches none of the options",
        ));
    }
    if let Some(Value::Array(one)) = object.get("oneOf") {
        let matching = one
            .iter()
            .filter(|part| check(root, part, value, at).is_ok())
            .count();
        match matching {
            1 => {}
            0 => {
                return Err(first_failure(
                    root,
                    one,
                    value,
                    at,
                    "matches none of the variants",
                ));
            }
            _ => return Err(format!("{at}: {value} matches more than one variant")),
        }
    }

    match value {
        Value::Object(fields) => check_object(root, object, fields, at),
        Value::Array(items) => check_array(root, object, items, at),
        Value::Number(number) => {
            let Some(number) = number.as_f64() else {
                return Ok(());
            };
            let limit = |key: &str| object.get(key).and_then(Value::as_f64);
            if let Some(minimum) = limit("minimum")
                && number < minimum
            {
                return Err(format!("{at}: {value} is below the minimum {minimum}"));
            }
            if let Some(maximum) = limit("maximum")
                && number > maximum
            {
                return Err(format!("{at}: {value} is above the maximum {maximum}"));
            }
            if let Some(minimum) = limit("exclusiveMinimum")
                && number <= minimum
            {
                return Err(format!("{at}: {value} isn't above {minimum}"));
            }
            if let Some(maximum) = limit("exclusiveMaximum")
                && number >= maximum
            {
                return Err(format!("{at}: {value} isn't below {maximum}"));
            }
            Ok(())
        }
        Value::String(text) => {
            let length = text.chars().count();
            let limit = |key: &str| object.get(key).and_then(Value::as_u64);
            if let Some(minimum) = limit("minLength")
                && (length as u64) < minimum
            {
                return Err(format!("{at}: shorter than {minimum} characters"));
            }
            if let Some(maximum) = limit("maxLength")
                && (length as u64) > maximum
            {
                return Err(format!("{at}: longer than {maximum} characters"));
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) => Ok(()),
    }
}

fn first_failure(root: &Value, options: &[Value], value: &Value, at: &str, what: &str) -> String {
    let reasons: Vec<String> = options
        .iter()
        .filter_map(|part| check(root, part, value, at).err())
        .collect();
    format!("{at}: {value} {what} ({})", reasons.join("; "))
}

fn check_object(
    root: &Value,
    schema: &Map<String, Value>,
    fields: &Map<String, Value>,
    at: &str,
) -> Result<(), String> {
    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(Value::Array(required)) = schema.get("required") {
        for name in required.iter().filter_map(Value::as_str) {
            if !fields.contains_key(name) {
                return Err(format!("{at}: missing `{name}`"));
            }
        }
    }
    let patterns = schema.get("patternProperties").and_then(Value::as_object);
    for (name, field) in fields {
        let path = format!("{at}.{name}");
        if let Some(property) = properties.and_then(|p| p.get(name)) {
            check(root, property, field, &path)?;
            continue;
        }
        let mut matched = false;
        for (pattern, property) in patterns.into_iter().flatten() {
            if pattern_matches(pattern, name) {
                matched = true;
                check(root, property, field, &path)?;
            }
        }
        if matched {
            continue;
        }
        match schema.get("additionalProperties") {
            None | Some(Value::Bool(true)) => {}
            Some(Value::Bool(false)) => return Err(format!("{at}: unknown field `{name}`")),
            Some(extra) => check(root, extra, field, &path)?,
        }
    }
    Ok(())
}

/// Whether a property name matches a `patternProperties` pattern. Only the
/// integer-key pattern schemars writes for maps keyed by numbers is
/// understood; any other pattern is taken to match.
fn pattern_matches(pattern: &str, name: &str) -> bool {
    if pattern == r"^-?\d+$" {
        let digits = name.strip_prefix('-').unwrap_or(name);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    } else {
        true
    }
}

fn check_array(
    root: &Value,
    schema: &Map<String, Value>,
    items: &[Value],
    at: &str,
) -> Result<(), String> {
    let count = items.len() as u64;
    if let Some(minimum) = schema.get("minItems").and_then(Value::as_u64)
        && count < minimum
    {
        return Err(format!("{at}: fewer than {minimum} items"));
    }
    if let Some(maximum) = schema.get("maxItems").and_then(Value::as_u64)
        && count > maximum
    {
        return Err(format!("{at}: more than {maximum} items"));
    }
    let prefix = schema
        .get("prefixItems")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for (index, item) in items.iter().enumerate() {
        let path = format!("{at}[{index}]");
        if let Some(part) = prefix.get(index) {
            check(root, part, item, &path)?;
        } else if let Some(rest) = schema.get("items") {
            check(root, rest, item, &path)?;
        }
    }
    Ok(())
}

fn has_type(value: &Value, name: &str) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A tagged enum like the document's commands.
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(tag = "op", rename_all = "snake_case")]
    enum Edit {
        /// Renames a thing.
        Rename {
            /// The thing.
            thing: Thing,
            /// The new name.
            name: String,
        },
        /// Moves a thing, with a description that runs
        /// over two lines.
        Move {
            /// The thing.
            thing: Thing,
            /// Where to.
            #[schemars(range(min = 0, max = 10))]
            to: u32,
            /// A group to move it into.
            group: Option<Group>,
        },
    }

    /// A thing's ID.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Thing(u64);

    /// A group's ID.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Group(u64);

    #[test]
    fn variants_are_cut_out_with_their_definitions() {
        let schema = args_schema::<Edit>();
        let rename = tagged_variant(&schema, "op", "rename").unwrap();
        assert_eq!(rename.get("description").unwrap(), "Renames a thing.");
        assert!(property(&rename, "op").is_none());
        assert!(property(&rename, "name").is_some());
        assert_eq!(rename.get("required").unwrap(), &json!(["thing", "name"]));
        let defs: Vec<_> = rename
            .get("$defs")
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(defs, ["Thing"], "only the definitions it uses");

        let moved = tagged_variant(&schema, "op", "move").unwrap();
        let defs: Vec<_> = moved
            .get("$defs")
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(defs, ["Group", "Thing"]);
        assert!(moved.get("title").is_none());
        assert!(moved.get("$schema").is_some());

        assert!(tagged_variant(&schema, "op", "delete").is_none());
        assert!(tagged_variant(&schema, "kind", "rename").is_none());
    }

    #[test]
    fn validation_follows_the_schema() {
        let moved = tagged_variant(&args_schema::<Edit>(), "op", "move").unwrap();
        assert_eq!(validate(&moved, &json!({ "thing": 3, "to": 4 })), Ok(()));
        assert_eq!(
            validate(&moved, &json!({ "thing": 3, "to": 4, "group": 9 })),
            Ok(())
        );
        assert_eq!(
            validate(&moved, &json!({ "thing": 3, "to": 4, "group": null })),
            Ok(())
        );
        for (bad, reason) in [
            (json!({ "to": 4 }), "value: missing `thing`"),
            (
                json!({ "thing": "3", "to": 4 }),
                "value.thing: expected integer",
            ),
            (
                json!({ "thing": 3, "to": 11 }),
                "value.to: 11 is above the maximum 10",
            ),
            (
                json!({ "thing": 3, "to": 1.5 }),
                "value.to: expected integer",
            ),
            (json!([]), "value: expected object"),
        ] {
            let error = validate(&moved, &bad).unwrap_err();
            assert!(error.starts_with(reason), "{bad}: {error}");
        }
        assert!(
            validate(&args_schema::<NoArgs>(), &json!({ "extra": 1 }))
                .unwrap_err()
                .contains("unknown field `extra`")
        );
        assert_eq!(validate(&args_schema::<NoArgs>(), &json!({})), Ok(()));
    }

    #[test]
    fn descriptions_lose_their_line_breaks() {
        assert_eq!(
            unwrap_lines("One\ntwo.\n\nThree\n four."),
            "One two.\n\nThree four."
        );
        let schema = args_schema::<Edit>();
        let text = serde_json::to_string(&schema).unwrap();
        assert!(!text.contains("\\n"), "{text}");
    }

    #[test]
    fn integer_keys_match_the_map_pattern() {
        assert!(pattern_matches(r"^-?\d+$", "12"));
        assert!(pattern_matches(r"^-?\d+$", "-3"));
        assert!(!pattern_matches(r"^-?\d+$", "x"));
        assert!(!pattern_matches(r"^-?\d+$", "-"));
    }
}
