//! The command registry: every action a person or an agent can take in a
//! program built on the plug-in kit.
//!
//! A command is a named action with:
//!
//! - an ID that never changes (`track.create`), which key maps, MIDI maps,
//!   agents, and logs use;
//! - a label a person sees (`Insert Track`);
//! - a description written for a model: what it does, when to use it, and
//!   what it changes;
//! - a [`Permission`] class (read, edit, destructive, or external), which
//!   decides whether an agent asks the user first;
//! - the [`Context`]s it applies in (for now, Beatlayer's, as its UI parity
//!   reference names them);
//! - default [`Binding`]s: shortcuts from a shortcut reference, by ID;
//! - JSON Schemas of its arguments and result, generated from the Rust
//!   types with `schemars`.
//!
//! Menus, shortcuts, key and MIDI mapping, the agent API, and tests all
//! find commands in a [`Registry`], so an action has one implementation
//! however it's reached. [`Registry::problems`] and
//! [`Registry::binding_problems`] fail the build's tests when a command
//! lacks any of the above, and [`reference::markdown`] renders the
//! reference.
//!
//! This crate holds only the registry's types and checks. It depends on no
//! other kit crate and on nothing that draws, so every crate that
//! implements commands can register them: a document, an engine's
//! transport, project files, and an app's shell.
//!
//! # Registering commands
//!
//! A crate that implements commands has a function that adds their specs
//! to a registry, and a function that runs them by ID with JSON arguments:
//!
//! ```
//! use lspk_commands::{Binding, CommandSpec, Context, NoArgs, Permission, RegisterError, Registry};
//!
//! pub fn register(registry: &mut Registry) -> Result<(), RegisterError> {
//!     registry.register(
//!         CommandSpec::new("view.toggle_session_arrangement", "Toggle Session/Arrangement View")
//!             .description(
//!                 "Switches the main window between the Session View and the Arrangement \
//!                  View. Use it to show the other view. It changes only what's on screen, \
//!                  not the project.",
//!             )
//!             .permission(Permission::Read)
//!             .context(Context::Global)
//!             .binding("S-VIEW-003")
//!             .args::<NoArgs>()
//!             .result::<()>(),
//!     )
//! }
//!
//! let mut registry = Registry::new();
//! register(&mut registry)?;
//! assert!(registry.problems().is_empty());
//! # Ok::<(), RegisterError>(())
//! ```

mod context;
mod permission;
pub mod reference;
mod registry;
mod schema;
mod shortcuts;
mod spec;

pub use context::Context;
pub use permission::Permission;
pub use registry::{Problem, Problems, RegisterError, Registry};
pub use schema::{
    NoArgs, args_schema, property, result_schema, tagged_variant, validate, validate_in,
};
/// The JSON Schema crate the registry's schemas come from, so providers can
/// name its types.
pub use schemars;
pub use shortcuts::{Shortcut, ShortcutTable};
pub use spec::{Binding, CommandSpec};
