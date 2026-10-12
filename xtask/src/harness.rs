//! The test harness's tasks: `cargo xtask golden` and `cargo xtask
//! validate`.

use std::path::{Path, PathBuf};
use std::process::Command;

use lspk_test::golden::UPDATE_ENV;
use lspk_test::validate::{Format, Validator, clap_bundle};

use crate::formats;

/// The plug-in crates `cargo xtask validate` checks when none are named.
pub const EXAMPLES: &[&str] = &["lspk-example-gain", "lspk-example-sine"];

/// Runs every crate's golden suite (its `golden` test target, by
/// convention `tests/golden.rs`). With `--update`, the suites first remake
/// the references whose renders changed (only the named ones, if any are
/// named) and record this platform as the one that made them. References
/// change only this way, never in an ordinary test run.
pub fn golden(root: &Path, options: &[String]) -> Result<(), String> {
    let mut update = false;
    let mut names = Vec::new();
    for option in options {
        match option.as_str() {
            "--update" => update = true,
            flag if flag.starts_with('-') => {
                return Err(format!(
                    "unexpected `{flag}`; usage: cargo xtask golden [--update] [NAME...]"
                ));
            }
            name => names.push(name.to_owned()),
        }
    }
    if !names.is_empty() && !update {
        return Err("name references only with --update".to_owned());
    }
    let mut command = Command::new(super::cargo_bin());
    command.current_dir(root).args([
        "test",
        "--workspace",
        "--all-features",
        "--locked",
        "--test",
        "golden",
        "--",
        // A suite's output says what changed and how each render nulled.
        "--nocapture",
    ]);
    if update {
        let selection = if names.is_empty() {
            "*".to_owned()
        } else {
            names.join(",")
        };
        command.env(UPDATE_ENV, selection);
    } else {
        command.env_remove(UPDATE_ENV);
    }
    let status = command
        .status()
        .map_err(|e| format!("couldn't run cargo test: {e}"))?;
    if status.success() {
        if update {
            eprintln!(
                "\nReferences updated. Commit the changed files under tests/golden/ and say \
                 why they changed in the pull request."
            );
        }
        Ok(())
    } else {
        Err("a golden render failed its null test".to_owned())
    }
}

/// Bundles each plug-in crate (the examples if none are named) and runs
/// every validator of each format it built: on macOS, each Audio Unit is
/// installed in the user's Audio Unit folder while its validators run, and
/// removed after.
pub fn validate(root: &Path, packages: &[String]) -> Result<(), String> {
    let packages: Vec<String> = if packages.is_empty() {
        EXAMPLES.iter().map(|p| (*p).to_owned()).collect()
    } else {
        packages.to_vec()
    };
    let bundled = formats::bundle(root, &packages)?;
    let target = formats::target_dir(root)?;

    // Steinberg's validator, built from the vendored SDK, goes first on the
    // PATH.
    let tools = formats::build_vst3_validator(root)?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(std::iter::once(tools).chain(std::env::split_paths(&path)))
        .map_err(|e| format!("couldn't extend PATH: {e}"))?;
    // SAFETY: xtask runs on one thread, and nothing reads the environment
    // while it changes.
    unsafe { std::env::set_var("PATH", path) };

    let mut installed = Vec::new();
    for plugin in &bundled {
        if let Some(component) = &plugin.auv2 {
            installed.push(formats::install_component(component)?);
        }
    }
    // Each format `cargo xtask bundle` builds, with its bundles.
    let mut built: Vec<(Format, Vec<PathBuf>)> = vec![
        (
            Format::Clap,
            packages.iter().map(|p| clap_bundle(&target, p)).collect(),
        ),
        (
            Format::Vst3,
            bundled.iter().map(|b| b.vst3.clone()).collect(),
        ),
    ];
    if !installed.is_empty() {
        built.push((Format::Auv2, installed.clone()));
    }
    let mut failed = Vec::new();
    for (format, bundles) in &built {
        for validator in Validator::for_format(*format) {
            let name = format!("{} ({format:?})", validator.program());
            eprintln!("\n==> {name}");
            match validator.run(bundles) {
                Ok(summary) => eprintln!("{summary}"),
                Err(message) => {
                    eprintln!("{message}");
                    failed.push(name);
                }
            }
        }
    }
    for component in &installed {
        std::fs::remove_dir_all(component)
            .map_err(|e| format!("couldn't remove {}: {e}", component.display()))?;
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed: {}", failed.join(", ")))
    }
}
