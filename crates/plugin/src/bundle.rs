//! Lays out a built plug-in library as each format's plug-in file, as the
//! formats place them. One library holds every format (see the crate's
//! `vst3` and `auv2` features), so each bundle holds a copy of it:
//!
//! - **CLAP**: on macOS a `.clap` bundle holding the library and an
//!   `Info.plist`, elsewhere the library itself renamed `.clap`.
//! - **VST3**: a `.vst3` folder, with the library in `Contents/MacOS` on
//!   macOS (beside an `Info.plist`), `Contents/<arch>-win/<name>.vst3` on
//!   Windows and `Contents/<arch>-linux/<name>.so` on Linux, as Steinberg's
//!   module format describes.
//! - **Audio Unit v2** (macOS): a `.component` bundle whose `Info.plist`
//!   lists the Audio Unit's type, subtype and manufacturer codes
//!   ([`AuCodes`]) and its factory function.
//! - **The standalone app** (`lspk-standalone`), from the plug-in crate's
//!   own executable rather than its library: an `.app` bundle on macOS,
//!   elsewhere the executable itself ([`write_app`]).
//!
//! Signing is left to the caller (`cargo xtask bundle` signs macOS bundles
//! ad hoc, so hosts and `auval` load them).

use std::io;
use std::path::{Path, PathBuf};

/// What a bundle says about the plug-in inside, as its CLAP descriptor
/// gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BundleInfo {
    /// The plug-in's reverse-DNS ID, which is also each bundle's
    /// identifier.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Who makes it.
    pub vendor: String,
    /// Its version, such as `1.2.3`.
    pub version: String,
    /// Its CLAP features, such as `instrument` or `audio-effect`.
    pub features: Vec<String>,
}

impl BundleInfo {
    fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

/// An Audio Unit's type, subtype and manufacturer codes, which a host finds
/// it by (`auval -v <type> <subtype> <manufacturer>`), derived from the
/// plug-in's info so a plug-in needn't choose them:
///
/// - **Type**: `aumu` (an instrument) if its features include
///   `instrument`; otherwise `aumi` (a MIDI processor) if they include
///   `note-effect` but not `audio-effect`; otherwise `aufx` (an effect).
/// - **Manufacturer**: the first four letters and digits of its vendor, the
///   first upper case and the rest lower case, padded with `x`, such as
///   `Loom` for Loombridge. Apple reserves codes in lower case only, which
///   this never makes.
/// - **Subtype**: four letters and digits from a hash (32-bit FNV-1a) of
///   its ID, so it stays the same while the ID does and differs between
///   one vendor's plug-ins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuCodes {
    /// `aumu`, `aufx` or `aumi`.
    pub kind: [u8; 4],
    /// Unique to the plug-in among its vendor's.
    pub subtype: [u8; 4],
    /// The vendor's code.
    pub manufacturer: [u8; 4],
}

impl AuCodes {
    /// The codes for `info`, as the type's documentation describes.
    #[must_use]
    pub fn derive(info: &BundleInfo) -> Self {
        let kind = if info.has("instrument") {
            *b"aumu"
        } else if info.has("note-effect") && !info.has("audio-effect") {
            *b"aumi"
        } else {
            *b"aufx"
        };

        let mut manufacturer = *b"xxxx";
        let letters = info.vendor.bytes().filter(u8::is_ascii_alphanumeric);
        for (i, c) in letters.take(4).enumerate() {
            manufacturer[i] = c.to_ascii_lowercase();
        }
        manufacturer[0] = manufacturer[0].to_ascii_uppercase();

        const ALPHABET: &[u8; 62] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut hash = fnv1a(info.id.as_bytes());
        let mut subtype = [0; 4];
        for c in &mut subtype {
            *c = ALPHABET[(hash % 62) as usize];
            hash /= 62;
        }
        Self {
            kind,
            subtype,
            manufacturer,
        }
    }

    /// Each code as text, in the order `auval` takes them: type, subtype,
    /// manufacturer.
    #[must_use]
    pub fn as_strs(&self) -> [&str; 3] {
        fn text(code: &[u8; 4]) -> &str {
            std::str::from_utf8(code).unwrap_or("????")
        }
        [
            text(&self.kind),
            text(&self.subtype),
            text(&self.manufacturer),
        ]
    }
}

