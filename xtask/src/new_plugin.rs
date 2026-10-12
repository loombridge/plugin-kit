//! `cargo xtask new-plugin` and `cargo xtask template-check`: a new plug-in
//! repository generated from the templates in `xtask/templates`, and the
//! check that the templates still make plug-ins that pass every check.
//!
//! # What a generated repository holds
//!
//! A workspace with one plug-in crate, `crates/<name>`, an instrument or an
//! effect with a few parameters in a parameter space, exported as CLAP, VST3,
//! Audio Unit v2 and a standalone app with the generic editor and the agent
//! interface; its tests (the formats, the agent interface, golden renders,
//! measurements, the golden parameter ID list), a benchmark suite with its
//! instruction-count baseline, and a small `xtask` crate that runs this
//! crate's tasks ([`crate::Repo::plugin`]). Every kit crate comes from the
//! kit's repository at one git revision: by default this checkout's commit,
//! which must be on `origin/main` so anyone can fetch it.
//!
//! # The templates
//!
//! `xtask/templates/common` holds the files every plug-in gets, and
//! `xtask/templates/instrument` and `xtask/templates/effect` those of each
//! kind. Paths and text are rendered the same way:
//!
//! - `{{crate}}` (`bright-delay`), `{{crate_lib}}` (`bright_delay`),
//!   `{{Type}}` (`BrightDelay`), `{{Name}}` (`Bright Delay`), `{{id}}`,
//!   `{{licence}}` (its SPDX expression), `{{kind}}` (`instrument` or
//!   `effect`), `{{rev}}` (the kit's revision), `{{rust_version}}` (the
//!   kit's), `{{nextest}}` ([`crate::NEXTEST_VERSION`]),
//!   `{{kit_dependencies}}` and `{{kit_patch}}` (one line per crate in
//!   [`KIT_CRATES`]) are replaced;
//! - a line holding only `{{#gpl}}`, `{{#permissive}}`, `{{#instrument}}` or
//!   `{{#effect}}` starts a block that's kept only for that licence or kind,
//!   and the matching `{{/...}}` line ends it;
//! - a path component starting with `_` starts with `.` instead (so the
//!   templates' own `.cargo`, `.github` and `.gitignore` don't act on the
//!   kit), and a trailing `.tmpl` is dropped (so tools don't take a
//!   template's `Cargo.toml` or `AGENTS.md` for the kit's own);
//! - FLAC files are copied as they are.
//!
//! Then `cargo fmt` formats the plug-in's crates, since names of other
//! lengths move line breaks and the order of imports.
//!
//! The golden references, parameter ID lists and benchmark baselines in the
//! templates are those of the plug-ins they generate, which don't depend on
//! a plug-in's name. `cargo xtask template-check --update` remakes the first
//! two; the baselines come from the template CI job's counts (see the kit's
//! AGENTS.md).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::{cargo, cargo_bin};

/// The kit's repository, which generated plug-ins depend on.
pub const KIT_REPOSITORY: &str = "https://github.com/loombridge/plugin-kit";

/// The kit crates a generated plug-in's workspace names, with each one's
/// folder in the kit: all from [`KIT_REPOSITORY`] at one revision, and all
/// patched together when working on the kit and the plug-in at once.
pub const KIT_CRATES: &[(&str, &str)] = &[
    ("lspk-agent", "crates/agent"),
    ("lspk-commands", "crates/commands"),
    ("lspk-params", "crates/params"),
    ("lspk-plugin", "crates/plugin"),
    ("lspk-plugin-agent", "crates/plugin-agent"),
    ("lspk-rt", "crates/rt"),
    ("lspk-standalone", "crates/standalone"),
    ("lspk-test", "crates/test"),
    ("lspk-ui", "crates/ui"),
    ("lspk-xtask", "xtask"),
];

const USAGE: &str = "usage: cargo xtask new-plugin <name> --kind instrument|effect [--dir PATH] \
                     [--id REVERSE-DNS] [--licence mit-or-apache-2.0|gpl-3.0-or-later] [--rev COMMIT]";

