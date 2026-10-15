//! The noise oscillator: the colour blocks' spectra, Rate, Fine, Key
//! track, Start and Start random, Loop against one-shot, the colour
//! controls Stereo and Tone, and a render that touches no heap.

use lspk_test::measure;
use lspk_wavetable::noise::{COLOUR_RATE, Colour, Inputs, Noise, Params, Samples};
use lspk_wavetable::osc::Onset;

lspk_rt::install_trap!();

const RATE: f32 = 48_000.0;
const FRAMES: usize = 4_800;

/// A player at Level 100 % whose first note has started.
fn a_noise() -> Noise {
    let mut noise = Noise::new(1, RATE, FRAMES, 3);
    noise.params_mut().level = 100.0;
    noise.start(0, Onset::Fresh);
    noise
}

/// `noise` rendered `frames` frames from `source` at `pitch`, inside a
/// real-time section, so anything it allocates fails the test. Returns the
/// left and right channels.
fn render(
    noise: &mut Noise,
    source: &Samples,
    frames: usize,
    pitch: f32,
    start_mod: Option<&[f32]>,
) -> (Vec<f32>, Vec<f32>) {
    let pitch = [pitch];
    let mut out = vec![0.0; frames * 2];
    let inputs = Inputs {
        lanes: 1,
        pitch: &pitch,
        start_mod,
        ..Inputs::default()
    };
    lspk_rt::rt_test!({
        noise.process(frames, &inputs, source, &mut out);
    });
    (
        out.iter().step_by(2).copied().collect(),
        out.iter().skip(1).step_by(2).copied().collect(),
    )
}

/// A 1,000-frame mono ramp: sample `i` is `i / 1_000`, so a rendered value
/// says which frame the read position is on.
fn ramp() -> Samples {
    Samples::new(
        1,
        COLOUR_RATE,
        (0..1_000).map(|i| i as f32 / 1_000.0).collect(),
    )
}

/// The level of `signal` in the octave band centred on `centre`, in dBFS.
fn band(noise: &[f32], centre: f64) -> f64 {
    measure::bands(noise, f64::from(RATE))
        .into_iter()
        .find(|b| (b.centre - centre).abs() < 1.0)
        .expect("the band")
        .level_db
}

/// Pearson correlation of two signals.
fn correlation(left: &[f32], right: &[f32]) -> f64 {
    let mean = |x: &[f32]| x.iter().map(|&s| f64::from(s)).sum::<f64>() / x.len() as f64;
    let (ml, mr) = (mean(left), mean(right));
    let mut covariance = 0.0;
    let mut power_l = 0.0;
    let mut power_r = 0.0;
    for (&l, &r) in left.iter().zip(right) {
        let (l, r) = (f64::from(l) - ml, f64::from(r) - mr);
        covariance += l * r;
        power_l += l * l;
        power_r += r * r;
    }
    covariance / (power_l * power_r).sqrt()
}

// Spec: engine-core R49 (white flat, pink −3 dB per octave, brown −6).
#[test]
fn the_colour_noises_have_their_slopes() {
    for (colour, slope) in [
        (Colour::White, 0.0),
        (Colour::Pink, -3.0),
        (Colour::Brown, -6.0),
    ] {
        let block = Samples::colour(colour, 4.0, f64::from(RATE), 11);
        let left: Vec<f32> = block.data().iter().step_by(2).copied().collect();
        // 250 Hz to 8 kHz: five octaves. An octave band is twice the
        // width of the one below, so a flat spectrum reads 3 dB per octave
        // in these bands; the density's own slope is 3 dB less.
        let measured = (band(&left, 8_000.0) - band(&left, 250.0)) / 5.0 - 3.0;
        assert!(
            (measured - slope).abs() < 1.0,
            "{colour:?}: {measured:.2} dB per octave"
        );
    }
}

// Spec: engine-core R49 (a crackle of random clicks).
#[test]
fn crackle_is_clicks_not_a_hiss() {
    let block = Samples::colour(Colour::Crackle, 2.0, f64::from(RATE), 5);
    let left: Vec<f32> = block.data().iter().step_by(2).copied().collect();
    let loud = left.iter().filter(|&&x| x.abs() > 0.2).count() as f64 / left.len() as f64;
    assert!(loud < 0.1, "{loud:.3} of the samples are loud");
    assert!(left.iter().any(|&x| x.abs() > 0.5), "clicks are audible");
}

