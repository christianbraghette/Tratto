// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Text boxes: picking, editing in place, committing, moving and resizing
//! them. The box being edited has a handle on either side: dragging one
//! changes its width, and the text wraps again as it goes. A knob above it
//! moves it.

use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{self, CursorIcon, Key, Margin, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};

use tratto_core::history::Action;
use tratto_core::model::TextBox;

use crate::markdown;
use crate::view::{Layout, View};

use super::{EditState, InkApp, TextPress};

/// Margin of the in-place editor around its text (points). The text inside
/// is where it is painted once committed, wrapped at the box's width.
pub(super) const EDIT_MARGIN: Margin = Margin::symmetric(4, 2);
/// How far either side of the editor's frame a press takes a side handle
/// (points): centred on the handle drawn there, and inside the frame only as
/// far as its margin, never over the text.
const HANDLE_REACH: f32 = 6.0;
/// The narrowest a text box gets (document units).
const MIN_WIDTH: f32 = 40.0;
/// How far above the editor's frame the move knob is, and how close to it a
/// press takes it (points): as the selection's rotate knob.
const MOVE_GAP: f32 = 28.0;
const KNOB_REACH: f32 = 12.0;

/// A handle of the box being edited.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum EditHandle {
    /// A side, which resizes it.
    Left,
    Right,
    /// The knob above it, which moves it.
    Move,
}

/// Where the move knob of the editor whose frame is `frame` (screen) is.
pub(super) fn move_knob(frame: Rect) -> Pos2 {
    frame.center_top() - vec2(0.0, MOVE_GAP)
}

/// The Markdown keys of the editor `id`, which writes `buf`, taken before it
/// sees them: Enter goes on with a list, Tab and Shift+Tab nest and unnest
/// its items, Ctrl+B / Ctrl+I / Ctrl+K make bold, italic and links (see
/// `markdown::enter` and the others). Tab never leaves the box.
pub(super) fn markdown_keys(ctx: &egui::Context, id: egui::Id, buf: &mut String) {
    if !ctx.memory(|m| m.has_focus(id)) {
        return;
    }
    let Some(mut state) = egui::TextEdit::load_state(ctx, id) else { return };
    let Some(range) = state.cursor.char_range() else { return };
    let chars = range.as_sorted_char_range();
    let byte = |c: usize| buf.char_indices().nth(c).map_or(buf.len(), |(b, _)| b);
    let sel = byte(chars.start.0)..byte(chars.end.0);
    let cmd = Modifiers::COMMAND;
    let edit = ctx.input_mut(|i| {
        if i.consume_key(Modifiers::SHIFT, Key::Tab) {
            markdown::indent(buf, sel, true)
        } else if i.consume_key(Modifiers::NONE, Key::Tab) {
            markdown::indent(buf, sel, false)
        } else if i.consume_key(cmd, Key::B) {
            Some(markdown::emphasis(buf, sel, "**"))
        } else if i.consume_key(cmd, Key::I) {
            Some(markdown::emphasis(buf, sel, "*"))
        } else if i.consume_key(cmd, Key::K) {
            Some(markdown::link(buf, sel))
        } else if i.modifiers.is_none() && i.key_pressed(Key::Enter) {
            // Only taken when it goes on with a list; else a line break.
            let edit = markdown::enter(buf, sel);
            if edit.is_some() {
                i.consume_key(Modifiers::NONE, Key::Enter);
            }
            edit
        } else {
            None
        }
    });
    let Some(edit) = edit else { return };
    let at = |b: usize| CCursor::new(edit.text[..b].chars().count());
    state.cursor.set_char_range(Some(CCursorRange::two(at(edit.sel.start), at(edit.sel.end))));
    *buf = edit.text;
    state.store(ctx, id);
}

impl InkApp {
    /// The frame of the editor of the box being edited, on screen: the text
    /// with the editor's margin, and a little room for the outline.
    pub(super) fn edit_frame(&self, view: &View) -> Option<Rect> {
        let e = self.edit.as_ref()?;
        let origin = view.page_origin_screen(e.page) + vec2(e.x, e.y) * view.scale;
        let margin = EDIT_MARGIN.sum();
        let h = e.last_h.max(e.size * view.scale * 1.3 + margin.y);
        let editor = Rect::from_min_size(origin - EDIT_MARGIN.left_top(), vec2(e.width * view.scale + margin.x, h));
        Some(editor.expand(2.0))
    }

