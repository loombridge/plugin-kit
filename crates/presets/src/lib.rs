//! Preset files: a plug-in's patch as documented, versioned JSON.
//!
//! A preset holds the parameters the person changed, each in its own units:
//! a continuous, gain or whole-number parameter as a number, a choice as the
//! name of the one chosen, a switch as `true` or `false`. Everything the
//! preset doesn't name takes its default when it loads. The plug-in's own
//! state — a modulation matrix, a wavetable, anything its parameters don't
//! hold — is JSON beside them, which the kit keeps without reading.
//!
//! ```json
//! {
//!   "format": "example-preset",
//!   "schema": 1,
//!   "params": [
//!     { "id": 1, "value": 0.5 },
//!     { "id": 2, "value": "Warm" },
//!     { "id": 3, "value": true }
//!   ],
//!   "state": { "whatever": "the plug-in keeps" }
//! }
//! ```
//!
//! `format` names the plug-in the preset is for, so one plug-in's file never
//! loads into another. `params` is sparse: a parameter at its default isn't
//! written, and a preset that names it anyway loads as if it weren't. The
//! values are in the parameter's own units, as a document holds them
//! ([`ParamDescriptor`](lspk_params::ParamDescriptor)).
//!
//! # Schema versions
//!
//! Every preset carries the version of the format it was written in,
//! [`SCHEMA`] now. When a change to the saved form would stop an older file
//! from loading as it is, the version goes up by one and a [`Migration`]
//! from the old version is added to [`MIGRATIONS`]. Loading runs every
//! migration from the file's version up to [`SCHEMA`] in turn, then reads
//! the result as the current version. A file above [`SCHEMA`] is a newer
//! build's; one below [`OLDEST_SCHEMA`] is older than this build reads.
//!
//! # A/B compare and undo
//!
//! [`Compare`] holds two patches side by side, as a plug-in's A/B compare
//! does, and [`History`] keeps the patches a patch has been, for undo and
//! redo.
//!
//! # Saving and loading
//!
//! [`save`] writes a [`ParamValues`] and a plug-in state as a preset's
//! text, and [`load`] reads one back, checked against the descriptors. A
//! plug-in that records edits as gestures applies the loaded values one by
//! one; [`apply`] sets them all at once, for a test or a headless tool.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lspk_params::{ParamDescriptor, ParamId, ParamSpace};
//! use lspk_plugin::ParamValues;
//! use lspk_presets::{apply, load, save};
//! use serde_json::json;
//!
//! let space = ParamSpace::builder()
//!     .global(ParamDescriptor::gain(ParamId(1), "Gain", 6.0, 0.0))
//!     .global(ParamDescriptor::choice(
//!         ParamId(2),
//!         "Mode",
//!         &["Clean", "Warm", "Hot"],
//!         0,
//!     ))
//!     .build()
//!     .expect("a space");
//! let values = ParamValues::with_space(space.descriptors(), Arc::new(space)).expect("values");
//! values.set(ParamId(1), 0.5);
//! values.set(ParamId(2), 1.0);
//!
//! let text = save("example-preset", &values, &json!({ "matrix": [] })).expect("saved");
//! let preset = load(&text, "example-preset", &values).expect("loaded");
//! assert_eq!(preset.value(ParamId(1)), Some(0.5));
//! assert_eq!(preset.value(ParamId(2)), Some(1.0));
//! assert_eq!(preset.state, json!({ "matrix": [] }));
//!
//! values.set(ParamId(1), 0.0);
//! values.set(ParamId(2), 2.0);
//! apply(&preset, &values);
//! assert_eq!(values.get(ParamId(1)), Some(0.5));
//! assert_eq!(values.get(ParamId(2)), Some(1.0));
//! ```

use core::fmt;

use lspk_params::{ParamId, ParamKind};
use lspk_plugin::ParamValues;
use serde::Deserialize;
use serde_json::{Value, json};

