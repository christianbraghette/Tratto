// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Persisted UI settings (stored through eframe's storage, not in the note file).

use inknote_core::model::{Document, PageKind};
use inknote_core::pdf_export::InkColors;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolbarPosition {
    Top,
    /// Also the migration target for the removed Left / Right positions
    /// that may still be present in saved settings.
    #[serde(alias = "Left", alias = "Right")]
    Bottom,
}

/// What the left panel of the editor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeftPanel {
    Closed,
    Pages,
    Settings,
    /// The table of contents of a note written on a PDF.
    Outline,
}

impl LeftPanel {
    /// Shows `view`, or closes the panel if `view` is already shown.
    pub fn toggle(&mut self, view: LeftPanel) {
        *self = if *self == view { LeftPanel::Closed } else { view };
    }
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
    pub pen: ToolStyle,
    pub highlighter: ToolStyle,
    /// Text boxes: `width` is the font size (document units).
    pub text: ToolStyle,
    /// Eraser radius in document units.
    pub eraser_radius: f32,
    /// Extrapolate the live stroke slightly ahead of the pen (hides latency).
    pub predict_pen: bool,
    pub zoom: ZoomMode,
    /// Left panel: page previews or settings.
    pub left_panel: LeftPanel,
    /// Right panel with the notebook hierarchy.
    pub library_panel_open: bool,
    /// Draw the ruled/grid lines into the exported PDF.
    pub pdf_rules: bool,
    /// Folder that holds all notebooks and notes.
    pub library_root: String,
    /// Draw our own title bar with the menus (system decorations off).
    pub custom_title_bar: bool,
    /// Interface scale (egui zoom factor).
    pub ui_scale: f32,
    /// Wheel glide and acceleration, flick to pan, animated page jumps.
    pub smooth_scroll: bool,
    /// Multiplier for wheel and touchpad scrolling.
    pub scroll_speed: f32,
    /// Line width follows the pen pressure.
    pub pressure: bool,
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
    pub pdf_paper_background: bool,
    pub pdf_ink: InkColors,
    /// Output page size in px; `None` = the note's own size.
    pub pdf_page_size: Option<(f32, f32)>,
    /// Margin in px, with `pdf_page_size` only.
    pub pdf_margin: f32,
    pub pdf_author: String,
    pub pdf_open_after: bool,
    /// Reopen, at startup, the note that was open when the app was closed.
    pub reopen_last_note: bool,
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
}

impl Default for Settings {
    fn default() -> Self {
        let blank = Document::default();
        Self {
            toolbar_position: ToolbarPosition::Bottom,
            pen: ToolStyle { color: [20, 20, 20], width: 2.5 },
            highlighter: ToolStyle { color: [255, 235, 0], width: 14.0 },
            text: ToolStyle { color: [20, 20, 20], width: 18.0 },
            eraser_radius: 12.0,
            predict_pen: true,
            zoom: ZoomMode::Percent(100),
            left_panel: LeftPanel::Closed,
            library_panel_open: false,
            pdf_rules: true,
            library_root: inknote_core::library::default_root().to_string_lossy().into_owned(),
            custom_title_bar: true,
            ui_scale: 1.0,
            smooth_scroll: true,
            scroll_speed: 1.0,
            pressure: true,
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
            pdf_paper_background: false,
            pdf_ink: InkColors::Original,
            pdf_page_size: None,
            // 10 mm.
            pdf_margin: 37.8,
            pdf_author: String::new(),
            pdf_open_after: false,
            reopen_last_note: true,
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
