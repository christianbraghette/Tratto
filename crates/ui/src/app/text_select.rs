// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The text of the pages, for selecting, copying and searching it.
//!
//! A page's text is one run of glyphs: the PDF's text (a note written on a
//! PDF, or the reader) and the note's text boxes, in reading order (a text
//! box goes before the PDF's text below its top). Each glyph has its place
//! on the page, except the breaks between words and lines that only exist
//! in the text.
//!
//! Selecting, in the reader and with the pointer tool: mouse and pen drag
//! over the text (a drag elsewhere scrolls, a double click takes a word); a
//! finger scrolls, and a long press on the text takes the word there, then
//! the drag goes on selecting.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2, pos2, vec2};

use tratto_core::model::{Document, PdfBackground, TextBox};
use tratto_core::pdf_pages::TextChar;

use crate::markdown;
use crate::theme::color;
use crate::view::View;

use super::InkApp;

/// How long a finger stays pressed on the text to select (seconds).
pub(super) const LONG_PRESS: f64 = 0.5;
/// A press further than this from the text (document units) misses it.
const SLACK: f32 = 2.0;

/// A character of a page's text and its place on the page (document units,
/// the page's own coordinates). `None`: a break between words or lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Glyph {
    pub ch: char,
    pub rect: Option<Rect>,
}

/// A place in the text: before glyph `index` of `page` (the glyph count:
/// after the last).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Caret {
    pub page: u32,
    pub index: usize,
}

/// Selected text: from where the selection started to where it is now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TextSelection {
    pub anchor: Caret,
    pub focus: Caret,
}

impl TextSelection {
    pub fn new(page: u32, range: Range<usize>) -> Self {
        Self { anchor: Caret { page, index: range.start }, focus: Caret { page, index: range.end } }
    }

    /// Start and end, in reading order.
    pub fn ordered(&self) -> (Caret, Caret) {
        if self.anchor <= self.focus { (self.anchor, self.focus) } else { (self.focus, self.anchor) }
    }

    /// The glyphs selected on `page` (of `len`), if any.
    pub fn on_page(&self, page: u32, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if page < start.page || page > end.page {
            return None;
        }
        let from = if page == start.page { start.index.min(len) } else { 0 };
        let to = if page == end.page { end.index.min(len) } else { len };
        (from < to).then_some(from..to)
    }
}

/// The glyphs of the pages, built when first wanted and kept until the note,
/// the PDF's text read so far or the page size change.
#[derive(Default)]
pub(super) struct TextLayer {
    key: (u64, u64, u32, u32),
    pages: HashMap<u32, Arc<Vec<Glyph>>>,
}

impl TextLayer {
    pub fn clear(&mut self) {
        self.pages.clear();
    }
}

/// A gesture selecting text: the mouse or pen dragging, or a finger pressed.
#[derive(Clone, Copy, Debug)]
pub(super) enum TextGesture {
    /// A finger pressed at `pos` (screen) at `time`; selects once held
    /// still long enough.
    Pressing { pos: Pos2, time: f64 },
    /// Selecting while the pointer moves, until it is released. `still`: a
    /// long press selected a word there; it stays until the pointer moves away.
    /// `grab`: from the pointer to where the selection ends (a handle is
    /// held below the text).
    Dragging { still: Option<Pos2>, grab: Vec2 },
}

/// The radius of a selection handle's drop, and how near a press takes it
/// (points).
const HANDLE: f32 = 11.0;
const HANDLE_HIT: f32 = 26.0;

/// How far a finger may move while pressed and still be a long press (points).
pub(super) const PRESS_MOVE: f32 = 8.0;
/// Near the canvas' top or bottom edge (points), a selection being dragged scrolls.
const SCROLL_EDGE: f32 = 40.0;

/// Search matches and the current match: yellow and orange, like a highlighter.
fn match_color(current: bool) -> Color32 {
    if current {
        Color32::from_rgba_unmultiplied(255, 140, 0, 130)
    } else {
        Color32::from_rgba_unmultiplied(255, 210, 0, 100)
    }
}

