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

/// A typeface made by code, the same on every platform: each printable
/// ASCII character is a solid bar as wide as a letter, its height one of
/// eight by its code, so text has real widths and a different shape for
/// different words, and no font file is needed. For snapshot tests and
/// benchmarks, whose pictures and costs mustn't depend on the platform's
/// font; `FontSource::Bytes` hands it to a theme.
#[must_use]
pub fn block_font() -> Arc<[u8]> {
    const FIRST: u16 = 0x20;
    const LAST: u16 = 0x7E;
    // Glyph 0 is the box drawn for any other character; glyph `n` is the
    // character `FIRST + n - 1`; glyph 1, the space, has no outline.
    const GLYPHS: u16 = LAST - FIRST + 2;
    const ADVANCE: u16 = 600;
    const LEFT: i16 = 80;
    const RIGHT: i16 = 520;
    const TOP: i16 = 1000;

    let mut glyf = Bytes::default();
    let mut loca = Bytes::default();
    let mut hmtx = Bytes::default();
    for glyph in 0..GLYPHS {
        loca.u32(glyf.0.len() as u32);
        let top = match glyph {
            0 => 700,
            1 => {
                hmtx.u16(ADVANCE).i16(0);
                continue;
            }
            n => 300 + (n % 8) as i16 * 60,
        };
        hmtx.u16(ADVANCE).i16(LEFT);
        glyf.i16(1).i16(LEFT).i16(0).i16(RIGHT).i16(top);
        // One contour of four on-curve points, clockwise, as 16-bit deltas.
        glyf.u16(3).u16(0).0.extend([1u8; 4]);
        let corners = [(LEFT, 0), (LEFT, top), (RIGHT, top), (RIGHT, 0)];
        let (mut x, mut y) = (0, 0);
        for (to, _) in corners {
            glyf.i16(to - x);
            x = to;
        }
        for (_, to) in corners {
            glyf.i16(to - y);
            y = to;
        }
        glyf.pad4();
    }
    loca.u32(glyf.0.len() as u32);

    // Characters FIRST..=LAST are glyphs 1.., in one segment, and 0xFFFF
    // ends the table.
    let mut cmap = Bytes::default();
    cmap.u16(0).u16(1).u16(3).u16(1).u32(12);
    cmap.u16(4).u16(32).u16(0).u16(4).u16(4).u16(1).u16(0);
    cmap.u16(LAST).u16(0xFFFF).u16(0);
    cmap.u16(FIRST).u16(0xFFFF);
    cmap.u16(1u16.wrapping_sub(FIRST)).u16(1);
    cmap.u16(0).u16(0);

    let mut head = Bytes::default();
    head.u32(0x0001_0000)
        .u32(0x0001_0000)
        .u32(0)
        .u32(0x5F0F_3CF5);
    head.u16(0b1011).u16(1000).u32(0).u32(0).u32(0).u32(0);
    head.i16(0).i16(0).i16(ADVANCE as i16).i16(TOP);
    head.u16(0).u16(8).i16(2).i16(1).i16(0);

    let mut hhea = Bytes::default();
    hhea.u32(0x0001_0000).i16(800).i16(-200).i16(0).u16(ADVANCE);
    hhea.i16(0).i16(0).i16(RIGHT).i16(1).i16(0).i16(0);
    hhea.u32(0).u32(0).i16(0).u16(GLYPHS);

    let mut maxp = Bytes::default();
    maxp.u32(0x0001_0000)
        .u16(GLYPHS)
        .u16(4)
        .u16(1)
        .u16(0)
        .u16(0)
        .u16(2);
    maxp.u32(0).u32(0).u32(0).u32(0).u16(0).u16(0);

    // The tables, by tag as the directory wants them.
    let tables: [(&[u8; 4], Vec<u8>); 7] = [
        (b"cmap", cmap.0),
        (b"glyf", glyf.0),
        (b"head", head.0),
        (b"hhea", hhea.0),
        (b"hmtx", hmtx.0),
        (b"loca", loca.0),
        (b"maxp", maxp.0),
    ];
    let mut font = Bytes::default();
    font.u32(0x0001_0000).u16(7).u16(64).u16(2).u16(48);
    let mut offset = 12 + 16 * tables.len();
    for (tag, table) in &tables {
        font.0.extend(*tag);
        let checksum = (table.chunks(4)).fold(0u32, |sum, word| {
            let mut padded = [0u8; 4];
            padded[..word.len()].copy_from_slice(word);
            sum.wrapping_add(u32::from_be_bytes(padded))
        });
        font.u32(checksum)
            .u32(offset as u32)
            .u32(table.len() as u32);
        offset += table.len().next_multiple_of(4);
    }
    for (_, table) in tables {
        font.0.extend(table);
        font.pad4();
    }
    font.0.into()
}

/// Big-endian bytes, as font tables are written.
#[derive(Default)]
struct Bytes(Vec<u8>);

impl Bytes {
    fn u16(&mut self, value: u16) -> &mut Self {
        self.0.extend(value.to_be_bytes());
        self
    }

    fn i16(&mut self, value: i16) -> &mut Self {
        self.0.extend(value.to_be_bytes());
        self
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend(value.to_be_bytes());
        self
    }

    fn pad4(&mut self) {
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
    }
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
    fn the_block_font_is_a_font_egui_draws_with_the_same_widths_everywhere() {
        let font = block_font();
        assert!(is_font(&font));
        let mut definitions = egui::FontDefinitions::empty();
        definitions.font_data.insert(
            "block".into(),
            Arc::new(egui::FontData::from_owned(font.to_vec())),
        );
        definitions
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("block".into());
        let ctx = egui::Context::default();
        ctx.set_fonts(definitions);
        let mut width = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(
                "Gain 12".to_owned(),
                egui::FontId::proportional(10.0),
                egui::Color32::WHITE,
            );
            width = galley.size().x;
        });
        output.textures_delta.clear();
        // Seven characters, each 0.6 em wide.
        assert!((width - 7.0 * 6.0).abs() < 0.5, "{width} points wide");
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
