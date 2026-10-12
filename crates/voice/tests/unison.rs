//! Unison's laws, per copy: detune and its laws, stack, blend, level
//! compensation, width and the pan law, the spreads, the per-note draws,
//! the lanes' layout, and the mix to stereo against its scalar reference.
//! The same rules measured from an oscillator's renders are in
//! `lspk-wavetable`'s `tests/unison.rs`.

use lspk_dsp::simd::Level;
use lspk_plugin::{Event, Note};
use lspk_voice::unison::{Draws, MAX_COPIES, pan_gains};
use lspk_voice::{
    DetuneLaw, Layout, Stack, Unison, UnisonLanes, VoiceBlock, VoiceEvent, VoiceEventKind,
    VoiceHandler, Voices,
};

lspk_rt::install_trap!();

fn close(got: f32, want: f32, tolerance: f32, what: &str) {
    assert!(
        (got - want).abs() <= tolerance,
        "{what}: got {got}, want {want}"
    );
}

fn unison(copies: usize) -> Unison {
    Unison {
        copies,
        ..Unison::default()
    }
}

/// Every copy's pitch offset, with no draws.
fn pitches(u: &Unison) -> Vec<f32> {
    (0..u.count())
        .map(|i| u.copy(i, Draws::default()).pitch)
        .collect()
}

#[test]
fn defaults_are_a_new_patch_s() {
    // Spec: engine-core R32 to R38 (the section's table, as observed)
    let u = Unison::default();
    assert_eq!(u.copies, 1);
    assert_eq!(u.detune, 0.25);
    assert_eq!(u.blend, 75.0);
    assert_eq!(u.law, DetuneLaw::Linear);
    assert_eq!(u.stack, Stack::Off);
    assert_eq!(u.width, 100.0);
    assert_eq!(u.range, 2.0);
    assert_eq!(u.frame_spread, 0.0);
    assert_eq!(u.shaper_spread, [0.0; 2]);
    assert_eq!(u.span_spread, 0.0);
    assert_eq!(MAX_COPIES, 16);
    assert_eq!(DetuneLaw::ALL.len(), 5);
    assert_eq!(Stack::ALL.len(), 9);
}

#[test]
fn a_single_copy_plays_the_oscillator_unchanged() {
    // Spec: engine-core R32, R33, R37
    let u = Unison {
        detune: 1.0,
        width: 100.0,
        ..unison(1)
    };
    let c = u.copy(0, Draws::default());
    assert_eq!(c.pitch, 0.0);
    close(c.left, 1.0, 1e-6, "left");
    close(c.right, 1.0, 1e-6, "right");
}

#[test]
fn the_outer_copies_sit_range_times_detune_to_the_1_25_either_side() {
    // Spec: engine-core R33: at the default Range, ±200 × n^2.5 cents (n
    // the knob, Detune shown as n²), the same for 2, 3 and 7 copies.
    for copies in [2, 3, 7, 16] {
        for (knob, cents) in [(1.0f32, 200.0), (0.707, 84.1), (0.5, 35.4), (0.25, 6.25)] {
            let u = Unison {
                detune: knob * knob,
                ..unison(copies)
            };
            let p = pitches(&u);
            close(p[0] * 100.0, -cents, 0.15, "lowest copy, cents");
            close(p[copies - 1] * 100.0, cents, 0.15, "highest copy, cents");
        }
    }
    // Range scales it; 0 detune stacks the copies in unison.
    let u = Unison {
        detune: 1.0,
        range: 12.0,
        ..unison(5)
    };
    close(u.spread(), 12.0, 1e-5, "Range 12 at full detune");
    assert!(
        pitches(&Unison {
            detune: 0.0,
            ..unison(5)
        })
        .iter()
        .all(|&p| p == 0.0)
    );
}

