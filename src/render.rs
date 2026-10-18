//! Painting: page background, cached anti-aliased stroke meshes, eraser cursor.

use std::collections::HashMap;

use eframe::egui::{epaint::Mesh, vec2, Color32, Painter, Pos2, Shape, Stroke as UiStroke, Vec2};

use crate::model::{PageKind, PageStyle, Point, Stroke, StrokeKind};
use crate::stroke_mesh;
use crate::view::View;

/// Distinct colour of the non-writable side padding.
pub const PADDING_COLOR: Color32 = Color32::from_gray(52);
pub const PAGE_COLOR: Color32 = Color32::from_rgb(252, 251, 247);

/// Flush the batched mesh before it grows beyond this many vertices.
const MAX_BATCH_VERTS: usize = 60_000;
/// Cache entries unused for this many frames are dropped (bounds memory).
const CACHE_TTL_FRAMES: u64 = 512;

// ------------------------------------------------------------------ cache

struct Entry {
    mesh: Mesh,
    last_used: u64,
}

/// Per-stroke tessellation cache. Meshes are stored at `document * scale`
/// without translation, so scrolling never invalidates them; only a change of
/// scale or pixels-per-point does.
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

    fn begin_frame(&mut self, scale: f32, ppp: f32) {
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
        let e = self.map.entry(s.id).or_insert_with(|| {
            let mut mesh = Mesh::default();
            stroke_mesh::tessellate(&mut mesh, s.kind, s.color, s.width, &s.points, scale, aa);
            Entry { mesh, last_used: frame }
        });
        e.last_used = frame;
        &e.mesh
    }
}

// ------------------------------------------------------------ strokes

/// Paints all visible strokes in document order as one (or a few) meshes.
pub fn paint_strokes(
    painter: &Painter,
    view: &View,
    strokes: &[Stroke],
    cache: &mut StrokeCache,
    ppp: f32,
) {
    let aa = 1.0 / ppp;
    cache.begin_frame(view.scale, ppp);
    let offset = mesh_offset(view);
    let (y0, y1) = view.visible_y();

    let mut batch = Mesh::default();
    for s in strokes {
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

/// Paints the stroke that is currently being drawn (rebuilt every frame).
pub fn paint_live(
    painter: &Painter,
    view: &View,
    kind: StrokeKind,
    rgba: [u8; 4],
    width: f32,
    pts: &[Point],
    ppp: f32,
) {
    let mut mesh = Mesh::default();
    stroke_mesh::tessellate(&mut mesh, kind, rgba, width, pts, view.scale, 1.0 / ppp);
    mesh.translate(mesh_offset(view));
    if !mesh.is_empty() {
        painter.add(Shape::mesh(mesh));
    }
}

/// Translation from "document * scale" space to screen space.
fn mesh_offset(view: &View) -> Vec2 {
    vec2(
        view.rect.left() + view.pad_x,
        view.rect.top() - view.scroll_y * view.scale,
    )
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

pub fn paint_background(painter: &Painter, view: &View, style: &PageStyle) {
    painter.rect_filled(view.rect, 0.0, PADDING_COLOR);
    let page = view.page_rect();
    painter.rect_filled(page, 0.0, PAGE_COLOR);

    let line = UiStroke::new(1.0, Color32::from_rgb(200, 214, 230));
    let spacing = style.spacing.max(8.0);

    if style.kind != PageKind::Blank {
        // Horizontal rules (infinite downwards): only those in view are drawn.
        let mut k = ((view.scroll_y / spacing).floor() as i64).max(1);
        loop {
            let sy = view.to_screen(0.0, k as f32 * spacing).y;
            if sy > view.rect.bottom() {
                break;
            }
            if sy >= view.rect.top() {
                painter.line_segment([Pos2::new(page.left(), sy), Pos2::new(page.right(), sy)], line);
            }
            k += 1;
        }
    }
    if style.kind == PageKind::Grid {
        let mut x = spacing;
        while x < view.page_width {
            let sx = view.to_screen(x, 0.0).x;
            painter.line_segment(
                [Pos2::new(sx, view.rect.top()), Pos2::new(sx, view.rect.bottom())],
                line,
            );
            x += spacing;
        }
    }

    // Page edges.
    let edge = UiStroke::new(1.0, Color32::from_gray(25));
    painter.line_segment([page.left_top(), page.left_bottom()], edge);
    painter.line_segment([page.right_top(), page.right_bottom()], edge);
}

pub fn paint_eraser_cursor(painter: &Painter, center: Pos2, radius_px: f32) {
    painter.circle_filled(center, radius_px, Color32::from_rgba_unmultiplied(120, 120, 120, 40));
    painter.circle_stroke(center, radius_px, UiStroke::new(1.5, Color32::from_gray(80)));
}
