//! Micro-benchmarks for `lspk-voice`'s hot paths: allocation under heavy
//! note traffic, the per-block cost of the voice loop, and the unison mix
//! of every voice's copies to stereo. `cargo xtask
//! bench` times them, and CI's instruction-count gate covers them.

use std::f32::consts::TAU;
use std::hint::black_box;
use std::ops::Range;
use std::process::ExitCode;

use lspk_dsp::simd::{self, F32s, Kernel};
use lspk_plugin::{Event, Note};
use lspk_test::bench::Suite;
use lspk_voice::{
    Glide, Layout, Settings, Start, Unison, UnisonLanes, VoiceBlock, VoiceEvent, VoiceEventKind,
    VoiceHandler, Voices,
};

/// Frames in a block, as hosts commonly use.
const BLOCK: usize = 512;

const RATE: f32 = 48_000.0;

/// Ends every voice as soon as it's released, rendering nothing: the
/// voices' own cost alone.
struct Silent;

impl VoiceHandler for Silent {
    fn voice_event(&mut self, voices: &mut VoiceBlock, event: VoiceEvent) {
        if event.kind == VoiceEventKind::Release {
            voices.end(event.voice);
        }
    }

    fn render(&mut self, voices: &mut VoiceBlock, frames: Range<usize>) {
        black_box((voices.pitch()[0], frames));
    }
}

/// A sine per lane, by the two-term recurrence, summed to one output: the
/// smallest synthesiser that runs its voices as SIMD lanes.
struct Sines {
    coef: Vec<f32>,
    y1: Vec<f32>,
    y2: Vec<f32>,
    gain: Vec<f32>,
    out: Vec<f32>,
}

impl Sines {
    fn new(voices: &Voices) -> Self {
        let lanes = voices.block().lane_count();
        Self {
            coef: vec![0.0; lanes],
            y1: vec![0.0; lanes],
            y2: vec![0.0; lanes],
            gain: vec![0.0; lanes],
            out: vec![0.0; BLOCK],
        }
    }
}

/// [`Sines`]' kernel over one render call.
struct SineKernel<'a> {
    span: usize,
    coef: &'a [f32],
    gain: &'a [f32],
    y1: &'a mut [f32],
    y2: &'a mut [f32],
    out: &'a mut [f32],
}

impl Kernel for SineKernel<'_> {
    type Output = ();

    #[inline(always)]
    fn run<S: F32s>(self) {
        for sample in self.out.iter_mut() {
            let mut mix = S::splat(0.0);
            for lane in (0..self.span).step_by(S::LANES) {
                let range = lane..lane + S::LANES;
                let y1 = S::load(&self.y1[range.clone()]);
                let y0 = S::load(&self.coef[range.clone()])
                    .mul_add(y1, -S::load(&self.y2[range.clone()]));
                y1.store(&mut self.y2[range.clone()]);
                y0.store(&mut self.y1[range.clone()]);
                mix = y0.mul_add(S::load(&self.gain[range]), mix);
            }
            *sample += mix.sum();
        }
    }
}

impl VoiceHandler for Sines {
    fn voice_event(&mut self, voices: &mut VoiceBlock, event: VoiceEvent) {
        match event.kind {
            VoiceEventKind::Start {
                start: Start::Fresh,
                ..
            } => {
                for lane in voices.lanes_of(event.voice) {
                    self.y1[lane] = 0.0;
                    self.y2[lane] = 0.0;
                }
            }
            VoiceEventKind::Release => voices.end(event.voice),
            _ => {}
        }
    }

    fn render(&mut self, voices: &mut VoiceBlock, frames: Range<usize>) {
        for voice in voices.active_voices() {
            for lane in voices.lanes_of(voice) {
                let hz = 440.0 * ((voices.pitch()[lane] - 69.0) / 12.0).exp2();
                let w = TAU * hz / RATE;
                self.coef[lane] = 2.0 * w.cos();
                if self.y1[lane] == 0.0 && self.y2[lane] == 0.0 {
                    self.y2[lane] = -w.sin();
                }
                self.gain[lane] = voices.velocity()[lane] * voices.active()[lane];
            }
        }
        let span = voices.span(simd::level().lanes());
        simd::dispatch(SineKernel {
            span,
            coef: &self.coef,
            gain: &self.gain,
            y1: &mut self.y1,
            y2: &mut self.y2,
            out: &mut self.out[frames],
        });
    }
}

