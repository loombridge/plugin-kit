//! Repository tasks for the plug-in kit and for every plug-in built on it.
//! Run `cargo xtask help` for the list.
//!
//! The kit's own `cargo xtask` is this crate's binary, run with [`KIT`]. A
//! plug-in's repository has a small `xtask` crate of its own that depends on
//! this one at the same git revision as its other kit crates and calls
//! [`main`] with its own [`Repo`], so its check, bundle, validate, golden and
//! bench tasks are the kit's, never copies of them. `cargo xtask new-plugin`
//! generates such a repository from the templates in `xtask/templates`
//! (see [`new_plugin`]).

mod bench;
mod formats;
mod harness;
pub mod new_plugin;
mod private_terms;
pub mod vendored;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::Value;

pub use vendored::Vendored;

const USAGE: &str = "\
Usage: cargo xtask <task>

Tasks:
  check          Run every check: format, clippy, tests (cargo-nextest, then the
                 doctests), layers, deny, the deny self-test, vendored code, and
                 private terms
  layers         Check the crate layering rules
  deny-selftest  Prove that deny.toml still rejects the licences it must
  vendored       Check every vendored C and C++ component's licence
  private-terms  Fail if a tracked file contains a word listed in .git/info/private-terms
                 (maintainers' checkouts only; skipped without it)
  bundle <crate>...
                 Build each plug-in crate in release and lay it out in target/bundle as
                 a .clap, a .vst3 and, on macOS, a .component (Audio Unit v2), and its
                 standalone app if it has a <crate>-standalone binary (an .app on
                 macOS, an executable elsewhere), signed ad hoc on macOS
  golden [--update] [NAME...]
                 Run every crate's golden suite (tests/golden.rs) against its references.
                 With --update, first remake the references whose renders changed (or
                 only those named), recording this platform as the one that made them;
                 say why in the pull request
  bench [--quick] [--machine NAME]
                 Build every crate's benchmarks (benches/perf.rs) in release, time each on
                 this machine, and write the results with the machine, CPU, OS and commit
                 as JSON under target/bench/
  bench --gate   Count each benchmark's instructions with Valgrind (x86-64 Linux, as CI
                 does) and fail if any is more than 3 % above bench/baseline.json
  bench --update-baseline --reason TEXT [--from COUNTS.json]
                 Write bench/baseline.json from counts made here (x86-64 Linux) or by CI
                 (the gate prints them); commit it and say why in the pull request
  validate [<crate>...]
                 Bundle the plug-in crates (the repository's own if none are named) and
                 run each format's validators on them: clap-validator; Steinberg's VST3
                 validator, built from the kit's vendored VST3 SDK; on macOS, auval, with
                 each .component installed in ~/Library/Audio/Plug-Ins/Components while it
                 runs; and pluginval at strictness 5 where it's on the PATH (skipped
                 otherwise)
";

const KIT_USAGE: &str = "\
  new-plugin <name> --kind instrument|effect [--dir PATH] [--id REVERSE-DNS]
             [--licence mit-or-apache-2.0|gpl-3.0-or-later] [--rev COMMIT]
                 Generate a new plug-in repository from the templates, depending on the
                 kit at this checkout's commit on main (or at --rev), beside this
                 checkout unless --dir says where
  template-check [--dir PATH] [--rev COMMIT] [--update]
                 Generate an instrument and an effect into PATH (a temporary folder by
                 default), built against this checkout through Cargo's [patch], and run
                 their check, validate and golden tasks and, on x86-64 Linux, their
                 benchmark gate, as CI does. With --update, first remake the templates'
                 golden references and parameter ID lists from the generated plug-ins
";

const HELP: &str = "  help           Show this message\n";

/// The usage text for `repo`.
fn usage(repo: &Repo) -> String {
    let kit = if repo.is_kit { KIT_USAGE } else { "" };
    format!("{USAGE}{kit}{HELP}")
}

/// The cargo-nextest release the check is written for, and CI installs. Keep
/// in step with `.config/nextest.toml` and the CI workflow.
pub const NEXTEST_VERSION: &str = "0.9.146";

