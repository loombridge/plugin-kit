//! Benchmarks: the performance harness every kit plug-in, and the kit's own
//! hot paths, measure themselves with.
//!
//! A crate keeps its benchmarks in `benches/perf.rs`, a bench target with
//! `harness = false` whose `main` builds a [`Suite`] and hands it to
//! [`Suite::main`]. A suite holds two kinds of benchmark:
//!
//! - **Scenarios** ([`Scenario`]): a plug-in playing a [`Script`] of notes,
//!   parameter changes and input at a sample rate, block size and voice
//!   count. Each scenario reports four measurements, each its own benchmark
//!   ID (`<suite>/<scenario>/<what>`):
//!   - `process`: CPU per block while the script plays (mean, median, 99th
//!     percentile and worst), per voice, and as a share of one core;
//!   - `idle`: CPU per block once every note and tail has ended, and
//!     whether the plug-in reported sleep, in which case a host doesn't call
//!     it at all and the idle cost is nothing;
//!   - `create`: making an instance and activating it;
//!   - `load-state`: loading a saved state into it.
//! - **Micro-benchmarks** ([`Suite::micro`]): one routine, such as a
//!   kernel over a block, timed per call (`<suite>/<name>`). Short routines
//!   are timed in batches of calls lasting at least 20 µs, so the 99th
//!   percentile and worst are of batch means.
//!
//! ```ignore
//! use lspk_test::bench::{Scenario, Suite};
//! use lspk_test::{Script, Settings};
//!
//! lspk_rt::install_trap!();
//!
//! fn main() -> std::process::ExitCode {
//!     let chord = (60..68).fold(Script::new(48_000), |s, key| s.note(0, 36_000, key, 0.8));
//!     let mut suite = Suite::new("my-synth");
//!     suite.scenario(Scenario::new::<MySynth>("chord", chord, Settings::new(48_000.0, 128)).voices(8));
//!     suite.main()
//! }
//! ```
//!
//! # Running them
//!
//! `cargo xtask bench` builds every crate's `perf` target in release and
//! times each benchmark on this machine, writing the results, with the
//! machine, CPU, OS and commit, as JSON under `target/bench/`. `cargo
//! bench` runs a crate's suite directly and prints a table.
//!
//! Timing varies too much on shared machines to gate a pull request on, so
//! CI counts instructions instead: `cargo xtask bench --gate` runs each
//! benchmark once under Valgrind's Callgrind, counting only what runs inside
//! [`measured`] (for `process` and `idle`, only the plug-in's `process`
//! calls, not the harness around them), and compares the counts with the
//! committed baseline.
//!
//! # The binary's interface
//!
//! [`Suite::main`] reads its arguments: `list` prints every benchmark ID;
//! `time [--quick]` times them all and prints the results as JSON;
//! `count <id>` runs one benchmark's measured region once, for Callgrind.
//! Anything else (as `cargo bench` passes `--bench`) times them all and
//! prints a table.

use std::io::Write as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lspk_plugin::{Plugin, ProcessStatus};
use serde::{Deserialize, Serialize};

use crate::cpu::{Timings, time};
use crate::render::{Instance, Render, Script, Settings};

/// The symbol of [`measured`], which Callgrind's `--toggle-collect` names.
pub const MEASURED_SYMBOL: &str = "lspk_bench_measured";

/// The longest a scenario plays silence after its script, waiting for the
/// plug-in to sleep, before its idle blocks are measured anyway.
const SETTLE_SECONDS: f64 = 10.0;

/// The shortest stretch a micro-benchmark's calls are timed over.
const MIN_BATCH: Duration = Duration::from_micros(20);

/// Whether the render harness runs each `process` call inside [`measured`].
static COUNT_PROCESS: AtomicBool = AtomicBool::new(false);

/// Whether the render harness should count `process` calls.
pub(crate) fn counting_process() -> bool {
    COUNT_PROCESS.load(Ordering::Relaxed)
}

