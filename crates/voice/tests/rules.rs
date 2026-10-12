//! The voicing rules of the engine specification's section 14, each test
//! citing the rule it checks.

use std::ops::Range;

use lspk_plugin::{Event, Expression, Note, ParamId};
use lspk_voice::{
    CONTROL_INTERVAL, Glide, LANE_ALIGN, Layout, Mpe, SameNote, Settings, Start, StealPriority,
    VoiceBlock, VoiceEvent, VoiceEventKind, VoiceHandler, VoiceState, Voices,
};

lspk_rt::install_trap!();

const RATE: f32 = 48_000.0;

/// What a voice event was, with its absolute frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Heard {
    at: u64,
    voice: usize,
    kind: VoiceEventKind,
}

/// Records what the voices do, frame by frame.
#[derive(Default)]
struct Recorder {
    /// The absolute frame the current block starts at.
    offset: u64,
    heard: Vec<Heard>,
    /// Voice 0 to 3's pitch at each frame (NaN while inactive).
    trace: Vec<[f32; 4]>,
    /// Whether a released voice ends at once.
    end_on_release: bool,
    params: Vec<(u64, ParamId)>,
}

impl VoiceHandler for Recorder {
    fn voice_event(&mut self, voices: &mut VoiceBlock, event: VoiceEvent) {
        self.heard.push(Heard {
            at: self.offset + u64::from(event.time),
            voice: event.voice,
            kind: event.kind,
        });
        if self.end_on_release && event.kind == VoiceEventKind::Release {
            voices.end(event.voice);
        }
    }

    fn render(&mut self, voices: &mut VoiceBlock, frames: Range<usize>) {
        let start = frames.start;
        for frame in frames {
            let mut row = [f32::NAN; 4];
            for (voice, value) in row.iter_mut().enumerate() {
                if voices.active_mask() & (1 << voice) != 0 {
                    let lane = voices.lanes_of(voice).start;
                    *value =
                        voices.pitch()[lane] + voices.pitch_step()[lane] * (frame - start) as f32;
                }
            }
            self.trace.push(row);
        }
    }

    fn event(&mut self, _voices: &mut VoiceBlock, event: Event) {
        if let Event::Param { time, id, .. } = event {
            self.params.push((self.offset + u64::from(time), id));
        }
    }
}

impl Recorder {
    fn starts(&self) -> Vec<(usize, Start, Option<Note>)> {
        self.heard
            .iter()
            .filter_map(|h| match h.kind {
                VoiceEventKind::Start { start, replaced } => Some((h.voice, start, replaced)),
                _ => None,
            })
            .collect()
    }

    fn last_start(&self) -> (usize, Start, Option<Note>) {
        *self.starts().last().expect("a start")
    }
}

/// Plays `events` (with absolute times, in order) over `frames` frames in
/// blocks of `block`.
fn play(voices: &mut Voices, rec: &mut Recorder, events: &[Event], frames: usize, block: usize) {
    let mut next = 0;
    let mut at = 0;
    while at < frames {
        let len = block.min(frames - at);
        let end = next
            + events[next..]
                .iter()
                .take_while(|e| (e.time() as usize) < at + len)
                .count();
        let local: Vec<Event> = events[next..end]
            .iter()
            .map(|e| retime(*e, e.time() - at as u32))
            .collect();
        rec.offset = voices.clock();
        voices.process(local, len, rec);
        next = end;
        at += len;
    }
}

fn retime(event: Event, time: u32) -> Event {
    match event {
        Event::NoteOn { note, velocity, .. } => Event::NoteOn {
            time,
            note,
            velocity,
        },
        Event::NoteOff { note, velocity, .. } => Event::NoteOff {
            time,
            note,
            velocity,
        },
        Event::NoteChoke { note, .. } => Event::NoteChoke { time, note },
        Event::NoteExpression {
            note,
            expression,
            value,
            ..
        } => Event::NoteExpression {
            time,
            note,
            expression,
            value,
        },
        Event::Midi { port, data, .. } => Event::Midi { time, port, data },
        Event::Param { id, value, .. } => Event::Param { time, id, value },
        other => other,
    }
}

fn on(time: u32, key: u8, velocity: f32) -> Event {
    on_ch(time, 0, key, velocity)
}

fn on_ch(time: u32, channel: u8, key: u8, velocity: f32) -> Event {
    Event::NoteOn {
        time,
        note: Note::key(channel, key),
        velocity,
    }
}

fn off(time: u32, key: u8) -> Event {
    off_ch(time, 0, key)
}

fn off_ch(time: u32, channel: u8, key: u8) -> Event {
    Event::NoteOff {
        time,
        note: Note::key(channel, key),
        velocity: 0.5,
    }
}

fn midi(time: u32, data: [u8; 3]) -> Event {
    Event::Midi {
        time,
        port: 0,
        data,
    }
}

fn cc(time: u32, number: u8, value: u8) -> Event {
    midi(time, [0xb0, number, value])
}