/// The kit's UI crate, which headless crates never depend on.
const UI: &str = "lspk-ui";

/// Windowing, GPU and UI-toolkit crates that headless crates must not reach.
const UI_TOOLKITS: &[&str] = &[
    "baseview", "eframe", "egui", "iced", "slint", "vizia", "wgpu", "winit",
];

/// The copyleft licences a permissive repository's deny.toml rejects, as
/// the deny self-test's fixture holds them: GPL, LGPL and AGPL.
pub const REJECTS_COPYLEFT: &[&str] =
    &["GPL-3.0-or-later", "LGPL-2.1-or-later", "AGPL-3.0-or-later"];

/// The licence a GPL-3.0-or-later repository's deny.toml rejects of those
/// in the fixture: AGPL.
pub const REJECTS_AGPL: &[&str] = &["AGPL-3.0-or-later"];

/// A repository the tasks run in: the kit itself ([`KIT`]) or a plug-in
/// built on it ([`Repo::plugin`]).
#[derive(Clone, Copy, Debug)]
pub struct Repo {
    /// Its xtask crate's package name.
    pub xtask: &'static str,
    /// The plug-in crates `cargo xtask validate` checks when none are named.
    pub plugins: &'static [&'static str],
    /// Crates that never depend on `lspk-ui` or reach a UI toolkit, so they
    /// build and test headless. Its xtask belongs here, so every task builds
    /// without a windowing system's headers.
    pub headless: &'static [&'static str],
    /// Whether a crate is one that no crate here may depend on.
    pub downstream: fn(&str) -> bool,
    /// The licences in the deny self-test's fixture that deny.toml must
    /// reject.
    pub rejects: &'static [&'static str],
    /// Its vendored C and C++ components.
    pub vendored: &'static [Vendored],
    /// Whether this is the kit itself, which also generates plug-ins, and
    /// whose own checkout holds the kit's files the tasks use (the VST3
    /// validator's sources and the deny self-test's fixture).
    pub is_kit: bool,
}

impl Repo {
    /// A plug-in repository whose xtask crate is `xtask`, whose plug-in
    /// crates are `plugins`, and whose deny.toml rejects `rejects`
    /// ([`REJECTS_COPYLEFT`] or [`REJECTS_AGPL`]).
    #[must_use]
    pub const fn plugin(
        plugins: &'static [&'static str],
        rejects: &'static [&'static str],
    ) -> Self {
        Self {
            xtask: "xtask",
            plugins,
            headless: &["xtask"],
            downstream: no_crate,
            rejects,
            vendored: &[],
            is_kit: false,
        }
    }

    /// Where the kit's own files are: this checkout for the kit, and for a
    /// plug-in the kit checkout Cargo made for its git dependency (or the
    /// local checkout a `[patch]` points at).
    fn kit_root(&self, root: &Path) -> PathBuf {
        if self.is_kit {
            root.to_path_buf()
        } else {
            let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
            manifest.parent().unwrap_or(manifest).to_path_buf()
        }
    }
}

fn no_crate(_: &str) -> bool {
    false
}

/// The plug-in kit.
pub const KIT: Repo = Repo {
    xtask: "lspk-xtask",
    plugins: harness::EXAMPLES,
    headless: &[
        "lspk-rt",
        "lspk-params",
        "lspk-commands",
        "lspk-dsp",
        "lspk-voice",
        "lspk-mod",
        "lspk-fx",
        "lspk-wavetable",
        "lspk-seq",
        "lspk-presets",
        // The format layer drives an editor through its own trait, so
        // plug-ins and their tests build without a windowing system.
        "lspk-plugin",
        "lspk-xtask",
    ],
    downstream: is_downstream,
    rejects: REJECTS_COPYLEFT,
    vendored: vendored::VENDORED,
    is_kit: true,
};

/// Whether a crate is one built on the kit (a plug-in or Beatlayer), which no
/// kit crate may depend on.
fn is_downstream(name: &str) -> bool {
    name == "beatlayer"
        || name == "reagent"
        || name.starts_with("reagent-")
        || name.starts_with("bl-")
}

