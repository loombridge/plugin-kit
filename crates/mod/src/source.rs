//! Modulation sources: the registry a plug-in fills with what can modulate.

use std::collections::HashMap;

/// The most sources a registry holds.
pub const MAX_SOURCES: usize = 128;

/// A source's number in its [`Sources`] registry, in the order it was added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u16);

impl SourceId {
    /// Its index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Whether something has a value for each voice or one for the whole
/// instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// One value per voice: an envelope, a voice's LFO, velocity.
    Voice,
    /// One value for every voice: a macro, the mod wheel, a shared LFO.
    Global,
}

/// The values a source takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceRange {
    /// 0 to 1: envelopes, LFOs (engine-core R93), velocity, macros.
    Unipolar,
    /// −1 to 1: pitch bend, MPE X, audio.
    Bipolar,
}

/// Where a source's value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// A smooth per-sample signal the plug-in renders each block and binds
    /// with [`Signals`](crate::Signals): an envelope, an LFO, an LFO's Y.
    Signal,
    /// Audio the plug-in renders each block and binds: an oscillator, the
    /// noise, a filter. A route from audio runs an
    /// [adaptive](crate::Rate::Adaptive) destination at audio rate.
    Audio,
    /// A value the plug-in sets between calls with
    /// [`Engine::set_held`](crate::Engine::set_held): a macro, the mod
    /// wheel, pressure, pitch bend, MPE expression, a sequencer's value.
    Held,
    /// The velocity curve's output for the voice's note (engine-core R99).
    Velocity,
    /// The key curve's output for the voice's note (engine-core R99).
    Key,
    /// The note's release velocity, taken at note-off (engine-core R110).
    ReleaseVelocity,
    /// A new random value at each note-on, the same for every route using
    /// it (engine-core R110).
    Random,
    /// 0 or 1, flipping at each note-on; `inverse` is the other one.
    Alternate {
        /// Whether this is the inverse of the first alternate.
        inverse: bool,
    },
    /// A new random value at each note-on, different for each route.
    RandomPerRoute,
    /// Active voices ÷ the voice limit.
    VoiceLoad,
    /// A constant 1, scaled by the route's amount.
    Constant,
}

impl SourceKind {
    /// Whether the engine makes this source's value itself.
    #[must_use]
    pub const fn is_engine_owned(self) -> bool {
        !matches!(self, Self::Signal | Self::Audio | Self::Held)
    }
}

/// One source: its key, label and what kind of value it gives.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    /// Its stable key, as commands and presets name it: `lfo3`.
    pub key: String,
    /// What a person reads: `LFO 3`.
    pub label: String,
    /// Where its value comes from.
    pub kind: SourceKind,
    /// Per voice or global.
    pub scope: Scope,
    /// Its values' range.
    pub range: SourceRange,
}

impl SourceInfo {
    /// A source of `kind` with its usual scope and range, which
    /// [`voice`](Self::voice), [`global`](Self::global) and
    /// [`bipolar`](Self::bipolar) change.
    #[must_use]
    pub fn new(key: &str, label: &str, kind: SourceKind) -> Self {
        let scope = match kind {
            SourceKind::Held | SourceKind::VoiceLoad | SourceKind::Constant => Scope::Global,
            _ => Scope::Voice,
        };
        let range = match kind {
            SourceKind::Audio => SourceRange::Bipolar,
            _ => SourceRange::Unipolar,
        };
        Self {
            key: key.to_owned(),
            label: label.to_owned(),
            kind,
            scope,
            range,
        }
    }

    /// Per voice. Engine-made sources keep their own scope.
    #[must_use]
    pub fn voice(mut self) -> Self {
        if !self.kind.is_engine_owned() {
            self.scope = Scope::Voice;
        }
        self
    }

    /// Global. Engine-made sources keep their own scope.
    #[must_use]
    pub fn global(mut self) -> Self {
        if !self.kind.is_engine_owned() {
            self.scope = Scope::Global;
        }
        self
    }

    /// Bipolar, −1 to 1.
    #[must_use]
    pub fn bipolar(mut self) -> Self {
        self.range = SourceRange::Bipolar;
        self
    }
}

/// How many of each kind [`Sources::standard`] makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Standard<'a> {
    /// Envelopes (`env1`…).
    pub envelopes: usize,
    /// LFOs (`lfo1`…), each with a Y output (`lfo1_y`…) for its XY path.
    pub lfos: usize,
    /// Macros (`macro1`…).
    pub macros: usize,
    /// Voice-sequencer values (`seq_value1`…), with the sequence index.
    pub sequence_values: usize,
    /// Audio sources as (key, label): oscillators, noise, filters.
    pub audio: &'a [(&'a str, &'a str)],
}

/// The sources a plug-in offers, each with a [`SourceId`] in the order
/// added.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    list: Vec<SourceInfo>,
    by_key: HashMap<String, SourceId>,
}

/// Why a source couldn't be added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// Another source has this key.
    DuplicateKey(String),
    /// The registry holds [`MAX_SOURCES`] already.
    Full,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateKey(key) => write!(f, "a source already has the key {key:?}"),
            Self::Full => write!(f, "a registry holds at most {MAX_SOURCES} sources"),
        }
    }
}

