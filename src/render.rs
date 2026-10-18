//! Painting with `egui::Painter`.

use eframe::egui::{Color32, Painter, Pos2, Shape, Stroke as UiStroke};

use crate::model::{PageKind, PageStyle, Point, Stroke, StrokeKind};
use crate::view::View;

/// Distinct colour of the non-writable side padding.
pub const PADDING_COLOR: Color32 = Color32::from_gray(52);
pub const PAGE_COLOR: Color32 = Color32::from_rgb(252, 251, 247);

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
                painter.line_segment([egui_pos(page.left(), sy), egui_pos(page.right(), sy)], line);
            }
            k += 1;
        }
    }
    if style.kind == PageKind::Grid {
        let mut x = spacing;
        while x < view.page_width {
            let sx = view.to_screen(x, 0.0).x;
            painter.line_segment(
                [egui_pos(sx, view.rect.top()), egui_pos(sx, view.rect.bottom())],
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

fn egui_pos(x: f32, y: f32) -> Pos2 {
    Pos2::new(x, y)
}

pub fn paint_stroke(painter: &Painter, view: &View, s: &Stroke) {
    paint_points(painter, view, s.kind, s.color, s.width, &s.points);
}

/// Paints a polyline; used for finished strokes and the live stroke.
pub fn paint_points(
    painter: &Painter,
    view: &View,
    kind: StrokeKind,
    rgba: [u8; 4],
    width: f32,
    pts: &[Point],
) {
    if pts.is_empty() {
        return;
    }
    // Unmultiplied alpha -> egui handles blending (highlighter is semi-transparent).
    let color = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
    let screen: Vec<Pos2> = pts.iter().map(|p| view.to_screen(p.x, p.y)).collect();
    let w = (width * view.scale).max(0.5);

    match kind {
        StrokeKind::Highlighter => {
            // One single path shape, constant width, butt caps.
            // (Per-segment circles would double-blend the translucent colour.)
            if screen.len() == 1 {
                painter.circle_filled(screen[0], w * 0.5, color);
            } else {
                painter.add(Shape::line(screen, UiStroke::new(w, color)));
            }
        }
        StrokeKind::Pen => paint_pen(painter, color, w, &screen, pts),
    }
}

/// Pressure -> width multiplier.
fn pressure_factor(p: f32) -> f32 {
    0.35 + 0.65 * p.clamp(0.0, 1.0)
}

fn paint_pen(painter: &Painter, color: Color32, w: f32, screen: &[Pos2], pts: &[Point]) {
    if screen.len() == 1 {
        painter.circle_filled(screen[0], w * pressure_factor(pts[0].pressure) * 0.5, color);
        return;
    }
    let uniform = pts.iter().all(|p| (p.pressure - pts[0].pressure).abs() < 0.02);
    if uniform {
        // Mouse / constant pressure: a single path is the cheapest.
        let ww = w * pressure_factor(pts[0].pressure);
        painter.add(Shape::line(screen.to_vec(), UiStroke::new(ww, color)));
        painter.circle_filled(screen[0], ww * 0.5, color); // round caps
        painter.circle_filled(screen[screen.len() - 1], ww * 0.5, color);
    } else {
        // Variable pressure: one capsule per segment (line + round joint).
        for i in 0..screen.len() - 1 {
            let ww = w * pressure_factor((pts[i].pressure + pts[i + 1].pressure) * 0.5);
            painter.line_segment([screen[i], screen[i + 1]], UiStroke::new(ww, color));
            painter.circle_filled(screen[i], ww * 0.5, color);
        }
        let last = screen.len() - 1;
        painter.circle_filled(
            screen[last],
            w * pressure_factor(pts[last].pressure) * 0.5,
            color,
        );
    }
}

pub fn paint_eraser_cursor(painter: &Painter, center: Pos2, radius_px: f32) {
    painter.circle_filled(center, radius_px, Color32::from_rgba_unmultiplied(120, 120, 120, 40));
    painter.circle_stroke(center, radius_px, UiStroke::new(1.5, Color32::from_gray(80)));
}
