// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Persisted UI settings (stored through eframe's storage, not in the note file).

use eframe::egui_wgpu::wgpu;
use serde::{Deserialize, Serialize};
use tratto_core::model::{Document, PageKind};
use tratto_core::pdf_export::InkColors;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolbarPosition {
    Top,
    Bottom,
}

/// How the select tool picks what to select.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectMode {
    /// A freehand loop around it.
    #[default]
    Lasso,
    /// A box dragged around it.
    Rectangle,
}

/// What the sidebar of the editor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sidebar {
    Closed,
    /// The sections and notes of the open note's notebook.
    Notebook,
    Pages,
    Settings,
    /// The table of contents of a note written on a PDF.
    Outline,
    /// Searching the text.
    Search,
}

impl Sidebar {
    /// Shows `view`, or closes the sidebar if `view` is already shown.
    pub fn toggle(&mut self, view: Sidebar) {
        *self = if *self == view { Sidebar::Closed } else { view };
    }
}

/// The edge of the window the sidebar is attached to (right for left-handed
/// writers, whose hand would cover a sidebar on the left).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidebarSide {
    #[default]
    Left,
    Right,
}

/// Where a note (or a PDF in the reader) was left: the scroll position
/// (content space, so it does not depend on the zoom or the window size) and
/// the zoom.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteView {
    pub scroll: [f32; 2],
    pub zoom: ZoomMode,
}

/// The order of the notes in the library's grids.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibrarySort {
    /// The folder's own order (dragged in the notebook panel).
    #[default]
    Manual,
    /// Most recently edited first.
    Edited,
    Title,
}

impl LibrarySort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "Your order",
            Self::Edited => "Last edited",
            Self::Title => "Title",
        }
    }
}

/// What the eraser takes away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EraserMode {
    /// Only the ink under it: strokes it crosses are cut in pieces.
    Partial,
    /// Every stroke it touches, whole.
    #[default]
    Stroke,
}

impl EraserMode {
    pub const ALL: [EraserMode; 2] = [Self::Partial, Self::Stroke];

    pub fn label(self) -> &'static str {
        match self {
            Self::Partial => "What it rubs out",
            Self::Stroke => "Whole strokes",
        }
    }
}

/// What happens when a new version is released.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateMode {
    /// Checked at startup, downloaded and installed (used from the next start).
    #[default]
    Install,
    /// Checked at startup; installed only when asked.
    Notify,
    /// Never checked automatically (Check now still works).
    Off,
}

impl UpdateMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Install => "Install automatically",
            Self::Notify => "Only tell me",
            Self::Off => "Don't check",
        }
    }
}

/// Which releases the updater follows, by the pre-release suffix of their
/// tag; a release flagged as a pre-release on GitHub is never Stable.
/// Each channel also gets those of the steadier ones, whichever is newest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UpdateChannel {
    /// Releases only ("1.2.0").
    #[default]
    Stable,
    /// Betas too ("1.2.0-beta1"): new features before their release.
    Testing,
    /// Alphas too ("1.2.0-alpha3"), and any other pre-release.
    Unstable,
}

impl UpdateChannel {
    pub const ALL: [UpdateChannel; 3] = [Self::Stable, Self::Testing, Self::Unstable];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Testing => "Testing",
            Self::Unstable => "Unstable",
        }
    }
}

/// When Tratto presents with vsync to save power, whatever `Settings::vsync`
/// says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatterySaver {
    /// While the computer runs on battery at `power::LOW_PERCENT` or less.
    #[default]
    Low,
    /// While the computer runs on battery.
    OnBattery,
    Always,
    Off,
}

impl BatterySaver {
    pub const ALL: [BatterySaver; 4] = [Self::Low, Self::OnBattery, Self::Always, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "When battery is low",
            Self::OnBattery => "On battery",
            Self::Always => "Always",
            Self::Off => "Off",
        }
    }
}

/// The graphics API the window draws with (read at startup: switching needs
/// a restart). `WGPU_BACKEND` overrides it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphicsBackend {
    /// Windows: DirectX 12 on Intel, Vulkan on NVIDIA and AMD (see
    /// `main::windows_backend`); elsewhere wgpu's choice.
    #[default]
    Auto,
    Vulkan,
    Dx12,
    OpenGl,
}