impl InkApp {
    /// The glyphs of `page`; `None` while the PDF's text of that page is
    /// being read (asked for here the first time).
    pub(super) fn page_glyphs(&mut self, page: u32) -> Option<Arc<Vec<Glyph>>> {
        let key = (
            self.history.version(),
            self.pdf_nav.texts_version(),
            self.doc.page_width.to_bits(),
            self.doc.page_height.to_bits(),
        );
        if self.text_layer.key != key {
            self.text_layer.pages.clear();
            self.text_layer.key = key;
        }
        if let Some(glyphs) = self.text_layer.pages.get(&page) {
            return Some(glyphs.clone());
        }
        let mut pdf_glyphs = Vec::new();
        if let Some(pdf) = &self.doc.pdf
            && let (Some(file), Some(pdf_page)) = (&pdf.file, self.doc.pdf_page(page))
        {
            let chars = self.pdf_nav.text(file, pdf_page)?;
            let size = (self.doc.page_width, self.doc.page_height);
            let covers: Vec<Rect> = self.doc.images.iter().filter(|i| i.page == page).map(|i| i.rect()).collect();
            pdf_glyphs = placed_pdf_text(pdf, pdf_page, size, &chars, &covers);
        }
        let ctx = &self.egui_ctx;
        let glyphs = Arc::new(page_text(&self.doc.texts, page, pdf_glyphs, |t| box_glyphs(ctx, t)));
        self.text_layer.pages.insert(page, glyphs.clone());
        Some(glyphs)
    }

    /// The page and the place on it (page coordinates) of a screen point;
    /// off the pages, the nearest page.
    pub(super) fn page_point(view: &View, pos: Pos2) -> (u32, Pos2) {
        let c = view.screen_to_content(pos);
        let page = view.layout.page_at(c).unwrap_or_else(|| view.layout.page_index_at_y(c.y));
        let o = view.layout.page_origin(page);
        (page, pos2(c.x - o.x, c.y - o.y))
    }

    /// The glyph under the screen point `pos`, if there is one with a place.
    pub(super) fn glyph_under(&mut self, view: &View, pos: Pos2) -> Option<(u32, usize)> {
        let (page, p) = Self::page_point(view, pos);
        let glyphs = self.page_glyphs(page)?;
        glyph_at(&glyphs, p, SLACK).map(|i| (page, i))
    }

    /// The caret nearest to the screen point `pos`.
    pub(super) fn caret_under(&mut self, view: &View, pos: Pos2) -> Option<Caret> {
        let (page, p) = Self::page_point(view, pos);
        let glyphs = self.page_glyphs(page)?;
        caret_at(&glyphs, p).map(|index| Caret { page, index })
    }

    /// Selects the word at the screen point `pos`; false if no text is there.
    pub(super) fn select_word_at(&mut self, view: &View, pos: Pos2) -> bool {
        let Some((page, i)) = self.glyph_under(view, pos) else { return false };
        let Some(glyphs) = self.page_glyphs(page) else { return false };
        self.text_sel = Some(TextSelection::new(page, word_at(&glyphs, i)));
        true
    }

    /// The screen point `pos` is on a glyph of the selected text.
    fn on_selected_text(&mut self, view: &View, pos: Pos2) -> bool {
        let Some(sel) = self.text_sel else { return false };
        let Some((page, i)) = self.glyph_under(view, pos) else { return false };
        let len = self.page_glyphs(page).map_or(0, |g| g.len());
        sel.on_page(page, len).is_some_and(|r| r.contains(&i))
    }

    /// Selects all the text of `page`.
    pub(super) fn select_all_text(&mut self, page: u32) {
        match self.page_glyphs(page) {
            Some(glyphs) if !glyphs.is_empty() => self.text_sel = Some(TextSelection::new(page, 0..glyphs.len())),
            Some(_) => self.toast("There is no text on this page"),
            None => self.toast("The text of this page is still being read"),
        }
    }

    /// The selected text; pages are separated by a line break.
    pub(super) fn selected_text(&mut self) -> String {
        let Some(sel) = self.text_sel else { return String::new() };
        let (start, end) = sel.ordered();
        let mut out = String::new();
        for page in start.page..=end.page {
            let Some(glyphs) = self.page_glyphs(page) else { continue };
            if let Some(range) = sel.on_page(page, glyphs.len()) {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.extend(glyphs[range].iter().map(|g| g.ch));
            }
        }
        out.trim().to_owned()
    }

    /// Copies the selected text to the clipboard.
    pub(super) fn copy_text(&mut self, ctx: &egui::Context) {
        let text = self.selected_text();
        if text.is_empty() {
            return;
        }
        ctx.copy_text(text);
        self.toast("Copied");
    }

    /// A text selection is being made (the canvas must not scroll with it).
    pub(super) fn selecting_text(&self) -> bool {
        matches!(self.text_gesture, Some(TextGesture::Dragging { .. }))
    }

