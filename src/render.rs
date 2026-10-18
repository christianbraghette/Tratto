//! Painting: pages, cached anti-aliased stroke meshes, eraser cursor.

use std::collections::HashMap;

use eframe::egui::{epaint::Mesh, pos2, vec2, Color32, Painter, Pos2, Shape, Stroke as UiStroke, Vec2};

use crate::model::{PageKind, PageStyle, Point, Stroke, StrokeKind};
use crate::stroke_mesh;
use crate::view::View;

/// Distinct colour of the non-writable area around the pages.
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

/// Paints the strokes of all visible pages, one batched mesh per page.
pub fn paint_strokes(
    painter: &Painter,
    view: &View,
    strokes: &[Stroke],
    cache: &mut StrokeCache,
    ppp: f32,
) {
    let aa = 1.0 / ppp;
    cache.begin_frame(view.scale, ppp);

    for page in view.visible_pages() {
        let offset = view.page_origin_screen(page).to_vec2();
        // Visible vertical range in this page's space.
        let y0 = view.scroll.y - view.layout.page_top(page);
        let y1 = y0 + view.rect.height() / view.scale;

        let mut batch = Mesh::default();
        for s in strokes.iter().filter(|s| s.page == page) {
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
    let rule = UiStroke::new(1.0, Color32::from_rgb(200, 214, 230));

    for page in view.visible_pages() {
        let rect = view.page_screen_rect(page);
        // Soft drop shadow, then the paper.
        painter.rect_filled(rect.translate(vec2(2.0, 3.0)), 0.0, Color32::from_black_alpha(70));
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
        painter.rect_stroke(rect, 0.0, UiStroke::new(1.0, Color32::from_gray(25)));
    }
}

pub fn paint_eraser_cursor(painter: &Painter, center: Pos2, radius_px: f32) {
    painter.circle_filled(center, radius_px, Color32::from_rgba_unmultiplied(120, 120, 120, 40));
    painter.circle_stroke(center, radius_px, UiStroke::new(1.5, Color32::from_gray(80)));
}
