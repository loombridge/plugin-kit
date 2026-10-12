//! The standalone app: any kit plug-in run as an application, with no host.
//!
//! A plug-in crate adds a binary whose `main` is one line,
//!
//! ```ignore
//! fn main() -> std::process::ExitCode {
//!     lspk_standalone::run::<my_plugin::MyPlugin>()
//! }
//! ```
//!
//! and `cargo xtask bundle` lays it out as an app on macOS and an
//! executable on Windows and Linux, beside the plug-in's other formats.
//!
//! # What the app does
//!
//! - **Audio:** it opens the system's default output (and, for an effect,
//!   the default input) and plays the plug-in's processor from the device's
//!   callback ([`engine`]), under the kit's real-time thread set-up, with no
//!   allocation, lock or blocking call on the audio thread. The person can
//!   choose the devices, the sample rate and the buffer size. It keeps
//!   playing through device changes: a device that goes away, changes its
//!   configuration or stops calling back is reopened, on the default device
//!   while the chosen one is missing and back on it when it returns
//!   ([`Standalone::poll`]).
//! - **MIDI:** every connected MIDI input plays the plug-in, and inputs
//!   plugged in later are picked up ([`midi`]); for an instrument, so does
//!   the computer keyboard ([`keyboard`]).
//! - **One window:** the plug-in's editor fills it, embedded as a host
//!   embeds one (the kit's generic editor for a plug-in without one), and
//!   follows its size; a thin strip along the bottom shows where the audio
//!   stands, the output's level and, for an instrument, the computer
//!   keyboard's switch, octave and velocity. The computer keyboard plays
//!   whenever the window has the focus, the editor's part included, unless
//!   a text field is taking the keys ([`keyboard`]).
//! - **Menus and settings:** the menu bar (the app's on macOS, the
//!   window's on Windows) opens and saves state files and opens the
//!   Settings dialog, which chooses the devices, the sample rate and the
//!   buffer size and lists the MIDI inputs. Linux has no menu bar without
//!   GTK, so there the strip has a Settings button and the dialog holds
//!   the state files.
//! - **Files:** it keeps its settings and the plug-in's state between runs
//!   ([`settings`]), and opens and saves the plug-in's state as a file, in
//!   the kit's state format, the same bytes a host saves.
//!
//! # Headless
//!
//! The app's audio loop runs the same on a [`FakeBackend`](fake::FakeBackend),
//! whose callback a test drives buffer by buffer, as on a real device: it's
//! how the app is tested without a sound card. From the command line,
//! `--run-for <seconds>` plays without a window and reports what it played
//! (`--note <key>` holds a note, for an instrument), and `--list-devices`
//! lists the audio devices.

pub mod audio;
pub mod engine;
pub mod fake;
pub mod keyboard;
pub mod midi;
pub mod settings;

mod app;
mod cpal_backend;
mod layout;
#[cfg_attr(
    not(any(target_os = "macos", windows)),
    allow(
        dead_code,
        reason = "Linux has no menu bar; only the tests read it there"
    )
)]
mod menu;
mod settings_window;
mod window;

pub use app::{Standalone, Status};
pub use cpal_backend::CpalBackend;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use lspk_plugin::Plugin;

const USAGE: &str = "\
Options:
  --list-devices       List the audio devices and quit
  --run-for <seconds>  Play without a window for this long, report what played, and quit
  --note <key>         With --run-for, hold this MIDI key (60 is middle C), for an instrument
  --help               Show this message
";

/// What the command line asked for.
#[derive(Debug, Default, PartialEq)]
struct Args {
    list_devices: bool,
    run_for: Option<f64>,
    note: Option<u8>,
    help: bool,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--list-devices" => parsed.list_devices = true,
            "--run-for" => {
                let seconds = value("--run-for")?;
                parsed.run_for = Some(
                    seconds
                        .parse()
                        .ok()
                        .filter(|s: &f64| s.is_finite() && *s > 0.0)
                        .ok_or_else(|| format!("`{seconds}` isn't a number of seconds"))?,
                );
            }
            "--note" => {
                let key = value("--note")?;
                parsed.note = Some(
                    key.parse()
                        .ok()
                        .filter(|k| *k < 128)
                        .ok_or_else(|| format!("`{key}` isn't a MIDI key from 0 to 127"))?,
                );
            }
            "-h" | "--help" => parsed.help = true,
            // macOS passes a process serial number to an app opened from
            // the Finder.
            other if other.starts_with("-psn_") => {}
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(parsed)
}

