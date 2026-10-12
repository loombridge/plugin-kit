//! `cargo xtask bundle` and `cargo xtask validate`: a plug-in crate's
//! library laid out as each format's bundle, and the formats' own
//! validators run on them.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use lspk_plugin::bundle::{self, AuCodes, BundleInfo};

use crate::{cargo, cargo_metadata};

/// The pluginval strictness the kit's plug-ins pass.
const PLUGINVAL_STRICTNESS: &str = "5";

/// A plug-in crate's bundles.
pub struct Bundled {
    /// The crate.
    pub package: String,
    /// What its CLAP descriptor says.
    pub info: BundleInfo,
    /// The `.vst3` bundle.
    pub vst3: PathBuf,
    /// The `.component` bundle, on macOS.
    pub auv2: Option<PathBuf>,
}

/// Builds each plug-in crate in `packages` in release, and writes its
/// bundles to `target/bundle`: `<crate>.clap`, `<crate>.vst3` and, on
/// macOS, `<crate>.component`, each signed ad hoc on macOS.
pub fn bundle(root: &Path, packages: &[String]) -> Result<Vec<Bundled>, String> {
    if packages.is_empty() {
        return Err(format!(
            "name the plug-in crates to bundle\n\n{}",
            crate::USAGE
        ));
    }
    let mut args = vec!["build", "--release", "--locked"];
    for package in packages {
        args.extend(["--package", package]);
    }
    cargo(root, &args)?;
    let target = target_dir(root)?;
    let dir = target.join("bundle");
    let mut bundled = Vec::new();
    for package in packages {
        let library = target.join("release").join(format!(
            "{}{}{}",
            std::env::consts::DLL_PREFIX,
            package.replace('-', "_"),
            std::env::consts::DLL_SUFFIX
        ));
        if !library.exists() {
            return Err(format!(
                "{package} has no library at {}: is it a cdylib?",
                library.display()
            ));
        }
        let info = plugin_info(&library)?;
        let written =
            |what: &str, e: std::io::Error| format!("couldn't write {package}'s {what}: {e}");
        let clap = bundle::write_clap(&library, &dir, package, &info.id)
            .map_err(|e| written("CLAP file", e))?;
        let vst3 = bundle::write_vst3(&library, &dir, package, &info)
            .map_err(|e| written("VST3 bundle", e))?;
        let auv2 = if cfg!(target_os = "macos") {
            Some(
                bundle::write_auv2(&library, &dir, package, &info)
                    .map_err(|e| written("Audio Unit bundle", e))?,
            )
        } else {
            None
        };
        for path in [Some(&clap), Some(&vst3), auv2.as_ref()]
            .into_iter()
            .flatten()
        {
            if cfg!(target_os = "macos") {
                sign_ad_hoc(path)?;
            }
            eprintln!("{}", path.display());
        }
        if auv2.is_some() {
            let codes = AuCodes::derive(&info);
            let [kind, subtype, manufacturer] = codes.as_strs();
            eprintln!("  Audio Unit: {kind} {subtype} {manufacturer}");
        }
        bundled.push(Bundled {
            package: package.clone(),
            info,
            vst3,
            auv2,
        });
    }
    Ok(bundled)
}

/// Bundles each plug-in crate in `packages`, then runs every validator
/// there is for each format on this platform, and fails if any fails.
pub fn validate(root: &Path, packages: &[String]) -> Result<(), String> {
    let bundled = bundle(root, packages)?;
    let validator = build_vst3_validator(root)?;
    let pluginval = std::env::var_os("PLUGINVAL").map(PathBuf::from);
    let mut results: Vec<(String, Result<(), String>)> = Vec::new();

    for plugin in &bundled {
        let name = &plugin.package;
        results.push((
            format!("{name}: VST3 validator"),
            run(Command::new(&validator).arg(&plugin.vst3)),
        ));
        if let Some(pluginval) = &pluginval {
            results.push((
                format!("{name}: pluginval (VST3)"),
                run(&mut pluginval_command(pluginval, &plugin.vst3)),
            ));
        }
        if let Some(component) = &plugin.auv2 {
            let installed = install_component(component)?;
            let codes = AuCodes::derive(&plugin.info);
            let [kind, subtype, manufacturer] = codes.as_strs();
            results.push((
                format!("{name}: auval"),
                run(Command::new("auval").args(["-v", kind, subtype, manufacturer])),
            ));
            if let Some(pluginval) = &pluginval {
                results.push((
                    format!("{name}: pluginval (Audio Unit)"),
                    run(&mut pluginval_command(pluginval, &installed)),
                ));
            }
            std::fs::remove_dir_all(&installed)
                .map_err(|e| format!("couldn't remove {}: {e}", installed.display()))?;
        }
    }

    eprintln!("\n==> validators");
    if pluginval.is_none() {
        eprintln!("skipped: pluginval (set PLUGINVAL to its binary to run it)");
    }
    let mut failed = 0;
    for (name, result) in &results {
        let status = if result.is_ok() { "ok" } else { "FAILED" };
        eprintln!("{status:>6}  {name}");
        failed += usize::from(result.is_err());
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(format!(
            "{failed} of {} validator runs failed",
            results.len()
        ))
    }
}

