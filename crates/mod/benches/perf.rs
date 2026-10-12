//! Micro-benchmarks for `lspk-mod`'s hot path, the engine's block: a full
//! matrix of 64 routes over 32 voices, mixed audio and control rate as a
//! patch has them; the same 64 routes all at audio rate, to show what the
//! control-rate rule saves; and an empty matrix, which should cost nothing.
//! `cargo xtask bench` times them, and CI's instruction-count gate covers
//! them.

use std::hint::black_box;
use std::process::ExitCode;

use lspk_dsp::lfo::{Point, Shape};
use lspk_mod::matrix::CurveSlot;
use lspk_mod::{
    Config, Curve, DestinationInfo, Destinations, Engine, Matrix, NoteOn, Polarity, Program,
    Signals, Sources, Standard, Target,
};
use lspk_params::ParamId;
use lspk_test::bench::Suite;

/// Frames in a block, as hosts commonly use.
const BLOCK: usize = 512;

/// Voices playing.
const VOICES: usize = 32;

/// Audio-rate destinations (pitch, FM depth, amplitude, pan per voice)
/// have IDs from 1; control-rate ones (cutoffs, resonances, frame
/// positions, levels) from 100; global ones from 200.
const AUDIO_DESTS: u32 = 4;
const CONTROL_DESTS: u32 = 12;
const GLOBAL_DESTS: u32 = 4;

fn sources() -> Sources {
    Sources::standard(&Standard {
        envelopes: 4,
        lfos: 10,
        macros: 8,
        sequence_values: 2,
        audio: &[("osc_a", "A audio")],
    })
}

/// The destinations, with every per-voice one at audio rate if
/// `all_audio`.
fn destinations(all_audio: bool) -> Destinations {
    let mut dests = Destinations::new();
    for n in 0..AUDIO_DESTS {
        dests
            .add(
                DestinationInfo::param(ParamId(1 + n), "Audio")
                    .voice()
                    .audio(),
            )
            .unwrap();
    }
    for n in 0..CONTROL_DESTS {
        let info = DestinationInfo::param(ParamId(100 + n), "Control").voice();
        dests
            .add(if all_audio { info.audio() } else { info })
            .unwrap();
    }
    for n in 0..GLOBAL_DESTS {
        dests
            .add(DestinationInfo::param(ParamId(200 + n), "Global"))
            .unwrap();
    }
    dests
}

/// 64 routes as a dense patch has them: 16 from envelopes and LFOs into
/// pitch, FM, amplitude and pan; 40 from LFOs, envelopes, velocity, key,
/// macros and the mod wheel into cutoffs, resonances and positions, a
/// quarter of them curved and some with an aux; 8 from macros into global
/// parameters.
fn matrix(sources: &Sources) -> Matrix {
    let mut m = Matrix::new();
    let s = |key: &str| sources.find(key).unwrap();
    let p = |n: u32| Target::Param(ParamId(n));
    let modulators = [
        "env1", "env2", "env3", "env4", "lfo1", "lfo2", "lfo3", "lfo4", "lfo5", "lfo6",
    ];
    for i in 0..16u32 {
        let src = s(modulators[i as usize % 8]);
        let polarity = if i % 2 == 0 {
            Polarity::Bipolar
        } else {
            Polarity::Unipolar
        };
        m.add(src, p(1 + i % AUDIO_DESTS), 0.1, polarity).unwrap();
    }
    let control_sources = [
        "lfo1",
        "lfo2",
        "lfo3",
        "env2",
        "env3",
        "velocity",
        "key",
        "macro1",
        "macro2",
        "mod_wheel",
    ];
    for i in 0..40u32 {
        let src = s(control_sources[i as usize % control_sources.len()]);
        let n = m
            .add(src, p(100 + i % CONTROL_DESTS), 0.2, Polarity::Unipolar)
            .unwrap();
        if i % 4 == 0 {
            m.set_curve(n, CurveSlot::Source, Curve::Bend(0.7)).unwrap();
        }
        if i % 5 == 0 {
            m.set_aux(n, Some(s("mod_wheel")), false).unwrap();
        }
        if i % 10 == 3 {
            let mut drawn = Curve::drawn(Shape::new(&[
                Point::new(0.0, 0.0),
                Point::new(0.5, 0.8).curve(0.3),
                Point::new(1.0, 1.0),
            ]));
            if let Curve::Drawn(d) = &mut drawn {
                d.rise = 0.005;
                d.fall = 0.02;
            }
            m.set_curve(n, CurveSlot::Source, drawn).unwrap();
        }
    }
    for i in 0..8u32 {
        let src = s(["macro3", "macro4", "macro5", "mod_wheel"][i as usize % 4]);
        m.add(src, p(200 + i % GLOBAL_DESTS), 0.3, Polarity::Bipolar)
            .unwrap();
    }
    m
}

/// A smooth per-voice signal, frame-major.
fn signal(seed: usize) -> Vec<f32> {
    (0..BLOCK * VOICES)
        .map(|i| {
            let (k, v) = (i / VOICES, i % VOICES);
            0.5 + 0.5 * ((k as f32 * 0.002 * (seed + 1) as f32) + v as f32).sin()
        })
        .collect()
}

/// An engine running `matrix` with every voice playing, and the block's
/// signals for its envelopes and LFOs.
fn engine(matrix: &Matrix, all_audio: bool) -> (Engine, Signals<'static>) {
    let sources = sources();
    let dests = destinations(all_audio);
    let config = Config {
        sample_rate: 48_000.0,
        max_frames: BLOCK,
        voices: VOICES,
    };
    let mut engine = Engine::new(&sources, &dests, config);
    drop(engine.swap(Box::new(Program::compile(matrix, &sources, &dests, config))));
    for v in 0..VOICES {
        engine.note_on(v, NoteOn::new(36.0 + v as f32, 0.5 + v as f32 / 64.0));
    }
    for key in [
        "macro1",
        "macro2",
        "macro3",
        "macro4",
        "macro5",
        "mod_wheel",
    ] {
        engine.set_held(sources.find(key).unwrap(), 0.4);
    }
    // Leaked so the signals can borrow them for the benchmark's life.
    let buffers: &'static [Vec<f32>] = Vec::leak((0..10).map(signal).collect());
    (engine, bind(&sources, buffers))
}

fn bind<'a>(sources: &Sources, buffers: &'a [Vec<f32>]) -> Signals<'a> {
    let mut signals = Signals::new();
    let keys = [
        "env1", "env2", "env3", "env4", "lfo1", "lfo2", "lfo3", "lfo4", "lfo5", "lfo6",
    ];
    for (key, data) in keys.iter().zip(buffers) {
        signals.bind(sources.find(key).unwrap(), data, VOICES);
    }
    signals
}

fn main() -> ExitCode {
    let mut suite = Suite::new("lspk-mod");

    // A block of the full matrix for 32 voices, per voice-sample.
    for (name, all_audio) in [("mixed-64x32", false), ("audio-64x32", true)] {
        suite.micro(name, BLOCK * VOICES, move || {
            let (mut engine, signals) = engine(&matrix(&sources()), all_audio);
            move || {
                engine.begin_block();
                engine.process(&signals, 0..BLOCK);
                black_box(&engine);
            }
        });
    }

    // An empty matrix with 32 voices playing: the cost of having the
    // engine at all.
    suite.micro("empty-32", BLOCK * VOICES, || {
        let (mut engine, signals) = engine(&Matrix::new(), false);
        move || {
            engine.begin_block();
            engine.process(&signals, 0..BLOCK);
            black_box(&engine);
        }
    });

    suite.main()
}