/// Writes `library` (a plug-in crate's built `cdylib`) into `dir` as
/// `<name>.clap`, replacing any older one, and returns its path. `id` is the
/// bundle's identifier on macOS, such as the plug-in's own ID.
///
/// # Errors
///
/// A file that couldn't be read or written.
pub fn write_clap(library: &Path, dir: &Path, name: &str, id: &str) -> io::Result<PathBuf> {
    let clap = dir.join(format!("{name}.clap"));
    remove(&clap)?;
    std::fs::create_dir_all(dir)?;
    if cfg!(target_os = "macos") {
        let info = BundleInfo {
            id: id.to_owned(),
            ..BundleInfo::default()
        };
        write_mac_bundle(library, &clap, name, &info, "", Package::Bundle)?;
    } else {
        std::fs::copy(library, &clap)?;
    }
    Ok(clap)
}

/// Writes `library` into `dir` as the VST3 bundle `<name>.vst3`, replacing
/// any older one, and returns its path.
///
/// # Errors
///
/// A file that couldn't be read or written.
pub fn write_vst3(
    library: &Path,
    dir: &Path,
    name: &str,
    info: &BundleInfo,
) -> io::Result<PathBuf> {
    let vst3 = dir.join(format!("{name}.vst3"));
    remove(&vst3)?;
    std::fs::create_dir_all(dir)?;
    if cfg!(target_os = "macos") {
        write_mac_bundle(library, &vst3, name, info, "", Package::Bundle)?;
    } else {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            "x86" => "x86",
            _ => "x86_64",
        };
        let (folder, file) = if cfg!(windows) {
            let arch = if arch == "aarch64" { "arm64" } else { arch };
            (format!("{arch}-win"), format!("{name}.vst3"))
        } else {
            (format!("{arch}-linux"), format!("{name}.so"))
        };
        let binaries = vst3.join("Contents").join(folder);
        std::fs::create_dir_all(&binaries)?;
        std::fs::copy(library, binaries.join(file))?;
    }
    Ok(vst3)
}

/// Writes `library` into `dir` as the Audio Unit bundle
/// `<name>.component`, its `Info.plist` listing the Audio Unit with the
/// codes [`AuCodes::derive`] gives, and returns its path. Audio Units are
/// macOS's alone; elsewhere this writes the same layout, which nothing
/// loads.
///
/// # Errors
///
/// A file that couldn't be read or written.
pub fn write_auv2(
    library: &Path,
    dir: &Path,
    name: &str,
    info: &BundleInfo,
) -> io::Result<PathBuf> {
    let component = dir.join(format!("{name}.component"));
    remove(&component)?;
    std::fs::create_dir_all(dir)?;
    let codes = AuCodes::derive(info);
    let [kind, subtype, manufacturer] = codes.as_strs();
    let tag = match &codes.kind {
        b"aumu" => "Synthesizer",
        b"aumi" => "MIDI",
        _ => "Effects",
    };
    let vendor = if info.vendor.is_empty() {
        manufacturer
    } else {
        &info.vendor
    };
    let entry = format!(
        "\t<key>AudioComponents</key>\n\
         \t<array>\n\
         \t\t<dict>\n\
         \t\t\t<key>type</key><string>{kind}</string>\n\
         \t\t\t<key>subtype</key><string>{subtype}</string>\n\
         \t\t\t<key>manufacturer</key><string>{manufacturer}</string>\n\
         \t\t\t<key>name</key><string>{vendor}: {name}</string>\n\
         \t\t\t<key>description</key><string>{name}</string>\n\
         \t\t\t<key>version</key><integer>{version}</integer>\n\
         \t\t\t<key>factoryFunction</key><string>GetPluginFactoryAUV2</string>\n\
         \t\t\t<key>sandboxSafe</key><true/>\n\
         \t\t\t<key>tags</key><array><string>{tag}</string></array>\n\
         \t\t</dict>\n\
         \t</array>\n",
        vendor = escape(vendor),
        name = escape(&info.name),
        version = au_version(&info.version),
    );
    write_mac_bundle(library, &component, name, info, &entry, Package::Bundle)?;
    Ok(component)
}

