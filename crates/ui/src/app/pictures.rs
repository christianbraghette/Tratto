// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pictures: inserting them (pasted, from a file, dropped onto the canvas)
//! and, with the select tool, picking, moving, resizing and deleting them.
//!
//! A tap on a picture picks it; dragging it moves it, onto another page
//! too (the one its middle is over, as a text box), dragging a corner
//! resizes it (its proportions kept). A drag that starts on a picture not
//! picked draws a lasso, as anywhere else: ink written on a picture can
//! still be selected.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{self, Color32, CursorIcon, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2, vec2};

use tratto_core::history::Action;
use tratto_core::images;
use tratto_core::model::{Image, ImageData};

use crate::file_picker::{Pick, Request};
use crate::theme::color;
use crate::toolbar::Tool;
use crate::view::{Layout, View};

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

/// A picture to insert, made into a note's picture in the background (a
/// photo takes a while to shrink): see `InkApp::insert_pictures`.
pub(super) enum PictureSource {
    File(PathBuf),
    /// RGBA pixels, row by row (the clipboard's).
    Pixels(u32, u32, Vec<u8>),
}

impl PictureSource {
    fn read(self) -> Result<Arc<ImageData>, String> {
        match self {
            PictureSource::File(path) => read_picture(&path),
            PictureSource::Pixels(w, h, rgba) => ImageData::from_rgba(w, h, rgba).map_err(|e| e.to_string()),
        }
    }
}

/// A picture to insert: what it is made from, where it goes and what a
/// failure says ("Cannot paste").
pub(super) type PictureRequest = (PictureSource, Option<(u32, Pos2)>, String);

/// A picture made in the background (or why it could not be), where it
/// goes and what a failure says.
type MadePicture = (Result<Arc<ImageData>, String>, Option<(u32, Pos2)>, String);

/// Pictures being made in the background for the note at `note`.
pub(super) struct PictureJob {
    note: Option<PathBuf>,
    done: Receiver<MadePicture>,
}

/// The clipboard's picture: its pixels (a screenshot, a picture copied in a
/// browser), else the first picture file copied (in the file manager).
pub(super) fn clipboard_picture() -> Result<Option<PictureSource>, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    if let Ok(img) = clipboard.get_image() {
        let (w, h) = (img.width as u32, img.height as u32);
        return Ok(Some(PictureSource::Pixels(w, h, img.bytes.into_owned())));
    }
    if let Some(path) = clipboard.get().file_list().ok().and_then(|files| files.into_iter().find(|p| is_picture(p))) {
        return Ok(Some(PictureSource::File(path)));
    }
    Ok(None)
}

/// The picture file `path` to insert at `at`.
fn file_request(path: &Path, at: Option<(u32, Pos2)>) -> PictureRequest {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    (PictureSource::File(path.to_path_buf()), at, format!("Cannot insert \u{201c}{name}\u{201d}"))
}

fn read_picture(path: &Path) -> Result<Arc<ImageData>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    ImageData::from_file(bytes).map_err(|e| e.to_string())
}

/// The corners of `rect`, clockwise from the top-left.
pub(super) fn corners(rect: Rect) -> [Pos2; 4] {
    [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()]
}

/// Where `v` goes for something `size` long to stay within `from..to`
/// (or, larger, to cover it).
fn within(v: f32, size: f32, (from, to): (f32, f32)) -> f32 {
    if size <= to - from { v.clamp(from, to - size) } else { v.clamp(to - size, from) }
}

/// `from` moved by `delta` (content space): onto the page its middle is
/// then nearest to, kept on it.
fn moved(from: &Image, delta: Vec2, layout: &Layout) -> Image {
    let src = layout.page_origin(from.page).to_vec2();
    let center = pos2(from.x + from.width * 0.5, from.y + from.height * 0.5) + src + delta;
    let page = layout.page_nearest_y(center.y);
    let local = pos2(from.x, from.y) + src + delta - layout.page_origin(page).to_vec2();
    let b = layout.page_box();
    let (x, y) = (within(local.x, from.width, (b.left(), b.right())), within(local.y, from.height, (b.top(), b.bottom())));
    Image { page, x, y, ..from.clone() }
}

