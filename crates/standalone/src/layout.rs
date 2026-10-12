//! The app's window, as sizes: the plug-in's editor fills it from the top
//! left, and the strip runs along the bottom. The window follows the
//! editor's size, and when the person resizes the window, the editor takes
//! the room above the strip.
//!
//! The window counts in points. The editor counts in its window system's
//! units ([`lspk_plugin::editor`]'s "Sizes"): points on macOS, pixels on
//! Windows and X11, where the window's scale factor turns one into the
//! other.

use lspk_plugin::{EditorSize, WindowApi};

/// The strip's height, in points.
pub(crate) const STRIP_HEIGHT: f64 = 30.0;

/// The window's narrowest, in points, so the strip has room whatever the
/// editor's size.
pub(crate) const MIN_WIDTH: f64 = 360.0;

/// How the editor's units relate to the window's points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layout {
    pub(crate) api: WindowApi,
    /// The window's scale factor.
    pub(crate) scale: f64,
}

impl Layout {
    /// How many of the editor's units make a point.
    fn units_per_point(self) -> f64 {
        if self.api.uses_logical_size() || !(self.scale.is_finite() && self.scale > 0.0) {
            1.0
        } else {
            self.scale
        }
    }

    /// The window's size, in points, for an editor of `editor`.
    pub(crate) fn window_for(self, editor: EditorSize) -> (f64, f64) {
        let per = self.units_per_point();
        (
            (f64::from(editor.width) / per).max(MIN_WIDTH),
            f64::from(editor.height) / per + STRIP_HEIGHT,
        )
    }

    /// The room for the editor in a window of `window` points, in the
    /// editor's units.
    pub(crate) fn editor_room(self, window: (f64, f64)) -> EditorSize {
        let per = self.units_per_point();
        let units = |points: f64| (points.max(0.0) * per).round() as u32;
        EditorSize::new(units(window.0), units(window.1 - STRIP_HEIGHT))
    }
}

/// What the strip shows, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripItem {
    /// Where the audio stands: the device and the sample rate, or why it
    /// isn't playing.
    Status,
    /// The output's level.
    Meter,
    /// The computer keyboard's on/off switch.
    Keyboard,
    /// The computer keyboard's octave.
    Octave,
    /// The computer keyboard's velocity.
    Velocity,
    /// Opens the Settings dialog, where there's no menu bar.
    Settings,
}

/// The strip's items, for a plug-in that does or doesn't take notes, on a
/// platform with or without a menu bar.
pub(crate) fn strip(takes_notes: bool, menu_bar: bool) -> Vec<StripItem> {
    let mut items = vec![StripItem::Status, StripItem::Meter];
    if takes_notes {
        items.extend([StripItem::Keyboard, StripItem::Octave, StripItem::Velocity]);
    }
    if !menu_bar {
        items.push(StripItem::Settings);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_editor_and_the_strip() {
        let mac = Layout {
            api: WindowApi::AppKit,
            scale: 2.0,
        };
        assert_eq!(
            mac.window_for(EditorSize::new(800, 500)),
            (800.0, 500.0 + STRIP_HEIGHT)
        );
        assert_eq!(
            mac.editor_room((800.0, 500.0 + STRIP_HEIGHT)),
            EditorSize::new(800, 500)
        );
        // Pixels on Windows and X11.
        let x11 = Layout {
            api: WindowApi::X11,
            scale: 1.5,
        };
        assert_eq!(
            x11.window_for(EditorSize::new(1200, 750)),
            (800.0, 500.0 + STRIP_HEIGHT)
        );
        assert_eq!(
            x11.editor_room((800.0, 500.0 + STRIP_HEIGHT)),
            EditorSize::new(1200, 750)
        );
    }

    #[test]
    fn a_small_editor_still_leaves_the_strip_its_width() {
        let layout = Layout {
            api: WindowApi::Win32,
            scale: 1.0,
        };
        assert_eq!(
            layout.window_for(EditorSize::new(200, 100)),
            (MIN_WIDTH, 100.0 + STRIP_HEIGHT)
        );
        assert_eq!(layout.editor_room((100.0, 10.0)), EditorSize::new(100, 0));
    }

    #[test]
    fn the_strip_shows_the_keyboard_only_for_an_instrument() {
        use StripItem::*;
        assert_eq!(
            strip(true, true),
            [Status, Meter, Keyboard, Octave, Velocity]
        );
        assert_eq!(strip(false, true), [Status, Meter]);
        assert_eq!(strip(false, false), [Status, Meter, Settings]);
    }
}