/// Writes `executable` (a plug-in crate's standalone app, built from its
/// `<crate>-standalone` binary) into `dir` as the app `<name>`, replacing
/// any older one, and returns its path: on macOS the bundle `<name>.app`,
/// identified as the plug-in's ID followed by `.standalone`, named for the
/// plug-in, and saying why it asks for the microphone (an effect's input);
/// elsewhere the executable itself, as `<name>.exe` on Windows and `<name>`
/// on Linux.
///
/// # Errors
///
/// A file that couldn't be read or written.
pub fn write_app(
    executable: &Path,
    dir: &Path,
    name: &str,
    info: &BundleInfo,
) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    if !cfg!(target_os = "macos") {
        let app = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        remove(&app)?;
        std::fs::copy(executable, &app)?;
        return Ok(app);
    }
    let app = dir.join(format!("{name}.app"));
    remove(&app)?;
    let shown = if info.name.is_empty() {
        name
    } else {
        &info.name
    };
    let extra = format!(
        "\t<key>CFBundleDisplayName</key><string>{shown}</string>\n\
         \t<key>NSHighResolutionCapable</key><true/>\n\
         \t<key>NSMicrophoneUsageDescription</key>\
         <string>{shown} processes the audio input.</string>\n",
        shown = escape(shown),
    );
    let info = BundleInfo {
        id: format!("{}.standalone", info.id),
        ..info.clone()
    };
    write_mac_bundle(executable, &app, name, &info, &extra, Package::App)?;
    Ok(app)
}

/// What kind of macOS bundle a bundle is.
#[derive(Clone, Copy)]
enum Package {
    /// A loadable bundle: a plug-in.
    Bundle,
    /// An application.
    App,
}

impl Package {
    fn code(self) -> &'static str {
        match self {
            Self::Bundle => "BNDL",
            Self::App => "APPL",
        }
    }
}

/// Writes a macOS bundle at `bundle`: `library` as its executable `name`,
/// an `Info.plist` with `extra` keys, and a `PkgInfo`.
fn write_mac_bundle(
    library: &Path,
    bundle: &Path,
    name: &str,
    info: &BundleInfo,
    extra: &str,
    package: Package,
) -> io::Result<()> {
    let contents = bundle.join("Contents");
    let binaries = contents.join("MacOS");
    std::fs::create_dir_all(&binaries)?;
    std::fs::copy(library, binaries.join(name))?;
    std::fs::write(
        contents.join("Info.plist"),
        info_plist(name, info, extra, package),
    )?;
    std::fs::write(contents.join("PkgInfo"), format!("{}????", package.code()))?;
    Ok(())
}

/// Removes an older bundle or file at `path`, if there is one.
fn remove(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else if path.exists() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

/// A bundle's `Info.plist` naming its executable `name`, with `extra` keys.
fn info_plist(name: &str, info: &BundleInfo, extra: &str, package: Package) -> String {
    let version = if info.version.is_empty() {
        "0.0.0"
    } else {
        &info.version
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
	<key>CFBundlePackageType</key><string>{package}</string>
	<key>CFBundleShortVersionString</key><string>{version}</string>
	<key>CFBundleVersion</key><string>{version}</string>
	<key>CFBundleSignature</key><string>????</string>
{extra}</dict>
</plist>
"#,
        name = escape(name),
        id = escape(&info.id),
        version = escape(version),
        package = package.code(),
    )
}

/// `text` with the characters XML reserves escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A version such as `1.2.3` as an Audio Unit's version number,
/// `0xMMMMmmpp`; parts that aren't numbers count as 0.
fn au_version(version: &str) -> u32 {
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0).min(0xffff);
    let minor = parts.next().unwrap_or(0).min(0xff);
    let patch = parts.next().unwrap_or(0).min(0xff);
    (major << 16) | (minor << 8) | patch
}