/// Pitch bend on `channel`, from −1 to +1.
fn bend(time: u32, channel: u8, value: f32) -> Event {
    let raw = (8192.0 + value * if value >= 0.0 { 8191.0 } else { 8192.0 }) as u16;
    midi(time, [0xe0 | channel, (raw & 0x7f) as u8, (raw >> 7) as u8])
}

fn voices_with(settings: Settings) -> Voices {
    let mut voices = Voices::new(Layout::default(), RATE);
    *voices.settings_mut() = settings;
    voices
}

/// The keys of the voices sounding, lowest voice first.
fn keys(voices: &Voices) -> Vec<u8> {
    let block = voices.block();
    block.active_voices().map(|v| block.info(v).key).collect()
}

fn lane(voices: &Voices, lane: fn(&VoiceBlock) -> &[f32], voice: usize) -> f32 {
    lane(voices.block())[voices.block().lanes_of(voice).start]
}

fn voice_of(voices: &Voices, key: u8) -> usize {
    let block = voices.block();
    block
        .active_voices()
        .find(|v| block.info(*v).key == key)
        .expect("a voice on that key")
}

// Spec: engine-core R117, R122
#[test]
fn eight_voices_by_default_and_the_oldest_is_stolen() {
    let mut voices = voices_with(Settings::default());
    let mut rec = Recorder::default();
    let events: Vec<Event> = (0..9).map(|i| on(i * 10, 60 + i as u8, 0.8)).collect();
    play(&mut voices, &mut rec, &events, 128, 128);
    assert_eq!(voices.block().active_count(), 8);
    let (voice, start, replaced) = rec.last_start();
    assert_eq!((voice, start), (0, Start::Steal));
    assert_eq!(replaced, Some(Note::key(0, 60)));

    // Of 40 held notes, the last 8 played survive.
    let mut voices = voices_with(Settings::default());
    let events: Vec<Event> = (0..40).map(|i| on(i, 40 + i as u8, 0.8)).collect();
    play(&mut voices, &mut rec, &events, 64, 64);
    let mut sounding = keys(&voices);
    sounding.sort_unstable();
    assert_eq!(sounding, (72..80).collect::<Vec<u8>>());
}

// Spec: engine-core R39, R117
#[test]
fn unison_lanes_dont_count_against_the_limit() {
    let layout = Layout {
        voices: 32,
        lanes_per_voice: 4,
    };
    let mut voices = Voices::new(layout, RATE);
    let mut rec = Recorder::default();
    let events: Vec<Event> = (0..8).map(|i| on(i, 60 + i as u8, 0.8)).collect();
    play(&mut voices, &mut rec, &events, 32, 32);
    let block = voices.block();
    assert_eq!(block.active_count(), 8);
    assert!(rec.starts().iter().all(|s| s.1 == Start::Fresh));
    // Every lane of a voice carries its values.
    assert_eq!(block.lanes_of(1), 4..8);
    assert_eq!(&block.pitch()[4..8], &[61.0; 4]);
    assert_eq!(&block.active()[28..32], &[1.0; 4]);
    assert_eq!(&block.active()[32..36], &[0.0; 4]);
    assert_eq!(block.span(16), 32);
    assert_eq!(block.lane_count(), 128);
}

// Spec: engine-core R117
#[test]
fn the_limit_doesnt_apply_in_mono() {
    let mut voices = voices_with(Settings {
        mono: true,
        voices: 1,
        ..Settings::default()
    });
    let mut rec = Recorder::default();
    play(
        &mut voices,
        &mut rec,
        &[on(0, 60, 0.8), on(1, 64, 0.8), on(2, 67, 0.8)],
        16,
        16,
    );
    assert_eq!(keys(&voices), [67]);
}

/// Holds keys 64, 60, 67 and 62 (velocities 0.5, 0.9, 0.5, 0.3) in four
/// voices, plays 65, and returns the key it stole.
fn stolen(priority: StealPriority) -> u8 {
    let mut voices = voices_with(Settings {
        voices: 4,
        steal: priority,
        ..Settings::default()
    });
    let mut rec = Recorder::default();
    let events = [
        on(0, 64, 0.5),
        on(1, 60, 0.9),
        on(2, 67, 0.5),
        on(3, 62, 0.3),
        on(4, 65, 0.7),
    ];
    play(&mut voices, &mut rec, &events, 16, 16);
    rec.last_start()
        .2
        .and_then(|n| n.key)
        .expect("a stolen note")
}

// Spec: engine-core R122
#[test]
fn each_steal_priority_keeps_what_it_names() {
    assert_eq!(stolen(StealPriority::Newest), 64);
    assert_eq!(stolen(StealPriority::Oldest), 62);
    assert_eq!(stolen(StealPriority::Highest), 60);
    assert_eq!(stolen(StealPriority::Lowest), 67);
    assert_eq!(stolen(StealPriority::Velocity), 62);
}

