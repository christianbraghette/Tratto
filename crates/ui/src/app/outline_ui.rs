// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The outline view of the left panel: the table of contents of a note
//! written on a PDF. A click on an entry goes there; entries with entries
//! under them open and close; the part being read is marked.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use eframe::egui::{self, Align2, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as icon;
use tratto_core::pdf_pages::OutlineEntry;

use super::InkApp;
use crate::pdf_nav::Read;
use crate::theme::{self, color, radius, size, space};

const OUTLINE_WIDTH: f32 = 260.0;
const OUTLINE_WIDTHS: RangeInclusive<f32> = 180.0..=480.0;
/// Indentation per level of the outline.
const INDENT: f32 = 14.0;
/// Width of the open / close arrow in front of an entry.
const CARET_W: f32 = 18.0;

/// What a click in the outline asks for.
enum Clicked {
    /// Go to the entry with this index.
    Go(usize),
    /// Open or close the entry with this index.
    Toggle(usize),
}

impl InkApp {
    pub(super) fn outline_view(&mut self, ui: &mut egui::Ui) {
        let Some(pdf) = self.doc.pdf.as_ref() else { return };
        let file = pdf.file.clone();
        // The note page written on each PDF page (the first, if several are).
        let mut note_page: HashMap<u32, u32> = HashMap::new();
        for (i, p) in pdf.pages.iter().enumerate() {
            if let Some(p) = p {
                note_page.entry(*p).or_insert(i as u32);
            }
        }
        let current = self.current_page;
        let open = &self.outline_open;
        let nav = &mut self.pdf_nav;
        let mut clicked = None;
        let mut target = None;

        egui::Panel::left("left_panel_outline")
            .resizable(true)
            .default_size(OUTLINE_WIDTH)
            .size_range(OUTLINE_WIDTHS)
            .show_separator_line(false)
            .frame(Self::view_frame())
            .show(ui, |ui| {
                Self::view_divider(ui);
                let Some(file) = file else { return };
                let entries = match nav.outline(&file) {
                    Read::Reading => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(theme::muted("Reading the contents..."));
                        });
                        return;
                    }
                    Read::Failed(e) => {
                        ui.label(theme::muted(format!("The contents cannot be read: {e}")));
                        return;
                    }
                    Read::Ready(entries) if entries.is_empty() => {
                        ui.label(theme::muted(
                            "This PDF has no table of contents. The pages panel shows all of its pages.",
                        ));
                        return;
                    }
                    Read::Ready(entries) => entries,
                };
                let page_of = |e: &OutlineEntry| e.target.and_then(|t| note_page.get(&t.page).copied());
                // The part being read: the last entry at or before the current page.
                let reading = entries.iter().rposition(|e| page_of(e).is_some_and(|p| p <= current));
                let shown = shown_entries(entries, open);
                // Marked: that entry, or the open entry it is hidden under.
                let marked = reading.and_then(|r| shown.iter().rev().copied().find(|&i| i <= r));

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for i in shown {
                        let e = &entries[i];
                        let parent = entries.get(i + 1).is_some_and(|next| next.depth > e.depth);
                        let caret = parent.then(|| open.contains(&i));
                        match outline_row(ui, i, e, caret, marked == Some(i), page_of(e)) {
                            Some(Clicked::Go(i)) => {
                                clicked = Some(Clicked::Go(i));
                                target = entries[i].target;
                            }
                            other @ Some(Clicked::Toggle(_)) => clicked = other,
                            None => {}
                        }
                    }
                });
            });

        match clicked {
            Some(Clicked::Go(_)) => {
                if let Some(t) = target {
                    self.go_to_target(t);
                }
            }
            Some(Clicked::Toggle(i)) => {
                if self.outline_open.contains(&i) {
                    self.outline_open.remove(&i);
                } else {
                    self.outline_open.insert(i);
                }
            }
            None => {}
        }
    }
}

