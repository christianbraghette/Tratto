// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The select tool: lasso or rectangle (`SelectMode`), select all on a page,
//! moving and deleting what is selected (strokes, text boxes and pictures
//! together); a tap on a picture picks it alone (see `pictures`). The
//! handles of the selection's box resize and rotate it (see `transform`).

use std::collections::HashSet;

use eframe::egui::{self, PointerButton, Pos2, Rect, Vec2, pos2};

use tratto_core::geometry::point_in_polygon;
use tratto_core::history::Action;

use crate::settings::SelectMode;
use crate::toolbar::Tool;
use crate::view::{Layout, View};

use super::{InkApp, Lasso, MoveDrag, Selection, to_page_point};

impl InkApp {
    pub(super) fn clear_selection(&mut self) {
        // Resized or rotated so far: kept, and undoable.
        self.finish_transform();
        self.selection = None;
        self.lasso = None;
        self.moving = None;
        self.picture = None;
        self.picture_drag = None;
    }

    /// Selects everything on `page` with the select tool.
    pub(super) fn select_all_on_page(&mut self, page: u32) {
        // What is being typed is kept, and selected with the rest.
        if self.edit.is_some() {
            let ctx = self.egui_ctx.clone();
            self.commit_edit(&ctx);
        }
        let strokes = self.doc.strokes.iter().filter(|s| s.page == page).map(|s| s.id).collect();
        let texts = self.doc.texts.iter().filter(|t| t.page == page).map(|t| t.id).collect();
        let images = self.doc.images.iter().filter(|i| i.page == page).map(|i| i.id).collect();
        let Some(selection) = self.selection_for(page, strokes, texts, images) else {
            self.toast(format!("Nothing to select on page {}", page + 1));
            return;
        };
        self.tool = Tool::Select;
        self.saved_tool = None;
        self.picture = None;
        self.selection = Some(selection);
    }

    /// The selection of these strokes, text boxes and pictures of `page`;
    /// `None` if there are none.
    pub(super) fn selection_for(
        &self,
        page: u32,
        ids: HashSet<u64>,
        texts: HashSet<u64>,
        images: HashSet<u64>,
    ) -> Option<Selection> {
        let strokes = self.doc.strokes.iter().filter(|s| ids.contains(&s.id)).map(|s| {
            let pad = s.width * 0.5;
            Rect::from_min_max(pos2(s.bounds[0] - pad, s.bounds[1] - pad), pos2(s.bounds[2] + pad, s.bounds[3] + pad))
        });
        let text_rects = self.doc.texts.iter().filter(|t| texts.contains(&t.id)).map(|t| t.rect());
        let image_rects = self.doc.images.iter().filter(|i| images.contains(&i.id)).map(|i| i.rect());
        let bounds = strokes.chain(text_rects).chain(image_rects).reduce(|a, b| a.union(b))?;
        Some(Selection { page, ids, texts, images, bounds })
    }

    /// Lasso (or rectangle) released: pick the strokes that are mostly inside
    /// it, and the text boxes and pictures whose middle is. A simple tap
    /// selects the stroke under the pen instead, else picks the picture, else
    /// selects the text box.
    fn finish_lasso(&mut self) {
        let Some(l) = self.lasso.take() else { return };
        let (mut ids, mut texts, mut images) = (HashSet::new(), HashSet::new(), HashSet::new());

        if l.is_tap() {
            if let Some(&p) = l.points.first() {
                if let Some(s) = self.doc.strokes.iter().rev().find(|s| s.page == l.page && s.hit_test(p, 4.0)) {
                    ids.insert(s.id);
                } else if let Some(id) = self.picture_at(l.page, p) {
                    self.picture = Some(id);
                } else if let Some(t) = self.doc.texts.iter().rev().find(|t| t.page == l.page && t.rect().contains(p)) {
                    texts.insert(t.id);
                }
            }
        } else {
            let outline = l.outline();
            let poly_bounds = Rect::from_points(&outline);
            for s in self.doc.strokes.iter().filter(|s| s.page == l.page) {
                let sb = Rect::from_min_max(pos2(s.bounds[0], s.bounds[1]), pos2(s.bounds[2], s.bounds[3]));
                if !sb.intersects(poly_bounds) {
                    continue;
                }
                let inside = s.points.iter().filter(|p| point_in_polygon(p.pos2(), &outline)).count();
                if inside * 2 >= s.points.len().max(1) {
                    ids.insert(s.id);
                }
            }
            let inside = |r: Rect| point_in_polygon(r.center(), &outline);
            texts.extend(self.doc.texts.iter().filter(|t| t.page == l.page && inside(t.rect())).map(|t| t.id));
            images.extend(self.doc.images.iter().filter(|i| i.page == l.page && inside(i.rect())).map(|i| i.id));
        }
        self.selection = self.selection_for(l.page, ids, texts, images);
    }

