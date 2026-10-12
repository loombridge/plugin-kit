//! The voice manager: notes in, voices out, at the frame each event names.

use std::ops::Range;

use lspk_plugin::{Event, Expression, Note};

use crate::block::{
    DEFAULT_EXPRESSION, DEFAULT_PAN, DEFAULT_PRESSURE, DEFAULT_TIMBRE, DEFAULT_VIBRATO,
    DEFAULT_VOLUME, Lane, VoiceBlock, VoiceInfo, VoiceState,
};
use crate::ramp::Ramp;
use crate::settings::{MAX_VOICES, SameNote, Settings, StealPriority};

/// Pitch is computed exactly every this many frames of the running clock
/// and moves linearly between, so it doesn't depend on block size. Events
/// and blocks split render calls; control points do too.
pub const CONTROL_INTERVAL: u32 = 32;

/// How a voice starts a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Start {
    /// From silence: a free voice.
    Fresh,
    /// A sounding voice taken for a new note: at the limit (R122), or by a
    /// repeated note (R120). The amplifier envelope restarts from its
    /// current level (R79).
    Steal,
    /// In mono without legato, the sounding voice re-pitched and
    /// retriggered; the amplifier envelope restarts its attack from its
    /// current level (R118).
    Retrigger,
    /// In mono legato, the sounding voice re-pitched without retriggering
    /// envelopes or LFOs (R118).
    Legato,
    /// Paraphonic legato (legato with mono off): a new voice that joins
    /// `leader`'s envelope at its current level and follows it, keeping its
    /// own pitch (R119).
    Join {
        /// The voice whose envelope it follows: the newest held one.
        leader: usize,
    },
}

/// What happens to a voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceEventKind {
    /// It starts playing a note. `replaced` is the note it was playing
    /// before, if it was a different one, which has now ended for the host
    /// (CLAP's note-end).
    Start {
        /// How it starts.
        start: Start,
        /// The note it played before, if any and different.
        replaced: Option<Note>,
    },
    /// Its note is released: its gate closes and it plays its release.
    Release,
    /// It has ended and is free: reported silent ([`VoiceBlock::end`]),
    /// choked, or silenced by "all sound off". Tell the host `note` ended.
    End {
        /// The note it played.
        note: Note,
    },
}

/// Something that happens to a voice at a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceEvent {
    /// The frame in the block.
    pub time: u32,
    /// Which voice.
    pub voice: usize,
    /// What happens.
    pub kind: VoiceEventKind,
}

/// A synthesiser, as [`Voices::process`] drives it.
pub trait VoiceHandler {
    /// A voice starts, is released or ends, at `event.time`. The voice
    /// block already holds its new values.
    fn voice_event(&mut self, voices: &mut VoiceBlock, event: VoiceEvent);

    /// Renders `frames` (a range of the block) for the active voices, whose
    /// values hold through the range (pitch moving by its step).
    fn render(&mut self, voices: &mut VoiceBlock, frames: Range<usize>);

    /// An event the voices don't consume (a parameter change, say), at its
    /// own frame, between render calls.
    fn event(&mut self, voices: &mut VoiceBlock, event: Event) {
        let _ = (voices, event);
    }
}

/// How many voices, and how many lanes each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Voices to hold, from 1 to [`MAX_VOICES`].
    pub voices: usize,
    /// Lanes per voice: its unison copies, at most 16.
    pub lanes_per_voice: usize,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            voices: MAX_VOICES,
            lanes_per_voice: 1,
        }
    }
}

/// A voice's state beyond what the block shows.
#[derive(Clone, Copy, Debug)]
struct Slot {
    velocity: f32,
    glide: Ramp,
    /// MPE per-note bend, from −1 to +1.
    note_bend: Ramp,
    /// CLAP tuning, in semitones.
    tuning: Ramp,
    seg_pitch: f32,
    seg_slope: f32,
    seg_start: u64,
    /// The exact pitch at the segment's end, `seg_end`: the next segment's
    /// start, unless the voice changes first.
    seg_end_pitch: f32,
    seg_end: u64,
    dirty: bool,
}

impl Slot {
    const IDLE: Self = Self {
        velocity: 0.0,
        glide: Ramp::at(0.0),
        note_bend: Ramp::at(0.0),
        tuning: Ramp::at(0.0),
        seg_pitch: 0.0,
        seg_slope: 0.0,
        seg_start: 0,
        seg_end_pitch: 0.0,
        seg_end: u64::MAX,
        dirty: true,
    };
}

