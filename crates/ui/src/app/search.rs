// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Searching the text of the note or PDF (`Ctrl+F`): the search view of the
//! left panel, with the matches listed by page; the matches are marked on
//! the pages, the current one more strongly.
//!
//! The pages are searched a few at a time, frame after frame, as their text
//! is read (`text_select`), so a long book never holds up the window. Any
//! change to the query or the note starts the search over.

use std::ops::{Range, RangeInclusive};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Sense, Ui, text::LayoutJob};
use egui_phosphor::regular as icon;

use super::InkApp;
use super::text_select::{Glyph, find_all, is_blank, line_rects, normalize_query, searchable};
use crate::theme::{self, color, radius, space};

const SEARCH_WIDTH: f32 = 280.0;
const SEARCH_WIDTHS: RangeInclusive<f32> = 200.0..=480.0;
/// Matches kept at most.
const MAX_HITS: usize = 5000;
/// Pages whose text is asked for ahead of the search.
const READ_AHEAD: u32 = 16;
/// Time spent searching per frame at most.
const FRAME_BUDGET: Duration = Duration::from_millis(4);
/// Characters of context shown on each side of a match.
const CONTEXT: usize = 32;

/// A match: glyphs of a page (see `InkApp::page_glyphs`).
pub(super) struct Hit {
    pub page: u32,
    pub glyphs: Range<usize>,
    /// The text before, of and after the match, for the list.
    snippet: (String, String, String),
}

#[derive(Default)]
pub(super) struct Search {
    /// What is typed in the field.
    pub query: String,
    /// The query and note (history version, page count) the matches are for.
    searched: (Vec<char>, u64, u32),
    /// The next page to search.
    next_page: u32,
    pub hits: Vec<Hit>,
    /// The match shown (see `go_to_hit`).
    pub current: Option<usize>,
    /// Focus the field next frame; `select_query`: and select what is in it.
    pub focus: bool,
    select_query: bool,
    /// Scroll the list to the current match next frame.
    focus_row: bool,
}

impl Search {
    /// Starts over (another note was opened).
    pub fn reset(&mut self) {
        self.searched = (Vec::new(), u64::MAX, 0);
        self.hits.clear();
        self.current = None;
    }

    /// The matches on `page`, and the index of the first (the hits are in
    /// page order).
    pub fn on_page(&self, page: u32) -> (usize, &[Hit]) {
        let start = self.hits.partition_point(|h| h.page < page);
        let end = self.hits.partition_point(|h| h.page <= page);
        (start, &self.hits[start..end])
    }
}

impl InkApp {
    /// Searches on, a few pages per frame. Call once per frame.
    pub(super) fn search_step(&mut self, ctx: &egui::Context) {
        let wanted = (normalize_query(&self.search.query), self.history.version(), self.doc.page_count.max(1));
        if wanted != self.search.searched {
            let s = &mut self.search;
            s.searched = wanted;
            s.next_page = 0;
            s.hits.clear();
            s.current = None;
        }
        let (query, _, count) = self.search.searched.clone();
        if query.is_empty() {
            return;
        }
        let started = Instant::now();
        while self.search.next_page < count && self.search.hits.len() < MAX_HITS {
            let page = self.search.next_page;
            let Some(glyphs) = self.page_glyphs(page) else {
                // Still being read (the answer repaints): ask for the next pages too.
                for ahead in page + 1..(page + READ_AHEAD).min(count) {
                    self.page_glyphs(ahead);
                }
                return;
            };
            let text = searchable(&glyphs);
            for range in find_all(&text, &query) {
                let snippet = snippet(&glyphs, &range);
                self.search.hits.push(Hit { page, glyphs: range, snippet });
            }
            self.search.next_page += 1;
            if started.elapsed() > FRAME_BUDGET {
                ctx.request_repaint();
                return;
            }
        }
    }

    /// Opens the search view and puts the cursor in its field; the selected
    /// text, if short, becomes the query. Closes the view if already open.
    pub(super) fn find(&mut self) {
        use crate::settings::LeftPanel;
        if self.settings.left_panel == LeftPanel::Search {
            self.settings.left_panel = LeftPanel::Closed;
            return;
        }
        let selected = self.selected_text();
        if !selected.is_empty() && selected.chars().count() <= 100 && !selected.contains('\n') {
            self.search.query = selected;
        }
        self.settings.left_panel = LeftPanel::Search;
        self.search.focus = true;
        self.search.select_query = true;
    }

    /// Goes to the next match (`forward`) or the previous one, from the
    /// current match or else from the page shown.
    pub(super) fn find_next(&mut self, forward: bool) {
        let n = self.search.hits.len();
        if n == 0 {
            return;
        }
        let next = match self.search.current {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if forward => self.search.hits.iter().position(|h| h.page >= self.current_page).unwrap_or(0),
            None => self.search.hits.iter().rposition(|h| h.page <= self.current_page).unwrap_or(n - 1),
        };
        self.go_to_hit(next);
    }

    /// Makes match `i` the current one and scrolls to it if it is not in view.
    fn go_to_hit(&mut self, i: usize) {
        let Some(hit) = self.search.hits.get(i) else { return };
        self.search.current = Some(i);
        let (page, range) = (hit.page, hit.glyphs.clone());
        let Some(glyphs) = self.page_glyphs(page) else { return };
        let Some(rect) = line_rects(&glyphs, range).into_iter().reduce(|a, b| a.union(b)) else { return };
        let layout = self.layout();
        let (top, bottom) = (layout.page_top(page) + rect.top(), layout.page_top(page) + rect.bottom());
        let (shown_top, shown_bottom) = self.shown_content;
        if top < shown_top || bottom > shown_bottom {
            // A third of the way down the view.
            let y = top - (shown_bottom - shown_top) / 3.0;
            self.scroll_to = Some(y.max(0.0));
        }
    }

