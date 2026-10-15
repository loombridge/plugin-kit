//! A/B compare: the two patches a plug-in keeps side by side, with the one
//! showing.

use crate::Preset;

/// Which of A/B compare's two patches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    /// The first patch.
    #[default]
    A,
    /// The second.
    B,
}

impl Side {
    /// Its letter, `a` or `b`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }

    /// The other side.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// Two patches kept side by side, as A/B compare holds them: one is
/// showing, and the other is kept for the switch.
#[derive(Clone, Debug, Default)]
pub struct Compare {
    a: Option<Preset>,
    b: Option<Preset>,
    showing: Side,
}

impl Compare {
    /// An empty compare, showing [`Side::A`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The side showing.
    #[must_use]
    pub fn showing(&self) -> Side {
        self.showing
    }

    /// The patch kept on `side`, if any.
    #[must_use]
    pub fn stored(&self, side: Side) -> Option<&Preset> {
        match side {
            Side::A => self.a.as_ref(),
            Side::B => self.b.as_ref(),
        }
    }

    /// Keeps `preset` as `side`'s patch and makes it the showing one: what
    /// the person is listening to when they store a side is that side's.
    pub fn store(&mut self, side: Side, preset: Preset) {
        match side {
            Side::A => self.a = Some(preset),
            Side::B => self.b = Some(preset),
        }
        self.showing = side;
    }

    /// The other side's patch, and makes it the showing one. `None` — and
    /// the showing side unchanged — when nothing is stored there.
    pub fn switch(&mut self) -> Option<&Preset> {
        let other = self.showing.other();
        self.stored(other)?;
        self.showing = other;
        self.stored(other)
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

    #[test]
    fn an_empty_compare_shows_a_and_switches_nowhere() {
        let mut compare = Compare::new();
        assert_eq!(compare.showing(), Side::A);
        assert_eq!(compare.switch(), None);
        assert_eq!(compare.showing(), Side::A);
    }

    #[test]
    fn switching_shows_the_other_side_and_keeps_both() {
        let mut compare = Compare::new();
        compare.store(Side::A, preset(1.0));
        compare.store(Side::B, preset(2.0));
        assert_eq!(compare.showing(), Side::B, "storing a side shows it");
        assert_eq!(compare.switch().unwrap().value(ParamId(1)), Some(1.0));
        assert_eq!(compare.showing(), Side::A);
        assert_eq!(compare.switch().unwrap().value(ParamId(1)), Some(2.0));
        assert_eq!(compare.showing(), Side::B);
        // Storing a side again replaces only that side, and shows it.
        compare.store(Side::A, preset(3.0));
        assert_eq!(
            compare.stored(Side::A).unwrap().value(ParamId(1)),
            Some(3.0)
        );
        assert_eq!(
            compare.stored(Side::B).unwrap().value(ParamId(1)),
            Some(2.0)
        );
        assert_eq!(compare.showing(), Side::A);
        // A switch with only the showing side stored stays put.
        let mut single = Compare::new();
        single.store(Side::A, preset(1.0));
        assert_eq!(single.switch(), None);
        assert_eq!(single.showing(), Side::A);
    }
}
