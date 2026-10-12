//! The modulation rules, each test citing the specification's rule
//! (engine-core sections 11 and 12) or the crate's documented rule.

use lspk_dsp::lfo::{Point, Shape};
use lspk_dsp::simd::Level;
use lspk_params::ParamId;

use super::*;
use crate::curve::{LUT_SIZE, lookup};

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;
const VOICES: usize = 8;

const CUTOFF: Target = Target::Param(ParamId(1));
const PITCH: Target = Target::Param(ParamId(2));
const AMP: Target = Target::Param(ParamId(3));
const MASTER: Target = Target::Param(ParamId(4));
const TUNING: Target = Target::Param(ParamId(5));
const FM: Target = Target::Param(ParamId(6));
const MASTER2: Target = Target::Param(ParamId(7));
const MACRO1: Target = Target::Param(ParamId(10));
const AMOUNT0: Target = Target::Param(ParamId(20));
const BUS: Target = Target::LfoBus { lfo: 0, bus: 0 };

fn config() -> Config {
    Config {
        sample_rate: SR,
        max_frames: BLOCK,
        voices: VOICES,
    }
}

/// A plug-in's registries, matrix and engine.
struct Rig {
    sources: Sources,
    dests: Destinations,
    matrix: Matrix,
    engine: Engine,
}

impl Rig {
    fn new() -> Self {
        let sources = Sources::standard(&Standard {
            envelopes: 4,
            lfos: 10,
            macros: 8,
            sequence_values: 2,
            audio: &[("osc_a", "A audio")],
        });
        let mut dests = Destinations::new();
        let macro1 = sources.find("macro1").unwrap();
        for info in [
            DestinationInfo::param(ParamId(1), "Cutoff").voice(),
            DestinationInfo::param(ParamId(2), "Pitch").voice().audio(),
            DestinationInfo::param(ParamId(3), "Amp").voice().audio(),
            DestinationInfo::param(ParamId(4), "Master"),
            DestinationInfo::param(ParamId(5), "Tuning").unbounded(),
            DestinationInfo::param(ParamId(6), "FM").voice().adaptive(),
            DestinationInfo::param(ParamId(7), "Master 2"),
            DestinationInfo::macro_control(ParamId(10), "Macro 1", macro1),
            DestinationInfo::slot_amount(ParamId(20), "Slot 1 amount", 0),
            DestinationInfo::lfo_bus(0, 0),
        ] {
            dests.add(info).unwrap();
        }
        let engine = Engine::new(&sources, &dests, config());
        Self {
            sources,
            dests,
            matrix: Matrix::new(),
            engine,
        }
    }

    fn src(&self, key: &str) -> SourceId {
        self.sources.find(key).unwrap()
    }

    fn dest(&self, target: Target) -> DestId {
        self.dests.find(target).unwrap()
    }

    fn add(&mut self, source: &str, target: Target, amount: f32, polarity: Polarity) -> usize {
        let s = self.src(source);
        self.matrix.add(s, target, amount, polarity).unwrap()
    }

    fn compile(&mut self) {
        let program = Program::compile(&self.matrix, &self.sources, &self.dests, config());
        drop(self.engine.swap(Box::new(program)));
    }

    fn held(&mut self, key: &str, value: f32) {
        let s = self.src(key);
        self.engine.set_held(s, value);
    }

    fn base(&mut self, target: Target, value: f32) {
        let d = self.dest(target);
        self.engine.set_base(d, value);
    }

    /// Processes `frames` with no signals bound.
    fn run(&mut self, frames: usize) {
        self.engine.process(&Signals::new(), 0..frames);
    }

    fn latest(&self, target: Target, voice: usize) -> f32 {
        self.engine
            .latest(self.dest(target), voice)
            .expect("modulated")
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

// Spec: engine-core R102
#[test]
fn a_matrix_has_64_slots_with_one_destination_each() {
    let mut rig = Rig::new();
    assert_eq!(rig.matrix.slots().len(), 64);
    for _ in 0..64 {
        rig.add("macro1", MASTER, 0.1, Polarity::Unipolar);
    }
    let lfo = rig.src("lfo1");
    assert_eq!(
        rig.matrix.add(lfo, CUTOFF, 0.5, Polarity::Unipolar),
        Err(EditError::Full)
    );
    rig.matrix.remove(10).unwrap();
    assert_eq!(rig.matrix.add(lfo, CUTOFF, 0.5, Polarity::Unipolar), Ok(10));
    assert_eq!(rig.matrix.slots()[10].destination, Some(CUTOFF));
}

// Spec: engine-core R103
#[test]
fn a_slot_is_curve_of_source_times_amount_times_output_times_aux() {
    let mut rig = Rig::new();
    let n = rig.add("macro1", MASTER, 0.5, Polarity::Unipolar);
    rig.matrix.set_output(n, 0.8).unwrap();
    rig.compile();
    rig.base(MASTER, 0.1);
    rig.held("macro1", 0.6);
    rig.run(64);
    assert!(close(rig.latest(MASTER, 0), 0.1 + 0.6 * 0.5 * 0.8));

    // With an aux source, times the aux; inverted, times 1 − aux.
    let wheel = rig.src("mod_wheel");
    rig.matrix.set_aux(n, Some(wheel), false).unwrap();
    rig.compile();
    rig.held("mod_wheel", 0.25);
    rig.run(64);
    assert!(close(rig.latest(MASTER, 0), 0.1 + 0.6 * 0.5 * 0.8 * 0.25));
    rig.matrix.set_aux(n, Some(wheel), true).unwrap();
    rig.compile();
    rig.run(64);
    assert!(close(rig.latest(MASTER, 0), 0.1 + 0.6 * 0.5 * 0.8 * 0.75));
}

// Spec: engine-core R104
#[test]
fn a_dragged_route_is_two_way_on_a_centred_control_and_one_way_otherwise() {
    assert_eq!(drag_polarity(0.5, false), Polarity::Bipolar);
    assert_eq!(drag_polarity(0.503, false), Polarity::Bipolar);
    assert_eq!(drag_polarity(0.0, false), Polarity::Unipolar);
    assert_eq!(drag_polarity(0.7, false), Polarity::Unipolar);
    // The gesture's modifier gives the other polarity.
    assert_eq!(drag_polarity(0.5, true), Polarity::Unipolar);
    assert_eq!(drag_polarity(0.7, true), Polarity::Bipolar);
    let mut rig = Rig::new();
    let lfo = rig.src("lfo1");
    let n = rig
        .matrix
        .drag(lfo, CUTOFF, 0.5, 0.4, Drag::default())
        .unwrap();
    assert_eq!(rig.matrix.slots()[n].polarity, Polarity::Bipolar);
    // The polarity can be changed later.
    rig.matrix.set_polarity(n, Polarity::Unipolar).unwrap();
    assert_eq!(rig.matrix.slots()[n].polarity, Polarity::Unipolar);
}

// Spec: engine-core R105
#[test]
fn one_way_routes_add_upward_and_two_way_routes_swing_around_the_control() {
    let mut rig = Rig::new();
    rig.add("macro1", MASTER, 0.3, Polarity::Unipolar);
    rig.add("macro2", MASTER2, 0.3, Polarity::Bipolar);
    rig.compile();
    rig.base(MASTER, 0.5);
    rig.base(MASTER2, 0.5);
    for (macro_value, one_way, two_way) in [(0.0, 0.5, 0.2), (0.5, 0.65, 0.5), (1.0, 0.8, 0.8)] {
        rig.held("macro1", macro_value);
        rig.held("macro2", macro_value);
        rig.run(32);
        assert!(close(rig.latest(MASTER, 0), one_way), "{macro_value}");
        assert!(close(rig.latest(MASTER2, 0), two_way), "{macro_value}");
    }
    // A bipolar source used one way is first taken to 0 to 1.
    let mut rig = Rig::new();
    rig.add("pitch_bend", MASTER, 0.4, Polarity::Unipolar);
    rig.add("pitch_bend", MASTER2, 0.4, Polarity::Bipolar);
    rig.compile();
    rig.held("pitch_bend", -1.0);
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.0));
    rig.held("pitch_bend", 0.5);
    rig.base(MASTER2, 0.5);
    rig.run(32);
    assert!(close(rig.latest(MASTER2, 0), 0.5 + 0.5 * 0.4));
}

