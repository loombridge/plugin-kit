//! The matrix's edits as data, and as commands a plug-in registers: every
//! edit is an [`Edit`], which [`Matrix::apply`] makes, and each kind of
//! edit is a command (`mod.add_route`, `mod.drag`, …) whose arguments are
//! that variant's fields.

use lspk_commands::{
    CommandSpec, Context, NoArgs, Permission, RegisterError, Registry, schemars, tagged_variant,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::curve::{Curve, CurveSpec};
use crate::dest::{Destinations, Target};
use crate::matrix::{CurveSlot, Drag, EditError, Matrix, Polarity, SortBy};
use crate::source::{SourceId, Sources};

/// One edit to the matrix. Sources are named by their keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    /// Adds a route in the first empty slot.
    AddRoute {
        /// The source's key, such as `lfo1`.
        source: String,
        /// What it modulates.
        destination: Target,
        /// How far, −1 to 1 of the destination's travel.
        amount: f32,
        /// One way (the default) or both ways.
        #[serde(default)]
        polarity: Polarity,
    },
    /// Empties a slot.
    RemoveRoute {
        /// The slot, from 0.
        slot: u8,
    },
    /// Switches a slot off or on.
    SetBypass {
        /// The slot, from 0.
        slot: u8,
        /// Whether it's off.
        bypass: bool,
    },
    /// Sets a slot's amount.
    SetAmount {
        /// The slot, from 0.
        slot: u8,
        /// −1 to 1; negative inverts the source.
        amount: f32,
    },
    /// Sets a slot's output scaler.
    SetOutput {
        /// The slot, from 0.
        slot: u8,
        /// 0 to 1.
        output: f32,
    },
    /// Sets a slot's polarity.
    SetPolarity {
        /// The slot, from 0.
        slot: u8,
        /// One way or both ways.
        polarity: Polarity,
    },
    /// Sets or clears a slot's aux source.
    SetAux {
        /// The slot, from 0.
        slot: u8,
        /// The aux source's key, or none to clear it.
        #[serde(default)]
        source: Option<String>,
        /// Whether the aux contributes 1 − aux.
        #[serde(default)]
        invert: bool,
    },
    /// Sets a slot's source or aux curve.
    SetCurve {
        /// The slot, from 0.
        slot: u8,
        /// Which curve.
        which: CurveSlot,
        /// The curve.
        curve: CurveSpec,
    },
    /// Empties every slot that modulates a destination.
    RemoveDestination {
        /// The destination.
        destination: Target,
    },
    /// Orders the slots.
    Sort {
        /// By source or by destination.
        by: SortBy,
    },
    /// Locks or unlocks the matrix.
    Lock {
        /// Whether it's locked.
        locked: bool,
    },
    /// Adds a route as dragging a source onto a control makes it.
    Drag {
        /// The source's key.
        source: String,
        /// The control's destination.
        destination: Target,
        /// The control's position, 0 to 1 of its travel.
        position: f32,
        /// How far, −1 to 1.
        amount: f32,
        /// The opposite polarity to the drag rule's.
        #[serde(default)]
        toggle: bool,
        /// Reduce the amount so a full-scale source just reaches the end of
        /// the control's travel.
        #[serde(default)]
        limit_depth: bool,
    },
    /// Adds a vibrato from the next free LFO, scaled by the mod wheel.
    CreateVibrato {
        /// The tuning destination.
        destination: Target,
        /// How far, −1 to 1.
        amount: f32,
    },
    /// Adds a route from the velocity curve to the amp.
    CreateVelocityToAmp {
        /// The amp destination.
        destination: Target,
        /// How far, −1 to 1.
        amount: f32,
    },
}

/// The slot an edit filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SlotResult {
    /// The slot, from 0.
    pub slot: u8,
}

/// How many slots an edit emptied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemovedResult {
    /// How many.
    pub removed: usize,
}

/// One slot in use, as `mod.list` describes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SlotView {
    /// The slot, from 0.
    pub slot: u8,
    /// The source's key.
    pub source: Option<String>,
    /// The source's curve.
    pub source_curve: CurveSpec,
    /// −1 to 1.
    pub amount: f32,
    /// One way or both ways.
    pub polarity: Polarity,
    /// What it modulates.
    pub destination: Option<Target>,
    /// The aux source's key.
    pub aux: Option<String>,
    /// Whether the aux is inverted.
    pub aux_invert: bool,
    /// The aux's curve.
    pub aux_curve: CurveSpec,
    /// 0 to 1.
    pub output: f32,
    /// Whether it's switched off.
    pub bypass: bool,
}

/// The ID of `mod.list`, which reads the matrix.
pub const LIST: &str = "mod.list";

