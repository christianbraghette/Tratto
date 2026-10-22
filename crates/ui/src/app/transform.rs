// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Changing what is selected with the select tool: resizing it by a corner
//! of its box (its proportions kept, the opposite corner where it was),
//! rotating it by the handle above the box (ink only: text boxes and
//! pictures do not turn) and recolouring it (strokes and text boxes).
//!
//! While a handle is dragged the selection is changed in the document
//! itself, from copies of how it was, so it is drawn as it will be; letting
//! go records one undo step that puts the copies back.

use std::f32::consts::{PI, TAU};

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2, vec2};

use tratto_core::history::Action;
use tratto_core::model::{Image, TextBox};

use crate::markdown;
use crate::theme::color;
use crate::view::View;

use super::pictures::corners;
use super::{InkApp, to_page_point};

/// How close to a handle a press grabs it (points).
const HANDLE: f32 = 12.0;
/// How far above the selection's box the rotate handle is (points).
const ROTATE_GAP: f32 = 28.0;
/// The smallest the selection gets when resized, its longer side
/// (document units).
const MIN_SIZE: f32 = 8.0;
/// A rotation this close to a multiple of `SNAP_STEP` snaps to it.
const SNAP: f32 = 4.0 * PI / 180.0;
const SNAP_STEP: f32 = 15.0 * PI / 180.0;

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    /// A corner of the box, clockwise from the top-left.
    Corner(usize),
    Rotate,
}

/// The selection being resized or rotated.
pub(super) struct TransformDrag {
    handle: Handle,
    /// The selection's box when the press started (its page's space).
    from: Rect,
    /// Where the press was (page space).
    start: Pos2,
    /// The selection as it was.
    strokes: Vec<tratto_core::model::Stroke>,
    texts: Vec<TextBox>,
    images: Vec<Image>,
    /// The rotation now (radians), shown by the handle.
    angle: f32,
    /// It is no longer as it was.
    changed: bool,
}

/// The scale of corner `k` of `from` dragged to `now` (page space), about
/// the opposite corner (returned with it): along the box's diagonal, kept
/// on a page `page` large (unless it is out of it already).
fn corner_scale(from: Rect, k: usize, now: Pos2, page: Vec2) -> (Pos2, f32) {
    let c = corners(from);
    let anchor = c[(k + 2) % 4];
    let diag = c[k] - anchor;
    let s = (now - anchor).dot(diag) / diag.length_sq().max(f32::EPSILON);
    let min = MIN_SIZE / from.width().max(from.height()).max(f32::EPSILON);
    let room = |d: f32, a: f32, extent: f32| match d {
        d if d > 0.0 => (extent - a) / d,
        d if d < 0.0 => a / -d,
        _ => f32::INFINITY,
    };
    let max = room(diag.x, anchor.x, page.x).min(room(diag.y, anchor.y, page.y)).max(1.0);
    (anchor, s.clamp(min, max.max(min)))
}

/// The rotation from `start` to `now` around `center`, in `-PI..=PI`,
/// snapped to multiples of `SNAP_STEP` when close.
fn rotation(center: Pos2, start: Pos2, now: Pos2) -> f32 {
    let mut a = (now - center).angle() - (start - center).angle();
    a = (a + PI).rem_euclid(TAU) - PI;
    let snapped = (a / SNAP_STEP).round() * SNAP_STEP;
    if (a - snapped).abs() <= SNAP { snapped } else { a }
}

/// `p` turned by `angle` around `center`.
fn rotate(p: Pos2, center: Pos2, angle: f32) -> Pos2 {
    let (sin, cos) = angle.sin_cos();
    let d = p - center;
    center + vec2(d.x * cos - d.y * sin, d.x * sin + d.y * cos)
}

impl InkApp {
    /// The selection's box on screen, and whether it can be rotated (only
    /// strokes in it).
    fn selection_frame(&self, view: &View) -> Option<(Rect, bool)> {
        let sel = self.selection.as_ref()?;
        let r = sel.bounds.translate(view.layout.page_origin(sel.page).to_vec2());
        let rect = Rect::from_min_max(view.content_to_screen(r.min), view.content_to_screen(r.max));
        let rotatable = !sel.ids.is_empty() && sel.texts.is_empty() && sel.images.is_empty();
        Some((rect, rotatable))
    }

