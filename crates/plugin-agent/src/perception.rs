//! Perception: hearing the plug-in as it's set now, without touching the
//! instance the host plays.
//!
//! `preview` renders notes (an instrument) or a test signal (an effect)
//! through a separate instance of the plug-in, made for the render with the
//! live instance's parameter values and own state, offline on a thread of
//! its own, so neither the host's audio thread nor its main thread waits.
//! The audio comes back as a WAV file, with an analysis as numbers and as
//! prose from `lspk-test`'s measurements: loudness, pitch, the spectrum in
//! octave bands and its centroid, and the envelope's timing.

use std::fmt;

use lspk_agent::Media;
use lspk_params::ParamId;
use lspk_plugin::Plugin;
use lspk_test::measure;
use lspk_test::render::{Instance, Output, Script, Settings};
use lspk_test::signal;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// The longest preview, in seconds.
pub const MAX_SECONDS: f64 = 10.0;
/// How many slices of a sound the spectrum's movement is read in.
const MOVEMENT_SLICES: usize = 4;
/// The shortest slice, in samples, that has a spectrum worth a centroid.
const MIN_SLICE: usize = 2_048;
/// The block size previews render in.
const BLOCK: usize = 512;

/// Arguments of `preview`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewArgs {
    /// The notes to play, for an instrument: one for a note, several at the
    /// same start for a chord, or a short phrase. Left out, an instrument
    /// plays middle C (key 60) for a second.
    #[serde(default)]
    pub notes: Vec<NoteArg>,
    /// What to feed an effect. Left out, an effect hears a 440 Hz sine at
    /// −12 dBFS.
    #[serde(default)]
    pub input: Option<Input>,
    /// How long the preview lasts, in seconds, up to 10. Left out: a second
    /// after the last note ends, or 2 seconds.
    #[serde(default)]
    pub seconds: Option<f64>,
    /// The sample rate, in hertz, from 22,050 to 192,000; 48,000 if left
    /// out.
    #[serde(default)]
    pub sample_rate: Option<u32>,
}

/// One note of a preview.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteArg {
    /// The key, from 0 to 127, where 60 is middle C and 69 is A4 (440 Hz).
    #[schemars(range(max = 127))]
    pub key: u8,
    /// When it starts, in seconds from the start of the preview.
    #[serde(default)]
    pub start: f64,
    /// How long it's held, in seconds.
    #[serde(default = "one")]
    pub length: f64,
    /// How hard it's played, from 0 to 1.
    #[serde(default = "velocity")]
    pub velocity: f32,
}

fn one() -> f64 {
    1.0
}

fn velocity() -> f32 {
    0.8
}

/// A test signal an effect hears.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    /// A sine.
    Sine {
        /// Its frequency, in hertz.
        frequency: f64,
        /// Its peak level, in dBFS.
        #[serde(default = "minus_twelve")]
        level_db: f64,
    },
    /// A sine sweeping from one frequency to another over the preview.
    Sweep {
        /// Where it starts, in hertz.
        from: f64,
        /// Where it ends, in hertz.
        to: f64,
        /// Its peak level, in dBFS.
        #[serde(default = "minus_twelve")]
        level_db: f64,
    },
    /// White noise.
    Noise {
        /// Its peak level, in dBFS.
        #[serde(default = "minus_twelve")]
        level_db: f64,
    },
    /// One full-scale sample at the start, then silence: the effect's
    /// impulse response.
    Impulse,
    /// Silence: what the effect makes of nothing.
    Silence,
}

fn minus_twelve() -> f64 {
    -12.0
}

/// A render waiting for its thread: the live instance's values and own
/// state, and what to play.
#[derive(Clone)]
pub struct Job {
    values: Vec<(ParamId, f64)>,
    own: Vec<u8>,
    script: Script,
    sample_rate: f64,
    /// When each note ends, in seconds, latest last.
    last_note_end: Option<f64>,
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("frames", &self.script.frames())
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}