/// Each edit command's tag, label, description and result.
const EDITS: &[(&str, &str, &str, Out)] = &[
    (
        "add_route",
        "Add Modulation Route",
        "Adds a route from a modulation source to a destination in the first empty slot of \
         the modulation matrix, and returns the slot. The amount is −1 to 1 of the \
         destination's travel; one-way routes add upward from the control's position, two-way \
         routes swing around it. It changes the patch.",
        Out::Slot,
    ),
    (
        "remove_route",
        "Remove Modulation Route",
        "Empties one slot of the modulation matrix, removing its route. It changes the patch.",
        Out::Nothing,
    ),
    (
        "set_bypass",
        "Bypass Modulation Route",
        "Switches one slot of the modulation matrix off or on without removing it. It changes \
         the patch.",
        Out::Nothing,
    ),
    (
        "set_amount",
        "Set Modulation Amount",
        "Sets how far one slot's route moves its destination, from −1 to 1 of the \
         destination's travel; a negative amount inverts the source. It changes the patch.",
        Out::Nothing,
    ),
    (
        "set_output",
        "Set Modulation Output",
        "Sets one slot's output scaler, from 0 to 1, which scales the route after its amount. \
         It changes the patch.",
        Out::Nothing,
    ),
    (
        "set_polarity",
        "Set Modulation Polarity",
        "Sets whether one slot's route moves its destination one way, upward from the \
         control's position, or both ways around it. It changes the patch.",
        Out::Nothing,
    ),
    (
        "set_aux",
        "Set Modulation Aux Source",
        "Sets or clears the auxiliary source multiplied into one slot's route, and whether it \
         is inverted (1 − aux). Use it to scale a route by a second source, such as the mod \
         wheel. It changes the patch.",
        Out::Nothing,
    ),
    (
        "set_curve",
        "Set Modulation Curve",
        "Sets the source or aux curve of one slot: a bend from 0 to 1, where 0.5 is linear, or \
         drawn points with rise and fall slew in seconds. It changes the patch.",
        Out::Nothing,
    ),
    (
        "remove_destination",
        "Remove All Modulation",
        "Empties every slot of the modulation matrix that modulates one destination, and \
         returns how many. Use it to clear a control's modulation. It changes the patch.",
        Out::Removed,
    ),
    (
        "sort",
        "Sort Modulation Matrix",
        "Orders the modulation matrix's slots by source or by destination, empty slots last. \
         The routes themselves don't change, only which slots hold them.",
        Out::Nothing,
    ),
    (
        "lock",
        "Lock Modulation Matrix",
        "Locks or unlocks the modulation matrix. Loading a patch leaves a locked matrix as it \
         is.",
        Out::Nothing,
    ),
    (
        "drag",
        "Drag to Modulate",
        "Adds a route as dragging a source onto a control does, and returns the slot: two-way \
         if the control sits at its centre, one-way otherwise, the other way with toggle; with \
         limit_depth the amount is reduced so a full-scale source just reaches the end of the \
         control's travel. It changes the patch.",
        Out::Slot,
    ),
    (
        "create_vibrato",
        "Create Vibrato",
        "Adds a two-way route from the first LFO that no slot uses to the given tuning \
         destination, scaled by the mod wheel as its aux, and returns the slot. It changes the \
         patch.",
        Out::Slot,
    ),
    (
        "create_velocity_to_amp",
        "Create Velocity to Amp",
        "Adds a one-way route from the velocity curve to the given amp destination, and \
         returns the slot. It changes the patch.",
        Out::Slot,
    ),
];

#[derive(Clone, Copy)]
enum Out {
    Nothing,
    Slot,
    Removed,
}

/// Registers the matrix's commands: one per kind of [`Edit`], as
/// `mod.<tag>`, and [`LIST`].
///
/// # Errors
///
/// A command with one of these IDs is already registered.
pub fn register(registry: &mut Registry) -> Result<(), RegisterError> {
    let schema = lspk_commands::args_schema::<Edit>();
    for (tag, label, description, out) in EDITS {
        let args = tagged_variant(&schema, "command", tag).expect("every tag is a variant");
        let spec = CommandSpec::new(format!("mod.{tag}"), *label)
            .description(*description)
            .permission(Permission::Edit)
            .context(Context::Global)
            .args_schema(args);
        registry.register(match out {
            Out::Nothing => spec.result::<()>(),
            Out::Slot => spec.result::<SlotResult>(),
            Out::Removed => spec.result::<RemovedResult>(),
        })?;
    }
    registry.register(
        CommandSpec::new(LIST, "List Modulation Routes")
            .description(
                "Lists every slot of the modulation matrix that's in use: its source, \
                 destination, amount, polarity, aux, curves, output and bypass. It changes \
                 nothing.",
            )
            .permission(Permission::Read)
            .context(Context::Global)
            .args::<NoArgs>()
            .result::<Vec<SlotView>>(),
    )
}

