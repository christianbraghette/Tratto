// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Action-based undo/redo.
//!
//! The history never clones the document. Every stack entry is the *command
//! that reverses the last change*. Applying a command returns its own inverse,
//! which is pushed on the opposite stack:
//!
//! * `AddStroke` / `RemoveStroke`   insert / remove a stroke (moved, not cloned).
//! * `AddText` / `RemoveText` / `ReplaceText`   the same for text boxes; a
//!   replace swaps the whole box (edit, move, restyle) and returns the old one.
//! * `InsertPage` / `RemovePage`    change the page list (strokes and text boxes
//!   of later pages are renumbered). A page written on a PDF keeps its PDF
//!   page through every page action and its inverse.
//! * `MoveStrokes`  moves strokes, possibly to another page.
//! * `MovePage`     reorders pages (their content follows).
//! * `DeletePage` / `RestorePage`   delete a page with its content and bring it
//!   back (the content travels inside the inverse action).
//! * `Batch`        groups actions, e.g. everything erased in one drag.

use std::collections::{HashSet, VecDeque};

use crate::model::{Document, Stroke, TextBox};

const MAX_DEPTH: usize = 1000;

pub enum Action {
    AddStroke {
        index: usize,
        stroke: Stroke,
    },
    RemoveStroke {
        id: u64,
    },
    AddText {
        index: usize,
        text: TextBox,
    },
    RemoveText {
        id: u64,
    },
    /// Replace the text box that has the same id.
    ReplaceText {
        text: TextBox,
    },
    /// `pdf_page`: the PDF page under the new page (notes on a PDF).
    InsertPage {
        index: u32,
        pdf_page: Option<u32>,
    },
    RemovePage {
        index: u32,
    },
    /// Translate the strokes by (dx, dy) in page space and put them on `to_page`.
    MoveStrokes {
        ids: Vec<u64>,
        to_page: u32,
        dx: f32,
        dy: f32,
    },
    /// Move page `from` so that it ends up at index `to`.
    MovePage {
        from: u32,
        to: u32,
    },
    /// Delete a page and its content (the only page is just emptied).
    DeletePage {
        index: u32,
    },
    /// Inverse of `DeletePage`: content with the original positions in the
    /// document lists; `insert_page` tells whether the page itself was removed.
    RestorePage {
        index: u32,
        strokes: Vec<(usize, Stroke)>,
        texts: Vec<(usize, TextBox)>,
        insert_page: bool,
        pdf_page: Option<u32>,
    },
    Batch(Vec<Action>),
}

/// Renumbers the page of every stroke and text box.
fn remap_pages(doc: &mut Document, f: impl Fn(u32) -> u32) {
    for s in doc.strokes.iter_mut() {
        s.page = f(s.page);
    }
    for t in doc.texts.iter_mut() {
        t.page = f(t.page);
    }
}

/// The per-page PDF list of a note written on a PDF.
fn pdf_pages(doc: &mut Document) -> Option<&mut Vec<Option<u32>>> {
    doc.pdf.as_mut().map(|p| &mut p.pages)
}

/// Splits `items` into (kept, removed-with-original-index).
fn split_page<T>(items: Vec<T>, index: u32, page_of: impl Fn(&T) -> u32) -> (Vec<T>, Vec<(usize, T)>) {
    let mut kept = Vec::with_capacity(items.len());
    let mut removed = Vec::new();
    for (i, it) in items.into_iter().enumerate() {
        if page_of(&it) == index {
            removed.push((i, it));
        } else {
            kept.push(it);
        }
    }
    (kept, removed)
}