#[test]
fn each_detune_law_places_the_copies_as_documented() {
    // Spec: engine-core R34. Seven copies, as fractions of the outer
    // copies' detune: Linear even (observed), Exponential near ±0.18 and
    // ±0.50, Inverse near ±0.56 and ±0.86 (measured on the earlier
    // version), Dense slightly uneven.
    let positions = |law: DetuneLaw| {
        let u = Unison {
            detune: 1.0,
            law,
            ..unison(7)
        };
        pitches(&u)
            .iter()
            .map(|p| p / u.spread())
            .collect::<Vec<_>>()
    };
    let linear = positions(DetuneLaw::Linear);
    for (i, p) in linear.iter().enumerate() {
        close(*p, i as f32 / 3.0 - 1.0, 1e-6, "linear");
    }
    let exp = positions(DetuneLaw::Exponential);
    close(exp[4], 0.18, 0.03, "exponential, first pair");
    close(exp[5], 0.50, 0.03, "exponential, second pair");
    let inv = positions(DetuneLaw::Inverse);
    close(inv[4], 0.56, 0.03, "inverse, first pair");
    close(inv[5], 0.86, 0.04, "inverse, second pair");
    for law in [DetuneLaw::Exponential, DetuneLaw::Inverse, DetuneLaw::Dense] {
        let p = positions(law);
        // Symmetric, outer copies at the full spread, centre at 0.
        for i in 0..7 {
            close(p[i], -p[6 - i], 1e-6, "symmetric");
        }
        close(p[6], 1.0, 1e-6, "outer copy");
        close(p[3], 0.0, 1e-6, "centre copy");
    }
    // Dense: every gap different, widening outward, within 10 % of even.
    let dense = positions(DetuneLaw::Dense);
    let gaps: Vec<f32> = (3..6).map(|i| dense[i + 1] - dense[i]).collect();
    assert!(gaps[0] < gaps[1] && gaps[1] < gaps[2], "{gaps:?}");
    for g in gaps {
        close(g, 1.0 / 3.0, 0.04, "dense gap");
    }
}

#[test]
fn the_random_law_draws_each_copy_s_detune_whenever_a_note_starts() {
    // Spec: engine-core R34
    let mut lanes = UnisonLanes::new(4, 8, 11);
    lanes.set(Unison {
        detune: 1.0,
        law: DetuneLaw::Random,
        ..unison(8)
    });
    let before: Vec<f32> = lanes.pitch()[8..16].to_vec();
    assert!(before.iter().all(|p| p.abs() <= 2.0));
    let mut sorted = before.clone();
    sorted.sort_by(f32::total_cmp);
    sorted.dedup();
    assert_eq!(sorted.len(), 8, "every copy its own detune: {before:?}");
    // Another voice drew its own; a control change keeps the draws.
    assert_ne!(&lanes.pitch()[0..8], &before[..]);
    lanes.set(Unison {
        detune: 0.25,
        law: DetuneLaw::Random,
        ..unison(8)
    });
    for (now, was) in lanes.pitch()[8..16].iter().zip(&before) {
        close(*now, was * 0.25f32.powf(1.25), 1e-5, "scaled, not redrawn");
    }
    // A new note draws again.
    lanes.start(1);
    assert_ne!(&lanes.pitch()[8..16], &before[..]);
}

#[test]
fn stacks_transpose_each_copy_by_its_rank_from_the_centre() {
    // Spec: engine-core R36, with the assignment documented in
    // lspk_voice::unison: rank 0 the centre copy or pair, intervals cycling
    // outward, pairs sharing one.
    let stacked = |stack: Stack, copies: usize| {
        pitches(&Unison {
            detune: 0.0,
            stack,
            ..unison(copies)
        })
    };
    assert_eq!(stacked(Stack::Octave1, 5), [0.0, 12.0, 0.0, 12.0, 0.0]);
    assert_eq!(
        stacked(Stack::Octave2, 7),
        [0.0, 24.0, 12.0, 0.0, 12.0, 24.0, 0.0]
    );
    assert_eq!(
        stacked(Stack::Octave3, 8),
        [36.0, 24.0, 12.0, 0.0, 0.0, 12.0, 24.0, 36.0]
    );
    assert_eq!(stacked(Stack::Fifth1, 5), [12.0, 7.0, 0.0, 7.0, 12.0]);
    assert_eq!(
        stacked(Stack::Fifth2, 9),
        [24.0, 19.0, 12.0, 7.0, 0.0, 7.0, 12.0, 19.0, 24.0]
    );
    let fifth3 = stacked(Stack::Fifth3, 13);
    assert_eq!(&fifth3[6..], &[0.0, 7.0, 12.0, 19.0, 24.0, 31.0, 36.0]);
    // The cycle repeats past its last interval.
    assert_eq!(
        &stacked(Stack::Octave1, 16)[8..],
        &[0.0, 12.0, 0.0, 12.0, 0.0, 12.0, 0.0, 12.0]
    );
    assert_eq!(stacked(Stack::CentreDown12, 5), [0.0, 0.0, -12.0, 0.0, 0.0]);
    assert_eq!(stacked(Stack::CentreDown24, 4), [0.0, -24.0, -24.0, 0.0]);
    assert_eq!(stacked(Stack::Off, 4), [0.0; 4]);
    // Detune adds to the stack.
    let u = Unison {
        detune: 1.0,
        stack: Stack::Octave1,
        ..unison(3)
    };
    assert_eq!(pitches(&u), [10.0, 0.0, 14.0]);
}

