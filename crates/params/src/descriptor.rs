//! Parameter descriptors: what each of a device's parameters is, so an
//! engine can clamp and smooth it, a generic UI can draw it, presets can load
//! it, and automation and agents can reach it.
//!
//! A descriptor gives a parameter its range, its unit and how its value
//! reads, its default, and whether it can be automated. Two rules hold for
//! every descriptor ([`ParamDescriptor::check`]):
//!
//! - **G1.** Every gain and level reaches −∞ dB. A gain's value is a linear
//!   amplitude factor whose range starts at 0, which is −∞ dB, so silence
//!   is a value a document can hold (every value in it is finite).
//! - **G2.** Every parameter can be automated and modulated.
//!
//! Values are always in the parameter's own units, as a document holds
//! them. The control's travel, from 0 at the bottom to 1 at the top, is the
//! value's [normalised](ParamDescriptor::normalize) form.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A parameter of a device, numbered by the device.
///
/// Native devices number their parameters in their descriptors.
/// Plug-ins use their format's own parameter IDs, which for CLAP and VST3 are
/// 32-bit.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
#[schemars(
    description = "A device parameter's number. Native devices number their own parameters; \
                   plug-ins use their format's parameter IDs."
)]
pub struct ParamId(pub u32);

/// What one of a device's parameters is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParamDescriptor {
    /// The parameter's number in its device.
    pub id: ParamId,
    /// Its name, as its control's label and the Status Bar show it, such as
    /// `Gain`.
    pub name: String,
    /// What it does, for the Info View (WP-2.24) and agents.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What kind of value it takes, and its range.
    pub kind: ParamKind,
    /// The value a new device starts with, in the parameter's own units.
    pub default: f64,
    /// Whether automation and modulation can move it. Always true (rule
    /// G2); [`check`](Self::check) refuses a descriptor without it.
    pub automatable: bool,
}

/// What kind of value a parameter takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// A continuous value from `min` to `max`.
    Continuous {
        /// The lowest value.
        min: f64,
        /// The highest value.
        max: f64,
        /// What it measures.
        unit: ParamUnit,
        /// How the control's travel maps onto the range.
        taper: Taper,
        /// How many decimals its value shows; left out, its unit's rule
        /// (three significant figures for most, as Live 12.4.6 shows them).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decimals: Option<u8>,
    },
    /// A gain or level. Its value is a linear amplitude factor, from 0
    /// (−∞ dB, silence) to `max_db`'s factor, and it reads in dB. The
    /// control's travel is even in dB from `floor_db` at its bottom step to
    /// `max_db` at the top, and the very bottom is −∞ dB (rule G1).
    Gain {
        /// The highest gain, in dB.
        max_db: f64,
        /// The lowest finite gain on the control's travel, in dB.
        floor_db: f64,
    },
    /// On (1) or off (0).
    Toggle,
    /// One of a list of choices, by its index from 0.
    Choice {
        /// The choices' names, in order.
        choices: Vec<String>,
    },
    /// A whole number from `min` to `max`.
    Integer {
        /// The lowest value.
        min: i64,
        /// The highest value.
        max: i64,
        /// What it counts.
        unit: ParamUnit,
    },
}

/// What a parameter's value measures, which says how it reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamUnit {
    /// A plain number.
    #[default]
    None,
    /// Decibels, for values that are already in dB (a threshold, say);
    /// gains themselves are [`ParamKind::Gain`].
    Decibels,
    /// Hertz; from 1,000 it reads in kHz.
    Hertz,
    /// Milliseconds; from 1,000 it reads in seconds.
    Milliseconds,
    /// Seconds.
    Seconds,
    /// Per cent.
    Percent,
    /// Semitones.
    Semitones,
    /// Cents.
    Cents,
    /// A ratio, such as a compressor's, read as `4.0 : 1`.
    Ratio,
}

/// How a continuous parameter's control travel maps onto its range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Taper {
    /// Evenly in the value.
    #[default]
    Linear,
    /// Evenly in the value's logarithm, as frequencies and times are: each
    /// octave or doubling takes the same travel. Needs a range above 0.
    Logarithmic,
    /// As a power of the travel: the value is `min + (max − min) × nᵏ` at
    /// travel `n`, for times and rates whose knobs reach 0, such as an
    /// envelope's `32 s × n⁵`. `k` is 2 to 8.
    Power(u8),
}

