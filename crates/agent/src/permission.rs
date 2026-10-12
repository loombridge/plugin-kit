//! Who may run what: a command's permission class against where the agent
//! interface runs and what the person allowed.
//!
//! Read and edit commands run straight away. Destructive and external ones
//! ask the person first, unless the person turned on autonomous mode for
//! that agent. Headless, there's no person to ask, so they're refused
//! unless the headless server was started with `--autonomous`. Commands that
//! change what agents may do (the agent server's own) are refused whatever
//! the agent's mode.
//!
//! A program's tools decide each call with [`decide`], giving the classes
//! of what it would run, and turn the decision into the call's outcome with
//! [`permit`]: a call that asks first ends as [`ToolOutcome::Ask`], the
//! server asks the person, and if they allow it, the server calls the tool
//! again with [`Call::confirmed`](crate::Call::confirmed) set.

use lspk_commands::Permission;

use crate::ToolOutcome;

/// Where the agent interface runs, which decides who answers when a command
/// asks first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A headless server, such as a command-line tool's `mcp` command: no
    /// window and no person to ask.
    Headless {
        /// Whether `--autonomous` was given, so destructive and external
        /// commands run without asking.
        autonomous: bool,
    },
    /// The running program, with a person to answer: they allow each agent
    /// once, and autonomous mode is set per agent in its settings.
    App,
}

/// What to do with a command an agent asked to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Run it.
    Run,
    /// Ask the person first.
    Ask,
    /// Refuse it, saying why.
    Refuse(String),
}

/// A program's own rules and words for [`decide`].
pub struct Rules<'a> {
    /// Whether a command is one only a person may run, because it changes
    /// what agents may do (such as allowing an agent or switching on its
    /// autonomous mode).
    pub person_only: &'a dyn Fn(&str) -> bool,
    /// Where the person changes what agents may do, as a refusal names it,
    /// such as "the Settings window (Agents)".
    pub settings: &'a str,
    /// The headless server's command, as a refusal names it, such as
    /// `my-synth-cli mcp`.
    pub headless: &'a str,
}

impl std::fmt::Debug for Rules<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rules")
            .field("settings", &self.settings)
            .field("headless", &self.headless)
            .finish_non_exhaustive()
    }
}

/// Decides whether an agent may run commands whose most far-reaching class
/// is `class`. `autonomous` says whether the agent whose rights the session
/// has is autonomous (asked only when it matters); `commands` are their
/// IDs, for the messages.
pub fn decide(
    mode: Mode,
    rules: &Rules<'_>,
    autonomous: impl FnOnce() -> bool,
    class: Permission,
    commands: &[&str],
) -> Decision {
    if let Some(id) = commands.iter().find(|id| (rules.person_only)(id)) {
        return Decision::Refuse(format!(
            "`{id}` changes what agents may do, so only a person can run it, in {}.",
            rules.settings
        ));
    }
    if !class.asks_first() {
        return Decision::Run;
    }
    match mode {
        Mode::Headless { autonomous: true } => Decision::Run,
        Mode::Headless { autonomous: false } => {
            let named = commands
                .iter()
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ");
            Decision::Refuse(format!(
                "{named} {} {class}, and `{}` has no person to ask. Start it with \
                 `--autonomous` to let agents run destructive and external commands without \
                 asking.",
                if commands.len() == 1 { "is" } else { "include" },
                rules.headless
            ))
        }
        Mode::App if autonomous() => Decision::Run,
        Mode::App => Decision::Ask,
    }
}

/// Turns a decision into how a tool call goes on: `Ok` to go ahead, or how
/// the call ends instead. `classes` are those of what it would run; the
/// most far-reaching of them (external if there are none) is decided by
/// `decide`. A call the person already allowed (`confirmed`) goes ahead
/// where it would ask; one that asks ends as [`ToolOutcome::Ask`], with
/// `summary` for the person to read.
///
/// # Errors
///
/// The call's outcome when it doesn't go ahead: the question for the
/// person, or the refusal.
pub fn permit<D>(
    classes: &[Permission],
    confirmed: bool,
    decide: impl FnOnce(Permission) -> Decision,
    summary: impl FnOnce() -> String,
) -> Result<(), ToolOutcome<D>> {
    let class = classes
        .iter()
        .copied()
        .max()
        .unwrap_or(Permission::External);
    match decide(class) {
        Decision::Run => Ok(()),
        Decision::Ask if confirmed => Ok(()),
        Decision::Ask => Err(ToolOutcome::Ask {
            class,
            classes: classes
                .iter()
                .copied()
                .filter(|class| class.asks_first())
                .collect(),
            summary: summary(),
        }),
        Decision::Refuse(message) => Err(ToolOutcome::Done(Err(message))),
    }
}

