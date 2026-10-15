//! The undo history: the patches a patch has been, for undo and redo.

use crate::Preset;

/// The patches a patch has been, newest last, as a plug-in's undo history
/// holds them. Recording a patch forgets what was undone after it.
#[derive(Clone, Debug)]
pub struct History {
    limit: usize,
    past: Vec<Preset>,
    future: Vec<Preset>,
}

impl History {
    /// A history holding the newest `limit` patches, at least one.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            past: Vec::new(),
            future: Vec::new(),
        }
    }

    /// How many patches it can undo, its newest first.
    #[must_use]
    pub fn len(&self) -> usize {
        self.past.len()
    }

    /// Whether there's nothing to undo.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.past.is_empty()
    }

    /// Records `preset` as the patch before a change the person makes: the
    /// patch [`undo`](Self::undo) returns next. It forgets every patch a
    /// [`redo`](Self::redo) hadn't taken back yet, and drops the oldest
    /// patch when the history is full.
    pub fn record(&mut self, preset: Preset) {
        self.future.clear();
        self.past.push(preset);
        if self.past.len() > self.limit {
            self.past.remove(0);
        }
    }

    /// The newest recorded patch, and records `current` as the one redo
    /// takes back to; `None` when nothing was recorded.
    pub fn undo(&mut self, current: &Preset) -> Option<Preset> {
        let restored = self.past.pop()?;
        self.push_future(current.clone());
        Some(restored)
    }

    /// The patch undone last, and records `current` as the one undo returns
    /// to; `None` when nothing was undone.
    pub fn redo(&mut self, current: &Preset) -> Option<Preset> {
        let restored = self.future.pop()?;
        self.past.push(current.clone());
        if self.past.len() > self.limit {
            self.past.remove(0);
        }
        Some(restored)
    }

    /// Adds `preset` to the redo side, dropping the oldest when full.
    fn push_future(&mut self, preset: Preset) {
        self.future.push(preset);
        if self.future.len() > self.limit {
            self.future.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use lspk_params::ParamId;

    fn preset(amount: f64) -> Preset {
        Preset {
            values: vec![(ParamId(1), amount)],
            state: json!({ "matrix": [] }),
        }
    }

    fn amount(preset: &Option<Preset>) -> Option<f64> {
        preset.as_ref()?.value(ParamId(1))
    }

    #[test]
    fn undo_and_redo_walk_the_patches_both_ways() {
        let mut history = History::new(8);
        assert!(history.is_empty());
        assert_eq!(amount(&history.undo(&preset(9.0))), None);
        assert_eq!(amount(&history.redo(&preset(9.0))), None);
        // The patch was 1.0, then 2.0, and is 3.0 now.
        history.record(preset(1.0));
        history.record(preset(2.0));
        assert_eq!(history.len(), 2);
        let back = history.undo(&preset(3.0)).expect("a patch");
        assert_eq!(back.value(ParamId(1)), Some(2.0));
        let back = history.undo(&back).expect("a patch");
        assert_eq!(back.value(ParamId(1)), Some(1.0));
        assert!(history.is_empty());
        assert_eq!(amount(&history.undo(&back)), None);
        // Redo takes them back, newest undone first.
        let forward = history.redo(&back).expect("a patch");
        assert_eq!(forward.value(ParamId(1)), Some(2.0));
        let forward = history.redo(&forward).expect("a patch");
        assert_eq!(forward.value(ParamId(1)), Some(3.0));
        assert_eq!(amount(&history.redo(&forward)), None);
        // A recorded patch forgets what was undone.
        history.undo(&forward).expect("a patch");
        history.record(preset(4.0));
        assert_eq!(amount(&history.redo(&forward)), None);
    }

    #[test]
    fn the_history_keeps_the_newest_patches_only() {
        let mut history = History::new(2);
        history.record(preset(1.0));
        history.record(preset(2.0));
        history.record(preset(3.0));
        assert_eq!(history.len(), 2);
        assert_eq!(
            amount(&history.undo(&preset(4.0))),
            Some(3.0),
            "the newest patch undone first"
        );
        assert_eq!(amount(&history.undo(&preset(3.0))), Some(2.0));
        assert_eq!(amount(&history.undo(&preset(2.0))), None);
        // A limit of zero still holds the one patch undo needs.
        let mut history = History::new(0);
        history.record(preset(1.0));
        assert_eq!(amount(&history.undo(&preset(2.0))), Some(1.0));
    }
}
