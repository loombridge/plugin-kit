//! A shortcut reference's shortcuts, as far as the registry needs them:
//! to check that every binding names a real shortcut in a fitting context,
//! and to show each binding's keys.
//!
//! The registry doesn't read the shortcut reference itself. The caller
//! parses it (Beatlayer's `cargo xtask` does) and fills a [`ShortcutTable`],
//! so this crate stays free of file formats and paths.

use std::collections::BTreeMap;

use crate::context::Context;
use crate::registry::{Problem, Registry};

/// One shortcut from the shortcut reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// Its ID, such as `S-EDIT-006`.
    pub id: String,
    /// What it does, in the reference's words.
    pub action: String,
    /// The context it applies in, such as `global`.
    pub context: String,
    /// Its macOS bindings, such as `Cmd+Z`.
    pub mac: Vec<String>,
    /// Its Windows bindings, such as `Ctrl+Z`.
    pub win: Vec<String>,
    /// The milestone that binds it, such as `M1`.
    pub milestone: String,
}

/// The shortcuts and contexts of the shortcut reference.
#[derive(Clone, Debug, Default)]
pub struct ShortcutTable {
    contexts: Vec<String>,
    shortcuts: BTreeMap<String, Shortcut>,
}

impl ShortcutTable {
    /// A table of these contexts and shortcuts.
    pub fn new(
        contexts: impl IntoIterator<Item = String>,
        shortcuts: impl IntoIterator<Item = Shortcut>,
    ) -> Self {
        Self {
            contexts: contexts.into_iter().collect(),
            shortcuts: shortcuts
                .into_iter()
                .map(|shortcut| (shortcut.id.clone(), shortcut))
                .collect(),
        }
    }

    /// The shortcut with this ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Shortcut> {
        self.shortcuts.get(id)
    }

    /// The reference's context IDs, in its order.
    #[must_use]
    pub fn contexts(&self) -> &[String] {
        &self.contexts
    }
}