    /// The search view of the left panel.
    pub(super) fn search_view(&mut self, ui: &mut egui::Ui) {
        let mut go = None;
        let mut step = None;
        egui::Panel::left("left_panel_search")
            .resizable(true)
            .default_size(SEARCH_WIDTH)
            .size_range(SEARCH_WIDTHS)
            .show_separator_line(false)
            .frame(Self::view_frame(true))
            .show(ui, |ui| {
                Self::view_divider(ui, true);
                let s = &mut self.search;
                let field = egui::TextEdit::singleline(&mut s.query)
                    .hint_text(format!("{}  Search the text", icon::MAGNIFYING_GLASS))
                    .margin(egui::Margin::symmetric(space::S as i8, space::XS as i8 + 2))
                    .desired_width(f32::INFINITY);
                let resp = ui.add(field);
                if std::mem::take(&mut s.focus) {
                    resp.request_focus();
                }
                if std::mem::take(&mut s.select_query)
                    && let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id)
                {
                    let all = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(s.query.chars().count()),
                    );
                    state.cursor.set_char_range(Some(all));
                    state.store(ui.ctx(), resp.id);
                }
                // Enter: the next match (Shift: the previous), staying in the field.
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    step = Some(!ui.input(|i| i.modifiers.shift));
                    s.focus = true;
                }

                ui.add_space(space::S);
                let searching = s.next_page < s.searched.2 && !s.searched.0.is_empty();
                ui.horizontal(|ui| {
                    let status = match (s.hits.len(), searching, s.current) {
                        _ if s.searched.0.is_empty() => String::new(),
                        (0, false, _) => "No matches".to_owned(),
                        (n, _, Some(i)) => format!("{} of {n}", i + 1),
                        (n, _, None) => format!("{n} {}", if n == 1 { "match" } else { "matches" }),
                    };
                    ui.label(theme::muted(status));
                    if searching {
                        ui.spinner();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        theme::ghost_buttons(ui);
                        let any = !s.hits.is_empty();
                        if ui.add_enabled(any, egui::Button::new(icon::CARET_DOWN)).on_hover_text("Next (F3)").clicked()
                        {
                            step = Some(true);
                        }
                        if ui
                            .add_enabled(any, egui::Button::new(icon::CARET_UP))
                            .on_hover_text("Previous (Shift+F3)")
                            .clicked()
                        {
                            step = Some(false);
                        }
                    });
                });
                ui.add_space(space::XS);

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut last_page = None;
                    for (i, hit) in s.hits.iter().enumerate() {
                        if last_page != Some(hit.page) {
                            last_page = Some(hit.page);
                            ui.add_space(space::S);
                            theme::caption(ui, &format!("Page {}", hit.page + 1));
                            ui.add_space(space::XS);
                        }
                        let current = s.current == Some(i);
                        let resp = hit_row(ui, hit, current);
                        if current && std::mem::take(&mut s.focus_row) {
                            resp.scroll_to_me(None);
                        }
                        if resp.clicked() {
                            go = Some(i);
                        }
                    }
                });
            });
        if let Some(forward) = step {
            self.find_next(forward);
            self.search.focus_row = true;
        }
        if let Some(i) = go {
            self.go_to_hit(i);
        }
    }
}

/// A match in the list: its context, the match itself in bold.
fn hit_row(ui: &mut Ui, hit: &Hit, current: bool) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let strong = egui::FontId::new(font.size, theme::strong_family());
    let fmt = |font: &egui::FontId, color| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = LayoutJob::default();
    let (before, matched, after) = &hit.snippet;
    job.append(before, 0.0, fmt(&font, color::text_muted()));
    job.append(matched, 0.0, fmt(&strong, color::text_strong()));
    job.append(after, 0.0, fmt(&font, color::text_muted()));
    job.wrap.max_rows = 2;
    job.wrap.max_width = ui.available_width() - 2.0 * space::S;
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let size = galley.size() + egui::vec2(2.0 * space::S, 2.0 * space::XS);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), size.y), Sense::click());
    let fill = if current { Some(color::accent_alpha(48)) } else { resp.hovered().then_some(color::accent_alpha(20)) };
    if let Some(fill) = fill {
        ui.painter().rect_filled(rect, radius::SMALL, fill);
    }
    ui.painter().galley(rect.min + egui::vec2(space::S, space::XS), galley, color::text_muted());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The text around a match, blanks (`is_blank`) as single spaces.
fn snippet(glyphs: &[Glyph], range: &Range<usize>) -> (String, String, String) {
    let text = |r: Range<usize>| {
        let s: String = glyphs[r].iter().map(|g| if is_blank(g.ch) { ' ' } else { g.ch }).collect();
        s.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
    };
    let from = range.start.saturating_sub(CONTEXT);
    let to = (range.end + CONTEXT).min(glyphs.len());
    let mut before = text(from..range.start);
    let mut after = text(range.end..to);
    if from > 0 {
        before.insert(0, '\u{2026}');
    }
    if !before.is_empty() && range.start > 0 && is_blank(glyphs[range.start - 1].ch) {
        before.push(' ');
    }
    if !after.is_empty() && glyphs.get(range.end).is_some_and(|g| is_blank(g.ch)) {
        after.insert(0, ' ');
    }
    if to < glyphs.len() {
        after.push('\u{2026}');
    }
    (before, text(range.clone()), after)
}
