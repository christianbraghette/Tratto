// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Painting: pages, cached anti-aliased stroke meshes, eraser cursor.
//!
//! A page is painted bottom to top: paper, the PDF page it is written on (if
//! any), rules, pictures, highlighter strokes, text boxes, pen strokes. Highlighters
//! are in the colour they leave on the paper (`model::highlight_on`) and
//! darken what is under them (`stroke_gpu`): overlapping highlighter
//! strokes never get darker, and nothing written or printed is covered.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    Align2, Color32, ColorImage, FontId, Painter, Pos2, Rect, Stroke as UiStroke, StrokeKind as UiStrokeKind,
    TextureFilter, TextureHandle, TextureId, TextureOptions, Vec2, epaint::Mesh, pos2, vec2,
};

use tratto_core::model::{self, Document, Image, ImageData, PageKind, Point, Stroke, StrokeKind, TextBox, TextMark};

use crate::markdown;
use crate::stroke_gpu::{self, Batch, Blend};
use crate::stroke_mesh::{self, LiveMesh};
use crate::theme::color;
use crate::view::View;

/// Pictures are decoded at one of these sizes (pixels on the longer side):
/// the smallest that covers the picture as shown, never larger than the
/// picture (a page preview needs a fraction of what the page does).
const PICTURE_LEVELS: [u32; 4] = [256, 512, 1024, 2048];
/// Seconds a picture's texture may go unpainted before it is dropped. The
/// smallest is kept while the note is open: shown at once when the picture
/// comes back into view, until the sharper one is decoded again.
const PICTURE_TTL: f64 = 30.0;

/// The size to decode a picture `longest` pixels on its longer side, shown
/// `shown` pixels large: the smallest level that covers it, at most the
/// picture itself.
fn picture_level(shown: f32, longest: u32) -> u32 {
    let level = PICTURE_LEVELS.iter().copied().find(|&l| l as f32 >= shown).unwrap_or(PICTURE_LEVELS[3]);
    level.min(longest.max(1))
}

/// A picture as it can be shown now.
enum Picture {
    /// Its file is being decoded (in the background).
    Decoding,
    Ready(TextureId),
    /// Its file cannot be read.
    Unreadable,
}

/// A picture file decoded for a level (and maybe the smallest too; `None`:
/// it cannot be read), by its key and that level.
type Decoded = ((u64, u32), Option<Vec<(u32, ColorImage)>>);

struct PictureTexture {
    texture: TextureHandle,
    /// The smallest level of its picture: never dropped.
    keep: bool,
    /// When it was last painted (egui's time, seconds).
    used_at: f64,
}

/// The pictures' textures, by their files' keys and levels. A file is
/// decoded in the background when first painted (a large photo would hold up
/// the window), at the level it is shown at, then made a texture; meanwhile
/// the sharpest texture of it there is stands in.
pub struct PictureTextures {
    textures: HashMap<(u64, u32), PictureTexture>,
    /// Being decoded.
    decoding: HashSet<(u64, u32)>,
    unreadable: HashSet<u64>,
    decoded: (Sender<Decoded>, Receiver<Decoded>),
    /// When unpainted textures were last dropped (egui's time).
    swept_at: f64,
    /// The time of this frame and of the one before (see `begin_frame`).
    frame_at: f64,
    previous_frame_at: f64,
}

impl Default for PictureTextures {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            decoding: HashSet::new(),
            unreadable: HashSet::new(),
            decoded: mpsc::channel(),
            swept_at: 0.0,
            frame_at: 0.0,
            previous_frame_at: 0.0,
        }
    }
}

impl PictureTextures {
    /// Once per frame, before painting: drops the textures unpainted for
    /// `PICTURE_TTL`. What the previous frame painted is still on screen and
    /// stays, however long ago that frame was: the window only repaints on a
    /// change, so after a pause every picture shown looked unpainted for
    /// long, and was dropped and decoded again.
    pub fn begin_frame(&mut self, ctx: &eframe::egui::Context, now: f64) {
        if now != self.frame_at {
            self.previous_frame_at = self.frame_at;
            self.frame_at = now;
        }
        self.update(ctx, now);
        if now - self.swept_at >= 1.0 {
            self.swept_at = now;
            let shown = self.previous_frame_at;
            self.textures.retain(|_, t| t.keep || t.used_at >= shown || now - t.used_at < PICTURE_TTL);
        }
    }

