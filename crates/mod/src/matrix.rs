//! The modulation matrix as data: 64 slots, edited off the audio thread and
//! compiled into a [`Program`](crate::Program).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::curve::{Curve, NoteCurves};
use crate::dest::Target;
use crate::source::SourceId;

/// How many slots a matrix has (engine-core R102).
pub const SLOTS: usize = 64;

/// How far from the middle of its travel a control may sit and still count
/// as centred for drag-to-modulate (engine-core R104): half a percent of
/// the travel, until the reference's tolerance is observed.
pub const CENTRE_TOLERANCE: f32 = 0.005;

/// Which way a route moves its destination (engine-core R105).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    /// One way: from the control's position upward (downward for a
    /// negative amount).
    #[default]
    Unipolar,
    /// Both ways: swinging around the control's position.
    Bipolar,
}

/// One slot of the matrix.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// What modulates.
    pub source: Option<SourceId>,
    /// The source's curve.
    pub source_curve: Curve,
    /// How far, −1 to 1 (−100 % to +100 % of the destination's travel). A
    /// negative amount inverts the source (engine-core R106).
    pub amount: f32,
    /// One way or both ways.
    pub polarity: Polarity,
    /// What it modulates; one destination per slot (engine-core R102).
    pub destination: Option<Target>,
    /// A second source multiplied in.
    pub aux: Option<SourceId>,
    /// Whether the aux contributes 1 − aux.
    pub aux_invert: bool,
    /// The aux's curve.
    pub aux_curve: Curve,
    /// The output scaler, 0 to 1.
    pub output: f32,
    /// Whether the slot is switched off.
    pub bypass: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            source: None,
            source_curve: Curve::LINEAR,
            amount: 0.0,
            polarity: Polarity::Unipolar,
            destination: None,
            aux: None,
            aux_invert: false,
            aux_curve: Curve::LINEAR,
            output: 1.0,
            bypass: false,
        }
    }
}

impl Slot {
    /// Whether nothing is assigned to it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.source.is_none() && self.destination.is_none()
    }
}

/// How [`Matrix::sort`] orders the slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    /// By source, in the registry's order.
    Source,
    /// By destination: parameters by ID, then LFO buses.
    Destination,
}

/// Which of a slot's two curves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CurveSlot {
    /// The source's.
    Source,
    /// The aux's.
    Aux,
}

/// Why an edit couldn't be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// Every slot is in use.
    Full,
    /// There's no such slot.
    NoSlot(usize),
    /// There's no source with that key.
    UnknownSource(String),
    /// There's no destination with that target.
    UnknownDestination(Target),
    /// Every LFO already modulates something.
    NoFreeLfo,
    /// The command's arguments don't fit its schema.
    BadArgs(String),
    /// There's no command with that ID.
    UnknownCommand(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "all {SLOTS} modulation slots are in use"),
            Self::NoSlot(n) => write!(f, "there's no slot {n}; slots are 0 to {}", SLOTS - 1),
            Self::UnknownSource(key) => write!(f, "there's no modulation source {key:?}"),
            Self::UnknownDestination(t) => write!(f, "{t:?} can't be modulated"),
            Self::NoFreeLfo => write!(f, "every LFO already modulates something"),
            Self::BadArgs(e) => write!(f, "bad arguments: {e}"),
            Self::UnknownCommand(id) => write!(f, "there's no command {id:?}"),
        }
    }
}

impl std::error::Error for EditError {}

/// The modulation matrix: [`SLOTS`] slots, the velocity and key curves, and
/// the lock.
#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    slots: Vec<Slot>,
    /// The velocity and key curves.
    pub note_curves: NoteCurves,
    locked: bool,
}

impl Default for Matrix {
    fn default() -> Self {
        Self {
            slots: vec![Slot::default(); SLOTS],
            note_curves: NoteCurves::default(),
            locked: false,
        }
    }
}