pub mod compare;
pub mod history;

pub use compare::{Compare, Side};
pub use history::History;

/// The schema this build writes, and the newest it can read.
pub const SCHEMA: u32 = 1;

/// The oldest schema version this build can read, through [`MIGRATIONS`].
pub const OLDEST_SCHEMA: u32 = SCHEMA - MIGRATIONS.len() as u32;

/// The largest preset read, in bytes: far more than any patch needs, so a
/// stray large file is refused rather than read.
pub const MAX_BYTES: usize = 16 << 20;

/// Every migration, oldest first: the one at index `i` reads version
/// [`OLDEST_SCHEMA`]` + i`.
pub const MIGRATIONS: &[Migration] = &[Migration {
    from: 0,
    summary: "a choice is written by its name and a switch as true or false, and each value \
              entry names its parameter beside it",
    apply: draft_to_one,
}];

/// One step from a schema version to the next.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// The version it reads. It writes `from + 1`.
    pub from: u32,
    /// What changed between the two versions, in a sentence.
    pub summary: &'static str,
    /// The migration itself: the whole saved JSON of version `from` and the
    /// plug-in's parameter values in, the JSON of version `from + 1` out,
    /// or a message saying why it can't be migrated.
    ///
    /// It must be a pure function of its inputs. It needn't touch the
    /// `schema` field, which the loader sets after each step.
    pub apply: fn(Value, &ParamValues) -> Result<Value, String>,
}

/// Schema 0 to 1: version 0, the format's first draft, wrote `params` as
/// `[id, value]` pairs and named a choice by its index, where version 1
/// writes each entry as `{ "id": …, "value": … }` and names the choice.
/// This rewrites a draft's values as the current form, looking each
/// parameter up so the entry is the choice its index held.
fn draft_to_one(mut value: Value, values: &ParamValues) -> Result<Value, String> {
    let object = value
        .as_object_mut()
        .ok_or("the preset isn't a JSON object")?;
    let params = match object.remove("params") {
        None => Vec::new(),
        Some(Value::Array(params)) => params,
        Some(_) => return Err("params isn't a list".to_owned()),
    };
    let mut migrated = Vec::with_capacity(params.len());
    for entry in params {
        let pair = entry
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or("a value entry isn't an [id, value] pair")?;
        let id = pair[0]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .ok_or("a value entry's parameter isn't a parameter ID")?;
        let descriptor = values
            .descriptor(ParamId(id))
            .ok_or_else(|| format!("no parameter {id} in this plug-in"))?;
        let written = match (&descriptor.kind, &pair[1]) {
            (ParamKind::Choice { choices }, Value::Number(number)) => {
                let at = number
                    .as_u64()
                    .and_then(|at| usize::try_from(at).ok())
                    .ok_or("a choice's value isn't a whole number")?;
                let name = choices
                    .get(at)
                    .ok_or_else(|| format!("{at} isn't a choice of {} ({id})", descriptor.name))?;
                json!(name)
            }
            (ParamKind::Toggle, Value::Number(number)) => {
                json!(number.as_f64().unwrap_or(0.0) >= 0.5)
            }
            (ParamKind::Choice { .. } | ParamKind::Toggle, _) => {
                return Err(format!(
                    "{} ({id}) isn't written as its index or 0 or 1",
                    descriptor.name
                ));
            }
            (_, Value::Number(_)) => pair[1].clone(),
            (_, _) => return Err(format!("{} ({id}) isn't a number", descriptor.name)),
        };
        migrated.push(json!({ "id": id, "value": written }));
    }
    object.insert("params".to_owned(), Value::Array(migrated));
    Ok(value)
}

