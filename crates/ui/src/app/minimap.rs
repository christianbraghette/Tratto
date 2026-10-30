// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The minimap: the vertical scroll bar widened into the note in small, to
//! find a place at a glance and go there.
//!
//! The scroll bar dragged (with a mouse, a pen or a finger) grows from its
//! right edge into the minimap,
//! over the canvas (nothing moves), its thumb becoming the view box, and
//! shrinks back into it when it goes.
//! It shows the pages as they are stacked (an infinite page as far as it is
//! written on), as wide as it is, and a box over what the view shows: drag
//! the box to scroll, press elsewhere to go there. A note taller than the
//! minimap scrolls in it with the view, as in VS Code. The mouse leaving it
//! (for `LEAVE_GRACE`) or a press elsewhere puts it away.
//!
//! On an infinite page it shows what is written and what the view shows
//! (`Layout::grown`). The box held against an edge of it scrolls on that
//! way and the page grows to follow, as with the scroll bars; while the box
//! is held the page only grows, so the minimap stays under the pointer.
//!
//! It is drawn with what the pages panel draws pages with (the stroke
//! batches, the small images of PDF pages, the pictures' smallest levels),
//! its strokes in a cache of their own, and only while it is out.

use eframe::egui::{self, Id, Order, Pos2, Rect, Sense, Vec2, pos2, vec2};

use crate::render::{self, StrokeCache};
use crate::scrollbar::BarState;
use crate::theme::{self, color};
use crate::view::{Layout, TOP_PAD_PX, View};
use crate::zoom::ZoomRender;

use super::InkApp;
use super::canvas::bar_push_speed;

/// Seconds the mouse may be away from it before it goes.
const LEAVE_GRACE: f64 = 0.3;
/// Its width (the scroll bar's included), and the room around the pages in
/// it (points).
const WIDTH: f32 = 140.0;
const PAD: f32 = 8.0;
/// The time it takes to grow out of the scroll bar, or back, in seconds.
const GROW_TIME: f32 = 0.15;

#[derive(Default)]
pub(super) struct Minimap {
    open: bool,
    /// Since when the mouse is away from it, while it is out.
    away_at: Option<f64>,
    /// A press on it: where, and the scroll then (after the box was brought
    /// there).
    drag: Option<(Pos2, Vec2)>,
    /// Where it was last frame (the pointer is tested against it).
    rect: Option<Rect>,
    /// Its strokes, at its own scale, and the zoom they are made at (it
    /// changes as an infinite page grows, or the window is resized).
    cache: StrokeCache,
    zoom: ZoomRender,
}

impl Minimap {
    /// It is out (or sliding).
    pub(super) fn out(&self) -> bool {
        self.rect.is_some()
    }

    /// Its view box is held (an infinite page then only grows, see
    /// `grow_canvas`).
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Content space moved by `by` (see `InkApp::shift_view`): the scroll
    /// a drag started from stays where it was on the page.
    pub(super) fn shift(&mut self, by: Vec2) {
        if let Some((_, start)) = self.drag.as_mut() {
            *start += by;
        }
    }

    /// Forgets the open note's strokes (another note).
    pub(super) fn clear(&mut self) {
        self.cache.clear();
        self.zoom = ZoomRender::default();
        self.drag = None;
    }
}

/// How the minimap shows the content: content space to its points.
struct Map {
    /// Where the pages are drawn (inside its padding).
    inner: Rect,
    /// Points per content unit.
    m: f32,
    /// The content y shown at its top (it scrolls with the view).
    top: f32,
}

impl Map {
    fn to_mini(&self, c: Pos2) -> Pos2 {
        pos2(self.inner.left() + c.x * self.m, self.inner.top() + (c.y - self.top) * self.m)
    }

    /// The content y shown at its top for the scroll `y` (of `max_y`): all
    /// of the content when it fits, else as far down as the view is.
    fn top_for(inner: Rect, m: f32, content_h: f32, y: f32, max_y: f32) -> f32 {
        let hidden = (content_h - inner.height() / m).max(0.0);
        if max_y > 0.0 { hidden * (y / max_y).clamp(0.0, 1.0) } else { 0.0 }
    }
}

