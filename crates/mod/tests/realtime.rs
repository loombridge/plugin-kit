//! The engine touches no heap once made: program swaps through the
//! mailbox, notes, held values, amounts and processing of every kind of
//! route, inside the real-time section.

use lspk_dsp::lfo::{Point, Shape};
use lspk_dsp::simd;
use lspk_mod::matrix::CurveSlot;
use lspk_mod::{
    Config, Curve, DestinationInfo, Destinations, Engine, Matrix, NoteOn, Output, Polarity,
    Program, Signals, Sources, Standard, Target, mailbox,
};
use lspk_params::ParamId;

lspk_rt::install_trap!();

#[test]
fn the_engine_never_allocates_on_the_audio_thread() {
    const VOICES: usize = 16;
    const FRAMES: usize = 256;
    let _ = simd::level();
    let sources = Sources::standard(&Standard {
        envelopes: 4,
        lfos: 10,
        macros: 8,
        sequence_values: 2,
        audio: &[("osc_a", "A audio")],
    });
    let mut dests = Destinations::new();
    let macro1 = sources.find("macro1").unwrap();
    dests
        .add(
            DestinationInfo::param(ParamId(1), "Cutoff")
                .voice()
                .adaptive(),
        )
        .unwrap();
    dests
        .add(DestinationInfo::param(ParamId(2), "Pitch").voice().audio())
        .unwrap();
    dests
        .add(DestinationInfo::param(ParamId(3), "Master"))
        .unwrap();
    dests
        .add(DestinationInfo::macro_control(
            ParamId(4),
            "Macro 1",
            macro1,
        ))
        .unwrap();
    dests
        .add(DestinationInfo::slot_amount(ParamId(5), "Slot 1 amount", 0))
        .unwrap();
    dests.add(DestinationInfo::lfo_bus(0, 0)).unwrap();
    let config = Config {
        sample_rate: 48_000.0,
        max_frames: FRAMES,
        voices: VOICES,
    };
    let mut matrix = Matrix::new();
    let src = |key: &str| sources.find(key).unwrap();
    let p = |n: u32| Target::Param(ParamId(n));
    matrix
        .add(src("env1"), p(2), 0.5, Polarity::Bipolar)
        .unwrap();
    matrix
        .add(src("lfo1"), p(1), 0.3, Polarity::Unipolar)
        .unwrap();
    matrix
        .add(src("osc_a"), p(1), 0.1, Polarity::Bipolar)
        .unwrap();
    matrix
        .add(src("macro1"), p(3), 1.0, Polarity::Unipolar)
        .unwrap();
    matrix
        .add(src("mod_wheel"), p(4), 0.5, Polarity::Unipolar)
        .unwrap();
    matrix
        .add(src("velocity"), p(5), 0.5, Polarity::Unipolar)
        .unwrap();
    matrix
        .add(
            src("random_route"),
            Target::LfoBus { lfo: 0, bus: 0 },
            0.5,
            Polarity::Bipolar,
        )
        .unwrap();
    let n = matrix
        .add(src("key"), p(3), 0.2, Polarity::Unipolar)
        .unwrap();
    let mut drawn = Curve::drawn(Shape::new(&[Point::new(0.0, 0.2), Point::new(1.0, 0.9)]));
    if let Curve::Drawn(d) = &mut drawn {
        d.rise = 0.01;
    }
    matrix.set_curve(n, CurveSlot::Source, drawn).unwrap();
    matrix.set_aux(n, Some(src("alternate1")), true).unwrap();
    matrix.note_curves.glide = true;

    let mut engine = Engine::new(&sources, &dests, config);
    let (mut sender, receiver) = mailbox();
    engine.attach(receiver);
    sender.send(Program::compile(&matrix, &sources, &dests, config));
    let env = vec![0.5; FRAMES * VOICES];
    let lfo = vec![0.25; FRAMES * VOICES];
    let osc = vec![-0.5; FRAMES * VOICES];
    let mut ramp = vec![0.0; FRAMES];
    let mut total = 0.0;
    lspk_rt::rt_test!({
        for block in 0..8 {
            engine.begin_block();
            let mut signals = Signals::new();
            signals.bind(src("env1"), &env, VOICES);
            signals.bind(src("lfo1"), &lfo, VOICES);
            signals.bind(src("osc_a"), &osc, VOICES);
            engine.set_held(src("mod_wheel"), 0.1 * block as f32);
            engine.set_voice_held(src("mpe_z"), 3, 0.5);
            engine.set_amount(1, 0.4);
            engine.process(&signals, 0..100);
            engine.note_on(block % VOICES, NoteOn::new(40.0 + block as f32, 0.7));
            engine.note_on(
                (block + 1) % VOICES,
                NoteOn {
                    legato: true,
                    glide_seconds: 0.01,
                    ..NoteOn::new(50.0, 0.5)
                },
            );
            engine.process_at(simd::Level::Portable, &signals, 100..FRAMES);
            engine.note_off(block % VOICES, 0.3);
            if block % 3 == 2 {
                engine.voice_ended((block + 5) % VOICES);
            }
            for d in 0..6 {
                match engine.output(lspk_mod::DestId(d)) {
                    Output::Audio(a) => total += a.at(FRAMES - 1, 0),
                    Output::Control(c) => {
                        c.ramp(0, &mut ramp[..FRAMES - 100]);
                        total += ramp[0];
                    }
                    Output::Unmodulated => {}
                }
            }
        }
    });
    assert!(total.is_finite());
    assert!(
        sender.collect(),
        "the empty program came back to be freed here"
    );
}
