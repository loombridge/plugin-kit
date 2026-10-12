//! Wavetables as WAV files, in the form the wavetable synthesiser community
//! shares them: frames one after another in a mono WAV, with a `clm ` chunk
//! giving the frame size and flags.
//!
//! The `clm ` chunk's text is `<!>`, the frame size in decimal, a space,
//! then eight flag digits, and optionally a space and a note naming the
//! tool that wrote it: `<!>2048 20000000 wavetable (lspk-wavetable)`. The
//! first flag is the [`Interpolation`] digit; the second marks another
//! synthesiser's factory content, which third parties never set, so
//! [`write`] always writes 0 there; the rest are written as 0.

use crate::spectrum::Ffts;
use crate::{Error, FRAME_SIZE, Interpolation, MAX_FRAMES, Wavetable};

/// The `fmt ` chunk's format tag for integer PCM.
const PCM: u16 = 1;
/// The format tag for IEEE floating point.
const FLOAT: u16 = 3;
/// The format tag for `WAVE_FORMAT_EXTENSIBLE`, whose subformat names the
/// real one.
const EXTENSIBLE: u16 = 0xFFFE;

/// The sample rate written in exported files. A wavetable has no sample
/// rate; this is the usual one.
const EXPORT_RATE: u32 = 44_100;

/// What the note in an exported `clm ` chunk says wrote it.
const WRITER: &str = "wavetable (lspk-wavetable)";

/// How [`write`] stores samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SampleFormat {
    /// 16-bit integers.
    Int16,
    /// 24-bit integers.
    Int24,
    /// 32-bit integers.
    Int32,
    /// 32-bit floating point: every sample exactly.
    #[default]
    Float32,
}

impl SampleFormat {
    const fn bits(self) -> u16 {
        match self {
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Int32 | Self::Float32 => 32,
        }
    }
}

/// What a `clm ` chunk says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clm {
    /// The samples in one frame.
    pub frame_size: usize,
    /// How the frames between drawn frames are made.
    pub interpolation: Interpolation,
    /// Whether the file is marked as another synthesiser's factory content.
    pub factory: bool,
}

impl Clm {
    /// Reads a `clm ` chunk's data; `None` if it doesn't start `<!>` and a
    /// frame size.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        let rest = data.strip_prefix(b"<!>")?;
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let frame_size = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
        let flags = rest[digits..].strip_prefix(b" ").unwrap_or(&[]);
        let flag = |n: usize| {
            flags
                .get(n)
                .filter(|b| b.is_ascii_digit())
                .map_or(0, |b| b - b'0')
        };
        Some(Self {
            frame_size,
            interpolation: Interpolation::from_digit(flag(0)),
            factory: flag(1) == 1,
        })
    }

    /// The chunk's text, without padding. The factory flag is written as
    /// given; [`write`] always gives `false`.
    #[must_use]
    pub fn text(&self) -> String {
        format!(
            "<!>{} {}{}000000 {WRITER}",
            self.frame_size,
            self.interpolation.digit(),
            u8::from(self.factory)
        )
    }
}

/// The audio and `clm ` chunk of a WAV file, before it's split into frames.
struct Parsed {
    samples: Vec<f32>,
    clm: Option<Clm>,
}

/// Reads a wavetable from a WAV file's bytes.
///
/// The frames are `frame_size` samples long if given, otherwise as long as
/// the file's `clm ` chunk says, otherwise [`FRAME_SIZE`]; a plain WAV of
/// single cycles is split at the size given. Frames of another size are
/// resampled to [`FRAME_SIZE`] by FFT, keeping each harmonic's amplitude
/// and phase and removing those above the frame's Nyquist. Samples after
/// the last whole frame are ignored. The interpolation and factory flag
/// come from the `clm ` chunk, if there is one.
///
/// Reads 8-bit, 16-bit, 24-bit and 32-bit integer and 32-bit and 64-bit
/// float data, plain or `WAVE_FORMAT_EXTENSIBLE`. Of several channels, the
/// first is the table.
///
/// # Errors
///
/// [`Error::NotWav`], [`Error::Malformed`], [`Error::Unsupported`],
/// [`Error::FrameSize`] for a frame size below 2, [`Error::NoFrames`] or
/// [`Error::TooManyFrames`].
pub fn read(bytes: &[u8], frame_size: Option<usize>) -> Result<Wavetable, Error> {
    let parsed = parse(bytes)?;
    let size = frame_size
        .or(parsed.clm.map(|c| c.frame_size))
        .unwrap_or(FRAME_SIZE);
    if size < 2 {
        return Err(Error::FrameSize(size));
    }
    let count = parsed.samples.len() / size;
    match count {
        0 => return Err(Error::NoFrames),
        n if n > MAX_FRAMES => return Err(Error::TooManyFrames(n)),
        _ => {}
    }
    let mut ffts = Ffts::new();
    let frames: Vec<_> = parsed
        .samples
        .chunks_exact(size)
        .map(|cycle| ffts.resample(cycle))
        .collect();
    let mut table = Wavetable::new(&frames.iter().map(|f| &f[..]).collect::<Vec<_>>())?;
    if let Some(clm) = parsed.clm {
        table.set_interpolation(clm.interpolation);
        table.set_factory(clm.factory);
    }
    Ok(table)
}

