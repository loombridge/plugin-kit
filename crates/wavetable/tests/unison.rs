//! Unison measured from the oscillator's renders: each copy's pitch under
//! every detune law and stack, the stereo width and blend levels of the
//! mix, level compensation as copies are added, frame spread, and each
//! copy's own start phase. The laws themselves are in `lspk-voice`'s
//! `unison` module, and tested per copy in its `tests/unison.rs`.

use std::f64::consts::TAU;

use lspk_dsp::simd;
use lspk_test::measure::{self, db, rms};
use lspk_voice::{DetuneLaw, Stack, Unison, UnisonLanes};
use lspk_wavetable::osc::{Inputs, Layout, Onset, Oscillator};
use lspk_wavetable::{Cache, FRAME_SIZE, SharedTable, Wavetable};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 128;
/// A3, 220 Hz.
const KEY: f32 = 57.0;

/// Frames of sines at harmonics `harmonics`, one per frame.
fn sines(harmonics: &[usize]) -> SharedTable {
    let frames: Vec<Vec<f32>> = harmonics
        .iter()
        .map(|&h| {
            (0..FRAME_SIZE)
                .map(|n| (h as f64 * n as f64 / FRAME_SIZE as f64 * TAU).sin() as f32)
                .collect()
        })
        .collect();
    Cache::global().share(Wavetable::new(&frames).unwrap())
}

/// One voice's oscillator and unison lanes, playing `KEY` at `unison`.
struct Stackup {
    osc: Oscillator,
    lanes: UnisonLanes,
}

impl Stackup {
    fn new(table: SharedTable, unison: Unison) -> Self {
        let copies = unison.copies;
        let mut osc = Oscillator::new(
            table,
            Layout {
                voices: 1,
                lanes_per_voice: copies,
            },
            RATE,
            BLOCK,
            9,
        );
        osc.params_mut().level = 100.0;
        let mut lanes = UnisonLanes::new(1, copies, 4);
        lanes.set(unison);
        lanes.start(0);
        osc.set_unison(lanes.lanes_of(0), lanes.pitch(), lanes.frame());
        osc.start(0, KEY, Onset::Fresh);
        Self { osc, lanes }
    }

    /// `frames` frames of every lane, and of the stereo mix.
    fn render(&mut self, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let n = self.osc.lane_count();
        let stride = self.lanes.voice_stride();
        let pitch = vec![KEY; n];
        let mut lanes = vec![0.0; frames * n];
        let mut mix = vec![0.0; frames * 2 * stride];
        let mut done = 0;
        while done < frames {
            let len = (frames - done).min(BLOCK);
            let inputs = Inputs {
                lanes: self.lanes.lanes_per_voice(),
                pitch: &pitch,
                ..Inputs::default()
            };
            let block = &mut lanes[done * n..(done + len) * n];
            self.osc.process(len, &inputs, block);
            self.lanes.mix(
                len,
                1,
                block,
                &mut mix[done * 2 * stride..(done + len) * 2 * stride],
            );
            done += len;
        }
        (lanes, mix)
    }

    /// Copy `copy`'s signal from a render's lanes.
    fn lane(&self, lanes: &[f32], copy: usize) -> Vec<f32> {
        let n = self.osc.lane_count();
        lanes.iter().skip(copy).step_by(n).copied().collect()
    }

    /// Channel `channel` (0 left, 1 right) of a render's mix.
    fn channel(&self, mix: &[f32], channel: usize) -> Vec<f32> {
        let stride = self.lanes.voice_stride();
        mix.iter()
            .skip(channel * stride)
            .step_by(2 * stride)
            .copied()
            .collect()
    }
}

fn hz(semitones: f32) -> f64 {
    220.0 * 2f64.powf(f64::from(semitones) / 12.0)
}

fn cents(got: f64, want: f64) -> f64 {
    1200.0 * (got / want).log2()
}

/// Each copy's measured pitch, in cents from the note.
fn measured_cents(unison: Unison, table: SharedTable) -> (Vec<f64>, Vec<f32>) {
    let mut s = Stackup::new(table, unison);
    let (lanes, _) = s.render(24_000);
    let offsets = s.lanes.pitch()[..unison.copies].to_vec();
    let got = (0..unison.copies)
        .map(|c| {
            let f = measure::pitch(&s.lane(&lanes, c), f64::from(RATE)).expect("a pitch");
            cents(f, 220.0)
        })
        .collect();
    (got, offsets)
}

