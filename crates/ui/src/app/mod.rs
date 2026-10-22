// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Application state and per-frame orchestration: [`InkApp`] holds the
//! state, this module runs a frame (`eframe::App::ui`, `editor_screen`) and
//! the submodules implement the rest of it.
//!
//! - `notes`          opening, showing, saving and closing notes; PDFs read or dropped
//! - `canvas`         the editor's canvas: input routing, scrolling, zoom, painting, links
//! - `ink`            pen and highlighter strokes, the eraser
//! - `selection`      lasso, select all, moving and deleting what is selected
//! - `transform`      resizing, rotating and recolouring what is selected
//! - `clipboard`      copy, cut and paste (note content, pages, library notes)
//! - `text`           text boxes
//! - `text_select`    the text of the pages: selecting and copying it
//! - `search`         searching the text (the left panel's search view)
//! - `pen_input`      native pen and drop events, pressure, the pen's eraser
//! - `pages_panel`    the left panel strip and its pages view
//! - `outline_ui`     the left panel's outline view (a PDF's contents)
//! - `settings_ui`    the left panel's note settings and the Settings window
//! - `library_ui`     the library screen and its dialogs
//! - `library_panel`  the notebook panel next to the editor
//! - `menus`          title bar menus, context menu, shortcuts, help windows
//! - `export_ui`      the Export PDF dialog
//! - `update_ui`      the updates window

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Id, Pos2, Rect, Sense, Vec2, vec2};
use eframe::egui_wgpu::wgpu;

use tratto_core::history::{Action, History};
use tratto_core::library::Library;
use tratto_core::model::{Document, Point, StrokeKind, TextBox};
use tratto_core::storage::{NoteMeta, NoteWriter};
use tratto_core::stroke_builder::StrokeBuilder;

use crate::file_picker::{self, FilePicker};
use crate::frame_limit::FrameLimiter;
use crate::input::wayland_drop::WaylandDrop;
use crate::input::wayland_pen::WaylandPen;
use crate::input::windows_pen::WindowsPen;
use crate::kinetic::Kinetic;
use crate::pdf_nav::PdfNav;
use crate::pdf_view::PdfTextures;
use crate::render::{LiveStroke, PictureTextures, StrokeCache};
use crate::saver::Saver;
use crate::scrollbar;
use crate::settings::{BatterySaver, NoteView, Settings, UpdateMode};
use crate::theme::{self, color, radius, space};
use crate::toolbar::{self, Tool, ToolbarEvent};
use crate::update::{self, Updater};
use crate::view::{Layout, View};

mod canvas;
pub(crate) mod clipboard;
mod export_ui;
mod ink;
mod library_panel;
mod library_ui;
mod menus;
mod notes;
mod outline_ui;
mod pages_panel;
mod pen_input;
mod pictures;
mod search;
mod selection;
mod settings_ui;
mod text;
mod text_select;
mod transform;
mod update_ui;

use menus::{Command, ContextMenu, HelpWindows};

/// Key under which eframe stores the window state (not public in eframe).
const WINDOW_STATE_KEY: &str = "window";
const SETTINGS_KEY: &str = "tratto_settings";
/// How long a status message (toast) is shown.
const TOAST_FOR: Duration = Duration::from_millis(3500);

/// The settings saved by the last run, read before the window exists:
/// `main` needs some of them to create it. Mirrors eframe's file storage
/// (not public): a RON map of keys to RON strings.
pub fn saved_settings() -> Option<Settings> {
    let file = eframe::storage_dir(crate::APP_ID)?.join("app.ron");
    let kv: std::collections::HashMap<String, String> = ron::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    ron::from_str(kv.get(SETTINGS_KEY)?).ok()
}
/// Stroke currently being drawn.
struct ActiveStroke {
    page: u32,
    kind: StrokeKind,
    color: [u8; 4],
    width: f32,
    builder: StrokeBuilder,
    /// Its triangles so far.
    live: LiveStroke,
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

/// A press with the text tool: tap = edit / create, drag = move / size, or
/// resize the box being edited.
struct TextPress {
    /// Existing text box under the pen.
    hit: Option<u64>,
    start: Pos2,
    now: Pos2,
    page: Option<u32>,
    moved: bool,
    /// A handle of the box being edited: its side, and the box's `x` and
    /// width when the press started.
    resize: Option<(text::Side, f32, f32)>,
}

/// Strokes, text boxes and pictures picked with the select tool (all on one
/// page).
struct Selection {
    page: u32,
    /// The strokes.
    ids: HashSet<u64>,
    texts: HashSet<u64>,
    images: HashSet<u64>,
    /// Bounding box in that page's space.
    bounds: Rect,
}

/// Freehand lasso being drawn (page space).
struct Lasso {
    page: u32,
    /// The loop drawn; a box (`rect`): the corner pressed and where the
    /// pointer is now.
    points: Vec<Pos2>,
    /// A rectangle (see `SelectMode`), not a loop.
    rect: bool,
}

impl Lasso {
    /// The outline that selects: the loop, or the box's four corners.
    fn outline(&self) -> Vec<Pos2> {
        match (self.rect, self.points.as_slice()) {
            (true, [a, .., b]) => {
                let r = Rect::from_two_pos(*a, *b);
                vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]
            }
            _ => self.points.clone(),
        }
    }