/// Runs `f`: the region an instruction count covers. Callgrind starts
/// counting when this is entered and stops when it returns
/// (`--collect-atstart=no --toggle-collect=lspk_bench_measured`); outside a
/// count it's an ordinary call. It never allocates.
#[unsafe(export_name = "lspk_bench_measured")]
#[inline(never)]
pub fn measured(f: &mut dyn FnMut()) {
    f();
}

/// What a measurement measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A scenario's blocks while its script plays.
    Process,
    /// A scenario's blocks after every note and tail has ended.
    Idle,
    /// Making and activating an instance.
    Create,
    /// Loading a state into an instance.
    LoadState,
    /// One call of a micro-benchmark's routine.
    Micro,
}

impl Kind {
    /// Its name in benchmark IDs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Idle => "idle",
            Self::Create => "create",
            Self::LoadState => "load-state",
            Self::Micro => "micro",
        }
    }
}

/// One benchmark's timings, summarised. Times are in nanoseconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    /// The benchmark's ID: `<suite>/<scenario>/<what>` or `<suite>/<name>`.
    pub id: String,
    /// What it measures.
    pub kind: Kind,
    /// How many timings it summarises (blocks, calls or instances).
    pub samples: usize,
    /// The mean.
    pub mean_ns: f64,
    /// The median.
    pub median_ns: f64,
    /// The 99th percentile.
    pub p99_ns: f64,
    /// The slowest.
    pub worst_ns: f64,
    /// A scenario's sample rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f64>,
    /// A scenario's block size, in frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<usize>,
    /// How many voices a scenario sounds at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voices: Option<usize>,
    /// The mean per voice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_voice_ns: Option<f64>,
    /// Processing time over audio time: the share of one core it needs to
    /// keep up in real time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<f64>,
    /// The slowest block's time over its own deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worst_deadline: Option<f64>,
    /// Whether the plug-in reported sleep through every idle block, so a
    /// host wouldn't have called it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleeps: Option<bool>,
    /// How many items (samples, frames) a micro-benchmark's call handles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<usize>,
    /// The mean per item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_item_ns: Option<f64>,
}

impl Measurement {
    /// `timings` summarised as benchmark `id`.
    #[must_use]
    pub fn new(id: String, kind: Kind, timings: &Timings) -> Self {
        let ns = |d: Duration| d.as_secs_f64() * 1e9;
        Self {
            id,
            kind,
            samples: timings.len(),
            mean_ns: ns(timings.mean()),
            median_ns: ns(timings.median()),
            p99_ns: ns(timings.percentile(0.99)),
            worst_ns: ns(timings.worst()),
            sample_rate: None,
            block: None,
            voices: None,
            per_voice_ns: None,
            load: None,
            worst_deadline: None,
            sleeps: None,
            items: None,
            per_item_ns: None,
        }
    }
}

/// How long to time each benchmark for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Keep timing until this much time has passed...
    pub time: Duration,
    /// ...and at least this many runs (scenarios) or calls (micro-benchmarks)
    /// have been timed.
    pub min_runs: usize,
}

impl Budget {
    /// About a second per benchmark, for results worth keeping.
    pub const FULL: Self = Self {
        time: Duration::from_secs(1),
        min_runs: 5,
    };
    /// A tenth of that, to check the benchmarks run.
    pub const QUICK: Self = Self {
        time: Duration::from_millis(100),
        min_runs: 2,
    };
}

/// A plug-in instance a scenario drives, whatever its type.
trait Driven {
    fn render(&mut self, script: &Script) -> Render;
    fn silent_block(&mut self, frames: usize) -> (ProcessStatus, Duration);
    fn save_state(&self) -> Vec<u8>;
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String>;
}

impl<P: Plugin> Driven for Instance<P> {
    fn render(&mut self, script: &Script) -> Render {
        Instance::render(self, script)
    }

