//! Theming tokens (colours, sizes, fonts) and the global zoom.
//!
//! Every colour, size and font an editor draws with comes from a [`Theme`],
//! so a plug-in restyles the whole interface in one place. The global
//! zoom scales everything, window included, from [`MIN_ZOOM`] to
//! [`MAX_ZOOM`], on top of the host's scale factor.

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals};

use crate::fonts;

/// The smallest global zoom: 50 %.
pub const MIN_ZOOM: f32 = 0.5;
/// The largest global zoom: 200 %.
pub const MAX_ZOOM: f32 = 2.0;
/// The zoom levels the zoom keys step through.
pub const ZOOM_STEPS: [f32; 11] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

/// `zoom`, kept between [`MIN_ZOOM`] and [`MAX_ZOOM`] (100 % if it isn't a
/// number).
#[must_use]
pub fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// The zoom level after `zoom` in the direction of `steps` (positive to
/// zoom in), along [`ZOOM_STEPS`].
#[must_use]
pub fn step_zoom(zoom: f32, steps: i32) -> f32 {
    let zoom = clamp_zoom(zoom);
    let last = ZOOM_STEPS.len() - 1;
    match ZOOM_STEPS
        .iter()
        .position(|&step| (step - zoom).abs() < 0.005)
    {
        Some(at) => {
            let to = at.saturating_add_signed(steps as isize).min(last);
            ZOOM_STEPS[to]
        }
        // Between two levels, one step goes to the next level that way.
        None if steps > 0 => ZOOM_STEPS
            .iter()
            .copied()
            .find(|&step| step > zoom)
            .unwrap_or(MAX_ZOOM),
        None if steps < 0 => ZOOM_STEPS
            .iter()
            .copied()
            .rev()
            .find(|&step| step < zoom)
            .unwrap_or(MIN_ZOOM),
        None => zoom,
    }
}

/// The colours an editor draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct Colors {
    /// The window's background.
    pub background: Color32,
    /// A control's face, and a knob's unlit track.
    pub control: Color32,
    /// A field's background: number boxes, choosers, typed entry.
    pub field: Color32,
    /// The lit part of a control: a knob's arc, a switch that's on.
    pub accent: Color32,
    /// Text on the accent.
    pub accent_text: Color32,
    /// Names and values.
    pub text: Color32,
    /// Less important text.
    pub text_dim: Color32,
    /// The ring around the control the keyboard is on.
    pub focus: Color32,
    /// Lines between areas.
    pub separator: Color32,
}

/// The sizes an editor lays out with, in points at 100 % zoom.
#[derive(Clone, Debug, PartialEq)]
pub struct Sizes {
    /// Body text.
    pub text: f32,
    /// Names and values under controls.
    pub small_text: f32,
    /// A knob's diameter.
    pub knob: f32,
    /// One control's cell in the generic interface: its width.
    pub cell_width: f32,
    /// One control's cell in the generic interface: its height.
    pub cell_height: f32,
    /// The space around and between cells.
    pub spacing: f32,
    /// A field's or button's height.
    pub field_height: f32,
    /// Rounded corners.
    pub corner: f32,
    /// A knob's arc.
    pub stroke: f32,
}

/// Where an editor's typeface comes from.
#[derive(Clone, Debug, Default)]
pub enum FontSource {
    /// The platform's own interface font, found at run time (San
    /// Francisco on macOS, Segoe UI on Windows, DejaVu Sans or another
    /// common sans-serif on Linux). Nothing is bundled.
    #[default]
    System,
    /// A TrueType or OpenType font a plug-in brings, whose licence is the
    /// plug-in's to meet.
    Bytes(Arc<[u8]>),
}

/// The typefaces an editor draws with.
#[derive(Clone, Debug, Default)]
pub struct Fonts {
    /// The font for every text.
    pub ui: FontSource,
}

