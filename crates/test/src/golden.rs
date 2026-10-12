//! Golden renders and null tests: a plug-in's renders compared with
//! reference renders kept in its repository.
//!
//! A [`Suite`] holds a folder of references and the renders to compare
//! with them ([`Suite::add`]); [`Suite::assert`] null-tests each render
//! ([`null_test`]):
//!
//! - The reference is a 24-bit FLAC file, `<name>.flac`. The render is
//!   rounded to 24 bits ([`to_i24`]) and subtracted from it sample by
//!   sample. The residual is the largest difference, in dBFS.
//! - `manifest.json` records, for each reference, the platform that made it
//!   (operating system and architecture), its format, and a hash of the
//!   32-bit float render it was made from ([`render_hash`]).
//! - On the platform that made the reference, the render must be bit-exact:
//!   every 24-bit sample the same, and the float render the same to the
//!   bit, which the hash checks.
//! - Elsewhere, the residual must be below [`THRESHOLD_DBFS`], −96 dBFS,
//!   because platforms' maths libraries may round the last bit
//!   differently.
//!
//! A render also fails if a sample isn't finite.
//!
//! # Changing a reference
//!
//! References change only when someone runs `cargo xtask golden --update`,
//! never in an ordinary test run, and the pull request that commits a
//! changed reference says why. The task runs the workspace's `golden` test
//! targets with [`UPDATE_ENV`] set, which tells each suite to remake the
//! references whose renders changed (or only those named) and record this
//! platform as the one that made them, then to null-test as usual.
//!
//! # Where references live
//!
//! Each plug-in crate keeps its suite in `tests/golden.rs` and its
//! references beside it in `tests/golden/`; give a suite
//! `concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden")`. One suite owns a
//! folder: two suites updating the same manifest at once would race.
//! Test audio is made by code ([`crate::signal`]); no reference is ever
//! made from recorded or third-party audio.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::measure::db;
use crate::render::Output;

/// Bits per sample in a reference.
pub const BITS: u32 = 24;

/// What a render's residual must stay below on a platform other than the
/// one that made its reference.
pub const THRESHOLD_DBFS: f64 = -96.0;

/// The manifest's file name.
pub const MANIFEST: &str = "manifest.json";

/// The environment variable `cargo xtask golden --update` sets: `*` to
/// update every reference, or the names to update, separated by commas.
pub const UPDATE_ENV: &str = "LSPK_GOLDEN_UPDATE";

/// What the manifest says about itself.
const ABOUT: &str = "Golden references: each render in this crate's golden suite stored as a \
    24-bit FLAC file. render_hash is an FNV-1a hash of the 32-bit float render the reference was \
    made from. The tests null-test every render against its reference: bit-exact on the platform \
    in made_on, below -96 dBFS elsewhere. Change references only with `cargo xtask golden \
    --update`, and say why in the pull request.";

/// A folder of references and the renders to compare with them.
#[derive(Debug)]
pub struct Suite {
    dir: PathBuf,
    renders: Vec<(String, Output)>,
}

