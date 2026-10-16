//! The format layer: a plug-in is written once against the [`Plugin`] and
//! [`Processor`] traits, and the kit exports it to each format.
//!
//! - **CLAP**, natively, through `clack`: [`export_clap!`] in the plug-in's
//!   `cdylib` crate (the `clap` feature, on by default).
//! - **VST3 and Audio Unit v2** (macOS), made from the CLAP export by
//!   free-audio's clap-wrapper (`lspk-clap-wrapper`): the `vst3` and `auv2`
//!   features, with which [`export_clap!`] also exports their entry points
//!   from the same library. They need nothing more from a plug-in.
//!   [`bundle`] lays the library out as each format's bundle, as
//!   `cargo xtask bundle` does.
//! - **A standalone app** (`lspk-standalone`) and **a native Beatlayer
//!   device** (R0.8) drive the same two traits through their own adapters,
//!   with [`ParamValues`], a [`HostBackend`], [`Events::from_slice`] and an
//!   [`EventSink`] of their own. [`bundle::write_app`] lays the app out.
//!
//! # The two halves of a plug-in
//!
//! A plug-in is two types, split along the threads a host runs it on:
//!
//! - The [`Plugin`] lives on the host's main thread for the whole life of
//!   an instance. It describes the parameters, saves and loads state,
//!   reports latency, runs commands, and makes the [`Processor`] when the
//!   host activates the instance, which is where every allocation happens.
//! - The [`Processor`] lives on an audio thread between activation and
//!   deactivation. It renders audio from sample-accurate [`Event`]s, and
//!   tells the host when it can sleep. It never allocates, locks or blocks.
//!
//! The two share the parameters' values through [`ParamValues`], which the
//! kit keeps current from every direction: the host's automation, the
//! plug-in's own edits on the main thread ([`Host::edit`]) and on the audio
//! thread ([`ProcessContext::set_param`]), and loaded state. Anything else
//! the halves share, a plug-in passes through lock-free queues of its own.
//!
//! # The host stays in step
//!
//! When the plug-in changes one of its own parameters (an agent's command,
//! a control in its interface), it calls [`Host::edit`], or
//! [`Host::begin_edit`], [`Host::set_param`] and [`Host::end_edit`] for a
//! drag. The kit updates the value, passes it to the processor at the start
//! of its next block, and tells the host exactly as if a person had moved
//! the control: a gesture begin, the value and a gesture end, sent from the
//! next process or parameter flush (the kit asks the host for one), and the
//! state marked dirty. The host's automation, undo and saved project then
//! match the sound.
//!
//! # An editor
//!
//! A plug-in with an interface returns an [`Editor`] from
//! [`Plugin::editor`]; the CLAP export serves it through CLAP's `gui`
//! extension, which the VST3 and Audio Unit builds made from the CLAP build
//! wrap in their own. The [`editor`] module says how a host drives it, and
//! `lspk-ui` provides editors drawn with egui. A plug-in without one has no
//! window, and this crate reaches no UI toolkit either way.
//!
//! # Values the host sees
//!
//! Values in the kit are always in a parameter's own units, as its
//! [`ParamDescriptor`] gives them. A format may show the host something
//! else: CLAP sees a continuous or gain parameter as its control's travel,
//! from 0 to 1, so automation moves evenly in dB or in octaves, and a
//! stepped parameter as its whole-number value. The kit converts in both
//! directions.

pub mod agent_commands;
pub mod audio;
pub mod bundle;
pub mod editor;
pub mod events;
pub mod host;
pub mod params;
pub mod state;

#[cfg(feature = "clap")]
pub mod clap;

pub use audio::{Buffers, MAX_CHANNELS};
pub use editor::{
    Editor, EditorContext, EditorKey, EditorSize, KeyboardFocus, ParentWindow, ResizeHints,
    WindowApi,
};
pub use events::{Event, EventSource, Events, Expression, Note};
pub use host::{
    EditError, EventSink, Host, HostBackend, OutputEvent, ProcessContext, Transport, Waker,
};
pub use lspk_commands::{RegisterError, Registry};
pub use lspk_params::{ParamDescriptor, ParamId, ParamSpace, ParamView};
pub use params::ParamValues;

