// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Application state and per-frame orchestration.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, Event, FontId, Id, Key, Modifiers, MouseWheelUnit, PointerButton,
    Pos2, Rect, Sense, TouchPhase, Vec2, pos2, vec2,
};

use tratto_core::geometry::point_in_polygon;
use tratto_core::history::{Action, History};
use tratto_core::library::{self, Library};
use tratto_core::model::{
    Document, Point, Stroke, StrokeKind, TextBox,
};
use tratto_core::pdf_pages::{self, LinkAction, PdfFile, Target};
use tratto_core::storage::{self, NoteMeta, NoteWriter};
use tratto_core::stroke_builder::StrokeBuilder;

use crate::frame_limit::FrameLimiter;
use crate::input::wayland_pen::{PenEvent, WaylandPen};
use crate::input::windows_pen::WindowsPen;
use crate::kinetic::Kinetic;
use crate::pdf_nav::PdfNav;
use crate::pdf_view::PdfTextures;
use crate::render::{self, Layer, Moved, StrokeCache};
use crate::scrollbar;
use crate::settings::{LeftPanel, NoteView, Settings, UpdateMode, ZoomMode};
use crate::theme::{self, color, radius, space};
use crate::toolbar::{self, Tool, ToolbarEvent};
use crate::file_picker::{self, FilePicker};
use crate::update::{self, Updater};
use crate::view::{Layout, MARGIN, TOP_PAD_PX, View};

mod library_panel;
mod library_ui;
mod export_ui;
mod menus;
mod outline_ui;
mod settings_ui;
mod update_ui;

use menus::{Command, ContextMenu, HelpWindows};

/// Width of the left page-preview panel (points).
/// Left panel views: default width and resize range of each (each view
/// remembers its own width).
const PAGES_WIDTH: f32 = 164.0;
/// The narrowest width still fits the "Add page" tile's label.
const PAGES_WIDTHS: std::ops::RangeInclusive<f32> = 150.0..=360.0;
/// Page previews never get wider than this, however wide the panel.
const THUMB_MAX_W: f32 = 200.0;
/// Places kept for Back (the oldest is forgotten).
const NAV_BACK_MAX: usize = 50;
/// Autosave this long after the last change.
/// Key under which eframe stores the window state (not public in eframe).
const WINDOW_STATE_KEY: &str = "window";
const SETTINGS_KEY: &str = "tratto_settings";

/// The settings saved by the last run, read before the window exists:
/// `main` needs some of them to create it. Mirrors eframe's file storage
/// (not public): a RON map of keys to RON strings.
pub fn saved_settings() -> Option<Settings> {
    let file = eframe::storage_dir(crate::APP_ID)?.join("app.ron");
    let kv: std::collections::HashMap<String, String> = ron::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    ron::from_str(kv.get(SETTINGS_KEY)?).ok()
}
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
    /// Caps the frame rate without vsync.
    frame_limit: FrameLimiter,
    pen_down: bool,
    pen_pos: Pos2,
    /// Wayland: where the pen hovers (in range, not touching).
    wl_pen_hover: Option<Pos2>,
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
    /// Notes written on a PDF: its pages as textures.
    pdf_textures: PdfTextures,
    /// The outline and links of the open note's PDF.
    pdf_nav: PdfNav,
    /// Where the view was before each link or outline entry followed
    /// (scroll offsets), for Back.
    nav_back: Vec<Vec2>,
    /// Outline entries shown open (their children visible), by index.
    outline_open: HashSet<usize>,
    /// A note read (or a PDF imported) in the background; a loading screen
    /// covers the window meanwhile.
    loading: Option<Loading>,
    /// To wake the UI from background threads.
    egui_ctx: egui::Context,
    /// Page being dragged in the preview panel.
    page_drag: Option<u32>,
    /// Current page the pages panel last scrolled to.
    panel_page: Option<u32>,
    /// Content-space y to scroll to on the next frame (jump to page).
    scroll_to: Option<f32>,
    /// Page in the middle of the view (highlighted in the panel).
    current_page: u32,
    /// The PDF open in the reader (read-only) instead of a note: its pages
    /// are turned and scrolled by pen, finger, mouse or keys; nothing is
    /// written or selected. `doc` holds it, `note_path` is `None`.
    reader: Option<PathBuf>,
    /// The place to open the next note at when it has none of its own: a
    /// PDF imported from the reader continues where it was being read.
    carry_view: Option<NoteView>,
    /// Pages to turn on the next frame (the reader; negative = back).
    page_turn: i32,
    screen: Screen,
    library: Library,
    lib_ui: library_ui::LibraryUi,
    /// File of the open note.
    note_path: Option<PathBuf>,
    note_meta: NoteMeta,
    /// Appends the open note's changes to its file.
    writer: NoteWriter,
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
    /// New versions from the GitHub releases (checked at startup).
    updater: Updater,
    /// The system file dialog opened by a Browse button, if any.
    file_picker: FilePicker<PathField>,
    /// A Browse button was clicked: the dialog to open at the end of the
    /// frame, where the window (its parent) is at hand.
    pick_request: Option<file_picker::Request<PathField>>,
}