/// The entries shown, by index: those whose every parent is open.
fn shown_entries(entries: &[OutlineEntry], open: &std::collections::HashSet<usize>) -> Vec<usize> {
    let mut shown = Vec::new();
    // Entries deeper than this are under a closed one.
    let mut closed_at: Option<u32> = None;
    for (i, e) in entries.iter().enumerate() {
        if let Some(depth) = closed_at {
            if e.depth > depth {
                continue;
            }
            closed_at = None;
        }
        shown.push(i);
        let parent = entries.get(i + 1).is_some_and(|next| next.depth > e.depth);
        if parent && !open.contains(&i) {
            closed_at = Some(e.depth);
        }
    }
    shown
}

/// One entry: indented by its depth, an open / close arrow (`caret`: whether
/// it is open) when it has entries under it, its title and the note page it
/// leads to (`None`: nowhere, shown faint and not clickable). `marked`: the
/// part being read.
fn outline_row(
    ui: &mut Ui,
    index: usize,
    e: &OutlineEntry,
    caret: Option<bool>,
    marked: bool,
    page: Option<u32>,
) -> Option<Clicked> {
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_HEIGHT), Sense::click());
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let indent = rect.left() + space::XS + e.depth as f32 * INDENT;
    let caret_rect = Rect::from_min_size(pos2(indent, rect.top()), vec2(CARET_W, rect.height()));
    let caret_resp = caret.map(|_| ui.interact(caret_rect, ui.id().with(("outline_caret", index)), Sense::click()));
    let hovered = ui.rect_contains_pointer(rect);
    let p = ui.painter();
    if marked {
        p.rect_filled(rect, radius::SMALL, color::accent_alpha(46));
        let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0));
        p.rect_filled(bar, 1.5, color::ACCENT);
    } else if hovered && page.is_some() {
        p.rect_filled(rect, radius::SMALL, color::HOVER);
    }
    let y = rect.center().y;
    if let (Some(is_open), Some(resp)) = (caret, &caret_resp) {
        let glyph = if is_open { icon::CARET_DOWN } else { icon::CARET_RIGHT };
        let fg = if resp.hovered() { color::TEXT_STRONG } else { color::TEXT_MUTED };
        p.text(caret_rect.center(), Align2::CENTER_CENTER, glyph, theme::font(size::CAPTION), fg);
    }
    let text_color = match (page, marked || hovered) {
        (None, _) => color::TEXT_FAINT,
        (Some(_), true) => color::TEXT_STRONG,
        (Some(_), false) => color::TEXT,
    };
    // The page number at the right, the title truncated before it.
    let right = rect.right() - space::S;
    let number_w = match page {
        Some(n) => {
            let g = p.layout_no_wrap(format!("{}", n + 1), theme::font(size::CAPTION), color::TEXT_MUTED);
            let w = g.size().x;
            p.galley(pos2(right - w, y - g.size().y * 0.5), g, color::TEXT_MUTED);
            w + space::S
        }
        None => 0.0,
    };
    let text_left = caret_rect.right() + 2.0;
    let mut job = egui::text::LayoutJob::simple_singleline(e.title.clone(), theme::font(size::BODY), text_color);
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - number_w - text_left).max(0.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let cut_short = galley.elided;
    ui.painter().galley(pos2(text_left, y - galley.size().y * 0.5), galley, text_color);

    if caret_resp.as_ref().is_some_and(|r| r.clicked()) {
        return Some(Clicked::Toggle(index));
    }
    // A title cut short shows in full on hover.
    let row = if cut_short { row.on_hover_text(&e.title) } else { row };
    page?;
    row.on_hover_cursor(egui::CursorIcon::PointingHand).clicked().then_some(Clicked::Go(index))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn entry(depth: u32) -> OutlineEntry {
        OutlineEntry { title: String::new(), depth, target: None }
    }

    #[test]
    fn only_entries_under_open_ones_are_shown() {
        // 0
        //   1
        //     2
        //   3
        // 4
        //   5
        let entries: Vec<_> = [0, 1, 2, 1, 0, 1].into_iter().map(entry).collect();
        assert_eq!(shown_entries(&entries, &HashSet::new()), [0, 4]);
        assert_eq!(shown_entries(&entries, &HashSet::from([0])), [0, 1, 3, 4]);
        assert_eq!(shown_entries(&entries, &HashSet::from([0, 1, 4])), [0, 1, 2, 3, 4, 5]);
        // A child open under a closed parent stays hidden.
        assert_eq!(shown_entries(&entries, &HashSet::from([1])), [0, 4]);
    }
}
