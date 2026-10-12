//! The parameter space of a modular plug-in: every parameter of every
//! module, each with an ID that never changes and its real name, and slots
//! whose module the person chooses (an effects rack, an oscillator whose
//! type changes).
//!
//! # The ID scheme
//!
//! A parameter's ID is the 32 bits CLAP, VST3 and Audio Units all give one,
//! packed from where it sits ([`ParamAddress`]):
//!
//! | Bits | What |
//! |---|---|
//! | 31 | always 0: VST3 keeps IDs from 2³¹ for hosts, and CLAP keeps `u32::MAX` |
//! | 30 to 24 | the module type, 1 to 127; 0 is the flat space below |
//! | 23 to 16 | the instance of that module, 0 to 255 |
//! | 15 to 0 | the parameter: below `0x8000` one of the module's own, numbered by the module; from `0x8000` a slot's reserved parameter `0x8000 + n`, where `n` is the number its current module gives the parameter |
//!
//! IDs below 2²⁴ (module type 0) are the flat space: a plug-in's global
//! parameters, numbered by the plug-in, so a plug-in that numbered its
//! parameters itself keeps every ID when it moves to a space.
//!
//! Nothing in an ID depends on order. Module types, instances and
//! parameters carry their numbers explicitly, so adding a module, an
//! instance, a parameter, or a module a slot can hold never changes an
//! existing ID: a new module type takes a new number, a new parameter the
//! next free number in its module, and a slot whose new choice needs more
//! parameters reserves more at the end. Numbers are never reused. A slot's
//! choices are listed by the index its type parameter stores, so new
//! choices go at the end of the list. [`ParamSpace::id_list`] gives every
//! ID with what it means, for a plug-in to keep as a golden list that its
//! tests check is only ever added to.
//!
//! # Fixed modules and slots
//!
//! A **fixed module** (an oscillator, a filter, an envelope) has every
//! parameter visible to the host, by name, always: `Osc A Level`.
//!
//! A **slot** holds one of several module types, which the person picks
//! with the slot's type parameter (number 0 in the slot's own module, a
//! choice). The slot reserves as many parameters as its largest choice
//! needs. Its current module's parameters take its reserved parameters by
//! their numbers, with their real names, ranges and value text; the ones
//! it doesn't use are hidden. A host keeps a reserved parameter's ID, and
//! so its automation, whichever module the slot holds.
//!
//! A host can't change a parameter's range or steps while the plug-in runs
//! (CLAP asks for a restart, and VST3 has no way at all), so a reserved
//! parameter always shows the host the same shape: a continuous value from
//! 0 to 1, its control's travel. Its current module's descriptor reads the
//! travel as a real value ([`ParamView::real`]), and the host sees that
//! value as text. A choice in a slot is a travel that steps through its
//! choices.
//!
//! # Paths
//!
//! Agents and the editor name a parameter by its path ([`ParamPath`]):
//! `osc "A" / param "Level"`, `fx "1" / param "Drive"`, or `param "Gain"`
//! for a global one.
//!
//! # On the audio thread
//!
//! [`ParamSpace::locate`], [`ParamSpace::is_selector`],
//! [`ParamSpace::slot_param`] and [`ParamSpace::slot_choice`] allocate
//! nothing, lock nothing and never block, so a processor may call them; it
//! keeps the space in an `Arc` from activation. Everything that returns
//! text runs on the main thread.

use std::fmt;
use std::str::FromStr;

use crate::descriptor::{ParamDescriptor, ParamId, ParamUnit};

/// The bits an ID's module type starts at.
pub const MODULE_SHIFT: u32 = 24;
/// The bits an ID's instance starts at.
pub const INSTANCE_SHIFT: u32 = 16;
/// The highest module type.
pub const MAX_MODULE: u8 = 127;
/// The first of a slot's reserved parameter numbers: reserved parameter `n`
/// is `SLOT_BASE + n`.
pub const SLOT_BASE: u16 = 0x8000;
/// The number of a slot's type parameter in the slot's own module.
pub const SELECTOR: u16 = 0;
/// The highest number a module may give one of its parameters, so that a
/// slot's reserved parameter for it stays below `0xFFFF`.
pub const MAX_PARAM: u16 = 0x7FFE;

/// Where a parameter sits: its module type, the module's instance and the
/// parameter's number, which [`id`](Self::id) packs into its ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParamAddress {
    /// The module type, from 1 to [`MAX_MODULE`]; 0 for the flat space.
    pub module: u8,
    /// The instance, from 0.
    pub instance: u8,
    /// The parameter: a module's own below [`SLOT_BASE`], a slot's reserved
    /// one from it.
    pub param: u16,
}

impl ParamAddress {
    /// The address of parameter `param` of instance `instance` of module
    /// type `module`.
    #[must_use]
    pub const fn new(module: u8, instance: u8, param: u16) -> Self {
        Self {
            module,
            instance,
            param,
        }
    }

    /// Its ID. A module type above [`MAX_MODULE`] loses its top bit.
    #[must_use]
    pub const fn id(self) -> ParamId {
        ParamId(
            ((self.module & MAX_MODULE) as u32) << MODULE_SHIFT
                | (self.instance as u32) << INSTANCE_SHIFT
                | self.param as u32,
        )
    }

    /// Where `id` sits; `None` for an ID with its top bit set, which the
    /// scheme never makes.
    #[must_use]
    pub const fn of(id: ParamId) -> Option<Self> {
        if id.0 >> 31 != 0 {
            return None;
        }
        Some(Self {
            module: (id.0 >> MODULE_SHIFT) as u8,
            instance: (id.0 >> INSTANCE_SHIFT) as u8,
            param: id.0 as u16,
        })
    }
}