// Spec: engine-core R106
#[test]
fn a_negative_amount_inverts_the_source() {
    let mut rig = Rig::new();
    rig.add("macro1", MASTER, -0.3, Polarity::Unipolar);
    rig.add("macro1", MASTER2, -0.3, Polarity::Bipolar);
    rig.compile();
    rig.base(MASTER, 0.6);
    rig.base(MASTER2, 0.6);
    rig.held("macro1", 1.0);
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.3));
    assert!(close(rig.latest(MASTER2, 0), 0.3));
    rig.held("macro1", 0.0);
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.6));
    assert!(close(rig.latest(MASTER2, 0), 0.9));
}

// Spec: engine-core R107
#[test]
fn a_curve_at_50_percent_is_linear_and_bypassed() {
    let mut rig = Rig::new();
    let n = rig.add("macro1", MASTER, 1.0, Polarity::Unipolar);
    rig.compile();
    let tables = rig.engine.program().luts.len();
    // Only the velocity and key curves have tables.
    assert_eq!(tables, 2 * (LUT_SIZE + 1));
    rig.held("macro1", 0.3);
    rig.run(32);
    assert_eq!(rig.latest(MASTER, 0), 0.3);
    // A bend remaps the source by its curve.
    rig.matrix
        .set_curve(n, matrix::CurveSlot::Source, Curve::Bend(0.8))
        .unwrap();
    rig.compile();
    rig.run(32);
    let bent = Curve::Bend(0.8).value(0.3);
    assert!(bent > 0.3, "above 50 % rises quickly first");
    assert!((rig.latest(MASTER, 0) - bent).abs() < 1e-4);
}

// Spec: engine-core R107
#[test]
fn a_drawn_curve_remaps_the_source_and_its_rise_and_fall_slew_it() {
    let mut rig = Rig::new();
    let n = rig.add("mpe_z", AMP, 1.0, Polarity::Unipolar);
    let shape = Shape::new(&[Point::new(0.0, 1.0), Point::new(1.0, 0.0)]);
    rig.matrix
        .set_curve(n, matrix::CurveSlot::Source, Curve::drawn(shape.clone()))
        .unwrap();
    rig.compile();
    rig.engine.note_on(0, NoteOn::new(60.0, 1.0));
    rig.held("mpe_z", 0.25);
    rig.run(16);
    let amp = rig.dest(AMP);
    let Output::Audio(out) = rig.engine.output(amp) else {
        panic!("amp runs at audio rate")
    };
    assert!((out.at(15, 0) - 0.75).abs() < 1e-4);

    // Rise over 10 ms: full scale in 480 samples, a straight line up.
    let mut slewed = Curve::drawn(Shape::new(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)]));
    if let Curve::Drawn(d) = &mut slewed {
        d.rise = 0.01;
        d.fall = 0.02;
    }
    rig.matrix
        .set_curve(n, matrix::CurveSlot::Source, slewed)
        .unwrap();
    rig.compile();
    rig.held("mpe_z", 0.0);
    rig.run(64);
    rig.held("mpe_z", 1.0);
    rig.engine.process(&Signals::new(), 0..BLOCK);
    let Output::Audio(out) = rig.engine.output(amp) else {
        panic!()
    };
    for k in [0, 99, 239, 478] {
        let want = (k + 1) as f32 / 480.0;
        assert!((out.at(k, 0) - want).abs() < 1e-4, "{k}: {}", out.at(k, 0));
    }
    assert!((out.at(500, 0) - 1.0).abs() < 1e-6);
    // Fall takes 20 ms.
    rig.held("mpe_z", 0.0);
    rig.engine.process(&Signals::new(), 0..BLOCK);
    let Output::Audio(out) = rig.engine.output(amp) else {
        panic!()
    };
    assert!((out.at(479, 0) - 0.5).abs() < 1e-3);
}