impl std::error::Error for SourceError {}

impl Sources {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `info` and returns its ID.
    ///
    /// # Errors
    ///
    /// The key is taken, or the registry is full.
    pub fn add(&mut self, info: SourceInfo) -> Result<SourceId, SourceError> {
        if self.by_key.contains_key(&info.key) {
            return Err(SourceError::DuplicateKey(info.key));
        }
        if self.list.len() >= MAX_SOURCES {
            return Err(SourceError::Full);
        }
        let id = SourceId(self.list.len() as u16);
        self.by_key.insert(info.key.clone(), id);
        self.list.push(info);
        Ok(id)
    }

    /// The modulation sources of a wavetable synthesiser's matrix
    /// (engine-core section 12), in its order: envelopes, LFOs and their Y
    /// outputs, the velocity and key curves, channel and poly pressure,
    /// note-on randoms and alternates, macros, the mod wheel, pitch bend,
    /// MPE X, Y and Z, release velocity, the constant, the audio sources,
    /// voice load, the sequencer's values and index, and a random value per
    /// route.
    ///
    /// # Panics
    ///
    /// If `standard` asks for more than [`MAX_SOURCES`] or repeats an audio
    /// key.
    #[must_use]
    pub fn standard(standard: &Standard<'_>) -> Self {
        use SourceKind as K;
        let mut all = Vec::new();
        for n in 1..=standard.envelopes {
            all.push(SourceInfo::new(
                &format!("env{n}"),
                &format!("Envelope {n}"),
                K::Signal,
            ));
        }
        for n in 1..=standard.lfos {
            all.push(SourceInfo::new(
                &format!("lfo{n}"),
                &format!("LFO {n}"),
                K::Signal,
            ));
            all.push(SourceInfo::new(
                &format!("lfo{n}_y"),
                &format!("LFO {n} Y"),
                K::Signal,
            ));
        }
        all.push(SourceInfo::new("velocity", "Velocity curve", K::Velocity));
        all.push(SourceInfo::new("key", "Key curve", K::Key));
        all.push(SourceInfo::new(
            "channel_pressure",
            "Channel pressure",
            K::Held,
        ));
        all.push(SourceInfo::new("poly_pressure", "Poly pressure", K::Held).voice());
        for n in 1..=2 {
            all.push(SourceInfo::new(
                &format!("random{n}"),
                &format!("Random {n}"),
                K::Random,
            ));
        }
        all.push(SourceInfo::new(
            "alternate1",
            "Alternate 1",
            K::Alternate { inverse: false },
        ));
        all.push(SourceInfo::new(
            "alternate2",
            "Alternate 2",
            K::Alternate { inverse: true },
        ));
        for n in 1..=standard.macros {
            all.push(SourceInfo::new(
                &format!("macro{n}"),
                &format!("Macro {n}"),
                K::Held,
            ));
        }
        all.push(SourceInfo::new("mod_wheel", "Mod wheel", K::Held));
        all.push(SourceInfo::new("pitch_bend", "Pitch bend", K::Held).bipolar());
        all.push(SourceInfo::new("mpe_x", "MPE X", K::Held).voice().bipolar());
        all.push(SourceInfo::new("mpe_y", "MPE Y", K::Held).voice());
        all.push(SourceInfo::new("mpe_z", "MPE Z", K::Held).voice());
        all.push(SourceInfo::new(
            "release_velocity",
            "Release velocity",
            K::ReleaseVelocity,
        ));
        all.push(SourceInfo::new("constant", "Constant", K::Constant));
        for (key, label) in standard.audio {
            all.push(SourceInfo::new(key, label, K::Audio));
        }
        all.push(SourceInfo::new("voice_load", "Voice load", K::VoiceLoad));
        for n in 1..=standard.sequence_values {
            all.push(
                SourceInfo::new(
                    &format!("seq_value{n}"),
                    &format!("Sequence value {n}"),
                    K::Held,
                )
                .voice(),
            );
        }
        if standard.sequence_values > 0 {
            all.push(SourceInfo::new("seq_index", "Sequence index", K::Held).voice());
        }
        all.push(SourceInfo::new(
            "random_route",
            "Random per route",
            K::RandomPerRoute,
        ));
        let mut sources = Self::new();
        for info in all {
            sources
                .add(info)
                .expect("the standard sources fit and have unique keys");
        }
        sources
    }

    /// How many sources it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Source `id`, if it exists.
    #[must_use]
    pub fn get(&self, id: SourceId) -> Option<&SourceInfo> {
        self.list.get(id.index())
    }

    /// The source with key `key`.
    #[must_use]
    pub fn find(&self, key: &str) -> Option<SourceId> {
        self.by_key.get(key).copied()
    }

    /// Every source, by ID.
    pub fn iter(&self) -> impl Iterator<Item = (SourceId, &SourceInfo)> {
        self.list
            .iter()
            .enumerate()
            .map(|(i, info)| (SourceId(i as u16), info))
    }
}
