//! An editor's interface drawn with no window: egui run by hand, frame by
//! frame, as a window runs it, and drawn into an image.
//!
//! [`Offscreen`] makes the same choices as the editor's window about when a
//! frame is drawn: when input arrives, when a parameter's value has changed,
//! and when egui has asked for one (an animation, a tooltip's delay, a view
//! that shows a moving value). With none of those, [`Offscreen::advance`]
//! draws nothing however long it's told time has passed, which is what
//! lets the interface cost nothing while nobody touches it, and what the
//! interface-cost benchmark measures.

use std::time::Duration;

use egui::{Event, RawInput, Rect, Vec2, ViewportId, ViewportInfo, pos2};
use lspk_plugin::{EditorContext, EditorImage};

use crate::editor::{View, draw, prepare};
use crate::raster::Rasterizer;
use crate::theme::Theme;

/// How many frames [`Offscreen::settle`] draws at most: egui draws a few
/// after each request, and the generic interface's scroll bar fades over
/// about a third of a second of them. A view that asks for frames for ever
/// (a meter) is cut off here.
const MAX_SETTLE: u32 = 60;

/// The time one of those frames takes, at least: 60 frames a second.
const FRAME: f64 = 1.0 / 60.0;

/// An editor's interface, running without a window.
pub struct Offscreen {
    ctx: egui::Context,
    view: Box<dyn View>,
    cx: EditorContext,
    theme: Theme,
    zoom: f32,
    /// The window's size, in points at 100 % zoom.
    size: Vec2,
    /// Pixels to a point at 100 % zoom.
    scale: f32,
    rasterizer: Rasterizer,
    /// Seconds since it opened, as egui's input has them.
    time: f64,
    frames: u64,
    /// Input waiting for the next frame.
    events: Vec<Event>,
    /// The parameters' generation as the last frame began.
    seen: u64,
    /// When egui wants the next frame, if it does.
    due: Option<f64>,
    last: egui::FullOutput,
}

impl Offscreen {
    /// `view` drawn at `size` points (at 100 % zoom) and `zoom`, with
    /// `scale` pixels to a point, reading `cx`'s values; its first frames
    /// are drawn already.
    #[must_use]
    pub fn new(
        view: Box<dyn View>,
        cx: EditorContext,
        theme: Theme,
        size: Vec2,
        zoom: f32,
        scale: f32,
    ) -> Self {
        let ctx = egui::Context::default();
        prepare(&ctx, &theme);
        ctx.set_zoom_factor(zoom);
        let seen = cx.values().generation();
        let mut offscreen = Self {
            ctx,
            view,
            cx,
            theme,
            zoom,
            size,
            scale,
            rasterizer: Rasterizer::default(),
            time: 0.0,
            frames: 0,
            events: Vec::new(),
            seen,
            due: Some(0.0),
            last: egui::FullOutput::default(),
        };
        offscreen.settle();
        offscreen
    }

    /// The egui context, to read what the interface drew.
    #[must_use]
    pub fn context(&self) -> &egui::Context {
        &self.ctx
    }

    /// The last frame's output.
    #[must_use]
    pub fn output(&self) -> &egui::FullOutput {
        &self.last
    }

    /// How many frames have been drawn.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Gives the interface some input, for the next frame.
    pub fn event(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Lets `time` pass and draws a frame if one is due: input has
    /// arrived, a parameter's value has changed, or egui asked for one by
    /// now. Returns whether it drew.
    pub fn advance(&mut self, time: Duration) -> bool {
        self.time += time.as_secs_f64();
        let wanted = !self.events.is_empty()
            || self.cx.values().generation() != self.seen
            || self.due.is_some_and(|due| due <= self.time);
        if wanted {
            self.frame();
        }
        wanted
    }

    /// Draws frames, letting time pass as it would between them, until
    /// egui wants no more: layout settled and animations over, which is
    /// where a window that opens comes to rest.
    pub fn settle(&mut self) {
        for _ in 0..MAX_SETTLE {
            self.frame();
            let Some(due) = self.due else {
                break;
            };
            self.time = due.max(self.time + FRAME);
        }
    }

    /// Settles, then draws the interface as it stands into an image.
    #[must_use]
    pub fn render(&mut self) -> EditorImage {
        self.settle();
        let ppp = self.last.pixels_per_point;
        let primitives = self
            .ctx
            .tessellate(std::mem::take(&mut self.last.shapes), ppp);
        let pixels = (self.size * ppp).max(Vec2::splat(1.0));
        self.rasterizer.render(
            &primitives,
            ppp,
            (pixels.x.ceil() as u32, pixels.y.ceil() as u32),
            self.theme.colors.background,
        )
    }

    /// Draws one frame.
    fn frame(&mut self) {
        self.seen = self.cx.values().generation();
        let mut viewports = egui::ViewportIdMap::default();
        viewports.insert(
            ViewportId::ROOT,
            ViewportInfo {
                native_pixels_per_point: Some(self.scale),
                ..ViewportInfo::default()
            },
        );
        let input = RawInput {
            screen_rect: Some(Rect::from_min_max(
                pos2(0.0, 0.0),
                pos2(self.size.x, self.size.y),
            )),
            time: Some(self.time),
            events: std::mem::take(&mut self.events),
            viewports,
            focused: true,
            ..RawInput::default()
        };
        let ctx = self.ctx.clone();
        let (view, cx, theme) = (self.view.as_mut(), &self.cx, &self.theme);
        let mut zoom = self.zoom;
        let mut output = ctx.run_ui(input, |ui| {
            zoom = draw(ui, view, cx, theme, zoom);
        });
        self.zoom = zoom;
        self.ctx.set_zoom_factor(zoom);
        self.rasterizer.update_textures(&output.textures_delta);
        output.textures_delta.clear();
        self.due = output
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|viewport| viewport.repaint_delay)
            .filter(|delay| *delay < Duration::MAX)
            .map(|delay| self.time + delay.as_secs_f64());
        self.last = output;
        self.frames += 1;
    }
}
