// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Painting: pages, cached anti-aliased stroke meshes, eraser cursor.

use std::collections::{HashMap, HashSet};

use eframe::egui::{
    epaint::Mesh, pos2, vec2, Align2, Color32, FontId, Painter, Pos2, Rect, Shape,
    Stroke as UiStroke, Vec2,
};

use inknote_core::model::{PageKind, PageStyle, Point, Stroke, StrokeKind, TextBox};

use crate::stroke_mesh;
use crate::theme::color;
use crate::view::View;

/// Distinct colour of the non-writable area around the pages.
pub const PADDING_COLOR: Color32 = color::DESK;
pub const PAGE_COLOR: Color32 = color::PAGE;

/// Flush the batched mesh before it grows beyond this many vertices.
const MAX_BATCH_VERTS: usize = 60_000;
/// Cache entries unused for this many frames are dropped (bounds memory).
const CACHE_TTL_FRAMES: u64 = 512;

// ------------------------------------------------------------------ cache

struct Entry {
    mesh: Mesh,
    last_used: u64,
    /// `Stroke::rev` the mesh was built for.
    rev: u32,
}

/// Per-stroke tessellation cache. Meshes are stored at `page_space * scale`
/// without translation, so neither scrolling nor page-size changes invalidate
/// them; only a change of zoom or pixels-per-point does.
#[derive(Default)]
pub struct StrokeCache {
    map: HashMap<u64, Entry>,
    scale_bits: u32,
    ppp_bits: u32,
    frame: u64,
}

impl StrokeCache {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn begin_frame(&mut self, scale: f32, ppp: f32) {
        let (s, p) = (scale.to_bits(), ppp.to_bits());
        if s != self.scale_bits || p != self.ppp_bits {
            self.map.clear();
            self.scale_bits = s;
            self.ppp_bits = p;
        }
        self.frame += 1;
        if self.frame % 256 == 0 {
            let f = self.frame;
            self.map.retain(|_, e| f - e.last_used < CACHE_TTL_FRAMES);
        }
    }

    fn get(&mut self, s: &Stroke, scale: f32, aa: f32) -> &Mesh {
        let frame = self.frame;
        let build = || {
            let mut mesh = Mesh::default();
            stroke_mesh::tessellate(&mut mesh, s.kind, s.color, s.width, &s.points, scale, aa);
            Entry { mesh, last_used: frame, rev: s.rev }
        };
        let e = self.map.entry(s.id).or_insert_with(build);
        if e.rev != s.rev {
            *e = build(); // the stroke was moved: rebuild
        }
        e.last_used = frame;
        &e.mesh
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

/// Paints the strokes of all visible pages, one batched mesh per page. Strokes
/// in `moved` are skipped there and drawn last, translated by the drag offset
/// (their cached meshes are reused, nothing is re-tessellated while dragging).
pub fn paint_strokes(
    painter: &Painter,
    view: &View,
    strokes: &[Stroke],
    cache: &mut StrokeCache,
    ppp: f32,
    moved: Option<&Moved>,
) {
    let aa = 1.0 / ppp;
    cache.begin_frame(view.scale, ppp);
    let is_moved = |id: u64| moved.map_or(false, |m| m.ids.contains(&id));

    for page in view.visible_pages() {
        let offset = view.page_origin_screen(page).to_vec2();
        // Visible vertical range in this page's space.
        let y0 = view.scroll.y - view.layout.page_top(page);
        let y1 = y0 + view.rect.height() / view.scale;

        let mut batch = Mesh::default();
        for s in strokes.iter().filter(|s| s.page == page && !is_moved(s.id)) {
            if s.bounds[3] + s.width < y0 || s.bounds[1] - s.width > y1 {
                continue; // outside the viewport
            }
            batch.append_ref(cache.get(s, view.scale, aa));
            if batch.vertices.len() > MAX_BATCH_VERTS {
                flush(painter, &mut batch, offset);
            }
        }
        flush(painter, &mut batch, offset);
    }

    if let Some(m) = moved {
        let offset = view.page_origin_screen(m.page).to_vec2() + m.offset * view.scale;
        let mut batch = Mesh::default();
        for s in strokes.iter().filter(|s| s.page == m.page && m.ids.contains(&s.id)) {
            batch.append_ref(cache.get(s, view.scale, aa));
            if batch.vertices.len() > MAX_BATCH_VERTS {
                flush(painter, &mut batch, offset);
            }
        }
        flush(painter, &mut batch, offset);
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
    let mut batch = Mesh::default();
    for s in strokes.iter().filter(|s| s.page == page) {
        batch.append_ref(cache.get(s, scale, aa));
        if batch.vertices.len() > MAX_BATCH_VERTS {
            flush(painter, &mut batch, offset);
        }
    }
    flush(painter, &mut batch, offset);
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
            if let Some((id, d)) = moved {
                if id == t.id {
                    pos += d * view.scale;
                }
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
                    painter.text(
                        pos2(pos.x, y),
                        Align2::LEFT_TOP,
                        line,
                        FontId::proportional(size_px),
                        color,
                    );
                }
            }
            if outline {
                painter.rect_stroke(
                    rect.expand(2.0),
                    2.0,
                    UiStroke::new(1.0, color::accent_alpha(110)),
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

/// Paints the stroke that is currently being drawn (rebuilt every frame).
#[allow(clippy::too_many_arguments)]
pub fn paint_live(
    painter: &Painter,
    view: &View,
    page: u32,
    kind: StrokeKind,
    rgba: [u8; 4],
    width: f32,
    pts: &[Point],
    ppp: f32,
) {
    let mut mesh = Mesh::default();
    stroke_mesh::tessellate(&mut mesh, kind, rgba, width, pts, view.scale, 1.0 / ppp);
    mesh.translate(view.page_origin_screen(page).to_vec2());
    if !mesh.is_empty() {
        painter.add(Shape::mesh(mesh));
    }
}

fn flush(painter: &Painter, batch: &mut Mesh, offset: Vec2) {
    if batch.is_empty() {
        return;
    }
    let mut m = std::mem::take(batch);
    m.translate(offset);
    painter.add(Shape::mesh(m));
}

// -------------------------------------------------------- background

/// Fills the area around the pages and draws every visible page.
pub fn paint_background(painter: &Painter, view: &View, style: &PageStyle) {
    painter.rect_filled(view.rect, 0.0, PADDING_COLOR);

    let l = view.layout;
    let s = view.scale;
    let spacing = style.spacing.max(8.0);
    let rule = UiStroke::new(1.0, color::PAGE_RULE);

    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        // Soft drop shadow, then the paper.
        painter.rect_filled(rect.translate(vec2(0.0, 3.0)).expand(1.0), 2.0, Color32::from_black_alpha(60));
        painter.rect_filled(rect, 0.0, PAGE_COLOR);

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
        painter.rect_stroke(rect, 0.0, UiStroke::new(1.0, color::PAGE_EDGE));
    }
}

pub fn paint_eraser_cursor(painter: &Painter, center: Pos2, radius_px: f32) {
    painter.circle_filled(center, radius_px, Color32::from_rgba_unmultiplied(120, 120, 120, 40));
    painter.circle_stroke(center, radius_px, UiStroke::new(1.5, Color32::from_gray(80)));
}
