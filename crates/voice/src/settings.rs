//! How the voices are played: polyphony, stealing, mono and legato, glide,
//! pitch bend and MPE.

/// The most voices a [`Voices`](crate::Voices) can hold, and the highest
/// [`Settings::voices`] limit.
pub const MAX_VOICES: usize = 32;

/// Which voices are kept when a new note needs one and the limit is
/// reached (engine-core R122). Ties under [`Highest`](Self::Highest),
/// [`Lowest`](Self::Lowest) and [`Velocity`](Self::Velocity) fall back to
/// [`Newest`](Self::Newest).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StealPriority {
    /// Keep the newest notes: the oldest is stolen.
    #[default]
    Newest,
    /// Keep the oldest notes: the newest is stolen.
    Oldest,
    /// Keep the highest notes: the lowest is stolen.
    Highest,
    /// Keep the lowest notes: the highest is stolen.
    Lowest,
    /// Keep the hardest-played notes: the softest is stolen.
    Velocity,
}

/// Whether a repeated note reuses the voice already playing that key
/// (engine-core R120), instead of stacking a second voice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SameNote {
    /// A repeated note gets a voice of its own. Re-striking a note the
    /// sustain pedal holds still releases the held one (R121).
    #[default]
    Off,
    /// A repeated note reuses its key's voice while the sustain pedal holds
    /// it, and otherwise gets a voice of its own.
    Sustain,
    /// A repeated note always reuses its key's voice, whether held,
    /// sustained or in its release.
    On,
}

/// Portamento: how a new note's pitch moves from the last one
/// (engine-core R123 to R126).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glide {
    /// Seconds, from 0 (no glide) to 8. With [`scaled`](Self::scaled), the
    /// time for an octave.
    pub time: f32,
    /// From −1 to +1: 0 moves linearly in semitones, above 0 leaves quickly
    /// and slows near the target, below 0 leaves slowly and arrives quickly;
    /// ±1 is almost a step.
    pub curve: f32,
    /// Glide to every new note from the last pitch, not only while a note
    /// is held.
    pub always: bool,
    /// Make the time proportional to the interval, [`time`](Self::time)
    /// being an octave's.
    pub scaled: bool,
}

impl Default for Glide {
    fn default() -> Self {
        Self {
            time: 0.0,
            curve: 0.0,
            always: false,
            scaled: false,
        }
    }
}

/// MPE: per-note pitch bend, pressure and timbre from each note's own MIDI
/// channel (engine-core R128).
///
/// Channel 1 (index 0) is the lower zone's master channel: its pitch bend
/// bends every note with [`Settings::bend_up`] and [`Settings::bend_down`],
/// and its controllers are global. Channels 2 to 16 are member channels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mpe {
    /// Whether MPE is on.
    pub enabled: bool,
    /// A member channel's full pitch bend, in semitones, from 1 to 96.
    pub bend_range: f32,
}

impl Default for Mpe {
    fn default() -> Self {
        Self {
            enabled: false,
            bend_range: 48.0,
        }
    }
}

/// How the voices are played. Change it between blocks, or from a
/// [`VoiceHandler`](crate::VoiceHandler) at an event's frame; a change
/// applies from the next note, and to pitch within one control interval
/// ([`CONTROL_INTERVAL`](crate::CONTROL_INTERVAL)).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// How many notes may sound at once, from 1 to [`MAX_VOICES`]
    /// (engine-core R117). Unison copies don't count (R39), and it doesn't
    /// apply in mono.
    pub voices: usize,
    /// Which voices a new note steals at the limit (R122).
    pub steal: StealPriority,
    /// Whether a repeated note reuses its key's voice (R120).
    pub same_note: SameNote,
    /// One note at a time (R118).
    pub mono: bool,
    /// In mono, a note played over a held one re-pitches without
    /// retriggering (R118); with mono off, the instrument is paraphonic
    /// (R119).
    pub legato: bool,
    /// Whether a legato note updates the voice's velocity (R101).
    pub velocity_legato: bool,
    /// Portamento.
    pub glide: Glide,
    /// Full upward pitch bend, in semitones, from −24 to +24 (R127).
    pub bend_up: f32,
    /// Full downward pitch bend, in semitones, from −24 to +24; negative
    /// bends down (R127).
    pub bend_down: f32,
    /// MPE.
    pub mpe: Mpe,
    /// How long, in seconds, pitch bend and tuning take to reach a new
    /// value (R127's smoothing); 0 jumps.
    pub smoothing: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voices: 8,
            steal: StealPriority::Newest,
            same_note: SameNote::Off,
            mono: false,
            legato: false,
            velocity_legato: false,
            glide: Glide::default(),
            bend_up: 2.0,
            bend_down: -2.0,
            mpe: Mpe::default(),
            smoothing: 0.005,
        }
    }
}