impl Matrix {
    /// An empty matrix.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Its slots.
    #[must_use]
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// Slot `n`.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn slot(&self, n: usize) -> Result<&Slot, EditError> {
        self.slots.get(n).ok_or(EditError::NoSlot(n))
    }

    /// Slot `n`, to change.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn slot_mut(&mut self, n: usize) -> Result<&mut Slot, EditError> {
        self.slots.get_mut(n).ok_or(EditError::NoSlot(n))
    }

    /// Adds a route from `source` to `destination` in the first empty slot
    /// and returns the slot.
    ///
    /// # Errors
    ///
    /// Every slot is in use.
    pub fn add(
        &mut self,
        source: SourceId,
        destination: Target,
        amount: f32,
        polarity: Polarity,
    ) -> Result<usize, EditError> {
        let n = self
            .slots
            .iter()
            .position(Slot::is_empty)
            .ok_or(EditError::Full)?;
        self.slots[n] = Slot {
            source: Some(source),
            destination: Some(destination),
            amount: amount.clamp(-1.0, 1.0),
            polarity,
            ..Slot::default()
        };
        Ok(n)
    }

    /// Empties slot `n`.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn remove(&mut self, n: usize) -> Result<(), EditError> {
        *self.slot_mut(n)? = Slot::default();
        Ok(())
    }

    /// Empties every slot that modulates `destination`, and returns how many.
    pub fn remove_destination(&mut self, destination: Target) -> usize {
        let mut count = 0;
        for slot in &mut self.slots {
            if slot.destination == Some(destination) {
                *slot = Slot::default();
                count += 1;
            }
        }
        count
    }

    /// Switches slot `n` off or on.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn set_bypass(&mut self, n: usize, bypass: bool) -> Result<(), EditError> {
        self.slot_mut(n)?.bypass = bypass;
        Ok(())
    }

    /// Sets slot `n`'s amount, −1 to 1.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn set_amount(&mut self, n: usize, amount: f32) -> Result<(), EditError> {
        self.slot_mut(n)?.amount = clamp_or(amount, -1.0, 1.0, 0.0);
        Ok(())
    }

    /// Sets slot `n`'s output scaler, 0 to 1.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn set_output(&mut self, n: usize, output: f32) -> Result<(), EditError> {
        self.slot_mut(n)?.output = clamp_or(output, 0.0, 1.0, 1.0);
        Ok(())
    }

    /// Sets slot `n`'s polarity.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn set_polarity(&mut self, n: usize, polarity: Polarity) -> Result<(), EditError> {
        self.slot_mut(n)?.polarity = polarity;
        Ok(())
    }

    /// Sets slot `n`'s aux source, or none, and whether it's inverted.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn set_aux(
        &mut self,
        n: usize,
        aux: Option<SourceId>,
        invert: bool,
    ) -> Result<(), EditError> {
        let slot = self.slot_mut(n)?;
        slot.aux = aux;
        slot.aux_invert = invert;
        Ok(())
    }

    /// Sets one of slot `n`'s curves.
    ///
    /// # Errors
    ///
    /// There's no slot `n`.
    pub fn set_curve(&mut self, n: usize, which: CurveSlot, curve: Curve) -> Result<(), EditError> {
        let slot = self.slot_mut(n)?;
        match which {
            CurveSlot::Source => slot.source_curve = curve,
            CurveSlot::Aux => slot.aux_curve = curve,
        }
        Ok(())
    }

    /// Orders the slots by source or by destination, keeping the order of
    /// slots that tie, with empty slots last.
    pub fn sort(&mut self, by: SortBy) {
        self.slots.sort_by_key(|slot| {
            let empty = slot.is_empty();
            match by {
                SortBy::Source => (empty, slot.source.map(|s| s.0), None),
                SortBy::Destination => (empty, None, slot.destination),
            }
        });
    }

    /// Whether it's locked: loading a patch leaves a locked matrix as it is.
    #[must_use]
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Locks or unlocks it.
    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
    }

    /// Takes `other`'s slots and curves, as loading a patch does, unless
    /// this matrix is locked. Returns whether it changed.
    pub fn load(&mut self, other: &Self) -> bool {
        if self.locked {
            return false;
        }
        self.slots.clone_from(&other.slots);
        self.note_curves = other.note_curves.clone();
        true
    }

    /// Adds a route as dragging `source` onto a control at `position` (0 to
    /// 1 of its travel) makes it: polarity by [`drag_polarity`], and with
    /// `limit` the amount reduced by [`limit_depth`]. Returns the slot.
    ///
    /// # Errors
    ///
    /// Every slot is in use.
    pub fn drag(
        &mut self,
        source: SourceId,
        destination: Target,
        position: f32,
        amount: f32,
        drag: Drag,
    ) -> Result<usize, EditError> {
        let polarity = drag_polarity(position, drag.toggle);
        let amount = if drag.limit {
            limit_depth(position, polarity, amount)
        } else {
            amount
        };
        self.add(source, destination, amount, polarity)
    }

    /// Adds a vibrato (engine-core R112): a two-way route from the first of
    /// `lfos` that no slot uses yet to `tuning`, scaled by `mod_wheel` as
    /// its aux. Returns the slot.
    ///
    /// # Errors
    ///
    /// Every LFO is in use, or every slot is.
    pub fn create_vibrato(
        &mut self,
        lfos: &[SourceId],
        mod_wheel: SourceId,
        tuning: Target,
        amount: f32,
    ) -> Result<usize, EditError> {
        let lfo = lfos
            .iter()
            .copied()
            .find(|lfo| !self.slots.iter().any(|s| s.source == Some(*lfo)))
            .ok_or(EditError::NoFreeLfo)?;
        let n = self.add(lfo, tuning, amount, Polarity::Bipolar)?;
        self.slots[n].aux = Some(mod_wheel);
        Ok(n)
    }

    /// Adds a one-way route from `velocity` to `amp` (engine-core R112).
    /// Returns the slot.
    ///
    /// # Errors
    ///
    /// Every slot is in use.
    pub fn create_velocity_to_amp(
        &mut self,
        velocity: SourceId,
        amp: Target,
        amount: f32,
    ) -> Result<usize, EditError> {
        self.add(velocity, amp, amount, Polarity::Unipolar)
    }
}

