// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The editor's canvas: input routing to the tools, scrolling and zoom,
//! painting the pages, undo / redo and the editor shortcuts, and following
//! the links and outline of a PDF.

use std::sync::{Arc, OnceLock};

use eframe::egui::{
    self, Color32, Event, FontId, Id, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, TouchPhase,
    Vec2, pos2, vec2,
};

use tratto_core::model::StrokeKind;
use tratto_core::pdf_pages::{LinkAction, Target};

use crate::input::touchpad::{self, is_wheel_notch};
use crate::markdown;
use crate::render::{self, Layer, Moved};
use crate::scrollbar::{self, BarState};
use crate::settings::ZoomMode;
use crate::theme::{self, color};
use crate::toolbar::Tool;
use crate::view::{Layout, MARGIN, TOP_PAD_PX, View};

use super::InkApp;
use super::menus::ContextMenu;
use super::text::EDIT_MARGIN;
use super::text_select::{LONG_PRESS, PRESS_MOVE};

/// Places kept for Back (the oldest is forgotten).
const NAV_BACK_MAX: usize = 50;
/// A dragged item this close to an edge of the canvas (points) scrolls the
/// view...
const AUTOSCROLL_EDGE: f32 = 48.0;
/// ...up to this fast, at the edge and past it (points per second).
const AUTOSCROLL_SPEED: f32 = 1400.0;
/// A scroll bar's thumb held against an end of an infinite page's bar
/// scrolls on at this share of `AUTOSCROLL_SPEED`, faster past the end.
const BAR_PUSH_MIN: f32 = 0.15;
/// One mouse wheel notch (points on screen): Windows' three lines.
const WHEEL_NOTCH_PT: f32 = 100.0;

/// A finger held on the canvas, a long press opening the context menu.
#[derive(Clone, Copy, Debug)]
pub(super) enum TouchHold {
    /// Pressed at `pos` (screen) at `time`; opens the menu once held still
    /// long enough.
    Pressing { pos: Pos2, time: f64 },
    /// The menu is open: the rest of the press, up to its release, does
    /// nothing else.
    Opened,
}

