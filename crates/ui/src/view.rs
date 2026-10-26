// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page layout and the mapping between screen, content and page space.
//!
//! Three coordinate spaces are used:
//!
//! * **page space**    – what strokes store: `(0,0)` is the top-left corner of
//!   the page the stroke belongs to, units are "document units" (1 unit = 1 CSS
//!   pixel at 100 % zoom, so A4 is 794 x 1123).
//! * **content space** – all pages stacked vertically, separated by `PAGE_GAP`,
//!   surrounded by `MARGIN`. Scrolling is an offset in this space.
//! * **screen space**  – egui points. The content starts `TOP_PAD_PX` below
//!   the top of the view (room for the tool notch), and that space scrolls
//!   away with the pages.
//!
//! Because strokes are stored in page space, changing the paper size or the
//! number of pages never moves existing ink relative to its page.
//!
//! An infinite page is laid out like any page, but it is very large
//! (`model::CANVAS_SIZE`), without the margin around it: the view goes only
//! as far as it is written on, with room to go on (`Layout::grown`), and so
//! it grows as it is written on, like a page of OneNote. Its fit zooms show
//! the paper at 100 %.

use std::ops::RangeInclusive;

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use tratto_core::model::Document;

use crate::settings::ZoomMode;

/// Space around the pages, in document units.
pub const MARGIN: f32 = 28.0;
/// Vertical gap between two pages, in document units.
pub const PAGE_GAP: f32 = 28.0;
/// Space below the last page (room for the "Add page" button), in screen px.
const BOTTOM_PAD_PX: f32 = 72.0;
/// Space above the first page, in screen px, on top of `MARGIN`: the tool
/// notch must not cover the page when scrolled to the top.
pub const TOP_PAD_PX: f32 = 48.0;

/// The zooms the view can have (percent); an infinite page can be seen
/// from further away.
const ZOOM_RANGE: RangeInclusive<u32> = 25..=400;
const CANVAS_ZOOM_RANGE: RangeInclusive<u32> = 10..=400;
/// How far an infinite page goes past what is written on it, in views.
const GROW_ROOM: f32 = 0.5;

/// Geometry of the stacked pages.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub page_w: f32,
    pub page_h: f32,
    pub count: u32,
    /// The page is infinite (`Document::is_canvas`).
    pub canvas: bool,
    /// An infinite page: how far the view goes (page space, see `grown`).
    pub extent: Vec2,
}

impl Layout {
    /// The pages of `doc`, stacked.
    pub fn of(doc: &Document) -> Self {
        let (page_w, page_h, canvas) = (doc.page_width, doc.page_height, doc.is_canvas());
        Self { page_w, page_h, count: doc.page_count.max(1), canvas, extent: vec2(page_w, page_h) }
    }

    /// An infinite page as far as the view goes: what is written on it
    /// (up to `written`, page space), and past it as much as half of what
    /// the view shows (`shown`, page units), right and down; at least what
    /// the view shows from where it is (`scroll`): it never shrinks under
    /// the view. Other pages as they are.
    pub fn grown(mut self, written: Option<Pos2>, shown: Vec2, scroll: Vec2) -> Self {
        if self.canvas {
            let room = written.map_or(Vec2::ZERO, |w| w.to_vec2() + shown * GROW_ROOM);
            let extent = room.max(shown).max(scroll + shown);
            self.extent = extent.min(vec2(self.page_w, self.page_h));
        }
        self
    }

    /// Space around the pages: none around an infinite page.
    fn margin(&self) -> f32 {
        if self.canvas { 0.0 } else { MARGIN }
    }

    /// The zooms (percent) the view of these pages can have.
    pub fn zoom_range(&self) -> RangeInclusive<u32> {
        if self.canvas { CANVAS_ZOOM_RANGE } else { ZOOM_RANGE }
    }

    pub fn content_w(&self) -> f32 {
        if self.canvas {
            return self.extent.x;
        }
        self.page_w + 2.0 * self.margin()
    }

    pub fn content_h(&self, scale: f32) -> f32 {
        if self.canvas {
            return TOP_PAD_PX / scale + self.extent.y;
        }
        TOP_PAD_PX / scale
            + self.margin()
            + self.count as f32 * self.page_h
            + self.count.saturating_sub(1) as f32 * PAGE_GAP
            + (BOTTOM_PAD_PX / scale).max(self.margin())
    }

    fn stride(&self) -> f32 {
        self.page_h + PAGE_GAP
    }

    pub fn page_top(&self, page: u32) -> f32 {
        self.margin() + page as f32 * self.stride()
    }

    /// Top-left corner of a page in content space.
    pub fn page_origin(&self, page: u32) -> Pos2 {
        pos2(self.margin(), self.page_top(page))
    }

    /// The page containing the content-space point, if it is on paper.
    pub fn page_at(&self, c: Pos2) -> Option<u32> {
        if c.x < self.margin() || c.x > self.margin() + self.page_w || c.y < self.margin() {
            return None;
        }
        let i = ((c.y - self.margin()) / self.stride()).floor() as u32;
        if i >= self.count {
            return None;
        }
        (c.y - self.page_top(i) <= self.page_h).then_some(i)
    }

