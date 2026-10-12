//! The LFO rules, each test citing the specification's rule.

use super::*;
use crate::simd::Level;

const SR: f64 = 48_000.0;

fn hz(hz: f32) -> Rate {
    Rate::Hz { hz, x10: false }
}

fn settings(kind: LfoType, mode: Mode, rate: Rate) -> Settings {
    Settings {
        kind,
        mode,
        rate,
        ..Settings::default()
    }
}

/// Plays one voice of `lfo` for `frames` samples in blocks of `block`,
/// with note-ons before the samples in `triggers`; returns X and Y.
fn play(
    lfo: &mut Lfo,
    transport: &mut Transport,
    frames: usize,
    block: usize,
    triggers: &[usize],
) -> (Vec<f32>, Vec<f32>) {
    let mut voice = lfo.voice(0);
    play_voice(lfo, &mut voice, transport, frames, block, triggers)
}

fn play_voice(
    lfo: &mut Lfo,
    voice: &mut LfoVoice,
    transport: &mut Transport,
    frames: usize,
    block: usize,
    triggers: &[usize],
) -> (Vec<f32>, Vec<f32>) {
    let mut x = vec![0.0; frames];
    let mut y = vec![0.0; frames];
    let mut pos = 0;
    while pos < frames {
        let n = block.min(frames - pos);
        let b = lfo.prepare(transport, n);
        let mut from = 0;
        for &t in triggers.iter().filter(|t| (pos..pos + n).contains(t)) {
            let at = t - pos;
            let (xs, ys) = (&mut x[pos + from..pos + at], &mut y[pos + from..pos + at]);
            lfo.render(&b, voice, from, xs, Some(ys));
            lfo.trigger(&b, voice, at);
            from = at;
        }
        let (xs, ys) = (&mut x[pos + from..pos + n], &mut y[pos + from..pos + n]);
        lfo.render(&b, voice, from, xs, Some(ys));
        transport.advance(n);
        pos += n;
    }
    (x, y)
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

/// A rising ramp, 0 to 1 over the cycle: its value is its position.
fn ramp() -> Shape {
    Shape::new(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)])
}

