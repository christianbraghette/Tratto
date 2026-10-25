// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Copy, cut and paste: what is selected on a note (strokes, text boxes and
//! pictures), whole pages, and notes of the library.
//!
//! Tratto keeps what was copied last ([`Clip`]); a paste puts it on the open
//! note (another one too), or a note into a notebook. Other apps get, on the
//! system clipboard, the text of copied text boxes or a picture of what was
//! copied (see `snapshot`). A paste takes Tratto's own copy while the system
//! clipboard still holds what Tratto put there; otherwise what another app
//! copied since: a picture becomes a picture, text a text box.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Pos2, Rect, Vec2, pos2, vec2};

use tratto_core::history::Action;
use tratto_core::model::{Image, Stroke, TextBox};

use crate::markdown;
use crate::toolbar::Tool;

use super::InkApp;
use super::library_ui::Act;

/// Pasted again where it was copied, a copy is moved this much (document
/// units) further each time, so the copies do not hide each other.
const PASTE_STEP: f32 = 16.0;

/// What was copied or cut last.
pub(crate) enum Clip {
    /// Strokes, text boxes and pictures, in the space of the page they were
    /// copied from.
    Content(Content),
    /// Whole pages, each with what is on it.
    Pages(Vec<PageContent>),
    /// A note of the library; `cut`: moved where it is pasted.
    Note { path: PathBuf, cut: bool },
}

pub(crate) struct Content {
    pub strokes: Vec<Stroke>,
    pub texts: Vec<TextBox>,
    pub images: Vec<Image>,
    /// Around all of it.
    pub bounds: Rect,
    /// The note and the page it was copied from.
    from: (Option<PathBuf>, u32),
    /// Times pasted there.
    pastes: u32,
}

pub(crate) struct PageContent {
    pub strokes: Vec<Stroke>,
    pub texts: Vec<TextBox>,
    pub images: Vec<Image>,
    /// The PDF page under it, by its PDF's key: pasted again only into a
    /// note on the same PDF.
    pdf: Option<(u64, u32)>,
}

/// What Tratto put on the system clipboard for its last copy.
// The web: not made by its stand-ins (the system is not there yet).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum SystemCopy {
    /// Nothing (or it could not).
    #[default]
    Nothing,
    /// Being made (a picture, in the background), by this export.
    Pending(u64),
    Text(String),
    /// A picture of this size (pixels).
    Image([usize; 2]),
}

#[derive(Default)]
pub(crate) struct Clipboard {
    pub clip: Option<Clip>,
    /// Shared with the thread that makes the picture.
    pub system: Arc<Mutex<SystemCopy>>,
    /// The paste key held now was reported by egui, pressed or as a paste
    /// (see `hidden_paste`).
    key_seen: bool,
}

impl Clipboard {
    pub fn has_pages(&self) -> bool {
        matches!(self.clip, Some(Clip::Pages(_)))
    }

    /// A note was copied or cut, to paste into a notebook.
    pub fn note(&self) -> Option<(&PathBuf, bool)> {
        match &self.clip {
            Some(Clip::Note { path, cut }) => Some((path, *cut)),
            _ => None,
        }
    }

    /// What is on the system clipboard now (for a picture being made, see
    /// `snapshot::export`, which says so itself).
    fn put(&self, system: SystemCopy) {
        *self.system.lock().unwrap_or_else(|e| e.into_inner()) = system;
    }

    /// The system clipboard still holds what Tratto put there for its copy
    /// (or holds nothing Tratto could paste instead).
    fn still_ours(&self) -> bool {
        let system = self.system.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if matches!(system, SystemCopy::Pending(_)) {
            return true;
        }
        // The web: the browser's clipboard cannot be read without asking.
        #[cfg(target_arch = "wasm32")]
        {
            true
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Ok(mut clipboard) = arboard::Clipboard::new() else { return true };
            let text = clipboard.get_text().ok().filter(|t| !t.trim().is_empty());
            let image = clipboard.get_image().ok().map(|i| [i.width, i.height]);
            // Files copied in a file manager.
            let files = clipboard.get().file_list().is_ok_and(|f| !f.is_empty());
            let empty = text.is_none() && image.is_none() && !files;
            match system {
                SystemCopy::Text(ours) => text.as_ref().is_some_and(|t| *t == ours) || empty,
                SystemCopy::Image(size) => image == Some(size) || empty,
                _ => empty,
            }
        }
    }
}