/// Runs the task the command line names in `repo`, and reports any error.
#[must_use]
pub fn main(repo: &Repo) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = workspace_root().and_then(|root| run(repo, &root, &args));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(repo: &Repo, root: &Path, args: &[String]) -> Result<(), String> {
    let task = args.first().map(String::as_str);
    match task {
        Some("bundle") => return formats::bundle(root, &args[1..], &usage(repo)).map(drop),
        Some(formats::PLUGIN_INFO) => {
            let library = args.get(1).ok_or("name the library")?;
            return formats::print_plugin_info(Path::new(library));
        }
        Some("golden") => return harness::golden(root, &args[1..]),
        Some("bench") => return bench::bench(root, &args[1..]),
        Some("validate") => return harness::validate(repo, root, &args[1..]),
        Some("new-plugin") if repo.is_kit => {
            return new_plugin::new_plugin(root, &args[1..]).map(drop);
        }
        Some("template-check") if repo.is_kit => {
            return new_plugin::template_check(root, &args[1..]);
        }
        _ => {}
    }
    if let Some(extra) = args.get(1) {
        return Err(format!("unexpected `{extra}`\n\n{}", usage(repo)));
    }
    match task {
        Some("check") => check(repo, root),
        Some("layers") => layers(repo, root),
        Some("deny-selftest") => deny_selftest(repo, root),
        Some("private-terms") => private_terms::check(root),
        Some("vendored") => vendored_check(repo, root),
        None | Some("help" | "-h" | "--help") => {
            print!("{}", usage(repo));
            Ok(())
        }
        Some(other) => Err(format!("unknown task `{other}`\n\n{}", usage(repo))),
    }
}

/// Runs every check, reports each result, and fails if any failed.
fn check(repo: &Repo, root: &Path) -> Result<(), String> {
    require_cargo_deny()?;
    require_cargo_nextest()?;
    type Step<'a> = (&'a str, Box<dyn Fn() -> Result<(), String> + 'a>);
    let steps: Vec<Step<'_>> = vec![
        (
            "format",
            Box::new(|| cargo(root, &["fmt", "--all", "--check"])),
        ),
        (
            "clippy",
            Box::new(|| {
                cargo(
                    root,
                    &[
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--all-features",
                        "--locked",
                        "--",
                        "-D",
                        "warnings",
                    ],
                )
            }),
        ),
        ("tests", Box::new(|| run_tests(root))),
        ("layers", Box::new(|| layers(repo, root))),
        (
            "licences, advisories, bans, sources",
            Box::new(|| cargo(root, &["deny", "--locked", "check"])),
        ),
        ("deny self-test", Box::new(|| deny_selftest(repo, root))),
        ("vendored code", Box::new(|| vendored_check(repo, root))),
        ("private terms", Box::new(|| private_terms::check(root))),
    ];

    let mut failed = Vec::new();
    for (name, step) in &steps {
        eprintln!("\n==> {name}");
        if let Err(message) = step() {
            eprintln!("{message}");
            failed.push(*name);
        }
    }

    eprintln!("\n==> summary");
    for (name, _) in &steps {
        let status = if failed.contains(name) {
            "FAILED"
        } else {
            "ok"
        };
        eprintln!("{status:>6}  {name}");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} of {} checks failed", failed.len(), steps.len()))
    }
}

/// Runs the tests with cargo-nextest, each in a process of its own and as
/// many at once as there are cores, then the doctests, which nextest doesn't
/// run, with `cargo test --doc`. Both run, so one run reports every failure.
fn run_tests(root: &Path) -> Result<(), String> {
    let tests = cargo(
        root,
        &[
            "nextest",
            "run",
            "--workspace",
            "--all-features",
            "--locked",
        ],
    );
    let docs = cargo(
        root,
        &["test", "--doc", "--workspace", "--all-features", "--locked"],
    );
    tests.and(docs)
}