#[test]
fn every_detune_law_s_spacing_measures_as_its_law() {
    // Spec: engine-core R33, R34: seven copies at full detune and the
    // default Range, the outer copies ±200 cents, each law's inner copies
    // where it puts them.
    for law in DetuneLaw::ALL {
        let unison = Unison {
            copies: 7,
            detune: 1.0,
            law,
            ..Unison::default()
        };
        let (got, offsets) = measured_cents(unison, sines(&[1]));
        for (c, (&g, &o)) in got.iter().zip(&offsets).enumerate() {
            let want = f64::from(o) * 100.0;
            assert!(
                (g - want).abs() < 0.5,
                "{law:?} copy {c}: measured {g:.2} cents, law {want:.2}"
            );
            if law != DetuneLaw::Random {
                let position = unison.law.position(unison.even(c));
                assert!((f64::from(position) * 200.0 - g).abs() < 0.5);
            }
        }
        if law != DetuneLaw::Random {
            assert!((got[0] + 200.0).abs() < 0.5 && (got[6] - 200.0).abs() < 0.5);
        }
        if law == DetuneLaw::Linear {
            // Equal steps, as observed.
            for pair in got.windows(2) {
                assert!((pair[1] - pair[0] - 200.0 / 3.0).abs() < 0.5, "{got:?}");
            }
        }
    }
}

#[test]
fn stacked_copies_measure_at_their_intervals() {
    // Spec: engine-core R36
    for (stack, copies, want) in [
        (
            Stack::Octave3,
            7,
            vec![36.0, 24.0, 12.0, 0.0, 12.0, 24.0, 36.0],
        ),
        (Stack::Fifth2, 5, vec![12.0, 7.0, 0.0, 7.0, 12.0]),
        (Stack::CentreDown24, 3, vec![0.0, -24.0, 0.0]),
        (Stack::CentreDown12, 2, vec![-12.0, -12.0]),
    ] {
        let unison = Unison {
            copies,
            detune: 0.0,
            stack,
            ..Unison::default()
        };
        let (got, _) = measured_cents(unison, sines(&[1]));
        for (c, (&g, &w)) in got.iter().zip(&want).enumerate() {
            assert!(
                (g - w * 100.0).abs() < 0.5,
                "{stack:?} copy {c}: {g:.2} cents"
            );
        }
    }
}

/// Each copy's amplitude in the left and right channels of the mix, the
/// copies three octaves apart (Range 12 at full detune, so their tones
/// separate cleanly).
fn copy_gains(unison: Unison) -> Vec<(f64, f64)> {
    let mut s = Stackup::new(sines(&[1]), unison);
    let (_, mix) = s.render(24_000);
    let (left, right) = (s.channel(&mix, 0), s.channel(&mix, 1));
    (0..unison.copies)
        .map(|c| {
            let f = hz(s.lanes.pitch()[c]);
            let rate = f64::from(RATE);
            (
                measure::tone(&left, rate, f).0,
                measure::tone(&right, rate, f).0,
            )
        })
        .collect()
}

fn spread_out(copies: usize) -> Unison {
    Unison {
        copies,
        detune: 1.0,
        range: 12.0,
        ..Unison::default()
    }
}

fn near(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() < 0.01,
        "{what}: measured {got:.4}, want {want:.4}"
    );
}

#[test]
fn width_measures_as_the_stereo_spread() {
    // Spec: engine-core R37 and the constant-power pan law: two copies at
    // −100, 0, 50 and 100 % width, each at level 1/√2.
    let level = std::f64::consts::FRAC_1_SQRT_2;
    let side = |pan: f64| {
        let theta = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
        (
            level * std::f64::consts::SQRT_2 * theta.cos(),
            level * std::f64::consts::SQRT_2 * theta.sin(),
        )
    };
    for width in [100.0, 50.0, 0.0, -100.0] {
        let gains = copy_gains(Unison {
            width,
            ..spread_out(2)
        });
        let pan = f64::from(width) / 100.0;
        for (c, (&(l, r), want_pan)) in gains.iter().zip([-pan, pan]).enumerate() {
            let (want_l, want_r) = side(want_pan);
            near(l, want_l, &format!("width {width}, copy {c}, left"));
            near(r, want_r, &format!("width {width}, copy {c}, right"));
        }
    }
}