/// Where the path chosen in a file dialog goes.
enum PathField {
    /// The path box of the open library dialog (import, PDF, library folder).
    Library,
    /// The file box of the Export PDF dialog.
    Export,
    /// Used at once (`file_picker::DIRECT`): what the library dialog would
    /// do with it, new notes going to `target_dir`.
    Run { dialog: library_ui::Dialog, target_dir: PathBuf },
    /// Used at once (`file_picker::DIRECT`): the Export PDF dialog exports
    /// to it.
    ExportTo,
}

impl InkApp {
    /// `vsync`: the window presents with vsync.
    pub fn new(cc: &eframe::CreationContext<'_>, vsync: bool) -> Self {
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
            frame_limit: FrameLimiter::new(vsync),
            pen_down: false,
            pen_pos: Pos2::ZERO,
            wl_pen_hover: None,
            saved_tool: None,
            eraser_native: false,
            panning: false,
            selection: None,
            lasso: None,
            moving: None,
            cache: StrokeCache::default(),
            thumb_cache: StrokeCache::default(),
            pdf_textures: PdfTextures::default(),
            pdf_nav: PdfNav::default(),
            nav_back: Vec::new(),
            outline_open: HashSet::new(),
            loading: None,
            egui_ctx: cc.egui_ctx.clone(),
            page_drag: None,
            panel_page: None,
            scroll_to: None,
            current_page: 0,
            reader: None,
            carry_view: None,
            page_turn: 0,
            screen: Screen::Library,
            library: Library::default(),
            lib_ui: library_ui::LibraryUi::default(),
            note_path: None,
            note_meta: NoteMeta::new("Untitled note", true),
            writer: NoteWriter::default(),
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
            os_title: "Tratto".to_owned(),
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
            updater: Updater::default(),
            file_picker: FilePicker::default(),
            pick_request: None,
        };
        if app.settings.updates != UpdateMode::Off {
            app.updater.check(&cc.egui_ctx, app.settings.updates == UpdateMode::Install);
        }
        app.refresh_library();
        // Back where the app was closed: the note that was open, if any.
        let last = app.settings.last_note.clone().filter(|_| app.settings.reopen_last_note).map(PathBuf::from);
        if let Some(path) = last.filter(|p| p.is_file()) {
            if is_pdf(&path) {
                let ctx = cc.egui_ctx.clone();
                app.open_pdf(&ctx, &path);
            } else {
                app.open_note(&path);
            }
        }
        app
    }

    // -------------------------------------------------------------- updates

    /// Tells about a new version found or installed in the background
    /// (failures and "up to date" show only in Help > Check for updates).
    fn poll_update(&mut self) {
        let msg = match self.updater.poll() {
            Some(update::State::Available(r)) => format!("Tratto {} is available (Help > Check for updates)", r.version),
            Some(update::State::Installed(v)) => format!("Tratto {v} is installed: it starts next time (or Help > Check for updates)"),
            _ => return,
        };
        self.toast(msg);
    }

    /// Puts the path chosen in a file dialog into the box that asked for
    /// it, if its dialog is still open.
    fn poll_file_picker(&mut self) {
        let Some((field, path)) = self.file_picker.poll() else { return };
        let path = path.to_string_lossy().into_owned();
        match field {
            PathField::Library => {
                if let Some(text) = self.lib_ui.dialog.as_mut().and_then(library_ui::Dialog::path_mut) {
                    *text = path;
                }
            }
            PathField::Export | PathField::ExportTo => {
                if let Some(d) = &mut self.export_dialog {
                    d.set_path(path, matches!(field, PathField::ExportTo));
                }
            }
            PathField::Run { dialog, target_dir } => {
                if let Some(act) = dialog.path_act(&target_dir, path) {
                    self.apply_act(act);
                }
            }
        }
    }

    /// Closes the window (saving, as usual) and starts the updated app.
    fn restart(&mut self, ctx: &egui::Context) {
        update::RESTART.store(true, std::sync::atomic::Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
        match self.writer.save(&path, &mut self.note_meta, &self.doc) {
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

    /// Opens a note. A large one (written on a PDF) is read in the background
    /// behind a loading screen, so the window never freezes.
    fn open_note(&mut self, path: &Path) {
        let large = std::fs::metadata(path).is_ok_and(|m| m.len() > LOAD_IN_BACKGROUND);
        if large {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let path = path.to_path_buf();
            self.load_in_background(format!("Opening \u{201c}{name}\u{201d}..."), move || Ok(path));
            return;
        }
        match storage::open_note(path) {
            Ok((meta, doc, writer)) => self.show_note(path, meta, doc, writer),
            Err(e) => self.toast(format!("Cannot open note: {e}")),
        }
    }

    /// Makes a loaded note the open one, in the editor, where it was left.
    fn show_note(&mut self, path: &Path, meta: NoteMeta, doc: Document, writer: NoteWriter) {
        self.doc = doc;
        self.note_meta = meta;
        self.writer = writer;
        self.note_path = Some(path.to_path_buf());
        self.ensure_pages();
        self.reset_editing_state();
        self.reader = None;
        let key = path.to_string_lossy();
        // A PDF just imported from the reader: where it was being read.
        let carried = self.carry_view.take();
        if let Some(view) = self.settings.note_view(&key).or(carried) {
            // The canvas clamps it to the note (e.g. pages deleted elsewhere).
            self.scroll = vec2(view.scroll[0], view.scroll[1]);
            self.settings.zoom = view.zoom;
        }
        self.settings.last_note = Some(key.into_owned());
        self.last_fp = self.current_fingerprint();
        self.dirty = false;
        self.screen = Screen::Editor;
    }

    /// Opens a PDF in the reader (read-only), read in the background. The
    /// open note is saved and closed first; stays put if that fails.
    fn open_pdf(&mut self, ctx: &egui::Context, path: &Path) {
        if self.screen == Screen::Editor {
            self.close_note(ctx);
            if self.screen == Screen::Editor {
                return;
            }
        }
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let path = path.to_path_buf();
        self.in_background(format!("Opening \u{201c}{name}\u{201d}..."), move || {
            let failed = |e: String| format!("Cannot open the PDF: {e}");
            let file = PdfFile::new(std::fs::read(&path).map_err(|e| failed(e.to_string()))?);
            let sizes = pdf_pages::page_sizes(&file).map_err(|e| failed(e.to_string()))?;
            if sizes.is_empty() {
                return Err(failed("it has no pages".into()));
            }
            Ok(Loaded::Pdf(path, Document::on_pdf(file, sizes)))
        });
    }

    /// Shows a PDF read for the reader, where it was left.
    fn show_pdf(&mut self, path: &Path, doc: Document) {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.doc = doc;
        self.note_meta = NoteMeta::new(&name, false);
        self.writer = NoteWriter::default();
        self.note_path = None;
        self.reader = Some(path.to_path_buf());
        self.ensure_pages();
        self.reset_editing_state();
        let key = path.to_string_lossy().into_owned();
        if let Some(view) = self.settings.note_view(&key) {
            self.scroll = vec2(view.scroll[0], view.scroll[1]);
            self.settings.zoom = view.zoom;
        }
        // The note settings change a note; a PDF is read as it is.
        if self.settings.left_panel == LeftPanel::Settings {
            self.settings.left_panel = LeftPanel::Pages;
        }
        self.settings.pdf_opened(&key);
        self.settings.last_note = Some(key);
        self.last_fp = self.current_fingerprint();
        self.dirty = false;
        self.screen = Screen::Editor;
    }

    /// Turns the PDF in the reader into a note to write on, in the notebook
    /// or section shown in the library, and opens it where it was being read.
    fn import_open_pdf(&mut self) {
        let Some(src) = self.reader.clone() else { return };
        self.remember_view();
        let scroll = self.kinetic.target().unwrap_or(self.scroll);
        self.carry_view = Some(NoteView { scroll: [scroll.x, scroll.y], zoom: self.settings.zoom });
        let dir = self.library_target_dir();
        self.import_pdf(dir, src);
    }

    /// Remembers where the open note (or PDF in the reader) is left (scroll
    /// and zoom), and that it is the open one.
    fn remember_view(&mut self) {
        let open = self.note_path.as_ref().or(self.reader.as_ref());
        let Some(path) = open.map(|p| p.to_string_lossy().into_owned()) else { return };
        // The place the view is gliding to, if it is.
        let scroll = self.kinetic.target().unwrap_or(self.scroll);
        let view = NoteView { scroll: [scroll.x, scroll.y], zoom: self.settings.zoom };
        self.settings.set_note_view(&path, view);
        self.settings.last_note = Some(path);
    }

    /// Runs `find` (e.g. importing a PDF, which returns the new note) and
    /// reads the note it names on a background thread; the note opens when
    /// it is ready. `label` is shown on the loading screen meanwhile.
    fn load_in_background(&mut self, label: String, find: impl FnOnce() -> Result<PathBuf, String> + Send + 'static) {
        self.in_background(label, move || {
            find().and_then(|path| {
                storage::open_note(&path)
                    .map(|(meta, doc, writer)| Loaded::Note(path, meta, doc, writer))
                    .map_err(|e| format!("Cannot open note: {e}"))
            })
        });
    }

    /// Runs `load` on a background thread behind the loading screen (showing
    /// `label`); what it read opens when it is ready.
    fn in_background(&mut self, label: String, load: impl FnOnce() -> Result<Loaded, String> + Send + 'static) {
        let (tx, done) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let spawned = thread::Builder::new().name("load-note".into()).spawn(move || {
            let _ = tx.send(load());
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => self.loading = Some(Loading { label, done }),
            Err(e) => self.toast(format!("Cannot open note: {e}")),
        }
    }

    /// Opens the note loaded in the background once it is ready.
    fn poll_loading(&mut self) {
        let Some(loading) = &self.loading else { return };
        let loaded = match loading.done.try_recv() {
            Ok(loaded) => loaded,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Cannot open note: the loading stopped unexpectedly".into()),
        };
        self.loading = None;
        match loaded {
            Ok(Loaded::Note(path, meta, doc, writer)) => {
                self.refresh_library();
                self.show_note(&path, meta, doc, writer);
            }
            Ok(Loaded::Pdf(path, doc)) => self.show_pdf(&path, doc),
            Err(e) => {
                self.carry_view = None;
                self.refresh_library();
                self.toast(e);
            }
        }
    }

    /// Imports a PDF into `dir` in the background and opens the new note.
    fn import_pdf(&mut self, dir: PathBuf, src: PathBuf) {
        let name = src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.load_in_background(format!("Importing \u{201c}{name}\u{201d}..."), move || {
            library::import_pdf(&dir, &src).map_err(|e| format!("Cannot import the PDF: {e}"))
        });
    }

    /// Saves and returns to the library. Stays in the editor if saving fails,
    /// so nothing typed or drawn can be lost. A PDF in the reader has
    /// nothing to save.
    fn close_note(&mut self, ctx: &egui::Context) {
        if self.reader.is_none() {
            if self.edit.is_some() {
                self.commit_edit(ctx);
            }
            self.sync_dirty();
            if self.dirty && !self.save_note(false) {
                return;
            }
        }
        self.remember_view();
        // Closed by hand: the app starts in the library next time.
        self.settings.last_note = None;
        self.note_path = None;
        self.reader = None;
        self.context_menu = None;
        self.export_dialog = None;
        self.doc = Document::default();
        self.writer = NoteWriter::default();
        self.reset_editing_state();
        self.screen = Screen::Library;
        self.refresh_library();
    }

    /// A PDF dropped onto the window opens in the reader. While a file is
    /// dragged over the window, a hint says so.
    fn dropped_pdf(&mut self, ctx: &egui::Context) {
        let (hovering, dropped) = ctx.input(|i| {
            let hovering = i.raw.hovered_files.iter().any(|f| f.path.as_deref().is_none_or(is_pdf));
            let dropped = i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).find(|p| is_pdf(p));
            (hovering, dropped)
        });
        if hovering {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("pdf_drop_hint")));
            let screen = ctx.screen_rect();
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(120));
            theme::paint_toast(&painter, screen.center(), "Drop the PDF to read it");
        }
        if let Some(src) = dropped {
            self.open_pdf(ctx, &src);
        }
    }

    fn reset_editing_state(&mut self) {
        self.cache.clear();
        self.thumb_cache.clear();
        self.pdf_textures.clear();
        self.pdf_nav.clear();
        self.nav_back.clear();
        self.outline_open.clear();
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
        // Reading changes nothing, so it has no undo either.
        let typing = self.edit.is_some() || ctx.wants_keyboard_input() || self.reading();
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
        if export && !self.reading() {
            self.open_export_dialog();
        }
        // While the context menu is open, Escape belongs to it.
        if self.selection.is_some() && self.context_menu.is_none() && !ctx.wants_keyboard_input() && !self.reading() {
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
                    self.wl_pen_hover = (!down).then_some(pos);
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
                    self.wl_pen_hover = None;
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

    /// A PDF is open in the reader (not a note).
    fn reading(&self) -> bool {
        self.reader.is_some()
    }

    /// The canvas moves around instead of writing: the reader, or a note
    /// with the pointer tool. Drag to scroll, tap to turn pages or follow a
    /// PDF link.
    fn navigating(&self) -> bool {
        self.reading() || self.tool == Tool::Pointer
    }

    /// The pages of the open note, stacked.
    fn layout(&self) -> Layout {
        Layout { page_w: self.doc.page_width, page_h: self.doc.page_height, count: self.doc.page_count.max(1) }
    }

    // ------------------------------------------------- PDF links and outline

    /// The note page written on page `pdf_page` of the PDF (the first, if
    /// several are; none if it was deleted).
    fn note_page_of(&self, pdf_page: u32) -> Option<u32> {
        let pages = &self.doc.pdf.as_ref()?.pages;
        pages.iter().position(|p| *p == Some(pdf_page)).map(|i| i as u32)
    }

    /// Scrolls to where a link or an outline entry leads, remembering where
    /// the view was for Back.
    fn go_to_target(&mut self, target: Target) {
        let Some(page) = self.note_page_of(target.page) else {
            self.toast("That page of the PDF is no longer in this note");
            return;
        };
        let layout = self.layout();
        let mut y = layout.page_stop(page);
        // A height on the page: shown at the top, with a little room above.
        if let Some(pdf) = &self.doc.pdf {
            let placed = pdf.placement(target.page, layout.page_w, layout.page_h);
            let size = pdf.sizes.get(target.page as usize);
            if let (Some(top), Some([_, py, _, ph]), Some([_, h])) = (target.top, placed, size) {
                let share = (1.0 - top / h.max(1.0)).clamp(0.0, 1.0);
                y = layout.page_top(page) + py + share * ph - MARGIN;
            }
        }
        if self.nav_back.len() >= NAV_BACK_MAX {
            self.nav_back.remove(0);
        }
        self.nav_back.push(self.kinetic.target().unwrap_or(self.scroll));
        self.scroll_to = Some(y.max(0.0));
    }

    /// Back to where the view was before the last link followed.
    fn go_back(&mut self) {
        if let Some(back) = self.nav_back.pop() {
            self.scroll_to = Some(back.y);
        }
    }

    /// The PDF link under the screen point `pos`, and its screen rectangle.
    /// Asks for the links of that page the first time.
    fn link_at(&mut self, view: &View, pos: Pos2) -> Option<(Rect, LinkAction)> {
        let pdf = self.doc.pdf.as_ref()?;
        let file = pdf.file.as_ref()?;
        let layout = view.layout;
        let page = layout.page_at(view.screen_to_content(pos))?;
        let pdf_page = (*pdf.pages.get(page as usize)?)?;
        let [x, y, w, h] = pdf.placement(pdf_page, layout.page_w, layout.page_h)?;
        let origin = view.page_origin_screen(page) + vec2(x, y) * view.scale;
        let size = vec2(w, h) * view.scale;
        self.pdf_nav.links(file, pdf_page).iter().find_map(|l| {
            let [l0, t0, r0, b0] = l.rect;
            let rect = Rect::from_min_max(origin + vec2(l0, t0) * size, origin + vec2(r0, b0) * size);
            rect.contains(pos).then(|| (rect, l.action.clone()))
        })
    }

    /// Follows a link: a page of the PDF, or a web address in the browser.
    fn follow_link(&mut self, ctx: &egui::Context, action: LinkAction) {
        match action {
            LinkAction::GoTo(target) => self.go_to_target(target),
            LinkAction::Uri(uri) => ctx.open_url(egui::OpenUrl::new_tab(uri)),
        }
    }

    /// Inserts an empty page so that it gets `index` (undoable).
    fn insert_page(&mut self, index: u32) {
        self.clear_selection();
        self.history
            .exec(&mut self.doc, Action::InsertPage { index, pdf_page: None });
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
            Tool::Pointer | Tool::Eraser | Tool::Select | Tool::Text => None,
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
                let accel = if is_wheel_notch(unit, delta) {
                    self.settings.mouse_accel
                } else {
                    self.settings.touchpad_accel
                };
                self.kinetic.notch(content, now, accel);
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

        // Reading also takes taps (to turn pages); a drag then starts only
        // past the click threshold, which is why writing senses drags only.
        let sense = if self.navigating() { Sense::click_and_drag() } else { Sense::drag() };
        let canvas_resp = ui.allocate_rect(canvas, sense);

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
        // Content height shown below the top pad.
        let visible = (canvas.height() - TOP_PAD_PX).max(1.0) / scale;
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
            if self.navigating() {
                let (next, prev, down, up, back) = ui.input(|i| {
                    let space = i.key_pressed(Key::Space);
                    let (left, alt) = (i.key_pressed(Key::ArrowLeft), i.modifiers.alt);
                    (
                        i.key_pressed(Key::ArrowRight) || space && !i.modifiers.shift,
                        left && !alt || space && i.modifiers.shift,
                        i.key_pressed(Key::ArrowDown),
                        i.key_pressed(Key::ArrowUp),
                        left && alt || i.key_pressed(Key::Backspace),
                    )
                });
                if back {
                    self.go_back();
                }
                self.page_turn += i32::from(pgdn || next) - i32::from(pgup || prev);
                let line = visible * 0.1;
                if down {
                    self.ease_scroll(base + vec2(0.0, line));
                }
                if up {
                    self.ease_scroll(base - vec2(0.0, line));
                }
            } else {
                if pgdn {
                    self.ease_scroll(base + vec2(0.0, page_len));
                }
                if pgup {
                    self.ease_scroll(base - vec2(0.0, page_len));
                }
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
        // The reader: links, under the pointer, show where they lead
        // and are followed by a tap; elsewhere a tap on the left or right
        // quarter of the view turns the page.
        let mut link_under_pointer = None;
        if self.navigating() {
            let v = View::new(canvas, self.scroll, scale, layout);
            let pointer = canvas_resp.interact_pointer_pos().or(canvas_resp.hover_pos());
            let link = pointer.and_then(|p| self.link_at(&v, p));
            if let Some((rect, action)) = &link {
                // Painted over the page, further down.
                link_under_pointer = Some(*rect);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if let LinkAction::Uri(uri) = action {
                    egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), Id::new("pdf_link"), |ui| {
                        ui.label(uri);
                    });
                }
            }
            if canvas_resp.clicked() {
                match (link, canvas_resp.interact_pointer_pos()) {
                    (Some((_, action)), _) => self.follow_link(ui.ctx(), action),
                    (None, Some(p)) => {
                        let zone = canvas.width() * 0.25;
                        if p.x < canvas.left() + zone {
                            self.page_turn -= 1;
                        } else if p.x > canvas.right() - zone {
                            self.page_turn += 1;
                        }
                    }
                    (None, None) => {}
                }
            }
        }
        let turn = std::mem::take(&mut self.page_turn);
        if turn != 0 {
            let base = self.kinetic.target().unwrap_or(self.scroll);
            let mut y = base.y;
            for _ in 0..turn.unsigned_abs() {
                y = if turn > 0 { layout.next_stop(y, visible) } else { layout.prev_stop(y, visible) };
                y = y.clamp(0.0, max.y);
            }
            self.ease_scroll(vec2(base.x, y));
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
            // The reader: everywhere; a note: only off the paper.
            self.panning = self.navigating()
                || start.map_or(false, |p| v.layout.page_at(v.screen_to_content(p)).is_none());
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
                    self.kinetic.release(now, scale, self.settings.touch_fling);
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
            && !self.reading()
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

        if !self.panning && !swallowed && !self.reading() {
            match self.tool {
                // Moving around is handled above.
                Tool::Pointer => {}
                Tool::Eraser => self.handle_erase(&canvas_resp, &view, press, &moves),
                Tool::Select => self.handle_select(&canvas_resp, &view, press, &moves),
                Tool::Text => self.handle_text(&canvas_resp, &view, press, &moves),
                Tool::Pen | Tool::Highlighter => {
                    self.handle_ink(&canvas_resp, &view, press, &moves)
                }
            }
        }

        // ---- painting
        // Bottom to top: paper, highlighters, rules, text, pen (see `render`).
        let painter = ui.painter_at(canvas);
        render::paint_paper(&painter, &view);
        // A note written on a PDF: its pages, right on the paper.
        for page in view.visible_pages() {
            self.pdf_textures.paint(&painter, &self.doc, page, view.page_screen_rect(page), true);
            // The reader: the links of the pages shown, ready before they are pointed at.
            if let (true, Some(pdf)) = (self.navigating(), &self.doc.pdf) {
                if let (Some(file), Some(pdf_page)) = (&pdf.file, self.doc.pdf_page(page)) {
                    self.pdf_nav.links(file, pdf_page);
                }
            }
        }
        let ppp = ui.ctx().pixels_per_point();
        let move_off = self.moving.as_ref().map_or(Vec2::ZERO, |m| m.delta);
        let moved = self.selection.as_ref().map(|s| Moved {
            ids: &s.ids,
            page: s.page,
            offset: move_off,
        });
        // The stroke being drawn, in its own layer.
        let live = self.active.as_ref().map(|a| (a, a.builder.preview_points(self.settings.predict_pen)));
        let paint_live = |kind: StrokeKind| {
            if let Some((a, pts)) = live.as_ref().filter(|(a, _)| a.kind == kind) {
                render::paint_live(&painter, &view, a.page, a.kind, a.color, a.width, pts, ppp);
            }
        };
        let strokes = &self.doc.strokes;
        render::paint_strokes(&painter, &view, Layer::Highlight, strokes, &mut self.cache, ppp, moved.as_ref());
        paint_live(StrokeKind::Highlighter);
        render::paint_rules(&painter, &view, &self.doc.page);
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

        render::paint_strokes(&painter, &view, Layer::Ink, strokes, &mut self.cache, ppp, moved.as_ref());
        paint_live(StrokeKind::Pen);

        if let Some(rect) = link_under_pointer {
            painter.rect_filled(rect.expand(2.0), 2.0, color::accent_alpha(36));
        }

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
        // The tool cursor, the same for pen and mouse: over a page, pen,
        // highlighter and eraser replace the system pointer with their tip
        // (a dot like the stroke they would draw, OneNote style, or the
        // eraser's circle). Pointer, select and text keep the system pointer.
        // A hovering pen is not reported by winit as a pointer, so it is read
        // from the native source. Not where something else (a toolbar, a
        // panel) is on top of the canvas.
        let ctx = ui.ctx().clone();
        let pen_hover = self
            .win_pen
            .as_ref()
            .and_then(|p| p.hover())
            .map(|(x, y)| pos2(x / ppp, y / ppp))
            .or(self.wl_pen_hover)
            .filter(|p| canvas.contains(*p) && ctx.layer_id_at(*p) == Some(ui.layer_id()));
        let tip_pos = pen_hover
            .or(canvas_resp.hover_pos())
            .or(canvas_resp.interact_pointer_pos())
            .filter(|p| view.layout.page_at(view.screen_to_content(*p)).is_some());
        let grabbing = self.panning || self.vbar_grab.is_some() || self.hbar_grab.is_some();
        if let (Some(pos), false, false) = (tip_pos, self.navigating(), grabbing) {
            if self.tool == Tool::Eraser {
                ctx.set_cursor_icon(egui::CursorIcon::None);
                render::paint_eraser_cursor(&painter, pos, self.settings.eraser_radius * scale);
            } else if let Some((kind, rgba, width)) = self.stroke_params() {
                ctx.set_cursor_icon(egui::CursorIcon::None);
                // While drawing, the stroke's own end is the tip.
                if self.active.is_none() {
                    render::paint_pen_cursor(&painter, pos, kind, rgba, width * scale);
                }
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
        if btn.intersects(canvas) && !self.reading() {
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
        // Only a note written on a PDF has an outline.
        let on_pdf = self.doc.pdf.is_some();
        let wrong_view = view == LeftPanel::Outline && !on_pdf || view == LeftPanel::Settings && self.reading();
        if wrong_view {
            self.settings.left_panel = LeftPanel::Pages;
            return self.left_panel(ctx);
        }
        let note_settings = !self.reading();
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
                    if note_settings && toolbar::settings_button(ui, view == LeftPanel::Settings) {
                        clicked = Some(LeftPanel::Settings);
                    }
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        if toolbar::pages_button(ui, view == LeftPanel::Pages) {
                            clicked = Some(LeftPanel::Pages);
                        }
                        if on_pdf && toolbar::outline_button(ui, view == LeftPanel::Outline) {
                            clicked = Some(LeftPanel::Outline);
                        }
                    });
                });
            });
        match view {
            LeftPanel::Pages => self.pages_view(ctx),
            LeftPanel::Settings => self.note_settings_view(ctx),
            LeftPanel::Outline => self.outline_view(ctx),
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
        // A PDF in the reader: its pages are looked at, not changed.
        let editable = !self.reading();

        // Disjoint field borrows, released again after `show`.
        let doc = &self.doc;
        let strokes = &self.doc.strokes;
        let texts = &self.doc.texts;
        let cache = &mut self.thumb_cache;
        let pdf_textures = &mut self.pdf_textures;
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
                                pdf_textures.paint(&clipped, doc, i, rect, false);
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
                            if editable && ui.rect_contains_pointer(card) && drag.is_none() {
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
                                if !editable {
                                    return;
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
                            if editable && del.clicked() {
                                delete = Some(i);
                            } else if resp.clicked() {
                                jump = Some(i);
                            }
                            if editable && resp.drag_started() {
                                *drag = Some(i);
                            }
                        }
                        if editable {
                            ui.add_space(space::XS);
                            let (row, _) = ui.allocate_exact_size(vec2(avail, 36.0), Sense::hover());
                            let tile = Rect::from_center_size(row.center(), vec2(thumb_w, 36.0));
                            if theme::add_tile(ui, tile, Id::new("add_page_tile"), "Add page").clicked() {
                                add = true;
                            }
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
        self.pdf_nav.begin_frame(ctx);
        self.handle_shortcuts(ctx);

        // Pen side button = eraser while held. Wayland pens report it through
        // `eraser_native`, Windows pens through `win_pen`; on X11 it can arrive
        // as the middle mouse button. (The right button opens the context
        // menu.) Never switch in the middle of a stroke.
        if !self.busy() && self.edit.is_none() && !self.reading() {
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
        let nav = toolbar::Navigation {
            page: self.current_page,
            count: self.doc.page_count.max(1),
            contents: self.doc.pdf.is_some(),
            can_go_back: !self.nav_back.is_empty(),
        };
        let event = if self.reading() {
            toolbar::show_reader(ctx, &mut self.settings, &nav, bottom_inset)
        } else {
            toolbar::show(
                ctx,
                &mut self.tool,
                &mut self.settings,
                self.history.can_undo(),
                self.history.can_redo(),
                &nav,
                bottom_inset,
            )
        };
        // After the pages panel, so the corner buttons sit next to it.
        let note_settings = !self.reading();
        toolbar::show_corners(ctx, &mut self.settings, note_settings, bottom_inset);
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
            Some(ToolbarEvent::Import) => self.import_open_pdf(),
            Some(ToolbarEvent::PrevPage) => self.page_turn -= 1,
            Some(ToolbarEvent::NextPage) => self.page_turn += 1,
            Some(ToolbarEvent::GoToPage(i)) => self.scroll_to = Some(self.layout().page_stop(i)),
            Some(ToolbarEvent::Back) => self.go_back(),
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

/// A note being read (or a PDF imported) on a background thread.
struct Loading {
    /// What the loading screen says.
    label: String,
    done: Receiver<Result<Loaded, String>>,
}

/// What a background load opens.
// One per load, moved once: the size difference costs nothing.
#[allow(clippy::large_enum_variant)]
enum Loaded {
    Note(PathBuf, NoteMeta, Document, NoteWriter),
    /// A PDF for the reader.
    Pdf(PathBuf, Document),
}

/// A wheel event from a mouse wheel's notches rather than a touchpad. Both
/// arrive as lines on Windows: a notch is whole lines, a touchpad sends
/// fractions (so does a high-resolution wheel, which then counts as a
/// touchpad). Pixel deltas come from touchpads.
fn is_wheel_notch(unit: MouseWheelUnit, delta: Vec2) -> bool {
    let whole = |v: f32| (v - v.round()).abs() < 1e-3;
    unit == MouseWheelUnit::Line && delta != Vec2::ZERO && whole(delta.x) && whole(delta.y)
}

fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Notes larger than this (bytes) are read in the background: notes written
/// on a PDF.
const LOAD_IN_BACKGROUND: u64 = 4 * 1024 * 1024;

/// Covers the window while a note loads: dimmed, a card with a spinner and
/// what is happening, and every click absorbed.
fn loading_screen(ctx: &egui::Context, label: &str) {
    let screen = ctx.screen_rect();
    egui::Area::new(Id::new("loading_screen"))
        .order(egui::Order::Tooltip)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.allocate_response(screen.size(), Sense::click_and_drag());
            ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(140));
            let card = Rect::from_center_size(screen.center(), vec2(320.0, 128.0));
            ui.painter().add(theme::card_shadow().as_shape(card, radius::LARGE));
            ui.painter().rect(card, radius::LARGE, color::SURFACE, egui::Stroke::new(1.0, color::BORDER));
            let inner = card.shrink(space::L);
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inner), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(space::S);
                    ui.add(egui::Spinner::new().size(28.0).color(color::ACCENT));
                    ui.add_space(space::M);
                    ui.add(egui::Label::new(theme::heading(label)).truncate());
                    ui.label(theme::muted("Large PDFs can take a moment."));
                });
            });
        });
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
        self.pdf_textures.begin_frame(ctx);
        if let Some(e) = self.pdf_textures.error.take() {
            self.toast(format!("The PDF pages cannot be shown: {e}"));
        }
        self.poll_loading();
        self.poll_update();
        self.poll_file_picker();
        // While a note loads, the window waits: no shortcuts, no drops.
        if self.loading.is_none() {
            self.dropped_pdf(ctx);
            self.menu_shortcuts(ctx);
        }
        self.window_chrome(ctx);
        match self.screen {
            Screen::Library => self.library_screen(ctx),
            Screen::Editor => self.editor_screen(ctx),
        }
        self.finish_chrome(ctx);
        if let Some(req) = self.pick_request.take() {
            self.file_picker.open(ctx, frame, req);
        }
        if let Some(loading) = &self.loading {
            loading_screen(ctx, &loading.label);
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Wait first, so the frame gets the pen samples that arrive meanwhile.
        self.frame_limit.wait(self.settings.fps_limit);
        self.inject_pen_events(ctx, raw_input);
    }

    /// The window background, visible only through gaps between panels:
    /// the bars' colour, so a gap can never show as a dark line.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        color::CHROME.to_normalized_gamma_f32()
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Periodically and on exit: where the open note is.
        if self.screen == Screen::Editor {
            self.remember_view();
        }
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
