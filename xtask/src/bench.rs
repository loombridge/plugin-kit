//! `cargo xtask bench`: the performance harness's tasks.
//!
//! - **Timing** (`cargo xtask bench`): builds every crate's `perf` bench
//!   target in release, times each benchmark on this machine, and writes the
//!   results with the machine, CPU, OS and commit to
//!   `target/bench/<time>-<commit>.json` (and `target/bench/latest.json`).
//!   Reference machines will run this nightly and a dashboard will keep the
//!   history; both wait for the machines.
//! - **The gate** (`cargo xtask bench --gate`, in CI on Linux x86-64):
//!   counts each benchmark's instructions with Valgrind's Callgrind and
//!   fails if any is more than [`THRESHOLD_PERCENT`] above the committed
//!   baseline, [`BASELINE`], or if a benchmark is missing from it.
//! - **Updating the baseline** (`cargo xtask bench --update-baseline
//!   --reason "..."`): counts here (Linux x86-64 with Valgrind), or takes
//!   the counts a CI run printed (`--from <file>`), and writes the baseline
//!   with the reason, for the pull request to commit and explain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lspk_test::bench::{MEASURED_SYMBOL, Measurement, table};
use serde_json::{Map, Value, json};

/// The committed instruction-count baseline, from the workspace root.
pub const BASELINE: &str = "bench/baseline.json";

/// How much slower, in instructions, a benchmark may get before the gate
/// fails.
pub const THRESHOLD_PERCENT: u64 = 3;

/// The bench target every crate keeps its suite in.
const TARGET: &str = "perf";

/// Where the baseline's counts are made: Callgrind on x86-64 Linux.
const PLATFORM: &str = "x86_64-linux";

/// The SIMD level counts are made at, so they don't depend on which x86-64
/// CPU the runner has (Valgrind doesn't run AVX-512 anyway).
const COUNT_SIMD: &str = "avx2";

const USAGE: &str = "usage: cargo xtask bench [--quick] [--machine NAME] | --gate | --count \
                     | --update-baseline --reason TEXT [--from COUNTS.json]";

/// What `cargo xtask bench` was asked to do.
#[derive(Debug, Default, PartialEq)]
struct Options {
    gate: bool,
    count: bool,
    update: bool,
    quick: bool,
    reason: Option<String>,
    from: Option<PathBuf>,
    machine: Option<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| format!("`{arg}` needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--gate" => options.gate = true,
            "--count" => options.count = true,
            "--update-baseline" => options.update = true,
            "--quick" => options.quick = true,
            "--reason" => options.reason = Some(value()?),
            "--from" => options.from = Some(PathBuf::from(value()?)),
            "--machine" => options.machine = Some(value()?),
            other => return Err(format!("unexpected `{other}`\n{USAGE}")),
        }
    }
    let modes = [options.gate, options.count, options.update];
    if modes.iter().filter(|m| **m).count() > 1 {
        return Err(format!(
            "choose one of --gate, --count and --update-baseline\n{USAGE}"
        ));
    }
    if options.update
        && options
            .reason
            .as_deref()
            .is_none_or(|r| r.trim().is_empty())
    {
        return Err(
            "--update-baseline needs --reason \"...\": why the counts changed, as the pull \
             request will say"
                .to_owned(),
        );
    }
    if !options.update && (options.reason.is_some() || options.from.is_some()) {
        return Err(format!(
            "--reason and --from go with --update-baseline\n{USAGE}"
        ));
    }
    Ok(options)
}

pub fn bench(root: &Path, args: &[String]) -> Result<(), String> {
    let options = parse(args)?;
    if options.gate {
        gate(root)
    } else if options.count {
        let counts = count_all(root)?;
        write_counts(root, &counts)?;
        Ok(())
    } else if options.update {
        update_baseline(root, &options)
    } else {
        time_all(root, &options)
    }
}

/// Builds every `perf` bench target in release and returns its binaries.
fn build(root: &Path) -> Result<Vec<PathBuf>, String> {
    eprintln!("==> building the benchmarks");
    let output = Command::new(super::cargo_bin())
        .args([
            "bench",
            "--workspace",
            "--all-features",
            "--locked",
            "--bench",
            TARGET,
            "--no-run",
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(root)
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("couldn't run cargo bench: {e}"))?;
    if !output.status.success() {
        return Err("the benchmarks didn't build".to_owned());
    }
    let mut binaries: Vec<PathBuf> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == TARGET)
        .filter_map(|m| m["executable"].as_str().map(PathBuf::from))
        .collect();
    binaries.sort();
    binaries.dedup();
    if binaries.is_empty() {
        return Err(format!("no crate has a `{TARGET}` bench target"));
    }
    Ok(binaries)
}

