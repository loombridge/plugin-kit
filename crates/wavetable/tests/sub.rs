//! The sub oscillator: its six shapes against their definitions, Octave
//! and Sweep, Phase and its options, Key tracking, and a render that
//! touches no heap.

use std::f32::consts::TAU;

use lspk_wavetable::osc::Onset;
use lspk_wavetable::sub::{Inputs, Params, Shape, Sub};

lspk_rt::install_trap!();

const RATE: f32 = 48_000.0;
const FRAMES: usize = 48_000;

/// A sub at Level 100 % with `shape`, its first note on A4.
fn a_sub(shape: Shape) -> Sub {
    let mut sub = Sub::new(1, RATE, FRAMES);
    *sub.params_mut() = Params {
        shape,
        level: 100.0,
        ..Params::default()
    };
    sub.start(0, 69.0, Onset::Fresh);
    sub
}

/// `sub` rendered `frames` frames at `pitch`, inside a real-time section,
/// so anything it allocates fails the test.
fn render_frames(sub: &mut Sub, frames: usize, pitch: f32) -> Vec<f32> {
    let pitch = [pitch];
    let mut out = vec![0.0; frames];
    let inputs = Inputs {
        lanes: 1,
        pitch: &pitch,
        ..Inputs::default()
    };
    lspk_rt::rt_test!({
        sub.process(frames, &inputs, &mut out);
    });
    out
}

/// `sub` rendered one second at `pitch`.
fn render(sub: &mut Sub, pitch: f32) -> Vec<f32> {
    render_frames(sub, FRAMES, pitch)
}

/// The measured fundamental of `signal`, in hertz.
fn frequency(signal: &[f32]) -> f64 {
    lspk_test::measure::pitch(signal, f64::from(RATE)).expect("a periodic signal")
}

// Spec: engine-core R40 (the six fixed shapes), section 5.
#[test]
fn every_shape_is_the_waveform_it_names() {
    // The reference walks the phase exactly as the oscillator does, so the
    // comparison sees the waveform, not float drift.
    let increment = (440.0f64 / 48_000.0) as f32;
    let check = |shape: Shape, skip: &dyn Fn(f32) -> bool, want: &dyn Fn(f32) -> f32| {
        let mut sub = a_sub(shape);
        let out = render(&mut sub, 69.0);
        let mut at = 0.0f32;
        for (k, &x) in out.iter().enumerate() {
            if !skip(at) {
                let expected = want(at);
                assert!(
                    (x - expected).abs() < 1e-5,
                    "{shape:?} at {k} (phase {at}): {x} against {expected}"
                );
            }
            at += increment;
            at -= at.floor();
        }
    };
    check(Shape::Sine, &|_| false, &|p| (TAU * p).sin());
    check(Shape::Triangle, &|_| false, &|p| {
        1.0 - 4.0 * ((p + 0.25).fract() - 0.5).abs()
    });
    check(Shape::Saw, &|p| (p - 0.5).abs() < 0.02, &|p| {
        2.0 * (p + 0.5).fract() - 1.0
    });
    check(
        Shape::Square,
        &|p| p < 0.02 || (p - 0.5).abs() < 0.02 || p > 0.98,
        &|p| if p < 0.5 { 1.0 } else { -1.0 },
    );
    check(
        Shape::Pulse,
        &|p| p < 0.02 || (p - 0.25).abs() < 0.02 || p > 0.98,
        &|p| if p < 0.25 { 1.0 } else { -1.0 },
    );
    // Rounded square: flat at ±1 away from its corners, through 0 at phase
    // 0.
    check(
        Shape::RoundedSquare,
        &|p| (p - 0.5).abs() < 0.1 || p < 0.1 || p > 0.9,
        &|p| if p < 0.5 { 1.0 } else { -1.0 },
    );
    let mut sub = a_sub(Shape::RoundedSquare);
    let out = render(&mut sub, 69.0);
    assert!(out[0].abs() < 1e-5, "a corner is halfway at phase 0");

    // Pulse is a quarter-cycle pulse (R44: unequal, its width isn't a
    // control): +1 through the first quarter, -1 through the rest, so its
    // mean is -1/2.
    let mut sub = a_sub(Shape::Pulse);
    let out = render(&mut sub, 69.0);
    let mean = out.iter().map(|&x| f64::from(x)).sum::<f64>() / out.len() as f64;
    assert!((mean + 0.5).abs() < 0.02, "pulse mean {mean}");
}