#[test]
fn blend_sets_the_outer_copies_against_the_centre() {
    // Spec: engine-core R35: 75 % equal; above two copies only; the centre
    // one (odd) or two (even).
    let weights = |copies: usize, blend: f32| {
        let u = Unison {
            blend,
            ..unison(copies)
        };
        (0..copies).map(|i| u.weight(i)).collect::<Vec<_>>()
    };
    assert_eq!(weights(5, 75.0), [1.0; 5]);
    assert_eq!(weights(5, 0.0), [0.0, 0.0, 1.0, 0.0, 0.0]);
    assert_eq!(weights(5, 100.0), [1.0, 1.0, 0.0, 1.0, 1.0]);
    assert_eq!(weights(4, 37.5), [0.5, 1.0, 1.0, 0.5]);
    assert_eq!(weights(4, 87.5), [1.0, 0.5, 0.5, 1.0]);
    assert_eq!(weights(2, 0.0), [1.0, 1.0]);
    assert_eq!(weights(1, 0.0), [1.0]);
}

#[test]
fn level_compensation_keeps_the_copies_total_power_that_of_one() {
    // Spec: engine-core R32 (the first law: weights over the root of their
    // squares summed).
    for copies in 1..=MAX_COPIES {
        for blend in [0.0, 30.0, 75.0, 90.0, 100.0] {
            let u = Unison {
                blend,
                width: 0.0,
                ..unison(copies)
            };
            let power: f32 = (0..copies)
                .map(|i| u.copy(i, Draws::default()).level.powi(2))
                .sum();
            close(power, 1.0, 1e-5, "total power");
        }
    }
    close(
        unison(16).copy(0, Draws::default()).level,
        0.25,
        1e-6,
        "16 copies at −12 dB",
    );
}

#[test]
fn width_spreads_the_copies_from_left_to_right_by_a_constant_power_law() {
    // Spec: engine-core R37 (bipolar, as observed), and the pan law
    // documented in lspk_voice::unison.
    let pans = |width: f32, copies: usize| {
        let u = Unison {
            width,
            ..unison(copies)
        };
        (0..copies)
            .map(|i| u.copy(i, Draws::default()).pan)
            .collect::<Vec<_>>()
    };
    assert_eq!(pans(100.0, 5), [-1.0, -0.5, 0.0, 0.5, 1.0]);
    assert_eq!(pans(-100.0, 3), [1.0, 0.0, -1.0]);
    assert_eq!(pans(50.0, 2), [-0.5, 0.5]);
    assert!(pans(0.0, 7).iter().all(|&p| p == 0.0));
    // Constant power, 0 dB at the centre in each channel.
    for pan in [-1.0, -0.3, 0.0, 0.6, 1.0] {
        let (l, r) = pan_gains(pan);
        close(l * l + r * r, 2.0, 1e-5, "constant power");
    }
    let (l, r) = pan_gains(0.0);
    close(l, 1.0, 1e-6, "centre left");
    close(r, 1.0, 1e-6, "centre right");
    let (l, r) = pan_gains(-1.0);
    close(l, std::f32::consts::SQRT_2, 1e-6, "hard left");
    close(r, 0.0, 1e-6, "hard left, right channel");
    // Gains carry the level too.
    let u = unison(4);
    let c = u.copy(0, Draws::default());
    close(
        c.left,
        c.level * std::f32::consts::SQRT_2,
        1e-6,
        "left gain",
    );
}

#[test]
fn spreads_offset_frame_shapers_and_span_around_the_knob() {
    // Spec: engine-core R31, R38, and UNI-006's start and span for
    // sample-based oscillators.
    let u = Unison {
        frame_spread: 50.0,
        shaper_spread: [100.0, -40.0],
        span_spread: 20.0,
        start_spread: 60.0,
        ..unison(5)
    };
    let draws = Draws {
        position: 0.0,
        start: 0.5,
    };
    let c: Vec<_> = (0..5).map(|i| u.copy(i, draws)).collect();
    let frames: Vec<f32> = c.iter().map(|c| c.frame).collect();
    assert_eq!(frames, [-0.5, -0.25, 0.0, 0.25, 0.5]);
    assert_eq!(c[0].shaper, [-50.0, 20.0]);
    assert_eq!(c[4].shaper, [50.0, -20.0]);
    close(c[3].span, 0.1, 1e-6, "span");
    assert!(c.iter().all(|c| (c.start - 0.3).abs() < 1e-6));
    // No spread, no offsets.
    let plain = unison(5).copy(4, draws);
    assert_eq!(
        (plain.frame, plain.shaper, plain.span, plain.start),
        (0.0, [0.0; 2], 0.0, 0.0)
    );
}