use std::ffi::CStr;
use std::fmt;

/// A plug-in's main-thread half: what it is, its parameters, its state, and
/// the [`Processor`] it makes when the host activates it.
///
/// Every method runs on the host's main thread. Which later work package
/// fills what, without changing this trait:
///
/// - **VST3 and Audio Unit v2:** made from the CLAP export by
///   clap-wrapper (the `vst3` and `auv2` features), which maps
///   [`PluginInfo::features`] to their categories, and the gestures to
///   VST3's `beginEdit`, `performEdit` and `endEdit` and Audio Units'
///   parameter gesture and change notifications. An edit made while a VST3
///   or Audio Unit instance is inactive reaches the host with its first
///   block once active. On Linux, clap-wrapper sends a VST3 host the
///   gestures only while the plug-in's editor is open, since it gets its
///   idle timer from the editor's run loop; without one, the values still
///   reach the host, as output parameter changes from `process`.
/// - **The standalone app** (`lspk-standalone`): calls
///   [`Plugin::activate`] and [`Processor::process`] from an audio
///   device's callback, with a [`HostBackend`] of its own, and opens
///   [`Plugin::editor`] in a window of its own.
/// - **R0.8, the Beatlayer device:** the same, inside Beatlayer's engine (its
///   own adapter, in Beatlayer), with [`Plugin::register_commands`] and
///   [`Plugin::run_command`] reached through Beatlayer's registry; and the
///   agent-commands CLAP extension ([`agent_commands`]), which serves the
///   same two methods to any CLAP host that knows it.
/// - **R0.9, the in-plug-in agent server** (`lspk-plugin-agent`): runs
///   from [`Plugin::on_main_thread`], woken from its own threads through
///   [`Host::waker`], changes parameters through [`Host::edit`], and reads
///   [`Plugin::summary`], [`Host::host_name`] and [`Host::track_name`].
/// - **R0.10, the editor:** [`Plugin::editor`], served through CLAP's
///   `gui` extension, which `clap-wrapper` turns into VST3's and Audio
///   Units' views.
/// - **R0.14, the parameter space:** [`Plugin::param_space`], with
///   hierarchical [`ParamId`]s within the 32 bits every format gives an ID
///   and slots whose parameters change meaning with their module; see
///   "Modular plug-ins" below.
///
/// # Modular plug-ins
///
/// A plug-in made of modules returns a [`ParamSpace`] from
/// [`param_space`](Self::param_space) and leaves
/// [`params`](Self::params) to its default, which lists the space's
/// parameters. Every parameter of every fixed module is then visible to the
/// host by its real name (`Osc A Level`), and each slot's reserved
/// parameters take its current module's names, value text and hidden
/// flags. When a slot's type parameter changes, from any direction (the
/// host's automation, an edit, the editor, an agent), the kit gives the
/// slot's reserved parameters the new module's defaults, passes them to the
/// processor, and tells the host: CLAP's `params.rescan` with the info,
/// text and values flags and `remote-controls.changed`, which clap-wrapper
/// turns into VST3's `restartComponent` (titles and values changed) and
/// Audio Units' parameter list and info notifications. Loaded state keeps
/// its values. The IDs never change, so automation a host has recorded on a
/// slot's parameter stays with the slot.
///
/// What each format's host is told on a slot's change, as the kit's test
/// hosts in `lspk-test` see it:
///
/// | Format | The host is told | Notes |
/// |---|---|---|
/// | CLAP | `params.rescan(INFO \| TEXT \| VALUES)`, then `remote-controls.changed` | Allowed while active, since no range, step or flag that needs a restart changes. The host rereads names, modules, the hidden flag, value text and values, and records no automation for the new defaults |
/// | VST3 | `restartComponent(kParamValuesChanged \| kParamTitlesChanged)` | clap-wrapper first refreshes its own copy of each title, short title, hidden flag and value. A change the host's automation makes reaches the main thread from clap-wrapper's idle timer, which on Linux runs only while the editor is open; the plug-in's own changes are told at once everywhere. Units (from CLAP's module paths) are made once, at load |
/// | Audio Unit v2 | `kAudioUnitProperty_ParameterList`, `kAudioUnitProperty_ParameterInfo` and `kAudioUnitProperty_ClassInfo` changed | clap-wrapper rereads every parameter's info: a hidden parameter is neither readable nor writable. Its module path is its clump |
///
/// How real hosts (Bitwig, Reaper, Live, Logic and the rest) show the
/// change is checked by hand before each release.
pub trait Plugin: Sized + 'static {
    /// The audio-thread half.
    type Processor: Processor;

    /// What the plug-in is: its ID, name, ports and features.
    const INFO: PluginInfo;

    /// Makes a new instance. `host` is how the plug-in reaches the host
    /// later: keep it to edit parameters and ask for callbacks. Its edits
    /// work once this returns.
    fn new(host: Host) -> Self;

    /// The parameters, each with an ID that never changes, in the order a
    /// host lists them. The kit asks once, as the instance starts, and
    /// refuses an instance whose descriptors break a rule
    /// ([`ParamDescriptor::check`]) or repeat an ID. By default, the
    /// parameters of [`param_space`](Self::param_space).
    fn params(&self) -> Vec<ParamDescriptor> {
        self.param_space()
            .map(|space| space.descriptors())
            .unwrap_or_default()
    }

    /// The plug-in's parameter space, if it's made of modules: asked for
    /// once, as the instance starts. Its parameters must be
    /// [`params`](Self::params)'s.
    fn param_space(&self) -> Option<ParamSpace> {
        None
    }

    /// Makes the processor for `config`: allocate everything it will need
    /// here, sized for [`ProcessConfig::max_frames`], since the processor
    /// may not. `values` holds every parameter's value now.
    ///
    /// # Errors
    ///
    /// Why it can't run at this configuration; the host doesn't activate it.
    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<Self::Processor, PluginError>;

    /// Takes the processor back when the host deactivates the instance. The
    /// processor is dropped here, on the main thread, so it may free
    /// memory as it goes.
    fn deactivate(&mut self, processor: Self::Processor) {
        drop(processor);
    }

    /// The plug-in's own state beyond its parameters' values, which the kit
    /// saves itself. Most plug-ins keep none.
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Loads what [`save_state`](Self::save_state) saved. The parameters'
    /// values are already loaded when it's called.
    ///
    /// # Errors
    ///
    /// Why `data` isn't a state this plug-in can load.
    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        if data.is_empty() {
            Ok(())
        } else {
            Err(PluginError::new("this plug-in keeps no state of its own"))
        }
    }

    /// How many frames late its output is, as of its last activation. A
    /// change takes effect when the host restarts it
    /// ([`Host::request_restart`]).
    fn latency(&self) -> u32 {
        0
    }

    /// Called on the main thread after [`Host::request_callback`] or a
    /// [`Host::waker`].
    fn on_main_thread(&mut self) {}

    /// Called on the main thread when the host says the track the instance
    /// is on has changed (its name, say): see [`Host::track_name`].
    fn track_info_changed(&mut self) {}

    /// The plug-in's own summary of its state, in a few sentences of prose,
    /// for agents that reason better from text than from a list of values:
    /// what it's set to do, and how the sound flows through it. `None`
    /// leaves agents the parameters alone.
    fn summary(&self) -> Option<String> {
        None
    }

    /// The plug-in's own guide for agents, in Markdown: its modules, how
    /// sound flows through them, and how to get good results from it. The
    /// agent interface serves it as a resource beside the kit's guide.
    /// `None` leaves agents the kit's guide alone.
    fn guide(&self) -> Option<String> {
        None
    }

    /// The plug-in's editor, if it has one: asked for once, as the
    /// instance starts, and kept for its life while the host opens and
    /// closes its window. It should hold no window until
    /// [`Editor::create`]. Without one, the host offers no window.
    fn editor(&mut self) -> Option<Box<dyn Editor>> {
        None
    }

    /// Adds the plug-in's own commands to `registry`, each with its schema,
    /// description and permission class, so agents and hosts can list them.
    ///
    /// # Errors
    ///
    /// A command the registry refuses.
    fn register_commands(&self, registry: &mut Registry) -> Result<(), RegisterError> {
        let _ = registry;
        Ok(())
    }

    /// Runs the command `id`, one [`register_commands`] registered, with
    /// arguments that match its schema, and returns its result.
    ///
    /// [`register_commands`]: Self::register_commands
    ///
    /// # Errors
    ///
    /// Why the command didn't run.
    fn run_command(
        &mut self,
        id: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        let _ = args;
        Err(PluginError::new(format!("no command `{id}`")))
    }
}

