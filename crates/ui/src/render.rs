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
use std::time::Duration;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    Color32, ColorImage, FontId, Painter, Pos2, Rect, Shape, Stroke as UiStroke, StrokeKind as UiStrokeKind,
    TextureFilter, TextureHandle, TextureId, TextureOptions, Vec2, emath::TSTransform, epaint::Mesh, pos2, vec2,
};

use tratto_core::model::{
    self, Document, Image, ImageData, PageKind, Point, Stroke, StrokeKind, TextBox, TextLayout, TextMark,
};

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
/// Seconds the view stays still (no scroll, no zoom) before pictures are
/// made sharper than `PICTURE_MOVING`: a large texture uploaded mid-scroll
/// stalls a frame, and a zoom would decode every level it passes.
const PICTURE_SETTLE: f64 = 0.15;
/// The sharpest level made while the view moves: a texture this large
/// uploads quickly, and the file is decoded whole whatever the level.
const PICTURE_MOVING: u32 = PICTURE_LEVELS[2];

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

/// One level of a picture file decoded (`None`: it cannot be read), by its
/// key and the level asked for: the smallest comes first, on its own, when
/// asked for with it.
type Decoded = ((u64, u32), Option<(u32, ColorImage)>);

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
/// the sharpest texture of it there is stands in. While the view moves only
/// the smallest level is decoded and uploaded (see `PICTURE_SETTLE`).
pub struct PictureTextures {
    textures: HashMap<(u64, u32), PictureTexture>,
    /// Being decoded.
    decoding: HashSet<(u64, u32)>,
    unreadable: HashSet<u64>,
    decoded: (Sender<Decoded>, Receiver<Decoded>),
    /// Sharper levels decoded while the view moved: made textures once it
    /// is still.
    deferred: Vec<(u64, u32, ColorImage)>,
    /// The view (scroll, zoom) of the last frame, and when it last changed.
    view: Option<(Vec2, f32)>,
    moved_at: f64,
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
            deferred: Vec::new(),
            view: None,
            moved_at: f64::NEG_INFINITY,
            swept_at: 0.0,
            frame_at: 0.0,
            previous_frame_at: 0.0,
        }
    }
}

impl PictureTextures {
    /// Once per frame, before painting, with the view's `scroll` and
    /// `scale`: notes whether it moves, and drops the textures unpainted for
    /// `PICTURE_TTL`. What the previous frame painted is still on screen and
    /// stays, however long ago that frame was: the window only repaints on a
    /// change, so after a pause every picture shown looked unpainted for
    /// long, and was dropped and decoded again.
    pub fn begin_frame(&mut self, ctx: &eframe::egui::Context, now: f64, scroll: Vec2, scale: f32) {
        if now != self.frame_at {
            self.previous_frame_at = self.frame_at;
            self.frame_at = now;
        }
        let view = Some((scroll, scale));
        if self.view.is_some() && self.view != view {
            self.moved_at = now;
        }
        self.view = view;
        // A frame once it is still, to make the pictures sharp.
        if let Some(left) = self.settling(now) {
            ctx.request_repaint_after(Duration::from_secs_f64(left));
        }
        self.update(ctx, now);
        if now - self.swept_at >= 1.0 {
            self.swept_at = now;
            let shown = self.previous_frame_at;
            self.textures.retain(|_, t| t.keep || t.used_at >= shown || now - t.used_at < PICTURE_TTL);
        }
    }

    /// While the view moves: the seconds left until it counts as still.
    fn settling(&self, now: f64) -> Option<f64> {
        let left = PICTURE_SETTLE - (now - self.moved_at);
        (left > 0.0).then_some(left)
    }