impl Job {
    /// A render of `args` through plug-in `P` set as the live instance is:
    /// `values` and `own` state.
    ///
    /// # Errors
    ///
    /// The arguments ask for something out of range.
    pub fn new<P: Plugin>(
        args: &PreviewArgs,
        values: Vec<(ParamId, f64)>,
        own: Vec<u8>,
    ) -> Result<Self, String> {
        let rate = f64::from(args.sample_rate.unwrap_or(48_000));
        if !(22_050.0..=192_000.0).contains(&rate) {
            return Err(format!(
                "the sample rate must be from 22,050 to 192,000 Hz, not {rate}"
            ));
        }
        let instrument = P::INFO.notes.input && P::INFO.audio.inputs == 0;
        let mut notes = args.notes.clone();
        if notes.is_empty() && P::INFO.notes.input && (instrument || args.input.is_none()) {
            notes.push(NoteArg {
                key: 60,
                start: 0.0,
                length: 1.0,
                velocity: velocity(),
            });
        }
        for note in &notes {
            if note.key > 127
                || !(0.0..=MAX_SECONDS).contains(&note.start)
                || !(0.0..=MAX_SECONDS).contains(&note.length)
                || !(0.0..=1.0).contains(&note.velocity)
            {
                return Err(format!(
                    "a note's key is 0 to 127, its start and length 0 to {MAX_SECONDS} seconds, \
                     and its velocity 0 to 1: {note:?}"
                ));
            }
        }
        if !P::INFO.notes.input && !notes.is_empty() {
            return Err(format!(
                "{} takes no notes; give it an `input` to hear instead",
                P::INFO.name
            ));
        }
        let last_note_end = notes
            .iter()
            .map(|n| n.start + n.length)
            .fold(None, |m: Option<f64>, e| Some(m.map_or(e, |m| m.max(e))));
        let seconds = args
            .seconds
            .unwrap_or_else(|| last_note_end.map_or(2.0, |end| end + 1.0));
        if !(0.05..=MAX_SECONDS).contains(&seconds) {
            return Err(format!(
                "a preview lasts from 0.05 to {MAX_SECONDS} seconds, not {seconds}"
            ));
        }
        let frames = (seconds * rate).round() as usize;
        let frame = |s: f64| (s * rate).round().min(frames as f64) as u32;
        let mut script = Script::new(frames);
        for note in &notes {
            let start = frame(note.start);
            let length = frame(note.start + note.length).saturating_sub(start).max(1);
            script = script.note(start, length, note.key, note.velocity);
        }
        if P::INFO.audio.inputs > 0 {
            let level = |db: f64| 10f64.powf(db.min(0.0) / 20.0);
            let input = match args.input.clone().unwrap_or(Input::Sine {
                frequency: 440.0,
                level_db: minus_twelve(),
            }) {
                Input::Sine {
                    frequency,
                    level_db,
                } => signal::sine(frequency, rate, frames, level(level_db)),
                Input::Sweep { from, to, level_db } => {
                    signal::sweep(from, to, rate, frames, level(level_db))
                }
                Input::Noise { level_db } => signal::noise(1, frames, level(level_db)),
                Input::Impulse => signal::impulse(frames, 0, 1.0),
                Input::Silence => signal::silence(frames),
            };
            script = script.with_mono_input(input);
        } else if args.input.is_some() {
            return Err(format!(
                "{} takes no audio in; give it `notes` instead",
                P::INFO.name
            ));
        }
        Ok(Self {
            values,
            own,
            script,
            sample_rate: rate,
            last_note_end,
        })
    }
}

/// Renders `job` through a new instance of `P`, and analyses it: the result
/// as JSON and the audio as a WAV file. Runs on the preview's own thread.
///
/// # Errors
///
/// The plug-in couldn't be made, take the state, or activate.
pub fn render<P: Plugin>(job: &Job) -> Result<(Value, Vec<Media>), String> {
    let mut instance = Instance::<P>::new()?;
    for &(id, value) in &job.values {
        instance.values().set(id, value);
    }
    instance
        .plugin_mut()
        .load_state(&job.own)
        .map_err(|e| format!("the preview instance refused the state: {e}"))?;
    instance.activate(Settings::new(job.sample_rate, BLOCK))?;
    let output = instance.render(&job.script).output;
    instance.deactivate();
    let wav = lspk_test::agent::encode_wav(&output)?;
    Ok((
        analyse(&output, job.last_note_end),
        vec![Media {
            mime_type: lspk_test::agent::WAV,
            bytes: wav,
            uri: None,
        }],
    ))
}

/// A frequency's nearest note name and how far from it, in cents.
fn note_name(hz: f64) -> (String, f64) {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let key = 69.0 + 12.0 * (hz / 440.0).log2();
    let nearest = key.round();
    let index = (nearest as i64).rem_euclid(12) as usize;
    let octave = (nearest as i64).div_euclid(12) - 1;
    (format!("{}{octave}", NAMES[index]), (key - nearest) * 100.0)
}

/// A number of decibels for JSON: `null` for −∞.
fn decibels(db: f64) -> Value {
    if db.is_finite() {
        json!((db * 10.0).round() / 10.0)
    } else {
        Value::Null
    }
}

/// Decibels for prose.
fn db_text(db: f64) -> String {
    if db.is_finite() {
        format!("{db:.1} dBFS")
    } else {
        "silence".to_owned()
    }
}

