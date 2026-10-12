//! WAV import and export: the round trip, the `clm ` chunk, every sample
//! format read, and plain WAVs split at a frame size.

use std::f64::consts::TAU;

use lspk_wavetable::wav::{self, Clm, SampleFormat};
use lspk_wavetable::{Error, FRAME_SIZE, Interpolation, MAX_FRAMES, Wavetable};

/// A table of `frames` frames, each a different mix of harmonics.
fn table(frames: usize) -> Wavetable {
    Wavetable::from_fn(frames, |frame, n| {
        let x = n as f64 / FRAME_SIZE as f64 * TAU;
        ((x.sin() + (frame as f64 + 2.0) * x).sin() * 0.8) as f32
    })
    .unwrap()
}

/// A WAV file's bytes, written by hand: format `tag`, `bits` per sample,
/// `channels`, samples already encoded, and an optional `clm ` text.
fn wav_file(tag: u16, bits: u16, channels: u16, data: &[u8], clm: Option<&str>) -> Vec<u8> {
    let mut fmt = Vec::new();
    let align = channels * bits.div_ceil(8);
    if tag == 0xFFFE {
        fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
    } else {
        fmt.extend_from_slice(&tag.to_le_bytes());
    }
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&48_000u32.to_le_bytes());
    fmt.extend_from_slice(&(48_000 * u32::from(align)).to_le_bytes());
    fmt.extend_from_slice(&align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if tag == 0xFFFE {
        // cbSize, valid bits, channel mask, then the float subformat GUID.
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&0u32.to_le_bytes());
        fmt.extend_from_slice(&3u16.to_le_bytes());
        fmt.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    let mut body = b"WAVE".to_vec();
    let mut chunk = |id: &[u8], content: &[u8]| {
        body.extend_from_slice(id);
        body.extend_from_slice(&(content.len() as u32).to_le_bytes());
        body.extend_from_slice(content);
        if content.len() % 2 == 1 {
            body.push(0);
        }
    };
    chunk(b"fmt ", &fmt);
    chunk(b"LIST", b"odd");
    if let Some(clm) = clm {
        chunk(b"clm ", clm.as_bytes());
    }
    chunk(b"data", data);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// The `clm ` chunk's text in a WAV file, if it has one.
fn clm_text(bytes: &[u8]) -> Option<String> {
    let at = bytes.windows(4).position(|w| w == b"clm ")?;
    let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
    Some(String::from_utf8(bytes[at + 8..at + 8 + len].to_vec()).unwrap())
}

#[test]
fn a_float_export_reads_back_exactly_with_its_interpolation() {
    // Spec: engine-core R13
    let source = table(5).with_interpolation(Interpolation::Spectral);
    let bytes = wav::write(&source, SampleFormat::Float32);
    let back = wav::read(&bytes, None).unwrap();
    assert_eq!(back, source);
    assert_eq!(back.interpolation(), Interpolation::Spectral);
    assert!(!back.factory());
}

#[test]
fn integer_exports_read_back_within_their_resolution() {
    let source = table(3).with_interpolation(Interpolation::Crossfade);
    for (format, step) in [
        (SampleFormat::Int16, 1.0 / 32_768.0),
        (SampleFormat::Int24, 1.0 / 8_388_608.0),
        (SampleFormat::Int32, 1.0 / 2_147_483_648.0),
    ] {
        let back = wav::read(&wav::write(&source, format), None).unwrap();
        assert_eq!(back.frame_count(), 3);
        assert_eq!(back.interpolation(), Interpolation::Crossfade);
        let worst = source
            .frames()
            .iter()
            .flat_map(|f| f.iter())
            .zip(back.frames().iter().flat_map(|f| f.iter()))
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        // Rounding to the nearest step, plus f32's own rounding.
        assert!(worst <= step * 0.5 + 1e-7, "{format:?}: {worst}");
    }
}

#[test]
fn every_interpolation_survives_the_clm_chunk() {
    // Spec: engine-core R13
    for interpolation in Interpolation::ALL {
        let source = table(2).with_interpolation(interpolation);
        let bytes = wav::write(&source, SampleFormat::Float32);
        let text = clm_text(&bytes).unwrap();
        assert!(
            text.starts_with(&format!("<!>2048 {}0000000 ", interpolation.digit())),
            "{text}"
        );
        assert_eq!(
            wav::read(&bytes, None).unwrap().interpolation(),
            interpolation
        );
    }
}

#[test]
fn the_clm_chunk_gives_the_frame_size_and_flags() {
    let clm = Clm::parse(b"<!>2048 21000000 wavetable (some tool)").unwrap();
    assert_eq!(clm.frame_size, 2048);
    assert_eq!(clm.interpolation, Interpolation::Spectral);
    assert!(clm.factory);

    // Flags are optional; a missing one reads as 0.
    let bare = Clm::parse(b"<!>1024").unwrap();
    assert_eq!(
        bare,
        Clm {
            frame_size: 1024,
            interpolation: Interpolation::None,
            factory: false,
        }
    );
    assert_eq!(Clm::parse(b"2048 10000000"), None);
    assert_eq!(Clm::parse(b"<!>wavetable"), None);
    let text = Clm {
        frame_size: 2048,
        interpolation: Interpolation::Crossfade,
        factory: false,
    }
    .text();
    assert!(text.starts_with("<!>2048 10000000"), "{text}");
}

#[test]
fn the_factory_flag_is_read_but_never_exported() {
    let data: Vec<u8> = table(2)
        .frames()
        .iter()
        .flat_map(|f| f.iter().flat_map(|x| x.to_le_bytes()))
        .collect();
    let bytes = wav_file(
        3,
        32,
        1,
        &data,
        Some("<!>2048 41000000 wavetable (a vendor)"),
    );
    let imported = wav::read(&bytes, None).unwrap();
    assert!(imported.factory());
    assert_eq!(imported.interpolation(), Interpolation::SpectralZeroPhases);

    let exported = wav::write(&imported, SampleFormat::Float32);
    let text = clm_text(&exported).unwrap();
    assert_eq!(&text[3..16], "2048 40000000", "{text}");
    assert!(!wav::read(&exported, None).unwrap().factory());
}

#[test]
fn every_sample_format_reads_to_the_same_table() {
    // Two frames of a sine and a half-level sine, held to values every
    // format stores exactly.
    let values: Vec<f64> = (0..2 * FRAME_SIZE)
        .map(|n| {
            let level = if n < FRAME_SIZE { 0.5 } else { 0.25 };
            let x = (level * (n as f64 / FRAME_SIZE as f64 * TAU).sin() * 128.0).round() / 128.0;
            x.min(127.0 / 128.0)
        })
        .collect();
    let encode =
        |f: &dyn Fn(f64) -> Vec<u8>| values.iter().flat_map(|&x| f(x)).collect::<Vec<u8>>();
    let files = [
        (
            "8-bit",
            wav_file(1, 8, 1, &encode(&|x| vec![(x * 128.0 + 128.0) as u8]), None),
        ),
        (
            "16-bit",
            wav_file(
                1,
                16,
                1,
                &encode(&|x| ((x * 32_768.0) as i16).to_le_bytes().to_vec()),
                None,
            ),
        ),
        (
            "24-bit",
            wav_file(
                1,
                24,
                1,
                &encode(&|x| ((x * 8_388_608.0) as i32).to_le_bytes()[..3].to_vec()),
                None,
            ),
        ),
        (
            "32-bit",
            wav_file(
                1,
                32,
                1,
                &encode(&|x| ((x * 2_147_483_648.0) as i32).to_le_bytes().to_vec()),
                None,
            ),
        ),
        (
            "float",
            wav_file(
                3,
                32,
                1,
                &encode(&|x| (x as f32).to_le_bytes().to_vec()),
                None,
            ),
        ),
        (
            "double",
            wav_file(3, 64, 1, &encode(&|x| x.to_le_bytes().to_vec()), None),
        ),
        (
            "extensible float",
            wav_file(
                0xFFFE,
                32,
                1,
                &encode(&|x| (x as f32).to_le_bytes().to_vec()),
                None,
            ),
        ),
        // Two channels: the first is the table, the second is ignored.
        (
            "stereo 16-bit",
            wav_file(
                1,
                16,
                2,
                &encode(&|x| {
                    let mut b = ((x * 32_768.0) as i16).to_le_bytes().to_vec();
                    b.extend_from_slice(&i16::MAX.to_le_bytes());
                    b
                }),
                None,
            ),
        ),
    ];
    for (name, bytes) in files {
        let table = wav::read(&bytes, None).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(table.frame_count(), 2, "{name}");
        for (got, want) in table.frames().iter().flat_map(|f| f.iter()).zip(&values) {
            assert_eq!(f64::from(*got), *want, "{name}");
        }
    }
}

#[test]
fn a_plain_wav_splits_at_the_frame_size_given_and_resamples_each_cycle() {
    // Four cycles of 600 samples, each a sine at a different phase plus
    // its third harmonic, and a few samples left over.
    let cycle = 600;
    let wave = |frame: usize, x: f64| (x + frame as f64).sin() * 0.5 + (3.0 * x).cos() * 0.25;
    let samples: Vec<f32> = (0..4 * cycle + 17)
        .map(|n| wave(n / cycle, (n % cycle) as f64 / cycle as f64 * TAU) as f32)
        .collect();
    let data: Vec<u8> = samples.iter().flat_map(|x| x.to_le_bytes()).collect();
    let bytes = wav_file(3, 32, 1, &data, None);

    let table = wav::read(&bytes, Some(cycle)).unwrap();
    assert_eq!(table.frame_count(), 4);
    assert_eq!(table.interpolation(), Interpolation::None);
    for (frame, samples) in table.frames().iter().enumerate() {
        for (n, &y) in samples.iter().enumerate() {
            let want = wave(frame, n as f64 / FRAME_SIZE as f64 * TAU);
            assert!(
                (f64::from(y) - want).abs() < 1e-5,
                "frame {frame}, sample {n}"
            );
        }
    }

    // A frame size in the `clm ` chunk splits the same way, and a size
    // given overrides it.
    let with_clm = wav_file(3, 32, 1, &data, Some("<!>600 00000000"));
    assert_eq!(wav::read(&with_clm, None).unwrap(), table);
    assert_eq!(wav::read(&with_clm, Some(1200)).unwrap().frame_count(), 2);
}

#[test]
fn a_longer_cycle_loses_the_harmonics_above_the_frame() {
    // A 4,096-sample cycle holding harmonic 3 and harmonic 1,500: the
    // frame keeps the first and removes the second.
    let n = 2 * FRAME_SIZE;
    let data: Vec<u8> = (0..n)
        .flat_map(|i| {
            let x = i as f64 / n as f64 * TAU;
            (((3.0 * x).sin() + (1500.0 * x).sin()) as f32 * 0.4).to_le_bytes()
        })
        .collect();
    let table = wav::read(&wav_file(3, 32, 1, &data, None), Some(n)).unwrap();
    for (i, &y) in table.frames()[0].iter().enumerate() {
        let want = 0.4 * (3.0 * i as f64 / FRAME_SIZE as f64 * TAU).sin();
        assert!((f64::from(y) - want).abs() < 1e-5, "sample {i}");
    }
}

#[test]
fn bad_files_and_sizes_are_errors() {
    assert_eq!(
        wav::read(b"not a wav file at all", None),
        Err(Error::NotWav)
    );
    let data = vec![0u8; 4 * FRAME_SIZE];
    assert_eq!(
        wav::read(&wav_file(1, 12, 1, &data, None), None),
        Err(Error::Unsupported {
            format: 1,
            bits: 12
        })
    );
    assert_eq!(
        wav::read(&wav_file(3, 32, 1, &data[..400], None), None),
        Err(Error::NoFrames)
    );
    assert_eq!(
        wav::read(&wav_file(3, 32, 1, &data, None), Some(1)),
        Err(Error::FrameSize(1))
    );
    let long = vec![0u8; 4 * 257 * 16];
    assert_eq!(
        wav::read(&wav_file(3, 32, 1, &long, None), Some(16)),
        Err(Error::TooManyFrames(257))
    );
    assert_eq!(Wavetable::new::<&[f32]>(&[]), Err(Error::NoFrames));
    assert_eq!(
        Wavetable::new(&[vec![0.0f32; 100]]),
        Err(Error::FrameSize(100))
    );
    assert_eq!(
        Wavetable::from_fn(MAX_FRAMES + 1, |_, _| 0.0),
        Err(Error::TooManyFrames(MAX_FRAMES + 1))
    );
}