/// What a plug-in is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// It plays notes.
    Instrument,
    /// It processes audio.
    Effect,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Instrument => "instrument",
            Self::Effect => "effect",
        }
    }
}

/// A generated repository's licence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Licence {
    /// MIT OR Apache-2.0, as the kit is.
    Permissive,
    /// GPL-3.0-or-later, so its own crates may take code from GPL projects.
    Gpl,
}

impl Licence {
    fn spdx(self) -> &'static str {
        match self {
            Self::Permissive => "MIT OR Apache-2.0",
            Self::Gpl => "GPL-3.0-or-later",
        }
    }
}

/// What `cargo xtask new-plugin` was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The plug-in crate's name, in kebab case.
    pub name: String,
    /// Instrument or effect.
    pub kind: Kind,
    /// Where the repository goes; beside the kit's checkout by default.
    pub dir: Option<PathBuf>,
    /// The plug-in's ID; `com.loombridge.<name>` by default.
    pub id: Option<String>,
    /// Its licence.
    pub licence: Licence,
    /// The kit revision it depends on; this checkout's commit by default.
    pub rev: Option<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut name = None;
    let mut kind = None;
    let mut dir = None;
    let mut id = None;
    let mut licence = Licence::Permissive;
    let mut rev = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("`{arg}` needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--kind" => {
                kind = Some(match value()?.as_str() {
                    "instrument" => Kind::Instrument,
                    "effect" => Kind::Effect,
                    other => return Err(format!("no kind `{other}`\n{USAGE}")),
                });
            }
            "--dir" => dir = Some(PathBuf::from(value()?)),
            "--id" => id = Some(value()?),
            "--licence" | "--license" => {
                licence = match value()?.as_str() {
                    "mit-or-apache-2.0" => Licence::Permissive,
                    "gpl-3.0-or-later" => Licence::Gpl,
                    other => return Err(format!("no licence `{other}`\n{USAGE}")),
                };
            }
            "--rev" => rev = Some(value()?),
            flag if flag.starts_with('-') => return Err(format!("unexpected `{flag}`\n{USAGE}")),
            other if name.is_none() => name = Some(other.to_owned()),
            other => return Err(format!("unexpected `{other}`\n{USAGE}")),
        }
    }
    Ok(Options {
        name: name.ok_or_else(|| format!("name the plug-in\n{USAGE}"))?,
        kind: kind.ok_or_else(|| format!("say what kind of plug-in it is\n{USAGE}"))?,
        dir,
        id,
        licence,
        rev,
    })
}

/// The names a plug-in goes by, from its crate name.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Names {
    /// `bright-delay`.
    krate: String,
    /// `bright_delay`.
    lib: String,
    /// `BrightDelay`.
    ty: String,
    /// `Bright Delay`.
    display: String,
}

impl Names {
    fn new(name: &str) -> Result<Self, String> {
        let words: Vec<&str> = name.split('-').collect();
        let valid = words.iter().all(|w| {
            !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        }) && name.starts_with(|c: char| c.is_ascii_lowercase());
        if !valid {
            return Err(format!(
                "`{name}` isn't a crate name in kebab case: lower-case letters and digits, \
                 words joined by single hyphens, starting with a letter (such as \
                 `bright-delay`)"
            ));
        }
        if name.starts_with("lspk") || name == "xtask" || name == "reagent" {
            return Err(format!(
                "`{name}` is taken by the kit or another Loombridge crate"
            ));
        }
        let capitalised: Vec<String> = words
            .iter()
            .map(|w| {
                let mut chars = w.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_ascii_uppercase().to_string() + chars.as_str()
                })
            })
            .collect();
        Ok(Self {
            krate: name.to_owned(),
            lib: name.replace('-', "_"),
            ty: capitalised.concat(),
            display: capitalised.join(" "),
        })
    }
}