/// Writes `table`'s drawn frames as a mono WAV with a `clm ` chunk giving
/// the frame size and the interpolation. The factory flag is never set,
/// whatever `table` says. Morphed frames aren't written: a reader makes
/// them again from the interpolation (engine-core R13).
#[must_use]
pub fn write(table: &Wavetable, format: SampleFormat) -> Vec<u8> {
    let clm = Clm {
        frame_size: FRAME_SIZE,
        interpolation: table.interpolation(),
        factory: false,
    }
    .text();
    let bits = format.bits();
    let width = usize::from(bits / 8);
    let data_len = table.frame_count() * FRAME_SIZE * width;
    let mut out = Vec::with_capacity(64 + clm.len() + data_len);
    out.extend_from_slice(b"RIFF\0\0\0\0WAVE");

    chunk(&mut out, b"fmt ", |out| {
        let tag = if format == SampleFormat::Float32 {
            FLOAT
        } else {
            PCM
        };
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&EXPORT_RATE.to_le_bytes());
        out.extend_from_slice(&(EXPORT_RATE * u32::from(bits / 8)).to_le_bytes());
        out.extend_from_slice(&(bits / 8).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
    });
    chunk(&mut out, b"clm ", |out| {
        out.extend_from_slice(clm.as_bytes())
    });
    chunk(&mut out, b"data", |out| {
        for &x in table.frames().iter().flat_map(|f| f.iter()) {
            let int = |full: f64| (f64::from(x) * full).round().clamp(-full, full - 1.0) as i64;
            match format {
                SampleFormat::Int16 => out.extend_from_slice(&(int(32_768.0) as i16).to_le_bytes()),
                SampleFormat::Int24 => {
                    out.extend_from_slice(&(int(8_388_608.0) as i32).to_le_bytes()[..3])
                }
                SampleFormat::Int32 => {
                    out.extend_from_slice(&(int(2_147_483_648.0) as i32).to_le_bytes());
                }
                SampleFormat::Float32 => out.extend_from_slice(&x.to_le_bytes()),
            }
        }
    });

    let riff = u32::try_from(out.len() - 8).unwrap_or(u32::MAX);
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    out
}

/// Appends a chunk whose data `fill` writes, with its length and padding.
fn chunk(out: &mut Vec<u8>, id: &[u8; 4], fill: impl FnOnce(&mut Vec<u8>)) {
    out.extend_from_slice(id);
    let at = out.len();
    out.extend_from_slice(&[0; 4]);
    fill(out);
    let len = out.len() - at - 4;
    out[at..at + 4].copy_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes());
    if len % 2 == 1 {
        out.push(0);
    }
}

/// The format a `fmt ` chunk gives.
struct Format {
    tag: u16,
    channels: usize,
    bits: u16,
    align: usize,
}

fn parse(bytes: &[u8]) -> Result<Parsed, Error> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Error::NotWav);
    }
    let mut format = None;
    let mut data = None;
    let mut clm = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len =
            u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("four bytes")) as usize;
        let body = at + 8;
        // A truncated last chunk is read as far as it goes, as most readers
        // do with a data chunk whose length was never filled in.
        let end = body.saturating_add(len).min(bytes.len());
        let content = &bytes[body..end];
        match id {
            b"fmt " => format = Some(parse_format(content)?),
            b"data" => data = Some(content),
            b"clm " => clm = Clm::parse(content),
            _ => {}
        }
        at = end + (len % 2);
    }
    let format = format.ok_or(Error::Malformed("no fmt chunk"))?;
    let data = data.ok_or(Error::Malformed("no data chunk"))?;
    let samples = decode(&format, data)?;
    Ok(Parsed { samples, clm })
}

fn parse_format(content: &[u8]) -> Result<Format, Error> {
    if content.len() < 16 {
        return Err(Error::Malformed("fmt chunk too short"));
    }
    let u16_at = |n: usize| u16::from_le_bytes([content[n], content[n + 1]]);
    let mut tag = u16_at(0);
    if tag == EXTENSIBLE {
        // The subformat GUID's first two bytes are the real format tag.
        if content.len() < 26 {
            return Err(Error::Malformed("extensible fmt chunk too short"));
        }
        tag = u16_at(24);
    }
    let format = Format {
        tag,
        channels: usize::from(u16_at(2)),
        bits: u16_at(14),
        align: usize::from(u16_at(12)),
    };
    if format.channels == 0 {
        return Err(Error::Malformed("no channels"));
    }
    Ok(format)
}

fn decode(format: &Format, data: &[u8]) -> Result<Vec<f32>, Error> {
    let width = usize::from(format.bits.div_ceil(8));
    let supported = matches!(
        (format.tag, format.bits),
        (PCM, 8 | 16 | 24 | 32) | (FLOAT, 32 | 64)
    );
    if !supported {
        return Err(Error::Unsupported {
            format: format.tag,
            bits: format.bits,
        });
    }
    let align = format.align.max(width * format.channels);
    let sample = |b: &[u8]| -> f32 {
        match (format.tag, format.bits) {
            (PCM, 8) => (f32::from(b[0]) - 128.0) / 128.0,
            (PCM, 16) => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0,
            (PCM, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            (PCM, _) => {
                (f64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])) / 2_147_483_648.0) as f32
            }
            (_, 32) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            _ => f64::from_le_bytes(b[..8].try_into().expect("eight bytes")) as f32,
        }
    };
    // The first channel of each sample frame.
    Ok(data
        .chunks_exact(align)
        .map(|f| sample(&f[..width]))
        .collect())
}
