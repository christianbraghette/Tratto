// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Application state and per-frame orchestration.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, Event, FontId, Id, Key, Modifiers, MouseWheelUnit, PointerButton,
    Pos2, Rect, Sense, TouchPhase, Vec2, pos2, vec2,
};

use inknote_core::geometry::point_in_polygon;
use inknote_core::history::{Action, History};
use inknote_core::library::Library;
use inknote_core::model::{
    Document, Point, Stroke, StrokeKind, TextBox,
};
use inknote_core::storage::{self, NoteMeta};
use inknote_core::stroke_builder::StrokeBuilder;

use crate::input::wayland_pen::{PenEvent, WaylandPen};
use crate::input::windows_pen::WindowsPen;
use crate::kinetic::Kinetic;
use crate::render::{self, Moved, StrokeCache};
use crate::scrollbar;
use crate::settings::{LeftPanel, Settings, ZoomMode};
use crate::theme::{self, color, radius, space};
use crate::toolbar::{self, Tool, ToolbarEvent};
use crate::view::{Layout, MARGIN, TOP_PAD_PX, View};

mod library_panel;
mod library_ui;
mod export_ui;
mod menus;
mod settings_ui;

use menus::{Command, ContextMenu, HelpWindows};

/// Width of the left page-preview panel (points).
/// Left panel views: default width and resize range of each (each view
/// remembers its own width).
const PAGES_WIDTH: f32 = 164.0;
/// The narrowest width still fits the "Add page" tile's label.
const PAGES_WIDTHS: std::ops::RangeInclusive<f32> = 150.0..=360.0;
/// Page previews never get wider than this, however wide the panel.
const THUMB_MAX_W: f32 = 200.0;
/// Autosave this long after the last change.
/// Key under which eframe stores the window state (not public in eframe).
const WINDOW_STATE_KEY: &str = "window";
const SETTINGS_KEY: &str = "inknote_settings";
/// Samples closer than this (document units) are treated as jitter.
const MIN_POINT_DIST: f32 = 0.6;
/// Smoothing strength at rest (0..1, lower = smoother) ...
const SMOOTH_ALPHA_MIN: f32 = 0.4;
/// ... fading to none at this per-sample distance (document units).
const SMOOTH_SPEED_REF: f32 = 6.0;
const HIGHLIGHTER_ALPHA: u8 = 96;

/// Stroke currently being drawn.
struct ActiveStroke {
    page: u32,
    kind: StrokeKind,
    color: [u8; 4],
    width: f32,
    builder: StrokeBuilder,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Library,
    Editor,
}

/// Cheap summary of everything that makes the open note "changed".
type Fingerprint = (u64, u32, u32, u8, u32);

/// A text box being typed into. It is not part of the document until it is
/// committed, so abandoned empty boxes never reach the undo history.
struct EditState {
    id: u64,
    page: u32,
    x: f32,
    y: f32,
    width: f32,
    buf: String,
    size: f32,
    color: [u8; 3],
    /// Height of the editor in the previous frame (for its background).
    last_h: f32,
    /// Ask egui for keyboard focus on the next frame.
    focus_pending: bool,
    /// The box as it was before editing (None = newly created).
    orig: Option<TextBox>,
}

/// A press with the text tool: tap = edit / create, drag = move / size.
struct TextPress {
    /// Existing text box under the pen.
    hit: Option<u64>,
    start: Pos2,
    now: Pos2,
    page: Option<u32>,
    moved: bool,
}

/// Strokes picked with the select tool (all on one page).
struct Selection {
    page: u32,
    ids: HashSet<u64>,
    /// Bounding box in that page's space.
    bounds: Rect,
}

/// Freehand lasso being drawn (page space).
struct Lasso {
    page: u32,
    points: Vec<Pos2>,
}

/// Selection being dragged; `delta` is in content space.
struct MoveDrag {
    start: Pos2,
    delta: Vec2,
}

pub struct InkApp {
    doc: Document,
    history: History,
    tool: Tool,
    settings: Settings,
    /// Content-space position at the top-left of the writing area.
    scroll: Vec2,
    /// Momentum, wheel acceleration and eased jumps of `scroll`.
    kinetic: Kinetic,
    vbar_grab: Option<f32>,
    hbar_grab: Option<f32>,
    /// Whether a horizontal scrollbar was shown last frame (toolbar placement).
    hbar_visible: bool,
    active: Option<ActiveStroke>,
    /// Inverse actions of strokes erased during the current eraser drag.
    erase_session: Vec<Action>,
    /// Last eraser position, in content space.
    last_erase_pos: Option<Pos2>,
    last_pressure: f32,
    status: String,
    /// When `status` was last set (drives the transient toast).
    status_at: Option<Instant>,
    /// Native Wayland pen source (None on X11/Windows or without tablet support).
    pen: Option<WaylandPen>,
    /// Windows: pen side button / eraser end (None elsewhere).
    win_pen: Option<WindowsPen>,
    pen_init_done: bool,
    pen_down: bool,
    pen_pos: Pos2,
    /// Tool to restore when the temporary eraser (pen eraser end / side button) is released.
    saved_tool: Option<Tool>,
    /// Eraser requested by the native pen (eraser end or side button).
    eraser_native: bool,
    /// A drag that started on the grey area is scrolling the view.
    panning: bool,
    selection: Option<Selection>,
    lasso: Option<Lasso>,
    moving: Option<MoveDrag>,
    /// Cached anti-aliased meshes of finished strokes.
    cache: StrokeCache,
    /// Same, at page-preview scale.
    thumb_cache: StrokeCache,
    /// Page being dragged in the preview panel.
    page_drag: Option<u32>,
    /// Current page the pages panel last scrolled to.
    panel_page: Option<u32>,
    /// Content-space y to scroll to on the next frame (jump to page).
    scroll_to: Option<f32>,
    /// Page in the middle of the view (highlighted in the panel).
    current_page: u32,
    screen: Screen,
    library: Library,
    lib_ui: library_ui::LibraryUi,
    /// File of the open note.
    note_path: Option<PathBuf>,
    note_meta: NoteMeta,
    dirty: bool,
    last_change: Instant,
    last_fp: Fingerprint,
    /// Text box being typed into.
    edit: Option<EditState>,
    text_press: Option<TextPress>,
    /// Canvas context menu (right click), if open.
    context_menu: Option<ContextMenu>,
    /// The press that closed the context menu must not reach the canvas.
    swallow_press: bool,
    /// Menu commands collected this frame, run after drawing.
    pending: Vec<Command>,
    help: HelpWindows,
    /// Decorations last requested from the OS (None = not yet).
    decorations: Option<bool>,
    /// The window state restored at startup has been checked.
    window_checked: bool,
    /// The window was maximized before entering fullscreen.
    maximize_after_fullscreen: bool,
    /// Window title last sent to the OS.
    os_title: String,
    /// Text of the note title field in the title bar, and whether it has focus.
    title_buf: String,
    title_focused: bool,
    /// A menu of the title bar was open last frame (keeps the fullscreen bar shown).
    menu_open: bool,
    /// File > Settings window.
    global_settings_open: bool,
    /// Export PDF dialog, if open.
    export_dialog: Option<export_ui::ExportDialog>,
    /// PDF being written in the background, if any.
    export_job: Option<export_ui::ExportJob>,
    /// Note being dragged in the notebook panel.
    note_drag: Option<library_panel::NoteDrag>,
    /// Same for the notebook name in the notebook panel.
    notebook_buf: String,
    notebook_focused: bool,
    /// Zoom of the last frame (points per document unit).
    view_scale: f32,
    /// Zoom factor requested by a menu command, applied around the centre.
    zoom_request: Option<f32>,
}

