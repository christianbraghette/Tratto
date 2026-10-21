// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The editor's canvas: input routing to the tools, scrolling and zoom,
//! painting the pages, undo / redo and the editor shortcuts, and following
//! the links and outline of a PDF.

use std::sync::{Arc, OnceLock};

use eframe::egui::{
    self, Color32, Event, FontId, Id, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Vec2, pos2,
    vec2,
};

use tratto_core::model::StrokeKind;
use tratto_core::pdf_pages::{LinkAction, Target};

use crate::markdown;
use crate::render::{self, Layer, Moved};
use crate::scrollbar;
use crate::settings::ZoomMode;
use crate::theme::{self, color};
use crate::toolbar::Tool;
use crate::view::{Layout, MARGIN, TOP_PAD_PX, View};

use super::InkApp;
use super::menus::ContextMenu;
use super::text::EDIT_MARGIN;

/// Places kept for Back (the oldest is forgotten).
const NAV_BACK_MAX: usize = 50;
/// A dragged item this close to an edge of the canvas (points) scrolls the
/// view...
const AUTOSCROLL_EDGE: f32 = 48.0;
/// ...up to this fast, at the edge and past it (points per second).
const AUTOSCROLL_SPEED: f32 = 1400.0;

impl InkApp {
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
            self.open_export_dialog();
        }
        // While the context menu is open, Escape belongs to it.
        if (self.selection.is_some() || self.picture.is_some())
            && self.context_menu.is_none()
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

    /// The note page written on page `pdf_page` of the PDF (the first, if
    /// several are; none if it was deleted).
    fn note_page_of(&self, pdf_page: u32) -> Option<u32> {
        let pages = &self.doc.pdf.as_ref()?.pages;
        pages.iter().position(|p| *p == Some(pdf_page)).map(|i| i as u32)
    }

    /// Scrolls to where a link or an outline entry leads, remembering where
    /// the view was for Back.
    pub(super) fn go_to_target(&mut self, target: Target) {
        let Some(page) = self.note_page_of(target.page) else {
            self.toast("That page of the PDF is no longer in this note");
            return;
        };
        let layout = self.layout();
        let mut y = layout.page_stop(page);
        // A height on the page: shown at the top, with a little room above.
        if let Some(pdf) = &self.doc.pdf {
            let placed = pdf.placement(target.page, layout.page_w, layout.page_h);
            let size = pdf.sizes.get(target.page as usize);
            if let (Some(top), Some([_, py, _, ph]), Some([_, h])) = (target.top, placed, size) {
                let share = (1.0 - top / h.max(1.0)).clamp(0.0, 1.0);
                y = layout.page_top(page) + py + share * ph - MARGIN;
            }
        }
        if self.nav_back.len() >= NAV_BACK_MAX {
            self.nav_back.remove(0);
        }
        self.nav_back.push(self.kinetic.target().unwrap_or(self.scroll));
        self.scroll_to = Some(y.max(0.0));
    }

    /// Back to where the view was before the last link followed.
    pub(super) fn go_back(&mut self) {
        if let Some(back) = self.nav_back.pop() {
            self.scroll_to = Some(back.y);
        }
    }

    /// The PDF link under the screen point `pos`, and its screen rectangle.
    /// Asks for the links of that page the first time.
    fn link_at(&mut self, view: &View, pos: Pos2) -> Option<(Rect, LinkAction)> {
        let pdf = self.doc.pdf.as_ref()?;
        let file = pdf.file.as_ref()?;
        let layout = view.layout;
        let page = layout.page_at(view.screen_to_content(pos))?;
        let pdf_page = (*pdf.pages.get(page as usize)?)?;
        let [x, y, w, h] = pdf.placement(pdf_page, layout.page_w, layout.page_h)?;
        let origin = view.page_origin_screen(page) + vec2(x, y) * view.scale;
        let size = vec2(w, h) * view.scale;
        self.pdf_nav.links(file, pdf_page).iter().find_map(|l| {
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
    /// is needed. The vertical scrollbar always occupies the right edge.
    fn canvas_geometry(&self, full: Rect, layout: &Layout) -> (Rect, f32, bool) {
        let sb = scrollbar::THICKNESS;
        let w = (full.width() - sb).max(50.0);
        let mut rect = Rect::from_min_size(full.min, vec2(w, full.height()));
        let mut scale = View::scale_for(self.settings.zoom, rect, layout);
        let hbar = layout.content_w() * scale > rect.width() + 0.5;
        if hbar {
            rect = Rect::from_min_size(full.min, vec2(w, (full.height() - sb).max(50.0)));
            scale = View::scale_for(self.settings.zoom, rect, layout);
        }
        (rect, scale, hbar)
    }

    /// Mouse wheel and touchpad scrolling over the canvas. Wheel notches
    /// glide and accelerate; precise touchpad deltas are applied directly.
    /// Ctrl / Cmd + wheel is zoom and is handled elsewhere.
    /// Scrolls to `target`: eased with smooth scrolling, at once without.
    fn ease_scroll(&mut self, target: Vec2) {
        if self.settings.smooth_scroll {
            self.kinetic.ease_to(target);
        } else {
            self.kinetic.stop();
            self.scroll = target;
        }
    }

    fn wheel_input(&mut self, ui: &egui::Ui, canvas: Rect, scale: f32, now: f64) {
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
            // Same conversion as egui's own scroll delta.
            let mut d = match unit {
                MouseWheelUnit::Point => delta,
                MouseWheelUnit::Line => delta * line,
                MouseWheelUnit::Page => delta * canvas.height(),
            };
            if m.shift {
                d = vec2(d.x + d.y, 0.0);
            }
            // Positive deltas move the content down/right, i.e. scroll up/left.
            let content = -d * self.settings.scroll_speed / scale;
            if unit == MouseWheelUnit::Point && d.length() < 8.0 || !self.settings.smooth_scroll {
                self.kinetic.stop();
                self.scroll += content;
            } else {
                let accel =
                    if is_wheel_notch(unit, delta) { self.settings.mouse_accel } else { self.settings.touchpad_accel };
                self.kinetic.notch(content, now, accel);
            }
        }
    }

    pub(super) fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        let layout =
            Layout { page_w: self.doc.page_width, page_h: self.doc.page_height, count: self.doc.page_count.max(1) };
        ui.painter().rect_filled(full, 0.0, render::PADDING_COLOR);

        // ---- geometry and zoom
        let (mut canvas, mut scale, mut hbar) = self.canvas_geometry(full, &layout);
        let idle = !self.busy();

        // Ctrl + wheel / pinch zoom, anchored under the pointer; zoom from the
        // menu, anchored at the centre.
        let request = self.zoom_request.take();
        if idle && (request.is_some() || ui.rect_contains_pointer(canvas)) {
            let zd = request.unwrap_or_else(|| ui.input(|i| i.zoom_delta()));
            if (zd - 1.0).abs() > 1e-4 {
                let anchor = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| request.is_none() && canvas.contains(*p))
                    .unwrap_or_else(|| canvas.center());
                let before = View::new(canvas, self.scroll, scale, layout);
                let c = before.screen_to_content(anchor);
                let pct = (scale * zd * 100.0).round().clamp(25.0, 400.0) as u32;
                self.settings.zoom = ZoomMode::Percent(pct);
                (canvas, scale, hbar) = self.canvas_geometry(full, &layout);
                let pad = View::pad_x_for(canvas.width(), layout.content_w(), scale);
                self.scroll.x = c.x - (anchor.x - canvas.left() - pad) / scale;
                self.scroll.y = c.y - (anchor.y - canvas.top() - TOP_PAD_PX) / scale;
                self.kinetic.stop();
            }
        }
        self.hbar_visible = hbar;
        self.view_scale = scale;

        // A finger pressed on a note moves around it, unless the pen was
        // picked to write with fingers too.
        if idle && ui.input(|i| i.pointer.any_pressed()) {
            self.finger_nav = self.finger(ui.ctx()) && !(self.tool == Tool::Pen && self.finger_ink);
        }

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
        let (now, dt) = ui.input(|i| (i.time, i.stable_dt.min(1.0 / 30.0)));
        let max = View::max_scroll(canvas, scale, &layout);
        // An item being dragged may scroll the view (the wheel, the edges):
        // it stays under the pointer.
        let dragging = self.dragging_item();
        if !idle && !dragging {
            // Never move the page under a stroke being drawn.
            self.kinetic.stop();
        }
        // While dragging, wherever the pointer is (often out of the window).
        if idle && ui.rect_contains_pointer(full) || dragging {
            self.wheel_input(ui, canvas, scale, now);
        }
        // Content height shown below the top pad.
        let visible = (canvas.height() - TOP_PAD_PX).max(1.0) / scale;
        if !ui.ctx().egui_wants_keyboard_input() {
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
            let page_len = canvas.height() / scale * 0.9;
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
                let line = visible * 0.1;
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
            if home {
                self.ease_scroll(vec2(base.x, 0.0));
            }
            if end {
                self.ease_scroll(vec2(base.x, max.y));
            }
        }
        if let Some(y) = self.scroll_to.take() {
            self.ease_scroll(vec2(self.scroll.x, y));
        }
        // Selecting text (the reader, the pointer tool), before taps and
        // links: a click that drops a selection does nothing else.
        let mut click_used = false;
        if self.navigating() && !self.busy() {
            let v = View::new(canvas, self.scroll, scale, layout);
            click_used = self.text_input(ui.ctx(), &canvas_resp, &v, now);
        }
        // The reader: links, under the pointer, show where they lead
        // and are followed by a tap.
        let mut link_under_pointer = None;
        if self.navigating() {
            let v = View::new(canvas, self.scroll, scale, layout);
            let pointer = canvas_resp.interact_pointer_pos().or(canvas_resp.hover_pos());
            let link = pointer.and_then(|p| self.link_at(&v, p));
            if let Some((rect, action)) = &link {
                // Painted over the page, further down.
                link_under_pointer = Some(*rect);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if let LinkAction::Uri(uri) = action {
                    egui::Tooltip::always_open(
                        ui.ctx().clone(),
                        ui.layer_id(),
                        Id::new("pdf_link"),
                        egui::PopupAnchor::Pointer,
                    )
                    .show(|ui| ui.label(uri));
                }
            }
            if canvas_resp.clicked()
                && !click_used
                && let Some((_, action)) = link
            {
                self.follow_link(ui.ctx(), action);
            }
        }
        let turn = std::mem::take(&mut self.page_turn);
        if turn != 0 {
            let base = self.kinetic.target().unwrap_or(self.scroll);
            let mut y = base.y;
            for _ in 0..turn.unsigned_abs() {
                y = if turn > 0 { layout.next_stop(y, visible) } else { layout.prev_stop(y, visible) };
                y = y.clamp(0.0, max.y);
            }
            self.ease_scroll(vec2(base.x, y));
        }
        self.kinetic.step(&mut self.scroll, dt, max, scale);
        // Dragged near (or past) an edge of the canvas: the view scrolls that
        // way. Out of the window egui loses the pointer: where it was last.
        self.drag_pointer = if dragging { ui.input(|i| i.pointer.latest_pos()).or(self.drag_pointer) } else { None };
        if let Some(p) = self.drag_pointer {
            let v = autoscroll_velocity(canvas, p);
            if v != Vec2::ZERO {
                self.scroll += v * dt / scale;
                ui.ctx().request_repaint();
            }
        }
        self.scroll = vec2(self.scroll.x.clamp(0.0, max.x), self.scroll.y.clamp(0.0, max.y));

        let swallowed = std::mem::take(&mut self.swallow_press);

        // ---- drag on the grey area around the pages = pan (touch / pen friendly)
        let press = ui.input(|i| i.pointer.press_origin());
        if idle && !swallowed && canvas_resp.drag_started_by(PointerButton::Primary) {
            let start = press.or(canvas_resp.interact_pointer_pos());
            let v = View::new(canvas, self.scroll, scale, layout);
            // The reader: everywhere (but not while selecting text); a note:
            // only off the paper.
            self.panning = !self.selecting_text()
                && (self.navigating() || start.is_some_and(|p| v.layout.page_at(v.screen_to_content(p)).is_none()));
        }
        if self.panning {
            if canvas_resp.dragged() {
                let before = self.scroll;
                self.scroll -= canvas_resp.drag_delta() / scale;
                self.scroll = vec2(self.scroll.x.clamp(0.0, max.x), self.scroll.y.clamp(0.0, max.y));
                self.kinetic.drag(self.scroll - before, now);
                // On Windows "grabbing" has no hand cursor and shows the
                // four-arrow move cross instead: keep the arrow there.
                #[cfg(not(windows))]
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else {
                // Let go: the view keeps gliding with the release velocity.
                self.panning = false;
                if self.settings.smooth_scroll {
                    self.kinetic.release(now, scale, self.settings.touch_fling);
                }
            }
        }
        // Hovering the grey area keeps the normal arrow.

        // ---- scrollbars (interaction first, so the view below is final)
        let vbar_rect = Rect::from_min_max(pos2(canvas.right(), full.top()), pos2(full.right(), canvas.bottom()));
        let hbar_rect = Rect::from_min_max(pos2(full.left(), canvas.bottom()), pos2(canvas.right(), full.bottom()));
        let vstate = scrollbar::update(
            ui,
            Id::new("vbar"),
            vbar_rect,
            true,
            &mut self.scroll.y,
            max.y,
            canvas.height() / scale,
            layout.content_h(scale),
            &mut self.vbar_grab,
        );
        let hstate = hbar.then(|| {
            scrollbar::update(
                ui,
                Id::new("hbar"),
                hbar_rect,
                false,
                &mut self.scroll.x,
                max.x,
                canvas.width() / scale,
                layout.content_w(),
                &mut self.hbar_grab,
            )
        });

        if self.vbar_grab.is_some() || self.hbar_grab.is_some() {
            self.kinetic.stop();
        }
        if self.kinetic.is_active() {
            ui.ctx().request_repaint();
        }

        let view = View::new(canvas, self.scroll, scale, layout);
        self.shown_content = (self.scroll.y, self.scroll.y + visible);
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| canvas.contains(*p));
        self.picture_input(ui.ctx(), &view, pointer);

        // ---- pen / mouse input
        self.update_pressure(ui.ctx());
        let mut moves: Vec<Pos2> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::PointerMoved(p) => Some(*p),
                    _ => None,
                })
                .collect()
        });
        if moves.is_empty() {
            // Out of the window (an item dragged there), where the pointer last was.
            moves.extend(canvas_resp.interact_pointer_pos().or(self.drag_pointer));
        }
        // ---- right click: context menu for what is under the pointer
        let right_click = ui.input(|i| i.pointer.secondary_clicked());
        if right_click
            && !self.busy()
            && self.saved_tool.is_none()
            && ui.rect_contains_pointer(canvas)
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
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

        if !self.panning && !swallowed && !self.navigating() {
            match self.tool {
                // Moving around is handled above.
                Tool::Pointer => {}
                Tool::Eraser => self.handle_erase(&canvas_resp, &view, press, &moves),
                Tool::Select => self.handle_select(&canvas_resp, &view, press, &moves),
                Tool::Text => self.handle_text(&canvas_resp, &view, press, &moves),
                Tool::Pen | Tool::Highlighter => self.handle_ink(&canvas_resp, &view, press, &moves),
            }
        }

        if !ui.input(|i| i.pointer.primary_down()) {
            self.finger_nav = false;
        }

        // ---- painting
        // Bottom to top: paper, highlighters, rules, text, pen (see `render`).
        let painter = ui.painter_at(canvas);
        render::paint_paper(&painter, &view);
        // A note written on a PDF: its pages, right on the paper.
        for page in view.visible_pages() {
            self.pdf_textures.paint(&painter, &self.doc, page, view.page_screen_rect(page), true);
            // The reader: the links of the pages shown, ready before they are pointed at.
            if let (true, Some(pdf)) = (self.navigating(), &self.doc.pdf)
                && let (Some(file), Some(pdf_page)) = (&pdf.file, self.doc.pdf_page(page))
            {
                self.pdf_nav.links(file, pdf_page);
            }
        }
        let ppp = ui.ctx().pixels_per_point();
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
                a.live.paint(&painter, &view, a.page, a.kind, a.color, a.width, points, ppp);
            }
        };
        render::paint_rules(&painter, &view, &self.doc);
        // Pictures, on the paper and under the ink; the ones dragged (alone or
        // selected) where they are now.
        picture_moves.extend(self.picture_drag.as_ref().map(|d| d.now.clone()));
        for page in view.visible_pages() {
            let origin = view.page_origin_screen(page);
            self.picture_textures.paint_page(&painter, &self.doc.images, page, origin, scale, &picture_moves);
        }
        // Those of a selection carried far (the view scrolled while dragging),
        // their page out of view: drawn where they are now all the same.
        for img in picture_moves.iter().filter(|i| !view.visible_pages().contains(&i.page)) {
            let origin = view.page_origin_screen(img.page);
            self.picture_textures.paint_page(&painter, std::slice::from_ref(img), img.page, origin, scale, &[]);
        }
        let strokes = &self.doc.strokes;
        render::paint_strokes(&painter, &view, Layer::Highlight, strokes, &mut self.cache, ppp, moved.as_ref());
        paint_live(StrokeKind::Highlighter);
        // A text box dragged with the text tool.
        text_moves.extend(
            self.text_press
                .as_ref()
                .and_then(|tp| if tp.moved { tp.hit.map(|id| (id, tp.now - tp.start)) } else { None }),
        );
        render::paint_texts(
            &painter,
            &view,
            &self.doc.texts,
            self.edit.as_ref().map(|e| e.id),
            &text_moves,
            self.tool == Tool::Text,
        );

        render::paint_strokes(&painter, &view, Layer::Ink, strokes, &mut self.cache, ppp, moved.as_ref());
        paint_live(StrokeKind::Pen);
        // Selected text and search matches, over the pages.
        self.paint_text_marks(&painter, &view);

        if let Some(rect) = link_under_pointer {
            painter.rect_filled(rect.expand(2.0), 2.0, color::accent_alpha(36));
        }

        // Selection box and lasso outline.
        let blue = color::ACCENT;
        if let Some(sel) = &self.selection {
            let r = sel.bounds.translate(layout.page_origin(sel.page).to_vec2() + move_off);
            let rect = Rect::from_min_max(view.content_to_screen(r.min), view.content_to_screen(r.max));
            painter.rect_filled(rect, 2.0, color::accent_alpha(28));
            let path = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
            painter.extend(egui::Shape::dashed_line(&path, egui::Stroke::new(1.5, blue), 6.0, 4.0));
        }
        if self.tool == Tool::Select {
            self.paint_picked_picture(&painter, &view, canvas_resp.hover_pos());
        }
        if let Some(l) = &self.lasso {
            let o = layout.page_origin(l.page).to_vec2();
            let pts: Vec<Pos2> = l.points.iter().map(|p| view.content_to_screen(*p + o)).collect();
            if pts.len() >= 2 {
                painter.extend(egui::Shape::dashed_line(&pts, egui::Stroke::new(1.5, blue), 5.0, 4.0));
            }
        }
        // The tool cursor, the same for pen and mouse: over a page, pen,
        // highlighter and eraser replace the system pointer with their tip
        // (a dot like the stroke they would draw, OneNote style, or the
        // eraser's circle). Pointer, select and text keep the system pointer.
        // A hovering pen is not reported by winit as a pointer, so it is read
        // from the native source. Not where something else (a toolbar, a
        // panel) is on top of the canvas.
        let ctx = ui.ctx().clone();
        let pen_hover = self
            .pen_input
            .windows
            .as_ref()
            .and_then(|p| p.hover())
            .map(|(x, y)| pos2(x / ppp, y / ppp))
            .or(self.pen_input.hover)
            .filter(|p| canvas.contains(*p) && ctx.layer_id_at(*p) == Some(ui.layer_id()));
        let tip_pos = pen_hover
            .or(canvas_resp.hover_pos())
            .or(canvas_resp.interact_pointer_pos())
            .filter(|p| view.layout.page_at(view.screen_to_content(*p)).is_some());
        let grabbing = self.panning || self.vbar_grab.is_some() || self.hbar_grab.is_some();
        if let (Some(pos), false, false) = (tip_pos, self.navigating(), grabbing) {
            if self.tool == Tool::Eraser {
                hide_pointer(&ctx);
                render::paint_eraser_cursor(&painter, pos, self.settings.eraser_radius * scale);
            } else if let Some((kind, rgba, width)) = self.stroke_params() {
                hide_pointer(&ctx);
                // While drawing, the stroke's own end is the tip.
                if self.active.is_none() {
                    render::paint_pen_cursor(&painter, pos, kind, rgba, width * scale);
                }
            }
        }

        // Rubber band while dragging out a new text box.
        if let Some(tp) = &self.text_press
            && tp.hit.is_none()
            && tp.resize.is_none()
            && tp.moved
            && tp.page.is_some()
        {
            let r = Rect::from_two_pos(view.content_to_screen(tp.start), view.content_to_screen(tp.now));
            let path = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.extend(egui::Shape::dashed_line(&path, egui::Stroke::new(1.5, blue), 6.0, 4.0));
        }

        // In-place editor of the text box being typed into (a real egui
        // TextEdit: cursor, selection, clipboard, IME, its own Ctrl+Z).
        let mut commit_now = false;
        if let Some(e) = self.edit.as_mut() {
            // Toolbar size / colour changes apply live.
            e.size = self.settings.text.width;
            e.color = self.settings.text.color;
        }
        if let (Some(frame), Some(e)) = (self.edit_frame(&view), self.edit.as_mut()) {
            painter.rect_filled(frame, 3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 215));
            painter.rect_stroke(frame, 3.0, egui::Stroke::new(1.5, blue), egui::StrokeKind::Middle);
            // The resize handles, one on either side.
            for x in [frame.left(), frame.right()] {
                let size = vec2(6.0, (frame.height() - 8.0).clamp(8.0, 22.0));
                let handle = Rect::from_center_size(pos2(x, frame.center().y), size);
                painter.rect(handle, 3.0, Color32::WHITE, egui::Stroke::new(1.5, blue), egui::StrokeKind::Middle);
            }

            // The editor: the frame without its room for the outline.
            let editor = frame.shrink(2.0);

            let id = Id::new(("text_edit", e.id));
            let color = Color32::from_rgb(e.color[0], e.color[1], e.color[2]);
            let size = (e.size * scale).max(6.0);
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
                .margin(EDIT_MARGIN)
                .frame(egui::Frame::NONE.inner_margin(EDIT_MARGIN))
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

        // "Add page" below the last page, the same dashed tile as in the
        // pages panel, as wide as the page.
        let last = view.page_screen_rect(layout.count - 1);
        let btn = Rect::from_min_size(pos2(last.left(), last.bottom() + 18.0), vec2(last.width(), 40.0));
        if btn.intersects(canvas) && !self.reading() {
            let mut tile = ui.new_child(egui::UiBuilder::new().max_rect(canvas));
            tile.set_clip_rect(canvas);
            if theme::add_tile(&mut tile, btn, Id::new("add_page"), "Add page").clicked() {
                self.insert_page(self.doc.page_count);
            }
        }

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
        scrollbar::paint(full_painter, vbar_rect, true, &vstate, Some(&label));
        if let Some(h) = &hstate {
            scrollbar::paint(full_painter, hbar_rect, false, h, None);
        }
    }
}

/// A wheel event from a mouse wheel's notches rather than a touchpad. Both
/// arrive as lines on Windows: a notch is whole lines, a touchpad sends
/// fractions (so does a high-resolution wheel, which then counts as a
/// touchpad). Pixel deltas come from touchpads.
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

fn is_wheel_notch(unit: MouseWheelUnit, delta: Vec2) -> bool {
    let whole = |v: f32| (v - v.round()).abs() < 1e-3;
    unit == MouseWheelUnit::Line && delta != Vec2::ZERO && whole(delta.x) && whole(delta.y)
}

#[cfg(test)]
mod tests {
    use super::*;

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