/// Why a preset didn't save or load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    /// An error saying `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// What went wrong, in a sentence.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// A plug-in's patch, as a preset holds it, decoded.
///
/// `values` holds each parameter the preset sets, in the units the plug-in
/// works in: a number for a continuous, gain or whole-number parameter, a
/// choice's index, and 1.0 or 0.0 for a switch.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    /// Each parameter the preset sets, by ID, sorted by ID, none of them at
    /// its default.
    pub values: Vec<(ParamId, f64)>,
    /// The plug-in's own state, as JSON the kit doesn't read.
    pub state: Value,
}

impl Preset {
    /// A preset that sets nothing: every parameter at its default, and no
    /// state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            state: Value::Null,
        }
    }

    /// The patch `values` and `state` hold: every parameter that differs
    /// from its default, in [`values`](Self::values)' form.
    #[must_use]
    pub fn of(values: &ParamValues, state: Value) -> Self {
        let mut set = Vec::new();
        for (index, descriptor) in values.descriptors().iter().enumerate() {
            let value = values.value_at(index);
            if value != descriptor.default {
                set.push((descriptor.id, value));
            }
        }
        // A space's descriptors are in the hosts' order, which needn't be
        // their IDs' order; `values` is sorted by ID, as `value` reads it.
        set.sort_unstable_by_key(|(id, _)| *id);
        Self { values: set, state }
    }

    /// The value `id` is set to, if the preset names it.
    #[must_use]
    pub fn value(&self, id: ParamId) -> Option<f64> {
        self.values
            .binary_search_by_key(&id, |(id, _)| *id)
            .ok()
            .map(|at| self.values[at].1)
    }
}

impl Default for Preset {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes `values` and `state` as a preset in `format`: every parameter
/// whose value differs from its default, each in its own units, a choice by
/// its name and a switch as `true` or `false`.
///
/// # Errors
///
/// A value that isn't finite, or a choice whose value isn't one of its
/// choices.
pub fn save(format: &str, values: &ParamValues, state: &Value) -> Result<String, Error> {
    let mut params = Vec::new();
    for (index, descriptor) in values.descriptors().iter().enumerate() {
        let value = values.value_at(index);
        if value == descriptor.default {
            continue;
        }
        let written = match &descriptor.kind {
            ParamKind::Choice { choices } => {
                let at = value.round().max(0.0) as usize;
                let name = choices.get(at).ok_or_else(|| {
                    Error::new(format!(
                        "{} ({}) has the value {value}, which isn't one of its choices",
                        descriptor.name, descriptor.id.0
                    ))
                })?;
                json!(name)
            }
            ParamKind::Toggle => json!(value >= 0.5),
            _ if value.is_finite() => json!(value),
            _ => {
                return Err(Error::new(format!(
                    "{} ({}) has the value {value}",
                    descriptor.name, descriptor.id.0
                )));
            }
        };
        params.push(json!({ "id": descriptor.id, "value": written }));
    }
    let mut file = json!({ "format": format, "schema": SCHEMA, "params": params });
    if !state.is_null() {
        file["state"] = state.clone();
    }
    let mut text =
        serde_json::to_string_pretty(&file).map_err(|error| Error::new(error.to_string()))?;
    text.push('\n');
    Ok(text)
}

/// Reads a preset in `format` from `text`, checking every value against
/// `values`' descriptors and migrating an older schema first.
///
/// # Errors
///
/// `text` isn't a preset in `format`, is in a schema this build can't read,
/// or names a parameter, a choice or a kind of value that isn't the
/// plug-in's.
pub fn load(text: &str, format: &str, values: &ParamValues) -> Result<Preset, Error> {
    if text.len() > MAX_BYTES {
        return Err(Error::new(format!(
            "the preset is {} bytes, more than a preset holds",
            text.len()
        )));
    }
    let raw: Value = serde_json::from_str(text)
        .map_err(|error| Error::new(format!("it isn't JSON: {error}")))?;
    if raw.get("format").and_then(Value::as_str) != Some(format) {
        return Err(Error::new(format!("its format isn't \"{format}\"")));
    }
    let schema = raw
        .get("schema")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::new("it has no schema version"))?;
    let schema = u32::try_from(schema).unwrap_or(u32::MAX);
    if schema > SCHEMA {
        return Err(Error::new(format!(
            "a preset in schema {schema}, which this version reads up to {SCHEMA}"
        )));
    }
    let file: File = serde_json::from_value(migrate(raw, schema, values)?)
        .map_err(|error| Error::new(error.to_string()))?;
    let mut decoded: Vec<(ParamId, f64)> = Vec::with_capacity(file.params.len());
    for entry in &file.params {
        let descriptor = values
            .descriptor(entry.id)
            .ok_or_else(|| Error::new(format!("no parameter {} in this plug-in", entry.id.0)))?;
        let value = match (&descriptor.kind, &entry.value) {
            (ParamKind::Choice { choices }, Value::String(name)) => choices
                .iter()
                .position(|choice| choice == name)
                .map(|at| at as f64)
                .ok_or_else(|| {
                    Error::new(format!(
                        "\"{name}\" isn't a choice of {} ({})",
                        descriptor.name, entry.id.0
                    ))
                })?,
            (ParamKind::Choice { .. }, _) => {
                return Err(Error::new(format!(
                    "{} ({}) is one of its choices, by name",
                    descriptor.name, entry.id.0
                )));
            }
            (ParamKind::Toggle, Value::Bool(on)) => f64::from(*on),
            (ParamKind::Toggle, _) => {
                return Err(Error::new(format!(
                    "{} ({}) is true or false",
                    descriptor.name, entry.id.0
                )));
            }
            (_, Value::Number(number)) => number.as_f64().ok_or_else(|| {
                Error::new(format!(
                    "{} ({}) has a number that isn't one",
                    descriptor.name, entry.id.0
                ))
            })?,
            (_, _) => {
                return Err(Error::new(format!(
                    "{} ({}) is a number",
                    descriptor.name, entry.id.0
                )));
            }
        };
        let value = descriptor.clamp(value);
        if value != descriptor.default {
            decoded.push((entry.id, value));
        }
    }
    decoded.sort_unstable_by_key(|(id, _)| *id);
    if let Some(pair) = decoded.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(Error::new(format!(
            "parameter {} is written twice",
            pair[0].0.0
        )));
    }
    Ok(Preset {
        values: decoded,
        state: file.state,
    })
}