// Spec: engine-core R122
#[test]
fn ties_fall_back_to_newest() {
    // Equal velocities: the oldest goes.
    let mut voices = voices_with(Settings {
        voices: 3,
        steal: StealPriority::Velocity,
        ..Settings::default()
    });
    let mut rec = Recorder::default();
    let events = [
        on(0, 70, 0.5),
        on(1, 50, 0.5),
        on(2, 60, 0.5),
        on(3, 65, 0.5),
    ];
    play(&mut voices, &mut rec, &events, 16, 16);
    assert_eq!(rec.last_start().2.and_then(|n| n.key), Some(70));

    // Two lowest notes on one key: the older goes.
    let mut voices = voices_with(Settings {
        voices: 3,
        steal: StealPriority::Highest,
        ..Settings::default()
    });
    let events = [
        on_ch(0, 1, 60, 0.5),
        on_ch(1, 2, 60, 0.5),
        on(2, 72, 0.5),
        on(3, 65, 0.5),
    ];
    play(&mut voices, &mut rec, &events, 16, 16);
    assert_eq!(rec.last_start().2, Some(Note::key(1, 60)));
}

/// A decision the specification leaves open: a voice in its release is
/// stolen before any held voice, whatever the priority.
// Spec: engine-core R122
#[test]
fn released_voices_are_stolen_first() {
    let mut voices = voices_with(Settings {
        voices: 2,
        ..Settings::default()
    });
    let mut rec = Recorder::default();
    let events = [on(0, 60, 0.8), on(1, 62, 0.8), off(2, 62), on(3, 64, 0.8)];
    play(&mut voices, &mut rec, &events, 16, 16);
    assert_eq!(rec.last_start().2.and_then(|n| n.key), Some(62));
    let mut sounding = keys(&voices);
    sounding.sort_unstable();
    assert_eq!(sounding, [60, 64]);
}

/// Plays 60, releases it (with the pedal down if `pedal`), plays 60 again
/// with `same_note`, and returns the voices then and the recorder.
fn restrike(same_note: SameNote, pedal: bool) -> (Voices, Recorder) {
    let mut voices = voices_with(Settings {
        same_note,
        ..Settings::default()
    });
    let mut rec = Recorder::default();
    let mut events = vec![on(1, 60, 0.8), off(2, 60), on(3, 60, 0.6)];
    if pedal {
        events.insert(0, cc(0, 64, 127));
    }
    play(&mut voices, &mut rec, &events, 16, 16);
    (voices, rec)
}

// Spec: engine-core R120, R121
#[test]
fn one_voice_per_key_and_its_sustain_option() {
    // Off: a second voice, the first ringing out.
    let (voices, _) = restrike(SameNote::Off, false);
    assert_eq!(keys(&voices), [60, 60]);
    assert_eq!(voices.block().info(0).state, VoiceState::Released);

    // Off, with the pedal holding the first: it's released (R121) and a
    // second voice plays.
    let (voices, rec) = restrike(SameNote::Off, true);
    assert_eq!(keys(&voices), [60, 60]);
    assert_eq!(voices.block().info(0).state, VoiceState::Released);
    assert!(
        rec.heard
            .iter()
            .any(|h| h.kind == VoiceEventKind::Release && h.voice == 0)
    );

    // Sustain: a released (not sustained) voice isn't reused...
    let (voices, _) = restrike(SameNote::Sustain, false);
    assert_eq!(keys(&voices), [60, 60]);
    // ...a sustained one is.
    let (voices, rec) = restrike(SameNote::Sustain, true);
    assert_eq!(keys(&voices), [60]);
    assert_eq!(rec.last_start(), (0, Start::Steal, None));

    // On: always reused, without touching other voices.
    let (voices, rec) = restrike(SameNote::On, false);
    assert_eq!(keys(&voices), [60]);
    assert_eq!(rec.last_start().1, Start::Steal);
    assert_eq!(lane(&voices, VoiceBlock::velocity, 0), 0.6);
}

#[test]
fn the_sustain_pedal_holds_released_notes() {
    let mut voices = voices_with(Settings::default());
    let mut rec = Recorder::default();
    play(
        &mut voices,
        &mut rec,
        &[cc(0, 64, 100), on(1, 60, 0.8), off(2, 60)],
        8,
        8,
    );
    assert_eq!(voices.block().info(0).state, VoiceState::Sustained);
    assert_eq!(lane(&voices, VoiceBlock::gate, 0), 1.0);
    assert!(voices.block().controllers().sustain);
    play(&mut voices, &mut rec, &[cc(0, 64, 0)], 8, 8);
    assert_eq!(voices.block().info(0).state, VoiceState::Released);
    assert_eq!(lane(&voices, VoiceBlock::gate, 0), 0.0);
    assert_eq!(
        rec.heard.last().map(|h| h.kind),
        Some(VoiceEventKind::Release)
    );
}

