//! Permission classes: what a command may touch, and so whether an agent
//! must ask before running it.

use core::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a command may touch.
///
/// The agent API runs `read` and `edit` commands straight away, and asks the
/// user before a `destructive` or `external` one unless the user has turned
/// on autonomous mode for that agent.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Changes nothing in the project or on disk: queries, and moving the
    /// keyboard focus or the view.
    Read,
    /// An undoable change to the project, or a preference that's trivially
    /// changed back.
    Edit,
    /// Removes project content (deleting tracks, clips, or devices), or
    /// loses work that undo can't restore: discards unsaved changes,
    /// overwrites a file, or quits. An agent asks first even when undo could
    /// restore the content.
    Destructive,
    /// Reaches outside the project: files outside the project folder, the
    /// network, audio and MIDI devices, or other applications.
    External,
}

impl Permission {
    /// Every class, from the least to the most reach.
    pub const ALL: [Self; 4] = [Self::Read, Self::Edit, Self::Destructive, Self::External];

    /// The class's name as it's serialised, such as `edit`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "edit",
            Self::Destructive => "destructive",
            Self::External => "external",
        }
    }

    /// What a command in this class may do, in a sentence.
    #[must_use]
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Read => {
                "Changes nothing in the project or on disk: queries, and moving the keyboard \
                 focus or the view."
            }
            Self::Edit => {
                "An undoable change to the project, or a preference that's trivially changed \
                 back."
            }
            Self::Destructive => {
                "Removes project content (deleting tracks, clips, or devices), or loses work \
                 that undo can't restore: discards unsaved changes, overwrites a file, or \
                 quits. An agent asks first even when undo could restore the content."
            }
            Self::External => {
                "Reaches outside the project: files outside the project folder, the network, \
                 audio and MIDI devices, or other applications."
            }
        }
    }

    /// Whether an agent must ask the user before running a command in this
    /// class (unless the user has turned on autonomous mode for it).
    #[must_use]
    pub const fn asks_first(self) -> bool {
        matches!(self, Self::Destructive | Self::External)
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_serde() {
        for permission in Permission::ALL {
            assert_eq!(
                serde_json::to_value(permission).unwrap(),
                permission.id(),
                "{permission:?}"
            );
        }
    }

    #[test]
    fn only_destructive_and_external_ask_first() {
        let asking: Vec<_> = Permission::ALL
            .into_iter()
            .filter(|p| p.asks_first())
            .collect();
        assert_eq!(asking, [Permission::Destructive, Permission::External]);
    }
}