/// Runs plug-in `P` as an app: the window, or what the command line asks
/// for. Returns the process's exit code.
pub fn run<P: Plugin>() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let result = if args.help {
        print!("{} {}\n\n{USAGE}", P::INFO.name, P::INFO.version);
        Ok(())
    } else if args.list_devices {
        list_devices()
    } else if let Some(seconds) = args.run_for {
        run_headless::<P>(Duration::from_secs_f64(seconds), args.note)
    } else {
        window::run::<P>()
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn list_devices() -> Result<(), String> {
    use audio::AudioBackend as _;
    let backend = CpalBackend::new();
    println!("{}", backend.name());
    for device in backend.devices()? {
        let default = match (device.is_default_output, device.is_default_input) {
            (true, true) => " (default output and input)",
            (true, false) => " (default output)",
            (false, true) => " (default input)",
            (false, false) => "",
        };
        println!(
            "  {}{default}\n    id {}, {} out, {} in, rates {:?}",
            device.name, device.id, device.outputs, device.inputs, device.sample_rates
        );
    }
    Ok(())
}

/// Plays `P` through the default devices for `length` with the saved
/// settings, holding `note` if given, then reports what played. Saves
/// nothing.
fn run_headless<P: Plugin>(length: Duration, note: Option<u8>) -> Result<(), String> {
    let saved = settings::folder(P::INFO.id)
        .map(|folder| settings::Settings::load(&folder))
        .unwrap_or_default();
    let mut app = Standalone::<P>::new(Box::new(CpalBackend::new()), saved, None)
        .map_err(|e| e.to_string())?;
    app.start();
    let mut inputs = midi::MidiInputs::new(P::INFO.name, app.midi().clone());
    inputs.scan();
    if let Some(key) = note {
        app.midi().send([0x90, key, 100]);
    }
    let started = Instant::now();
    let mut peak = 0.0f32;
    while started.elapsed() < length {
        std::thread::sleep(Duration::from_millis(20));
        app.poll();
        peak = peak.max(app.meter().take());
    }
    if let Some(key) = note {
        app.midi().send([0x80, key, 0]);
    }
    println!("{} {}", P::INFO.name, P::INFO.version);
    match app.status() {
        Status::Running(info) => {
            println!(
                "  output: {} ({} channels){}",
                info.output_name,
                info.output_channels,
                if info.follows_default {
                    ", the default"
                } else {
                    ""
                }
            );
            if let Some(input) = &info.input_name {
                println!(
                    "  input: {input} ({} channel{})",
                    info.input_channels,
                    if info.input_channels == 1 { "" } else { "s" }
                );
            }
            println!(
                "  {} Hz, buffer {}, blocks of up to {} frames",
                info.sample_rate,
                info.buffer_size
                    .map_or_else(|| "the device's own".to_owned(), |b| format!("{b} frames")),
                info.max_frames
            );
        }
        Status::Waiting(why) => println!("  not playing: {why}"),
        Status::Stopped => println!("  stopped"),
    }
    let names = inputs.names();
    if !names.is_empty() {
        println!("  MIDI in: {}", names.join(", "));
    }
    println!("  callbacks: {}", app.callbacks());
    let db = if peak > 0.0 {
        format!("{:.1} dBFS", 20.0 * peak.log10())
    } else {
        "silence".to_owned()
    };
    println!("  output peak: {db}");
    let playing = matches!(app.status(), Status::Running(_));
    app.stop();
    if playing {
        Ok(())
    } else {
        Err("the audio didn't start".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse(list.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn the_command_line_is_read() {
        assert_eq!(args(&[]), Ok(Args::default()));
        assert_eq!(
            args(&["--run-for", "1.5", "--note", "69"]),
            Ok(Args {
                run_for: Some(1.5),
                note: Some(69),
                ..Args::default()
            })
        );
        assert!(args(&["--list-devices", "-psn_0_12345"]).is_ok_and(|a| a.list_devices));
        assert!(args(&["--run-for"]).is_err());
        assert!(args(&["--run-for", "-1"]).is_err());
        assert!(args(&["--note", "128"]).is_err());
        assert!(args(&["--loud"]).is_err());
    }
}