// Spec: engine-core R108, R137
#[test]
fn routes_to_a_destination_add_and_the_sum_is_clamped_to_its_range() {
    let mut rig = Rig::new();
    rig.add("macro1", MASTER, 0.3, Polarity::Unipolar);
    rig.add("macro2", MASTER, 0.2, Polarity::Unipolar);
    rig.add("macro1", TUNING, 1.0, Polarity::Unipolar);
    rig.add("macro2", TUNING, 1.0, Polarity::Unipolar);
    rig.compile();
    rig.base(MASTER, 0.1);
    rig.base(TUNING, 0.9);
    rig.held("macro1", 1.0);
    rig.held("macro2", 0.5);
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.1 + 0.3 + 0.1));
    rig.held("macro2", 1.0);
    rig.base(MASTER, 0.8);
    rig.run(32);
    assert_eq!(rig.latest(MASTER, 0), 1.0);
    // An unbounded destination (main tuning) isn't clamped.
    assert!(close(rig.latest(TUNING, 0), 2.9));
}

// Spec: engine-core R109
#[test]
fn limit_depth_on_drop_lets_a_full_source_just_reach_the_end() {
    assert!(close(limit_depth(0.7, Polarity::Unipolar, 1.0), 0.3));
    assert!(close(limit_depth(0.7, Polarity::Unipolar, 0.2), 0.2));
    assert!(close(limit_depth(0.7, Polarity::Unipolar, -1.0), -0.7));
    assert!(close(limit_depth(0.5, Polarity::Bipolar, 0.9), 0.5));
    assert!(close(limit_depth(0.2, Polarity::Bipolar, -0.9), -0.2));
    let mut rig = Rig::new();
    let lfo = rig.src("lfo1");
    let n = rig
        .matrix
        .drag(
            lfo,
            CUTOFF,
            0.75,
            1.0,
            Drag {
                toggle: false,
                limit: true,
            },
        )
        .unwrap();
    assert!(close(rig.matrix.slots()[n].amount, 0.25));
}

// Spec: engine-core R110
#[test]
fn note_on_sources_take_a_value_at_note_on_and_hold_it() {
    let mut rig = Rig::new();
    rig.add("random1", CUTOFF, 1.0, Polarity::Unipolar);
    rig.add("random1", FM, 1.0, Polarity::Unipolar);
    rig.add("random_route", PITCH, 1.0, Polarity::Unipolar);
    rig.add("random_route", AMP, 1.0, Polarity::Unipolar);
    rig.add("alternate1", BUS, 1.0, Polarity::Unipolar);
    rig.compile();
    let mut seen = Vec::new();
    for note in 0..4 {
        let voice = note % 2;
        rig.engine.note_on(voice, NoteOn::new(60.0, 0.8));
        rig.run(64);
        let r1 = rig.latest(CUTOFF, voice);
        // Random 1 is the same for every route using it.
        assert_eq!(r1, rig.latest(FM, voice));
        // Random per route differs between routes.
        let (a, b) = (rig.latest(PITCH, voice), rig.latest(AMP, voice));
        assert_ne!(a, b);
        // Held while the note plays.
        rig.run(BLOCK);
        assert_eq!(rig.latest(CUTOFF, voice), r1);
        assert_eq!(rig.latest(PITCH, voice), a);
        // Alternate flips at each note-on.
        let alt = rig.latest(BUS, voice);
        assert_eq!(alt, if note % 2 == 0 { 1.0 } else { 0.0 });
        seen.push(r1);
    }
    seen.dedup();
    assert_eq!(seen.len(), 4, "each note takes a new value");

    // Alternate 2 is the inverse of 1; release velocity is taken at
    // note-off and held.
    let mut rig = Rig::new();
    rig.add("alternate1", MASTER, 1.0, Polarity::Unipolar);
    rig.add("alternate2", MASTER2, 1.0, Polarity::Unipolar);
    rig.add("release_velocity", CUTOFF, 1.0, Polarity::Unipolar);
    rig.compile();
    rig.engine.note_on(0, NoteOn::new(60.0, 0.8));
    rig.run(32);
    assert_eq!(rig.latest(MASTER, 0) + rig.latest(MASTER2, 0), 1.0);
    assert_eq!(rig.latest(CUTOFF, 0), 0.0);
    rig.engine.note_off(0, 0.4);
    rig.run(64);
    assert!(close(rig.latest(CUTOFF, 0), 0.4));
    rig.run(BLOCK);
    assert!(close(rig.latest(CUTOFF, 0), 0.4));
}

// Spec: engine-core R111
#[test]
fn macros_lfo_buses_and_slot_amounts_are_destinations() {
    // A route into Macro 1's control moves what Macro 1 modulates, in the
    // same tick.
    let mut rig = Rig::new();
    rig.add("macro1", MASTER, 1.0, Polarity::Unipolar);
    rig.add("mod_wheel", MACRO1, 0.4, Polarity::Unipolar);
    rig.compile();
    rig.held("macro1", 0.2);
    rig.held("mod_wheel", 0.5);
    rig.run(1);
    assert!(close(rig.latest(MACRO1, 0), 0.4));
    assert!(close(rig.latest(MASTER, 0), 0.4));
    rig.held("mod_wheel", 1.0);
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.6));

    // A route into slot 0's amount scales slot 0's route.
    let mut rig = Rig::new();
    let n = rig.add("macro2", MASTER2, 0.5, Polarity::Unipolar);
    assert_eq!(n, 0);
    rig.add("mod_wheel", AMOUNT0, 0.5, Polarity::Unipolar);
    rig.compile();
    rig.held("macro2", 0.8);
    rig.held("mod_wheel", 0.0);
    rig.run(32);
    assert!(close(rig.latest(MASTER2, 0), 0.4));
    rig.held("mod_wheel", 1.0);
    rig.run(32);
    assert!(close(rig.latest(MASTER2, 0), 0.8));

    // An LFO point bus, per voice, from −1 to 1 around 0.
    let mut rig = Rig::new();
    rig.add("velocity", BUS, -1.0, Polarity::Bipolar);
    rig.compile();
    rig.engine.note_on(3, NoteOn::new(60.0, 0.25));
    rig.run(32);
    assert!(close(rig.latest(BUS, 3), 0.5));
}

