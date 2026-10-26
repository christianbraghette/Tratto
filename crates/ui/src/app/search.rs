// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Finding text in the open note or PDF, while the sidebar's search view
//! (`search_panel`) is shown, whatever it searches: the matches are marked
//! on the pages, the current one more strongly, and `F3` / `Shift+F3` go
//! from one to the next. A result of another note opens that note at its
//! match (`Jump`).
//!
//! The pages are searched a few at a time, frame after frame, as their text
//! is read (`text_select`), so a long book never holds up the window. Any
//! change to the query or the note starts the search over.

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Id, Ui};

use super::InkApp;
use super::text_select::{Glyph, find_all, is_blank, line_rects, normalize_query, searchable};
use crate::settings::Sidebar;

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
    pub snippet: (String, String, String),
}

/// A match to show once the note at `path` is open and searched that far:
/// the `nth` match on `page` (a result of another note).
pub(super) struct Jump {
    pub path: PathBuf,
    pub page: u32,
    pub nth: usize,
}

#[derive(Default)]
pub(super) struct Search {
    /// What is typed in the search view's field (every scope searches it).
    pub query: String,
    /// The query and note (history version, page count) the matches are for.
    searched: (Vec<char>, u64, u32),
    /// The next page to search.
    next_page: u32,
    pub hits: Vec<Hit>,
    /// The match shown (see `show_hit`).
    pub current: Option<usize>,
    /// Scroll the list to the current match next frame.
    pub focus_row: bool,
    /// Kept by `reset`: it waits for another note to open.
    pub jump: Option<Jump>,
}

impl Search {
    /// Starts over (another note was opened, or the search view closed).
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

    /// Something is searched for.
    pub fn typed(&self) -> bool {
        !self.searched.0.is_empty()
    }

    /// The note is still being searched.
    pub fn searching(&self) -> bool {
        self.typed() && self.next_page < self.searched.2
    }
}

impl InkApp {
    /// Searches on, a few pages per frame, while the search view is shown.
    /// Call once per frame.
    pub(super) fn search_step(&mut self, ctx: &egui::Context) {
        if self.settings.sidebar != Sidebar::Search {
            self.search.reset();
            return;
        }
        let wanted = (normalize_query(&self.search.query), self.history.version(), self.doc.page_count.max(1));
        if wanted != self.search.searched {
            let s = &mut self.search;
            s.searched = wanted;
            s.next_page = 0;
            s.hits.clear();
            s.current = None;
        }
        self.take_jump();
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

    /// Shows the match a result of another note asked for, once that note
    /// is open and searched past its page.
    fn take_jump(&mut self) {
        let s = &self.search;
        let Some(jump) = &s.jump else { return };
        let searched_past = s.next_page > jump.page || s.next_page >= s.searched.2;
        if self.note_path.as_ref() != Some(&jump.path) || !searched_past {
            return;
        }
        let (first, hits) = s.on_page(jump.page);
        let (page, i) = (jump.page, first + jump.nth.min(hits.len().saturating_sub(1)));
        let found = !hits.is_empty();
        self.search.jump = None;
        if found {
            self.show_hit(i, true);
        } else {
            // The note changed since it was searched: its page, at least.
            self.scroll_to = Some(self.layout().page_stop(page));
        }
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
        self.show_hit(next, false);
    }

    /// Makes match `i` the current one and scrolls to it if it is not in
    /// view, or anyway (`always`: what is in view may be another note's).
    pub(super) fn show_hit(&mut self, i: usize, always: bool) {
        let Some(hit) = self.search.hits.get(i) else { return };
        self.search.current = Some(i);
        self.search.focus_row = true;
        let (page, range) = (hit.page, hit.glyphs.clone());
        let Some(glyphs) = self.page_glyphs(page) else { return };
        let Some(rect) = line_rects(&glyphs, range).into_iter().reduce(|a, b| a.union(b)) else { return };
        let layout = self.layout();
        let rect = rect.translate(layout.page_origin(page).to_vec2());
        let shown = self.shown_content;
        // An infinite page also scrolls across: a match may be far to the side.
        let aside = layout.canvas && (rect.left() < shown.left() || rect.right() > shown.right());
        if always || aside || rect.top() < shown.top() || rect.bottom() > shown.bottom() {
            // A third of the way down the view (and across, on an infinite page).
            let y = rect.top() - shown.height() / 3.0;
            self.scroll_to = Some(y.max(0.0));
            if layout.canvas {
                self.scroll_to_x = Some((rect.left() - shown.width() / 3.0).max(0.0));
            }
        }
    }
}

/// The text around a match, blanks (`is_blank`) as single spaces.
pub(super) fn snippet(glyphs: &[Glyph], range: &Range<usize>) -> (String, String, String) {
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

/// Selects all of `text`, the text of the field `id`.
pub(super) fn select_all(ui: &Ui, id: Id, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
        let all =
            egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()));
        state.cursor.set_char_range(Some(all));
        state.store(ui.ctx(), id);
    }
}