    fn silent_block(&mut self, frames: usize) -> (ProcessStatus, Duration) {
        let block = self.process(frames, &[], &[]);
        (block.status, block.duration)
    }

    fn save_state(&self) -> Vec<u8> {
        Instance::save_state(self)
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        Instance::load_state(self, bytes)
    }
}

/// Makes and activates an instance of `P`.
fn make<P: Plugin>(settings: Settings) -> Result<Box<dyn Driven>, String> {
    let mut instance = Instance::<P>::new()?;
    instance.activate(settings)?;
    Ok(Box::new(instance))
}

/// A plug-in playing a script: the benchmark a plug-in's CPU is judged by.
pub struct Scenario {
    name: String,
    script: Script,
    settings: Settings,
    voices: usize,
    state: Option<Vec<u8>>,
    idle_blocks: Option<usize>,
    make: fn(Settings) -> Result<Box<dyn Driven>, String>,
}

impl Scenario {
    /// Plug-in `P` playing `script` at `settings`, from its default state,
    /// with no voices counted.
    #[must_use]
    pub fn new<P: Plugin>(name: &str, script: Script, settings: Settings) -> Self {
        Self {
            name: name.to_owned(),
            script,
            settings,
            voices: 0,
            state: None,
            idle_blocks: None,
            make: make::<P>,
        }
    }

    /// With `voices` voices sounding at once, for the CPU per voice.
    #[must_use]
    pub fn voices(mut self, voices: usize) -> Self {
        self.voices = voices;
        self
    }

    /// Loading `state` (as [`Instance::save_state`] makes it) before it
    /// plays, instead of the plug-in's default state. `load-state` measures
    /// loading it.
    #[must_use]
    pub fn state(mut self, state: Vec<u8>) -> Self {
        self.state = Some(state);
        self
    }

    /// Measuring `blocks` idle blocks instead of a second's worth.
    #[must_use]
    pub fn idle_blocks(mut self, blocks: usize) -> Self {
        self.idle_blocks = Some(blocks);
        self
    }

    fn idle_block_count(&self) -> usize {
        self.idle_blocks.unwrap_or_else(|| {
            (self.settings.sample_rate / self.settings.block as f64).ceil() as usize
        })
    }

    /// A new instance with the scenario's state loaded.
    fn prepared(&self) -> Result<Box<dyn Driven>, String> {
        let mut instance = (self.make)(self.settings)?;
        let state = self.state.clone().unwrap_or_else(|| instance.save_state());
        instance.load_state(&state)?;
        Ok(instance)
    }

    /// Plays silence until the plug-in sleeps, for at most
    /// [`SETTLE_SECONDS`].
    fn settle(&self, instance: &mut dyn Driven) {
        let limit = (SETTLE_SECONDS * self.settings.sample_rate) as usize / self.settings.block;
        for _ in 0..limit {
            if instance.silent_block(self.settings.block).0 == ProcessStatus::Sleep {
                return;
            }
        }
    }

    /// One timed run: make, load, play, settle, idle.
    fn run(&self, runs: &mut Runs) -> Result<(), String> {
        let (instance, took) = time(|| (self.make)(self.settings));
        let mut instance = instance?;
        runs.create.push(1, took);
        let state = self.state.clone().unwrap_or_else(|| instance.save_state());
        let (loaded, took) = time(|| instance.load_state(&state));
        loaded?;
        runs.load.push(1, took);
        let render = instance.render(&self.script);
        runs.process.blocks.extend(render.timings.blocks);
        self.settle(instance.as_mut());
        for _ in 0..self.idle_block_count() {
            let (status, took) = instance.silent_block(self.settings.block);
            runs.sleeps &= status == ProcessStatus::Sleep;
            runs.idle.push(self.settings.block, took);
        }
        Ok(())
    }