// Spec: engine-core R112
#[test]
fn create_vibrato_and_velocity_to_amp_are_commands() {
    let mut rig = Rig::new();
    rig.add("lfo1", CUTOFF, 0.5, Polarity::Unipolar);
    let result = rig
        .matrix
        .run(
            "mod.create_vibrato",
            serde_json::json!({ "destination": TUNING, "amount": 0.1 }),
            &rig.sources,
            &rig.dests,
        )
        .unwrap();
    let n = result["slot"].as_u64().unwrap() as usize;
    let slot = &rig.matrix.slots()[n];
    assert_eq!(slot.source, Some(rig.src("lfo2")), "the next unused LFO");
    assert_eq!(slot.aux, Some(rig.src("mod_wheel")));
    assert_eq!(slot.polarity, Polarity::Bipolar);
    assert_eq!(slot.destination, Some(TUNING));
    let result = rig
        .matrix
        .run(
            "mod.create_velocity_to_amp",
            serde_json::json!({ "destination": AMP, "amount": 1.0 }),
            &rig.sources,
            &rig.dests,
        )
        .unwrap();
    let slot = &rig.matrix.slots()[result["slot"].as_u64().unwrap() as usize];
    assert_eq!(slot.source, Some(rig.src("velocity")));
    assert_eq!(slot.polarity, Polarity::Unipolar);
}

// Spec: engine-core R113 (the newest voice wins, a first rule until
// observed)
#[test]
fn a_per_voice_source_into_a_global_destination_takes_the_newest_voice() {
    let mut rig = Rig::new();
    rig.add("velocity", MASTER, 1.0, Polarity::Unipolar);
    rig.compile();
    rig.engine.note_on(2, NoteOn::new(60.0, 0.3));
    rig.engine.note_on(5, NoteOn::new(64.0, 0.7));
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.7));
    rig.engine.voice_ended(5);
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.3));
}

// Spec: engine-core R99
#[test]
fn the_velocity_and_key_curves_map_through_their_drawn_graphs() {
    let mut rig = Rig::new();
    rig.add("velocity", CUTOFF, 1.0, Polarity::Unipolar);
    rig.add("key", MASTER, 1.0, Polarity::Unipolar);
    rig.compile();
    // Init is the diagonal.
    rig.engine.note_on(0, NoteOn::new(127.0, 0.6));
    rig.run(32);
    assert!(close(rig.latest(CUTOFF, 0), 0.6));
    assert!(close(rig.latest(MASTER, 0), 1.0));
    rig.matrix.note_curves.velocity = Shape::new(&[Point::new(0.0, 0.0), Point::new(1.0, 0.5)]);
    rig.matrix.note_curves.key = Shape::new(&[Point::new(0.0, 1.0), Point::new(1.0, 0.0)]);
    rig.compile();
    rig.engine.note_on(1, NoteOn::new(0.0, 1.0));
    rig.run(32);
    assert!(close(rig.latest(CUTOFF, 1), 0.5));
    assert!(close(rig.latest(MASTER, 0), 1.0));
}

// Spec: engine-core R100
#[test]
fn with_glide_a_legato_note_moves_the_curve_output_over_the_glide_time() {
    let mut rig = Rig::new();
    rig.add("key", PITCH, 1.0, Polarity::Unipolar);
    rig.matrix.note_curves.glide = true;
    rig.compile();
    rig.engine.note_on(0, NoteOn::new(0.0, 1.0));
    rig.run(64);
    let legato = NoteOn {
        key: 127.0,
        velocity: 1.0,
        legato: true,
        glide_seconds: 0.004,
    };
    rig.engine.note_on(0, legato);
    rig.engine.process(&Signals::new(), 0..BLOCK);
    let Output::Audio(out) = rig.engine.output(rig.dest(PITCH)) else {
        panic!()
    };
    // 192 samples of glide; the output moves a chunk at a time.
    assert!(out.at(0, 0) < 0.01);
    assert!((out.at(96, 0) - 0.5).abs() < 0.2);
    assert!(close(out.at(200, 0), 1.0));
    // Without glide, the curve jumps.
    rig.matrix.note_curves.glide = false;
    rig.compile();
    rig.engine.note_on(0, NoteOn { key: 0.0, ..legato });
    rig.run(1);
    assert!(close(rig.latest(PITCH, 0), 0.0));
}

// Spec: engine-core R101
#[test]
fn velocity_legato_decides_whether_a_legato_note_updates_velocity() {
    for update in [false, true] {
        let mut rig = Rig::new();
        rig.add("velocity", CUTOFF, 1.0, Polarity::Unipolar);
        rig.matrix.note_curves.velocity_legato = update;
        rig.compile();
        rig.engine.note_on(0, NoteOn::new(60.0, 0.3));
        rig.run(32);
        rig.engine.note_on(
            0,
            NoteOn {
                legato: true,
                ..NoteOn::new(62.0, 0.9)
            },
        );
        rig.run(32);
        let want = if update { 0.9 } else { 0.3 };
        assert!(close(rig.latest(CUTOFF, 0), want), "{update}");
    }
}

/// A frame-major per-voice signal: voice `v`'s value at frame `k` is
/// `f(k, v)`, one value per voice a frame.
fn signal(frames: usize, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
    signal_at(VOICES, frames, f)
}

/// [`signal`] with `stride` values a frame.
fn signal_at(stride: usize, frames: usize, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
    let mut out = vec![0.0; frames * stride];
    for k in 0..frames {
        for v in 0..VOICES {
            out[k * stride + v] = f(k, v);
        }
    }
    out
}

/// The engine's stride, which the plain row path needs.
const WIDE: usize = 16;