    /// Makes textures of the decoded pictures; the sharper levels only once
    /// the view is still.
    fn update(&mut self, ctx: &eframe::egui::Context, now: f64) {
        let moving = self.settling(now).is_some();
        let decoded: Vec<Decoded> = self.decoded.1.try_iter().collect();
        for ((key, want), level) in decoded {
            let Some((level, image)) = level else {
                self.decoding.remove(&(key, want));
                self.unreadable.insert(key);
                continue;
            };
            if level == want {
                self.decoding.remove(&(key, want));
            }
            if moving && level > PICTURE_MOVING {
                self.deferred.push((key, level, image));
            } else {
                self.load(ctx, key, level, image, now);
            }
        }
        if !moving {
            for (key, level, image) in std::mem::take(&mut self.deferred) {
                self.load(ctx, key, level, image, now);
            }
        }
    }

    fn load(&mut self, ctx: &eframe::egui::Context, key: u64, level: u32, image: ColorImage, now: f64) {
        // Mipmaps: smooth when shown smaller than decoded.
        let options = TextureOptions::LINEAR.with_mipmap_mode(Some(TextureFilter::Linear));
        let texture = ctx.load_texture(format!("picture-{key:x}-{level}"), image, options);
        // The smallest level of any picture (a picture smaller than it has
        // only one level).
        let keep = level <= PICTURE_LEVELS[0];
        self.textures.insert((key, level), PictureTexture { texture, keep, used_at: now });
    }

    /// Some level of the picture `key` is there (maybe waiting to be made a
    /// texture).
    fn has(&self, key: u64) -> bool {
        self.has_at_least(key, 0)
    }

    /// A level of the picture `key` at least `level` is there (maybe waiting
    /// to be made a texture).
    fn has_at_least(&self, key: u64, level: u32) -> bool {
        let at_least = |k: u64, l: u32| k == key && l >= level;
        self.textures.keys().any(|&(k, l)| at_least(k, l)) || self.deferred.iter().any(|&(k, l, _)| at_least(k, l))
    }

    /// The level to make of a picture `longest` pixels large, shown `shown`
    /// pixels large, while the view moves.
    fn moving_level(shown: f32, longest: u32) -> u32 {
        picture_level(shown, longest).min(PICTURE_MOVING)
    }

    /// Decodes `data` at `want` in the background (and the smallest level
    /// first, the first time), unless it is already being decoded: zooming
    /// through several levels decodes the one wanted once the previous is
    /// done, not every one.
    fn request(&mut self, ctx: &eframe::egui::Context, data: &Arc<ImageData>, want: u32) {
        let key = data.key;
        if self.decoding.iter().any(|(k, _)| *k == key) {
            return;
        }
        let smallest = picture_level(0.0, data.width.max(data.height));
        let levels: Vec<u32> = if self.has(key) || want == smallest { vec![want] } else { vec![smallest, want] };
        self.decoding.insert((key, want));
        let (data, done, ctx) = (data.clone(), self.decoded.0.clone(), ctx.clone());
        std::thread::spawn(move || {
            // Each level as soon as it is made: the smallest shows while the
            // sharper is still being made smaller.
            let mut sizes = levels.iter().copied();
            let read = data.decode_each(&levels, |rgba| {
                let size = [rgba.width() as usize, rgba.height() as usize];
                let level = sizes.next().unwrap_or(want);
                let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                // The note may have been closed meanwhile: then nobody waits.
                if done.send(((key, want), Some((level, image)))).is_ok() {
                    ctx.request_repaint();
                }
            });
            if read.is_err() && done.send(((key, want), None)).is_ok() {
                ctx.request_repaint();
            }
        });
    }

