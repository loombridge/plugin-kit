//! Example Gain: a stereo gain, the smallest effect built on the plug-in
//! kit. It proves the kit's CLAP export in tests, and is the seed of the
//! effect template `cargo xtask new-plugin` will generate.
//!
//! One parameter, [`GAIN`], from −∞ dB to +12 dB, glides to each new value
//! over 20 ms so a change never clicks. The processor sleeps while its input
//! is silent. One command of its own, [`RESET`], puts the gain back to 0 dB.
//!
//! It's exported with the agent interface ([`lspk_plugin_agent::WithAgent`]),
//! so an agent can drive it in any host once the person switches its agent
//! server on.

use lspk_commands::{CommandSpec, Context, NoArgs, Permission};
use lspk_params::smooth::DEFAULT_SMOOTHING_SECONDS;
use lspk_params::{LinearSmoother, ParamDescriptor, ParamId, gain_to_db};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, Plugin, PluginError, PluginInfo, ProcessConfig,
    ProcessContext, ProcessStatus, Processor, RegisterError, Registry, features,
};

/// The plug-in's ID.
pub const ID: &str = "com.loombridge.example.gain";
/// The plug-in's name.
pub const NAME: &str = "Example Gain";
/// The gain: a linear factor from 0 (−∞ dB) to +12 dB, 0 dB by default.
pub const GAIN: ParamId = ParamId(1);
/// The gain's highest value, in dB.
pub const MAX_DB: f64 = 12.0;
/// The plug-in's own command: put the gain back to 0 dB.
pub const RESET: &str = "gain.reset";

/// The parameters.
#[must_use]
pub fn params() -> Vec<ParamDescriptor> {
    vec![
        ParamDescriptor::gain(GAIN, "Gain", MAX_DB, 0.0)
            .describe("How much louder or quieter the output is than the input."),
    ]
}

/// The plug-in on the main thread.
#[derive(Debug)]
pub struct Gain {
    host: Host,
}

impl Plugin for Gain {
    type Processor = GainProcessor;

    const INFO: PluginInfo = PluginInfo::new(ID, NAME)
        .vendor("Loombridge")
        .version(env!("CARGO_PKG_VERSION"))
        .url("https://github.com/loombridge/plugin-kit")
        .description("A stereo gain: the plug-in kit's example effect.")
        .features(&[features::AUDIO_EFFECT, features::UTILITY, features::STEREO])
        .audio(AudioLayout::effect(2));

    fn new(host: Host) -> Self {
        Self { host }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        params()
    }

    fn summary(&self) -> Option<String> {
        let gain = self.host.param(GAIN)?;
        Some(format!(
            "A stereo gain: both channels are scaled by {}, gliding over 20 ms to each new value.",
            if gain > 0.0 {
                format!("{:+.2} dB", gain_to_db(gain))
            } else {
                "-inf dB (silence)".to_owned()
            }
        ))
    }

    fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
        registry.register(
            CommandSpec::new(RESET, "Reset Gain")
                .description(
                    "Puts the gain back to 0 dB, so the output matches the input. The host \
                     records it like a person's change.",
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
                self.host
                    .edit(GAIN, 1.0)
                    .map_err(|e| PluginError::new(e.to_string()))?;
                Ok(serde_json::Value::Null)
            }
            other => Err(PluginError::new(format!("no command `{other}`"))),
        }
    }

    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &lspk_plugin::ParamValues,
    ) -> Result<GainProcessor, PluginError> {
        let frames = LinearSmoother::frames_for(DEFAULT_SMOOTHING_SECONDS, config.sample_rate);
        let gain = values.get(GAIN).unwrap_or(1.0);
        Ok(GainProcessor {
            gain: LinearSmoother::new(gain, frames),
        })
    }
}

/// The plug-in on the audio thread.
#[derive(Debug)]
pub struct GainProcessor {
    gain: LinearSmoother,
}

impl GainProcessor {
    /// Applies the gain to frames `from..to` of every channel.
    fn render(&mut self, audio: &mut Buffers<'_>, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let start = self.gain;
        for c in 0..audio.channels() {
            // Every channel follows the same glide.
            let mut gain = start;
            for sample in &mut audio.channel_mut(c)[from..to] {
                *sample *= gain.step() as f32;
            }
            self.gain = gain;
        }
        if audio.channels() == 0 {
            self.gain.skip(to - from);
        }
    }
}

impl Processor for GainProcessor {
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
            if let Event::Param {
                time,
                id: GAIN,
                value,
            } = event
            {
                let at = (time as usize).clamp(done, frames);
                self.render(audio, done, at);
                done = at;
                self.gain.set_target(value);
            }
        }
        self.render(audio, done, frames);
        if silent && self.gain.is_settled() {
            ProcessStatus::Sleep
        } else {
            ProcessStatus::ContinueIfNotQuiet
        }
    }

    fn flush(&mut self, events: &Events<'_>, _cx: &mut ProcessContext<'_>) {
        for event in events.iter() {
            if let Event::Param {
                id: GAIN, value, ..
            } = event
            {
                self.gain.snap(value);
            }
        }
    }
}

lspk_plugin::export_clap!(lspk_plugin_agent::WithAgent<Gain>);