// Rule: rates (the crate's control-rate rule)
#[test]
fn audio_routes_run_every_sample_and_control_routes_tick_every_period() {
    let mut rig = Rig::new();
    rig.add("env1", PITCH, 0.5, Polarity::Unipolar);
    rig.add("env1", CUTOFF, 0.5, Polarity::Unipolar);
    rig.compile();
    let env = signal(BLOCK, |k, v| {
        (k as f32 / BLOCK as f32) * (v + 1) as f32 / VOICES as f32
    });
    let mut signals = Signals::new();
    signals.bind(rig.src("env1"), &env, VOICES);
    for v in 0..VOICES {
        rig.engine.note_on(v, NoteOn::new(60.0, 1.0));
    }
    rig.engine.process(&signals, 0..BLOCK);
    let pitch = rig.dest(PITCH);
    let cutoff = rig.dest(CUTOFF);
    assert_eq!(rig.engine.program().rate(pitch), Some(Rate::Audio));
    assert_eq!(rig.engine.program().rate(cutoff), Some(Rate::Control));
    let Output::Audio(audio) = rig.engine.output(pitch) else {
        panic!()
    };
    for k in [0, 1, 100, 511] {
        for v in [0, 3, 7] {
            assert!(close(audio.at(k, v), 0.5 * env[k * VOICES + v]));
        }
    }
    let Output::Control(control) = rig.engine.output(cutoff) else {
        panic!()
    };
    assert_eq!(control.chunks().len(), BLOCK / CONTROL_PERIOD);
    for (c, chunk) in control.chunks().iter().enumerate() {
        assert_eq!(chunk.start, c * CONTROL_PERIOD);
        assert!(chunk.tick);
        // The tick reads the source at the period's first frame.
        assert!(close(
            control.value(c, 7),
            0.5 * env[chunk.start * VOICES + 7]
        ));
    }
    // The ramp moves from one tick to the next across each period, one
    // period behind, and never steps by more than a period's change ÷ 32.
    let mut ramp = vec![0.0; BLOCK];
    control.ramp(7, &mut ramp);
    for k in CONTROL_PERIOD..BLOCK {
        let lagged = 0.5 * env[(k - CONTROL_PERIOD) * VOICES + 7];
        assert!((ramp[k] - lagged).abs() < 1e-5, "{k}");
    }
}

// Rule: rates (the crate's control-rate rule)
#[test]
fn an_adaptive_destination_runs_at_audio_rate_only_with_an_audio_source() {
    let mut rig = Rig::new();
    let n = rig.add("lfo1", FM, 0.5, Polarity::Bipolar);
    rig.compile();
    let fm = rig.dest(FM);
    assert_eq!(rig.engine.program().rate(fm), Some(Rate::Control));
    rig.add("osc_a", FM, 0.5, Polarity::Bipolar);
    rig.compile();
    assert_eq!(rig.engine.program().rate(fm), Some(Rate::Audio));
    rig.matrix.set_bypass(n + 1, true).unwrap();
    rig.compile();
    assert_eq!(rig.engine.program().rate(fm), Some(Rate::Control));
}

// Rule: rates; the error bound the crate documents
#[test]
fn a_control_route_stays_within_two_periods_change_of_audio_rate() {
    let mut rig = Rig::new();
    rig.add("lfo1", PITCH, 1.0, Polarity::Unipolar);
    rig.add("lfo1", CUTOFF, 1.0, Polarity::Unipolar);
    rig.compile();
    rig.engine.note_on(0, NoteOn::new(60.0, 1.0));
    let lfo = |k: usize| 0.5 + 0.5 * (std::f32::consts::TAU * 10.0 * k as f32 / SR).sin();
    let mut worst = 0.0f32;
    for block in 0..20 {
        let data = signal(BLOCK, |k, _| lfo(block * BLOCK + k));
        let mut signals = Signals::new();
        signals.bind(rig.src("lfo1"), &data, VOICES);
        rig.engine.process(&signals, 0..BLOCK);
        let Output::Audio(exact) = rig.engine.output(rig.dest(PITCH)) else {
            panic!()
        };
        let Output::Control(control) = rig.engine.output(rig.dest(CUTOFF)) else {
            panic!()
        };
        let mut ramp = vec![0.0; BLOCK];
        control.ramp(0, &mut ramp);
        if block > 0 {
            for (k, r) in ramp.iter().enumerate() {
                worst = worst.max((r - exact.at(k, 0)).abs());
            }
        }
    }
    assert!(worst < 0.042, "{worst}");
}

/// Plays a patch with audio, control, global and slewed routes for 3
/// blocks, splitting each block at `splits`, with notes at fixed frames on
/// voices 0 to 3; returns every audio-rate value and every control-rate
/// ramp value of those voices. (A voice that isn't playing has no defined
/// value: a SIMD level may compute it with its lane group or not.)
fn play_split(splits: &[usize], level: Option<Level>) -> (Vec<f32>, Vec<f32>) {
    const PLAYED: usize = 4;
    let mut rig = Rig::new();
    assert_eq!(rig.engine.stride(), WIDE);
    rig.add("env1", PITCH, 0.7, Polarity::Bipolar);
    rig.add("env1", CUTOFF, 0.3, Polarity::Bipolar);
    rig.add("lfo1", CUTOFF, 0.4, Polarity::Unipolar);
    rig.add("macro1", CUTOFF, 0.2, Polarity::Bipolar);
    rig.add("velocity", AMP, 0.5, Polarity::Unipolar);
    rig.add("random_route", MASTER, 0.5, Polarity::Unipolar);
    let n = rig.add("lfo1", AMP, -0.3, Polarity::Unipolar);
    let mut slew = Curve::drawn(Shape::new(&[
        Point::new(0.0, 0.0),
        Point::new(1.0, 1.0).curve(0.3),
    ]));
    if let Curve::Drawn(d) = &mut slew {
        d.rise = 0.001;
        d.fall = 0.003;
    }
    rig.matrix
        .set_curve(n, matrix::CurveSlot::Source, slew)
        .unwrap();
    let mw = rig.src("mod_wheel");
    rig.matrix.set_aux(n, Some(mw), true).unwrap();
    rig.compile();
    rig.held("macro1", 0.3);
    rig.held("mod_wheel", 0.2);
    rig.base(CUTOFF, 0.5);
    rig.base(PITCH, 0.5);
    let notes = [(0usize, 0usize), (77, 1), (300, 2), (700, 3), (1030, 0)];
    let mut audio = Vec::new();
    let mut control = vec![0.0; 3 * BLOCK * (VOICES + 1)];
    for block in 0..3 {
        // Envelope 1 bound at the engine's stride (rows of voices), LFO 1
        // at the voice count (a voice at a time).
        let env = signal_at(WIDE, BLOCK, |k, v| {
            ((block * BLOCK + k) as f32 * 0.003 + v as f32).sin() * 0.5 + 0.5
        });
        let lfo = signal(BLOCK, |k, v| {
            ((block * BLOCK + k) as f32 * 0.01 + v as f32 * 0.7).cos() * 0.5 + 0.5
        });
        let mut signals = Signals::new();
        signals.bind(rig.src("env1"), &env, WIDE);
        signals.bind(rig.src("lfo1"), &lfo, VOICES);
        let mut cuts: Vec<usize> = splits
            .iter()
            .copied()
            .chain(
                notes
                    .iter()
                    .map(|(f, _)| *f)
                    .filter(|f| f / BLOCK == block)
                    .map(|f| f % BLOCK),
            )
            .filter(|c| *c > 0 && *c < BLOCK)
            .collect();
        cuts.push(BLOCK);
        cuts.sort_unstable();
        cuts.dedup();
        let mut start = 0;
        for end in cuts {
            for (frame, voice) in notes {
                if frame == block * BLOCK + start {
                    rig.engine.note_on(
                        voice,
                        NoteOn::new(60.0 + voice as f32, 0.2 * voice as f32 + 0.1),
                    );
                }
            }
            match level {
                Some(level) => rig.engine.process_at(level, &signals, start..end),
                None => rig.engine.process(&signals, start..end),
            }
            let mut ramp = vec![0.0; end - start];
            for (dest, voices) in [(CUTOFF, 0..PLAYED), (MASTER, VOICES..VOICES + 1)] {
                let Output::Control(c) = rig.engine.output(rig.dest(dest)) else {
                    panic!()
                };
                for v in voices {
                    c.ramp(v, &mut ramp);
                    for (i, r) in ramp.iter().enumerate() {
                        control[(block * BLOCK + start + i) * (VOICES + 1) + v] = *r;
                    }
                }
            }
            start = end;
        }
        for dest in [PITCH, AMP] {
            let Output::Audio(a) = rig.engine.output(rig.dest(dest)) else {
                panic!()
            };
            for k in 0..BLOCK {
                for v in 0..PLAYED {
                    audio.push(a.at(k, v));
                }
            }
        }
    }
    (audio, control)
}