/// Runs a bench binary with `args` and returns what it printed.
fn run_binary(binary: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(binary)
        .args(args)
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("couldn't run {}: {e}", binary.display()))?;
    if !output.status.success() {
        return Err(format!("{} {} failed", binary.display(), args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Times every benchmark and writes the results under `target/bench/`.
fn time_all(root: &Path, options: &Options) -> Result<(), String> {
    let binaries = build(root)?;
    let mut measurements: Vec<Measurement> = Vec::new();
    for binary in &binaries {
        let mut args = vec!["time"];
        if options.quick {
            args.push("--quick");
        }
        let json = run_binary(binary, &args)?;
        let mut more: Vec<Measurement> = serde_json::from_str(json.trim())
            .map_err(|e| format!("{} printed results that aren't JSON: {e}", binary.display()))?;
        measurements.append(&mut more);
    }
    let machine = machine(options.machine.clone());
    let (commit, dirty) = commit(root);
    let date = utc_now();
    let results = json!({
        "schema": 1,
        "date": date,
        "commit": commit,
        "dirty": dirty,
        "rustc": first_line(Command::new("rustc").arg("--version")),
        "quick": options.quick,
        "machine": machine,
        "measurements": measurements,
    });
    let dir = target_dir(root)?.join("bench");
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    let stamp: String = date.chars().filter(char::is_ascii_alphanumeric).collect();
    let short = commit.get(..10).unwrap_or(&commit);
    let file = dir.join(format!("{stamp}-{short}.json"));
    let text = serde_json::to_string_pretty(&results).map_err(|e| e.to_string())?;
    for path in [&file, &dir.join("latest.json")] {
        std::fs::write(path, &text)
            .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    }
    println!(
        "\n{} ({}), {}, {} cores, SIMD {}; commit {short}{}\n",
        machine["cpu"].as_str().unwrap_or("unknown CPU"),
        machine["model"].as_str().unwrap_or("unknown model"),
        machine["os"].as_str().unwrap_or("unknown OS"),
        machine["cores"],
        machine["simd"].as_str().unwrap_or("?"),
        if dirty { " with local changes" } else { "" },
    );
    print!("{}", table(&measurements));
    eprintln!("\nResults written to {}", file.display());
    Ok(())
}

/// Counts every benchmark's instructions under Callgrind.
fn count_all(root: &Path) -> Result<BTreeMap<String, u64>, String> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(format!(
            "instruction counts are made on x86-64 Linux, where the baseline was counted; this \
             is {}-{}. Take the counts from CI instead: the gate prints them, and \
             `--update-baseline --from <file>` takes them",
            std::env::consts::ARCH,
            std::env::consts::OS
        ));
    }
    let valgrind = valgrind_version()?;
    let binaries = build(root)?;
    let mut work = Vec::new();
    for binary in &binaries {
        for id in run_binary(binary, &["list"])?
            .lines()
            .filter(|l| !l.is_empty())
        {
            work.push((binary.clone(), id.to_owned()));
        }
    }
    let dir = target_dir(root)?.join("bench").join("callgrind");
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    eprintln!("==> counting {} benchmarks with {valgrind}", work.len());
    let next = AtomicUsize::new(0);
    let results = Mutex::new(BTreeMap::new());
    let errors = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(2, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some((binary, id)) = work.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let file = dir.join(format!("{}.out", id.replace('/', "__")));
                    match count_one(binary, id, &file) {
                        Ok(n) => {
                            results
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(id.clone(), n);
                        }
                        Err(e) => errors.lock().unwrap_or_else(|e| e.into_inner()).push(e),
                    }
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap_or_else(|e| e.into_inner());
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok(results.into_inner().unwrap_or_else(|e| e.into_inner()))
}

/// Counts benchmark `id`'s measured region in `binary`, writing
/// Callgrind's profile to `file`.
fn count_one(binary: &Path, id: &str, file: &Path) -> Result<u64, String> {
    let output = Command::new("valgrind")
        .arg("--tool=callgrind")
        .arg(format!("--callgrind-out-file={}", file.display()))
        .arg("--collect-atstart=no")
        .arg(format!("--toggle-collect={MEASURED_SYMBOL}"))
        .arg(binary)
        .args(["count", id])
        .env(lspk_dsp::simd::LEVEL_ENV, COUNT_SIMD)
        .output()
        .map_err(|e| format!("couldn't run valgrind: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{id}: valgrind failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let profile = std::fs::read_to_string(file)
        .map_err(|e| format!("{id}: couldn't read {}: {e}", file.display()))?;
    match totals(&profile) {
        Some(0) | None => Err(format!(
            "{id}: Callgrind counted nothing; did the benchmark call `{MEASURED_SYMBOL}`?"
        )),
        Some(n) => Ok(n),
    }
}

/// The instructions a Callgrind profile counted: its `totals:` line, or
/// failing that its `summary:` line.
fn totals(profile: &str) -> Option<u64> {
    let line = |key: &str| {
        profile
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
    };
    line("totals:").or_else(|| line("summary:"))
}

fn valgrind_version() -> Result<String, String> {
    first_line(Command::new("valgrind").arg("--version")).ok_or_else(|| {
        "valgrind isn't installed; instruction counts need it (`apt-get install valgrind`)"
            .to_owned()
    })
}

/// A counts file: the counts with where they were made.
fn counts_document(counts: &BTreeMap<String, u64>, reason: Option<&str>) -> Value {
    let mut doc = Map::new();
    doc.insert(
        "about".into(),
        json!(
            "Instructions each benchmark's measured region runs, counted by Callgrind on \
             x86-64 Linux at the AVX2 SIMD level. CI fails a pull request that makes any of \
             them more than 3 % higher; change them only with `cargo xtask bench \
             --update-baseline --reason ...`, and say why in the pull request."
        ),
    );
    doc.insert("platform".into(), json!(PLATFORM));
    doc.insert("simd".into(), json!(COUNT_SIMD));
    doc.insert("valgrind".into(), json!(valgrind_version().ok()));
    if let Some(reason) = reason {
        doc.insert("reason".into(), json!(reason));
    }
    doc.insert("instructions".into(), json!(counts));
    Value::Object(doc)
}

fn write_counts(root: &Path, counts: &BTreeMap<String, u64>) -> Result<String, String> {
    let text =
        serde_json::to_string_pretty(&counts_document(counts, None)).map_err(|e| e.to_string())?;
    let dir = target_dir(root)?.join("bench");
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    let path = dir.join("instructions.json");
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    eprintln!("Counts written to {}", path.display());
    Ok(text)
}

/// Reads the counts from a baseline or counts file.
fn read_counts(path: &Path) -> Result<BTreeMap<String, u64>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    let doc: Value =
        serde_json::from_str(&text).map_err(|e| format!("{} isn't JSON: {e}", path.display()))?;
    if doc["platform"] != PLATFORM {
        return Err(format!(
            "{} wasn't counted on {PLATFORM}, so it doesn't compare",
            path.display()
        ));
    }
    doc["instructions"]
        .as_object()
        .ok_or_else(|| format!("{} has no instruction counts", path.display()))?
        .iter()
        .map(|(id, n)| {
            n.as_u64()
                .map(|n| (id.clone(), n))
                .ok_or_else(|| format!("{}: `{id}` isn't a count", path.display()))
        })
        .collect()
}

/// The gate's verdict on counts against a baseline.
#[derive(Debug, Default, PartialEq)]
struct Verdict {
    /// One line per benchmark.
    lines: Vec<String>,
    /// Why it fails, one line each; empty if it passes.
    failures: Vec<String>,
    /// Benchmarks that got cheaper by more than the threshold.
    faster: Vec<String>,
}

fn compare(baseline: &BTreeMap<String, u64>, counts: &BTreeMap<String, u64>) -> Verdict {
    let mut verdict = Verdict::default();
    let ids: std::collections::BTreeSet<&String> = baseline.keys().chain(counts.keys()).collect();
    let width = ids.iter().map(|id| id.len()).max().unwrap_or(0);
    for id in ids {
        match (baseline.get(id), counts.get(id)) {
            (Some(&base), Some(&now)) => {
                let change = (now as f64 - base as f64) / base.max(1) as f64 * 100.0;
                let slower =
                    u128::from(now) * 100 > u128::from(base) * u128::from(100 + THRESHOLD_PERCENT);
                let faster =
                    u128::from(now) * 100 < u128::from(base) * u128::from(100 - THRESHOLD_PERCENT);
                let mark = if slower { "  SLOWER" } else { "" };
                verdict.lines.push(format!(
                    "{id:width$}  {base:>13}  {now:>13}  {change:>+7.2} %{mark}"
                ));
                if slower {
                    verdict.failures.push(format!(
                        "{id}: {change:+.2} % ({base} to {now} instructions), more than {THRESHOLD_PERCENT} %"
                    ));
                } else if faster {
                    verdict.faster.push(format!("{id}: {change:+.2} %"));
                }
            }
            (None, Some(&now)) => {
                verdict
                    .lines
                    .push(format!("{id:width$}  {:>13}  {now:>13}  new", "-"));
                verdict
                    .failures
                    .push(format!("{id}: new, with no baseline"));
            }
            (Some(&base), None) => {
                verdict
                    .lines
                    .push(format!("{id:width$}  {base:>13}  {:>13}  gone", "-"));
                verdict
                    .failures
                    .push(format!("{id}: in the baseline but no longer a benchmark"));
            }
            (None, None) => {}
        }
    }
    verdict
}

fn print_verdict(verdict: &Verdict) {
    println!("\n{}", verdict.lines.join("\n"));
    if !verdict.faster.is_empty() {
        println!(
            "\nFaster by more than {THRESHOLD_PERCENT} %, so worth a new baseline to keep the gate tight:\n  {}",
            verdict.faster.join("\n  ")
        );
    }
}

/// Counts every benchmark and compares the counts with the baseline.
fn gate(root: &Path) -> Result<(), String> {
    let baseline = read_counts(&root.join(BASELINE));
    let counts = count_all(root)?;
    let text = write_counts(root, &counts)?;
    let baseline = match baseline {
        Ok(baseline) => baseline,
        Err(e) => {
            println!("\n--- instruction counts ---\n{text}\n--- end ---");
            return Err(format!(
                "{e}\nMake one: save the counts above as a file and run `cargo xtask bench \
                 --update-baseline --reason \"...\" --from <file>`."
            ));
        }
    };
    let verdict = compare(&baseline, &counts);
    print_verdict(&verdict);
    // Printed whatever the verdict, for `--update-baseline --from`.
    println!("\n--- instruction counts ---\n{text}\n--- end ---");
    if verdict.failures.is_empty() {
        println!("\nNo benchmark is more than {THRESHOLD_PERCENT} % slower than {BASELINE}.");
        return Ok(());
    }
    Err(format!(
        "the benchmark gate failed:\n  {}\n\nIf this is intended, update the baseline and say why \
         in the pull request: `cargo xtask bench --update-baseline --reason \"...\"` on x86-64 \
         Linux, or anywhere with the counts printed above saved as a file and passed with `--from \
         <file>`. Commit {BASELINE}.",
        verdict.failures.join("\n  ")
    ))
}

/// Writes a new baseline from counts made here or read from a file.
fn update_baseline(root: &Path, options: &Options) -> Result<(), String> {
    let counts = match &options.from {
        Some(path) => read_counts(path)?,
        None => count_all(root)?,
    };
    if let Ok(previous) = read_counts(&root.join(BASELINE)) {
        print_verdict(&compare(&previous, &counts));
    }
    let mut doc = counts_document(&counts, options.reason.as_deref());
    if options.from.is_some() {
        // The counts' own Valgrind, not this machine's.
        let from = std::fs::read_to_string(options.from.as_deref().unwrap_or(Path::new("")))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        doc["valgrind"] = from.map_or(Value::Null, |f| f["valgrind"].clone());
    }
    let path = root.join(BASELINE);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    eprintln!(
        "\n{} written with {} benchmarks. Commit it, and say why the counts changed in the pull \
         request.",
        BASELINE,
        counts.len()
    );
    Ok(())
}

/// The machine timings ran on: no host name, only what identifies the
/// hardware and system.
fn machine(label: Option<String>) -> Value {
    let label = label.or_else(|| std::env::var("LSPK_BENCH_MACHINE").ok());
    let (model, cpu, os) = if cfg!(target_os = "macos") {
        let sysctl = |key: &str| first_line(Command::new("sysctl").args(["-n", key]));
        let product = first_line(Command::new("sw_vers").arg("-productVersion"));
        let build = first_line(Command::new("sw_vers").arg("-buildVersion"));
        (
            sysctl("hw.model"),
            sysctl("machdep.cpu.brand_string"),
            product.map(|p| format!("macOS {p} ({})", build.unwrap_or_default())),
        )
    } else if cfg!(target_os = "linux") {
        let read = |path: &str| std::fs::read_to_string(path).ok();
        let field = |text: Option<String>, key: &str| {
            text?.lines().find_map(|l| {
                let (k, v) = l.split_once(['=', ':'])?;
                (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
            })
        };
        (
            read("/sys/devices/virtual/dmi/id/product_name").map(|s| s.trim().to_owned()),
            field(read("/proc/cpuinfo"), "model name"),
            field(read("/etc/os-release"), "PRETTY_NAME"),
        )
    } else {
        (
            None,
            std::env::var("PROCESSOR_IDENTIFIER").ok(),
            first_line(Command::new("cmd").args(["/C", "ver"])),
        )
    };
    json!({
        "label": label,
        "model": model,
        "cpu": cpu,
        "cores": std::thread::available_parallelism().map_or(0, usize::from),
        "arch": std::env::consts::ARCH,
        "os": os,
        "simd": lspk_dsp::simd::level().name(),
    })
}

/// The commit checked out, and whether tracked files have changed since.
fn commit(root: &Path) -> (String, bool) {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    (commit, dirty)
}

/// The first non-empty line a command prints, if it runs.
fn first_line(command: &mut Command) -> Option<String> {
    let output = command.output().ok().filter(|o| o.status.success())?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

fn target_dir(root: &Path) -> Result<PathBuf, String> {
    super::cargo_metadata(root)?["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "cargo metadata gave no target directory".to_owned())
}

/// Now in UTC, as `2026-10-12T14:03:09Z`.
fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// The calendar date `days` days after 1970-01-01 (Howard Hinnant's
/// algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(entries: &[(&str, u64)]) -> BTreeMap<String, u64> {
        entries
            .iter()
            .map(|(id, n)| ((*id).to_owned(), *n))
            .collect()
    }

    #[test]
    fn three_percent_slower_passes_and_more_fails() {
        let baseline = counts(&[("a", 100_000), ("b", 100_000)]);
        let passing = compare(&baseline, &counts(&[("a", 103_000), ("b", 90_000)]));
        assert!(passing.failures.is_empty(), "{passing:?}");
        assert_eq!(passing.faster.len(), 1);
        let failing = compare(&baseline, &counts(&[("a", 103_001), ("b", 100_000)]));
        assert_eq!(failing.failures.len(), 1, "{failing:?}");
        assert!(failing.failures[0].starts_with("a: +3.00 %"), "{failing:?}");
    }

    #[test]
    fn new_and_vanished_benchmarks_fail_until_the_baseline_has_them() {
        let verdict = compare(&counts(&[("old", 10)]), &counts(&[("new", 10)]));
        assert_eq!(
            verdict.failures,
            [
                "new: new, with no baseline",
                "old: in the baseline but no longer a benchmark"
            ]
        );
    }

    #[test]
    fn callgrind_totals_are_read() {
        let profile = "events: Ir\nfn=(1) main\n3 4\nsummary: 900\ntotals: 1234\n";
        assert_eq!(totals(profile), Some(1234));
        assert_eq!(totals("events: Ir\nsummary: 77\n"), Some(77));
        assert_eq!(totals("events: Ir\n"), None);
    }

    #[test]
    fn options_need_a_reason_to_update_the_baseline() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(parse(&args(&["--update-baseline"])).is_err());
        assert!(parse(&args(&["--update-baseline", "--reason", " "])).is_err());
        let options = parse(&args(&[
            "--update-baseline",
            "--reason",
            "faster oscillator",
        ]))
        .expect("valid");
        assert!(options.update);
        assert!(parse(&args(&["--gate", "--count"])).is_err());
        assert!(parse(&args(&["--reason", "x"])).is_err());
        assert!(parse(&args(&["--quick", "--machine", "m5-max"])).is_ok());
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_738), (2026, 10, 12));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }
}
