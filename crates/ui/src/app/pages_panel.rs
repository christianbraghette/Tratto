// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The sidebar, on the left or (for left-handed writers) on the right: its
//! strip of view buttons and the pages view (previews to jump to, reorder and
//! delete pages).

use eframe::egui::{self, Align2, Color32, Id, Rect, Sense, pos2, vec2};

use tratto_core::history::Action;

use crate::render;
use crate::scrollbar;
use crate::settings::{Sidebar, SidebarSide};
use crate::theme::{self, color, radius, space};
use crate::toolbar;
use crate::view::{Layout, MARGIN};

use super::InkApp;
use super::menus;

/// Width of the pages view (points); every view of the sidebar has its own,
/// fixed.
const PAGES_WIDTH: f32 = 164.0;
/// Page previews never get wider than this, however wide the panel.
const THUMB_MAX_W: f32 = 200.0;
/// Fullscreen: a swipe that shows the sidebar starts this close to its
/// window edge (as the title bar's from the top edge) and goes at least
/// this far in.
const SWIPE_EDGE: f32 = 24.0;
const SWIPE_IN: f32 = 48.0;

/// Whether `p` is close enough to the window's bottom edge to start the
/// swipe that shows the taskbar.
pub(super) fn from_bottom_edge(ctx: &egui::Context, p: egui::Pos2) -> bool {
    ctx.viewport_rect().bottom() - p.y <= SWIPE_EDGE
}
/// Slide in / out time of the sidebar in fullscreen, in seconds.
const SLIDE_TIME: f32 = 0.2;
/// Fade-in time of a sidebar view opened or switched to, in seconds.
const VIEW_FADE_TIME: f32 = 0.15;

impl InkApp {
    /// Before the pages are added, removed or moved: the text box being
    /// typed is committed (it knows its page by number, as the selection
    /// does), the selection dropped.
    pub(super) fn before_page_change(&mut self) {
        if self.edit.is_some() {
            let ctx = self.egui_ctx.clone();
            self.commit_edit(&ctx);
        }
        self.clear_selection();
    }

    /// Inserts an empty page so that it gets `index` (undoable). Not on an
    /// infinite page, the only one.
    pub(super) fn insert_page(&mut self, index: u32) {
        if self.doc.is_canvas() {
            return;
        }
        self.before_page_change();
        self.history.exec(&mut self.doc, Action::InsertPage { index, pdf_page: None });
    }

    /// Deletes a page with its content (undoable). Not the page of an
    /// infinite page.
    pub(super) fn delete_page(&mut self, index: u32) {
        if self.doc.is_canvas() {
            return;
        }
        self.before_page_change();
        self.history.exec(&mut self.doc, Action::DeletePage { index });
        self.toast(format!("Page {} deleted (Ctrl+Z restores it)", index + 1));
    }

