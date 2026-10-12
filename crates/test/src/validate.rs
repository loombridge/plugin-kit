//! The format validators CI runs on every built plug-in, each an outside
//! tool on the `PATH`:
//!
//! | Validator | Format | Tool |
//! |---|---|---|
//! | [`Validator::ClapValidator`] | CLAP | `clap-validator` (free-audio, MIT), at the revision CI pins |
//! | [`Validator::Vst3Validator`] | VST3 | `vst3-validator`: Steinberg's validator, which `cargo xtask validate` builds from the VST3 SDK the kit vendors (`tools/vst3-validator`) |
//! | [`Validator::Auval`] | Audio Unit v2 | `auval`, part of macOS |
//! | [`Validator::Pluginval`] | VST3 and Audio Unit v2 | `pluginval` (Tracktion, GPL-3.0, a tool only), at strictness 5, from the release CI pins; skipped where it isn't installed |
//!
//! `cargo xtask validate` bundles the kit's example plug-ins and runs every
//! validator for each format it built; CI installs the tools. A plug-in's
//! own repository does the same through its xtask.
//!
//! **Adding a format's validators**: add a [`Format`] and a [`Validator`]
//! for each tool, with the arguments each takes in [`Validator::command`]
//! and how it reports failure in [`Validator::run`]; list them in
//! [`Validator::for_format`]; have `cargo xtask validate` bundle that
//! format; and install the tools in the CI workflow beside
//! `clap-validator`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A plug-in format a validator checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
    /// CLAP, the kit's native format.
    Clap,
    /// VST3, made from the CLAP build by clap-wrapper.
    Vst3,
    /// Audio Unit v2 (macOS), made from the CLAP build by clap-wrapper.
    Auv2,
}

/// A validator: an outside tool that loads built plug-ins and checks them
/// against their format's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Validator {
    /// `clap-validator`, which runs CLAP's conformance tests.
    ClapValidator,
    /// Steinberg's VST3 validator, which runs the VST3 SDK's test suite.
    Vst3Validator,
    /// `auval`, Apple's Audio Unit validator. It finds an Audio Unit by its
    /// codes among those installed, so `cargo xtask validate` installs each
    /// `.component` while it runs.
    Auval,
    /// `pluginval` at the kit's strictness, on bundles of one format.
    Pluginval(Format),
}

/// The pluginval strictness the kit's plug-ins pass (of 1 to 10).
pub const PLUGINVAL_STRICTNESS: u32 = 5;

