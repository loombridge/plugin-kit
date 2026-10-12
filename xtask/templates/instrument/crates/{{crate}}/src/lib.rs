//! {{Name}}: a polyphonic instrument built on the plug-in kit.
//!
//! Eight voices, each a sine at its key's pitch (A4, key 69, at 440 Hz,
//! equal-tempered), shaped by an attack and release envelope and scaled by
//! its velocity. When every voice is taken, a new note takes the oldest. The
//! instrument sleeps while no voice sounds.
//!
//! Its parameters live in a parameter space ([`space`]): a global
//! [`LEVEL`], which glides to each new value over 20 ms, and an envelope
//! module, `Env 1`, with [`ATTACK`] and [`RELEASE`]. Their IDs never change:
//! `tests/ids.txt` keeps them, and may only be added to. One command of its
//! own, [`RESET`], puts every parameter back to its default.
//!
//! It's exported with the agent interface (`lspk_plugin_agent::WithAgent`),
//! so an agent can drive it in any host once the person switches its agent
//! server on. Without the `export` feature (on by default) the crate is the
//! plug-in's types alone, for a host that compiles it in, as Beatlayer's
//! native devices do.
//!
//! This is the plug-in kit's instrument template: make the sound your own by
//! changing the voice, the parameters and the tests together.

use std::f64::consts::TAU;

use lspk_commands::{CommandSpec, Context, NoArgs, Permission};
use lspk_params::smooth::DEFAULT_SMOOTHING_SECONDS;
use lspk_params::{
    LinearSmoother, ModuleType, ParamAddress, ParamDescriptor, ParamId, ParamSpace, ParamUnit,
    gain_to_db,
};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, Note, NotePorts, ParamValues, Plugin, PluginError,
    PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor, RegisterError, Registry,
    features,
};

/// The plug-in's ID.
pub const ID: &str = "{{id}}";
/// The plug-in's name.
pub const NAME: &str = "{{Name}}";
/// The level: a linear factor from 0 (−∞ dB) to 0 dB, −6 dB by default.
pub const LEVEL: ParamId = ParamId(1);
/// The envelope module's type number.
pub const ENV: u8 = 1;
/// The envelope's attack, in milliseconds.
pub const ATTACK: ParamId = ParamAddress::new(ENV, 0, 0).id();
/// The envelope's release, in milliseconds.
pub const RELEASE: ParamId = ParamAddress::new(ENV, 0, 1).id();
/// The plug-in's own command: put every parameter back to its default.
pub const RESET: &str = "{{crate_lib}}.reset";
/// How many notes can sound at once.
pub const VOICES: usize = 8;
/// Each voice's peak amplitude at full velocity and level, so eight at once
/// stay below full scale.
pub const VOICE_PEAK: f32 = 0.125;

/// The parameter space: the level, then the envelope module.
///
/// # Panics
///
/// Never: the space is valid, as the tests check.
#[must_use]
pub fn space() -> ParamSpace {
    let time = |number: u32, name: &str, default: f64, text: &str| {
        ParamDescriptor::continuous(
            ParamId(number),
            name,
            (1.0, 5_000.0),
            default,
            ParamUnit::Milliseconds,
        )
        .logarithmic()
        .describe(text)
    };
    ParamSpace::builder()
        .global(
            ParamDescriptor::gain(LEVEL, "Level", 0.0, -6.0)
                .describe("How loud the instrument plays, from silence to full level."),
        )
        .module(
            ModuleType::new(ENV, "env", "Env")
                .param(time(
                    0,
                    "Attack",
                    5.0,
                    "How long a note takes to rise to its full level.",
                ))
                .param(time(
                    1,
                    "Release",
                    100.0,
                    "How long a note takes to fade away once its key is let go.",
                )),
            &["1"],
        )
        .build()
        .expect("the parameter space is valid")
}

/// Every parameter, in the order hosts list them.
#[must_use]
pub fn params() -> Vec<ParamDescriptor> {
    space().descriptors()
}