impl Action {
    /// Executes the action on `doc` and returns the action that undoes it.
    pub fn apply(self, doc: &mut Document) -> Action {
        match self {
            Action::AddStroke { index, stroke } => {
                let id = stroke.id;
                let index = index.min(doc.strokes.len());
                doc.strokes.insert(index, stroke);
                Action::RemoveStroke { id }
            }
            Action::RemoveStroke { id } => match doc.strokes.iter().position(|s| s.id == id) {
                Some(index) => {
                    let stroke = doc.strokes.remove(index);
                    Action::AddStroke { index, stroke }
                }
                None => Action::Batch(Vec::new()), // already gone: no-op
            },
            Action::AddText { index, text } => {
                let id = text.id;
                let index = index.min(doc.texts.len());
                doc.texts.insert(index, text);
                Action::RemoveText { id }
            }
            Action::RemoveText { id } => match doc.texts.iter().position(|t| t.id == id) {
                Some(index) => {
                    let text = doc.texts.remove(index);
                    Action::AddText { index, text }
                }
                None => Action::Batch(Vec::new()),
            },
            Action::ReplaceText { text } => match doc.texts.iter().position(|t| t.id == text.id) {
                Some(i) => {
                    let old = std::mem::replace(&mut doc.texts[i], text);
                    Action::ReplaceText { text: old }
                }
                None => Action::Batch(Vec::new()),
            },
            Action::InsertPage { index, pdf_page } => {
                let index = index.min(doc.page_count);
                remap_pages(doc, |p| if p >= index { p + 1 } else { p });
                if let Some(pages) = pdf_pages(doc) {
                    pages.insert((index as usize).min(pages.len()), pdf_page);
                }
                doc.page_count += 1;
                Action::RemovePage { index }
            }
            Action::RemovePage { index } => {
                if doc.page_count <= 1 || index >= doc.page_count {
                    return Action::Batch(Vec::new());
                }
                // The page is empty when reached through undo/redo or the UI;
                // as a safety net any leftover content goes to the previous page.
                remap_pages(doc, |p| {
                    if p == index {
                        index.saturating_sub(1)
                    } else if p > index {
                        p - 1
                    } else {
                        p
                    }
                });
                let pdf_page = pdf_pages(doc)
                    .and_then(|pages| ((index as usize) < pages.len()).then(|| pages.remove(index as usize)).flatten());
                doc.page_count -= 1;
                Action::InsertPage { index, pdf_page }
            }
            Action::MoveStrokes { ids, to_page, dx, dy } => {
                let set: HashSet<u64> = ids.iter().copied().collect();
                let mut old_page = to_page;
                let mut first = true;
                for s in doc.strokes.iter_mut().filter(|s| set.contains(&s.id)) {
                    if first {
                        old_page = s.page;
                        first = false;
                    }
                    s.translate(dx, dy);
                    s.page = to_page;
                }
                Action::MoveStrokes { ids, to_page: old_page, dx: -dx, dy: -dy }
            }
            Action::MovePage { from, to } => {
                let n = doc.page_count;
                if from >= n || to >= n || from == to {
                    return Action::Batch(Vec::new());
                }
                remap_pages(doc, |p| {
                    if p == from {
                        to
                    } else if from < to && p > from && p <= to {
                        p - 1
                    } else if from > to && p >= to && p < from {
                        p + 1
                    } else {
                        p
                    }
                });
                if let Some(pages) = pdf_pages(doc)
                    && (from as usize) < pages.len()
                    && (to as usize) < pages.len()
                {
                    let moved = pages.remove(from as usize);
                    pages.insert(to as usize, moved);
                }
                Action::MovePage { from: to, to: from }
            }
            Action::DeletePage { index } => {
                if index >= doc.page_count {
                    return Action::Batch(Vec::new());
                }
                let (kept_s, strokes) = split_page(std::mem::take(&mut doc.strokes), index, |s| s.page);
                let (kept_t, texts) = split_page(std::mem::take(&mut doc.texts), index, |t| t.page);
                doc.strokes = kept_s;
                doc.texts = kept_t;
                let remove_page = doc.page_count > 1;
                let mut pdf_page = None;
                if remove_page {
                    remap_pages(doc, |p| if p > index { p - 1 } else { p });
                    if let Some(pages) = pdf_pages(doc)
                        && (index as usize) < pages.len()
                    {
                        pdf_page = pages.remove(index as usize);
                    }
                    doc.page_count -= 1;
                }
                Action::RestorePage { index, strokes, texts, insert_page: remove_page, pdf_page }
            }
            Action::RestorePage { index, strokes, texts, insert_page, pdf_page } => {
                if insert_page {
                    let at = index.min(doc.page_count);
                    remap_pages(doc, |p| if p >= at { p + 1 } else { p });
                    if let Some(pages) = pdf_pages(doc) {
                        pages.insert((at as usize).min(pages.len()), pdf_page);
                    }
                    doc.page_count += 1;
                }
                // Ascending original positions reproduce the old order.
                for (i, mut s) in strokes {
                    s.page = index;
                    let at = i.min(doc.strokes.len());
                    doc.strokes.insert(at, s);
                }
                for (i, mut t) in texts {
                    t.page = index;
                    let at = i.min(doc.texts.len());
                    doc.texts.insert(at, t);
                }
                Action::DeletePage { index }
            }
            Action::Batch(actions) => {
                let mut inverses: Vec<Action> = actions.into_iter().map(|a| a.apply(doc)).collect();
                inverses.reverse(); // undo in the opposite order
                Action::Batch(inverses)
            }
        }
    }
}

