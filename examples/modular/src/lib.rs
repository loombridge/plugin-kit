//! Example Modular: a small monophonic synthesiser built from modules, the
//! plug-in kit's test of its parameter space for modular plug-ins.
//!
//! - **Fixed modules**, every parameter visible to the host by name: two
//!   oscillators (`Osc A`, `Osc B`: level, shape, pitch), a low-pass filter
//!   (`Filter 1`: cutoff) and an envelope (`Env 1`: attack, release).
//! - **A two-slot effects rack** (`FX 1`, `FX 2`), each slot holding one of
//!   Off, Distortion, Delay or Tremolo, chosen by its `Type` parameter. Each
//!   slot reserves four parameters, as many as Delay needs; the current
//!   effect's parameters take them with their real names, ranges and value
//!   text, and the rest are hidden.
//! - A global `Volume`.
//!
//! The IDs follow the kit's scheme ([`lspk_params::space`]) and are kept in
//! `tests/ids.txt`, which the tests check is only ever added to.
//!
//! The sound is deliberately simple: the newest held note plays both
//! oscillators through a one-pole low-pass filter and a linear envelope,
//! then the two effects in order. The processor reads its parameters once
//! a block, which is enough for a test instrument.

use std::cell::RefCell;
use std::f64::consts::TAU;
use std::sync::Arc;

use lspk_params::{ModuleType, ParamAddress, ParamDescriptor, ParamId, ParamSpace, ParamUnit};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, NotePorts, ParamValues, Plugin, PluginError,
    PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor, Tail, features,
};

/// The plug-in's ID.
pub const ID: &str = "com.loombridge.example.modular";
/// The plug-in's name.
pub const NAME: &str = "Example Modular";

/// The global volume.
pub const VOLUME: ParamId = ParamId(1);

/// The oscillator module's type number.
pub const OSC: u8 = 1;
/// The filter module's type number.
pub const FILTER: u8 = 2;
/// The envelope module's type number.
pub const ENV: u8 = 3;
/// The effect slots' module type number.
pub const FX: u8 = 20;

/// An oscillator's level.
pub const OSC_LEVEL: u16 = 0;
/// An oscillator's shape.
pub const OSC_SHAPE: u16 = 1;
/// An oscillator's pitch, in semitones.
pub const OSC_PITCH: u16 = 2;
/// The filter's cutoff.
pub const CUTOFF: u16 = 0;
/// The envelope's attack.
pub const ATTACK: u16 = 0;
/// The envelope's release.
pub const RELEASE: u16 = 1;

/// The effects a slot can hold, by the index its type parameter stores.
pub mod effects {
    /// No effect.
    pub const OFF: usize = 0;
    /// Distortion: drive, shape, mix.
    pub const DISTORTION: usize = 1;
    /// Delay: time, feedback, mix, tone.
    pub const DELAY: usize = 2;
    /// Tremolo: rate, depth.
    pub const TREMOLO: usize = 3;
}

/// Parameter `param` of instance `instance` of module type `module`.
#[must_use]
pub const fn id(module: u8, instance: u8, param: u16) -> ParamId {
    ParamAddress::new(module, instance, param).id()
}

/// Slot `slot`'s type parameter.
#[must_use]
pub const fn slot_type(slot: u8) -> ParamId {
    id(FX, slot, lspk_params::space::SELECTOR)
}

/// Slot `slot`'s reserved parameter `n`.
#[must_use]
pub const fn slot_param(slot: u8, n: u16) -> ParamId {
    id(FX, slot, lspk_params::space::SLOT_BASE + n)
}

fn percent(number: u32, name: &str, default: f64) -> ParamDescriptor {
    ParamDescriptor::continuous(
        ParamId(number),
        name,
        (0.0, 100.0),
        default,
        ParamUnit::Percent,
    )
}

/// The oscillator module.
#[must_use]
pub fn oscillator() -> ModuleType {
    ModuleType::new(OSC, "osc", "Osc")
        .param(
            ParamDescriptor::gain(ParamId(OSC_LEVEL.into()), "Level", 0.0, -6.0)
                .describe("How loud the oscillator plays."),
        )
        .param(ParamDescriptor::choice(
            ParamId(OSC_SHAPE.into()),
            "Shape",
            &["Sine", "Saw", "Square"],
            0,
        ))
        .param(ParamDescriptor::integer(
            ParamId(OSC_PITCH.into()),
            "Pitch",
            (-24, 24),
            0,
            ParamUnit::Semitones,
        ))
}

