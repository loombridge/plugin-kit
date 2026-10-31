//! The engine's live values for an interface: what a plug-in's audio thread
//! publishes to its editor while the editor is open.
//!
//! A [`Frame`] holds what an interface draws that the parameters don't say:
//! each destination's modulated value per voice (a modulation ring), voice
//! and LFO phases, envelope stages, output levels, and a scope and spectrum
//! of the output. [`channel`] makes the wait-free pair: the audio thread
//! asks its [`Publisher`] whether to publish ([`Publisher::due`]), fills
//! the frame in place ([`Frame::set_modulated`] and the public fields), and
//! the editor reads the latest from its [`Watcher`]. Nothing is written
//! while no editor watches ([`Watcher::watch`]), and the frame's buffers
//! are made once, so publishing allocates nothing.

use lspk_dsp::envelope::Stage;
pub use lspk_rt::telemetry::{Publisher, Watcher};

use crate::dest::DestId;
use crate::engine::Engine;

/// The frames between publications: 512, about 94 a second at 48 kHz, more
/// than an interface redraws.
pub const INTERVAL: usize = 512;

/// The sizes of a [`Frame`]'s lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Destinations in the plug-in's registry.
    pub destinations: usize,
    /// Voices the engine has.
    pub voices: usize,
    /// LFO slots.
    pub lfos: usize,
    /// Envelope slots.
    pub envelopes: usize,
    /// Output channels metered.
    pub channels: usize,
    /// Samples in the scope.
    pub scope: usize,
    /// Bins in the spectrum.
    pub bins: usize,
}

/// One publication of the engine's live values.
///
/// The audio thread fills a slot that holds an older frame, so it sets every
/// list. Lists indexed by voice use the layout's voice count as their
/// stride.
#[derive(Clone, Debug)]
pub struct Frame {
    /// The sizes the lists were made for.
    pub layout: Layout,
    /// The engine's clock, in host frames, when it was published.
    pub clock: u64,
    /// Which voices are playing.
    pub active: Vec<bool>,
    /// Each destination's modulated value per voice, as the matrix gives it;
    /// NaN where no route reaches it or the voice isn't playing. Fill it
    /// with [`set_modulated`](Self::set_modulated).
    pub modulated: Vec<f32>,
    /// Each voice's oscillator phase, 0 up to 1.
    pub voice_phase: Vec<f32>,
    /// Each LFO's phase per voice, 0 up to 1.
    pub lfo_phase: Vec<f32>,
    /// Each envelope's stage per voice.
    pub env_stage: Vec<Stage>,
    /// Each output channel's peak since the last publication.
    pub levels: Vec<f32>,
    /// The last samples of the output, the oldest first.
    pub scope: Vec<f32>,
    /// The output's spectrum, DC to Nyquist, where a full-scale sine reads 1.
    pub spectrum: Vec<f32>,
}

impl Frame {
    /// A silent frame of `layout`. Allocates.
    #[must_use]
    pub fn new(layout: Layout) -> Self {
        let Layout {
            destinations,
            voices,
            lfos,
            envelopes,
            channels,
            scope,
            bins,
        } = layout;
        Self {
            layout,
            clock: 0,
            active: vec![false; voices],
            modulated: vec![f32::NAN; destinations * voices],
            voice_phase: vec![0.0; voices],
            lfo_phase: vec![0.0; lfos * voices],
            env_stage: vec![Stage::Idle; envelopes * voices],
            levels: vec![0.0; channels],
            scope: vec![0.0; scope],
            spectrum: vec![0.0; bins],
        }
    }

    /// Takes every destination's latest value from `engine`.
    pub fn set_modulated(&mut self, engine: &Engine) {
        engine.latest_all(self.layout.voices, &mut self.modulated);
    }

    /// `dest`'s modulated value for `voice`, or none where no route reaches
    /// it or the voice isn't playing.
    #[must_use]
    pub fn modulated_at(&self, dest: DestId, voice: usize) -> Option<f32> {
        let v = *self
            .modulated
            .get(dest.index() * self.layout.voices + voice)?;
        (!v.is_nan()).then_some(v)
    }
}

/// A wait-free channel of [`Frame`]s of `layout`, published every
/// [`INTERVAL`] frames while watched. Allocates; make it off the audio
/// thread.
#[must_use]
pub fn channel(layout: Layout) -> (Publisher<Frame>, Watcher<Frame>) {
    lspk_rt::telemetry::channel(&Frame::new(layout), INTERVAL)
}