// Spec: engine-core R118
#[test]
fn mono_retriggers_without_legato_and_slides_with_it() {
    for (legato, expected) in [(false, Start::Retrigger), (true, Start::Legato)] {
        let mut voices = voices_with(Settings {
            mono: true,
            legato,
            ..Settings::default()
        });
        let mut rec = Recorder::default();
        play(
            &mut voices,
            &mut rec,
            &[on(0, 60, 0.8), on(4, 64, 0.8)],
            8,
            8,
        );
        assert_eq!(rec.last_start(), (0, expected, Some(Note::key(0, 60))));
        assert_eq!(keys(&voices), [64]);

        // Releasing the newest returns to the held key (a decision: the
        // specification doesn't say), the same way.
        play(&mut voices, &mut rec, &[off(0, 64)], 8, 8);
        assert_eq!(rec.last_start().1, expected);
        assert_eq!(keys(&voices), [60]);
        assert_eq!(lane(&voices, VoiceBlock::pitch, 0), 60.0);

        // Releasing the last key releases the voice; a note played in its
        // release retriggers it, legato or not.
        play(&mut voices, &mut rec, &[off(0, 60), on(4, 67, 0.8)], 8, 8);
        assert!(rec.heard.iter().any(|h| h.kind == VoiceEventKind::Release));
        assert_eq!(rec.last_start().1, Start::Retrigger);
    }
}

// Spec: engine-core R119
#[test]
fn legato_without_mono_is_paraphonic() {
    let mut voices = voices_with(Settings {
        legato: true,
        ..Settings::default()
    });
    let mut rec = Recorder::default();
    play(
        &mut voices,
        &mut rec,
        &[on(0, 60, 0.8), on(2, 64, 0.8), on(3, 67, 0.8)],
        8,
        8,
    );
    let starts = rec.starts();
    assert_eq!(starts[0].1, Start::Fresh);
    assert_eq!(starts[1].1, Start::Join { leader: 0 });
    assert_eq!(starts[2].1, Start::Join { leader: 1 });
    // Each keeps its own pitch.
    assert_eq!(keys(&voices), [60, 64, 67]);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 1), 64.0);

    // Once nothing is held, a new note starts on its own.
    rec.end_on_release = true;
    let events = [off(0, 60), off(0, 64), off(0, 67), on(2, 72, 0.8)];
    play(&mut voices, &mut rec, &events, 8, 8);
    assert_eq!(rec.last_start().1, Start::Fresh);
}

// Spec: engine-core R101
#[test]
fn velocity_legato_decides_whether_a_legato_note_updates_velocity() {
    for (velocity_legato, expected) in [(false, 0.8), (true, 0.3)] {
        let mut voices = voices_with(Settings {
            mono: true,
            legato: true,
            velocity_legato,
            ..Settings::default()
        });
        let mut rec = Recorder::default();
        play(
            &mut voices,
            &mut rec,
            &[on(0, 60, 0.8), on(1, 64, 0.3)],
            8,
            8,
        );
        assert_eq!(lane(&voices, VoiceBlock::velocity, 0), expected);
    }
}

fn glide(time: f32, curve: f32) -> Settings {
    Settings {
        mono: true,
        legato: true,
        glide: Glide {
            time,
            curve,
            always: false,
            scaled: false,
        },
        ..Settings::default()
    }
}

/// Voice 0's pitch at each frame after holding 60 and playing `to` at
/// frame 0 of the second block, over `frames`.
fn glide_trace(settings: Settings, to: u8, frames: usize) -> Vec<f32> {
    let mut voices = voices_with(settings);
    let mut rec = Recorder::default();
    play(&mut voices, &mut rec, &[on(0, 60, 0.8)], 64, 64);
    rec.trace.clear();
    play(&mut voices, &mut rec, &[on(0, to, 0.8)], frames, 64);
    rec.trace.iter().map(|row| row[0]).collect()
}

// Spec: engine-core R123
#[test]
fn glide_takes_the_same_time_whatever_the_interval() {
    // 0.1 s is 4800 frames at 48 kHz.
    for to in [72, 84, 48] {
        let trace = glide_trace(glide(0.1, 0.0), to, 6000);
        assert_eq!(trace[0], 60.0);
        assert!((trace[4799] - f32::from(to)).abs() > 1e-3);
        assert_eq!(trace[4800], f32::from(to));
        // Linear in semitones at curve 0, exactly so at control points.
        let middle = 60.0 + (f32::from(to) - 60.0) / 2.0;
        assert!((trace[2400] - middle).abs() < 1e-3, "{}", trace[2400]);
    }
}

// Spec: engine-core R123
#[test]
fn the_glide_curve_bends_the_move() {
    // +1: 80 % there within 2.5 % of the time (120 frames of 4800).
    let fast = glide_trace(glide(0.1, 1.0), 72, 4864);
    assert!(fast[128] > 60.0 + 0.8 * 12.0, "{}", fast[128]);
    // −1: still close to the old pitch at 94 % of the time, there at the end.
    let slow = glide_trace(glide(0.1, -1.0), 72, 4864);
    assert!(slow[4512] < 60.0 + 0.03 * 12.0, "{}", slow[4512]);
    assert_eq!(slow[4800], 72.0);
    // Moderate curves on either side of linear.
    let up = glide_trace(glide(0.1, 0.5), 72, 4864);
    let down = glide_trace(glide(0.1, -0.5), 72, 4864);
    assert!(up[2400] > 66.5 && down[2400] < 65.5);
}