    /// The pictures `images` (those next to the ones shown, `scale` points
    /// per document unit), at the level made while the view moves: there at
    /// once when scrolled into view.
    pub fn prefetch<'i>(&mut self, ctx: &eframe::egui::Context, images: impl Iterator<Item = &'i Image>, scale: f32) {
        let ppp = ctx.pixels_per_point();
        for img in images {
            let key = img.data.key;
            let shown = img.width.max(img.height) * scale * ppp;
            let level = Self::moving_level(shown, img.data.width.max(img.data.height));
            if !self.unreadable.contains(&key) && !self.has_at_least(key, level) {
                self.request(ctx, &img.data, level);
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
        // While the view moves, no sharper than `PICTURE_MOVING`.
        if self.settling(now).is_some() {
            let level = Self::moving_level(shown, longest);
            if !self.has_at_least(key, level) {
                self.request(ctx, data, level);
            }
        } else if !self.textures.contains_key(&(key, want)) {
            self.request(ctx, data, want);
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
    /// on screen, `scale` points per document unit, decoded for the zoom
    /// `made` (the zoom the page is made at, see `zoom`). `moved` are
    /// pictures shown at another place (being moved or resized).
    pub fn paint_page(
        &mut self,
        painter: &Painter,
        images: &[Image],
        page: u32,
        origin: Pos2,
        (scale, made): (f32, f32),
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
            let shown = rect.width().max(rect.height()) * painter.ctx().pixels_per_point() * made / scale;
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
/// A batch whose strokes were removed is compacted in place, its new strokes
/// at the end made, unless more than this many of them are new or changed.
const COMPACT_MAX_NEW: usize = 8;

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
/// selected strokes of a page while they are moved, or (`part`) those of
/// one part of a page, shown in the same frame as its other parts.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BatchKey {
    page: u32,
    layer: Layer,
    moved: bool,
    part: Option<u32>,
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
/// keep them, a change of pixels-per-point rebuilds them. They are made at
/// the zoom asked for, which while the view zooms is not the one shown (see
/// `zoom`): they are drawn scaled then. A batch is redone when its strokes
/// change: strokes added at the end are just added (and only they are
/// uploaded); otherwise the triangles of the strokes that did not change are
/// copied, not made again.
#[derive(Default)]
pub struct StrokeCache {
    batches: HashMap<BatchKey, PageBatch>,
    ppp_bits: u32,
    frame: u64,
}

impl StrokeCache {
    pub fn clear(&mut self) {
        self.batches.clear();
    }

    pub fn begin_frame(&mut self, ppp: f32) {
        let p = ppp.to_bits();
        if p != self.ppp_bits {
            self.batches.clear();
            self.ppp_bits = p;
        }
        self.frame += 1;
        if self.frame.is_multiple_of(256) {
            let f = self.frame;
            self.batches.retain(|_, b| f - b.last_used < CACHE_TTL_FRAMES);
        }
    }

    /// The batch `key` holding exactly `strokes`, in this order, made at
    /// `scale`.
    fn batch<'s>(&mut self, key: BatchKey, strokes: impl Iterator<Item = &'s Stroke>, scale: f32, aa: f32) -> &Batch {
        let strokes: Vec<&Stroke> = strokes.collect();
        let same = |p: &Part, s: &&Stroke| p.id == s.id && p.rev == s.rev;
        // Made at another zoom: made again at this one.
        if self.batches.get(&key).is_some_and(|e| e.scale != scale) {
            self.batches.remove(&key);
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
            return &entry.batch;
        }
        // The triangles of the strokes before `kept` stay where they are, on
        // the GPU too: only the rest is uploaded again.
        let (v0, i0) = (entry.parts[kept].vertices.start as usize, entry.parts[kept].indices.start as usize);
        let tail = &entry.parts[kept..];
        let rest = &strokes[kept..];
        // Strokes removed (erased, a stroke undone), then perhaps some added
        // at the end: the triangles of the others move down in place.
        let mut matched = Vec::with_capacity(rest.len());
        let mut parts = tail.iter().enumerate();
        for s in rest {
            match parts.find(|(_, p)| same(p, s)) {
                Some((j, _)) => matched.push(j),
                None => break,
            }
        }
        if rest.len() - matched.len() <= COMPACT_MAX_NEW {
            let added = &rest[matched.len()..];
            let mut new_parts = Vec::with_capacity(matched.len() + added.len());
            entry.batch.edit(v0, i0, |mesh| {
                let (mut v, mut i) = (v0 as u32, i0 as u32);
                for &j in &matched {
                    let p = &tail[j];
                    new_parts.push(move_part(mesh, p, v, i));
                    (v, i) = (v + p.vertices.len() as u32, i + p.indices.len() as u32);
                }
                mesh.vertices.truncate(v as usize);
                mesh.indices.truncate(i as usize);
                new_parts.extend(added.iter().map(|s| tessellate_part(mesh, s, scale, aa)));
            });
            entry.parts.truncate(kept);
            entry.parts.extend(new_parts);
            return &entry.batch;
        }
        // Changed otherwise (strokes moved, put back in the middle): the rest
        // made again, the triangles of the unchanged strokes copied.
        let old_mesh = entry.batch.mesh();
        let old: HashMap<(u64, u32), &Part> = tail.iter().map(|p| ((p.id, p.rev), p)).collect();
        let mut mesh = Mesh::default();
        // About as large as before: no growing on the way.
        mesh.reserve_vertices(old_mesh.vertices.len() - v0);
        mesh.reserve_triangles((old_mesh.indices.len() - i0) / 3);
        let new_parts: Vec<Part> = rest
            .iter()
            .map(|s| {
                let mut part = match old.get(&(s.id, s.rev)) {
                    Some(p) => copy_part(&mut mesh, old_mesh, p),
                    None => tessellate_part(&mut mesh, s, scale, aa),
                };
                part.vertices = part.vertices.start + v0 as u32..part.vertices.end + v0 as u32;
                part.indices = part.indices.start + i0 as u32..part.indices.end + i0 as u32;
                part
            })
            .collect();
        entry.batch.edit(v0, i0, |m| {
            m.vertices.truncate(v0);
            m.indices.truncate(i0);
            m.vertices.extend_from_slice(&mesh.vertices);
            m.indices.extend(mesh.indices.iter().map(|i| i + v0 as u32));
        });
        entry.parts.truncate(kept);
        entry.parts.extend(new_parts);
        &entry.batch
    }
}

/// Moves the triangles of `part` down to `vertices` and `indices` in `mesh`
/// (at or before where they are).
fn move_part(mesh: &mut Mesh, part: &Part, vertices: u32, indices: u32) -> Part {
    let (v, i) = (part.vertices.start, part.indices.start);
    let (nv, ni) = (part.vertices.len() as u32, part.indices.len() as u32);
    if v != vertices {
        mesh.vertices.copy_within(v as usize..(v + nv) as usize, vertices as usize);
    }
    if v != vertices || i != indices {
        let shift = v - vertices;
        for k in 0..ni as usize {
            mesh.indices[indices as usize + k] = mesh.indices[i as usize + k] - shift;
        }
    }
    Part { id: part.id, rev: part.rev, vertices: vertices..vertices + nv, indices: indices..indices + ni }
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
/// (see [`StrokeCache`]), made at `view.made`. Strokes in `moved` are left
/// out there and drawn as a batch of their own, at the drag offset (nothing
/// is redone while dragging).
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
    cache.begin_frame(ppp);
    let (made, k) = (view.made, view.scale / view.made);
    let is_moved = |id: u64| moved.is_some_and(|m| m.ids.contains(&id));
    for page in view.visible_pages() {
        let in_page = strokes.iter().filter(|s| s.page == page && Layer::of(s.kind) == layer && !is_moved(s.id));
        let batch = cache.batch(BatchKey { page, layer, moved: false, part: None }, in_page, made, aa);
        stroke_gpu::paint_batch(painter, batch, view.page_origin_screen(page).to_vec2(), k, layer.blend());
    }
    if let Some(m) = moved {
        let selected =
            strokes.iter().filter(|s| s.page == m.page && Layer::of(s.kind) == layer && m.ids.contains(&s.id));
        let batch = cache.batch(BatchKey { page: m.page, layer, moved: true, part: None }, selected, made, aa);
        let offset = view.page_origin_screen(m.page).to_vec2() + m.offset * view.scale;
        stroke_gpu::paint_batch(painter, batch, offset, k, layer.blend());
    }
}

/// Paints one page's strokes at an arbitrary scale (page previews), made
/// at `made` (see `zoom`). The caller must have called
/// `cache.begin_frame(ppp)` this frame; `offset` is the screen position of
/// the page's top-left corner.
#[allow(clippy::too_many_arguments)]
pub fn paint_page_strokes(
    painter: &Painter,
    offset: Vec2,
    scale: f32,
    made: f32,
    strokes: &[Stroke],
    page: u32,
    cache: &mut StrokeCache,
    aa: f32,
) {
    // Highlighters first, as on the page.
    for layer in [Layer::Highlight, Layer::Ink] {
        let in_page = strokes.iter().filter(|s| s.page == page && Layer::of(s.kind) == layer);
        let batch = cache.batch(BatchKey { page, layer, moved: false, part: None }, in_page, made, aa);
        stroke_gpu::paint_batch(painter, batch, offset, scale / made, layer.blend());
    }
}

/// Paints the strokes of `page` that touch `within` (page space), as
/// `paint_page_strokes` does, in batches of their own (`part`): the parts
/// of one page shown at once, as the pages view shows the pages of a PDF on
/// an infinite page (a batch is drawn at one place a frame).
#[allow(clippy::too_many_arguments)]
pub fn paint_page_part_strokes(
    painter: &Painter,
    (offset, scale, made): (Vec2, f32, f32),
    strokes: &[Stroke],
    (page, within, part): (u32, Rect, u32),
    cache: &mut StrokeCache,
    aa: f32,
) {
    let touches = |s: &Stroke| s.rect().expand(s.width * 0.5).intersects(within);
    for layer in [Layer::Highlight, Layer::Ink] {
        let in_part = strokes.iter().filter(|s| s.page == page && Layer::of(s.kind) == layer && touches(s));
        let batch = cache.batch(BatchKey { page, layer, moved: false, part: Some(part) }, in_part, made, aa);
        stroke_gpu::paint_batch(painter, batch, offset, scale / made, layer.blend());
    }
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
        paint_text_box(painter, pos, (view.scale, view.made), t, color);
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
///
/// The text is laid out at the zoom `made` and drawn scaled to `scale`
/// (see `zoom`): zooming lays out (and rasterises glyphs) at no new size.
pub fn paint_text_box(painter: &Painter, pos: Pos2, scales: (f32, f32), t: &TextBox, color: Color32) {
    painter.extend(text_box_shapes(painter, pos, scales, t, color));
}

/// What `paint_text_box` paints.
fn text_box_shapes(painter: &Painter, pos: Pos2, (scale, made): (f32, f32), t: &TextBox, color: Color32) -> Vec<Shape> {
    let mut shapes = Vec::new();
    if t.size * scale < 2.0 {
        return shapes;
    }
    // Made at `made` from `pos`, then scaled about `pos` to `scale`.
    let k = scale / made;
    let to_shown = TSTransform::new(pos.to_vec2() * (1.0 - k), k);
    let clip = to_shown.inverse() * painter.clip_rect();
    match &t.layout {
        Some(layout) => text_layout_shapes(&mut shapes, painter, pos, made, clip, t, layout, color),
        None => {
            let line_h = t.line_h * made;
            for (i, line) in t.lines.iter().enumerate() {
                let y = pos.y + i as f32 * line_h;
                if line.is_empty() || y > clip.bottom() || y + line_h < clip.top() {
                    continue;
                }
                let galley = painter.layout_no_wrap(line.clone(), FontId::proportional(t.size * made), color);
                shapes.push(Shape::galley(pos2(pos.x, y), galley, color));
            }
        }
    }
    if k != 1.0 {
        for shape in &mut shapes {
            shape.transform(to_shown);
        }
    }
    shapes
}

/// The shapes of a text box laid out (see `paint_text_box`), `scale` points
/// per document unit, those out of `clip` left out.
#[allow(clippy::too_many_arguments)]
fn text_layout_shapes(
    shapes: &mut Vec<Shape>,
    painter: &Painter,
    pos: Pos2,
    scale: f32,
    clip: Rect,
    t: &TextBox,
    layout: &TextLayout,
    color: Color32,
) {
    let at = |x: f32, y: f32| pos + vec2(x, y) * scale;
    let thin = UiStroke::new((t.size * scale * 0.06).max(1.0), color);
    // Code blocks' grey first, under everything.
    for mark in &layout.marks {
        if let TextMark::Shade { x, top, width, height } = *mark {
            let r = Rect::from_min_max(at(x, top), at(x + width, top + height));
            shapes.push(Shape::rect_filled(r, 3.0, Color32::from_black_alpha(15)));
        }
    }
    for mark in &layout.marks {
        match *mark {
            TextMark::Line { x0, y0, x1, y1 } => {
                let stroke = UiStroke::new(thin.width, color.gamma_multiply(0.5));
                shapes.push(Shape::line_segment([at(x0, y0), at(x1, y1)], stroke));
            }
            TextMark::Shade { .. } => {}
            TextMark::Quote { x, top, bottom } => {
                let bar = Rect::from_min_max(at(x, top), at(x + t.size * 0.18, bottom));
                shapes.push(Shape::rect_filled(bar, 1.0, color.gamma_multiply(0.45)));
            }
            TextMark::Check { x, top, size, checked } => {
                let r = Rect::from_min_size(at(x, top), vec2(size, size) * scale);
                shapes.push(Shape::rect_stroke(r, 2.0, thin, UiStrokeKind::Inside));
                if checked {
                    let tick =
                        [(0.22, 0.52), (0.42, 0.72), (0.78, 0.28)].map(|(u, v)| at(x + u * size, top + v * size));
                    shapes.push(Shape::line(tick.to_vec(), thin));
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
            shapes.push(Shape::rect_filled(bg, 2.0, Color32::from_black_alpha(15)));
        }
        let format = TextFormat {
            font_id: markdown::font(run.style, run.size * scale),
            color,
            italics: markdown::slanted(run.style),
            ..Default::default()
        };
        let galley = painter.layout_job(LayoutJob::single_section(run.text.clone(), format));
        shapes.push(Shape::galley(at(run.x, run.baseline - run.ascent), galley, color));
        if run.style.strike || run.style.underline {
            let y = if run.style.strike { run.baseline - run.size * 0.3 } else { run.baseline + run.size * 0.12 };
            shapes.push(Shape::line_segment([at(run.x, y), at(run.x + run.width, y)], thin));
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

/// Fills the area around the pages and the paper of every visible page; an
/// infinite page is all the view shows, without edges.
pub fn paint_paper(painter: &Painter, view: &View) {
    if view.layout.canvas {
        painter.rect_filled(view.rect, 0.0, color::page());
        return;
    }
    painter.rect_filled(view.rect, 0.0, color::desk());
    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        // Soft drop shadow, then the paper.
        painter.rect_filled(rect.translate(vec2(0.0, 3.0)).expand(1.0), 2.0, Color32::from_black_alpha(60));
        painter.rect_filled(rect, 0.0, color::page());
    }
}

/// Dots of dotted paper closer than this on screen (points) are not drawn:
/// they would only grey the page, and cost a shape each.
const MIN_DOT_GAP: f32 = 10.0;

/// The page-local positions `k * spacing` (`k >= 1`; any `k` on an
/// infinite page, written on before its corner too) from `from` to `to` and
/// before `end`, the end of the page.
fn rule_steps(spacing: f32, from: f32, to: f32, end: f32, canvas: bool) -> impl Iterator<Item = f32> {
    let first = (from / spacing).ceil();
    let first = if canvas { first } else { first.max(1.0) } as i64;
    (first..).map(move |k| k as f32 * spacing).take_while(move |v| *v <= to && *v < end)
}

/// The rules, grid or dots of the pages that have them (`Document::ruled`)
/// and the edge of every visible page, over the highlighters. Only what is
/// in view is drawn (a canvas is one very large page).
pub fn paint_rules(painter: &Painter, view: &View, doc: &Document) {
    let l = view.layout;
    let s = view.scale;
    let style = &doc.page;
    let spacing = style.spacing.max(8.0);
    let rule = UiStroke::new(1.0, color::page_rule());

    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        // An infinite page goes left of and above its corner too.
        let b = l.page_box();
        let paper = Rect::from_min_max(rect.min + b.min.to_vec2() * s, rect.min + b.max.to_vec2() * s);
        let shown = paper.intersect(view.rect);
        if doc.ruled(page) && shown.is_positive() {
            // The part of the page in view, page-local.
            let (lo, hi) = ((shown.min - rect.min) / s, (shown.max - rect.min) / s);
            let ys = || rule_steps(spacing, lo.y, hi.y, l.page_h, l.canvas);
            let xs = || rule_steps(spacing, lo.x, hi.x, l.page_w, l.canvas);
            match style.kind {
                PageKind::Dotted if spacing * s >= MIN_DOT_GAP => {
                    let (r, dot) = ((model::DOT_RADIUS * s).clamp(0.9, 2.2), color::page_dot());
                    for y in ys() {
                        for x in xs() {
                            painter.circle_filled(rect.min + vec2(x, y) * s, r, dot);
                        }
                    }
                }
                PageKind::Dotted | PageKind::Blank => {}
                PageKind::Ruled | PageKind::Grid => {
                    for y in ys() {
                        let sy = rect.top() + y * s;
                        painter.line_segment([pos2(shown.left(), sy), pos2(shown.right(), sy)], rule);
                    }
                    if style.kind == PageKind::Grid {
                        for x in xs() {
                            let sx = rect.left() + x * s;
                            painter.line_segment([pos2(sx, shown.top()), pos2(sx, shown.bottom())], rule);
                        }
                    }
                }
            }
        }
        if !l.canvas {
            painter.rect_stroke(rect, 0.0, UiStroke::new(1.0, color::page_edge()), UiStrokeKind::Middle);
        }
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
    use std::time::Instant;
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
        tex.begin_frame(&ctx, PICTURE_TTL + 1.0, Vec2::ZERO, 1.0);
        assert_eq!(levels(&tex), [256, 1000], "on screen: kept");

        // Out of view (the frames go on without it) for a while: only the
        // small one stays.
        tex.begin_frame(&ctx, PICTURE_TTL + 2.0, Vec2::ZERO, 1.0);
        tex.begin_frame(&ctx, PICTURE_TTL + 3.0, Vec2::ZERO, 1.0);
        assert_eq!(levels(&tex), [256]);
    }

    #[test]
    fn pictures_get_sharp_once_the_view_is_still() {
        let ctx = eframe::egui::Context::default();
        let data = ImageData::from_rgba(3000, 1500, vec![200; 3000 * 1500 * 4]).unwrap();
        let mut tex = PictureTextures::default();
        let wait = |tex: &mut PictureTextures, now: f64| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !tex.decoding.is_empty() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                tex.update(&ctx, now);
            }
        };
        tex.begin_frame(&ctx, 0.0, Vec2::ZERO, 1.0);
        // Scrolled: no sharper than `PICTURE_MOVING`, however large shown.
        tex.begin_frame(&ctx, 0.0, vec2(0.0, 50.0), 1.0);
        let _ = tex.texture(&ctx, &data, 2500.0);
        wait(&mut tex, 0.0);
        let mut levels: Vec<u32> = tex.textures.keys().map(|(_, l)| *l).collect();
        levels.sort();
        assert_eq!(levels, [256, 1024]);
        // Still (egui's time is 0 in a test context, so the move is moved back).
        tex.moved_at = -PICTURE_SETTLE;
        let _ = tex.texture(&ctx, &data, 2500.0);
        wait(&mut tex, 0.0);
        let mut levels: Vec<u32> = tex.textures.keys().map(|(_, l)| *l).collect();
        levels.sort();
        assert_eq!(levels, [256, 1024, 2048]);
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
        let key = BatchKey { page: 0, layer: Layer::Ink, moved: false, part: None };
        let mut strokes: Vec<Stroke> = (0..5).map(|i| stroke(i, i as f32 * 40.0, StrokeKind::Pen)).collect();
        let mut cache = StrokeCache::default();
        let check = |strokes: &[Stroke], cache: &mut StrokeCache| {
            cache.begin_frame(1.0);
            let mesh = cache.batch(key, strokes.iter(), 1.5, 1.0).mesh().clone();
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
        // The last one undone; the first erased; one erased and one written.
        strokes.pop();
        check(&strokes, &mut cache);
        strokes.remove(0);
        check(&strokes, &mut cache);
        strokes.remove(1);
        strokes.push(stroke(10, 340.0, StrokeKind::Pen));
        check(&strokes, &mut cache);
        // Many written in the middle (a paste): made again.
        let pasted: Vec<Stroke> = (20..32).map(|i| stroke(i, i as f32 * 7.0, StrokeKind::Pen)).collect();
        strokes.splice(1..1, pasted);
        check(&strokes, &mut cache);
        // Every other one erased.
        let mut n = 0;
        strokes.retain(|_| {
            n += 1;
            n % 2 == 0
        });
        check(&strokes, &mut cache);
        // Everything gone.
        check(&[], &mut cache);
    }

    #[test]
    fn batches_are_made_again_at_another_zoom() {
        let key = BatchKey { page: 0, layer: Layer::Ink, moved: false, part: None };
        let strokes: Vec<Stroke> = (0..3).map(|i| stroke(i, i as f32 * 40.0, StrokeKind::Pen)).collect();
        let mut cache = StrokeCache::default();
        cache.begin_frame(1.0);
        cache.batch(key, strokes.iter(), 1.0, 1.0);
        let batch = cache.batch(key, strokes.iter(), 2.0, 1.0);
        let expected = fresh(&strokes.iter().collect::<Vec<_>>(), 2.0);
        assert_eq!(batch.mesh().vertices, expected.vertices);
    }

    #[test]
    fn text_is_laid_out_at_the_zoom_made_and_drawn_scaled() {
        let ctx = eframe::egui::Context::default();
        let t = TextBox {
            id: 1,
            page: 0,
            x: 0.0,
            y: 0.0,
            width: 300.0,
            size: 20.0,
            color: [0, 0, 0],
            text: "Hello".into(),
            lines: vec!["Hello".into()],
            line_h: 24.0,
            layout: None,
        };
        // The bounds of what a box paints at `scales` (shown, made).
        let bounds = |scales: (f32, f32)| {
            let mut rect = Rect::NOTHING;
            crate::test_frame(&ctx, eframe::egui::RawInput::default(), |ui| {
                for s in text_box_shapes(ui.painter(), pos2(10.0, 10.0), scales, &t, Color32::BLACK) {
                    rect = rect.union(s.visual_bounding_rect());
                }
            });
            rect
        };
        let sharp = bounds((2.0, 2.0));
        let scaled = bounds((2.0, 1.0));
        // Laid out at half the size, then drawn twice as large: about where
        // and as large as laid out at that size.
        assert!((scaled.min - sharp.min).length() < 2.0, "{scaled:?} {sharp:?}");
        assert!((scaled.width() - sharp.width()).abs() < sharp.width() * 0.1, "{scaled:?} {sharp:?}");
    }
}