    /// It was a tap, not a loop or a box around something.
    fn is_tap(&self) -> bool {
        if self.rect {
            let o = self.outline();
            return o.len() < 4 || o[0].distance(o[2]) < 8.0;
        }
        let path_len: f32 = self.points.windows(2).map(|w| w[0].distance(w[1])).sum();
        self.points.len() < 4 || path_len < 8.0
    }
}

/// Selection being dragged; `delta` is in content space.
struct MoveDrag {
    start: Pos2,
    delta: Vec2,
}

/// Native pen and drag-and-drop input (see `pen_input`).
#[derive(Default)]
struct PenInput {
    /// Wayland pen source (None on X11/Windows or without tablet support).
    wayland: Option<WaylandPen>,
    /// Wayland: dropped files, which winit does not report there.
    drop: Option<WaylandDrop>,
    /// Wayland: files are dragged over the window.
    drop_hover: bool,
    /// Windows: pen side button / eraser end (None elsewhere).
    windows: Option<WindowsPen>,
    /// The sources above were started (on the first frame).
    started: bool,
    down: bool,
    pos: Pos2,
    /// Wayland: where the pen hovers (in range, not touching).
    hover: Option<Pos2>,
    /// Eraser requested by the pen (eraser end or side button).
    eraser: bool,
    /// Pressure of the pen's last sample (1.0 for a mouse).
    pressure: f32,
    /// Windows: the touch of this frame is the pen's (see `finger`).
    windows_touch: bool,
    /// Windows: a paste key was pressed this frame (see `picture_input`).
    paste: bool,
}