/// A plug-in's audio-thread half, made by [`Plugin::activate`].
///
/// Nothing here may allocate, lock or block: the kit's tests run it under
/// the allocation trap.
pub trait Processor: Send + 'static {
    /// Renders one block. `audio` holds the main output's channels; an
    /// effect finds its input already in them, to process in place, and an
    /// instrument finds them silent, to add its voices to. `events` come in
    /// time order, each at its frame in the block.
    ///
    /// Returns whether the host may let it sleep.
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus;

    /// Takes parameter events while the host isn't processing, with no
    /// audio to render: apply each value at once.
    fn flush(&mut self, events: &Events<'_>, cx: &mut ProcessContext<'_>);

    /// Forgets everything it was playing: voices, envelopes, delay lines.
    fn reset(&mut self) {}

    /// How long its output goes on after its input falls silent.
    fn tail(&self) -> Tail {
        Tail::None
    }
}

/// What a plug-in is, as a host lists it.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PluginInfo {
    /// A reverse-DNS ID that never changes, such as
    /// `com.loombridge.reagent`.
    pub id: &'static str,
    /// Its name.
    pub name: &'static str,
    /// Who makes it.
    pub vendor: &'static str,
    /// Its version, such as `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
    /// Its web page.
    pub url: &'static str,
    /// One sentence on what it does.
    pub description: &'static str,
    /// What kind of plug-in it is, from [`features`], which hosts use to
    /// sort it.
    pub features: &'static [&'static CStr],
    /// Its main audio input and output.
    pub audio: AudioLayout,
    /// Whether it takes notes in, and sends notes out.
    pub notes: NotePorts,
}

