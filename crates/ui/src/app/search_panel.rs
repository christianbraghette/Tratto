// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The search view of the sidebar, like VS Code's: one field, and radio
//! buttons for where to search: the open **note** (`Ctrl+F`), its
//! **section** or its whole **notebook** (`Ctrl+Shift+F`). A quick note
//! has no section and searches the quick notes as its notebook; a PDF being
//! read has only itself.
//!
//! While the view is shown, the open note's matches are marked on its pages
//! whatever the scope (`search`). The note scope lists them by page; the
//! others list the matches of every note searched, by note, and clicking
//! one opens its note (the open one is saved first) at that match.
//!
//! The section or notebook is searched on a thread of its own, one note
//! after the other, the text of their PDF one page at a time
//! (`pdf_pages::text`, so the PDF pages being rendered never wait long);
//! the results come in as each note is done. The open note is searched as
//! it is now, saved or not. Typing starts over after a pause (Enter at
//! once), and so do another scope or opening a note elsewhere; the search
//! under way stops.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Key, Sense, Ui, text::LayoutJob};
use egui_phosphor::regular as icon;

use tratto_core::library::NoteInfo;
use tratto_core::pdf_pages::{self, TextChar};
use tratto_core::storage;

use super::search::{Jump, select_all, snippet};
use super::text_select::{NoteText, find_all, normalize_query, searchable};
use super::{InkApp, Screen};
use crate::settings::Sidebar;
use crate::theme::{self, color, radius, size, space};

const SEARCH_WIDTH: f32 = 280.0;
/// Results kept at most, of all the notes.
const MAX_HITS: usize = 5000;
/// The pause in typing after which a section or notebook is searched.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Results of a note are indented under its title.
const INDENT: f32 = 26.0;

/// Where the search view searches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Scope {
    #[default]
    Note,
    Section,
    Notebook,
}

impl Scope {
    const ALL: [Scope; 3] = [Scope::Note, Scope::Section, Scope::Notebook];

    fn label(self) -> &'static str {
        match self {
            Scope::Note => "Note",
            Scope::Section => "Section",
            Scope::Notebook => "Notebook",
        }
    }
}

/// A match in a note: the `nth` on `page`.
struct NoteHit {
    page: u32,
    nth: usize,
    snippet: (String, String, String),
}

/// The matches in one note.
struct NoteResult {
    path: PathBuf,
    title: String,
    hits: Vec<NoteHit>,
}

/// What the search thread sends.
enum Found {
    /// A note was searched (sent for each, for the progress), with its
    /// matches if it has any.
    Note(Option<NoteResult>),
    /// Stopped at `MAX_HITS`.
    Limit,
}

/// The search under way; dropping it stops it.
struct Running {
    stop: Arc<AtomicBool>,
    found: Receiver<Found>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(super) struct SearchPanel {
    pub scope: Scope,
    /// The query, scope and folder the results are for (no folder: nothing
    /// searched but the note).
    searched: (Vec<char>, Option<(Scope, PathBuf)>),
    /// A query typed since, and when: it is searched after a pause.
    typed: Option<(Vec<char>, Instant)>,
    /// Search again even if nothing changed (the refresh button).
    again: bool,
    results: Vec<NoteResult>,
    /// Notes searched so far, of `total`.
    done: usize,
    total: usize,
    limited: bool,
    running: Option<Running>,
    /// Notes whose results are folded.
    folded: HashSet<PathBuf>,
    /// The result last opened: its note and its index there.
    current: Option<(PathBuf, usize)>,
    /// Focus the field next frame; `select_query`: and select what is in it.
    pub focus: bool,
    pub select_query: bool,
}

impl InkApp {
    /// Opens the search view on `scope` (the note if that one has nothing
    /// to search) and puts the cursor in its field, its text selected; the
    /// selected text, if short, becomes the query.
    pub(super) fn find(&mut self, scope: Scope) {
        let selected = self.selected_text();
        if !selected.is_empty() && selected.chars().count() <= 100 && !selected.contains('\n') {
            self.search.query = selected;
        }
        let available = scope == Scope::Note || self.scope_folder(scope).is_some();
        let panel = &mut self.search_panel;
        panel.scope = if available { scope } else { Scope::Note };
        panel.focus = true;
        panel.select_query = true;
        self.settings.sidebar = Sidebar::Search;
    }

