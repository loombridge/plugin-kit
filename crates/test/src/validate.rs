//! The format validators CI runs on every built plug-in, each an outside
//! tool on the `PATH`:
//!
//! | Validator | Format | Tool |
//! |---|---|---|
//! | [`Validator::ClapValidator`] | CLAP | `clap-validator` (free-audio, MIT), at the revision CI pins |
//!
//! `cargo xtask validate` bundles the kit's example plug-ins and runs every
//! validator for each format it built; CI installs the tools. A plug-in's
//! own repository does the same through its xtask.
//!
//! **Adding a format's validators** (the VST3 and Audio Unit exports): add
//! a [`Format`] and a [`Validator`] for each tool (the Steinberg VST3
//! validator, `pluginval`, `auval`), with the arguments each takes in
//! [`Validator::command`] and how it reports failure in
//! [`Validator::run`]; list them in [`Validator::for_format`]; have
//! `cargo xtask validate` bundle that format; and install the tools in the
//! CI workflow beside `clap-validator`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A plug-in format a validator checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
    /// CLAP, the kit's native format.
    Clap,
}

/// A validator: an outside tool that loads built plug-ins and checks them
/// against their format's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Validator {
    /// `clap-validator`, which runs CLAP's conformance tests.
    ClapValidator,
}

impl Validator {
    /// Every validator for `format`.
    #[must_use]
    pub fn for_format(format: Format) -> &'static [Self] {
        match format {
            Format::Clap => &[Self::ClapValidator],
        }
    }

    /// The tool's name, as it's found on the `PATH`.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Self::ClapValidator => "clap-validator",
        }
    }

    /// The format it checks.
    #[must_use]
    pub fn format(self) -> Format {
        match self {
            Self::ClapValidator => Format::Clap,
        }
    }

    /// How to install it, for the message when it's missing.
    #[must_use]
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::ClapValidator => {
                "cargo install --locked --git https://github.com/free-audio/clap-validator \
                 --rev <the revision in .github/workflows/ci.yml> clap-validator"
            }
        }
    }

    /// The command that validates `bundles` (built plug-ins of its
    /// format).
    #[must_use]
    pub fn command(self, bundles: &[PathBuf]) -> Command {
        let mut command = Command::new(self.program());
        match self {
            Self::ClapValidator => {
                command.arg("validate").args(bundles);
            }
        }
        command
    }

    /// Validates `bundles` and returns the tool's summary line.
    ///
    /// # Errors
    ///
    /// The tool isn't installed, or it failed a bundle (with its output).
    pub fn run(self, bundles: &[PathBuf]) -> Result<String, String> {
        if bundles.is_empty() {
            return Err(format!("{}: no plug-ins to validate", self.program()));
        }
        let output = self.command(bundles).output().map_err(|e| {
            format!(
                "can't run {} ({e}); install it with `{}`",
                self.program(),
                self.install_hint()
            )
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let summary = summary_line(&stdout).unwrap_or_default();
        let failed = !output.status.success() || failures(&summary).unwrap_or(0) > 0;
        if failed {
            Err(format!(
                "{} failed ({}):\n{stdout}{stderr}",
                self.program(),
                output.status
            ))
        } else {
            Ok(summary)
        }
    }
}

/// The last line of `clap-validator`'s output that counts its tests, such
/// as `88 tests run, 68 passed, 0 failed, 0 warnings, 20 skipped`, without
/// its colours.
#[must_use]
pub fn summary_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(|line| strip_colours(line).trim().to_owned())
        .find(|line| line.contains("tests run"))
}

/// `text` without terminal escape sequences (`ESC [ … letter`).
fn strip_colours(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// How many tests a summary line says failed.
#[must_use]
pub fn failures(summary: &str) -> Option<u32> {
    summary
        .split(',')
        .map(str::trim)
        .find_map(|part| part.strip_suffix(" failed")?.trim().parse().ok())
}

/// The path of a CLAP file `cargo xtask bundle` wrote for `package` under
/// `target`.
#[must_use]
pub fn clap_bundle(target: &Path, package: &str) -> PathBuf {
    target.join("bundle").join(format!("{package}.clap"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_and_its_failures_are_read() {
        let output = "Running tests...\n\n88 tests run, \u{1b}[1;32m68\u{1b}[0m passed, \
                      \u{1b}[1;31m2\u{1b}[0m failed, 0 warnings, 20 skipped\n";
        let summary = summary_line(output).expect("a summary");
        assert_eq!(
            summary,
            "88 tests run, 68 passed, 2 failed, 0 warnings, 20 skipped"
        );
        assert_eq!(failures(&summary), Some(2));
        assert_eq!(
            failures("3 tests run, 1 passed, 2 failed, 0 skipped"),
            Some(2)
        );
        assert_eq!(summary_line("nothing"), None);
        assert_eq!(failures("no counts"), None);
    }

    #[test]
    fn each_format_has_its_validators() {
        let validators = Validator::for_format(Format::Clap);
        assert_eq!(validators, [Validator::ClapValidator]);
        assert!(validators.iter().all(|v| v.format() == Format::Clap));
        let command = Validator::ClapValidator.command(&[PathBuf::from("a.clap")]);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["validate", "a.clap"]);
        assert_eq!(
            clap_bundle(Path::new("target"), "x"),
            Path::new("target").join("bundle").join("x.clap")
        );
        assert!(Validator::ClapValidator.run(&[]).is_err());
    }
}