/// The lowest finite gain a gain control's travel reaches by default, in
/// dB, as the mixer's fader does (observed in Live 12.4.6: −69.6 dB is the
/// fader's lowest finite value; below it, −∞).
pub const DEFAULT_FLOOR_DB: f64 = -70.0;

/// What a gain at 0 reads: minus infinity decibels.
pub const MINUS_INFINITY_DB: &str = "-inf dB";

/// Decibels to a linear amplitude factor.
#[must_use]
pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// A linear amplitude factor to decibels: −∞ for 0.
#[must_use]
pub fn gain_to_db(gain: f64) -> f64 {
    20.0 * gain.log10()
}

impl ParamDescriptor {
    fn new(id: ParamId, name: &str, kind: ParamKind, default: f64) -> Self {
        Self {
            id,
            name: name.to_owned(),
            description: String::new(),
            kind,
            default,
            automatable: true,
        }
    }

    /// A gain from −∞ dB to `max_db`, starting at `default_db` (which may be
    /// `f64::NEG_INFINITY`). Its value is a linear amplitude factor.
    #[must_use]
    pub fn gain(id: ParamId, name: &str, max_db: f64, default_db: f64) -> Self {
        let default = if default_db == f64::NEG_INFINITY {
            0.0
        } else {
            db_to_gain(default_db)
        };
        Self::new(
            id,
            name,
            ParamKind::Gain {
                max_db,
                floor_db: DEFAULT_FLOOR_DB,
            },
            default,
        )
    }

    /// A continuous value from `min` to `max` in `unit`, linear, with two
    /// decimals.
    #[must_use]
    pub fn continuous(
        id: ParamId,
        name: &str,
        (min, max): (f64, f64),
        default: f64,
        unit: ParamUnit,
    ) -> Self {
        Self::new(
            id,
            name,
            ParamKind::Continuous {
                min,
                max,
                unit,
                taper: Taper::Linear,
                decimals: None,
            },
            default,
        )
    }

    /// A switch, on or off.
    #[must_use]
    pub fn toggle(id: ParamId, name: &str, on: bool) -> Self {
        Self::new(id, name, ParamKind::Toggle, if on { 1.0 } else { 0.0 })
    }

    /// One of `choices`, starting at the one numbered `default`.
    #[must_use]
    pub fn choice(id: ParamId, name: &str, choices: &[&str], default: usize) -> Self {
        Self::new(
            id,
            name,
            ParamKind::Choice {
                choices: choices.iter().map(|c| (*c).to_owned()).collect(),
            },
            default as f64,
        )
    }

    /// A whole number from `min` to `max`.
    #[must_use]
    pub fn integer(
        id: ParamId,
        name: &str,
        (min, max): (i64, i64),
        default: i64,
        unit: ParamUnit,
    ) -> Self {
        Self::new(
            id,
            name,
            ParamKind::Integer { min, max, unit },
            default as f64,
        )
    }

    /// The same, with a logarithmic taper (a continuous parameter only).
    #[must_use]
    pub fn logarithmic(mut self) -> Self {
        if let ParamKind::Continuous { taper, .. } = &mut self.kind {
            *taper = Taper::Logarithmic;
        }
        self
    }

    /// The same, with a power taper: the value is `min + (max − min) × nᵏ`
    /// at travel `n`, `k` from 2 to 8 (a continuous parameter only).
    #[must_use]
    pub fn power(mut self, k: u8) -> Self {
        if let ParamKind::Continuous { taper, .. } = &mut self.kind {
            *taper = Taper::Power(k);
        }
        self
    }

    /// The same, always showing `count` decimals rather than its unit's
    /// rule (a continuous parameter only).
    #[must_use]
    pub fn decimals(mut self, count: u8) -> Self {
        if let ParamKind::Continuous { decimals, .. } = &mut self.kind {
            *decimals = Some(count);
        }
        self
    }

    /// The same, with its gain control's lowest finite step at `floor_db`
    /// (a gain only).
    #[must_use]
    pub fn floor_db(mut self, db: f64) -> Self {
        if let ParamKind::Gain { floor_db, .. } = &mut self.kind {
            *floor_db = db;
        }
        self
    }

    /// The same, with `text` describing what it does.
    #[must_use]
    pub fn describe(mut self, text: &str) -> Self {
        text.clone_into(&mut self.description);
        self
    }

    /// Its lowest value.
    #[must_use]
    pub fn min(&self) -> f64 {
        match &self.kind {
            ParamKind::Continuous { min, .. } => *min,
            ParamKind::Gain { .. } | ParamKind::Toggle | ParamKind::Choice { .. } => 0.0,
            ParamKind::Integer { min, .. } => *min as f64,
        }
    }

