use super::curve::{self, MAX_SEGMENT};
use super::*;

const RATE: f64 = 48_000.0;

fn timing() -> Timing {
    Timing::new(RATE)
}

/// Straight stages, for exact checks.
fn straight(params: EnvelopeParams) -> EnvelopeParams {
    EnvelopeParams {
        attack_curve: 0.5,
        decay_curve: 0.5,
        release_curve: 0.5,
        ..params
    }
}

fn render(env: &mut Envelope, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames];
    env.process(&mut out);
    out
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

// Spec: engine-core R70
#[test]
fn time_knobs_follow_32_seconds_times_n_to_the_fifth() {
    assert_eq!(law::milliseconds(0.0), 0.0);
    assert_eq!(law::milliseconds(0.5), 1000.0);
    assert_eq!(law::milliseconds(1.0), 32_000.0);
    for ms in [0.5, 1.0, 15.0, 1000.0, 32_000.0] {
        let n = law::normalised_for_milliseconds(ms);
        assert!(close(law::milliseconds(n), ms, ms * 1e-5), "{ms} ms");
    }
}

// Spec: engine-core R70, R71
#[test]
fn the_defaults_are_an_initialised_patch() {
    let amp = EnvelopeParams::AMP;
    assert_eq!(
        (amp.attack, amp.hold, amp.decay, amp.sustain, amp.release),
        (1.0, 0.0, 1000.0, 1.0, 15.0)
    );
    assert_eq!((amp.start, amp.end, amp.delay), (0.0, 0.0, 0.0));
    let modulation = EnvelopeParams::MODULATION;
    assert_eq!(modulation.attack, 0.5);
    assert_eq!(modulation.sustain, 1.0);
}

