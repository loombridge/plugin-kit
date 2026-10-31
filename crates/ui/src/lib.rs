//! The plug-in kit's interface crate: plug-in editors drawn with egui and
//! embedded in the host's window through `baseview`, a generic interface
//! for any plug-in's parameters, theming tokens and the global zoom.
//!
//! A plug-in gets the generic interface by returning it from
//! [`Plugin::editor`](lspk_plugin::Plugin::editor):
//!
//! ```no_run
//! # use lspk_params::ParamDescriptor;
//! # fn params() -> Vec<ParamDescriptor> { Vec::new() }
//! fn editor() -> Option<Box<dyn lspk_plugin::Editor>> {
//!     Some(Box::new(lspk_ui::generic_editor("Example Gain", &params())))
//! }
//! ```
//!
//! and an interface of its own by implementing [`View`] and handing it to
//! an [`EguiEditor`], drawing its parameters with [`param_control`].
//!
//! Every control sends its changes to the host as gestures (begin, value,
//! end), so hosts record automation and undo; exposes its role, name and
//! value through AccessKit; and works from the keyboard
//! ([`controls`] says how). Headless crates never depend on this one: the
//! format layer drives an editor through [`lspk_plugin::Editor`] alone.

mod accessibility;
pub mod controls;
pub mod editor;
pub mod fonts;
pub mod generic;
pub mod offscreen;
mod raster;
pub mod theme;

pub use controls::{ControlKind, param_control};
pub use editor::{EditorOptions, EditorUi, EguiEditor, View, open_windows};
pub use generic::GenericView;
pub use theme::{MAX_ZOOM, MIN_ZOOM, Theme};

/// The re-exported egui, the version a [`View`] draws with.
pub use egui;

use lspk_params::ParamDescriptor;

/// The generic interface for a plug-in named `title` with the parameters
/// `params`: each drawn with its control, in a grid that reflows as the
/// window is resized, opening at a size that fits them.
#[must_use]
pub fn generic_editor(title: &str, params: &[ParamDescriptor]) -> EguiEditor {
    let theme = Theme::default();
    let mut options = EditorOptions::new(generic::generic_size(params.len(), &theme));
    options.min_size = generic::generic_min_size(&theme);
    options.theme = theme;
    options.title = title.to_owned();
    let title = title.to_owned();
    EguiEditor::new(options, move || GenericView::new(title.clone()))
}