/// A key's pitch in hertz.
#[must_use]
pub fn frequency(key: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// How far an envelope moves per frame to cross its whole range in `ms`
/// milliseconds at `rate`.
fn per_frame(ms: f64, rate: f64) -> f32 {
    (1.0 / (ms * 0.001 * rate).max(1.0)) as f32
}

/// The plug-in on the main thread.
#[derive(Debug)]
pub struct {{Type}} {
    host: Host,
}

impl Plugin for {{Type}} {
    type Processor = {{Type}}Processor;

    const INFO: PluginInfo = PluginInfo::new(ID, NAME)
        .vendor("Loombridge")
        .version(env!("CARGO_PKG_VERSION"))
        .description("A polyphonic instrument built on the plug-in kit.")
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

    fn param_space(&self) -> Option<ParamSpace> {
        Some(space())
    }

    fn summary(&self) -> Option<String> {
        let level = self.host.param(LEVEL)?;
        Some(format!(
            "A polyphonic instrument: up to {VOICES} notes, each a sine at its key's \
             equal-tempered pitch (A4 = 440 Hz), rising over {:.0} ms and fading over {:.0} ms \
             once released, scaled by velocity, at a level of {}.",
            self.host.param(ATTACK)?,
            self.host.param(RELEASE)?,
            if level > 0.0 {
                format!("{:.2} dB", gain_to_db(level))
            } else {
                "-inf dB (silence)".to_owned()
            }
        ))
    }

    fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
        registry.register(
            CommandSpec::new(RESET, "Reset {{Name}}")
                .description(
                    "Puts every parameter back to its default. The host records each change \
                     like a person's.",
                )
                .permission(Permission::Edit)
                .context(Context::Global)
                .args::<NoArgs>()
                .result::<()>(),
        )
    }

    fn run_command(
        &mut self,
        id: &str,
        _args: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        match id {
            RESET => {
                for param in params() {
                    self.host
                        .edit(param.id, param.default)
                        .map_err(|e| PluginError::new(e.to_string()))?;
                }
                Ok(serde_json::Value::Null)
            }
            other => Err(PluginError::new(format!("no command `{other}`"))),
        }
    }

    #[cfg(feature = "export")]
    fn editor(&mut self) -> Option<Box<dyn lspk_plugin::Editor>> {
        Some(Box::new(lspk_ui::generic_editor(NAME, &params())))
    }

    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<{{Type}}Processor, PluginError> {
        let rate = config.sample_rate;
        let frames = LinearSmoother::frames_for(DEFAULT_SMOOTHING_SECONDS, rate);
        Ok({{Type}}Processor {
            rate,
            level: LinearSmoother::new(values.get(LEVEL).unwrap_or(0.5), frames),
            voices: [Voice::IDLE; VOICES],
            attack: per_frame(values.get(ATTACK).unwrap_or(5.0), rate),
            release: per_frame(values.get(RELEASE).unwrap_or(100.0), rate),
            started: 0,
        })
    }
}

/// Where a voice's envelope is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Held,
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
pub struct {{Type}}Processor {
    rate: f64,
    level: LinearSmoother,
    voices: [Voice; VOICES],
    /// The envelope's rise per frame, and its fall per frame from full level.
    attack: f32,
    release: f32,
    /// Notes started so far.
    started: u64,
}

impl {{Type}}Processor {
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
        let attack = self.attack;
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
                            voice.stage = Stage::Held;
                        }
                    }
                    Stage::Held => {}
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
        let taken = voice.is_sounding();
        *voice = Voice {
            note,
            stage: Stage::Attack,
            // A voice taken over rises from where it was, without a click.
            envelope: if taken { voice.envelope } else { 0.0 },
            release: 0.0,
            velocity,
            phase: if taken { voice.phase } else { 0.0 },
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

    /// Takes a parameter's new value; the level glides to it.
    fn set(&mut self, id: ParamId, value: f64) {
        match id {
            LEVEL => self.level.set_target(value),
            ATTACK => self.attack = per_frame(value, self.rate),
            RELEASE => self.release = per_frame(value, self.rate),
            _ => {}
        }
    }
}

impl Processor for {{Type}}Processor {
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
                Event::Param { id, value, .. } => self.set(id, value),
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
            if let Event::Param { id, value, .. } = event {
                self.set(id, value);
                if id == LEVEL {
                    self.level.snap(value);
                }
            }
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; VOICES];
        self.level.snap(self.level.target());
    }
}

#[cfg(feature = "export")]
lspk_plugin::export_clap!(lspk_plugin_agent::WithAgent<{{Type}}>);