impl Suite {
    /// The references in `dir`.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            renders: Vec::new(),
        }
    }

    /// Adds the render `name`, to compare with `<name>.flac`.
    ///
    /// # Panics
    ///
    /// If the suite already has a render `name`, or `name` isn't lower-case
    /// letters, digits and hyphens.
    pub fn add(&mut self, name: &str, output: Output) -> &mut Self {
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "a golden render's name is lower-case letters, digits and hyphens, not {name:?}"
        );
        assert!(
            self.renders.iter().all(|(n, _)| n != name),
            "two golden renders are called {name}"
        );
        self.renders.push((name.to_owned(), output));
        self
    }

    /// The renders' names, in the order they were added.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.renders.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Updates references if [`UPDATE_ENV`] asks, then null-tests every
    /// render against its reference.
    ///
    /// # Errors
    ///
    /// A reference or the manifest couldn't be read or written.
    pub fn run(&self) -> Result<Vec<Null>, String> {
        if let Some(selection) = update_selection() {
            for (name, change) in self.update(&selection)? {
                println!("{name}: {change}");
            }
        }
        self.check()
    }

    /// Null-tests every render against its reference, without updating.
    ///
    /// # Errors
    ///
    /// The manifest couldn't be read.
    pub fn check(&self) -> Result<Vec<Null>, String> {
        let manifest = Manifest::load(&self.dir)?;
        Ok(self
            .renders
            .iter()
            .map(|(name, output)| self.check_one(name, output, &manifest))
            .collect())
    }

    /// [`run`](Self::run)s the suite and panics, listing every failure, if
    /// any render failed its null test.
    ///
    /// # Panics
    ///
    /// If a render failed, or a file couldn't be read or written.
    pub fn assert(&self) {
        let nulls = self.run().unwrap_or_else(|e| panic!("{e}"));
        let failed: Vec<String> = nulls
            .iter()
            .inspect(|null| println!("{null}"))
            .filter(|null| !null.passed())
            .map(ToString::to_string)
            .collect();
        assert!(
            failed.is_empty(),
            "{}\n\nIf the change is intended, remake the references with `cargo xtask golden \
             --update` and say why in the pull request.",
            failed.join("\n")
        );
    }

    fn check_one(&self, name: &str, output: &Output, manifest: &Manifest) -> Null {
        let path = self.dir.join(format!("{name}.flac"));
        match (manifest.references.get(name), read_flac(&path)) {
            (Some(entry), Ok(reference)) => null_test(name, output, &reference, entry),
            (entry, reference) => {
                let mut problems = render_problems(output);
                if entry.is_none() {
                    problems.push(format!("{MANIFEST} has no entry for it"));
                }
                if let Err(error) = reference {
                    problems.push(error);
                }
                problems.push("make its reference with `cargo xtask golden --update`".to_owned());
                Null {
                    name: name.to_owned(),
                    made_on: entry.map_or_else(String::new, |e| e.made_on.clone()),
                    platform: platform(),
                    peak_dbfs: db(crate::measure::peak(&output.interleaved())),
                    problems,
                    residual: 0.0,
                    worst: None,
                    differing: 0,
                    same_hash: false,
                }
            }
        }
    }

    /// Makes or remakes the references `selection` names, where their
    /// renders changed, and records this platform as the one that made
    /// them. With every render selected, it also removes references that
    /// belong to none.
    ///
    /// # Errors
    ///
    /// A render can't be a reference (a sample isn't finite, or its sample
    /// rate isn't a whole number), or a file couldn't be read or written.
    pub fn update(&self, selection: &Selection) -> Result<Vec<(String, Change)>, String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("can't create {}: {e}", self.dir.display()))?;
        let mut manifest = Manifest::load(&self.dir)?;
        manifest.about = ABOUT.to_owned();
        let mut changes = Vec::new();
        for (name, output) in &self.renders {
            if !selection.includes(name) {
                continue;
            }
            let problems = render_problems(output);
            if !problems.is_empty() {
                return Err(format!(
                    "{name} can't be a reference: {}",
                    problems.join("; ")
                ));
            }
            let path = self.dir.join(format!("{name}.flac"));
            let entry = Entry::for_output(output)?;
            let quantised = quantise(output);
            let same_samples = read_flac(&path).is_ok_and(|reference| {
                reference.sample_rate == entry.sample_rate
                    && reference.channels == entry.channels
                    && reference.bits == BITS
                    && reference.samples == quantised
            });
            // A render that matches its reference to the bit leaves it
            // alone, even on another platform.
            let matches = |old: &Entry| {
                Entry {
                    made_on: entry.made_on.clone(),
                    ..old.clone()
                } == entry
            };
            let change = match manifest.references.get(name) {
                Some(old) if same_samples && matches(old) => Change::Unchanged,
                Some(_) if same_samples => Change::Hash,
                _ => {
                    write_flac(&path, output)?;
                    Change::Written
                }
            };
            if change != Change::Unchanged {
                manifest.references.insert(name.clone(), entry);
            }
            changes.push((name.clone(), change));
        }
        if *selection == Selection::All {
            let unknown: Vec<String> = manifest
                .references
                .keys()
                .filter(|name| self.renders.iter().all(|(n, _)| n != *name))
                .cloned()
                .collect();
            for name in unknown {
                manifest.references.remove(&name);
                let _ = fs::remove_file(self.dir.join(format!("{name}.flac")));
                changes.push((name, Change::Removed));
            }
        }
        manifest.save(&self.dir)?;
        Ok(changes)
    }
}