impl InkApp {
    /// A finger's long press on the canvas (`resp`): where to open the
    /// context menu, the frame it is held long enough. Not on text in the
    /// reader or with the pointer tool, where it selects the word instead.
    /// What the press began (a stroke, a lasso, a drag) ends there.
    fn touch_hold_input(&mut self, ctx: &egui::Context, resp: &egui::Response, now: f64) -> Option<Pos2> {
        let (pressed, down, pointer, multi) = ctx.input(|i| {
            (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.multi_touch().is_some())
        });
        match self.touch_hold {
            None => {
                if pressed
                    && !self.swallow_press
                    && self.finger(ctx)
                    && resp.is_pointer_button_down_on()
                    && let Some(pos) = ctx.input(|i| i.pointer.press_origin())
                {
                    self.touch_hold = Some(TouchHold::Pressing { pos, time: now });
                }
            }
            Some(TouchHold::Pressing { pos, time }) => {
                let moved = pointer.is_some_and(|p| p.distance(pos) > PRESS_MOVE);
                if !down || moved || multi || self.panning || self.selecting_text() {
                    self.touch_hold = None;
                } else if now - time >= LONG_PRESS {
                    self.touch_hold = Some(TouchHold::Opened);
                    self.swallow_press = true;
                    self.active = None;
                    self.lasso = None;
                    self.moving = None;
                    self.text_press = None;
                    self.finish_erase();
                    self.finish_picture_drag();
                    self.finish_transform();
                    self.kinetic.stop();
                    return Some(pos);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(LONG_PRESS - (now - time)));
                }
            }
            Some(TouchHold::Opened) => {
                self.swallow_press = true;
                if !down {
                    self.touch_hold = None;
                }
            }
        }
        None
    }

    pub(super) fn undo(&mut self) {
        if !self.busy() && self.edit.is_none() {
            self.history.undo(&mut self.doc);
            self.ensure_pages();
            self.clear_selection();
        }
    }

    pub(super) fn redo(&mut self) {
        if !self.busy() && self.edit.is_none() {
            self.history.redo(&mut self.doc);
            self.ensure_pages();
            self.clear_selection();
        }
    }

    /// A selection, a picture or a text box is being dragged (moved, or a
    /// picture resized): the view may scroll meanwhile, unlike while writing.
    fn dragging_item(&self) -> bool {
        self.moving.is_some()
            || self.picture_drag.is_some()
            || self.text_press.as_ref().is_some_and(|tp| tp.moved && tp.hit.is_some())
    }

    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // While typing (a text box or a text field such as the note title),
        // Ctrl+Z / Ctrl+Y belong to the text editor.
        // Reading changes nothing, so it has no undo either.
        let typing = self.edit.is_some() || ctx.egui_wants_keyboard_input() || self.reading();
        // Order matters: the Shift variant must be consumed before plain Ctrl+Z.
        let (redo_shift, redo_y, undo, save, export) = ctx.input_mut(|i| {
            (
                !typing && i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
                !typing && i.consume_key(Modifiers::COMMAND, Key::Y),
                !typing && i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::COMMAND, Key::E),
            )
        });
        if redo_shift || redo_y {
            self.redo();
        } else if undo {
            self.undo();
        }
        if save {
            self.save_note(true);
        }
        if export && !self.reading() {
            self.open_export_dialog(ctx, false);
        }
        // While the context menu (or another menu, a dialog) is open, Escape
        // belongs to it (see `escape_for_overlay`).
        if (self.selection.is_some() || self.picture.is_some())
            && self.context_menu.is_none()
            && !self.escape_for_overlay
            && !ctx.egui_wants_keyboard_input()
            && !self.reading()
        {
            let (delete, escape) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            if delete {
                self.delete_selection();
            } else if escape {
                self.clear_selection();
            }
        }
    }

    /// Scrolls to where a link or an outline entry leads, remembering where
    /// the view was for Back.
    pub(super) fn go_to_target(&mut self, target: Target) {
        // The note page that PDF page is on (the first, if several are; none
        // if it was deleted), and its place there.
        let Some((page, placed)) = self.doc.pdf_place_of(target.page) else {
            self.toast("That page of the PDF is no longer in this note");
            return;
        };
        let layout = self.layout();
        // From the PDF page's top (on an infinite page, it is among others).
        let mut y = if layout.canvas { layout.page_top(page) + placed.top() - MARGIN } else { layout.page_stop(page) };
        // A height on the page: shown at the top, with a little room above.
        let size = self.doc.pdf.as_ref().and_then(|pdf| pdf.sizes.get(target.page as usize));
        if let (Some(top), Some([_, h])) = (target.top, size) {
            let share = (1.0 - top / h.max(1.0)).clamp(0.0, 1.0);
            y = layout.page_top(page) + placed.top() + share * placed.height() - MARGIN;
        }
        if layout.canvas {
            // Across the middle of the view if it fits, else from its left.
            let placed = placed.translate(layout.page_origin(page).to_vec2());
            let x = centred_left(placed, self.shown_content.width()).unwrap_or(placed.left() - MARGIN);
            self.scroll_to_x = Some(x);
        }
        if self.nav_back.len() >= NAV_BACK_MAX {
            self.nav_back.remove(0);
        }
        self.nav_back.push(self.kinetic.target().unwrap_or(self.scroll));
        // Clamped where it is eased to (an infinite page grows to it).
        self.scroll_to = Some(y);
    }

    /// Back to where the view was before the last link followed.
    pub(super) fn go_back(&mut self) {
        if let Some(back) = self.nav_back.pop() {
            // Across too: on an infinite page a link may lead to the side.
            self.scroll_to = Some(back.y);
            self.scroll_to_x = Some(back.x);
        }
    }

    /// The PDF link under the screen point `pos`, and its screen rectangle.
    /// Asks for the links of that page the first time.
    fn link_at(&mut self, view: &View, pos: Pos2) -> Option<(Rect, LinkAction)> {
        let file = self.doc.pdf.as_ref()?.file.clone()?;
        let page = view.layout.page_at(view.screen_to_content(pos))?;
        let page_origin = view.page_origin_screen(page);
        let on_screen =
            |r: Rect| Rect::from_min_size(page_origin + r.min.to_vec2() * view.scale, r.size() * view.scale);
        let (pdf_page, placed) =
            self.doc.pdf_places(page).into_iter().map(|(p, r)| (p, on_screen(r))).find(|(_, r)| r.contains(pos))?;
        let (origin, size) = (placed.min, placed.size());
        self.pdf_nav.links(&file, pdf_page).iter().find_map(|l| {
            let [l0, t0, r0, b0] = l.rect;
            let rect = Rect::from_min_max(origin + vec2(l0, t0) * size, origin + vec2(r0, b0) * size);
            rect.contains(pos).then(|| (rect, l.action.clone()))
        })
    }

    /// Follows a link: a page of the PDF, or a web address in the browser.
    fn follow_link(&mut self, ctx: &egui::Context, action: LinkAction) {
        match action {
            LinkAction::GoTo(target) => self.go_to_target(target),
            LinkAction::Uri(uri) => ctx.open_url(egui::OpenUrl::new_tab(uri)),
        }
    }

    /// Writing-area rectangle, zoom scale and whether a horizontal scrollbar
    /// is needed. The vertical scrollbar always occupies the right edge,
    /// except while presenting: then the page has it all (no scroll bars).
    fn canvas_geometry(&self, full: Rect, layout: &Layout) -> (Rect, f32, bool) {
        if self.presenting.is_some() {
            return (full, View::scale_for(self.zoom(), full, layout), false);
        }
        let sb = scrollbar::THICKNESS;
        let w = (full.width() - sb).max(50.0);
        let mut rect = Rect::from_min_size(full.min, vec2(w, full.height()));
        let mut scale = View::scale_for(self.settings.zoom, rect, layout);
        // An infinite page is always wider than the view (it grows to it):
        // not even the frame of a zoom out, before it has, drops the bar.
        let hbar = layout.canvas || layout.content_w() * scale > rect.width() + 0.5;
        if hbar {
            rect = Rect::from_min_size(full.min, vec2(w, (full.height() - sb).max(50.0)));
            scale = View::scale_for(self.settings.zoom, rect, layout);
        }
        (rect, scale, hbar)
    }

    /// Scrolls to `target`: eased with smooth scrolling, at once without.
    fn ease_scroll(&mut self, target: Vec2) {
        if self.settings.smooth_scroll {
            self.kinetic.ease_to(target);
        } else {
            self.kinetic.stop();
            self.scroll = target;
        }
    }

    /// Mouse wheel and touchpad scrolling over the canvas. Wheel notches
    /// glide and accelerate; touchpad swipes come already followed,
    /// accelerated and gliding on, as points (see `input::touchpad`).
    /// Ctrl / Cmd + wheel is zoom and is handled elsewhere. With Shift or
    /// Alt, or over the horizontal scroll bar (`sideways`), the wheel
    /// scrolls across.
    fn wheel_input(&mut self, ui: &egui::Ui, canvas: Rect, scale: f32, now: f64, sideways: bool) {
        let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
        let wheel: Vec<_> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel { unit, delta, modifiers, .. } => Some((*unit, *delta, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (unit, delta, m) in wheel {
            if m.ctrl || m.command || m.mac_cmd {
                continue;
            }
            let notch = is_wheel_notch(unit, delta);
            // egui's line (40 pt) is less than half the step of a notch in
            // other Windows apps: notches get their own.
            let mut d = match unit {
                MouseWheelUnit::Point => delta,
                MouseWheelUnit::Line if notch => delta * WHEEL_NOTCH_PT,
                MouseWheelUnit::Line => delta * line,
                MouseWheelUnit::Page => delta * canvas.height(),
            };
            if m.shift || m.alt || sideways {
                d = vec2(d.x + d.y, 0.0);
            }
            // Positive deltas move the content down/right, i.e. scroll up/left.
            let content = -d * self.settings.scroll_speed / scale;
            if notch && self.settings.smooth_scroll {
                self.kinetic.notch(content, now, self.settings.mouse_accel);
            } else if notch
                || unit == MouseWheelUnit::Page
                || self.settings.smooth_scroll && unit == MouseWheelUnit::Point
            {
                self.kinetic.stop();
                self.scroll += content;
            } else {
                // Without smooth scrolling the touchpad's deltas as reported.
                self.kinetic.stop();
                self.scroll += content * touchpad::GAIN;
            }
        }
    }

    /// The pages of the open note; an infinite page as far as it is written
    /// on and the view goes, every way, growing and shrinking with both (see
    /// `Layout::grown`). Content space starts at its corner: the view moves
    /// with it.
    fn grow_canvas(&mut self, full: Rect) -> Layout {
        let mut layout = Layout::of(&self.doc);
        let centre = std::mem::take(&mut self.canvas_centre);
        if !layout.canvas {
            self.canvas_area = None;
            return layout;
        }
        let shown = full.size() / View::scale_for(self.zoom(), full, &layout);
        let before = self.canvas_area;
        let corner = before.map_or(Vec2::ZERO, |a| a.min.to_vec2());
        // Just opened: what is on it (its PDF) across the middle of the view.
        if centre && let Some(written) = self.doc.canvas_used() {
            let (rect, scale, _) = self.canvas_geometry(full, &layout);
            if let Some(left) = centred_left(written, rect.width() / scale) {
                self.scroll.x = left - corner.x;
            }
        }
        let seen = Rect::from_min_size((corner + self.scroll).to_pos2(), shown);
        layout = layout.grown(self.doc.canvas_used(), seen);
        if let Some(before) = before {
            if self.busy() {
                // Not under a pen, eraser or drag: only larger, from where it starts.
                layout = layout.with_area(Rect::from_min_max(before.min, before.max.max(layout.area().max)));
            } else if self.vbar_grab.is_some() || self.hbar_grab.is_some() || self.minimap.dragging() {
                // A scroll bar or the minimap's box held: only larger, so
                // they stay under the pointer; it shrinks back once let go.
                layout = layout.with_area(before.union(layout.area()));
            }
        }
        let moved = corner - layout.origin;
        if moved != Vec2::ZERO {
            self.shift_view(moved);
        }
        self.canvas_area = Some(layout.area());
        layout
    }

    pub(super) fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        let mut layout = self.grow_canvas(full);
        let grown_at = self.scroll;
        ui.painter().rect_filled(full, 0.0, color::desk());

        // ---- geometry and zoom
        let idle = !self.busy();
        let f = self.zoom_input(ui, full, &layout, idle);
        let (canvas, scale, now) = (f.canvas, f.scale, f.now);
        self.hbar_visible = f.hbar;
        self.view_scale = scale;

        // A finger pressed on a note moves around it, unless the pen, the
        // highlighter or the eraser was picked to use with fingers too, or
        // the select tool is in hand, or the laser is on.
        if idle && ui.input(|i| i.pointer.any_pressed()) {
            self.finger_nav = self.finger(ui.ctx()) && !self.finger_uses_tool();
        }
        // A swipe in from a fullscreen edge (see `edge_swipe`) neither
        // writes nor pans.
        self.swallow_press |= self.edge_touch;

        // Reading also takes taps (to follow links); a drag then starts only
        // past the click threshold, which is why writing senses drags only.
        let sense = if self.navigating() { Sense::click_and_drag() } else { Sense::drag() };
        let canvas_resp = ui.allocate_rect(canvas, sense);

        // A press on the canvas (outside the editor, which sits on top) ends
        // text editing, but not on its resize handles. Off any text box it
        // does only that: it opens no new box.
        if self.edit.is_some() && canvas_resp.drag_started() {
            let v = View::new(canvas, self.scroll, scale, layout);
            let at = ui.input(|i| i.pointer.press_origin()).or(canvas_resp.interact_pointer_pos());
            if at.is_none_or(|p| self.edit_handle_at(&v, p).is_none()) {
                let on_text = at.is_some_and(|p| self.text_at(&layout, v.screen_to_content(p)).is_some());
                self.commit_edit(ui.ctx());
                self.swallow_press |= !on_text;
            }
        }

        // ---- scrolling: wheel / touchpad / keys, with momentum
        let mut max = View::max_scroll(canvas, scale, &layout);
        // Where the scroll may go: past the edges of an infinite page too.
        let bounds = View::scroll_bounds(canvas, scale, &layout);
        // An item being dragged may scroll the view (the wheel, the edges):
        // it stays under the pointer.
        let dragging = self.dragging_item();
        if !idle && !dragging {
            // Never move the page under a stroke being drawn.
            self.kinetic.stop();
        }
        // While dragging, wherever the pointer is (often out of the window).
        if idle && ui.rect_contains_pointer(full) || dragging {
            let sideways = f.hbar && ui.rect_contains_pointer(f.hbar_rect);
            self.wheel_input(ui, canvas, scale, now, sideways);
        }
        if !ui.ctx().egui_wants_keyboard_input() {
            self.scroll_keys(ui, &f, &layout, max);
        }
        if let Some(y) = self.scroll_to.take() {
            let x = self.scroll_to_x.take().unwrap_or(self.scroll.x);
            self.ease_scroll(vec2(x, y));
        }
        // Selecting text (the reader, the pointer tool), before taps and
        // links: a click that drops a selection does nothing else.
        // The release of a long press that opened the menu is no tap.
        let mut click_used = matches!(self.touch_hold, Some(TouchHold::Opened));
        if self.navigating() && !self.busy() && !self.lasering() && !click_used {
            let v = View::new(canvas, self.scroll, scale, layout);
            click_used = self.text_input(ui.ctx(), &canvas_resp, &v, now);
        }
        let link_under_pointer = if self.navigating() && !self.lasering() {
            let v = View::new(canvas, self.scroll, scale, layout);
            self.link_input(ui, &canvas_resp, &v, click_used)
        } else {
            None
        };
        self.turn_pages(&layout, f.visible, bounds);
        self.kinetic.step(&mut self.scroll, f.dt, bounds, scale);
        self.autoscroll(ui, &f, dragging);
        self.scroll = self.scroll.clamp(bounds.min.to_vec2(), bounds.max.to_vec2());

        // A finger held still opens the context menu (further down).
        let held_at = self.touch_hold_input(ui.ctx(), &canvas_resp, now);
        let swallowed = std::mem::take(&mut self.swallow_press);

        // ---- drag on the grey area around the pages = pan (touch / pen friendly)
        let press = ui.input(|i| i.pointer.press_origin());
        self.pan_input(&canvas_resp, &f, &layout, bounds, press, idle && !swallowed);
        // Hovering the grey area keeps the normal arrow.

        // Scrolled past an edge of an infinite page: it grows there now, so
        // the scroll bars and everything below see it.
        if layout.canvas && self.scroll != grown_at {
            layout = self.grow_canvas(full);
            max = View::max_scroll(canvas, scale, &layout);
        }

        // ---- scrollbars (interaction first, so the view below is final)
        let (vstate, hstate) = self.scrollbars_input(ui, &f, &mut layout, &mut max);

        // The minimap beside the vertical bar, which may scroll too.
        let before_map = self.scroll;
        self.minimap(ui.ctx(), canvas, f.vbar_rect, &vstate, &layout, scale, max);
        // Its box pushed past an edge of an infinite page: it grows there.
        if layout.canvas && self.scroll != before_map {
            layout = self.grow_canvas(full);
        }

        if self.vbar_grab.is_some() || self.hbar_grab.is_some() {
            self.kinetic.stop();
        }
        if self.kinetic.is_active() {
            ui.ctx().request_repaint();
        }

        let mut view = View::new(canvas, self.scroll, scale, layout);
        // While zooming, the page is drawn as made at another zoom (see `zoom`).
        let pinching = ui.input(|i| i.multi_touch().is_some());
        view.made = self.zoom_render.update(ui.ctx(), scale, pinching);
        let (left, right) = (view.screen_to_content(canvas.left_top()).x, view.screen_to_content(canvas.right_top()).x);
        self.shown_content = Rect::from_min_max(pos2(left, self.scroll.y), pos2(right, self.scroll.y + f.visible));
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| canvas.contains(*p));
        self.picture_input(ui.ctx(), &view, pointer);

        // ---- pen / mouse input
        let (moves, pressures) = self.pointer_samples(ui, &canvas_resp);
        self.context_menu_input(ui, &view, held_at);
        if !self.panning && !swallowed && !self.navigating() {
            match self.tool {
                // Moving around is handled above.
                Tool::Pointer => {}
                Tool::Eraser => self.handle_erase(&canvas_resp, &view, press, &moves),
                Tool::Select => self.handle_select(&canvas_resp, &view, press, &moves),
                Tool::Text => self.handle_text(&canvas_resp, &view, press, &moves),
                Tool::Pen | Tool::Highlighter => self.handle_ink(&canvas_resp, &view, press, &moves, &pressures),
            }
        }
        self.laser_input(ui.ctx(), &canvas_resp, &view, &moves, now);

        if !ui.input(|i| i.pointer.primary_down()) {
            self.finger_nav = false;
        }

        // ---- painting
        let painter = ui.painter_at(canvas);
        self.paint_pages(ui.ctx(), &painter, &view, now);
        if let Some(rect) = link_under_pointer {
            painter.rect_filled(rect.expand(2.0), 2.0, color::accent_alpha(36));
        }
        self.paint_selection_marks(&painter, &view, canvas_resp.hover_pos());
        self.paint_cursors(ui, &painter, &view, &canvas_resp, now);
        self.text_box_ui(ui, &painter, &view);
        self.add_page_tile(ui, &view);

        // Transient status message (saved / exported / errors).
        if let Some(left) = self.status_at.and_then(|t| super::TOAST_FOR.checked_sub(t.elapsed()))
            && !self.status.is_empty()
        {
            theme::paint_toast(&painter, pos2(canvas.center().x, canvas.bottom() - 90.0), &self.status);
            // Once more when it is gone (it does not fade).
            ui.ctx().request_repaint_after(left);
        }

        // Scrollbars on top; the vertical one shows a page indicator while dragged.
        let center_y = view.screen_to_content(canvas.center()).y;
        let cur = layout.page_index_at_y(center_y);
        self.current_page = cur;
        let label = format!("Page {} / {}", cur + 1, layout.count);
        let full_painter = ui.painter();
        // Presenting: no scroll bar (its track has no width then). An
        // infinite page has no page number to show.
        if self.presenting.is_none() {
            // The minimap, out, shows it itself.
            let label = (!layout.canvas && !self.minimap.out()).then_some(label.as_str());
            scrollbar::paint(full_painter, f.vbar_rect, true, &vstate, label);
        }
        if let Some(h) = &hstate {
            scrollbar::paint(full_painter, f.hbar_rect, false, h, None);
        }
    }

    /// Ctrl + wheel / pinch zoom, anchored under the pointer; zoom from the
    /// menu, anchored at the centre. Returns the frame's geometry at the new
    /// zoom.
    fn zoom_input(&mut self, ui: &egui::Ui, full: Rect, layout: &Layout, idle: bool) -> CanvasFrame {
        let (mut canvas, mut scale, mut hbar) = self.canvas_geometry(full, layout);
        let request = self.zoom_request.take();
        // Not with three or four fingers down (see `system_gestures`).
        if idle && (request.is_some() || ui.rect_contains_pointer(canvas) && !self.edge_touch) {
            let zd = request.unwrap_or_else(|| ui.input(|i| i.zoom_delta()));
            if (zd - 1.0).abs() > 1e-4 {
                let anchor = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| request.is_none() && canvas.contains(*p))
                    .unwrap_or_else(|| canvas.center());
                let before = View::new(canvas, self.scroll, scale, *layout);
                let c = before.screen_to_content(anchor);
                let range = layout.zoom_range();
                let pct = ((scale * zd * 100.0).round() as u32).clamp(*range.start(), *range.end());
                *self.zoom_mut() = ZoomMode::Percent(pct);
                (canvas, scale, hbar) = self.canvas_geometry(full, layout);
                let pad = View::pad_x_for(canvas.width(), layout, scale);
                self.scroll.x = c.x - (anchor.x - canvas.left() - pad) / scale;
                self.scroll.y = c.y - (anchor.y - canvas.top() - TOP_PAD_PX) / scale;
                self.kinetic.stop();
            }
        }
        let (now, dt) = ui.input(|i| (i.time, i.stable_dt.min(1.0 / 30.0)));
        CanvasFrame {
            full,
            canvas,
            scale,
            hbar,
            vbar_rect: Rect::from_min_max(pos2(canvas.right(), full.top()), pos2(full.right(), canvas.bottom())),
            hbar_rect: Rect::from_min_max(pos2(full.left(), canvas.bottom()), pos2(canvas.right(), full.bottom())),
            visible: (canvas.height() - TOP_PAD_PX).max(1.0) / scale,
            now,
            dt,
        }
    }

    /// Page Up / Down, Home / End, and in the reader the arrows, Space and
    /// Back. `max` is the furthest scroll of a paged note.
    fn scroll_keys(&mut self, ui: &egui::Ui, f: &CanvasFrame, layout: &Layout, max: Vec2) {
        let (pgdn, pgup, home, end) = ui.input(|i| {
            (
                i.key_pressed(Key::PageDown),
                i.key_pressed(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
            )
        });
        // Repeated presses continue from where the running ease ends.
        let base = self.kinetic.target().unwrap_or(self.scroll);
        let page_len = f.canvas.height() / f.scale * 0.9;
        if self.navigating() {
            let (next, prev, down, up, back) = ui.input(|i| {
                let space = i.key_pressed(Key::Space);
                let (left, alt) = (i.key_pressed(Key::ArrowLeft), i.modifiers.alt);
                (
                    i.key_pressed(Key::ArrowRight) || space && !i.modifiers.shift,
                    left && !alt || space && i.modifiers.shift,
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowUp),
                    left && alt || i.key_pressed(Key::Backspace),
                )
            });
            if back {
                self.go_back();
            }
            self.page_turn += i32::from(pgdn || next) - i32::from(pgup || prev);
            let line = f.visible * 0.1;
            if down {
                self.ease_scroll(base + vec2(0.0, line));
            }
            if up {
                self.ease_scroll(base - vec2(0.0, line));
            }
        } else {
            if pgdn {
                self.ease_scroll(base + vec2(0.0, page_len));
            }
            if pgup {
                self.ease_scroll(base - vec2(0.0, page_len));
            }
        }
        // An infinite page: the top of what is written on it (across
        // the middle of the view if it fits, else from its left), and
        // its bottom (it reaches further only while the view is there).
        let (first, last) = if layout.canvas {
            let w = self.doc.canvas_used().unwrap_or(Rect::ZERO).translate(-layout.origin);
            let left = centred_left(w, f.canvas.width() / f.scale).unwrap_or(w.left());
            (vec2(left, w.top()), vec2(left, (w.bottom() - f.visible).max(w.top())))
        } else {
            (vec2(base.x, 0.0), vec2(base.x, max.y))
        };
        if home {
            self.ease_scroll(first);
        }
        if end {
            self.ease_scroll(last);
        }
    }

    /// The reader: links, under the pointer, show where they lead and are
    /// followed by a tap (unless the click was `used` already). Returns the
    /// link under the pointer, to paint over the page.
    fn link_input(&mut self, ui: &egui::Ui, resp: &egui::Response, view: &View, used: bool) -> Option<Rect> {
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let link = pointer.and_then(|p| self.link_at(view, p));
        let mut under_pointer = None;
        if let Some((rect, action)) = &link {
            under_pointer = Some(*rect);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if let LinkAction::Uri(uri) = action {
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    Id::unique("pdf_link"),
                    egui::PopupAnchor::Pointer,
                )
                .show(|ui| ui.label(uri));
            }
        }
        if resp.clicked()
            && !used
            && let Some((_, action)) = link
        {
            self.follow_link(ui.ctx(), action);
        }
        under_pointer
    }

    /// Eases to the page (or screen) the pending page turns lead to.
    fn turn_pages(&mut self, layout: &Layout, visible: f32, bounds: Rect) {
        let turn = std::mem::take(&mut self.page_turn);
        if turn != 0 {
            let base = self.kinetic.target().unwrap_or(self.scroll);
            let mut y = base.y;
            for _ in 0..turn.unsigned_abs() {
                y = if turn > 0 { layout.next_stop(y, visible) } else { layout.prev_stop(y, visible) };
                y = y.clamp(bounds.min.y, bounds.max.y);
            }
            self.ease_scroll(vec2(base.x, y));
        }
    }

    /// Dragged near (or past) an edge of the canvas: the view scrolls that
    /// way. Out of the window egui loses the pointer: where it was last.
    fn autoscroll(&mut self, ui: &egui::Ui, f: &CanvasFrame, dragging: bool) {
        self.drag_pointer = if dragging { ui.input(|i| i.pointer.latest_pos()).or(self.drag_pointer) } else { None };
        if let Some(p) = self.drag_pointer {
            let v = autoscroll_velocity(f.canvas, p);
            if v != Vec2::ZERO {
                self.scroll += v * f.dt / f.scale;
                ui.ctx().request_repaint();
            }
        }
    }

    /// A drag on the grey area around the pages (anywhere in the reader)
    /// moves the view, and glides on once let go. A pan begins only when
    /// `may_start`.
    fn pan_input(
        &mut self,
        resp: &egui::Response,
        f: &CanvasFrame,
        layout: &Layout,
        bounds: Rect,
        press: Option<Pos2>,
        may_start: bool,
    ) {
        if may_start && resp.drag_started_by(PointerButton::Primary) {
            let start = press.or(resp.interact_pointer_pos());
            let v = View::new(f.canvas, self.scroll, f.scale, *layout);
            // The reader: everywhere (but not while selecting text); a note:
            // only off the paper, as with the laser.
            self.panning = !self.selecting_text()
                && (self.navigating() && !self.lasering()
                    || start.is_some_and(|p| v.layout.page_at(v.screen_to_content(p)).is_none()));
        }
        if self.panning {
            if resp.dragged() {
                let before = self.scroll;
                self.scroll -= resp.drag_delta() / f.scale;
                self.scroll = self.scroll.clamp(bounds.min.to_vec2(), bounds.max.to_vec2());
                self.kinetic.drag(self.scroll - before, f.now);
                // On Windows "grabbing" has no hand cursor and shows the
                // four-arrow move cross instead: keep the arrow there.
                #[cfg(not(windows))]
                resp.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            } else {
                // Let go: the view keeps gliding with the release velocity.
                self.panning = false;
                if self.settings.smooth_scroll {
                    self.kinetic.release(f.now, f.scale, self.settings.touch_fling);
                }
            }
        }
    }

    /// The scroll bars' interaction. An infinite page grows to follow a
    /// thumb held against an end, updating `layout` and `max`.
    fn scrollbars_input(
        &mut self,
        ui: &mut egui::Ui,
        f: &CanvasFrame,
        layout: &mut Layout,
        max: &mut Vec2,
    ) -> (BarState, Option<BarState>) {
        let vstate = scrollbar::update(
            ui,
            Id::unique("vbar"),
            f.vbar_rect,
            true,
            &mut self.scroll.y,
            max.y,
            f.canvas.height() / f.scale,
            layout.content_h(f.scale),
            &mut self.vbar_grab,
        );
        let hstate = f.hbar.then(|| {
            scrollbar::update(
                ui,
                Id::unique("hbar"),
                f.hbar_rect,
                false,
                &mut self.scroll.x,
                max.x,
                f.canvas.width() / f.scale,
                layout.content_w(),
                &mut self.hbar_grab,
            )
        });
        // A thumb held against an end of an infinite page's bar: the view
        // goes on that way, and the page grows to follow it.
        let push = vec2(bar_push_speed(hstate.as_ref().and_then(|s| s.pushed)), bar_push_speed(vstate.pushed));
        if layout.canvas && push != Vec2::ZERO {
            self.scroll += push * f.dt / f.scale;
            *layout = self.grow_canvas(f.full);
            *max = View::max_scroll(f.canvas, f.scale, layout);
            ui.ctx().request_repaint();
        }
        (vstate, hstate)
    }

    /// The pointer's moves this frame, each with the pressure it was taken
    /// at: a pen sends several per frame, each touch (its force) just before
    /// the move it makes.
    fn pointer_samples(&mut self, ui: &egui::Ui, resp: &egui::Response) -> (Vec<Pos2>, Vec<f32>) {
        let samples = std::mem::take(&mut self.pen_input.samples);
        let (mut moves, pressures) = ui.input(|i| moves_with_pressure(&i.events, self.pen_input.pressure, &samples));
        self.update_pressure(ui.ctx());
        if moves.is_empty() {
            // Out of the window (an item dragged there), where the pointer last was.
            moves.extend(resp.interact_pointer_pos().or(self.drag_pointer));
        }
        (moves, pressures)
    }

    /// A right click (or a finger's long press, `held_at`) opens the context
    /// menu for what is under the pointer.
    fn context_menu_input(&mut self, ui: &egui::Ui, view: &View, held_at: Option<Pos2>) {
        let right_click = ui.input(|i| i.pointer.secondary_clicked());
        let click_at = (right_click && ui.rect_contains_pointer(view.rect))
            .then(|| ui.input(|i| i.pointer.interact_pos()))
            .flatten();
        if let Some(pos) = click_at.or(held_at)
            && !self.busy()
            && self.saved_tool.is_none()
        {
            let c = view.screen_to_content(pos);
            let page = view.layout.page_at(c);
            let on_selection = match (&self.selection, page) {
                (Some(sel), Some(p)) if sel.page == p => {
                    let o = view.layout.page_origin(p);
                    sel.bounds.expand(6.0).contains(pos2(c.x - o.x, c.y - o.y))
                }
                _ => false,
            };
            let at = page.map(|p| (p, c - view.layout.page_origin(p).to_vec2()));
            let picture = at.and_then(|(p, local)| self.picture_at(p, local));
            let text = self.text_at(&view.layout, c);
            self.context_menu = Some(ContextMenu { pos, page, text, on_selection, picture, at });
            ui.ctx().request_repaint();
        }
    }

    /// The pages and what is on them. Bottom to top: paper, the PDF's pages,
    /// rules, pictures, highlighters, text, pen (see `render`), then selected
    /// text and search matches.
    fn paint_pages(&mut self, ctx: &egui::Context, painter: &egui::Painter, view: &View, now: f64) {
        let (layout, scale, made) = (view.layout, view.scale, view.made);
        self.picture_textures.begin_frame(ctx, now, self.scroll, scale);
        render::paint_paper(painter, view);
        // An infinite page with a PDF: its rules under the PDF's pages, which
        // cover them.
        let rules_under = layout.canvas && self.doc.pdf.is_some();
        if rules_under {
            render::paint_rules(painter, view, &self.doc);
        }
        // A note written on a PDF: its pages, right on the paper.
        for page in view.visible_pages() {
            self.pdf_textures.paint(painter, &self.doc, page, view.page_screen_rect(page), true, made / scale);
            // The reader: the links of the PDF pages shown, ready before they are pointed at.
            if self.navigating()
                && let Some(file) = self.doc.pdf.as_ref().and_then(|pdf| pdf.file.clone())
            {
                let page_rect = view.page_screen_rect(page);
                let k = page_rect.width() / self.doc.page_width;
                for (pdf_page, r) in self.doc.pdf_places(page) {
                    let shown = Rect::from_min_size(page_rect.min + r.min.to_vec2() * k, r.size() * k);
                    if shown.intersects(view.rect) {
                        self.pdf_nav.links(&file, pdf_page);
                    }
                }
            }
        }
        let ppp = ctx.pixels_per_point();
        let move_off = self.moving.as_ref().map_or(Vec2::ZERO, |m| m.delta);
        let moved = self.selection.as_ref().map(|s| Moved { ids: &s.ids, page: s.page, offset: move_off });
        // The text boxes and pictures of a selection being dragged.
        let (mut text_moves, mut picture_moves) = self.selection_offsets();
        // The stroke being drawn, in its own layer.
        let predict = self.settings.predict_pen;
        let mut live = self.active.as_mut();
        let mut paint_live = |kind: StrokeKind| {
            if let Some(a) = live.as_deref_mut().filter(|a| a.kind == kind) {
                let tip = a.builder.tip(predict);
                let points = (a.builder.points(), &tip[..]);
                a.live.paint(painter, view, a.page, a.kind, a.color, a.width, points, ppp);
            }
        };
        if !rules_under {
            render::paint_rules(painter, view, &self.doc);
        }
        // Pictures, on the paper and under the ink; the ones dragged (alone or
        // selected) where they are now.
        picture_moves.extend(self.picture_drag.as_ref().map(|d| d.now.clone()));
        for page in view.visible_pages() {
            let origin = view.page_origin_screen(page);
            let scales = (scale, made);
            self.picture_textures.paint_page(painter, &self.doc.images, page, origin, scales, &picture_moves);
        }
        // The pictures of the pages next to those shown, ready to scroll to;
        // on an infinite page those within a screen of the view.
        let shown = view.visible_pages();
        let near = shown.start().saturating_sub(1)..=(shown.end() + 1).min(layout.count - 1);
        let around = self.shown_content.expand2(self.shown_content.size()).translate(-layout.page_origin(0).to_vec2());
        let images = self
            .doc
            .images
            .iter()
            .filter(|i| near.contains(&i.page) && (!layout.canvas || i.rect().intersects(around)));
        self.picture_textures.prefetch(ctx, images, made);
        // Those of a selection carried far (the view scrolled while dragging),
        // their page out of view: drawn where they are now all the same.
        for img in picture_moves.iter().filter(|i| !view.visible_pages().contains(&i.page)) {
            let origin = view.page_origin_screen(img.page);
            self.picture_textures.paint_page(painter, std::slice::from_ref(img), img.page, origin, (scale, made), &[]);
        }
        let strokes = &self.doc.strokes;
        render::paint_strokes(painter, view, Layer::Highlight, strokes, &mut self.cache, ppp, moved.as_ref());
        paint_live(StrokeKind::Highlighter);
        // A text box dragged with the text tool.
        text_moves.extend(
            self.text_press
                .as_ref()
                .and_then(|tp| if tp.moved { tp.hit.map(|id| (id, tp.now - tp.start)) } else { None }),
        );
        render::paint_texts(
            painter,
            view,
            &self.doc.texts,
            self.edit.as_ref().map(|e| e.id),
            &text_moves,
            self.tool == Tool::Text,
        );

        render::paint_strokes(painter, view, Layer::Ink, strokes, &mut self.cache, ppp, moved.as_ref());
        paint_live(StrokeKind::Pen);
        // Selected text and search matches, over the pages.
        self.paint_text_marks(painter, view);
    }

    /// The selection box (and, with the select tool, its handles) and the
    /// lasso outline. `hover` is the pointer over the canvas.
    fn paint_selection_marks(&self, painter: &egui::Painter, view: &View, hover: Option<Pos2>) {
        let blue = color::accent();
        if let Some(sel) = &self.selection {
            let move_off = self.moving.as_ref().map_or(Vec2::ZERO, |m| m.delta);
            let r = sel.bounds.translate(view.layout.page_origin(sel.page).to_vec2() + move_off);
            let rect = Rect::from_min_max(view.content_to_screen(r.min), view.content_to_screen(r.max));
            painter.rect_filled(rect, 2.0, color::accent_alpha(28));
            let path = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
            painter.extend(egui::Shape::dashed_line(&path, egui::Stroke::new(1.5, blue), 6.0, 4.0));
        }
        if self.tool == Tool::Select {
            self.paint_picked_picture(painter, view, hover);
            self.paint_selection_handles(painter, view, hover);
        }
        if let Some(l) = &self.lasso {
            let o = view.layout.page_origin(l.page).to_vec2();
            let mut pts: Vec<Pos2> = l.outline().iter().map(|p| view.content_to_screen(*p + o)).collect();
            if l.rect && pts.len() == 4 {
                // The box, closed and faintly filled, like the selection's.
                painter.rect_filled(Rect::from_two_pos(pts[0], pts[2]), 0.0, color::accent_alpha(18));
                pts.push(pts[0]);
            }
            if pts.len() >= 2 {
                painter.extend(egui::Shape::dashed_line(&pts, egui::Stroke::new(1.5, blue), 5.0, 4.0));
            }
        }
    }

    /// The tool cursor, the same for pen and mouse: over a page, pen,
    /// highlighter and eraser replace the system pointer with their tip
    /// (a dot like the stroke they would draw, OneNote style, or the
    /// eraser's circle). Pointer, select and text keep the system pointer.
    /// Then the laser's dot and trail.
    fn paint_cursors(&self, ui: &egui::Ui, painter: &egui::Painter, view: &View, resp: &egui::Response, now: f64) {
        let ctx = ui.ctx();
        let ppp = ctx.pixels_per_point();
        // A hovering pen is not reported by winit as a pointer, so it is read
        // from the native source. Not where something else (a toolbar, a
        // panel) is on top of the canvas.
        let pen_hover = self
            .pen_input
            .windows
            .as_ref()
            .and_then(|p| p.hover())
            .map(|(x, y)| pos2(x / ppp, y / ppp))
            .or(self.pen_input.hover)
            .filter(|p| view.rect.contains(*p) && ctx.layer_id_at(*p) == Some(ui.layer_id()));
        let tip_pos = pen_hover
            .or(resp.hover_pos())
            .or(resp.interact_pointer_pos())
            .filter(|p| view.layout.page_at(view.screen_to_content(*p)).is_some());
        let grabbing = self.panning || self.vbar_grab.is_some() || self.hbar_grab.is_some();
        if let (Some(pos), false, false) = (tip_pos, self.navigating(), grabbing) {
            if self.tool == Tool::Eraser {
                hide_pointer(ctx);
                render::paint_eraser_cursor(painter, pos, self.settings.eraser_radius * view.scale);
            } else if let Some((kind, rgba, width)) = self.stroke_params() {
                hide_pointer(ctx);
                // While drawing, the stroke's own end is the tip.
                if self.active.is_none() {
                    render::paint_pen_cursor(painter, pos, kind, rgba, width * view.scale);
                }
            }
        }
        // The laser: over a page its dot in place of the system pointer
        // (off the paper the usual arrow), and its trail.
        let laser_dot = pen_hover
            .or(resp.hover_pos())
            .or(resp.interact_pointer_pos())
            .filter(|p| self.lasering() && !grabbing && view.layout.page_at(view.screen_to_content(*p)).is_some());
        if laser_dot.is_some() {
            hide_pointer(ctx);
        }
        self.paint_laser(painter, view, laser_dot, now);
    }

    /// The rubber band of a text box being dragged out, and the in-place
    /// editor of the text box being typed into (a real egui TextEdit:
    /// cursor, selection, clipboard, IME, its own Ctrl+Z).
    fn text_box_ui(&mut self, ui: &mut egui::Ui, painter: &egui::Painter, view: &View) {
        let blue = color::accent();
        if let Some(tp) = &self.text_press
            && tp.hit.is_none()
            && tp.handle.is_none()
            && tp.moved
            && tp.page.is_some()
        {
            let r = Rect::from_two_pos(view.content_to_screen(tp.start), view.content_to_screen(tp.now));
            let path = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.extend(egui::Shape::dashed_line(&path, egui::Stroke::new(1.5, blue), 6.0, 4.0));
        }

        let mut commit_now = false;
        if let Some(e) = self.edit.as_mut() {
            // Toolbar size / colour changes apply live.
            e.size = self.settings.text.width;
            e.color = self.settings.text.color;
        }
        if let (Some(frame), Some(e)) = (self.edit_frame(view), self.edit.as_mut()) {
            painter.rect_filled(frame, 3.0, color::edit_backdrop());
            painter.rect_stroke(frame, 3.0, egui::Stroke::new(1.5, blue), egui::StrokeKind::Middle);
            // The resize handles, one on either side.
            for x in [frame.left(), frame.right()] {
                let size = vec2(6.0, (frame.height() - 8.0).clamp(8.0, 22.0));
                let handle = Rect::from_center_size(pos2(x, frame.center().y), size);
                painter.rect(handle, 3.0, color::handle(), egui::Stroke::new(1.5, blue), egui::StrokeKind::Middle);
            }
            // The move knob above it, like the selection's rotate knob.
            let knob = super::text::move_knob(frame);
            painter.line_segment([frame.center_top(), knob], egui::Stroke::new(1.5, blue));
            painter.circle(knob, 6.0, color::handle(), egui::Stroke::new(1.5, blue));

            // The editor: the frame without its room for the outline, nor its
            // margin on the sides, where the side handles take the presses
            // (it would get them first). The text stays where it was.
            let side_margin = f32::from(EDIT_MARGIN.left);
            let editor = frame.shrink2(vec2(2.0 + side_margin, 2.0));
            let margin = egui::Margin { left: 0, right: 0, ..EDIT_MARGIN };

            let id = Id::unique(("text_edit", e.id));
            let color = Color32::from_rgb(e.color[0], e.color[1], e.color[2]);
            let size = (e.size * view.scale).max(6.0);
            // The Markdown in the styles it makes, its marks dimmed.
            super::text::markdown_keys(ui.ctx(), id, &mut e.buf);
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                let mut job = markdown::highlight(buf.as_str(), size, color);
                job.wrap.max_width = wrap;
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let widget = egui::TextEdit::multiline(&mut e.buf)
                .id(id)
                .font(FontId::proportional(size))
                .text_color(color)
                .layouter(&mut layouter)
                .desired_width(editor.width())
                .desired_rows(1)
                // No background; the text wraps at the box's width.
                .margin(margin)
                .frame(egui::Frame::NONE.inner_margin(margin))
                .lock_focus(true);
            let max_rect = Rect::from_min_size(editor.min, vec2(editor.width(), 10.0));
            let resp = ui.scope_builder(egui::UiBuilder::new().max_rect(max_rect), |ui| ui.add(widget)).inner;
            e.last_h = resp.rect.height();

            if e.focus_pending {
                resp.request_focus();
                e.focus_pending = false;
            } else if !resp.has_focus() {
                // Focus left the editor. A press on a toolbar button only steals it
                // temporarily (take it back); another text field or Esc ends editing.
                let other = ui.ctx().memory(|m| m.focused()).is_some_and(|f| f != id);
                let pressed = ui.input(|i| i.pointer.any_pressed());
                if other || !pressed {
                    commit_now = true;
                } else {
                    resp.request_focus();
                }
            }
        }
        if commit_now {
            self.commit_edit(ui.ctx());
        }
    }

    /// "Add page" below the last page, the same dashed tile as in the
    /// pages panel, as wide as the page.
    fn add_page_tile(&mut self, ui: &mut egui::Ui, view: &View) {
        let (canvas, layout) = (view.rect, view.layout);
        let last = view.page_screen_rect(layout.count - 1);
        let btn = Rect::from_min_size(pos2(last.left(), last.bottom() + 18.0), vec2(last.width(), 40.0));
        if btn.intersects(canvas) && !self.reading() && !layout.canvas {
            let mut tile = ui.new_child(egui::UiBuilder::new().max_rect(canvas));
            tile.set_clip_rect(canvas);
            if theme::add_tile(&mut tile, btn, Id::unique("add_page"), "Add page").clicked() {
                self.insert_page(self.doc.page_count);
            }
        }
    }
}