// Rule: sample accuracy and blocks (the crate's documented guarantee)
#[test]
fn results_dont_depend_on_how_blocks_are_split() {
    let (audio, control) = play_split(&[], None);
    for splits in [
        vec![1, 2, 3, 100, 101, 257],
        vec![31, 32, 33, 480],
        vec![7, 64, 333],
    ] {
        let (a, c) = play_split(&splits, None);
        assert_eq!(audio, a, "{splits:?}");
        assert_eq!(control, c, "{splits:?}");
    }
}

// Rule: the SIMD lanes give the same results as one voice at a time
#[test]
fn every_simd_level_gives_the_same_results() {
    let reference = play_split(&[100], Some(Level::Portable));
    for level in Level::ALL {
        if level.is_supported() {
            assert_eq!(play_split(&[100], Some(level)), reference, "{level}");
        }
    }
}

// Rule: sample accuracy
#[test]
fn a_note_takes_effect_at_its_frame() {
    let mut rig = Rig::new();
    rig.add("velocity", AMP, 1.0, Polarity::Unipolar);
    rig.add("velocity", CUTOFF, 1.0, Polarity::Unipolar);
    rig.compile();
    rig.engine.process(&Signals::new(), 0..100);
    rig.engine.note_on(4, NoteOn::new(60.0, 0.9));
    rig.engine.process(&Signals::new(), 100..BLOCK);
    let Output::Audio(amp) = rig.engine.output(rig.dest(AMP)) else {
        panic!()
    };
    assert!(close(amp.at(100, 4), 0.9));
    // The control route ticks at the note's frame, off the grid, without
    // ramping from the voice's old value.
    let Output::Control(cutoff) = rig.engine.output(rig.dest(CUTOFF)) else {
        panic!()
    };
    let first = cutoff.chunks()[0];
    assert_eq!(
        (first.start, first.phase, first.tick),
        (100, 100 % CONTROL_PERIOD, true)
    );
    let mut ramp = vec![0.0; BLOCK - 100];
    cutoff.ramp(4, &mut ramp);
    assert!(close(ramp[0], 0.9));
}

// Rule: only active routes cost anything
#[test]
fn only_routes_in_use_are_compiled() {
    let mut rig = Rig::new();
    let program = Program::compile(&rig.matrix, &rig.sources, &rig.dests, config());
    assert_eq!(program.route_count(), 0);
    rig.add("lfo1", CUTOFF, 0.5, Polarity::Unipolar);
    let bypassed = rig.add("lfo2", CUTOFF, 0.5, Polarity::Unipolar);
    rig.matrix.set_bypass(bypassed, true).unwrap();
    rig.add("lfo3", Target::Param(ParamId(999)), 0.5, Polarity::Unipolar);
    let program = Program::compile(&rig.matrix, &rig.sources, &rig.dests, config());
    assert_eq!(program.route_count(), 1);
    assert_eq!(
        program.skipped(),
        &[Skipped {
            slot: 2,
            reason: SkipReason::UnknownDestination
        }]
    );
    // Destinations no route reaches have no output.
    assert_eq!(program.rate(rig.dest(PITCH)), None);
    rig.compile();
    rig.run(64);
    assert!(matches!(
        rig.engine.output(rig.dest(PITCH)),
        Output::Unmodulated
    ));
    // A route whose amount is 0 contributes nothing.
    rig.engine.set_amount(0, 0.0);
    rig.base(CUTOFF, 0.25);
    rig.engine.note_on(0, NoteOn::new(60.0, 1.0));
    let data = signal(BLOCK, |_, _| 1.0);
    let mut signals = Signals::new();
    signals.bind(rig.src("lfo1"), &data, VOICES);
    rig.engine.process(&signals, 0..64);
    assert_eq!(rig.latest(CUTOFF, 0), 0.25);
    rig.engine.set_amount(0, 0.5);
    rig.engine.process(&signals, 0..64);
    assert_eq!(rig.latest(CUTOFF, 0), 0.75);
}

