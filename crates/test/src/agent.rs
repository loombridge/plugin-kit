//! The scripted MCP client for a plug-in's agent tests: `lspk-agent`'s own
//! client ([`Client`] over [`Stdio`], [`Http`] or [`InProcess`]), and what a
//! test needs around it.
//!
//! - [`in_process`]: a client already initialised against a server in this
//!   process.
//! - [`check_tools`]: every tool has a name, a description for the model
//!   and an object input schema.
//! - [`audio`]: the audio a tool returned (a render, a preview), decoded
//!   from its WAV attachment into an [`Output`], ready for [`crate::measure`]
//!   and [`crate::golden`]; [`encode_wav`] makes such an attachment, for a
//!   test program.
//!
//! ```ignore
//! let mut client = lspk_test::agent::in_process(server, host)?;
//! lspk_test::agent::check_tools(&client.tools()?)?;
//! let preview = client.call("render_note", json!({ "key": 69 }))?;
//! let output = lspk_test::agent::audio(&preview)?;
//! assert!((lspk_test::measure::pitch(output.channel(0), output.sample_rate).unwrap() - 440.0).abs() < 0.1);
//! ```

use std::io::Cursor;

use lspk_agent::Program;
use lspk_agent::Server;
pub use lspk_agent::client::{Client, Http, InProcess, RawResponse, Stdio, ToolResult, Transport};
use serde_json::Value;

use crate::render::Output;

/// The media type of the audio [`audio`] reads and [`encode_wav`] makes.
pub const WAV: &str = "audio/wav";

/// A client of `server`, in this process with `host`, initialised as the
/// client `lspk-test`. Questions for the person are declined; set
/// `client.transport().answer` to answer them.
///
/// # Errors
///
/// The server refused to initialise.
pub fn in_process<P: Program>(
    server: Server<P>,
    host: P::Host,
) -> Result<Client<InProcess<P>>, String> {
    let mut client = Client::new(InProcess::new(server, host));
    client.initialize("lspk-test")?;
    Ok(client)
}

