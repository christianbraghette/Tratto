// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! What changed in a document between two saves: the records of a note's
//! journal (see `storage`).
//!
//! A `Delta` is computed by comparing two versions of the document, not by
//! recording edits, so every change is caught however it was made (undo
//! history, eraser, page settings...). Its values are absolute (the new
//! stroke, the new page count), so replaying it never depends on how an
//! edit is implemented.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::{Document, Image, ImageData, PageStyle, Stroke, TextBox};

/// The changes from one version of a document to the next.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Delta {
    layout: Option<Layout>,
    /// The PDF page under each note page (notes on a PDF).
    pdf_pages: Option<Vec<Option<u32>>>,
    strokes: ListDelta<Stroke>,
    texts: ListDelta<TextBox>,
    /// Pictures by their place; their files are in `image_files`, once.
    images: ListDelta<ImagePlace>,
    /// The files of the pictures that were not in the document before.
    image_files: Vec<Arc<ImageData>>,
    /// The room between the PDF's pages on an infinite page.
    pdf_gap: Option<f32>,
}

/// A record as formats 7 and 8 saved it, before the room between the PDF's
/// pages (see `storage`).
#[derive(Deserialize)]
pub(crate) struct DeltaV8 {
    layout: Option<Layout>,
    pdf_pages: Option<Vec<Option<u32>>>,
    strokes: ListDelta<Stroke>,
    texts: ListDelta<TextBox>,
    images: ListDelta<ImagePlace>,
    image_files: Vec<Arc<ImageData>>,
}

impl From<DeltaV8> for Delta {
    fn from(d: DeltaV8) -> Self {
        let DeltaV8 { layout, pdf_pages, strokes, texts, images, image_files } = d;
        Delta { layout, pdf_pages, strokes, texts, images, image_files, pdf_gap: None }
    }
}

/// A record as format 6 saved it, before pictures (see `storage`).
#[derive(Deserialize)]
pub(crate) struct DeltaV6 {
    layout: Option<Layout>,
    pdf_pages: Option<Vec<Option<u32>>>,
    strokes: ListDelta<Stroke>,
    texts: ListDelta<TextBox>,
}

impl From<DeltaV6> for Delta {
    fn from(d: DeltaV6) -> Self {
        let DeltaV6 { layout, pdf_pages, strokes, texts } = d;
        let (images, image_files) = (ListDelta::default(), Vec::new());
        DeltaV8 { layout, pdf_pages, strokes, texts, images, image_files }.into()
    }
}

/// A picture as the journal sees it: its place, and its file by its key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ImagePlace {
    id: u64,
    page: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    key: u64,
}

impl ImagePlace {
    fn of(images: &[Image]) -> Vec<ImagePlace> {
        images
            .iter()
            .map(|i| ImagePlace {
                id: i.id,
                page: i.page,
                x: i.x,
                y: i.y,
                width: i.width,
                height: i.height,
                key: i.data.key,
            })
            .collect()
    }
}

/// Page size, paper, page count and the id counter.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Layout {
    page_width: f32,
    page_height: f32,
    page_count: u32,
    page: PageStyle,
    next_id: u64,
}

impl Layout {
    fn of(doc: &Document) -> Self {
        Self {
            page_width: doc.page_width,
            page_height: doc.page_height,
            page_count: doc.page_count,
            page: doc.page,
            next_id: doc.next_id(),
        }
    }

    fn apply(self, doc: &mut Document) {
        doc.page_width = self.page_width;
        doc.page_height = self.page_height;
        doc.page_count = self.page_count;
        doc.page = self.page;
        doc.set_next_id(self.next_id);
    }
}

/// The changes to the strokes, the text boxes or the pictures, by id.
#[derive(Debug, Serialize, Deserialize)]
struct ListDelta<T> {
    removed: Vec<u64>,
    /// New items, and changed ones (they replace the item with their id).
    changed: Vec<T>,
    /// Items that only moved to another page (a page was inserted, deleted
    /// or moved): `(id, page)`.
    pages: Vec<(u64, u32)>,
    /// Ids of the whole list in order, when that is not the old order with
    /// the new items appended (e.g. an undo put a stroke back in the middle).
    order: Option<Vec<u64>>,
}

impl<T> Default for ListDelta<T> {
    fn default() -> Self {
        Self { removed: Vec::new(), changed: Vec::new(), pages: Vec::new(), order: None }
    }
}

impl<T> ListDelta<T> {
    fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.changed.is_empty() && self.pages.is_empty() && self.order.is_none()
    }
}

