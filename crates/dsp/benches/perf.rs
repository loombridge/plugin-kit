//! Micro-benchmarks for `lspk-dsp`'s hot paths. Add one here for each new
//! hot path as it lands; `cargo xtask bench` times them, and CI's
//! instruction-count gate covers them.

use std::hint::black_box;
use std::process::ExitCode;

use lspk_dsp::envelope::{Timing, Trigger};
use lspk_dsp::lfo::{Lfo, LfoBank, LfoType, Point, Rate, Settings, Transport};
use lspk_dsp::{
    Envelope, EnvelopeBank, EnvelopeParams, FilterBank, FilterType, LinearSmoother, Oversampler,
    VoiceParams, simd,
};
use lspk_test::bench::Suite;
use lspk_test::signal;

/// Frames in a block, as hosts commonly use.
const BLOCK: usize = 512;

/// Voices in the filter benchmarks' banks.
const FILTER_VOICES: usize = 16;

/// Voices in the envelope benchmarks.
const VOICES: usize = 32;

/// Short stages, so a block crosses several of them: every stage, bent.
const BUSY: EnvelopeParams = EnvelopeParams {
    attack: 2.0,
    hold: 1.0,
    decay: 4.0,
    sustain: 0.5,
    release: 3.0,
    attack_curve: 0.3,
    ..EnvelopeParams::AMP
};

/// What happens to voice `voice` at the start of block `block`: notes 8
/// blocks long, each voice's starting a block after the last's.
fn envelope_event(block: usize, voice: usize) -> Option<bool> {
    match (block + voice) % 8 {
        0 => Some(true),
        4 => Some(false),
        _ => None,
    }
}