    /// Makes textures of the decoded pictures.
    fn update(&mut self, ctx: &eframe::egui::Context, now: f64) {
        for ((key, want), levels) in self.decoded.1.try_iter() {
            self.decoding.remove(&(key, want));
            let Some(levels) = levels else {
                self.unreadable.insert(key);
                continue;
            };
            for (level, image) in levels {
                // Mipmaps: smooth when shown smaller than decoded.
                let options = TextureOptions::LINEAR.with_mipmap_mode(Some(TextureFilter::Linear));
                let texture = ctx.load_texture(format!("picture-{key:x}-{level}"), image, options);
                // The smallest level of any picture (a picture smaller than it has
                // only one level).
                let keep = level <= PICTURE_LEVELS[0];
                self.textures.insert((key, level), PictureTexture { texture, keep, used_at: now });
            }
        }
    }

    /// The texture to paint `data` with, shown `shown` pixels large.
    fn texture(&mut self, ctx: &eframe::egui::Context, data: &Arc<ImageData>, shown: f32) -> Picture {
        let now = ctx.input(|i| i.time);
        self.update(ctx, now);
        let key = data.key;
        if self.unreadable.contains(&key) {
            return Picture::Unreadable;
        }
        let longest = data.width.max(data.height);
        let want = picture_level(shown, longest);
        let has = |textures: &HashMap<(u64, u32), PictureTexture>| textures.keys().any(|(k, _)| *k == key);
        // One decode of a picture at a time: zooming through several levels
        // decodes the one wanted once the previous is done, not every one.
        if !self.textures.contains_key(&(key, want)) && !self.decoding.iter().any(|(k, _)| *k == key) {
            // The first time, the smallest level too (from the same decode).
            let smallest = picture_level(0.0, longest);
            let levels: Vec<u32> =
                if has(&self.textures) || want == smallest { vec![want] } else { vec![want, smallest] };
            self.decoding.insert((key, want));
            let (data, done, ctx) = (data.clone(), self.decoded.0.clone(), ctx.clone());
            std::thread::spawn(move || {
                let images = data.decode_sizes(&levels).ok().map(|images| {
                    let made = images.iter().map(|rgba| {
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
                    });
                    levels.iter().copied().zip(made).collect()
                });
                // The note may have been closed meanwhile: then nobody waits.
                if done.send(((data.key, want), images)).is_ok() {
                    ctx.request_repaint();
                }
            });
        }
        // The level wanted, else the sharpest there is meanwhile.
        let best = self.textures.iter_mut().filter(|((k, _), _)| *k == key).max_by_key(|((_, l), _)| (*l == want, *l));
        match best {
            Some((_, t)) => {
                t.used_at = now;
                Picture::Ready(t.texture.id())
            }
            None => Picture::Decoding,
        }
    }

    /// Paints the pictures of `page`, whose top-left corner is at `origin`
    /// on screen, `scale` points per document unit. `moved` are pictures
    /// shown at another place (being moved or resized).
    pub fn paint_page(
        &mut self,
        painter: &Painter,
        images: &[Image],
        page: u32,
        origin: Pos2,
        scale: f32,
        moved: &[Image],
    ) {
        for img in images {
            // One moved where it is now, maybe on another page.
            let img = moved.iter().find(|m| m.id == img.id).unwrap_or(img);
            if img.page != page {
                continue;
            }
            let rect = Rect::from_min_size(origin + vec2(img.x, img.y) * scale, vec2(img.width, img.height) * scale);
            if !rect.intersects(painter.clip_rect()) {
                continue;
            }
            let shown = rect.width().max(rect.height()) * painter.ctx().pixels_per_point();
            match self.texture(painter.ctx(), &img.data, shown) {
                Picture::Ready(tex) => {
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    painter.image(tex, rect, uv, Color32::WHITE);
                }
                // Its place, faintly, until it is there.
                Picture::Decoding => {
                    painter.rect_filled(rect, 0.0, Color32::from_gray(242));
                }
                // A file that cannot be read: its place, greyed.
                Picture::Unreadable => {
                    painter.rect_filled(rect, 0.0, Color32::from_gray(225));
                }
            }
        }
    }