    /// The folder `scope` searches and its name; `None` for the note, and
    /// where there is no such folder (a quick note's section, a PDF being
    /// read).
    fn scope_folder(&self, scope: Scope) -> Option<(PathBuf, String)> {
        let path = self.note_path.as_ref()?;
        let notebook = self.library.notebooks.iter().find(|nb| path.starts_with(&nb.path));
        match (scope, notebook) {
            (Scope::Note, _) | (Scope::Section, None) => None,
            (Scope::Notebook, Some(nb)) => Some((nb.path.clone(), nb.name.clone())),
            (Scope::Section, Some(nb)) => {
                let dir = path.parent()?;
                match nb.sections.iter().find(|s| s.path == dir) {
                    Some(s) => Some((s.path.clone(), s.name.clone())),
                    // Directly in the notebook.
                    None => Some((nb.path.clone(), "General".to_owned())),
                }
            }
            (Scope::Notebook, None) => {
                let root = self.library_root();
                (path.parent() == Some(root.as_path())).then(|| (root, "Quick notes".to_owned()))
            }
        }
    }

    /// The notes `scope` searches in `dir` (from `scope_folder`), in the
    /// notebook view's order, with their titles.
    fn folder_notes(&self, scope: Scope, dir: &PathBuf) -> Vec<(PathBuf, String)> {
        let info = |n: &NoteInfo| (n.path.clone(), n.meta.title.clone());
        for nb in &self.library.notebooks {
            if &nb.path == dir {
                return match scope {
                    Scope::Section => nb.loose.iter().map(info).collect(),
                    _ => nb.loose.iter().chain(nb.sections.iter().flat_map(|s| &s.notes)).map(info).collect(),
                };
            }
            if let Some(s) = nb.sections.iter().find(|s| &s.path == dir) {
                return s.notes.iter().map(info).collect();
            }
        }
        self.library.quick.iter().map(info).collect()
    }

    /// Starts searching the section or notebook when the query, the scope
    /// or the folder changed, and takes in what was found since the last
    /// frame. `now`: without waiting for a pause in typing.
    fn search_panel_step(&mut self, ctx: &egui::Context, folder: Option<&PathBuf>, now: bool) {
        let query = normalize_query(&self.search.query);
        let panel = &mut self.search_panel;
        let target = folder.map(|dir| (panel.scope, dir.clone()));
        let elsewhere = panel.searched.1 != target;
        if elsewhere || query != panel.searched.0 || panel.again {
            if panel.typed.as_ref().is_none_or(|(q, _)| *q != query) {
                panel.typed = Some((query.clone(), Instant::now()));
            }
            let waited = panel.typed.as_ref().map_or(Duration::ZERO, |(_, at)| at.elapsed());
            if now || elsewhere || panel.again || waited >= DEBOUNCE {
                self.start_search(ctx, query, target);
            } else {
                ctx.request_repaint_after(DEBOUNCE - waited);
            }
        }

        let panel = &mut self.search_panel;
        let Some(running) = &panel.running else { return };
        loop {
            match running.found.try_recv() {
                Ok(Found::Note(result)) => {
                    panel.done += 1;
                    panel.results.extend(result);
                }
                Ok(Found::Limit) => panel.limited = true,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    panel.running = None;
                    break;
                }
            }
        }
    }

    /// Searches the notes of `target` (scope, folder) for `query`; either
    /// may be empty: no results.
    fn start_search(&mut self, ctx: &egui::Context, query: Vec<char>, target: Option<(Scope, PathBuf)>) {
        let notes = match &target {
            Some((scope, dir)) if !query.is_empty() => self.folder_notes(*scope, dir),
            _ => Vec::new(),
        };
        let open = self.note_path.clone().map(|path| (path, NoteText::of(&self.doc)));
        let panel = &mut self.search_panel;
        panel.searched = (query.clone(), target);
        panel.typed = None;
        panel.again = false;
        panel.results.clear();
        panel.current = None;
        panel.limited = false;
        panel.done = 0;
        panel.total = notes.len();
        // Stops the search under way.
        panel.running = None;
        if notes.is_empty() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (found, receiver) = mpsc::channel();
        let (thread_stop, ctx) = (stop.clone(), ctx.clone());
        let spawned = thread::Builder::new()
            .name("notebook search".into())
            .spawn(move || search_notes(notes, open, &query, &thread_stop, &found, &ctx));
        if spawned.is_ok() {
            panel.running = Some(Running { stop, found: receiver });
        }
    }

