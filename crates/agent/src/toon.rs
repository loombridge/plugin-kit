//! An encoder for TOON (Token-Oriented Object Notation), version 4.4 of
//! its specification (<https://github.com/toon-format/spec>, `SPEC.md`).
//!
//! Encoding only: a model reads TOON here and never writes it back. The
//! encoder follows the specification with two exceptions:
//!
//! - An object of uniform objects (the keyed tabular form, section 9.5) is
//!   written as a nested object, which any decoder reads the same.
//! - Where an array's items share one shape and an item holds a nested
//!   object of primitives, the object's fields become dotted columns
//!   (`mixer.volume`) instead of the specification's nested field group
//!   (`mixer{volume,pan}`), so a table of tracks reads as a plain table.
//!
//! Every header declares the default delimiter, a comma.

use std::fmt::Write as _;

use serde_json::{Map, Number, Value};

/// A tabular array's rows, at least how many of them make TOON worth using
/// for a result (a short table saves less than its header costs).
pub const MIN_TABLE_ROWS: usize = 8;

/// A value written as TOON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toon {
    /// The document, without a trailing newline.
    pub text: String,
    /// The row count of the longest table in it, or 0 if it has none.
    pub longest_table: usize,
}

/// `value` as a TOON document.
#[must_use]
pub fn encode(value: &Value) -> Toon {
    let mut encoder = Encoder {
        lines: Vec::new(),
        longest_table: 0,
    };
    match value {
        Value::Object(map) => encoder.object(map, 0),
        Value::Array(items) => encoder.array("", items, 0),
        primitive => encoder.lines.push((0, primitive_token(primitive))),
    }
    let mut text = String::new();
    for (depth, line) in &encoder.lines {
        if !text.is_empty() {
            text.push('\n');
        }
        text.extend(std::iter::repeat_n(' ', depth * 2));
        text.push_str(line);
    }
    Toon {
        text,
        longest_table: encoder.longest_table,
    }
}

/// The columns of a table: a primitive, or an object of columns.
enum Shape {
    Leaf,
    Group(Vec<(String, Shape)>),
}

impl Shape {
    /// The shape of `value` as a column, or `None` if it's an array or an
    /// empty object.
    fn of(value: &Value) -> Option<Self> {
        match value {
            Value::Array(_) => None,
            Value::Object(map) if map.is_empty() => None,
            Value::Object(map) => map
                .iter()
                .map(|(key, value)| Some((key.clone(), Self::of(value)?)))
                .collect::<Option<_>>()
                .map(Self::Group),
            _ => Some(Self::Leaf),
        }
    }

    /// Appends the dotted name of each leaf column below `prefix`.
    fn names(&self, prefix: &str, out: &mut Vec<String>) {
        match self {
            Self::Leaf => out.push(prefix.to_owned()),
            Self::Group(fields) => {
                for (key, shape) in fields {
                    let name = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    shape.names(&name, out);
                }
            }
        }
    }

    /// Appends `value`'s leaf cells if it has exactly this shape.
    fn cells(&self, value: &Value, out: &mut Vec<String>) -> bool {
        match (self, value) {
            (Self::Leaf, Value::Array(_) | Value::Object(_)) => false,
            (Self::Leaf, primitive) => {
                out.push(primitive_token(primitive));
                true
            }
            (Self::Group(fields), Value::Object(map)) => {
                fields.len() == map.len()
                    && fields
                        .iter()
                        .all(|(key, shape)| map.get(key).is_some_and(|v| shape.cells(v, out)))
            }
            (Self::Group(_), _) => false,
        }
    }
}

struct Encoder {
    /// Each line with its depth.
    lines: Vec<(usize, String)>,
    longest_table: usize,
}

impl Encoder {
    fn object(&mut self, map: &Map<String, Value>, depth: usize) {
        for (key, value) in map {
            let key = key_token(key);
            match value {
                Value::Array(items) => self.array(&key, items, depth),
                Value::Object(inner) if inner.is_empty() => {
                    self.lines.push((depth, format!("{key}:")));
                }
                Value::Object(inner) => {
                    self.lines.push((depth, format!("{key}:")));
                    self.object(inner, depth + 1);
                }
                primitive => self
                    .lines
                    .push((depth, format!("{key}: {}", primitive_token(primitive)))),
            }
        }
    }