/// A held key in mono's note stack.
#[derive(Clone, Copy, Debug)]
struct Held {
    note: Note,
    key: u8,
    velocity: f32,
    sustained: bool,
}

/// MPE's per-channel values, which a note on that channel starts with.
#[derive(Clone, Copy, Debug)]
struct Channel {
    bend: f32,
    pressure: f32,
    timbre: f32,
}

impl Channel {
    const RESET: Self = Self {
        bend: 0.0,
        pressure: DEFAULT_PRESSURE,
        timbre: DEFAULT_TIMBRE,
    };
}

/// The longest mono note stack: one entry per key.
const STACK: usize = 128;

/// Polyphonic voice management: allocation and stealing, mono and legato,
/// glide, sustain, pitch bend, MPE and CLAP's per-note expressions, with
/// every voice's values in a [`VoiceBlock`].
///
/// Construction allocates; nothing after it does, so
/// [`process`](Self::process) is safe on an audio thread.
#[derive(Debug)]
pub struct Voices {
    block: VoiceBlock,
    slots: [Slot; MAX_VOICES],
    stack: [Held; STACK],
    stack_len: usize,
    mono_voice: Option<usize>,
    channels: [Channel; 16],
    bend: Ramp,
    clock: u64,
    next_age: u64,
    sample_rate: f32,
    last: Option<(usize, u64)>,
    last_key: Option<f32>,
}

impl Voices {
    /// Voices for `layout` at `sample_rate`, with default settings.
    ///
    /// # Panics
    ///
    /// If `layout.voices` isn't 1 to [`MAX_VOICES`], or
    /// `layout.lanes_per_voice` isn't 1 to 16.
    #[must_use]
    pub fn new(layout: Layout, sample_rate: f32) -> Self {
        assert!(
            (1..=MAX_VOICES).contains(&layout.voices),
            "voices must be 1 to {MAX_VOICES}"
        );
        assert!(
            (1..=16).contains(&layout.lanes_per_voice),
            "lanes per voice must be 1 to 16"
        );
        let idle = Held {
            note: Note::default(),
            key: 0,
            velocity: 0.0,
            sustained: false,
        };
        Self {
            block: VoiceBlock::new(layout.voices, layout.lanes_per_voice),
            slots: [Slot::IDLE; MAX_VOICES],
            stack: [idle; STACK],
            stack_len: 0,
            mono_voice: None,
            channels: [Channel::RESET; 16],
            bend: Ramp::at(0.0),
            clock: 0,
            next_age: 1,
            sample_rate,
            last: None,
            last_key: None,
        }
    }