#[test]
fn lanes_follow_the_voice_block_s_layout() {
    // Spec: engine-core R32
    let mut lanes = UnisonLanes::new(5, 7, 1);
    assert_eq!(lanes.lane_count(), 48);
    assert_eq!(lanes.voice_stride(), 16);
    assert_eq!(lanes.lanes_of(2), 14..21);
    lanes.set(Unison {
        detune: 1.0,
        frame_spread: 100.0,
        shaper_spread: [100.0, 100.0],
        ..unison(5)
    });
    // Five copies in lanes of seven: the last two silent and still.
    for voice in 0..5 {
        let r = lanes.lanes_of(voice);
        assert_eq!(lanes.pitch()[r.start], -2.0);
        assert_eq!(lanes.pitch()[r.start + 4], 2.0);
        for l in r.start + 5..r.end {
            let values = [
                lanes.pitch()[l],
                lanes.frame()[l],
                lanes.shaper(0)[l],
                lanes.shaper(1)[l],
                lanes.left()[l],
                lanes.right()[l],
            ];
            assert_eq!(values, [0.0; 6]);
        }
    }
    // More copies than lanes: as many as fit.
    lanes.set(unison(16));
    assert_eq!(lanes.unison().copies, 7);
}

/// A frame-by-frame lane buffer of noise.
fn noise(len: usize) -> Vec<f32> {
    let mut x = 0x1234_5678u32;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        })
        .collect()
}

#[test]
fn every_simd_level_mixes_as_the_scalar_path() {
    // Spec: engine-core R35, R37
    for (voices, copies) in [
        (32, 1),
        (7, 4),
        (8, 7),
        (32, 7),
        (32, 16),
        (5, 13),
        (3, 8),
        (1, 3),
    ] {
        let mut lanes = UnisonLanes::new(voices, copies, 5);
        lanes.set(Unison {
            blend: 60.0,
            width: 80.0,
            ..unison(copies)
        });
        let frames = 37;
        let input = noise(frames * lanes.lane_count());
        let mut want = vec![0.25; frames * 2 * lanes.voice_stride()];
        lanes.mix_scalar(frames, voices, &input, &mut want);
        for level in Level::ALL {
            let mut got = vec![0.25; want.len()];
            lanes.mix_at(level, frames, voices, &input, &mut got);
            for (g, w) in got.iter().zip(&want) {
                close(*g, *w, 1e-5, &format!("{voices} × {copies} at {level}"));
            }
        }
    }
}

#[test]
fn the_mix_puts_each_copy_at_its_gains() {
    // Spec: engine-core R37: two copies at full width, hard left and right.
    let mut lanes = UnisonLanes::new(2, 2, 5);
    lanes.set(unison(2));
    let stride = lanes.voice_stride();
    let mut input = vec![0.0; lanes.lane_count()];
    input[2] = 1.0; // voice 1, first copy
    let mut out = vec![0.0; 2 * stride];
    lanes.mix(1, 2, &input, &mut out);
    let level = std::f32::consts::FRAC_1_SQRT_2;
    close(
        out[1],
        level * std::f32::consts::SQRT_2,
        1e-6,
        "voice 1 left",
    );
    close(out[stride + 1], 0.0, 1e-6, "voice 1 right");
    assert_eq!((out[0], out[stride]), (0.0, 0.0));
}

#[test]
fn changing_starting_and_mixing_stay_off_the_heap() {
    let mut lanes = UnisonLanes::new(32, 16, 3);
    let frames = 64;
    let input = noise(frames * lanes.lane_count());
    let mut out = vec![0.0; frames * 2 * lanes.voice_stride()];
    let _ = lspk_dsp::simd::level();
    lspk_rt::rt_test!({
        lanes.set(Unison {
            law: DetuneLaw::Random,
            ..unison(16)
        });
        lanes.start(9);
        lanes.mix(frames, 32, &input, &mut out);
        lanes.mix_scalar(frames, 32, &input, &mut out);
    });
    assert!(out.iter().any(|&x| x != 0.0));
}

/// Ends voices as soon as they're released.
struct Silent;

impl VoiceHandler for Silent {
    fn voice_event(&mut self, voices: &mut VoiceBlock, event: VoiceEvent) {
        if event.kind == VoiceEventKind::Release {
            voices.end(event.voice);
        }
    }

    fn render(&mut self, _: &mut VoiceBlock, _: std::ops::Range<usize>) {}
}

#[test]
fn sixteen_copies_don_t_count_against_the_voice_limit() {
    // Spec: engine-core R39: at the default limit of 8, eight notes sound
    // with 16 copies each.
    let mut voices = Voices::new(
        Layout {
            voices: 32,
            lanes_per_voice: 16,
        },
        48_000.0,
    );
    assert_eq!(voices.settings_mut().voices, 8);
    let events: Vec<Event> = (0..8)
        .map(|i| Event::NoteOn {
            time: i,
            note: Note::key(0, 60 + i as u8),
            velocity: 0.8,
        })
        .collect();
    voices.process(events, 64, &mut Silent);
    assert_eq!(voices.block().active_count(), 8);
    assert_eq!(voices.block().span(16), 128);
}
