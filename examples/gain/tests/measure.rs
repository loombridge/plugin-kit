//! Example Gain measured: a flat response with no phase shift at any
//! setting, no distortion, silence in for silence out, and the level it's
//! set to.

use lspk_example_gain::{GAIN, Gain};
use lspk_params::db_to_gain;
use lspk_test::measure::{self, db, log_frequencies};
use lspk_test::render::{self, Script, Settings};
use lspk_test::signal;

lspk_rt::install_trap!();

const RATE: f64 = 48_000.0;
const SETTINGS: Settings = Settings::new(RATE, 256);

/// The left channel of `input` through the gain at `gain_db`, set from the
/// first frame (its 20 ms glide is over long before anything is measured).
fn through(input: &[f32], gain_db: f64) -> Vec<f32> {
    let script = Script::new(input.len())
        .with_mono_input(input.to_vec())
        .param(0, GAIN, db_to_gain(gain_db));
    render::render::<Gain>(&script, SETTINGS)
        .expect("a render")
        .output
        .channels
        .swap_remove(0)
}

#[test]
fn its_response_is_flat_with_no_phase_shift() {
    for gain_db in [0.0, -6.0, 12.0] {
        let frequencies = log_frequencies(20.0, 20_000.0, 7);
        for point in measure::frequency_response(RATE, &frequencies, |x| through(x, gain_db)) {
            assert!(
                (point.gain_db - gain_db).abs() < 1e-3,
                "{point:?} at {gain_db} dB"
            );
            assert!(point.phase.abs() < 1e-6, "{point:?}");
        }
    }
}

#[test]
fn it_adds_no_distortion() {
    let input = signal::sine(997.0, RATE, 48_000, 0.5);
    let output = through(&input, -12.0);
    let steady = &output[4_800..];
    assert!(db(measure::thd_n(steady, RATE, 997.0)) < -120.0);
    assert!(db(measure::thd(steady, RATE, 997.0, 10)) < -120.0);
}

#[test]
fn silence_in_is_silence_out() {
    let output = through(&signal::silence(4_800), 12.0);
    assert_eq!(measure::noise_floor(&output), f64::NEG_INFINITY);
}

#[test]
fn levels_scale_by_the_gain() {
    let input = signal::noise(11, 48_000, 0.5);
    let output = through(&input, -6.0);
    let (rms_in, rms_out) = (
        measure::rms(&input[4_800..]),
        measure::rms(&output[4_800..]),
    );
    assert!((db(rms_out / rms_in) + 6.0).abs() < 1e-4);
    let (peak_in, peak_out) = (
        measure::peak(&input[4_800..]),
        measure::peak(&output[4_800..]),
    );
    assert!((db(peak_out / peak_in) + 6.0).abs() < 1e-4);
}
