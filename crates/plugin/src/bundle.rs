//! Lays out a built plug-in library as a CLAP plug-in file, as the CLAP
//! specification places them: on macOS a `.clap` bundle holding the library
//! and an `Info.plist`, elsewhere the library itself renamed `.clap`.

use std::io;
use std::path::{Path, PathBuf};

/// Writes `library` (a plug-in crate's built `cdylib`) into `dir` as
/// `<name>.clap`, replacing any older one, and returns its path. `id` is the
/// bundle's identifier on macOS, such as the plug-in's own ID.
///
/// # Errors
///
/// A file that couldn't be read or written.
pub fn write_clap(library: &Path, dir: &Path, name: &str, id: &str) -> io::Result<PathBuf> {
    let clap = dir.join(format!("{name}.clap"));
    if clap.is_dir() {
        std::fs::remove_dir_all(&clap)?;
    } else if clap.exists() {
        std::fs::remove_file(&clap)?;
    }
    std::fs::create_dir_all(dir)?;
    if cfg!(target_os = "macos") {
        let contents = clap.join("Contents");
        let binaries = contents.join("MacOS");
        std::fs::create_dir_all(&binaries)?;
        std::fs::copy(library, binaries.join(name))?;
        std::fs::write(contents.join("Info.plist"), info_plist(name, id))?;
    } else {
        std::fs::copy(library, &clap)?;
    }
    Ok(clap)
}

/// A bundle's `Info.plist` naming its executable `name`.
fn info_plist(name: &str, id: &str) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key><string>English</string>
	<key>CFBundleExecutable</key><string>{name}</string>
	<key>CFBundleIdentifier</key><string>{id}</string>
	<key>CFBundleName</key><string>{name}</string>
	<key>CFBundlePackageType</key><string>BNDL</string>
	<key>CFBundleSignature</key><string>????</string>
</dict>
</plist>
"#,
        name = escape(name),
        id = escape(id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_becomes_a_clap_file() {
        let dir = std::env::temp_dir().join(format!("lspk-bundle-test-{}", std::process::id()));
        let library = dir.join("input.lib");
        std::fs::create_dir_all(&dir).expect("a folder");
        std::fs::write(&library, b"library").expect("a library");
        let out = dir.join("out");
        // Twice: the second replaces the first.
        write_clap(&library, &out, "Test", "com.example.test").expect("a bundle");
        let clap = write_clap(&library, &out, "Test", "com.example.test").expect("a bundle");
        assert_eq!(clap, out.join("Test.clap"));
        let binary = if cfg!(target_os = "macos") {
            let plist = std::fs::read_to_string(clap.join("Contents/Info.plist")).expect("a plist");
            assert!(plist.contains("<string>com.example.test</string>"));
            clap.join("Contents/MacOS/Test")
        } else {
            clap
        };
        assert_eq!(std::fs::read(binary).expect("the library"), b"library");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
