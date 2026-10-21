// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A picture of what was copied, for other apps (the system clipboard).
//!
//! On the UI thread, the strokes and text boxes are painted as they are on
//! screen (the same tessellation and text layout) onto a hidden layer, which
//! is taken back at once and turned into triangles; the font atlas comes
//! along for the text. A thread then rasterises those triangles, over the
//! pictures (decoded from their files) and the paper of a whole page, and
//! puts the result on the system clipboard. Transparent around ink: pasted
//! onto anything, only what was written shows.
//!
//! On Windows it is a picture. Elsewhere egui only notices the paste key
//! when the clipboard has text, so Tratto could not paste its own copy with
//! the keyboard: there it is HTML holding the picture (what Word, mail or a
//! browser take), with the text of the copied text boxes as plain text (a
//! space if there are none).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Color32, ColorImage, Id, LayerId, Order, Painter, Pos2, Rect, Shape, TextureId,
    epaint::{ClippedPrimitive, Mesh, Primitive},
    pos2,
};

use tratto_core::model::{Image, ImageData, Stroke, TextBox};

use crate::app::clipboard::{Content, PageContent, SystemCopy};
use crate::stroke_mesh;

/// The picture's longer side, at most (pixels).
const MAX_SIDE: f32 = 2048.0;
/// Pixels per document unit, at most (small selections stay sharp, not huge).
const MAX_DENSITY: f32 = 3.0;
/// Room around copied content, in document units (antialiased edges).
const MARGIN: f32 = 4.0;

/// What to make a picture of.
pub struct Snapshot {
    strokes: Vec<Stroke>,
    texts: Vec<TextBox>,
    images: Vec<Image>,
    /// The part of the page shown (page space).
    area: Rect,
    /// On white paper (a whole page); else transparent.
    paper: bool,
    /// Plain text with it, where it goes as HTML (see the module docs).
    alt: String,
}

/// The text of `texts`, one box a paragraph; a space without any.
fn alt_text(texts: &[TextBox]) -> String {
    let text = texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("\n\n");
    if text.trim().is_empty() { " ".to_owned() } else { text }
}

impl Snapshot {
    pub fn of_content(c: &Content) -> Self {
        let area = c.bounds.expand(MARGIN);
        let alt = alt_text(&c.texts);
        Self { strokes: c.strokes.clone(), texts: c.texts.clone(), images: c.images.clone(), area, paper: false, alt }
    }

    pub fn of_page(p: &PageContent, (w, h): (f32, f32)) -> Self {
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(w, h));
        let alt = alt_text(&p.texts);
        Self { strokes: p.strokes.clone(), texts: p.texts.clone(), images: p.images.clone(), area, paper: true, alt }
    }
}

/// Everything the thread needs: triangles (pixels), the font atlas, the
/// pictures and where they go (pixels).
struct Scene {
    size: [usize; 2],
    paper: bool,
    pictures: Vec<(Rect, Arc<ImageData>)>,
    primitives: Vec<ClippedPrimitive>,
    /// Points to pixels of `primitives`.
    ppp: f32,
    atlas: ColorImage,
    alt: String,
}

/// Makes a picture of `snap` and puts it on the system clipboard, in the
/// background; `system` says it is being made, then what is there.
pub fn export(ctx: &egui::Context, snap: Snapshot, system: Arc<Mutex<SystemCopy>>) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    // Before the thread starts: it may be done at once.
    *system.lock().unwrap_or_else(|e| e.into_inner()) = SystemCopy::Pending(id);
    let scene = scene(ctx, &snap);
    let shared = system.clone();
    let spawned = std::thread::Builder::new().name("copy-picture".into()).spawn(move || {
        let image = rasterise(&scene);
        let done = put_on_clipboard(&image, &scene.alt).unwrap_or_default();
        let mut system = shared.lock().unwrap_or_else(|e| e.into_inner());
        // Unless a later copy replaced this one meanwhile.
        if *system == SystemCopy::Pending(id) {
            *system = done;
        }
    });
    if spawned.is_err() {
        *system.lock().unwrap_or_else(|e| e.into_inner()) = SystemCopy::Nothing;
    }
}