#[test]
fn the_init_slot_is_the_observed_init_patch() {
    // Spec: engine-core section 10 table (defaults), observed init: Normal
    // type, Retrig, a triangle, Forward, 8 by 8 grid, synced at 1/4, Mono off.
    let s = Settings::default();
    assert_eq!(s.kind, LfoType::Drawn);
    assert_eq!(s.mode, Mode::Restart);
    assert_eq!(s.direction, Direction::Forward);
    assert!(!s.shared);
    assert_eq!((s.grid_x, s.grid_y), (8, 8));
    assert_eq!(
        s.rate,
        Rate::Synced {
            division: Division::QUARTER,
            feel: Feel::Straight
        }
    );
    assert!(s.song_lock);
    assert_eq!(
        (s.fade_in, s.delay, s.smooth, s.phase),
        (Span::Off, Span::Off, 0.0, 0.0)
    );
    let shape = Shape::default();
    let xs: Vec<_> = shape.points().iter().map(|p| (p.x, p.y)).collect();
    assert_eq!(xs, [(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]);
}

#[test]
fn there_are_ten_independent_slots() {
    // Spec: engine-core R81
    assert_eq!(LFO_COUNT, 10);
    let firsts: Vec<f32> = (0..LFO_COUNT as u64)
        .map(|seed| {
            let mut lfo = Lfo::new(
                settings(LfoType::SampleAndHold, Mode::Restart, hz(10.0)),
                seed,
            );
            play(&mut lfo, &mut Transport::new(SR), 1, 1, &[0]).0[0]
        })
        .collect();
    for (i, a) in firsts.iter().enumerate() {
        assert!(firsts[i + 1..].iter().all(|b| a != b), "{firsts:?}");
    }
}

#[test]
fn a_shared_slot_plays_one_lfo_for_every_voice() {
    // Spec: engine-core R82
    let mut s = settings(LfoType::Drawn, Mode::Restart, hz(5.0));
    let frames = 256;
    let mut out = vec![0.0; frames * 4];
    for shared in [false, true] {
        s.shared = shared;
        let mut bank = LfoBank::new(Lfo::new(s, 1), 4);
        for v in 0..4 {
            bank.trigger(v, v * 50);
        }
        bank.process(&Transport::new(SR), frames, &mut out, None);
        let alike = out.chunks(4).all(|f| f.iter().all(|x| *x == f[0]));
        assert_eq!(alike, shared);
    }
}

#[test]
fn free_ignores_notes_restart_starts_over_and_one_shot_plays_once() {
    // Spec: engine-core R83
    let frames = 48_000;
    let t = &mut Transport::new(SR);
    let mut free = Lfo::new(settings(LfoType::Drawn, Mode::Free, hz(3.0)), 0);
    let untouched = play(&mut free, t, frames, 128, &[]).0;
    let mut free = Lfo::new(settings(LfoType::Drawn, Mode::Free, hz(3.0)), 0);
    let noted = play(
        &mut free,
        &mut Transport::new(SR),
        frames,
        128,
        &[1000, 20_000],
    )
    .0;
    assert_eq!(untouched, noted);

    let mut restart = Lfo::new(settings(LfoType::Drawn, Mode::Restart, hz(3.0)), 0);
    let out = play(
        &mut restart,
        &mut Transport::new(SR),
        frames,
        128,
        &[0, 20_000],
    )
    .0;
    assert_eq!(out[..1000], out[20_000..21_000]);

    let mut once = Lfo::new(settings(LfoType::Drawn, Mode::OneShot, hz(4.0)), 0);
    once.shape_mut()
        .set_points(&[Point::new(0.0, 0.2), Point::new(1.0, 0.9)]);
    let out = play(&mut once, &mut Transport::new(SR), frames, 128, &[0]).0;
    // One cycle is 12 000 samples; then it holds the last point's value.
    assert!(out[11_999] < 0.9 && out[11_999] > 0.89);
    assert!(out[12_000..].iter().all(|x| *x == 0.9));

    // With a loopback point it loops from there to the end.
    once.shape_mut().set_points(&[
        Point::new(0.0, 0.0),
        Point::new(0.5, 0.5),
        Point::new(1.0, 1.0),
    ]);
    once.shape_mut().set_loopback(Some(1));
    let out = play(&mut once, &mut Transport::new(SR), frames, 128, &[0]).0;
    let after = &out[12_000..];
    assert!(after.iter().all(|x| (0.5..1.0).contains(x)));
    // Half a cycle of loop: 6000 samples, back to 0.5.
    assert!(close(after[0], 0.5, 1e-6) && close(after[6000], 0.5, 1e-6));
    assert!(close(after[3000], 0.75, 1e-4));
}

#[test]
fn song_lock_follows_the_song_position() {
    // Spec: engine-core R84
    let mut s = settings(LfoType::Drawn, Mode::Free, Rate::default());
    s.song_lock = true;
    let mut lfo = Lfo::new(s, 0);
    let mut t = Transport::new(SR);
    t.playing = true;
    t.beats = 10.25;
    // A quarter-note cycle at beat 10.25 is a quarter through: the triangle is at 0.5.
    let out = play(&mut lfo, &mut t.clone(), 1, 1, &[]).0;
    assert!(close(out[0], 0.5, 1e-6));
    // Changing the rate jumps to stay aligned: at 1/2, beat 10.25 is an eighth through.
    lfo.settings.rate = Rate::Synced {
        division: Division::new(6).unwrap(),
        feel: Feel::Straight,
    };
    let out = play(&mut lfo, &mut t.clone(), 1, 1, &[]).0;
    assert!(close(out[0], 0.25, 1e-6));

    // In hertz, a locked free LFO computes its phase from the transport.
    lfo.settings.rate = hz(1.0);
    t.seconds = 0.25;
    let out = play(&mut lfo, &mut t.clone(), 1, 1, &[]).0;
    assert!(close(out[0], 0.5, 1e-6));

    // Without song lock it runs on its own clock, from the start.
    lfo.settings.song_lock = false;
    let mut lfo = Lfo::new(lfo.settings, 0);
    let out = play(&mut lfo, &mut t.clone(), 1, 1, &[]).0;
    assert_eq!(out[0], 0.0);

    // Two locked renders starting at different places agree where they meet.
    let mut s = settings(LfoType::Drawn, Mode::Free, Rate::default());
    s.song_lock = true;
    let mut a = Lfo::new(s, 0);
    let mut b = Lfo::new(s, 0);
    let mut ta = Transport::new(SR);
    ta.playing = true;
    let mut tb = ta;
    tb.advance(4800);
    let xa = play(&mut a, &mut ta, 9600, 256, &[]).0;
    let xb = play(&mut b, &mut tb, 4800, 100, &[]).0;
    for (x, y) in xa[4800..].iter().zip(&xb) {
        assert!(close(*x, *y, 1e-5));
    }
}

#[test]
fn synced_rates_follow_the_division_table() {
    // Spec: engine-core R85
    let named = [
        (3, 16.0),
        (5, 4.0),
        (6, 2.0),
        (7, 1.0),
        (9, 0.25),
        (10, 0.125),
    ];
    for (k, beats) in named {
        let d = Division::new(k).unwrap();
        assert_eq!(d.beats(4.0), beats, "division {k}");
        assert_eq!(Division::from_normalised(f32::from(k) / 14.0), d);
        assert!(close(
            d.stored(),
            100.0 * (f32::from(k) / 14.0).powi(4),
            1e-4
        ));
    }
    assert_eq!(Division::BARS_32.beats(4.0), 128.0);
    assert_eq!(Division::BAR.beats(3.0), 3.0);
    assert_eq!(Division::N256.beats(4.0), 4.0 / 256.0);
    assert!(Division::FAST.beats(4.0) < Division::N256.beats(4.0));
    assert_eq!(Division::new(15), None);

    // 1/4 at 120 beats per minute: two cycles a second; triplet and dotted scale the division.
    for (feel, cycles) in [
        (Feel::Straight, 2.0),
        (Feel::Triplet, 3.0),
        (Feel::Dotted, 4.0 / 3.0),
    ] {
        let rate = Rate::Synced {
            division: Division::QUARTER,
            feel,
        };
        let mut lfo = Lfo::new(settings(LfoType::Drawn, Mode::Restart, rate), 0);
        let inc = lfo.prepare(&Transport::new(SR), 64).cycles_per_sample();
        assert!((inc * SR - cycles).abs() < 1e-9, "{feel:?}");
    }
}

#[test]
fn hertz_rates_follow_the_fourth_power_law() {
    // Spec: engine-core R86
    assert_eq!(
        Rate::hz_from_normalised(0.5, false),
        Rate::Hz {
            hz: 6.25,
            x10: false
        }
    );
    assert_eq!(
        Rate::hz_from_normalised(1.0, true),
        Rate::Hz {
            hz: 1000.0,
            x10: true
        }
    );
    let inc = |rate| {
        Lfo::new(settings(LfoType::Drawn, Mode::Restart, rate), 0)
            .prepare(&Transport::new(SR), 64)
            .cycles_per_sample()
            * SR
    };
    assert!(
        (inc(Rate::Hz {
            hz: 1000.0,
            x10: true
        }) - 1000.0)
            .abs()
            < 1e-9
    );
    // Without ×10 the rate stops at 100 Hz.
    assert!(
        (inc(Rate::Hz {
            hz: 1000.0,
            x10: false
        }) - 100.0)
            .abs()
            < 1e-9
    );

    // At 1000 Hz the ramp completes a cycle every 48 samples, at audio rate.
    let s = settings(
        LfoType::Drawn,
        Mode::Restart,
        Rate::Hz {
            hz: 1000.0,
            x10: true,
        },
    );
    let mut lfo = Lfo::new(s, 0);
    *lfo.shape_mut() = ramp();
    let out = play(&mut lfo, &mut Transport::new(SR), 480, 64, &[0]).0;
    for (k, value) in out.iter().enumerate() {
        let expected = (k % 48) as f32 / 48.0;
        assert!(close(*value, expected, 1e-4), "sample {k}: {value}");
    }
}

#[test]
fn swing_lengthens_the_first_cycle_of_each_pair() {
    // Spec: engine-core R87
    let mut s = settings(LfoType::Drawn, Mode::Restart, Rate::default());
    s.swing = true;
    let mut lfo = Lfo::new(s, 0);
    *lfo.shape_mut() = ramp();
    let mut t = Transport::new(SR);
    t.swing = 1.0;
    // A quarter-note cycle is 24 000 samples; at full swing the first of a
    // pair takes 75 % of the pair's 48 000.
    let out = play(&mut lfo, &mut t, 48_000, 512, &[0]).0;
    assert!(close(out[18_000], 0.5, 1e-4));
    assert!(out[35_999] > 0.99 && out[36_000] < 0.01);
    assert!(close(out[42_000], 0.5, 1e-4));
    // Swing does nothing in hertz.
    lfo.settings.rate = hz(2.0);
    let out = play(&mut lfo, &mut t, 48_000, 512, &[0]).0;
    assert!(close(out[12_000], 0.5, 1e-4));
}

#[test]
fn delay_holds_the_start_value_then_fade_in_blends_to_the_shape() {
    // Spec: engine-core R88
    let mut s = settings(LfoType::Drawn, Mode::Restart, hz(1.0));
    s.delay = Span::Seconds(0.1);
    s.fade_in = Span::Seconds(0.1);
    let mut lfo = Lfo::new(s, 0);
    lfo.shape_mut()
        .set_points(&[Point::new(0.0, 0.3), Point::new(1.0, 1.0)]);
    let out = play(&mut lfo, &mut Transport::new(SR), 24_000, 128, &[0]).0;
    // The first point's value through the delay.
    assert!(out[..4800].iter().all(|x| *x == 0.3));
    // Half-way through the fade, half the way to the shape.
    let live = 0.3 + 0.7 * (7200.0 / 48_000.0);
    assert!(close(out[7200], 0.3 + 0.5 * (live - 0.3), 1e-4));
    // Then the shape itself.
    assert!(close(out[12_000], 0.3 + 0.7 * 0.25, 1e-4));

    // In tempo mode, the knob's divisions run Off, then 1/256 to 32 bars.
    assert_eq!(Span::from_index(0), Span::Off);
    assert_eq!(Span::from_index(1), Span::Synced(Division::N256));
    assert_eq!(Span::from_index(14), Span::Synced(Division::BARS_32));
    lfo.settings.delay = Span::from_index(7); // 1/4: half a second at 120
    lfo.settings.fade_in = Span::Off;
    let out = play(&mut lfo, &mut Transport::new(SR), 24_010, 128, &[0]).0;
    assert!(out[..24_000].iter().all(|x| *x == 0.3) && out[24_000] > 0.3);
}

#[test]
fn smooth_softens_jumps() {
    // Spec: engine-core R89
    let square = Shape::new(&[
        Point::new(0.0, 0.0).step(),
        Point::new(0.5, 1.0).step(),
        Point::new(1.0, 1.0),
    ]);
    let biggest_jump = |smooth: f32| {
        let mut s = settings(LfoType::Drawn, Mode::Restart, hz(10.0));
        s.smooth = smooth;
        let mut lfo = Lfo::new(s, 0);
        *lfo.shape_mut() = square.clone();
        let out = play(&mut lfo, &mut Transport::new(SR), 9600, 128, &[0]).0;
        assert!(out.iter().all(|x| (0.0..=1.0).contains(x)));
        out.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    };
    assert_eq!(biggest_jump(0.0), 1.0);
    let some = biggest_jump(30.0);
    let more = biggest_jump(100.0);
    assert!(some < 0.1 && more < some, "{some} {more}");
}

#[test]
fn phase_sets_the_start_and_snaps_to_the_grid() {
    // Spec: engine-core R90
    let mut s = settings(LfoType::Drawn, Mode::Restart, hz(1.0));
    s.phase = 90.0;
    let mut lfo = Lfo::new(s, 0);
    *lfo.shape_mut() = ramp();
    assert!(close(
        play(&mut lfo, &mut Transport::new(SR), 1, 1, &[0]).0[0],
        0.25,
        1e-7
    ));
    lfo.settings.phase = 100.0;
    lfo.settings.phase_snap = true;
    assert!(close(
        play(&mut lfo, &mut Transport::new(SR), 1, 1, &[0]).0[0],
        0.25,
        1e-7
    ));
    lfo.settings.grid_x = 16;
    lfo.settings.phase = 110.0;
    assert!(close(
        play(&mut lfo, &mut Transport::new(SR), 1, 1, &[0]).0[0],
        0.3125,
        1e-7
    ));
}

#[test]
fn reverse_and_ping_pong_play_backwards_with_the_loopback() {
    // Spec: engine-core R91
    let mut s = settings(LfoType::Drawn, Mode::Restart, hz(4.0));
    s.direction = Direction::Reverse;
    let mut lfo = Lfo::new(s, 0);
    *lfo.shape_mut() = ramp();
    let out = play(&mut lfo, &mut Transport::new(SR), 24_000, 100, &[0]).0;
    assert!(close(out[3000], 0.75, 1e-4) && close(out[15_000], 0.75, 1e-4));

    lfo.settings.direction = Direction::PingPong;
    let out = play(&mut lfo, &mut Transport::new(SR), 24_000, 100, &[0]).0;
    assert!(close(out[3000], 0.25, 1e-4) && close(out[15_000], 0.75, 1e-4));
    assert!(close(out[27_000 - 24_000], 0.25, 1e-4));

    // One-shot with the loopback at x = 0.5: reverse plays 1 to 0, then loops 0.5 down to 0.
    lfo.settings.mode = Mode::OneShot;
    lfo.settings.direction = Direction::Reverse;
    lfo.shape_mut().set_points(&[
        Point::new(0.0, 0.0),
        Point::new(0.5, 0.5),
        Point::new(1.0, 1.0),
    ]);
    lfo.shape_mut().set_loopback(Some(1));
    let out = play(&mut lfo, &mut Transport::new(SR), 36_000, 100, &[0]).0;
    assert!(out[12_000..].iter().all(|x| *x <= 0.5 + 1e-6));
    // The loop runs at the shape's speed: half a cycle from 0.5 down to 0.
    assert!(close(out[12_000], 0.5, 1e-6) && close(out[13_500], 0.375, 1e-4));
    assert!(close(out[18_000], 0.5, 1e-4));
    // Ping-pong bounces between the end and the loopback point.
    lfo.settings.direction = Direction::PingPong;
    let out = play(&mut lfo, &mut Transport::new(SR), 36_000, 100, &[0]).0;
    assert!(out[12_000..].iter().all(|x| *x >= 0.5 - 1e-6));
    assert!(close(out[12_000 + 6000], 0.5, 1e-4) && close(out[12_000 + 12_000], 1.0, 1e-4));
}

#[test]
fn the_last_point_holds_without_wrapping() {
    // Spec: engine-core R92
    let mut lfo = Lfo::new(settings(LfoType::Drawn, Mode::Restart, hz(1.0)), 0);
    lfo.shape_mut()
        .set_points(&[Point::new(0.2, 0.1), Point::new(0.6, 0.8)]);
    let out = play(&mut lfo, &mut Transport::new(SR), 48_000, 512, &[0]).0;
    assert!(out[..9600].iter().all(|x| *x == 0.1));
    assert!(out[28_800..].iter().all(|x| *x == 0.8));
}

#[test]
fn every_type_stays_between_zero_and_one() {
    // Spec: engine-core R93
    for kind in [
        LfoType::Drawn,
        LfoType::Path,
        LfoType::Lorenz,
        LfoType::Rossler,
        LfoType::SampleAndHold,
    ] {
        for rate in [
            hz(0.5),
            hz(30.0),
            Rate::Hz {
                hz: 1000.0,
                x10: true,
            },
        ] {
            let mut lfo = Lfo::new(settings(kind, Mode::Restart, rate), 3);
            let mut t = Transport::new(SR);
            t.scale = Scale::Rate(1000.0);
            let (x, y) = play(&mut lfo, &mut t, 48_000, 256, &[0]);
            assert!(
                x.iter().chain(&y).all(|v| (0.0..=1.0).contains(v)),
                "{kind:?} {rate:?}"
            );
        }
    }
}

#[test]
fn point_buses_move_points() {
    // Spec: engine-core R94
    let mut lfo = Lfo::new(settings(LfoType::Drawn, Mode::Restart, hz(1.0)), 0);
    let mut flat = [
        Point::new(0.0, 0.2),
        Point::new(0.5, 0.2),
        Point::new(1.0, 0.2),
    ];
    // Bus 3 lifts both end points; bus 5 moves the middle point's x.
    flat[0].y_mod = Some(PointMod { bus: 3, depth: 0.5 });
    flat[2].y_mod = Some(PointMod { bus: 3, depth: 0.5 });
    flat[1].x_mod = Some(PointMod { bus: 5, depth: 1.0 });
    lfo.shape_mut().set_points(&flat);
    let mut voice = lfo.voice(0);
    let t = &mut Transport::new(SR);
    let before = play_voice(&mut lfo, &mut voice, t, 1, 1, &[0]).0[0];
    assert_eq!(before, 0.2);
    voice.set_bus(3, 1.0);
    let lifted = play_voice(
        &mut lfo,
        &mut voice,
        &mut Transport::new(SR),
        48_000,
        512,
        &[0],
    )
    .0;
    assert!(close(lifted[0], 0.7, 1e-6) && close(lifted[47_999], 0.7, 1e-3));
    assert!(close(lifted[24_000], 0.2, 1e-6));
    // Moving x past a neighbour keeps the points in order.
    voice.set_bus(5, 2.0);
    let moved = play_voice(
        &mut lfo,
        &mut voice,
        &mut Transport::new(SR),
        48_000,
        512,
        &[0],
    )
    .0;
    assert!(moved.iter().all(|x| x.is_finite()));
    assert!(close(moved[24_000], 0.45, 1e-3));
    assert_eq!(POINT_BUSES, 16);
}

#[test]
fn generators_follow_the_rate() {
    // Spec: engine-core R95
    // Sample and hold takes one new value a cycle.
    let mut lfo = Lfo::new(settings(LfoType::SampleAndHold, Mode::Restart, hz(10.0)), 9);
    let out = play(&mut lfo, &mut Transport::new(SR), 48_000, 512, &[0]).0;
    let changes = out.windows(2).filter(|w| w[0] != w[1]).count();
    assert_eq!(changes, 9);
    assert!(out[..4800].iter().all(|x| *x == out[0]));

    // Chaos turns faster at a faster rate, and stays bounded at any rate.
    for kind in [LfoType::Lorenz, LfoType::Rossler] {
        let crossings = |rate: f32| {
            let mut lfo = Lfo::new(settings(kind, Mode::Restart, hz(rate)), 0);
            let out = play(&mut lfo, &mut Transport::new(SR), 96_000, 512, &[0]).0;
            let mean = out.iter().sum::<f32>() / out.len() as f32;
            out.windows(2)
                .filter(|w| (w[0] < mean) != (w[1] < mean))
                .count()
        };
        let (slow, fast) = (crossings(1.0), crossings(8.0));
        assert!(fast > 3 * slow && slow > 0, "{kind:?}: {slow} {fast}");
        let mut lfo = Lfo::new(
            settings(
                kind,
                Mode::Free,
                Rate::Hz {
                    hz: 1000.0,
                    x10: true,
                },
            ),
            0,
        );
        let mut t = Transport::new(22_050.0);
        t.scale = Scale::Rate(1000.0);
        let out = play(&mut lfo, &mut t, 22_050, 512, &[]).0;
        assert!(out.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)));
        assert!(out.windows(2).any(|w| w[0] != w[1]));
    }

    // Moving a point bus doesn't restart a chaos generator.
    let mut lfo = Lfo::new(settings(LfoType::Lorenz, Mode::Restart, hz(4.0)), 0);
    let mut voice = lfo.voice(0);
    let t = &mut Transport::new(SR);
    let a = play_voice(&mut lfo, &mut voice, t, 2000, 500, &[0]).0;
    let mut other = voice.clone();
    other.set_bus(2, 0.5);
    let b = play_voice(&mut lfo, &mut voice, &mut t.clone(), 500, 500, &[]).0;
    let c = play_voice(&mut lfo, &mut other, t, 500, 500, &[]).0;
    assert_eq!(b, c);
    assert_ne!(a[0], b[0]);
}