    /// Its highest value.
    #[must_use]
    pub fn max(&self) -> f64 {
        match &self.kind {
            ParamKind::Continuous { max, .. } => *max,
            ParamKind::Gain { max_db, .. } => db_to_gain(*max_db),
            ParamKind::Toggle => 1.0,
            ParamKind::Choice { choices } => choices.len().saturating_sub(1) as f64,
            ParamKind::Integer { max, .. } => *max as f64,
        }
    }

    /// Whether it's a gain or level (rule G1).
    #[must_use]
    pub fn is_gain(&self) -> bool {
        matches!(self.kind, ParamKind::Gain { .. })
    }

    /// Whether it takes whole steps (a switch, a choice, or a whole
    /// number), which change at once rather than glide.
    #[must_use]
    pub fn is_stepped(&self) -> bool {
        matches!(
            self.kind,
            ParamKind::Toggle | ParamKind::Choice { .. } | ParamKind::Integer { .. }
        )
    }

    /// `value` inside the range, on a step for a stepped parameter. A value
    /// that isn't a number gives the default.
    #[must_use]
    pub fn clamp(&self, value: f64) -> f64 {
        if value.is_nan() {
            return self.default;
        }
        let value = value.clamp(self.min(), self.max());
        if self.is_stepped() {
            value.round()
        } else {
            value
        }
    }

    /// Where `value` sits on the control's travel, from 0 to 1.
    #[must_use]
    pub fn normalize(&self, value: f64) -> f64 {
        let value = self.clamp(value);
        let (min, max) = (self.min(), self.max());
        let n = match &self.kind {
            ParamKind::Continuous {
                taper: Taper::Logarithmic,
                ..
            } => (value / min).ln() / (max / min).ln(),
            ParamKind::Continuous {
                taper: Taper::Power(k),
                ..
            } => ((value - min) / (max - min)).powf(1.0 / f64::from(*k)),
            ParamKind::Gain { max_db, floor_db } => {
                if value <= 0.0 {
                    0.0
                } else {
                    let db = gain_to_db(value);
                    let step = 1.0 / (max_db - floor_db);
                    // The bottom of the travel is −∞; the floor is one dB's
                    // worth of travel above it.
                    (step + (db - floor_db) / (max_db - floor_db) * (1.0 - step)).max(0.0)
                }
            }
            _ => {
                if max > min {
                    (value - min) / (max - min)
                } else {
                    0.0
                }
            }
        };
        n.clamp(0.0, 1.0)
    }

    /// The value at `position` on the control's travel, from 0 to 1.
    #[must_use]
    pub fn denormalize(&self, position: f64) -> f64 {
        let n = if position.is_nan() {
            0.0
        } else {
            position.clamp(0.0, 1.0)
        };
        let (min, max) = (self.min(), self.max());
        let value = match &self.kind {
            ParamKind::Continuous {
                taper: Taper::Logarithmic,
                ..
            } => min * (max / min).powf(n),
            ParamKind::Continuous {
                taper: Taper::Power(k),
                ..
            } => min + (max - min) * n.powi(i32::from(*k)),
            ParamKind::Gain { max_db, floor_db } => {
                let step = 1.0 / (max_db - floor_db);
                if n < step / 2.0 {
                    0.0
                } else {
                    let along = ((n - step) / (1.0 - step)).max(0.0);
                    db_to_gain(floor_db + along * (max_db - floor_db))
                }
            }
            _ => min + n * (max - min),
        };
        self.clamp(value)
    }

    /// How `value` reads, with its unit, as Live 12.4.6 shows values
    /// (observed): `-4.9 dB`, `0.00 dB`, `-inf dB`, `80.0 Hz`, `1.24 kHz`,
    /// `30.0 ms`, `4.00 : 1`, `21 %`, `On`, or a choice's name.
    #[must_use]
    pub fn format(&self, value: f64) -> String {
        let value = self.clamp(value);
        match &self.kind {
            ParamKind::Continuous { unit, decimals, .. } => {
                format_number(value, *unit, decimals.map(usize::from))
            }
            ParamKind::Gain { .. } => {
                if value <= 0.0 {
                    MINUS_INFINITY_DB.to_owned()
                } else {
                    format_number(gain_to_db(value), ParamUnit::Decibels, None)
                }
            }
            ParamKind::Toggle => if value >= 0.5 { "On" } else { "Off" }.to_owned(),
            ParamKind::Choice { choices } => {
                choices.get(value as usize).cloned().unwrap_or_default()
            }
            ParamKind::Integer { unit, .. } => format_number(value, *unit, Some(0)),
        }
    }