/// Which references to update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Every one, removing those no render has.
    All,
    /// Only these.
    Named(Vec<String>),
}

impl Selection {
    /// Whether it includes `name`.
    #[must_use]
    pub fn includes(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Named(names) => names.iter().any(|n| n == name),
        }
    }

    /// Reads [`UPDATE_ENV`]'s value: `*` (or `1`, or nothing) for every
    /// reference, or names separated by commas.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let names: Vec<String> = value
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect();
        if names.is_empty() || names.iter().any(|n| n == "*" || n == "1") {
            Self::All
        } else {
            Self::Named(names)
        }
    }
}

/// What [`UPDATE_ENV`] asks for, if it's set.
fn update_selection() -> Option<Selection> {
    std::env::var(UPDATE_ENV).ok().map(|v| Selection::parse(&v))
}

/// What an update did to one reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The render matches the reference to the bit: nothing to do.
    Unchanged,
    /// The 24-bit samples match, but the float render differs below them:
    /// only the manifest's hash (and platform) changed.
    Hash,
    /// The reference was made or remade.
    Written,
    /// The reference belonged to no render and was removed.
    Removed,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unchanged => "unchanged",
            Self::Hash => "same 24-bit samples; the manifest's render_hash and made_on updated",
            Self::Written => "reference written",
            Self::Removed => "removed: no render has this name",
        })
    }
}

/// Why a render can't be compared or kept.
fn render_problems(output: &Output) -> Vec<String> {
    let mut problems = Vec::new();
    let bad = output
        .channels
        .iter()
        .flatten()
        .filter(|s| !s.is_finite())
        .count();
    if bad > 0 {
        problems.push(format!("{bad} samples are NaN or infinite"));
    }
    if output.channels.iter().any(|c| c.len() != output.frames()) {
        problems.push("its channels aren't all the same length".to_owned());
    }
    problems
}

/// `sample` rounded to the nearest 24-bit step, without dither, and clamped
/// to the 24-bit range.
#[must_use]
pub fn to_i24(sample: f32) -> i32 {
    const SCALE: f64 = 8_388_608.0;
    // Scaling by a power of two is exact, so this is the one rounding.
    (f64::from(sample) * SCALE)
        .round()
        .clamp(-SCALE, SCALE - 1.0) as i32
}

/// Every sample of `output`, interleaved, rounded to 24 bits.
#[must_use]
pub fn quantise(output: &Output) -> Vec<i32> {
    output.interleaved().into_iter().map(to_i24).collect()
}

/// An FNV-1a hash of `output`'s 32-bit float samples, interleaved, as the
/// manifest records it: `fnv1a64:` and 16 hexadecimal digits.
#[must_use]
pub fn render_hash(output: &Output) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for sample in output.interleaved() {
        for byte in sample.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("fnv1a64:{hash:016x}")
}

/// A reference read from its FLAC file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// Frames per second.
    pub sample_rate: u32,
    /// How many channels.
    pub channels: usize,
    /// Bits per sample.
    pub bits: u32,
    /// The samples, interleaved.
    pub samples: Vec<i32>,
}