impl Validator {
    /// Every validator for `format`.
    #[must_use]
    pub fn for_format(format: Format) -> &'static [Self] {
        match format {
            Format::Clap => &[Self::ClapValidator],
            Format::Vst3 => &[Self::Vst3Validator, Self::Pluginval(Format::Vst3)],
            Format::Auv2 => &[Self::Auval, Self::Pluginval(Format::Auv2)],
        }
    }

    /// The tool's name, as it's found on the `PATH`.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Self::ClapValidator => "clap-validator",
            Self::Vst3Validator => "vst3-validator",
            Self::Auval => "auval",
            Self::Pluginval(_) => "pluginval",
        }
    }

    /// The format it checks.
    #[must_use]
    pub fn format(self) -> Format {
        match self {
            Self::ClapValidator => Format::Clap,
            Self::Vst3Validator => Format::Vst3,
            Self::Auval => Format::Auv2,
            Self::Pluginval(format) => format,
        }
    }

    /// Whether a run without the tool installed is skipped rather than
    /// failed: pluginval is installed by hand, and CI installs it on macOS
    /// only.
    #[must_use]
    pub fn optional(self) -> bool {
        matches!(self, Self::Pluginval(_))
    }

    /// How to install it, for the message when it's missing.
    #[must_use]
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::ClapValidator => {
                "cargo install --locked --git https://github.com/free-audio/clap-validator \
                 --rev <the revision in .github/workflows/ci.yml> clap-validator"
            }
            Self::Vst3Validator => {
                "cargo build --release --manifest-path tools/vst3-validator/Cargo.toml \
                 (cargo xtask validate builds it and puts it on the PATH)"
            }
            Self::Auval => "auval comes with macOS",
            Self::Pluginval(_) => {
                "download the release named in .github/workflows/ci.yml from \
                 https://github.com/Tracktion/pluginval/releases and put pluginval on the PATH"
            }
        }
    }

    /// Whether one run checks every bundle, rather than one run per bundle.
    fn takes_many(self) -> bool {
        matches!(self, Self::ClapValidator)
    }

    /// The command that validates `bundles` (built plug-ins of its format;
    /// for a validator that checks one at a time, the first).
    #[must_use]
    pub fn command(self, bundles: &[PathBuf]) -> Command {
        let mut command = Command::new(self.program());
        let first = bundles.first().map(PathBuf::as_path);
        match self {
            Self::ClapValidator => {
                command.arg("validate").args(bundles);
            }
            Self::Vst3Validator => {
                command.args(first);
            }
            Self::Auval => {
                let codes = first.and_then(au_codes).unwrap_or_default();
                command.arg("-v").args(codes);
            }
            Self::Pluginval(_) => {
                command
                    .args([
                        "--strictness-level",
                        &PLUGINVAL_STRICTNESS.to_string(),
                        "--validate",
                    ])
                    .args(first);
            }
        }
        command
    }

    /// Validates `bundles` and returns the tool's summary, a line per run.
    ///
    /// # Errors
    ///
    /// The tool isn't installed (unless it's [optional](Self::optional)),
    /// or it failed a bundle (with its output).
    pub fn run(self, bundles: &[PathBuf]) -> Result<String, String> {
        if bundles.is_empty() {
            return Err(format!("{}: no plug-ins to validate", self.program()));
        }
        let runs: Vec<&[PathBuf]> = if self.takes_many() {
            vec![bundles]
        } else {
            bundles.chunks(1).collect()
        };
        let mut summaries = Vec::new();
        for run in runs {
            let output = match self.command(run).output() {
                Ok(output) => output,
                Err(_) if self.optional() => {
                    return Ok(format!("skipped: {} isn't on the PATH", self.program()));
                }
                Err(e) => {
                    return Err(format!(
                        "can't run {} ({e}); install it with `{}`",
                        self.program(),
                        self.install_hint()
                    ));
                }
            };
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let summary = self.summary(&stdout).unwrap_or_default();
            let failed = !output.status.success()
                || match self {
                    Self::ClapValidator => failures(&summary).unwrap_or(0) > 0,
                    Self::Vst3Validator => vst3_failures(&summary).is_none_or(|n| n > 0),
                    Self::Auval => summary != AUVAL_SUCCESS,
                    Self::Pluginval(_) => summary != "SUCCESS",
                };
            if failed {
                return Err(format!(
                    "{} failed ({}):\n{stdout}{stderr}",
                    self.program(),
                    output.status
                ));
            }
            let name = run
                .iter()
                .filter_map(|b| b.file_name())
                .map(|n| n.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ");
            summaries.push(if self.takes_many() {
                summary
            } else {
                format!("{name}: {summary}")
            });
        }
        Ok(summaries.join("\n"))
    }

    /// The line of the tool's output that sums up its run.
    fn summary(self, output: &str) -> Option<String> {
        match self {
            Self::ClapValidator => summary_line(output),
            Self::Vst3Validator => last_line_with(output, "Result:"),
            Self::Auval => last_line_with(output, "AU VALIDATION"),
            Self::Pluginval(_) => output
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| *line == "SUCCESS" || line.starts_with("FAILURE"))
                .map(str::to_owned),
        }
    }
}

/// `auval`'s last line when every test passed.
const AUVAL_SUCCESS: &str = "AU VALIDATION SUCCEEDED.";

/// The last line of `clap-validator`'s output that counts its tests, such
/// as `88 tests run, 68 passed, 0 failed, 0 warnings, 20 skipped`, without
/// its colours.
#[must_use]
pub fn summary_line(output: &str) -> Option<String> {
    last_line_with(output, "tests run")
}