/// A type of module: its number, its names, and its parameters, each with
/// the number the module gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleType {
    /// Its number, from 1 to [`MAX_MODULE`], never reused for another type.
    pub number: u8,
    /// Its word in a path, such as `osc`: lower case, no spaces or quotes.
    pub key: String,
    /// Its name as hosts show it, such as `Osc`.
    pub label: String,
    /// Its parameters, in the order hosts list them. Each descriptor's
    /// [`id`](ParamDescriptor::id) is the number the module gives it, up to
    /// [`MAX_PARAM`], and its name is its own (`Level`, not `Osc A
    /// Level`).
    pub params: Vec<ParamDescriptor>,
}

impl ModuleType {
    /// A module type with no parameters yet.
    #[must_use]
    pub fn new(number: u8, key: &str, label: &str) -> Self {
        Self {
            number,
            key: key.to_owned(),
            label: label.to_owned(),
            params: Vec::new(),
        }
    }

    /// The same, with `param` added; its ID is its number in the module.
    #[must_use]
    pub fn param(mut self, param: ParamDescriptor) -> Self {
        self.params.push(param);
        self
    }

    /// Its parameter numbered `number`.
    #[must_use]
    pub fn get(&self, number: u16) -> Option<&ParamDescriptor> {
        self.params.iter().find(|p| p.id.0 == u32::from(number))
    }

    /// How many reserved parameters a slot needs to hold it: one more than
    /// its highest parameter number.
    #[must_use]
    pub fn span(&self) -> u16 {
        self.params
            .iter()
            .map(|p| p.id.0 as u16 + 1)
            .max()
            .unwrap_or(0)
    }
}

/// A parameter's path, as agents and the editor name it:
/// `osc "A" / param "Level"`, or `param "Gain"` for a global parameter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParamPath {
    /// The module's key and the instance's name; `None` for a global
    /// parameter.
    pub module: Option<(String, String)>,
    /// The parameter's name.
    pub param: String,
}

impl fmt::Display for ParamPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((key, instance)) = &self.module {
            write!(f, "{key} \"{instance}\" / ")?;
        }
        write!(f, "param \"{}\"", self.param)
    }
}

impl FromStr for ParamPath {
    type Err = String;

    /// Reads a path as [`Display`](fmt::Display) writes it; spaces around
    /// the parts don't matter.
    fn from_str(text: &str) -> Result<Self, String> {
        let bad = || format!("`{text}` isn't a path such as osc \"A\" / param \"Level\"");
        let quoted = |part: &str, word: Option<&str>| -> Option<(String, String)> {
            let part = part.trim();
            let (head, rest) = part.split_once('"')?;
            let name = rest.strip_suffix('"')?;
            if name.contains('"') || name.is_empty() {
                return None;
            }
            let head = head.trim();
            if word.is_some_and(|w| w != head) || head.is_empty() || head.contains(' ') {
                return None;
            }
            Some((head.to_owned(), name.to_owned()))
        };
        let parts: Vec<&str> = text.split(" / ").collect();
        match parts.as_slice() {
            [param] => {
                let (_, param) = quoted(param, Some("param")).ok_or_else(bad)?;
                Ok(Self {
                    module: None,
                    param,
                })
            }
            [module, param] => {
                let module = quoted(module, None).ok_or_else(bad)?;
                let (_, param) = quoted(param, Some("param")).ok_or_else(bad)?;
                Ok(Self {
                    module: Some(module),
                    param,
                })
            }
            _ => Err(bad()),
        }
    }
}

/// What a parameter of a space is, as a host, an agent or the editor sees
/// it now: its real name and descriptor, and whether it's hidden.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamView {
    /// Its ID.
    pub id: ParamId,
    /// Its name as hosts show it: `Osc A Level`, `FX 1 Drive`, `Gain`.
    pub name: String,
    /// The module it's in, as CLAP's module path: `Osc A`, or empty for a
    /// global parameter.
    pub module: String,
    /// Its path.
    pub path: ParamPath,
    /// What it really is now: for a reserved parameter, its current
    /// module's descriptor. Its [`id`](ParamDescriptor::id) is this ID, and
    /// its name the host's.
    pub descriptor: ParamDescriptor,
    /// Whether hosts should hide it: a reserved parameter its slot's
    /// current module doesn't use.
    pub hidden: bool,
    /// Whether the value stored for it is its control's travel, from 0 to
    /// 1, which [`descriptor`](Self::descriptor) reads: a reserved
    /// parameter.
    pub carried: bool,
}

impl ParamView {
    /// A parameter of a plug-in without a space, as it is.
    #[must_use]
    pub fn plain(descriptor: &ParamDescriptor) -> Self {
        Self {
            id: descriptor.id,
            name: descriptor.name.clone(),
            module: String::new(),
            path: ParamPath {
                module: None,
                param: descriptor.name.clone(),
            },
            descriptor: descriptor.clone(),
            hidden: false,
            carried: false,
        }
    }

    /// The real value, in its descriptor's units, of `stored`, the value
    /// kept for it.
    #[must_use]
    pub fn real(&self, stored: f64) -> f64 {
        if self.carried {
            self.descriptor.denormalize(stored)
        } else {
            self.descriptor.clamp(stored)
        }
    }