/// Reads the reference at `path`.
///
/// # Errors
///
/// It can't be read, or isn't FLAC.
pub fn read_flac(path: &Path) -> Result<Reference, String> {
    let failed = |error: claxon::Error| format!("can't read {}: {error}", path.display());
    let mut reader = claxon::FlacReader::open(path).map_err(failed)?;
    let info = reader.streaminfo();
    let samples = reader
        .samples()
        .collect::<Result<Vec<i32>, _>>()
        .map_err(failed)?;
    Ok(Reference {
        sample_rate: info.sample_rate,
        channels: info.channels as usize,
        bits: info.bits_per_sample,
        samples,
    })
}

/// Writes `output`, rounded to 24 bits, to `path` as FLAC, and reads it
/// back to check.
///
/// # Errors
///
/// Its sample rate isn't a whole number, it can't be encoded or written,
/// or it doesn't read back as written.
pub fn write_flac(path: &Path, output: &Output) -> Result<(), String> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let sample_rate = whole_rate(output.sample_rate)?;
    let samples = quantise(output);
    let channels = output.channels.len();
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| format!("the FLAC encoder's settings are invalid: {e:?}"))?;
    let source = flacenc::source::MemSource::from_samples(
        &samples,
        channels,
        BITS as usize,
        sample_rate as usize,
    );
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("can't encode {}: {e:?}", path.display()))?;
    // A fixed-blocksize stream's minimum block size leaves out the short
    // last block; the encoder counts it, which makes the stream look
    // variable-blocksize to decoders that check.
    stream
        .stream_info_mut()
        .set_block_sizes(config.block_size, config.block_size)
        .map_err(|e| format!("can't encode {}: {e:?}", path.display()))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("can't encode {}: {e:?}", path.display()))?;
    fs::write(path, sink.as_slice()).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    let expected = Reference {
        sample_rate,
        channels,
        bits: BITS,
        samples,
    };
    if read_flac(path)? != expected {
        return Err(format!(
            "{} doesn't read back as the samples written to it",
            path.display()
        ));
    }
    Ok(())
}

/// `rate` as the whole number FLAC stores.
fn whole_rate(rate: f64) -> Result<u32, String> {
    if rate.fract() == 0.0 && rate >= 1.0 && rate <= f64::from(u32::MAX) {
        Ok(rate as u32)
    } else {
        Err(format!(
            "a reference's sample rate must be a whole number of hertz, not {rate}"
        ))
    }
}

/// The manifest: what each reference is and where it was made.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// What the manifest is.
    pub about: String,
    /// Each reference by name.
    pub references: BTreeMap<String, Entry>,
}

/// One reference in the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The platform that made it ([`platform`]).
    pub made_on: String,
    /// Frames per second.
    pub sample_rate: u32,
    /// How many channels.
    pub channels: usize,
    /// How many frames.
    pub frames: usize,
    /// Bits per sample.
    pub bits: u32,
    /// The float render's hash ([`render_hash`]).
    pub render_hash: String,
}

impl Entry {
    /// The entry for `output`, made on this platform.
    ///
    /// # Errors
    ///
    /// Its sample rate isn't a whole number.
    pub fn for_output(output: &Output) -> Result<Self, String> {
        Ok(Self {
            made_on: platform(),
            sample_rate: whole_rate(output.sample_rate)?,
            channels: output.channels.len(),
            frames: output.frames(),
            bits: BITS,
            render_hash: render_hash(output),
        })
    }
}

impl Manifest {
    /// The manifest in `dir`, or an empty one if there's none.
    ///
    /// # Errors
    ///
    /// It can't be read or parsed.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join(MANIFEST);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("can't read {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                about: ABOUT.to_owned(),
                references: BTreeMap::new(),
            }),
            Err(e) => Err(format!("can't read {}: {e}", path.display())),
        }
    }

    /// Writes it to `dir`.
    ///
    /// # Errors
    ///
    /// It can't be written.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let path = dir.join(MANIFEST);
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("can't write {}: {e}", path.display()))?;
        json.push('\n');
        fs::write(&path, json).map_err(|e| format!("can't write {}: {e}", path.display()))
    }
}