impl InkApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // We implement zoom ourselves; keep egui's UI from zooming on Ctrl +/-.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        crate::theme::install(&cc.egui_ctx);

        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();

        let mut app = Self {
            doc: Document::default(),
            history: History::default(),
            tool: Tool::Pen,
            settings,
            scroll: Vec2::ZERO,
            kinetic: Kinetic::default(),
            vbar_grab: None,
            hbar_grab: None,
            hbar_visible: false,
            active: None,
            erase_session: Vec::new(),
            last_erase_pos: None,
            last_pressure: 1.0,
            status: String::new(),
            status_at: None,
            pen: None,
            win_pen: None,
            pen_init_done: false,
            pen_down: false,
            pen_pos: Pos2::ZERO,
            saved_tool: None,
            eraser_native: false,
            panning: false,
            selection: None,
            lasso: None,
            moving: None,
            cache: StrokeCache::default(),
            thumb_cache: StrokeCache::default(),
            page_drag: None,
            panel_page: None,
            scroll_to: None,
            current_page: 0,
            screen: Screen::Library,
            library: Library::default(),
            lib_ui: library_ui::LibraryUi::default(),
            note_path: None,
            note_meta: NoteMeta::new("Untitled note", true),
            dirty: false,
            last_change: Instant::now(),
            last_fp: (0, 0, 0, 0, 0),
            edit: None,
            text_press: None,
            context_menu: None,
            swallow_press: false,
            pending: Vec::new(),
            help: HelpWindows::default(),
            // main.rs creates the window without decorations.
            decorations: Some(false),
            window_checked: false,
            maximize_after_fullscreen: false,
            os_title: "InkNote".to_owned(),
            title_buf: String::new(),
            title_focused: false,
            menu_open: false,
            global_settings_open: false,
            export_dialog: None,
            export_job: None,
            note_drag: None,
            notebook_buf: String::new(),
            notebook_focused: false,
            view_scale: 1.0,
            zoom_request: None,
        };
        app.refresh_library();
        app
    }

    // ---------------------------------------------------------------- notes

    fn toast(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_at = Some(Instant::now());
    }

    fn library_root(&self) -> PathBuf {
        PathBuf::from(&self.settings.library_root)
    }

    /// Re-reads the library folder (cheap: only the small header of each note).
    fn refresh_library(&mut self) {
        let root = self.library_root();
        self.library = match Library::scan(&root) {
            Ok(l) => l,
            Err(e) => {
                self.toast(format!("Cannot read library folder: {e}"));
                Library::empty(root)
            }
        };
    }

    fn current_fingerprint(&self) -> Fingerprint {
        (
            self.history.version(),
            self.doc.page_width.to_bits(),
            self.doc.page_height.to_bits(),
            self.doc.page.kind as u8,
            self.doc.page.spacing.to_bits(),
        )
    }

    /// Marks the note dirty when anything changed since the last check.
    fn sync_dirty(&mut self) {
        let fp = self.current_fingerprint();
        if fp != self.last_fp {
            self.last_fp = fp;
            self.dirty = true;
            self.last_change = Instant::now();
        }
    }

    /// Writes the open note. Returns false (and shows why) on failure.
    fn save_note(&mut self, announce: bool) -> bool {
        let Some(path) = self.note_path.clone() else {
            return false;
        };
        match storage::save_note(&path, &mut self.note_meta, &self.doc) {
            Ok(()) => {
                self.dirty = false;
                if announce {
                    self.toast("Saved");
                }
                true
            }
            Err(e) => {
                self.toast(format!("Save failed: {e}"));
                false
            }
        }
    }

    fn open_note(&mut self, path: &Path) {
        match storage::load_note(path) {
            Ok((meta, doc)) => {
                self.doc = doc;
                self.note_meta = meta;
                self.note_path = Some(path.to_path_buf());
                self.ensure_pages();
                self.reset_editing_state();
                self.last_fp = self.current_fingerprint();
                self.dirty = false;
                self.screen = Screen::Editor;
            }
            Err(e) => self.toast(format!("Cannot open note: {e}")),
        }
    }

    /// Saves and returns to the library. Stays in the editor if saving fails,
    /// so nothing typed or drawn can be lost.
    fn close_note(&mut self, ctx: &egui::Context) {
        if self.edit.is_some() {
            self.commit_edit(ctx);
        }
        self.sync_dirty();
        if self.dirty && !self.save_note(false) {
            return;
        }
        self.note_path = None;
        self.context_menu = None;
        self.export_dialog = None;
        self.doc = Document::default();
        self.reset_editing_state();
        self.screen = Screen::Library;
        self.refresh_library();
    }

    fn reset_editing_state(&mut self) {
        self.cache.clear();
        self.thumb_cache.clear();
        self.page_drag = None;
        self.edit = None;
        self.text_press = None;
        self.history.clear();
        self.active = None;
        self.erase_session.clear();
        self.last_erase_pos = None;
        self.scroll = Vec2::ZERO;
        self.kinetic.stop();
    }

    /// Guarantees that every stroke lies on an existing page (e.g. after an
    /// undo that restores a stroke whose page was removed).
    fn ensure_pages(&mut self) {
        let need = self
            .doc
            .strokes
            .iter()
            .map(|s| s.page + 1)
            .max()
            .unwrap_or(1)
            .max(1);
        if self.doc.page_count < need {
            self.doc.page_count = need;
        }
    }

    // ------------------------------------------------------- undo / redo

    fn undo(&mut self) {
        if !self.busy() && self.edit.is_none() {
            self.history.undo(&mut self.doc);
            self.ensure_pages();
            self.clear_selection();
        }
    }

    fn redo(&mut self) {
        if !self.busy() && self.edit.is_none() {
            self.history.redo(&mut self.doc);
            self.ensure_pages();
            self.clear_selection();
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // While typing (a text box or a text field such as the note title),
        // Ctrl+Z / Ctrl+Y belong to the text editor.
        let typing = self.edit.is_some() || ctx.wants_keyboard_input();
        // Order matters: the Shift variant must be consumed before plain Ctrl+Z.
        let (redo_shift, redo_y, undo, save, export) = ctx.input_mut(|i| {
            (
                !typing && i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
                !typing && i.consume_key(Modifiers::COMMAND, Key::Y),
                !typing && i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::COMMAND, Key::E),
            )
        });
        if redo_shift || redo_y {
            self.redo();
        } else if undo {
            self.undo();
        }
        if save {
            self.save_note(true);
        }
        if export {
            self.open_export_dialog();
        }
        // While the context menu is open, Escape belongs to it.
        if self.selection.is_some() && self.context_menu.is_none() && !ctx.wants_keyboard_input() {
            let (delete, escape) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Delete)
                        || i.consume_key(Modifiers::NONE, Key::Backspace),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            if delete {
                self.delete_selection();
            } else if escape {
                self.clear_selection();
            }
        }
    }

    // ------------------------------------------------------------- input

    /// Tracks pen pressure from touch/pen events (mouse stays at 1.0).
    fn update_pressure(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            for e in &i.events {
                if let Event::Touch { phase, force, .. } = e {
                    match phase {
                        TouchPhase::End | TouchPhase::Cancel => self.last_pressure = 1.0,
                        _ => {
                            if let Some(f) = force {
                                if *f > 0.0 {
                                    self.last_pressure = f.clamp(0.0, 1.0);
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Converts native pen events into regular egui pointer events and pressure.
    /// Called from `raw_input_hook`, i.e. before egui processes the frame.
    fn inject_pen_events(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(pen) = self.pen.as_ref() else { return };
        let events: Vec<PenEvent> = pen.try_iter().collect();
        let zoom = ctx.zoom_factor();

        for ev in events {
            match ev {
                PenEvent::Motion {
                    x,
                    y,
                    pressure,
                    down,
                    eraser,
                } => {
                    let pos = Pos2::new(x / zoom, y / zoom);
                    self.pen_pos = pos;
                    raw_input.events.push(Event::PointerMoved(pos));
                    if down != self.pen_down {
                        self.pen_down = down;
                        raw_input.events.push(Event::PointerButton {
                            pos,
                            button: PointerButton::Primary,
                            pressed: down,
                            modifiers: raw_input.modifiers,
                        });
                    }
                    if down {
                        self.last_pressure = pressure.clamp(0.0, 1.0);
                    }
                    self.eraser_native = eraser;
                }
                PenEvent::Leave => {
                    if self.pen_down {
                        self.pen_down = false;
                        raw_input.events.push(Event::PointerButton {
                            pos: self.pen_pos,
                            button: PointerButton::Primary,
                            pressed: false,
                            modifiers: raw_input.modifiers,
                        });
                    }
                    raw_input.events.push(Event::PointerGone);
                    self.eraser_native = false;
                    self.last_pressure = 1.0;
                }
            }
        }
    }

    /// Switches to the eraser while it is requested (pen eraser end or a held
    /// side button) and restores the previous tool afterwards.
    /// Pressure for new points: the pen's, or full when pressure sensitivity
    /// is off (Settings).
    fn pen_pressure(&self) -> f32 {
        if self.settings.pressure { self.last_pressure } else { 1.0 }
    }

    fn apply_pen_eraser(&mut self, eraser: bool) {
        if eraser {
            if self.saved_tool.is_none() && self.tool != Tool::Eraser {
                self.saved_tool = Some(self.tool);
                self.tool = Tool::Eraser;
            }
        } else if let Some(t) = self.saved_tool.take() {
            self.tool = t;
        }
    }

    // ---------------------------------------------------------- text boxes

    /// Top-most text box under a content-space point.
    fn text_at(&self, layout: &Layout, c: Pos2) -> Option<u64> {
        self.doc
            .texts
            .iter()
            .rev()
            .find(|t| {
                let o = layout.page_origin(t.page);
                Rect::from_min_size(pos2(o.x + t.x, o.y + t.y), vec2(t.width, t.height()))
                    .expand(4.0)
                    .contains(c)
            })
            .map(|t| t.id)
    }

    fn begin_edit(&mut self, id: u64) {
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
    fn commit_edit(&mut self, ctx: &egui::Context) {
        let Some(e) = self.edit.take() else { return };
        if e.buf.trim().is_empty() {
            if let Some(orig) = e.orig {
                self.history
                    .exec(&mut self.doc, Action::RemoveText { id: orig.id });
            }
            return;
        }
        let (lines, line_h) = layout_lines(ctx, &e.buf, e.size, e.width);
        let tb = TextBox {
            id: e.id,
            page: e.page,
            x: e.x,
            y: e.y,
            width: e.width,
            size: e.size,
            color: e.color,
            text: e.buf,
            lines,
            line_h,
        };
        match e.orig {
            None => {
                let index = self.doc.texts.len();
                self.history
                    .exec(&mut self.doc, Action::AddText { index, text: tb });
            }
            Some(orig) => {
                if orig != tb {
                    self.history
                        .exec(&mut self.doc, Action::ReplaceText { text: tb });
                }
            }
        }
    }

    fn handle_text(
        &mut self,
        resp: &egui::Response,
        view: &View,
        press: Option<Pos2>,
        moves: &[Pos2],
    ) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = press.or(resp.interact_pointer_pos()) {
                let c = view.screen_to_content(pos);
                self.text_press = Some(TextPress {
                    hit: self.text_at(&view.layout, c),
                    start: c,
                    now: c,
                    page: view.layout.page_at(c),
                    moved: false,
                });
            }
        } else if resp.dragged() || resp.drag_stopped() {
            if let (Some(tp), Some(last)) = (self.text_press.as_mut(), moves.last()) {
                tp.now = view.screen_to_content(*last);
                if (tp.now - tp.start).length() * view.scale > 5.0 {
                    tp.moved = true;
                }
            }
        }
        if !resp.dragged() {
            if let Some(tp) = self.text_press.take() {
                self.finish_text_press(tp, view.layout);
            }
        }
    }

    fn finish_text_press(&mut self, tp: TextPress, layout: Layout) {
        match (tp.hit, tp.moved) {
            (Some(id), false) => self.begin_edit(id),
            (Some(id), true) => self.commit_text_move(id, tp.now - tp.start, layout),
            (None, _) => {
                // Tap = default width; drag = box as wide as the drag.
                let Some(page) = tp.page else { return };
                let o = layout.page_origin(page);
                let dragged_w = (tp.now.x - tp.start.x).abs();
                let (left, width) = if tp.moved && dragged_w > 40.0 {
                    (tp.start.x.min(tp.now.x), dragged_w)
                } else {
                    let room = layout.page_w - (tp.start.x - o.x) - 20.0;
                    (tp.start.x, room.clamp(120.0, 360.0))
                };
                let top = if tp.moved {
                    tp.start.y.min(tp.now.y)
                } else {
                    tp.start.y
                };
                let x = (left - o.x).clamp(0.0, (layout.page_w - 40.0).max(0.0));
                let y = (top - o.y).clamp(0.0, (layout.page_h - 20.0).max(0.0));
                let width = width.min(layout.page_w - x).max(40.0);
                self.begin_new_edit(page, x, y, width);
            }
        }
    }

    /// Drop a dragged text box, possibly onto another page, kept inside it.
    fn commit_text_move(&mut self, id: u64, d: Vec2, layout: Layout) {
        let Some(old) = self.doc.texts.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if d.length() < 0.5 {
            return;
        }
        let src_o = layout.page_origin(old.page).to_vec2();
        let center = pos2(old.x + old.width * 0.5, old.y + old.height() * 0.5) + src_o + d;
        let dst = layout.page_at(center).unwrap_or(old.page);
        let local = d + src_o - layout.page_origin(dst).to_vec2();
        let mut new = old.clone();
        new.page = dst;
        new.x = (old.x + local.x).clamp(0.0, (layout.page_w - old.width).max(0.0));
        new.y = (old.y + local.y).clamp(0.0, (layout.page_h - old.height()).max(0.0));
        self.history
            .exec(&mut self.doc, Action::ReplaceText { text: new });
    }

    // ---------------------------------------------------- pages / selection

    /// True while a stroke, erase drag, lasso or selection drag is in progress.
    fn busy(&self) -> bool {
        self.active.is_some()
            || !self.erase_session.is_empty()
            || self.lasso.is_some()
            || self.moving.is_some()
    }

    fn clear_selection(&mut self) {
        self.selection = None;
        self.lasso = None;
        self.moving = None;
    }

    /// Inserts an empty page so that it gets `index` (undoable).
    fn insert_page(&mut self, index: u32) {
        self.clear_selection();
        self.history
            .exec(&mut self.doc, Action::InsertPage { index });
    }

    /// Deletes a page with its content (undoable).
    fn delete_page(&mut self, index: u32) {
        self.clear_selection();
        self.history
            .exec(&mut self.doc, Action::DeletePage { index });
        self.toast(format!("Page {} deleted (Ctrl+Z restores it)", index + 1));
    }

    /// Selects every stroke on `page` with the select tool.
    fn select_all_on_page(&mut self, page: u32) {
        let ids: HashSet<u64> = self
            .doc
            .strokes
            .iter()
            .filter(|s| s.page == page)
            .map(|s| s.id)
            .collect();
        if ids.is_empty() {
            self.toast(format!("Nothing to select on page {}", page + 1));
            return;
        }
        if self.edit.is_some() {
            self.edit = None;
        }
        self.tool = Tool::Select;
        self.saved_tool = None;
        self.selection = self.selection_for(page, ids);
    }

    fn selection_for(&self, page: u32, ids: HashSet<u64>) -> Option<Selection> {
        let mut bounds: Option<Rect> = None;
        for s in self.doc.strokes.iter().filter(|s| ids.contains(&s.id)) {
            let pad = s.width * 0.5;
            let r = Rect::from_min_max(
                pos2(s.bounds[0] - pad, s.bounds[1] - pad),
                pos2(s.bounds[2] + pad, s.bounds[3] + pad),
            );
            bounds = Some(bounds.map_or(r, |b| b.union(r)));
        }
        bounds.map(|bounds| Selection { page, ids, bounds })
    }

    /// Lasso released: pick the strokes that are mostly inside it. A simple
    /// tap selects the stroke under the pen instead.
    fn finish_lasso(&mut self) {
        let Some(l) = self.lasso.take() else { return };
        let mut ids = HashSet::new();
        let path_len: f32 = l.points.windows(2).map(|w| w[0].distance(w[1])).sum();

        if l.points.len() < 4 || path_len < 8.0 {
            if let Some(p) = l.points.first() {
                if let Some(s) = self
                    .doc
                    .strokes
                    .iter()
                    .rev()
                    .find(|s| s.page == l.page && s.hit_test(*p, 4.0))
                {
                    ids.insert(s.id);
                }
            }
        } else {
            let poly_bounds = Rect::from_points(&l.points);
            for s in self.doc.strokes.iter().filter(|s| s.page == l.page) {
                let sb = Rect::from_min_max(
                    pos2(s.bounds[0], s.bounds[1]),
                    pos2(s.bounds[2], s.bounds[3]),
                );
                if !sb.intersects(poly_bounds) {
                    continue;
                }
                let inside = s
                    .points
                    .iter()
                    .filter(|p| point_in_polygon(p.pos2(), &l.points))
                    .count();
                if inside * 2 >= s.points.len().max(1) {
                    ids.insert(s.id);
                }
            }
        }
        self.selection = self.selection_for(l.page, ids);
    }

    /// Drop the dragged selection: possibly onto another page, kept inside it.
    fn commit_move(&mut self, layout: Layout) {
        let Some(m) = self.moving.take() else { return };
        let Some(sel) = self.selection.as_mut() else {
            return;
        };
        let d = m.delta;
        if d.length() < 0.5 {
            return;
        }
        let src_o = layout.page_origin(sel.page).to_vec2();
        let center = sel.bounds.center() + src_o + d;
        let dst = layout.page_at(center).unwrap_or(sel.page);
        let dst_o = layout.page_origin(dst).to_vec2();

        // Delta expressed in the destination page's space.
        let mut local = d + src_o - dst_o;
        let nb = sel.bounds.translate(local);
        if nb.width() <= layout.page_w {
            if nb.min.x < 0.0 {
                local.x -= nb.min.x;
            } else if nb.max.x > layout.page_w {
                local.x -= nb.max.x - layout.page_w;
            }
        }
        if nb.height() <= layout.page_h {
            if nb.min.y < 0.0 {
                local.y -= nb.min.y;
            } else if nb.max.y > layout.page_h {
                local.y -= nb.max.y - layout.page_h;
            }
        }

        let ids: Vec<u64> = sel.ids.iter().copied().collect();
        sel.bounds = sel.bounds.translate(local);
        sel.page = dst;
        self.history.exec(
            &mut self.doc,
            Action::MoveStrokes {
                ids,
                to_page: dst,
                dx: local.x,
                dy: local.y,
            },
        );
    }

    fn delete_selection(&mut self) {
        let Some(sel) = self.selection.take() else {
            return;
        };
        self.lasso = None;
        self.moving = None;
        let mut inverses = Vec::new();
        for id in sel.ids {
            inverses.push(Action::RemoveStroke { id }.apply(&mut self.doc));
        }
        if !inverses.is_empty() {
            self.history
                .push_undo(Action::Batch(inverses.into_iter().rev().collect()));
        }
    }

    fn handle_select(
        &mut self,
        resp: &egui::Response,
        view: &View,
        press: Option<Pos2>,
        moves: &[Pos2],
    ) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = press.or(resp.interact_pointer_pos()) {
                let c = view.screen_to_content(pos);
                let inside = self.selection.as_ref().map_or(false, |s| {
                    s.bounds
                        .translate(view.layout.page_origin(s.page).to_vec2())
                        .contains(c)
                });
                if inside {
                    self.moving = Some(MoveDrag {
                        start: c,
                        delta: Vec2::ZERO,
                    });
                } else {
                    self.selection = None;
                    if let Some(page) = view.layout.page_at(c) {
                        let p = to_page_point(view, page, pos, 1.0).pos2();
                        self.lasso = Some(Lasso {
                            page,
                            points: vec![p],
                        });
                    }
                }
            }
        } else if resp.dragged() || resp.drag_stopped() {
            if let Some(m) = self.moving.as_mut() {
                if let Some(last) = moves.last() {
                    m.delta = view.screen_to_content(*last) - m.start;
                }
            } else if let Some(l) = self.lasso.as_mut() {
                for pos in moves {
                    let p = to_page_point(view, l.page, *pos, 1.0).pos2();
                    if l.points.last().map_or(true, |q| q.distance(p) > 1.5) {
                        l.points.push(p);
                    }
                }
            }
        }
        if !resp.dragged() {
            if self.moving.is_some() {
                self.commit_move(view.layout);
            } else if self.lasso.is_some() {
                self.finish_lasso();
            }
        }
    }

    fn stroke_params(&self) -> Option<(StrokeKind, [u8; 4], f32)> {
        match self.tool {
            Tool::Pen => {
                let s = self.settings.pen;
                Some((
                    StrokeKind::Pen,
                    [s.color[0], s.color[1], s.color[2], 255],
                    s.width,
                ))
            }
            Tool::Highlighter => {
                let s = self.settings.highlighter;
                Some((
                    StrokeKind::Highlighter,
                    [s.color[0], s.color[1], s.color[2], HIGHLIGHTER_ALPHA],
                    s.width,
                ))
            }
            Tool::Eraser | Tool::Select | Tool::Text => None,
        }
    }

    fn handle_ink(
        &mut self,
        resp: &egui::Response,
        view: &View,
        press: Option<Pos2>,
        moves: &[Pos2],
    ) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let (Some(pos), Some((kind, color, width))) =
                (press.or(resp.interact_pointer_pos()), self.stroke_params())
            {
                // A stroke belongs to the page it starts on; the gaps and
                // margins between pages are not writable.
                if let Some(page) = view.layout.page_at(view.screen_to_content(pos)) {
                    let mut builder =
                        StrokeBuilder::new(MIN_POINT_DIST, SMOOTH_ALPHA_MIN, SMOOTH_SPEED_REF);
                    builder.begin(to_page_point(view, page, pos, self.pen_pressure()));
                    self.active = Some(ActiveStroke {
                        page,
                        kind,
                        color,
                        width,
                        builder,
                    });
                }
            }
        } else if resp.dragged() || resp.drag_stopped() {
            let pressure = self.pen_pressure();
            if let Some(active) = self.active.as_mut() {
                let page = active.page;
                for pos in moves {
                    active
                        .builder
                        .push(to_page_point(view, page, *pos, pressure));
                }
            }
        }
        if self.active.is_some() && !resp.dragged() {
            self.commit_active();
        }
    }

    fn commit_active(&mut self) {
        if let Some(a) = self.active.take() {
            let points = a.builder.finish();
            if points.is_empty() {
                return;
            }
            let id = self.doc.alloc_id();
            let stroke = Stroke::new(id, a.page, a.kind, a.color, a.width, points);
            let index = self.doc.strokes.len();
            self.history
                .exec(&mut self.doc, Action::AddStroke { index, stroke });
        }
    }

    fn handle_erase(
        &mut self,
        resp: &egui::Response,
        view: &View,
        press: Option<Pos2>,
        moves: &[Pos2],
    ) {
        if resp.drag_started_by(PointerButton::Primary) {
            self.last_erase_pos = None;
            if let Some(p) = press.or(resp.interact_pointer_pos()) {
                self.erase_along(view.screen_to_content(p));
            }
        } else if resp.dragged() || resp.drag_stopped() {
            for p in moves {
                self.erase_along(view.screen_to_content(*p));
            }
        }
        if !resp.dragged() {
            self.finish_erase();
        }
    }

    /// Erases along the path from the previous cursor position to `target`
    /// (content space). Intermediate samples prevent fast flicks from
    /// skipping over strokes.
    fn erase_along(&mut self, target: Pos2) {
        let radius = self.settings.eraser_radius;
        let from = self.last_erase_pos.unwrap_or(target);
        let step = (radius * 0.5).max(1.0);
        let steps = ((from.distance(target) / step).ceil() as usize).max(1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let c = from + (target - from) * t;
            self.erase_at(c, radius);
        }
        self.last_erase_pos = Some(target);
    }

    fn erase_at(&mut self, center: Pos2, radius: f32) {
        let layout = Layout {
            page_w: self.doc.page_width,
            page_h: self.doc.page_height,
            count: self.doc.page_count,
        };
        let mut hits: Vec<u64> = Vec::new();
        // The eraser circle may touch two neighbouring pages near the gap.
        for page in layout.pages_overlapping(center.y - radius, center.y + radius) {
            let local = center - layout.page_origin(page).to_vec2();
            hits.extend(
                self.doc
                    .strokes
                    .iter()
                    .filter(|s| s.page == page && s.hit_test(local, radius))
                    .map(|s| s.id),
            );
        }
        // A text box is erased when the eraser centre is inside it.
        let mut text_hits: Vec<u64> = Vec::new();
        for page in layout.pages_overlapping(center.y - radius, center.y + radius) {
            let local = center - layout.page_origin(page).to_vec2();
            text_hits.extend(
                self.doc
                    .texts
                    .iter()
                    .filter(|t| t.page == page && t.rect().contains(local))
                    .map(|t| t.id),
            );
        }
        for id in text_hits {
            let inverse = Action::RemoveText { id }.apply(&mut self.doc);
            self.erase_session.push(inverse);
        }
        for id in hits {
            let inverse = Action::RemoveStroke { id }.apply(&mut self.doc);
            self.erase_session.push(inverse);
        }
    }

    /// Records everything erased during one drag as a single undo step.
    fn finish_erase(&mut self) {
        self.last_erase_pos = None;
        if self.erase_session.is_empty() {
            return;
        }
        let inverses = std::mem::take(&mut self.erase_session);
        // Undo must re-insert in the reverse order of removal.
        self.history
            .push_undo(Action::Batch(inverses.into_iter().rev().collect()));
    }

    // ---------------------------------------------------------------- UI

    /// Writing-area rectangle, zoom scale and whether a horizontal scrollbar
    /// is needed. The vertical scrollbar always occupies the right edge.
    fn canvas_geometry(&self, full: Rect, layout: &Layout) -> (Rect, f32, bool) {
        let sb = scrollbar::THICKNESS;
        let w = (full.width() - sb).max(50.0);
        let mut rect = Rect::from_min_size(full.min, vec2(w, full.height()));
        let mut scale = View::scale_for(self.settings.zoom, rect, layout);
        let hbar = layout.content_w() * scale > rect.width() + 0.5;
        if hbar {
            rect = Rect::from_min_size(full.min, vec2(w, (full.height() - sb).max(50.0)));
            scale = View::scale_for(self.settings.zoom, rect, layout);
        }
        (rect, scale, hbar)
    }

    /// Mouse wheel and touchpad scrolling over the canvas. Wheel notches
    /// glide and accelerate; precise touchpad deltas are applied directly.
    /// Ctrl / Cmd + wheel is zoom and is handled elsewhere.
    /// Scrolls to `target`: eased with smooth scrolling, at once without.
    fn ease_scroll(&mut self, target: Vec2) {
        if self.settings.smooth_scroll {
            self.kinetic.ease_to(target);
        } else {
            self.kinetic.stop();
            self.scroll = target;
        }
    }

    fn wheel_input(&mut self, ui: &egui::Ui, canvas: Rect, scale: f32, now: f64) {
        let line = ui.ctx().options(|o| o.line_scroll_speed);
        let wheel: Vec<_> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                    } => Some((*unit, *delta, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (unit, delta, m) in wheel {
            if m.ctrl || m.command || m.mac_cmd {
                continue;
            }
            // Same conversion as egui's own scroll delta.
            let mut d = match unit {
                MouseWheelUnit::Point => delta,
                MouseWheelUnit::Line => delta * line,
                MouseWheelUnit::Page => delta * canvas.height(),
            };
            if m.shift {
                d = vec2(d.x + d.y, 0.0);
            }
            // Positive deltas move the content down/right, i.e. scroll up/left.
            let content = -d * self.settings.scroll_speed / scale;
            if unit == MouseWheelUnit::Point && d.length() < 8.0 || !self.settings.smooth_scroll {
                self.kinetic.stop();
                self.scroll += content;
            } else {
                self.kinetic.notch(content, now);
            }
        }
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        let layout = Layout {
            page_w: self.doc.page_width,
            page_h: self.doc.page_height,
            count: self.doc.page_count.max(1),
        };
        ui.painter().rect_filled(full, 0.0, render::PADDING_COLOR);

        // ---- geometry and zoom
        let (mut canvas, mut scale, mut hbar) = self.canvas_geometry(full, &layout);
        let idle = !self.busy();

        // Ctrl + wheel / pinch zoom, anchored under the pointer; zoom from the
        // menu, anchored at the centre.
        let request = self.zoom_request.take();
        if idle && (request.is_some() || ui.rect_contains_pointer(canvas)) {
            let zd = request.unwrap_or_else(|| ui.input(|i| i.zoom_delta()));
            if (zd - 1.0).abs() > 1e-4 {
                let anchor = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| request.is_none() && canvas.contains(*p))
                    .unwrap_or_else(|| canvas.center());
                let before = View::new(canvas, self.scroll, scale, layout);
                let c = before.screen_to_content(anchor);
                let pct = (scale * zd * 100.0).round().clamp(25.0, 400.0) as u32;
                self.settings.zoom = ZoomMode::Percent(pct);
                (canvas, scale, hbar) = self.canvas_geometry(full, &layout);
                let pad = View::pad_x_for(canvas.width(), layout.content_w(), scale);
                self.scroll.x = c.x - (anchor.x - canvas.left() - pad) / scale;
                self.scroll.y = c.y - (anchor.y - canvas.top() - TOP_PAD_PX) / scale;
                self.kinetic.stop();
            }
        }
        self.hbar_visible = hbar;
        self.view_scale = scale;

        let canvas_resp = ui.allocate_rect(canvas, Sense::drag());

        // A press on the canvas (outside the editor, which sits on top) ends text editing.
        if self.edit.is_some() && canvas_resp.drag_started() {
            self.commit_edit(ui.ctx());
        }

        // ---- scrolling: wheel / touchpad / keys, with momentum
        let (now, dt) = ui.input(|i| (i.time, i.stable_dt.min(1.0 / 30.0)));
        let max = View::max_scroll(canvas, scale, &layout);
        if !idle {
            // Never move the page under a stroke being drawn.
            self.kinetic.stop();
        }
        if idle && ui.rect_contains_pointer(full) {
            self.wheel_input(ui, canvas, scale, now);
        }
        if !ui.ctx().wants_keyboard_input() {
            let (pgdn, pgup, home, end) = ui.input(|i| {
                (
                    i.key_pressed(Key::PageDown),
                    i.key_pressed(Key::PageUp),
                    i.key_pressed(Key::Home),
                    i.key_pressed(Key::End),
                )
            });
            // Repeated presses continue from where the running ease ends.
            let base = self.kinetic.target().unwrap_or(self.scroll);
            let page_len = canvas.height() / scale * 0.9;
            if pgdn {
                self.ease_scroll(base + vec2(0.0, page_len));
            }
            if pgup {
                self.ease_scroll(base - vec2(0.0, page_len));
            }
            if home {
                self.ease_scroll(vec2(base.x, 0.0));
            }
            if end {
                self.ease_scroll(vec2(base.x, max.y));
            }
        }
        if let Some(y) = self.scroll_to.take() {
            self.ease_scroll(vec2(self.scroll.x, y));
        }
        self.kinetic.step(&mut self.scroll, dt, max, scale);
        self.scroll = vec2(
            self.scroll.x.clamp(0.0, max.x),
            self.scroll.y.clamp(0.0, max.y),
        );

        let swallowed = std::mem::take(&mut self.swallow_press);

        // ---- drag on the grey area around the pages = pan (touch / pen friendly)
        let press = ui.input(|i| i.pointer.press_origin());
        if idle && !swallowed && canvas_resp.drag_started_by(PointerButton::Primary) {
            let start = press.or(canvas_resp.interact_pointer_pos());
            let v = View::new(canvas, self.scroll, scale, layout);
            self.panning = start.map_or(false, |p| {
                v.layout.page_at(v.screen_to_content(p)).is_none()
            });
        }
        if self.panning {
            if canvas_resp.dragged() {
                let before = self.scroll;
                self.scroll -= canvas_resp.drag_delta() / scale;
                self.scroll = vec2(
                    self.scroll.x.clamp(0.0, max.x),
                    self.scroll.y.clamp(0.0, max.y),
                );
                self.kinetic.drag(self.scroll - before, now);
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else {
                // Let go: the view keeps gliding with the release velocity.
                self.panning = false;
                if self.settings.smooth_scroll {
                    self.kinetic.release(now, scale);
                }
            }
        }
        // Hovering the grey area keeps the normal arrow: on Windows "grab"
        // has no hand cursor and shows the four-arrow move cross instead.

        // ---- scrollbars (interaction first, so the view below is final)
        let vbar_rect = Rect::from_min_max(
            pos2(canvas.right(), full.top()),
            pos2(full.right(), canvas.bottom()),
        );
        let hbar_rect = Rect::from_min_max(
            pos2(full.left(), canvas.bottom()),
            pos2(canvas.right(), full.bottom()),
        );
        let vstate = scrollbar::update(
            ui,
            Id::new("vbar"),
            vbar_rect,
            true,
            &mut self.scroll.y,
            max.y,
            canvas.height() / scale,
            layout.content_h(scale),
            &mut self.vbar_grab,
        );
        let hstate = hbar.then(|| {
            scrollbar::update(
                ui,
                Id::new("hbar"),
                hbar_rect,
                false,
                &mut self.scroll.x,
                max.x,
                canvas.width() / scale,
                layout.content_w(),
                &mut self.hbar_grab,
            )
        });

        if self.vbar_grab.is_some() || self.hbar_grab.is_some() {
            self.kinetic.stop();
        }
        if self.kinetic.is_active() {
            ui.ctx().request_repaint();
        }

        let view = View::new(canvas, self.scroll, scale, layout);

        // ---- pen / mouse input
        self.update_pressure(ui.ctx());
        let mut moves: Vec<Pos2> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::PointerMoved(p) => Some(*p),
                    _ => None,
                })
                .collect()
        });
        if moves.is_empty() {
            moves.extend(canvas_resp.interact_pointer_pos());
        }
        // ---- right click: context menu for what is under the pointer
        let right_click = ui.input(|i| i.pointer.secondary_clicked());
        if right_click
            && !self.busy()
            && self.saved_tool.is_none()
            && ui.rect_contains_pointer(canvas)
        {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                let c = view.screen_to_content(pos);
                let page = view.layout.page_at(c);
                let on_selection = match (&self.selection, page) {
                    (Some(sel), Some(p)) if sel.page == p => {
                        let o = view.layout.page_origin(p);
                        sel.bounds.expand(6.0).contains(pos2(c.x - o.x, c.y - o.y))
                    }
                    _ => false,
                };
                self.context_menu = Some(ContextMenu {
                    pos,
                    page,
                    text: self.text_at(&view.layout, c),
                    on_selection,
                });
                ui.ctx().request_repaint();
            }
        }

        if !self.panning && !swallowed {
            match self.tool {
                Tool::Eraser => self.handle_erase(&canvas_resp, &view, press, &moves),
                Tool::Select => self.handle_select(&canvas_resp, &view, press, &moves),
                Tool::Text => self.handle_text(&canvas_resp, &view, press, &moves),
                Tool::Pen | Tool::Highlighter => {
                    self.handle_ink(&canvas_resp, &view, press, &moves)
                }
            }
        }

        // ---- painting
        let painter = ui.painter_at(canvas);
        render::paint_background(&painter, &view, &self.doc.page);
        let ppp = ui.ctx().pixels_per_point();
        let text_move = self.text_press.as_ref().and_then(|tp| {
            if tp.moved {
                tp.hit.map(|id| (id, tp.now - tp.start))
            } else {
                None
            }
        });
        render::paint_texts(
            &painter,
            &view,
            &self.doc.texts,
            self.edit.as_ref().map(|e| e.id),
            text_move,
            self.tool == Tool::Text,
        );

        let move_off = self.moving.as_ref().map_or(Vec2::ZERO, |m| m.delta);
        let moved = self.selection.as_ref().map(|s| Moved {
            ids: &s.ids,
            page: s.page,
            offset: move_off,
        });
        render::paint_strokes(
            &painter,
            &view,
            &self.doc.strokes,
            &mut self.cache,
            ppp,
            moved.as_ref(),
        );

        // Selection box and lasso outline.
        let blue = color::ACCENT;
        if let Some(sel) = &self.selection {
            let r = sel
                .bounds
                .translate(layout.page_origin(sel.page).to_vec2() + move_off);
            let rect =
                Rect::from_min_max(view.content_to_screen(r.min), view.content_to_screen(r.max));
            painter.rect_filled(rect, 2.0, color::accent_alpha(28));
            let path = [
                rect.left_top(),
                rect.right_top(),
                rect.right_bottom(),
                rect.left_bottom(),
                rect.left_top(),
            ];
            painter.extend(egui::Shape::dashed_line(
                &path,
                egui::Stroke::new(1.5, blue),
                6.0,
                4.0,
            ));
        }
        if let Some(l) = &self.lasso {
            let o = layout.page_origin(l.page).to_vec2();
            let pts: Vec<Pos2> = l
                .points
                .iter()
                .map(|p| view.content_to_screen(*p + o))
                .collect();
            if pts.len() >= 2 {
                painter.extend(egui::Shape::dashed_line(
                    &pts,
                    egui::Stroke::new(1.5, blue),
                    5.0,
                    4.0,
                ));
            }
        }
        if let Some(a) = &self.active {
            let pts = a.builder.preview_points(self.settings.predict_pen);
            render::paint_live(&painter, &view, a.page, a.kind, a.color, a.width, &pts, ppp);
        }
        if self.tool == Tool::Eraser {
            if let Some(pos) = canvas_resp
                .hover_pos()
                .or(canvas_resp.interact_pointer_pos())
            {
                render::paint_eraser_cursor(&painter, pos, self.settings.eraser_radius * scale);
            }
        }

        // Rubber band while dragging out a new text box.
        if let Some(tp) = &self.text_press {
            if tp.hit.is_none() && tp.moved && tp.page.is_some() {
                let r = Rect::from_two_pos(
                    view.content_to_screen(tp.start),
                    view.content_to_screen(tp.now),
                );
                let path = [
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                    r.left_top(),
                ];
                painter.extend(egui::Shape::dashed_line(
                    &path,
                    egui::Stroke::new(1.5, blue),
                    6.0,
                    4.0,
                ));
            }
        }

        // In-place editor of the text box being typed into (a real egui
        // TextEdit: cursor, selection, clipboard, IME, its own Ctrl+Z).
        let mut commit_now = false;
        if let Some(e) = self.edit.as_mut() {
            // Toolbar size / colour changes apply live.
            e.size = self.settings.text.width;
            e.color = self.settings.text.color;

            let origin = view.page_origin_screen(e.page) + vec2(e.x, e.y) * scale;
            let w = e.width * scale;
            let shown = Rect::from_min_size(origin, vec2(w, e.last_h.max(e.size * scale * 1.3)));
            painter.rect_filled(
                shown.expand(3.0),
                3.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, 215),
            );
            painter.rect_stroke(shown.expand(3.0), 3.0, egui::Stroke::new(1.5, blue));

            let id = Id::new(("text_edit", e.id));
            let color = Color32::from_rgb(e.color[0], e.color[1], e.color[2]);
            let widget = egui::TextEdit::multiline(&mut e.buf)
                .id(id)
                .font(FontId::proportional((e.size * scale).max(6.0)))
                .text_color(color)
                .desired_width(w)
                .desired_rows(1)
                .frame(false)
                .lock_focus(true);
            let resp = ui
                .allocate_new_ui(
                    egui::UiBuilder::new().max_rect(Rect::from_min_size(origin, vec2(w, 10.0))),
                    |ui| ui.add(widget),
                )
                .inner;
            e.last_h = resp.rect.height();

            if e.focus_pending {
                resp.request_focus();
                e.focus_pending = false;
            } else if !resp.has_focus() {
                // Focus left the editor. A press on a toolbar button only steals it
                // temporarily (take it back); another text field or Esc ends editing.
                let other = ui.ctx().memory(|m| m.focused()).map_or(false, |f| f != id);
                let pressed = ui.input(|i| i.pointer.any_pressed());
                if other || !pressed {
                    commit_now = true;
                } else {
                    resp.request_focus();
                }
            }
        }
        if commit_now {
            self.commit_edit(ui.ctx());
        }

        // "Add page" below the last page, the same dashed tile as in the
        // pages panel, as wide as the page.
        let last = view.page_screen_rect(layout.count - 1);
        let btn = Rect::from_min_size(pos2(last.left(), last.bottom() + 18.0), vec2(last.width(), 40.0));
        if btn.intersects(canvas) {
            let mut tile = ui.new_child(egui::UiBuilder::new().max_rect(canvas));
            tile.set_clip_rect(canvas);
            if theme::add_tile(&mut tile, btn, Id::new("add_page"), "Add page").clicked() {
                self.insert_page(self.doc.page_count);
            }
        }

        // Transient status message (saved / exported / errors).
        if let Some(t) = self.status_at {
            if t.elapsed() < Duration::from_millis(3500) && !self.status.is_empty() {
                theme::paint_toast(
                    &painter,
                    pos2(canvas.center().x, canvas.bottom() - 90.0),
                    &self.status,
                );
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            }
        }

        // Scrollbars on top; the vertical one shows a page indicator while dragged.
        let center_y = view.screen_to_content(canvas.center()).y;
        let cur = layout.page_index_at_y(center_y);
        self.current_page = cur;
        let label = format!("Page {} / {}", cur + 1, layout.count);
        let full_painter = ui.painter();
        scrollbar::paint(full_painter, vbar_rect, true, &vstate, Some(&label));
        if let Some(h) = &hstate {
            scrollbar::paint(full_painter, hbar_rect, false, h, None);
        }
    }

    /// Collapsible left panel: previews of all pages. Click = jump to the page,
    /// drag = reorder, "x" (on hover) = delete. Everything is undoable.
    /// The left panel: a strip with the pages and settings buttons (the two
    /// left notches joined), and the shown view to its right. Each view is
    /// its own resizable panel, so each keeps its own width.
    fn left_panel(&mut self, ctx: &egui::Context) {
        let view = self.settings.left_panel;
        if view != LeftPanel::Pages {
            self.page_drag = None;
        }
        if view == LeftPanel::Closed {
            return;
        }
        let (top, bottom) = (toolbar::notch_margin(true), toolbar::notch_margin(false));
        let mut clicked = None;
        egui::SidePanel::left("left_panel_strip")
            .resizable(false)
            .exact_width(toolbar::STRIP_WIDTH)
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(color::CHROME)
                    .inner_margin(egui::Margin {
                        left: top.left,
                        right: top.right,
                        top: top.top,
                        bottom: bottom.bottom,
                    }),
            )
            .show(ctx, |ui| {
                toolbar::notch_buttons_style(ui);
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    if toolbar::settings_button(ui, view == LeftPanel::Settings) {
                        clicked = Some(LeftPanel::Settings);
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        if toolbar::pages_button(ui, view == LeftPanel::Pages) {
                            clicked = Some(LeftPanel::Pages);
                        }
                    });
                });
            });
        match view {
            LeftPanel::Pages => self.pages_view(ctx),
            LeftPanel::Settings => self.note_settings_view(ctx),
            LeftPanel::Closed => {}
        }
        if let Some(v) = clicked {
            self.settings.left_panel.toggle(v);
        }
    }

    /// Frame of a left panel view: the strip's surface, content starting
    /// level with the top button.
    fn view_frame() -> egui::Frame {
        let top = toolbar::notch_margin(true);
        egui::Frame::none()
            .fill(color::CHROME)
            .inner_margin(egui::Margin {
                left: space::S,
                right: space::S,
                top: top.top,
                bottom: space::S,
            })
    }

    /// Hairline between the button strip and a left panel view.
    fn view_divider(ui: &egui::Ui) {
        let r = ui.max_rect();
        let x = r.left() - space::S + 0.5;
        let y = (r.top() + space::XS)..=(r.bottom() - space::XS);
        ui.painter()
            .vline(x, y, egui::Stroke::new(1.0, color::BORDER));
    }

    fn pages_view(&mut self, ctx: &egui::Context) {
        let layout = Layout {
            page_w: self.doc.page_width,
            page_h: self.doc.page_height,
            count: self.doc.page_count.max(1),
        };
        let ppp = ctx.pixels_per_point();
        let current = self.current_page;
        let blue = color::ACCENT;

        // Disjoint field borrows, released again after `show`.
        let strokes = &self.doc.strokes;
        let texts = &self.doc.texts;
        let cache = &mut self.thumb_cache;
        let drag = &mut self.page_drag;

        // Scroll the list to the current page when it changes.
        let follow = self.panel_page != Some(current) && drag.is_none();
        self.panel_page = Some(current);
        let mut jump: Option<u32> = None;
        let mut delete: Option<u32> = None;
        let mut add = false;
        let mut insert: Option<u32> = None;
        let mut reorder: Option<(u32, u32)> = None;

        // The concave joins with the title bar and the bottom edge are drawn
        // by `toolbar::show_corners`.
        egui::SidePanel::left("left_panel_pages")
            .resizable(true)
            .default_width(PAGES_WIDTH)
            .width_range(PAGES_WIDTHS)
            .show_separator_line(false)
            .frame(Self::view_frame())
            .show(ctx, |ui| {
                Self::view_divider(ui);

                // Page cards, centred; the scroll bar floats over the content,
                // so the full width is usable.
                let avail = ui.available_width();
                let thumb_w = (avail - 2.0 * space::M).clamp(40.0, THUMB_MAX_W);
                let thumb_h = thumb_w * layout.page_h / layout.page_w;
                let scale = thumb_w / layout.page_w;
                cache.begin_frame(scale, ppp);
                let aa = 1.0 / ppp;
                let pointer = ui.input(|i| i.pointer.interact_pos());
                // Room around the page inside its card (more above, so the
                // hover fill and the current-page outline have space).
                const PAD_TOP: f32 = 12.0;
                const PAD: f32 = 6.0;
                const NUMBER_H: f32 = 18.0;

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let mut rects: Vec<Rect> = Vec::with_capacity(layout.count as usize);
                        for i in 0..layout.count {
                            let (card, _) = ui.allocate_exact_size(
                                vec2(avail, PAD_TOP + thumb_h + PAD + NUMBER_H + PAD),
                                Sense::hover(),
                            );
                            let rect = Rect::from_min_size(
                                pos2(card.center().x - thumb_w * 0.5, card.top() + PAD_TOP),
                                vec2(thumb_w, thumb_h),
                            );
                            rects.push(rect);
                            // The whole card (page and number) is the target.
                            let resp = ui.interact(
                                card,
                                ui.id().with(("page_card", i)),
                                Sense::click_and_drag(),
                            );
                            if follow && i == current {
                                ui.scroll_to_rect(card, None);
                            }

                            if ui.is_rect_visible(card) {
                                let p = ui.painter();
                                if resp.hovered() && drag.is_none() {
                                    p.rect_filled(
                                        card,
                                        radius::MEDIUM,
                                        color::HOVER.gamma_multiply(0.6),
                                    );
                                }
                                p.rect_filled(
                                    rect.translate(vec2(0.0, 2.0)),
                                    2.0,
                                    Color32::from_black_alpha(90),
                                );
                                p.rect_filled(rect, 0.0, render::PAGE_COLOR);
                                let clipped = ui.painter_at(rect);
                                render::paint_page_strokes(
                                    &clipped,
                                    rect.min.to_vec2(),
                                    scale,
                                    strokes,
                                    i,
                                    cache,
                                    aa,
                                );
                                render::paint_page_texts(
                                    &clipped,
                                    rect.min.to_vec2(),
                                    scale,
                                    texts,
                                    i,
                                );
                                let p = ui.painter();
                                if i == current {
                                    p.rect_stroke(
                                        rect.expand(2.5),
                                        3.0,
                                        egui::Stroke::new(2.0, blue),
                                    );
                                } else {
                                    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, color::BORDER));
                                }
                                // Page number; the current one bright and semi-bold.
                                let center =
                                    pos2(card.center().x, rect.bottom() + PAD + NUMBER_H * 0.5);
                                let text = format!("{}", i + 1);
                                let (font, ink) = if i == current {
                                    (theme::font_strong(theme::size::CAPTION), color::TEXT_STRONG)
                                } else {
                                    (theme::font(theme::size::CAPTION), color::TEXT_MUTED)
                                };
                                p.text(center, Align2::CENTER_CENTER, text, font, ink);
                            }

                            // Delete button, registered after the card so it wins hit-testing.
                            let del_rect = Rect::from_center_size(
                                rect.right_top() + vec2(-13.0, 13.0),
                                vec2(22.0, 22.0),
                            );
                            let del = ui.interact(
                                del_rect,
                                ui.id().with(("page_del", i)),
                                Sense::click(),
                            );
                            if ui.rect_contains_pointer(card) && drag.is_none() {
                                let p = ui.painter();
                                let fill = if del.hovered() {
                                    color::DANGER
                                } else {
                                    color::CHROME
                                };
                                p.circle(
                                    del_rect.center(),
                                    10.0,
                                    fill,
                                    egui::Stroke::new(1.0, color::BORDER),
                                );
                                let a = 3.5;
                                let x = egui::Stroke::new(
                                    1.6,
                                    if del.hovered() {
                                        color::ON_ACCENT
                                    } else {
                                        color::TEXT
                                    },
                                );
                                let c = del_rect.center();
                                p.line_segment([c + vec2(-a, -a), c + vec2(a, a)], x);
                                p.line_segment([c + vec2(-a, a), c + vec2(a, -a)], x);
                            }
                            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                            resp.context_menu(|ui| {
                                use egui_phosphor::regular as icon;
                                if menus::entry(ui, icon::ARROW_SQUARE_OUT, "Go to page", "", true)
                                {
                                    jump = Some(i);
                                    ui.close_menu();
                                }
                                ui.separator();
                                if menus::entry(
                                    ui,
                                    icon::ARROW_LINE_UP,
                                    "Insert page before",
                                    "",
                                    true,
                                ) {
                                    insert = Some(i);
                                    ui.close_menu();
                                }
                                if menus::entry(
                                    ui,
                                    icon::ARROW_LINE_DOWN,
                                    "Insert page after",
                                    "",
                                    true,
                                ) {
                                    insert = Some(i + 1);
                                    ui.close_menu();
                                }
                                ui.separator();
                                if menus::entry(
                                    ui,
                                    icon::TRASH,
                                    &format!("Delete page {}", i + 1),
                                    "",
                                    true,
                                ) {
                                    delete = Some(i);
                                    ui.close_menu();
                                }
                            });
                            if del.clicked() {
                                delete = Some(i);
                            } else if resp.clicked() {
                                jump = Some(i);
                            }
                            if resp.drag_started() {
                                *drag = Some(i);
                            }
                        }
                        ui.add_space(space::XS);
                        let (row, _) = ui.allocate_exact_size(vec2(avail, 36.0), Sense::hover());
                        let tile = Rect::from_center_size(row.center(), vec2(thumb_w, 36.0));
                        if theme::add_tile(ui, tile, Id::new("add_page_tile"), "Add page").clicked()
                        {
                            add = true;
                        }

                        // Drag & drop reordering.
                        if let Some(from) = *drag {
                            match pointer {
                                Some(p) if !rects.is_empty() => {
                                    let slot = rects.iter().filter(|r| r.center().y < p.y).count();
                                    let y = if slot < rects.len() {
                                        rects[slot].top() - 3.0
                                    } else {
                                        rects[rects.len() - 1].bottom() + 3.0
                                    };
                                    let r0 = rects[0];
                                    ui.painter().line_segment(
                                        [pos2(r0.left(), y), pos2(r0.right(), y)],
                                        egui::Stroke::new(3.0, blue),
                                    );

                                    // Auto-scroll near the edges of the list.
                                    let clip = ui.clip_rect();
                                    if p.y < clip.top() + 30.0 {
                                        ui.scroll_with_delta(vec2(0.0, 14.0));
                                    } else if p.y > clip.bottom() - 30.0 {
                                        ui.scroll_with_delta(vec2(0.0, -14.0));
                                    }

                                    // Floating label under the pen.
                                    let ghost = ui.ctx().layer_painter(egui::LayerId::new(
                                        egui::Order::Tooltip,
                                        Id::new("page_drag_ghost"),
                                    ));
                                    theme::paint_toast(
                                        &ghost,
                                        p + vec2(56.0, 20.0),
                                        &format!("Page {}", from + 1),
                                    );

                                    if ui.input(|i| !i.pointer.any_down()) {
                                        let slot = slot as u32;
                                        let to = if slot > from { slot - 1 } else { slot };
                                        if to != from {
                                            reorder = Some((from, to));
                                        }
                                        *drag = None;
                                    }
                                }
                                _ => *drag = None,
                            }
                        }
                    });
            });

        if let Some((from, to)) = reorder {
            self.clear_selection();
            self.history
                .exec(&mut self.doc, Action::MovePage { from, to });
            self.scroll_to = Some(layout.page_top(to) - MARGIN);
        }
        if let Some(index) = delete {
            self.delete_page(index);
        }
        if let Some(i) = jump {
            self.scroll_to = Some(layout.page_top(i) - MARGIN);
        }
        if add {
            self.insert_page(self.doc.page_count);
        }
        if let Some(index) = insert {
            self.insert_page(index);
            self.scroll_to = Some(layout.page_top(index) - MARGIN);
        }
    }

    /// One frame of the note editor.
    fn editor_screen(&mut self, ctx: &egui::Context) {
        self.handle_shortcuts(ctx);

        // Pen side button = eraser while held. Wayland pens report it through
        // `eraser_native`, Windows pens through `win_pen`; on X11 it can arrive
        // as the middle mouse button. (The right button opens the context
        // menu.) Never switch in the middle of a stroke.
        if !self.busy() && self.edit.is_none() {
            let button_held = ctx.input(|i| i.pointer.middle_down());
            let win = self
                .win_pen
                .as_ref()
                .map(|p| p.buttons())
                .unwrap_or_default();
            // The eraser end always erases; the side button only if enabled.
            let side = win.barrel || button_held;
            let eraser = self.eraser_native || win.eraser_end || (self.settings.side_button_eraser && side);
            self.apply_pen_eraser(eraser);
        }
        // Leaving the text tool ends editing.
        if self.tool != Tool::Text && self.edit.is_some() {
            self.commit_edit(ctx);
        }
        if self.tool != Tool::Text {
            self.text_press = None;
        }

        // Leaving the select tool drops the selection (the temporary eraser
        // from the pen button must not).
        if self.tool != Tool::Select && self.saved_tool != Some(Tool::Select) && !self.busy() {
            self.clear_selection();
        }

        // The toolbar is an Area above the canvas, so egui's hit-testing
        // automatically keeps pen strokes from starting underneath it. It is
        // moved up so it never covers the horizontal scrollbar.
        let bottom_inset = if self.hbar_visible {
            scrollbar::THICKNESS
        } else {
            0.0
        };
        let library_was_open = self.settings.library_panel_open;
        self.left_panel(ctx);
        self.library_panel(ctx);
        if self.screen != Screen::Editor {
            // A note was picked in the library panel and the editor closed.
            return;
        }
        // After the side panels, so the toolbar is centred over the canvas.
        let event = toolbar::show(
            ctx,
            &mut self.tool,
            &mut self.settings,
            self.history.can_undo(),
            self.history.can_redo(),
            bottom_inset,
        );
        // After the pages panel, so the corner buttons sit next to it.
        toolbar::show_corners(ctx, &mut self.settings, bottom_inset);
        // Before the canvas, which must not see the press that closes it.
        self.context_menu_ui(ctx);
        // Rename / delete / new section from the notebook panel.
        self.editor_dialogs(ctx);
        self.export_window(ctx);

        // Show the library as it is on disk now (it may have changed).
        if self.settings.library_panel_open && !library_was_open {
            self.refresh_library();
        }

        match event {
            Some(ToolbarEvent::Undo) => self.undo(),
            Some(ToolbarEvent::Redo) => self.redo(),
            Some(ToolbarEvent::ZoomIn) => self.pending.push(Command::ZoomIn),
            Some(ToolbarEvent::ZoomOut) => self.pending.push(Command::ZoomOut),
            None => {}
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| self.canvas_ui(ui));

        // Autosave shortly after the last change, never in the middle of a stroke.
        self.sync_dirty();
        if self.dirty && !self.busy() {
            let delay = Duration::from_secs_f32(self.settings.autosave_secs.clamp(0.5, 60.0));
            let waited = self.last_change.elapsed();
            if waited >= delay {
                self.save_note(false);
            } else {
                ctx.request_repaint_after(delay - waited);
            }
        }
    }

}