    /// The value to keep for the real value `real`.
    #[must_use]
    pub fn stored(&self, real: f64) -> f64 {
        if self.carried {
            self.descriptor.normalize(real)
        } else {
            self.descriptor.clamp(real)
        }
    }

    /// How `stored` reads, with its unit.
    #[must_use]
    pub fn format(&self, stored: f64) -> String {
        self.descriptor.format(self.real(stored))
    }

    /// The value to keep for `text`, as typed; `None` if it names none.
    #[must_use]
    pub fn parse(&self, text: &str) -> Option<f64> {
        self.descriptor.parse(text).map(|real| self.stored(real))
    }
}

/// Where a parameter of a space sits, as a processor finds it without
/// allocating ([`ParamSpace::locate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// A global parameter: its index among the globals.
    Global(usize),
    /// A fixed module's parameter.
    Fixed {
        /// The module type's number.
        module: u8,
        /// The instance.
        instance: u8,
        /// The parameter's number in the module.
        param: u16,
    },
    /// A slot's type parameter.
    Selector {
        /// The slot, by its index in the space.
        slot: usize,
    },
    /// A slot's reserved parameter.
    Reserved {
        /// The slot, by its index in the space.
        slot: usize,
        /// The parameter number its current module gives it.
        param: u16,
    },
}

/// One of a space's slots: a module type's instance whose module the
/// person chooses.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// The slot module's type number.
    pub module: u8,
    /// Its instance.
    pub instance: u8,
    /// Its type parameter.
    pub selector: ParamId,
    /// How many parameters it reserves.
    pub reserved: u16,
    group: usize,
}

impl Slot {
    /// Its reserved parameter numbered `n`.
    #[must_use]
    pub fn reserved_id(&self, n: u16) -> ParamId {
        ParamAddress::new(self.module, self.instance, SLOT_BASE + n).id()
    }
}

/// A remote-controls page: up to eight parameters a controller maps to its
/// knobs, generated from the module layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotePage {
    /// Its ID, which never changes: the address of its module's instance,
    /// with the page's index as the parameter.
    pub id: u32,
    /// The section it's in: its module's name, such as `Osc`.
    pub section: String,
    /// Its name, such as `Osc A`, or `Osc A 2` for a second page.
    pub name: String,
    /// Its eight controls.
    pub params: [Option<ParamId>; 8],
}