    /// Selecting text with the pointer on the canvas (`resp`), in the reader
    /// and with the pointer tool. Returns true when this frame's click was
    /// used (it follows no link).
    pub(super) fn text_input(&mut self, ctx: &egui::Context, resp: &egui::Response, view: &View, now: f64) -> bool {
        // The pen selects as the mouse does.
        let mouse = !self.finger(ctx);
        let pen = self.pen_input.down || self.pen_input.windows_touch;
        let (pressed, down, pointer) =
            ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos()));
        let press = ctx.input(|i| i.pointer.press_origin());
        let mut used_click = false;

        match self.text_gesture {
            None => {
                if pressed
                    && resp.is_pointer_button_down_on()
                    && let Some(p) = press
                    && let Some((first, grab)) = self.text_handle_at(view, p)
                    && let Some(sel) = self.text_sel
                {
                    // A handle pressed: its end of the selection follows, the other stays.
                    let (start, end) = sel.ordered();
                    let (anchor, focus) = if first { (end, start) } else { (start, end) };
                    self.text_sel = Some(TextSelection { anchor, focus });
                    self.text_gesture = Some(TextGesture::Dragging { still: None, grab });
                    self.kinetic.stop();
                } else if mouse
                    && resp.drag_started_by(egui::PointerButton::Primary)
                    && let Some(p) = press.filter(|p| self.glyph_under(view, *p).is_some())
                    && let Some(caret) = self.caret_under(view, p)
                {
                    self.text_sel = Some(TextSelection { anchor: caret, focus: caret });
                    self.text_handles = pen;
                    self.text_gesture = Some(TextGesture::Dragging { still: None, grab: Vec2::ZERO });
                } else if !mouse
                    && pressed
                    && resp.is_pointer_button_down_on()
                    && let Some(pos) = press
                {
                    self.text_gesture = Some(TextGesture::Pressing { pos, time: now });
                } else if mouse
                    && resp.double_clicked()
                    && let Some(p) = pointer
                {
                    used_click = self.select_word_at(view, p);
                    self.text_handles = pen;
                } else if resp.clicked() && self.text_sel.take().is_some() {
                    // A click elsewhere only drops the selection.
                    used_click = true;
                }
            }
            Some(TextGesture::Pressing { pos, time }) => {
                let moved = pointer.is_some_and(|p| p.distance(pos) > PRESS_MOVE);
                if !down || moved || self.panning {
                    self.text_gesture = None;
                    if resp.clicked() && self.text_sel.take().is_some() {
                        used_click = true;
                    }
                } else if now - time >= LONG_PRESS {
                    // On the selected text the selection stays, for the
                    // context menu the press opens (see `touch_hold_input`).
                    let selected = !self.on_selected_text(view, pos) && self.select_word_at(view, pos);
                    self.text_handles |= selected;
                    self.text_gesture =
                        selected.then_some(TextGesture::Dragging { still: Some(pos), grab: Vec2::ZERO });
                    self.kinetic.stop();
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(LONG_PRESS - (now - time)));
                }
            }
            Some(TextGesture::Dragging { still, grab }) => {
                if !down {
                    self.text_gesture = None;
                    // The release of a long press is no tap.
                    used_click = true;
                } else if let Some(p) = pointer
                    // The word a long press selected stays while held still on it.
                    && !still.is_some_and(|s| p.distance(s) <= PRESS_MOVE)
                {
                    self.text_gesture = Some(TextGesture::Dragging { still: None, grab });
                    if let (Some(caret), Some(sel)) = (self.caret_under(view, p + grab), self.text_sel.as_mut()) {
                        sel.focus = caret;
                    }
                    self.scroll_at_edge(ctx, view, p);
                }
            }
        }

        // The text cursor over text (the mouse; a finger has none).
        let over_text = mouse && resp.hover_pos().is_some_and(|p| self.glyph_under(view, p).is_some());
        if self.selecting_text() || over_text {
            ctx.set_cursor_icon(egui::CursorIcon::Text);
        }
        used_click
    }

    /// Dragging a selection near the top or bottom of the canvas scrolls.
    fn scroll_at_edge(&mut self, ctx: &egui::Context, view: &View, p: Pos2) {
        let r = view.rect;
        let over = if p.y < r.top() + SCROLL_EDGE {
            p.y - (r.top() + SCROLL_EDGE)
        } else if p.y > r.bottom() - SCROLL_EDGE {
            p.y - (r.bottom() - SCROLL_EDGE)
        } else {
            return;
        };
        let dt = ctx.input(|i| i.stable_dt).min(1.0 / 30.0);
        // Faster the further past the edge: up to about a screen a second.
        self.scroll.y += over / SCROLL_EDGE * r.height() * dt / view.scale;
        self.kinetic.stop();
        ctx.request_repaint();
    }

    /// Marks the selected text and the search matches on the pages shown.
    pub(super) fn paint_text_marks(&mut self, painter: &egui::Painter, view: &View) {
        for page in view.visible_pages() {
            let (first, hits) = self.search.on_page(page);
            let hits: Vec<(Range<usize>, bool)> = hits
                .iter()
                .enumerate()
                .map(|(i, h)| (h.glyphs.clone(), self.search.current == Some(first + i)))
                .collect();
            let selected = self.text_sel.filter(|s| {
                let (start, end) = s.ordered();
                (start.page..=end.page).contains(&page)
            });
            if hits.is_empty() && selected.is_none() {
                continue;
            }
            let Some(glyphs) = self.page_glyphs(page) else { continue };
            let origin = view.page_origin_screen(page);
            let fill = |range: Range<usize>, color: Color32| {
                for r in line_rects(&glyphs, range) {
                    let rect = Rect::from_min_max(
                        origin + r.min.to_vec2() * view.scale,
                        origin + r.max.to_vec2() * view.scale,
                    );
                    painter.rect_filled(rect, 2.0, color);
                }
            };
            for (range, current) in hits {
                fill(range, match_color(current));
            }
            if let Some(range) = selected.and_then(|s| s.on_page(page, glyphs.len())) {
                fill(range, color::accent_alpha(80));
            }
        }
        self.paint_text_handles(painter, view);
    }

    /// The ends of text selected with a finger or the pen on screen, where
    /// it can be selected (see `text_input`): the left edge of its first
    /// line and the right edge of its last, each as the middle of that edge
    /// and the line's height.
    fn selection_ends(&mut self, view: &View) -> Option<[(Pos2, f32); 2]> {
        if !self.text_handles || !self.navigating() || self.lasering() {
            return None;
        }
        let sel = self.text_sel?;
        let (start, end) = sel.ordered();
        let mut line = |page: u32, last: bool| {
            let glyphs = self.page_glyphs(page)?;
            let lines = line_rects(&glyphs, sel.on_page(page, glyphs.len())?);
            let r = if last { lines.last() } else { lines.first() }?;
            let origin = view.page_origin_screen(page);
            let x = if last { r.right() } else { r.left() };
            Some((origin + vec2(x, r.center().y) * view.scale, r.height() * view.scale))
        };
        Some([line(start.page, false)?, line(end.page, true)?])
    }

    /// The handle of the selection near the screen point `pos`: whether it is
    /// the first one, and from `pos` to the end of the selection it moves.
    fn text_handle_at(&mut self, view: &View, pos: Pos2) -> Option<(bool, Vec2)> {
        let ends = self.selection_ends(view)?;
        let (first, (mid, _)) = ends
            .into_iter()
            .enumerate()
            .map(|(i, end)| (i == 0, end))
            .filter(|(_, (mid, h))| handle_center(*mid, *h).distance(pos) <= HANDLE_HIT)
            .min_by(|(_, a), (_, b)| {
                handle_center(a.0, a.1).distance(pos).total_cmp(&handle_center(b.0, b.1).distance(pos))
            })?;
        Some((first, mid - pos))
    }

    /// Two drops hanging below the ends of the selection, to drag them.
    fn paint_text_handles(&mut self, painter: &egui::Painter, view: &View) {
        let Some(ends) = self.selection_ends(view) else { return };
        let ink = color::accent();
        for (mid, h) in ends {
            let bottom = pos2(mid.x, mid.y + h * 0.5);
            // A circle and its tangents up to the end of the line: a drop.
            let c = handle_center(mid, h);
            let up = (c.y - bottom.y).max(HANDLE + 0.1);
            let a = (HANDLE / up).acos();
            let (s, k) = (HANDLE * a.sin(), HANDLE * a.cos());
            painter.add(Shape::convex_polygon(
                vec![bottom, pos2(c.x + s, c.y - k), pos2(c.x - s, c.y - k)],
                ink,
                Stroke::NONE,
            ));
            painter.circle_filled(c, HANDLE, ink);
        }
    }
}

