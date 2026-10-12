//! Polyphonic voice management for the kit's instruments: allocation and
//! stealing, mono, legato and paraphonic legato, portamento, the sustain
//! pedal, pitch bend, MPE and CLAP's per-note expressions, with every
//! voice's values in a structure-of-arrays [`VoiceBlock`] laid out as SIMD
//! lanes. Unison stacks each voice's detuned copies as more lanes per
//! voice ([`unison`]), and mixes them to a stereo pair per voice.
//!
//! An instrument owns a [`Voices`], hands it each block's events with
//! [`Voices::process`], and implements [`VoiceHandler`]: the voices call it
//! back to render the frames between events, sample-accurately, and to say
//! when a voice starts ([`Start`] says how: fresh, stolen, retriggered,
//! legato or joining a paraphonic envelope), is released or ends. A voice
//! ends when the instrument reports it silent ([`VoiceBlock::end`]), so
//! released voices cost nothing once their tails have died away.
//!
//! ```
//! use std::ops::Range;
//! use lspk_plugin::{Event, Note};
//! use lspk_voice::{Layout, VoiceBlock, VoiceEvent, VoiceEventKind, VoiceHandler, Voices};
//!
//! /// Ends a voice as soon as it's released.
//! struct Synth;
//!
//! impl VoiceHandler for Synth {
//!     fn voice_event(&mut self, voices: &mut VoiceBlock, event: VoiceEvent) {
//!         if event.kind == VoiceEventKind::Release {
//!             voices.end(event.voice);
//!         }
//!     }
//!     fn render(&mut self, voices: &mut VoiceBlock, frames: Range<usize>) {
//!         // Kernels read voices.pitch(), voices.velocity() ... over
//!         // 0..voices.span(lanes), for frames.
//!         let _ = (voices, frames);
//!     }
//! }
//!
//! let mut voices = Voices::new(Layout::default(), 48_000.0);
//! let note = Note::key(0, 60);
//! let events = [Event::NoteOn { time: 10, note, velocity: 0.8 }];
//! voices.process(events, 128, &mut Synth);
//! assert_eq!(voices.block().active_count(), 1);
//! assert_eq!(voices.block().pitch()[0], 60.0);
//! ```
//!
//! The rules it follows are the engine specification's section 14 (cited
//! as R117 to R131 in the tests), with R39 (unison copies don't count
//! against the voice limit) and R79 (a stolen voice's envelope restarts
//! from its current level, which [`Start::Steal`] tells the instrument).
//!
//! Construction allocates; nothing after it does, so processing is safe on
//! an audio thread.

mod block;
mod ramp;
mod settings;
pub mod unison;
mod voices;

pub use block::{Controllers, LANE_ALIGN, VoiceBlock, VoiceInfo, VoiceState};
pub use settings::{Glide, MAX_VOICES, Mpe, SameNote, Settings, StealPriority};
pub use unison::{DetuneLaw, Stack, Unison, UnisonLanes};
pub use voices::{
    CONTROL_INTERVAL, Layout, Start, VoiceEvent, VoiceEventKind, VoiceHandler, Voices,
};