/// The effects a slot can hold, in the order of its type parameter's
/// choices. New effects go at the end, and new parameters take the next
/// free number in their effect.
#[must_use]
pub fn effect_modules() -> Vec<ModuleType> {
    vec![
        ModuleType::new(30, "off", "Off"),
        ModuleType::new(31, "distortion", "Distortion")
            .param(percent(0, "Drive", 50.0))
            .param(ParamDescriptor::choice(
                ParamId(1),
                "Shape",
                &["Soft", "Hard"],
                0,
            ))
            .param(percent(2, "Mix", 100.0)),
        ModuleType::new(32, "delay", "Delay")
            .param(
                ParamDescriptor::continuous(
                    ParamId(0),
                    "Time",
                    (1.0, 1000.0),
                    250.0,
                    ParamUnit::Milliseconds,
                )
                .logarithmic(),
            )
            .param(ParamDescriptor::continuous(
                ParamId(1),
                "Feedback",
                (0.0, 95.0),
                35.0,
                ParamUnit::Percent,
            ))
            .param(percent(2, "Mix", 30.0))
            .param(
                ParamDescriptor::continuous(
                    ParamId(3),
                    "Tone",
                    (200.0, 20_000.0),
                    8000.0,
                    ParamUnit::Hertz,
                )
                .logarithmic(),
            ),
        ModuleType::new(33, "tremolo", "Tremolo")
            .param(
                ParamDescriptor::continuous(ParamId(0), "Rate", (0.1, 20.0), 4.0, ParamUnit::Hertz)
                    .logarithmic(),
            )
            .param(percent(1, "Depth", 50.0)),
    ]
}

/// The parameter space.
///
/// # Panics
///
/// Never: the space is valid, as the tests check.
#[must_use]
pub fn space() -> ParamSpace {
    ParamSpace::builder()
        .global(ParamDescriptor::gain(VOLUME, "Volume", 6.0, 0.0))
        .module(oscillator(), &["A", "B"])
        .module(
            ModuleType::new(FILTER, "filter", "Filter").param(
                ParamDescriptor::continuous(
                    ParamId(CUTOFF.into()),
                    "Cutoff",
                    (20.0, 20_000.0),
                    20_000.0,
                    ParamUnit::Hertz,
                )
                .logarithmic(),
            ),
            &["1"],
        )
        .module(
            ModuleType::new(ENV, "env", "Env")
                .param(ParamDescriptor::continuous(
                    ParamId(ATTACK.into()),
                    "Attack",
                    (0.0, 5000.0),
                    5.0,
                    ParamUnit::Milliseconds,
                ))
                .param(ParamDescriptor::continuous(
                    ParamId(RELEASE.into()),
                    "Release",
                    (0.0, 5000.0),
                    200.0,
                    ParamUnit::Milliseconds,
                )),
            &["1"],
        )
        .slots((FX, "fx", "FX"), "Type", &["1", "2"], effect_modules())
        .build()
        .expect("the example's space is valid")
}