/// How far to move `bounds` (page space) by `delta` for it to stay on a
/// `page_w` x `page_h` page (unless it is larger).
pub(super) fn keep_on_page(bounds: Rect, mut delta: Vec2, page_w: f32, page_h: f32) -> Vec2 {
    let nb = bounds.translate(delta);
    if nb.width() <= page_w {
        if nb.min.x < 0.0 {
            delta.x -= nb.min.x;
        } else if nb.max.x > page_w {
            delta.x -= nb.max.x - page_w;
        }
    }
    if nb.height() <= page_h {
        if nb.min.y < 0.0 {
            delta.y -= nb.min.y;
        } else if nb.max.y > page_h {
            delta.y -= nb.max.y - page_h;
        }
    }
    delta
}

impl InkApp {
    /// Something on the note can be copied: selected text, a selection or a
    /// picked picture.
    pub(super) fn can_copy(&self) -> bool {
        self.text_sel.is_some() || self.selection.is_some() || self.picture.is_some()
    }

    /// Something on the note can be cut: a selection or a picked picture.
    pub(super) fn can_cut(&self) -> bool {
        !self.reading() && (self.selection.is_some() || self.picture.is_some())
    }

    /// What is selected on the note (or the picked picture), copied.
    pub(super) fn selected_content(&self) -> Option<Content> {
        let (page, strokes, texts, images) = match (&self.selection, self.picture) {
            (Some(sel), _) => (sel.page, sel.ids.clone(), sel.texts.clone(), sel.images.clone()),
            (None, Some(id)) => {
                let page = self.doc.images.iter().find(|i| i.id == id)?.page;
                (page, HashSet::new(), HashSet::new(), HashSet::from([id]))
            }
            (None, None) => return None,
        };
        let strokes: Vec<Stroke> = self.doc.strokes.iter().filter(|s| strokes.contains(&s.id)).cloned().collect();
        let texts: Vec<TextBox> = self.doc.texts.iter().filter(|t| texts.contains(&t.id)).cloned().collect();
        let images: Vec<Image> = self.doc.images.iter().filter(|i| images.contains(&i.id)).cloned().collect();
        let bounds = content_bounds(&strokes, &texts, &images)?;
        Some(Content { strokes, texts, images, bounds, from: (self.note_path.clone(), page), pastes: 0 })
    }

    /// Copies the selected text, else what is selected on the note.
    pub(super) fn copy(&mut self, ctx: &egui::Context) {
        if self.text_sel.is_some() {
            self.copy_text(ctx);
            // A paste takes that text (as a text box), not an older copy.
            self.clipboard.clip = None;
            return;
        }
        let Some(content) = self.selected_content() else { return };
        self.export_content(ctx, &content);
        self.clipboard.clip = Some(Clip::Content(content));
    }

    /// Copies what is selected on the note and takes it off.
    pub(super) fn cut(&mut self, ctx: &egui::Context) {
        if !self.can_cut() {
            return;
        }
        self.copy(ctx);
        self.delete_selection();
    }

    /// What other apps get for `content`: the text of text boxes alone,
    /// else a picture of it (made in the background).
    fn export_content(&self, ctx: &egui::Context, content: &Content) {
        if content.strokes.is_empty() && content.images.is_empty() {
            let text = content.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("\n\n");
            ctx.copy_text(text.clone());
            self.clipboard.put(SystemCopy::Text(text));
            return;
        }
        crate::snapshot::export(ctx, crate::snapshot::Snapshot::of_content(content), self.clipboard.system.clone());
    }