    /// The handle of the box being edited at the screen point `pos`.
    /// Handles reach out from the frame, never over the text (the editor's).
    pub(super) fn edit_handle_at(&self, view: &View, pos: Pos2) -> Option<EditHandle> {
        let f = self.edit_frame(view)?;
        if move_knob(f).distance(pos) <= KNOB_REACH {
            Some(EditHandle::Move)
        } else if pos.y < f.top() - 2.0 || pos.y > f.bottom() + 2.0 {
            None
        } else if (f.left() - HANDLE_REACH..=f.left() + HANDLE_REACH).contains(&pos.x) {
            Some(EditHandle::Left)
        } else if (f.right() - HANDLE_REACH..=f.right() + HANDLE_REACH).contains(&pos.x) {
            Some(EditHandle::Right)
        } else {
            None
        }
    }

    /// Top-most text box under a content-space point.
    pub(super) fn text_at(&self, layout: &Layout, c: Pos2) -> Option<u64> {
        self.doc
            .texts
            .iter()
            .rev()
            .find(|t| {
                let o = layout.page_origin(t.page);
                Rect::from_min_size(pos2(o.x + t.x, o.y + t.y), vec2(t.width, t.height())).expand(4.0).contains(c)
            })
            .map(|t| t.id)
    }

    pub(super) fn begin_edit(&mut self, id: u64) {
        let Some(t) = self.doc.texts.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        // The toolbar now shows (and changes) this box's style.
        self.settings.text.color = t.color;
        self.settings.text.width = t.size;
        self.edit = Some(EditState {
            id,
            page: t.page,
            x: t.x,
            y: t.y,
            width: t.width,
            buf: t.text.clone(),
            size: t.size,
            color: t.color,
            last_h: 0.0,
            focus_pending: true,
            orig: Some(t),
        });
    }

    fn begin_new_edit(&mut self, page: u32, x: f32, y: f32, width: f32) {
        let id = self.doc.alloc_id();
        self.edit = Some(EditState {
            id,
            page,
            x,
            y,
            width,
            buf: String::new(),
            size: self.settings.text.width,
            color: self.settings.text.color,
            last_h: 0.0,
            focus_pending: true,
            orig: None,
        });
    }

    /// Finishes editing: lays the text out, then records an add / replace /
    /// remove in the history (nothing if unchanged, or an empty new box).
    pub(super) fn commit_edit(&mut self, ctx: &egui::Context) {
        let Some(e) = self.edit.take() else { return };
        if e.buf.trim().is_empty() {
            if let Some(orig) = e.orig {
                self.history.exec(&mut self.doc, Action::RemoveText { id: orig.id });
            }
            return;
        }
        let mut tb = TextBox {
            id: e.id,
            page: e.page,
            x: e.x,
            y: e.y,
            width: e.width,
            size: e.size,
            color: e.color,
            text: e.buf,
            lines: Vec::new(),
            line_h: 0.0,
            layout: None,
        };
        markdown::lay_out_box(ctx, &mut tb);
        match e.orig {
            None => {
                let index = self.doc.texts.len();
                self.history.exec(&mut self.doc, Action::AddText { index, text: tb });
            }
            Some(orig) => {
                if orig != tb {
                    self.history.exec(&mut self.doc, Action::ReplaceText { text: tb });
                }
            }
        }
    }