impl InkApp {
    /// The minimap beside the vertical scroll bar (`bar`, its track, and
    /// `state`), over `canvas`: whether it comes out or goes, and its view
    /// box dragged or pressed, which scrolls the view (before the view is
    /// drawn). `max` is the largest scroll.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn minimap(
        &mut self,
        ctx: &egui::Context,
        canvas: Rect,
        bar: Rect,
        state: &BarState,
        layout: &Layout,
        scale: f32,
        max: Vec2,
    ) {
        let now = ctx.input(|i| i.time);
        let (pointer, pressed, origin) =
            ctx.input(|i| (i.pointer.hover_pos(), i.pointer.any_pressed(), i.pointer.press_origin()));
        let touch = ctx.input(|i| i.any_touches() || i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. })));
        // Out, it covers the bar, grown left from its right edge.
        let full = Rect::from_min_max(pos2(bar.right() - WIDTH, canvas.top()), pos2(bar.right(), canvas.bottom()));
        let was_open = self.minimap.open;
        let on = |p: Pos2| bar.contains(p) || was_open && full.contains(p);
        let usable = self.settings.minimap && state.enabled && !self.busy() && self.presenting.is_none();

        let mm = &mut self.minimap;
        // Out: the bar dragged.
        if state.active {
            mm.open = true;
        }
        // Away: a press elsewhere, or the mouse gone from it for a moment (not
        // while the bar is dragged). A pen or a finger lifted leaves no
        // pointer: it stays.
        if mm.open && mm.drag.is_none() && !state.active {
            if pressed && origin.is_some_and(|o| !on(o)) {
                mm.open = false;
            } else if pointer.is_some_and(|p| !on(p)) && !touch {
                let since = *mm.away_at.get_or_insert(now);
                if now - since >= LEAVE_GRACE {
                    mm.open = false;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(LEAVE_GRACE - (now - since)));
                }
            } else {
                mm.away_at = None;
            }
        }
        if !usable {
            mm.open = false;
            mm.drag = None;
        }
        if !mm.open {
            mm.away_at = None;
        }
        let grown = ctx.animate_bool_with_time(Id::unique("minimap_reveal"), mm.open, GROW_TIME);
        if grown <= 0.0 {
            mm.rect = None;
            return;
        }
        let width = bar.width() + (WIDTH - bar.width()) * grown;
        let map_rect = Rect::from_min_max(pos2(bar.right() - width, full.top()), full.max);
        mm.rect = Some(map_rect);

        // Content space as the minimap shows it: the pages as wide as it.
        // While it grows they grow with it, made for its full width.
        let inner = map_rect.shrink(PAD.min(width * 0.25));
        let content_w = layout.content_w().max(1.0);
        let content_h = layout.content_h(scale) - TOP_PAD_PX / scale;
        let m = inner.width().max(1.0) / content_w;
        let m_full = full.shrink(PAD).width() / content_w;
        let top = Map::top_for(inner, m, content_h, self.scroll.y, max.y);
        let map = Map { inner, m, top };
        let view = View::new(canvas, self.scroll, scale, *layout);
        let seen = Rect::from_min_max(
            view.screen_to_content(canvas.left_top() + vec2(0.0, TOP_PAD_PX)),
            view.screen_to_content(canvas.right_bottom()),
        );
        let view_box = Rect::from_min_max(map.to_mini(seen.min), map.to_mini(seen.max));
        // The box's top on the minimap for a scroll `y` (it scrolls too).
        let box_top = |y: f32| (y - Map::top_for(inner, m, content_h, y, max.y)) * m;
        // Points the box moves per content unit scrolled down.
        let per_unit = if max.y > 0.0 { ((box_top(max.y) - box_top(0.0)) / max.y).max(m * 0.05) } else { m };

        let area = egui::Area::new(Id::unique("minimap"))
            .order(Order::Foreground)
            .constrain(false)
            .fixed_pos(map_rect.min)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(map_rect.size(), Sense::click_and_drag());
                let painter = ui.painter_at(rect);
                // The bar's track, wider; the pages come in as it grows.
                painter.rect_filled(rect, 0.0, color::chrome());
                painter.line_segment([rect.left_top(), rect.left_bottom()], egui::Stroke::new(1.0, color::border()));
                let mut pages = painter.with_clip_rect(inner);
                pages.set_opacity(grown);
                self.paint_minimap_pages(&pages, &map, m_full, layout);
                let boxed = view_box.intersect(inner.expand(1.0));
                let active = self.minimap.drag.is_some();
                // Grey, as the scroll bar's thumb it grew from (darker held).
                let grey = if active { color::text_faint() } else { color::border_strong() };
                painter.rect_filled(boxed, 2.0, grey.gamma_multiply(0.6));
                painter.rect_stroke(boxed, 2.0, egui::Stroke::new(1.5, grey), egui::StrokeKind::Inside);
                // Where the box is, in pages (once there is room for it).
                if !layout.canvas && grown >= 1.0 {
                    let page = layout.page_index_at_y(seen.center().y);
                    let label = format!("{} / {}", page + 1, layout.count);
                    let galley = painter.layout_no_wrap(label, theme::font(theme::size::CAPTION), color::text_strong());
                    let at = pos2(boxed.center().x - galley.size().x * 0.5, boxed.top() + 3.0);
                    let bg = Rect::from_min_size(at, galley.size()).expand2(vec2(5.0, 2.0));
                    painter.rect_filled(bg, 4.0, color::raised().gamma_multiply(0.9));
                    painter.galley(at, galley, color::text_strong());
                }
                resp
            });
        let resp = area.inner;

        // The box dragged: the view follows (as the scroll bar's thumb does).
        // A press elsewhere: the box goes there, centred on it, and follows.
        let per = vec2(m, per_unit);
        match resp.interact_pointer_pos().filter(|_| resp.is_pointer_button_down_on()) {
            Some(p) => {
                let (at, start) = *self.minimap.drag.get_or_insert_with(|| {
                    let to = if view_box.contains(p) { Vec2::ZERO } else { (p - view_box.center()) / per };
                    (p, (self.scroll + to).clamp(Vec2::ZERO, max))
                });
                let wanted = start + (p - at) / per;
                self.scroll = wanted.clamp(Vec2::ZERO, max);
                // An infinite page: the box held against an edge of it goes
                // on that way, faster the further the pointer is past it, and
                // the page grows to follow (as with the scroll bars). The
                // drag goes on from there, the pointer as far past the edge.
                let past = (wanted - self.scroll) * per;
                if layout.canvas && past != Vec2::ZERO {
                    let speed = |d: f32| if d == 0.0 { 0.0 } else { bar_push_speed(Some((d > 0.0, d.abs()))) };
                    let dt = ctx.input(|i| i.stable_dt.min(1.0 / 30.0));
                    self.scroll += vec2(speed(past.x), speed(past.y)) * dt / scale;
                    self.minimap.drag = Some((p - past, self.scroll));
                }
                self.kinetic.stop();
                ctx.request_repaint();
            }
            None => self.minimap.drag = None,
        }
    }

    /// The pages the minimap shows (`map`), drawn with `painter` (clipped to
    /// them), their strokes made for `m_full` points per unit, its scale
    /// grown.
    fn paint_minimap_pages(&mut self, painter: &egui::Painter, map: &Map, m_full: f32, layout: &Layout) {
        let ctx = painter.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let made = self.minimap.zoom.update(&ctx, m_full, false);
        self.minimap.cache.begin_frame(ppp);
        let (from, to) = (map.top, map.top + map.inner.height() / map.m);
        for page in layout.pages_overlapping(from, to) {
            let origin = map.to_mini(layout.page_origin(page));
            // The whole page, from its corner (what its PDF is placed on).
            let whole = Rect::from_min_size(origin, vec2(layout.page_w, layout.page_h) * map.m);
            // An infinite page: as far as it is shown, from the corner of content space.
            let rect = if layout.canvas {
                Rect::from_min_size(map.to_mini(pos2(0.0, 0.0)), layout.extent * map.m)
            } else {
                whole
            };
            if !rect.intersects(painter.clip_rect()) {
                continue;
            }
            painter.rect_filled(rect, 0.0, color::page());
            let clipped = painter.with_clip_rect(painter.clip_rect().intersect(rect));
            self.pdf_textures.paint(&clipped, &self.doc, page, whole, false, 1.0);
            self.picture_textures.paint_page(&clipped, &self.doc.images, page, origin, (map.m, made), &[]);
            let strokes = &self.doc.strokes;
            let aa = 1.0 / ppp;
            render::paint_page_strokes(
                &clipped,
                origin.to_vec2(),
                map.m,
                made,
                strokes,
                page,
                &mut self.minimap.cache,
                aa,
            );
            render::paint_page_texts(&clipped, origin.to_vec2(), map.m, &self.doc.texts, page);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tall_note_scrolls_in_it_with_the_view() {
        let inner = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 400.0));
        // 0.1 point per unit: 4000 units fit, a note 10000 tall does not.
        let (m, content_h, max_y) = (0.1, 10_000.0, 9_000.0);
        assert_eq!(Map::top_for(inner, m, content_h, 0.0, max_y), 0.0);
        assert_eq!(Map::top_for(inner, m, content_h, max_y, max_y), 6_000.0, "its end at the bottom");
        assert_eq!(Map::top_for(inner, m, content_h, max_y * 0.5, max_y), 3_000.0);
        // A short note: all of it, always.
        assert_eq!(Map::top_for(inner, m, 3_000.0, 500.0, 1_000.0), 0.0);
        let map = Map { inner, m, top: 3_000.0 };
        assert_eq!(map.to_mini(pos2(500.0, 3_500.0)), pos2(50.0, 50.0));
    }
}