#[derive(Clone, Debug, PartialEq)]
enum Group {
    Fixed {
        kind: ModuleType,
        instances: Vec<String>,
    },
    Slots {
        number: u8,
        key: String,
        label: String,
        selector: String,
        instances: Vec<String>,
        choices: Vec<ModuleType>,
        reserved: u16,
        first_slot: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Entry {
    Global(usize),
    Fixed {
        group: usize,
        instance: u8,
        param: usize,
    },
    Selector(usize),
    Reserved {
        slot: usize,
        param: u16,
    },
}

/// The parameter space of a modular plug-in: its global parameters, its
/// fixed modules and its slots, with the descriptors hosts see and each
/// parameter's real meaning now. Build one with [`ParamSpace::builder`].
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpace {
    globals: Vec<ParamDescriptor>,
    groups: Vec<Group>,
    slots: Vec<Slot>,
    /// Every parameter in the hosts' order, with its stored descriptor.
    entries: Vec<(Entry, ParamDescriptor)>,
    /// `(id, index in entries)`, sorted by ID.
    by_id: Vec<(ParamId, usize)>,
    /// Each slot's type parameter, sorted by ID.
    selectors: Vec<(ParamId, usize)>,
}

/// Builds a [`ParamSpace`]: globals, fixed modules and slots, in the order
/// hosts list them.
#[derive(Clone, Debug, Default)]
pub struct SpaceBuilder {
    globals: Vec<ParamDescriptor>,
    groups: Vec<Group>,
    defaults: Vec<(ParamId, f64)>,
}

impl SpaceBuilder {
    /// Adds a global parameter. Its ID is its own, below 2²⁴.
    #[must_use]
    pub fn global(mut self, param: ParamDescriptor) -> Self {
        self.globals.push(param);
        self
    }

    /// Adds a fixed module with one instance named each of `instances`
    /// (`["A", "B"]`), numbered from 0 in that order.
    #[must_use]
    pub fn module(mut self, kind: ModuleType, instances: &[&str]) -> Self {
        self.groups.push(Group::Fixed {
            kind,
            instances: instances.iter().map(|i| (*i).to_owned()).collect(),
        });
        self
    }

    /// Gives one instance of a fixed module its own default for one
    /// parameter, by the parameter's ID: where instances differ in a new
    /// patch, as a synthesiser's first oscillator is on and the others off.
    /// The module type's default stays every other instance's.
    #[must_use]
    pub fn instance_default(mut self, id: ParamId, default: f64) -> Self {
        self.defaults.push((id, default));
        self
    }

    /// Adds slots of module type `number` (its path word `key` and host
    /// name `label`), one named each of `instances`, each holding one of
    /// `choices`. Each slot's type parameter, `selector` (such as `Type`),
    /// is a choice of the choices' labels, starting at the first.
    #[must_use]
    pub fn slots(
        mut self,
        (number, key, label): (u8, &str, &str),
        selector: &str,
        instances: &[&str],
        choices: Vec<ModuleType>,
    ) -> Self {
        self.groups.push(Group::Slots {
            number,
            key: key.to_owned(),
            label: label.to_owned(),
            selector: selector.to_owned(),
            instances: instances.iter().map(|i| (*i).to_owned()).collect(),
            reserved: choices.iter().map(ModuleType::span).max().unwrap_or(0),
            choices,
            first_slot: 0,
        });
        self
    }

    /// The space.
    ///
    /// # Errors
    ///
    /// A broken rule, named: a module number out of range or used twice, a
    /// parameter numbered out of range or twice in its module, a global ID
    /// outside the flat space, too many instances, a slot without choices,
    /// a name with a quote in it, a descriptor that breaks its own rules
    /// ([`ParamDescriptor::check`]), or an instance default for a parameter
    /// that isn't a fixed module's or that its descriptor refuses.
    pub fn build(self) -> Result<ParamSpace, String> {
        let mut groups = self.groups;
        let mut seen_modules = Vec::new();
        let mut slots = Vec::new();
        let mut entries = Vec::new();
        for (index, global) in self.globals.iter().enumerate() {
            global.check()?;
            if global.id.0 >= 1 << MODULE_SHIFT {
                return Err(format!(
                    "global parameter {} ({}) is outside the flat space, below 2^24",
                    global.id.0, global.name
                ));
            }
            check_name(&global.name)?;
            entries.push((Entry::Global(index), global.clone()));
        }
        for (g, group) in groups.iter_mut().enumerate() {
            let (number, key, label, instances) = match group {
                Group::Fixed { kind, instances } => {
                    (kind.number, &kind.key, &kind.label, &*instances)
                }
                Group::Slots {
                    number,
                    key,
                    label,
                    instances,
                    ..
                } => (*number, &*key, &*label, &*instances),
            };
            if number == 0 || number > MAX_MODULE {
                return Err(format!(
                    "module {key} has the number {number}, outside 1 to {MAX_MODULE}"
                ));
            }
            if seen_modules.contains(&number) {
                return Err(format!("two modules have the number {number}"));
            }
            seen_modules.push(number);
            check_key(key)?;
            check_name(label)?;
            if instances.is_empty() || instances.len() > 256 {
                return Err(format!("module {key} needs 1 to 256 instances"));
            }
            for instance in instances {
                check_name(instance)?;
            }
            match group {
                Group::Fixed { kind, instances } => {
                    check_module(kind)?;
                    for (i, instance) in instances.iter().enumerate() {
                        for (p, param) in kind.params.iter().enumerate() {
                            let mut host = param.clone();
                            host.id =
                                ParamAddress::new(kind.number, i as u8, param.id.0 as u16).id();
                            host.name = format!("{} {instance} {}", kind.label, param.name);
                            entries.push((
                                Entry::Fixed {
                                    group: g,
                                    instance: i as u8,
                                    param: p,
                                },
                                host,
                            ));
                        }
                    }
                }
                Group::Slots {
                    number,
                    label,
                    selector,
                    instances,
                    choices,
                    reserved,
                    first_slot,
                    key,
                } => {
                    if choices.is_empty() {
                        return Err(format!("slot {key} has no choices"));
                    }
                    check_name(selector)?;
                    for choice in choices.iter() {
                        check_module(choice)?;
                    }
                    *first_slot = slots.len();
                    let names: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
                    for (i, instance) in instances.iter().enumerate() {
                        let slot = Slot {
                            module: *number,
                            instance: i as u8,
                            selector: ParamAddress::new(*number, i as u8, SELECTOR).id(),
                            reserved: *reserved,
                            group: g,
                        };
                        let selector = ParamDescriptor::choice(
                            slot.selector,
                            &format!("{label} {instance} {selector}"),
                            &names,
                            0,
                        )
                        .describe("Which module the slot holds.");
                        entries.push((Entry::Selector(slots.len()), selector));
                        for n in 0..*reserved {
                            let carrier = ParamDescriptor::continuous(
                                slot.reserved_id(n),
                                &format!("{label} {instance} Slot {}", n + 1),
                                (0.0, 1.0),
                                0.0,
                                ParamUnit::None,
                            );
                            entries.push((
                                Entry::Reserved {
                                    slot: slots.len(),
                                    param: n,
                                },
                                carrier,
                            ));
                        }
                        slots.push(slot);
                    }
                }
            }
        }
        for (id, default) in &self.defaults {
            let Some((_, descriptor)) = entries
                .iter_mut()
                .find(|(entry, d)| d.id == *id && matches!(entry, Entry::Fixed { .. }))
            else {
                return Err(format!(
                    "an instance default names {}, which isn't a fixed module's parameter",
                    id.0
                ));
            };
            descriptor.default = *default;
            descriptor.check()?;
        }
        let mut by_id: Vec<(ParamId, usize)> = entries
            .iter()
            .enumerate()
            .map(|(index, (_, d))| (d.id, index))
            .collect();
        by_id.sort_unstable();
        if let Some(pair) = by_id.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(format!("two parameters have the ID {}", pair[0].0.0));
        }
        let mut selectors: Vec<(ParamId, usize)> = slots
            .iter()
            .enumerate()
            .map(|(index, s)| (s.selector, index))
            .collect();
        selectors.sort_unstable();
        Ok(ParamSpace {
            globals: self.globals,
            groups,
            slots,
            entries,
            by_id,
            selectors,
        })
    }
}

fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "`{key}` isn't a module key: lower-case letters, digits, `-` and `_`"
        ));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.contains('"') || name.contains(" / ") {
        return Err(format!(
            "`{name}` can't be a name: it's empty, or has a quote or ` / ` in it"
        ));
    }
    Ok(())
}