/// Enforces the repository's layering:
///
/// 1. No crate depends on one `repo.downstream` names (for the kit, a crate
///    built on the kit: a plug-in or Beatlayer).
/// 2. The headless crates never depend on `lspk-ui`.
/// 3. The headless crates never reach a windowing, GPU or UI-toolkit crate.
///
/// The rules follow normal and build dependencies, transitively. Dev
/// dependencies don't ship, so they're not checked.
fn layers(repo: &Repo, root: &Path) -> Result<(), String> {
    let graph = DependencyGraph::from_metadata(&cargo_metadata(root)?)?;
    let violations = layer_violations(repo, &graph);
    if violations.is_empty() {
        eprintln!("{} crates follow the layering rules", graph.members.len());
        Ok(())
    } else {
        Err(format!(
            "layering rules broken:\n  {}",
            violations.join("\n  ")
        ))
    }
}

/// Every way the graph breaks the layering rules, one line each.
fn layer_violations(repo: &Repo, graph: &DependencyGraph) -> Vec<String> {
    let mut violations = Vec::new();
    let names: BTreeSet<&str> = graph.members.iter().map(|m| graph.name(m)).collect();
    for headless in repo.headless {
        if !names.contains(headless) {
            violations.push(format!(
                "{headless} is listed as headless but isn't in the workspace; update the list"
            ));
        }
    }
    for member in &graph.members {
        let name = graph.name(member);
        if let Some(path) = graph.path_to(member, repo.downstream) {
            violations.push(format!(
                "{name} depends on a crate built on the kit: {path}"
            ));
        }
        if !repo.headless.contains(&name) {
            continue;
        }
        if let Some(path) = graph.path_to(member, |dep| dep == UI) {
            violations.push(format!("{name} depends on {UI}: {path}"));
        }
        if let Some(path) = graph.path_to(member, |dep| UI_TOOLKITS.contains(&dep)) {
            violations.push(format!("{name} reaches a UI crate: {path}"));
        }
    }
    violations
}

/// Checks the licences of the vendored C and C++ code.
fn vendored_check(repo: &Repo, root: &Path) -> Result<(), String> {
    vendored::check(root, repo.vendored, &private_terms::tracked_files(root)?)
}

