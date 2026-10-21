// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pictures: inserting them (pasted, from a file, dropped onto the canvas)
//! and, with the select tool, picking, moving, resizing and deleting them.
//!
//! A tap on a picture picks it; dragging it moves it, dragging a corner
//! resizes it (its proportions kept). A drag that starts on a picture not
//! picked draws a lasso, as anywhere else: ink written on a picture can
//! still be selected.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, Color32, CursorIcon, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};

use tratto_core::history::Action;
use tratto_core::images;
use tratto_core::model::{Image, ImageData};

use crate::file_picker::{Pick, Request};
use crate::theme::color;
use crate::toolbar::Tool;
use crate::view::View;

use super::{InkApp, PathField};

/// Extensions of the picture files that can be inserted.
pub(super) const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "bmp", "webp"];
/// How close to a corner of the picked picture a press resizes it (points).
const HANDLE: f32 = 12.0;
/// The smallest a picture gets when resized (document units).
const MIN_SIZE: f32 = 16.0;

/// A picture being moved or resized with the select tool.
pub(super) struct PictureDrag {
    /// Where the press was (content space).
    start: Pos2,
    /// The corner held, clockwise from the top-left; `None`: moving it.
    corner: Option<usize>,
    from: Image,
    /// Where it is now: shown instead of it until let go.
    pub now: Image,
}

/// A picture file, by its extension.
pub(super) fn is_picture(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The clipboard's picture: its pixels (a screenshot, a picture copied in a
/// browser), else the first picture file copied (in the file manager).
fn clipboard_picture() -> Result<Option<Arc<ImageData>>, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    if let Ok(img) = clipboard.get_image() {
        let (w, h) = (img.width as u32, img.height as u32);
        return ImageData::from_rgba(w, h, img.bytes.into_owned()).map(Some).map_err(|e| e.to_string());
    }
    if let Some(path) = clipboard.get().file_list().ok().and_then(|files| files.into_iter().find(|p| is_picture(p))) {
        return read_picture(&path).map(Some);
    }
    Ok(None)
}

fn read_picture(path: &Path) -> Result<Arc<ImageData>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    ImageData::from_file(bytes).map_err(|e| e.to_string())
}

/// The corners of `rect`, clockwise from the top-left.
fn corners(rect: Rect) -> [Pos2; 4] {
    [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()]
}

/// Where `v` goes for something `size` long to stay within `0..extent`
/// (or, larger, to cover it).
fn within(v: f32, size: f32, extent: f32) -> f32 {
    if size <= extent { v.clamp(0.0, extent - size) } else { v.clamp(extent - size, 0.0) }
}

/// `from` dragged from `start` to `now` (its page's space), on a page of
/// `size`: moved, kept on the page; or by `corner`, resized with the
/// opposite corner where it was and its proportions kept.
fn reshaped(from: &Image, corner: Option<usize>, start: Pos2, now: Pos2, (page_w, page_h): (f32, f32)) -> Image {
    let f = from;
    let Some(k) = corner else {
        let d = now - start;
        let (x, y) = (within(f.x + d.x, f.width, page_w), within(f.y + d.y, f.height, page_h));
        return Image { x, y, ..f.clone() };
    };
    let (left, top) = (k == 0 || k == 3, k <= 1);
    let ax = if left { f.x + f.width } else { f.x };
    let ay = if top { f.y + f.height } else { f.y };
    // The larger stretch wins.
    let scale = ((now.x - ax).abs() / f.width).max((now.y - ay).abs() / f.height).max(MIN_SIZE / f.width.min(f.height));
    let (width, height) = (f.width * scale, f.height * scale);
    let x = if left { ax - width } else { ax };
    let y = if top { ay - height } else { ay };
    Image { x, y, width, height, ..f.clone() }
}

impl InkApp {
    /// Puts `data` on the open note, centred at `at` (a page and a point on
    /// it) or in the middle of what is shown of the current page, and picks
    /// it with the select tool.
    pub(super) fn insert_picture(&mut self, data: Arc<ImageData>, at: Option<(u32, Pos2)>) {
        if self.reading() {
            self.toast("Pictures go in notes: write on this PDF first");
            return;
        }
        if self.edit.is_some() {
            let ctx = self.egui_ctx.clone();
            self.commit_edit(&ctx);
        }
        let layout = self.layout();
        let (page, center) = at.unwrap_or_else(|| {
            let page = self.current_page.min(layout.count - 1);
            let top = layout.page_top(page);
            let (from, to) = self.shown_content;
            let middle = (from.max(top) + to.min(top + layout.page_h)) * 0.5 - top;
            (page, pos2(layout.page_w * 0.5, middle.clamp(0.0, layout.page_h)))
        });
        let id = self.doc.alloc_id();
        let image = images::place(id, page, center, data, layout.page_w, layout.page_h);
        let index = self.doc.images.len();
        self.history.exec(&mut self.doc, Action::AddImage { index, image });
        self.clear_selection();
        self.tool = Tool::Select;
        self.saved_tool = None;
        self.picture = Some(id);
    }