// Spec: engine-core R124
#[test]
fn glide_needs_a_held_note_unless_always() {
    for always in [false, true] {
        let mut settings = glide(0.1, 0.0);
        settings.glide.always = always;
        let mut voices = voices_with(settings);
        let mut rec = Recorder {
            end_on_release: true,
            ..Recorder::default()
        };
        play(
            &mut voices,
            &mut rec,
            &[on(0, 60, 0.8), off(10, 60)],
            64,
            64,
        );
        rec.trace.clear();
        play(&mut voices, &mut rec, &[on(0, 72, 0.8)], 64, 64);
        let start = rec.trace[0][0];
        assert_eq!(start, if always { 60.0 } else { 72.0 });
    }
    // In poly too: a held note makes the next one glide from it.
    let mut settings = glide(0.1, 0.0);
    settings.mono = false;
    let mut voices = voices_with(settings);
    let mut rec = Recorder::default();
    play(&mut voices, &mut rec, &[on(0, 60, 0.8)], 64, 64);
    play(&mut voices, &mut rec, &[on(0, 72, 0.8)], 64, 64);
    assert_eq!(rec.trace[64][1], 60.0);
}

// Spec: engine-core R125
#[test]
fn scaled_glide_is_proportional_to_the_interval() {
    let mut settings = glide(0.25, 0.0);
    settings.glide.scaled = true;
    // 250 ms per octave: 6 semitones in 125 ms (6000 frames), 24 in 500 ms.
    // Pitch is exact at control points and linear between, so a glide that
    // ends between two arrives at the second.
    let interval = CONTROL_INTERVAL as usize;
    for (to, frames) in [(66u8, 6000usize), (72, 12_000), (84, 24_000)] {
        let trace = glide_trace(settings, to, frames + 64);
        let target = f32::from(to);
        assert!((trace[frames - interval] - target).abs() > 1e-3);
        assert!((trace[frames] - target).abs() < 0.01);
        assert_eq!(trace[frames + interval], target);
    }
}

// Spec: engine-core R127
#[test]
fn bend_up_and_down_have_their_own_ranges_and_are_smoothed() {
    let mut voices = voices_with(Settings {
        bend_up: 2.0,
        bend_down: -12.0,
        ..Settings::default()
    });
    let mut rec = Recorder::default();
    play(
        &mut voices,
        &mut rec,
        &[on(0, 60, 0.8), bend(0, 0, 1.0)],
        64,
        64,
    );
    // Smoothed: not there at once, there after 5 ms (240 frames).
    assert!(rec.trace[32][0] < 61.0);
    rec.trace.clear();
    play(&mut voices, &mut rec, &[], 256, 64);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 0), 62.0);
    play(&mut voices, &mut rec, &[bend(0, 0, -1.0)], 512, 64);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 0), 48.0);
    assert_eq!(voices.block().controllers().pitch_bend, -1.0);
    // A positive down range bends up.
    voices.settings_mut().bend_down = 5.0;
    play(&mut voices, &mut rec, &[], 64, 64);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 0), 65.0);
    // Without smoothing, a bend is heard at its frame.
    voices.settings_mut().smoothing = 0.0;
    play(&mut voices, &mut rec, &[bend(0, 0, 0.0)], 64, 64);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 0), 60.0);
}

fn mpe() -> Settings {
    Settings {
        mpe: Mpe {
            enabled: true,
            bend_range: 48.0,
        },
        smoothing: 0.0,
        ..Settings::default()
    }
}

// Spec: engine-core R128
#[test]
fn mpe_gives_each_note_its_own_bend_pressure_and_timbre() {
    let mut voices = voices_with(mpe());
    let mut rec = Recorder::default();
    let events = [
        // Sent before its note, as MPE senders do: the note starts bent.
        bend(0, 3, 0.25),
        on_ch(1, 1, 60, 0.8),
        on_ch(2, 2, 64, 0.8),
        on_ch(3, 3, 67, 0.8),
        bend(4, 1, 1.0),
        midi(5, [0xd2, 100, 0]),
        cc(6, 74, 0), // on the master channel: global, not timbre
    ];
    let mut events = events.to_vec();
    events.push(midi(7, [0xb1, 74, 127]));
    play(&mut voices, &mut rec, &events, 64, 64);
    let (a, b, c) = (
        voice_of(&voices, 60),
        voice_of(&voices, 64),
        voice_of(&voices, 67),
    );
    assert_eq!(lane(&voices, VoiceBlock::pitch, a), 108.0);
    assert_eq!(lane(&voices, VoiceBlock::pitch, b), 64.0);
    // 0.25 of a 14-bit bend is 0.2499 of 48 semitones.
    assert!((lane(&voices, VoiceBlock::pitch, c) - 79.0).abs() < 0.01);
    assert!((lane(&voices, VoiceBlock::pressure, b) - 100.0 / 127.0).abs() < 1e-6);
    assert_eq!(lane(&voices, VoiceBlock::pressure, a), 0.0);
    assert_eq!(lane(&voices, VoiceBlock::timbre, a), 1.0);
    assert_eq!(lane(&voices, VoiceBlock::timbre, b), 0.5);

    // The master channel's bend moves every note, with the bend ranges.
    play(&mut voices, &mut rec, &[bend(0, 0, 1.0)], 64, 64);
    assert_eq!(lane(&voices, VoiceBlock::pitch, b), 66.0);
    assert_eq!(lane(&voices, VoiceBlock::pitch, a), 110.0);

    // A configurable range.
    voices.settings_mut().mpe.bend_range = 12.0;
    play(&mut voices, &mut rec, &[], 64, 64);
    assert_eq!(lane(&voices, VoiceBlock::pitch, a), 74.0);
}