fn main() -> ExitCode {
    let mut suite = Suite::new("lspk-dsp");

    // A parameter gliding to a new value every block: 20 ms glides at
    // 48 kHz, a block of steps per call.
    suite.micro("smoother/glide", BLOCK, || {
        let mut smoother = LinearSmoother::new(0.0, 960);
        let mut target = 1.0;
        move || {
            smoother.set_target(target);
            target = 1.0 - target;
            let mut sum = 0.0;
            for _ in 0..BLOCK {
                sum += smoother.step();
            }
            black_box(sum);
        }
    });

    // A soft clipper oversampled by 2, 4 and 8: a block in, through the
    // filters up, the clipper, and the filters down.
    for (stages, name) in [
        (1, "oversampler/2x"),
        (2, "oversampler/4x"),
        (3, "oversampler/8x"),
    ] {
        suite.micro(name, BLOCK, move || {
            let mut oversampler = Oversampler::new(stages, 1, BLOCK);
            let input = signal::sine(1_000.0, 48_000.0, BLOCK, 0.9);
            let mut audio = input.clone();
            move || {
                audio.copy_from_slice(&input);
                oversampler.process(0, &mut audio, |high| {
                    for sample in high.iter_mut() {
                        *sample = 2.0 * *sample / (1.0 + (2.0 * *sample).abs());
                    }
                });
                black_box(&audio);
            }
        });
    }

    // Mixing one block into another at the SIMD level this CPU dispatches.
    suite.micro("simd/mix", BLOCK, || {
        let source = signal::noise(1, BLOCK, 0.5);
        let mut bus = vec![0.0; BLOCK];
        move || {
            simd::mix(&mut bus, &source, 0.25);
            black_box(&bus);
        }
    });

    // Envelopes, a block per call. One voice, scalar: the per-sample cost
    // of a single envelope crossing stages.
    suite.micro("envelope/one", BLOCK, || {
        let mut envelope = Envelope::new(BUSY, Timing::new(48_000.0));
        let mut out = vec![0.0; BLOCK];
        let mut block = 0;
        move || {
            match envelope_event(block, 0) {
                Some(true) => envelope.trigger(Trigger::New),
                Some(false) => envelope.release(),
                None => {}
            }
            block += 1;
            envelope.process(&mut out);
            black_box(&out);
        }
    });

    // 32 voices with staggered notes, as 32 scalar envelopes and as one
    // bank in SIMD lanes, so the two compare.
    suite.micro("envelope/scalar-32", BLOCK, || {
        let mut envelopes = vec![Envelope::new(BUSY, Timing::new(48_000.0)); VOICES];
        let mut out = vec![0.0; BLOCK * VOICES];
        let mut block = 0;
        move || {
            for (voice, (envelope, out)) in envelopes
                .iter_mut()
                .zip(out.as_chunks_mut::<BLOCK>().0)
                .enumerate()
            {
                match envelope_event(block, voice) {
                    Some(true) => envelope.trigger(Trigger::New),
                    Some(false) => envelope.release(),
                    None => {}
                }
                envelope.process(out);
            }
            block += 1;
            black_box(&out);
        }
    });
    suite.micro("envelope/simd-32", BLOCK, || {
        let mut bank = EnvelopeBank::new(VOICES, BUSY, Timing::new(48_000.0));
        let mut out = vec![0.0; BLOCK * bank.stride()];
        let mut block = 0;
        move || {
            for voice in 0..VOICES {
                match envelope_event(block, voice) {
                    Some(true) => bank.trigger(voice, Trigger::New),
                    Some(false) => bank.release(voice),
                    None => {}
                }
            }
            block += 1;
            bank.process(&mut out);
            black_box(&out);
        }
    });

    // 32 voices holding their sustain, and 32 idle: what a bank costs when
    // nothing moves.
    suite.micro("envelope/sustain-32", BLOCK, || {
        let mut bank = EnvelopeBank::new(VOICES, BUSY, Timing::new(48_000.0));
        let mut out = vec![0.0; BLOCK * bank.stride()];
        for voice in 0..VOICES {
            bank.trigger(voice, Trigger::New);
        }
        bank.process(&mut out);
        move || {
            bank.process(&mut out);
            black_box(&out);
        }
    });
    suite.micro("envelope/idle-32", BLOCK, || {
        let mut bank = EnvelopeBank::new(VOICES, BUSY, Timing::new(48_000.0));
        let mut out = vec![0.0; BLOCK * bank.stride()];
        move || {
            bank.process(&mut out);
            black_box(&out);
        }
    });

    // Ten LFO slots for 32 voices is a full patch; one slot's block for 32
    // voices, per voice-sample, for the drawn, path and chaos types, one
    // voice at a time (scalar) and across voices in SIMD lanes.
    for (kind, name) in [
        (LfoType::Drawn, "drawn"),
        (LfoType::Path, "path"),
        (LfoType::Lorenz, "lorenz"),
    ] {
        for simd_path in [false, true] {
            let id = format!("lfo/{name}/{}", if simd_path { "simd" } else { "scalar" });
            suite.micro(&id, BLOCK * LFO_VOICES, move || {
                let mut bank = lfo_bank(kind);
                let transport = Transport::new(48_000.0);
                let mut out = vec![0.0; BLOCK * LFO_VOICES];
                let mut out_y = out.clone();
                move || {
                    if simd_path {
                        let level = simd::level();
                        bank.process_at(level, &transport, BLOCK, &mut out, Some(&mut out_y));
                    } else {
                        bank.process_scalar(&transport, BLOCK, &mut out, Some(&mut out_y));
                    }
                    black_box((&out, &out_y));
                }
            });
        }
    }

    // Every filter type, in all four groups, over a block of 16 mono
    // voices, each with its cutoff swept at audio rate and its own
    // resonance, drive and character: one voice at a time (scalar), and in
    // SIMD lanes across voices. Items are voice samples, so the time per
    // item is the cost of one voice's sample.
    for kind in FilterType::ALL {
        for simd in [false, true] {
            let name = format!("filter/{kind}/{}", if simd { "simd" } else { "scalar" });
            suite.micro(&name, BLOCK * FILTER_VOICES, move || {
                let mut bank = if simd {
                    FilterBank::new(48_000.0, FILTER_VOICES, 1)
                } else {
                    FilterBank::scalar(48_000.0, FILTER_VOICES, 1)
                };
                bank.set_type(kind);
                let stride = bank.stride();
                for voice in 0..FILTER_VOICES {
                    let x = voice as f32 / FILTER_VOICES as f32;
                    bank.reset_voice(
                        voice,
                        &VoiceParams {
                            resonance: 0.9 * x,
                            drive: 0.5 * x,
                            saturation: x,
                            character: x,
                            ..VoiceParams::default()
                        },
                    );
                }
                let pitch: Vec<f32> = (0..BLOCK * stride)
                    .map(|i| 70.0 + 30.0 * ((i / stride) as f32 * 0.01 + (i % stride) as f32).sin())
                    .collect();
                let noise = signal::noise(2, BLOCK * stride, 0.5);
                let mut audio = noise.clone();
                move || {
                    audio.copy_from_slice(&noise);
                    bank.process(&pitch, &mut audio);
                    black_box(&audio);
                }
            });
        }
    }

    suite.main()
}

/// Voices in the LFO benchmarks.
const LFO_VOICES: usize = 32;

/// An LFO slot of `kind` for 32 voices at 5 Hz, each voice started at a
/// different time, the drawn shape eight curved points.
fn lfo_bank(kind: LfoType) -> LfoBank {
    let settings = Settings {
        kind,
        rate: Rate::Hz {
            hz: 5.0,
            x10: false,
        },
        ..Settings::default()
    };
    let mut lfo = Lfo::new(settings, 1);
    let points: Vec<Point> = (0..8)
        .map(|i| Point::new(i as f32 / 7.0, if i % 2 == 0 { 0.1 } else { 0.9 }).curve(0.3))
        .collect();
    lfo.shape_mut().set_points(&points);
    let mut bank = LfoBank::new(lfo, LFO_VOICES);
    for v in 0..LFO_VOICES {
        bank.trigger(v, v * 11);
    }
    let mut out = vec![0.0; BLOCK * LFO_VOICES];
    bank.process(&Transport::new(48_000.0), BLOCK, &mut out, None);
    bank
}
