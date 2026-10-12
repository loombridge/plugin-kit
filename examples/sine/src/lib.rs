//! Example Sine: a polyphonic sine instrument, the smallest instrument
//! built on the plug-in kit. It proves the kit's CLAP export in tests, and
//! is the seed of the instrument template `cargo xtask new-plugin` will
//! generate.
//!
//! Eight voices, each a sine at its key's pitch (A4, key 69, at 440 Hz,
//! equal-tempered) shaped by an attack, decay, sustain and release
//! envelope and scaled by its velocity. When every voice is taken, a new
//! note takes the oldest. One parameter, [`LEVEL`], from −∞ dB to 0 dB,
//! glides to each new value over 20 ms. The instrument sleeps while no
//! voice sounds.
//!
//! It's exported with the agent interface ([`lspk_plugin_agent::WithAgent`]),
//! so an agent can drive it in any host once the person switches its agent
//! server on.

use std::f64::consts::TAU;

use lspk_params::smooth::DEFAULT_SMOOTHING_SECONDS;
use lspk_params::{LinearSmoother, ParamDescriptor, ParamId, gain_to_db};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, Note, NotePorts, ParamValues, Plugin, PluginError,
    PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor, features,
};

/// The plug-in's ID.
pub const ID: &str = "com.loombridge.example.sine";
/// The plug-in's name.
pub const NAME: &str = "Example Sine";
/// The level: a linear factor from 0 (−∞ dB) to 0 dB, −6 dB by default.
pub const LEVEL: ParamId = ParamId(1);
/// How many notes can sound at once.
pub const VOICES: usize = 8;
/// Each voice's peak amplitude at full velocity and level, so eight at once
/// stay below full scale.
pub const VOICE_PEAK: f32 = 0.125;

/// The envelope's attack, decay and release times, in seconds, and its
/// sustain level.
const ATTACK: f64 = 0.005;
const DECAY: f64 = 0.05;
const SUSTAIN: f32 = 0.7;
const RELEASE: f64 = 0.1;

/// The parameters.
#[must_use]
pub fn params() -> Vec<ParamDescriptor> {
    vec![
        ParamDescriptor::gain(LEVEL, "Level", 0.0, -6.0)
            .describe("How loud the instrument plays, from silence to full level."),
    ]
}

