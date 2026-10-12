//! {{Name}}: a stereo effect built on the plug-in kit.
//!
//! Three parameters, in a parameter space ([`space`]), each gliding to its
//! new value over 20 ms so a change never clicks:
//!
//! - [`DRIVE`] saturates the signal, from clean (0 %, the default) to a
//!   strong soft clip (100 %);
//! - [`WIDTH`] narrows or widens the stereo image through mid and side,
//!   from mono (0 %) through unchanged (100 %, the default) to twice as
//!   wide (200 %);
//! - [`GAIN`], from −∞ dB to +12 dB, sets the output level.
//!
//! At its defaults it passes the input unchanged. The processor sleeps while
//! its input is silent. The parameters' IDs never change: `tests/ids.txt`
//! keeps them, and may only be added to. One command of its own, [`RESET`],
//! puts every parameter back to its default.
//!
//! It's exported with the agent interface (`lspk_plugin_agent::WithAgent`),
//! so an agent can drive it in any host once the person switches its agent
//! server on. Without the `export` feature (on by default) the crate is the
//! plug-in's types alone, for a host that compiles it in, as Beatlayer's
//! native devices do.
//!
//! This is the plug-in kit's effect template: make the sound your own by
//! changing the processing, the parameters and the tests together.

use lspk_commands::{CommandSpec, Context, NoArgs, Permission};
use lspk_params::smooth::DEFAULT_SMOOTHING_SECONDS;
use lspk_params::{
    LinearSmoother, ParamDescriptor, ParamId, ParamSpace, ParamUnit, gain_to_db,
};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, ParamValues, Plugin, PluginError, PluginInfo,
    ProcessConfig, ProcessContext, ProcessStatus, Processor, RegisterError, Registry, features,
};

/// The plug-in's ID.
pub const ID: &str = "{{id}}";
/// The plug-in's name.
pub const NAME: &str = "{{Name}}";
/// The output gain: a linear factor from 0 (−∞ dB) to +12 dB, 0 dB by
/// default.
pub const GAIN: ParamId = ParamId(1);
/// The drive, in percent: 0 is clean.
pub const DRIVE: ParamId = ParamId(2);
/// The stereo width, in percent: 0 is mono, 100 unchanged, 200 twice as
/// wide.
pub const WIDTH: ParamId = ParamId(3);
/// The gain's highest value, in dB.
pub const MAX_DB: f64 = 12.0;
/// The plug-in's own command: put every parameter back to its default.
pub const RESET: &str = "{{crate_lib}}.reset";

/// How much louder than the input the drive pushes the signal into its
/// curve at 100 %.
const MAX_PUSH: f32 = 10.0;

/// The parameter space: three global parameters.
///
/// # Panics
///
/// Never: the space is valid, as the tests check.
#[must_use]
pub fn space() -> ParamSpace {
    ParamSpace::builder()
        .global(
            ParamDescriptor::gain(GAIN, "Gain", MAX_DB, 0.0)
                .describe("How much louder or quieter the output is than the input."),
        )
        .global(
            ParamDescriptor::continuous(DRIVE, "Drive", (0.0, 100.0), 0.0, ParamUnit::Percent)
                .describe("How hard the signal is saturated, from clean to a strong soft clip."),
        )
        .global(
            ParamDescriptor::continuous(WIDTH, "Width", (0.0, 200.0), 100.0, ParamUnit::Percent)
                .describe(
                    "How wide the stereo image is, from mono through unchanged to twice as wide.",
                ),
        )
        .build()
        .expect("the parameter space is valid")
}

/// Every parameter, in the order hosts list them.
#[must_use]
pub fn params() -> Vec<ParamDescriptor> {
    space().descriptors()
}