    /// The handle of the selection under the screen point `pos`.
    fn handle_at(&self, view: &View, pos: Pos2) -> Option<Handle> {
        let (rect, rotatable) = self.selection_frame(view)?;
        if rotatable && rotate_handle(rect).distance(pos) <= HANDLE {
            return Some(Handle::Rotate);
        }
        corners(rect).iter().position(|c| c.distance(pos) <= HANDLE).map(Handle::Corner)
    }

    /// A press at `pos` on a handle of the selection starts resizing or
    /// rotating it.
    pub(super) fn begin_transform(&mut self, view: &View, pos: Pos2) -> bool {
        let Some(handle) = self.handle_at(view, pos) else { return false };
        let Some(sel) = &self.selection else { return false };
        let start = to_page_point(view, sel.page, pos, 1.0).pos2();
        let strokes = self.doc.strokes.iter().filter(|s| sel.ids.contains(&s.id)).cloned().collect();
        let texts = self.doc.texts.iter().filter(|t| sel.texts.contains(&t.id)).cloned().collect();
        let images = self.doc.images.iter().filter(|i| sel.images.contains(&i.id)).cloned().collect();
        self.transform =
            Some(TransformDrag { handle, from: sel.bounds, start, strokes, texts, images, angle: 0.0, changed: false });
        true
    }

    /// The selection follows the handle dragged to the screen point `pos`.
    pub(super) fn drag_transform(&mut self, view: &View, pos: Pos2) {
        let (Some(d), Some(sel)) = (self.transform.as_mut(), self.selection.as_ref()) else { return };
        let now = to_page_point(view, sel.page, pos, 1.0).pos2();
        let ctx = self.egui_ctx.clone();
        let doc = &mut self.doc;
        let (map, scale): (Box<dyn Fn(Pos2) -> Pos2>, f32) = match d.handle {
            Handle::Corner(k) => {
                let page = vec2(view.layout.page_w, view.layout.page_h);
                let (anchor, s) = corner_scale(d.from, k, now, page);
                (Box::new(move |p| anchor + (p - anchor) * s), s)
            }
            Handle::Rotate => {
                let center = d.from.center();
                let angle = rotation(center, d.start, now);
                d.angle = angle;
                (Box::new(move |p| rotate(p, center, angle)), 1.0)
            }
        };
        d.changed = true;
        let strokes = d.strokes.iter().map(|s| s.mapped(&map, scale)).collect();
        Action::ReplaceStrokes { strokes }.apply(doc);
        for orig in &d.texts {
            let at = map(egui::pos2(orig.x, orig.y));
            let mut t =
                TextBox { x: at.x, y: at.y, width: orig.width * scale, size: orig.size * scale, ..orig.clone() };
            markdown::lay_out_box(&ctx, &mut t);
            Action::ReplaceText { text: t }.apply(doc);
        }
        for orig in &d.images {
            let at = map(egui::pos2(orig.x, orig.y));
            let (width, height) = (orig.width * scale, orig.height * scale);
            Action::ReplaceImage { image: Image { x: at.x, y: at.y, width, height, ..orig.clone() } }.apply(doc);
        }
        self.refresh_selection_bounds();
    }

    /// Lets go of the handle: one undo step for the change, if any.
    pub(super) fn finish_transform(&mut self) {
        let Some(d) = self.transform.take() else { return };
        let mut back = vec![Action::ReplaceStrokes { strokes: d.strokes }];
        back.extend(d.texts.into_iter().map(|text| Action::ReplaceText { text }));
        back.extend(d.images.into_iter().map(|image| Action::ReplaceImage { image }));
        let back = Action::Batch(back);
        if d.changed {
            self.history.push_undo(back);
        } else {
            back.apply(&mut self.doc);
        }
    }

    /// The selection's box, around what is selected as it is now.
    fn refresh_selection_bounds(&mut self) {
        let Some(sel) = self.selection.take() else { return };
        let page = sel.page;
        self.selection = self.selection_for(page, sel.ids, sel.texts, sel.images);
    }