fn scene(ctx: &egui::Context, snap: &Snapshot) -> Scene {
    let ppp = ctx.pixels_per_point();
    let area = snap.area;
    // Pixels per document unit, then points per document unit.
    let density = (MAX_SIDE / area.width().max(area.height()).max(1.0)).min(MAX_DENSITY);
    let scale = density / ppp;
    let size = [(area.width() * density).ceil().max(1.0) as usize, (area.height() * density).ceil().max(1.0) as usize];
    let at = |p: Pos2| ((p - area.min) * scale).to_pos2();

    // Bottom to top, as on the page: highlighters, text, pen.
    let layer = LayerId::new(Order::Background, Id::new("clipboard_snapshot"));
    let painter = Painter::new(ctx.clone(), layer, Rect::EVERYTHING);
    let strokes = |kind: tratto_core::model::StrokeKind| {
        let mut mesh = Mesh::default();
        for s in snap.strokes.iter().filter(|s| s.kind == kind) {
            stroke_mesh::tessellate(&mut mesh, s.kind, s.color, s.width, &s.points, scale, 1.0 / ppp);
        }
        mesh.translate(-(area.min.to_vec2() * scale));
        mesh
    };
    painter.add(Shape::mesh(strokes(tratto_core::model::StrokeKind::Highlighter)));
    for t in &snap.texts {
        let color = Color32::from_rgb(t.color[0], t.color[1], t.color[2]);
        crate::render::paint_text_box(&painter, at(pos2(t.x, t.y)), scale, t, color);
    }
    painter.add(Shape::mesh(strokes(tratto_core::model::StrokeKind::Pen)));
    // Taken back before the frame ends: never shown.
    let shapes: Vec<_> = ctx.graphics_mut(|g| std::mem::take(g.entry(layer)).all_entries().cloned().collect());
    let primitives = ctx.tessellate(shapes, ppp);
    let atlas = ctx.fonts(|f| f.image());

    let pictures = snap
        .images
        .iter()
        .map(|i| {
            let min = (pos2(i.x, i.y) - area.min) * density;
            (Rect::from_min_size(min.to_pos2(), i.rect().size() * density), i.data.clone())
        })
        .collect();
    Scene { size, paper: snap.paper, pictures, primitives, ppp, atlas, alt: snap.alt.clone() }
}

/// Premultiplied RGBA pixels.
struct Canvas {
    size: [usize; 2],
    px: Vec<[f32; 4]>,
}

impl Canvas {
    /// `c` (premultiplied) over the pixel at `i`.
    fn over(&mut self, i: usize, c: [f32; 4]) {
        let d = &mut self.px[i];
        let k = 1.0 - c[3];
        for n in 0..4 {
            d[n] = c[n] + d[n] * k;
        }
    }
}

/// A picture as image files and the clipboard keep it: RGBA, not
/// premultiplied, row by row.
struct Rgba {
    size: [usize; 2],
    bytes: Vec<u8>,
}

impl Rgba {
    #[cfg(test)]
    fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.size[0] + x) * 4;
        [self.bytes[i], self.bytes[i + 1], self.bytes[i + 2], self.bytes[i + 3]]
    }
}

fn rasterise(scene: &Scene) -> Rgba {
    let [w, h] = scene.size;
    let paper = if scene.paper { [1.0; 4] } else { [0.0; 4] };
    let mut canvas = Canvas { size: scene.size, px: vec![paper; w * h] };
    for (rect, data) in &scene.pictures {
        draw_picture(&mut canvas, *rect, data);
    }
    for p in &scene.primitives {
        if let Primitive::Mesh(mesh) = &p.primitive {
            let clip = Rect::from_min_max(
                (p.clip_rect.min.to_vec2() * scene.ppp).to_pos2(),
                (p.clip_rect.max.to_vec2() * scene.ppp).to_pos2(),
            );
            draw_mesh(&mut canvas, mesh, scene.ppp, clip, &scene.atlas);
        }
    }
    // Unmultiplied. Where nothing is, white (still fully transparent): an
    // app that ignores the alpha of a pasted picture (Windows' plain bitmap
    // has none) shows white paper there, not black.
    let bytes = canvas
        .px
        .iter()
        .flat_map(|p| {
            let a = p[3].clamp(0.0, 1.0);
            if a <= 0.0 {
                return [255, 255, 255, 0];
            }
            let un = |c: f32| (c / a * 255.0).round().clamp(0.0, 255.0) as u8;
            [un(p[0]), un(p[1]), un(p[2]), (a * 255.0).round() as u8]
        })
        .collect();
    Rgba { size: [w, h], bytes }
}