    /// Forgets every texture (another note). Decodes still running are
    /// forgotten too: their results are dropped.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Cache entries unused for this many frames are dropped (bounds memory).
const CACHE_TTL_FRAMES: u64 = 512;
/// While zooming, batches are drawn scaled; they are made again at the new
/// zoom once it has not changed for this long.
const ZOOM_SETTLE: Duration = Duration::from_millis(150);

/// The two stroke layers of a page (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Highlighter strokes, under the rules and the text.
    Highlight,
    /// Pen strokes, on top.
    Ink,
}

impl Layer {
    pub fn of(kind: StrokeKind) -> Self {
        match kind {
            StrokeKind::Highlighter => Layer::Highlight,
            StrokeKind::Pen => Layer::Ink,
        }
    }

    fn blend(self) -> Blend {
        match self {
            Layer::Highlight => Blend::Darken,
            Layer::Ink => Blend::Over,
        }
    }
}

/// The colour a stroke is painted with: highlighters as the opaque colour
/// they leave on the paper.
pub fn paint_rgba(kind: StrokeKind, rgba: [u8; 4]) -> [u8; 4] {
    match kind {
        StrokeKind::Highlighter => {
            let [r, g, b] = model::highlight_on(rgba, model::PAPER_RGB);
            [r, g, b, 255]
        }
        StrokeKind::Pen => rgba,
    }
}

// ------------------------------------------------------------------ cache

/// One batch of the cache: the strokes of one layer of a page, or the
/// selected strokes of a page while they are moved.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BatchKey {
    page: u32,
    layer: Layer,
    moved: bool,
}

/// Where a stroke's triangles are in its batch.
struct Part {
    id: u64,
    /// `Stroke::rev` the triangles were made for.
    rev: u32,
    vertices: Range<u32>,
    indices: Range<u32>,
}

struct PageBatch {
    batch: Batch,
    /// The zoom it was made at.
    scale: f32,
    /// The strokes in the batch, in paint order.
    parts: Vec<Part>,
    last_used: u64,
}

/// The strokes as batches kept on the GPU (see `stroke_gpu`), at
/// `page_space * scale` without translation: scrolling and page-size changes
/// keep them, a change of pixels-per-point rebuilds them. While zooming they
/// are drawn scaled (nothing is made again at every step) and rebuilt once
/// the zoom settles. A batch is redone when its strokes change: strokes added
/// at the end are just added (and only they are uploaded); otherwise the
/// triangles of the strokes that did not change are copied, not made again.
#[derive(Default)]
pub struct StrokeCache {
    batches: HashMap<BatchKey, PageBatch>,
    scale_bits: u32,
    /// When the zoom last changed (`None`: not since the cache was made).
    zoomed_at: Option<Instant>,
    ppp_bits: u32,
    frame: u64,
}

impl StrokeCache {
    pub fn clear(&mut self) {
        self.batches.clear();
    }

    pub fn begin_frame(&mut self, scale: f32, ppp: f32) {
        let (s, p) = (scale.to_bits(), ppp.to_bits());
        if p != self.ppp_bits {
            self.batches.clear();
            self.ppp_bits = p;
        }
        if s != self.scale_bits {
            self.scale_bits = s;
            self.zoomed_at = Some(Instant::now());
        }
        self.frame += 1;
        if self.frame.is_multiple_of(256) {
            let f = self.frame;
            self.batches.retain(|_, b| f - b.last_used < CACHE_TTL_FRAMES);
        }
    }

    /// The zoom is changing (it changed less than `ZOOM_SETTLE` ago).
    fn zooming(&self) -> bool {
        self.zoomed_at.is_some_and(|t| t.elapsed() < ZOOM_SETTLE)
    }

    /// While zooming: a frame once the zoom has settled, to make the batches
    /// drawn scaled again at the new zoom (sharp, with the right edges).
    fn settle_later(&self, painter: &Painter) {
        if let Some(left) = self.zoomed_at.and_then(|t| ZOOM_SETTLE.checked_sub(t.elapsed())) {
            painter.ctx().request_repaint_after(left);
        }
    }