/// Wraps `text` to `width` with egui's font at the box's own size and returns
/// the lines plus the line height (all in document units).
fn layout_lines(ctx: &egui::Context, text: &str, size: f32, width: f32) -> (Vec<String>, f32) {
    let galley = ctx.fonts(|f| {
        f.layout(
            text.to_owned(),
            FontId::proportional(size),
            Color32::BLACK,
            width,
        )
    });
    let lines: Vec<String> = galley
        .rows
        .iter()
        .map(|r| r.glyphs.iter().map(|g| g.chr).collect::<String>())
        .collect();
    let line_h = galley.rows.first().map_or(size * 1.25, |r| r.rect.height());
    (lines, line_h)
}

/// Screen position -> point in the page's space, clamped to the page.
fn to_page_point(view: &View, page: u32, pos: Pos2, pressure: f32) -> Point {
    let c = view.screen_to_content(pos);
    let o = view.layout.page_origin(page);
    Point::new(
        (c.x - o.x).clamp(0.0, view.layout.page_w),
        (c.y - o.y).clamp(0.0, view.layout.page_h),
        pressure,
    )
}

impl eframe::App for InkApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The Wayland display handle is only available through `Frame`, so the
        // native pen source is started lazily on the first frame.
        if !self.pen_init_done {
            self.pen_init_done = true;
            self.pen = WaylandPen::start(ctx, frame);
            self.win_pen = WindowsPen::start(ctx, frame);
            crate::chrome::round_corners(frame);
        }

        // Closing the window while editing: finish the text box and write the note.
        if ctx.input(|i| i.viewport().close_requested()) && self.screen == Screen::Editor {
            if self.edit.is_some() {
                self.commit_edit(ctx);
            }
            self.sync_dirty();
            if self.dirty {
                self.save_note(false);
            }
        }

        let scale = self.settings.ui_scale.clamp(0.5, 2.0);
        if (ctx.zoom_factor() - scale).abs() > 1e-3 && !ctx.input(|i| i.pointer.any_down()) {
            ctx.set_zoom_factor(scale);
        }
        self.menu_shortcuts(ctx);
        self.window_chrome(ctx);
        match self.screen {
            Screen::Library => self.library_screen(ctx),
            Screen::Editor => self.editor_screen(ctx),
        }
        self.finish_chrome(ctx);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.inject_pen_events(ctx, raw_input);
    }

    /// The window background, visible only through gaps between panels:
    /// the bars' colour, so a gap can never show as a dark line.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        color::CHROME.to_normalized_gamma_f32()
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
        // Overwrite the window state saved by earlier versions (eframe's key);
        // an unparsable value makes eframe skip restoring it.
        storage.set_string(WINDOW_STATE_KEY, String::new());
        // eframe calls this periodically and on exit. Only write what is already
        // committed; a text box being typed stays open (it is committed on close).
        if self.screen == Screen::Editor && self.note_path.is_some() {
            self.sync_dirty();
            if self.dirty {
                self.save_note(false);
            }
        }
    }
}