#[test]
fn the_xy_path_has_two_outputs() {
    // Spec: engine-core R96
    let mut lfo = Lfo::new(settings(LfoType::Path, Mode::Restart, hz(1.0)), 0);
    let (x, y) = play(&mut lfo, &mut Transport::new(SR), 48_000, 512, &[0]);
    // The init diamond: X and Y are triangles a quarter cycle apart.
    for (k, (ex, ey)) in [
        (0, (0.5, 0.0)),
        (12_000, (1.0, 0.5)),
        (24_000, (0.5, 1.0)),
        (36_000, (0.0, 0.5)),
    ] {
        assert!(
            close(x[k], ex, 1e-5) && close(y[k], ey, 1e-5),
            "{k}: {} {}",
            x[k],
            y[k]
        );
    }
    // An open path's segments take time by length: the long side takes twice the short.
    let open = Path::new(
        &[
            PathPoint::new(0.0, 0.0),
            PathPoint::new(0.5, 0.0),
            PathPoint::new(0.5, 1.0),
        ],
        false,
    );
    assert_eq!(open.value(1.0 / 3.0), (0.5, 0.0));
    assert_eq!(open.value(1.0), (0.5, 1.0));
    // A bent segment bows out at its middle.
    let mut bent = [PathPoint::new(0.0, 0.5), PathPoint::new(1.0, 0.5)];
    bent[0].bend = 1.0;
    let (bx, by) = Path::new(&bent, false).value(0.5);
    assert!(close(bx, 0.5, 1e-6) && close(by, 1.0, 1e-6));
}