/// The spectral centroid, in hertz, of each of [`MOVEMENT_SLICES`] equal
/// slices of the sound from `onset` to `end` (seconds): how the spectrum
/// moves. Empty if the sound is too short for its slices to have a
/// spectrum, or a slice is silent.
fn centroid_movement(mono: &[f32], rate: f64, onset: f64, end: f64) -> Vec<f64> {
    let from = (onset * rate) as usize;
    let to = ((end * rate) as usize).min(mono.len());
    let slice = to.saturating_sub(from) / MOVEMENT_SLICES;
    if slice < MIN_SLICE {
        return Vec::new();
    }
    (0..MOVEMENT_SLICES)
        .map(|i| measure::centroid(&mono[from + i * slice..from + (i + 1) * slice], rate))
        .collect::<Option<_>>()
        .unwrap_or_default()
}

/// How wide the stereo image is, and how alike the channels are, or `None`
/// for one channel: the share of the energy that is side (left minus
/// right) rather than mid, from 0 for mono to 1 for opposite channels, and
/// the channels' correlation from −1 to 1.
fn stereo_width(output: &Output) -> Option<(f64, f64)> {
    let [left, right, ..] = output.channels.as_slice() else {
        return None;
    };
    let (mut mid, mut side, mut ll, mut rr, mut lr) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (&l, &r) in left.iter().zip(right) {
        let (l, r) = (f64::from(l), f64::from(r));
        mid += (l + r) * (l + r) / 4.0;
        side += (l - r) * (l - r) / 4.0;
        (ll, rr, lr) = (ll + l * l, rr + r * r, lr + l * r);
    }
    let total = mid + side;
    let norm = (ll * rr).sqrt();
    // A silent channel correlates with nothing.
    (total > 0.0).then(|| (side / total, if norm > 0.0 { lr / norm } else { 0.0 }))
}