/// The text of a note's pages without the ink: its text boxes and the PDF it
/// is written on. Enough to search a note away from the window (see
/// `search_panel`), with the same glyphs `InkApp::page_glyphs` makes, but
/// for the places of the text boxes' characters.
#[derive(Clone)]
pub(super) struct NoteText {
    pub page_count: u32,
    page_size: (f32, f32),
    texts: Vec<TextBox>,
    /// The pictures' places, by page: they hide the PDF's text under them.
    pictures: Vec<(u32, Rect)>,
    pub pdf: Option<PdfBackground>,
}

impl NoteText {
    pub fn of(doc: &Document) -> Self {
        Self {
            page_count: doc.page_count.max(1),
            page_size: (doc.page_width, doc.page_height),
            texts: doc.texts.clone(),
            pictures: doc.images.iter().map(|i| (i.page, i.rect())).collect(),
            pdf: doc.pdf.clone(),
        }
    }

    /// The PDF page under note page `page`, if any.
    pub fn pdf_page(&self, page: u32) -> Option<u32> {
        self.pdf.as_ref()?.pages.get(page as usize).copied().flatten()
    }

    /// The glyphs of `page`; `chars`: the text of the PDF page under it.
    pub fn page_glyphs(&self, page: u32, chars: &[TextChar]) -> Vec<Glyph> {
        let pdf = match (&self.pdf, self.pdf_page(page)) {
            (Some(pdf), Some(pdf_page)) => {
                let covers: Vec<Rect> = self.pictures.iter().filter(|(p, _)| *p == page).map(|(_, r)| *r).collect();
                placed_pdf_text(pdf, pdf_page, self.page_size, chars, &covers)
            }
            _ => Vec::new(),
        };
        page_text(&self.texts, page, pdf, |t| t.text.chars().map(|ch| Glyph { ch, rect: None }).collect())
    }
}