#[test]
fn the_global_scale_stretches_or_speeds_every_lfo() {
    // Spec: engine-core R97
    let inc = |scale| {
        let mut t = Transport::new(SR);
        t.scale = scale;
        Lfo::new(settings(LfoType::Drawn, Mode::Restart, hz(10.0)), 0)
            .prepare(&t, 64)
            .cycles_per_sample()
            * SR
    };
    assert!((inc(Scale::default()) - 10.0).abs() < 1e-9);
    assert!((inc(Scale::Percent(200.0)) - 5.0).abs() < 1e-9);
    assert!((inc(Scale::Rate(200.0)) - 20.0).abs() < 1e-9);
    assert!((inc(Scale::Rate(5000.0)) - 100.0).abs() < 1e-9);
    assert!((inc(Scale::Percent(1.0)) - 100.0).abs() < 1e-9);
}

#[test]
fn stored_y_counts_from_the_top() {
    // Spec: engine-core R98
    assert_eq!(Shape::y_from_top(0.0), 1.0);
    assert_eq!(Shape::y_from_top(0.25), 0.75);
    assert_eq!(Shape::y_from_top(Shape::y_from_top(0.3)), 0.3);
}

#[test]
fn curves_bend_and_steps_hold() {
    // Spec: engine-core section 10 table (Shape: points with curves between them)
    let shape = Shape::new(&[
        Point::new(0.0, 0.0).curve(0.5),
        Point::new(0.5, 1.0).step(),
        Point::new(0.75, 0.0).curve(-0.5),
        Point::new(1.0, 1.0),
    ]);
    assert!(shape.value(0.25) < 0.5);
    assert!(close(shape.value(0.5), 1.0, 1e-7));
    assert_eq!(shape.value(0.6), 1.0);
    assert_eq!(shape.value(0.74), 1.0);
    assert!(shape.value(0.875) > 0.5);
    let straight = Shape::new(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)]);
    assert!(close(straight.value(0.3), 0.3, 1e-7));
}

