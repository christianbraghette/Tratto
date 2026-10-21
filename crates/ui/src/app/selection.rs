// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The select tool: lasso, select all on a page, moving and deleting the
//! selected strokes; a tap on a picture picks it (see `pictures`).

use std::collections::HashSet;

use eframe::egui::{self, PointerButton, Pos2, Rect, Vec2, pos2};

use tratto_core::geometry::point_in_polygon;
use tratto_core::history::Action;

use crate::toolbar::Tool;
use crate::view::{Layout, View};

use super::{InkApp, Lasso, MoveDrag, Selection, to_page_point};

impl InkApp {
    pub(super) fn clear_selection(&mut self) {
        self.selection = None;
        self.lasso = None;
        self.moving = None;
        self.picture = None;
        self.picture_drag = None;
    }

    /// Selects every stroke on `page` with the select tool.
    pub(super) fn select_all_on_page(&mut self, page: u32) {
        let ids: HashSet<u64> = self.doc.strokes.iter().filter(|s| s.page == page).map(|s| s.id).collect();
        if ids.is_empty() {
            self.toast(format!("Nothing to select on page {}", page + 1));
            return;
        }
        if self.edit.is_some() {
            self.edit = None;
        }
        self.tool = Tool::Select;
        self.saved_tool = None;
        self.picture = None;
        self.selection = self.selection_for(page, ids);
    }

    fn selection_for(&self, page: u32, ids: HashSet<u64>) -> Option<Selection> {
        let mut bounds: Option<Rect> = None;
        for s in self.doc.strokes.iter().filter(|s| ids.contains(&s.id)) {
            let pad = s.width * 0.5;
            let r = Rect::from_min_max(
                pos2(s.bounds[0] - pad, s.bounds[1] - pad),
                pos2(s.bounds[2] + pad, s.bounds[3] + pad),
            );
            bounds = Some(bounds.map_or(r, |b| b.union(r)));
        }
        bounds.map(|bounds| Selection { page, ids, bounds })
    }

    /// Lasso released: pick the strokes that are mostly inside it. A simple
    /// tap selects the stroke under the pen instead, or else the picture.
    fn finish_lasso(&mut self) {
        let Some(l) = self.lasso.take() else { return };
        let mut ids = HashSet::new();
        let path_len: f32 = l.points.windows(2).map(|w| w[0].distance(w[1])).sum();

        if l.points.len() < 4 || path_len < 8.0 {
            if let Some(p) = l.points.first() {
                match self.doc.strokes.iter().rev().find(|s| s.page == l.page && s.hit_test(*p, 4.0)) {
                    Some(s) => {
                        ids.insert(s.id);
                    }
                    None => self.picture = self.picture_at(l.page, *p),
                }
            }
        } else {
            let poly_bounds = Rect::from_points(&l.points);
            for s in self.doc.strokes.iter().filter(|s| s.page == l.page) {
                let sb = Rect::from_min_max(pos2(s.bounds[0], s.bounds[1]), pos2(s.bounds[2], s.bounds[3]));
                if !sb.intersects(poly_bounds) {
                    continue;
                }
                let inside = s.points.iter().filter(|p| point_in_polygon(p.pos2(), &l.points)).count();
                if inside * 2 >= s.points.len().max(1) {
                    ids.insert(s.id);
                }
            }
        }
        self.selection = self.selection_for(l.page, ids);
    }

    /// Drop the dragged selection: possibly onto another page, kept inside it.
    fn commit_move(&mut self, layout: Layout) {
        let Some(m) = self.moving.take() else { return };
        let Some(sel) = self.selection.as_mut() else {
            return;
        };
        let d = m.delta;
        if d.length() < 0.5 {
            return;
        }
        let src_o = layout.page_origin(sel.page).to_vec2();
        let center = sel.bounds.center() + src_o + d;
        let dst = layout.page_at(center).unwrap_or(sel.page);
        let dst_o = layout.page_origin(dst).to_vec2();

        // Delta expressed in the destination page's space.
        let mut local = d + src_o - dst_o;
        let nb = sel.bounds.translate(local);
        if nb.width() <= layout.page_w {
            if nb.min.x < 0.0 {
                local.x -= nb.min.x;
            } else if nb.max.x > layout.page_w {
                local.x -= nb.max.x - layout.page_w;
            }
        }
        if nb.height() <= layout.page_h {
            if nb.min.y < 0.0 {
                local.y -= nb.min.y;
            } else if nb.max.y > layout.page_h {
                local.y -= nb.max.y - layout.page_h;
            }
        }

        let ids: Vec<u64> = sel.ids.iter().copied().collect();
        sel.bounds = sel.bounds.translate(local);
        sel.page = dst;
        self.history.exec(&mut self.doc, Action::MoveStrokes { ids, to_page: dst, dx: local.x, dy: local.y });
    }

    pub(super) fn delete_selection(&mut self) {
        if let Some(id) = self.picture {
            self.delete_picture(id);
            return;
        }
        let Some(sel) = self.selection.take() else {
            return;
        };
        self.lasso = None;
        self.moving = None;
        let mut inverses = Vec::new();
        for id in sel.ids {
            inverses.push(Action::RemoveStroke { id }.apply(&mut self.doc));
        }
        if !inverses.is_empty() {
            self.history.push_undo(Action::Batch(inverses.into_iter().rev().collect()));
        }
    }

    pub(super) fn handle_select(&mut self, resp: &egui::Response, view: &View, press: Option<Pos2>, moves: &[Pos2]) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = press.or(resp.interact_pointer_pos())
                && !self.begin_picture_drag(view, pos)
            {
                let c = view.screen_to_content(pos);
                let inside = self
                    .selection
                    .as_ref()
                    .is_some_and(|s| s.bounds.translate(view.layout.page_origin(s.page).to_vec2()).contains(c));
                if inside {
                    self.moving = Some(MoveDrag { start: c, delta: Vec2::ZERO });
                } else {
                    self.selection = None;
                    self.picture = None;
                    if let Some(page) = view.layout.page_at(c) {
                        let p = to_page_point(view, page, pos, 1.0).pos2();
                        self.lasso = Some(Lasso { page, points: vec![p] });
                    }
                }
            }
        } else if resp.dragged() || resp.drag_stopped() {
            if self.picture_drag.is_some() {
                if let Some(last) = moves.last() {
                    self.drag_picture(view, *last);
                }
            } else if let Some(m) = self.moving.as_mut() {
                if let Some(last) = moves.last() {
                    m.delta = view.screen_to_content(*last) - m.start;
                }
            } else if let Some(l) = self.lasso.as_mut() {
                for pos in moves {
                    let p = to_page_point(view, l.page, *pos, 1.0).pos2();
                    if l.points.last().is_none_or(|q| q.distance(p) > 1.5) {
                        l.points.push(p);
                    }
                }
            }
        }
        if !resp.dragged() {
            if self.picture_drag.is_some() {
                self.finish_picture_drag();
            } else if self.moving.is_some() {
                self.commit_move(view.layout);
            } else if self.lasso.is_some() {
                self.finish_lasso();
            }
        }
    }
}