/// The glyphs of `page`: the PDF's text on it (`pdf`) and its text boxes,
/// each made by `boxed`, in reading order (see `merge`).
fn page_text(texts: &[TextBox], page: u32, pdf: Vec<Glyph>, boxed: impl Fn(&TextBox) -> Vec<Glyph>) -> Vec<Glyph> {
    let mut boxes: Vec<&TextBox> = texts.iter().filter(|t| t.page == page).collect();
    boxes.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    merge(pdf, boxes.into_iter().map(|t| (t.y, boxed(t))).collect())
}

/// The text `chars` of PDF page `pdf_page`, placed where that page sits on
/// a note page of `page_size`, without what the pictures there (`covers`)
/// hide.
fn placed_pdf_text(
    pdf: &PdfBackground,
    pdf_page: u32,
    page_size: (f32, f32),
    chars: &[TextChar],
    covers: &[Rect],
) -> Vec<Glyph> {
    let placed = pdf.placement(pdf_page, page_size.0, page_size.1).map(|placed| place_pdf_text(chars, placed));
    uncovered(placed.unwrap_or_default(), covers)
}

/// `glyphs` without those whose middle is under one of `covers` (pictures
/// are painted over the PDF). The breaks left next to each other become
/// one, a line break if there was one.
fn uncovered(glyphs: Vec<Glyph>, covers: &[Rect]) -> Vec<Glyph> {
    if covers.is_empty() {
        return glyphs;
    }
    let mut out: Vec<Glyph> = Vec::with_capacity(glyphs.len());
    for g in glyphs {
        match g.rect {
            Some(r) if covers.iter().any(|c| c.contains(r.center())) => {}
            Some(_) => out.push(g),
            None => match out.last().map(|last| last.rect.is_some()) {
                Some(true) => out.push(g),
                Some(false) if g.ch == '\n' => *out.last_mut().expect("there is one") = g,
                _ => {}
            },
        }
    }
    out
}

/// The PDF's text placed on a note page: `placed` is where the PDF page
/// sits, `[x, y, width, height]` in page coordinates.
fn place_pdf_text(chars: &[TextChar], placed: [f32; 4]) -> Vec<Glyph> {
    let [x, y, w, h] = placed;
    chars
        .iter()
        .map(|c| Glyph {
            ch: c.ch,
            rect: c.rect.map(|[l, t, r, b]| Rect::from_min_max(pos2(x + l * w, y + t * h), pos2(x + r * w, y + b * h))),
        })
        .collect()
}

