// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Page layout and the mapping between screen, content and page space.
//!
//! Three coordinate spaces are used:
//!
//! * **page space**    – what strokes store: `(0,0)` is the top-left corner of
//!   the page the stroke belongs to, units are "document units" (1 unit = 1 CSS
//!   pixel at 100 % zoom, so A4 is 794 x 1123).
//! * **content space** – all pages stacked vertically, separated by `PAGE_GAP`,
//!   surrounded by `MARGIN`. Scrolling is an offset in this space.
//! * **screen space**  – egui points.
//!
//! Because strokes are stored in page space, changing the paper size or the
//! number of pages never moves existing ink relative to its page.

use std::ops::RangeInclusive;

use eframe::egui::{pos2, vec2, Pos2, Rect, Vec2};

use crate::settings::ZoomMode;

/// Space around the pages, in document units.
pub const MARGIN: f32 = 28.0;
/// Vertical gap between two pages, in document units.
pub const PAGE_GAP: f32 = 28.0;
/// Space below the last page (room for the "Add page" button), in screen px.
const BOTTOM_PAD_PX: f32 = 72.0;

/// Geometry of the stacked pages.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub page_w: f32,
    pub page_h: f32,
    pub count: u32,
}

impl Layout {
    pub fn content_w(&self) -> f32 {
        self.page_w + 2.0 * MARGIN
    }

    pub fn content_h(&self, scale: f32) -> f32 {
        MARGIN
            + self.count as f32 * self.page_h
            + self.count.saturating_sub(1) as f32 * PAGE_GAP
            + (BOTTOM_PAD_PX / scale).max(MARGIN)
    }

    fn stride(&self) -> f32 {
        self.page_h + PAGE_GAP
    }

    pub fn page_top(&self, page: u32) -> f32 {
        MARGIN + page as f32 * self.stride()
    }

    /// Top-left corner of a page in content space.
    pub fn page_origin(&self, page: u32) -> Pos2 {
        pos2(MARGIN, self.page_top(page))
    }

    /// The page containing the content-space point, if it is on paper.
    pub fn page_at(&self, c: Pos2) -> Option<u32> {
        if c.x < MARGIN || c.x > MARGIN + self.page_w || c.y < MARGIN {
            return None;
        }
        let i = ((c.y - MARGIN) / self.stride()).floor() as u32;
        if i >= self.count {
            return None;
        }
        (c.y - self.page_top(i) <= self.page_h).then_some(i)
    }

    /// Pages that may overlap the content-space y range (a superset is fine).
    pub fn pages_overlapping(&self, y0: f32, y1: f32) -> RangeInclusive<u32> {
        if self.count == 0 || y1 < MARGIN {
            return 1..=0; // empty
        }
        let first = ((y0 - MARGIN) / self.stride()).floor().max(0.0) as u32;
        let last = (((y1 - MARGIN) / self.stride()).floor() as u32).min(self.count - 1);
        first..=last
    }

    /// Index of the page nearest to a content-space y (for the page indicator).
    pub fn page_index_at_y(&self, y: f32) -> u32 {
        (((y - MARGIN) / self.stride()).floor().max(0.0) as u32).min(self.count.saturating_sub(1))
    }
}

#[derive(Clone, Copy)]
pub struct View {
    /// Screen rectangle of the writing area (scrollbars excluded).
    pub rect: Rect,
    /// Content-space position shown at the top-left of `rect`.
    pub scroll: Vec2,
    pub scale: f32,
    /// Horizontal centring offset (screen px) when the content is narrower than `rect`.
    pub pad_x: f32,
    pub layout: Layout,
}

impl View {
    pub fn scale_for(zoom: ZoomMode, rect: Rect, layout: &Layout) -> f32 {
        let fit_w = rect.width() / layout.content_w();
        let s = match zoom {
            ZoomMode::FitWidth => fit_w,
            ZoomMode::FitPage => fit_w.min(rect.height() / (layout.page_h + 2.0 * MARGIN)),
            ZoomMode::Percent(p) => p as f32 / 100.0,
        };
        s.clamp(0.1, 8.0)
    }

    pub fn pad_x_for(rect_w: f32, content_w: f32, scale: f32) -> f32 {
        ((rect_w - content_w * scale) * 0.5).max(0.0)
    }

    /// Largest valid scroll offsets (content units).
    pub fn max_scroll(rect: Rect, scale: f32, layout: &Layout) -> Vec2 {
        vec2(
            (layout.content_w() - rect.width() / scale).max(0.0),
            (layout.content_h(scale) - rect.height() / scale).max(0.0),
        )
    }

    pub fn new(rect: Rect, scroll: Vec2, scale: f32, layout: Layout) -> Self {
        Self { rect, scroll, scale, pad_x: Self::pad_x_for(rect.width(), layout.content_w(), scale), layout }
    }

    pub fn content_to_screen(&self, c: Pos2) -> Pos2 {
        pos2(
            self.rect.left() + self.pad_x + (c.x - self.scroll.x) * self.scale,
            self.rect.top() + (c.y - self.scroll.y) * self.scale,
        )
    }

    pub fn screen_to_content(&self, p: Pos2) -> Pos2 {
        pos2(
            (p.x - self.rect.left() - self.pad_x) / self.scale + self.scroll.x,
            (p.y - self.rect.top()) / self.scale + self.scroll.y,
        )
    }

    /// Screen position of a page's top-left corner.
    pub fn page_origin_screen(&self, page: u32) -> Pos2 {
        self.content_to_screen(self.layout.page_origin(page))
    }

    pub fn page_screen_rect(&self, page: u32) -> Rect {
        Rect::from_min_size(
            self.page_origin_screen(page),
            vec2(self.layout.page_w, self.layout.page_h) * self.scale,
        )
    }

    pub fn visible_pages(&self) -> RangeInclusive<u32> {
        let top = self.scroll.y;
        self.layout.pages_overlapping(top, top + self.rect.height() / self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout { page_w: 794.0, page_h: 1123.0, count: 3 }
    }

    #[test]
    fn page_hit_testing() {
        let l = layout();
        assert_eq!(l.page_at(pos2(MARGIN + 10.0, MARGIN + 10.0)), Some(0));
        // In the gap between page 0 and 1.
        assert_eq!(l.page_at(pos2(MARGIN + 10.0, MARGIN + 1123.0 + 5.0)), None);
        assert_eq!(l.page_at(pos2(MARGIN + 10.0, l.page_top(2) + 50.0)), Some(2));
        // Left of the paper, and below the last page.
        assert_eq!(l.page_at(pos2(5.0, 100.0)), None);
        assert_eq!(l.page_at(pos2(100.0, l.page_top(2) + 1123.0 + 10.0)), None);
    }

    #[test]
    fn screen_content_roundtrip() {
        let l = layout();
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(1200.0, 700.0));
        let v = View::new(rect, vec2(0.0, 300.0), 1.25, l);
        let p = pos2(640.0, 333.0);
        let back = v.content_to_screen(v.screen_to_content(p));
        assert!((back - p).length() < 1e-3);
    }
}