/// The window and its title bar (see `menus`).
#[derive(Default)]
struct WindowChrome {
    /// Decorations last requested from the OS (None = not yet).
    decorations: Option<bool>,
    /// The window state restored at startup has been checked.
    checked: bool,
    /// The window was maximized before entering fullscreen.
    maximize_after_fullscreen: bool,
    /// Window title last sent to the OS.
    os_title: String,
    /// Text of the note title field in the title bar, and whether it has focus.
    title_buf: String,
    title_focused: bool,
    /// A menu of the title bar was open last frame (keeps the fullscreen bar shown).
    menu_open: bool,
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
    status: String,
    /// When `status` was last set (drives the transient toast).
    status_at: Option<Instant>,
    pen_input: PenInput,
    /// Caps the frame rate without vsync.
    frame_limit: FrameLimiter,
    /// Tool to restore when the temporary eraser (pen eraser end / side button) is released.
    saved_tool: Option<Tool>,
    /// A drag that started on the grey area is scrolling the view.
    panning: bool,
    /// Where the pointer last was while an item is dragged: past the window's
    /// edge egui no longer knows, and the view keeps scrolling that way.
    drag_pointer: Option<Pos2>,
    selection: Option<Selection>,
    /// What was copied or cut last.
    clipboard: clipboard::Clipboard,
    lasso: Option<Lasso>,
    moving: Option<MoveDrag>,
    /// The picture picked with the select tool, and one being dragged.
    picture: Option<u64>,
    picture_drag: Option<pictures::PictureDrag>,
    /// The selection being resized or rotated by a handle.
    transform: Option<transform::TransformDrag>,
    /// The pictures' textures (canvas, page previews, export preview).
    picture_textures: PictureTextures,
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
    /// A finger writes with the pen tool: it was picked by the user (from
    /// the toolbar, a menu or a shortcut). Otherwise fingers move around.
    finger_ink: bool,
    /// The press being held is a finger's that moves around the note
    /// (see `finger_ink`), until it is released.
    finger_nav: bool,
    screen: Screen,
    library: Library,
    lib_ui: library_ui::LibraryUi,
    /// File of the open note.
    note_path: Option<PathBuf>,
    note_meta: NoteMeta,
    /// Writes the open note in the background.
    saver: Saver,
    dirty: bool,
    last_change: Instant,
    last_fp: Fingerprint,
    /// Text box being typed into.
    edit: Option<EditState>,
    text_press: Option<TextPress>,
    /// The text of the pages (PDF and text boxes), for selecting and searching.
    text_layer: text_select::TextLayer,
    /// Selected text (the reader, the pointer tool).
    text_sel: Option<text_select::TextSelection>,
    /// A press or drag selecting text, if one is going on.
    text_gesture: Option<text_select::TextGesture>,
    search: search::Search,
    /// Canvas context menu (right click), if open.
    context_menu: Option<ContextMenu>,
    /// The press that closed the context menu must not reach the canvas.
    swallow_press: bool,
    /// Menu commands collected this frame, run after drawing.
    pending: Vec<Command>,
    help: HelpWindows,
    chrome: WindowChrome,
    /// File > Settings window.
    global_settings_open: bool,
    /// Export PDF dialog, if open.
    export_dialog: Option<export_ui::ExportDialog>,
    /// PDF being written in the background, if any.
    export_job: Option<export_ui::ExportJob>,
    /// Pictures being saved as PNG files in the background.
    png_jobs: Vec<Receiver<Result<PathBuf, String>>>,
    /// Note being dragged in the notebook panel.
    note_drag: Option<library_panel::NoteDrag>,
    /// Same for the notebook name in the notebook panel.
    notebook_buf: String,
    notebook_focused: bool,
    /// Zoom of the last frame (points per document unit).
    view_scale: f32,
    /// The content shown in the last frame, from top to bottom (content y).
    shown_content: (f32, f32),
    /// Zoom factor requested by a menu command, applied around the centre.
    zoom_request: Option<f32>,
    /// New versions from the GitHub releases (checked at startup).
    updater: Updater,
    /// The system file dialog opened by a Browse button, if any.
    file_picker: FilePicker<PathField>,
    /// A Browse button was clicked: the dialog to open at the end of the
    /// frame, where the window (its parent) is at hand.
    pick_request: Option<file_picker::Request<PathField>>,
    /// Folders outside the library shown as notebooks (from the command
    /// line), for this session.
    external_notebooks: Vec<PathBuf>,
    /// A reader-only window (`--reader`, how the system opens PDFs): no
    /// library, closing the PDF closes the window. `None`: the full app.
    standalone: Option<Standalone>,
}

/// A reader-only window. Other Tratto windows may be open and share the
/// settings file, so it saves only what it changed (see `Settings::merge_reader`).
struct Standalone {
    /// The settings as they were when the window opened.
    start: Settings,
    /// The PDFs read in this window.
    pdfs: Vec<String>,
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
    /// A picture to insert, at a page and a point on it (or where the view is).
    Picture { at: Option<(u32, Pos2)> },
    /// Where to save this picture of a page or of the selection, as PNG.
    Png(Box<crate::snapshot::Snapshot>),
}

