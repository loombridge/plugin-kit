//! Platforms the kit doesn't target: no way to read or raise priority.

use std::time::Duration;

use crate::thread::{Priority, Promotion};

pub(crate) fn warm_up() {}

#[derive(Debug, Default)]
pub(crate) struct HeldPriority;

impl HeldPriority {
    pub(crate) fn revert(&mut self) {}

    pub(crate) fn forget(&mut self) {}
}

pub(crate) fn current_priority() -> Priority {
    Priority::Unknown
}

pub(crate) fn promote(_period: Duration, _held: &mut HeldPriority) -> Promotion {
    Promotion::Unsupported
}
