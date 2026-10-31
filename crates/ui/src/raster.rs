//! A software renderer for egui's output: the triangles it tessellates into,
//! drawn into an RGBA image with no window and no GPU.
//!
//! It draws as `egui_glow` does where that matters to a picture: colours and
//! textures multiply and blend as premultiplied sRGB values, no gamma
//! conversion, and each clip rectangle cuts what it holds. The edges are
//! egui's own feathered triangles, so shapes are as smooth as on screen.

use std::collections::HashMap;

use egui::TexturesDelta;
use egui::epaint::{ClippedPrimitive, ImageData, ImageDelta, Primitive, TextureId, Vertex};
use egui::{Color32, Pos2, Rect};
use lspk_plugin::EditorImage;

/// Draws what egui produced, frame after frame, into images.
#[derive(Default)]
pub struct Rasterizer {
    textures: HashMap<TextureId, Texture>,
}

struct Texture {
    size: [usize; 2],
    pixels: Vec<Color32>,
}

impl Rasterizer {
    /// Takes the texture changes a frame made, which its meshes refer to.
    pub fn update_textures(&mut self, delta: &TexturesDelta) {
        for (id, changes) in &delta.set {
            for change in changes {
                self.apply(*id, change);
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    fn apply(&mut self, id: TextureId, change: &ImageDelta) {
        let ImageData::Color(image) = &change.image;
        let Some([x, y]) = change.pos else {
            self.textures.insert(
                id,
                Texture {
                    size: image.size,
                    pixels: image.pixels.clone(),
                },
            );
            return;
        };
        let Some(texture) = self.textures.get_mut(&id) else {
            return;
        };
        for row in 0..image.size[1] {
            let to = (y + row) * texture.size[0] + x;
            let from = row * image.size[0];
            if let (Some(to), Some(from)) = (
                texture.pixels.get_mut(to..to + image.size[0]),
                image.pixels.get(from..from + image.size[0]),
            ) {
                to.copy_from_slice(from);
            }
        }
    }

    /// Draws `primitives` over `background` into an image `width` by
    /// `height` pixels, with `pixels_per_point` pixels to egui's point.
    #[must_use]
    pub fn render(
        &self,
        primitives: &[ClippedPrimitive],
        pixels_per_point: f32,
        (width, height): (u32, u32),
        background: Color32,
    ) -> EditorImage {
        let (w, h) = (width as usize, height as usize);
        let back = to_f32(background);
        let mut canvas = vec![back; w * h];
        for clipped in primitives {
            let Primitive::Mesh(mesh) = &clipped.primitive else {
                continue;
            };
            let Some(texture) = self.textures.get(&mesh.texture_id) else {
                continue;
            };
            let clip = pixel_rect(clipped.clip_rect, pixels_per_point, w, h);
            for triangle in mesh.indices.as_chunks::<3>().0 {
                let corners = triangle.map(|index| mesh.vertices.get(index as usize));
                if let [Some(a), Some(b), Some(c)] = corners {
                    fill(&mut canvas, w, clip, texture, pixels_per_point, [a, b, c]);
                }
            }
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for pixel in canvas {
            // The background is opaque, so what's drawn over it is too.
            let alpha = pixel[3].clamp(0.0, 255.0);
            let straight = |value: f32| {
                if alpha <= 0.0 {
                    0
                } else {
                    (value * 255.0 / alpha).round().clamp(0.0, 255.0) as u8
                }
            };
            rgba.extend([
                straight(pixel[0]),
                straight(pixel[1]),
                straight(pixel[2]),
                alpha.round() as u8,
            ]);
        }
        EditorImage {
            width,
            height,
            rgba,
        }
    }
}

fn to_f32(color: Color32) -> [f32; 4] {
    [color.r(), color.g(), color.b(), color.a()].map(f32::from)
}

/// The pixels `rect` covers, as `(min x, min y, max x, max y)`, half open
/// and inside an image `w` by `h`.
fn pixel_rect(rect: Rect, ppp: f32, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let clamp = |value: f32, max: usize| (value.round().max(0.0) as usize).min(max);
    (
        clamp(rect.min.x * ppp, w),
        clamp(rect.min.y * ppp, h),
        clamp(rect.max.x * ppp, w),
        clamp(rect.max.y * ppp, h),
    )
}

/// Twice the signed area of the triangle `a`, `b`, `p`: positive where `p`
/// is on one side of the line from `a` to `b`.
fn edge(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

/// Whether a pixel centre exactly on the edge from `a` to `b` belongs to
/// this triangle: of the two triangles sharing an edge, one takes it.
fn owns(a: Pos2, b: Pos2) -> bool {
    b.y > a.y || (b.y == a.y && b.x < a.x)
}

/// Draws one triangle of a mesh, blending over `canvas`.
fn fill(
    canvas: &mut [[f32; 4]],
    width: usize,
    clip: (usize, usize, usize, usize),
    texture: &Texture,
    ppp: f32,
    corners: [&Vertex; 3],
) {
    let mut points = corners.map(|v| Pos2::new(v.pos.x * ppp, v.pos.y * ppp));
    let mut vertices = corners;
    let mut area = edge(points[0], points[1], points[2]);
    if area == 0.0 || !area.is_finite() {
        return;
    }
    if area < 0.0 {
        points.swap(1, 2);
        vertices.swap(1, 2);
        area = -area;
    }
    let [a, b, c] = points;
    let (min_x, min_y, max_x, max_y) = clip;
    let low_x = (a.x.min(b.x).min(c.x).floor().max(min_x as f32)) as usize;
    let low_y = (a.y.min(b.y).min(c.y).floor().max(min_y as f32)) as usize;
    let high_x = (a.x.max(b.x).max(c.x).ceil().min(max_x as f32)) as usize;
    let high_y = (a.y.max(b.y).max(c.y).ceil().min(max_y as f32)) as usize;
    let (own_ab, own_bc, own_ca) = (owns(a, b), owns(b, c), owns(c, a));
    let colors = vertices.map(|v| to_f32(v.color));
    for y in low_y..high_y {
        for x in low_x..high_x {
            let p = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
            // Each weight is the area opposite its vertex.
            let w = [edge(b, c, p), edge(c, a, p), edge(a, b, p)];
            let inside = |weight: f32, owned: bool| weight > 0.0 || (weight == 0.0 && owned);
            if !(inside(w[0], own_bc) && inside(w[1], own_ca) && inside(w[2], own_ab)) {
                continue;
            }
            let l = w.map(|weight| weight / area);
            let uv = [0, 1].map(|axis| {
                let at = |v: &Vertex| if axis == 0 { v.uv.x } else { v.uv.y };
                l[0] * at(vertices[0]) + l[1] * at(vertices[1]) + l[2] * at(vertices[2])
            });
            let texel = sample(texture, uv[0], uv[1]);
            let mut source = [0.0; 4];
            for (channel, out) in source.iter_mut().enumerate() {
                let color = l[0] * colors[0][channel]
                    + l[1] * colors[1][channel]
                    + l[2] * colors[2][channel];
                *out = texel[channel] * color / 255.0;
            }
            let pixel = &mut canvas[y * width + x];
            let through = 1.0 - source[3] / 255.0;
            for channel in 0..4 {
                pixel[channel] = source[channel] + pixel[channel] * through;
            }
        }
    }
}

/// The texel at `(u, v)`, in the texture's own units.
fn sample(texture: &Texture, u: f32, v: f32) -> [f32; 4] {
    let [w, h] = texture.size;
    if w == 0 || h == 0 {
        return [255.0; 4];
    }
    let at =
        |value: f32, size: usize| ((value * size as f32).floor().max(0.0) as usize).min(size - 1);
    texture
        .pixels
        .get(at(v, h) * w + at(u, w))
        .map_or([255.0; 4], |&color| to_f32(color))
}