impl InkApp {
    /// `vsync`: the window presents with vsync. `open`: the path given on
    /// the command line (see `open_path`), instead of the last note; with
    /// `reader`, a PDF opens in a reader-only window (`Standalone`).
    pub fn new(cc: &eframe::CreationContext<'_>, vsync: bool, open: Option<PathBuf>, reader: bool) -> Self {
        // We implement zoom ourselves; keep egui's UI from zooming on Ctrl +/-.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        crate::theme::install(&cc.egui_ctx);

        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();

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
            status: String::new(),
            status_at: None,
            pen_input: PenInput { pressure: 1.0, ..PenInput::default() },
            frame_limit: FrameLimiter::new(vsync),
            saved_tool: None,
            panning: false,
            drag_pointer: None,
            selection: None,
            clipboard: clipboard::Clipboard::default(),
            lasso: None,
            moving: None,
            picture: None,
            picture_drag: None,
            transform: None,
            picture_textures: PictureTextures::default(),
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
            finger_ink: false,
            finger_nav: false,
            screen: Screen::Library,
            library: Library::default(),
            lib_ui: library_ui::LibraryUi::default(),
            note_path: None,
            note_meta: NoteMeta::new("Untitled note", true),
            saver: Saver::new(cc.egui_ctx.clone()),
            dirty: false,
            last_change: Instant::now(),
            last_fp: (0, 0, 0, 0, 0),
            edit: None,
            text_press: None,
            text_layer: text_select::TextLayer::default(),
            text_sel: None,
            text_gesture: None,
            search: search::Search::default(),
            context_menu: None,
            swallow_press: false,
            pending: Vec::new(),
            help: HelpWindows::default(),
            chrome: WindowChrome {
                // main.rs creates the window without decorations.
                decorations: Some(false),
                os_title: "Tratto".to_owned(),
                ..WindowChrome::default()
            },
            global_settings_open: false,
            export_dialog: None,
            export_job: None,
            png_jobs: Vec::new(),
            note_drag: None,
            notebook_buf: String::new(),
            notebook_focused: false,
            view_scale: 1.0,
            shown_content: (0.0, 0.0),
            zoom_request: None,
            updater: Updater::default(),
            file_picker: FilePicker::default(),
            pick_request: None,
            external_notebooks: Vec::new(),
            standalone: None,
        };
        if let Some(path) = open.as_ref().filter(|p| reader && p.is_file() && is_pdf(p)) {
            // Just the PDF: no library, no update check from every reader window.
            app.standalone = Some(Standalone { start: app.settings.clone(), pdfs: Vec::new() });
            app.open_pdf(&cc.egui_ctx, path);
            return app;
        }
        if app.settings.updates != UpdateMode::Off {
            let install = app.settings.updates == UpdateMode::Install;
            app.updater.check(&cc.egui_ctx, app.settings.update_channel, install);
        }
        app.refresh_library();
        if let Some(path) = open {
            app.open_path(&cc.egui_ctx, &path);
            return app;
        }
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
            Some(update::State::Available(r)) => {
                format!("Tratto {} is available (Help > Check for updates)", r.version)
            }
            Some(update::State::Installed(v)) => {
                format!("Tratto {v} is installed: it starts next time (or Help > Check for updates)")
            }
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
            PathField::Picture { at } => self.insert_picture_file(Path::new(&path), at),
            PathField::Png(snap) => {
                let job = crate::snapshot::save_png(&self.egui_ctx, *snap, PathBuf::from(path));
                self.png_jobs.push(job);
            }
        }
    }

    /// The battery saver is on now (see `Settings::battery_saver`).
    fn saving_power(&self, ctx: &egui::Context) -> bool {
        match self.settings.battery_saver {
            BatterySaver::Off => false,
            BatterySaver::Always => true,
            BatterySaver::Low => crate::power::battery_low(ctx),
            BatterySaver::OnBattery => crate::power::on_battery(ctx),
        }
    }

    /// Presents with vsync when the setting, the battery saver or
    /// TRATTO_VSYNC asks for it. The surface is reconfigured only when that
    /// changes (plugging in, a setting), never in the middle of writing.
    fn present_mode(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let vsync = self.settings.vsync || self.frame_limit.vsync_forced() || self.saving_power(ctx);
        if vsync == self.frame_limit.vsync() || self.busy() {
            return;
        }
        let Some(mut config) = frame.wgpu_surface_config() else { return };
        config.present_mode = if vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync };
        frame.set_wgpu_surface_config(config);
        self.frame_limit.set_vsync(vsync);
    }

    /// Closes the window (saving, as usual) and starts the updated app.
    fn restart(&self, ctx: &egui::Context) {
        update::RESTART.store(true, std::sync::atomic::Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // ---------------------------------------------------------------- state

    /// True while a stroke, erase drag, lasso or selection drag is in progress.
    fn busy(&self) -> bool {
        self.active.is_some()
            || !self.erase_session.is_empty()
            || self.lasso.is_some()
            || self.moving.is_some()
            || self.picture_drag.is_some()
            || self.transform.is_some()
    }

    /// A PDF is open in the reader (not a note).
    fn reading(&self) -> bool {
        self.reader.is_some()
    }

    /// The canvas moves around instead of writing: the reader, a note with
    /// the pointer tool, or a finger on a note (see `finger_ink`). Drag to
    /// scroll, tap to follow a PDF link.
    fn navigating(&self) -> bool {
        self.reading() || self.tool == Tool::Pointer || self.finger_nav
    }

    /// The pointer is a finger on a touch screen, not the mouse or a pen.
    /// A Windows pen arrives as a touch too; the native source tells them
    /// apart. The touch events count, so does the frame a touch ends.
    pub(super) fn finger(&self, ctx: &egui::Context) -> bool {
        let touch = ctx.input(|i| i.any_touches() || i.events.iter().any(|e| matches!(e, egui::Event::Touch { .. })));
        touch && !self.pen_input.down && !self.pen_input.windows_touch
    }

    /// The user picks a tool: fingers write only with the pen picked.
    pub(super) fn pick_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.finger_ink = tool == Tool::Pen;
    }

    /// The pages of the open note, stacked.
    fn layout(&self) -> Layout {
        Layout { page_w: self.doc.page_width, page_h: self.doc.page_height, count: self.doc.page_count.max(1) }
    }

    /// One frame of the note editor.
    fn editor_screen(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        self.pdf_nav.begin_frame(ctx);
        // The text boxes of a note just opened (their layouts are not saved).
        // Here, not when it is opened: one opened at startup has no fonts yet.
        crate::markdown::lay_out_document(ctx, &mut self.doc);
        self.pen_input.windows_touch = self.pen_input.windows.as_ref().is_some_and(|p| p.take_touching());
        self.search_step(ctx);
        self.handle_shortcuts(ctx);

        // Pen side button = eraser while held. Wayland pens report it through
        // `pen_input.eraser`, Windows pens through `pen_input.windows`; on X11 it can arrive
        // as the middle mouse button. (The right button opens the context
        // menu.) Never switch in the middle of a stroke.
        if !self.busy() && self.edit.is_none() && !self.reading() {
            let button_held = ctx.input(|i| i.pointer.middle_down());
            let win = self.pen_input.windows.as_ref().map(|p| p.buttons()).unwrap_or_default();
            // The eraser end always erases; the side button only if enabled.
            let side = win.barrel || button_held;
            let eraser = self.pen_input.eraser || win.eraser_end || (self.settings.side_button_eraser && side);
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
        let bottom_inset = if self.hbar_visible { scrollbar::THICKNESS } else { 0.0 };
        let library_was_open = self.settings.library_panel_open;
        self.left_panel(ui);
        // A reader-only window has no library.
        if self.standalone.is_none() {
            self.library_panel(ui);
        }
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
        let selected = self.selection_color();
        let event = if self.reading() {
            toolbar::show_reader(ui, &mut self.settings, &nav, bottom_inset)
        } else {
            toolbar::show(
                ui,
                &mut self.tool,
                &mut self.settings,
                self.history.can_undo(),
                self.history.can_redo(),
                &nav,
                selected,
                bottom_inset,
            )
        };
        // After the pages panel, so the corner buttons sit next to it.
        let note_settings = !self.reading();
        let library = self.standalone.is_none();
        toolbar::show_corners(ui, &mut self.settings, note_settings, library, bottom_inset);
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
            Some(ToolbarEvent::Tool(t)) => self.pick_tool(t),
            Some(ToolbarEvent::Recolor(rgb)) => self.recolor_selection(rgb),
            None => {}
        }

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.canvas_ui(ui));

        // Autosave shortly after the last change, never in the middle of a stroke.
        self.sync_dirty();
        if self.dirty && !self.busy() {
            let delay = Duration::from_secs_f32(self.settings.autosave_secs.clamp(0.5, 60.0));
            let waited = self.last_change.elapsed();
            if waited >= delay {
                self.autosave();
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

fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Covers the window while a note loads: dimmed, a card with a spinner and
/// what is happening, and every click absorbed.
fn loading_screen(ctx: &egui::Context, label: &str) {
    let screen = ctx.viewport_rect();
    egui::Area::new(Id::new("loading_screen")).order(egui::Order::Tooltip).fixed_pos(screen.min).show(ctx, |ui| {
        ui.allocate_response(screen.size(), Sense::click_and_drag());
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(140));
        let card = Rect::from_center_size(screen.center(), vec2(320.0, 128.0));
        ui.painter().add(theme::card_shadow().as_shape(card, radius::LARGE));
        ui.painter().rect(
            card,
            radius::LARGE,
            color::surface(),
            egui::Stroke::new(1.0, color::border()),
            egui::StrokeKind::Middle,
        );
        let inner = card.shrink(space::L);
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(space::S);
                ui.add(egui::Spinner::new().size(28.0).color(color::accent()));
                ui.add_space(space::M);
                ui.add(egui::Label::new(theme::heading(label)).truncate());
                ui.label(theme::muted("Large PDFs can take a moment."));
            });
        });
    });
}