/// The pointer's moves among `events`, each with the pressure it was taken
/// at, `last` until one is known: a touch's force comes just before the
/// move it makes; a Wayland pen's moves are its `samples` (where each went,
/// in order, and its pressure), matched to them as they come.
fn moves_with_pressure(events: &[Event], last: f32, samples: &[(Pos2, f32)]) -> (Vec<Pos2>, Vec<f32>) {
    let mut force = last;
    let mut samples = samples.iter().peekable();
    events
        .iter()
        .filter_map(|e| match e {
            Event::Touch { phase: TouchPhase::Start | TouchPhase::Move, force: Some(f), .. } if *f > 0.0 => {
                force = f.clamp(0.0, 1.0);
                None
            }
            Event::PointerMoved(p) => {
                if let Some(&&(at, pressure)) = samples.peek()
                    && at == *p
                {
                    force = pressure;
                    samples.next();
                }
                Some((*p, force))
            }
            _ => None,
        })
        .unzip()
}

/// The canvas's geometry and clock for one frame, at the zoom it settled on:
/// what the phases of [`InkApp::canvas_ui`] share.
struct CanvasFrame {
    /// The whole area: the canvas and its scroll bars.
    full: Rect,
    /// Where the pages show (the area without its scroll bars).
    canvas: Rect,
    scale: f32,
    /// Whether the horizontal scroll bar shows.
    hbar: bool,
    vbar_rect: Rect,
    hbar_rect: Rect,
    /// Content height shown below the top pad.
    visible: f32,
    now: f64,
    /// Seconds since the last frame (at most a 30 fps frame).
    dt: f32,
}