/// Checks every tool in `tools` (as [`Client::tools`] lists them) has a
/// name, a description for the model, and an input schema that's a JSON
/// Schema object, and that no two share a name.
///
/// # Errors
///
/// Every tool that falls short, and how.
pub fn check_tools(tools: &[Value]) -> Result<(), String> {
    let mut problems = Vec::new();
    let mut names = Vec::new();
    for (index, tool) in tools.iter().enumerate() {
        let name = tool["name"].as_str().unwrap_or_default();
        let label = if name.is_empty() {
            format!("tool {index}")
        } else {
            format!("`{name}`")
        };
        if name.is_empty() {
            problems.push(format!("{label} has no name"));
        } else if names.contains(&name) {
            problems.push(format!("{label} is listed twice"));
        }
        names.push(name);
        if tool["description"]
            .as_str()
            .is_none_or(|d| d.trim().is_empty())
        {
            problems.push(format!("{label} has no description"));
        }
        let schema = &tool["inputSchema"];
        if !schema.is_object() || schema["type"] != "object" {
            problems.push(format!("{label}'s input schema isn't an object schema"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// The audio `result` carries as a WAV attachment.
///
/// # Errors
///
/// It has none, or it isn't a WAV file this can read.
pub fn audio(result: &ToolResult) -> Result<Output, String> {
    let bytes = result
        .attachment(WAV)
        .ok_or_else(|| format!("the result has no {WAV} attachment: {}", result.text))?;
    decode_wav(&bytes)
}

/// Reads a WAV file: 16-, 24- or 32-bit integer, or 32-bit float.
///
/// # Errors
///
/// It isn't a WAV file in one of those formats.
pub fn decode_wav(bytes: &[u8]) -> Result<Output, String> {
    let mut reader =
        hound::WavReader::new(Cursor::new(bytes)).map_err(|e| format!("not a WAV file: {e}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / f64::from(1u32 << (spec.bits_per_sample.clamp(1, 32) - 1));
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| (f64::from(s) * scale) as f32))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    let mut out = vec![Vec::with_capacity(interleaved.len() / channels); channels];
    for frame in interleaved.chunks_exact(channels) {
        for (channel, &sample) in out.iter_mut().zip(frame) {
            channel.push(sample);
        }
    }
    Ok(Output::new(f64::from(spec.sample_rate), out))
}

/// `output` as a 32-bit float WAV file, as a tool attaches audio.
///
/// # Errors
///
/// Its sample rate isn't a whole number, or it has no channels or more than
/// a WAV file holds.
pub fn encode_wav(output: &Output) -> Result<Vec<u8>, String> {
    if output.sample_rate.fract() != 0.0 || output.sample_rate < 1.0 {
        return Err(format!(
            "a WAV file's sample rate is a whole number, not {}",
            output.sample_rate
        ));
    }
    let channels = u16::try_from(output.channels.len())
        .ok()
        .filter(|c| *c > 0)
        .ok_or("a WAV file holds 1 to 65,535 channels")?;
    let spec = hound::WavSpec {
        channels,
        sample_rate: output.sample_rate as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut bytes, spec).map_err(|e| e.to_string())?;
    for sample in output.interleaved() {
        writer.write_sample(sample).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(bytes.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::sine;
    use lspk_agent::permission::Mode;
    use lspk_agent::{Call, Media, ServerInfo, ToolOutcome, tool_definition};
    use serde_json::json;

    /// A program with one tool, which renders a sine at the key it's given.
    struct Preview;

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct KeyArgs {
        /// The key, where 69 is A4.
        key: u8,
    }

    impl Program for Preview {
        type Host = ();
        type Event = ();
        type Deferred = ();

        fn info(&self) -> ServerInfo {
            ServerInfo {
                name: "preview".to_owned(),
                title: "Preview".to_owned(),
                version: "1.0.0".to_owned(),
                instructions: "Render a note with render_note.".to_owned(),
            }
        }

        fn tools(&self) -> Vec<Value> {
            vec![tool_definition::<KeyArgs>(
                "render_note",
                "Render note",
                "Renders a second of a sine at the key's pitch.",
                true,
            )]
        }

        fn call(
            &mut self,
            _call: Call<'_, ()>,
            name: &str,
            args: Value,
        ) -> Result<ToolOutcome<()>, String> {
            if name != "render_note" {
                return Err(format!("no tool is called `{name}`"));
            }
            let key = args["key"].as_f64().unwrap_or(69.0);
            let frequency = 440.0 * 2f64.powf((key - 69.0) / 12.0);
            let output = Output::new(48_000.0, vec![sine(frequency, 48_000.0, 48_000, 0.5)]);
            let wav = encode_wav(&output)?;
            Ok(ToolOutcome::WithMedia(
                json!({ "frequency": frequency }),
                vec![Media {
                    mime_type: WAV,
                    bytes: wav,
                    uri: None,
                }],
            ))
        }

        fn allows(&self, _: &(), _: &str) -> bool {
            true
        }

        fn approve(&mut self, _: &mut (), _: &str) -> bool {
            true
        }
    }

    #[test]
    fn a_tool_returns_audio_the_test_can_measure() {
        let server = Server::new(Mode::Headless { autonomous: false }, Preview);
        let mut client = in_process(server, ()).expect("a client");
        check_tools(&client.tools().expect("tools")).expect("well-described tools");
        let result = client
            .call("render_note", json!({ "key": 81 }))
            .expect("a call");
        let output = audio(&result).expect("audio");
        assert_eq!((output.sample_rate, output.channels.len()), (48_000.0, 1));
        let pitch = crate::measure::pitch(output.channel(0), output.sample_rate).expect("a pitch");
        assert!((pitch - 880.0).abs() < 0.01, "{pitch}");
    }

    #[test]
    fn badly_described_tools_are_reported() {
        let tools = [
            json!({ "name": "a", "description": "Does a.", "inputSchema": { "type": "object" } }),
            json!({ "name": "a", "description": " ", "inputSchema": {} }),
            json!({ "description": "Nameless." , "inputSchema": { "type": "object" } }),
        ];
        let error = check_tools(&tools).unwrap_err();
        assert!(error.contains("`a` is listed twice"), "{error}");
        assert!(error.contains("`a` has no description"), "{error}");
        assert!(error.contains("`a`'s input schema"), "{error}");
        assert!(error.contains("tool 2 has no name"), "{error}");
    }

    #[test]
    fn wav_round_trips_and_integer_wav_reads() {
        let output = Output::new(44_100.0, vec![vec![0.5, -0.25], vec![0.0, 1.0]]);
        assert_eq!(decode_wav(&encode_wav(&output).expect("a WAV")), Ok(output));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut bytes = Cursor::new(Vec::new());
        let mut writer = hound::WavWriter::new(&mut bytes, spec).expect("a writer");
        writer.write_sample(16_384i16).expect("a sample");
        writer.finalize().expect("finished");
        let read = decode_wav(&bytes.into_inner()).expect("a WAV");
        assert_eq!(read.channels, [vec![0.5]]);
        assert!(decode_wav(b"not audio").is_err());
        assert!(encode_wav(&Output::new(44_100.5, vec![vec![0.0]])).is_err());
    }
}