thread_local! {
    /// The newest instance's host link on this thread, so a test can edit
    /// a parameter as the plug-in's interface or an agent would.
    static NEWEST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// Edits parameter `id` to `value` (as kept: a slot's parameter as its
/// travel) through the newest instance on this thread, as its interface
/// would: one gesture the host hears. For tests.
///
/// # Errors
///
/// No instance, or the edit wasn't made.
pub fn edit_newest(id: ParamId, value: f64) -> Result<(), String> {
    NEWEST.with_borrow(|host| {
        host.as_ref()
            .ok_or_else(|| "no instance".to_owned())?
            .edit(id, value)
            .map_err(|e| e.to_string())
    })
}

/// The plug-in on the main thread.
#[derive(Debug)]
pub struct Modular {
    host: Host,
}

impl Plugin for Modular {
    type Processor = ModularProcessor;

    const INFO: PluginInfo = PluginInfo::new(ID, NAME)
        .vendor("Loombridge")
        .version(env!("CARGO_PKG_VERSION"))
        .url("https://github.com/loombridge/plugin-kit")
        .description("A small modular synthesiser: the plug-in kit's parameter-space example.")
        .features(&[
            features::INSTRUMENT,
            features::SYNTHESIZER,
            features::STEREO,
        ])
        .audio(AudioLayout::instrument(2))
        .notes(NotePorts::INPUT);

    fn new(host: Host) -> Self {
        NEWEST.set(Some(host.clone()));
        Self { host }
    }

    fn param_space(&self) -> Option<ParamSpace> {
        Some(space())
    }

    fn summary(&self) -> Option<String> {
        let values = self.host.values()?;
        let space = values.space()?;
        let rack: Vec<String> = (0..space.slots().len())
            .map(|slot| {
                let choice = space.slot_choice(slot, values.get(space.slots()[slot].selector)?);
                Some(format!(
                    "slot {} holds {}",
                    slot + 1,
                    space.choices(slot)[choice].label
                ))
            })
            .collect::<Option<_>>()?;
        Some(format!(
            "A monophonic synthesiser: two oscillators through a low-pass filter and an \
             envelope, then an effects rack in which {}.",
            rack.join(" and ")
        ))
    }

    fn editor(&mut self) -> Option<Box<dyn lspk_plugin::Editor>> {
        Some(Box::new(lspk_ui::generic_editor(
            NAME,
            &space().descriptors(),
        )))
    }

    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<ModularProcessor, PluginError> {
        let space = values
            .space()
            .cloned()
            .ok_or_else(|| PluginError::new("no parameter space"))?;
        let rate = config.sample_rate;
        let delay_frames = (rate * 1.001) as usize + 2;
        let slots = (0..space.slots().len())
            .map(|_| SlotState {
                choice: usize::MAX,
                delay: vec![0.0; delay_frames],
                at: 0,
                tone: 0.0,
                phase: 0.0,
            })
            .collect();
        Ok(ModularProcessor {
            space,
            rate,
            key: None,
            note: 69,
            phases: [0.0; 2],
            envelope: 0.0,
            filter: 0.0,
            slots,
        })
    }
}

/// One effect slot's state on the audio thread.
#[derive(Debug)]
struct SlotState {
    /// The effect it holds, as of the last block.
    choice: usize,
    /// The delay line, a little over a second long.
    delay: Vec<f32>,
    at: usize,
    /// The delay's tone filter.
    tone: f32,
    /// The tremolo's phase, in turns.
    phase: f64,
}

/// The plug-in on the audio thread.
#[derive(Debug)]
pub struct ModularProcessor {
    space: Arc<ParamSpace>,
    rate: f64,
    /// The key held, if any.
    key: Option<u8>,
    /// The last key played, which sounds on through the release.
    note: u8,
    /// Each oscillator's phase, in turns.
    phases: [f64; 2],
    envelope: f32,
    filter: f32,
    slots: Vec<SlotState>,
}

/// What a block reads from the parameters.
struct Settings {
    volume: f32,
    oscs: [(f32, usize, f64); 2],
    cutoff: f32,
    attack: f32,
    release: f32,
    effects: [(usize, [f64; 4]); 2],
}

impl ModularProcessor {
    fn settings(&self, values: &ParamValues) -> Settings {
        let get = |id: ParamId, default: f64| values.get(id).unwrap_or(default);
        let osc = |i: u8| {
            (
                get(id(OSC, i, OSC_LEVEL), 0.5) as f32,
                get(id(OSC, i, OSC_SHAPE), 0.0) as usize,
                get(id(OSC, i, OSC_PITCH), 0.0),
            )
        };
        let per_frame = |ms: f64| (1.0 / (ms * 0.001 * self.rate).max(1.0)) as f32;
        let cutoff = get(id(FILTER, 0, CUTOFF), 20_000.0);
        let mut effects = [(0, [0.0; 4]); 2];
        for (slot, effect) in effects.iter_mut().enumerate() {
            let s = &self.space.slots()[slot];
            let choice = self.space.slot_choice(slot, get(s.selector, 0.0));
            effect.0 = choice;
            for (n, value) in effect.1.iter_mut().enumerate() {
                let n = n as u16;
                *value = self
                    .space
                    .slot_param(slot, choice, n)
                    .map_or(0.0, |d| d.denormalize(get(s.reserved_id(n), 0.0)));
            }
        }
        Settings {
            volume: get(VOLUME, 1.0) as f32,
            oscs: [osc(0), osc(1)],
            cutoff: (1.0 - (-TAU * cutoff / self.rate).exp()) as f32,
            attack: per_frame(get(id(ENV, 0, ATTACK), 5.0)),
            release: per_frame(get(id(ENV, 0, RELEASE), 200.0)),
            effects,
        }
    }

    /// Renders frames `from..to` into every channel.
    fn render(&mut self, audio: &mut Buffers<'_>, settings: &Settings, from: usize, to: usize) {
        for frame in from..to {
            let mut sample = 0.0f32;
            self.envelope = if self.key.is_some() {
                (self.envelope + settings.attack).min(1.0)
            } else {
                (self.envelope - settings.release).max(0.0)
            };
            for (phase, &(level, shape, pitch)) in self.phases.iter_mut().zip(&settings.oscs) {
                let hz = 440.0 * 2f64.powf((f64::from(self.note) + pitch - 69.0) / 12.0);
                let wave = match shape {
                    0 => (*phase * TAU).sin(),
                    1 => 2.0 * *phase - 1.0,
                    _ if *phase < 0.5 => 1.0,
                    _ => -1.0,
                };
                sample += wave as f32 * level * 0.25;
                *phase = (*phase + hz / self.rate).fract();
            }
            self.filter =
                flush(self.filter + settings.cutoff * (sample * self.envelope - self.filter));
            let mut out = self.filter;
            for (state, &(choice, p)) in self.slots.iter_mut().zip(&settings.effects) {
                out = effect(state, choice, &p, out, self.rate);
            }
            out = flush(out * settings.volume);
            for c in 0..audio.channels() {
                audio.channel_mut(c)[frame] = out;
            }
        }
    }
}

/// `x`, or 0 if it's too small to hear, so no state decays into subnormal
/// numbers.
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

/// One frame of `input` through the effect `choice` with parameters `p`,
/// in their real units.
fn effect(state: &mut SlotState, choice: usize, p: &[f64; 4], input: f32, rate: f64) -> f32 {
    match choice {
        effects::DISTORTION => {
            let drive = 1.0 + p[0] as f32 * 0.2;
            let wet = if p[1] >= 0.5 {
                (input * drive).clamp(-1.0, 1.0)
            } else {
                (input * drive).tanh()
            };
            input + (wet - input) * p[2] as f32 / 100.0
        }
        effects::DELAY => {
            let len = state.delay.len();
            let back = ((p[0] * 0.001 * rate) as usize).clamp(1, len - 1);
            let read = state.delay[(state.at + len - back) % len];
            let coefficient = (1.0 - (-TAU * p[3] / rate).exp()) as f32;
            state.tone = flush(state.tone + coefficient * (read - state.tone));
            state.delay[state.at] = flush(input + state.tone * p[1] as f32 / 100.0);
            state.at = (state.at + 1) % len;
            input + state.tone * p[2] as f32 / 100.0
        }
        effects::TREMOLO => {
            let depth = p[1] as f32 / 100.0;
            let lfo = 0.5 + 0.5 * (state.phase * TAU).sin() as f32;
            state.phase = (state.phase + p[0] / rate).fract();
            input * (1.0 - depth * lfo)
        }
        _ => input,
    }
}

impl Processor for ModularProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        let settings = self.settings(cx.values());
        // A slot that now holds another effect starts it afresh.
        for (state, &(choice, _)) in self.slots.iter_mut().zip(&settings.effects) {
            if state.choice != choice {
                state.choice = choice;
                state.delay.fill(0.0);
                state.at = 0;
                state.tone = 0.0;
                state.phase = 0.0;
            }
        }
        let frames = audio.frames();
        let mut done = 0;
        for event in events.iter() {
            let (time, key) = match event {
                Event::NoteOn { time, note, .. } => (time, note.key),
                Event::NoteOff { time, note, .. } | Event::NoteChoke { time, note } => {
                    if note.key.is_some() && note.key != self.key {
                        continue;
                    }
                    (time, None)
                }
                _ => continue,
            };
            let at = (time as usize).clamp(done, frames);
            self.render(audio, &settings, done, at);
            done = at;
            self.key = key;
            if let Some(key) = key {
                self.note = key;
            }
        }
        self.render(audio, &settings, done, frames);
        ProcessStatus::Continue
    }

    fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}

    fn reset(&mut self) {
        self.key = None;
        self.envelope = 0.0;
        self.filter = 0.0;
        for state in &mut self.slots {
            state.delay.fill(0.0);
            state.tone = 0.0;
        }
    }

    fn tail(&self) -> Tail {
        Tail::Infinite
    }
}

lspk_plugin::export_clap!(lspk_plugin_agent::WithAgent<Modular>);