    /// The sidebar: a strip of view buttons along the window edge, always
    /// shown, and the shown view, if any, next to it. Each view is its own
    /// panel, of its own fixed width.
    ///
    /// In fullscreen it starts hidden behind a notch in the top corner
    /// ([`Self::fullscreen_sidebar_notch`]); clicking the highlighted view
    /// hides it there again. It slides in and out from the window edge, as
    /// the title bar does from the top.
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui, fullscreen: bool) {
        if !fullscreen {
            self.chrome.fullscreen_sidebar = false;
        } else if self.chrome.sidebar_seen.is_some_and(|s| s != self.settings.sidebar)
            && self.settings.sidebar != Sidebar::Closed
        {
            self.chrome.fullscreen_sidebar = true;
        }
        let shown = !fullscreen || self.chrome.fullscreen_sidebar;
        // Escape closes it (in full screen, back behind the notch), when
        // nothing else wants it: a window, a menu, a dialog, a field being
        // typed in, a selection (all take it before).
        let free = self.context_menu.is_none()
            && self.lib_ui.dialog.is_none()
            && !ui.ctx().egui_wants_keyboard_input()
            && !egui::Popup::is_any_open(ui.ctx());
        if shown
            && (fullscreen || self.settings.sidebar != Sidebar::Closed)
            && free
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            if fullscreen {
                self.chrome.fullscreen_sidebar = false;
            } else {
                self.settings.sidebar = Sidebar::Closed;
            }
        }
        let shown = !fullscreen || self.chrome.fullscreen_sidebar;
        // Into or out of full screen (or presenting) it is there or not at
        // once: sliding while the window itself changes size looks broken.
        let switched = self.chrome.sidebar_fullscreen != Some(fullscreen);
        self.chrome.sidebar_fullscreen = Some(fullscreen);
        let time = if switched { 0.0 } else { SLIDE_TIME };
        let reveal = ui.ctx().animate_bool_with_time(Id::unique("sidebar_reveal"), shown, time);
        self.chrome.sidebar_reveal = reveal;
        if reveal <= 0.0 {
            self.page_drag = None;
        } else if reveal >= 1.0 {
            let before = ui.available_rect_before_wrap().width();
            self.sidebar_panels(ui, fullscreen);
            self.chrome.sidebar_width = before - ui.available_rect_before_wrap().width();
        } else {
            // The panel grows with the reveal, the sidebar (as wide as when
            // last shown) slides out from the edge inside it.
            let full = self.chrome.sidebar_width.max(toolbar::STRIP_WIDTH);
            let side = self.settings.sidebar_side;
            Self::side_panel(side, "sidebar_slide")
                .resizable(false)
                .exact_size(full * reveal)
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(color::chrome()))
                .show(ui, |ui| {
                    let r = ui.max_rect();
                    let rect = match side {
                        SidebarSide::Left => Rect::from_min_max(pos2(r.right() - full, r.top()), r.max),
                        SidebarSide::Right => Rect::from_min_size(r.min, vec2(full, r.height())),
                    };
                    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    inner.set_clip_rect(r.intersect(ui.clip_rect()));
                    self.sidebar_panels(&mut inner, fullscreen);
                });
        }
        self.chrome.sidebar_seen = Some(self.settings.sidebar);
    }

    /// The sidebar's panels: the strip, and the shown view next to it.
    fn sidebar_panels(&mut self, ui: &mut egui::Ui, fullscreen: bool) {
        let view = self.settings.sidebar;
        if view != Sidebar::Pages {
            self.page_drag = None;
        }
        // Only a note written on a PDF has an outline; a reader-only window
        // has no library.
        let on_pdf = self.doc.pdf.is_some();
        let library = self.standalone.is_none();
        let wrong_view = view == Sidebar::Outline && !on_pdf
            || view == Sidebar::Settings && self.reading()
            || view == Sidebar::Notebook && !library;
        if wrong_view {
            self.settings.sidebar = Sidebar::Pages;
            return self.sidebar_panels(ui, fullscreen);
        }
        let last = self.settings.sidebar_last;
        if last == Sidebar::Outline && !on_pdf || last == Sidebar::Notebook && !library {
            self.settings.sidebar_last = Sidebar::Pages;
        }
        if view != Sidebar::Closed {
            self.settings.sidebar_last = view;
        }
        let note_settings = !self.reading();
        // An infinite page is the only one: no pages to show.
        let canvas = self.doc.is_canvas();
        let (top, bottom) = (toolbar::notch_margin(true), toolbar::notch_margin(false));
        // On the right the canvas scrollbar (same surface) sits next to the
        // sidebar, so the extra margin keeps the buttons exactly where they
        // are in the notches.
        let (width, outer) = match self.settings.sidebar_side {
            SidebarSide::Left => (toolbar::STRIP_WIDTH, 0),
            SidebarSide::Right => (toolbar::STRIP_WIDTH + scrollbar::THICKNESS, scrollbar::THICKNESS as i8),
        };
        let mut clicked = None;
        Self::side_panel(self.settings.sidebar_side, "sidebar_strip")
            .resizable(false)
            .exact_size(width)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(color::chrome()).inner_margin(egui::Margin {
                left: top.left,
                right: top.right + outer,
                top: top.top,
                bottom: bottom.bottom,
            }))
            .show(ui, |ui| {
                toolbar::notch_buttons_style(ui);
                if library && toolbar::notebook_button(ui, view == Sidebar::Notebook) {
                    clicked = Some(Sidebar::Notebook);
                }
                if !canvas && toolbar::pages_button(ui, view == Sidebar::Pages) {
                    clicked = Some(Sidebar::Pages);
                }
                if on_pdf && toolbar::outline_button(ui, view == Sidebar::Outline) {
                    clicked = Some(Sidebar::Outline);
                }
                if toolbar::search_button(ui, view == Sidebar::Search) {
                    clicked = Some(Sidebar::Search);
                }
                if note_settings && toolbar::settings_button(ui, view == Sidebar::Settings) {
                    clicked = Some(Sidebar::Settings);
                }
            });
        self.view_with_fade(ui, view);
        if let Some(v) = clicked {
            if fullscreen && v == view {
                // Back behind the notch, keeping the view for next time.
                self.chrome.fullscreen_sidebar = false;
            } else {
                self.settings.sidebar.toggle(v);
            }
            if self.settings.sidebar == Sidebar::Search {
                self.search_panel.focus = true;
            }
        }
    }

    /// Shows `view` next to the strip. A view just opened, or switched to,
    /// fades in as the menus do; its panel's surface is there at once, so the
    /// canvas never shows through.
    fn view_with_fade(&mut self, ui: &mut egui::Ui, view: Sidebar) {
        let now = ui.input(|i| i.time);
        if self.chrome.view_fade.is_none_or(|(v, _)| v != view) {
            self.chrome.view_fade = Some((view, now));
        }
        let start = self.chrome.view_fade.map_or(now, |(_, t)| t);
        let t = ((now - start) as f32 / VIEW_FADE_TIME).clamp(0.0, 1.0);
        let alpha = egui::emath::easing::cubic_out(t);
        if t < 1.0 {
            ui.ctx().request_repaint();
        }

        let surface = ui.painter().add(egui::Shape::Noop);
        let before = ui.available_rect_before_wrap();
        let opacity = ui.opacity();
        ui.set_opacity(opacity * alpha);
        match view {
            Sidebar::Notebook => self.notebook_view(ui),
            // Kept for the next note with pages.
            Sidebar::Pages if self.doc.is_canvas() => {}
            Sidebar::Pages => self.pages_view(ui),
            Sidebar::Settings => self.note_settings_view(ui),
            Sidebar::Outline => self.outline_view(ui),
            Sidebar::Search => self.search_view(ui),
            Sidebar::Closed => {}
        }
        ui.set_opacity(opacity);
        // The width the view took from the side of the window.
        let after = ui.available_rect_before_wrap();
        let taken = match self.settings.sidebar_side {
            SidebarSide::Left => Rect::from_min_max(before.min, pos2(after.left(), before.bottom())),
            SidebarSide::Right => Rect::from_min_max(pos2(after.right(), before.top()), before.max),
        };
        if alpha < 1.0 && taken.width() > 0.0 {
            ui.painter().set(surface, egui::Shape::rect_filled(taken, 0.0, color::chrome()));
        }
    }

    /// Fullscreen, with the sidebar hidden: the notch in its top corner that
    /// shows it again, with the view it last showed.
    pub(super) fn fullscreen_sidebar_notch(&mut self, ui: &egui::Ui) {
        if toolbar::show_sidebar_notch(ui, self.settings.sidebar_side, self.settings.sidebar_last) {
            self.show_fullscreen_sidebar();
        }
    }

    /// Fullscreen, touch screens: a finger swiping in from the sidebar's
    /// window edge shows the sidebar, like its notch. The swipe down from
    /// the top edge shows the title bar (see `chrome`), the one up from the
    /// bottom edge the taskbar (see `taskbar_swipe`); either way the canvas
    /// ignores the finger (`edge_touch`) until it lifts.
    pub(super) fn edge_swipe(&mut self, ctx: &egui::Context, fullscreen: bool) {
        let (origin, pos, down, pressed) = ctx.input(|i| {
            (i.pointer.press_origin(), i.pointer.latest_pos(), i.pointer.primary_down(), i.pointer.any_pressed())
        });
        if !down {
            self.edge_touch = false;
            return;
        }
        let screen = ctx.viewport_rect();
        let right = self.settings.sidebar_side == SidebarSide::Right;
        let from_side = |p: egui::Pos2| {
            let d = if right { screen.right() - p.x } else { p.x - screen.left() };
            d <= SWIPE_EDGE
        };
        let hidden = !self.chrome.fullscreen_sidebar;
        if pressed {
            self.edge_touch = fullscreen
                && self.finger(ctx)
                && origin.is_some_and(|o| {
                    o.y - screen.top() <= SWIPE_EDGE || from_bottom_edge(ctx, o) || hidden && from_side(o)
                });
        }
        if self.edge_touch
            && hidden
            && let (Some(o), Some(p)) = (origin, pos)
            && from_side(o)
        {
            let inward = if right { o.x - p.x } else { p.x - o.x };
            if inward >= SWIPE_IN && inward > (p.y - o.y).abs() {
                self.show_fullscreen_sidebar();
            }
        }
    }

    /// Fullscreen, touch screens: a finger that started on the bottom edge
    /// (`edge_touch`) and swipes up shows the taskbar, as outside the app
    /// (the system's own swipe is turned off, see `chrome::own_edge_swipes`).
    /// Once per touch.
    pub(super) fn taskbar_swipe(&mut self, ctx: &egui::Context) {
        let (origin, pos, down) =
            ctx.input(|i| (i.pointer.press_origin(), i.pointer.latest_pos(), i.pointer.primary_down()));
        if !down {
            self.taskbar_swiped = false;
            return;
        }
        if self.taskbar_swiped || !self.edge_touch {
            return;
        }
        if let (Some(o), Some(p)) = (origin, pos)
            && from_bottom_edge(ctx, o)
        {
            let up = o.y - p.y;
            if up >= SWIPE_IN && up > (p.x - o.x).abs() {
                self.taskbar_swiped = true;
                crate::chrome::show_taskbar();
            }
        }
    }

    /// Fullscreen: shows the hidden sidebar, with the view it last showed.
    fn show_fullscreen_sidebar(&mut self) {
        self.chrome.fullscreen_sidebar = true;
        if self.settings.sidebar == Sidebar::Closed {
            self.settings.sidebar = match self.settings.sidebar_last {
                Sidebar::Closed => Sidebar::Pages,
                last => last,
            };
        }
        // Not taken as a view opened from elsewhere.
        self.chrome.sidebar_seen = Some(self.settings.sidebar);
    }

    /// A panel of the sidebar, on its `side` of the window.
    pub(super) fn side_panel(side: SidebarSide, id: &'static str) -> egui::Panel {
        match side {
            SidebarSide::Left => egui::Panel::left(id),
            SidebarSide::Right => egui::Panel::right(id),
        }
    }

    /// Margin of a sidebar view: content starting level with the top
    /// button; `padded` (views of text: search, contents, settings) leaves
    /// more room around it.
    fn view_margin(padded: bool) -> egui::Margin {
        let top = toolbar::notch_margin(true).top;
        if padded {
            egui::Margin {
                left: space::M as i8,
                right: space::M as i8,
                top: top + space::S as i8,
                bottom: space::S as i8,
            }
        } else {
            egui::Margin { left: space::S as i8, right: space::S as i8, top, bottom: space::S as i8 }
        }
    }

    /// Frame of a sidebar view: the strip's surface, with `view_margin`.
    pub(super) fn view_frame(padded: bool) -> egui::Frame {
        egui::Frame::NONE.fill(color::chrome()).inner_margin(Self::view_margin(padded))
    }

    /// Hairline between the button strip and a sidebar view (framed with
    /// the same `padded`) on the sidebar's `side`.
    pub(super) fn view_divider(ui: &egui::Ui, padded: bool, side: SidebarSide) {
        let r = ui.max_rect();
        let x = match side {
            SidebarSide::Left => r.left() - f32::from(Self::view_margin(padded).left) + 0.5,
            SidebarSide::Right => r.right() + f32::from(Self::view_margin(padded).right) - 0.5,
        };
        let y = (r.top() + space::XS)..=(r.bottom() - space::XS);
        ui.painter().vline(x, y, egui::Stroke::new(1.0, color::border()));
    }

    fn pages_view(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        let layout = Layout::of(&self.doc);
        let ppp = ctx.pixels_per_point();
        let current = self.current_page;
        let blue = color::accent();
        // A PDF in the reader: its pages are looked at, not changed.
        let editable = !self.reading();
        let can_paste = self.clipboard.has_pages();

        // Disjoint field borrows, released again after `show`.
        let doc = &self.doc;
        let strokes = &self.doc.strokes;
        let texts = &self.doc.texts;
        let cache = &mut self.thumb_cache;
        let thumb_zoom = &mut self.thumb_zoom;
        let pdf_textures = &mut self.pdf_textures;
        let pictures = &mut self.picture_textures;
        let drag = &mut self.page_drag;

        // Scroll the list to the current page when it changes.
        let follow = self.panel_page != Some(current) && drag.is_none();
        self.panel_page = Some(current);
        let mut jump: Option<u32> = None;
        let mut delete: Option<u32> = None;
        let mut add = false;
        let mut insert: Option<u32> = None;
        let mut reorder: Option<(u32, u32)> = None;
        // Copy, cut or paste pages (see `clipboard`).
        let mut clip: Option<menus::Command> = None;
        let side = self.settings.sidebar_side;

        // The concave joins with the title bar and the bottom edge are drawn
        // by `toolbar::show_joins`.
        Self::side_panel(side, "sidebar_pages")
            .resizable(false)
            .exact_size(PAGES_WIDTH)
            .show_separator_line(false)
            .frame(Self::view_frame(false))
            .show(ui, |ui| {
                Self::view_divider(ui, false, side);

                // Page cards, centred; the scroll bar floats over the content,
                // so the full width is usable.
                let avail = ui.available_width();
                let thumb_w = (avail - 2.0 * space::M).clamp(40.0, THUMB_MAX_W);
                let thumb_h = thumb_w * layout.page_h / layout.page_w;
                let scale = thumb_w / layout.page_w;
                // Resizing the panel: made again once it is done.
                let made = thumb_zoom.update(ui.ctx(), scale, false);
                cache.begin_frame(ppp);
                let aa = 1.0 / ppp;
                let pointer = ui.input(|i| i.pointer.interact_pos());
                // Room around the page inside its card (more above, so the
                // hover fill and the current-page outline have space).
                const PAD_TOP: f32 = 12.0;
                const PAD: f32 = 6.0;
                const NUMBER_H: f32 = 18.0;

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let mut rects: Vec<Rect> = Vec::with_capacity(layout.count as usize);
                    for i in 0..layout.count {
                        let (card, _) = ui
                            .allocate_exact_size(vec2(avail, PAD_TOP + thumb_h + PAD + NUMBER_H + PAD), Sense::hover());
                        let rect = Rect::from_min_size(
                            pos2(card.center().x - thumb_w * 0.5, card.top() + PAD_TOP),
                            vec2(thumb_w, thumb_h),
                        );
                        rects.push(rect);
                        // The whole card (page and number) is the target.
                        let resp = ui.interact(card, ui.scope_id().with(("page_card", i)), Sense::click_and_drag());
                        if follow && i == current {
                            ui.scroll_to_rect(card, None);
                        }

                        if ui.is_rect_visible(card) {
                            let p = ui.painter();
                            if resp.hovered() && drag.is_none() {
                                p.rect_filled(card, radius::MEDIUM, color::hover().gamma_multiply(0.6));
                            }
                            p.rect_filled(rect.translate(vec2(0.0, 2.0)), 2.0, Color32::from_black_alpha(90));
                            p.rect_filled(rect, 0.0, crate::theme::color::page());
                            let clipped = ui.painter_at(rect);
                            pdf_textures.paint(&clipped, doc, i, rect, false, 1.0);
                            pictures.paint_page(&clipped, &doc.images, i, rect.min, (scale, made), &[]);
                            let offset = rect.min.to_vec2();
                            render::paint_page_strokes(&clipped, offset, scale, made, strokes, i, cache, aa);
                            render::paint_page_texts(&clipped, rect.min.to_vec2(), scale, texts, i);
                            let p = ui.painter();
                            if i == current {
                                p.rect_stroke(
                                    rect.expand(2.5),
                                    3.0,
                                    egui::Stroke::new(2.0, blue),
                                    egui::StrokeKind::Middle,
                                );
                            } else {
                                p.rect_stroke(
                                    rect,
                                    0.0,
                                    egui::Stroke::new(1.0, color::border()),
                                    egui::StrokeKind::Middle,
                                );
                            }
                            // Page number; the current one bright and semi-bold.
                            let center = pos2(card.center().x, rect.bottom() + PAD + NUMBER_H * 0.5);
                            let text = format!("{}", i + 1);
                            let (font, ink) = if i == current {
                                (theme::font_strong(theme::size::CAPTION), color::text_strong())
                            } else {
                                (theme::font(theme::size::CAPTION), color::text_muted())
                            };
                            p.text(center, Align2::CENTER_CENTER, text, font, ink);
                        }

                        // Delete button, registered after the card so it wins hit-testing.
                        let del_rect = Rect::from_center_size(rect.right_top() + vec2(-13.0, 13.0), vec2(22.0, 22.0));
                        let del = ui.interact(del_rect, ui.scope_id().with(("page_del", i)), Sense::click());
                        if editable && ui.rect_contains_pointer(card) && drag.is_none() {
                            let p = ui.painter();
                            let fill = if del.hovered() { color::danger() } else { color::chrome() };
                            p.circle(del_rect.center(), 10.0, fill, egui::Stroke::new(1.0, color::border()));
                            let a = 3.5;
                            let x =
                                egui::Stroke::new(1.6, if del.hovered() { color::on_accent() } else { color::text() });
                            let c = del_rect.center();
                            p.line_segment([c + vec2(-a, -a), c + vec2(a, a)], x);
                            p.line_segment([c + vec2(-a, a), c + vec2(a, -a)], x);
                        }
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                        resp.context_menu(|ui| {
                            use egui_phosphor::regular as icon;
                            if menus::entry(ui, icon::ARROW_SQUARE_OUT, "Go to page", "", true) {
                                jump = Some(i);
                                ui.close();
                            }
                            if !editable {
                                return;
                            }
                            ui.separator();
                            if menus::entry(ui, icon::COPY, "Copy page", "", true) {
                                clip = Some(menus::Command::CopyPage(i));
                                ui.close();
                            }
                            if menus::entry(ui, icon::SCISSORS, "Cut page", "", true) {
                                clip = Some(menus::Command::CutPage(i));
                                ui.close();
                            }
                            if menus::entry(ui, icon::CLIPBOARD, "Paste page", "", can_paste) {
                                clip = Some(menus::Command::PastePages(i + 1));
                                ui.close();
                            }
                            ui.separator();
                            if menus::entry(ui, icon::ARROW_LINE_UP, "Insert page before", "", true) {
                                insert = Some(i);
                                ui.close();
                            }
                            if menus::entry(ui, icon::ARROW_LINE_DOWN, "Insert page after", "", true) {
                                insert = Some(i + 1);
                                ui.close();
                            }
                            if menus::entry(ui, icon::TRASH, "Delete page", "", true) {
                                delete = Some(i);
                                ui.close();
                            }
                        });
                        if editable && del.clicked() {
                            delete = Some(i);
                        } else if resp.clicked() {
                            jump = Some(i);
                        }
                        if editable && resp.drag_started() {
                            *drag = Some(i);
                        }
                    }
                    if editable {
                        ui.add_space(space::XS);
                        let (row, _) = ui.allocate_exact_size(vec2(avail, 36.0), Sense::hover());
                        let tile = Rect::from_center_size(row.center(), vec2(thumb_w, 36.0));
                        if theme::add_tile(ui, tile, Id::unique("add_page_tile"), "Add page").clicked() {
                            add = true;
                        }
                    }

                    // Drag & drop reordering.
                    if let Some(from) = *drag {
                        match pointer {
                            Some(p) if !rects.is_empty() => {
                                let slot = rects.iter().filter(|r| r.center().y < p.y).count();
                                let y = if slot < rects.len() {
                                    rects[slot].top() - 3.0
                                } else {
                                    rects[rects.len() - 1].bottom() + 3.0
                                };
                                let r0 = rects[0];
                                ui.painter().line_segment(
                                    [pos2(r0.left(), y), pos2(r0.right(), y)],
                                    egui::Stroke::new(3.0, blue),
                                );

                                // Auto-scroll near the edges of the list.
                                let clip = ui.clip_rect();
                                if p.y < clip.top() + 30.0 {
                                    ui.scroll_with_delta(vec2(0.0, 14.0));
                                } else if p.y > clip.bottom() - 30.0 {
                                    ui.scroll_with_delta(vec2(0.0, -14.0));
                                }

                                // Floating label under the pen.
                                let ghost = ui.ctx().layer_painter(egui::LayerId::new(
                                    egui::Order::Tooltip,
                                    Id::unique("page_drag_ghost"),
                                ));
                                theme::paint_toast(&ghost, p + vec2(56.0, 20.0), &format!("Page {}", from + 1));

                                if ui.input(|i| !i.pointer.any_down()) {
                                    let slot = slot as u32;
                                    let to = if slot > from { slot - 1 } else { slot };
                                    if to != from {
                                        reorder = Some((from, to));
                                    }
                                    *drag = None;
                                }
                            }
                            _ => *drag = None,
                        }
                    }
                });
            });

        if let Some((from, to)) = reorder {
            self.before_page_change();
            self.history.exec(&mut self.doc, Action::MovePage { from, to });
            self.scroll_to = Some(layout.page_top(to) - MARGIN);
        }
        if let Some(index) = delete {
            self.delete_page(index);
        }
        if let Some(i) = jump {
            self.scroll_to = Some(layout.page_top(i) - MARGIN);
        }
        if add {
            self.insert_page(self.doc.page_count);
        }
        if let Some(index) = insert {
            self.insert_page(index);
            self.scroll_to = Some(layout.page_top(index) - MARGIN);
        }
        self.pending.extend(clip);
    }
}
