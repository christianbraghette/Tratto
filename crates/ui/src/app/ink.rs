// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Writing and erasing: the pen and highlighter strokes being drawn, and the
//! eraser (whole strokes, or only the ink under it: `EraserMode`).

use eframe::egui::{self, PointerButton, Pos2};

use tratto_core::history::Action;
use tratto_core::model::{Stroke, StrokeKind};
use tratto_core::stroke_builder::StrokeBuilder;

use crate::render::LiveStroke;
use crate::settings::EraserMode;
use crate::toolbar::Tool;
use crate::view::{Layout, View};

use super::{ActiveStroke, InkApp, to_page_point};

/// Samples closer than this (document units) are treated as jitter.
const MIN_POINT_DIST: f32 = 0.6;
/// Smoothing strength at rest (0..1, lower = smoother) ...
const SMOOTH_ALPHA_MIN: f32 = 0.4;
/// ... fading to none at this per-sample distance (document units).
const SMOOTH_SPEED_REF: f32 = 6.0;
const HIGHLIGHTER_ALPHA: u8 = 96;

impl InkApp {
    pub(super) fn stroke_params(&self) -> Option<(StrokeKind, [u8; 4], f32)> {
        match self.tool {
            Tool::Pen => {
                let s = self.settings.pen;
                Some((StrokeKind::Pen, [s.color[0], s.color[1], s.color[2], 255], s.width))
            }
            Tool::Highlighter => {
                let s = self.settings.highlighter;
                Some((StrokeKind::Highlighter, [s.color[0], s.color[1], s.color[2], HIGHLIGHTER_ALPHA], s.width))
            }
            Tool::Pointer | Tool::Eraser | Tool::Select | Tool::Text => None,
        }
    }

    pub(super) fn handle_ink(&mut self, resp: &egui::Response, view: &View, press: Option<Pos2>, moves: &[Pos2]) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let (Some(pos), Some((kind, color, width))) =
                (press.or(resp.interact_pointer_pos()), self.stroke_params())
            {
                // A stroke belongs to the page it starts on; the gaps and
                // margins between pages are not writable.
                if let Some(page) = view.layout.page_at(view.screen_to_content(pos)) {
                    let mut builder = StrokeBuilder::new(MIN_POINT_DIST, SMOOTH_ALPHA_MIN, SMOOTH_SPEED_REF);
                    builder.begin(to_page_point(view, page, pos, self.pen_pressure()));
                    self.active = Some(ActiveStroke { page, kind, color, width, builder, live: LiveStroke::default() });
                }
            }
        } else if resp.dragged() || resp.drag_stopped() {
            let pressure = self.pen_pressure();
            if let Some(active) = self.active.as_mut() {
                let page = active.page;
                for pos in moves {
                    active.builder.push(to_page_point(view, page, *pos, pressure));
                }
            }
        }
        if self.active.is_some() && !resp.dragged() {
            self.commit_active();
        }
    }

    fn commit_active(&mut self) {
        if let Some(a) = self.active.take() {
            let points = a.builder.finish();
            if points.is_empty() {
                return;
            }
            let id = self.doc.alloc_id();
            let stroke = Stroke::new(id, a.page, a.kind, a.color, a.width, points);
            let index = self.doc.strokes.len();
            self.history.exec(&mut self.doc, Action::AddStroke { index, stroke });
        }
    }

    pub(super) fn handle_erase(&mut self, resp: &egui::Response, view: &View, press: Option<Pos2>, moves: &[Pos2]) {
        if resp.drag_started_by(PointerButton::Primary) {
            self.last_erase_pos = None;
            if let Some(p) = press.or(resp.interact_pointer_pos()) {
                self.erase_along(view.screen_to_content(p));
            }
        } else if resp.dragged() || resp.drag_stopped() {
            for p in moves {
                self.erase_along(view.screen_to_content(*p));
            }
        }
        if !resp.dragged() {
            self.finish_erase();
        }
    }

    /// Erases along the path from the previous cursor position to `target`
    /// (content space). Intermediate samples prevent fast flicks from
    /// skipping over strokes.
    fn erase_along(&mut self, target: Pos2) {
        let radius = self.settings.eraser_radius;
        let from = self.last_erase_pos.unwrap_or(target);
        let step = (radius * 0.5).max(1.0);
        let steps = ((from.distance(target) / step).ceil() as usize).max(1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let c = from + (target - from) * t;
            self.erase_at(c, radius);
        }
        self.last_erase_pos = Some(target);
    }

    fn erase_at(&mut self, center: Pos2, radius: f32) {
        let layout = Layout { page_w: self.doc.page_width, page_h: self.doc.page_height, count: self.doc.page_count };
        // The strokes hit, and where the eraser is on their page.
        let mut hits: Vec<(u64, Pos2)> = Vec::new();
        // The eraser circle may touch two neighbouring pages near the gap.
        for page in layout.pages_overlapping(center.y - radius, center.y + radius) {
            let local = center - layout.page_origin(page).to_vec2();
            hits.extend(
                self.doc.strokes.iter().filter(|s| s.page == page && s.hit_test(local, radius)).map(|s| (s.id, local)),
            );
        }
        // Only ink: text boxes are deleted from their context menu (or by
        // emptying them), never wiped by accident.
        let partial = self.settings.eraser_mode == EraserMode::Partial;
        for (id, local) in hits {
            if !partial {
                let inverse = Action::RemoveStroke { id }.apply(&mut self.doc);
                self.erase_session.push(inverse);
                continue;
            }
            // Partial: the stroke gives way to what is left of it, new
            // strokes in its place in the draw order.
            let Some(index) = self.doc.strokes.iter().position(|s| s.id == id) else { continue };
            let s = &self.doc.strokes[index];
            let Some(pieces) = s.erased(local, radius) else { continue };
            let (page, kind, color, width) = (s.page, s.kind, s.color, s.width);
            self.erase_session.push(Action::RemoveStroke { id }.apply(&mut self.doc));
            for (k, points) in pieces.into_iter().enumerate() {
                let stroke = Stroke::new(self.doc.alloc_id(), page, kind, color, width, points);
                self.erase_session.push(Action::AddStroke { index: index + k, stroke }.apply(&mut self.doc));
            }
        }
    }

    /// Records everything erased during one drag as a single undo step.
    fn finish_erase(&mut self) {
        self.last_erase_pos = None;
        if self.erase_session.is_empty() {
            return;
        }
        let inverses = std::mem::take(&mut self.erase_session);
        // Undo must re-insert in the reverse order of removal.
        self.history.push_undo(Action::Batch(inverses.into_iter().rev().collect()));
    }
}