    /// The value `text` names, as typed into a control: a number with or
    /// without its unit, `-inf` for a gain's silence, `on` or `off`, or a
    /// choice's name. `None` if it names none.
    #[must_use]
    pub fn parse(&self, text: &str) -> Option<f64> {
        let text = text.trim().replace('−', "-");
        let lower = text.to_lowercase();
        let value = match &self.kind {
            ParamKind::Toggle => match lower.as_str() {
                "on" | "1" | "true" => 1.0,
                "off" | "0" | "false" => 0.0,
                _ => return None,
            },
            ParamKind::Choice { choices } => {
                if let Some(index) = choices.iter().position(|c| c.to_lowercase() == lower) {
                    index as f64
                } else {
                    lower.parse::<f64>().ok()?
                }
            }
            ParamKind::Gain { .. } => {
                let number = lower.trim_end_matches("db").trim();
                if matches!(number, "-inf" | "-∞" | "-infinity") {
                    0.0
                } else {
                    db_to_gain(number.parse::<f64>().ok().filter(|n| n.is_finite())?)
                }
            }
            ParamKind::Continuous { unit, .. } | ParamKind::Integer { unit, .. } => {
                parse_number(&lower, *unit)?
            }
        };
        value.is_finite().then(|| self.clamp(value))
    }

    /// Why the descriptor breaks a rule, if it does: a range that isn't
    /// finite or is empty, a default outside it, a logarithmic taper that
    /// reaches 0, a choice with no choices, or one that can't be automated
    /// (rule G2).
    ///
    /// # Errors
    ///
    /// The broken rule, naming the parameter.
    pub fn check(&self) -> Result<(), String> {
        let name = &self.name;
        let fail = |why: &str| Err(format!("parameter {} ({name}) {why}", self.id.0));
        if name.trim().is_empty() {
            return fail("has no name");
        }
        if !self.automatable {
            return fail("can't be automated; every parameter must be (rule G2)");
        }
        let (min, max) = (self.min(), self.max());
        if !(min.is_finite() && max.is_finite() && self.default.is_finite()) {
            return fail("has a range or default that isn't finite");
        }
        match &self.kind {
            ParamKind::Continuous { taper, .. } => {
                if min >= max {
                    return fail("has an empty range");
                }
                if *taper == Taper::Logarithmic && min <= 0.0 {
                    return fail("has a logarithmic taper reaching 0");
                }
                if let Taper::Power(k) = taper
                    && !(2..=8).contains(k)
                {
                    return fail("has a power taper outside 2 to 8");
                }
            }
            ParamKind::Gain { max_db, floor_db } => {
                if !(floor_db.is_finite() && max_db.is_finite() && floor_db < max_db) {
                    return fail("has a gain floor that isn't below its top");
                }
            }
            ParamKind::Choice { choices } => {
                if choices.is_empty() {
                    return fail("has no choices");
                }
            }
            ParamKind::Integer { .. } => {
                if min >= max {
                    return fail("has an empty range");
                }
            }
            ParamKind::Toggle => {}
        }
        if self.clamp(self.default) != self.default {
            return fail("has a default outside its range or between its steps");
        }
        Ok(())
    }
}

/// Decimals that show `value` to three significant figures, as Live shows
/// most values (observed: 1.00 ms, 30.0 ms, 80.0 Hz, 1.24 kHz, 4.00 : 1).
fn significant(value: f64) -> usize {
    if value == 0.0 {
        return 2;
    }
    let magnitude = value.abs().log10().floor() as i32;
    (2 - magnitude).clamp(0, 2) as usize
}