    /// An array under `key` (empty at the root).
    fn array(&mut self, key: &str, items: &[Value], depth: usize) {
        if items.is_empty() {
            let line = if key.is_empty() {
                "[]".to_owned()
            } else {
                format!("{key}: []")
            };
            self.lines.push((depth, line));
        } else if let Some(cells) = inline(items) {
            self.lines
                .push((depth, format!("{key}[{}]: {cells}", items.len())));
        } else if let Some((names, rows)) = table(items) {
            self.longest_table = self.longest_table.max(rows.len());
            self.lines.push((
                depth,
                format!("{key}[{}]{{{}}}:", items.len(), names.join(",")),
            ));
            self.lines
                .extend(rows.into_iter().map(|row| (depth + 1, row)));
        } else {
            self.lines.push((depth, format!("{key}[{}]:", items.len())));
            self.list(items, depth + 1);
        }
    }

    /// The items of an array in list form, each at `depth`.
    fn list(&mut self, items: &[Value], depth: usize) {
        for item in items {
            match item {
                Value::Array(inner) if inner.is_empty() => {
                    self.lines.push((depth, "- [0]:".to_owned()));
                }
                Value::Array(inner) => {
                    if let Some(cells) = inline(inner) {
                        self.lines
                            .push((depth, format!("- [{}]: {cells}", inner.len())));
                    } else {
                        self.lines.push((depth, format!("- [{}]:", inner.len())));
                        self.list(inner, depth + 1);
                    }
                }
                Value::Object(map) if map.is_empty() => self.lines.push((depth, "-".to_owned())),
                Value::Object(map) => {
                    // The first field stands on the hyphen line; the rest
                    // and anything its scope opens keep their depths.
                    let start = self.lines.len();
                    self.object(map, depth + 1);
                    let (_, first) = &mut self.lines[start];
                    *first = format!("- {first}");
                    self.lines[start].0 = depth;
                }
                primitive => self
                    .lines
                    .push((depth, format!("- {}", primitive_token(primitive)))),
            }
        }
    }
}

/// The comma-joined cells of an array of primitives, or `None` if any item
/// is not one.
fn inline(items: &[Value]) -> Option<String> {
    items
        .iter()
        .map(|item| (!item.is_array() && !item.is_object()).then(|| primitive_token(item)))
        .collect::<Option<Vec<_>>>()
        .map(|cells| cells.join(","))
}

/// The column names and the rows of an array whose items are objects of
/// one shape, or `None` if it isn't one.
fn table(items: &[Value]) -> Option<(Vec<String>, Vec<String>)> {
    let shape = Shape::of(&items[0]).filter(|shape| matches!(shape, Shape::Group(_)))?;
    let mut names = Vec::new();
    shape.names("", &mut names);
    // A literal key `a.b` beside a nested `a: { b }` would name two columns alike.
    let mut sorted = names.iter().collect::<Vec<_>>();
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return None;
    }
    let rows = items
        .iter()
        .map(|item| {
            let mut cells = Vec::with_capacity(names.len());
            shape.cells(item, &mut cells).then(|| cells.join(","))
        })
        .collect::<Option<_>>()?;
    Some((names.iter().map(|name| key_token(name)).collect(), rows))
}

/// A key, quoted unless it is an identifier with dots (section 7.3).
fn key_token(key: &str) -> String {
    let mut chars = key.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if plain { key.to_owned() } else { quoted(key) }
}

fn primitive_token(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number(n),
        Value::String(s) => string_token(s),
        Value::Array(_) | Value::Object(_) => unreachable!("callers pass primitives only"),
    }
}

/// A number in canonical decimal form (section 2); outside 1e-6 to 1e21 an
/// exponent, as the JSON grammar writes it.
fn number(n: &Number) -> String {
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    if f == 0.0 {
        "0".to_owned()
    } else if (1e-6..1e21).contains(&f.abs()) {
        f.to_string()
    } else {
        let exponent = format!("{f:e}");
        if exponent.contains("e-") {
            exponent
        } else {
            exponent.replace('e', "e+")
        }
    }
}

