//! Action-based undo/redo.
//!
//! The history never clones the document. Every stack entry is the *command
//! that reverses the last change*. Applying a command returns its own inverse,
//! which is pushed on the opposite stack:
//!
//! * `AddStroke`    inserts a stroke (moved in) and returns `RemoveStroke`.
//! * `RemoveStroke` removes a stroke (moved out) and returns `AddStroke`.
//! * `Batch`        groups actions, e.g. everything erased in one drag.

use std::collections::VecDeque;

use crate::model::{Document, Stroke};

const MAX_DEPTH: usize = 1000;

pub enum Action {
    AddStroke { index: usize, stroke: Stroke },
    RemoveStroke { id: u64 },
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