#[test]
fn shapes_copy_to_and_from_wavetable_frames() {
    // Spec: inventory LFO-009 (wavetable frame to LFO and back)
    let mut frame = vec![0.0; 64];
    Shape::default().render(&mut frame);
    assert_eq!(frame[0], -1.0);
    assert_eq!(frame[32], 1.0);
    let back = Shape::from_samples(&frame);
    assert_eq!(back.points().len(), 64);
    let mut again = vec![0.0; 64];
    back.render(&mut again);
    for (a, b) in frame.iter().zip(&again) {
        assert!(close(*a, *b, 0.05));
    }
}

/// Every type and mode, with fades, smoothing and swing: the settings the
/// invariance and SIMD tests run through.
fn variety() -> Vec<Settings> {
    let mut all = Vec::new();
    for kind in [
        LfoType::Drawn,
        LfoType::Path,
        LfoType::Lorenz,
        LfoType::Rossler,
        LfoType::SampleAndHold,
    ] {
        for mode in [Mode::Free, Mode::Restart, Mode::OneShot] {
            for (i, rate) in [
                hz(7.3),
                Rate::default(),
                Rate::Hz {
                    hz: 650.0,
                    x10: true,
                },
            ]
            .into_iter()
            .enumerate()
            {
                let mut s = settings(kind, mode, rate);
                s.song_lock = false;
                s.direction = [Direction::Forward, Direction::Reverse, Direction::PingPong][i];
                s.smooth = [0.0, 40.0, 10.0][i];
                s.delay = [Span::Off, Span::Seconds(0.004), Span::Off][i];
                s.fade_in = [Span::Seconds(0.01), Span::Off, Span::Off][i];
                s.phase = [0.0, 45.0, 200.0][i];
                s.swing = i == 1;
                all.push(s);
            }
        }
    }
    all
}