/// The analysis of a preview, as numbers and a summary in prose.
pub fn analyse(output: &Output, last_note_end: Option<f64>) -> Value {
    let rate = output.sample_rate;
    let mono = output.mono();
    let seconds = output.frames() as f64 / rate;
    let peak = measure::db(measure::peak(&output.interleaved()));
    let rms = measure::db(measure::rms(&mono));
    let envelope = measure::envelope(&mono, rate);
    let mut summary = vec![format!(
        "{seconds:.2} s at {} Hz, {}.",
        rate,
        match output.channels.len() {
            1 => "mono".to_owned(),
            2 => "stereo".to_owned(),
            n => format!("{n} channels"),
        }
    )];
    let Some(envelope) = envelope else {
        summary.push("It's silent.".to_owned());
        return json!({
            "seconds": seconds,
            "sample_rate": rate,
            "channels": output.channels.len(),
            "silent": true,
            "summary": summary.join(" "),
        });
    };
    summary.push(format!("Peak {}, RMS {}.", db_text(peak), db_text(rms)));
    // The pitch of the steady part: from the end of the attack, for up to a
    // second.
    let from = (envelope.peak * rate) as usize;
    let to = ((envelope.end.min(envelope.peak + 1.0)) * rate) as usize;
    let pitch = (to > from + 2_048)
        .then(|| measure::pitch(&mono[from..to.min(mono.len())], rate))
        .flatten();
    let pitch_json = match pitch {
        Some(hz) => {
            let (note, cents) = note_name(hz);
            summary.push(format!("Pitch {hz:.1} Hz ({note}, {cents:+.0} cents)."));
            json!({ "hz": (hz * 100.0).round() / 100.0, "note": note, "cents": cents.round() })
        }
        None => {
            summary.push("No single clear pitch.".to_owned());
            Value::Null
        }
    };
    let bands = measure::bands(&mono, rate);
    let centroid = measure::centroid(&mono, rate);
    if let Some(loudest) = bands
        .iter()
        .max_by(|a, b| a.level_db.total_cmp(&b.level_db))
    {
        summary.push(format!(
            "Spectral centroid {}; loudest octave band {} Hz ({}).",
            centroid.map_or("none".to_owned(), |c| format!("{c:.0} Hz")),
            loudest.centre,
            db_text(loudest.level_db)
        ));
    }
    let movement = centroid_movement(&mono, rate, envelope.onset, envelope.end);
    if let [first, .., last] = movement.as_slice() {
        summary.push(format!(
            "The spectrum {}: its centroid goes from {first:.0} Hz to {last:.0} Hz.",
            match last / first {
                r if r > 1.25 => "brightens",
                r if r < 0.8 => "darkens",
                _ => "stays about as bright",
            }
        ));
    }
    let stereo = stereo_width(output);
    summary.push(match stereo {
        Some((width, _)) if width < 0.01 => {
            "The two channels are identical (no stereo width).".to_owned()
        }
        Some((width, correlation)) => format!(
            "Stereo width {width:.2} (0 is mono, 1 is wide), channel correlation {correlation:.2}."
        ),
        None => "It's mono.".to_owned(),
    });
    let release = last_note_end
        .filter(|end| envelope.end > *end)
        .map(|end| envelope.end - end);
    summary.push(format!(
        "It starts at {:.3} s, reaches its peak at {:.3} s and dies away by {:.2} s{}.",
        envelope.onset,
        envelope.peak,
        envelope.end,
        release.map_or(String::new(), |r| format!(
            ", {r:.2} s after the last note ends"
        ))
    ));
    json!({
        "seconds": seconds,
        "sample_rate": rate,
        "channels": output.channels.len(),
        "silent": false,
        "loudness": { "peak_dbfs": decibels(peak), "rms_dbfs": decibels(rms) },
        "pitch": pitch_json,
        "spectrum": {
            "centroid_hz": centroid.map(f64::round),
            "octave_bands": bands
                .iter()
                .map(|b| json!({ "hz": b.centre, "dbfs": decibels(b.level_db) }))
                .collect::<Vec<_>>(),
            "centroid_over_time_hz": movement.iter().map(|c| c.round()).collect::<Vec<_>>(),
        },
        "stereo": stereo.map(|(width, correlation)| json!({
            "width": (width * 100.0).round() / 100.0,
            "correlation": (correlation * 100.0).round() / 100.0,
        })),
        "envelope": {
            "onset_s": envelope.onset,
            "attack_end_s": envelope.peak,
            "peak_dbfs": decibels(envelope.peak_db),
            "end_s": envelope.end,
            "release_s": release,
        },
        "summary": summary.join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_have_note_names() {
        assert_eq!(note_name(440.0), ("A4".to_owned(), 0.0));
        let (name, cents) = note_name(261.63);
        assert_eq!(name, "C4");
        assert!(cents.abs() < 1.0);
        assert_eq!(note_name(27.5).0, "A0");
    }

    #[test]
    fn stereo_width_and_the_spectrums_movement_are_read() {
        let rate = 48_000.0;
        // A sweep from low to high: the centroid rises through the sound.
        let sweep: Vec<f32> = (0..96_000)
            .map(|i| {
                let t = f64::from(i) / rate;
                (std::f64::consts::TAU * (200.0 * t + 1_900.0 * t * t)).sin() as f32 * 0.5
            })
            .collect();
        let opposite: Vec<f32> = sweep.iter().map(|s| -0.5 * s).collect();
        let mono = analyse(&Output::new(rate, vec![sweep.clone(), sweep.clone()]), None);
        assert_eq!(mono["stereo"]["width"], 0.0);
        assert_eq!(mono["stereo"]["correlation"], 1.0);
        let rising = mono["spectrum"]["centroid_over_time_hz"]
            .as_array()
            .unwrap();
        assert_eq!(rising.len(), MOVEMENT_SLICES);
        assert!(
            rising.windows(2).all(|w| w[0].as_f64() < w[1].as_f64()),
            "{rising:?}"
        );
        let text = mono["summary"].as_str().unwrap();
        assert!(text.contains("The spectrum brightens"), "{text}");
        assert!(text.contains("no stereo width"), "{text}");
        let wide = analyse(&Output::new(rate, vec![sweep.clone(), opposite]), None);
        assert_eq!(wide["stereo"]["width"], 0.9);
        assert_eq!(wide["stereo"]["correlation"], -1.0);
        let single = analyse(&Output::new(rate, vec![sweep]), None);
        assert!(single["stereo"].is_null());
        assert!(single["summary"].as_str().unwrap().contains("It's mono."));
    }

    #[test]
    fn a_sine_is_analysed_in_numbers_and_words() {
        let mut tone = signal::sine(440.0, 48_000.0, 48_000, 0.5);
        tone.extend(signal::silence(24_000));
        let output = Output::new(48_000.0, vec![tone.clone(), tone]);
        let analysis = analyse(&output, Some(1.0));
        assert_eq!(analysis["pitch"]["note"], "A4");
        assert!((analysis["loudness"]["peak_dbfs"].as_f64().unwrap() + 6.0).abs() < 0.1);
        let text = analysis["summary"].as_str().unwrap();
        assert!(text.contains("Pitch 440.0 Hz (A4"), "{text}");
        assert!(text.contains("loudest octave band 500 Hz"), "{text}");
        let silent = analyse(&Output::new(48_000.0, vec![vec![0.0; 4_800]]), None);
        assert_eq!(silent["silent"], true);
    }
}
