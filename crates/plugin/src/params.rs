//! Every parameter's value now, shared by a plug-in's main thread and its
//! audio thread, and, for a plug-in with a [`ParamSpace`], what each
//! parameter means now.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use lspk_params::{ParamDescriptor, ParamId, ParamSpace, ParamView};

use crate::{Plugin, PluginError};

/// The ID no parameter may have: CLAP's "no parameter".
const INVALID_ID: u32 = u32::MAX;

/// Each of a plug-in's parameters with its value now, in its own units.
///
/// Reading and writing a value is an atomic load or store and a binary
/// search, so either thread may do it at any time: nothing allocates, locks
/// or blocks. Each value is independent of the others; a reader may see one
/// change before another made just before it.
#[derive(Debug)]
pub struct ParamValues {
    descriptors: Box<[ParamDescriptor]>,
    values: Box<[AtomicU64]>,
    /// How many times a value has changed ([`generation`](Self::generation)).
    changes: AtomicU64,
    /// `(id, index)` sorted by ID.
    by_id: Box<[(ParamId, usize)]>,
    /// The plug-in's parameter space, if it has one.
    space: Option<Arc<ParamSpace>>,
}

impl ParamValues {
    /// The parameters `descriptors` describe, in their order, each at its
    /// default.
    ///
    /// # Errors
    ///
    /// A descriptor that breaks a rule ([`ParamDescriptor::check`]), an ID
    /// used twice, or the ID `u32::MAX`, which formats reserve.
    pub fn new(descriptors: Vec<ParamDescriptor>) -> Result<Self, PluginError> {
        for descriptor in &descriptors {
            descriptor.check().map_err(PluginError::new)?;
            if descriptor.id.0 == INVALID_ID {
                return Err(PluginError::new(format!(
                    "parameter {} ({}) has the ID formats reserve",
                    descriptor.id.0, descriptor.name
                )));
            }
        }
        let mut by_id: Vec<(ParamId, usize)> = descriptors
            .iter()
            .enumerate()
            .map(|(index, d)| (d.id, index))
            .collect();
        by_id.sort_unstable();
        if let Some(pair) = by_id.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(PluginError::new(format!(
                "two parameters have the ID {}",
                pair[0].0.0
            )));
        }
        let values = descriptors
            .iter()
            .map(|d| AtomicU64::new(d.clamp(d.default).to_bits()))
            .collect();
        Ok(Self {
            descriptors: descriptors.into_boxed_slice(),
            values,
            changes: AtomicU64::new(0),
            by_id: by_id.into_boxed_slice(),
            space: None,
        })
    }

    /// The parameters `descriptors` describe, as [`new`](Self::new) makes
    /// them, read through `space`, which must describe every one of them.
    ///
    /// # Errors
    ///
    /// As [`new`](Self::new), or a parameter of the space that isn't among
    /// `descriptors`.
    pub fn with_space(
        descriptors: Vec<ParamDescriptor>,
        space: Arc<ParamSpace>,
    ) -> Result<Self, PluginError> {
        let mut values = Self::new(descriptors)?;
        if let Some(missing) = space
            .descriptors()
            .iter()
            .find(|d| values.descriptor(d.id) != Some(*d))
        {
            return Err(PluginError::new(format!(
                "the plug-in's parameters don't match its space at {} ({})",
                missing.id.0, missing.name
            )));
        }
        values.space = Some(space);
        Ok(values)
    }

    /// Plug-in `plugin`'s parameters ([`Plugin::params`]), read through
    /// its space if it has one ([`Plugin::param_space`]), each at its
    /// default.
    ///
    /// # Errors
    ///
    /// As [`with_space`](Self::with_space).
    pub fn for_plugin<P: Plugin>(plugin: &P) -> Result<Self, PluginError> {
        match plugin.param_space() {
            Some(space) => Self::with_space(plugin.params(), Arc::new(space)),
            None => Self::new(plugin.params()),
        }
    }

    /// The plug-in's parameter space, if it has one.
    #[must_use]
    pub fn space(&self) -> Option<&Arc<ParamSpace>> {
        self.space.as_ref()
    }

    /// What parameter `id` is now: its real name, path and descriptor, and
    /// whether it's hidden. Without a space, the descriptor as it is.
    #[must_use]
    pub fn view(&self, id: ParamId) -> Option<ParamView> {
        match &self.space {
            Some(space) => space.view(id, &|id| self.get(id)),
            None => self.descriptor(id).map(ParamView::plain),
        }
    }

    /// Every parameter's [view](Self::view), in order, hidden ones too.
    #[must_use]
    pub fn views(&self) -> Vec<ParamView> {
        self.descriptors
            .iter()
            .filter_map(|d| self.view(d.id))
            .collect()
    }

    /// The parameter `text` names: a path (`osc "A" / param "Level"`) or a
    /// name as hosts show it, in any case; never a hidden one.
    #[must_use]
    pub fn find(&self, text: &str) -> Option<ParamId> {
        match &self.space {
            Some(space) => space.find(text, &|id| self.get(id)),
            None => {
                let path: Option<lspk_params::ParamPath> = text.trim().parse().ok();
                self.descriptors
                    .iter()
                    .find(|d| match &path {
                        Some(path) if path.module.is_none() => {
                            d.name.eq_ignore_ascii_case(&path.param)
                        }
                        Some(_) => false,
                        None => d.name.eq_ignore_ascii_case(text.trim()),
                    })
                    .map(|d| d.id)
            }
        }
    }

    /// Whether `id` is a slot's type parameter, whose change changes what
    /// other parameters mean. Allocates nothing.
    #[must_use]
    pub fn is_selector(&self, id: ParamId) -> bool {
        self.space.as_ref().is_some_and(|s| s.is_selector(id))
    }

    /// How many parameters there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }

    /// Every parameter's descriptor, in order.
    #[must_use]
    pub fn descriptors(&self) -> &[ParamDescriptor] {
        &self.descriptors
    }

    /// Where parameter `id` is in the order.
    #[must_use]
    pub fn index_of(&self, id: ParamId) -> Option<usize> {
        self.by_id
            .binary_search_by_key(&id, |(id, _)| *id)
            .ok()
            .map(|at| self.by_id[at].1)
    }

    /// Parameter `id`'s descriptor.
    #[must_use]
    pub fn descriptor(&self, id: ParamId) -> Option<&ParamDescriptor> {
        self.index_of(id).map(|index| &self.descriptors[index])
    }

    /// Parameter `id`'s value now.
    #[must_use]
    pub fn get(&self, id: ParamId) -> Option<f64> {
        self.index_of(id).map(|index| self.value_at(index))
    }

    /// The value of the parameter at `index` in the order.
    ///
    /// # Panics
    ///
    /// If there's no parameter at `index`.
    #[must_use]
    pub fn value_at(&self, index: usize) -> f64 {
        f64::from_bits(self.values[index].load(Relaxed))
    }

    /// Sets parameter `id` to `value`, clamped to its range (and onto a
    /// step, for a stepped one), and returns the value it now has; `None`
    /// if there's no such parameter.
    ///
    /// This only records the value. To tell the processor and the host,
    /// edit it through [`Host`](crate::Host) or
    /// [`ProcessContext`](crate::ProcessContext).
    pub fn set(&self, id: ParamId, value: f64) -> Option<f64> {
        let index = self.index_of(id)?;
        let value = self.descriptors[index].clamp(value);
        if self.values[index].swap(value.to_bits(), Relaxed) != value.to_bits() {
            self.changes.fetch_add(1, Relaxed);
        }
        Some(value)
    }

    /// How many times a value has changed since these were made, from any
    /// thread. A reader that wants to know whether anything changed
    /// compares it with what it saw last, as an editor does to redraw only
    /// when it must.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.changes.load(Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lspk_params::db_to_gain;

    fn values() -> ParamValues {
        ParamValues::new(vec![
            ParamDescriptor::gain(ParamId(7), "Gain", 6.0, 0.0),
            ParamDescriptor::choice(ParamId(2), "Mode", &["A", "B", "C"], 1),
        ])
        .expect("valid parameters")
    }

    #[test]
    fn values_start_at_their_defaults_and_clamp() {
        let values = values();
        assert_eq!(values.len(), 2);
        assert_eq!(values.get(ParamId(7)), Some(1.0));
        assert_eq!(values.get(ParamId(2)), Some(1.0));
        assert_eq!(values.index_of(ParamId(2)), Some(1));
        assert_eq!(values.set(ParamId(7), 100.0), Some(db_to_gain(6.0)));
        assert_eq!(values.set(ParamId(2), 1.6), Some(2.0));
        assert_eq!(values.get(ParamId(2)), Some(2.0));
        assert_eq!(values.set(ParamId(3), 1.0), None);
        assert_eq!(values.get(ParamId(3)), None);
    }

    #[test]
    fn the_generation_counts_only_values_that_change() {
        let values = values();
        let before = values.generation();
        values.set(ParamId(2), 1.0);
        assert_eq!(values.generation(), before, "the same value is no change");
        values.set(ParamId(2), 2.0);
        assert_eq!(values.generation(), before + 1);
        values.set(ParamId(3), 1.0);
        assert_eq!(values.generation(), before + 1, "no such parameter");
    }

    #[test]
    fn broken_parameter_lists_are_refused() {
        let twice = ParamValues::new(vec![
            ParamDescriptor::toggle(ParamId(1), "A", true),
            ParamDescriptor::toggle(ParamId(1), "B", false),
        ]);
        assert!(twice.unwrap_err().message().contains("two parameters"));
        let reserved =
            ParamValues::new(vec![ParamDescriptor::toggle(ParamId(u32::MAX), "A", true)]);
        assert!(reserved.is_err());
        let mut fixed = ParamDescriptor::toggle(ParamId(1), "A", true);
        fixed.automatable = false;
        assert!(ParamValues::new(vec![fixed]).is_err());
    }
}