/// Runs cargo-deny with this repository's deny.toml against the kit's
/// fixture, which depends on a GPL, an LGPL and an AGPL crate, and fails
/// unless it rejects each licence in `repo.rejects`.
fn deny_selftest(repo: &Repo, root: &Path) -> Result<(), String> {
    require_cargo_deny()?;
    let manifest = repo
        .kit_root(root)
        .join("xtask/fixtures/licence-reject/Cargo.toml");
    let output = Command::new(cargo_bin())
        .arg("deny")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--config")
        .arg(root.join("deny.toml"))
        .args(["check", "licenses"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("couldn't run cargo deny: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let licences = repo.rejects.join(", ");

    if output.status.success() {
        return Err(format!(
            "deny.toml accepted {licences}; the licence policy is broken"
        ));
    }
    let missed: Vec<&str> = repo
        .rejects
        .iter()
        .copied()
        .filter(|licence| !rejected(&stderr, licence))
        .collect();
    if !missed.is_empty() {
        return Err(format!(
            "cargo deny failed on the fixture, but didn't reject {}:\n{stderr}",
            missed.join(", ")
        ));
    }
    eprintln!("deny.toml rejects {licences}, as intended");
    Ok(())
}

/// Whether cargo-deny's report names `licence` as a whole word (so
/// `GPL-3.0-or-later` isn't found inside `AGPL-3.0-or-later`).
fn rejected(report: &str, licence: &str) -> bool {
    report.match_indices(licence).any(|(at, _)| {
        !report[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric())
    })
}

/// The resolved dependency graph from `cargo metadata`, by package ID.
struct DependencyGraph {
    members: Vec<String>,
    names: BTreeMap<String, String>,
    edges: BTreeMap<String, Vec<String>>,
}

impl DependencyGraph {
    fn from_metadata(metadata: &Value) -> Result<Self, String> {
        let malformed = |what: &str| format!("unexpected `cargo metadata` output: no {what}");

        let names = metadata["packages"]
            .as_array()
            .ok_or_else(|| malformed("packages"))?
            .iter()
            .filter_map(|p| Some((p["id"].as_str()?.to_owned(), p["name"].as_str()?.to_owned())))
            .collect();

        let members = metadata["workspace_members"]
            .as_array()
            .ok_or_else(|| malformed("workspace_members"))?
            .iter()
            .filter_map(|id| id.as_str().map(str::to_owned))
            .collect();

        let mut edges = BTreeMap::new();
        for node in metadata["resolve"]["nodes"]
            .as_array()
            .ok_or_else(|| malformed("resolve.nodes"))?
        {
            let Some(id) = node["id"].as_str() else {
                continue;
            };
            let deps = node["deps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|dep| ships(dep))
                .filter_map(|dep| dep["pkg"].as_str().map(str::to_owned))
                .collect();
            edges.insert(id.to_owned(), deps);
        }

        Ok(Self {
            members,
            names,
            edges,
        })
    }

    fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.names.get(id).map_or(id, String::as_str)
    }

    /// The shortest dependency path from `start` to a package whose name
    /// matches, written as `a -> b -> c`, or `None` if there isn't one.
    fn path_to(&self, start: &str, matches: impl Fn(&str) -> bool) -> Option<String> {
        let mut previous: BTreeMap<&str, &str> = BTreeMap::new();
        let mut seen = BTreeSet::from([start]);
        let mut queue = VecDeque::from([start]);

        while let Some(id) = queue.pop_front() {
            for dep in self.edges.get(id).into_iter().flatten() {
                let dep = dep.as_str();
                if !seen.insert(dep) {
                    continue;
                }
                previous.insert(dep, id);
                if matches(self.name(dep)) {
                    let mut path = vec![self.name(dep)];
                    let mut at = dep;
                    while let Some(&before) = previous.get(at) {
                        path.push(self.name(before));
                        at = before;
                    }
                    path.reverse();
                    return Some(path.join(" -> "));
                }
                queue.push_back(dep);
            }
        }
        None
    }
}

/// Whether a dependency edge from `cargo metadata` ships in a build, that is,
/// whether it's a normal or build dependency rather than only a dev one.
fn ships(dep: &Value) -> bool {
    dep["dep_kinds"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|kind| kind["kind"].is_null() || kind["kind"] == "build")
}

pub(crate) fn cargo_metadata(root: &Path) -> Result<Value, String> {
    let output = Command::new(cargo_bin())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--locked",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("couldn't run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("couldn't parse cargo metadata: {e}"))
}

/// Runs a cargo command in the workspace root, streaming its output.
pub(crate) fn cargo(root: &Path, args: &[&str]) -> Result<(), String> {
    let status = Command::new(cargo_bin())
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("couldn't run cargo {}: {e}", args.join(" ")))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo {} failed", args.join(" ")))
    }
}

fn require_cargo_deny() -> Result<(), String> {
    let installed = Command::new(cargo_bin())
        .args(["deny", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if installed {
        Ok(())
    } else {
        Err(
            "cargo-deny isn't installed. Install it with `cargo install --locked cargo-deny`."
                .into(),
        )
    }
}

fn require_cargo_nextest() -> Result<(), String> {
    let installed = Command::new(cargo_bin())
        .args(["nextest", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if installed {
        Ok(())
    } else {
        Err(format!(
            "cargo-nextest isn't installed, and the check runs the tests with it. Install it \
             with `cargo install --locked cargo-nextest@{NEXTEST_VERSION}`."
        ))
    }
}

/// The cargo that invoked this task, so the pinned toolchain is used.
fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

/// The root of the workspace that `cargo xtask` was run in, asked of cargo
/// at run time rather than fixed when xtask was compiled, because a target
/// directory shared between checkouts can reuse a binary built in another.
fn workspace_root() -> Result<PathBuf, String> {
    let output = Command::new(cargo_bin())
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .output()
        .map_err(|e| format!("couldn't run cargo locate-project: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let manifest = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    manifest
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent directory", manifest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph of the headless crates plus `extra` members, with `edges`
    /// between packages named, not by ID.
    fn graph(extra: &[&str], edges: &[(&str, &str)]) -> DependencyGraph {
        let members: Vec<String> = KIT
            .headless
            .iter()
            .chain(extra)
            .map(|&name| name.to_owned())
            .collect();
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for &(from, to) in edges {
            map.entry(from.to_owned()).or_default().push(to.to_owned());
        }
        DependencyGraph {
            members,
            names: BTreeMap::new(),
            edges: map,
        }
    }

    #[test]
    fn the_kit_as_planned_follows_the_rules() {
        let graph = graph(
            &["lspk-ui", "lspk-agent", "lspk-test"],
            &[
                ("lspk-ui", "lspk-plugin"),
                ("lspk-ui", "egui"),
                ("lspk-ui", "lspk-params"),
                ("lspk-dsp", "lspk-rt"),
            ],
        );
        assert_eq!(layer_violations(&KIT, &graph), Vec::<String>::new());
    }

    #[test]
    fn a_headless_crate_reaching_the_ui_crate_is_caught() {
        let graph = graph(
            &["lspk-ui"],
            &[("lspk-fx", "lspk-dsp"), ("lspk-dsp", "lspk-ui")],
        );
        let violations = layer_violations(&KIT, &graph);
        assert!(
            violations.contains(&"lspk-dsp depends on lspk-ui: lspk-dsp -> lspk-ui".to_owned()),
            "{violations:?}"
        );
        assert!(
            violations
                .contains(&"lspk-fx depends on lspk-ui: lspk-fx -> lspk-dsp -> lspk-ui".to_owned()),
            "{violations:?}"
        );
    }

    #[test]
    fn the_format_layer_reaching_the_ui_crate_is_caught() {
        let graph = graph(&["lspk-ui"], &[("lspk-plugin", "lspk-ui")]);
        assert_eq!(
            layer_violations(&KIT, &graph),
            ["lspk-plugin depends on lspk-ui: lspk-plugin -> lspk-ui"]
        );
    }

    #[test]
    fn a_headless_crate_reaching_a_toolkit_is_caught() {
        let graph = graph(&[], &[("lspk-voice", "winit")]);
        assert_eq!(
            layer_violations(&KIT, &graph),
            ["lspk-voice reaches a UI crate: lspk-voice -> winit"]
        );
    }

    #[test]
    fn depending_on_a_plug_in_or_beatlayer_is_caught() {
        let graph = graph(
            &[],
            &[("lspk-plugin", "reagent-engine"), ("lspk-rt", "bl-rt")],
        );
        let violations = layer_violations(&KIT, &graph);
        assert_eq!(violations.len(), 2, "{violations:?}");
    }

    #[test]
    fn a_missing_headless_crate_is_reported() {
        let mut graph = graph(&[], &[]);
        graph.members.retain(|name| name != "lspk-seq");
        assert_eq!(layer_violations(&KIT, &graph).len(), 1);
    }

    #[test]
    fn a_plugins_xtask_stays_headless() {
        const PLUGIN: Repo = Repo::plugin(&["bright-delay"], REJECTS_COPYLEFT);
        let graph = |edges: &[(&str, &str)]| DependencyGraph {
            members: vec!["bright-delay".to_owned(), "xtask".to_owned()],
            names: BTreeMap::new(),
            edges: edges
                .iter()
                .map(|(from, to)| ((*from).to_owned(), vec![(*to).to_owned()]))
                .collect(),
        };
        let fine = graph(&[("bright-delay", "lspk-ui"), ("xtask", "lspk-xtask")]);
        assert_eq!(layer_violations(&PLUGIN, &fine), Vec::<String>::new());
        let broken = graph(&[("xtask", "lspk-ui")]);
        assert_eq!(
            layer_violations(&PLUGIN, &broken),
            ["xtask depends on lspk-ui: xtask -> lspk-ui"]
        );
    }

    #[test]
    fn rejected_licences_match_whole_identifiers() {
        let report = "rejected: AGPL-3.0-or-later";
        assert!(rejected(report, "AGPL-3.0-or-later"));
        assert!(!rejected(report, "GPL-3.0-or-later"));
        assert!(rejected("license = GPL-3.0-or-later", "GPL-3.0-or-later"));
    }
}