/// The picture `data` scaled into `rect` (pixels).
fn draw_picture(canvas: &mut Canvas, rect: Rect, data: &ImageData) {
    let side = rect.width().max(rect.height()).ceil() as u32;
    let Ok(decoded) = data.decode_sizes(&[side.max(1)]) else { return };
    let Some(img) = decoded.first() else { return };
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let [w, h] = canvas.size;
    let (x0, y0) = (rect.min.x.max(0.0) as usize, rect.min.y.max(0.0) as usize);
    let (x1, y1) = ((rect.max.x.ceil() as usize).min(w), (rect.max.y.ceil() as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let u = ((x as f32 + 0.5 - rect.min.x) / rect.width() * iw).clamp(0.0, iw - 1.0);
            let v = ((y as f32 + 0.5 - rect.min.y) / rect.height() * ih).clamp(0.0, ih - 1.0);
            let p = img.get_pixel(u as u32, v as u32).0;
            let a = p[3] as f32 / 255.0;
            let c = [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a];
            canvas.over(y * w + x, c);
        }
    }
}

/// The triangles of `mesh` (points, `ppp` pixels each), within `clip`
/// (pixels), sampling the font atlas for text.
fn draw_mesh(canvas: &mut Canvas, mesh: &Mesh, ppp: f32, clip: Rect, atlas: &ColorImage) {
    let textured = mesh.texture_id == TextureId::default();
    let [w, h] = canvas.size;
    let [aw, ah] = atlas.size;
    for tri in mesh.indices.chunks_exact(3) {
        let v = [0, 1, 2].map(|k| &mesh.vertices[tri[k] as usize]);
        let p = v.map(|v| (v.pos.to_vec2() * ppp).to_pos2());
        let area = edge(p[0], p[1], p[2]);
        if area.abs() < 1e-6 {
            continue;
        }
        let bounds = Rect::from_points(&p).intersect(clip);
        let (x0, y0) = (bounds.min.x.floor().max(0.0) as usize, bounds.min.y.floor().max(0.0) as usize);
        let (x1, y1) = ((bounds.max.x.ceil().max(0.0) as usize).min(w), (bounds.max.y.ceil().max(0.0) as usize).min(h));
        let colors = v.map(|v| v.color.to_array().map(|c| c as f32 / 255.0));
        for y in y0..y1 {
            for x in x0..x1 {
                let c = pos2(x as f32 + 0.5, y as f32 + 0.5);
                // Barycentric weights; inside when all have the triangle's sign.
                let b = [edge(p[1], p[2], c) / area, edge(p[2], p[0], c) / area, edge(p[0], p[1], c) / area];
                if b.iter().any(|&k| k < 0.0) {
                    continue;
                }
                let mut color = [0.0f32; 4];
                for n in 0..4 {
                    color[n] = b[0] * colors[0][n] + b[1] * colors[1][n] + b[2] * colors[2][n];
                }
                if textured {
                    let u = b[0] * v[0].uv.x + b[1] * v[1].uv.x + b[2] * v[2].uv.x;
                    let t = b[0] * v[0].uv.y + b[1] * v[1].uv.y + b[2] * v[2].uv.y;
                    let tx = ((u * aw as f32) as usize).min(aw - 1);
                    let ty = ((t * ah as f32) as usize).min(ah - 1);
                    let texel = atlas.pixels[ty * aw + tx].to_array().map(|c| c as f32 / 255.0);
                    for n in 0..4 {
                        color[n] *= texel[n];
                    }
                }
                canvas.over(y * w + x, color);
            }
        }
    }
}

