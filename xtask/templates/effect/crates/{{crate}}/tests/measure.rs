//! {{Name}} measured: at its defaults a flat response with no phase shift
//! and no distortion; silence in for silence out; the level it's set to; and
//! the drive adding harmonics, more as it's turned up.

use lspk_params::db_to_gain;
use lspk_test::measure::{self, db, log_frequencies};
use lspk_test::render::{self, Script, Settings};
use lspk_test::signal;
use {{crate_lib}}::{ {{Type}}, DRIVE, GAIN};

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;
const SETTINGS: Settings = Settings::new(RATE, 256);

/// The left channel of `input` through the effect with the gain at
/// `gain_db` and the drive at `drive` %, set from the first frame (their
/// 20 ms glides are over long before anything is measured).
fn through(input: &[f32], gain_db: f64, drive: f64) -> Vec<f32> {
    let script = Script::new(input.len())
        .with_mono_input(input.to_vec())
        .param(0, GAIN, db_to_gain(gain_db))
        .param(0, DRIVE, drive);
    render::render::<{{Type}}>(&script, SETTINGS)
        .expect("a render")
        .output
        .channels
        .swap_remove(0)
}

#[test]
fn clean_its_response_is_flat_with_no_phase_shift() {
    for gain_db in [0.0, -6.0, 12.0] {
        let frequencies = log_frequencies(20.0, 20_000.0, 7);
        for point in measure::frequency_response(RATE, &frequencies, |x| through(x, gain_db, 0.0)) {
            assert!(
                (point.gain_db - gain_db).abs() < 1e-3,
                "{point:?} at {gain_db} dB"
            );
            assert!(point.phase.abs() < 1e-6, "{point:?}");
        }
    }
}

#[test]
fn clean_it_adds_no_distortion() {
    let input = signal::sine(997.0, RATE, 48_000, 0.5);
    let steady = through(&input, -12.0, 0.0)[4_800..].to_vec();
    assert!(db(measure::thd_n(&steady, RATE, 997.0)) < -120.0);
}

#[test]
fn the_drive_adds_harmonics() {
    let input = signal::sine(997.0, RATE, 48_000, 0.5);
    let thd = |drive: f64| {
        let steady = through(&input, 0.0, drive)[4_800..].to_vec();
        db(measure::thd(&steady, RATE, 997.0, 10))
    };
    let (some, full) = (thd(30.0), thd(100.0));
    assert!(some > -60.0 && full > some, "{some} dB, then {full} dB");
}

#[test]
fn silence_in_is_silence_out() {
    let output = through(&signal::silence(4_800), 12.0, 100.0);
    assert_eq!(measure::noise_floor(&output), f64::NEG_INFINITY);
}

#[test]
fn levels_scale_by_the_gain() {
    let input = signal::noise(11, 48_000, 0.5);
    let output = through(&input, -6.0, 0.0);
    let (rms_in, rms_out) = (
        measure::rms(&input[4_800..]),
        measure::rms(&output[4_800..]),
    );
    assert!((db(rms_out / rms_in) + 6.0).abs() < 1e-4);
}