#[test]
fn programs_reach_the_engine_through_the_mailbox_and_leave_it_the_same_way() {
    let mut rig = Rig::new();
    let (mut sender, receiver) = mailbox();
    rig.engine.attach(receiver);
    rig.add("macro1", MASTER, 1.0, Polarity::Unipolar);
    sender.send(Program::compile(
        &rig.matrix,
        &rig.sources,
        &rig.dests,
        config(),
    ));
    assert!(sender.pending());
    assert_eq!(rig.engine.program().route_count(), 0);
    rig.engine.begin_block();
    assert!(!sender.pending());
    assert_eq!(rig.engine.program().route_count(), 1);
    // The retired program waits in the mailbox for the sender to free it.
    assert!(sender.collect());
    assert!(!sender.collect());
    // A program sent twice before the engine takes it: the newer wins.
    sender.send(Program::empty(config()));
    sender.send(Program::compile(
        &rig.matrix,
        &rig.sources,
        &rig.dests,
        config(),
    ));
    rig.engine.begin_block();
    assert_eq!(rig.engine.program().route_count(), 1);
    // While the retired slot is full, the engine leaves the next program
    // waiting rather than free one itself.
    let (mut tx, mut rx) = mailbox();
    tx.send(Program::empty(config()));
    rx.retire(Box::new(Program::empty(config())));
    assert!(rx.take().is_none());
    assert!(tx.collect());
    assert!(rx.take().is_some());
    // A program made for another configuration is refused.
    let other = Config {
        voices: 64,
        ..config()
    };
    let refused = rig.engine.swap(Box::new(Program::empty(other)));
    assert_eq!(refused.config(), other);
    assert_eq!(rig.engine.program().route_count(), 1);
}

#[test]
fn the_commands_are_complete_and_edit_the_matrix() {
    let mut registry = lspk_commands::Registry::new();
    register(&mut registry).unwrap();
    assert!(registry.problems().is_empty(), "{:?}", registry.problems());
    assert_eq!(registry.into_iter().count(), 15);
    let mut rig = Rig::new();
    let (sources, dests) = (rig.sources.clone(), rig.dests.clone());
    let m = &mut rig.matrix;
    let run = |m: &mut Matrix, id: &str, args: serde_json::Value| m.run(id, args, &sources, &dests);
    let slot = run(
        m,
        "mod.add_route",
        serde_json::json!({"source": "lfo2", "destination": {"param": 1}, "amount": 0.5}),
    )
    .unwrap();
    assert_eq!(slot, serde_json::json!({"slot": 0}));
    run(m, "mod.add_route", serde_json::json!({"source": "env1", "destination": {"lfo_bus": {"lfo": 0, "bus": 0}}, "amount": -0.2, "polarity": "bipolar"}))
        .unwrap();
    run(
        m,
        "mod.set_bypass",
        serde_json::json!({"slot": 1, "bypass": true}),
    )
    .unwrap();
    run(
        m,
        "mod.set_amount",
        serde_json::json!({"slot": 0, "amount": 2.0}),
    )
    .unwrap();
    assert_eq!(m.slots()[0].amount, 1.0);
    run(
        m,
        "mod.set_aux",
        serde_json::json!({"slot": 0, "source": "mod_wheel", "invert": true}),
    )
    .unwrap();
    run(m, "mod.set_curve", serde_json::json!({"slot": 0, "which": "source", "curve": {"drawn": {"points": [{"x": 0, "y": 1}, {"x": 1, "y": 0}], "rise": 0.1}}}))
        .unwrap();
    assert_eq!(m.slots()[0].source_curve.slew(), (0.1, 0.0));
    let list = run(m, LIST_ID, serde_json::Value::Null).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["aux"], "mod_wheel");
    // Sort by source: Envelope 1 comes before LFO 2.
    run(m, "mod.sort", serde_json::json!({"by": "source"})).unwrap();
    assert_eq!(m.slots()[0].source, sources.find("env1"));
    run(m, "mod.sort", serde_json::json!({"by": "destination"})).unwrap();
    assert_eq!(m.slots()[0].destination, Some(CUTOFF));
    // Lock: loading a patch leaves a locked matrix as it is.
    run(m, "mod.lock", serde_json::json!({"locked": true})).unwrap();
    assert!(!m.load(&Matrix::new()));
    assert_eq!(m.describe(&sources).len(), 2);
    run(m, "mod.lock", serde_json::json!({"locked": false})).unwrap();
    let drag = run(m, "mod.drag", serde_json::json!({"source": "macro1", "destination": {"param": 4}, "position": 0.5, "amount": 0.8, "limit_depth": true}))
        .unwrap();
    let n = drag["slot"].as_u64().unwrap() as usize;
    assert_eq!(m.slots()[n].polarity, Polarity::Bipolar);
    assert!(close(m.slots()[n].amount, 0.5));
    let removed = run(
        m,
        "mod.remove_destination",
        serde_json::json!({"destination": {"param": 1}}),
    )
    .unwrap();
    assert_eq!(removed, serde_json::json!({"removed": 1}));
    run(m, "mod.remove_route", serde_json::json!({"slot": n})).unwrap();
    assert!(m.load(&Matrix::new()));
    assert!(m.describe(&sources).is_empty());
    // Bad calls are refused.
    assert!(matches!(
        run(m, "mod.nothing", serde_json::Value::Null),
        Err(EditError::UnknownCommand(_))
    ));
    assert!(matches!(
        run(
            m,
            "mod.add_route",
            serde_json::json!({"source": "nope", "destination": {"param": 1}, "amount": 1})
        ),
        Err(EditError::UnknownSource(_))
    ));
    assert!(matches!(
        run(
            m,
            "mod.add_route",
            serde_json::json!({"source": "lfo1", "destination": {"param": 99}, "amount": 1})
        ),
        Err(EditError::UnknownDestination(_))
    ));
    assert!(matches!(
        run(
            m,
            "mod.set_amount",
            serde_json::json!({"slot": 70, "amount": 1})
        ),
        Err(EditError::NoSlot(70))
    ));
    assert!(matches!(
        run(m, "mod.set_amount", serde_json::json!({"slot": 1})),
        Err(EditError::BadArgs(_))
    ));
}