/// The glyphs of a text box, laid out as it is painted (`render::paint_texts`):
/// its Markdown's runs, each where its text is in the Markdown, or (not laid
/// out) its lines one under the other, each with the box's font. The
/// characters of its text that are not shown (the Markdown's marks, line
/// breaks, the spaces where a line was wrapped) are kept, without a place.
fn box_glyphs(ctx: &egui::Context, t: &TextBox) -> Vec<Glyph> {
    if let Some(layout) = &t.layout {
        let unplaced = |s: &str| s.chars().map(|ch| Glyph { ch, rect: None }).collect::<Vec<_>>();
        let mut out = Vec::new();
        // Bytes of the text done.
        let mut done = 0;
        for run in &layout.runs {
            let Some(at) = run.source.filter(|at| *at >= done) else { continue };
            let Some(before) = t.text.get(done..at) else { continue };
            out.extend(unplaced(before));
            let font = markdown::font(run.style, run.size);
            let galley = ctx.fonts_mut(|f| f.layout_no_wrap(run.text.clone(), font, Color32::BLACK));
            for g in galley.rows.iter().flat_map(|r| r.row.glyphs.iter()) {
                let left = t.x + run.x + g.pos.x;
                let top = t.y + run.top;
                let rect = Rect::from_min_max(pos2(left, top), pos2(left + g.advance_width, top + run.line_h));
                out.push(Glyph { ch: g.chr, rect: Some(rect) });
            }
            done = at + run.text.len();
        }
        out.extend(unplaced(t.text.get(done..).unwrap_or_default()));
        return out;
    }
    let mut source: Vec<char> = t.text.chars().collect();
    source.reverse();
    let mut out = Vec::new();
    for (i, line) in t.lines.iter().enumerate() {
        let top = t.y + i as f32 * t.line_h;
        let galley = ctx.fonts_mut(|f| f.layout_no_wrap(line.clone(), FontId::proportional(t.size), Color32::BLACK));
        for row in &galley.rows {
            for g in &row.row.glyphs {
                // What the text has before this character, if it is near.
                if let Some(skip) = source.iter().rev().take(8).position(|c| *c == g.chr) {
                    for _ in 0..skip {
                        let ch = source.pop().expect("counted");
                        out.push(Glyph { ch, rect: None });
                    }
                    source.pop();
                }
                let left = t.x + row.pos.x + g.pos.x;
                let rect = Rect::from_min_max(pos2(left, top), pos2(left + g.advance_width, top + t.line_h));
                out.push(Glyph { ch: g.chr, rect: Some(rect) });
            }
        }
    }
    out.extend(source.into_iter().rev().map(|ch| Glyph { ch, rect: None }));
    out
}

/// The PDF's glyphs with the text boxes' (`(top, glyphs)`, by their top)
/// put in reading order: each box before the first of the PDF's glyphs
/// that starts below its top, set off by line breaks.
fn merge(pdf: Vec<Glyph>, boxes: Vec<(f32, Vec<Glyph>)>) -> Vec<Glyph> {
    let brk = Glyph { ch: '\n', rect: None };
    let mut boxes = boxes.into_iter().peekable();
    let mut out = Vec::with_capacity(pdf.len());
    let push_box = |out: &mut Vec<Glyph>, glyphs: Vec<Glyph>| {
        if out.last().is_some_and(|g| g.ch != '\n') {
            out.push(brk);
        }
        out.extend(glyphs);
        out.push(brk);
    };
    for g in pdf {
        while let (Some(r), Some((top, _))) = (g.rect, boxes.peek())
            && r.top() >= *top
        {
            let (_, glyphs) = boxes.next().expect("peeked");
            push_box(&mut out, glyphs);
        }
        out.push(g);
    }
    for (_, glyphs) in boxes {
        push_box(&mut out, glyphs);
    }
    while out.last().is_some_and(|g| g.ch == '\n') {
        out.pop();
    }
    out
}

/// The glyph at `p` (page coordinates), within `slack`.
pub(super) fn glyph_at(glyphs: &[Glyph], p: Pos2, slack: f32) -> Option<usize> {
    glyphs.iter().position(|g| g.rect.is_some_and(|r| r.expand(slack).contains(p)))
}

/// The caret nearest to `p`: before or after the glyph under it, or else
/// the nearest glyph, preferring the same line. `None` without glyphs that
/// have a place.
pub(super) fn caret_at(glyphs: &[Glyph], p: Pos2) -> Option<usize> {
    let distance = |r: Rect| {
        let dy = (r.top() - p.y).max(p.y - r.bottom()).max(0.0);
        let dx = (r.left() - p.x).max(p.x - r.right()).max(0.0);
        dy * 10.0 + dx
    };
    let (i, rect) = glyphs
        .iter()
        .enumerate()
        .filter_map(|(i, g)| g.rect.map(|r| (i, r)))
        .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))?;
    Some(if p.x > rect.center().x { i + 1 } else { i })
}

