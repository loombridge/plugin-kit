//! The events a processor receives: notes and parameter changes, each at
//! its frame in the block.

use lspk_params::ParamId;

/// Which note an event is for. A field left `None` matches every value, so
/// a note-off with no key ends every note on its channel, as CLAP's
/// wildcards do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Note {
    /// The note port it came through.
    pub port: Option<u16>,
    /// The MIDI channel, from 0 to 15.
    pub channel: Option<u8>,
    /// The key, from 0 to 127, where 60 is middle C.
    pub key: Option<u8>,
    /// The host's ID for this one note, where it gives one.
    pub id: Option<u32>,
}

impl Note {
    /// Key `key` on channel `channel`, port 0, with no note ID.
    #[must_use]
    pub const fn key(channel: u8, key: u8) -> Self {
        Self {
            port: Some(0),
            channel: Some(channel),
            key: Some(key),
            id: None,
        }
    }

    /// Whether this note, as an event names it, includes `voice`: every
    /// field it names matches.
    #[must_use]
    pub fn matches(&self, voice: &Note) -> bool {
        fn field<T: PartialEq>(pattern: Option<T>, value: Option<T>) -> bool {
            pattern.is_none() || value.is_none() || pattern == value
        }
        field(self.port, voice.port)
            && field(self.channel, voice.channel)
            && field(self.key, voice.key)
            && field(self.id, voice.id)
    }
}

/// Something that happens at a frame in a block.
///
/// More kinds will come (per-note expression, polyphonic modulation), so a
/// processor matches the ones it handles and ignores the rest.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A note starts.
    NoteOn {
        /// Its frame in the block.
        time: u32,
        /// Which note.
        note: Note,
        /// How hard, from 0 to 1.
        velocity: f32,
    },
    /// A note is released; its voice may go on through its release.
    NoteOff {
        /// Its frame in the block.
        time: u32,
        /// Which notes.
        note: Note,
        /// How fast it was released, from 0 to 1.
        velocity: f32,
    },
    /// A note stops at once, with no release.
    NoteChoke {
        /// Its frame in the block.
        time: u32,
        /// Which notes.
        note: Note,
    },
    /// A parameter takes a new value, in its own units.
    Param {
        /// Its frame in the block.
        time: u32,
        /// Which parameter.
        id: ParamId,
        /// Its new value.
        value: f64,
    },
}

impl Event {
    /// The event's frame in its block.
    #[must_use]
    pub fn time(&self) -> u32 {
        match *self {
            Self::NoteOn { time, .. }
            | Self::NoteOff { time, .. }
            | Self::NoteChoke { time, .. }
            | Self::Param { time, .. } => time,
        }
    }
}

/// Where a block's events come from: a format's own event list, read
/// without copying.
pub trait EventSource {
    /// How many entries it has, including ones that aren't events the kit
    /// passes on.
    fn len(&self) -> usize;

    /// Whether it has no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The entry at `index` as an [`Event`], or `None` if it isn't one the
    /// kit passes on.
    fn get(&self, index: usize) -> Option<Event>;
}

/// A source with no entries.
struct NoEvents;

impl EventSource for NoEvents {
    fn len(&self) -> usize {
        0
    }

    fn get(&self, _index: usize) -> Option<Event> {
        None
    }
}

/// A block's events, in time order: the plug-in's own parameter edits from
/// the main thread, at frame 0, then the host's.
#[derive(Clone, Copy)]
pub struct Events<'a> {
    first: &'a [Event],
    rest: &'a dyn EventSource,
}

impl<'a> Events<'a> {
    /// `first`, then `rest`. Both must be in time order, and every event in
    /// `first` must be no later than the first in `rest`.
    #[must_use]
    pub fn new(first: &'a [Event], rest: &'a dyn EventSource) -> Self {
        Self { first, rest }
    }

    /// The events in `events`, which must be in time order.
    #[must_use]
    pub fn from_slice(events: &'a [Event]) -> Self {
        Self {
            first: events,
            rest: &NoEvents,
        }
    }

    /// No events.
    #[must_use]
    pub fn none() -> Self {
        Self::from_slice(&[])
    }

    /// Every event, in time order.
    pub fn iter(&self) -> impl Iterator<Item = Event> + 'a {
        let rest = self.rest;
        self.first
            .iter()
            .copied()
            .chain((0..rest.len()).filter_map(move |i| rest.get(i)))
    }
}

impl std::fmt::Debug for Events<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_match_every_value() {
        let voice = Note {
            id: Some(4),
            ..Note::key(0, 60)
        };
        assert!(Note::key(0, 60).matches(&voice));
        assert!(!Note::key(1, 60).matches(&voice));
        let all_on_channel = Note {
            channel: Some(0),
            ..Note::default()
        };
        assert!(all_on_channel.matches(&voice));
        let by_id = Note {
            id: Some(5),
            ..Note::default()
        };
        assert!(!by_id.matches(&voice));
    }

    /// A source that holds a non-event entry, as a format's lists do.
    struct Gappy;

    impl EventSource for Gappy {
        fn len(&self) -> usize {
            3
        }

        fn get(&self, index: usize) -> Option<Event> {
            (index != 1).then_some(Event::NoteChoke {
                time: 10 + index as u32,
                note: Note::default(),
            })
        }
    }

    #[test]
    fn edits_come_first_and_gaps_are_skipped() {
        let first = [Event::Param {
            time: 0,
            id: ParamId(1),
            value: 0.5,
        }];
        let events = Events::new(&first, &Gappy);
        let times: Vec<u32> = events.iter().map(|e| e.time()).collect();
        assert_eq!(times, [0, 10, 12]);
        assert_eq!(Events::none().iter().count(), 0);
        assert_eq!(Events::from_slice(&first).iter().count(), 1);
    }
}
