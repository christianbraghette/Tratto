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

/// Default width of the pages view (points); every view of the sidebar
/// remembers its own width.
const PAGES_WIDTH: f32 = 164.0;
/// The narrowest width still fits the "Add page" tile's label.
const PAGES_WIDTHS: std::ops::RangeInclusive<f32> = 150.0..=360.0;
/// Page previews never get wider than this, however wide the panel.
const THUMB_MAX_W: f32 = 200.0;
/// Fullscreen: a swipe that shows the sidebar starts this close to its
/// window edge (as the title bar's from the top edge) and goes at least
/// this far in.
const SWIPE_EDGE: f32 = 24.0;
const SWIPE_IN: f32 = 48.0;

impl InkApp {
    /// Inserts an empty page so that it gets `index` (undoable).
    pub(super) fn insert_page(&mut self, index: u32) {
        self.clear_selection();
        self.history.exec(&mut self.doc, Action::InsertPage { index, pdf_page: None });
    }

    /// Deletes a page with its content (undoable).
    pub(super) fn delete_page(&mut self, index: u32) {
        self.clear_selection();
        self.history.exec(&mut self.doc, Action::DeletePage { index });
        self.toast(format!("Page {} deleted (Ctrl+Z restores it)", index + 1));
    }

    /// The sidebar: a strip of view buttons along the window edge, always
    /// shown, and the shown view, if any, next to it. Each view is its own
    /// resizable panel, so each keeps its own width.
    ///
    /// In fullscreen it starts hidden behind a notch in the top corner
    /// ([`Self::fullscreen_sidebar_notch`]); clicking the highlighted view
    /// hides it there again.
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui, fullscreen: bool) {
        if !fullscreen {
            self.chrome.fullscreen_sidebar = false;
        } else if self.chrome.sidebar_seen.is_some_and(|s| s != self.settings.sidebar)
            && self.settings.sidebar != Sidebar::Closed
        {
            self.chrome.fullscreen_sidebar = true;
        }
        let hidden = fullscreen && !self.chrome.fullscreen_sidebar;
        let view = if hidden { Sidebar::Closed } else { self.settings.sidebar };
        if view != Sidebar::Pages {
            self.page_drag = None;
        }
        if hidden {
            self.chrome.sidebar_seen = Some(self.settings.sidebar);
            return;
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
            return self.sidebar(ui, fullscreen);
        }
        let last = self.settings.sidebar_last;
        if last == Sidebar::Outline && !on_pdf || last == Sidebar::Notebook && !library {
            self.settings.sidebar_last = Sidebar::Pages;
        }
        if view != Sidebar::Closed {
            self.settings.sidebar_last = view;
        }
        let note_settings = !self.reading();
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
                if toolbar::pages_button(ui, view == Sidebar::Pages) {
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
        match view {
            Sidebar::Notebook => self.notebook_view(ui),
            Sidebar::Pages => self.pages_view(ui),
            Sidebar::Settings => self.note_settings_view(ui),
            Sidebar::Outline => self.outline_view(ui),
            Sidebar::Search => self.search_view(ui),
            Sidebar::Closed => {}
        }
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
        self.chrome.sidebar_seen = Some(self.settings.sidebar);
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
    /// the top edge shows the title bar (see `chrome`); either way the
    /// canvas ignores the finger (`edge_touch`) until it lifts.
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
                && origin.is_some_and(|o| o.y - screen.top() <= SWIPE_EDGE || hidden && from_side(o));
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
        let layout =
            Layout { page_w: self.doc.page_width, page_h: self.doc.page_height, count: self.doc.page_count.max(1) };
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
            .resizable(true)
            .default_size(PAGES_WIDTH)
            .size_range(PAGES_WIDTHS)
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
                cache.begin_frame(scale, ppp);
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
                        let resp = ui.interact(card, ui.id().with(("page_card", i)), Sense::click_and_drag());
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
                            pdf_textures.paint(&clipped, doc, i, rect, false);
                            pictures.paint_page(&clipped, &doc.images, i, rect.min, scale, &[]);
                            render::paint_page_strokes(&clipped, rect.min.to_vec2(), scale, strokes, i, cache, aa);
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
                        let del = ui.interact(del_rect, ui.id().with(("page_del", i)), Sense::click());
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
                            if menus::entry(ui, icon::ARROW_LINE_UP, "Insert page before", "", true) {
                                insert = Some(i);
                                ui.close();
                            }
                            if menus::entry(ui, icon::ARROW_LINE_DOWN, "Insert page after", "", true) {
                                insert = Some(i + 1);
                                ui.close();
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
                            if menus::entry(ui, icon::CLIPBOARD, "Paste page after", "", can_paste) {
                                clip = Some(menus::Command::PastePages(i + 1));
                                ui.close();
                            }
                            ui.separator();
                            if menus::entry(ui, icon::TRASH, &format!("Delete page {}", i + 1), "", true) {
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
                        if theme::add_tile(ui, tile, Id::new("add_page_tile"), "Add page").clicked() {
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
                                    Id::new("page_drag_ghost"),
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
            self.clear_selection();
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