/// Applies `preset` to `values`: every parameter it names takes its value,
/// and every other its default.
pub fn apply(preset: &Preset, values: &ParamValues) {
    for descriptor in values.descriptors() {
        let value = preset.value(descriptor.id).unwrap_or(descriptor.default);
        values.set(descriptor.id, value);
    }
}

/// Runs the migrations from version `from` up to [`SCHEMA`], setting the
/// `schema` field after each step.
fn migrate(mut raw: Value, from: u32, values: &ParamValues) -> Result<Value, Error> {
    let mut version = from;
    while version < SCHEMA {
        let step = MIGRATIONS
            .iter()
            .find(|step| step.from == version)
            .ok_or_else(|| Error::new(format!("there's no migration from schema {version}")))?;
        raw = (step.apply)(raw, values)
            .map_err(|message| Error::new(format!("schema {version}: {message}")))?;
        let Some(object) = raw.as_object_mut() else {
            return Err(Error::new(format!(
                "the migration from schema {version} didn't return a JSON object"
            )));
        };
        object.insert("schema".to_owned(), Value::from(version + 1));
        version += 1;
    }
    Ok(raw)
}

/// The current form of a preset file, as [`load`] reads it.
#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    params: Vec<Entry>,
    #[serde(default)]
    state: Value,
}

/// One value in the current form.
#[derive(Debug, Deserialize)]
struct Entry {
    id: ParamId,
    value: Value,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lspk_params::{ParamDescriptor, ParamId, ParamSpace, ParamUnit};