/// The word around glyph `i`: letters and digits (a single other character
/// on its own).
pub(super) fn word_at(glyphs: &[Glyph], i: usize) -> Range<usize> {
    let word = |g: &Glyph| g.ch.is_alphanumeric() || g.ch == '_' || g.ch == '\'';
    if !glyphs.get(i).is_some_and(word) {
        return i..(i + 1).min(glyphs.len());
    }
    let start = glyphs[..i].iter().rposition(|g| !word(g)).map_or(0, |s| s + 1);
    let end = glyphs[i..].iter().position(|g| !word(g)).map_or(glyphs.len(), |e| i + e);
    start..end
}

/// The rectangles covering `range`, one per line (gaps between the words
/// of a line filled).
/// The middle of the drop of a selection handle, below the end of a line at
/// `mid` (the middle of its edge) and `h` high.
fn handle_center(mid: Pos2, h: f32) -> Pos2 {
    pos2(mid.x, mid.y + h * 0.5 + HANDLE * 1.5)
}

pub(super) fn line_rects(glyphs: &[Glyph], range: Range<usize>) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for r in glyphs[range].iter().filter_map(|g| g.rect) {
        match out.last_mut() {
            Some(last) if same_line(*last, r) && r.left() >= last.left() - r.height() => *last = last.union(r),
            _ => out.push(r),
        }
    }
    // Lines whose glyphs are taller than the line spacing would overlap
    // (and the selection look darker there): they meet halfway instead.
    for i in 1..out.len() {
        let (a, b) = (out[i - 1], out[i]);
        let side_by_side = a.left() < b.right() && b.left() < a.right();
        if side_by_side && b.top() < a.bottom() && b.top() > a.top() && b.bottom() > a.bottom() {
            let mid = (a.bottom() + b.top()) * 0.5;
            out[i - 1].max.y = mid;
            out[i].min.y = mid;
        }
    }
    out
}

/// Two rectangles on the same line: they overlap by half the lower one's height.
fn same_line(a: Rect, b: Rect) -> bool {
    let overlap = a.bottom().min(b.bottom()) - a.top().max(b.top());
    overlap > 0.5 * a.height().min(b.height())
}

/// Not text to read or search: white space, and the private characters of
/// symbol fonts (bullets and the like, unreadable elsewhere).
pub(super) fn is_blank(ch: char) -> bool {
    ch.is_whitespace() || ('\u{E000}'..='\u{F8FF}').contains(&ch)
}

/// The text of `glyphs` for searching: lower case, every run of blanks
/// (`is_blank`) one space; each character with the glyph it comes from.
pub(super) fn searchable(glyphs: &[Glyph]) -> Vec<(char, usize)> {
    let mut out: Vec<(char, usize)> = Vec::with_capacity(glyphs.len());
    for (i, g) in glyphs.iter().enumerate() {
        if is_blank(g.ch) {
            if out.last().is_some_and(|(c, _)| *c != ' ') {
                out.push((' ', i));
            }
        } else {
            out.extend(g.ch.to_lowercase().map(|c| (c, i)));
        }
    }
    out
}

/// `query` as `searchable` makes text: lower case, white space runs as one space, trimmed.
pub(super) fn normalize_query(query: &str) -> Vec<char> {
    query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().chars().collect()
}