    /// The clipboard's picture onto the note; a `quiet` paste (of text,
    /// outside a text field) says nothing when there is none.
    pub(super) fn paste_picture(&mut self, at: Option<(u32, Pos2)>, quiet: bool) {
        match clipboard_picture() {
            Ok(Some(data)) => self.insert_picture(data, at),
            Ok(None) if quiet => {}
            Ok(None) => self.toast("The clipboard has no picture"),
            Err(e) => self.toast(format!("Cannot paste the picture: {e}")),
        }
    }

    /// The picture file `path` onto the note.
    pub(super) fn insert_picture_file(&mut self, path: &Path, at: Option<(u32, Pos2)>) {
        match read_picture(path) {
            Ok(data) => self.insert_picture(data, at),
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.toast(format!("Cannot insert \u{201c}{name}\u{201d}: {e}"));
            }
        }
    }

    /// Asks for a picture file to insert.
    pub(super) fn ask_picture_file(&mut self, at: Option<(u32, Pos2)>) {
        if !self.file_picker.is_open() {
            self.pick_request = Some(Request {
                pick: Pick::Open("Pictures", &EXTENSIONS),
                title: "Insert a picture",
                current: String::new(),
                field: PathField::Picture { at },
            });
        }
    }

    /// Pictures pasted (a paste key, not in a text field) or dropped onto
    /// the canvas, at `pointer` when it is over a page.
    pub(super) fn picture_input(&mut self, ctx: &egui::Context, view: &View, pointer: Option<Pos2>) {
        if self.reading() {
            return;
        }
        let at = pointer.and_then(|p| {
            let c = view.screen_to_content(p);
            let page = view.layout.page_at(c)?;
            Some((page, c - view.layout.page_origin(page).to_vec2()))
        });
        let typing = self.edit.is_some() || ctx.egui_wants_keyboard_input();
        // The key on Windows (egui hides it without text on the clipboard);
        // elsewhere a paste of text, which may come with a picture.
        let key = std::mem::take(&mut self.pen_input.paste);
        let text = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
        if (key || text) && !typing {
            self.paste_picture(at, !key);
        }
        let dropped: Vec<PathBuf> = ctx
            .input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| is_picture(p)).collect());
        for (n, path) in dropped.iter().enumerate() {
            // Several: one a little below and right of the other.
            let at = at.map(|(page, p)| (page, p + vec2(24.0, 24.0) * n as f32));
            self.insert_picture_file(path, at);
        }
    }

    /// The picture on `page` under `p` (page space), the top one.
    pub(super) fn picture_at(&self, page: u32, p: Pos2) -> Option<u64> {
        self.doc.images.iter().rev().find(|i| i.page == page && i.rect().contains(p)).map(|i| i.id)
    }

    /// Where `img` is on screen.
    fn picture_rect(view: &View, img: &Image) -> Rect {
        let origin = view.page_origin_screen(img.page) + vec2(img.x, img.y) * view.scale;
        Rect::from_min_size(origin, vec2(img.width, img.height) * view.scale)
    }

    /// The picked picture as it is shown (being dragged: where it is now).
    fn picked_picture(&self) -> Option<&Image> {
        let dragged = self.picture_drag.as_ref().map(|d| &d.now);
        dragged.or_else(|| self.doc.images.iter().find(|i| Some(i.id) == self.picture))
    }

    /// What a press at the screen point `pos` grabs of the picked picture: a
    /// corner (to resize it) or the picture (to move it).
    fn picture_grab(&self, view: &View, pos: Pos2) -> Option<(Image, Option<usize>)> {
        let img = self.picked_picture()?;
        let rect = Self::picture_rect(view, img);
        if let Some(k) = corners(rect).iter().position(|c| c.distance(pos) <= HANDLE) {
            return Some((img.clone(), Some(k)));
        }
        rect.contains(pos).then(|| (img.clone(), None))
    }

    /// A press at `pos` on the picked picture starts moving or resizing it.
    pub(super) fn begin_picture_drag(&mut self, view: &View, pos: Pos2) -> bool {
        let Some((img, corner)) = self.picture_grab(view, pos) else { return false };
        self.picture_drag =
            Some(PictureDrag { start: view.screen_to_content(pos), corner, from: img.clone(), now: img });
        true
    }

    /// The picture being dragged follows the pointer at `pos`.
    pub(super) fn drag_picture(&mut self, view: &View, pos: Pos2) {
        let Some(d) = self.picture_drag.as_mut() else { return };
        let origin = view.layout.page_origin(d.from.page).to_vec2();
        let (start, now) = (d.start - origin, view.screen_to_content(pos) - origin);
        d.now = reshaped(&d.from, d.corner, start, now, (view.layout.page_w, view.layout.page_h));
    }

    /// Lets go of the dragged picture: where it is now (undoable).
    pub(super) fn finish_picture_drag(&mut self) {
        let Some(d) = self.picture_drag.take() else { return };
        if d.now != d.from {
            self.history.exec(&mut self.doc, Action::ReplaceImage { image: d.now });
        }
    }

    /// Deletes the picture `id` (undoable).
    pub(super) fn delete_picture(&mut self, id: u64) {
        if self.picture == Some(id) {
            self.picture = None;
            self.picture_drag = None;
        }
        self.history.exec(&mut self.doc, Action::RemoveImage { id });
    }

    /// The picked picture's frame and corner handles, and the cursor that
    /// says what a press at `hover` does.
    pub(super) fn paint_picked_picture(&self, painter: &Painter, view: &View, hover: Option<Pos2>) {
        let Some(img) = self.picked_picture() else { return };
        let rect = Self::picture_rect(view, img);
        painter.rect_stroke(rect, 0.0, Stroke::new(1.5, color::ACCENT), StrokeKind::Outside);
        for c in corners(rect) {
            let handle = Rect::from_center_size(c, vec2(10.0, 10.0));
            painter.rect(handle, 2.0, Color32::WHITE, Stroke::new(1.5, color::ACCENT), StrokeKind::Middle);
        }
        let dragging = self.picture_drag.as_ref().map(|d| d.corner);
        let grabbed = dragging.or_else(|| hover.and_then(|p| self.picture_grab(view, p)).map(|(_, corner)| corner));
        let icon = match grabbed {
            Some(Some(0 | 2)) => CursorIcon::ResizeNwSe,
            Some(Some(_)) => CursorIcon::ResizeNeSw,
            Some(None) => CursorIcon::Move,
            None => return,
        };
        painter.ctx().set_cursor_icon(icon);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Image {
        let data = Arc::new(ImageData::new(200, 100, vec![0]));
        Image { id: 1, page: 0, x: 100.0, y: 100.0, width: 200.0, height: 100.0, data }
    }

    const PAGE: (f32, f32) = (800.0, 1000.0);

    #[test]
    fn moving_keeps_it_on_the_page() {
        let moved = reshaped(&picture(), None, pos2(150.0, 150.0), pos2(170.0, 140.0), PAGE);
        assert_eq!((moved.x, moved.y, moved.width), (120.0, 90.0, 200.0));
        let far = reshaped(&picture(), None, pos2(150.0, 150.0), pos2(2000.0, -500.0), PAGE);
        assert_eq!((far.x, far.y), (600.0, 0.0));
    }

    #[test]
    fn resizing_keeps_the_opposite_corner_and_the_proportions() {
        // The bottom-right corner, pulled right: twice as wide, as tall.
        let r = reshaped(&picture(), Some(2), pos2(300.0, 200.0), pos2(500.0, 200.0), PAGE);
        assert_eq!((r.x, r.y, r.width, r.height), (100.0, 100.0, 400.0, 200.0));
        // The top-left corner, pushed in: the bottom-right stays.
        let r = reshaped(&picture(), Some(0), pos2(100.0, 100.0), pos2(200.0, 150.0), PAGE);
        assert_eq!((r.x + r.width, r.y + r.height), (300.0, 200.0));
        assert!((r.width / r.height - 2.0).abs() < 1e-4);
        // Never smaller than the smallest size.
        let r = reshaped(&picture(), Some(2), pos2(300.0, 200.0), pos2(100.0, 100.0), PAGE);
        assert!(r.height >= MIN_SIZE - 1e-4);
    }

    #[test]
    fn picture_files() {
        assert!(is_picture(Path::new("a/Photo.JPG")) && is_picture(Path::new("x.webp")));
        assert!(!is_picture(Path::new("notes.pdf")) && !is_picture(Path::new("noext")));
    }
}