/// How a drag makes its route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drag {
    /// The gesture's modifier was held: the opposite polarity.
    pub toggle: bool,
    /// The "limit depth on drop" preference (engine-core R109).
    pub limit: bool,
}

/// The polarity of a route made by dragging onto a control at `position`
/// (0 to 1 of its travel): both ways when the control is centred, one way
/// otherwise (engine-core R104); `toggle` gives the other.
#[must_use]
pub fn drag_polarity(position: f32, toggle: bool) -> Polarity {
    let centred = (position - 0.5).abs() <= CENTRE_TOLERANCE;
    match (centred, toggle) {
        (true, false) | (false, true) => Polarity::Bipolar,
        _ => Polarity::Unipolar,
    }
}

/// `amount` reduced so a full-scale source just reaches the end of the
/// control's travel from `position` (engine-core R109): a one-way route to
/// the top (the bottom for a negative amount), a two-way route to whichever
/// end is nearer, so its swing stays within the travel.
#[must_use]
pub fn limit_depth(position: f32, polarity: Polarity, amount: f32) -> f32 {
    let position = position.clamp(0.0, 1.0);
    let room = match polarity {
        Polarity::Unipolar if amount >= 0.0 => 1.0 - position,
        Polarity::Unipolar => position,
        Polarity::Bipolar => position.min(1.0 - position),
    };
    amount.clamp(-room, room)
}

/// `value` within `lo` to `hi`, or `fallback` if it isn't a number.
fn clamp_or(value: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(lo, hi)
    }
}