// Spec: engine-core R129
#[test]
fn clap_note_expressions_reach_the_notes_they_name() {
    let mut voices = voices_with(Settings {
        smoothing: 0.0,
        ..Settings::default()
    });
    let mut rec = Recorder {
        end_on_release: true,
        ..Recorder::default()
    };
    let with_id = |id| Note {
        id: Some(id),
        ..Note::key(0, 60)
    };
    let expr = |time, id, expression, value| Event::NoteExpression {
        time,
        note: Note {
            id: Some(id),
            ..Note::default()
        },
        expression,
        value,
    };
    let events = [
        Event::NoteOn {
            time: 0,
            note: with_id(7),
            velocity: 0.8,
        },
        Event::NoteOn {
            time: 1,
            note: with_id(8),
            velocity: 0.8,
        },
        expr(2, 7, Expression::Tuning, 12.0),
        expr(2, 8, Expression::Brightness, 0.9),
        expr(2, 8, Expression::Volume, 9.0),
        expr(2, 7, Expression::Pan, 0.0),
        expr(2, 7, Expression::Pressure, 0.4),
        expr(2, 7, Expression::Vibrato, 0.3),
        expr(2, 7, Expression::Expression, 0.2),
    ];
    play(&mut voices, &mut rec, &events, 64, 64);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 0), 72.0);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 1), 60.0);
    assert_eq!(lane(&voices, VoiceBlock::timbre, 1), 0.9);
    assert_eq!(lane(&voices, VoiceBlock::volume, 1), 4.0);
    assert_eq!(lane(&voices, VoiceBlock::volume, 0), 1.0);
    assert_eq!(lane(&voices, VoiceBlock::pan, 0), 0.0);
    assert_eq!(lane(&voices, VoiceBlock::pressure, 0), 0.4);
    assert_eq!(lane(&voices, VoiceBlock::vibrato, 0), 0.3);
    assert_eq!(lane(&voices, VoiceBlock::expression, 0), 0.2);

    // A note-off by ID releases that note only, and its end names it, for
    // CLAP's note-end.
    let off_id = Event::NoteOff {
        time: 0,
        note: Note {
            id: Some(8),
            ..Note::default()
        },
        velocity: 0.0,
    };
    play(&mut voices, &mut rec, &[off_id], 8, 8);
    assert_eq!(keys(&voices), [60]);
    assert_eq!(
        rec.heard.last().map(|h| h.kind),
        Some(VoiceEventKind::End { note: with_id(8) })
    );
}

// Spec: engine-core R130
#[test]
fn a_new_note_starts_with_fresh_expression() {
    // A stolen voice takes its new channel's values, not the old note's.
    let mut voices = voices_with(Settings { voices: 1, ..mpe() });
    let mut rec = Recorder::default();
    let events = [
        midi(0, [0xd1, 127, 0]),
        midi(0, [0xd2, 20, 0]),
        on_ch(1, 1, 60, 0.8),
        on_ch(2, 2, 62, 0.8),
    ];
    play(&mut voices, &mut rec, &events, 8, 8);
    assert_eq!(rec.last_start().1, Start::Steal);
    assert!((lane(&voices, VoiceBlock::pressure, 0) - 20.0 / 127.0).abs() < 1e-6);

    // A CLAP expression doesn't outlive its note, in mono legato too.
    let mut voices = voices_with(Settings {
        mono: true,
        legato: true,
        ..Settings::default()
    });
    let events = [
        on(0, 60, 0.8),
        Event::NoteExpression {
            time: 1,
            note: Note::key(0, 60),
            expression: Expression::Volume,
            value: 2.0,
        },
        on(2, 64, 0.8),
    ];
    play(&mut voices, &mut rec, &events, 8, 8);
    assert_eq!(rec.last_start().1, Start::Legato);
    assert_eq!(lane(&voices, VoiceBlock::volume, 0), 1.0);
}