/// Voices with `count` notes held, gliding, and a pitch bend that moves
/// every block.
fn held(count: u8) -> Voices {
    let mut voices = Voices::new(Layout::default(), RATE);
    *voices.settings_mut() = Settings {
        voices: 32,
        glide: Glide {
            time: 2.0,
            curve: 0.3,
            always: true,
            scaled: false,
        },
        ..Settings::default()
    };
    let notes: Vec<Event> = (0..count)
        .map(|i| Event::NoteOn {
            time: u32::from(i),
            note: Note::key(0, 40 + i),
            velocity: 0.7,
        })
        .collect();
    voices.process(notes, BLOCK, &mut Silent);
    voices
}

/// A pitch bend at frame 100, alternating up and down each call.
fn bend(up: bool) -> Event {
    Event::Midi {
        time: 100,
        port: 0,
        data: [0xe0, 0, if up { 96 } else { 32 }],
    }
}

fn main() -> ExitCode {
    let _ = simd::level();
    let mut suite = Suite::new("lspk-voice");

    // A block with 64 note-ons and their note-offs at an 8-voice limit, so
    // most notes steal, with pitch bends between: allocation and stealing
    // under the heaviest traffic a host sends.
    suite.micro("allocation/heavy-traffic", BLOCK, || {
        let mut voices = Voices::new(Layout::default(), RATE);
        let mut events = Vec::new();
        for i in 0..64u32 {
            let key = 36 + (i * 11 % 48) as u8;
            events.push(Event::NoteOn {
                time: i * 8,
                note: Note::key((i % 3) as u8, key),
                velocity: 0.2 + (i % 7) as f32 * 0.1,
            });
            if i >= 12 {
                let old = 36 + ((i - 12) * 11 % 48) as u8;
                events.push(Event::NoteOff {
                    time: i * 8 + 3,
                    note: Note::key(((i - 12) % 3) as u8, old),
                    velocity: 0.5,
                });
            }
            if i % 8 == 5 {
                events.push(bend(i % 16 == 5));
                if let Some(Event::Midi { time, .. }) = events.last_mut() {
                    *time = i * 8 + 5;
                }
            }
        }
        move || {
            voices.process(events.iter().copied(), BLOCK, &mut Silent);
            black_box(voices.block().active_mask());
        }
    });

    // The voice loop's own cost: 32 held notes gliding under a moving pitch
    // bend, the pitch of every voice recomputed at each control point.
    suite.micro("voice-loop/32-voices", BLOCK, || {
        let mut voices = held(32);
        let mut up = false;
        move || {
            up = !up;
            voices.process([bend(up)], BLOCK, &mut Silent);
            black_box(voices.block().pitch());
        }
    });

    // The same with a sine per voice run as SIMD lanes: what a minimal
    // synthesiser's block costs on top of the voices.
    suite.micro("voice-loop/32-sines", BLOCK, || {
        let mut voices = held(32);
        let mut sines = Sines::new(&voices);
        let mut up = false;
        move || {
            up = !up;
            sines.out.fill(0.0);
            voices.process([bend(up)], BLOCK, &mut sines);
            black_box(&sines.out);
        }
    });

    // The unison mix alone, 128 frames of every voice's copies to a stereo
    // pair per voice, scalar and in SIMD: 8 voices of 7 copies (voices
    // straddling vectors) and 32 of 16 (whole vectors per voice). Items
    // are voice-copy samples.
    for (voices, copies) in [(8, 7), (32, 16)] {
        for simd in [false, true] {
            let id = format!(
                "unison-mix/{voices}x{copies}/{}",
                if simd { "simd" } else { "scalar" }
            );
            suite.micro(&id, MIX_BLOCK * voices * copies, move || {
                let mut lanes = UnisonLanes::new(voices, copies, 1);
                lanes.set(Unison {
                    copies,
                    blend: 60.0,
                    ..Unison::default()
                });
                let input: Vec<f32> = (0..MIX_BLOCK * lanes.lane_count())
                    .map(|i| (i as f32 * 0.37).sin())
                    .collect();
                let mut out = vec![0.0f32; MIX_BLOCK * 2 * lanes.voice_stride()];
                let level = simd::level();
                move || {
                    if simd {
                        lanes.mix_at(level, MIX_BLOCK, voices, &input, &mut out);
                    } else {
                        lanes.mix_scalar(MIX_BLOCK, voices, &input, &mut out);
                    }
                    black_box(&out);
                }
            });
        }
    }

    suite.main()
}

/// The block the unison mix is benchmarked at: the plan's reference, 128
/// frames.
const MIX_BLOCK: usize = 128;
