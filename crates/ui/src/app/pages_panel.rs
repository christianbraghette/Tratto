// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The left panel: its strip of view buttons and the pages view (previews to
//! jump to, reorder and delete pages).

use eframe::egui::{self, Align2, Color32, Id, Rect, Sense, pos2, vec2};

use tratto_core::history::Action;

use crate::render;
use crate::settings::LeftPanel;
use crate::theme::{self, color, radius, space};
use crate::toolbar;
use crate::view::{Layout, MARGIN};

use super::InkApp;
use super::menus;

/// Default width of the pages view (points); every view of the left panel
/// remembers its own width.
const PAGES_WIDTH: f32 = 164.0;
/// The narrowest width still fits the "Add page" tile's label.
const PAGES_WIDTHS: std::ops::RangeInclusive<f32> = 150.0..=360.0;
/// Page previews never get wider than this, however wide the panel.
const THUMB_MAX_W: f32 = 200.0;

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

    /// Collapsible left panel: previews of all pages. Click = jump to the page,
    /// drag = reorder, "x" (on hover) = delete. Everything is undoable.
    /// The left panel: a strip with the pages and settings buttons (the two
    /// left notches joined), and the shown view to its right. Each view is
    /// its own resizable panel, so each keeps its own width.
    pub(super) fn left_panel(&mut self, ui: &mut egui::Ui) {
        let view = self.settings.left_panel;
        if view != LeftPanel::Pages {
            self.page_drag = None;
        }
        // Only a note written on a PDF has an outline.
        let on_pdf = self.doc.pdf.is_some();
        let wrong_view = view == LeftPanel::Outline && !on_pdf || view == LeftPanel::Settings && self.reading();
        if wrong_view {
            self.settings.left_panel = LeftPanel::Pages;
            return self.left_panel(ui);
        }
        let note_settings = !self.reading();
        if view == LeftPanel::Closed {
            return;
        }
        let (top, bottom) = (toolbar::notch_margin(true), toolbar::notch_margin(false));
        let mut clicked = None;
        egui::Panel::left("left_panel_strip")
            .resizable(false)
            .exact_size(toolbar::STRIP_WIDTH)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(color::CHROME).inner_margin(egui::Margin {
                left: top.left,
                right: top.right,
                top: top.top,
                bottom: bottom.bottom,
            }))
            .show(ui, |ui| {
                toolbar::notch_buttons_style(ui);
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if note_settings && toolbar::settings_button(ui, view == LeftPanel::Settings) {
                        clicked = Some(LeftPanel::Settings);
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        if toolbar::pages_button(ui, view == LeftPanel::Pages) {
                            clicked = Some(LeftPanel::Pages);
                        }
                        if on_pdf && toolbar::outline_button(ui, view == LeftPanel::Outline) {
                            clicked = Some(LeftPanel::Outline);
                        }
                        if toolbar::search_button(ui, view == LeftPanel::Search) {
                            clicked = Some(LeftPanel::Search);
                        }
                    });
                });
            });
        match view {
            LeftPanel::Pages => self.pages_view(ui),
            LeftPanel::Settings => self.note_settings_view(ui),
            LeftPanel::Outline => self.outline_view(ui),
            LeftPanel::Search => self.search_view(ui),
            LeftPanel::Closed => {}
        }
        if let Some(v) = clicked {
            self.settings.left_panel.toggle(v);
        }
    }

    /// Frame of a left panel view: the strip's surface, content starting
    /// level with the top button.
    pub(super) fn view_frame() -> egui::Frame {
        let top = toolbar::notch_margin(true);
        egui::Frame::NONE.fill(color::CHROME).inner_margin(egui::Margin {
            left: space::S as i8,
            right: space::S as i8,
            top: top.top,
            bottom: space::S as i8,
        })
    }

    /// Hairline between the button strip and a left panel view.
    pub(super) fn view_divider(ui: &egui::Ui) {
        let r = ui.max_rect();
        let x = r.left() - space::S + 0.5;
        let y = (r.top() + space::XS)..=(r.bottom() - space::XS);
        ui.painter().vline(x, y, egui::Stroke::new(1.0, color::BORDER));
    }

    fn pages_view(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        let layout =
            Layout { page_w: self.doc.page_width, page_h: self.doc.page_height, count: self.doc.page_count.max(1) };
        let ppp = ctx.pixels_per_point();
        let current = self.current_page;
        let blue = color::ACCENT;
        // A PDF in the reader: its pages are looked at, not changed.
        let editable = !self.reading();

        // Disjoint field borrows, released again after `show`.
        let doc = &self.doc;
        let strokes = &self.doc.strokes;
        let texts = &self.doc.texts;
        let cache = &mut self.thumb_cache;
        let pdf_textures = &mut self.pdf_textures;
        let drag = &mut self.page_drag;

        // Scroll the list to the current page when it changes.
        let follow = self.panel_page != Some(current) && drag.is_none();
        self.panel_page = Some(current);
        let mut jump: Option<u32> = None;
        let mut delete: Option<u32> = None;
        let mut add = false;
        let mut insert: Option<u32> = None;
        let mut reorder: Option<(u32, u32)> = None;

        // The concave joins with the title bar and the bottom edge are drawn
        // by `toolbar::show_corners`.
        egui::Panel::left("left_panel_pages")
            .resizable(true)
            .default_size(PAGES_WIDTH)
            .size_range(PAGES_WIDTHS)
            .show_separator_line(false)
            .frame(Self::view_frame())
            .show(ui, |ui| {
                Self::view_divider(ui);

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
                                p.rect_filled(card, radius::MEDIUM, color::HOVER.gamma_multiply(0.6));
                            }
                            p.rect_filled(rect.translate(vec2(0.0, 2.0)), 2.0, Color32::from_black_alpha(90));
                            p.rect_filled(rect, 0.0, render::PAGE_COLOR);
                            let clipped = ui.painter_at(rect);
                            pdf_textures.paint(&clipped, doc, i, rect, false);
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
                                    egui::Stroke::new(1.0, color::BORDER),
                                    egui::StrokeKind::Middle,
                                );
                            }
                            // Page number; the current one bright and semi-bold.
                            let center = pos2(card.center().x, rect.bottom() + PAD + NUMBER_H * 0.5);
                            let text = format!("{}", i + 1);
                            let (font, ink) = if i == current {
                                (theme::font_strong(theme::size::CAPTION), color::TEXT_STRONG)
                            } else {
                                (theme::font(theme::size::CAPTION), color::TEXT_MUTED)
                            };
                            p.text(center, Align2::CENTER_CENTER, text, font, ink);
                        }

                        // Delete button, registered after the card so it wins hit-testing.
                        let del_rect = Rect::from_center_size(rect.right_top() + vec2(-13.0, 13.0), vec2(22.0, 22.0));
                        let del = ui.interact(del_rect, ui.id().with(("page_del", i)), Sense::click());
                        if editable && ui.rect_contains_pointer(card) && drag.is_none() {
                            let p = ui.painter();
                            let fill = if del.hovered() { color::DANGER } else { color::CHROME };
                            p.circle(del_rect.center(), 10.0, fill, egui::Stroke::new(1.0, color::BORDER));
                            let a = 3.5;
                            let x = egui::Stroke::new(1.6, if del.hovered() { color::ON_ACCENT } else { color::TEXT });
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
    }
}
