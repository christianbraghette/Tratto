// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Text boxes: picking, editing in place, committing and moving them.

use eframe::egui::{self, Color32, FontId, PointerButton, Pos2, Rect, Vec2, pos2, vec2};

use tratto_core::history::Action;
use tratto_core::model::TextBox;

use crate::view::{Layout, View};

use super::{EditState, InkApp, TextPress};

impl InkApp {
    /// Top-most text box under a content-space point.
    pub(super) fn text_at(&self, layout: &Layout, c: Pos2) -> Option<u64> {
        self.doc
            .texts
            .iter()
            .rev()
            .find(|t| {
                let o = layout.page_origin(t.page);
                Rect::from_min_size(pos2(o.x + t.x, o.y + t.y), vec2(t.width, t.height())).expand(4.0).contains(c)
            })
            .map(|t| t.id)
    }

    pub(super) fn begin_edit(&mut self, id: u64) {
        let Some(t) = self.doc.texts.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        // The toolbar now shows (and changes) this box's style.
        self.settings.text.color = t.color;
        self.settings.text.width = t.size;
        self.edit = Some(EditState {
            id,
            page: t.page,
            x: t.x,
            y: t.y,
            width: t.width,
            buf: t.text.clone(),
            size: t.size,
            color: t.color,
            last_h: 0.0,
            focus_pending: true,
            orig: Some(t),
        });
    }

    fn begin_new_edit(&mut self, page: u32, x: f32, y: f32, width: f32) {
        let id = self.doc.alloc_id();
        self.edit = Some(EditState {
            id,
            page,
            x,
            y,
            width,
            buf: String::new(),
            size: self.settings.text.width,
            color: self.settings.text.color,
            last_h: 0.0,
            focus_pending: true,
            orig: None,
        });
    }

    /// Finishes editing: lays the text out, then records an add / replace /
    /// remove in the history (nothing if unchanged, or an empty new box).
    pub(super) fn commit_edit(&mut self, ctx: &egui::Context) {
        let Some(e) = self.edit.take() else { return };
        if e.buf.trim().is_empty() {
            if let Some(orig) = e.orig {
                self.history.exec(&mut self.doc, Action::RemoveText { id: orig.id });
            }
            return;
        }
        let (lines, line_h) = layout_lines(ctx, &e.buf, e.size, e.width);
        let tb = TextBox {
            id: e.id,
            page: e.page,
            x: e.x,
            y: e.y,
            width: e.width,
            size: e.size,
            color: e.color,
            text: e.buf,
            lines,
            line_h,
        };
        match e.orig {
            None => {
                let index = self.doc.texts.len();
                self.history.exec(&mut self.doc, Action::AddText { index, text: tb });
            }
            Some(orig) => {
                if orig != tb {
                    self.history.exec(&mut self.doc, Action::ReplaceText { text: tb });
                }
            }
        }
    }

    pub(super) fn handle_text(&mut self, resp: &egui::Response, view: &View, press: Option<Pos2>, moves: &[Pos2]) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = press.or(resp.interact_pointer_pos()) {
                let c = view.screen_to_content(pos);
                self.text_press = Some(TextPress {
                    hit: self.text_at(&view.layout, c),
                    start: c,
                    now: c,
                    page: view.layout.page_at(c),
                    moved: false,
                });
            }
        } else if (resp.dragged() || resp.drag_stopped())
            && let (Some(tp), Some(last)) = (self.text_press.as_mut(), moves.last())
        {
            tp.now = view.screen_to_content(*last);
            if (tp.now - tp.start).length() * view.scale > 5.0 {
                tp.moved = true;
            }
        }
        if !resp.dragged()
            && let Some(tp) = self.text_press.take()
        {
            self.finish_text_press(tp, view.layout);
        }
    }

    fn finish_text_press(&mut self, tp: TextPress, layout: Layout) {
        match (tp.hit, tp.moved) {
            (Some(id), false) => self.begin_edit(id),
            (Some(id), true) => self.commit_text_move(id, tp.now - tp.start, layout),
            (None, _) => {
                // Tap = default width; drag = box as wide as the drag.
                let Some(page) = tp.page else { return };
                let o = layout.page_origin(page);
                let dragged_w = (tp.now.x - tp.start.x).abs();
                let (left, width) = if tp.moved && dragged_w > 40.0 {
                    (tp.start.x.min(tp.now.x), dragged_w)
                } else {
                    let room = layout.page_w - (tp.start.x - o.x) - 20.0;
                    (tp.start.x, room.clamp(120.0, 360.0))
                };
                let top = if tp.moved { tp.start.y.min(tp.now.y) } else { tp.start.y };
                let x = (left - o.x).clamp(0.0, (layout.page_w - 40.0).max(0.0));
                let y = (top - o.y).clamp(0.0, (layout.page_h - 20.0).max(0.0));
                let width = width.min(layout.page_w - x).max(40.0);
                self.begin_new_edit(page, x, y, width);
            }
        }
    }

    /// Drop a dragged text box, possibly onto another page, kept inside it.
    fn commit_text_move(&mut self, id: u64, d: Vec2, layout: Layout) {
        let Some(old) = self.doc.texts.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if d.length() < 0.5 {
            return;
        }
        let src_o = layout.page_origin(old.page).to_vec2();
        let center = pos2(old.x + old.width * 0.5, old.y + old.height() * 0.5) + src_o + d;
        let dst = layout.page_at(center).unwrap_or(old.page);
        let local = d + src_o - layout.page_origin(dst).to_vec2();
        let mut new = old.clone();
        new.page = dst;
        new.x = (old.x + local.x).clamp(0.0, (layout.page_w - old.width).max(0.0));
        new.y = (old.y + local.y).clamp(0.0, (layout.page_h - old.height()).max(0.0));
        self.history.exec(&mut self.doc, Action::ReplaceText { text: new });
    }
}

/// Wraps `text` to `width` with egui's font at the box's own size and returns
/// the lines plus the line height (all in document units).
fn layout_lines(ctx: &egui::Context, text: &str, size: f32, width: f32) -> (Vec<String>, f32) {
    let galley = ctx.fonts_mut(|f| f.layout(text.to_owned(), FontId::proportional(size), Color32::BLACK, width));
    let lines: Vec<String> = galley.rows.iter().map(|r| r.glyphs.iter().map(|g| g.chr).collect::<String>()).collect();
    let line_h = galley.rows.first().map_or(size * 1.25, |r| r.rect().height());
    (lines, line_h)
}