    /// Changes the sample rate, which glide and smoothing times are counted
    /// in. Allocates nothing.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
    }

    /// Frees every voice at once and resets every controller, without
    /// events: for a reset or a deactivation.
    pub fn reset(&mut self) {
        for voice in self.block.active_voices() {
            self.free(voice);
        }
        self.block.controllers = crate::block::Controllers::default();
        self.stack_len = 0;
        self.channels = [Channel::RESET; 16];
        self.bend = Ramp::at(0.0);
        self.last = None;
        self.last_key = None;
    }

    /// The voices' values.
    #[must_use]
    pub const fn block(&self) -> &VoiceBlock {
        &self.block
    }

    /// How the voices are played.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.block.settings
    }

    /// How the voices are played, to change.
    pub const fn settings_mut(&mut self) -> &mut Settings {
        &mut self.block.settings
    }

    /// Frames processed since construction: the clock control points and
    /// glides are counted on.
    #[must_use]
    pub const fn clock(&self) -> u64 {
        self.clock
    }

    /// Plays one block of `frames` frames: each event at its frame (events
    /// must be in time order; a time past the block counts as its end),
    /// with `handler` rendering the frames between and hearing what happens
    /// to each voice.
    pub fn process<H: VoiceHandler>(
        &mut self,
        events: impl IntoIterator<Item = Event>,
        frames: usize,
        handler: &mut H,
    ) {
        let mut now = 0;
        for event in events {
            let time = (event.time() as usize).clamp(now, frames);
            self.render(now, time, handler);
            now = time;
            self.handle(event, time, handler);
            self.collect_ends(time, handler);
        }
        self.render(now, frames, handler);
        self.clock += frames as u64;
    }

    /// Renders frames `from..to` in pieces split at control points.
    fn render<H: VoiceHandler>(&mut self, from: usize, to: usize, handler: &mut H) {
        let interval = u64::from(CONTROL_INTERVAL);
        let mut at = from;
        while at < to {
            let now = self.clock + at as u64;
            let next = (now / interval + 1) * interval;
            let end = to.min(at + (next - now) as usize);
            self.update_pitch(now, next);
            handler.render(&mut self.block, at..end);
            self.collect_ends(end, handler);
            at = end;
        }
    }

    /// Writes each active voice's pitch at frame `now`, starting a new
    /// linear segment towards control point `next` where `now` is a control
    /// point or the voice changed.
    fn update_pitch(&mut self, now: u64, next: u64) {
        let at_point = now.is_multiple_of(u64::from(CONTROL_INTERVAL));
        for voice in self.block.active_voices() {
            let slot = &self.slots[voice];
            if at_point || slot.dirty {
                // The last segment's end is this one's start, already
                // computed, unless the voice has changed since.
                let p0 = if !slot.dirty && slot.seg_end == now {
                    slot.seg_end_pitch
                } else {
                    self.pitch_at(voice, now)
                };
                let p1 = self.pitch_at(voice, next);
                let slot = &mut self.slots[voice];
                slot.seg_pitch = p0;
                slot.seg_slope = (p1 - p0) / (next - now) as f32;
                slot.seg_start = now;
                slot.seg_end_pitch = p1;
                slot.seg_end = next;
                slot.dirty = false;
            }
            let slot = &self.slots[voice];
            let pitch = slot.seg_pitch + slot.seg_slope * (now - slot.seg_start) as f32;
            self.block.write_pitch(voice, pitch, slot.seg_slope);
        }
    }

    /// Voice `voice`'s exact pitch at frame `t`.
    fn pitch_at(&self, voice: usize, t: u64) -> f32 {
        let settings = &self.block.settings;
        let slot = &self.slots[voice];
        let bend = self.bend.value(t);
        let bend = if bend >= 0.0 {
            bend * settings.bend_up
        } else {
            -bend * settings.bend_down
        };
        let note_bend = if settings.mpe.enabled {
            slot.note_bend.value(t) * settings.mpe.bend_range
        } else {
            0.0
        };
        slot.glide.value(t) + bend + note_bend + slot.tuning.value(t)
    }

    /// Frees the voices reported silent or choked, telling `handler`.
    fn collect_ends<H: VoiceHandler>(&mut self, time: usize, handler: &mut H) {
        while self.block.ending != 0 {
            let voice = self.block.ending.trailing_zeros() as usize;
            self.block.ending &= !(1 << voice);
            let note = self.block.info[voice].note;
            self.free(voice);
            handler.voice_event(
                &mut self.block,
                VoiceEvent {
                    time: time as u32,
                    voice,
                    kind: VoiceEventKind::End { note },
                },
            );
        }
    }

    fn free(&mut self, voice: usize) {
        self.block.active_mask &= !(1 << voice);
        self.block.ending &= !(1 << voice);
        self.block.info[voice].state = VoiceState::Free;
        self.block.write(Lane::Active, voice, 0.0);
        self.block.write(Lane::Gate, voice, 0.0);
        if self.mono_voice == Some(voice) {
            self.mono_voice = None;
        }
    }

    /// Handles one event at frame `time`.
    fn handle<H: VoiceHandler>(&mut self, event: Event, time: usize, handler: &mut H) {
        let now = self.clock + time as u64;
        match event {
            Event::NoteOn { note, velocity, .. } => {
                if let Some(key) = note.key {
                    let note = Note {
                        key: Some(key),
                        ..note
                    };
                    if self.block.settings.mono {
                        self.mono_on(note, key, velocity, now, time, handler);
                    } else {
                        self.poly_on(note, key, velocity, now, time, handler);
                    }
                }
            }
            Event::NoteOff { note, velocity, .. } => {
                self.note_off(&note, velocity, now, time, handler);
            }
            Event::NoteChoke { note, .. } => {
                for voice in self.block.active_voices() {
                    if note.matches(&self.block.info[voice].note) {
                        self.block.ending |= 1 << voice;
                    }
                }
                self.stack_retain(|held| !note.matches(&held.note));
            }
            Event::NoteExpression {
                note,
                expression,
                value,
                ..
            } => self.expression(&note, expression, value as f32, now),
            Event::Midi { data, .. } => self.midi(data, now, time, handler),
            other => handler.event(&mut self.block, other),
        }
    }

    /// Smoothing, in frames.
    fn smoothing(&self) -> u32 {
        (self.block.settings.smoothing.max(0.0) * self.sample_rate) as u32
    }

    /// Whether any voice's key is down or sustained.
    fn any_gated(&self) -> bool {
        self.block
            .active_voices()
            .any(|v| self.block.info[v].state.is_gated())
    }

    /// The pitch a new note glides from: the last note's current pitch.
    fn last_pitch(&self, now: u64) -> Option<f32> {
        match self.last {
            Some((voice, age))
                if self.block.active_mask & (1 << voice) != 0
                    && self.block.info[voice].age == age =>
            {
                Some(self.slots[voice].glide.value(now))
            }
            _ => self.last_key,
        }
    }

    /// The pitch a new note glides from, if it glides: `held` says whether
    /// a note is held, which legato glide needs (R124).
    fn glide_from(&self, held: bool, from: Option<f32>) -> Option<f32> {
        let glide = &self.block.settings.glide;
        (glide.time > 0.0 && (glide.always || held))
            .then_some(from)
            .flatten()
    }

    fn poly_on<H: VoiceHandler>(
        &mut self,
        note: Note,
        key: u8,
        velocity: f32,
        now: u64,
        time: usize,
        handler: &mut H,
    ) {
        let settings = self.block.settings;
        // A repeated note: reuse its key's voice, or release a sustained one
        // (R120, R121).
        let mut reuse = None;
        for voice in self.block.active_voices() {
            let info = self.block.info[voice];
            if info.key != key || info.note.port != note.port {
                continue;
            }
            let sustained = info.state == VoiceState::Sustained;
            let take = match settings.same_note {
                SameNote::On => true,
                SameNote::Sustain => sustained,
                SameNote::Off => false,
            };
            if take {
                if reuse.is_none_or(|v: usize| self.block.info[v].age < info.age) {
                    reuse = Some(voice);
                }
            } else if sustained {
                self.release(voice, 0.0, time, handler);
            }
        }
        let held = self.any_gated();
        let leader = (settings.legato && held)
            .then(|| self.newest(|state| state.is_gated()))
            .flatten();
        let from = self.glide_from(held, self.last_pitch(now));
        let (voice, start) = if let Some(voice) = reuse {
            (voice, Start::Steal)
        } else {
            let count = self.block.active_count();
            let free = (!self.block.active_mask).trailing_zeros() as usize;
            if count < settings.voices.clamp(1, MAX_VOICES) && free < self.block.capacity {
                (free, Start::Fresh)
            } else {
                (self.victim(settings.steal), Start::Steal)
            }
        };
        let start = match leader {
            Some(leader) if leader != voice => Start::Join { leader },
            _ => start,
        };
        self.start(voice, note, key, velocity, start, from, now, time, handler);
    }

    /// The newest active voice whose state passes `filter`.
    fn newest(&self, filter: impl Fn(VoiceState) -> bool) -> Option<usize> {
        self.block
            .active_voices()
            .filter(|v| filter(self.block.info[*v].state))
            .max_by_key(|v| self.block.info[*v].age)
    }

    /// The voice a new note steals under `priority`: a released voice if
    /// there is one, then by priority, ties going to the oldest (R122).
    fn victim(&self, priority: StealPriority) -> usize {
        let info = &self.block.info;
        let released = self
            .block
            .active_voices()
            .any(|v| info[v].state == VoiceState::Released);
        let candidates = self
            .block
            .active_voices()
            .filter(|v| !released || info[*v].state == VoiceState::Released);
        // The smallest key is stolen; age breaks ties, oldest first.
        let rank = |v: &usize| -> (i64, u64) {
            let i = &info[*v];
            let primary = match priority {
                StealPriority::Newest => 0,
                // The newest is stolen: rank by age, newest smallest.
                StealPriority::Oldest => return (0, u64::MAX - i.age),
                StealPriority::Highest => i64::from(i.key),
                StealPriority::Lowest => -i64::from(i.key),
                StealPriority::Velocity => (self.slots[*v].velocity * 1_000_000.0) as i64,
            };
            (primary, i.age)
        };
        candidates.min_by_key(rank).unwrap_or(0)
    }

    fn mono_on<H: VoiceHandler>(
        &mut self,
        note: Note,
        key: u8,
        velocity: f32,
        now: u64,
        time: usize,
        handler: &mut H,
    ) {
        let held = self.stack_len > 0;
        self.stack_retain(|h| !(h.key == key && h.note.channel == note.channel));
        self.stack_push(Held {
            note,
            key,
            velocity,
            sustained: false,
        });
        let legato = self.block.settings.legato;
        match self.mono_voice {
            Some(voice) => {
                let start = if held && legato {
                    Start::Legato
                } else {
                    Start::Retrigger
                };
                let from = self.glide_from(held, Some(self.slots[voice].glide.value(now)));
                self.start(voice, note, key, velocity, start, from, now, time, handler);
            }
            None => {
                let free = (!self.block.active_mask).trailing_zeros() as usize;
                let (voice, start) = if free < self.block.capacity {
                    (free, Start::Fresh)
                } else {
                    (self.victim(self.block.settings.steal), Start::Steal)
                };
                let from = self.glide_from(held, self.last_pitch(now));
                self.start(voice, note, key, velocity, start, from, now, time, handler);
                self.mono_voice = Some(voice);
            }
        }
    }

    /// Starts `voice` playing `note`.
    #[allow(clippy::too_many_arguments, reason = "one internal call shape")]
    fn start<H: VoiceHandler>(
        &mut self,
        voice: usize,
        note: Note,
        key: u8,
        velocity: f32,
        start: Start,
        from: Option<f32>,
        now: u64,
        time: usize,
        handler: &mut H,
    ) {
        let settings = self.block.settings;
        let was_active = self.block.active_mask & (1 << voice) != 0;
        let old = self.block.info[voice].note;
        let replaced = (was_active && old != note).then_some(old);
        let target = f32::from(key);
        let glide = match from {
            Some(from) if from != target => {
                let octaves = if settings.glide.scaled {
                    (target - from).abs() / 12.0
                } else {
                    1.0
                };
                let len = (settings.glide.time * octaves * self.sample_rate) as u32;
                Ramp::new(
                    from,
                    target,
                    now,
                    len,
                    settings.glide.curve.clamp(-1.0, 1.0),
                )
            }
            _ => Ramp::at(target),
        };
        let channel = usize::from(note.channel.unwrap_or(0) & 0x0f);
        let mpe = settings.mpe.enabled && channel != 0;
        let fresh = if mpe {
            self.channels[channel]
        } else {
            Channel::RESET
        };
        let keep_velocity = start == Start::Legato && !settings.velocity_legato;
        let slot = &mut self.slots[voice];
        slot.glide = glide;
        slot.note_bend = Ramp::at(fresh.bend);
        slot.tuning = Ramp::at(0.0);
        slot.dirty = true;
        if !keep_velocity {
            slot.velocity = velocity;
        }
        let velocity = slot.velocity;
        self.next_age += 1;
        self.block.info[voice] = VoiceInfo {
            state: VoiceState::Held,
            note,
            key,
            age: self.next_age,
        };
        self.block.active_mask |= 1 << voice;
        self.block.ending &= !(1 << voice);
        // Fresh expression values at every note-on, stolen or legato (R130).
        for (lane, value) in [
            (Lane::Velocity, velocity),
            (Lane::ReleaseVelocity, 0.0),
            (Lane::Gate, 1.0),
            (Lane::Active, 1.0),
            (Lane::Pressure, fresh.pressure),
            (Lane::Timbre, fresh.timbre),
            (Lane::Volume, DEFAULT_VOLUME),
            (Lane::Pan, DEFAULT_PAN),
            (Lane::Vibrato, DEFAULT_VIBRATO),
            (Lane::Expression, DEFAULT_EXPRESSION),
        ] {
            self.block.write(lane, voice, value);
        }
        // The block shows the voice's pitch before the event is heard.
        let pitch = self.pitch_at(voice, now);
        self.block.write_pitch(voice, pitch, 0.0);
        self.last = Some((voice, self.next_age));
        self.last_key = Some(target);
        handler.voice_event(
            &mut self.block,
            VoiceEvent {
                time: time as u32,
                voice,
                kind: VoiceEventKind::Start { start, replaced },
            },
        );
    }

    /// Releases `voice`.
    fn release<H: VoiceHandler>(
        &mut self,
        voice: usize,
        velocity: f32,
        time: usize,
        handler: &mut H,
    ) {
        self.block.info[voice].state = VoiceState::Released;
        self.block.write(Lane::Gate, voice, 0.0);
        self.block.write(Lane::ReleaseVelocity, voice, velocity);
        handler.voice_event(
            &mut self.block,
            VoiceEvent {
                time: time as u32,
                voice,
                kind: VoiceEventKind::Release,
            },
        );
    }

    fn note_off<H: VoiceHandler>(
        &mut self,
        note: &Note,
        velocity: f32,
        now: u64,
        time: usize,
        handler: &mut H,
    ) {
        let sustain = self.block.controllers.sustain;
        if self.block.settings.mono {
            for held in &mut self.stack[..self.stack_len] {
                if note.matches(&held.note) {
                    held.sustained = true;
                }
            }
            if !sustain {
                self.stack_retain(|held| !held.sustained);
            }
            self.mono_follow(velocity, now, time, handler);
            return;
        }
        self.stack_retain(|held| !note.matches(&held.note));
        for voice in self.block.active_voices() {
            let info = self.block.info[voice];
            if info.state == VoiceState::Held && note.matches(&info.note) {
                if sustain {
                    self.block.info[voice].state = VoiceState::Sustained;
                } else {
                    self.release(voice, velocity, time, handler);
                }
            }
        }
    }

    /// In mono, after the stack changes: keeps playing the current note if
    /// it's still held, returns to the newest held key if not, and releases
    /// if no key is held.
    fn mono_follow<H: VoiceHandler>(
        &mut self,
        velocity: f32,
        now: u64,
        time: usize,
        handler: &mut H,
    ) {
        let Some(voice) = self.mono_voice else {
            return;
        };
        let info = self.block.info[voice];
        if !info.state.is_gated() {
            return;
        }
        let current = self.stack[..self.stack_len]
            .iter()
            .find(|held| held.note == info.note);
        if let Some(current) = current {
            self.block.info[voice].state = if current.sustained {
                VoiceState::Sustained
            } else {
                VoiceState::Held
            };
        } else if self.stack_len > 0 {
            let top = self.stack[self.stack_len - 1];
            let start = if self.block.settings.legato {
                Start::Legato
            } else {
                Start::Retrigger
            };
            let from = self.glide_from(true, Some(self.slots[voice].glide.value(now)));
            self.start(
                voice,
                top.note,
                top.key,
                top.velocity,
                start,
                from,
                now,
                time,
                handler,
            );
        } else {
            self.release(voice, velocity, time, handler);
        }
    }

    fn stack_push(&mut self, held: Held) {
        if self.stack_len == STACK {
            self.stack.copy_within(1.., 0);
            self.stack_len -= 1;
        }
        self.stack[self.stack_len] = held;
        self.stack_len += 1;
    }

    fn stack_retain(&mut self, keep: impl Fn(&Held) -> bool) {
        let mut kept = 0;
        for i in 0..self.stack_len {
            if keep(&self.stack[i]) {
                self.stack[kept] = self.stack[i];
                kept += 1;
            }
        }
        self.stack_len = kept;
    }

    fn sustain_pedal<H: VoiceHandler>(&mut self, down: bool, now: u64, time: usize, h: &mut H) {
        let was = self.block.controllers.sustain;
        self.block.controllers.sustain = down;
        if !was || down {
            return;
        }
        if self.block.settings.mono {
            self.stack_retain(|held| !held.sustained);
            self.mono_follow(0.0, now, time, h);
        }
        for voice in self.block.active_voices() {
            if self.block.info[voice].state == VoiceState::Sustained {
                self.release(voice, 0.0, time, h);
            }
        }
    }

    /// A CLAP note expression for the voices `note` names (R129).
    fn expression(&mut self, note: &Note, expression: Expression, value: f32, now: u64) {
        let smoothing = self.smoothing();
        for voice in self.block.active_voices() {
            if !note.matches(&self.block.info[voice].note) {
                continue;
            }
            let lane = match expression {
                Expression::Tuning => {
                    let slot = &mut self.slots[voice];
                    slot.tuning
                        .retarget(value.clamp(-120.0, 120.0), now, smoothing);
                    slot.dirty = true;
                    continue;
                }
                Expression::Volume => (Lane::Volume, value.clamp(0.0, 4.0)),
                Expression::Pan => (Lane::Pan, value.clamp(0.0, 1.0)),
                Expression::Vibrato => (Lane::Vibrato, value.clamp(0.0, 1.0)),
                Expression::Expression => (Lane::Expression, value.clamp(0.0, 1.0)),
                Expression::Brightness => (Lane::Timbre, value.clamp(0.0, 1.0)),
                Expression::Pressure => (Lane::Pressure, value.clamp(0.0, 1.0)),
            };
            self.block.write(lane.0, voice, lane.1);
        }
    }

    /// The active voices on MIDI channel `channel`.
    fn on_channel(&self, channel: u8) -> impl Iterator<Item = usize> + use<> {
        let info = self.block.info;
        self.block
            .active_voices()
            .filter(move |v| info[*v].note.channel == Some(channel))
    }

    /// A MIDI 1.0 channel message: pressure, controllers and pitch bend,
    /// per note on MPE's member channels (R128, R131).
    fn midi<H: VoiceHandler>(&mut self, data: [u8; 3], now: u64, time: usize, h: &mut H) {
        let channel = data[0] & 0x0f;
        let member = self.block.settings.mpe.enabled && channel != 0;
        let value = f32::from(data[2] & 0x7f) / 127.0;
        let ch = usize::from(channel);
        match data[0] & 0xf0 {
            // Polyphonic pressure.
            0xa0 => {
                for voice in self.on_channel(channel) {
                    if self.block.info[voice].key == data[1] {
                        self.block.write(Lane::Pressure, voice, value);
                    }
                }
            }
            0xb0 if member && data[1] == 74 => {
                self.channels[ch].timbre = value;
                for voice in self.on_channel(channel) {
                    self.block.write(Lane::Timbre, voice, value);
                }
            }
            0xb0 => {
                self.block.controllers.set_cc(data[1], value);
                match data[1] {
                    64 => self.sustain_pedal(data[2] >= 64, now, time, h),
                    // All sound off: every voice ends at once.
                    120 => {
                        self.block.ending |= self.block.active_mask;
                        self.stack_len = 0;
                    }
                    121 => self.reset_controllers(now, time, h),
                    // All notes off: as if every key were let go.
                    123 => self.note_off(&Note::default(), 0.0, now, time, h),
                    _ => {}
                }
            }
            // Channel pressure.
            0xd0 => {
                let value = f32::from(data[1] & 0x7f) / 127.0;
                if member {
                    self.channels[ch].pressure = value;
                    for voice in self.on_channel(channel) {
                        self.block.write(Lane::Pressure, voice, value);
                    }
                } else {
                    self.block.controllers.channel_pressure = value;
                }
            }
            0xe0 => {
                let raw = i32::from(data[1] & 0x7f) | i32::from(data[2] & 0x7f) << 7;
                let bend = if raw >= 8192 {
                    (raw - 8192) as f32 / 8191.0
                } else {
                    (raw - 8192) as f32 / 8192.0
                };
                let smoothing = self.smoothing();
                if member {
                    self.channels[ch].bend = bend;
                    for voice in self.on_channel(channel) {
                        let slot = &mut self.slots[voice];
                        slot.note_bend.retarget(bend, now, smoothing);
                        slot.dirty = true;
                    }
                } else {
                    self.block.controllers.pitch_bend = bend;
                    self.bend.retarget(bend, now, smoothing);
                    for voice in self.block.active_voices() {
                        self.slots[voice].dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Controller reset (CC121): bend centred, pressures and the mod wheel
    /// at zero, the sustain pedal up, MPE's channels and every voice's
    /// per-note values back to their defaults.
    fn reset_controllers<H: VoiceHandler>(&mut self, now: u64, time: usize, h: &mut H) {
        let smoothing = self.smoothing();
        self.block.controllers.pitch_bend = 0.0;
        self.block.controllers.channel_pressure = 0.0;
        self.block.controllers.set_cc(1, 0.0);
        self.block.controllers.set_cc(11, 1.0);
        self.bend.retarget(0.0, now, smoothing);
        self.channels = [Channel::RESET; 16];
        for voice in self.block.active_voices() {
            let slot = &mut self.slots[voice];
            if slot.note_bend.target() != 0.0 {
                slot.note_bend.retarget(0.0, now, smoothing);
            }
            slot.dirty = true;
            self.block.write(Lane::Pressure, voice, DEFAULT_PRESSURE);
            self.block.write(Lane::Timbre, voice, DEFAULT_TIMBRE);
        }
        self.block.controllers.set_cc(64, 0.0);
        self.sustain_pedal(false, now, time, h);
    }
}