#[test]
fn blend_levels_measure_as_its_law() {
    // Spec: engine-core R35 and R32: three copies, mono (width 0).
    for (blend, weights) in [
        (0.0, [0.0, 1.0, 0.0]),
        (37.5, [0.5, 1.0, 0.5]),
        (75.0, [1.0, 1.0, 1.0]),
        (100.0, [1.0, 0.0, 1.0]),
    ] {
        let gains = copy_gains(Unison {
            blend,
            width: 0.0,
            ..spread_out(3)
        });
        let norm = weights.iter().map(|w: &f64| w * w).sum::<f64>().sqrt();
        for (c, (&(l, r), w)) in gains.iter().zip(weights).enumerate() {
            near(l, w / norm, &format!("blend {blend}, copy {c}, left"));
            near(r, w / norm, &format!("blend {blend}, copy {c}, right"));
        }
    }
}

#[test]
fn loudness_holds_as_copies_are_added() {
    // Spec: engine-core R32: the mix's level within half a decibel of one
    // copy's, from 1 to 16 copies, detuned at the default Range.
    let level = |copies: usize| {
        let mut s = Stackup::new(
            sines(&[1]),
            Unison {
                copies,
                detune: 1.0,
                width: 0.0,
                ..Unison::default()
            },
        );
        let (_, mix) = s.render(48_000);
        db(rms(&s.channel(&mix, 0)))
    };
    let one = level(1);
    for copies in [2, 3, 5, 8, 12, 16] {
        let got = level(copies);
        assert!(
            (got - one).abs() < 0.5,
            "{copies} copies: {got:.2} dB against {one:.2}"
        );
    }
}

#[test]
fn frame_spread_plays_each_copy_at_its_own_frame() {
    // Spec: engine-core R38: five frames (harmonics 1 to 5), Frame 3, a
    // full spread: the copies play frames 1 to 5.
    let mut s = Stackup::new(
        sines(&[1, 2, 3, 4, 5]),
        Unison {
            copies: 5,
            detune: 0.0,
            frame_spread: 100.0,
            ..Unison::default()
        },
    );
    s.osc.params_mut().frame = 3.0;
    let (lanes, _) = s.render(24_000);
    for c in 0..5 {
        let f = measure::pitch(&s.lane(&lanes, c), f64::from(RATE)).expect("a pitch");
        let want = 220.0 * (c + 1) as f64;
        assert!(
            cents(f, want).abs() < 0.5,
            "copy {c}: {f:.2} Hz, want {want}"
        );
    }
    // A negative spread mirrors it.
    let mut s = Stackup::new(
        sines(&[1, 2, 3, 4, 5]),
        Unison {
            copies: 5,
            detune: 0.0,
            frame_spread: -50.0,
            ..Unison::default()
        },
    );
    s.osc.params_mut().frame = 3.0;
    let (lanes, _) = s.render(24_000);
    let f = measure::pitch(&s.lane(&lanes, 0), f64::from(RATE)).expect("a pitch");
    assert!(
        cents(f, 880.0).abs() < 0.5,
        "first copy at frame 4: {f:.2} Hz"
    );
}

#[test]
fn each_copy_starts_at_its_own_random_phase() {
    // Spec: engine-core R17: Phase random at its default, 100 %.
    let table = sines(&[1]);
    let mut osc = Oscillator::new(
        table,
        Layout {
            voices: 2,
            lanes_per_voice: 16,
        },
        RATE,
        BLOCK,
        2,
    );
    osc.start(1, KEY, Onset::Fresh);
    let mut phases: Vec<u32> = osc.lanes_of(1).map(|l| osc.phase(l)).collect();
    phases.sort_unstable();
    phases.dedup();
    assert_eq!(phases.len(), 16);
    let _ = simd::level();
}