    /// Opens the note of result `i` of `path` at its match.
    fn open_result(&mut self, ctx: &egui::Context, path: PathBuf, i: usize) {
        let panel = &mut self.search_panel;
        let Some(hit) = panel.results.iter().find(|r| r.path == path).and_then(|r| r.hits.get(i)) else { return };
        self.search.jump = Some(Jump { path: path.clone(), page: hit.page, nth: hit.nth });
        panel.current = Some((path.clone(), i));
        if self.note_path.as_ref() != Some(&path) {
            // Saves the open note first; stays put if that fails.
            self.close_note(ctx);
            if self.screen == Screen::Library {
                self.open_note(&path);
            }
        }
    }

    /// The search view of the sidebar.
    pub(super) fn search_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let folders = [None, self.scope_folder(Scope::Section), self.scope_folder(Scope::Notebook)];
        if self.search_panel.scope != Scope::Note && folders[self.search_panel.scope as usize].is_none() {
            self.search_panel.scope = Scope::Note;
        }
        let scope = self.search_panel.scope;
        let mut now = false;
        let mut step = None;
        let mut go_hit = None;
        let mut go_result = None;
        let side = self.settings.sidebar_side;
        let (search, panel, note_path) = (&mut self.search, &mut self.search_panel, &self.note_path);
        Self::side_panel(side, "sidebar_search")
            .resizable(false)
            .exact_size(SEARCH_WIDTH)
            .show_separator_line(false)
            .frame(Self::view_frame(true))
            .show(ui, |ui| {
                Self::view_divider(ui, true, side);
                let field = egui::TextEdit::singleline(&mut search.query)
                    .hint_text(format!("{}  Search", icon::MAGNIFYING_GLASS))
                    .margin(egui::Margin::symmetric(space::S as i8, space::XS as i8 + 2))
                    .desired_width(f32::INFINITY);
                let resp = ui.add(field);
                if std::mem::take(&mut panel.focus) {
                    resp.request_focus();
                }
                if std::mem::take(&mut panel.select_query) {
                    select_all(ui, resp.id, &search.query);
                }
                // Enter: the next match in the note (Shift: the previous), or
                // search the section or notebook now; staying in the field.
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    match scope {
                        Scope::Note => step = Some(!ui.input(|i| i.modifiers.shift)),
                        _ => now = true,
                    }
                    panel.focus = true;
                }

                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    for s in Scope::ALL {
                        let enabled = s == Scope::Note || folders[s as usize].is_some();
                        let radio = ui.add_enabled(enabled, egui::RadioButton::new(panel.scope == s, s.label()));
                        let why = match s {
                            Scope::Section if note_path.is_some() => "A quick note is in no section",
                            _ => "A PDF being read is in no notebook",
                        };
                        if radio.on_disabled_hover_text(why).clicked() {
                            panel.scope = s;
                        }
                    }
                });
                ui.add_space(space::XS);

                let folder = folders[scope as usize].as_ref();
                ui.horizontal(|ui| {
                    let (status, searching) = match scope {
                        Scope::Note => note_status(search),
                        _ => (folder_status(panel), panel.running.is_some()),
                    };
                    ui.label(theme::muted(status));
                    if searching {
                        ui.spinner();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        theme::ghost_buttons(ui);
                        if scope == Scope::Note {
                            let any = !search.hits.is_empty();
                            let next = ui.add_enabled(any, egui::Button::new(icon::CARET_DOWN));
                            if next.on_hover_text("Next (Enter, F3)").clicked() {
                                step = Some(true);
                            }
                            let previous = ui.add_enabled(any, egui::Button::new(icon::CARET_UP));
                            if previous.on_hover_text("Previous (Shift+Enter, Shift+F3)").clicked() {
                                step = Some(false);
                            }
                        } else {
                            let again = ui.add_enabled(search.typed(), egui::Button::new(icon::ARROW_CLOCKWISE));
                            if again.on_hover_text("Search again").clicked() {
                                panel.again = true;
                            }
                            let any = !panel.results.is_empty();
                            let fold = ui.add_enabled(any, egui::Button::new(icon::ARROWS_IN_LINE_VERTICAL));
                            if fold.on_hover_text("Fold all").clicked() {
                                panel.folded = panel.results.iter().map(|r| r.path.clone()).collect();
                            }
                        }
                    });
                });
                if let Some((_, name)) = folder {
                    theme::caption(ui, &format!("In {name}"));
                }
                ui.add_space(space::XS);

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    match scope {
                        Scope::Note => go_hit = note_list(ui, search),
                        _ => go_result = folder_list(ui, panel, note_path.as_ref()),
                    }
                });
            });
        self.search_panel_step(&ctx, folders[scope as usize].as_ref().map(|(dir, _)| dir), now);
        if let Some(forward) = step {
            self.find_next(forward);
        }
        if let Some(i) = go_hit {
            self.show_hit(i, false);
        }
        if let Some((path, i)) = go_result {
            self.open_result(&ctx, path, i);
        }
    }
}