/// A stroke, a text box or a picture, as the journal sees it.
trait Item: Clone {
    fn id(&self) -> u64;
    fn page(&self) -> u32;
    fn set_page(&mut self, page: u32);
    /// Equal apart from the page.
    fn same_content(&self, other: &Self) -> bool;
}

impl Item for Stroke {
    fn id(&self) -> u64 {
        self.id
    }

    fn page(&self) -> u32 {
        self.page
    }

    fn set_page(&mut self, page: u32) {
        self.page = page;
    }

    fn same_content(&self, other: &Self) -> bool {
        // Spelled out so that a new field cannot be forgotten here. `bounds`
        // follows from the points, `rev` is not saved.
        let Stroke { id, page: _, kind, color, width, points, bounds: _, rev: _ } = self;
        *id == other.id
            && *kind == other.kind
            && *color == other.color
            && *width == other.width
            // Points shared with the other version are unchanged: not compared.
            && (Arc::ptr_eq(points, &other.points) || *points == other.points)
    }
}

impl Item for TextBox {
    fn id(&self) -> u64 {
        self.id
    }

    fn page(&self) -> u32 {
        self.page
    }

    fn set_page(&mut self, page: u32) {
        self.page = page;
    }

    fn same_content(&self, other: &Self) -> bool {
        // The layout is made from the rest, and not saved.
        let TextBox { id, page: _, x, y, width, size, color, text, lines, line_h, layout: _ } = self;
        *id == other.id
            && *x == other.x
            && *y == other.y
            && *width == other.width
            && *size == other.size
            && *color == other.color
            && *text == other.text
            && *lines == other.lines
            && *line_h == other.line_h
    }
}

impl Item for ImagePlace {
    fn id(&self) -> u64 {
        self.id
    }

    fn page(&self) -> u32 {
        self.page
    }

    fn set_page(&mut self, page: u32) {
        self.page = page;
    }

    fn same_content(&self, other: &Self) -> bool {
        Self { page: other.page, ..self.clone() } == *other
    }
}

impl Delta {
    pub(crate) fn is_empty(&self) -> bool {
        self.layout.is_none()
            && self.pdf_pages.is_none()
            && self.strokes.is_empty()
            && self.texts.is_empty()
            && self.images.is_empty()
            && self.image_files.is_empty()
            && self.pdf_gap.is_none()
    }

    pub(crate) fn apply(self, doc: &mut Document) {
        if let Some(layout) = self.layout {
            layout.apply(doc);
        }
        if let (Some(pages), Some(pdf)) = (self.pdf_pages, doc.pdf.as_mut()) {
            pdf.pages = pages;
        }
        if let (Some(gap), Some(pdf)) = (self.pdf_gap, doc.pdf.as_mut()) {
            pdf.gap = gap;
        }
        apply_list(&mut doc.strokes, self.strokes);
        apply_list(&mut doc.texts, self.texts);
        if !self.images.is_empty() {
            // The files: those of the pictures there, and the new ones.
            let mut files: HashMap<u64, Arc<ImageData>> =
                doc.images.iter().map(|i| (i.data.key, i.data.clone())).collect();
            files.extend(self.image_files.into_iter().map(|f| (f.key, f)));
            let mut places = ImagePlace::of(&doc.images);
            apply_list(&mut places, self.images);
            doc.images = places
                .into_iter()
                .filter_map(|p| {
                    let data = files.get(&p.key)?.clone();
                    let ImagePlace { id, page, x, y, width, height, key: _ } = p;
                    Some(Image { id, page, x, y, width, height, data })
                })
                .collect();
        }
    }
}

/// The changes from `old` to `new`. `None` when a delta cannot express them:
/// the note is now on another PDF, or ids are not unique.
pub(crate) fn diff(old: &Document, new: &Document) -> Option<Delta> {
    let (pdf_pages, pdf_gap) = match (&old.pdf, &new.pdf) {
        (None, None) => (None, None),
        (Some(a), Some(b)) => {
            let same_file = match (&a.file, &b.file) {
                (Some(x), Some(y)) => x.key == y.key && x.bytes.len() == y.bytes.len(),
                (None, None) => true,
                _ => false,
            };
            if !same_file || a.sizes != b.sizes {
                return None;
            }
            ((a.pages != b.pages).then(|| b.pages.clone()), (a.gap != b.gap).then_some(b.gap))
        }
        _ => return None,
    };
    let layout = Layout::of(new);
    let had: HashSet<u64> = old.images.iter().map(|i| i.data.key).collect();
    let mut image_files: Vec<Arc<ImageData>> = Vec::new();
    for i in &new.images {
        if !had.contains(&i.data.key) && !image_files.iter().any(|f| f.key == i.data.key) {
            image_files.push(i.data.clone());
        }
    }
    Some(Delta {
        layout: (Layout::of(old) != layout).then_some(layout),
        pdf_pages,
        strokes: diff_list(&old.strokes, &new.strokes)?,
        texts: diff_list(&old.texts, &new.texts)?,
        images: diff_list(&ImagePlace::of(&old.images), &ImagePlace::of(&new.images))?,
        image_files,
        pdf_gap,
    })
}