/// Whether `id` is a reverse-DNS plug-in ID: two or more dot-separated
/// parts of lower-case letters, digits and hyphens.
fn valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// `cargo xtask new-plugin`: generates the repository and returns its path.
pub fn new_plugin(kit: &Path, args: &[String]) -> Result<PathBuf, String> {
    let options = parse(args)?;
    let rev = match &options.rev {
        Some(rev) => rev.clone(),
        None => main_commit(kit)?,
    };
    let started = Instant::now();
    let dir = generate(kit, &options, &rev, false)?;
    eprintln!(
        "\nGenerated {} {} in {} ({:.1} s), depending on the kit at {rev}.\n\nNext:\n  cd {}\n  \
         cargo xtask check      # format, clippy, tests, layers, deny, private terms\n  \
         cargo xtask validate   # every format's validators\n  \
         git commit             # every generated file is staged\n\nIts README says how to \
         build it in every format, and its CONTRIBUTING.md how to work on it and the kit at \
         once.",
        options.kind.name(),
        options.name,
        dir.display(),
        started.elapsed().as_secs_f64(),
        dir.display(),
    );
    Ok(dir)
}

/// This checkout's commit, which must be on `origin/main` so that anyone
/// can fetch the revision a new plug-in pins.
fn main_commit(kit: &Path) -> Result<String, String> {
    let head = git(kit, &["rev-parse", "HEAD"])?;
    let on_main = Command::new("git")
        .args(["merge-base", "--is-ancestor", "HEAD", "origin/main"])
        .current_dir(kit)
        .status()
        .is_ok_and(|s| s.success());
    if !on_main {
        return Err(format!(
            "this checkout's commit, {head}, isn't on origin/main, so others couldn't fetch the \
             kit revision a new plug-in would pin. Check out main and pull, or pass --rev with a \
             commit on main"
        ));
    }
    if !git(kit, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        eprintln!(
            "warning: this checkout has changes that aren't committed; the plug-in is generated \
             from its templates as they are, but depends on the kit at {head}"
        );
    }
    Ok(head)
}