fn shaped(s: Settings) -> Lfo {
    let mut lfo = Lfo::new(s, 42);
    lfo.shape_mut().set_points(&[
        Point::new(0.0, 0.1).curve(0.7),
        Point::new(0.3, 0.9).step(),
        Point {
            y_mod: Some(PointMod { bus: 0, depth: 0.3 }),
            ..Point::new(0.55, 0.4)
        },
        Point::new(0.8, 1.0).curve(-0.4),
        Point::new(1.0, 0.0),
    ]);
    lfo.shape_mut().set_loopback(Some(2));
    lfo
}

#[test]
fn output_does_not_depend_on_block_size() {
    // Spec: plan R1.12 (sample-accurate and block-size-invariant)
    let triggers = [0, 333, 5000, 5001, 9000];
    for s in variety() {
        let mut t = Transport::new(SR);
        t.swing = 0.6;
        let reference = play(&mut shaped(s), &mut t.clone(), 12_000, 12_000, &triggers);
        for block in [1, 7, 64, 511] {
            let got = play(&mut shaped(s), &mut t.clone(), 12_000, block, &triggers);
            assert!(got == reference, "{s:?} in blocks of {block}");
        }
    }
    // Following a playing host's song position, the same within rounding.
    let mut s = settings(LfoType::Drawn, Mode::Free, Rate::default());
    s.song_lock = true;
    let mut t = Transport::new(SR);
    t.playing = true;
    t.tempo = 133.0;
    let a = play(&mut shaped(s), &mut t.clone(), 12_000, 12_000, &[]).0;
    let b = play(&mut shaped(s), &mut t.clone(), 12_000, 37, &[]).0;
    assert!(a.iter().zip(&b).all(|(a, b)| close(*a, *b, 1e-5)));
}