/// "3 of 12" for the note, and whether it is still being searched.
fn note_status(search: &super::search::Search) -> (String, bool) {
    let searching = search.searching();
    let status = match (search.hits.len(), search.current) {
        _ if !search.typed() => String::new(),
        (0, _) if searching => String::new(),
        (0, _) => "No results".to_owned(),
        (n, Some(i)) => format!("{} of {n}", i + 1),
        (n, None) => format!("{n} {}", if n == 1 { "match" } else { "matches" }),
    };
    (status, searching)
}

/// "12 results in 3 notes", with the progress while searching.
fn folder_status(panel: &SearchPanel) -> String {
    if panel.searched.0.is_empty() {
        return String::new();
    }
    let hits: usize = panel.results.iter().map(|r| r.hits.len()).sum();
    let notes = panel.results.len();
    let mut s = match hits {
        0 if panel.running.is_some() => String::new(),
        0 => "No results".to_owned(),
        _ => format!(
            "{hits} {} in {notes} {}",
            if hits == 1 { "result" } else { "results" },
            if notes == 1 { "note" } else { "notes" }
        ),
    };
    if panel.limited {
        s.push_str(" (stopped there)");
    } else if panel.running.is_some() {
        s.push_str(&format!("{}{} of {} notes", if s.is_empty() { "" } else { ", " }, panel.done, panel.total));
    }
    s
}

