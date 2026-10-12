//! The agent-commands CLAP extension, `com.loombridge.agent-commands/1`:
//! how a host lists a plug-in's commands and runs them, so it can serve
//! them through its own agent interface.
//!
//! Every kit plug-in offers it: the CLAP export (the `clap` feature) serves
//! [`Plugin::register_commands`](crate::Plugin::register_commands) and
//! [`Plugin::run_command`](crate::Plugin::run_command) through it, with
//! nothing more from the plug-in. A host built on `clack-host` reaches it
//! with [`PluginAgentCommands`] (the `clap-host` feature); any other host
//! uses the declaration below.
//!
//! # The extension
//!
//! ```c
//! static const char CLAP_EXT_LOOMBRIDGE_AGENT_COMMANDS[] = "com.loombridge.agent-commands/1";
//!
//! typedef struct clap_plugin_agent_commands {
//!    // [main-thread]
//!    // Writes the plug-in's commands to `out` as UTF-8 JSON (see "The
//!    // list"). Returns false if it couldn't.
//!    bool(CLAP_ABI *list)(const clap_plugin_t *plugin, const clap_ostream_t *out);
//!
//!    // [main-thread]
//!    // Runs the command `id` (a NUL-terminated UTF-8 string) with `args`,
//!    // NUL-terminated UTF-8 JSON matching its argument schema, or NULL for
//!    // no arguments. On success, writes the command's result to `out` as
//!    // UTF-8 JSON matching its result schema, and returns true. Otherwise
//!    // writes why, as UTF-8 text for a person or a model to read, and
//!    // returns false.
//!    bool(CLAP_ABI *run)(const clap_plugin_t *plugin,
//!                        const char            *id,
//!                        const char            *args,
//!                        const clap_ostream_t  *out);
//! } clap_plugin_agent_commands_t;
//! ```
//!
//! The version is the number after the slash. A change that a host written
//! for an earlier version would misread gets a new number; fields only ever
//! join the list's objects, so a host ignores fields it doesn't know.
//!
//! # The list
//!
//! A JSON array with one object per command, as `lspk-commands` serialises
//! a [`CommandSpec`]:
//!
//! | Field | What it is |
//! |---|---|
//! | `id` | The command's ID within the plug-in, lower-case words joined by underscores in dot-separated parts, such as `gain.reset` |
//! | `label` | What a person sees, such as `Reset Gain` |
//! | `description` | What it does, when to use it and what it changes, in whole sentences written for a model |
//! | `permission` | `read`, `edit`, `destructive` or `external`: what it may touch, and so whether an agent asks the person first |
//! | `contexts` | Where a person reaches it |
//! | `args` | The JSON Schema of its arguments: always an object schema |
//! | `result` | The JSON Schema of its result |
//!
//! Commands in the `agent` area set up the plug-in's own agent server and
//! are for the person alone. A host that serves the plug-in's commands to
//! agents itself leaves them out.
//!
//! # The host stays in step
//!
//! A command that changes a parameter does it as a person would: the
//! plug-in asks the host for a parameter flush and sends a gesture begin,
//! the value and a gesture end from its next process or flush, and marks
//! its state dirty. So a host that records gestures as undo steps or
//! automation records an agent's command the same way. A host that wants
//! the change at once, in the step it records for the command, reads the
//! parameters' values after `run` returns: they already hold the new ones.

use lspk_commands::{CommandSpec, Registry};
use serde_json::Value;

/// The extension's ID, with its version.
pub const ID: &std::ffi::CStr = c"com.loombridge.agent-commands/1";

/// The area of the commands a host serving agents leaves out: the plug-in's
/// own agent server's settings, which are the person's.
pub const PERSON_ONLY_AREA: &str = "agent";

/// The list a plug-in writes for `registry`'s commands.
#[must_use]
pub fn encode_list(registry: &Registry) -> Vec<u8> {
    serde_json::to_vec(registry).unwrap_or_else(|_| b"[]".to_vec())
}

/// The commands in a list a plug-in wrote.
///
/// # Errors
///
/// It isn't UTF-8 JSON holding a list of command specs.
pub fn decode_list(bytes: &[u8]) -> Result<Vec<CommandSpec>, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("the plug-in's command list is broken: {e}"))
}

/// Runs command `id` of `registry` with `args` through `run`, as the
/// extension's `run` does: the command must be one `registry` holds, and
/// `args` must match its argument schema (`null` reads as no arguments).
///
/// # Errors
///
/// Why the command didn't run.
pub fn run_checked(
    registry: &Registry,
    id: &str,
    args: &Value,
    run: impl FnOnce(&str, &Value) -> Result<Value, String>,
) -> Result<Value, String> {
    let spec = registry
        .get(id)
        .ok_or_else(|| format!("the plug-in has no command `{id}`"))?;
    let args = if args.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        args.clone()
    };
    if let Some(schema) = &spec.args {
        lspk_commands::validate(schema, &args)
            .map_err(|e| format!("`{id}`'s arguments don't fit its schema: {e}"))?;
    }
    run(id, &args)
}

#[cfg(any(feature = "clap", feature = "clap-host"))]
pub use raw::{PluginAgentCommands, clap_plugin_agent_commands};

#[cfg(any(feature = "clap", feature = "clap-host"))]
mod raw {
    use std::ffi::{CStr, c_char};

    use clack_common::extensions::{Extension, PluginExtensionSide, RawExtension};
    use clap_sys::plugin::clap_plugin;
    use clap_sys::stream::clap_ostream;