    use super::*;

    const GAIN: ParamId = ParamId(1);
    const MODE: ParamId = ParamId(2);
    const ON: ParamId = ParamId(3);
    const VOICES: ParamId = ParamId(4);
    const AMOUNT: ParamId = ParamId(5);

    fn values() -> ParamValues {
        let space = ParamSpace::builder()
            .global(ParamDescriptor::gain(GAIN, "Gain", 6.0, 0.0))
            .global(ParamDescriptor::choice(
                MODE,
                "Mode",
                &["Clean", "Warm", "Hot"],
                1,
            ))
            .global(ParamDescriptor::toggle(ON, "On", true))
            .global(ParamDescriptor::integer(
                VOICES,
                "Voices",
                (1, 16),
                8,
                ParamUnit::None,
            ))
            .global(ParamDescriptor::continuous(
                AMOUNT,
                "Amount",
                (0.0, 100.0),
                50.0,
                ParamUnit::Percent,
            ))
            .build()
            .expect("a space");
        ParamValues::with_space(space.descriptors(), Arc::new(space)).expect("values")
    }

    fn loaded(text: &str) -> Preset {
        load(text, "test-preset", &values()).expect("a preset")
    }

    fn draft(params: Value) -> String {
        json!({
            "format": "test-preset",
            "schema": 0,
            "params": params,
            "state": { "kept": true },
        })
        .to_string()
    }

    #[test]
    fn the_migrations_form_one_chain_up_to_the_current_version() {
        for (offset, step) in (0..).zip(MIGRATIONS) {
            assert_eq!(
                step.from,
                OLDEST_SCHEMA + offset,
                "migration {offset} ({}) is out of order",
                step.summary
            );
            assert!(!step.summary.is_empty());
        }
    }

    #[test]
    fn a_preset_round_trips_every_kind_of_value() {
        let base = values();
        base.set(GAIN, 0.5);
        base.set(MODE, 2.0);
        base.set(ON, 0.0);
        base.set(VOICES, 3.0);
        base.set(AMOUNT, 25.0);
        let state = json!({ "matrix": [{ "slot": 0 }] });
        let text = save("test-preset", &base, &state).expect("saved");
        let preset = load(&text, "test-preset", &base).expect("loaded");
        assert_eq!(
            preset.values,
            vec![
                (GAIN, 0.5),
                (MODE, 2.0),
                (ON, 0.0),
                (VOICES, 3.0),
                (AMOUNT, 25.0)
            ]
        );
        assert_eq!(preset.state, state);
        assert_eq!(preset.value(GAIN), Some(0.5));
        assert_eq!(preset.value(ParamId(9)), None);
        // The values come back as the numbers a patch holds, and the same
        // preset saves to the same text.
        let back = values();
        apply(&preset, &back);
        for id in [GAIN, MODE, ON, VOICES, AMOUNT] {
            assert_eq!(back.get(id), base.get(id), "{id:?}");
        }
        assert_eq!(save("test-preset", &back, &state).unwrap(), text);
    }

    #[test]
    fn a_patch_as_a_preset_is_the_preset_its_text_saves() {
        let base = values();
        base.set(GAIN, 0.5);
        base.set(MODE, 2.0);
        let state = json!({ "matrix": [{ "slot": 0 }] });
        let built = Preset::of(&base, state.clone());
        assert_eq!(built.values, vec![(GAIN, 0.5), (MODE, 2.0)]);
        assert_eq!(built.state, state);
        // The text a patch writes loads back as that patch.
        let text = save("test-preset", &base, &state).expect("saved");
        assert_eq!(load(&text, "test-preset", &base).expect("loaded"), built);
    }