    /// The batch `key` holding exactly `strokes`, in this order, and how much
    /// larger than made to draw it: made at `scale`, or, while zooming, an
    /// unchanged batch made at another zoom.
    fn batch<'s>(
        &mut self,
        key: BatchKey,
        strokes: impl Iterator<Item = &'s Stroke>,
        scale: f32,
        aa: f32,
    ) -> (&Batch, f32) {
        let strokes: Vec<&Stroke> = strokes.collect();
        let same = |p: &Part, s: &&Stroke| p.id == s.id && p.rev == s.rev;
        let zooming = self.zooming();
        let other_zoom = self.batches.get(&key).filter(|e| e.scale != scale).map(|e| {
            zooming && e.parts.len() == strokes.len() && e.parts.iter().zip(&strokes).all(|(p, s)| same(p, s))
        });
        match other_zoom {
            Some(true) => {
                let entry = self.batches.get_mut(&key).expect("looked up above");
                entry.last_used = self.frame;
                return (&entry.batch, scale / entry.scale);
            }
            // Settled, or changed meanwhile: made again at this zoom.
            Some(false) => {
                self.batches.remove(&key);
            }
            None => {}
        }
        let entry = self.batches.entry(key).or_insert_with(|| PageBatch {
            batch: Batch::default(),
            scale,
            parts: Vec::new(),
            last_used: 0,
        });
        entry.last_used = self.frame;
        let kept = entry.parts.iter().zip(&strokes).take_while(|(p, s)| same(p, s)).count();
        if kept == entry.parts.len() {
            // Strokes added at the end, if any.
            if kept < strokes.len() {
                let parts = &mut entry.parts;
                entry.batch.append(|mesh| {
                    parts.extend(strokes[kept..].iter().map(|s| tessellate_part(mesh, s, scale, aa)));
                });
            }
        } else {
            let old_mesh = entry.batch.mesh();
            let old: HashMap<(u64, u32), &Part> = entry.parts.iter().map(|p| ((p.id, p.rev), p)).collect();
            let mut mesh = Mesh::default();
            // About as large as before: no growing on the way.
            mesh.reserve_vertices(old_mesh.vertices.len());
            mesh.reserve_triangles(old_mesh.indices.len() / 3);
            let parts = strokes
                .iter()
                .map(|s| match old.get(&(s.id, s.rev)) {
                    Some(p) => copy_part(&mut mesh, old_mesh, p),
                    None => tessellate_part(&mut mesh, s, scale, aa),
                })
                .collect();
            entry.batch.replace(mesh);
            entry.parts = parts;
        }
        (&entry.batch, 1.0)
    }
}

/// Adds the triangles of `s` to `mesh`.
fn tessellate_part(mesh: &mut Mesh, s: &Stroke, scale: f32, aa: f32) -> Part {
    let (v0, i0) = (mesh.vertices.len() as u32, mesh.indices.len() as u32);
    stroke_mesh::tessellate(mesh, s.kind, paint_rgba(s.kind, s.color), s.width, &s.points, scale, aa);
    Part { id: s.id, rev: s.rev, vertices: v0..mesh.vertices.len() as u32, indices: i0..mesh.indices.len() as u32 }
}

/// Adds the triangles of `part` of `from` to `mesh`.
fn copy_part(mesh: &mut Mesh, from: &Mesh, part: &Part) -> Part {
    let (v0, i0) = (mesh.vertices.len() as u32, mesh.indices.len() as u32);
    mesh.vertices.extend_from_slice(&from.vertices[part.vertices.start as usize..part.vertices.end as usize]);
    let shift = v0.wrapping_sub(part.vertices.start);
    let indices = &from.indices[part.indices.start as usize..part.indices.end as usize];
    mesh.indices.extend(indices.iter().map(|i| i.wrapping_add(shift)));
    Part {
        id: part.id,
        rev: part.rev,
        vertices: v0..mesh.vertices.len() as u32,
        indices: i0..mesh.indices.len() as u32,
    }
}

// ------------------------------------------------------------ strokes

/// The selected strokes while they are shown at an offset (being dragged).
pub struct Moved<'a> {
    pub ids: &'a HashSet<u64>,
    pub page: u32,
    /// Offset in content space.
    pub offset: Vec2,
}