/// Everything an editor draws with: colours, sizes, fonts, and the zoom it
/// opens at.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Colours.
    pub colors: Colors,
    /// Sizes.
    pub sizes: Sizes,
    /// Typefaces.
    pub fonts: Fonts,
    /// The global zoom the editor first opens at.
    pub zoom: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// The kit's dark theme.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            colors: Colors {
                background: Color32::from_rgb(0x24, 0x26, 0x2b),
                control: Color32::from_rgb(0x3a, 0x3d, 0x45),
                field: Color32::from_rgb(0x17, 0x18, 0x1c),
                accent: Color32::from_rgb(0x4f, 0xb8, 0xa6),
                accent_text: Color32::from_rgb(0x10, 0x14, 0x14),
                text: Color32::from_rgb(0xe4, 0xe6, 0xea),
                text_dim: Color32::from_rgb(0x9a, 0x9f, 0xa8),
                focus: Color32::from_rgb(0xf0, 0xc0, 0x4a),
                separator: Color32::from_rgb(0x44, 0x47, 0x4f),
            },
            sizes: Sizes {
                text: 13.0,
                small_text: 11.0,
                knob: 44.0,
                cell_width: 84.0,
                cell_height: 92.0,
                spacing: 12.0,
                field_height: 20.0,
                corner: 3.0,
                stroke: 3.0,
            },
            fonts: Fonts::default(),
            zoom: 1.0,
        }
    }

    /// The kit's light theme.
    #[must_use]
    pub fn light() -> Self {
        let mut theme = Self::dark();
        theme.colors = Colors {
            background: Color32::from_rgb(0xee, 0xef, 0xf1),
            control: Color32::from_rgb(0xcf, 0xd2, 0xd8),
            field: Color32::from_rgb(0xff, 0xff, 0xff),
            accent: Color32::from_rgb(0x1f, 0x86, 0x76),
            accent_text: Color32::from_rgb(0xff, 0xff, 0xff),
            text: Color32::from_rgb(0x1c, 0x1e, 0x22),
            text_dim: Color32::from_rgb(0x5c, 0x61, 0x6a),
            focus: Color32::from_rgb(0xc2, 0x7c, 0x00),
            separator: Color32::from_rgb(0xbd, 0xc1, 0xc8),
        };
        theme
    }

    /// The body font.
    #[must_use]
    pub fn font(&self) -> FontId {
        FontId::proportional(self.sizes.text)
    }

    /// The font for names and values.
    #[must_use]
    pub fn small_font(&self) -> FontId {
        FontId::proportional(self.sizes.small_text)
    }

    /// The font definitions for egui: the theme's typeface for every
    /// family, or none if the platform has no font the kit can find, when
    /// text isn't drawn but nothing fails.
    #[must_use]
    pub fn font_definitions(&self) -> FontDefinitions {
        let mut definitions = FontDefinitions::empty();
        let bytes = match &self.fonts.ui {
            FontSource::System => fonts::system_ui_font(),
            FontSource::Bytes(bytes) => fonts::is_font(bytes).then(|| bytes.clone()),
        };
        if let Some(bytes) = bytes {
            definitions
                .font_data
                .insert("ui".into(), Arc::new(FontData::from_owned(bytes.to_vec())));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                definitions
                    .families
                    .entry(family)
                    .or_default()
                    .push("ui".into());
            }
        }
        definitions
    }

    /// Applies the theme to `ctx`: its fonts, colours and text sizes.
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_fonts(self.font_definitions());
        let c = &self.colors;
        let mut visuals = if c.background.r() < 128 {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        visuals.panel_fill = c.background;
        visuals.window_fill = c.background;
        visuals.extreme_bg_color = c.field;
        visuals.override_text_color = Some(c.text);
        visuals.selection.bg_fill = c.accent;
        visuals.selection.stroke = Stroke::new(1.0, c.accent_text);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, c.separator);
        visuals.widgets.inactive.bg_fill = c.control;
        visuals.widgets.inactive.weak_bg_fill = c.control;
        ctx.set_visuals(visuals);
        let sizes = self.sizes.clone();
        ctx.all_styles_mut(|style| {
            for (text_style, font) in &mut style.text_styles {
                font.size = match text_style {
                    egui::TextStyle::Small => sizes.small_text,
                    egui::TextStyle::Heading => sizes.text * 1.3,
                    _ => sizes.text,
                };
            }
            style.spacing.item_spacing = egui::vec2(sizes.spacing / 2.0, sizes.spacing / 2.0);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_stays_between_half_and_double() {
        assert_eq!(clamp_zoom(0.1), MIN_ZOOM);
        assert_eq!(clamp_zoom(3.0), MAX_ZOOM);
        assert_eq!(clamp_zoom(f32::NAN), 1.0);
        assert_eq!(clamp_zoom(1.3), 1.3);
    }

    #[test]
    fn zoom_steps_walk_the_levels() {
        assert_eq!(step_zoom(1.0, 1), 1.1);
        assert_eq!(step_zoom(1.0, -1), 0.9);
        assert_eq!(step_zoom(2.0, 1), 2.0);
        assert_eq!(step_zoom(0.5, -1), 0.5);
        assert_eq!(step_zoom(1.3, 1), 1.5);
        assert_eq!(step_zoom(1.3, -1), 1.25);
        let mut zoom = MIN_ZOOM;
        for _ in 0..ZOOM_STEPS.len() {
            zoom = step_zoom(zoom, 1);
        }
        assert_eq!(zoom, MAX_ZOOM);
    }

    #[test]
    fn a_font_that_isnt_one_is_left_out() {
        let mut theme = Theme::dark();
        theme.fonts.ui = FontSource::Bytes(Arc::from(&b"not a font"[..]));
        let definitions = theme.font_definitions();
        assert!(definitions.font_data.is_empty());
        // Text without a font draws nothing, and doesn't panic.
        let ctx = egui::Context::default();
        ctx.set_fonts(definitions);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("no glyphs");
        });
        output.textures_delta.clear();
    }
}