/// `from` resized by its corner `k` dragged to `now` (its page's space):
/// the opposite corner where it was and its proportions kept.
fn resized(from: &Image, k: usize, now: Pos2) -> Image {
    let f = from;
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
        let (page, center) = at.unwrap_or_else(|| self.paste_place());
        let id = self.doc.alloc_id();
        let image = images::place(id, page, center, data, layout.page_w, layout.page_h);
        let index = self.doc.images.len();
        self.history.exec(&mut self.doc, Action::AddImage { index, image });
        self.clear_selection();
        self.tool = Tool::Select;
        self.saved_tool = None;
        self.picture = Some(id);
    }

    /// The picture file `path` onto the note.
    pub(super) fn insert_picture_file(&mut self, path: &Path, at: Option<(u32, Pos2)>) {
        self.insert_pictures(vec![file_request(path, at)]);
    }

    /// Makes the pictures on a background thread and inserts each, in order,
    /// once it is made (see `poll_pictures`): the window never waits for a
    /// photo to be shrunk.
    pub(super) fn insert_pictures(&mut self, pictures: Vec<PictureRequest>) {
        if pictures.is_empty() {
            return;
        }
        let (tx, done) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let work = move || {
            for (source, at, failure) in pictures {
                if tx.send((source.read(), at, failure)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        };
        match thread::Builder::new().name("pictures".into()).spawn(work) {
            Ok(_) => self.picture_jobs.push(PictureJob { note: self.note_path.clone(), done }),
            Err(e) => self.toast(format!("Cannot insert the picture: {e}")),
        }
    }

    /// Inserts the pictures made in the background, into the note they were
    /// for if it is still open.
    pub(super) fn poll_pictures(&mut self) {
        let mut made = Vec::new();
        self.picture_jobs.retain(|job| {
            loop {
                match job.done.try_recv() {
                    Ok(picture) => made.push((job.note.clone(), picture)),
                    Err(TryRecvError::Empty) => return true,
                    Err(TryRecvError::Disconnected) => return false,
                }
            }
        });
        for (note, (read, at, failure)) in made {
            if note != self.note_path || self.screen != super::Screen::Editor {
                continue;
            }
            // Its page may have been deleted meanwhile.
            let at = at.filter(|(page, _)| *page < self.doc.page_count);
            match read {
                Ok(data) => self.insert_picture(data, at),
                Err(e) => self.toast(format!("{failure}: {e}")),
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

    /// A paste (a paste key, not in a text field; see `clipboard`) and
    /// pictures dropped onto the canvas, at `pointer` when it is over a page.
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
        // The key, which egui hides without text on the clipboard (see
        // `hidden_paste`); or a paste of text, which may come with a picture.
        let key = std::mem::take(&mut self.pen_input.paste);
        let text = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
        if (key || text) && !typing {
            self.paste(at, !key);
        }
        let dropped: Vec<PathBuf> = ctx
            .input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| is_picture(p)).collect());
        let requests = dropped.iter().enumerate().map(|(n, path)| {
            // Several: one a little below and right of the other.
            file_request(path, at.map(|(page, p)| (page, p + vec2(24.0, 24.0) * n as f32)))
        });
        self.insert_pictures(requests.collect());
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
        let now = view.screen_to_content(pos);
        d.now = match d.corner {
            None => moved(&d.from, now - d.start, &view.layout),
            Some(k) => {
                let origin = view.layout.page_origin(d.from.page).to_vec2();
                resized(&d.from, k, now - origin)
            }
        };
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
        painter.rect_stroke(rect, 0.0, Stroke::new(1.5, color::accent()), StrokeKind::Outside);
        for c in corners(rect) {
            let handle = Rect::from_center_size(c, vec2(10.0, 10.0));
            painter.rect(handle, 2.0, Color32::WHITE, Stroke::new(1.5, color::accent()), StrokeKind::Middle);
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

    const LAYOUT: Layout = Layout { page_w: 800.0, page_h: 1000.0, count: 3, canvas: false, extent: egui::Vec2::ZERO, origin: egui::Vec2::ZERO };

    #[test]
    fn moving_keeps_it_on_the_page() {
        let m = moved(&picture(), vec2(20.0, -10.0), &LAYOUT);
        assert_eq!((m.page, m.x, m.y, m.width), (0, 120.0, 90.0, 200.0));
        let far = moved(&picture(), vec2(2000.0, -500.0), &LAYOUT);
        assert_eq!((far.page, far.x, far.y), (0, 600.0, 0.0));
    }

    #[test]
    fn moving_goes_onto_the_page_its_middle_is_over() {
        let from = picture();
        let to_page_1 = LAYOUT.page_top(1) - LAYOUT.page_top(0);
        // Its middle onto page 1: there, at the same place on the page.
        let m = moved(&from, vec2(0.0, to_page_1), &LAYOUT);
        assert_eq!((m.page, m.x, m.y), (1, 100.0, 100.0));
        // Half over the gap, its middle still on page 0: kept on page 0.
        let low = moved(&from, vec2(0.0, 1000.0 - 150.0 - 10.0), &LAYOUT);
        assert_eq!((low.page, low.y), (0, 900.0));
        // Off the last page: on the last page.
        let off = moved(&from, vec2(0.0, 10.0 * to_page_1), &LAYOUT);
        assert_eq!((off.page, off.y), (2, 900.0));
    }

    #[test]
    fn resizing_keeps_the_opposite_corner_and_the_proportions() {
        // The bottom-right corner, pulled right: twice as wide, as tall.
        let r = resized(&picture(), 2, pos2(500.0, 200.0));
        assert_eq!((r.x, r.y, r.width, r.height), (100.0, 100.0, 400.0, 200.0));
        // The top-left corner, pushed in: the bottom-right stays.
        let r = resized(&picture(), 0, pos2(200.0, 150.0));
        assert_eq!((r.x + r.width, r.y + r.height), (300.0, 200.0));
        assert!((r.width / r.height - 2.0).abs() < 1e-4);
        // Never smaller than the smallest size.
        let r = resized(&picture(), 2, pos2(100.0, 100.0));
        assert!(r.height >= MIN_SIZE - 1e-4);
    }

    #[test]
    fn picture_files() {
        assert!(is_picture(Path::new("a/Photo.JPG")) && is_picture(Path::new("x.webp")));
        assert!(!is_picture(Path::new("notes.pdf")) && !is_picture(Path::new("noext")));
    }
}