#[test]
fn a_note_starts_on_its_sample() {
    // Spec: plan R1.12 (sample-accurate)
    let s = settings(LfoType::Drawn, Mode::Restart, hz(50.0));
    let late = play(&mut shaped(s), &mut Transport::new(SR), 1000, 64, &[37]).0;
    let fresh = play(&mut shaped(s), &mut Transport::new(SR), 963, 64, &[0]).0;
    assert_eq!(late[37..], fresh[..]);
}

#[test]
fn the_simd_path_matches_the_scalar_path_at_every_level() {
    // Spec: plan "Performance" (SIMD across voices)
    const VOICES: usize = 19;
    const FRAMES: usize = 300;
    for s in variety().into_iter().chain([Settings {
        shared: true,
        ..settings(LfoType::Drawn, Mode::Restart, hz(9.0))
    }]) {
        let mut t = Transport::new(SR);
        t.swing = 0.3;
        let mut scalar = LfoBank::new(shaped(s), VOICES);
        let mut expected = vec![0.0; VOICES * FRAMES * 4];
        let mut expected_y = expected.clone();
        let mut tt = t;
        for (b, (out, out_y)) in expected
            .chunks_mut(VOICES * FRAMES)
            .zip(expected_y.chunks_mut(VOICES * FRAMES))
            .enumerate()
        {
            for v in (b % 3..VOICES).step_by(4) {
                scalar.trigger(v, (v * 13 + b * 7) % FRAMES);
                scalar.voice_mut(v).set_bus(0, b as f32 * 0.1);
            }
            scalar.process_scalar(&tt, FRAMES, out, Some(out_y));
            tt.advance(FRAMES);
        }
        for level in Level::ALL.into_iter().filter(|l| l.is_supported()) {
            let mut simd = LfoBank::new(shaped(s), VOICES);
            let mut got = vec![0.0; VOICES * FRAMES * 4];
            let mut got_y = got.clone();
            let mut tt = t;
            for (b, (out, out_y)) in got
                .chunks_mut(VOICES * FRAMES)
                .zip(got_y.chunks_mut(VOICES * FRAMES))
                .enumerate()
            {
                for v in (b % 3..VOICES).step_by(4) {
                    simd.trigger(v, (v * 13 + b * 7) % FRAMES);
                    simd.voice_mut(v).set_bus(0, b as f32 * 0.1);
                }
                simd.process_at(level, &tt, FRAMES, out, Some(out_y));
                tt.advance(FRAMES);
            }
            if let Some(i) =
                (0..got.len()).find(|&i| got[i] != expected[i] || got_y[i] != expected_y[i])
            {
                panic!(
                    "{s:?} at {level}: first difference at frame {} voice {}: {} {} / {} {}",
                    i / VOICES,
                    i % VOICES,
                    got[i],
                    expected[i],
                    got_y[i],
                    expected_y[i]
                );
            }
        }
    }
}

#[test]
fn a_bank_voice_matches_a_lone_voice() {
    // Spec: engine-core R82 (each voice its own LFO)
    let s = settings(LfoType::Drawn, Mode::Restart, hz(11.0));
    let mut bank = LfoBank::new(shaped(s), 3);
    bank.trigger(1, 10);
    let mut out = vec![0.0; 3 * 256];
    bank.process(&Transport::new(SR), 256, &mut out, None);
    let lone = play(&mut shaped(s), &mut Transport::new(SR), 256, 256, &[10]).0;
    let voice1: Vec<f32> = out.chunks(3).map(|f| f[1]).collect();
    assert_eq!(voice1, lone);
}