/// This platform, as the manifest names it: operating system and
/// architecture, such as `macos-aarch64`.
#[must_use]
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// One render's null test against its reference.
#[derive(Clone, Debug)]
pub struct Null {
    /// The render's name.
    pub name: String,
    /// The platform that made the reference.
    pub made_on: String,
    /// The platform rendering now.
    pub platform: String,
    /// The render's peak, in dBFS.
    pub peak_dbfs: f64,
    /// Why it can't be compared, or doesn't match the reference's format.
    pub problems: Vec<String>,
    /// The largest difference between a render sample and its reference
    /// sample, at 24 bits, as a linear amplitude.
    pub residual: f64,
    /// Where the largest difference is: frame and channel.
    pub worst: Option<(usize, usize)>,
    /// How many 24-bit samples differ.
    pub differing: usize,
    /// Whether the float render hashes to the manifest's hash.
    pub same_hash: bool,
}

impl Null {
    /// Whether this platform made the reference, so the render must match
    /// it to the bit.
    #[must_use]
    pub fn exact_required(&self) -> bool {
        self.made_on == self.platform
    }

    /// Whether the render matches the reference to the bit.
    #[must_use]
    pub fn bit_exact(&self) -> bool {
        self.problems.is_empty() && self.differing == 0 && self.same_hash
    }

    /// The residual, in dBFS (−∞ if no sample differs).
    #[must_use]
    pub fn residual_dbfs(&self) -> f64 {
        db(self.residual)
    }

    /// Whether the render passed: bit-exact on the reference's platform,
    /// below [`THRESHOLD_DBFS`] elsewhere, with no problems.
    #[must_use]
    pub fn passed(&self) -> bool {
        if !self.problems.is_empty() {
            return false;
        }
        if self.exact_required() {
            self.bit_exact()
        } else {
            self.residual_dbfs() < THRESHOLD_DBFS
        }
    }

    fn worst(&self) -> String {
        match self.worst {
            Some((frame, channel)) => format!(" at frame {frame}, channel {channel}"),
            None => String::new(),
        }
    }
}

impl fmt::Display for Null {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.name)?;
        if !self.problems.is_empty() {
            return write!(f, "FAILED: {}", self.problems.join("; "));
        }
        let differ = match self.differing {
            1 => "1 sample differs".to_owned(),
            n => format!("{n} samples differ"),
        };
        let places = format!(
            "reference made on {}, rendered on {}",
            self.made_on, self.platform
        );
        match (self.passed(), self.exact_required()) {
            (true, _) if self.bit_exact() => write!(f, "bit-exact ({places})"),
            (true, _) if self.differing == 0 => write!(
                f,
                "every 24-bit sample matches, though the 32-bit float render differs below them ({places})"
            ),
            (true, _) => write!(
                f,
                "residual {:.1} dBFS{} ({differ}), below {THRESHOLD_DBFS} dBFS ({places})",
                self.residual_dbfs(),
                self.worst()
            ),
            (false, true) if self.differing == 0 => write!(
                f,
                "FAILED: not bit-exact on {}, which made the reference: every 24-bit sample matches, but the 32-bit float render doesn't hash to the manifest's render_hash",
                self.made_on
            ),
            (false, true) => write!(
                f,
                "FAILED: not bit-exact on {}, which made the reference: {differ}, the largest by {:.1} dBFS{}",
                self.made_on,
                self.residual_dbfs(),
                self.worst()
            ),
            (false, false) => write!(
                f,
                "FAILED: residual {:.1} dBFS{} ({differ}), not below {THRESHOLD_DBFS} dBFS ({places})",
                self.residual_dbfs(),
                self.worst()
            ),
        }
    }
}

