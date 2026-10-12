//! Modulation destinations: the registry a plug-in fills with what can be
//! modulated, and how each one is evaluated.

use std::collections::HashMap;

use lspk_params::ParamId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::source::{Scope, SourceId};

/// What a route modulates: a parameter, by its ID in the plug-in's
/// parameter space, or one of an LFO's point-modulation buses.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// A parameter, by its ID.
    Param(ParamId),
    /// Point-modulation bus `bus` (0 to 15) of LFO `lfo` (0 to 9), which
    /// moves the points it drives (engine-core R94).
    LfoBus {
        /// The LFO, from 0.
        lfo: u8,
        /// The bus, from 0.
        bus: u8,
    },
}

/// How often a destination's routes are evaluated.
///
/// The rule (see the crate's documentation): a destination the ear follows
/// sample by sample (pitch, FM depth, amplitude, phase) is `Audio`; one
/// whose effect is smooth by nature (a filter's cutoff, a level, a
/// shaper's amount, an LFO's rate) is `Control`; one that is usually
/// smooth but can take audio-rate modulation (cutoff for filter FM) is
/// `Adaptive`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rate {
    /// Every sample.
    Audio,
    /// Every [`CONTROL_PERIOD`](crate::CONTROL_PERIOD) samples, ramped
    /// between ticks.
    Control,
    /// At audio rate while any route into it reads an
    /// [audio](crate::SourceKind::Audio) source, at control rate otherwise.
    Adaptive,
}

/// What a destination's modulated value feeds back into the matrix
/// (engine-core R111: routes can scale routes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feed {
    /// The destination is this held source's control (a macro): routes
    /// that read the source read the modulated value, and the source's held
    /// value is the destination's base.
    Source(SourceId),
    /// Slot `n`'s amount: the route in that slot takes the modulated
    /// amount, −1 to 1, and the slot's own amount is the base.
    Amount(u8),
    /// Slot `n`'s output scaler, 0 to 1, with the slot's own output as the
    /// base.
    Output(u8),
}

/// One destination: what it is, its scope, its rate and its range.
#[derive(Clone, Debug, PartialEq)]
pub struct DestinationInfo {
    /// What it modulates.
    pub target: Target,
    /// What a person reads.
    pub label: String,
    /// Per voice or global.
    pub scope: Scope,
    /// How often its routes run.
    pub rate: Rate,
    /// The lowest value the modulated result takes.
    pub min: f32,
    /// The highest.
    pub max: f32,
    /// What it feeds back into the matrix, if anything.
    pub feed: Option<Feed>,
}

impl DestinationInfo {
    /// Parameter `id`, global, at control rate, its modulated position on
    /// the control's travel kept within 0 to 1.
    #[must_use]
    pub fn param(id: ParamId, label: &str) -> Self {
        Self {
            target: Target::Param(id),
            label: label.to_owned(),
            scope: Scope::Global,
            rate: Rate::Control,
            min: 0.0,
            max: 1.0,
            feed: None,
        }
    }

    /// Point-modulation bus `bus` of LFO `lfo`, per voice, at control rate,
    /// from −1 to 1 around 0.
    #[must_use]
    pub fn lfo_bus(lfo: u8, bus: u8) -> Self {
        Self {
            target: Target::LfoBus { lfo, bus },
            label: format!("LFO {} bus {}", lfo + 1, bus + 1),
            scope: Scope::Voice,
            rate: Rate::Control,
            min: -1.0,
            max: 1.0,
            feed: None,
        }
    }

    /// Parameter `id`, the control of held source `source` (a macro).
    #[must_use]
    pub fn macro_control(id: ParamId, label: &str, source: SourceId) -> Self {
        Self::param(id, label).feeds(Feed::Source(source))
    }

    /// Parameter `id`, slot `slot`'s amount, −1 to 1.
    #[must_use]
    pub fn slot_amount(id: ParamId, label: &str, slot: u8) -> Self {
        Self::param(id, label)
            .bounds(-1.0, 1.0)
            .feeds(Feed::Amount(slot))
    }