/// The glyphs of every match of `query` (from `normalize_query`) in `text`
/// (from `searchable`), in order, not overlapping.
pub(super) fn find_all(text: &[(char, usize)], query: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let mut i = 0;
    while i + query.len() <= text.len() {
        if text[i..i + query.len()].iter().zip(query).all(|((c, _), q)| c == q) {
            out.push(text[i].1..text[i + query.len() - 1].1 + 1);
            i += query.len();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "ab cd" on one line, "ef" on the next; the break between the lines has no place.
    fn sample() -> Vec<Glyph> {
        let at = |x: f32, y: f32| Some(Rect::from_min_max(pos2(x, y), pos2(x + 10.0, y + 12.0)));
        vec![
            Glyph { ch: 'a', rect: at(0.0, 0.0) },
            Glyph { ch: 'b', rect: at(10.0, 0.0) },
            Glyph { ch: ' ', rect: None },
            Glyph { ch: 'c', rect: at(30.0, 0.0) },
            Glyph { ch: 'd', rect: at(40.0, 0.0) },
            Glyph { ch: '\n', rect: None },
            Glyph { ch: 'E', rect: at(0.0, 20.0) },
            Glyph { ch: 'f', rect: at(10.0, 20.0) },
        ]
    }

    #[test]
    fn carets_and_words() {
        let g = sample();
        assert_eq!(glyph_at(&g, pos2(12.0, 5.0), 0.0), Some(1));
        assert_eq!(glyph_at(&g, pos2(22.0, 5.0), 0.0), None, "between the words");
        // Right half of "b": after it; past the end of the line: after "d".
        assert_eq!(caret_at(&g, pos2(18.0, 5.0)), Some(2));
        assert_eq!(caret_at(&g, pos2(90.0, 6.0)), Some(5));
        // Below everything: on the last line.
        assert_eq!(caret_at(&g, pos2(1.0, 80.0)), Some(6));
        assert_eq!(word_at(&g, 4), 3..5);
        assert_eq!(word_at(&g, 2), 2..3);
    }

    #[test]
    fn selections_across_pages() {
        let sel = TextSelection { anchor: Caret { page: 2, index: 3 }, focus: Caret { page: 0, index: 5 } };
        assert_eq!(sel.on_page(0, 8), Some(5..8));
        assert_eq!(sel.on_page(1, 4), Some(0..4));
        assert_eq!(sel.on_page(2, 8), Some(0..3));
        assert_eq!(sel.on_page(3, 8), None);
        assert_eq!(TextSelection::new(0, 2..2).on_page(0, 8), None);
    }

    #[test]
    fn one_rectangle_per_line() {
        let rects = line_rects(&sample(), 1..8);
        assert_eq!(rects.len(), 2);
        assert_eq!((rects[0].left(), rects[0].right()), (10.0, 50.0));
        assert_eq!((rects[1].left(), rects[1].right()), (0.0, 20.0));
    }

    #[test]
    fn search_ignores_case_and_line_breaks() {
        let text = searchable(&sample());
        let s: String = text.iter().map(|(c, _)| *c).collect();
        assert_eq!(s, "ab cd ef");
        assert_eq!(find_all(&text, &normalize_query("CD  e")), vec![3..7]);
        assert_eq!(find_all(&text, &normalize_query("b")).first(), Some(&(1..2)));
        assert!(find_all(&text, &normalize_query("  ")).is_empty());
    }

    #[test]
    fn pictures_hide_the_text_under_them() {
        // A picture over "cd" and the start of the next line.
        let picture = Rect::from_min_max(pos2(25.0, -5.0), pos2(60.0, 30.0));
        let s: String = uncovered(sample(), &[picture]).iter().map(|g| g.ch).collect();
        assert_eq!(s, "ab\nEf");
        // Over everything: nothing is left, not even the breaks.
        let all = Rect::from_min_max(pos2(-5.0, -5.0), pos2(60.0, 40.0));
        assert!(uncovered(sample(), &[all]).is_empty());
    }

    #[test]
    fn text_boxes_go_in_reading_order() {
        let at = |ch: char, y: f32| Glyph { ch, rect: Some(Rect::from_min_max(pos2(0.0, y), pos2(5.0, y + 10.0))) };
        let pdf = vec![at('a', 0.0), at('b', 50.0)];
        let boxed = vec![(30.0, vec![at('X', 30.0)]), (80.0, vec![at('Y', 80.0)])];
        let s: String = merge(pdf, boxed).iter().map(|g| g.ch).collect();
        assert_eq!(s, "a\nX\nb\nY");
    }

    #[test]
    fn note_text_has_each_pages_boxes_top_down() {
        let mut doc = Document::default();
        let tb = |page, y, text: &str| TextBox {
            id: 0,
            page,
            x: 10.0,
            y,
            width: 100.0,
            size: 12.0,
            color: [0, 0, 0],
            text: text.to_owned(),
            lines: Vec::new(),
            line_h: 14.0,
            layout: None,
        };
        doc.page_count = 2;
        doc.texts = vec![tb(0, 90.0, "Second *box*"), tb(1, 0.0, "Other page"), tb(0, 20.0, "First")];
        let note = NoteText::of(&doc);
        let s: String = note.page_glyphs(0, &[]).iter().map(|g| g.ch).collect();
        assert_eq!(s, "First\nSecond *box*");
        let text = searchable(&note.page_glyphs(0, &[]));
        assert_eq!(find_all(&text, &normalize_query("second")).len(), 1);
        assert!(find_all(&text, &normalize_query("other")).is_empty());
    }
}
