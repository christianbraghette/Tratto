//! Document <-> screen mapping.
//!
//! The page has a fixed width. If the canvas is wider, the page is centred
//! (`pad_x` on each side). If it is narrower, the page is uniformly scaled
//! down to fit so nothing is ever unreachable horizontally.

use eframe::egui::{pos2, Pos2, Rect};

#[derive(Clone, Copy)]
pub struct View {
    /// Screen rectangle of the whole canvas area.
    pub rect: Rect,
    /// Top of the viewport in document units.
    pub scroll_y: f32,
    pub page_width: f32,
    pub scale: f32,
    /// Horizontal padding (screen px) on the left of the page.
    pub pad_x: f32,
}

impl View {
    pub fn scale_for(canvas_width: f32, page_width: f32) -> f32 {
        (canvas_width / page_width).clamp(0.05, 1.0)
    }

    pub fn new(rect: Rect, scroll_y: f32, page_width: f32) -> Self {
        let scale = Self::scale_for(rect.width(), page_width);
        let pad_x = ((rect.width() - page_width * scale) * 0.5).max(0.0);
        Self { rect, scroll_y, page_width, scale, pad_x }
    }

    pub fn to_screen(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.rect.left() + self.pad_x + x * self.scale,
            self.rect.top() + (y - self.scroll_y) * self.scale,
        )
    }

    pub fn to_doc(&self, p: Pos2) -> Pos2 {
        pos2(
            (p.x - self.rect.left() - self.pad_x) / self.scale,
            (p.y - self.rect.top()) / self.scale + self.scroll_y,
        )
    }

    /// The writable area in screen space.
    pub fn page_rect(&self) -> Rect {
        let left = self.rect.left() + self.pad_x;
        Rect::from_min_max(
            pos2(left, self.rect.top()),
            pos2(left + self.page_width * self.scale, self.rect.bottom()),
        )
    }

    /// Visible document y-range.
    pub fn visible_y(&self) -> (f32, f32) {
        (self.scroll_y, self.scroll_y + self.rect.height() / self.scale)
    }
}