/// The last line of `output` containing `marker`, trimmed, without its
/// colours.
fn last_line_with(output: &str, marker: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(|line| strip_colours(line).trim().to_owned())
        .find(|line| line.contains(marker))
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

/// How many tests the VST3 validator's summary says failed, as in
/// `Result: 47 tests passed, 0 tests failed`.
#[must_use]
pub fn vst3_failures(summary: &str) -> Option<u32> {
    summary
        .split(',')
        .map(str::trim)
        .find_map(|part| part.strip_suffix(" tests failed")?.trim().parse().ok())
}

/// An Audio Unit bundle's type, subtype and manufacturer codes, as its
/// `Info.plist` lists them (the first Audio Unit in it).
#[must_use]
pub fn au_codes(component: &Path) -> Option<[String; 3]> {
    let plist = std::fs::read_to_string(component.join("Contents/Info.plist")).ok()?;
    let value = |key: &str| {
        let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
        let start = after.find("<string>")? + "<string>".len();
        let end = after[start..].find("</string>")? + start;
        Some(after[start..end].to_owned())
    };
    Some([value("type")?, value("subtype")?, value("manufacturer")?])
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
    fn each_tools_summary_is_read() {
        let vst3 = "[Succeeded]\n----\nResult: 47 tests passed, 0 tests failed\n----\n";
        let summary = Validator::Vst3Validator.summary(vst3).expect("a summary");
        assert_eq!(summary, "Result: 47 tests passed, 0 tests failed");
        assert_eq!(vst3_failures(&summary), Some(0));
        assert_eq!(
            vst3_failures("Result: 40 tests passed, 7 tests failed"),
            Some(7)
        );
        let auval = "* * PASS\n----\nAU VALIDATION SUCCEEDED.\n----\n";
        assert_eq!(
            Validator::Auval.summary(auval).as_deref(),
            Some(AUVAL_SUCCESS)
        );
        let pluginval = "Completed tests\nSUCCESS\n";
        assert_eq!(
            Validator::Pluginval(Format::Vst3)
                .summary(pluginval)
                .as_deref(),
            Some("SUCCESS")
        );
    }

    #[test]
    fn each_format_has_its_validators() {
        for format in [Format::Clap, Format::Vst3, Format::Auv2] {
            let validators = Validator::for_format(format);
            assert!(!validators.is_empty());
            assert!(validators.iter().all(|v| v.format() == format));
        }
        let command = Validator::ClapValidator.command(&[PathBuf::from("a.clap")]);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["validate", "a.clap"]);
        let command = Validator::Pluginval(Format::Vst3).command(&[PathBuf::from("a.vst3")]);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--strictness-level", "5", "--validate", "a.vst3"]);
        assert_eq!(
            clap_bundle(Path::new("target"), "x"),
            Path::new("target").join("bundle").join("x.clap")
        );
        assert!(Validator::ClapValidator.run(&[]).is_err());
        assert!(Validator::Pluginval(Format::Auv2).optional());
        assert!(!Validator::Vst3Validator.optional());
    }

    #[test]
    fn audio_unit_codes_are_read_from_the_bundle() {
        let dir = std::env::temp_dir().join(format!("lspk-au-codes-{}", std::process::id()));
        let component = dir.join("Test.component");
        let info = lspk_plugin::bundle::BundleInfo {
            id: "com.example.test".into(),
            name: "Test".into(),
            vendor: "Example".into(),
            version: "1.0.0".into(),
            features: vec!["audio-effect".into()],
        };
        let library = dir.join("lib");
        std::fs::create_dir_all(&dir).expect("a folder");
        std::fs::write(&library, b"").expect("a library");
        lspk_plugin::bundle::write_auv2(&library, &dir, "Test", &info).expect("a bundle");
        let codes = lspk_plugin::bundle::AuCodes::derive(&info);
        assert_eq!(
            au_codes(&component),
            Some(codes.as_strs().map(str::to_owned))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