/// Hides the system pointer this frame (the tool paints its own cursor).
///
/// With a transparent cursor image, not a hidden cursor: winit hides it with
/// Windows' cursor display count and only updates that on a change of its
/// own state, which goes out of step when Windows moves or resizes the
/// window (the pointer then stayed visible next to the pen's dot). An image
/// is set again on every cursor update. `InkApp::ui` clears it each frame.
fn hide_pointer(ctx: &egui::Context) {
    static TRANSPARENT: OnceLock<egui::CustomCursorImage> = OnceLock::new();
    let image = TRANSPARENT.get_or_init(|| egui::CustomCursorImage {
        rgba: Arc::from(vec![0u8; 4 * 4 * 4]),
        size: [4, 4],
        hotspot: [0, 0],
    });
    // Integrations without bitmap cursors fall back to this.
    ctx.set_cursor_icon(egui::CursorIcon::None);
    ctx.set_cursor_image(Some(image.clone()));
}

/// The view's speed (points per second) while an item is dragged with the
/// pointer at `pointer`: towards an edge of `canvas` the pointer is near or
/// past, faster the closer (see `AUTOSCROLL_EDGE`).
fn autoscroll_velocity(canvas: Rect, pointer: Pos2) -> Vec2 {
    // 0 out of reach, 1 at the edge and beyond; eased in.
    let depth = |d: f32| ((AUTOSCROLL_EDGE - d) / AUTOSCROLL_EDGE).clamp(0.0, 1.0).powi(2);
    let axis = |p: f32, lo: f32, hi: f32| AUTOSCROLL_SPEED * (depth(hi - p) - depth(p - lo));
    vec2(axis(pointer.x, canvas.left(), canvas.right()), axis(pointer.y, canvas.top(), canvas.bottom()))
}