// Spec: engine-core R131
#[test]
fn all_sound_off_reset_and_all_notes_off() {
    let mut voices = voices_with(Settings::default());
    let mut rec = Recorder::default();
    // CC123: every held note released as if its key were let go, so the
    // pedal still holds them.
    let events = [
        on(0, 60, 0.8),
        on(0, 64, 0.8),
        cc(1, 64, 127),
        cc(2, 123, 0),
    ];
    play(&mut voices, &mut rec, &events, 8, 8);
    assert_eq!(voices.block().info(0).state, VoiceState::Sustained);
    // CC121: the pedal up (releasing them), bend centred.
    play(
        &mut voices,
        &mut rec,
        &[bend(0, 0, 1.0), cc(1, 121, 0)],
        512,
        64,
    );
    assert_eq!(voices.block().info(1).state, VoiceState::Released);
    assert!(!voices.block().controllers().sustain);
    assert_eq!(lane(&voices, VoiceBlock::pitch, 0), 60.0);
    // CC120: every voice ends at once.
    play(
        &mut voices,
        &mut rec,
        &[on(0, 67, 0.8), cc(3, 120, 0)],
        8,
        8,
    );
    assert_eq!(voices.block().active_count(), 0);
    let ends = rec
        .heard
        .iter()
        .filter(|h| matches!(h.kind, VoiceEventKind::End { .. }))
        .count();
    assert_eq!(ends, 3);
}

#[test]
fn voices_end_when_reported_silent_and_lanes_stay_low() {
    /// Ends a voice 100 frames into its release.
    struct Tail {
        released: [Option<usize>; 32],
        at: usize,
        ends: Vec<(usize, usize)>,
    }
    impl VoiceHandler for Tail {
        fn voice_event(&mut self, _: &mut VoiceBlock, event: VoiceEvent) {
            match event.kind {
                VoiceEventKind::Release => {
                    self.released[event.voice] = Some(self.at + event.time as usize);
                }
                VoiceEventKind::End { .. } => {
                    self.ends.push((event.voice, self.at + event.time as usize));
                }
                VoiceEventKind::Start { .. } => {}
            }
        }
        fn render(&mut self, voices: &mut VoiceBlock, frames: Range<usize>) {
            for voice in voices.active_voices() {
                if self.released[voice].is_some_and(|r| self.at + frames.end >= r + 100) {
                    self.released[voice] = None;
                    voices.end(voice);
                }
            }
        }
    }
    let mut voices = voices_with(Settings::default());
    let mut tail = Tail {
        released: [None; 32],
        at: 0,
        ends: Vec::new(),
    };
    let events = [on(0, 60, 0.8), on(0, 62, 0.8), on(0, 64, 0.8), off(10, 62)];
    voices.process(events, 64, &mut tail);
    assert_eq!(voices.block().span(4), 4);
    tail.at = 64;
    voices.process([], 64, &mut tail);
    // Released at 10, silent 100 frames later, at the next render boundary.
    assert_eq!(tail.ends, [(1, 128)]);
    assert_eq!(voices.block().active_mask(), 0b101);
    assert_eq!(voices.block().active()[1], 0.0);
    // The freed voice is the lowest free one, so the next note takes it.
    let mut rec = Recorder::default();
    play(&mut voices, &mut rec, &[on(0, 70, 0.8)], 8, 8);
    assert_eq!(rec.last_start().0, 1);
    assert_eq!(voices.block().span(16), 16);
    assert_eq!(voices.block().lane_count() % LANE_ALIGN, 0);
}

#[test]
fn a_choke_ends_its_voice_at_once_and_parameters_pass_through() {
    let mut voices = voices_with(Settings::default());
    let mut rec = Recorder::default();
    let events = [
        on(0, 60, 0.8),
        Event::Param {
            time: 5,
            id: ParamId(3),
            value: 1.0,
        },
        Event::NoteChoke {
            time: 9,
            note: Note::key(0, 60),
        },
    ];
    play(&mut voices, &mut rec, &events, 16, 16);
    assert_eq!(voices.block().active_count(), 0);
    assert_eq!(rec.params, [(5, ParamId(3))]);
    assert_eq!(
        rec.heard.last().copied(),
        Some(Heard {
            at: 9,
            voice: 0,
            kind: VoiceEventKind::End {
                note: Note::key(0, 60)
            }
        })
    );
}

/// A busy minute of playing, compressed: notes, steals, glides, bends,
/// MPE, expressions and the pedal.
fn busy_script() -> Vec<Event> {
    let mut events = Vec::new();
    for i in 0..48u32 {
        let t = i * 211;
        let key = 40 + (i * 7 % 36) as u8;
        events.push(on_ch(t, (i % 4) as u8, key, 0.3 + (i % 5) as f32 * 0.1));
        if i % 3 == 0 {
            events.push(bend(t + 17, 0, (i as f32 * 0.37).sin()));
        }
        if i % 4 == 1 {
            events.push(bend(t + 29, 1, (i as f32 * 0.21).cos()));
        }
        if i % 5 == 2 {
            events.push(Event::NoteExpression {
                time: t + 41,
                note: Note::key((i % 4) as u8, key),
                expression: Expression::Tuning,
                value: 0.5,
            });
        }
        if i % 7 == 3 {
            events.push(cc(t + 53, 64, if i % 2 == 0 { 127 } else { 0 }));
        }
        if i >= 2 {
            let old = 40 + ((i - 2) * 7 % 36) as u8;
            events.push(off_ch(t + 101, ((i - 2) % 4) as u8, old));
        }
    }
    events.sort_by_key(Event::time);
    events
}