    #[test]
    fn a_patch_is_the_preset_sorted_by_id() {
        // A space whose descriptors aren't in their IDs' order — a host
        // lists a parameter where it likes: `of` sorts, so `value` finds
        // each.
        let space = ParamSpace::builder()
            .global(ParamDescriptor::choice(
                MODE,
                "Mode",
                &["Clean", "Warm", "Hot"],
                1,
            ))
            .global(ParamDescriptor::gain(GAIN, "Gain", 6.0, 0.0))
            .build()
            .expect("a space");
        let values = ParamValues::with_space(space.descriptors(), Arc::new(space)).expect("values");
        values.set(MODE, 2.0);
        values.set(GAIN, 0.5);
        let preset = Preset::of(&values, Value::Null);
        assert_eq!(preset.values, vec![(GAIN, 0.5), (MODE, 2.0)]);
        assert_eq!(preset.value(GAIN), Some(0.5));
        assert_eq!(preset.value(MODE), Some(2.0));
    }

    #[test]
    fn only_values_that_differ_from_their_defaults_are_written() {
        let base = values();
        let text = save("test-preset", &base, &Value::Null).expect("saved");
        let file: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(file["params"], json!([]));
        assert!(file.get("state").is_none(), "a null state isn't written");
        assert_eq!(loaded(&text).values, Vec::new());
        // A parameter set back to its default isn't written either, and a
        // preset that names it anyway loads as if it weren't.
        base.set(AMOUNT, 50.0);
        let text = save("test-preset", &base, &Value::Null).expect("saved");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["params"],
            json!([])
        );
        let named = json!({
            "format": "test-preset",
            "schema": 1,
            "params": [{ "id": AMOUNT.0, "value": 50.0 }],
        })
        .to_string();
        assert_eq!(loaded(&named).values, Vec::new());
    }

    #[test]
    fn choices_are_written_by_name_and_switches_as_booleans() {
        let base = values();
        base.set(MODE, 2.0);
        base.set(ON, 0.0);
        let text = save("test-preset", &base, &Value::Null).expect("saved");
        let file: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(
            file["params"],
            json!([
                { "id": MODE.0, "value": "Hot" },
                { "id": ON.0, "value": false },
            ])
        );
        // Every choice round-trips by its name; the default choice isn't
        // written at all.
        for (index, name) in ["Clean", "Warm", "Hot"].iter().enumerate() {
            let fresh = values();
            fresh.set(MODE, index as f64);
            let text = save("test-preset", &fresh, &Value::Null).expect("saved");
            if index == 1 {
                assert!(!text.contains(name), "{text}");
                assert_eq!(loaded(&text).value(MODE), None);
            } else {
                assert!(text.contains(name), "{text}");
                assert_eq!(loaded(&text).value(MODE), Some(index as f64));
            }
        }
    }

    #[test]
    fn an_older_schema_migrates() {
        // The draft wrote a choice by its index and a switch as 0 or 1.
        let text = draft(json!([
            [GAIN.0, 0.5],
            [MODE.0, 0],
            [ON.0, 0],
            [VOICES.0, 3],
            [AMOUNT.0, 25.0],
        ]));
        let preset = loaded(&text);
        assert_eq!(
            preset.values,
            vec![
                (GAIN, 0.5),
                (MODE, 0.0),
                (ON, 0.0),
                (VOICES, 3.0),
                (AMOUNT, 25.0)
            ]
        );
        assert_eq!(preset.state, json!({ "kept": true }));
        // The migrated preset is the current form, so it saves and loads
        // back the same.
        let back = values();
        apply(&preset, &back);
        let text = save("test-preset", &back, &preset.state).expect("saved");
        assert_eq!(loaded(&text), preset);
        // A draft that names a parameter that isn't there, or writes a
        // choice or a switch the wrong way, is refused.
        assert_eq!(loaded(&draft(json!([]))).values, Vec::new());
        assert!(load(&draft(json!([[99, 1.0]])), "test-preset", &values()).is_err());
        assert!(load(&draft(json!([[MODE.0, 9]])), "test-preset", &values()).is_err());
        assert!(load(&draft(json!([[MODE.0, "Warm"]])), "test-preset", &values()).is_err());
        assert!(load(&draft(json!([[7]])), "test-preset", &values()).is_err());
    }

    #[test]
    fn the_oldest_schema_is_the_draft() {
        assert_eq!(OLDEST_SCHEMA, 0);
        // A file in the oldest schema loads, which is what OLDEST_SCHEMA
        // promises.
        let text = draft(json!([[AMOUNT.0, 25.0]]));
        assert_eq!(loaded(&text).value(AMOUNT), Some(25.0));
    }

    #[test]
    fn a_preset_in_a_schema_this_build_cannot_read_is_refused() {
        let newer = json!({ "format": "test-preset", "schema": SCHEMA + 1, "params": [] });
        let error = load(&newer.to_string(), "test-preset", &values()).unwrap_err();
        assert!(error.message().contains("up to"), "{error}");
    }

    #[test]
    fn malformed_presets_are_refused() {
        let bad = [
            "",
            "{",
            "[]",
            "3",
            r#"{"schema": 1, "params": []}"#,
            r#"{"format": "other", "schema": 1}"#,
            r#"{"format": "test-preset"}"#,
            r#"{"format": "test-preset", "schema": "one"}"#,
            r#"{"format": "test-preset", "schema": 1, "params": 3}"#,
            r#"{"format": "test-preset", "schema": 1, "params": ["x"]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 1}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": "1", "value": 0.5}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 99, "value": 0.5}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 2, "value": "Loud"}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 2, "value": 1.0}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 2, "value": true}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 3, "value": 1.0}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 3, "value": "on"}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 1, "value": "x"}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 1, "value": null}]}"#,
            r#"{"format": "test-preset", "schema": 1,
                "params": [{"id": 1, "value": 0.5}, {"id": 1, "value": 0.25}]}"#,
        ];
        for text in bad {
            assert!(
                load(text, "test-preset", &values()).is_err(),
                "{text:?} loaded"
            );
        }
        // The messages name what's wrong.
        let errors: Vec<String> = [
            r#"{"schema": 1}"#,
            r#"{"format": "other", "schema": 1}"#,
            r#"{"format": "test-preset"}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 99, "value": 0.5}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 2, "value": "Loud"}]}"#,
            r#"{"format": "test-preset", "schema": 1, "params": [{"id": 3, "value": 1.0}]}"#,
        ]
        .iter()
        .map(|text| {
            load(text, "test-preset", &values())
                .unwrap_err()
                .to_string()
        })
        .collect();
        assert!(errors[0].contains("format"), "{}", errors[0]);
        assert!(errors[1].contains("format"), "{}", errors[1]);
        assert!(errors[2].contains("schema"), "{}", errors[2]);
        assert!(errors[3].contains("no parameter 99"), "{}", errors[3]);
        assert!(errors[4].contains("choice"), "{}", errors[4]);
        assert!(errors[5].contains("true or false"), "{}", errors[5]);
    }

    #[test]
    fn a_value_outside_its_range_loads_clamped_onto_its_step() {
        let named = json!({
            "format": "test-preset",
            "schema": 1,
            "params": [
                { "id": AMOUNT.0, "value": 1000.0 },
                { "id": VOICES.0, "value": 3.7 },
            ],
        })
        .to_string();
        assert_eq!(loaded(&named).values, vec![(VOICES, 4.0), (AMOUNT, 100.0)]);
    }

    #[test]
    fn a_preset_too_large_to_be_one_is_refused() {
        let text = format!(
            "{{\"format\": \"test-preset\", \"schema\": 1, \"params\": [], \"state\": \"{}\"}}",
            "x".repeat(MAX_BYTES)
        );
        let error = load(&text, "test-preset", &values()).unwrap_err();
        assert!(error.message().contains("more than"), "{error}");
    }
}