/// The view's speed (points per second, along the bar) while a scroll bar's
/// thumb is held against an end of its track (`BarState::pushed`): slow at
/// the end, faster as the pointer goes on past it (see `AUTOSCROLL_EDGE`).
pub(super) fn bar_push_speed(pushed: Option<(bool, f32)>) -> f32 {
    let Some((forward, past)) = pushed else { return 0.0 };
    let depth = (past / AUTOSCROLL_EDGE).clamp(0.0, 1.0).powi(2);
    let speed = AUTOSCROLL_SPEED * (BAR_PUSH_MIN + (1.0 - BAR_PUSH_MIN) * depth);
    if forward { speed } else { -speed }
}

/// On an infinite page, the left of a view `width` wide that shows
/// `written` across its middle; `None` if it is wider than the view.
pub(super) fn centred_left(written: Rect, width: f32) -> Option<f32> {
    (written.width() <= width).then(|| written.center().x - width * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pen_sample_keeps_its_own_pressure() {
        let moved = |x: f32| Event::PointerMoved(pos2(x, 10.0));
        // A Wayland pen: three samples in one frame, each with its pressure.
        let events = [moved(1.0), moved(2.0), moved(3.0)];
        let samples = [(pos2(1.0, 10.0), 0.2), (pos2(2.0, 10.0), 0.5), (pos2(3.0, 10.0), 0.9)];
        let (moves, pressures) = moves_with_pressure(&events, 0.9, &samples);
        assert_eq!(moves.len(), 3);
        assert_eq!(pressures, [0.2, 0.5, 0.9]);
        // A move that is not the pen's (the mouse) keeps the last pressure.
        let events = [moved(1.0), moved(7.0), moved(2.0)];
        let (_, pressures) = moves_with_pressure(&events, 1.0, &samples[..2]);
        assert_eq!(pressures, [0.2, 0.2, 0.5]);
        // No pen samples: the pressure known so far.
        assert_eq!(moves_with_pressure(&[moved(4.0)], 0.7, &[]).1, [0.7]);
    }

    #[test]
    fn what_fits_across_the_view_is_shown_in_its_middle() {
        // A PDF 48 from the corner, 600 wide, in a view 1000 wide: 200 of
        // room each side, the view starting left of the corner.
        let pdf = Rect::from_min_size(pos2(48.0, 48.0), vec2(600.0, 800.0));
        assert_eq!(centred_left(pdf, 1000.0), Some(-152.0));
        assert_eq!(centred_left(pdf, 500.0), None, "wider than the view: from its left");
    }

    #[test]
    fn a_thumb_pushed_against_an_end_scrolls_that_way() {
        assert_eq!(bar_push_speed(None), 0.0);
        let at_end = bar_push_speed(Some((true, 0.0)));
        assert!(at_end > 0.0, "held at the end: it goes on");
        assert!(bar_push_speed(Some((true, AUTOSCROLL_EDGE))) > at_end, "faster further past");
        assert_eq!(bar_push_speed(Some((false, 10.0))), -bar_push_speed(Some((true, 10.0))));
    }

    #[test]
    fn dragging_near_an_edge_scrolls_that_way() {
        let canvas = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        // In the middle: still.
        assert_eq!(autoscroll_velocity(canvas, pos2(400.0, 300.0)), Vec2::ZERO);
        // Near the bottom: down, slower than at the edge; past it: full speed.
        let near = autoscroll_velocity(canvas, pos2(400.0, 590.0));
        assert!(near.x == 0.0 && near.y > 0.0 && near.y < AUTOSCROLL_SPEED);
        assert_eq!(autoscroll_velocity(canvas, pos2(400.0, 700.0)), vec2(0.0, AUTOSCROLL_SPEED));
        // In the top-left corner: up and left.
        let corner = autoscroll_velocity(canvas, pos2(5.0, 5.0));
        assert!(corner.x < 0.0 && corner.y < 0.0);
    }
}