/// Classes as the person reads them, such as "destructive and external".
#[must_use]
pub fn class_names(classes: &[Permission]) -> String {
    let names: Vec<&str> = classes.iter().map(|class| class.id()).collect();
    and_list(&names)
}

/// Items as a sentence lists them: "a", "a and b", "a, b and c".
#[must_use]
pub fn and_list<T: AsRef<str>>(items: &[T]) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_owned(),
        [rest @ .., last] => {
            let rest: Vec<&str> = rest.iter().map(AsRef::as_ref).collect();
            format!("{} and {}", rest.join(", "), last.as_ref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: Rules<'static> = Rules {
        person_only: &|id| id.starts_with("agent."),
        settings: "the Settings window (Agents)",
        headless: "demo mcp",
    };

    #[test]
    fn read_and_edit_run_and_the_rest_ask_or_refuse() {
        let headless = Mode::Headless { autonomous: false };
        let free = Mode::Headless { autonomous: true };
        for class in [Permission::Read, Permission::Edit] {
            assert_eq!(
                decide(headless, &RULES, || false, class, &["x.y"]),
                Decision::Run
            );
            assert_eq!(
                decide(Mode::App, &RULES, || false, class, &["x.y"]),
                Decision::Run
            );
        }
        for class in [Permission::Destructive, Permission::External] {
            let Decision::Refuse(message) =
                decide(headless, &RULES, || false, class, &["track.delete"])
            else {
                panic!("headless refuses {class}");
            };
            assert!(message.contains("--autonomous"), "{message}");
            assert!(message.contains("`track.delete` is"), "{message}");
            assert!(message.contains("`demo mcp` has no person"), "{message}");
            assert_eq!(
                decide(free, &RULES, || false, class, &["x.y"]),
                Decision::Run
            );
            assert_eq!(
                decide(Mode::App, &RULES, || false, class, &["x.y"]),
                Decision::Ask
            );
            assert_eq!(
                decide(Mode::App, &RULES, || true, class, &["x.y"]),
                Decision::Run
            );
        }
        let Decision::Refuse(message) = decide(
            headless,
            &RULES,
            || false,
            Permission::Destructive,
            &["a.b", "c.d"],
        ) else {
            panic!("headless refuses destructive commands");
        };
        assert!(
            message.starts_with("`a.b`, `c.d` include destructive"),
            "{message}"
        );
    }

    #[test]
    fn agents_never_change_their_own_permissions() {
        for mode in [Mode::App, Mode::Headless { autonomous: true }] {
            let decision = decide(
                mode,
                &RULES,
                || true,
                Permission::External,
                &["agent.set_autonomous"],
            );
            assert!(matches!(
                decision,
                Decision::Refuse(m) if m.contains("only a person can run it, in the Settings")
            ));
        }
    }

    #[test]
    fn a_decision_becomes_the_calls_outcome() {
        let ask = |_| Decision::Ask;
        let summary = || "Delete track".to_owned();
        let classes = [Permission::Edit, Permission::Destructive];
        let Err(ToolOutcome::<()>::Ask {
            class,
            classes: asking,
            summary: text,
        }) = permit(&classes, false, ask, summary)
        else {
            panic!("an unconfirmed call asks");
        };
        assert_eq!(class, Permission::Destructive);
        assert_eq!(asking, [Permission::Destructive]);
        assert_eq!(text, "Delete track");
        assert!(permit::<()>(&classes, true, ask, summary).is_ok());
        assert!(matches!(
            permit::<()>(&[], false, |class| Decision::Refuse(class.to_string()), String::new),
            Err(ToolOutcome::Done(Err(message))) if message == "external"
        ));
    }

    #[test]
    fn lists_read_as_sentences() {
        assert_eq!(and_list::<&str>(&[]), "");
        assert_eq!(and_list(&["a"]), "a");
        assert_eq!(and_list(&["a", "b", "c"]), "a, b and c");
        assert_eq!(
            class_names(&[Permission::Destructive, Permission::External]),
            "destructive and external"
        );
    }
}