/// Screen position -> point in the page's space, clamped to the page.
fn to_page_point(view: &View, page: u32, pos: Pos2, pressure: f32) -> Point {
    let c = view.screen_to_content(pos);
    let o = view.layout.page_origin(page);
    Point::new((c.x - o.x).clamp(0.0, view.layout.page_w), (c.y - o.y).clamp(0.0, view.layout.page_h), pressure)
}

impl eframe::App for InkApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        // The Wayland display handle is only available through `Frame`, so the
        // native pen source is started lazily on the first frame.
        if !self.pen_input.started {
            self.pen_input.started = true;
            self.pen_input.wayland = WaylandPen::start(ctx, frame);
            self.pen_input.drop = WaylandDrop::start(ctx, frame);
            self.pen_input.windows = WindowsPen::start(ctx, frame);
            crate::chrome::round_corners(frame);
            crate::win_frame::install(frame);
        }
        // Before anything is painted: the palette of this frame.
        crate::theme::follow(ctx, self.settings.theme);
        self.present_mode(ctx, frame);
        self.publish_clipboard(ctx);
        // Every frame, so that one pressed elsewhere is not pasted later.
        self.pen_input.paste = self.pen_input.windows.as_ref().is_some_and(|p| p.take_paste());

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
        if ctx.input(|i| i.viewport().close_requested()) {
            self.finish_saves();
        }

        // The cursor image stays until changed: only the canvas sets it, for
        // this frame (see `canvas::hide_pointer`).
        ctx.set_cursor_image(None);
        let scale = self.settings.ui_scale.clamp(0.5, 2.0);
        if (ctx.zoom_factor() - scale).abs() > 1e-3 && !ctx.input(|i| i.pointer.any_down()) {
            ctx.set_zoom_factor(scale);
        }
        self.pdf_textures.begin_frame(ctx);
        if let Some(e) = self.pdf_textures.error.take() {
            self.toast(format!("The PDF pages cannot be shown: {e}"));
        }
        self.poll_loading();
        self.poll_saves();
        self.poll_update();
        self.poll_file_picker();
        // While a note loads, the window waits: no shortcuts, no drops.
        if self.loading.is_none() {
            self.dropped_pdf(ctx);
            self.menu_shortcuts(ctx);
        }
        self.window_chrome(ui);
        match self.screen {
            Screen::Library => self.library_screen(ui),
            Screen::Editor => self.editor_screen(ui),
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
        self.inject_drop_events(raw_input);
    }

    /// The window background, visible only through gaps between panels:
    /// the bars' colour, so a gap can never show as a dark line.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        color::chrome().to_normalized_gamma_f32()
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Periodically and on exit: where the open note is.
        if self.screen == Screen::Editor {
            self.remember_view();
        }
        match &self.standalone {
            Some(s) => {
                // The settings as other windows left them, with what this one changed.
                let mut saved = saved_settings().unwrap_or_else(|| s.start.clone());
                saved.merge_reader(&self.settings, &s.start, &s.pdfs);
                eframe::set_value(storage, SETTINGS_KEY, &saved);
            }
            None => eframe::set_value(storage, SETTINGS_KEY, &self.settings),
        }
        // Overwrite the window state saved by earlier versions (eframe's key);
        // an unparsable value makes eframe skip restoring it.
        storage.set_string(WINDOW_STATE_KEY, String::new());
        // eframe calls this periodically and on exit. Only write what is already
        // committed; a text box being typed stays open (it is committed on close).
        // In the background: the window does not wait for the disk every 30 s
        // (`on_exit` waits for it).
        if self.screen == Screen::Editor && self.note_path.is_some() {
            self.sync_dirty();
            if self.dirty {
                self.autosave();
            }
        }
    }

    fn on_exit(&mut self) {
        // Nothing may be left half written when the app exits: what `save`
        // could not start (a save was being written) and every save started.
        if self.screen == Screen::Editor && self.note_path.is_some() {
            self.sync_dirty();
            if self.dirty {
                self.save_note(false);
                return;
            }
        }
        self.finish_saves();
    }
}