/// Null-tests `output`, the render `name`, against `reference`, which
/// `entry` describes.
#[must_use]
pub fn null_test(name: &str, output: &Output, reference: &Reference, entry: &Entry) -> Null {
    let mut null = Null {
        name: name.to_owned(),
        made_on: entry.made_on.clone(),
        platform: platform(),
        peak_dbfs: db(crate::measure::peak(&output.interleaved())),
        problems: render_problems(output),
        residual: 0.0,
        worst: None,
        differing: 0,
        same_hash: render_hash(output) == entry.render_hash,
    };
    let format = |rate: u32, channels: usize, bits: u32| {
        format!("{rate} Hz, {channels} channels, {bits} bits")
    };
    let rendered = match whole_rate(output.sample_rate) {
        Ok(rate) => format(rate, output.channels.len(), BITS),
        Err(e) => e,
    };
    let stored = format(reference.sample_rate, reference.channels, reference.bits);
    let recorded = format(entry.sample_rate, entry.channels, entry.bits);
    if stored != recorded {
        null.problems.push(format!(
            "the reference is {stored}, but the manifest says {recorded}"
        ));
    }
    if rendered != stored {
        null.problems.push(format!(
            "the render is {rendered}, but the reference is {stored}"
        ));
    }
    let channels = reference.channels.max(1);
    let stored_frames = reference.samples.len() / channels;
    if stored_frames != entry.frames {
        null.problems.push(format!(
            "the reference has {stored_frames} frames, but the manifest says {}",
            entry.frames
        ));
    }
    if output.frames() != stored_frames {
        null.problems.push(format!(
            "the render has {} frames, but the reference has {stored_frames}",
            output.frames()
        ));
    }
    let mut largest = 0i64;
    for (i, (&ours, &theirs)) in quantise(output).iter().zip(&reference.samples).enumerate() {
        let difference = (i64::from(ours) - i64::from(theirs)).abs();
        if difference != 0 {
            null.differing += 1;
            if difference > largest {
                largest = difference;
                null.worst = Some((i / channels, i % channels));
            }
        }
    }
    null.residual = largest as f64 / 8_388_608.0;
    null
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::sine;

    /// A folder in the system's temporary folder, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("lspk-test-golden-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a temporary folder");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tone() -> Output {
        let left = sine(440.0, 48_000.0, 4_800, 0.5);
        let right = sine(660.0, 48_000.0, 4_800, 0.25);
        Output::new(48_000.0, vec![left, right])
    }

    #[test]
    fn quantising_rounds_to_the_nearest_step_and_clamps() {
        assert_eq!(to_i24(0.0), 0);
        assert_eq!(to_i24(1.0), 8_388_607);
        assert_eq!(to_i24(-1.0), -8_388_608);
        assert_eq!(to_i24(2.0), 8_388_607);
        assert_eq!(to_i24(0.5 / 8_388_608.0), 1);
    }

    #[test]
    fn selections_parse_from_the_environment_variable() {
        assert_eq!(Selection::parse("*"), Selection::All);
        assert_eq!(Selection::parse("1"), Selection::All);
        assert_eq!(Selection::parse(""), Selection::All);
        let named = Selection::parse("a, b");
        assert_eq!(
            named,
            Selection::Named(vec!["a".to_owned(), "b".to_owned()])
        );
        assert!(named.includes("b") && !named.includes("c"));
    }

    #[test]
    fn a_reference_round_trips_and_nulls_bit_exactly() {
        let dir = TempDir::new("round-trip");
        let mut suite = Suite::new(&dir.0);
        suite.add("tone", tone());
        // No reference yet: it fails and says how to make one.
        let nulls = suite.check().expect("the suite runs");
        assert!(!nulls[0].passed());
        assert!(nulls[0].to_string().contains("cargo xtask golden --update"));

        let changes = suite.update(&Selection::All).expect("an update");
        assert_eq!(changes, [("tone".to_owned(), Change::Written)]);
        let nulls = suite.check().expect("the suite runs");
        assert!(nulls[0].passed() && nulls[0].bit_exact(), "{}", nulls[0]);
        let manifest = Manifest::load(&dir.0).expect("a manifest");
        let entry = &manifest.references["tone"];
        assert_eq!((entry.channels, entry.frames, entry.bits), (2, 4_800, 24));
        assert_eq!(entry.made_on, platform());

        // A second update changes nothing.
        let changes = suite.update(&Selection::All).expect("an update");
        assert_eq!(changes, [("tone".to_owned(), Change::Unchanged)]);
    }

    #[test]
    fn a_changed_render_fails_here_and_passes_elsewhere_if_small() {
        let dir = TempDir::new("changed");
        let mut suite = Suite::new(&dir.0);
        suite.add("tone", tone());
        suite.update(&Selection::All).expect("an update");
        let manifest = Manifest::load(&dir.0).expect("a manifest");
        let reference = read_flac(&dir.0.join("tone.flac")).expect("a reference");
        let entry = manifest.references["tone"].clone();

        // One sample a few 24-bit steps off: about −122 dBFS.
        let mut nudged = tone();
        nudged.channels[1][100] += 6.0 / 8_388_608.0;
        let null = null_test("tone", &nudged, &reference, &entry);
        assert!(!null.passed(), "bit-exactness is required here");
        assert_eq!((null.differing, null.worst), (1, Some((100, 1))));
        assert!(null.to_string().contains("not bit-exact"), "{null}");

        // On another platform the same residual is within the threshold.
        let elsewhere = Entry {
            made_on: "elsewhere-cpu".to_owned(),
            ..entry.clone()
        };
        let null = null_test("tone", &nudged, &reference, &elsewhere);
        assert!(null.passed(), "{null}");
        assert!(null.residual_dbfs() < -110.0);

        // A large change fails everywhere.
        let mut broken = tone();
        broken.channels[0][10] = 0.9;
        assert!(!null_test("tone", &broken, &reference, &elsewhere).passed());

        // So does a change of shape.
        let short = tone().slice(0, 100);
        let null = null_test("tone", &short, &reference, &elsewhere);
        assert!(null.problems.iter().any(|p| p.contains("frames")), "{null}");
    }

    #[test]
    fn updates_only_what_was_selected_and_prunes_on_a_full_update() {
        let dir = TempDir::new("select");
        let mut first = Suite::new(&dir.0);
        first.add("a", tone()).add("b", tone().slice(0, 10));
        first.update(&Selection::All).expect("an update");

        let mut changed = Suite::new(&dir.0);
        changed.add("a", tone().slice(0, 20)).add("b", tone());
        let changes = changed
            .update(&Selection::Named(vec!["a".to_owned()]))
            .expect("an update");
        assert_eq!(changes, [("a".to_owned(), Change::Written)]);
        let nulls = changed.check().expect("the suite runs");
        assert!(nulls[0].passed() && !nulls[1].passed());

        let mut fewer = Suite::new(&dir.0);
        fewer.add("a", tone().slice(0, 20));
        let changes = fewer.update(&Selection::All).expect("an update");
        assert!(changes.contains(&("b".to_owned(), Change::Removed)));
        assert!(!dir.0.join("b.flac").exists());
    }

    #[test]
    fn unfit_renders_are_refused() {
        let dir = TempDir::new("unfit");
        let mut suite = Suite::new(&dir.0);
        suite.add("nan", Output::new(48_000.0, vec![vec![f32::NAN]]));
        assert!(suite.update(&Selection::All).unwrap_err().contains("NaN"));
        let mut suite = Suite::new(&dir.0);
        suite.add("odd-rate", Output::new(44_100.5, vec![vec![0.0]]));
        assert!(
            suite
                .update(&Selection::All)
                .unwrap_err()
                .contains("whole number")
        );
    }

    #[test]
    #[should_panic(expected = "lower-case")]
    fn names_are_checked() {
        Suite::new("x").add("Not OK", tone());
    }
}