fn check_module(kind: &ModuleType) -> Result<(), String> {
    check_key(&kind.key)?;
    check_name(&kind.label)?;
    let mut numbers = Vec::new();
    for param in &kind.params {
        param.check()?;
        check_name(&param.name)?;
        if param.id.0 > u32::from(MAX_PARAM) {
            return Err(format!(
                "module {}'s parameter {} is numbered {}, above {MAX_PARAM}",
                kind.key, param.name, param.id.0
            ));
        }
        if numbers.contains(&param.id.0) {
            return Err(format!(
                "module {} numbers two parameters {}",
                kind.key, param.id.0
            ));
        }
        numbers.push(param.id.0);
    }
    Ok(())
}

impl ParamSpace {
    /// A builder for a new space.
    #[must_use]
    pub fn builder() -> SpaceBuilder {
        SpaceBuilder::default()
    }

    /// Every parameter as hosts see it, in their order: each fixed one with
    /// its real descriptor, each slot's type parameter, and each reserved
    /// parameter as a travel from 0 to 1.
    #[must_use]
    pub fn descriptors(&self) -> Vec<ParamDescriptor> {
        self.entries.iter().map(|(_, d)| d.clone()).collect()
    }

    /// How many parameters it has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it has none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Its slots, in order.
    #[must_use]
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// Where parameter `id` sits. Allocates nothing.
    #[must_use]
    pub fn locate(&self, id: ParamId) -> Option<Location> {
        let at = self.by_id.binary_search_by_key(&id, |(id, _)| *id).ok()?;
        Some(match self.entries[self.by_id[at].1].0 {
            Entry::Global(index) => Location::Global(index),
            Entry::Fixed { .. } => {
                let address = ParamAddress::of(id)?;
                Location::Fixed {
                    module: address.module,
                    instance: address.instance,
                    param: address.param,
                }
            }
            Entry::Selector(slot) => Location::Selector { slot },
            Entry::Reserved { slot, param } => Location::Reserved { slot, param },
        })
    }

    /// Whether `id` is a slot's type parameter, whose change changes what
    /// the slot's reserved parameters mean. Allocates nothing.
    #[must_use]
    pub fn is_selector(&self, id: ParamId) -> bool {
        self.selectors
            .binary_search_by_key(&id, |(id, _)| *id)
            .is_ok()
    }

    /// The choices slot `slot` can hold, in its type parameter's order.
    #[must_use]
    pub fn choices(&self, slot: usize) -> &[ModuleType] {
        match self.slots.get(slot).map(|s| &self.groups[s.group]) {
            Some(Group::Slots { choices, .. }) => choices,
            _ => &[],
        }
    }

    /// The choice slot `slot`'s type parameter at `value` names. Allocates
    /// nothing.
    #[must_use]
    pub fn slot_choice(&self, slot: usize, value: f64) -> usize {
        let count = self.choices(slot).len();
        if count == 0 || !value.is_finite() {
            return 0;
        }
        (value.round().max(0.0) as usize).min(count - 1)
    }

    /// The descriptor of parameter `param` of choice `choice` in slot
    /// `slot`, which reads its reserved parameter's travel. Allocates
    /// nothing.
    #[must_use]
    pub fn slot_param(&self, slot: usize, choice: usize, param: u16) -> Option<&ParamDescriptor> {
        self.choices(slot).get(choice)?.get(param)
    }

    /// Each of slot `slot`'s reserved parameters with the value it takes
    /// when the slot comes to hold choice `choice`: the choice's default,
    /// as travel, for each parameter it uses, and 0 for the rest.
    #[must_use]
    pub fn slot_defaults(&self, slot: usize, choice: usize) -> Vec<(ParamId, f64)> {
        let Some(s) = self.slots.get(slot) else {
            return Vec::new();
        };
        (0..s.reserved)
            .map(|n| {
                let value = self
                    .slot_param(slot, choice, n)
                    .map_or(0.0, |d| d.normalize(d.default));
                (s.reserved_id(n), value)
            })
            .collect()
    }

