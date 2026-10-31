//! Snapshot tests for editors: an interface drawn off screen
//! ([`Editor::capture`](lspk_plugin::Editor::capture)) compared with a
//! reference picture kept in the repository.
//!
//! A [`Suite`] holds a folder of references and the images to compare with
//! them ([`Suite::add`]); [`Suite::assert`] compares each with
//! `<name>.png`:
//!
//! - The size must be the same.
//! - A pixel differs when any of its channels is more than [`CHANNEL`]
//!   apart. Platforms' maths libraries round layout and anti-aliasing a
//!   little differently, so a few edge pixels may; at most [`SHARE`] of the
//!   pixels may differ, and a failure names how many did and where the
//!   worst one is.
//!
//! The drawing is software, with no window and no GPU, so a suite runs
//! headless in CI on every platform. Text is where platforms differ most
//! (each has its own interface font), so a suite that keeps one reference
//! for every platform draws with a font of its own making rather than the
//! platform's: the theme's `FontSource::Bytes`.
//!
//! # Changing a reference
//!
//! References change only when someone runs `cargo xtask golden --update`
//! (the same task and [`UPDATE_ENV`](crate::golden::UPDATE_ENV) as the
//! audio references), which remakes those whose image no longer matches
//! (or only those named); the pull request that commits one says why. A
//! suite lives in a crate's `tests/golden.rs`, beside `tests/golden/`.
//!
//! An agent gets the same picture from the plug-in's agent interface (the
//! `ui.capture` command).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use lspk_plugin::{EditorContext, EditorImage, Plugin};

use crate::golden::{Selection, UPDATE_ENV};
use crate::render::Instance;

/// How far apart, in one channel, two pixels may be and still match.
pub const CHANNEL: u8 = 16;

/// The share of pixels that may differ.
pub const SHARE: f64 = 0.002;

/// The image `P`'s editor draws for `instance`, as its values are now, at
/// `scale` pixels to a point.
///
/// # Errors
///
/// The plug-in has no editor, its editor can't draw off screen, or its
/// parameters aren't described yet.
pub fn capture<P: Plugin>(instance: &mut Instance<P>, scale: f32) -> Result<EditorImage, String> {
    let values = instance
        .host()
        .values_shared()
        .ok_or("the plug-in's parameters aren't described yet")?;
    let editor = instance
        .plugin_mut()
        .editor()
        .ok_or("the plug-in has no editor")?;
    editor
        .capture(&EditorContext::detached(values), scale)
        .map_err(|e| e.to_string())
}

/// `image` as a PNG file.
///
/// # Errors
///
/// The image's bytes don't fit its size.
pub fn encode_png(image: &EditorImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&image.rgba)
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// The image in PNG file `bytes`.
///
/// # Errors
///
/// It isn't an 8-bit RGBA PNG.
pub fn decode_png(bytes: &[u8]) -> Result<EditorImage, String> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .map_err(|e| e.to_string())?;
    let mut rgba = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("the image is too large")?
    ];
    let info = reader.next_frame(&mut rgba).map_err(|e| e.to_string())?;
    if (info.color_type, info.bit_depth) != (png::ColorType::Rgba, png::BitDepth::Eight) {
        return Err("a reference is an 8-bit RGBA PNG".to_owned());
    }
    rgba.truncate(info.buffer_size());
    Ok(EditorImage {
        width: info.width,
        height: info.height,
        rgba,
    })
}

/// How two images of one size differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Difference {
    /// How many pixels differ by more than [`CHANNEL`] in some channel.
    pub differing: usize,
    /// That as a share of all the pixels.
    pub share: f64,
    /// Where the pixel that differs most is, as `(x, y)`, and by how much.
    pub worst: Option<((u32, u32), u8)>,
}

/// How `image` differs from `reference`.
///
/// # Panics
///
/// If the images differ in size.
#[must_use]
pub fn compare(reference: &EditorImage, image: &EditorImage) -> Difference {
    assert_eq!(
        (reference.width, reference.height),
        (image.width, image.height),
        "only images of one size compare"
    );
    let mut differing = 0;
    let mut worst: Option<((u32, u32), u8)> = None;
    let pixels = reference
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(image.rgba.as_chunks::<4>().0);
    for (at, (a, b)) in pixels.enumerate() {
        let apart = a
            .iter()
            .zip(b)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        if apart > CHANNEL {
            differing += 1;
        }
        if apart > worst.map_or(0, |(_, most)| most) {
            let at = at as u32;
            worst = Some(((at % reference.width, at / reference.width), apart));
        }
    }
    let total = (reference.width as usize * reference.height as usize).max(1);
    Difference {
        differing,
        share: differing as f64 / total as f64,
        worst,
    }
}

/// A folder of references and the images to compare with them.
#[derive(Debug)]
pub struct Suite {
    dir: PathBuf,
    images: Vec<(String, EditorImage)>,
}

/// What a suite found for one image.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    /// The image's name.
    pub name: String,
    /// Why it failed, if it did.
    pub problem: Option<String>,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.problem {
            None => write!(f, "{}: matches its reference", self.name),
            Some(problem) => write!(f, "{}: {problem}", self.name),
        }
    }
}