// Spec: engine-core R70
#[test]
fn stage_times_land_on_exact_samples() {
    // A 1 ms attack at 48 kHz: the start level at sample 0, full at 48.
    let params = EnvelopeParams {
        decay: 2.0,
        sustain: 0.5,
        ..straight(EnvelopeParams::AMP)
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    let out = render(&mut env, 200);
    assert_eq!(out[0], 0.0);
    assert!(out[47] < 1.0);
    assert_eq!(out[48], 1.0);
    // A 2 ms decay to 0.5 after it: sustain from sample 48 + 96.
    assert!(out[143] > 0.5);
    assert_eq!(out[144], 0.5);
    assert_eq!(out[199], 0.5);
    assert_eq!(env.stage(), Stage::Sustain);
}

// Spec: engine-core R71, R74
#[test]
fn stages_run_from_start_to_end_level_in_order() {
    let params = EnvelopeParams {
        delay: 1.0,
        attack: 1.0,
        hold: 1.0,
        decay: 1.0,
        sustain: law::level(0.5),
        release: 1.0,
        start: 0.25,
        end: 0.125,
        ..straight(EnvelopeParams::MODULATION)
    };
    let mut env = Envelope::new(params, timing());
    assert_eq!(env.stage(), Stage::Idle);
    assert_eq!(env.level(), 0.125, "idle holds the end level");
    env.trigger(Trigger::New);
    let mut stages = vec![];
    let mut out = vec![];
    for _ in 0..240 {
        stages.push(env.stage());
        out.push(env.next_level());
    }
    assert_eq!(&stages[..48], &[Stage::Delay; 48]);
    assert_eq!(&stages[48..96], &[Stage::Attack; 48]);
    assert_eq!(&stages[96..144], &[Stage::Hold; 48]);
    assert_eq!(&stages[144..192], &[Stage::Decay; 48]);
    assert_eq!(&stages[192..], &[Stage::Sustain; 48]);
    assert!(
        out[..48].iter().all(|&y| y == 0.25),
        "delay holds the start"
    );
    assert_eq!(out[48], 0.25);
    assert!(close(out[72], 0.625, 1e-6), "halfway up a straight attack");
    assert!(out[96..144].iter().all(|&y| y == 1.0), "hold holds full");
    assert!(
        out[192..].iter().all(|&y| y == 0.25),
        "sustain at n² = 25 %"
    );
    env.release();
    let tail = render(&mut env, 60);
    assert_eq!(tail[0], 0.25);
    assert!(close(tail[24], 0.1875, 1e-6), "halfway down the release");
    assert_eq!(tail[48], 0.125);
    assert!(env.is_idle());
}

// Spec: engine-core R71, R69
#[test]
fn an_amplifier_envelope_starts_and_ends_at_silence_and_goes_idle() {
    let mut env = Envelope::new(EnvelopeParams::AMP, timing());
    env.trigger(Trigger::New);
    assert_eq!(env.next_level(), 0.0);
    render(&mut env, 4800);
    env.release();
    render(&mut env, 720);
    assert!(env.is_idle(), "15 ms release is 720 samples");
    assert_eq!(env.level(), 0.0);
}

// Spec: engine-core R72
#[test]
fn an_early_release_starts_from_the_current_level() {
    let params = EnvelopeParams {
        attack: 10.0,
        release: 10.0,
        ..straight(EnvelopeParams::AMP)
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 120);
    let level = env.level();
    assert!(close(level, 0.25, 1e-6));
    env.release();
    let out = render(&mut env, 481);
    assert_eq!(out[0], level, "no jump");
    assert!(close(out[240], level / 2.0, 1e-6), "the full release time");
    assert_eq!(out[480], 0.0);
}

// Spec: engine-core R73
#[test]
fn curves_are_straight_at_50_percent_and_bend_either_way() {
    for x in [0.0, 0.1, 0.5, 0.9, 1.0] {
        assert!((curve::shape(0.5, x) - x).abs() < 1e-12);
    }
    // Above 50 % fast first, below slow first; the two mirror each other.
    for c in [0.6_f32, 0.666, 0.8, 1.0] {
        for x in [0.1, 0.25, 0.5, 0.75, 0.9] {
            let fast = curve::shape(c, x);
            let slow = curve::shape(1.0 - c, 1.0 - x);
            assert!(fast > x, "{c} at {x}");
            assert!((fast - (1.0 - slow)).abs() < 1e-12, "{c} mirrors at {x}");
        }
    }
    // Monotonic in time and in the control; the ends are fixed.
    let mut previous_curve = f64::NEG_INFINITY;
    for step in 0..=20 {
        let c = step as f32 / 20.0;
        assert_eq!(curve::shape(c, 0.0), 0.0);
        assert!((curve::shape(c, 1.0) - 1.0).abs() < 1e-12);
        let mid = curve::shape(c, 0.4);
        assert!(mid >= previous_curve);
        previous_curve = mid;
        let mut previous = 0.0;
        for i in 1..=100 {
            let y = curve::shape(c, f64::from(i) / 100.0);
            assert!(y >= previous);
            previous = y;
        }
    }
    // The default 66.6 % covers about 81 % in the first 40 % of the time.
    assert!((curve::shape(0.666, 0.4) - 0.81).abs() < 0.01);
}

// Spec: engine-core R73
#[test]
fn the_default_curves_are_50_66_6_and_66_6_percent() {
    for params in [EnvelopeParams::AMP, EnvelopeParams::MODULATION] {
        assert_eq!(
            (
                params.attack_curve,
                params.decay_curve,
                params.release_curve
            ),
            (0.5, 0.666, 0.666)
        );
    }
}

// Spec: engine-core R73
#[test]
fn a_bent_decay_follows_its_curve_sample_by_sample() {
    let params = EnvelopeParams {
        attack: 0.0,
        decay: 100.0,
        sustain: 0.2,
        decay_curve: 0.8,
        ..EnvelopeParams::AMP
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    let out = render(&mut env, 4801);
    for (i, &y) in out.iter().enumerate().take(4800) {
        let x = i as f64 / 4800.0;
        let expected = 1.0 - 0.8 * curve::shape(0.8, x);
        assert!((f64::from(y) - expected).abs() < 2e-6, "sample {i}: {y}");
    }
    assert_eq!(out[4800], 0.2);
}

// Spec: engine-core R70, R73, R80
#[test]
fn long_stages_stay_on_their_curve() {
    // The longest knob time at the largest scale: 320 s, at 8 kHz to keep
    // the test quick (2.56 million samples, 157 segments).
    for curve_control in [0.5, 0.5004, 0.52, 0.666, 1.0, 0.0] {
        let params = EnvelopeParams {
            attack: law::MAX_SECONDS * 1000.0,
            attack_curve: curve_control,
            ..EnvelopeParams::AMP
        };
        let timing = Timing::new(8000.0).with_scale(MAX_TIME_SCALE);
        let total = timing.samples(params.attack, params.unit);
        assert_eq!(total, 2_560_000);
        assert!(total > 100 * u64::from(MAX_SEGMENT));
        let mut env = Envelope::new(params, timing);
        env.trigger(Trigger::New);
        let mut block = vec![0.0; 10_007];
        let mut at = 0u64;
        let mut worst = 0.0_f64;
        while at < total {
            env.process(&mut block);
            for (i, &y) in block.iter().enumerate() {
                let n = at + i as u64;
                if n < total {
                    let x = n as f64 / total as f64;
                    let expected = curve::shape(curve_control, x);
                    worst = worst.max((f64::from(y) - expected).abs());
                }
            }
            at += block.len() as u64;
        }
        assert!(worst < 2e-4, "curve {curve_control}: off by {worst}");
    }
}

// Spec: engine-core R74
#[test]
fn levels_follow_n_squared_and_amp_sustain_reads_in_decibels() {
    assert_eq!(law::level(0.5), 0.25);
    assert_eq!(law::level(1.0), 1.0);
    assert_eq!(law::normalised_for_level(0.25), 0.5);
    assert_eq!(law::decibels(1.0), 0.0);
    assert!((law::decibels(law::level(0.5)) + 12.041).abs() < 1e-3);
    assert_eq!(law::decibels(0.0), f32::NEG_INFINITY);
}

// Spec: engine-core R75
#[test]
fn tempo_times_are_beats_at_the_host_tempo() {
    let params = EnvelopeParams {
        unit: TimeUnit::Tempo,
        attack: 1.0,
        ..straight(EnvelopeParams::AMP)
    };
    let mut env = Envelope::new(params, timing().with_tempo(120.0));
    env.trigger(Trigger::New);
    let out = render(&mut env, 24_001);
    assert!(out[23_999] < 1.0);
    assert_eq!(out[24_000], 1.0, "a beat at 120 bpm is 0.5 s");
}

// Spec: engine-core R75
#[test]
fn switching_to_tempo_snaps_to_the_nearest_division() {
    // 500 ms at 120 bpm is a beat; 260 ms is nearest an eighth (250 ms).
    assert_eq!(division_for_milliseconds(500.0, 120.0), 1.0);
    assert_eq!(division_for_milliseconds(260.0, 120.0), 0.5);
    // Dotted and triplet divisions: 750 ms is a dotted quarter, 333 ms a
    // quarter triplet.
    assert_eq!(division_for_milliseconds(750.0, 120.0), 1.5);
    assert!((division_for_milliseconds(333.3, 120.0) - 2.0 / 3.0).abs() < 1e-6);
    assert_eq!(nearest_division(0.0), 0.0);
    assert_eq!(nearest_division(1000.0), 128.0, "32 bars at most");
    assert_eq!(nearest_division(1e-6), 4.0 / 256.0 * 2.0 / 3.0);
}

// Spec: engine-core R75
#[test]
fn a_tempo_change_re_plans_the_stage_in_progress() {
    let params = EnvelopeParams {
        unit: TimeUnit::Tempo,
        attack: 1.0,
        ..straight(EnvelopeParams::AMP)
    };
    let mut env = Envelope::new(params, timing().with_tempo(120.0));
    env.trigger(Trigger::New);
    render(&mut env, 12_000);
    let level = env.level();
    assert!(close(level, 0.5, 1e-6));
    // At 60 bpm the beat is 48 000 samples: 36 000 remain.
    env.set_timing(timing().with_tempo(60.0));
    let out = render(&mut env, 36_001);
    assert_eq!(out[0], level, "no jump");
    assert!(out[35_999] < 1.0);
    assert_eq!(out[36_000], 1.0);
}

// Spec: engine-core R76
#[test]
fn a_legato_note_leaves_the_envelope_alone() {
    let mut env = Envelope::new(EnvelopeParams::AMP, timing());
    env.trigger(Trigger::New);
    render(&mut env, 1000);
    let (stage, level) = (env.stage(), env.level());
    env.trigger(Trigger::Legato);
    assert_eq!((env.stage(), env.level()), (stage, level));
}

// Spec: engine-core R76
#[test]
fn a_legato_inverted_envelope_restarts_on_a_legato_note() {
    let params = EnvelopeParams {
        legato_inverted: true,
        attack: 100.0,
        sustain: 0.5,
        ..EnvelopeParams::AMP
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 6000);
    assert_eq!(env.stage(), Stage::Decay);
    env.trigger(Trigger::Legato);
    assert_eq!(env.stage(), Stage::Attack);
    // A modulation envelope restarts from its start level by default.
    let params = EnvelopeParams {
        legato_inverted: true,
        start: 0.0,
        ..EnvelopeParams::MODULATION
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 1000);
    env.trigger(Trigger::Legato);
    assert_eq!((env.stage(), env.level()), (Stage::Attack, 0.0));
}

// Spec: engine-core R77
#[test]
fn legato_notes_keep_the_sustain_level_with_no_release() {
    let params = EnvelopeParams {
        release: 0.0,
        sustain: 0.6,
        decay: 1.0,
        ..EnvelopeParams::AMP
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 500);
    assert_eq!(env.level(), 0.6);
    for _ in 0..4 {
        env.trigger(Trigger::Legato);
        let out = render(&mut env, 100);
        assert!(out.iter().all(|&y| y == 0.6));
    }
    env.release();
    assert_eq!(env.next_level(), 0.0, "a release of 0 ends at once");
    assert!(env.is_idle());
}

// Spec: engine-core R78
#[test]
fn hold_can_change_while_holding() {
    let params = EnvelopeParams {
        attack: 0.0,
        hold: 10.0,
        ..EnvelopeParams::AMP
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 240);
    assert_eq!(env.stage(), Stage::Hold);
    // Longer: 20 ms in all, so 720 more samples of hold.
    env.set_params(EnvelopeParams {
        hold: 20.0,
        ..params
    });
    let out = render(&mut env, 721);
    assert!(out[..720].iter().all(|&y| y == 1.0));
    assert_eq!(env.stage(), Stage::Decay);
    // Shorter than what has passed: the hold ends at once.
    env.trigger(Trigger::New);
    render(&mut env, 240);
    env.set_params(EnvelopeParams {
        hold: 2.0,
        ..params
    });
    assert_eq!(env.stage(), Stage::Decay);
}

// Spec: engine-core R78
#[test]
fn unchanged_settings_leave_the_stage_untouched() {
    let mut env = Envelope::new(EnvelopeParams::AMP, timing());
    let mut reference = env.clone();
    env.trigger(Trigger::New);
    reference.trigger(Trigger::New);
    let expected = render(&mut reference, 48_000);
    let mut out = vec![];
    for _ in 0..375 {
        env.set_params(EnvelopeParams::AMP);
        env.set_timing(timing());
        out.extend(render(&mut env, 128));
    }
    assert_eq!(out, expected);
}

// Spec: engine-core R79
#[test]
fn a_stolen_amp_envelope_restarts_from_its_current_level() {
    let params = EnvelopeParams {
        attack: 10.0,
        ..straight(EnvelopeParams::AMP)
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 240);
    let level = env.level();
    assert!(close(level, 0.5, 1e-6));
    env.trigger(Trigger::Retrigger);
    let out = render(&mut env, 241);
    assert!(close(out[0], level, 1e-6), "mid-attack, not from silence");
    // It joins the attack's curve, so only the rest of the attack remains.
    assert_eq!(out[240], 1.0);
}

// Spec: engine-core R79
#[test]
fn a_stolen_amp_envelope_can_restart_from_zero() {
    let params = EnvelopeParams {
        steal_restart: StealRestart::StartLevel,
        ..EnvelopeParams::AMP
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 2400);
    env.trigger(Trigger::Retrigger);
    assert_eq!((env.stage(), env.level()), (Stage::Attack, 0.0));
}

// Spec: engine-core R79
#[test]
fn a_stolen_modulation_envelope_restarts_from_its_start_level_by_default() {
    let params = EnvelopeParams {
        start: 0.25,
        attack: 10.0,
        ..straight(EnvelopeParams::MODULATION)
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 2400);
    env.trigger(Trigger::Retrigger);
    assert_eq!(env.level(), 0.25);
    // From the stolen voice's level, when chosen.
    let mut env = Envelope::new(
        EnvelopeParams {
            steal_restart: StealRestart::CurrentLevel,
            ..params
        },
        timing(),
    );
    env.trigger(Trigger::New);
    render(&mut env, 240);
    let level = env.level();
    env.trigger(Trigger::Retrigger);
    assert!(close(env.level(), level, 1e-6));
}

// Spec: engine-core R79
#[test]
fn a_restart_below_the_start_level_takes_the_whole_attack() {
    let params = EnvelopeParams {
        start: 0.5,
        attack: 1.0,
        release: 1.0,
        end: 0.0,
        steal_restart: StealRestart::CurrentLevel,
        ..straight(EnvelopeParams::MODULATION)
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 100);
    env.release();
    render(&mut env, 36);
    let level = env.level();
    assert!(level < 0.5);
    env.trigger(Trigger::Retrigger);
    let out = render(&mut env, 49);
    assert_eq!(out[0], level);
    assert!(out[47] < 1.0);
    assert_eq!(out[48], 1.0);
}

// Spec: engine-core R80
#[test]
fn the_global_scale_multiplies_every_time() {
    let params = straight(EnvelopeParams::AMP);
    for (scale, samples) in [(2.0, 96), (0.5, 24), (10.0, 480), (0.1, 5), (20.0, 480)] {
        let mut env = Envelope::new(params, timing().with_scale(scale));
        env.trigger(Trigger::New);
        let out = render(&mut env, samples + 1);
        assert!(out[samples - 1] < 1.0, "scale {scale}");
        assert_eq!(out[samples], 1.0, "scale {scale}");
    }
}

// Spec: engine-core R118
#[test]
fn a_mono_retrigger_restarts_the_attack_from_the_current_level() {
    let params = EnvelopeParams {
        attack: 100.0,
        sustain: 0.5,
        ..EnvelopeParams::AMP
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 24_000);
    assert_eq!(env.stage(), Stage::Decay);
    let level = env.level();
    env.trigger(Trigger::Retrigger);
    assert_eq!(env.stage(), Stage::Attack);
    assert!(close(env.next_level(), level, 1e-3));
}

// Spec: engine-core R119
#[test]
fn a_joined_voice_follows_the_held_voice_s_envelope() {
    let mut bank = EnvelopeBank::new(2, EnvelopeParams::AMP, timing());
    bank.trigger(0, Trigger::New);
    let mut out = vec![0.0; bank.stride() * 100];
    bank.process(&mut out);
    bank.join(1, 0);
    assert_eq!(bank.level(1), bank.level(0));
    bank.process(&mut out);
    for frame in out.chunks_exact(bank.stride()) {
        assert_eq!(frame[0], frame[1]);
    }
}

#[test]
fn a_new_sustain_level_retargets_the_decay() {
    let params = EnvelopeParams {
        attack: 0.0,
        decay: 10.0,
        sustain: 0.0,
        ..straight(EnvelopeParams::AMP)
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    render(&mut env, 240);
    let level = env.level();
    env.set_params(EnvelopeParams {
        sustain: 0.5,
        ..params
    });
    let out = render(&mut env, 241);
    assert_eq!(out[0], level);
    assert_eq!(out[240], 0.5);
    env.set_params(EnvelopeParams {
        sustain: 0.75,
        ..params
    });
    assert_eq!(env.level(), 0.75, "a sustain level applies at once");
}

#[test]
fn the_samples_do_not_depend_on_the_block_size() {
    let params = EnvelopeParams {
        delay: 0.3,
        attack: 3.0,
        hold: 0.7,
        decay: 5.0,
        sustain: 0.3,
        release: 4.0,
        attack_curve: 0.3,
        ..EnvelopeParams::MODULATION
    };
    let run = |block: usize| {
        let mut env = Envelope::new(params, timing());
        let mut out = vec![0.0; 2000];
        let events: [(usize, Option<Trigger>); 4] = [
            (0, Some(Trigger::New)),
            (500, None),
            (600, Some(Trigger::Retrigger)),
            (900, None),
        ];
        let mut at = 0;
        for (index, chunk) in out.chunks_mut(block).enumerate() {
            let start = index * block;
            let mut done = 0;
            for &(when, event) in &events {
                if (start..start + chunk.len()).contains(&when) {
                    env.process(&mut chunk[done..when - start]);
                    done = when - start;
                    match event {
                        Some(trigger) => env.trigger(trigger),
                        None => env.release(),
                    }
                }
            }
            env.process(&mut chunk[done..]);
            at += chunk.len();
        }
        assert_eq!(at, 2000);
        out
    };
    let reference = run(2000);
    for block in [1, 7, 32, 128, 333] {
        assert_eq!(run(block), reference, "block {block}");
    }
}

/// The bank's output for one voice.
fn lane(out: &[f32], stride: usize, voice: usize) -> Vec<f32> {
    out.chunks_exact(stride).map(|frame| frame[voice]).collect()
}

#[test]
fn every_bank_lane_matches_a_single_envelope_on_every_simd_level() {
    let voices = 21;
    let params = |voice: usize| EnvelopeParams {
        attack: 1.0 + voice as f32 * 0.37,
        decay: 2.0 + voice as f32 * 0.5,
        sustain: 0.05 * voice as f32,
        release: 3.0,
        attack_curve: voice as f32 / 20.0,
        ..EnvelopeParams::MODULATION
    };
    let frames = 1500;
    let mut expected = vec![];
    for voice in 0..voices {
        let mut env = Envelope::new(params(voice), timing());
        let mut out = render(&mut env, voice * 11);
        env.trigger(Trigger::New);
        out.extend(render(&mut env, 700 - voice * 11));
        env.release();
        out.extend(render(&mut env, frames - 700));
        expected.push(out);
    }
    for level in Level::ALL {
        let mut bank = EnvelopeBank::new(voices, EnvelopeParams::AMP, timing());
        assert_eq!(bank.stride(), 32);
        for voice in 0..voices {
            bank.set_params(voice, params(voice));
        }
        let stride = bank.stride();
        let mut out = vec![0.0; frames * stride];
        let mut at = 0;
        let mut ok = true;
        for frame in 0..=frames {
            let note_on: Vec<usize> = (0..voices).filter(|v| v * 11 == frame).collect();
            if !note_on.is_empty() || frame == 700 || frame == frames {
                ok &= bank.process_at(level, &mut out[at * stride..frame * stride]);
                at = frame;
                for &voice in &note_on {
                    bank.trigger(voice, Trigger::New);
                }
                if frame == 700 {
                    (0..voices).for_each(|voice| bank.release(voice));
                }
            }
        }
        if !ok {
            continue;
        }
        for (voice, expected) in expected.iter().enumerate() {
            assert_eq!(
                &lane(&out, stride, voice),
                expected,
                "{level}, voice {voice}"
            );
        }
        for voice in voices..stride {
            assert!(lane(&out, stride, voice).iter().all(|&y| y == 0.0));
        }
    }
}

#[test]
fn an_idle_bank_writes_its_resting_levels() {
    let params = EnvelopeParams {
        end: 0.5,
        ..EnvelopeParams::MODULATION
    };
    let mut bank = EnvelopeBank::new(4, params, timing());
    let mut out = vec![1.0; bank.stride() * 64];
    bank.process(&mut out);
    for frame in out.chunks_exact(bank.stride()) {
        assert_eq!(&frame[..4], &[0.5; 4]);
        assert!(frame[4..].iter().all(|&y| y == 0.0));
    }
    assert!((0..4).all(|voice| bank.is_idle(voice)));
}

#[test]
fn reset_goes_idle_at_once() {
    let mut env = Envelope::new(EnvelopeParams::AMP, timing());
    env.trigger(Trigger::New);
    render(&mut env, 100);
    env.reset();
    assert!(env.is_idle());
    assert_eq!(env.level(), 0.0);
}

#[test]
fn out_of_range_settings_are_clamped() {
    let params = EnvelopeParams {
        attack: -5.0,
        sustain: 2.0,
        start: f32::NAN,
        decay_curve: 7.0,
        ..EnvelopeParams::AMP
    };
    let mut env = Envelope::new(params, timing());
    env.trigger(Trigger::New);
    let out = render(&mut env, 10);
    assert!(out.iter().all(|&y| y == 1.0), "{out:?}");
    assert_eq!(
        timing()
            .with_scale(0.0)
            .samples(1.0, TimeUnit::Milliseconds),
        5
    );
    assert_eq!(timing().samples(f32::NAN, TimeUnit::Milliseconds), 0);
}
