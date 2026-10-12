//! Example Sine measured: each key at its pitch, a pure tone with no
//! harmonics or aliasing even at the top of the keyboard, digital silence
//! when idle, and the level a voice is meant to have.

use lspk_example_sine::{Sine, VOICE_PEAK, frequency};
use lspk_params::db_to_gain;
use lspk_test::measure::{self, Pitch, db};
use lspk_test::render::{self, Script, Settings};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;

/// A second of `key` held at full velocity, from 0.1 s in (past the attack
/// and decay), left channel.
fn held(key: u8) -> Vec<f32> {
    let script = Script::new(52_800).note_on(0, key, 1.0);
    let render = render::render::<Sine>(&script, Settings::new(RATE, 128)).expect("a render");
    render.output.channel(0)[4_800..].to_vec()
}

#[test]
fn each_key_plays_at_its_pitch() {
    for key in [33u8, 60, 69, 96] {
        let measured = measure::pitch(&held(key), RATE).expect("a pitch");
        let expected = frequency(key);
        assert!(
            (measured - expected).abs() < 0.01,
            "key {key}: {measured} Hz, not {expected} Hz"
        );
    }
}

#[test]
fn a_held_note_is_a_pure_tone() {
    let note = held(69);
    assert!(db(measure::thd_n(&note, RATE, 440.0)) < -100.0);
    assert!(db(measure::thd(&note, RATE, 440.0, 10)) < -120.0);
}

#[test]
fn a_high_note_does_not_alias() {
    // Key 117 is 7,040 Hz, which doesn't divide 48 kHz.
    let key = 117;
    let note = held(key);
    let aliasing = measure::aliasing(&note, RATE, Pitch::Steady(frequency(key)));
    assert!(aliasing.ratio_db() < -100.0, "{aliasing:?}");
}

#[test]
fn idle_it_is_digitally_silent() {
    let render =
        render::render::<Sine>(&Script::new(4_800), Settings::new(RATE, 64)).expect("a render");
    assert_eq!(
        measure::noise_floor(render.output.channel(0)),
        f64::NEG_INFINITY
    );
    assert!(render.sleeps());
}

#[test]
fn a_voice_peaks_at_its_level() {
    // Full velocity at the default −6 dB level, sustained at 0.7.
    let expected = f64::from(VOICE_PEAK) * 0.7 * db_to_gain(-6.0);
    let note = held(69);
    assert!((measure::peak(&note) - expected).abs() < 1e-5);
    assert!((measure::rms(&note) - expected / 2f64.sqrt()).abs() < 1e-5);
}