    /// Pastes at `at` (a page and a point on it), else on the current page:
    /// Tratto's own copy, or what another app copied since. `quiet`: says
    /// nothing when there is nothing to paste (a paste key that may have
    /// been meant for something else).
    pub(super) fn paste(&mut self, at: Option<(u32, Pos2)>, quiet: bool) {
        if self.reading() {
            if !quiet {
                self.toast("Nothing can be pasted on a PDF being read: write on it first");
            }
            return;
        }
        let ours =
            matches!(self.clipboard.clip, Some(Clip::Content(_) | Clip::Pages(_))) && self.clipboard.still_ours();
        if ours {
            match self.clipboard.clip.take() {
                Some(Clip::Content(mut content)) => {
                    self.paste_content(&mut content, at);
                    self.clipboard.clip = Some(Clip::Content(content));
                }
                Some(Clip::Pages(pages)) => {
                    self.clipboard.clip = Some(Clip::Pages(pages));
                    self.paste_pages_here(self.current_page + 1);
                }
                other => self.clipboard.clip = other,
            }
            return;
        }
        // Another app's: a picture, else text.
        match super::pictures::clipboard_picture() {
            Ok(Some(picture)) => return self.insert_pictures(vec![(picture, at, "Cannot paste".to_owned())]),
            Ok(None) => {}
            Err(e) if !quiet => return self.toast(format!("Cannot paste: {e}")),
            Err(_) => return,
        }
        #[cfg(not(target_arch = "wasm32"))]
        let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).ok().filter(|t| !t.trim().is_empty());
        #[cfg(target_arch = "wasm32")]
        let text: Option<String> = None;
        match text {
            Some(text) => self.paste_text(&text, at),
            None if !quiet => self.toast("There is nothing to paste"),
            None => {}
        }
    }

    /// Where something pasted goes when no place is given: the middle of
    /// what is shown of the current page.
    fn paste_place(&self) -> (u32, Pos2) {
        let layout = self.layout();
        let page = self.current_page.min(layout.count - 1);
        let top = layout.page_top(page);
        let (from, to) = self.shown_content;
        let middle = (from.max(top) + to.min(top + layout.page_h)) * 0.5 - top;
        (page, pos2(layout.page_w * 0.5, middle.clamp(0.0, layout.page_h)))
    }

    /// Puts a copy of `content` on the note: centred at `at`, else where it
    /// was copied from on the current page (moved a little when pasted again
    /// over itself). The copy is selected.
    fn paste_content(&mut self, content: &mut Content, at: Option<(u32, Pos2)>) {
        if self.edit.is_some() {
            let ctx = self.egui_ctx.clone();
            self.commit_edit(&ctx);
        }
        let layout = self.layout();
        let (page, delta) = match at {
            Some((page, point)) => (page, point - content.bounds.center()),
            None => {
                let page = self.paste_place().0;
                let here = (self.note_path.clone(), page) == content.from;
                if here {
                    content.pastes += 1;
                }
                let step = if here { content.pastes as f32 * PASTE_STEP } else { 0.0 };
                (page, vec2(step, step))
            }
        };
        let delta = keep_on_page(content.bounds, delta, layout.page_w, layout.page_h);
        let (actions, strokes, texts, images) =
            self.added(page, delta, &content.strokes, &content.texts, &content.images);
        self.history.exec(&mut self.doc, Action::Batch(actions));
        self.select_pasted(page, strokes, texts, images);
    }

    /// The actions that add copies of these strokes, text boxes and pictures
    /// (new ids) to `page`, moved by `delta`; and their ids.
    fn added(
        &mut self,
        page: u32,
        delta: Vec2,
        strokes: &[Stroke],
        texts: &[TextBox],
        images: &[Image],
    ) -> (Vec<Action>, HashSet<u64>, HashSet<u64>, HashSet<u64>) {
        let mut actions = Vec::new();
        let (mut stroke_ids, mut text_ids, mut image_ids) = (HashSet::new(), HashSet::new(), HashSet::new());
        for (n, s) in strokes.iter().enumerate() {
            let mut stroke = s.clone();
            (stroke.id, stroke.page) = (self.doc.alloc_id(), page);
            stroke.translate(delta.x, delta.y);
            stroke_ids.insert(stroke.id);
            actions.push(Action::AddStroke { index: self.doc.strokes.len() + n, stroke });
        }
        for (n, t) in texts.iter().enumerate() {
            let text = TextBox { id: self.doc.alloc_id(), page, x: t.x + delta.x, y: t.y + delta.y, ..t.clone() };
            text_ids.insert(text.id);
            actions.push(Action::AddText { index: self.doc.texts.len() + n, text });
        }
        for (n, i) in images.iter().enumerate() {
            let image = Image { id: self.doc.alloc_id(), page, x: i.x + delta.x, y: i.y + delta.y, ..i.clone() };
            image_ids.insert(image.id);
            actions.push(Action::AddImage { index: self.doc.images.len() + n, image });
        }
        (actions, stroke_ids, text_ids, image_ids)
    }

    /// What was just pasted becomes the selection, with the select tool.
    fn select_pasted(&mut self, page: u32, strokes: HashSet<u64>, texts: HashSet<u64>, images: HashSet<u64>) {
        self.clear_selection();
        self.text_sel = None;
        self.tool = Tool::Select;
        self.saved_tool = None;
        self.selection = self.selection_for(page, strokes, texts, images);
    }

    /// Another app's text, as a text box at `at` (else on the current page),
    /// in the text tool's size and colour.
    fn paste_text(&mut self, text: &str, at: Option<(u32, Pos2)>) {
        if self.edit.is_some() {
            let ctx = self.egui_ctx.clone();
            self.commit_edit(&ctx);
        }
        let layout = self.layout();
        let (page, point) = at.unwrap_or_else(|| self.paste_place());
        let width = (layout.page_w * 0.6).min(360.0);
        let mut tb = TextBox {
            id: self.doc.alloc_id(),
            page,
            x: point.x - width * 0.5,
            y: point.y,
            width,
            size: self.settings.text.width,
            color: self.settings.text.color,
            text: text.replace("\r\n", "\n"),
            lines: Vec::new(),
            line_h: 0.0,
            layout: None,
        };
        markdown::lay_out_box(&self.egui_ctx, &mut tb);
        let delta = keep_on_page(tb.rect(), Vec2::ZERO, layout.page_w, layout.page_h);
        (tb.x, tb.y) = (tb.x + delta.x, tb.y + delta.y);
        let id = tb.id;
        let index = self.doc.texts.len();
        self.history.exec(&mut self.doc, Action::AddText { index, text: tb });
        self.select_pasted(page, HashSet::new(), HashSet::from([id]), HashSet::new());
    }

    // ------------------------------------------------------------- pages

    /// The pages `pages` copied, with what is on them.
    fn pages_content(&self, pages: &[u32]) -> Vec<PageContent> {
        let pdf_key = self.doc.pdf.as_ref().and_then(|p| p.file.as_ref()).map(|f| f.key);
        let on = |page: u32| PageContent {
            strokes: self.doc.strokes.iter().filter(|s| s.page == page).cloned().collect(),
            texts: self.doc.texts.iter().filter(|t| t.page == page).cloned().collect(),
            images: self.doc.images.iter().filter(|i| i.page == page).cloned().collect(),
            pdf: pdf_key.zip(self.doc.pdf_page(page)),
        };
        pages.iter().map(|&p| on(p)).collect()
    }

    /// Copies page `page` with what is on it.
    pub(super) fn copy_page(&mut self, ctx: &egui::Context, page: u32) {
        let pages = self.pages_content(&[page]);
        let size = (self.doc.page_width, self.doc.page_height);
        crate::snapshot::export(
            ctx,
            crate::snapshot::Snapshot::of_page(&pages[0], size),
            self.clipboard.system.clone(),
        );
        self.clipboard.clip = Some(Clip::Pages(pages));
        self.toast(format!("Page {} copied", page + 1));
    }

    /// Copies page `page` and deletes it.
    pub(super) fn cut_page(&mut self, ctx: &egui::Context, page: u32) {
        self.copy_page(ctx, page);
        self.delete_page(page);
    }

    /// Inserts the copied pages, if pages were copied, so that the first
    /// gets `index`.
    pub(super) fn paste_pages_here(&mut self, index: u32) {
        if let Some(Clip::Pages(pages)) = self.clipboard.clip.take() {
            self.paste_pages(&pages, index);
            self.clipboard.clip = Some(Clip::Pages(pages));
        }
    }

    /// Inserts `pages` so that the first gets `index`; one undo step.
    fn paste_pages(&mut self, pages: &[PageContent], index: u32) {
        self.before_page_change();
        let index = index.min(self.doc.page_count);
        let pdf_key = self.doc.pdf.as_ref().and_then(|p| p.file.as_ref()).map(|f| f.key);
        let mut actions = Vec::new();
        let mut lost_pdf = false;
        for (n, p) in pages.iter().enumerate() {
            let page = index + n as u32;
            // The PDF page under it only comes along into a note on that PDF.
            let pdf_page = p.pdf.filter(|(key, _)| Some(*key) == pdf_key).map(|(_, pdf_page)| pdf_page);
            lost_pdf |= p.pdf.is_some() && pdf_page.is_none();
            actions.push(Action::InsertPage { index: page, pdf_page });
            actions.extend(self.added(page, Vec2::ZERO, &p.strokes, &p.texts, &p.images).0);
        }
        self.history.exec(&mut self.doc, Action::Batch(actions));
        let layout = self.layout();
        self.scroll_to = Some(layout.page_stop(index));
        if lost_pdf {
            self.toast("Pasted without the PDF page under it: it belongs to another PDF");
        }
    }

    // ------------------------------------------------------------- notes

    /// Copies (or cuts, to move it) the note `path` of the library.
    pub(super) fn copy_note(&mut self, path: PathBuf, cut: bool) {
        let title = tratto_core::storage::read_meta(&path).map(|m| m.title).unwrap_or_default();
        self.clipboard.clip = Some(Clip::Note { path, cut });
        self.clipboard.put(SystemCopy::Nothing);
        let what = if cut { "Cut" } else { "Copied" };
        self.toast(format!("{what} \u{201c}{title}\u{201d}: paste it into a notebook or section"));
    }

    /// Pastes the copied note into `dir` (a copy of it), or moves the cut
    /// one there.
    pub(super) fn paste_note(&mut self, dir: PathBuf) {
        let Some((path, cut)) = self.clipboard.note().map(|(p, cut)| (p.clone(), cut)) else { return };
        if !tratto_core::fs::is_file(&path) {
            self.clipboard.clip = None;
            self.toast("The note copied is no longer there");
            return;
        }
        if cut {
            // Moved once: the clipboard is empty again, as in a file manager.
            self.clipboard.clip = None;
            self.apply_act(Act::MoveNote(path, dir));
        } else if self.save_if_open(&path) {
            match tratto_core::library::copy_note(&path, &dir) {
                Ok(_) => self.refresh_library(),
                Err(e) => self.toast(format!("Paste failed: {e}")),
            }
        }
    }
}