    /// The colour of what is selected that can be recoloured (strokes and
    /// text boxes): the first one's, and whether all of it is highlighter.
    pub(super) fn selection_color(&self) -> Option<([u8; 3], bool)> {
        let sel = self.selection.as_ref()?;
        let strokes: Vec<_> = self.doc.strokes.iter().filter(|s| sel.ids.contains(&s.id)).collect();
        let highlighter =
            sel.texts.is_empty() && strokes.iter().all(|s| s.kind == tratto_core::model::StrokeKind::Highlighter);
        let first = strokes.first().map(|s| [s.color[0], s.color[1], s.color[2]]);
        let first = first.or_else(|| self.doc.texts.iter().find(|t| sel.texts.contains(&t.id)).map(|t| t.color))?;
        Some((first, highlighter && !strokes.is_empty()))
    }

    /// Gives the selected strokes and text boxes the colour `rgb` (one undo
    /// step); highlighters stay as see-through as they were.
    pub(super) fn recolor_selection(&mut self, rgb: [u8; 3]) {
        let Some(sel) = &self.selection else { return };
        let strokes: Vec<_> = (self.doc.strokes.iter())
            .filter(|s| sel.ids.contains(&s.id) && s.color[..3] != rgb)
            .map(|s| s.recolored(rgb))
            .collect();
        let mut actions = vec![Action::ReplaceStrokes { strokes }];
        actions.extend(
            (self.doc.texts.iter())
                .filter(|t| sel.texts.contains(&t.id) && t.color != rgb)
                .map(|t| Action::ReplaceText { text: TextBox { color: rgb, ..t.clone() } }),
        );
        if matches!(&actions[..], [Action::ReplaceStrokes { strokes }] if strokes.is_empty()) {
            return;
        }
        self.history.exec(&mut self.doc, Action::Batch(actions));
    }

    /// The selection's handles (corners, and rotation for ink), the angle
    /// while rotating, and the cursor that says what a press at `hover` does.
    pub(super) fn paint_selection_handles(&self, painter: &Painter, view: &View, hover: Option<Pos2>) {
        if self.moving.is_some() {
            return;
        }
        let Some((rect, rotatable)) = self.selection_frame(view) else { return };
        let handle_stroke = Stroke::new(1.5, color::accent());
        for c in corners(rect) {
            painter.rect(
                Rect::from_center_size(c, vec2(10.0, 10.0)),
                2.0,
                Color32::WHITE,
                handle_stroke,
                StrokeKind::Middle,
            );
        }
        if rotatable {
            let knob = rotate_handle(rect);
            painter.line_segment([rect.center_top(), knob], handle_stroke);
            painter.circle(knob, 6.0, Color32::WHITE, handle_stroke);
        }
        let dragging = self.transform.as_ref().map(|d| d.handle);
        if let Some(d) = self.transform.as_ref().filter(|d| d.handle == Handle::Rotate) {
            let label = format!("{:.0}°", d.angle.to_degrees());
            painter.text(
                rotate_handle(rect) + vec2(0.0, -14.0),
                Align2::CENTER_BOTTOM,
                label,
                FontId::proportional(13.0),
                color::accent(),
            );
        }
        let icon = match dragging.or_else(|| hover.and_then(|p| self.handle_at(view, p))) {
            Some(Handle::Corner(0 | 2)) => CursorIcon::ResizeNwSe,
            Some(Handle::Corner(_)) => CursorIcon::ResizeNeSw,
            Some(Handle::Rotate) => {
                // No system cursor says "rotate": a drawn one.
                painter.ctx().set_cursor_image(Some(rotate_cursor().clone()));
                return;
            }
            None => return,
        };
        painter.ctx().set_cursor_icon(icon);
    }
}