/// The 32-bit FNV-1a hash of `bytes`.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, &b| {
        (hash ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(features: &[&str]) -> BundleInfo {
        BundleInfo {
            id: "com.example.test".into(),
            name: "Test".into(),
            vendor: "Example Audio".into(),
            version: "1.2.3".into(),
            features: features.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    /// A folder of this test's own, empty, with a fake library in it.
    fn scratch(test: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("lspk-bundle-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a folder");
        let library = dir.join("input.lib");
        std::fs::write(&library, b"library").expect("a library");
        (dir, library)
    }

    #[test]
    fn a_library_becomes_a_clap_file() {
        let (dir, library) = scratch("clap");
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

    #[test]
    fn a_library_becomes_a_vst3_bundle_in_the_platforms_layout() {
        let (dir, library) = scratch("vst3");
        let out = dir.join("out");
        write_vst3(&library, &out, "Test", &info(&["audio-effect"])).expect("a bundle");
        let vst3 = write_vst3(&library, &out, "Test", &info(&["audio-effect"])).expect("a bundle");
        assert_eq!(vst3, out.join("Test.vst3"));
        let binary = if cfg!(target_os = "macos") {
            vst3.join("Contents/MacOS/Test")
        } else {
            let folder = std::fs::read_dir(vst3.join("Contents"))
                .expect("Contents")
                .next()
                .expect("one folder")
                .expect("an entry")
                .path();
            let name = folder.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if cfg!(windows) {
                assert!(name.ends_with("-win"), "{name}");
                folder.join("Test.vst3")
            } else {
                assert!(name.ends_with("-linux"), "{name}");
                folder.join("Test.so")
            }
        };
        assert_eq!(std::fs::read(binary).expect("the library"), b"library");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_audio_unit_bundle_lists_its_codes_and_factory() {
        let (dir, library) = scratch("auv2");
        let component = write_auv2(
            &library,
            &dir,
            "Test",
            &info(&["instrument", "synthesizer"]),
        )
        .expect("a bundle");
        assert_eq!(component, dir.join("Test.component"));
        let plist =
            std::fs::read_to_string(component.join("Contents/Info.plist")).expect("a plist");
        let codes = AuCodes::derive(&info(&["instrument"]));
        let [kind, subtype, manufacturer] = codes.as_strs();
        for expected in [
            format!("<key>type</key><string>{kind}</string>"),
            format!("<key>subtype</key><string>{subtype}</string>"),
            format!("<key>manufacturer</key><string>{manufacturer}</string>"),
            "<string>Example Audio: Test</string>".to_owned(),
            "<key>factoryFunction</key><string>GetPluginFactoryAUV2</string>".to_owned(),
            format!("<integer>{}</integer>", 0x0001_0203),
            "<string>com.example.test</string>".to_owned(),
        ] {
            assert!(plist.contains(&expected), "no {expected} in\n{plist}");
        }
        assert_eq!(
            std::fs::read(component.join("Contents/MacOS/Test")).expect("the library"),
            b"library"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_executable_becomes_an_app() {
        let (dir, executable) = scratch("app");
        let out = dir.join("out");
        write_app(&executable, &out, "Test", &info(&["instrument"])).expect("an app");
        let app = write_app(&executable, &out, "Test", &info(&["instrument"])).expect("an app");
        let binary = if cfg!(target_os = "macos") {
            assert_eq!(app, out.join("Test.app"));
            let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).expect("a plist");
            for expected in [
                "<key>CFBundlePackageType</key><string>APPL</string>",
                "<string>com.example.test.standalone</string>",
                "<key>CFBundleDisplayName</key><string>Test</string>",
                "NSMicrophoneUsageDescription",
            ] {
                assert!(plist.contains(expected), "no {expected} in\n{plist}");
            }
            assert_eq!(
                std::fs::read_to_string(app.join("Contents/PkgInfo")).expect("PkgInfo"),
                "APPL????"
            );
            app.join("Contents/MacOS/Test")
        } else {
            assert_eq!(
                app,
                out.join(format!("Test{}", std::env::consts::EXE_SUFFIX))
            );
            app
        };
        assert_eq!(std::fs::read(binary).expect("the executable"), b"library");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_unit_codes_follow_the_plugins_info() {
        let instrument = AuCodes::derive(&info(&["instrument", "synthesizer"]));
        assert_eq!(&instrument.kind, b"aumu");
        assert_eq!(&instrument.manufacturer, b"Exam");
        assert_eq!(&AuCodes::derive(&info(&["audio-effect"])).kind, b"aufx");
        assert_eq!(&AuCodes::derive(&info(&["note-effect"])).kind, b"aumi");
        assert_eq!(
            &AuCodes::derive(&info(&["note-effect", "audio-effect"])).kind,
            b"aufx"
        );
        assert_eq!(&AuCodes::derive(&info(&[])).kind, b"aufx");

        // The subtype comes from the ID alone, in letters and digits.
        let mut other = info(&["instrument"]);
        assert_eq!(AuCodes::derive(&other).subtype, instrument.subtype);
        other.id = "com.example.other".into();
        assert_ne!(AuCodes::derive(&other).subtype, instrument.subtype);
        assert!(instrument.subtype.iter().all(u8::is_ascii_alphanumeric));

        // Short or empty vendors are padded, never all lower case.
        other.vendor = "a-b".into();
        assert_eq!(&AuCodes::derive(&other).manufacturer, b"Abxx");
        other.vendor = String::new();
        assert_eq!(&AuCodes::derive(&other).manufacturer, b"Xxxx");
    }

    #[test]
    fn versions_become_audio_unit_version_numbers() {
        assert_eq!(au_version("1.2.3"), 0x0001_0203);
        assert_eq!(au_version("0.0.0"), 0);
        assert_eq!(au_version("2"), 0x0002_0000);
        assert_eq!(au_version("1.2.3-beta.1"), 0x0001_0203);
        assert_eq!(au_version("x"), 0);
    }
}
