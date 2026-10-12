//! The platform's own interface font, found at run time.
//!
//! The kit bundles no typeface: egui's own fonts are under licences the
//! kit's dependency policy rejects (the OFL and the Ubuntu Font Licence),
//! so an editor draws with a font the platform already has, read once per
//! process and shared by every instance.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Whether `bytes` start like a TrueType or OpenType font, or a collection
/// of them, so egui won't refuse them.
#[must_use]
pub fn is_font(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0x00, 0x01, 0x00, 0x00] | b"true" | b"OTTO" | b"ttcf")
    )
}

/// The platform's interface font, read the first time it's asked for;
/// `None` if none of the places the kit looks has one.
#[must_use]
pub fn system_ui_font() -> Option<Arc<[u8]>> {
    static FONT: OnceLock<Option<Arc<[u8]>>> = OnceLock::new();
    FONT.get_or_init(|| {
        candidates()
            .into_iter()
            .filter_map(|path| std::fs::read(path).ok())
            .find(|bytes| is_font(bytes))
            .map(Arc::from)
    })
    .clone()
}

/// Where the platform's interface font may be, best first.
fn candidates() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        [
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/Library/Fonts/Arial Unicode.ttf",
        ]
        .map(PathBuf::from)
        .into()
    } else if cfg!(target_os = "windows") {
        let windows = std::env::var_os("SystemRoot")
            .or_else(|| std::env::var_os("WINDIR"))
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        ["segoeui.ttf", "tahoma.ttf", "arial.ttf"]
            .map(|name| windows.join("Fonts").join(name))
            .into()
    } else {
        let mut paths: Vec<PathBuf> = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/ubuntu/Ubuntu-R.ttf",
        ]
        .map(PathBuf::from)
        .into();
        // Failing those, any regular sans-serif in the usual folders.
        for root in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            find_sans(Path::new(root), 3, &mut paths);
        }
        paths
    }
}

/// Adds every regular sans-serif TrueType font under `dir`, down to
/// `depth` folders, to `found`.
fn find_sans(dir: &Path, depth: u32, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                find_sans(&path, depth - 1, found);
            }
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let plain = !["Bold", "Italic", "Oblique", "Mono", "Condensed", "Light"]
            .iter()
            .any(|style| name.contains(style));
        if name.ends_with(".ttf") && name.contains("Sans") && plain {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_are_recognised_by_their_first_bytes() {
        assert!(is_font(&[0, 1, 0, 0, 9]));
        assert!(is_font(b"OTTO...."));
        assert!(is_font(b"ttcf...."));
        assert!(!is_font(b"<html>"));
        assert!(!is_font(&[0, 1]));
    }

    #[test]
    fn every_platform_has_places_to_look() {
        assert!(!candidates().is_empty());
        // Where a font is found, it's one egui will read.
        if let Some(font) = system_ui_font() {
            assert!(is_font(&font));
            let mut definitions = egui::FontDefinitions::empty();
            definitions.font_data.insert(
                "ui".into(),
                Arc::new(egui::FontData::from_owned(font.to_vec())),
            );
            definitions
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("ui".into());
            let ctx = egui::Context::default();
            ctx.set_fonts(definitions);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.label("Gain −∞ dB");
            });
            output.textures_delta.clear();
        }
    }
}