    pub(super) fn handle_text(&mut self, resp: &egui::Response, view: &View, press: Option<Pos2>, moves: &[Pos2]) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = press.or(resp.interact_pointer_pos()) {
                let c = view.screen_to_content(pos);
                let handle =
                    (self.edit_handle_at(view, pos).zip(self.edit.as_ref())).map(|(h, e)| (h, pos2(e.x, e.y), e.width));
                self.text_press = Some(TextPress {
                    hit: if handle.is_some() { None } else { self.text_at(&view.layout, c) },
                    start: c,
                    now: c,
                    page: view.layout.page_at(c),
                    moved: false,
                    handle,
                });
            }
        } else if (resp.dragged() || resp.drag_stopped())
            && let (Some(tp), Some(last)) = (self.text_press.as_mut(), moves.last())
        {
            tp.now = view.screen_to_content(*last);
            if (tp.now - tp.start).length() * view.scale > 5.0 {
                tp.moved = true;
            }
            // Resizing: the editor wraps the text at the new width at once.
            // Moving: the box follows, kept on its page.
            if let (Some((handle, at, w0)), Some(e)) = (tp.handle, self.edit.as_mut()) {
                let d = tp.now - tp.start;
                let x0 = at.x;
                let page = view.layout.page_box();
                let room = (page.right() - x0).max(MIN_WIDTH);
                match handle {
                    EditHandle::Right => e.width = (w0 + d.x).clamp(MIN_WIDTH, room),
                    EditHandle::Left => {
                        let right = x0 + w0;
                        e.x = (x0 + d.x).clamp(page.left(), (right - MIN_WIDTH).max(page.left()));
                        e.width = right - e.x;
                    }
                    EditHandle::Move => {
                        let h = e.last_h / view.scale;
                        e.x = (at.x + d.x).clamp(page.left(), (page.right() - e.width).max(page.left()));
                        e.y = (at.y + d.y).clamp(page.top(), (page.bottom() - h).max(page.top()));
                    }
                }
            }
        }
        if !resp.dragged()
            && let Some(tp) = self.text_press.take()
        {
            self.finish_text_press(tp, view.layout);
        }

        // The cursor tells what a press does: resize, move a box, or type.
        let handle_icon = |h: EditHandle| match h {
            EditHandle::Move => CursorIcon::Move,
            EditHandle::Left | EditHandle::Right => CursorIcon::ResizeHorizontal,
        };
        let icon = match &self.text_press {
            Some(TextPress { handle: Some((h, ..)), .. }) => Some(handle_icon(*h)),
            Some(tp) if tp.hit.is_some() && tp.moved => Some(CursorIcon::Move),
            _ => resp.hover_pos().and_then(|p| {
                let c = view.screen_to_content(p);
                if let Some(h) = self.edit_handle_at(view, p) {
                    Some(handle_icon(h))
                } else if self.text_at(&view.layout, c).is_some() {
                    Some(CursorIcon::Move)
                } else {
                    view.layout.page_at(c).map(|_| CursorIcon::Text)
                }
            }),
        };
        if let Some(icon) = icon {
            resp.ctx.set_cursor_icon(icon);
        }
    }

    fn finish_text_press(&mut self, tp: TextPress, layout: Layout) {
        if tp.handle.is_some() {
            // Already applied to the box being edited.
            return;
        }
        match (tp.hit, tp.moved) {
            (Some(id), false) => self.begin_edit(id),
            (Some(id), true) => self.commit_text_move(id, tp.now - tp.start, layout),
            (None, _) => {
                // Tap = default width; drag = box as wide as the drag.
                let Some(page) = tp.page else { return };
                let o = layout.page_origin(page);
                let b = layout.page_box();
                let dragged_w = (tp.now.x - tp.start.x).abs();
                let (left, width) = if tp.moved && dragged_w > 40.0 {
                    (tp.start.x.min(tp.now.x), dragged_w)
                } else {
                    let room = b.right() - (tp.start.x - o.x) - 20.0;
                    (tp.start.x, room.clamp(120.0, 360.0))
                };
                let top = if tp.moved { tp.start.y.min(tp.now.y) } else { tp.start.y };
                let x = (left - o.x).clamp(b.left(), (b.right() - 40.0).max(b.left()));
                let y = (top - o.y).clamp(b.top(), (b.bottom() - 20.0).max(b.top()));
                let width = width.min(b.right() - x).max(MIN_WIDTH);
                self.begin_new_edit(page, x, y, width);
            }
        }
    }

    /// Drop a dragged text box onto the page its middle is nearest to
    /// (possibly another), kept inside it.
    fn commit_text_move(&mut self, id: u64, d: Vec2, layout: Layout) {
        let Some(old) = self.doc.texts.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if d.length() < 0.5 {
            return;
        }
        let src_o = layout.page_origin(old.page).to_vec2();
        let center = pos2(old.x + old.width * 0.5, old.y + old.height() * 0.5) + src_o + d;
        let dst = layout.page_nearest_y(center.y);
        let local = d + src_o - layout.page_origin(dst).to_vec2();
        let mut new = old.clone();
        new.page = dst;
        let b = layout.page_box();
        new.x = (old.x + local.x).clamp(b.left(), (b.right() - old.width).max(b.left()));
        new.y = (old.y + local.y).clamp(b.top(), (b.bottom() - old.height()).max(b.top()));
        self.history.exec(&mut self.doc, Action::ReplaceText { text: new });
    }
}