/// Twice the signed area of (a, b, c).
fn edge(a: Pos2, b: Pos2, c: Pos2) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Puts `image` on the system clipboard (see the module docs), with `alt`
/// as its text where it goes as HTML; what is there then. The clipboard is
/// kept for the whole run: on Linux what it offers goes with it.
fn put_on_clipboard(image: &Rgba, alt: &str) -> Option<SystemCopy> {
    static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
    let mut clipboard = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    if clipboard.is_none() {
        *clipboard = arboard::Clipboard::new().ok();
    }
    let clipboard = clipboard.as_mut()?;
    let bytes = image.bytes.clone();
    let [w, h] = image.size;
    if cfg!(windows) {
        let data = arboard::ImageData { width: w, height: h, bytes: bytes.into() };
        clipboard.set_image(data).ok()?;
        return Some(SystemCopy::Image(image.size));
    }
    let png = tratto_core::images::encode_png(w as u32, h as u32, bytes).ok()?;
    let html = format!("<img src=\"data:image/png;base64,{}\" width=\"{w}\" height=\"{h}\">", base64(&png));
    clipboard.set_html(html, Some(alt.to_owned())).ok()?;
    Some(SystemCopy::Text(alt.to_owned()))
}

/// `bytes` in base64 (RFC 4648, with padding).
fn base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { DIGITS[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::epaint::Vertex;

    #[test]
    fn a_picture_of_ink_and_text() {
        use tratto_core::model::{Point, StrokeKind};
        // A line across the top, a text box below it.
        let pts = (0..20).map(|i| Point::new(10.0 + i as f32 * 5.0, 10.0, 1.0)).collect();
        let stroke = Stroke::new(1, 0, StrokeKind::Pen, [0, 0, 255, 255], 4.0, pts);
        let text = TextBox {
            id: 2,
            page: 0,
            x: 10.0,
            y: 30.0,
            width: 100.0,
            size: 16.0,
            color: [0, 0, 0],
            text: "Hello".into(),
            lines: vec!["Hello".into()],
            line_h: 20.0,
            layout: None,
        };
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(120.0, 60.0));
        let snap = Snapshot {
            strokes: vec![stroke],
            texts: vec![text],
            images: Vec::new(),
            area,
            paper: false,
            alt: " ".into(),
        };
        let ctx = egui::Context::default();
        let mut made = None;
        crate::test_frame(&ctx, egui::RawInput::default(), |ui| made = Some(scene(ui.ctx(), &snap)));
        let image = rasterise(&made.unwrap());
        let [w, h] = image.size;
        assert_eq!([w, h], [360, 180], "3 pixels a unit");
        let px = |x: f32, y: f32| image.pixel((x * 3.0) as usize, (y * 3.0) as usize);
        // The line, blue and opaque; nothing where nothing was drawn.
        assert_eq!(px(50.0, 10.0), [0, 0, 255, 255]);
        // Nothing: transparent, and white for apps that ignore transparency.
        assert_eq!(px(50.0, 50.0), [255, 255, 255, 0]);
        // The text: some dark, opaque pixels in its box.
        let image = &image;
        let text_px = (30 * 3..50 * 3).flat_map(|y| (10 * 3..60 * 3).map(move |x| image.pixel(x, y)));
        assert!(text_px.filter(|c| c[3] > 200 && c[0] < 80).count() > 20, "the text is drawn");
    }

    #[test]
    fn base64_of_bytes() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn triangles_are_filled_with_their_colour() {
        let mut canvas = Canvas { size: [4, 4], px: vec![[0.0; 4]; 16] };
        let mut mesh = Mesh::default();
        let red = Color32::from_rgb(255, 0, 0);
        // The left half (2 x 4 pixels), as two triangles.
        for p in [pos2(0.0, 0.0), pos2(2.0, 0.0), pos2(2.0, 4.0), pos2(0.0, 4.0)] {
            mesh.vertices.push(Vertex { pos: p, uv: egui::epaint::WHITE_UV, color: red });
        }
        mesh.indices = vec![0, 1, 2, 0, 2, 3];
        mesh.texture_id = TextureId::User(1);
        let atlas = ColorImage::filled([1, 1], Color32::WHITE);
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(4.0, 4.0));
        draw_mesh(&mut canvas, &mesh, 1.0, clip, &atlas);
        let row: Vec<f32> = (0..4).map(|x| canvas.px[4 + x][0]).collect();
        assert_eq!(row, [1.0, 1.0, 0.0, 0.0], "only the left half, fully red");
        assert!(canvas.px.iter().all(|p| p[1] == 0.0 && p[2] == 0.0));
    }
}