impl PluginInfo {
    /// A plug-in with ID `id` and name `name`, a stereo effect with no notes
    /// until said otherwise.
    #[must_use]
    pub const fn new(id: &'static str, name: &'static str) -> Self {
        Self {
            id,
            name,
            vendor: "",
            version: "0.0.0",
            url: "",
            description: "",
            features: &[],
            audio: AudioLayout::effect(2),
            notes: NotePorts::NONE,
        }
    }

    /// Who makes it.
    #[must_use]
    pub const fn vendor(mut self, vendor: &'static str) -> Self {
        self.vendor = vendor;
        self
    }

    /// Its version.
    #[must_use]
    pub const fn version(mut self, version: &'static str) -> Self {
        self.version = version;
        self
    }

    /// Its web page.
    #[must_use]
    pub const fn url(mut self, url: &'static str) -> Self {
        self.url = url;
        self
    }

    /// One sentence on what it does.
    #[must_use]
    pub const fn description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    /// What kind of plug-in it is.
    #[must_use]
    pub const fn features(mut self, features: &'static [&'static CStr]) -> Self {
        self.features = features;
        self
    }

    /// Its main audio input and output.
    #[must_use]
    pub const fn audio(mut self, audio: AudioLayout) -> Self {
        self.audio = audio;
        self
    }

    /// Whether it takes and sends notes.
    #[must_use]
    pub const fn notes(mut self, notes: NotePorts) -> Self {
        self.notes = notes;
        self
    }
}