    /// Pages that may overlap the content-space y range (a superset is fine).
    #[allow(clippy::reversed_empty_ranges)] // `1..=0`: the empty range
    pub fn pages_overlapping(&self, y0: f32, y1: f32) -> RangeInclusive<u32> {
        if self.count == 0 || y1 < self.margin() {
            return 1..=0; // empty
        }
        let first = ((y0 - self.margin()) / self.stride()).floor().max(0.0) as u32;
        let last = (((y1 - self.margin()) / self.stride()).floor() as u32).min(self.count - 1);
        first..=last
    }

    /// Index of the page nearest to a content-space y (for the page indicator).
    pub fn page_index_at_y(&self, y: f32) -> u32 {
        (((y - self.margin()) / self.stride()).floor().max(0.0) as u32).min(self.count.saturating_sub(1))
    }

    /// The page nearest to the content-space height `y`: in the gap between
    /// two pages, the closer one.
    pub fn page_nearest_y(&self, y: f32) -> u32 {
        let p = self.page_index_at_y(y);
        let bottom = self.page_top(p) + self.page_h;
        if y > bottom && p + 1 < self.count && y - bottom > self.page_top(p + 1) - y { p + 1 } else { p }
    }

    /// The scroll offset that shows `page` from its top, margin included.
    pub fn page_stop(&self, page: u32) -> f32 {
        self.page_top(page) - self.margin()
    }

    /// The page whose `page_stop` is at or above the scroll offset `y`.
    fn page_of_stop(&self, y: f32) -> u32 {
        ((y / self.stride()).floor().max(0.0) as u32).min(self.count.saturating_sub(1))
    }

    /// Reading: the scroll offset that shows what follows `top`, given the
    /// content height the view shows (`visible`). The rest of the current
    /// page first, its bottom at the bottom of the view; then the next page
    /// from its top. A page that fits the view is one step.
    pub fn next_stop(&self, top: f32, visible: f32) -> f32 {
        let i = self.page_of_stop(top + STOP_SLACK);
        let page_end = self.page_top(i) + self.page_h + self.margin();
        if page_end > top + visible + STOP_SLACK {
            (top + visible * READ_STEP).min(page_end - visible)
        } else if i + 1 < self.count {
            self.page_stop(i + 1)
        } else {
            top
        }
    }

    /// Reading: the way back from `top`, mirroring `next_stop`.
    pub fn prev_stop(&self, top: f32, visible: f32) -> f32 {
        if top <= STOP_SLACK {
            return 0.0;
        }
        let i = self.page_of_stop(top - STOP_SLACK);
        if top >= self.page_top(i) + self.page_h - STOP_SLACK {
            // All of page `i` is above: its end, or all of it if it fits.
            let page_end = self.page_top(i) + self.page_h + self.margin();
            (page_end - visible).max(self.page_stop(i))
        } else {
            (top - visible * READ_STEP).max(self.page_stop(i))
        }
    }
}

/// Reading: the share of the view one step moves within a tall page.
const READ_STEP: f32 = 0.9;
/// Reading: offsets this close to a stop count as being at it.
const STOP_SLACK: f32 = 0.5;

#[derive(Clone, Copy)]
pub struct View {
    /// Screen rectangle of the writing area (scrollbars excluded).
    pub rect: Rect,
    /// Content-space position shown at the top-left of `rect`.
    pub scroll: Vec2,
    pub scale: f32,
    /// The zoom the page is made at, drawn scaled to `scale` while the view
    /// zooms (see `zoom`); else `scale`.
    pub made: f32,
    /// Horizontal centring offset (screen px) when the content is narrower than `rect`.
    pub pad_x: f32,
    pub layout: Layout,
}