/// pluginval validating `path` at the kit's strictness.
fn pluginval_command(pluginval: &Path, path: &Path) -> Command {
    let mut command = Command::new(pluginval);
    command
        .args(["--strictness-level", PLUGINVAL_STRICTNESS, "--validate"])
        .arg(path);
    command
}

/// Runs `command`, streaming its output, and fails unless it succeeds.
fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| format!("couldn't run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed ({status})"))
    }
}

/// The hidden task that prints a library's plug-in info, one field a line:
/// ID, name, vendor, version, then each feature.
pub const PLUGIN_INFO: &str = "__plugin-info";

/// What the CLAP descriptor of the one plug-in in `library` says, read in a
/// process of its own: a library can't be unloaded once loaded, and two
/// plug-ins from one build tree in one process share the Audio Unit view's
/// Objective-C classes (see `lspk-clap-wrapper`), which the runtime warns of.
fn plugin_info(library: &Path) -> Result<BundleInfo, String> {
    let exe = std::env::current_exe().map_err(|e| format!("couldn't find xtask: {e}"))?;
    let output = Command::new(exe)
        .arg(PLUGIN_INFO)
        .arg(library)
        .output()
        .map_err(|e| format!("couldn't run xtask: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines().map(str::to_owned);
    let mut next = || lines.next().unwrap_or_default();
    let info = BundleInfo {
        id: next(),
        name: next(),
        vendor: next(),
        version: next(),
        features: lines.collect(),
    };
    if info.id.is_empty() {
        return Err(format!("{}'s plug-in has no ID", library.display()));
    }
    Ok(info)
}

/// Prints what the CLAP descriptor of the one plug-in in `library` says,
/// for [`plugin_info`].
pub fn print_plugin_info(library: &Path) -> Result<(), String> {
    // SAFETY: loading the library runs its code in this process; it's a
    // plug-in just built from this workspace, and clack checks the entry
    // point and its CLAP version.
    let entry = unsafe { clack_host::entry::PluginEntry::load(library) }
        .map_err(|e| format!("couldn't load {}: {e}", library.display()))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| format!("{} has no CLAP plug-in factory", library.display()))?;
    let descriptor = factory
        .plugin_descriptors()
        .next()
        .ok_or_else(|| format!("{} holds no plug-in", library.display()))?;
    let text = |s: Option<&CStr>| {
        s.map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let one_line = |s: Option<&CStr>| text(s).replace(['\n', '\r'], " ");
    println!("{}", one_line(descriptor.id()));
    println!("{}", one_line(descriptor.name()));
    println!("{}", one_line(descriptor.vendor()));
    println!("{}", one_line(descriptor.version()));
    for feature in descriptor.features() {
        println!("{}", one_line(Some(feature)));
    }
    Ok(())
}

/// Signs a macOS bundle ad hoc, as hosts and `auval` need on Apple silicon.
fn sign_ad_hoc(path: &Path) -> Result<(), String> {
    let output = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .output()
        .map_err(|e| format!("couldn't run codesign: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "codesign failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Copies `component` into the user's Audio Unit folder, where `auval` and
/// hosts look, and has the system rescan; returns where it went.
fn install_component(component: &Path) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME isn't set")?;
    let folder = PathBuf::from(home).join("Library/Audio/Plug-Ins/Components");
    let name = component.file_name().ok_or("a component has a name")?;
    let installed = folder.join(name);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    if installed.exists() {
        std::fs::remove_dir_all(&installed).map_err(|e| format!("{}: {e}", installed.display()))?;
    }
    copy_dir(component, &installed).map_err(|e| format!("{}: {e}", installed.display()))?;
    // The registrar caches what it found; it restarts when next asked.
    let _ = Command::new("killall")
        .args(["-9", "AudioComponentRegistrar"])
        .output();
    Ok(installed)
}

/// Copies the folder `from` to `to`, recursively.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Builds Steinberg's VST3 validator from the vendored SDK
/// (`tools/vst3-validator`), into the workspace's target folder, and
/// returns its path.
fn build_vst3_validator(root: &Path) -> Result<PathBuf, String> {
    let target = target_dir(root)?.join("tools");
    let manifest = root.join("tools/vst3-validator/Cargo.toml");
    let manifest = manifest.to_string_lossy();
    let target_arg = target.to_string_lossy();
    cargo(
        root,
        &[
            "build",
            "--release",
            "--locked",
            "--manifest-path",
            &manifest,
            "--target-dir",
            &target_arg,
        ],
    )?;
    Ok(target
        .join("release")
        .join(format!("validator{}", std::env::consts::EXE_SUFFIX)))
}

/// The workspace's target folder.
fn target_dir(root: &Path) -> Result<PathBuf, String> {
    cargo_metadata(root)?["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "cargo metadata gave no target directory".to_owned())
}