/// A string, quoted where section 7.2 requires it.
fn string_token(s: &str) -> String {
    if needs_quotes(s) {
        quoted(s)
    } else {
        s.to_owned()
    }
}

fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.starts_with([' ', '\t', '-', '#', '\u{feff}'])
        || s.ends_with([' ', '\t'])
        || matches!(s, "true" | "false" | "null")
        || numeric_like(s)
        || s.chars()
            .any(|c| c.is_control() && c <= '\u{1f}' || ":\"\\[]{},".contains(c))
}

/// Whether `s` matches `^[+-]?[0-9]+(\.[0-9]+)?(e[+-]?[0-9]+)?$`, any case.
fn numeric_like(s: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exponent) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e.strip_prefix(['+', '-']).unwrap_or(e))),
        None => (s, None),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (mantissa, None),
    };
    digits(whole) && fraction.is_none_or(digits) && exponent.is_none_or(digits)
}

fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c <= '\u{1f}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn toon(value: &Value) -> String {
        encode(value).text
    }

    // Most examples below are the specification's own (Appendix A). Without
    // serde_json's `preserve_order`, an object's keys come out sorted, so
    // columns and fields are in key order here.

    #[test]
    fn objects_and_nested_objects() {
        assert_eq!(
            toon(&json!({ "id": 123, "name": "Ada", "active": true })),
            "active: true\nid: 123\nname: Ada"
        );
        assert_eq!(
            toon(&json!({ "user": { "id": 123, "name": "Ada" } })),
            "user:\n  id: 123\n  name: Ada"
        );
    }

    #[test]
    fn primitive_arrays_are_inline() {
        assert_eq!(
            toon(&json!({ "tags": ["admin", "ops", "dev"] })),
            "tags[3]: admin,ops,dev"
        );
        assert_eq!(toon(&json!({ "tags": [] })), "tags: []");
        assert_eq!(toon(&json!([])), "[]");
        assert_eq!(toon(&json!([1, 2])), "[2]: 1,2");
    }

    #[test]
    fn arrays_of_primitive_arrays_are_a_list() {
        assert_eq!(
            toon(&json!({ "pairs": [[1, 2], [3, 4]] })),
            "pairs[2]:\n  - [2]: 1,2\n  - [2]: 3,4"
        );
        assert_eq!(toon(&json!({ "p": [[]] })), "p[1]:\n  - [0]:");
    }

    #[test]
    fn uniform_objects_are_a_table() {
        let encoded = encode(&json!({
            "items": [
                { "sku": "A1", "qty": 2, "price": 9.99 },
                { "sku": "B2", "qty": 1, "price": 14.5 },
            ]
        }));
        assert_eq!(
            encoded.text,
            "items[2]{price,qty,sku}:\n  9.99,2,A1\n  14.5,1,B2"
        );
        assert_eq!(encoded.longest_table, 2);
    }

    #[test]
    fn nested_objects_of_primitives_become_dotted_columns() {
        let encoded = encode(&json!({
            "orders": [
                { "id": 1, "customer": { "name": "Ada", "country": "DK" }, "total": 99 },
                { "id": 2, "customer": { "name": "Bob", "country": "UK" }, "total": 149 },
            ]
        }));
        assert_eq!(
            encoded.text,
            "orders[2]{customer.country,customer.name,id,total}:\n  DK,Ada,1,99\n  UK,Bob,2,149"
        );
    }

    #[test]
    fn items_that_differ_are_a_list() {
        assert_eq!(
            toon(&json!({ "items": [1, { "a": 1 }, "text"] })),
            "items[3]:\n  - 1\n  - a: 1\n  - text"
        );
        // A different key set, an array in a column, and an empty object.
        assert_eq!(
            toon(&json!({ "items": [{ "id": 1 }, { "id": 2, "label": "x" }] })),
            "items[2]:\n  - id: 1\n  - id: 2\n    label: x"
        );
        assert_eq!(
            toon(&json!({ "items": [{ "a": [1] }, { "a": [2] }] })),
            "items[2]:\n  - a[1]: 1\n  - a[1]: 2"
        );
        assert_eq!(toon(&json!({ "items": [{}, {}] })), "items[2]:\n  -\n  -");
    }

    #[test]
    fn objects_as_list_items_keep_the_first_field_on_the_hyphen() {
        assert_eq!(
            toon(&json!({ "items": [
                { "id": 1, "name": "First" },
                { "id": 2, "name": "Second", "extra": true },
            ] })),
            "items[2]:\n  - id: 1\n    name: First\n  - extra: true\n    id: 2\n    name: Second"
        );
    }

    #[test]
    fn a_table_inside_a_list_item() {
        assert_eq!(
            toon(&json!({ "items": [
                { "users": [{ "id": 1, "name": "Ada" }, { "id": 2, "name": "Bob" }] },
                { "status": "active" },
            ] })),
            "items[2]:\n  - users[2]{id,name}:\n      1,Ada\n      2,Bob\n  - status: active"
        );
    }

    #[test]
    fn strings_are_quoted_only_where_needed() {
        let quoted = [
            "", " a", "a ", "true", "null", "42", "-3.14", "05", "+1", "1e-6", "a:b", "a\"b",
            "a\\b", "[x]", "{x}", "a,b", "-x", "#x", "a\nb",
        ];
        for s in quoted {
            assert!(string_token(s).starts_with('"'), "{s:?} should be quoted");
        }
        for s in [
            "Ada",
            "Hello 世界 👋",
            "a b",
            "1.2.3",
            "x-y",
            "e5",
            "1e",
            "tag_1",
        ] {
            assert_eq!(string_token(s), s, "{s:?} should be plain");
        }
        assert_eq!(string_token("a\nb\t\u{1}\"\\"), r#""a\nb\t\u0001\"\\""#);
    }

    #[test]
    fn keys_are_quoted_unless_identifiers() {
        assert_eq!(
            toon(&json!({ "my-key": [1, 2, 3] })),
            "\"my-key\"[3]: 1,2,3"
        );
        assert_eq!(
            toon(&json!({ "x-items": [{ "id": 1, "name": "Ada" }, { "id": 2, "name": "Bob" }] })),
            "\"x-items\"[2]{id,name}:\n  1,Ada\n  2,Bob"
        );
        assert_eq!(
            toon(&json!({ "a.b": 1, "": 2, "2x": 3 })),
            "\"\": 2\n\"2x\": 3\na.b: 1"
        );
    }

    #[test]
    fn numbers_are_canonical() {
        assert_eq!(
            toon(&json!({ "a": 1.0, "b": -0.0, "c": 0.1, "d": 9_007_199_254_740_992_u64 })),
            "a: 1\nb: 0\nc: 0.1\nd: 9007199254740992"
        );
        assert_eq!(
            toon(&json!({ "a": 1e21, "b": 1.5e-7, "c": 1e20 })),
            "a: 1e+21\nb: 1.5e-7\nc: 100000000000000000000"
        );
    }

    #[test]
    fn empty_objects_and_the_longest_table() {
        assert_eq!(toon(&json!({})), "");
        assert_eq!(toon(&json!({ "a": {} })), "a:");
        let rows = (0..9).map(|i| json!({ "i": i })).collect::<Vec<_>>();
        let encoded = encode(&json!({ "a": [{ "x": 1 }], "b": { "c": rows } }));
        assert_eq!(encoded.longest_table, 9);
        assert!(encoded.text.contains("\n  c[9]{i}:\n    0\n"));
    }

    #[test]
    fn literal_dotted_keys_that_clash_with_a_nested_column_fall_back_to_a_list() {
        let value = json!({ "t": [
            { "a": { "b": 1 }, "a.b": 2 },
            { "a": { "b": 3 }, "a.b": 4 },
        ] });
        assert!(toon(&value).starts_with("t[2]:\n  - a:\n      b: 1"));
    }
}