/// Paints the strokes of one layer of all visible pages, one batch per page
/// (see [`StrokeCache`]). Strokes in `moved` are left out there and drawn
/// as a batch of their own, at the drag offset (nothing is redone while
/// dragging).
pub fn paint_strokes(
    painter: &Painter,
    view: &View,
    layer: Layer,
    strokes: &[Stroke],
    cache: &mut StrokeCache,
    ppp: f32,
    moved: Option<&Moved>,
) {
    let aa = 1.0 / ppp;
    cache.begin_frame(view.scale, ppp);
    let is_moved = |id: u64| moved.is_some_and(|m| m.ids.contains(&id));
    for page in view.visible_pages() {
        let in_page = strokes.iter().filter(|s| s.page == page && Layer::of(s.kind) == layer && !is_moved(s.id));
        let (batch, k) = cache.batch(BatchKey { page, layer, moved: false }, in_page, view.scale, aa);
        stroke_gpu::paint_batch(painter, batch, view.page_origin_screen(page).to_vec2(), k, layer.blend());
    }
    if let Some(m) = moved {
        let selected =
            strokes.iter().filter(|s| s.page == m.page && Layer::of(s.kind) == layer && m.ids.contains(&s.id));
        let (batch, k) = cache.batch(BatchKey { page: m.page, layer, moved: true }, selected, view.scale, aa);
        let offset = view.page_origin_screen(m.page).to_vec2() + m.offset * view.scale;
        stroke_gpu::paint_batch(painter, batch, offset, k, layer.blend());
    }
    cache.settle_later(painter);
}

/// Paints one page's strokes at an arbitrary scale (page previews). The
/// caller must have called `cache.begin_frame(scale, ppp)` this frame;
/// `offset` is the screen position of the page's top-left corner.
pub fn paint_page_strokes(
    painter: &Painter,
    offset: Vec2,
    scale: f32,
    strokes: &[Stroke],
    page: u32,
    cache: &mut StrokeCache,
    aa: f32,
) {
    // Highlighters first, as on the page.
    for layer in [Layer::Highlight, Layer::Ink] {
        let in_page = strokes.iter().filter(|s| s.page == page && Layer::of(s.kind) == layer);
        let (batch, k) = cache.batch(BatchKey { page, layer, moved: false }, in_page, scale, aa);
        stroke_gpu::paint_batch(painter, batch, offset, k, layer.blend());
    }
    cache.settle_later(painter);
}

/// Paints the text boxes in view with their stored line breaks.
///
/// * `skip`    – a box that is being edited (the live editor draws it instead)
/// * `moved`   – boxes shown at an offset (content space) while dragged
/// * `outline` – draw a faint frame around every box (text tool active)
pub fn paint_texts(
    painter: &Painter,
    view: &View,
    texts: &[TextBox],
    skip: Option<u64>,
    moved: &[(u64, Vec2)],
    outline: bool,
) {
    for t in texts.iter().filter(|t| Some(t.id) != skip) {
        let d = moved.iter().find(|(id, _)| *id == t.id).map_or(Vec2::ZERO, |(_, d)| *d);
        let pos = view.page_origin_screen(t.page) + (vec2(t.x, t.y) + d) * view.scale;
        let rect = Rect::from_min_size(pos, vec2(t.width, t.height()) * view.scale);
        if !rect.intersects(view.rect) {
            continue;
        }
        let color = Color32::from_rgb(t.color[0], t.color[1], t.color[2]);
        paint_text_box(painter, pos, view.scale, t, color);
        if outline {
            painter.rect_stroke(
                rect.expand(2.0),
                2.0,
                UiStroke::new(1.0, color::accent_alpha(110)),
                UiStrokeKind::Middle,
            );
        }
    }
}