impl Registry {
    /// Checks the bindings against the shortcut reference's shortcuts:
    ///
    /// - every binding names a shortcut the reference has;
    /// - each shortcut applies globally or in one of the command's
    ///   contexts;
    /// - no two commands bind the same shortcut in the same context, so a
    ///   key press always picks one command (a global shortcut bound by a
    ///   command with narrower contexts applies in those);
    /// - the registry's [`Context`]s are exactly the reference's.
    #[must_use]
    pub fn binding_problems(&self, table: &ShortcutTable) -> Vec<Problem> {
        let mut problems = Vec::new();

        let ours: Vec<String> = Context::ALL.iter().map(|c| c.id().to_owned()).collect();
        let missing: Vec<String> = table
            .contexts
            .iter()
            .filter(|id| !ours.contains(id))
            .cloned()
            .collect();
        let extra: Vec<String> = ours
            .iter()
            .filter(|id| !table.contexts.contains(id))
            .cloned()
            .collect();
        if !missing.is_empty() || !extra.is_empty() {
            problems.push(Problem::ContextDrift { missing, extra });
        }

        // For each shortcut, the commands that bind it and the contexts in
        // which each one does.
        let mut users: BTreeMap<&str, Vec<(&str, Vec<Context>)>> = BTreeMap::new();
        for spec in self {
            for binding in &spec.bindings {
                let Some(shortcut) = table.get(&binding.shortcut) else {
                    problems.push(Problem::UnknownShortcut {
                        command: spec.id.to_string(),
                        shortcut: binding.shortcut.to_string(),
                    });
                    continue;
                };
                let effective = if shortcut.context == Context::Global.id() {
                    spec.contexts.clone()
                } else {
                    match Context::from_id(&shortcut.context) {
                        Some(context) if spec.contexts.contains(&context) => vec![context],
                        _ => {
                            problems.push(Problem::ShortcutContext {
                                command: spec.id.to_string(),
                                shortcut: shortcut.id.clone(),
                                context: shortcut.context.clone(),
                            });
                            continue;
                        }
                    }
                };
                let entry = users.entry(shortcut.id.as_str()).or_default();
                if !entry.iter().any(|(command, _)| *command == spec.id) {
                    entry.push((&spec.id, effective));
                }
            }
        }
        for (shortcut, commands) in users {
            for (i, (first, first_contexts)) in commands.iter().enumerate() {
                for (second, second_contexts) in &commands[i + 1..] {
                    if let Some(context) = first_contexts
                        .iter()
                        .find(|context| second_contexts.contains(context))
                    {
                        problems.push(Problem::SharedShortcut {
                            shortcut: shortcut.to_owned(),
                            first: (*first).to_owned(),
                            second: (*second).to_owned(),
                            context: context.id().to_owned(),
                        });
                    }
                }
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandSpec, NoArgs, Permission};

    fn shortcut(id: &str, context: &str) -> Shortcut {
        Shortcut {
            id: id.to_owned(),
            action: format!("Action {id}"),
            context: context.to_owned(),
            mac: vec!["Cmd+Z".to_owned()],
            win: vec!["Ctrl+Z".to_owned()],
            milestone: "M1".to_owned(),
        }
    }

    fn table() -> ShortcutTable {
        ShortcutTable::new(
            Context::ALL.iter().map(|c| c.id().to_owned()),
            [
                shortcut("S-EDIT-005", "global"),
                shortcut("S-TRACK-018", "track-headers"),
            ],
        )
    }

    fn command(id: &'static str, contexts: &[Context], bindings: &[&'static str]) -> CommandSpec {
        let spec = CommandSpec::new(id, "Delete")
            .description("Deletes it.")
            .permission(Permission::Edit)
            .contexts(contexts.iter().copied())
            .args::<NoArgs>()
            .result::<()>();
        bindings.iter().fold(spec, |spec, b| spec.binding(*b))
    }

    fn problems(commands: impl IntoIterator<Item = CommandSpec>) -> Vec<String> {
        let mut registry = Registry::new();
        for spec in commands {
            registry.register(spec).unwrap();
        }
        registry
            .binding_problems(&table())
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_global_shortcut_can_run_different_commands_in_different_contexts() {
        assert!(
            problems([
                command("clip.delete", &[Context::Arrangement], &["S-EDIT-005"]),
                command("device.remove", &[Context::DeviceView], &["S-EDIT-005"]),
                command("track.delete", &[Context::TrackHeaders], &["S-TRACK-018"]),
            ])
            .is_empty()
        );
    }

    #[test]
    fn bad_bindings_are_reported() {
        assert_eq!(
            problems([
                command(
                    "clip.delete",
                    &[Context::Arrangement],
                    &["S-EDIT-005", "S-NOPE-001"]
                ),
                command(
                    "clip.remove",
                    &[Context::Arrangement, Context::Session],
                    &["S-EDIT-005"]
                ),
                command("track.delete", &[Context::Arrangement], &["S-TRACK-018"]),
            ]),
            [
                "clip.delete: binds S-NOPE-001, which isn't in the shortcut reference",
                "track.delete: binds S-TRACK-018, whose context `track-headers` isn't global or \
                 one of the command's contexts",
                "S-EDIT-005 runs both clip.delete and clip.remove in `arrangement`",
            ]
        );
    }

    #[test]
    fn contexts_must_match_the_reference() {
        let table = ShortcutTable::new(
            ["global".to_owned(), "mixer".to_owned()],
            [shortcut("S-EDIT-005", "global")],
        );
        let problems = Registry::new().binding_problems(&table);
        assert_eq!(problems.len(), 1);
        let Problem::ContextDrift { missing, extra } = &problems[0] else {
            panic!("{problems:?}");
        };
        assert_eq!(missing, &["mixer"]);
        assert_eq!(extra.len(), Context::ALL.len() - 1);
    }
}