#[test]
fn note_timing_is_sample_accurate_at_any_block_size() {
    let settings = Settings {
        voices: 4,
        glide: Glide {
            time: 0.05,
            curve: 0.4,
            always: true,
            scaled: false,
        },
        mpe: Mpe {
            enabled: true,
            bend_range: 24.0,
        },
        ..Settings::default()
    };
    let script = busy_script();
    let frames = 11_000;
    let run = |block: usize| {
        let mut voices = voices_with(settings);
        let mut rec = Recorder {
            end_on_release: true,
            ..Recorder::default()
        };
        play(&mut voices, &mut rec, &script, frames, block);
        rec
    };
    let reference = run(frames);
    // Every note starts at its event's frame.
    let ons: Vec<u64> = script
        .iter()
        .filter(|e| matches!(e, Event::NoteOn { .. }))
        .map(|e| u64::from(e.time()))
        .collect();
    let starts: Vec<u64> = reference
        .heard
        .iter()
        .filter(|h| matches!(h.kind, VoiceEventKind::Start { .. }))
        .map(|h| h.at)
        .collect();
    assert_eq!(starts, ons);
    for block in [1, 7, CONTROL_INTERVAL as usize, 64, 100, 128, 511, 4096] {
        let rec = run(block);
        assert_eq!(rec.heard, reference.heard, "block size {block}");
        assert_eq!(rec.trace.len(), frames);
        for (frame, (a, b)) in rec.trace.iter().zip(&reference.trace).enumerate() {
            for (x, y) in a.iter().zip(b) {
                assert!(
                    x.is_nan() && y.is_nan() || (x - y).abs() < 1e-3,
                    "block size {block}, frame {frame}: {x} against {y}"
                );
            }
        }
    }
}

#[test]
fn processing_allocates_nothing() {
    let mut voices = voices_with(Settings {
        voices: 6,
        legato: true,
        glide: Glide {
            time: 0.02,
            curve: -0.3,
            always: true,
            scaled: true,
        },
        mpe: Mpe {
            enabled: true,
            bend_range: 48.0,
        },
        ..Settings::default()
    });
    let script = busy_script();
    let mut handler = Silent;
    lspk_rt::rt_test!({
        for event in &script {
            voices.process([*event].into_iter().map(|e| retime(e, 0)), 64, &mut handler);
        }
        voices.settings_mut().mono = true;
        for event in &script {
            voices.process([*event].into_iter().map(|e| retime(e, 3)), 32, &mut handler);
        }
    });
}

/// Ends every voice as soon as it's released, rendering nothing.
struct Silent;

impl VoiceHandler for Silent {
    fn voice_event(&mut self, voices: &mut VoiceBlock, event: VoiceEvent) {
        if event.kind == VoiceEventKind::Release {
            voices.end(event.voice);
        }
    }
    fn render(&mut self, _: &mut VoiceBlock, _: Range<usize>) {}
}

/// A kernel over the pitch lanes, as a synthesiser's would run: every
/// SIMD level this machine has reads the same values.
#[test]
fn the_lanes_suit_every_simd_level() {
    use lspk_dsp::simd::{self, F32s, Kernel, Level};

    struct SumActive<'a>(&'a VoiceBlock);
    impl Kernel for SumActive<'_> {
        type Output = f32;
        #[inline(always)]
        fn run<S: F32s>(self) -> f32 {
            let span = self.0.span(S::LANES);
            let pitch = self.0.pitch()[..span].chunks_exact(S::LANES);
            let active = self.0.active()[..span].chunks_exact(S::LANES);
            let mut total = S::splat(0.0);
            for (p, a) in pitch.zip(active) {
                total = S::load(p).mul_add(S::load(a), total);
            }
            total.sum()
        }
    }

    let mut voices = Voices::new(
        Layout {
            voices: 32,
            lanes_per_voice: 3,
        },
        RATE,
    );
    let mut rec = Recorder::default();
    let events: Vec<Event> = (0..7).map(|i| on(i, 60 + i as u8, 0.8)).collect();
    play(&mut voices, &mut rec, &events, 8, 8);
    let expected = 3.0 * (60u8..67).map(f32::from).sum::<f32>();
    for level in Level::ALL {
        if let Ok(sum) = simd::dispatch_at(level, SumActive(voices.block())) {
            assert_eq!(sum, expected, "{level}");
        }
    }
}
