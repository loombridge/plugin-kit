//! What the app keeps between runs, in a folder of its own per plug-in:
//! `settings.json`, the audio and keyboard settings, and `state.lspk`, the
//! plug-in's state as it was when the app last quit, in the kit's state
//! format (the same bytes a host saves).
//!
//! The folder is the platform's place for an app's own files, named by the
//! plug-in's ID: `~/Library/Application Support/<id>` on macOS,
//! `%APPDATA%\<id>` on Windows, and `$XDG_CONFIG_HOME/<id>` (or
//! `~/.config/<id>`) on Linux. `LSPK_STANDALONE_DIR` overrides it.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The settings file's name.
pub const SETTINGS_FILE: &str = "settings.json";
/// The saved state's name.
pub const STATE_FILE: &str = "state.lspk";
/// The extension of state files the person saves and opens.
pub const STATE_EXTENSION: &str = "lspk";

/// The audio and keyboard settings. `None` leaves a choice to the system
/// or the device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The output device's ID; `None` follows the system's default.
    pub output: Option<String>,
    /// The input device's ID, for an effect; `None` for the default.
    pub input: Option<String>,
    /// The sample rate; `None` for the device's own.
    pub sample_rate: Option<u32>,
    /// The device buffer in frames; `None` for the device's own.
    pub buffer_size: Option<u32>,
    /// The computer keyboard's octave shift.
    pub keyboard_octave: i8,
    /// The computer keyboard's velocity; 0 for the default.
    pub keyboard_velocity: u8,
}

/// The app's folder for the plug-in with ID `id`, or `None` if the
/// platform gives no home for it.
#[must_use]
pub fn folder(id: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LSPK_STANDALONE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library/Application Support"))
    } else if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|home| home.join(".config")))
    }?;
    Some(base.join(id))
}

impl Settings {
    /// The settings saved in `folder`, or the defaults if there are none or
    /// they can't be read.
    #[must_use]
    pub fn load(folder: &Path) -> Self {
        std::fs::read(folder.join(SETTINGS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Saves the settings in `folder`, making it if need be.
    ///
    /// # Errors
    ///
    /// The file couldn't be written.
    pub fn save(&self, folder: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_atomically(&folder.join(SETTINGS_FILE), &json)
    }
}

/// Writes `bytes` to `path` through a temporary file beside it, so a crash
/// never leaves half a file; makes the folder if need be.
///
/// # Errors
///
/// The file couldn't be written.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_missing_ones_take_defaults() {
        let dir = std::env::temp_dir().join(format!("lspk-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&dir), Settings::default());
        let settings = Settings {
            output: Some("fake:out".into()),
            sample_rate: Some(96_000),
            keyboard_octave: -1,
            ..Settings::default()
        };
        settings.save(&dir).expect("saved");
        assert_eq!(Settings::load(&dir), settings);
        std::fs::write(dir.join(SETTINGS_FILE), br#"{"buffer_size": 256}"#).expect("written");
        assert_eq!(Settings::load(&dir).buffer_size, Some(256));
        std::fs::write(dir.join(SETTINGS_FILE), b"not json").expect("written");
        assert_eq!(Settings::load(&dir), Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