    /// Which choice each slot holds, given each parameter's stored value.
    pub fn selection(&self, value_of: &dyn Fn(ParamId) -> Option<f64>) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .map(|(index, s)| self.slot_choice(index, value_of(s.selector).unwrap_or(0.0)))
            .collect()
    }

    /// What parameter `id` is now, given each parameter's stored value
    /// (only the slots' type parameters are read).
    pub fn view(
        &self,
        id: ParamId,
        value_of: &dyn Fn(ParamId) -> Option<f64>,
    ) -> Option<ParamView> {
        let at = self.by_id.binary_search_by_key(&id, |(id, _)| *id).ok()?;
        let (entry, stored) = &self.entries[self.by_id[at].1];
        let view = match *entry {
            Entry::Global(_) => ParamView::plain(stored),
            Entry::Fixed {
                group,
                instance,
                param,
            } => {
                let Group::Fixed { kind, instances } = &self.groups[group] else {
                    return None;
                };
                let name = &instances[usize::from(instance)];
                ParamView {
                    id,
                    name: stored.name.clone(),
                    module: format!("{} {name}", kind.label),
                    path: ParamPath {
                        module: Some((kind.key.clone(), name.clone())),
                        param: kind.params[param].name.clone(),
                    },
                    descriptor: stored.clone(),
                    hidden: false,
                    carried: false,
                }
            }
            Entry::Selector(slot) => {
                let (key, label, selector, name) = self.slot_names(slot)?;
                ParamView {
                    id,
                    name: stored.name.clone(),
                    module: format!("{label} {name}"),
                    path: ParamPath {
                        module: Some((key.to_owned(), name.to_owned())),
                        param: selector.to_owned(),
                    },
                    descriptor: stored.clone(),
                    hidden: false,
                    carried: false,
                }
            }
            Entry::Reserved { slot, param } => {
                let (key, label, _, name) = self.slot_names(slot)?;
                let choice = self.slot_choice(slot, value_of(self.slots[slot].selector)?);
                let module = format!("{label} {name}");
                match self.slot_param(slot, choice, param) {
                    Some(real) => {
                        let mut descriptor = real.clone();
                        descriptor.id = id;
                        descriptor.name = format!("{module} {}", real.name);
                        ParamView {
                            id,
                            name: descriptor.name.clone(),
                            module,
                            path: ParamPath {
                                module: Some((key.to_owned(), name.to_owned())),
                                param: real.name.clone(),
                            },
                            descriptor,
                            hidden: false,
                            carried: true,
                        }
                    }
                    None => ParamView {
                        id,
                        name: stored.name.clone(),
                        module,
                        path: ParamPath {
                            module: Some((key.to_owned(), name.to_owned())),
                            param: format!("Slot {}", param + 1),
                        },
                        descriptor: stored.clone(),
                        hidden: true,
                        carried: true,
                    },
                }
            }
        };
        Some(view)
    }

    /// The key, label, type parameter's name and instance name of slot
    /// `slot`.
    fn slot_names(&self, slot: usize) -> Option<(&str, &str, &str, &str)> {
        let s = self.slots.get(slot)?;
        match &self.groups[s.group] {
            Group::Slots {
                key,
                label,
                selector,
                instances,
                ..
            } => Some((key, label, selector, &instances[usize::from(s.instance)])),
            Group::Fixed { .. } => None,
        }
    }

    /// The visible parameter `text` names: a path (`fx "1" / param
    /// "Drive"`), or a name as hosts show it (`FX 1 Drive`), in any case.
    pub fn find(&self, text: &str, value_of: &dyn Fn(ParamId) -> Option<f64>) -> Option<ParamId> {
        let text = text.trim();
        let path: Option<ParamPath> = text.parse().ok();
        let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        self.entries.iter().find_map(|(_, d)| {
            let view = self.view(d.id, value_of)?;
            if view.hidden {
                return None;
            }
            let matches = match &path {
                Some(path) => {
                    same(&path.param, &view.path.param)
                        && match (&path.module, &view.path.module) {
                            (None, None) => true,
                            (Some((k, i)), Some((vk, vi))) => same(k, vk) && same(i, vi),
                            _ => false,
                        }
                }
                None => same(text, &view.name),
            };
            matches.then_some(view.id)
        })
    }

    /// The remote-controls pages, given each parameter's stored value: a
    /// page per module instance (more where a module has more than eight
    /// parameters), each slot's starting with its type parameter and going
    /// on with its current module's parameters, then the globals on pages
    /// of their own.
    pub fn pages(&self, value_of: &dyn Fn(ParamId) -> Option<f64>) -> Vec<RemotePage> {
        let mut pages = Vec::new();
        let mut add = |module: u8, instance: u8, section: &str, name: &str, ids: Vec<ParamId>| {
            for (index, chunk) in ids.chunks(8).enumerate() {
                let mut params = [None; 8];
                for (slot, id) in params.iter_mut().zip(chunk) {
                    *slot = Some(*id);
                }
                pages.push(RemotePage {
                    id: ParamAddress::new(module, instance, index as u16).id().0,
                    section: section.to_owned(),
                    name: if index == 0 {
                        name.to_owned()
                    } else {
                        format!("{name} {}", index + 1)
                    },
                    params,
                });
            }
        };
        for group in &self.groups {
            match group {
                Group::Fixed { kind, instances } => {
                    for (i, instance) in instances.iter().enumerate() {
                        let ids = kind
                            .params
                            .iter()
                            .map(|p| ParamAddress::new(kind.number, i as u8, p.id.0 as u16).id())
                            .collect();
                        add(
                            kind.number,
                            i as u8,
                            &kind.label,
                            &format!("{} {instance}", kind.label),
                            ids,
                        );
                    }
                }
                Group::Slots {
                    number,
                    label,
                    instances,
                    first_slot,
                    ..
                } => {
                    for (i, instance) in instances.iter().enumerate() {
                        let slot = first_slot + i;
                        let s = &self.slots[slot];
                        let choice = self.slot_choice(slot, value_of(s.selector).unwrap_or(0.0));
                        let mut ids = vec![s.selector];
                        if let Some(module) = self.choices(slot).get(choice) {
                            ids.extend(module.params.iter().map(|p| s.reserved_id(p.id.0 as u16)));
                        }
                        add(*number, i as u8, label, &format!("{label} {instance}"), ids);
                    }
                }
            }
        }
        if !self.globals.is_empty() {
            add(
                0,
                0,
                "Global",
                "Global",
                self.globals.iter().map(|g| g.id).collect(),
            );
        }
        pages
    }

    /// Every ID with what it means, one per line, in ID order: the ID, then
    /// its path with the names the module types give it (a reserved
    /// parameter as `slot <n>`, a slot's choices after its type
    /// parameter). A plug-in keeps this as a golden list its tests check
    /// is only ever added to, so no ID it has published changes meaning.
    #[must_use]
    pub fn id_list(&self) -> String {
        let mut lines: Vec<(ParamId, String)> = self
            .entries
            .iter()
            .filter_map(|(entry, d)| {
                let line = match *entry {
                    Entry::Global(_) => format!("param \"{}\"", d.name),
                    Entry::Fixed {
                        group,
                        instance,
                        param,
                    } => {
                        let Group::Fixed { kind, instances } = &self.groups[group] else {
                            return None;
                        };
                        format!(
                            "{} \"{}\" / param \"{}\"",
                            kind.key,
                            instances[usize::from(instance)],
                            kind.params[param].name
                        )
                    }
                    Entry::Selector(slot) => {
                        let (key, _, selector, name) = self.slot_names(slot)?;
                        let choices: Vec<String> = self
                            .choices(slot)
                            .iter()
                            .map(|c| format!("\"{}\"", c.label))
                            .collect();
                        format!(
                            "{key} \"{name}\" / param \"{selector}\": {}",
                            choices.join(", ")
                        )
                    }
                    Entry::Reserved { slot, param } => {
                        let (key, _, _, name) = self.slot_names(slot)?;
                        format!("{key} \"{name}\" / slot {param}")
                    }
                };
                Some((d.id, line))
            })
            .collect();
        lines.sort();
        lines
            .iter()
            .map(|(id, line)| format!("{:#010x} {line}\n", id.0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc() -> ModuleType {
        ModuleType::new(1, "osc", "Osc")
            .param(ParamDescriptor::gain(ParamId(0), "Level", 0.0, -6.0))
            .param(ParamDescriptor::integer(
                ParamId(2),
                "Pitch",
                (-24, 24),
                0,
                ParamUnit::Semitones,
            ))
    }

    fn drive() -> ModuleType {
        ModuleType::new(40, "drive", "Drive")
            .param(ParamDescriptor::continuous(
                ParamId(0),
                "Amount",
                (0.0, 100.0),
                25.0,
                ParamUnit::Percent,
            ))
            .param(ParamDescriptor::choice(
                ParamId(1),
                "Shape",
                &["Soft", "Hard"],
                0,
            ))
    }

    fn echo() -> ModuleType {
        ModuleType::new(41, "echo", "Echo")
            .param(
                ParamDescriptor::continuous(
                    ParamId(0),
                    "Time",
                    (1.0, 1000.0),
                    250.0,
                    ParamUnit::Milliseconds,
                )
                .logarithmic(),
            )
            .param(ParamDescriptor::continuous(
                ParamId(1),
                "Feedback",
                (0.0, 100.0),
                40.0,
                ParamUnit::Percent,
            ))
            .param(ParamDescriptor::continuous(
                ParamId(2),
                "Mix",
                (0.0, 100.0),
                30.0,
                ParamUnit::Percent,
            ))
    }

    #[test]
    fn an_instance_can_have_its_own_default() {
        let b_pitch = ParamAddress::new(1, 1, 2).id();
        let space = ParamSpace::builder()
            .module(osc(), &["A", "B"])
            .instance_default(b_pitch, 7.0)
            .build()
            .expect("a valid space");
        let default = |id: ParamId| {
            space
                .descriptors()
                .into_iter()
                .find(|d| d.id == id)
                .map(|d| d.default)
        };
        assert_eq!(default(ParamAddress::new(1, 0, 2).id()), Some(0.0));
        assert_eq!(default(b_pitch), Some(7.0));
        let refused = |id: ParamId, value: f64| {
            ParamSpace::builder()
                .global(ParamDescriptor::gain(ParamId(1), "Volume", 6.0, 0.0))
                .module(osc(), &["A", "B"])
                .instance_default(id, value)
                .build()
                .is_err()
        };
        assert!(refused(b_pitch, 99.0), "outside its range");
        assert!(refused(ParamId(1), 0.5), "a global, not a module's");
        assert!(
            refused(ParamAddress::new(1, 2, 2).id(), 0.0),
            "no such instance"
        );
    }

    fn space() -> ParamSpace {
        ParamSpace::builder()
            .global(ParamDescriptor::gain(ParamId(1), "Volume", 6.0, 0.0))
            .module(osc(), &["A", "B"])
            .slots(
                (20, "fx", "FX"),
                "Type",
                &["1", "2"],
                vec![ModuleType::new(39, "off", "Off"), drive(), echo()],
            )
            .build()
            .expect("a valid space")
    }

    #[test]
    fn ids_pack_module_instance_and_parameter() {
        let address = ParamAddress::new(1, 1, 2);
        assert_eq!(address.id(), ParamId(0x0101_0002));
        assert_eq!(ParamAddress::of(address.id()), Some(address));
        assert_eq!(ParamAddress::of(ParamId(u32::MAX)), None);
        assert_eq!(
            ParamAddress::new(MAX_MODULE, 255, 0xFFFE).id().0 >> 31,
            0,
            "VST3's upper half stays free"
        );
    }

    #[test]
    fn every_parameter_is_listed_with_its_real_name() {
        let space = space();
        let names: Vec<String> = space.descriptors().into_iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            [
                "Volume",
                "Osc A Level",
                "Osc A Pitch",
                "Osc B Level",
                "Osc B Pitch",
                "FX 1 Type",
                "FX 1 Slot 1",
                "FX 1 Slot 2",
                "FX 1 Slot 3",
                "FX 2 Type",
                "FX 2 Slot 1",
                "FX 2 Slot 2",
                "FX 2 Slot 3",
            ]
        );
        assert_eq!(space.slots()[1].reserved_id(2), ParamId(0x1401_8002));
    }

    #[test]
    fn adding_modules_parameters_and_choices_changes_no_id() {
        let before = space().id_list();
        let grown = ParamSpace::builder()
            .global(ParamDescriptor::gain(ParamId(1), "Volume", 6.0, 0.0))
            .global(ParamDescriptor::toggle(ParamId(2), "Mono", false))
            .module(
                osc().param(ParamDescriptor::toggle(ParamId(1), "Invert", false)),
                &["A", "B", "C"],
            )
            .module(
                ModuleType::new(2, "env", "Env").param(ParamDescriptor::continuous(
                    ParamId(0),
                    "Attack",
                    (0.0, 10.0),
                    0.0,
                    ParamUnit::Seconds,
                )),
                &["1"],
            )
            .slots(
                (20, "fx", "FX"),
                "Type",
                &["1", "2", "3"],
                vec![
                    ModuleType::new(39, "off", "Off"),
                    drive(),
                    echo(),
                    ModuleType::new(42, "wide", "Wide").param(ParamDescriptor::toggle(
                        ParamId(4),
                        "Mono Bass",
                        false,
                    )),
                ],
            )
            .build()
            .expect("a valid space");
        let after = grown.id_list();
        for line in before.lines() {
            // A slot's type parameter lists its choices, which only grow.
            let kept = match line.split_once(": ") {
                Some((head, choices)) => after
                    .lines()
                    .any(|l| l.starts_with(head) && l.contains(choices)),
                None => after.lines().any(|l| l == line),
            };
            assert!(kept, "`{line}` changed");
        }
        assert!(after.contains("0x14008004 fx \"1\" / slot 4"));
    }

    #[test]
    fn a_slot_shows_its_current_modules_parameters_and_hides_the_rest() {
        let space = space();
        let mut type_1 = 1.0;
        let values = |id: ParamId| {
            if id == space.slots()[0].selector {
                Some(type_1)
            } else {
                Some(0.0)
            }
        };
        let slot = &space.slots()[0];
        let amount = space.view(slot.reserved_id(0), &values).expect("a view");
        assert_eq!(amount.name, "FX 1 Amount");
        assert_eq!(amount.path.to_string(), "fx \"1\" / param \"Amount\"");
        assert!(amount.carried && !amount.hidden);
        assert_eq!(amount.format(0.5), "50 %");
        assert_eq!(amount.parse("25 %"), Some(0.25));
        let shape = space.view(slot.reserved_id(1), &values).expect("a view");
        assert_eq!(shape.format(1.0), "Hard");
        assert_eq!(shape.format(0.2), "Soft");
        let unused = space.view(slot.reserved_id(2), &values).expect("a view");
        assert!(unused.hidden);
        assert_eq!(
            space.find("fx \"1\" / param \"amount\"", &values),
            Some(slot.reserved_id(0))
        );
        assert_eq!(space.find("FX 1 Shape", &values), Some(slot.reserved_id(1)));

        type_1 = 2.0;
        let values = |id: ParamId| {
            if id == space.slots()[0].selector {
                Some(type_1)
            } else {
                Some(0.0)
            }
        };
        let time = space.view(slot.reserved_id(0), &values).expect("a view");
        assert_eq!(time.name, "FX 1 Time");
        assert_eq!(time.format(0.0), "1.00 ms");
        assert_eq!(time.format(1.0), "1.00 s");
        assert!(
            !space
                .view(slot.reserved_id(2), &values)
                .expect("a view")
                .hidden
        );
        assert_eq!(space.find("FX 1 Amount", &values), None);
        let defaults = space.slot_defaults(0, 2);
        assert_eq!(defaults.len(), 3);
        assert!((defaults[2].1 - 0.3).abs() < 1e-12);
        assert!(space.is_selector(slot.selector));
        assert_eq!(
            space.locate(slot.reserved_id(1)),
            Some(Location::Reserved { slot: 0, param: 1 })
        );
    }

    #[test]
    fn pages_follow_the_modules() {
        let space = space();
        let values = |_: ParamId| Some(2.0);
        let pages = space.pages(&values);
        let names: Vec<&str> = pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Osc A", "Osc B", "FX 1", "FX 2", "Global"]);
        let fx = &pages[2];
        assert_eq!(fx.section, "FX");
        assert_eq!(fx.params[0], Some(space.slots()[0].selector));
        assert_eq!(fx.params[3], Some(space.slots()[0].reserved_id(2)));
        assert_eq!(fx.params[4], None);
    }

    #[test]
    fn paths_read_back() {
        for text in ["osc \"A\" / param \"Level\"", "param \"Volume\""] {
            assert_eq!(text.parse::<ParamPath>().expect("a path").to_string(), text);
        }
        assert!("osc A / Level".parse::<ParamPath>().is_err());
        assert!("osc \"A\" / knob \"Level\"".parse::<ParamPath>().is_err());
    }

    #[test]
    fn broken_spaces_are_refused() {
        let twice = ParamSpace::builder()
            .module(osc(), &["A"])
            .module(osc(), &["B"])
            .build();
        assert!(twice.unwrap_err().contains("number 1"));
        let high = ParamSpace::builder()
            .module(
                ModuleType::new(3, "x", "X").param(ParamDescriptor::toggle(
                    ParamId(0x8000),
                    "On",
                    false,
                )),
                &["1"],
            )
            .build();
        assert!(high.is_err());
        let flat = ParamSpace::builder()
            .global(ParamDescriptor::toggle(ParamId(1 << 24), "On", false))
            .build();
        assert!(flat.is_err());
        let quoted = ParamSpace::builder()
            .module(ModuleType::new(3, "x", "X \"1\""), &["1"])
            .build();
        assert!(quoted.is_err());
    }
}