/// Paints the text box `t` with its top-left corner at `pos`, `scale` points
/// per document unit: its Markdown as laid out (see `markdown`), or, not laid
/// out, its stored lines. Lines out of the painter's clip are skipped.
pub fn paint_text_box(painter: &Painter, pos: Pos2, scale: f32, t: &TextBox, color: Color32) {
    if t.size * scale < 2.0 {
        return;
    }
    let clip = painter.clip_rect();
    let Some(layout) = &t.layout else {
        let line_h = t.line_h * scale;
        for (i, line) in t.lines.iter().enumerate() {
            let y = pos.y + i as f32 * line_h;
            if line.is_empty() || y > clip.bottom() || y + line_h < clip.top() {
                continue;
            }
            painter.text(pos2(pos.x, y), Align2::LEFT_TOP, line, FontId::proportional(t.size * scale), color);
        }
        return;
    };
    let at = |x: f32, y: f32| pos + vec2(x, y) * scale;
    let thin = UiStroke::new((t.size * scale * 0.06).max(1.0), color);
    // Code blocks' grey first, under everything.
    for mark in &layout.marks {
        if let TextMark::Shade { x, top, width, height } = *mark {
            let r = Rect::from_min_max(at(x, top), at(x + width, top + height));
            painter.rect_filled(r, 3.0, Color32::from_black_alpha(15));
        }
    }
    for mark in &layout.marks {
        match *mark {
            TextMark::Line { x0, y0, x1, y1 } => {
                painter.line_segment([at(x0, y0), at(x1, y1)], UiStroke::new(thin.width, color.gamma_multiply(0.5)));
            }
            TextMark::Shade { .. } => {}
            TextMark::Quote { x, top, bottom } => {
                let bar = Rect::from_min_max(at(x, top), at(x + t.size * 0.18, bottom));
                painter.rect_filled(bar, 1.0, color.gamma_multiply(0.45));
            }
            TextMark::Check { x, top, size, checked } => {
                let r = Rect::from_min_size(at(x, top), vec2(size, size) * scale);
                painter.rect_stroke(r, 2.0, thin, UiStrokeKind::Inside);
                if checked {
                    let tick =
                        [(0.22, 0.52), (0.42, 0.72), (0.78, 0.28)].map(|(u, v)| at(x + u * size, top + v * size));
                    painter.line(tick.to_vec(), thin);
                }
            }
        }
    }
    for run in &layout.runs {
        let (top, bottom) = (at(0.0, run.top).y, at(0.0, run.top + run.line_h).y);
        if bottom < clip.top() || top > clip.bottom() {
            continue;
        }
        if run.style.code_span {
            let bg = Rect::from_min_max(
                at(run.x - 2.0, run.top + run.line_h * 0.08),
                at(run.x + run.width + 2.0, run.top + run.line_h * 0.92),
            );
            painter.rect_filled(bg, 2.0, Color32::from_black_alpha(15));
        }
        let format = TextFormat {
            font_id: markdown::font(run.style, run.size * scale),
            color,
            italics: markdown::slanted(run.style),
            ..Default::default()
        };
        let galley = painter.layout_job(LayoutJob::single_section(run.text.clone(), format));
        painter.galley(at(run.x, run.baseline - run.ascent), galley, color);
        if run.style.strike || run.style.underline {
            let y = if run.style.strike { run.baseline - run.size * 0.3 } else { run.baseline + run.size * 0.12 };
            painter.line_segment([at(run.x, y), at(run.x + run.width, y)], thin);
        }
    }
}

/// Text in a page preview: one thin bar per line (no font layout needed).
pub fn paint_page_texts(painter: &Painter, offset: Vec2, scale: f32, texts: &[TextBox], page: u32) {
    for t in texts.iter().filter(|t| t.page == page) {
        let color = Color32::from_rgba_unmultiplied(t.color[0], t.color[1], t.color[2], 150);
        let h = (t.size * 0.5 * scale).max(1.0);
        for (i, line) in t.lines.iter().enumerate() {
            let chars = line.chars().count();
            if chars == 0 {
                continue;
            }
            let w = (chars as f32 * t.size * 0.5).min(t.width) * scale;
            let top = offset.y + (t.y + i as f32 * t.line_h + t.line_h * 0.3) * scale;
            let left = offset.x + t.x * scale;
            painter.rect_filled(Rect::from_min_size(pos2(left, top), vec2(w, h)), 0.5, color);
        }
    }
}

/// The triangles of the stroke being drawn (see `LiveMesh`), for the zoom
/// they were made at.
#[derive(Default)]
pub struct LiveStroke {
    mesh: Option<((u32, u32), LiveMesh)>,
}

impl LiveStroke {
    /// Paints the stroke being drawn: `fixed` points (which only grow) and
    /// `tip` (see `StrokeBuilder`). Only its end is made again each frame.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &Painter,
        view: &View,
        page: u32,
        kind: StrokeKind,
        rgba: [u8; 4],
        width: f32,
        (fixed, tip): (&[Point], &[Point]),
        ppp: f32,
    ) {
        let key = (view.scale.to_bits(), ppp.to_bits());
        if self.mesh.as_ref().is_none_or(|(k, _)| *k != key) {
            // Zoomed while drawing: made again at the new scale.
            let mesh = LiveMesh::new(kind, paint_rgba(kind, rgba), width, view.scale, 1.0 / ppp);
            self.mesh = Some((key, mesh));
        }
        let Some((_, live)) = self.mesh.as_mut() else { return };
        let mut end = live.update(fixed, tip);
        let offset = view.page_origin_screen(page).to_vec2();
        let layer = Layer::of(kind);
        stroke_gpu::paint_batch(painter, live.fixed(), offset, 1.0, layer.blend());
        end.translate(offset);
        paint_mesh(painter, layer, end);
    }
}