/// The open note's matches, by page; returns the one clicked.
fn note_list(ui: &mut Ui, search: &mut super::search::Search) -> Option<usize> {
    let mut clicked = None;
    let mut last_page = None;
    for (i, hit) in search.hits.iter().enumerate() {
        if last_page != Some(hit.page) {
            last_page = Some(hit.page);
            ui.add_space(space::S);
            theme::caption(ui, &format!("Page {}", hit.page + 1));
            ui.add_space(space::XS);
        }
        let current = search.current == Some(i);
        let resp = hit_row(ui, &hit.snippet, None, 0.0, current);
        if current && std::mem::take(&mut search.focus_row) {
            resp.scroll_to_me(None);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// The matches of the section or notebook, by note (a note's header folds
/// them); returns the one clicked.
fn folder_list(ui: &mut Ui, panel: &mut SearchPanel, open: Option<&PathBuf>) -> Option<(PathBuf, usize)> {
    let mut clicked = None;
    for result in &panel.results {
        let folded = panel.folded.contains(&result.path);
        if note_row(ui, result, folded, open == Some(&result.path)).clicked() && !panel.folded.remove(&result.path) {
            panel.folded.insert(result.path.clone());
        }
        if folded {
            continue;
        }
        for (i, hit) in result.hits.iter().enumerate() {
            let current = panel.current.as_ref().is_some_and(|(p, c)| *p == result.path && *c == i);
            if hit_row(ui, &hit.snippet, Some(hit.page), INDENT, current).clicked() {
                clicked = Some((result.path.clone(), i));
            }
        }
    }
    clicked
}

/// The header of a note's results: fold caret, title, how many; the open
/// note's title stands out.
fn note_row(ui: &mut Ui, result: &NoteResult, folded: bool, open_note: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), theme::ROW_HEIGHT), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, radius::SMALL, color::hover());
    }
    let y = rect.center().y;
    let caret = if folded { icon::CARET_RIGHT } else { icon::CARET_DOWN };
    let font = theme::font(size::BODY);
    p.text(egui::pos2(rect.left() + 8.0, y), Align2::CENTER_CENTER, caret, font.clone(), color::text_faint());
    p.text(
        egui::pos2(rect.left() + INDENT, y),
        Align2::CENTER_CENTER,
        icon::NOTE_BLANK,
        font.clone(),
        color::text_muted(),
    );
    let count = result.hits.len().to_string();
    let count_at = egui::pos2(rect.right() - space::S, y);
    let count_rect = p.text(count_at, Align2::RIGHT_CENTER, count, theme::font(size::CAPTION), color::text_faint());
    let (title_font, title_color) =
        if open_note { (theme::font_strong(size::BODY), color::text_strong()) } else { (font, color::text()) };
    let left = rect.left() + INDENT + 14.0;
    let mut job = LayoutJob::simple_singleline(result.title.clone(), title_font, title_color);
    job.wrap = egui::text::TextWrapping::truncate_at_width((count_rect.left() - space::S - left).max(0.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    p.galley(egui::pos2(left, y - galley.size().y / 2.0), galley, title_color);
    resp.on_hover_text(&result.title)
}

/// A match in a list: its page (if given), its context and the match itself
/// in bold, `indent` from the left.
fn hit_row(
    ui: &mut Ui,
    snippet: &(String, String, String),
    page: Option<u32>,
    indent: f32,
    current: bool,
) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let strong = egui::FontId::new(font.size, theme::strong_family());
    let fmt = |font: &egui::FontId, color| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = LayoutJob::default();
    if let Some(page) = page {
        job.append(&format!("p. {}  ", page + 1), 0.0, fmt(&theme::font(size::CAPTION), color::text_faint()));
    }
    let (before, matched, after) = snippet;
    job.append(before, 0.0, fmt(&font, color::text_muted()));
    job.append(matched, 0.0, fmt(&strong, color::text_strong()));
    job.append(after, 0.0, fmt(&font, color::text_muted()));
    job.wrap.max_rows = 2;
    let pad = indent.max(space::S);
    job.wrap.max_width = ui.available_width() - pad - space::S;
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let size = galley.size() + egui::vec2(pad + space::S, 2.0 * space::XS);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), size.y), Sense::click());
    let fill = if current { Some(color::accent_alpha(48)) } else { resp.hovered().then_some(color::accent_alpha(20)) };
    if let Some(fill) = fill {
        ui.painter().rect_filled(rect, radius::SMALL, fill);
    }
    ui.painter().galley(rect.min + egui::vec2(pad, space::XS), galley, color::text_muted());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Searches `notes` (path, title) for `query`, one after the other, sending
/// each one's matches, until `stop`. `open`: the open note, searched as it
/// is (not as saved).
fn search_notes(
    notes: Vec<(PathBuf, String)>,
    open: Option<(PathBuf, NoteText)>,
    query: &[char],
    stop: &AtomicBool,
    found: &Sender<Found>,
    ctx: &egui::Context,
) {
    let mut count = 0;
    for (path, title) in notes {
        let loaded;
        let text = match &open {
            Some((open_path, text)) if *open_path == path => Some(text),
            // Unreadable notes have nothing to find.
            _ => {
                loaded = storage::load_note(&path).ok().map(|(_, doc)| NoteText::of(&doc));
                loaded.as_ref()
            }
        };
        let mut hits = Vec::new();
        if let Some(text) = text {
            for page in 0..text.page_count {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let glyphs = text.page_glyphs(page, |pdf_page| pdf_text(text, pdf_page));
                for (nth, range) in find_all(&searchable(&glyphs), query).into_iter().enumerate() {
                    hits.push(NoteHit { page, nth, snippet: snippet(&glyphs, &range) });
                }
            }
        }
        count += hits.len();
        let result = (!hits.is_empty()).then_some(NoteResult { path, title, hits });
        if found.send(Found::Note(result)).is_err() {
            return;
        }
        ctx.request_repaint();
        if count >= MAX_HITS {
            let _ = found.send(Found::Limit);
            return;
        }
    }
}

/// The text of PDF page `pdf_page`, waiting for the PDF worker (none if it
/// cannot be read).
fn pdf_text(text: &NoteText, pdf_page: u32) -> Vec<TextChar> {
    let Some(file) = text.pdf.as_ref() else { return Vec::new() };
    let (reply, answer) = mpsc::channel();
    pdf_pages::text(file, pdf_page, reply, Arc::new(|| {}));
    answer.recv().ok().and_then(|t| t.chars.ok()).unwrap_or_default()
}