const LIST_ID: &str = commands::LIST;

#[test]
fn voice_load_is_active_voices_over_the_limit() {
    let mut rig = Rig::new();
    rig.add("voice_load", MASTER, 1.0, Polarity::Unipolar);
    rig.compile();
    rig.engine.set_voice_limit(4);
    rig.engine.note_on(0, NoteOn::new(60.0, 1.0));
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.25));
    rig.engine.note_on(1, NoteOn::new(60.0, 1.0));
    rig.engine.note_on(2, NoteOn::new(60.0, 1.0));
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.75));
}

#[test]
fn the_standard_sources_are_the_matrix_list() {
    let sources = Sources::standard(&Standard {
        envelopes: 4,
        lfos: 10,
        macros: 8,
        sequence_values: 2,
        audio: &[
            ("osc_a", "A audio"),
            ("osc_b", "B audio"),
            ("osc_c", "C audio"),
            ("sub", "Sub audio"),
            ("noise", "Noise audio"),
            ("filter1", "Filter 1 audio"),
            ("filter2", "Filter 2 audio"),
        ],
    });
    // 49 sources in the reference's list, plus each LFO's Y output.
    assert_eq!(sources.len(), 49 + 10);
    for key in [
        "env1",
        "lfo10",
        "lfo3_y",
        "velocity",
        "key",
        "channel_pressure",
        "poly_pressure",
        "random1",
        "random2",
        "alternate1",
        "alternate2",
        "macro8",
        "mod_wheel",
        "pitch_bend",
        "mpe_x",
        "mpe_y",
        "mpe_z",
        "release_velocity",
        "constant",
        "osc_a",
        "filter2",
        "voice_load",
        "seq_value1",
        "seq_value2",
        "seq_index",
        "random_route",
    ] {
        assert!(sources.find(key).is_some(), "{key}");
    }
    let bend = sources.get(sources.find("pitch_bend").unwrap()).unwrap();
    assert_eq!(
        (bend.scope, bend.range),
        (Scope::Global, SourceRange::Bipolar)
    );
    let mut copy = sources.clone();
    assert!(
        copy.add(SourceInfo::new("env1", "Again", SourceKind::Signal))
            .is_err()
    );
}

#[test]
fn a_compiled_curve_stays_within_its_table_error() {
    for c in [0.0, 0.1, 0.3, 0.7, 1.0] {
        let curve = Curve::Bend(c);
        let mut table = Vec::new();
        curve.tabulate(&mut table);
        for i in 0..=4000 {
            let x = i as f32 / 4000.0;
            assert!(
                (lookup(&table, x) - curve.value(x)).abs() < 1e-4,
                "{c} at {x}"
            );
        }
    }
}

#[test]
fn the_constant_source_is_one_and_amount_automation_needs_no_recompile() {
    let mut rig = Rig::new();
    let n = rig.add("constant", MASTER, 0.2, Polarity::Unipolar);
    rig.compile();
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.2));
    rig.engine.set_amount(n, -0.1);
    rig.engine.set_output(n, 0.5);
    rig.base(MASTER, 0.5);
    rig.run(32);
    assert!(close(rig.latest(MASTER, 0), 0.45));
}

// Spec: engine-core R108 (the sum is clamped once, after every route)
#[test]
fn a_sum_is_clamped_after_every_route_has_added() {
    for stride in [VOICES, WIDE] {
        let mut rig = Rig::new();
        rig.add("env1", AMP, 0.5, Polarity::Unipolar);
        let n = rig.add("mpe_z", AMP, -0.5, Polarity::Unipolar);
        rig.matrix
            .set_curve(n, matrix::CurveSlot::Source, Curve::Bend(0.7))
            .unwrap();
        rig.compile();
        rig.base(AMP, 0.9);
        rig.held("mpe_z", 1.0);
        rig.engine.note_on(0, NoteOn::new(60.0, 1.0));
        let env = signal_at(stride, BLOCK, |_, _| 1.0);
        let mut signals = Signals::new();
        signals.bind(rig.src("env1"), &env, stride);
        rig.engine.process(&signals, 0..64);
        assert!(
            close(rig.latest(AMP, 0), 0.9),
            "{stride}: {}",
            rig.latest(AMP, 0)
        );
    }
}

// Rule: a plain route gives the same values a row at a time as a voice at
// a time
#[test]
fn plain_routes_agree_whether_run_by_rows_or_by_voices() {
    let run = |stride: usize| {
        let mut rig = Rig::new();
        rig.add("env1", PITCH, 0.6, Polarity::Bipolar);
        rig.add("env1", AMP, -0.3, Polarity::Unipolar);
        rig.add("env1", CUTOFF, 0.4, Polarity::Bipolar);
        rig.add("velocity", CUTOFF, 0.2, Polarity::Unipolar);
        rig.compile();
        rig.base(PITCH, 0.5);
        rig.base(AMP, 0.5);
        rig.base(CUTOFF, 0.3);
        for v in 0..VOICES {
            rig.engine.note_on(v, NoteOn::new(60.0, v as f32 / 8.0));
        }
        let env = signal_at(stride, BLOCK, |k, v| {
            ((k + 9 * v) as f32 * 0.01).sin() * 0.5 + 0.5
        });
        let mut signals = Signals::new();
        signals.bind(rig.src("env1"), &env, stride);
        rig.engine.process(&signals, 0..BLOCK);
        let mut values = Vec::new();
        for dest in [PITCH, AMP] {
            let Output::Audio(a) = rig.engine.output(rig.dest(dest)) else {
                panic!()
            };
            values.extend((0..BLOCK).flat_map(|k| (0..VOICES).map(move |v| a.at(k, v))));
        }
        let Output::Control(c) = rig.engine.output(rig.dest(CUTOFF)) else {
            panic!()
        };
        values.extend((0..c.chunks().len()).flat_map(|i| (0..VOICES).map(move |v| c.value(i, v))));
        values
    };
    let (rows, voices) = (run(WIDE), run(VOICES));
    for (r, v) in rows.iter().zip(&voices) {
        assert!((r - v).abs() < 1e-6, "{r} {v}");
    }
}