impl View {
    pub fn scale_for(zoom: ZoomMode, rect: Rect, layout: &Layout) -> f32 {
        let fit_w = rect.width() / layout.content_w();
        let s = match zoom {
            // A canvas has no width to fit.
            ZoomMode::FitWidth | ZoomMode::FitPage if layout.canvas => 1.0,
            ZoomMode::FitWidth => fit_w,
            ZoomMode::FitPage => fit_w.min((rect.height() - TOP_PAD_PX).max(1.0) / (layout.page_h + 2.0 * MARGIN)),
            ZoomMode::Percent(p) => p.clamp(*layout.zoom_range().start(), *layout.zoom_range().end()) as f32 / 100.0,
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
        let pad_x = Self::pad_x_for(rect.width(), layout.content_w(), scale);
        Self { rect, scroll, scale, made: scale, pad_x, layout }
    }

    pub fn content_to_screen(&self, c: Pos2) -> Pos2 {
        pos2(
            self.rect.left() + self.pad_x + (c.x - self.scroll.x) * self.scale,
            self.rect.top() + TOP_PAD_PX + (c.y - self.scroll.y) * self.scale,
        )
    }

    pub fn screen_to_content(&self, p: Pos2) -> Pos2 {
        pos2(
            (p.x - self.rect.left() - self.pad_x) / self.scale + self.scroll.x,
            (p.y - self.rect.top() - TOP_PAD_PX) / self.scale + self.scroll.y,
        )
    }

    /// Screen position of a page's top-left corner.
    pub fn page_origin_screen(&self, page: u32) -> Pos2 {
        self.content_to_screen(self.layout.page_origin(page))
    }

    pub fn page_screen_rect(&self, page: u32) -> Rect {
        Rect::from_min_size(self.page_origin_screen(page), vec2(self.layout.page_w, self.layout.page_h) * self.scale)
    }

    /// Content-space y range shown in the view.
    pub fn visible_y(&self) -> (f32, f32) {
        let top = self.scroll.y - TOP_PAD_PX / self.scale;
        (top, top + self.rect.height() / self.scale)
    }

    pub fn visible_pages(&self) -> RangeInclusive<u32> {
        let (top, bottom) = self.visible_y();
        self.layout.pages_overlapping(top, bottom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout { page_w: 794.0, page_h: 1123.0, count: 3, canvas: false, extent: Vec2::ZERO }
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
    fn a_canvas_fits_at_100_percent_and_zooms_further_out() {
        let canvas = Layout { page_w: 100_000.0, page_h: 100_000.0, count: 1, canvas: true, extent: Vec2::ZERO };
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 700.0));
        assert_eq!(View::scale_for(ZoomMode::FitWidth, rect, &canvas), 1.0);
        assert_eq!(View::scale_for(ZoomMode::FitPage, rect, &canvas), 1.0);
        assert!(canvas.zoom_range().start() < layout().zoom_range().start());
    }

    #[test]
    fn an_infinite_page_grows_as_it_is_written_on() {
        let page = Layout { page_w: 100_000.0, page_h: 100_000.0, count: 1, canvas: true, extent: Vec2::ZERO };
        let shown = vec2(1000.0, 600.0);
        // Nothing written: what the view shows, from the page's corner.
        let empty = page.grown(None, shown, Vec2::ZERO);
        assert_eq!(empty.extent, shown);
        assert_eq!(empty.page_origin(0), pos2(0.0, 0.0), "no margin");
        // Written down to 2000: half a view more.
        let written = page.grown(Some(pos2(400.0, 2000.0)), shown, Vec2::ZERO);
        assert_eq!(written.extent, vec2(1000.0, 2300.0));
        // Never less than what is in view (something was just erased).
        assert_eq!(page.grown(None, shown, vec2(0.0, 5000.0)).extent, vec2(1000.0, 5600.0));
        // Other pages do not grow.
        assert_eq!(layout().grown(Some(pos2(5000.0, 5000.0)), shown, Vec2::ZERO).content_w(), layout().content_w());
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

    #[test]
    fn first_page_clears_the_top_pad() {
        let l = layout();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 700.0));
        let v = View::new(rect, Vec2::ZERO, 0.5, l);
        assert!((v.page_screen_rect(0).top() - (TOP_PAD_PX + MARGIN * 0.5)).abs() < 1e-3);
        // Scrolled to the bottom, the last page still ends inside the view.
        let max = View::max_scroll(rect, 0.5, &l);
        let v = View::new(rect, max, 0.5, l);
        assert!(v.page_screen_rect(2).bottom() < rect.bottom());
    }

    #[test]
    fn reading_turns_whole_pages_that_fit() {
        let l = layout();
        // Fit page: the view shows a page and its margins.
        let visible = l.page_h + 2.0 * MARGIN;
        let mut y = 0.0;
        for page in 1..3 {
            y = l.next_stop(y, visible);
            assert_eq!(y, l.page_stop(page));
        }
        assert_eq!(l.next_stop(y, visible), y, "the last page stays");
        for page in (0..2).rev() {
            y = l.prev_stop(y, visible);
            assert_eq!(y, l.page_stop(page));
        }
        assert_eq!(l.prev_stop(0.0, visible), 0.0);
    }

    #[test]
    fn reading_shows_all_of_a_tall_page_before_the_next() {
        let l = layout();
        let visible = 500.0;
        let page_end = l.page_top(0) + l.page_h + MARGIN;
        // Forward: steps of the view, the last one ending at the page's end.
        let mut stops = vec![0.0];
        while *stops.last().unwrap() < l.page_stop(1) {
            let y = l.next_stop(*stops.last().unwrap(), visible);
            assert!(y > *stops.last().unwrap());
            stops.push(y);
        }
        assert_eq!(stops, [0.0, 450.0, page_end - visible, l.page_stop(1)]);
        for w in stops.windows(2) {
            assert!(w[1] - w[0] <= visible, "nothing is skipped");
        }
        // Back: the end of page 0 first, then up to its top.
        let y = l.prev_stop(l.page_stop(1), visible);
        assert_eq!(y, page_end - visible);
        assert_eq!(l.prev_stop(l.prev_stop(y, visible), visible), 0.0);
        // From the middle of a page, back to its top.
        assert_eq!(l.prev_stop(l.page_stop(1) + 100.0, visible), l.page_stop(1));
    }
}