/// One sample through the drive at `drive` (0 to 1): a blend towards a
/// soft clip of the signal pushed up to [`MAX_PUSH`] times louder, scaled
/// back so quiet signals keep their level.
#[must_use]
pub fn saturate(x: f32, drive: f32) -> f32 {
    if drive <= 0.0 {
        return x;
    }
    let push = 1.0 + drive * (MAX_PUSH - 1.0);
    x + drive * ((x * push).tanh() / push - x)
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
        .description("A stereo effect built on the plug-in kit.")
        .features(&[features::AUDIO_EFFECT, features::DISTORTION, features::STEREO])
        .audio(AudioLayout::effect(2));

    fn new(host: Host) -> Self {
        Self { host }
    }

    fn param_space(&self) -> Option<ParamSpace> {
        Some(space())
    }

    fn summary(&self) -> Option<String> {
        let gain = self.host.param(GAIN)?;
        Some(format!(
            "A stereo effect: the signal is driven {:.0} % of the way to a soft clip, its stereo \
             image set to {:.0} % of its width, and its level to {}.",
            self.host.param(DRIVE)?,
            self.host.param(WIDTH)?,
            if gain > 0.0 {
                format!("{:+.2} dB", gain_to_db(gain))
            } else {
                "-inf dB (silence)".to_owned()
            }
        ))
    }

    fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
        registry.register(
            CommandSpec::new(RESET, "Reset {{Name}}")
                .description(
                    "Puts every parameter back to its default, so the output matches the input. \
                     The host records each change like a person's.",
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
        let frames = LinearSmoother::frames_for(DEFAULT_SMOOTHING_SECONDS, config.sample_rate);
        let smoother = |id: ParamId, default: f64, per: f64| {
            LinearSmoother::new(values.get(id).unwrap_or(default) / per, frames)
        };
        Ok({{Type}}Processor {
            gain: smoother(GAIN, 1.0, 1.0),
            drive: smoother(DRIVE, 0.0, 100.0),
            width: smoother(WIDTH, 100.0, 100.0),
        })
    }
}

/// The plug-in on the audio thread. The drive and width are kept as
/// fractions (0 to 1, and 0 to 2).
#[derive(Debug)]
pub struct {{Type}}Processor {
    gain: LinearSmoother,
    drive: LinearSmoother,
    width: LinearSmoother,
}

impl {{Type}}Processor {
    /// Processes frames `from..to` of every channel: drive, then width
    /// (on the first two channels), then gain.
    fn render(&mut self, audio: &mut Buffers<'_>, from: usize, to: usize) {
        for frame in from..to {
            let (gain, drive, width) = (
                self.gain.step() as f32,
                self.drive.step() as f32,
                self.width.step() as f32,
            );
            if let Some((left, right)) = audio.stereo_mut() {
                let (mut l, mut r) = (saturate(left[frame], drive), saturate(right[frame], drive));
                // Unchanged at 100 %, to the bit.
                if (width - 1.0).abs() > f32::EPSILON {
                    let (mid, side) = ((l + r) * 0.5, (l - r) * 0.5 * width);
                    (l, r) = (mid + side, mid - side);
                }
                (left[frame], right[frame]) = (l * gain, r * gain);
            } else {
                for c in 0..audio.channels() {
                    let sample = &mut audio.channel_mut(c)[frame];
                    *sample = saturate(*sample, drive) * gain;
                }
            }
        }
    }

    /// Takes a parameter's new value, to glide to.
    fn set(&mut self, id: ParamId, value: f64) {
        match id {
            GAIN => self.gain.set_target(value),
            DRIVE => self.drive.set_target(value / 100.0),
            WIDTH => self.width.set_target(value / 100.0),
            _ => {}
        }
    }

    fn settled(&self) -> bool {
        self.gain.is_settled() && self.drive.is_settled() && self.width.is_settled()
    }
}

impl Processor for {{Type}}Processor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        _cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        let silent = audio.is_silent();
        let frames = audio.frames();
        let mut done = 0;
        for event in events.iter() {
            if let Event::Param { time, id, value } = event {
                let at = (time as usize).clamp(done, frames);
                self.render(audio, done, at);
                done = at;
                self.set(id, value);
            }
        }
        self.render(audio, done, frames);
        if silent && self.settled() {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::ContinueIfNotQuiet
        }
    }

    fn flush(&mut self, events: &Events<'_>, _cx: &mut ProcessContext<'_>) {
        for event in events.iter() {
            if let Event::Param { id, value, .. } = event {
                self.set(id, value);
                for smoother in [&mut self.gain, &mut self.drive, &mut self.width] {
                    smoother.snap(smoother.target());
                }
            }
        }
    }
}

#[cfg(feature = "export")]
lspk_plugin::export_clap!(lspk_plugin_agent::WithAgent<{{Type}}>);