/// Generates a plug-in repository from the templates in the kit's checkout
/// at `kit`, depending on the kit at `rev`; returns where it went. With
/// `patched`, a `[patch]` of the kit's crates is expected to be on.
fn generate(kit: &Path, options: &Options, rev: &str, patched: bool) -> Result<PathBuf, String> {
    let names = Names::new(&options.name)?;
    let id = options
        .id
        .clone()
        .unwrap_or_else(|| format!("com.loombridge.{}", names.krate));
    if !valid_id(&id) {
        return Err(format!(
            "`{id}` isn't a reverse-DNS ID such as com.loombridge.{}",
            names.krate
        ));
    }
    let dir = match &options.dir {
        Some(dir) => absolute(dir)?,
        None => kit
            .parent()
            .ok_or("the kit's checkout has no parent folder")?
            .join(&names.krate),
    };
    if dir.exists()
        && std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .next()
            .is_some()
    {
        return Err(format!("{} already exists and isn't empty", dir.display()));
    }
    let kit_real = std::fs::canonicalize(kit).map_err(|e| format!("{}: {e}", kit.display()))?;
    let mut ancestor = Some(dir.as_path());
    while let Some(path) = ancestor {
        if std::fs::canonicalize(path).is_ok_and(|p| p == kit_real) {
            return Err(format!(
                "{} is inside the kit's checkout; a plug-in is a repository of its own",
                dir.display()
            ));
        }
        ancestor = path.parent();
    }

    let vars = Vars {
        names,
        id,
        kind: options.kind,
        licence: options.licence,
        rev: rev.to_owned(),
        rust_version: rust_version(kit)?,
    };
    let templates = kit.join("xtask/templates");
    let mut written = Vec::new();
    for part in ["common", options.kind.name()] {
        render_tree(&templates.join(part), &dir, &vars, &mut written)?;
    }
    let mut copy = |from: PathBuf, to: &str| -> Result<(), String> {
        std::fs::copy(&from, dir.join(to))
            .map_err(|e| format!("couldn't copy {}: {e}", from.display()))?;
        written.push(to.to_owned());
        Ok(())
    };
    match options.licence {
        Licence::Permissive => {
            copy(kit.join("LICENSE-MIT"), "LICENSE-MIT")?;
            copy(kit.join("LICENSE-APACHE"), "LICENSE-APACHE")?;
        }
        Licence::Gpl => copy(templates.join("licences/GPL-3.0-or-later.txt"), "LICENSE")?,
    }
    // The kit's own toolchain pin and formatting, so the two stay in step.
    copy(kit.join("rust-toolchain.toml"), "rust-toolchain.toml")?;
    copy(kit.join("rustfmt.toml"), "rustfmt.toml")?;

    // The lock file starts from the kit's, so the plug-in builds with the
    // versions the kit is tested with; Cargo then drops what it doesn't use
    // and adds the kit's crates from git.
    copy(kit.join("Cargo.lock"), "Cargo.lock")?;
    let output = Command::new(cargo_bin())
        .args(["metadata", "--format-version", "1"])
        .current_dir(&dir)
        .output()
        .map_err(|e| format!("couldn't run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "couldn't resolve the new plug-in's dependencies:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let lock = std::fs::read_to_string(dir.join("Cargo.lock"))
        .map_err(|e| format!("couldn't read the new Cargo.lock: {e}"))?;
    if !patched && !lock.contains(&format!("git+{KIT_REPOSITORY}")) {
        eprintln!(
            "warning: Cargo.lock doesn't take the kit from git, so a `[patch]` of the kit's \
             crates is on in a folder above {}; turn it off and run `cargo metadata` before \
             committing Cargo.lock",
            dir.display()
        );
    }
    cargo(
        &dir,
        &["fmt", "--package", &vars.names.krate, "--package", "xtask"],
    )?;

    git(&dir, &["init", "--quiet", "--initial-branch=main"])?;
    written.sort();
    let mut add = vec!["add", "--"];
    add.extend(written.iter().map(String::as_str));
    git(&dir, &add)?;
    Ok(dir)
}

/// What the templates' placeholders stand for.
struct Vars {
    names: Names,
    id: String,
    kind: Kind,
    licence: Licence,
    rev: String,
    rust_version: String,
}

/// The kit's `rust-version`, from its workspace manifest.
fn rust_version(kit: &Path) -> Result<String, String> {
    let manifest = std::fs::read_to_string(kit.join("Cargo.toml"))
        .map_err(|e| format!("couldn't read the kit's Cargo.toml: {e}"))?;
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("rust-version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or_else(|| "the kit's Cargo.toml has no rust-version".to_owned())
}

impl Vars {
    /// Whether a block named `name` is kept.
    fn keeps(&self, name: &str) -> Option<bool> {
        Some(match name {
            "gpl" => self.licence == Licence::Gpl,
            "permissive" => self.licence == Licence::Permissive,
            "instrument" => self.kind == Kind::Instrument,
            "effect" => self.kind == Kind::Effect,
            _ => return None,
        })
    }

    fn replacements(&self) -> Vec<(&'static str, String)> {
        let dependencies = KIT_CRATES
            .iter()
            .map(|(name, _)| {
                // As in the kit's own workspace: a plug-in crate turns on
                // the formats it exports.
                let features = if *name == "lspk-plugin" {
                    ", default-features = false"
                } else {
                    ""
                };
                format!(
                    "{name} = {{ git = \"{KIT_REPOSITORY}\", rev = \"{}\"{features} }}",
                    self.rev
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let patch = KIT_CRATES
            .iter()
            // Indented for the list item it's shown in.
            .map(|(name, path)| format!("   {name} = {{ path = \"plugin-kit/{path}\" }}"))
            .collect::<Vec<_>>()
            .join("\n");
        vec![
            ("{{kit_dependencies}}", dependencies),
            ("{{kit_patch}}", patch),
            ("{{crate_lib}}", self.names.lib.clone()),
            ("{{crate}}", self.names.krate.clone()),
            ("{{Type}}", self.names.ty.clone()),
            ("{{Name}}", self.names.display.clone()),
            ("{{id}}", self.id.clone()),
            ("{{licence}}", self.licence.spdx().to_owned()),
            ("{{kind}}", self.kind.name().to_owned()),
            ("{{rev}}", self.rev.clone()),
            ("{{rust_version}}", self.rust_version.clone()),
            ("{{nextest}}", crate::NEXTEST_VERSION.to_owned()),
        ]
    }
}

/// Renders every file under `from` into `to`, adding each one's path,
/// relative to `to`, to `written`.
fn render_tree(
    from: &Path,
    to: &Path,
    vars: &Vars,
    written: &mut Vec<String>,
) -> Result<(), String> {
    let replacements = vars.replacements();
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        let folder = from.join(&relative);
        let entries =
            std::fs::read_dir(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", folder.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".DS_Store" {
                continue;
            }
            let source = relative.join(&name);
            if entry.path().is_dir() {
                stack.push(source);
                continue;
            }
            let target = render_path(&source, &replacements);
            let out = to.join(&target);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("couldn't make {}: {e}", parent.display()))?;
            }
            if name.ends_with(".flac") {
                std::fs::copy(entry.path(), &out)
                    .map_err(|e| format!("couldn't write {}: {e}", out.display()))?;
            } else {
                let text = std::fs::read_to_string(entry.path())
                    .map_err(|e| format!("{}: {e}", entry.path().display()))?;
                let text = render(&text, vars, &replacements)
                    .map_err(|e| format!("{}: {e}", entry.path().display()))?;
                std::fs::write(&out, text)
                    .map_err(|e| format!("couldn't write {}: {e}", out.display()))?;
            }
            written.push(target);
        }
    }
    Ok(())
}

/// A template's path rendered: placeholders replaced, a leading `_` made a
/// `.`, a trailing `.tmpl` dropped, and `/` between components.
fn render_path(source: &Path, replacements: &[(&str, String)]) -> String {
    let parts: Vec<String> = source
        .components()
        .map(|c| {
            let mut part = c.as_os_str().to_string_lossy().into_owned();
            for (from, to) in replacements {
                part = part.replace(from, to);
            }
            if let Some(rest) = part.strip_prefix('_') {
                part = format!(".{rest}");
            }
            if let Some(rest) = part.strip_suffix(".tmpl") {
                part = rest.to_owned();
            }
            part
        })
        .collect();
    parts.join("/")
}

/// A template's text rendered: blocks kept or dropped, then placeholders
/// replaced. Fails on an unknown or unclosed block, or a placeholder left.
fn render(text: &str, vars: &Vars, replacements: &[(&str, String)]) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    // The open blocks, each with whether it's kept.
    let mut open: Vec<(String, bool)> = Vec::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(name) = block_marker(trimmed, "{{#") {
            let keep = vars
                .keeps(name)
                .ok_or_else(|| format!("unknown block `{name}`"))?;
            open.push((name.to_owned(), keep));
            continue;
        }
        if let Some(name) = block_marker(trimmed, "{{/") {
            match open.pop() {
                Some((opened, _)) if opened == name => continue,
                _ => return Err(format!("`{{{{/{name}}}}}` closes no block")),
            }
        }
        if open.iter().all(|(_, keep)| *keep) {
            out.push_str(line);
        }
    }
    if let Some((name, _)) = open.last() {
        return Err(format!("block `{name}` isn't closed"));
    }
    for (from, to) in replacements {
        out = out.replace(from, to);
    }
    if let Some(left) = leftover_placeholder(&out) {
        return Err(format!("unknown placeholder `{left}`"));
    }
    Ok(out)
}