    /// The extension's C struct, as the module's declaration gives it.
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    #[allow(non_camel_case_types, reason = "the name in the C declaration")]
    pub struct clap_plugin_agent_commands {
        /// Writes the plug-in's commands to `out`.
        pub list:
            Option<unsafe extern "C" fn(plugin: *const clap_plugin, out: *const clap_ostream) -> bool>,
        /// Runs a command and writes its result, or why it failed, to `out`.
        pub run: Option<
            unsafe extern "C" fn(
                plugin: *const clap_plugin,
                id: *const c_char,
                args: *const c_char,
                out: *const clap_ostream,
            ) -> bool,
        >,
    }

    /// The agent-commands extension of one plug-in instance.
    #[derive(Clone, Copy)]
    pub struct PluginAgentCommands(
        // Read by the host side only (the `clap-host` feature).
        #[cfg_attr(not(feature = "clap-host"), allow(dead_code))]
        pub(crate) RawExtension<PluginExtensionSide, clap_plugin_agent_commands>,
    );

    impl std::fmt::Debug for PluginAgentCommands {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("PluginAgentCommands")
        }
    }

    // SAFETY: the identifier is this extension's, and the struct is the one
    // its plug-in side provides.
    unsafe impl Extension for PluginAgentCommands {
        const IDENTIFIERS: &[&CStr] = &[super::ID];
        type ExtensionSide = PluginExtensionSide;

        unsafe fn from_raw(raw: RawExtension<Self::ExtensionSide>) -> Self {
            // SAFETY: the plug-in returned this struct for this extension's ID.
            Self(unsafe { raw.cast() })
        }
    }
}

#[cfg(feature = "clap-host")]
mod host {
    use std::ffi::CString;

    use clack_host::plugin::PluginMainThreadHandle;
    use clack_host::stream::OutputStream;
    use lspk_commands::CommandSpec;
    use serde_json::Value;

    use super::PluginAgentCommands;

    impl PluginAgentCommands {
        /// The plug-in's commands, each with its schemas, description and
        /// permission class. Main thread only.
        ///
        /// # Errors
        ///
        /// The plug-in couldn't list them, or its list is broken.
        pub fn list(&self, plugin: &mut PluginMainThreadHandle<'_>) -> Result<Vec<CommandSpec>, String> {
            let list = plugin
                .use_extension(&self.0)
                .list
                .ok_or("the plug-in can't list its commands")?;
            let mut bytes = Vec::new();
            let mut out = OutputStream::from_writer(&mut bytes);
            // SAFETY: the function is the plug-in's, called on the main
            // thread with its own instance and a stream that lives through
            // the call.
            if !unsafe { list(plugin.as_raw(), out.as_raw_mut()) } {
                return Err("the plug-in couldn't list its commands".to_owned());
            }
            super::decode_list(&bytes)
        }

        /// Runs the plug-in's command `id` with `args` on the main thread,
        /// and returns its result. Parameter changes it makes reach the
        /// host as gestures, as a person's would.
        ///
        /// # Errors
        ///
        /// Why the command didn't run, in the plug-in's words.
        pub fn run(
            &self,
            plugin: &mut PluginMainThreadHandle<'_>,
            id: &str,
            args: &Value,
        ) -> Result<Value, String> {
            let run = plugin
                .use_extension(&self.0)
                .run
                .ok_or("the plug-in can't run commands")?;
            let id_c = CString::new(id).map_err(|_| format!("`{id}` isn't a command ID"))?;
            let args_c = CString::new(args.to_string())
                .map_err(|_| "the arguments hold a NUL character".to_owned())?;
            let mut bytes = Vec::new();
            let mut out = OutputStream::from_writer(&mut bytes);
            // SAFETY: as in `list`; both strings are NUL-terminated and
            // outlive the call.
            let ran = unsafe {
                run(
                    plugin.as_raw(),
                    id_c.as_ptr(),
                    args_c.as_ptr(),
                    out.as_raw_mut(),
                )
            };
            if ran {
                if bytes.is_empty() {
                    return Ok(Value::Null);
                }
                serde_json::from_slice(&bytes)
                    .map_err(|e| format!("the plug-in's result for `{id}` isn't JSON: {e}"))
            } else {
                let why = String::from_utf8_lossy(&bytes).trim().to_owned();
                Err(if why.is_empty() {
                    format!("the plug-in couldn't run `{id}`")
                } else {
                    why
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lspk_commands::{Context, NoArgs, Permission};
    use serde_json::json;

    fn registry() -> Registry {
        let mut registry = Registry::new();
        registry
            .register(
                CommandSpec::new("gain.reset", "Reset Gain")
                    .description("Puts the gain back to 0 dB.")
                    .permission(Permission::Edit)
                    .context(Context::Global)
                    .args::<NoArgs>()
                    .result::<()>(),
            )
            .expect("one command");
        registry
    }

    #[test]
    fn the_list_reads_back_as_the_registry() {
        let registry = registry();
        let listed = decode_list(&encode_list(&registry)).expect("a list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "gain.reset");
        assert_eq!(listed[0].permission, Some(Permission::Edit));
        assert!(listed[0].args.is_some() && listed[0].result.is_some());
        assert!(decode_list(b"{").is_err());
    }

    #[test]
    fn a_run_is_checked_against_the_registry_and_the_schema() {
        let registry = registry();
        let ok = run_checked(&registry, "gain.reset", &Value::Null, |id, args| {
            assert_eq!((id, args), ("gain.reset", &json!({})));
            Ok(json!(null))
        });
        assert_eq!(ok, Ok(Value::Null));
        let unknown = run_checked(&registry, "gain.nope", &json!({}), |_, _| Ok(json!(1)));
        assert!(unknown.unwrap_err().contains("no command"));
        let extra = run_checked(&registry, "gain.reset", &json!({"x": 1}), |_, _| Ok(json!(1)));
        assert!(extra.unwrap_err().contains("schema"));
    }
}