impl GraphicsBackend {
    /// The choices on this system (none on macOS, which only has Metal).
    pub const CHOICES: &[GraphicsBackend] = if cfg!(windows) {
        &[Self::Auto, Self::Dx12, Self::Vulkan, Self::OpenGl]
    } else if cfg!(target_os = "macos") {
        &[]
    } else {
        &[Self::Auto, Self::Vulkan, Self::OpenGl]
    };

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Vulkan => "Vulkan",
            Self::Dx12 => "DirectX 12",
            Self::OpenGl => "OpenGL",
        }
    }

    /// The backend asked for (`None`: automatic).
    pub fn backend(self) -> Option<wgpu::Backend> {
        match self {
            Self::Auto => None,
            Self::Vulkan => Some(wgpu::Backend::Vulkan),
            Self::Dx12 => Some(wgpu::Backend::Dx12),
            Self::OpenGl => Some(wgpu::Backend::Gl),
        }
    }
}

/// The name of a backend wgpu draws with, for the settings.
pub fn backend_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Dx12 => "DirectX 12",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::Metal => "Metal",
        _ => "another backend",
    }
}

/// Notes whose place is remembered, at most (the least recent is forgotten).
pub const NOTE_VIEWS: usize = 200;
/// PDFs listed as recently read, at most.
pub const RECENT_PDFS: usize = 12;

/// How the pages are scaled in the window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ZoomMode {
    /// Page width (plus margins) fills the window.
    FitWidth,
    /// A whole page fits in the window.
    FitPage,
    /// Fixed zoom; 100 % = 1 document unit per screen point.
    Percent(u32),
}

impl ZoomMode {
    pub const PRESETS: [ZoomMode; 9] = [
        ZoomMode::FitWidth,
        ZoomMode::FitPage,
        ZoomMode::Percent(50),
        ZoomMode::Percent(75),
        ZoomMode::Percent(100),
        ZoomMode::Percent(125),
        ZoomMode::Percent(150),
        ZoomMode::Percent(200),
        ZoomMode::Percent(300),
    ];