impl Matrix {
    /// Runs the command `id` with JSON `args`, as an agent or a menu calls
    /// it, and returns its JSON result.
    ///
    /// # Errors
    ///
    /// There's no such command, its arguments don't fit, or the edit fails.
    pub fn run(
        &mut self,
        id: &str,
        args: Value,
        sources: &Sources,
        destinations: &Destinations,
    ) -> Result<Value, EditError> {
        if id == LIST {
            return Ok(serde_json::to_value(self.describe(sources)).expect("serialisable"));
        }
        let tag = id
            .strip_prefix("mod.")
            .filter(|t| EDITS.iter().any(|(e, ..)| e == t))
            .ok_or_else(|| EditError::UnknownCommand(id.to_owned()))?;
        let mut object = match args {
            Value::Object(o) => o,
            Value::Null => Map::new(),
            _ => return Err(EditError::BadArgs("arguments must be an object".into())),
        };
        object.insert("command".into(), Value::String(tag.to_owned()));
        let edit: Edit = serde_json::from_value(Value::Object(object))
            .map_err(|e| EditError::BadArgs(e.to_string()))?;
        self.apply(&edit, sources, destinations)
    }

    /// Makes `edit` and returns its result as JSON: `{"slot": n}` for an
    /// edit that fills a slot, `{"removed": n}` for one that empties
    /// several, `null` otherwise.
    ///
    /// # Errors
    ///
    /// A key or target isn't known, a slot doesn't exist, or the matrix is
    /// full.
    pub fn apply(
        &mut self,
        edit: &Edit,
        sources: &Sources,
        destinations: &Destinations,
    ) -> Result<Value, EditError> {
        let source = |key: &str| {
            sources
                .find(key)
                .ok_or_else(|| EditError::UnknownSource(key.to_owned()))
        };
        let target = |t: Target| {
            destinations
                .find(t)
                .map(|_| t)
                .ok_or(EditError::UnknownDestination(t))
        };
        let filled = |slot: usize| Ok(serde_json::json!({ "slot": slot }));
        match edit {
            Edit::AddRoute {
                source: s,
                destination,
                amount,
                polarity,
            } => filled(self.add(source(s)?, target(*destination)?, *amount, *polarity)?),
            Edit::RemoveRoute { slot } => self.remove(usize::from(*slot)).map(|()| Value::Null),
            Edit::SetBypass { slot, bypass } => self
                .set_bypass(usize::from(*slot), *bypass)
                .map(|()| Value::Null),
            Edit::SetAmount { slot, amount } => self
                .set_amount(usize::from(*slot), *amount)
                .map(|()| Value::Null),
            Edit::SetOutput { slot, output } => self
                .set_output(usize::from(*slot), *output)
                .map(|()| Value::Null),
            Edit::SetPolarity { slot, polarity } => self
                .set_polarity(usize::from(*slot), *polarity)
                .map(|()| Value::Null),
            Edit::SetAux {
                slot,
                source: s,
                invert,
            } => {
                let aux = s.as_deref().map(source).transpose()?;
                self.set_aux(usize::from(*slot), aux, *invert)
                    .map(|()| Value::Null)
            }
            Edit::SetCurve { slot, which, curve } => self
                .set_curve(usize::from(*slot), *which, Curve::from(curve))
                .map(|()| Value::Null),
            Edit::RemoveDestination { destination } => {
                let removed = self.remove_destination(*destination);
                Ok(serde_json::json!({ "removed": removed }))
            }
            Edit::Sort { by } => {
                self.sort(*by);
                Ok(Value::Null)
            }
            Edit::Lock { locked } => {
                self.set_locked(*locked);
                Ok(Value::Null)
            }
            Edit::Drag {
                source: s,
                destination,
                position,
                amount,
                toggle,
                limit_depth,
            } => filled(self.drag(
                source(s)?,
                target(*destination)?,
                *position,
                *amount,
                Drag {
                    toggle: *toggle,
                    limit: *limit_depth,
                },
            )?),
            Edit::CreateVibrato {
                destination,
                amount,
            } => {
                let lfos: Vec<SourceId> = sources
                    .iter()
                    .filter(|(_, info)| {
                        info.key
                            .strip_prefix("lfo")
                            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                    })
                    .map(|(id, _)| id)
                    .collect();
                filled(self.create_vibrato(
                    &lfos,
                    source("mod_wheel")?,
                    target(*destination)?,
                    *amount,
                )?)
            }
            Edit::CreateVelocityToAmp {
                destination,
                amount,
            } => filled(self.create_velocity_to_amp(
                source("velocity")?,
                target(*destination)?,
                *amount,
            )?),
        }
    }

    /// Every slot in use, as `mod.list` describes it.
    #[must_use]
    pub fn describe(&self, sources: &Sources) -> Vec<SlotView> {
        let key = |id: Option<SourceId>| {
            id.map(|id| {
                sources
                    .get(id)
                    .map_or_else(|| format!("#{}", id.0), |s| s.key.clone())
            })
        };
        self.slots()
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_empty())
            .map(|(n, s)| SlotView {
                slot: n as u8,
                source: key(s.source),
                source_curve: CurveSpec::from(&s.source_curve),
                amount: s.amount,
                polarity: s.polarity,
                destination: s.destination,
                aux: key(s.aux),
                aux_invert: s.aux_invert,
                aux_curve: CurveSpec::from(&s.aux_curve),
                output: s.output,
                bypass: s.bypass,
            })
            .collect()
    }
}