/// Where the library's menus (free functions) find whether a note can be
/// pasted (see `InkApp::publish_clipboard`).
fn note_pastable_id() -> egui::Id {
    egui::Id::unique("clipboard_has_note")
}

/// A note was copied or cut: the library's menus offer to paste it.
pub(super) fn note_pastable(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(note_pastable_id())).unwrap_or(false)
}

impl InkApp {
    /// A paste key pressed this frame that egui did not report as a paste,
    /// outside Windows (there see `WindowsPen`): without text on the
    /// clipboard egui hides the press, and only its release comes through.
    pub(super) fn hidden_paste(&mut self, ctx: &egui::Context) -> bool {
        if cfg!(windows) {
            return false;
        }
        let seen = &mut self.clipboard.key_seen;
        ctx.input(|i| {
            let mut hidden = false;
            for e in &i.events {
                match e {
                    egui::Event::Paste(_) => *seen = true,
                    egui::Event::Key { key: egui::Key::V | egui::Key::Paste, pressed, .. } => {
                        hidden |= !*pressed && !*seen;
                        *seen = *pressed;
                    }
                    _ => {}
                }
            }
            hidden
        })
    }

    /// Tells the library's menus whether a note can be pasted (each frame).
    pub(super) fn publish_clipboard(&self, ctx: &egui::Context) {
        let note = self.clipboard.note().is_some();
        ctx.data_mut(|d| d.insert_temp(note_pastable_id(), note));
    }
}

/// Around `strokes` (with their width), `texts` and `images`, page space.
pub(super) fn content_bounds(strokes: &[Stroke], texts: &[TextBox], images: &[Image]) -> Option<Rect> {
    let strokes = strokes.iter().map(|s| {
        let pad = s.width * 0.5;
        Rect::from_min_max(pos2(s.bounds[0] - pad, s.bounds[1] - pad), pos2(s.bounds[2] + pad, s.bounds[3] + pad))
    });
    let texts = texts.iter().map(TextBox::rect);
    let images = images.iter().map(Image::rect);
    strokes.chain(texts).chain(images).reduce(|a, b| a.union(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_content_stays_on_the_page() {
        let bounds = Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 200.0));
        // Fits where it lands: unchanged.
        assert_eq!(keep_on_page(bounds, vec2(16.0, 16.0), 800.0, 1000.0), vec2(16.0, 16.0));
        // Past the right and the top: pushed back in.
        assert_eq!(keep_on_page(bounds, vec2(600.0, -150.0), 800.0, 1000.0), vec2(500.0, -100.0));
        // Wider than the page: left where it is across.
        assert_eq!(keep_on_page(bounds, vec2(10.0, 0.0), 150.0, 1000.0).x, 10.0);
    }
}