// Spec: engine-core R40 (Octave and Sweep), R5 (Key track off plays MIDI
// note 60).
#[test]
fn octave_sweep_and_key_tracking_move_the_pitch() {
    let note = 57.0; // A3, 220 Hz
    let mut sub = a_sub(Shape::Sine);
    sub.start(0, note, Onset::Fresh);
    let base = frequency(&render(&mut sub, note));
    assert!((base - 220.0).abs() < 0.5, "{base} Hz");

    let mut sub = a_sub(Shape::Sine);
    sub.params_mut().octave = 1;
    sub.start(0, note, Onset::Fresh);
    let octave = frequency(&render(&mut sub, note));
    assert!((octave - 440.0).abs() < 1.0, "{octave} Hz");

    let mut sub = a_sub(Shape::Sine);
    sub.params_mut().sweep = 12.0;
    sub.start(0, note, Onset::Fresh);
    let sweep = frequency(&render(&mut sub, note));
    assert!((sweep - 440.0).abs() < 1.0, "{sweep} Hz");

    let mut sub = a_sub(Shape::Sine);
    sub.params_mut().key_track = false;
    sub.start(0, note, Onset::Fresh);
    let flat = frequency(&render(&mut sub, note));
    // MIDI note 60: 261.63 Hz at A4 = 440 Hz.
    assert!((flat - 261.63).abs() < 0.5, "{flat} Hz");
}

// Spec: engine-core R41 (Phase and Continue phase).
#[test]
fn phase_starts_the_note_and_continue_phase_carries_it_on() {
    let mut sub = a_sub(Shape::Sine);
    sub.params_mut().phase = 90.0;
    sub.start(0, 69.0, Onset::Fresh);
    let out = render(&mut sub, 69.0);
    assert!((out[0] - 1.0).abs() < 1e-6, "phase 90° starts at the peak");

    // Continue phase off: a new note starts from Phase again. On: it
    // carries on from where the last note left.
    for continue_phase in [false, true] {
        let mut sub = a_sub(Shape::Sine);
        sub.params_mut().continue_phase = continue_phase;
        let _ = render_frames(&mut sub, 1_000, 69.0);
        let at = sub.phase(0);
        assert!(at > 0.1, "nine cycles in: {at}");
        sub.start(0, 69.0, Onset::Fresh);
        if continue_phase {
            assert_eq!(sub.phase(0), at, "Continue phase keeps the phase");
        } else {
            assert_eq!(sub.phase(0), 0.0, "a fresh note starts at Phase");
        }
    }
}

// Spec: engine-core R42 (Wrap phase).
#[test]
fn phase_past_a_cycle_wraps_only_when_asked() {
    // A quarter cycle past a whole one: 1.25 cycles read as 0.25 with Wrap
    // phase on, and hold at the end of the shape with it off.
    let modulation = vec![1.25f32; FRAMES];
    for wrap_phase in [true, false] {
        let mut sub = a_sub(Shape::Sine);
        sub.params_mut().wrap_phase = wrap_phase;
        let pitch = [69.0];
        let mut out = vec![0.0; FRAMES];
        let inputs = Inputs {
            lanes: 1,
            pitch: &pitch,
            phase_mod: Some(&modulation),
            ..Inputs::default()
        };
        lspk_rt::rt_test!({
            sub.process(FRAMES, &inputs, &mut out);
        });
        let want = if wrap_phase { 1.0 } else { 0.0 };
        assert!(
            (out[0] - want).abs() < 1e-5,
            "wrap {wrap_phase}: {} against {want}",
            out[0]
        );
    }
}

// Spec: engine-core R43 (the sub has the same level law as the
// oscillators, R7).
#[test]
fn level_follows_the_square_law() {
    let mut sub = a_sub(Shape::Sine);
    sub.params_mut().level = 50.0;
    let out = render(&mut sub, 69.0);
    let peak = out.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    assert!((peak - 0.25).abs() < 1e-3, "50 % is a quarter, {peak}");
    assert_eq!(Params::default().gain(), 0.5625, "75 % is -5.0 dB");
}