// Spec: engine-core R46 (Rate 50 % is the original speed; Key track
// follows the note), R5 (Key track off plays MIDI note 60).
#[test]
fn rate_fine_and_key_tracking_move_the_read_position() {
    let source = ramp();
    let mut noise = a_noise();
    let (left, _) = render(&mut noise, &source, 1_000, 60.0, None);
    assert!((left[100] - 0.1).abs() < 1e-3, "{}", left[100]);

    // Rate's provisional law: a quarter to four times around 50 %.
    let mut noise = a_noise();
    noise.params_mut().rate = 100.0;
    let (left, _) = render(&mut noise, &source, 200, 60.0, None);
    assert!((left[100] - 0.4).abs() < 1e-3, "{}", left[100]);

    // Fine is a semitone at 1.
    let mut noise = a_noise();
    noise.params_mut().fine = 1.0;
    let (left, _) = render(&mut noise, &source, 800, 60.0, None);
    let want = 799.0 * 2.0f64.powf(1.0 / 12.0) / 1_000.0;
    assert!((f64::from(left[799]) - want).abs() < 1e-3, "{}", left[799]);

    // Key tracking: note 72 is twice the speed, note 48 half of it.
    for (pitch, factor) in [(72.0f32, 2.0f64), (48.0, 0.5)] {
        let mut noise = a_noise();
        noise.params_mut().key_track = true;
        let (left, _) = render(&mut noise, &source, 500, pitch, None);
        let want = (499.0 * factor / 1_000.0).fract();
        assert!(
            (f64::from(left[499]) - want).abs() < 1e-3,
            "{pitch}: {} against {want}",
            left[499]
        );
    }

    // Off, every note plays at the block's own speed.
    let mut noise = a_noise();
    let (left, _) = render(&mut noise, &source, 1_000, 96.0, None);
    assert!((left[999] - 0.999).abs() < 1e-3);
}

// Spec: engine-core R47 (Start and Start random).
#[test]
fn start_and_start_random_move_where_a_note_begins() {
    let source = ramp();
    let mut noise = a_noise();
    noise.params_mut().start = 25.0;
    noise.start(0, Onset::Fresh);
    let (left, _) = render(&mut noise, &source, 100, 60.0, None);
    assert!((left[0] - 0.25).abs() < 1e-3, "{}", left[0]);

    // Start random spreads each voice's start across the block (R47:
    // chords don't repeat the same noise).
    let mut noise = Noise::new(2, RATE, FRAMES, 9);
    noise.params_mut().level = 100.0;
    noise.params_mut().start_random = 100.0;
    noise.start(0, Onset::Fresh);
    noise.start(1, Onset::Fresh);
    let (a, b) = (noise.position(0), noise.position(1));
    assert!(
        (0.0..1.0).contains(&a) && (0.0..1.0).contains(&b),
        "{a} {b}"
    );
    assert_ne!(a, b, "two voices draw different randoms");

    // Start modulation moves the read too (R47's scratching effect).
    let mut noise = a_noise();
    let modulation = vec![50.0f32; 100];
    let (left, _) = render(&mut noise, &source, 100, 60.0, Some(&modulation));
    assert!((left[0] - 0.5).abs() < 1e-3, "{}", left[0]);
}

// Spec: engine-core R48 (one-shot stops at the end of the block; Loop
// wraps).
#[test]
fn one_shot_stops_and_loop_wraps() {
    let source = ramp();
    let mut noise = a_noise();
    noise.params_mut().looped = false;
    let (left, _) = render(&mut noise, &source, 1_500, 60.0, None);
    assert!(left[500] > 0.4);
    assert_eq!(left[1_200], 0.0, "past the end of the block");

    let mut noise = a_noise();
    let (left, _) = render(&mut noise, &source, 1_500, 60.0, None);
    assert!((left[1_200] - 0.2).abs() < 1e-3, "{}", left[1_200]);
}

// Spec: engine-core R49 (Stereo from mono to uncorrelated channels, Tone
// tilting from low-passed to high-passed).
#[test]
fn the_colour_controls_run_from_mono_to_stereo_and_dark_to_bright() {
    let block = Samples::colour(Colour::White, 2.0, f64::from(RATE), 7);

    let mut noise = a_noise();
    let (left, right) = render(&mut noise, &block, FRAMES, 60.0, None);
    assert_eq!(left, right, "0 % is one mono stream");

    let mut noise = a_noise();
    noise.params_mut().stereo = 100.0;
    let (left, right) = render(&mut noise, &block, FRAMES, 60.0, None);
    let correlation = correlation(&left, &right);
    assert!(
        correlation.abs() < 0.05,
        "100 % is uncorrelated: {correlation}"
    );

    let mut noise = a_noise();
    noise.params_mut().tone = 0.0;
    let (dark, _) = render(&mut noise, &block, FRAMES, 60.0, None);
    let mut noise = a_noise();
    noise.params_mut().tone = 100.0;
    let (bright, _) = render(&mut noise, &block, FRAMES, 60.0, None);
    let tilt = |x: &[f32]| band(x, 4_000.0) - band(x, 250.0);
    assert!(
        tilt(&dark) < tilt(&bright) - 20.0,
        "0 % reads {} and 100 % {} dB from 250 Hz to 4 kHz",
        tilt(&dark),
        tilt(&bright)
    );
}

// Spec: engine-core R7 (Level is gain n²), section 6 (the noise's default
// level is 25 %).
#[test]
fn level_follows_the_square_law() {
    assert_eq!(Params::default().gain(), 0.0625);
    assert_eq!(
        Params {
            level: 0.0,
            ..Params::default()
        }
        .gain(),
        0.0
    );
}