    fn time(&self, suite: &str, budget: Budget) -> Result<Vec<Measurement>, String> {
        let rate = self.settings.sample_rate;
        // A warm-up run, not kept.
        self.run(&mut Runs::new(rate))?;
        let mut runs = Runs::new(rate);
        let start = Instant::now();
        let mut count = 0;
        while count < budget.min_runs || start.elapsed() < budget.time {
            self.run(&mut runs)?;
            count += 1;
        }
        let id = |kind: Kind| format!("{suite}/{}/{}", self.name, kind.name());
        let scenario = |mut m: Measurement| {
            m.sample_rate = Some(rate);
            m.block = Some(self.settings.block);
            m
        };
        let mut process = scenario(Measurement::new(
            id(Kind::Process),
            Kind::Process,
            &runs.process,
        ));
        process.load = Some(runs.process.load());
        process.worst_deadline = Some(runs.process.worst_deadline_fraction());
        if self.voices > 0 {
            process.voices = Some(self.voices);
            process.per_voice_ns = Some(process.mean_ns / self.voices as f64);
        }
        let mut idle = scenario(Measurement::new(id(Kind::Idle), Kind::Idle, &runs.idle));
        idle.load = Some(runs.idle.load());
        idle.sleeps = Some(runs.sleeps);
        Ok(vec![
            process,
            idle,
            scenario(Measurement::new(
                id(Kind::Create),
                Kind::Create,
                &runs.create,
            )),
            scenario(Measurement::new(
                id(Kind::LoadState),
                Kind::LoadState,
                &runs.load,
            )),
        ])
    }

    /// Runs `kind`'s measured region once.
    fn count(&self, kind: Kind) -> Result<(), String> {
        match kind {
            Kind::Create => {
                let mut made = None;
                measured(&mut || made = Some((self.make)(self.settings)));
                made.unwrap_or_else(|| Err("not made".to_owned())).map(drop)
            }
            Kind::LoadState => {
                let mut instance = (self.make)(self.settings)?;
                let state = self.state.clone().unwrap_or_else(|| instance.save_state());
                let mut loaded = Ok(());
                measured(&mut || loaded = instance.load_state(&state));
                loaded
            }
            Kind::Process => {
                let mut instance = self.prepared()?;
                COUNT_PROCESS.store(true, Ordering::Relaxed);
                instance.render(&self.script);
                COUNT_PROCESS.store(false, Ordering::Relaxed);
                Ok(())
            }
            Kind::Idle => {
                let mut instance = self.prepared()?;
                instance.render(&self.script);
                self.settle(instance.as_mut());
                COUNT_PROCESS.store(true, Ordering::Relaxed);
                for _ in 0..self.idle_block_count() {
                    instance.silent_block(self.settings.block);
                }
                COUNT_PROCESS.store(false, Ordering::Relaxed);
                Ok(())
            }
            Kind::Micro => Err("a scenario isn't a micro-benchmark".to_owned()),
        }
    }
}

/// A scenario's timings across its runs.
struct Runs {
    process: Timings,
    idle: Timings,
    create: Timings,
    load: Timings,
    sleeps: bool,
}

impl Runs {
    fn new(rate: f64) -> Self {
        Self {
            process: Timings::new(rate),
            idle: Timings::new(rate),
            create: Timings::new(0.0),
            load: Timings::new(0.0),
            sleeps: true,
        }
    }
}

/// A routine timed per call.
struct Micro {
    name: String,
    items: usize,
    setup: Box<dyn Fn() -> Box<dyn FnMut()>>,
}