/// The block name in a marker line such as `{{#gpl}}`, after `prefix`.
fn block_marker<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let name = line.strip_prefix(prefix)?.strip_suffix("}}")?;
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then_some(name)
}

/// A placeholder-shaped `{{word}}` left in rendered text. GitHub Actions'
/// `${{ ... }}` expressions have spaces inside, so they aren't matched.
fn leftover_placeholder(text: &str) -> Option<&str> {
    text.match_indices("{{").find_map(|(at, _)| {
        let rest = &text[at + 2..];
        let end = rest.find("}}")?;
        let inner = &rest[..end];
        (!inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '#' | '/')))
        .then(|| &text[at..at + 2 + end + 2])
    })
}

/// `path` made absolute against the current folder.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Runs git in `dir` and returns what it printed, trimmed.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("couldn't run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

const CHECK_USAGE: &str =
    "usage: cargo xtask template-check [--dir PATH] [--rev COMMIT] [--update]";

/// The plug-ins `cargo xtask template-check` generates: one of each kind,
/// and between them both licences.
const CHECKED: &[(&str, Kind, Licence)] = &[
    ("template-instrument", Kind::Instrument, Licence::Gpl),
    ("template-effect", Kind::Effect, Licence::Permissive),
];

/// `cargo xtask template-check`: generates an instrument and an effect into
/// a folder of their own, built against this checkout through a `[patch]` in
/// that folder's `.cargo/config.toml` (with one target folder for both), and
/// runs each one's tasks as its CI would.
pub fn template_check(kit: &Path, args: &[String]) -> Result<(), String> {
    let mut dir = None;
    let mut rev = None;
    let mut update = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("`{arg}` needs a value\n{CHECK_USAGE}"))
        };
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(value()?)),
            "--rev" => rev = Some(value()?),
            "--update" => update = true,
            other => return Err(format!("unexpected `{other}`\n{CHECK_USAGE}")),
        }
    }
    let dir = match dir {
        Some(dir) => absolute(&dir)?,
        None => std::env::temp_dir().join("lspk-template-check"),
    };
    // Any revision on main that Cargo can fetch: the patch replaces every
    // kit crate with this checkout's.
    let rev = match rev {
        Some(rev) => rev,
        None => git(kit, &["merge-base", "HEAD", "origin/main"])
            .or_else(|_| git(kit, &["rev-parse", "HEAD"]))?,
    };
    std::fs::create_dir_all(dir.join(".cargo"))
        .map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    let path = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let mut config = format!(
        "# Written by `cargo xtask template-check`: the generated plug-ins build against the \
         kit's checkout\n# at {}, into one target folder.\n\n[build]\ntarget-dir = \"{}\"\n\n\
         [patch.\"{KIT_REPOSITORY}\"]\n",
        path(kit),
        path(&dir.join("target"))
    );
    for (name, folder) in KIT_CRATES {
        config.push_str(&format!(
            "{name} = {{ path = \"{}\" }}\n",
            path(&kit.join(folder))
        ));
    }
    std::fs::write(dir.join(".cargo/config.toml"), config)
        .map_err(|e| format!("couldn't write the patch: {e}"))?;

    let counts = cfg!(all(target_os = "linux", target_arch = "x86_64"));
    let mut results: Vec<(String, f64, bool)> = Vec::new();
    for &(name, kind, licence) in CHECKED {
        let repo = dir.join(name);
        if repo.exists() {
            std::fs::remove_dir_all(&repo)
                .map_err(|e| format!("couldn't remove {}: {e}", repo.display()))?;
        }
        let options = Options {
            name: name.to_owned(),
            kind,
            dir: Some(repo.clone()),
            id: None,
            licence,
            rev: Some(rev.clone()),
        };
        eprintln!("\n==> {name}: new-plugin");
        let started = Instant::now();
        generate(kit, &options, &rev, true)?;
        results.push((
            format!("{name}: new-plugin"),
            started.elapsed().as_secs_f64(),
            true,
        ));
        if update {
            let crate_dir = format!("crates/{name}");
            cargo(&repo, &["xtask", "golden", "--update"])?;
            let status = Command::new(cargo_bin())
                .args(["test", "--locked", "-p", name, "--test", "ids"])
                .env(lspk_test::ids::UPDATE_ENV, "1")
                .current_dir(&repo)
                .status()
                .map_err(|e| format!("couldn't run cargo test: {e}"))?;
            if !status.success() {
                return Err(format!("{name}'s parameter ID test failed"));
            }
            let templates = kit
                .join("xtask/templates")
                .join(kind.name())
                .join("crates/{{crate}}/tests");
            let golden = repo.join(&crate_dir).join("tests/golden");
            std::fs::create_dir_all(templates.join("golden"))
                .map_err(|e| format!("couldn't make the template's golden folder: {e}"))?;
            for entry in
                std::fs::read_dir(&golden).map_err(|e| format!("{}: {e}", golden.display()))?
            {
                let entry = entry.map_err(|e| e.to_string())?;
                let to = templates.join("golden").join(entry.file_name());
                std::fs::copy(entry.path(), &to)
                    .map_err(|e| format!("couldn't copy to {}: {e}", to.display()))?;
            }
            std::fs::copy(
                repo.join(&crate_dir).join("tests/ids.txt"),
                templates.join("ids.txt"),
            )
            .map_err(|e| format!("couldn't copy the ID list: {e}"))?;
            eprintln!(
                "\nThe {} template's golden references and ID list are updated; commit them.",
                kind.name()
            );
        }
        let mut tasks: Vec<Vec<&str>> = vec![
            vec!["xtask", "check"],
            vec!["xtask", "validate"],
            vec!["xtask", "golden"],
        ];
        tasks.push(if counts {
            vec!["xtask", "bench", "--gate"]
        } else {
            vec!["xtask", "bench", "--quick"]
        });
        for task in tasks {
            let label = format!("{name}: cargo {}", task.join(" "));
            eprintln!("\n==> {label}");
            let started = Instant::now();
            let ok = cargo(&repo, &task)
                .inspect_err(|e| eprintln!("{e}"))
                .is_ok();
            results.push((label, started.elapsed().as_secs_f64(), ok));
        }
    }

    eprintln!("\n==> template-check summary ({})", dir.display());
    for (label, seconds, ok) in &results {
        let status = if *ok { "ok" } else { "FAILED" };
        eprintln!("{status:>6}  {seconds:>7.1} s  {label}");
    }
    if !counts {
        eprintln!(
            "\nThe benchmark gate counts instructions on x86-64 Linux only, so here the \
             benchmarks were timed instead; CI runs the gate."
        );
    }
    let failed = results.iter().filter(|(_, _, ok)| !ok).count();
    if failed == 0 {
        Ok(())
    } else {
        Err(format!(
            "{failed} of {} template tasks failed",
            results.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(kind: Kind, licence: Licence) -> Vars {
        Vars {
            names: Names::new("bright-delay").expect("a name"),
            id: "com.loombridge.bright-delay".to_owned(),
            kind,
            licence,
            rev: "abc123".to_owned(),
            rust_version: "1.99".to_owned(),
        }
    }

    #[test]
    fn names_follow_from_the_crate_name() {
        let names = Names::new("bright-delay-2").expect("a name");
        assert_eq!(names.lib, "bright_delay_2");
        assert_eq!(names.ty, "BrightDelay2");
        assert_eq!(names.display, "Bright Delay 2");
        for bad in [
            "Bright",
            "bright_delay",
            "-bright",
            "bright-",
            "bright--delay",
            "2bright",
            "lspk-x",
            "xtask",
        ] {
            assert!(Names::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_are_reverse_dns() {
        assert!(valid_id("com.loombridge.bright-delay"));
        assert!(!valid_id("bright"));
        assert!(!valid_id("com..x"));
        assert!(!valid_id("com.Loombridge.x"));
    }

    #[test]
    fn blocks_keep_their_licence_and_kind() {
        let text = "a\n{{#gpl}}\ngpl {{crate}}\n{{/gpl}}\n{{#permissive}}\nmit\n{{/permissive}}\n\
                    {{#effect}}\n{{#gpl}}\ngpl effect\n{{/gpl}}\n{{/effect}}\nz ${{ matrix.os }}\n";
        let v = vars(Kind::Effect, Licence::Gpl);
        assert_eq!(
            render(text, &v, &v.replacements()).expect("rendered"),
            "a\ngpl bright-delay\ngpl effect\nz ${{ matrix.os }}\n"
        );
        let v = vars(Kind::Instrument, Licence::Permissive);
        assert_eq!(
            render(text, &v, &v.replacements()).expect("rendered"),
            "a\nmit\nz ${{ matrix.os }}\n"
        );
    }

    #[test]
    fn mistakes_in_a_template_are_caught() {
        let v = vars(Kind::Effect, Licence::Gpl);
        let r = v.replacements();
        assert!(render("{{#gpl}}\nx\n", &v, &r).is_err());
        assert!(render("{{/gpl}}\n", &v, &r).is_err());
        assert!(render("{{#other}}\n{{/other}}\n", &v, &r).is_err());
        assert!(render("{{nmae}}\n", &v, &r).is_err());
        assert!(render("format!(\"{{}}\")\n", &v, &r).is_ok());
    }

    #[test]
    fn paths_are_rendered() {
        let v = vars(Kind::Effect, Licence::Gpl);
        let r = v.replacements();
        assert_eq!(
            render_path(Path::new("crates/{{crate}}/Cargo.toml.tmpl"), &r),
            "crates/bright-delay/Cargo.toml"
        );
        assert_eq!(
            render_path(Path::new("_github/workflows/ci.yml"), &r),
            ".github/workflows/ci.yml"
        );
    }

    #[test]
    fn every_kit_crate_comes_from_one_revision() {
        let v = vars(Kind::Effect, Licence::Gpl);
        let r = v.replacements();
        let deps = &r[0].1;
        assert_eq!(deps.lines().count(), KIT_CRATES.len());
        assert!(deps.lines().all(|l| l.contains("rev = \"abc123\"")));
    }

    fn kit_root() -> &'static Path {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap_or(manifest)
    }

    #[test]
    fn every_template_renders_for_every_kind_and_licence() {
        let templates = kit_root().join("xtask/templates");
        for kind in [Kind::Instrument, Kind::Effect] {
            for licence in [Licence::Permissive, Licence::Gpl] {
                let v = vars(kind, licence);
                let r = v.replacements();
                let mut stack = vec![templates.join("common"), templates.join(kind.name())];
                while let Some(dir) = stack.pop() {
                    for entry in std::fs::read_dir(&dir).expect("a template folder") {
                        let path = entry.expect("an entry").path();
                        if path.is_dir() {
                            stack.push(path);
                        } else if path.extension().is_none_or(|e| e != "flac") {
                            let text = std::fs::read_to_string(&path).expect("a text template");
                            if let Err(e) = render(&text, &v, &r) {
                                panic!("{}: {e}", path.display());
                            }
                        }
                    }
                }
            }
        }
    }

    /// The generated CI installs the same tools at the same versions, with
    /// the same actions at the same commits, as the kit's CI.
    #[test]
    fn the_template_ci_pins_what_the_kit_ci_pins() {
        let read = |path: &str| std::fs::read_to_string(kit_root().join(path)).expect(path);
        let kit = read(".github/workflows/ci.yml");
        let template = read("xtask/templates/common/_github/workflows/ci.yml");
        let pins = template.lines().map(str::trim).filter(|l| {
            l.starts_with("uses: ")
                || l.starts_with("- uses: ")
                || l.starts_with("tool: ")
                || ["_REV: ", "_VERSION: ", "_SHA256: "]
                    .iter()
                    .any(|k| l.contains(k))
        });
        let mut count = 0;
        for pin in pins {
            count += 1;
            assert!(
                kit.lines().any(|k| k.trim() == pin),
                "the template's CI pins `{pin}`, which the kit's CI doesn't"
            );
        }
        assert!(count >= 7, "{count} pins");
    }

    #[test]
    fn options_are_parsed() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let options = parse(&args(&[
            "bright-delay",
            "--kind",
            "effect",
            "--licence",
            "gpl-3.0-or-later",
        ]))
        .expect("options");
        assert_eq!(options.kind, Kind::Effect);
        assert_eq!(options.licence, Licence::Gpl);
        assert!(parse(&args(&["x"])).is_err());
        assert!(parse(&args(&["x", "--kind", "synth"])).is_err());
        assert!(parse(&args(&["x", "y", "--kind", "effect"])).is_err());
    }
}
