//! Contexts: the part of the interface a command applies in.

use core::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a command applies: the part of the interface that has focus.
///
/// For now these are Beatlayer's contexts, from its UI parity reference
/// (the shortcut reference), so a command and the shortcuts bound
/// to it name places the same way. Beatlayer's `cargo xtask check` fails if
/// this list and the reference's drift apart.
///
/// Contexts say where a person reaches a command: they decide which command
/// a key runs when the same keys do different things in different places,
/// and where a menu item is enabled. An agent can run any command whatever
/// has focus.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Context {
    /// Anywhere in a main window, unless a narrower context binds the same
    /// keys.
    Global,
    /// A secondary window or dialog has focus.
    Dialog,
    /// Tab moves the keyboard focus between controls.
    TabFocus,
    /// A name or other text is being edited.
    TextField,
    /// A value control (knob, slider, or number box) has focus.
    ValueField,
    /// A menu is open.
    Menu,
    /// The Session View has focus.
    Session,
    /// The Arrangement View has focus.
    Arrangement,
    /// A track's title bar is selected, in either view.
    TrackHeaders,
    /// A take lane is selected in the Arrangement.
    TakeLanes,
    /// A loop brace, or a clip's start or end marker, is selected.
    LoopBrace,
    /// An automation lane or the Clip View's envelope editor has focus.
    Envelope,
    /// The Clip View has focus, whichever editor it shows.
    ClipView,
    /// The Clip View shows an audio clip's sample editor.
    SampleEditor,
    /// The Clip View shows a MIDI clip's note editor.
    NoteEditor,
    /// Notes are selected in the note editor.
    NoteSelection,
    /// The Device View has focus.
    DeviceView,
    /// A rack's chain list has focus.
    RackChains,
    /// A drum rack, a pad, or a sampler device is selected, so a sample can
    /// be swapped.
    SampleSwap,
    /// The browser's content pane has focus.
    Browser,
    /// The browser's sidebar has focus.
    BrowserSidebar,
    /// The browser's search field has focus.
    BrowserSearch,
    /// The computer keyboard plays notes.
    ComputerMidiKeyboard,
    /// The Settings window has focus.
    Settings,
}

impl Context {
    /// Every context, in the order the UI parity reference lists them.
    pub const ALL: [Self; 24] = [
        Self::Global,
        Self::Dialog,
        Self::TabFocus,
        Self::TextField,
        Self::ValueField,
        Self::Menu,
        Self::Session,
        Self::Arrangement,
        Self::TrackHeaders,
        Self::TakeLanes,
        Self::LoopBrace,
        Self::Envelope,
        Self::ClipView,
        Self::SampleEditor,
        Self::NoteEditor,
        Self::NoteSelection,
        Self::DeviceView,
        Self::RackChains,
        Self::SampleSwap,
        Self::Browser,
        Self::BrowserSidebar,
        Self::BrowserSearch,
        Self::ComputerMidiKeyboard,
        Self::Settings,
    ];

    /// The context's ID in the UI parity reference, such as `track-headers`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Dialog => "dialog",
            Self::TabFocus => "tab-focus",
            Self::TextField => "text-field",
            Self::ValueField => "value-field",
            Self::Menu => "menu",
            Self::Session => "session",
            Self::Arrangement => "arrangement",
            Self::TrackHeaders => "track-headers",
            Self::TakeLanes => "take-lanes",
            Self::LoopBrace => "loop-brace",
            Self::Envelope => "envelope",
            Self::ClipView => "clip-view",
            Self::SampleEditor => "sample-editor",
            Self::NoteEditor => "note-editor",
            Self::NoteSelection => "note-selection",
            Self::DeviceView => "device-view",
            Self::RackChains => "rack-chains",
            Self::SampleSwap => "sample-swap",
            Self::Browser => "browser",
            Self::BrowserSidebar => "browser-sidebar",
            Self::BrowserSearch => "browser-search",
            Self::ComputerMidiKeyboard => "computer-midi-keyboard",
            Self::Settings => "settings",
        }
    }

    /// The context with this ID, if there is one.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|context| context.id() == id)
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_serde_and_round_trip() {
        for context in Context::ALL {
            assert_eq!(
                serde_json::to_value(context).unwrap(),
                context.id(),
                "{context:?}"
            );
            assert_eq!(Context::from_id(context.id()), Some(context));
        }
        assert_eq!(Context::from_id("nowhere"), None);
    }

    #[test]
    fn all_lists_each_context_once() {
        let mut ids: Vec<_> = Context::ALL.iter().map(|c| c.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), Context::ALL.len());
    }
}