/// A key's pitch in hertz.
#[must_use]
pub fn frequency(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// The plug-in on the main thread.
#[derive(Debug)]
pub struct Sine {
    host: Host,
}

impl Plugin for Sine {
    type Processor = SineProcessor;

    const INFO: PluginInfo = PluginInfo::new(ID, NAME)
        .vendor("Loombridge")
        .version(env!("CARGO_PKG_VERSION"))
        .url("https://github.com/loombridge/plugin-kit")
        .description("A polyphonic sine: the plug-in kit's example instrument.")
        .features(&[
            features::INSTRUMENT,
            features::SYNTHESIZER,
            features::STEREO,
        ])
        .audio(AudioLayout::instrument(2))
        .notes(NotePorts::INPUT);

    fn new(host: Host) -> Self {
        Self { host }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        params()
    }

    fn summary(&self) -> Option<String> {
        let level = self.host.param(LEVEL)?;
        Some(format!(
            "A polyphonic sine instrument: up to {VOICES} notes, each a pure sine at its key's \
             equal-tempered pitch (A4 = 440 Hz) with a {:.0} ms attack, {:.0} ms decay to {:.0} % \
             sustain and {:.0} ms release, scaled by velocity, at a level of {}.",
            ATTACK * 1000.0,
            DECAY * 1000.0,
            SUSTAIN * 100.0,
            RELEASE * 1000.0,
            if level > 0.0 {
                format!("{:.2} dB", gain_to_db(level))
            } else {
                "-inf dB (silence)".to_owned()
            }
        ))
    }

    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<SineProcessor, PluginError> {
        let rate = config.sample_rate;
        let frames = LinearSmoother::frames_for(DEFAULT_SMOOTHING_SECONDS, rate);
        let per_frame = |seconds: f64| (1.0 / (seconds * rate).max(1.0)) as f32;
        Ok(SineProcessor {
            rate,
            level: LinearSmoother::new(values.get(LEVEL).unwrap_or(0.5), frames),
            voices: [Voice::IDLE; VOICES],
            attack: per_frame(ATTACK),
            decay: per_frame(DECAY) * (1.0 - SUSTAIN),
            release: per_frame(RELEASE),
            started: 0,
        })
    }
}

/// Where a voice's envelope is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// One sounding note.
#[derive(Clone, Copy, Debug)]
struct Voice {
    note: Note,
    stage: Stage,
    /// The envelope's level, from 0 to 1.
    envelope: f32,
    /// How fast the release falls, per frame.
    release: f32,
    velocity: f32,
    /// The oscillator's phase, in cycles, and its step per frame.
    phase: f64,
    step: f64,
    /// When it started, by note count, for taking the oldest.
    started: u64,
}

impl Voice {
    const IDLE: Self = Self {
        note: Note {
            port: None,
            channel: None,
            key: None,
            id: None,
        },
        stage: Stage::Idle,
        envelope: 0.0,
        release: 0.0,
        velocity: 0.0,
        phase: 0.0,
        step: 0.0,
        started: 0,
    };

    fn is_sounding(&self) -> bool {
        self.stage != Stage::Idle
    }
}

/// The plug-in on the audio thread.
#[derive(Debug)]
pub struct SineProcessor {
    rate: f64,
    level: LinearSmoother,
    voices: [Voice; VOICES],
    /// Envelope steps per frame.
    attack: f32,
    decay: f32,
    release: f32,
    /// Notes started so far.
    started: u64,
}

impl SineProcessor {
    /// Renders frames `from..to` into the left channel and copies them to
    /// the right; reports each voice that ends to the host.
    fn render(
        &mut self,
        audio: &mut Buffers<'_>,
        from: usize,
        to: usize,
        cx: &mut ProcessContext<'_>,
    ) {
        if from >= to || audio.channels() == 0 {
            self.level.skip(to.saturating_sub(from));
            return;
        }
        let (attack, decay) = (self.attack, self.decay);
        let out = &mut audio.channel_mut(0)[from..to];
        // The voices that end here, reported in time order once all have
        // rendered.
        let mut ended = [(u32::MAX, Note::default()); VOICES];
        for (voice, ended) in self.voices.iter_mut().zip(&mut ended) {
            if !voice.is_sounding() {
                continue;
            }
            for (frame, sample) in out.iter_mut().enumerate() {
                match voice.stage {
                    Stage::Attack => {
                        voice.envelope += attack;
                        if voice.envelope >= 1.0 {
                            voice.envelope = 1.0;
                            voice.stage = Stage::Decay;
                        }
                    }
                    Stage::Decay => {
                        voice.envelope -= decay;
                        if voice.envelope <= SUSTAIN {
                            voice.envelope = SUSTAIN;
                            voice.stage = Stage::Sustain;
                        }
                    }
                    Stage::Sustain => {}
                    Stage::Release => {
                        voice.envelope -= voice.release;
                        if voice.envelope <= 0.0 {
                            voice.envelope = 0.0;
                            voice.stage = Stage::Idle;
                            *ended = ((from + frame) as u32, voice.note);
                            break;
                        }
                    }
                    Stage::Idle => break,
                }
                *sample +=
                    (voice.phase * TAU).sin() as f32 * voice.envelope * voice.velocity * VOICE_PEAK;
                voice.phase = (voice.phase + voice.step).fract();
            }
        }
        ended.sort_unstable_by_key(|(time, _)| *time);
        for &(time, note) in ended.iter().take_while(|(time, _)| *time != u32::MAX) {
            cx.note_end(time, note);
        }
        for sample in out.iter_mut() {
            *sample *= self.level.step() as f32;
        }
        if let Some((left, right)) = audio.stereo_mut() {
            right[from..to].copy_from_slice(&left[from..to]);
        }
    }

    fn note_on(&mut self, note: Note, velocity: f32, time: u32, cx: &mut ProcessContext<'_>) {
        let Some(key) = note.key else { return };
        self.started += 1;
        let index = self
            .voices
            .iter()
            .position(|v| !v.is_sounding())
            .unwrap_or_else(|| {
                // Every voice is taken: the oldest gives way.
                let oldest = (0..VOICES)
                    .min_by_key(|&i| self.voices[i].started)
                    .unwrap_or(0);
                cx.note_end(time, self.voices[oldest].note);
                oldest
            });
        let voice = &mut self.voices[index];
        *voice = Voice {
            note,
            stage: Stage::Attack,
            // A voice taken over rises from where it was, without a click.
            envelope: if voice.is_sounding() {
                voice.envelope
            } else {
                0.0
            },
            release: 0.0,
            velocity,
            phase: if voice.is_sounding() {
                voice.phase
            } else {
                0.0
            },
            step: frequency(key) / self.rate,
            started: self.started,
        };
    }

    fn note_off(&mut self, note: &Note) {
        let release = self.release;
        for voice in &mut self.voices {
            if voice.is_sounding() && voice.stage != Stage::Release && note.matches(&voice.note) {
                voice.stage = Stage::Release;
                // The release takes its full time from wherever it starts.
                voice.release = voice.envelope * release;
            }
        }
    }

    fn choke(&mut self, note: &Note, time: u32, cx: &mut ProcessContext<'_>) {
        for voice in &mut self.voices {
            if voice.is_sounding() && note.matches(&voice.note) {
                voice.stage = Stage::Idle;
                voice.envelope = 0.0;
                cx.note_end(time, voice.note);
            }
        }
    }
}

impl Processor for SineProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        let frames = audio.frames();
        let mut done = 0;
        for event in events.iter() {
            let at = (event.time() as usize).clamp(done, frames);
            self.render(audio, done, at, cx);
            done = at;
            match event {
                Event::NoteOn { note, velocity, .. } => self.note_on(note, velocity, at as u32, cx),
                Event::NoteOff { note, .. } => self.note_off(&note),
                Event::NoteChoke { note, .. } => self.choke(&note, at as u32, cx),
                Event::Param {
                    id: LEVEL, value, ..
                } => self.level.set_target(value),
                _ => {}
            }
        }
        self.render(audio, done, frames, cx);
        if self.voices.iter().any(Voice::is_sounding) || !self.level.is_settled() {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }

    fn flush(&mut self, events: &Events<'_>, _cx: &mut ProcessContext<'_>) {
        for event in events.iter() {
            if let Event::Param {
                id: LEVEL, value, ..
            } = event
            {
                self.level.snap(value);
            }
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; VOICES];
        self.level.snap(self.level.target());
    }
}

lspk_plugin::export_clap!(lspk_plugin_agent::WithAgent<Sine>);