impl Suite {
    /// The references in `dir`.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            images: Vec::new(),
        }
    }

    /// Adds the image `name`, to compare with `<name>.png`.
    ///
    /// # Panics
    ///
    /// If the suite already has an image `name`, or `name` isn't
    /// lower-case letters, digits and hyphens.
    pub fn add(&mut self, name: &str, image: EditorImage) -> &mut Self {
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "a snapshot's name is lower-case letters, digits and hyphens, not {name:?}"
        );
        assert!(
            self.images.iter().all(|(n, _)| n != name),
            "two snapshots are called {name}"
        );
        self.images.push((name.to_owned(), image));
        self
    }

    /// Remakes the references [`UPDATE_ENV`] asks for, if it's set, then
    /// compares every image with its reference.
    ///
    /// # Errors
    ///
    /// A reference couldn't be written.
    pub fn run(&self) -> Result<Vec<Check>, String> {
        if let Ok(value) = std::env::var(UPDATE_ENV) {
            self.update(&Selection::parse(&value))?;
        }
        Ok(self
            .images
            .iter()
            .map(|(name, image)| Check {
                name: name.clone(),
                problem: self.problem(name, image),
            })
            .collect())
    }

    /// [`run`](Self::run)s the suite and panics, listing every failure, if
    /// an image doesn't match its reference.
    ///
    /// # Panics
    ///
    /// If one doesn't, or a file couldn't be written.
    pub fn assert(&self) {
        let checks = self.run().unwrap_or_else(|e| panic!("{e}"));
        let failed: Vec<String> = checks
            .iter()
            .inspect(|check| println!("{check}"))
            .filter(|check| check.problem.is_some())
            .map(ToString::to_string)
            .collect();
        assert!(
            failed.is_empty(),
            "{}\n\nIf the change is intended, remake the references with `cargo xtask golden \
             --update` and say why in the pull request.",
            failed.join("\n")
        );
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.png"))
    }

    fn problem(&self, name: &str, image: &EditorImage) -> Option<String> {
        let path = self.path(name);
        let reference = match fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| decode_png(&b))
        {
            Ok(reference) => reference,
            Err(error) => {
                return Some(format!(
                    "no reference at {} ({error}); make it with `cargo xtask golden --update`",
                    path.display()
                ));
            }
        };
        if (reference.width, reference.height) != (image.width, image.height) {
            return Some(format!(
                "drawn {}x{} pixels, the reference is {}x{}",
                image.width, image.height, reference.width, reference.height
            ));
        }
        let difference = compare(&reference, image);
        (difference.share > SHARE).then(|| {
            format!(
                "{} pixels differ ({:.2} % of them; at most {:.2} % may), the worst at {:?}",
                difference.differing,
                difference.share * 100.0,
                SHARE * 100.0,
                difference.worst
            )
        })
    }

    /// Writes the references `selection` names that are missing or no
    /// longer match.
    fn update(&self, selection: &Selection) -> Result<(), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("can't create {}: {e}", self.dir.display()))?;
        for (name, image) in &self.images {
            if selection.includes(name) && self.problem(name, image).is_some() {
                write(&self.path(name), image)?;
                println!("{name}: reference written");
            }
        }
        Ok(())
    }
}

fn write(path: &Path, image: &EditorImage) -> Result<(), String> {
    fs::write(path, encode_png(image)?).map_err(|e| format!("can't write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: u32, height: u32, value: u8) -> EditorImage {
        EditorImage {
            width,
            height,
            rgba: vec![value; (width * height * 4) as usize],
        }
    }

    #[test]
    fn a_png_round_trips() {
        let mut image = flat(3, 2, 7);
        image.rgba[5] = 200;
        assert_eq!(decode_png(&encode_png(&image).unwrap()).unwrap(), image);
    }

    #[test]
    fn a_few_edge_pixels_may_differ_but_not_many() {
        let reference = flat(100, 100, 50);
        let mut near = reference.clone();
        // A pixel one level off is no difference; 10 pixels far off (0.1 %)
        // are within the share; 50 are not.
        near.rgba[0] += 1;
        assert_eq!(compare(&reference, &near).differing, 0);
        for pixel in 0..10 {
            near.rgba[pixel * 4] = 255;
        }
        let few = compare(&reference, &near);
        assert_eq!((few.differing, few.worst), (10, Some(((0, 0), 205))));
        assert!(few.share <= SHARE);
        for pixel in 0..50 {
            near.rgba[pixel * 4] = 255;
        }
        assert!(compare(&reference, &near).share > SHARE);
    }

    #[test]
    fn a_suite_makes_a_reference_and_then_holds_to_it() {
        let dir = std::env::temp_dir().join(format!("lspk-snapshot-{}", std::process::id()));
        let mut suite = Suite::new(&dir);
        suite.add("flat", flat(4, 4, 90));
        assert!(
            suite.run().unwrap()[0]
                .problem
                .as_deref()
                .unwrap()
                .contains("no reference")
        );
        suite.update(&Selection::All).unwrap();
        assert_eq!(suite.run().unwrap()[0].problem, None);
        let mut other = Suite::new(&dir);
        other.add("flat", flat(4, 4, 200));
        assert!(
            other.run().unwrap()[0]
                .problem
                .as_deref()
                .unwrap()
                .contains("differ")
        );
        let mut wider = Suite::new(&dir);
        wider.add("flat", flat(5, 4, 90));
        assert!(
            wider.run().unwrap()[0]
                .problem
                .as_deref()
                .unwrap()
                .contains("5x4")
        );
        fs::remove_file(dir.join("flat.png")).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
}