    pub fn label(&self) -> String {
        match self {
            ZoomMode::FitWidth => "Fit width".into(),
            ZoomMode::FitPage => "Fit page".into(),
            ZoomMode::Percent(p) => format!("{p}%"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ToolStyle {
    pub color: [u8; 3],
    /// Width in document units.
    pub width: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub toolbar_position: ToolbarPosition,
    /// The select tool's lasso or rectangle.
    pub select_mode: SelectMode,
    pub pen: ToolStyle,
    pub highlighter: ToolStyle,
    /// Text boxes: `width` is the font size (document units).
    pub text: ToolStyle,
    /// Eraser radius in document units.
    pub eraser_radius: f32,
    pub eraser_mode: EraserMode,
    /// Extrapolate the live stroke slightly ahead of the pen (hides latency).
    pub predict_pen: bool,
    /// Present with vsync: saves power, adds latency.
    pub vsync: bool,
    /// When vsync is used anyway, to save power.
    pub battery_saver: BatterySaver,
    /// Frames per second at most, without vsync (`0`: no limit).
    pub fps_limit: u32,
    pub graphics_backend: GraphicsBackend,
    pub zoom: ZoomMode,
    /// What the sidebar shows (the notebook, pages, settings...).
    #[serde(alias = "left_panel")]
    pub sidebar: Sidebar,
    /// The view the sidebar's notch reopens: the last one shown (not the
    /// note settings, which have their own notch).
    pub sidebar_last: Sidebar,
    pub sidebar_side: SidebarSide,
    /// Draw the ruled/grid lines into the exported PDF.
    pub pdf_rules: bool,
    /// Folder that holds all notebooks and notes.
    pub library_root: String,
    /// Draw our own title bar with the menus (system decorations off).
    pub custom_title_bar: bool,
    /// Dark or light interface, or as the system is (the paper stays white).
    pub theme: crate::theme::ThemeMode,
    /// Interface scale (egui zoom factor).
    pub ui_scale: f32,
    /// Wheel glide and acceleration, flick to pan, animated page jumps.
    pub smooth_scroll: bool,
    /// Multiplier for wheel and touchpad scrolling.
    pub scroll_speed: f32,
    /// Acceleration of quick wheel notches and of fast touchpad swipes, the
    /// latter also their momentum (`0`: none, `1`: the default; with
    /// `smooth_scroll`).
    pub mouse_accel: f32,
    pub touchpad_accel: f32,
    /// Glide after a flick (a finger, or pen or mouse drag that pans):
    /// `0` none, `1` the default.
    pub touch_fling: f32,
    /// Line width follows the pen pressure.
    pub pressure: bool,
    /// How much it follows it: `1` all of the pen's range, less keeps the
    /// lines nearer their full width (a light-pressure Wacom thins them).
    pub pressure_strength: f32,
    /// The pen side button (or the middle mouse button) erases while held.
    pub side_button_eraser: bool,
    /// Paper of new notes.
    pub new_page_width: f32,
    pub new_page_height: f32,
    pub new_page_kind: PageKind,
    pub new_page_spacing: f32,
    /// Seconds between the last change and the automatic save.
    pub autosave_secs: f32,
    /// Notes shown under "Recent" on the library screen.
    pub recent_count: usize,
    /// PDF export (the Export dialog remembers these).
    pub pdf_highlighter: bool,
    pub pdf_text: bool,
    /// Notes written on a PDF: export the PDF's pages under the ink.
    pub pdf_pages: bool,
    pub pdf_ink: InkColors,
    /// Output page size in px; `None` = the note's own size.
    pub pdf_page_size: Option<(f32, f32)>,
    /// Margin in px, with `pdf_page_size` only.
    pub pdf_margin: f32,
    pub pdf_author: String,
    pub pdf_open_after: bool,
    /// Reopen, at startup, the note that was open when the app was closed.
    pub reopen_last_note: bool,
    /// Checking for, and installing, new versions at startup.
    pub updates: UpdateMode,
    /// Which releases count as new versions.
    pub update_channel: UpdateChannel,
    /// The order of the notes in the library.
    pub library_sort: LibrarySort,
    /// The note (or PDF in the reader) open when the app was last closed
    /// (`None`: the library).
    pub last_note: Option<String>,
    /// Where each recently opened note or PDF was left (by path), most
    /// recent last.
    pub note_views: Vec<(String, NoteView)>,
    /// PDFs recently opened in the reader (paths), most recent last.
    pub recent_pdfs: Vec<String>,
}

impl Settings {
    /// Where the note at `path` was left, if remembered.
    pub fn note_view(&self, path: &str) -> Option<NoteView> {
        self.note_views.iter().rev().find(|(p, _)| p == path).map(|(_, v)| *v)
    }

    /// A note or folder moved from `old` to `new` (renamed, moved): the
    /// places of the notes in it follow.
    pub fn moved(&mut self, old: &std::path::Path, new: &std::path::Path) {
        let follow = |p: &mut String| {
            if let Ok(rest) = std::path::Path::new(p.as_str()).strip_prefix(old) {
                let moved = if rest.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rest) };
                *p = moved.to_string_lossy().into_owned();
            }
        };
        self.note_views.iter_mut().for_each(|(p, _)| follow(p));
        if let Some(p) = self.last_note.as_mut() {
            follow(p);
        }
    }

    /// Remembers where the note at `path` is left (most recent last).
    pub fn set_note_view(&mut self, path: &str, view: NoteView) {
        self.note_views.retain(|(p, _)| p != path);
        self.note_views.push((path.to_owned(), view));
        let excess = self.note_views.len().saturating_sub(NOTE_VIEWS);
        self.note_views.drain(..excess);
    }

    /// The PDF at `path` was opened in the reader: first among the recent ones.
    pub fn pdf_opened(&mut self, path: &str) {
        self.recent_pdfs.retain(|p| p != path);
        self.recent_pdfs.push(path.to_owned());
        let excess = self.recent_pdfs.len().saturating_sub(RECENT_PDFS);
        self.recent_pdfs.drain(..excess);
    }

    /// Takes the PDF at `path` off the recent ones.
    pub fn forget_pdf(&mut self, path: &str) {
        self.recent_pdfs.retain(|p| p != path);
    }

    /// What a reader-only window saves into the settings as other windows
    /// left them (`self`): where the `pdfs` it read were left, the PDFs as
    /// recently opened, and its preferences if they were changed in that
    /// window (`reader` now, `start` when it opened). The rest (the open
    /// note, the library, what the sidebar shows) is the other windows'.
    pub fn merge_reader(&mut self, reader: &Settings, start: &Settings, pdfs: &[String]) {
        if reader.preferences() != start.preferences() {
            let state = std::mem::replace(self, reader.clone());
            self.zoom = state.zoom;
            self.sidebar = state.sidebar;
            self.sidebar_last = state.sidebar_last;
            self.last_note = state.last_note;
            self.note_views = state.note_views;
            self.recent_pdfs = state.recent_pdfs;
        }
        for pdf in pdfs {
            if let Some(view) = reader.note_view(pdf) {
                self.set_note_view(pdf, view);
            }
            self.pdf_opened(pdf);
        }
    }

    /// The settings without what changes by itself while reading (the zoom
    /// of the PDF, the sidebar's view, the open note, places, recent PDFs),
    /// for comparing.
    fn preferences(&self) -> String {
        let prefs = Settings {
            zoom: ZoomMode::Percent(100),
            sidebar: Sidebar::Closed,
            sidebar_last: Sidebar::Closed,
            last_note: None,
            note_views: Vec::new(),
            recent_pdfs: Vec::new(),
            ..self.clone()
        };
        ron::to_string(&prefs).unwrap_or_default()
    }
}

impl Default for Settings {
    fn default() -> Self {
        let blank = Document::default();
        Self {
            toolbar_position: ToolbarPosition::Top,
            select_mode: SelectMode::Lasso,
            pen: ToolStyle { color: [20, 20, 20], width: 2.5 },
            highlighter: ToolStyle { color: [255, 235, 0], width: 14.0 },
            text: ToolStyle { color: [20, 20, 20], width: 18.0 },
            eraser_radius: 12.0,
            eraser_mode: EraserMode::Stroke,
            predict_pen: true,
            vsync: false,
            battery_saver: BatterySaver::default(),
            fps_limit: crate::frame_limit::DEFAULT_FPS,
            graphics_backend: GraphicsBackend::Auto,
            zoom: ZoomMode::Percent(100),
            sidebar: Sidebar::Closed,
            sidebar_last: Sidebar::Notebook,
            sidebar_side: SidebarSide::Left,
            pdf_rules: true,
            library_root: tratto_core::library::default_root().to_string_lossy().into_owned(),
            custom_title_bar: true,
            theme: crate::theme::ThemeMode::Dark,
            ui_scale: 1.0,
            smooth_scroll: true,
            scroll_speed: 1.0,
            mouse_accel: 1.0,
            touchpad_accel: 1.0,
            touch_fling: 1.0,
            pressure: true,
            pressure_strength: 0.5,
            side_button_eraser: true,
            new_page_width: blank.page_width,
            new_page_height: blank.page_height,
            new_page_kind: blank.page.kind,
            new_page_spacing: blank.page.spacing,
            autosave_secs: 2.0,
            recent_count: 12,
            pdf_highlighter: true,
            pdf_text: true,
            pdf_pages: true,
            pdf_ink: InkColors::Original,
            pdf_page_size: None,
            // 10 mm.
            pdf_margin: 37.8,
            pdf_author: String::new(),
            pdf_open_after: false,
            reopen_last_note: true,
            updates: UpdateMode::Install,
            update_channel: UpdateChannel::Stable,
            library_sort: LibrarySort::Manual,
            last_note: None,
            note_views: Vec::new(),
            recent_pdfs: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn view(y: f32) -> NoteView {
        NoteView { scroll: [0.0, y], zoom: ZoomMode::FitWidth }
    }

    #[test]
    fn note_places_are_remembered_most_recent_last() {
        let mut s = Settings::default();
        s.set_note_view("a.ink", view(1.0));
        s.set_note_view("b.ink", view(2.0));
        s.set_note_view("a.ink", view(3.0));
        assert_eq!(s.note_view("a.ink"), Some(view(3.0)));
        assert_eq!(s.note_views.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), ["b.ink", "a.ink"]);
        for i in 0..NOTE_VIEWS {
            s.set_note_view(&format!("n{i}.ink"), view(0.0));
        }
        assert_eq!(s.note_views.len(), NOTE_VIEWS);
        assert_eq!(s.note_view("b.ink"), None, "the least recent is forgotten");
    }

    #[test]
    fn reader_window_saves_only_what_it_changed() {
        let start = Settings::default();
        // Meanwhile the full app opened a note and changed a preference.
        let mut disk = start.clone();
        disk.last_note = Some("lib/Plan.ink".into());
        disk.scroll_speed = 2.0;

        // The reader read a PDF, zooming as it went.
        let mut reader = start.clone();
        reader.zoom = ZoomMode::FitWidth;
        // A reader window has no notebook view.
        reader.sidebar = Sidebar::Pages;
        reader.sidebar_last = Sidebar::Pages;
        reader.set_note_view("a.pdf", view(4.0));
        reader.pdf_opened("a.pdf");
        reader.last_note = Some("a.pdf".into());
        let mut saved = disk.clone();
        saved.merge_reader(&reader, &start, &["a.pdf".into()]);
        assert_eq!((saved.last_note.as_deref(), saved.scroll_speed), (Some("lib/Plan.ink"), 2.0));
        assert_eq!((saved.sidebar, saved.sidebar_last), (Sidebar::Closed, Sidebar::Notebook));
        assert_eq!(
            (saved.note_view("a.pdf"), saved.recent_pdfs.as_slice()),
            (Some(view(4.0)), ["a.pdf".into()].as_slice())
        );

        // A preference changed in the reader is kept, the library state too.
        reader.ui_scale = 1.5;
        let mut saved = disk.clone();
        saved.merge_reader(&reader, &start, &["a.pdf".into()]);
        assert_eq!((saved.ui_scale, saved.last_note.as_deref()), (1.5, Some("lib/Plan.ink")));
        assert_eq!(saved.sidebar_last, Sidebar::Notebook);
        assert_eq!(saved.note_view("a.pdf"), Some(view(4.0)));
    }

    #[test]
    fn recent_pdfs_most_recent_last() {
        let mut s = Settings::default();
        s.pdf_opened("a.pdf");
        s.pdf_opened("b.pdf");
        s.pdf_opened("a.pdf");
        assert_eq!(s.recent_pdfs, ["b.pdf", "a.pdf"]);
        for i in 0..RECENT_PDFS {
            s.pdf_opened(&format!("{i}.pdf"));
        }
        assert_eq!(s.recent_pdfs.len(), RECENT_PDFS);
        assert!(!s.recent_pdfs.iter().any(|p| p == "a.pdf"), "the least recent is forgotten");
        s.forget_pdf("0.pdf");
        assert_eq!(s.recent_pdfs.first().map(String::as_str), Some("1.pdf"));
    }

    #[test]
    fn places_follow_renames_and_moves() {
        let mut s = Settings::default();
        let nb = Path::new("lib").join("Work");
        let note = nb.join("Section 1").join("Plan.ink");
        let note_s = note.to_string_lossy().into_owned();
        s.set_note_view(&note_s, view(5.0));
        s.set_note_view("lib/Other.ink", view(6.0));
        s.last_note = Some(note_s);

        // The notebook renamed: the note inside follows.
        let renamed = Path::new("lib").join("Job");
        s.moved(&nb, &renamed);
        let now = renamed.join("Section 1").join("Plan.ink");
        assert_eq!(s.note_view(&now.to_string_lossy()), Some(view(5.0)));
        assert_eq!(s.last_note.as_deref(), Some(now.to_string_lossy().as_ref()));
        // The note itself renamed.
        let final_path = renamed.join("Section 1").join("Plan v2.ink");
        s.moved(&now, &final_path);
        assert_eq!(s.note_view(&final_path.to_string_lossy()), Some(view(5.0)));
        // Others untouched.
        assert_eq!(s.note_view("lib/Other.ink"), Some(view(6.0)));
    }
}