    /// Drop the dragged selection: onto the page its middle is nearest to
    /// (possibly another), kept inside it.
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
        let dst = layout.page_nearest_y(center.y);
        let dst_o = layout.page_origin(dst).to_vec2();

        // Delta expressed in the destination page's space, kept on it.
        let local = super::clipboard::keep_on_page(sel.bounds, d + src_o - dst_o, layout.page_box());

        sel.bounds = sel.bounds.translate(local);
        sel.page = dst;
        // One undo step for everything moved.
        let mut actions = Vec::new();
        if !sel.ids.is_empty() {
            let ids = sel.ids.iter().copied().collect();
            actions.push(Action::MoveStrokes { ids, to_page: dst, dx: local.x, dy: local.y });
        }
        for t in self.doc.texts.iter().filter(|t| sel.texts.contains(&t.id)) {
            let text = tratto_core::model::TextBox { page: dst, x: t.x + local.x, y: t.y + local.y, ..t.clone() };
            actions.push(Action::ReplaceText { text });
        }
        for i in self.doc.images.iter().filter(|i| sel.images.contains(&i.id)) {
            let image = tratto_core::model::Image { page: dst, x: i.x + local.x, y: i.y + local.y, ..i.clone() };
            actions.push(Action::ReplaceImage { image });
        }
        self.history.exec(&mut self.doc, Action::Batch(actions));
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
        let removals = (sel.ids.into_iter().map(|id| Action::RemoveStroke { id }))
            .chain(sel.texts.into_iter().map(|id| Action::RemoveText { id }))
            .chain(sel.images.into_iter().map(|id| Action::RemoveImage { id }));
        self.history.exec(&mut self.doc, Action::Batch(removals.collect()));
    }

    /// The selection being dragged: where its text boxes and pictures are
    /// shown meanwhile (their page's space, moved by `offset`, content space).
    pub(super) fn selection_offsets(&self) -> (Vec<(u64, Vec2)>, Vec<tratto_core::model::Image>) {
        let (Some(sel), Some(m)) = (&self.selection, &self.moving) else { return Default::default() };
        let texts = sel.texts.iter().map(|id| (*id, m.delta)).collect();
        let images = (self.doc.images.iter().filter(|i| sel.images.contains(&i.id)))
            .map(|i| tratto_core::model::Image { x: i.x + m.delta.x, y: i.y + m.delta.y, ..i.clone() })
            .collect();
        (texts, images)
    }

    pub(super) fn handle_select(&mut self, resp: &egui::Response, view: &View, press: Option<Pos2>, moves: &[Pos2]) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = press.or(resp.interact_pointer_pos())
                && !self.begin_transform(view, pos)
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
                        let rect = self.settings.select_mode == SelectMode::Rectangle;
                        self.lasso = Some(Lasso { page, points: vec![p], rect });
                    }
                }
            }
        } else if resp.dragged() || resp.drag_stopped() {
            if self.transform.is_some() {
                if let Some(last) = moves.last() {
                    self.drag_transform(view, *last);
                }
            } else if self.picture_drag.is_some() {
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
                    if l.rect {
                        // The box: from the corner pressed to here.
                        l.points.truncate(1);
                        l.points.push(p);
                    } else if l.points.last().is_none_or(|q| q.distance(p) > 1.5) {
                        l.points.push(p);
                    }
                }
            }
        }
        if !resp.dragged() {
            if self.transform.is_some() {
                self.finish_transform();
            } else if self.picture_drag.is_some() {
                self.finish_picture_drag();
            } else if self.moving.is_some() {
                self.commit_move(view.layout);
            } else if self.lasso.is_some() {
                self.finish_lasso();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_selects_with_its_box() {
        let rect = |points: Vec<Pos2>| Lasso { page: 0, points, rect: true };
        // Dragged up and left: the same box, its corners clockwise.
        let r = rect(vec![pos2(100.0, 80.0), pos2(20.0, 10.0)]);
        assert_eq!(r.outline(), [pos2(20.0, 10.0), pos2(100.0, 10.0), pos2(100.0, 80.0), pos2(20.0, 80.0)]);
        assert!(!r.is_tap());
        // Pressed and let go (almost) in place: a tap.
        assert!(rect(vec![pos2(5.0, 5.0)]).is_tap());
        assert!(rect(vec![pos2(5.0, 5.0), pos2(8.0, 7.0)]).is_tap());
        // A lasso is its loop.
        let lasso = Lasso {
            page: 0,
            points: vec![pos2(0.0, 0.0), pos2(30.0, 0.0), pos2(30.0, 30.0), pos2(0.0, 30.0)],
            rect: false,
        };
        assert_eq!(lasso.outline().len(), 4);
        assert!(!lasso.is_tap());
    }
}
