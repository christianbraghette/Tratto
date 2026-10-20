// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Painting: pages, cached anti-aliased stroke meshes, eraser cursor.
//!
//! A page is painted bottom to top: paper, the PDF page it is written on (if
//! any), highlighter strokes, rules, text boxes, pen strokes. Highlighters
//! are in the colour they leave on the paper (`model::highlight_on`) and
//! darken what is under them (`stroke_gpu`): overlapping highlighter
//! strokes never get darker, and nothing written or printed is covered.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use eframe::egui::{
    Align2, Color32, FontId, Painter, Pos2, Rect, Stroke as UiStroke, StrokeKind as UiStrokeKind, Vec2, epaint::Mesh,
    pos2, vec2,
};

use tratto_core::model::{self, PageKind, PageStyle, Point, Stroke, StrokeKind, TextBox};

use crate::stroke_gpu::{self, Batch, Blend};
use crate::stroke_mesh::{self, LiveMesh};
use crate::theme::color;
use crate::view::View;

/// Distinct colour of the non-writable area around the pages.
pub const PADDING_COLOR: Color32 = color::DESK;
pub const PAGE_COLOR: Color32 = color::PAGE;

/// Cache entries unused for this many frames are dropped (bounds memory).
const CACHE_TTL_FRAMES: u64 = 512;

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
    /// The strokes in the batch, in paint order.
    parts: Vec<Part>,
    last_used: u64,
}

/// The strokes as batches kept on the GPU (see `stroke_gpu`), at
/// `page_space * scale` without translation: scrolling and page-size changes
/// keep them, a change of zoom or pixels-per-point rebuilds them. A batch is
/// redone when its strokes change: strokes added at the end are just added
/// (and only they are uploaded); otherwise the triangles of the strokes that
/// did not change are copied, not made again.
#[derive(Default)]
pub struct StrokeCache {
    batches: HashMap<BatchKey, PageBatch>,
    scale_bits: u32,
    ppp_bits: u32,
    frame: u64,
}

impl StrokeCache {
    pub fn clear(&mut self) {
        self.batches.clear();
    }

    pub fn begin_frame(&mut self, scale: f32, ppp: f32) {
        let (s, p) = (scale.to_bits(), ppp.to_bits());
        if s != self.scale_bits || p != self.ppp_bits {
            self.batches.clear();
            self.scale_bits = s;
            self.ppp_bits = p;
        }
        self.frame += 1;
        if self.frame.is_multiple_of(256) {
            let f = self.frame;
            self.batches.retain(|_, b| f - b.last_used < CACHE_TTL_FRAMES);
        }
    }

    /// The batch `key` holding exactly `strokes`, in this order.
    fn batch<'s>(&mut self, key: BatchKey, strokes: impl Iterator<Item = &'s Stroke>, scale: f32, aa: f32) -> &Batch {
        let strokes: Vec<&Stroke> = strokes.collect();
        let entry = self.batches.entry(key).or_insert_with(|| PageBatch {
            batch: Batch::default(),
            parts: Vec::new(),
            last_used: 0,
        });
        entry.last_used = self.frame;
        let same = |p: &Part, s: &&Stroke| p.id == s.id && p.rev == s.rev;
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
        &entry.batch
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
        let batch = cache.batch(BatchKey { page, layer, moved: false }, in_page, view.scale, aa);
        stroke_gpu::paint_batch(painter, batch, view.page_origin_screen(page).to_vec2(), layer.blend());
    }
    if let Some(m) = moved {
        let selected =
            strokes.iter().filter(|s| s.page == m.page && Layer::of(s.kind) == layer && m.ids.contains(&s.id));
        let batch = cache.batch(BatchKey { page: m.page, layer, moved: true }, selected, view.scale, aa);
        let offset = view.page_origin_screen(m.page).to_vec2() + m.offset * view.scale;
        stroke_gpu::paint_batch(painter, batch, offset, layer.blend());
    }
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
        let batch = cache.batch(BatchKey { page, layer, moved: false }, in_page, scale, aa);
        stroke_gpu::paint_batch(painter, batch, offset, layer.blend());
    }
}

/// Paints the text boxes of the visible pages with their stored line breaks.
///
/// * `skip`    – a box that is being edited (the live editor draws it instead)
/// * `moved`   – a box shown at an offset (content space) while it is dragged
/// * `outline` – draw a faint frame around every box (text tool active)
pub fn paint_texts(
    painter: &Painter,
    view: &View,
    texts: &[TextBox],
    skip: Option<u64>,
    moved: Option<(u64, Vec2)>,
    outline: bool,
) {
    for page in view.visible_pages() {
        let origin = view.page_origin_screen(page);
        for t in texts.iter().filter(|t| t.page == page && Some(t.id) != skip) {
            let mut pos = origin + vec2(t.x, t.y) * view.scale;
            if let Some((id, d)) = moved
                && id == t.id
            {
                pos += d * view.scale;
            }
            let line_h = t.line_h * view.scale;
            let rect = Rect::from_min_size(pos, vec2(t.width, t.height()) * view.scale);
            if !rect.intersects(view.rect) {
                continue;
            }
            let color = Color32::from_rgb(t.color[0], t.color[1], t.color[2]);
            let size_px = t.size * view.scale;
            if size_px >= 2.0 {
                for (i, line) in t.lines.iter().enumerate() {
                    let y = pos.y + i as f32 * line_h;
                    if line.is_empty() || y > view.rect.bottom() || y + line_h < view.rect.top() {
                        continue;
                    }
                    painter.text(pos2(pos.x, y), Align2::LEFT_TOP, line, FontId::proportional(size_px), color);
                }
            }
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
        stroke_gpu::paint_batch(painter, live.fixed(), offset, layer.blend());
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
    painter.rect_filled(view.rect, 0.0, PADDING_COLOR);
    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        // Soft drop shadow, then the paper.
        painter.rect_filled(rect.translate(vec2(0.0, 3.0)).expand(1.0), 2.0, Color32::from_black_alpha(60));
        painter.rect_filled(rect, 0.0, PAGE_COLOR);
    }
}

/// The rules (or grid) and the edge of every visible page, over the
/// highlighters.
pub fn paint_rules(painter: &Painter, view: &View, style: &PageStyle) {
    let l = view.layout;
    let s = view.scale;
    let spacing = style.spacing.max(8.0);
    let rule = UiStroke::new(1.0, color::PAGE_RULE);

    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        if style.kind != PageKind::Blank {
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
        if style.kind == PageKind::Grid {
            let (y0, y1) = (rect.top().max(view.rect.top()), rect.bottom().min(view.rect.bottom()));
            let mut x = spacing;
            while x < l.page_w {
                let sx = rect.left() + x * s;
                painter.line_segment([pos2(sx, y0), pos2(sx, y1)], rule);
                x += spacing;
            }
        }
        painter.rect_stroke(rect, 0.0, UiStroke::new(1.0, color::PAGE_EDGE), UiStrokeKind::Middle);
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
        // Everything gone.
        check(&[], &mut cache);
    }
}