/// Paints stroke triangles of `layer` for this frame only (screen points):
/// highlighters with the darken blend.
pub fn paint_mesh(painter: &Painter, layer: Layer, mesh: Mesh) {
    stroke_gpu::paint_mesh(painter, mesh, layer.blend());
}

// -------------------------------------------------------- background

/// Fills the area around the pages and the paper of every visible page.
pub fn paint_paper(painter: &Painter, view: &View) {
    painter.rect_filled(view.rect, 0.0, color::desk());
    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        // Soft drop shadow, then the paper.
        painter.rect_filled(rect.translate(vec2(0.0, 3.0)).expand(1.0), 2.0, Color32::from_black_alpha(60));
        painter.rect_filled(rect, 0.0, color::page());
    }
}

/// The rules (or grid) of the pages that have them (`Document::ruled`) and
/// the edge of every visible page, over the highlighters.
pub fn paint_rules(painter: &Painter, view: &View, doc: &Document) {
    let l = view.layout;
    let s = view.scale;
    let style = &doc.page;
    let spacing = style.spacing.max(8.0);
    let rule = UiStroke::new(1.0, color::page_rule());

    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        let ruled = doc.ruled(page);
        if ruled {
            // Rules at page-local y = k * spacing; only those in view are drawn.
            let vis_top = ((view.rect.top() - rect.top()) / s).max(0.0);
            let vis_bottom = ((view.rect.bottom() - rect.top()) / s).min(l.page_h);
            let mut k = ((vis_top / spacing).floor() as i64).max(1);
            loop {
                let ly = k as f32 * spacing;
                if ly >= l.page_h || ly > vis_bottom {
                    break;
                }
                let sy = rect.top() + ly * s;
                painter.line_segment([pos2(rect.left(), sy), pos2(rect.right(), sy)], rule);
                k += 1;
            }
        }
        if ruled && style.kind == PageKind::Grid {
            let (y0, y1) = (rect.top().max(view.rect.top()), rect.bottom().min(view.rect.bottom()));
            let mut x = spacing;
            while x < l.page_w {
                let sx = rect.left() + x * s;
                painter.line_segment([pos2(sx, y0), pos2(sx, y1)], rule);
                x += spacing;
            }
        }
        painter.rect_stroke(rect, 0.0, UiStroke::new(1.0, color::page_edge()), UiStrokeKind::Middle);
    }
}

/// The pen cursor (pen or highlighter hovering over a page): a dot in the
/// colour and width of the stroke the pen would draw.
pub fn paint_pen_cursor(painter: &Painter, center: Pos2, kind: StrokeKind, rgba: [u8; 4], width_px: f32) {
    let [r, g, b, _] = paint_rgba(kind, rgba);
    let radius = (width_px * 0.5).clamp(2.0, 40.0);
    // A faint ring keeps light colours visible on the paper.
    painter.circle(center, radius, Color32::from_rgb(r, g, b), UiStroke::new(1.0, Color32::from_black_alpha(70)));
}