/// How a number reads in `unit`: `decimals` if given, otherwise the unit's
/// rule, observed in Live 12.4.6 (the device framework spec, R5).
fn format_number(value: f64, unit: ParamUnit, decimals: Option<usize>) -> String {
    let number = |v: f64, rule: usize| {
        let d = decimals.unwrap_or(rule);
        let text = format!("{v:.d$}");
        // No "-0.0".
        if text
            .trim_start_matches('-')
            .chars()
            .all(|c| c == '0' || c == '.')
        {
            text.trim_start_matches('-').to_owned()
        } else {
            text
        }
    };
    match unit {
        ParamUnit::None => number(value, 2),
        // Observed: 0.00 dB, -4.9 dB, 6.0 dB; the mixer's -21.0 dB.
        ParamUnit::Decibels => {
            let rule = if value.abs() < 0.995 { 2 } else { 1 };
            format!("{} dB", number(value, rule))
        }
        ParamUnit::Hertz if value.abs() >= 1000.0 => {
            let khz = value / 1000.0;
            format!("{} kHz", number(khz, significant(khz)))
        }
        ParamUnit::Hertz => format!("{} Hz", number(value, significant(value))),
        ParamUnit::Milliseconds if value.abs() >= 1000.0 => {
            let s = value / 1000.0;
            format!("{} s", number(s, significant(s)))
        }
        // Observed: 0 ms.
        ParamUnit::Milliseconds if value == 0.0 => format!("{} ms", number(value, 0)),
        ParamUnit::Milliseconds => format!("{} ms", number(value, significant(value))),
        ParamUnit::Seconds => format!("{} s", number(value, significant(value))),
        // Observed: 100 %, 21 %, 0.0 %.
        ParamUnit::Percent => {
            let rule = if value.abs() >= 9.95 { 0 } else { 1 };
            format!("{} %", number(value, rule))
        }
        ParamUnit::Semitones => format!("{} st", number(value, 0)),
        ParamUnit::Cents => format!("{} ct", number(value, 0)),
        ParamUnit::Ratio => format!("{} : 1", number(value, significant(value))),
    }
}