impl Micro {
    fn time(&self, suite: &str, budget: Budget) -> Measurement {
        let mut routine = (self.setup)();
        // Warm up for a tenth of the budget.
        let warm = Instant::now();
        while warm.elapsed() < budget.time / 10 {
            routine();
        }
        // Calls are timed in batches of at least MIN_BATCH, so the clock's
        // resolution doesn't swamp a short routine; each timing is a
        // batch's mean.
        let mut batch = 1u32;
        while batch < 1 << 20 && time(|| (0..batch).for_each(|_| routine())).1 < MIN_BATCH {
            batch *= 2;
        }
        let mut timings = Timings::new(0.0);
        let start = Instant::now();
        while timings.len() < budget.min_runs.max(10) || start.elapsed() < budget.time {
            let ((), took) = time(|| (0..batch).for_each(|_| routine()));
            timings.push(self.items, took / batch);
        }
        let mut m = Measurement::new(format!("{suite}/{}", self.name), Kind::Micro, &timings);
        m.items = Some(self.items);
        m.per_item_ns = Some(m.mean_ns / self.items.max(1) as f64);
        m
    }

    fn count(&self) {
        let mut routine = (self.setup)();
        // Once outside the count, so lazy set-up (detecting the SIMD level,
        // say) doesn't count.
        routine();
        measured(&mut routine);
    }
}

/// A crate's benchmarks.
pub struct Suite {
    name: String,
    scenarios: Vec<Scenario>,
    micros: Vec<Micro>,
}

impl Suite {
    /// An empty suite named `name`, by convention the crate's name.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            scenarios: Vec::new(),
            micros: Vec::new(),
        }
    }

    /// Adds a scenario.
    pub fn scenario(&mut self, scenario: Scenario) -> &mut Self {
        self.scenarios.push(scenario);
        self
    }

    /// Adds a micro-benchmark: `setup` makes the routine (allocating what it
    /// needs), and each call of the routine is timed, or counted once. Each
    /// call handles `items` items (samples, say), for the time per item.
    /// The routine should pass its results to [`std::hint::black_box`] so
    /// the compiler can't drop the work.
    pub fn micro<R: FnMut() + 'static>(
        &mut self,
        name: &str,
        items: usize,
        setup: impl Fn() -> R + 'static,
    ) -> &mut Self {
        self.micros.push(Micro {
            name: name.to_owned(),
            items,
            setup: Box::new(move || Box::new(setup())),
        });
        self
    }

    /// Every benchmark ID, in order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let kinds = [Kind::Process, Kind::Idle, Kind::Create, Kind::LoadState];
        let scenarios = self.scenarios.iter().flat_map(|s| {
            kinds
                .iter()
                .map(move |kind| format!("{}/{}/{}", self.name, s.name, kind.name()))
        });
        let micros = self
            .micros
            .iter()
            .map(|m| format!("{}/{}", self.name, m.name));
        scenarios.chain(micros).collect()
    }

    /// Times every benchmark.
    ///
    /// # Errors
    ///
    /// A scenario's plug-in couldn't be made, activated or given its state.
    pub fn time(&self, budget: Budget) -> Result<Vec<Measurement>, String> {
        let mut out = Vec::new();
        for scenario in &self.scenarios {
            out.extend(
                scenario
                    .time(&self.name, budget)
                    .map_err(|e| format!("{}/{}: {e}", self.name, scenario.name))?,
            );
        }
        out.extend(self.micros.iter().map(|m| m.time(&self.name, budget)));
        Ok(out)
    }

    /// Runs benchmark `id`'s measured region once, for an instruction count.
    ///
    /// # Errors
    ///
    /// There's no benchmark `id`, or its plug-in failed.
    pub fn count(&self, id: &str) -> Result<(), String> {
        let rest = id
            .strip_prefix(&self.name)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(|| format!("no benchmark `{id}`"))?;
        if let Some(micro) = self.micros.iter().find(|m| m.name == rest) {
            micro.count();
            return Ok(());
        }
        for scenario in &self.scenarios {
            if let Some(kind) = rest
                .strip_prefix(&scenario.name)
                .and_then(|r| r.strip_prefix('/'))
            {
                let kind = [Kind::Process, Kind::Idle, Kind::Create, Kind::LoadState]
                    .into_iter()
                    .find(|k| k.name() == kind)
                    .ok_or_else(|| format!("no benchmark `{id}`"))?;
                return scenario.count(kind);
            }
        }
        Err(format!("no benchmark `{id}`"))
    }

    /// Runs as the bench binary's `main`, as the module documentation
    /// describes.
    #[must_use]
    pub fn main(self) -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let result = match args.as_slice() {
            ["list"] => {
                println!("{}", self.ids().join("\n"));
                Ok(())
            }
            ["count", id] => self.count(id),
            ["time", rest @ ..] => {
                let budget = if rest.contains(&"--quick") {
                    Budget::QUICK
                } else {
                    Budget::FULL
                };
                self.time(budget)
                    .and_then(|measurements| {
                        serde_json::to_string(&measurements).map_err(|e| e.to_string())
                    })
                    .map(|json| println!("{json}"))
            }
            _ => self.time(Budget::FULL).map(|measurements| {
                print!("{}", table(&measurements));
            }),
        };
        let _ = std::io::stdout().flush();
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::FAILURE
            }
        }
    }
}