    /// Parameter `id`, slot `slot`'s output scaler, 0 to 1.
    #[must_use]
    pub fn slot_output(id: ParamId, label: &str, slot: u8) -> Self {
        Self::param(id, label).feeds(Feed::Output(slot))
    }

    /// Per voice.
    #[must_use]
    pub fn voice(mut self) -> Self {
        self.scope = Scope::Voice;
        self
    }

    /// Global.
    #[must_use]
    pub fn global(mut self) -> Self {
        self.scope = Scope::Global;
        self
    }

    /// At audio rate.
    #[must_use]
    pub fn audio(mut self) -> Self {
        self.rate = Rate::Audio;
        self
    }

    /// At control rate.
    #[must_use]
    pub fn control(mut self) -> Self {
        self.rate = Rate::Control;
        self
    }

    /// At audio rate when a route into it reads audio.
    #[must_use]
    pub fn adaptive(mut self) -> Self {
        self.rate = Rate::Adaptive;
        self
    }

    /// Kept within `min` to `max`.
    #[must_use]
    pub fn bounds(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Not kept within any range, as a main tuning that modulation may take
    /// past its control's range (engine-core R137).
    #[must_use]
    pub fn unbounded(self) -> Self {
        self.bounds(f32::NEG_INFINITY, f32::INFINITY)
    }

    /// Feeding `feed` back into the matrix. A feeding destination runs at
    /// control rate.
    #[must_use]
    pub fn feeds(mut self, feed: Feed) -> Self {
        self.feed = Some(feed);
        self.rate = Rate::Control;
        self
    }
}

/// A destination's number in its [`Destinations`] registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DestId(pub u16);

impl DestId {
    /// Its index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The destinations a plug-in offers.
#[derive(Clone, Debug, Default)]
pub struct Destinations {
    list: Vec<DestinationInfo>,
    by_target: HashMap<Target, DestId>,
}

/// Why a destination couldn't be added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DestError {
    /// Another destination has this target.
    Duplicate(Target),
    /// Its range is empty or not a range.
    BadBounds(Target),
    /// It feeds a slot past the matrix's last.
    BadSlot(Target),
    /// The registry holds `u16::MAX` already.
    Full,
}

impl std::fmt::Display for DestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate(t) => write!(f, "a destination already has the target {t:?}"),
            Self::BadBounds(t) => write!(f, "{t:?}'s minimum must be below its maximum"),
            Self::BadSlot(t) => write!(f, "{t:?} feeds a slot the matrix doesn't have"),
            Self::Full => write!(f, "the registry is full"),
        }
    }
}

impl std::error::Error for DestError {}

impl Destinations {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `info` and returns its ID.
    ///
    /// # Errors
    ///
    /// Its target is taken, its range is empty, it feeds a slot that doesn't
    /// exist, or the registry is full.
    pub fn add(&mut self, info: DestinationInfo) -> Result<DestId, DestError> {
        if self.by_target.contains_key(&info.target) {
            return Err(DestError::Duplicate(info.target));
        }
        if info.min.is_nan() || info.max.is_nan() || info.min >= info.max {
            return Err(DestError::BadBounds(info.target));
        }
        if let Some(Feed::Amount(slot) | Feed::Output(slot)) = info.feed
            && usize::from(slot) >= crate::SLOTS
        {
            return Err(DestError::BadSlot(info.target));
        }
        if self.list.len() >= usize::from(u16::MAX) {
            return Err(DestError::Full);
        }
        let id = DestId(self.list.len() as u16);
        self.by_target.insert(info.target, id);
        self.list.push(info);
        Ok(id)
    }

    /// How many it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Destination `id`.
    #[must_use]
    pub fn get(&self, id: DestId) -> Option<&DestinationInfo> {
        self.list.get(id.index())
    }

    /// The destination for `target`.
    #[must_use]
    pub fn find(&self, target: Target) -> Option<DestId> {
        self.by_target.get(&target).copied()
    }

    /// Every destination, by ID.
    pub fn iter(&self) -> impl Iterator<Item = (DestId, &DestinationInfo)> {
        self.list
            .iter()
            .enumerate()
            .map(|(i, info)| (DestId(i as u16), info))
    }
}