#[derive(Default)]
pub struct History {
    undo: VecDeque<Action>,
    redo: Vec<Action>,
    /// Incremented on every document change (exec / undo / redo); autosave watches it.
    version: u64,
}

impl History {
    /// Executes `action` and records its inverse. Clears the redo stack.
    pub fn exec(&mut self, doc: &mut Document, action: Action) {
        let inverse = action.apply(doc);
        self.push_undo(inverse);
    }

    /// Records an already-performed change by its inverse.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn push_undo(&mut self, inverse: Action) {
        self.version += 1;
        self.undo.push_back(inverse);
        if self.undo.len() > MAX_DEPTH {
            self.undo.pop_front();
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        match self.undo.pop_back() {
            Some(a) => {
                let inverse = a.apply(doc);
                self.redo.push(inverse);
                self.version += 1;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        match self.redo.pop() {
            Some(a) => {
                let inverse = a.apply(doc);
                self.undo.push_back(inverse);
                self.version += 1;
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Point, StrokeKind, TextBox};

    fn stroke(id: u64) -> Stroke {
        Stroke::new(
            id,
            0,
            StrokeKind::Pen,
            [0, 0, 0, 255],
            2.0,
            vec![Point::new(0.0, 0.0, 1.0), Point::new(10.0, 0.0, 1.0)],
        )
    }

    fn text(id: u64, page: u32) -> TextBox {
        TextBox {
            id,
            page,
            x: 10.0,
            y: 20.0,
            width: 200.0,
            size: 16.0,
            color: [0, 0, 0],
            text: "hello".into(),
            lines: vec!["hello".into()],
            line_h: 20.0,
        }
    }

    #[test]
    fn text_add_replace_remove_undo() {
        let mut doc = Document::default();
        let mut h = History::default();
        h.exec(&mut doc, Action::AddText { index: 0, text: text(1, 0) });
        let mut edited = text(1, 0);
        edited.text = "changed".into();
        h.exec(&mut doc, Action::ReplaceText { text: edited });
        assert_eq!(doc.texts[0].text, "changed");
        h.undo(&mut doc);
        assert_eq!(doc.texts[0].text, "hello");
        h.undo(&mut doc);
        assert!(doc.texts.is_empty());
        h.redo(&mut doc);
        h.exec(&mut doc, Action::RemoveText { id: 1 });
        assert!(doc.texts.is_empty());
        h.undo(&mut doc);
        assert_eq!(doc.texts.len(), 1);
    }

    #[test]
    fn page_actions_carry_text_boxes() {
        let mut doc = Document::default();
        doc.page_count = 3;
        let mut h = History::default();
        h.exec(&mut doc, Action::AddText { index: 0, text: text(1, 0) });
        h.exec(&mut doc, Action::AddText { index: 1, text: text(2, 2) });
        h.exec(&mut doc, Action::MovePage { from: 0, to: 2 });
        assert_eq!(doc.texts.iter().map(|t| t.page).collect::<Vec<_>>(), vec![2, 1]);
        h.undo(&mut doc);
        h.exec(&mut doc, Action::DeletePage { index: 2 });
        assert_eq!((doc.page_count, doc.texts.len()), (2, 1));
        h.undo(&mut doc);
        assert_eq!((doc.page_count, doc.texts.len()), (3, 2));
        assert_eq!(doc.texts[1].page, 2);
    }

    #[test]
    fn add_undo_redo() {
        let mut doc = Document::default();
        let mut h = History::default();
        h.exec(&mut doc, Action::AddStroke { index: 0, stroke: stroke(1) });
        h.exec(&mut doc, Action::AddStroke { index: 1, stroke: stroke(2) });
        assert_eq!(doc.strokes.len(), 2);
        h.undo(&mut doc);
        assert_eq!(doc.strokes.len(), 1);
        h.redo(&mut doc);
        assert_eq!(doc.strokes.len(), 2);
    }

    #[test]
    fn insert_page_shifts_strokes_and_undoes() {
        let mut doc = Document::default();
        doc.page_count = 2;
        let mut h = History::default();
        let mut s0 = stroke(1);
        s0.page = 0;
        let mut s1 = stroke(2);
        s1.page = 1;
        h.exec(&mut doc, Action::AddStroke { index: 0, stroke: s0 });
        h.exec(&mut doc, Action::AddStroke { index: 1, stroke: s1 });
        h.exec(&mut doc, Action::InsertPage { index: 1, pdf_page: None });
        assert_eq!(doc.page_count, 3);
        assert_eq!(doc.strokes.iter().map(|s| s.page).collect::<Vec<_>>(), vec![0, 2]);
        h.undo(&mut doc);
        assert_eq!(doc.page_count, 2);
        assert_eq!(doc.strokes.iter().map(|s| s.page).collect::<Vec<_>>(), vec![0, 1]);
        h.redo(&mut doc);
        assert_eq!(doc.strokes[1].page, 2);
    }

    #[test]
    fn pdf_pages_follow_every_page_action() {
        use crate::model::PdfBackground;
        let mut doc = Document::default();
        doc.page_count = 3;
        doc.pdf =
            Some(PdfBackground { sizes: vec![[600.0, 800.0]; 3], pages: vec![Some(0), Some(1), Some(2)], file: None });
        let pages = |d: &Document| d.pdf.as_ref().unwrap().pages.clone();
        let mut h = History::default();

        h.exec(&mut doc, Action::InsertPage { index: 1, pdf_page: None });
        assert_eq!(pages(&doc), [Some(0), None, Some(1), Some(2)]);
        h.exec(&mut doc, Action::MovePage { from: 3, to: 0 });
        assert_eq!(pages(&doc), [Some(2), Some(0), None, Some(1)]);
        h.exec(&mut doc, Action::DeletePage { index: 1 });
        assert_eq!(pages(&doc), [Some(2), None, Some(1)]);
        h.exec(&mut doc, Action::RemovePage { index: 0 });
        assert_eq!(pages(&doc), [None, Some(1)]);
        assert_eq!(doc.pdf_page(1), Some(1));

        // Undo everything, then redo it: the PDF pages come back each time.
        for expected in [
            vec![Some(2), None, Some(1)],
            vec![Some(2), Some(0), None, Some(1)],
            vec![Some(0), None, Some(1), Some(2)],
            vec![Some(0), Some(1), Some(2)],
        ] {
            h.undo(&mut doc);
            assert_eq!(pages(&doc), expected);
            assert_eq!(doc.page_count as usize, expected.len());
        }
        for _ in 0..4 {
            h.redo(&mut doc);
        }
        assert_eq!(pages(&doc), [None, Some(1)]);
    }

    #[test]
    fn reorder_pages_moves_strokes_with_them() {
        let mut doc = Document::default();
        doc.page_count = 3;
        let mut h = History::default();
        for (i, page) in [0u32, 1, 2].into_iter().enumerate() {
            let mut s = stroke(i as u64 + 1);
            s.page = page;
            h.exec(&mut doc, Action::AddStroke { index: i, stroke: s });
        }
        h.exec(&mut doc, Action::MovePage { from: 0, to: 2 });
        assert_eq!(doc.strokes.iter().map(|s| s.page).collect::<Vec<_>>(), vec![2, 0, 1]);
        h.undo(&mut doc);
        assert_eq!(doc.strokes.iter().map(|s| s.page).collect::<Vec<_>>(), vec![0, 1, 2]);
        h.exec(&mut doc, Action::MovePage { from: 2, to: 0 });
        assert_eq!(doc.strokes.iter().map(|s| s.page).collect::<Vec<_>>(), vec![1, 2, 0]);
    }

    #[test]
    fn delete_page_and_restore() {
        let mut doc = Document::default();
        doc.page_count = 3;
        let mut h = History::default();
        for (i, page) in [0u32, 1, 1, 2].into_iter().enumerate() {
            let mut s = stroke(i as u64 + 1);
            s.page = page;
            h.exec(&mut doc, Action::AddStroke { index: i, stroke: s });
        }
        h.exec(&mut doc, Action::DeletePage { index: 1 });
        assert_eq!(doc.page_count, 2);
        assert_eq!(doc.strokes.iter().map(|s| (s.id, s.page)).collect::<Vec<_>>(), vec![(1, 0), (4, 1)]);
        h.undo(&mut doc);
        assert_eq!(doc.page_count, 3);
        assert_eq!(
            doc.strokes.iter().map(|s| (s.id, s.page)).collect::<Vec<_>>(),
            vec![(1, 0), (2, 1), (3, 1), (4, 2)]
        );
        h.redo(&mut doc);
        assert_eq!(doc.page_count, 2);
    }

    #[test]
    fn deleting_the_only_page_just_empties_it() {
        let mut doc = Document::default();
        let mut h = History::default();
        h.exec(&mut doc, Action::AddStroke { index: 0, stroke: stroke(1) });
        h.exec(&mut doc, Action::DeletePage { index: 0 });
        assert_eq!((doc.page_count, doc.strokes.len()), (1, 0));
        h.undo(&mut doc);
        assert_eq!((doc.page_count, doc.strokes.len()), (1, 1));
    }

    #[test]
    fn move_strokes_roundtrip() {
        let mut doc = Document::default();
        doc.page_count = 2;
        let mut h = History::default();
        h.exec(&mut doc, Action::AddStroke { index: 0, stroke: stroke(1) });
        h.exec(&mut doc, Action::MoveStrokes { ids: vec![1], to_page: 1, dx: 5.0, dy: 7.0 });
        assert_eq!(doc.strokes[0].page, 1);
        assert_eq!(doc.strokes[0].points[0].x, 5.0);
        assert_eq!(doc.strokes[0].bounds[1], 7.0);
        h.undo(&mut doc);
        assert_eq!(doc.strokes[0].page, 0);
        assert_eq!(doc.strokes[0].points[0].x, 0.0);
        h.redo(&mut doc);
        assert_eq!(doc.strokes[0].points[1].x, 15.0);
    }

    #[test]
    fn batch_erase_restores_order() {
        let mut doc = Document::default();
        let mut h = History::default();
        for i in 0..3 {
            h.exec(&mut doc, Action::AddStroke { index: i as usize, stroke: stroke(i + 1) });
        }
        // Erase strokes 1 and 3 as one batch (as the eraser does in a drag).
        let inv = vec![Action::RemoveStroke { id: 1 }.apply(&mut doc), Action::RemoveStroke { id: 3 }.apply(&mut doc)];
        h.push_undo(Action::Batch(inv.into_iter().rev().collect()));
        assert_eq!(doc.strokes.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
        h.undo(&mut doc);
        assert_eq!(doc.strokes.iter().map(|s| s.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        h.redo(&mut doc);
        assert_eq!(doc.strokes.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
    }
}
