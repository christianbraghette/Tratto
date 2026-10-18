//! Action-based undo/redo.
//!
//! The history never clones the document. Every stack entry is the *command
//! that reverses the last change*. Applying a command returns its own inverse,
//! which is pushed on the opposite stack:
//!
//! * `AddStroke`    inserts a stroke (moved in) and returns `RemoveStroke`.
//! * `RemoveStroke` removes a stroke (moved out) and returns `AddStroke`.
//! * `InsertPage` / `RemovePage` change the page list (shifting stroke pages).
//! * `MoveStrokes` moves strokes, possibly to another page.
//! * `Batch`        groups actions, e.g. everything erased in one drag.

use std::collections::{HashSet, VecDeque};

use crate::model::{Document, Stroke};

const MAX_DEPTH: usize = 1000;

pub enum Action {
    AddStroke { index: usize, stroke: Stroke },
    RemoveStroke { id: u64 },
    InsertPage { index: u32 },
    RemovePage { index: u32 },
    /// Translate the strokes by (dx, dy) in page space and put them on `to_page`.
    MoveStrokes { ids: Vec<u64>, to_page: u32, dx: f32, dy: f32 },
    Batch(Vec<Action>),
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
            Action::InsertPage { index } => {
                let index = index.min(doc.page_count);
                for s in doc.strokes.iter_mut().filter(|s| s.page >= index) {
                    s.page += 1;
                }
                doc.page_count += 1;
                Action::RemovePage { index }
            }
            Action::RemovePage { index } => {
                if doc.page_count <= 1 || index >= doc.page_count {
                    return Action::Batch(Vec::new());
                }
                // The page is empty when reached through undo/redo or the UI;
                // as a safety net any leftover strokes go to the previous page.
                for s in doc.strokes.iter_mut() {
                    if s.page == index {
                        s.page = index.saturating_sub(1);
                    } else if s.page > index {
                        s.page -= 1;
                    }
                }
                doc.page_count -= 1;
                Action::InsertPage { index }
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
            Action::Batch(actions) => {
                let mut inverses: Vec<Action> =
                    actions.into_iter().map(|a| a.apply(doc)).collect();
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
}

impl History {
    /// Executes `action` and records its inverse. Clears the redo stack.
    pub fn exec(&mut self, doc: &mut Document, action: Action) {
        let inverse = action.apply(doc);
        self.push_undo(inverse);
    }

    /// Records an already-performed change by its inverse.
    pub fn push_undo(&mut self, inverse: Action) {
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
    use crate::model::{Point, StrokeKind};

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
        h.exec(&mut doc, Action::InsertPage { index: 1 });
        assert_eq!(doc.page_count, 3);
        assert_eq!(doc.strokes.iter().map(|s| s.page).collect::<Vec<_>>(), vec![0, 2]);
        h.undo(&mut doc);
        assert_eq!(doc.page_count, 2);
        assert_eq!(doc.strokes.iter().map(|s| s.page).collect::<Vec<_>>(), vec![0, 1]);
        h.redo(&mut doc);
        assert_eq!(doc.strokes[1].page, 2);
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
        let inv = vec![
            Action::RemoveStroke { id: 1 }.apply(&mut doc),
            Action::RemoveStroke { id: 3 }.apply(&mut doc),
        ];
        h.push_undo(Action::Batch(inv.into_iter().rev().collect()));
        assert_eq!(doc.strokes.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
        h.undo(&mut doc);
        assert_eq!(doc.strokes.iter().map(|s| s.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        h.redo(&mut doc);
        assert_eq!(doc.strokes.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
    }
}