/// `measurements` as a table for people: each benchmark's mean, 99th
/// percentile and worst, and what else it reports.
#[must_use]
pub fn table(measurements: &[Measurement]) -> String {
    let width = measurements
        .iter()
        .map(|m| m.id.len())
        .max()
        .unwrap_or(0)
        .max("benchmark".len());
    let mut out = format!(
        "{:width$}  {:>10}  {:>10}  {:>10}  notes\n",
        "benchmark", "mean", "p99", "worst"
    );
    for m in measurements {
        let mut notes = Vec::new();
        if let (Some(voices), Some(per_voice)) = (m.voices, m.per_voice_ns) {
            let plural = if voices == 1 { "" } else { "s" };
            notes.push(format!(
                "{} per voice ({voices} voice{plural})",
                duration(per_voice)
            ));
        }
        if let (Kind::Process, Some(load)) = (m.kind, m.load) {
            notes.push(format!("{:.3} % of a core", load * 100.0));
        }
        if let Some(sleeps) = m.sleeps {
            notes.push(
                if sleeps {
                    "asleep: costs a host nothing"
                } else {
                    "never sleeps"
                }
                .to_owned(),
            );
        }
        if let Some(per_item) = m.per_item_ns {
            notes.push(format!("{per_item:.2} ns per item"));
        }
        out.push_str(&format!(
            "{:width$}  {:>10}  {:>10}  {:>10}  {}\n",
            m.id,
            duration(m.mean_ns),
            duration(m.p99_ns),
            duration(m.worst_ns),
            notes.join("; ")
        ));
    }
    out
}

/// `ns` nanoseconds in a readable unit.
fn duration(ns: f64) -> String {
    if ns >= 1e6 {
        format!("{:.2} ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.2} µs", ns / 1e3)
    } else {
        format!("{ns:.0} ns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_in_their_unit() {
        assert_eq!(duration(512.0), "512 ns");
        assert_eq!(duration(1_500.0), "1.50 µs");
        assert_eq!(duration(2_250_000.0), "2.25 ms");
    }

    #[test]
    fn micro_benchmarks_are_timed_and_counted() {
        let mut suite = Suite::new("suite");
        suite.micro("sum/64", 64, || {
            let data = vec![1.0f32; 64];
            move || {
                std::hint::black_box(data.iter().sum::<f32>());
            }
        });
        assert_eq!(suite.ids(), ["suite/sum/64"]);
        let measured = suite.time(Budget::QUICK).expect("timed");
        assert_eq!(measured.len(), 1);
        assert!(measured[0].samples >= 10);
        assert_eq!(measured[0].items, Some(64));
        assert!(suite.count("suite/sum/64").is_ok());
        assert!(suite.count("suite/sum/65").is_err());
        assert!(suite.count("other/sum/64").is_err());
        assert!(table(&measured).contains("suite/sum/64"));
    }
}