fn diff_list<T: Item>(old: &[T], new: &[T]) -> Option<ListDelta<T>> {
    let old_by_id: HashMap<u64, &T> = old.iter().map(|x| (x.id(), x)).collect();
    let new_ids: HashSet<u64> = new.iter().map(Item::id).collect();
    if old_by_id.len() != old.len() || new_ids.len() != new.len() {
        return None;
    }
    let removed = old.iter().map(Item::id).filter(|id| !new_ids.contains(id)).collect();
    let mut changed = Vec::new();
    let mut pages = Vec::new();
    for x in new {
        match old_by_id.get(&x.id()) {
            Some(o) if o.same_content(x) => {
                if o.page() != x.page() {
                    pages.push((x.id(), x.page()));
                }
            }
            _ => changed.push(x.clone()),
        }
    }
    // The order replaying gives without an explicit one.
    let kept = old.iter().map(Item::id).filter(|id| new_ids.contains(id));
    let added = new.iter().map(Item::id).filter(|id| !old_by_id.contains_key(id));
    let in_order = kept.chain(added).eq(new.iter().map(Item::id));
    let order = (!in_order).then(|| new.iter().map(Item::id).collect());
    Some(ListDelta { removed, changed, pages, order })
}

fn apply_list<T: Item>(list: &mut Vec<T>, delta: ListDelta<T>) {
    let ListDelta { removed, changed, pages, order } = delta;
    if !removed.is_empty() {
        let removed: HashSet<u64> = removed.into_iter().collect();
        list.retain(|x| !removed.contains(&x.id()));
    }
    if !changed.is_empty() || !pages.is_empty() {
        let mut at: HashMap<u64, usize> = list.iter().enumerate().map(|(i, x)| (x.id(), i)).collect();
        for x in changed {
            match at.get(&x.id()) {
                Some(&i) => list[i] = x,
                None => {
                    at.insert(x.id(), list.len());
                    list.push(x);
                }
            }
        }
        for (id, page) in pages {
            if let Some(&i) = at.get(&id) {
                list[i].set_page(page);
            }
        }
    }
    if let Some(order) = order {
        let pos: HashMap<u64, usize> = order.into_iter().enumerate().map(|(i, id)| (id, i)).collect();
        list.sort_by_key(|x| pos.get(&x.id()).copied().unwrap_or(usize::MAX));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::history::{Action, History};
    use crate::model::{PageKind, Point, StrokeKind};
    use crate::pdf_pages::PdfFile;

    fn stroke(doc: &mut Document, page: u32, x: f32) -> Stroke {
        let id = doc.alloc_id();
        Stroke::new(
            id,
            page,
            StrokeKind::Pen,
            [0, 0, 0, 255],
            2.0,
            vec![Point::new(x, 0.0, 1.0), Point::new(x, 9.0, 1.0)],
        )
    }

    fn text(doc: &mut Document, page: u32, s: &str) -> TextBox {
        TextBox {
            id: doc.alloc_id(),
            page,
            x: 10.0,
            y: 20.0,
            width: 200.0,
            size: 16.0,
            color: [0, 0, 0],
            text: s.into(),
            lines: vec![s.into()],
            line_h: 20.0,
            layout: None,
        }
    }

    /// Replays the delta from `old` to `new` on a copy of `old`, checks it
    /// gives `new` and returns the delta.
    fn replays(old: &Document, new: &Document) -> Delta {
        let delta = diff(old, new).expect("expressible");
        let encoded = bincode::serialize(&delta).unwrap();
        let mut replayed = old.clone();
        bincode::deserialize::<Delta>(&encoded).unwrap().apply(&mut replayed);
        let left = diff(&replayed, new).unwrap();
        assert!(left.is_empty(), "replay differs: {left:?}");
        assert_eq!(bincode::serialize(&replayed).unwrap(), bincode::serialize(new).unwrap());
        delta
    }

    #[test]
    fn nothing_changed_is_empty() {
        let mut doc = Document::default();
        let s = stroke(&mut doc, 0, 1.0);
        doc.strokes.push(s);
        assert!(diff(&doc, &doc.clone()).unwrap().is_empty());
    }

    #[test]
    fn edits_replay_exactly() {
        let mut doc = Document::default();
        let mut history = History::default();
        for i in 0..4 {
            let s = stroke(&mut doc, 0, i as f32);
            history.exec(&mut doc, Action::AddStroke { index: usize::MAX, stroke: s });
        }
        let t = text(&mut doc, 0, "title");
        history.exec(&mut doc, Action::AddText { index: 0, text: t });

        // A new stroke only: no order, no layout beyond the id counter.
        let saved = doc.clone();
        let s = stroke(&mut doc, 0, 9.0);
        history.exec(&mut doc, Action::AddStroke { index: usize::MAX, stroke: s });
        let d = replays(&saved, &doc);
        assert_eq!((d.strokes.changed.len(), d.strokes.order.is_none()), (1, true));

        // Erase one, then undo: it comes back in the middle.
        let saved = doc.clone();
        let id = doc.strokes[1].id;
        let inverse = Action::RemoveStroke { id }.apply(&mut doc);
        history.push_undo(inverse);
        replays(&saved, &doc);
        let saved = doc.clone();
        history.undo(&mut doc);
        let d = replays(&saved, &doc);
        assert!(d.strokes.order.is_some());

        // Moving strokes rewrites them; inserting a page only renumbers.
        let saved = doc.clone();
        let ids = vec![doc.strokes[0].id, doc.strokes[2].id];
        history.exec(&mut doc, Action::InsertPage { index: 1, pdf_page: None });
        history.exec(&mut doc, Action::MoveStrokes { ids, to_page: 1, dx: 5.0, dy: 7.0 });
        replays(&saved, &doc);
        let saved = doc.clone();
        history.exec(&mut doc, Action::InsertPage { index: 0, pdf_page: None });
        let d = replays(&saved, &doc);
        assert!(d.strokes.changed.is_empty() && !d.strokes.pages.is_empty());

        // Text edit, page move and delete, paper change outside the history.
        let saved = doc.clone();
        let mut t = doc.texts[0].clone();
        t.text = "renamed".into();
        history.exec(&mut doc, Action::ReplaceText { text: t });
        history.exec(&mut doc, Action::MovePage { from: 0, to: 2 });
        history.exec(&mut doc, Action::DeletePage { index: 1 });
        doc.page.kind = PageKind::Grid;
        doc.page_width = 500.0;
        replays(&saved, &doc);

        // Everything undone in one go.
        let saved = doc.clone();
        while history.undo(&mut doc) {}
        replays(&saved, &doc);
    }

    #[test]
    fn pdf_page_list_is_recorded() {
        let file = PdfFile::new(b"%PDF-1.5 pretend".to_vec());
        let mut doc = Document::on_pdf(file, vec![[612.0, 792.0]; 3]);
        let saved = doc.clone();
        Action::InsertPage { index: 1, pdf_page: None }.apply(&mut doc);
        Action::MovePage { from: 0, to: 3 }.apply(&mut doc);
        let d = replays(&saved, &doc);
        assert_eq!(d.pdf_pages.unwrap(), [None, Some(1), Some(2), Some(0)]);
    }

    #[test]
    fn a_note_on_a_pdf_made_an_infinite_page_replays() {
        let file = PdfFile::new(b"%PDF-1.5 pretend".to_vec());
        let mut doc = Document::on_pdf(file, vec![[612.0, 792.0]; 3]);
        let s = stroke(&mut doc, 2, 40.0);
        doc.strokes.push(s);
        let saved = doc.clone();
        doc.pdf_to_canvas();
        replays(&saved, &doc);
        // More room between its pages.
        let saved = doc.clone();
        doc.set_pdf_gap(120.0);
        assert_eq!(replays(&saved, &doc).pdf_gap, Some(120.0));
        // And back.
        let canvas = doc.clone();
        assert!(doc.canvas_to_pdf_pages());
        replays(&canvas, &doc);
    }

    #[test]
    fn what_a_delta_cannot_express() {
        let mut doc = Document::default();
        let s = stroke(&mut doc, 0, 1.0);
        doc.strokes.push(s.clone());
        let mut twice = doc.clone();
        twice.strokes.push(s);
        assert!(diff(&doc, &twice).is_none(), "duplicate ids");

        let on_pdf = Document::on_pdf(PdfFile::new(b"a".to_vec()), vec![[612.0, 792.0]]);
        assert!(diff(&doc, &on_pdf).is_none());
        let mut other = on_pdf.clone();
        other.pdf.as_mut().unwrap().file = Some(Arc::new(PdfFile { bytes: b"b".to_vec(), key: 1 }));
        assert!(diff(&on_pdf, &other).is_none());
    }
}