/// The cursor over the rotate handle: a circular arrow, black with a white
/// outline (seen on the paper and on the dark desk), made once. Its
/// hotspot is the circle's centre.
fn rotate_cursor() -> &'static egui::CustomCursorImage {
    static CURSOR: std::sync::OnceLock<egui::CustomCursorImage> = std::sync::OnceLock::new();
    CURSOR.get_or_init(|| {
        const SIZE: usize = 32;
        const SAMPLES: usize = 4;
        let center = egui::pos2(16.0, 16.0);
        let radius = 8.0;
        // The arc goes clockwise (y down) from `from` to `to`, leaving a gap
        // at the top right, and ends in the arrow head.
        let (from, to) = (-30f32.to_radians(), 240f32.to_radians());
        let on_arc = |p: Pos2| {
            let d = p - center;
            let a = d.y.atan2(d.x);
            let a = if a < from { a + std::f32::consts::TAU } else { a };
            (a <= to, (d.length() - radius).abs())
        };
        let end = center + radius * vec2(to.cos(), to.sin());
        let (along, out) = (vec2(-to.sin(), to.cos()), vec2(to.cos(), to.sin()));
        // The head: a triangle pointing along the arc, `grow` outlines it.
        let in_head = |p: Pos2, grow: f32| {
            let tip = end + along * (5.0 + grow);
            let (b1, b2) = (end - along * grow + out * (4.5 + grow), end - along * grow - out * (4.5 + grow));
            let side = |a: Pos2, b: Pos2| (b - a).x * (p - a).y - (b - a).y * (p - a).x;
            let (s1, s2, s3) = (side(tip, b1), side(b1, b2), side(b2, tip));
            (s1 >= 0.0 && s2 >= 0.0 && s3 >= 0.0) || (s1 <= 0.0 && s2 <= 0.0 && s3 <= 0.0)
        };
        let mut rgba = vec![0u8; SIZE * SIZE * 4];
        for y in 0..SIZE {
            for x in 0..SIZE {
                let (mut core, mut outline) = (0.0, 0.0);
                for sy in 0..SAMPLES {
                    for sx in 0..SAMPLES {
                        let p = egui::pos2(
                            x as f32 + (sx as f32 + 0.5) / SAMPLES as f32,
                            y as f32 + (sy as f32 + 0.5) / SAMPLES as f32,
                        );
                        let (along_arc, off) = on_arc(p);
                        if along_arc && off <= 1.1 || in_head(p, 0.0) {
                            core += 1.0;
                            outline += 1.0;
                        } else if along_arc && off <= 2.6 || in_head(p, 1.6) {
                            outline += 1.0;
                        }
                    }
                }
                let n = (SAMPLES * SAMPLES) as f32;
                let (core, outline) = (core / n, outline / n);
                if outline > 0.0 {
                    let grey = (255.0 * (1.0 - core / outline)).round() as u8;
                    let i = (y * SIZE + x) * 4;
                    rgba[i..i + 4].copy_from_slice(&[grey, grey, grey, (255.0 * outline).round() as u8]);
                }
            }
        }
        egui::CustomCursorImage { rgba: rgba.into(), size: [SIZE as u16; 2], hotspot: [16, 16] }
    })
}

/// Where the rotate handle of a selection whose box is `rect` (screen) is.
fn rotate_handle(rect: Rect) -> Pos2 {
    rect.center_top() - vec2(0.0, ROTATE_GAP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    const PAGE: Vec2 = vec2(800.0, 1000.0);

    #[test]
    fn a_corner_scales_about_the_opposite_one() {
        let from = Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 200.0));
        // The bottom-right corner pulled out along the diagonal: twice as large.
        let (anchor, s) = corner_scale(from, 2, pos2(500.0, 300.0), PAGE);
        assert_eq!((anchor, s), (pos2(100.0, 100.0), 2.0));
        // The top-left one pushed in: half as large, about the bottom-right.
        let (anchor, s) = corner_scale(from, 0, pos2(200.0, 150.0), PAGE);
        assert_eq!((anchor, s), (pos2(300.0, 200.0), 0.5));
        // Never smaller than the smallest size, nor out of the page.
        assert!(corner_scale(from, 2, pos2(0.0, 0.0), PAGE).1 * 200.0 >= MIN_SIZE - 1e-4);
        assert_eq!(corner_scale(from, 2, pos2(5000.0, 2500.0), PAGE).1, 3.5);
    }

    #[test]
    fn rotation_snaps_near_round_angles() {
        let c = pos2(0.0, 0.0);
        let start = pos2(10.0, 0.0);
        let quarter = rotation(c, start, pos2(0.0, 10.0));
        assert!((quarter - PI / 2.0).abs() < 1e-5);
        // 47° is near 45°: snapped.
        let a = 47f32.to_radians();
        assert!((rotation(c, start, pos2(a.cos(), a.sin())) - 45f32.to_radians()).abs() < 1e-5);
        // 52° is not.
        let a = 52f32.to_radians();
        assert!((rotation(c, start, pos2(a.cos(), a.sin())) - a).abs() < 1e-5);
        let p = rotate(pos2(10.0, 0.0), c, PI / 2.0);
        assert!((p - pos2(0.0, 10.0)).length() < 1e-4);
    }
}
