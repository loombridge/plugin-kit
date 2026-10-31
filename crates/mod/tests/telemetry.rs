//! The engine's live values reach an editor: a modulated value per voice is
//! published while a note plays, nothing is written while no editor
//! watches, and publishing touches no heap.

use lspk_mod::telemetry::{Layout, channel};
use lspk_mod::{
    Config, DestId, DestinationInfo, Destinations, Engine, Matrix, NoteOn, Polarity, Program,
    Signals, Sources, Standard, Target, mailbox,
};
use lspk_params::ParamId;

lspk_rt::install_trap!();

const VOICES: usize = 4;
const FRAMES: usize = 256;

#[test]
fn a_modulated_value_is_published_while_watched_and_off_the_heap() {
    let sources = Sources::standard(&Standard {
        envelopes: 1,
        lfos: 1,
        macros: 1,
        sequence_values: 0,
        audio: &[],
    });
    let mut dests = Destinations::new();
    for (n, name) in [(1, "Cutoff"), (2, "Level")] {
        dests
            .add(DestinationInfo::param(ParamId(n), name).voice().control())
            .unwrap();
    }
    let config = Config {
        sample_rate: 48_000.0,
        max_frames: FRAMES,
        voices: VOICES,
    };
    let src = |key: &str| sources.find(key).unwrap();
    let mut matrix = Matrix::new();
    matrix
        .add(
            src("env1"),
            Target::Param(ParamId(1)),
            0.5,
            Polarity::Unipolar,
        )
        .unwrap();
    let mut engine = Engine::new(&sources, &dests, config);
    let (mut sender, receiver) = mailbox();
    engine.attach(receiver);
    sender.send(Program::compile(&matrix, &sources, &dests, config));
    let env = vec![1.0; FRAMES * VOICES];

    let layout = Layout {
        destinations: dests.len(),
        voices: VOICES,
        lfos: 1,
        envelopes: 1,
        channels: 2,
        scope: 64,
        bins: 33,
    };
    let (mut publisher, mut watcher) = channel(layout);
    let env_source = src("env1");
    let block = |engine: &mut Engine| {
        engine.begin_block();
        let mut signals = Signals::new();
        signals.bind(env_source, &env, VOICES);
        engine.process(&signals, 0..FRAMES);
    };
    engine.note_on(1, NoteOn::new(60.0, 1.0));

    // Closed: nothing is due, so nothing is written.
    block(&mut engine);
    assert!(!publisher.due(FRAMES));
    assert!(watcher.latest().modulated_at(DestId(0), 1).is_none());

    // Open: the next block publishes the note's modulated cutoff.
    watcher.watch(true);
    lspk_rt::rt_test!({
        assert!(publisher.due(FRAMES));
        publisher.publish(|frame| frame.set_modulated(&engine));
    });
    let frame = watcher.latest();
    assert_eq!(
        frame.modulated_at(DestId(0), 1),
        engine.latest(DestId(0), 1)
    );
    let cutoff = frame
        .modulated_at(DestId(0), 1)
        .expect("the route reaches it");
    assert!(
        cutoff > 0.0,
        "an envelope at 1 raises the cutoff, read {cutoff}"
    );
    assert!(
        frame.modulated_at(DestId(0), 0).is_none(),
        "voice 0 isn't playing"
    );
    assert!(
        frame.modulated_at(DestId(1), 1).is_none(),
        "no route reaches Level"
    );
}