fn parse_number(text: &str, unit: ParamUnit) -> Option<f64> {
    let (number, scale) = match unit {
        ParamUnit::Hertz if text.ends_with("khz") => (text.trim_end_matches("khz"), 1000.0),
        ParamUnit::Hertz if text.ends_with('k') => (text.trim_end_matches('k'), 1000.0),
        ParamUnit::Hertz => (text.trim_end_matches("hz"), 1.0),
        ParamUnit::Milliseconds if text.ends_with("ms") => (text.trim_end_matches("ms"), 1.0),
        ParamUnit::Milliseconds if text.ends_with('s') => (text.trim_end_matches('s'), 1000.0),
        ParamUnit::Seconds => (text.trim_end_matches('s'), 1.0),
        ParamUnit::Decibels => (text.trim_end_matches("db"), 1.0),
        ParamUnit::Percent => (text.trim_end_matches('%'), 1.0),
        ParamUnit::Semitones => (text.trim_end_matches("st"), 1.0),
        ParamUnit::Cents => (text.trim_end_matches("ct"), 1.0),
        ParamUnit::Ratio => (text.trim_end_matches(": 1").trim_end_matches(":1"), 1.0),
        ParamUnit::Milliseconds | ParamUnit::None => (text, 1.0),
    };
    number
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map(|n| n * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> ParamId {
        ParamId(n)
    }

    // Rule G1.
    #[test]
    fn a_gain_reaches_minus_infinity_and_reads_in_db() {
        let gain = ParamDescriptor::gain(id(0), "Gain", 6.0, 0.0);
        gain.check().unwrap();
        assert_eq!(gain.min(), 0.0, "the bottom is silence");
        assert_eq!(gain.denormalize(0.0), 0.0);
        assert_eq!(gain.normalize(0.0), 0.0);
        assert_eq!(gain.format(0.0), MINUS_INFINITY_DB);
        assert_eq!(gain.format(1.0), "0.00 dB");
        assert_eq!(gain.format(db_to_gain(-4.9)), "-4.9 dB");
        assert_eq!(gain.format(db_to_gain(6.0)), "6.0 dB");
        assert_eq!(gain.parse("-inf"), Some(0.0));
        assert_eq!(gain.parse("−∞ dB"), Some(0.0));
        assert!((gain.parse("-6 dB").unwrap() - db_to_gain(-6.0)).abs() < 1e-12);
        assert!((gain.max() - db_to_gain(6.0)).abs() < 1e-12);
        assert!((gain.denormalize(1.0) - gain.max()).abs() < 1e-12);
        // The travel is even in dB above the floor.
        let half = gain.denormalize(gain.normalize(db_to_gain(-32.0)));
        assert!((gain_to_db(half) + 32.0).abs() < 1e-9);
    }

    #[test]
    fn a_power_taper_reaches_zero_and_follows_its_power() {
        // An envelope time: 32 s × n⁵, so the middle of the travel is 1 s.
        let time = ParamDescriptor::continuous(
            id(4),
            "Attack",
            (0.0, 32_000.0),
            1.0,
            ParamUnit::Milliseconds,
        )
        .power(5);
        time.check().unwrap();
        assert_eq!(time.denormalize(0.0), 0.0);
        assert!((time.denormalize(0.5) - 1000.0).abs() < 1e-9);
        assert!((time.normalize(1000.0) - 0.5).abs() < 1e-12);
        assert_eq!(time.denormalize(1.0), 32_000.0);
        for n in [0.0, 0.1, 0.5, 0.9, 1.0] {
            assert!((time.normalize(time.denormalize(n)) - n).abs() < 1e-12);
        }
        let json = serde_json::to_value(&time.kind).unwrap();
        assert_eq!(
            json["continuous"]["taper"],
            serde_json::json!({ "power": 5 })
        );
        assert!(time.clone().power(1).check().is_err());
        assert!(time.power(9).check().is_err());
    }

    #[test]
    fn values_round_trip_through_the_travel_and_the_text() {
        let freq = ParamDescriptor::continuous(
            id(1),
            "Frequency",
            (20.0, 20_000.0),
            1000.0,
            ParamUnit::Hertz,
        )
        .logarithmic();
        freq.check().unwrap();
        assert!(
            (freq.normalize(632.455_532) - 0.5).abs() < 1e-6,
            "octaves are even"
        );
        for n in [0.0, 0.1, 0.5, 0.9, 1.0] {
            assert!((freq.normalize(freq.denormalize(n)) - n).abs() < 1e-12);
        }
        // Observed in Live 12.4.6: three significant figures.
        assert_eq!(freq.format(80.0), "80.0 Hz");
        assert_eq!(freq.format(440.0), "440 Hz");
        assert_eq!(freq.format(1240.0), "1.24 kHz");
        assert_eq!(freq.format(10_000.0), "10.0 kHz");
        let ms = |v| {
            ParamDescriptor::continuous(id(5), "T", (0.0, 5000.0), 0.0, ParamUnit::Milliseconds)
                .format(v)
        };
        assert_eq!([ms(0.0), ms(1.0), ms(30.0)], ["0 ms", "1.00 ms", "30.0 ms"]);
        let pc = |v| {
            ParamDescriptor::continuous(id(5), "D", (0.0, 100.0), 0.0, ParamUnit::Percent).format(v)
        };
        assert_eq!([pc(100.0), pc(21.0), pc(0.0)], ["100 %", "21 %", "0.0 %"]);
        let ratio = ParamDescriptor::continuous(id(5), "R", (1.0, 100.0), 4.0, ParamUnit::Ratio);
        assert_eq!(ratio.format(4.0), "4.00 : 1");
        let plain = ParamDescriptor::continuous(id(5), "S", (0.0, 1.0), 0.0, ParamUnit::None);
        assert_eq!(plain.format(0.71), "0.71");
        assert_eq!(freq.parse("1.2 kHz"), Some(1200.0));
        assert_eq!(freq.parse("5"), Some(20.0), "clamped into the range");

        let mode = ParamDescriptor::choice(id(2), "Mode", &["Clean", "Warm", "Hot"], 1);
        mode.check().unwrap();
        assert_eq!(mode.format(2.0), "Hot");
        assert_eq!(mode.parse("warm"), Some(1.0));
        assert_eq!(mode.clamp(1.4), 1.0);
        assert_eq!(mode.denormalize(1.0), 2.0);

        let on = ParamDescriptor::toggle(id(3), "Mute", false);
        assert_eq!(on.format(1.0), "On");
        assert_eq!(on.parse("off"), Some(0.0));

        let semis =
            ParamDescriptor::integer(id(4), "Transpose", (-48, 48), 0, ParamUnit::Semitones);
        assert_eq!(semis.format(-12.0), "-12 st");
        assert_eq!(semis.parse("7 st"), Some(7.0));
    }

    // Rule G2.
    #[test]
    fn a_parameter_that_cant_be_automated_is_refused() {
        let mut gain = ParamDescriptor::gain(id(0), "Gain", 6.0, 0.0);
        gain.automatable = false;
        assert!(gain.check().unwrap_err().contains("G2"));
        let bad = ParamDescriptor::continuous(id(1), "Time", (0.0, 1.0), 2.0, ParamUnit::Seconds);
        assert!(bad.check().is_err(), "a default outside the range");
        let log = ParamDescriptor::continuous(id(1), "Time", (0.0, 1.0), 0.5, ParamUnit::Seconds)
            .logarithmic();
        assert!(log.check().is_err());
    }
}