/// The channels of a plug-in's main audio input and output, at most
/// [`MAX_CHANNELS`] each; 0 means it has none. Side-chain inputs come
/// later, as more fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AudioLayout {
    /// Channels in.
    pub inputs: u16,
    /// Channels out.
    pub outputs: u16,
}

impl AudioLayout {
    /// An effect: `channels` in and out.
    #[must_use]
    pub const fn effect(channels: u16) -> Self {
        Self {
            inputs: channels,
            outputs: channels,
        }
    }

    /// An instrument: no audio in, `channels` out.
    #[must_use]
    pub const fn instrument(channels: u16) -> Self {
        Self {
            inputs: 0,
            outputs: channels,
        }
    }
}

/// Whether a plug-in takes notes in and sends notes out, each through one
/// port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct NotePorts {
    /// It takes notes.
    pub input: bool,
    /// It sends notes.
    pub output: bool,
}

impl NotePorts {
    /// No notes either way.
    pub const NONE: Self = Self {
        input: false,
        output: false,
    };
    /// Notes in, as an instrument takes them.
    pub const INPUT: Self = Self {
        input: true,
        output: false,
    };
    /// Notes in and out, as a note effect has.
    pub const BOTH: Self = Self {
        input: true,
        output: true,
    };
}

/// The standard feature names a host sorts plug-ins by (CLAP's, which
/// `clap-wrapper` maps to VST3's and Audio Units' categories).
pub mod features {
    use std::ffi::CStr;

    /// Plays notes.
    pub const INSTRUMENT: &CStr = c"instrument";
    /// Processes audio.
    pub const AUDIO_EFFECT: &CStr = c"audio-effect";
    /// Processes notes.
    pub const NOTE_EFFECT: &CStr = c"note-effect";
    /// Analyses audio.
    pub const ANALYZER: &CStr = c"analyzer";
    /// A synthesiser.
    pub const SYNTHESIZER: &CStr = c"synthesizer";
    /// A sampler.
    pub const SAMPLER: &CStr = c"sampler";
    /// A filter.
    pub const FILTER: &CStr = c"filter";
    /// A delay.
    pub const DELAY: &CStr = c"delay";
    /// A reverb.
    pub const REVERB: &CStr = c"reverb";
    /// A distortion.
    pub const DISTORTION: &CStr = c"distortion";
    /// A compressor.
    pub const COMPRESSOR: &CStr = c"compressor";
    /// An equaliser.
    pub const EQUALIZER: &CStr = c"equalizer";
    /// A utility, such as a gain.
    pub const UTILITY: &CStr = c"utility";
    /// Mono audio.
    pub const MONO: &CStr = c"mono";
    /// Stereo audio.
    pub const STEREO: &CStr = c"stereo";
}

/// The audio configuration a processor is made for.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ProcessConfig {
    /// Frames per second.
    pub sample_rate: f64,
    /// The fewest frames a block will hold.
    pub min_frames: u32,
    /// The most frames a block will hold.
    pub max_frames: u32,
}

impl ProcessConfig {
    /// A configuration at `sample_rate`, with blocks of 1 to `max_frames`
    /// frames.
    #[must_use]
    pub const fn new(sample_rate: f64, max_frames: u32) -> Self {
        Self {
            sample_rate,
            min_frames: 1,
            max_frames,
        }
    }
}

/// Whether a processor needs the next block, as [`Processor::process`]
/// tells the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Call it again, whatever the input.
    Continue,
    /// Call it again while its input isn't silent.
    ContinueIfNotQuiet,
    /// Call it again until its [tail](Processor::tail) has played out.
    Tail,
    /// It's asleep: its output is silent and stays so until an event or
    /// input that isn't silent arrives, so the host may stop calling it
    /// until then.
    Sleep,
}

/// How long a processor's output goes on after its input falls silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tail {
    /// It stops at once.
    None,
    /// It goes on for this many frames.
    Frames(u32),
    /// It may go on for ever, as a looping delay can.
    Infinite,
}

/// Why a plug-in couldn't do what it was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError {
    message: String,
}

impl PluginError {
    /// An error saying `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// What went wrong.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}