pub fn paint_eraser_cursor(painter: &Painter, center: Pos2, radius_px: f32) {
    painter.circle_filled(center, radius_px, Color32::from_rgba_unmultiplied(120, 120, 120, 40));
    painter.circle_stroke(center, radius_px, UiStroke::new(1.5, Color32::from_gray(80)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tratto_core::model::Point;

    #[test]
    fn pictures_are_decoded_as_large_as_shown() {
        assert_eq!(picture_level(100.0, 4000), 256, "a page preview");
        assert_eq!(picture_level(700.0, 4000), 1024);
        assert_eq!(picture_level(1024.0, 4000), 1024);
        assert_eq!(picture_level(5000.0, 4000), 2048, "never past the largest level");
        assert_eq!(picture_level(700.0, 600), 600, "never larger than the picture");
        assert_eq!(picture_level(0.0, 100), 100);
    }

    #[test]
    fn picture_textures_follow_the_size_shown() {
        let ctx = eframe::egui::Context::default();
        let data = ImageData::from_rgba(1000, 500, vec![200; 1000 * 500 * 4]).unwrap();
        let mut tex = PictureTextures::default();
        // Waits for the decodes asked for, at `now`.
        let settle = |tex: &mut PictureTextures, now: f64| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !tex.decoding.is_empty() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                tex.update(&ctx, now);
            }
        };
        let levels = |tex: &PictureTextures| {
            let mut l: Vec<u32> = tex.textures.keys().map(|(_, l)| *l).collect();
            l.sort();
            l
        };

        // A page preview: decoded small, nothing shown meanwhile.
        assert!(matches!(tex.texture(&ctx, &data, 120.0), Picture::Decoding));
        settle(&mut tex, 0.0);
        assert_eq!(levels(&tex), [256]);

        // Zoomed in: the small one stands in while the sharp one is decoded.
        assert!(matches!(tex.texture(&ctx, &data, 900.0), Picture::Ready(_)));
        settle(&mut tex, 0.0);
        assert_eq!(levels(&tex), [256, 1000], "never larger than the picture");

        // A pause: the next frame comes long after, the picture still shown.
        tex.begin_frame(&ctx, PICTURE_TTL + 1.0);
        assert_eq!(levels(&tex), [256, 1000], "on screen: kept");

        // Out of view (the frames go on without it) for a while: only the
        // small one stays.
        tex.begin_frame(&ctx, PICTURE_TTL + 2.0);
        tex.begin_frame(&ctx, PICTURE_TTL + 3.0);
        assert_eq!(levels(&tex), [256]);
    }

    fn stroke(id: u64, x: f32, kind: StrokeKind) -> Stroke {
        let pts = (0..12).map(|i| Point::new(x + i as f32 * 3.0, (i as f32).sin() * 5.0, 0.5)).collect();
        Stroke::new(id, 0, kind, [10, 20, 30, 255], 2.0, pts)
    }

    /// The batch as made from scratch: what the cache must always hold.
    fn fresh(strokes: &[&Stroke], scale: f32) -> Mesh {
        let mut mesh = Mesh::default();
        for s in strokes {
            tessellate_part(&mut mesh, s, scale, 1.0);
        }
        mesh
    }

    #[test]
    fn batches_follow_the_strokes() {
        let key = BatchKey { page: 0, layer: Layer::Ink, moved: false };
        let mut strokes: Vec<Stroke> = (0..5).map(|i| stroke(i, i as f32 * 40.0, StrokeKind::Pen)).collect();
        let mut cache = StrokeCache::default();
        let check = |strokes: &[Stroke], cache: &mut StrokeCache| {
            cache.begin_frame(1.5, 1.0);
            let mesh = cache.batch(key, strokes.iter(), 1.5, 1.0).0.mesh().clone();
            let expected = fresh(&strokes.iter().collect::<Vec<_>>(), 1.5);
            assert_eq!((mesh.vertices, mesh.indices), (expected.vertices, expected.indices));
        };
        check(&strokes, &mut cache);
        // Written: appended.
        strokes.push(stroke(9, 300.0, StrokeKind::Pen));
        check(&strokes, &mut cache);
        // Erased in the middle, one moved, one put back elsewhere (undo).
        let erased = strokes.remove(1);
        strokes[2].translate(4.0, 1.0);
        check(&strokes, &mut cache);
        strokes.insert(3, erased);
        check(&strokes, &mut cache);
        // Everything gone.
        check(&[], &mut cache);
    }

    #[test]
    fn zooming_draws_batches_scaled_until_it_settles() {
        let key = BatchKey { page: 0, layer: Layer::Ink, moved: false };
        let strokes: Vec<Stroke> = (0..3).map(|i| stroke(i, i as f32 * 40.0, StrokeKind::Pen)).collect();
        let mut cache = StrokeCache::default();
        cache.begin_frame(1.0, 1.0);
        let made = cache.batch(key, strokes.iter(), 1.0, 1.0).0.mesh().clone();

        // Zooming: the same triangles, drawn twice as large.
        cache.begin_frame(2.0, 1.0);
        let (batch, k) = cache.batch(key, strokes.iter(), 2.0, 1.0);
        assert_eq!((batch.mesh().vertices.len(), k), (made.vertices.len(), 2.0));

        // Settled: made again at the new zoom.
        cache.zoomed_at = Some(Instant::now() - ZOOM_SETTLE);
        let (batch, k) = cache.batch(key, strokes.iter(), 2.0, 1.0);
        let expected = fresh(&strokes.iter().collect::<Vec<_>>(), 2.0);
        assert_eq!((&batch.mesh().vertices, k), (&expected.vertices, 1.0));
    }
}
