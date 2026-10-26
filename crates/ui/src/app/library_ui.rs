// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The library screen: a sidebar (Quick note, Home, the quick notes, the
//! notebooks, the trash) and the place chosen there: home (quick starts,
//! recent notes, notebooks), the quick notes, a notebook with its section
//! tabs, the trash; plus search and the library's dialogs. Note cards can be
//! dragged onto a notebook, a section, the quick notes or the trash.
//!
//! This is a child module of `app`, so it can use the editor state directly
//! (opening a note, toasts, settings) without extra plumbing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Align2, Color32, CursorIcon, Frame, Id, Margin, Rect, RichText, Sense, Stroke, TextEdit, Ui, pos2, vec2,
};
use egui_phosphor::regular as icon;

use tratto_core::library::{self, NoteInfo, NotebookInfo};
use tratto_core::model::{Point, StrokeKind};
use tratto_core::storage::{Preview, now_secs};

use super::menus::{button_menu, entry};
use super::{InkApp, PathField};
use crate::file_picker::{self, Pick, Request};
use crate::settings::LibrarySort;
use crate::stroke_gpu::{self, Batch, Blend};
use crate::theme::{self, color, radius, size, space};

const CARD_W: f32 = 168.0;
const COVER_W: f32 = 168.0;
const COVER_H: f32 = 118.0;

// ------------------------------------------------------------------ state

/// Modal dialogs of the library.
pub enum Dialog {
    NewNotebook(String),
    NewSection {
        notebook: PathBuf,
        text: String,
    },
    RenameNote {
        path: PathBuf,
        text: String,
    },
    RenameDir {
        path: PathBuf,
        text: String,
    },
    Trash {
        path: PathBuf,
        label: String,
    },
    /// Erase a trash item for good.
    DeleteForever {
        path: PathBuf,
        label: String,
    },
    /// Erase everything in the trash (the number of items).
    EmptyTrash(usize),
    Import(String),
    /// A PDF to write on (its path).
    ImportPdf(String),
    /// A PDF to read (its path).
    OpenPdf(String),
    Folder(String),
}

/// The places of the sidebar besides the notebooks.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum LibView {
    #[default]
    Home,
    Quick,
    Trash,
}

#[derive(Default)]
pub struct LibraryUi {
    pub search: String,
    /// The place shown when no notebook is open.
    pub view: LibView,
    /// Open notebook (None = `view`).
    pub notebook: Option<PathBuf>,
    /// Selected section tab inside the notebook (None = "General").
    pub section: Option<PathBuf>,
    pub dialog: Option<Dialog>,
}

impl Dialog {
    /// The path box of the dialogs that ask for a file or folder.
    pub fn path_mut(&mut self) -> Option<&mut String> {
        match self {
            Dialog::Import(text) | Dialog::ImportPdf(text) | Dialog::OpenPdf(text) | Dialog::Folder(text) => Some(text),
            _ => None,
        }
    }

    /// The system file dialog that chooses the path: what it picks, its title.
    fn pick(&self) -> Option<(Pick, &'static str)> {
        match self {
            Dialog::Import(_) => Some((Pick::Open("Tratto notes", &["ink"]), "Import a note file")),
            Dialog::ImportPdf(_) => Some((Pick::Open("PDF", &["pdf"]), "Write on a PDF")),
            Dialog::OpenPdf(_) => Some((Pick::Open("PDF", &["pdf"]), "Read a PDF")),
            Dialog::Folder(_) => Some((Pick::Folder, "Library folder")),
            _ => None,
        }
    }

    /// What the dialog does with `path`, new notes going to `target_dir`.
    pub fn path_act(&self, target_dir: &Path, path: String) -> Option<Act> {
        match self {
            Dialog::Import(_) => Some(Act::Import(target_dir.to_path_buf(), path)),
            Dialog::ImportPdf(_) => Some(Act::ImportPdf(target_dir.to_path_buf(), path)),
            Dialog::OpenPdf(_) => Some(Act::OpenPdf(PathBuf::from(path.trim().trim_matches('"')))),
            Dialog::Folder(_) => Some(Act::SetRoot(path)),
            _ => None,
        }
    }
}

/// Everything the UI can ask for; applied after drawing, so drawing only
/// ever borrows the data immutably. Also used by the editor's notebook panel.
pub(super) enum Act {
    Open(PathBuf),
    /// A new note in the quick notes, opened at once.
    QuickNote,
    /// Show a place of the sidebar (closes the notebook, clears the search).
    Show(LibView),
    Sort(LibrarySort),
    NewNote(PathBuf),
    OpenNotebook(PathBuf),
    /// Stop showing an external notebook (its folder is left alone).
    CloseNotebook(PathBuf),
    Home,
    SelectSection(Option<PathBuf>),
    Dialog(Dialog),
    CloseDialog,
    CreateNotebook(String),
    CreateSection(PathBuf, String),
    RenameNote(PathBuf, String),
    RenameDir(PathBuf, String),
    Trash(PathBuf),
    /// Put a trash item back where it was.
    Restore(PathBuf),
    DeleteForever(PathBuf),
    EmptyTrash,
    Duplicate(PathBuf),
    MoveNote(PathBuf, PathBuf),
    /// Copy (false) or cut (true) a note, to paste it into a folder.
    Clip(PathBuf, bool),
    /// Paste the note copied or cut into a folder.
    Paste(PathBuf),
    Import(PathBuf, String),
    /// A PDF to write on: the folder of the new note, the PDF's path.
    ImportPdf(PathBuf, String),
    /// A PDF to read, in the reader.
    OpenPdf(PathBuf),
    /// Take a PDF off the recently read ones.
    ForgetPdf(String),
    SetRoot(String),
    Refresh,
}

// ----------------------------------------------------------------- screen

/// Width of the library's sidebar.
const SIDEBAR_W: f32 = 232.0;

/// A note card being dragged (onto a notebook, the quick notes or the trash).
struct DraggedNote {
    path: PathBuf,
    title: String,
}

impl InkApp {
    pub(super) fn library_screen(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        // Drop selections that no longer exist (renamed / deleted / synced away),
        // once the library is read (one just created is not in it before).
        if let Some(nb) = self.lib_ui.notebook.clone().filter(|_| !self.library_refreshing()) {
            match self.library.notebooks.iter().find(|n| n.path == nb) {
                None => {
                    self.lib_ui.notebook = None;
                    self.lib_ui.section = None;
                }
                Some(n) => {
                    if let Some(sec) = &self.lib_ui.section
                        && !n.sections.iter().any(|s| &s.path == sec)
                    {
                        self.lib_ui.section = None;
                    }
                }
            }
        }

        let mut acts: Vec<Act> = Vec::new();
        let root = self.library_root();
        let notebook =
            self.lib_ui.notebook.as_ref().and_then(|p| self.library.notebooks.iter().find(|n| &n.path == p)).cloned();
        let target_dir = self.library_target_dir();
        let searching = !self.lib_ui.search.trim().is_empty();

        self.library_sidebar(ui, notebook.as_ref(), &root, &mut acts);
        self.library_top_bar(ui, notebook.as_ref(), &target_dir, &mut acts);

        let sort = self.settings.library_sort;
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(color::desk()).inner_margin(Margin::symmetric(space::XXL as i8, space::L as i8)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if !self.library_loaded {
                        ui.add_space(96.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new().size(28.0).color(color::accent()));
                            ui.add_space(space::M);
                            ui.label(theme::muted("Reading the library..."));
                        });
                    } else if searching {
                        let q = self.lib_ui.search.trim().to_lowercase();
                        let hits: Vec<&NoteInfo> = self
                            .library
                            .all_notes()
                            .into_iter()
                            .filter(|n| n.meta.title.to_lowercase().contains(&q))
                            .collect();
                        heading(
                            ui,
                            &format!(
                                "{} for \u{201c}{}\u{201d}",
                                count(hits.len(), "result"),
                                self.lib_ui.search.trim()
                            ),
                        );
                        if hits.is_empty() {
                            empty_state(
                                ui,
                                icon::MAGNIFYING_GLASS,
                                "No note has that title",
                                "Titles are searched in every notebook and the quick notes.",
                            );
                        }
                        note_grid(ui, &hits, &root, true, None, &mut acts);
                    } else if let Some(nb) = &notebook {
                        notebook_view(ui, nb, self.lib_ui.section.as_deref(), &root, sort, &mut acts);
                    } else {
                        match self.lib_ui.view {
                            LibView::Home => {
                                let pdfs = &self.settings.recent_pdfs;
                                home_view(ui, &self.library, &root, self.settings.recent_count, pdfs, &mut acts)
                            }
                            LibView::Quick => quick_view(ui, &self.library, &root, sort, &mut acts),
                            LibView::Trash => trash_view(ui, &self.library, &mut acts),
                        }
                    }
                });
            });

        // A dragged card follows the pointer.
        if let Some(d) = egui::DragAndDrop::payload::<DraggedNote>(ctx)
            && let Some(p) = ctx.input(|i| i.pointer.interact_pos())
        {
            ctx.set_cursor_icon(CursorIcon::Grabbing);
            let ghost =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("note_card_ghost")));
            theme::paint_toast(&ghost, p + vec2(60.0, 20.0), &d.title);
        }

        self.dialogs(ctx, &target_dir, &mut acts);
        self.draw_toast(ctx);
        for a in acts {
            self.apply_act(a);
        }
    }

    /// Quick note, the places of the library, the notebooks, the trash. Notes
    /// dragged from the grid drop onto a notebook, the quick notes or the
    /// trash.
    fn library_sidebar(&self, ui: &mut egui::Ui, notebook: Option<&NotebookInfo>, root: &Path, acts: &mut Vec<Act>) {
        let view = self.lib_ui.view;
        let browsing = notebook.is_none() && self.lib_ui.search.trim().is_empty();
        egui::Panel::left("lib_side")
            .exact_size(SIDEBAR_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(color::chrome()).inner_margin(Margin::symmetric(space::M as i8, space::L as i8)))
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                    let quick =
                        theme::primary_button(format!("{}  Quick note", icon::LIGHTNING)).min_size(vec2(0.0, 38.0));
                    if ui.add(quick).on_hover_text("A new note in the quick notes, right away (Ctrl+Shift+N)").clicked()
                    {
                        acts.push(Act::QuickNote);
                    }
                    ui.add_space(space::L);
                    ui.spacing_mut().item_spacing.y = 2.0;

                    if nav_row(ui, icon::HOUSE, "Home", None, browsing && view == LibView::Home, None).clicked() {
                        acts.push(Act::Home);
                    }
                    let resp = nav_row(
                        ui,
                        icon::LIGHTNING,
                        "Quick notes",
                        Some(self.library.quick.len()),
                        browsing && view == LibView::Quick,
                        None,
                    );
                    if resp.clicked() {
                        acts.push(Act::Show(LibView::Quick));
                    }
                    drop_target(&resp, acts, |path| Act::MoveNote(path, root.to_path_buf()));

                    ui.add_space(space::L);
                    ui.horizontal(|ui| {
                        theme::caption(ui, "Notebooks");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            theme::ghost_buttons(ui);
                            if ui.small_button(icon::PLUS).on_hover_text("New notebook").clicked() {
                                acts.push(Act::Dialog(Dialog::NewNotebook(String::new())));
                            }
                        });
                    });
                    ui.add_space(space::XS);
                    let trash_h = theme::ROW_HEIGHT + space::L;
                    egui::ScrollArea::vertical()
                        .max_height((ui.available_height() - trash_h).max(60.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            if self.library.notebooks.is_empty() {
                                ui.label(theme::muted("No notebooks yet.").small());
                            }
                            for nb in &self.library.notebooks {
                                let total = nb.loose.len() + nb.sections.iter().map(|s| s.notes.len()).sum::<usize>();
                                let tint = Color32::from_rgb(nb.color[0], nb.color[1], nb.color[2]);
                                let open = notebook.is_some_and(|n| n.path == nb.path);
                                let resp = nav_row(ui, icon::NOTEBOOK, &nb.name, Some(total), open, Some(tint));
                                if resp.clicked() {
                                    acts.push(Act::OpenNotebook(nb.path.clone()));
                                }
                                drop_target(&resp, acts, |path| Act::MoveNote(path, nb.path.clone()));
                                resp.context_menu(|ui| notebook_menu(ui, nb, acts));
                            }
                        });

                    // The trash, at the bottom.
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                            let n = self.library.trash.len();
                            let resp = nav_row(
                                ui,
                                icon::TRASH,
                                "Trash",
                                (n > 0).then_some(n),
                                browsing && view == LibView::Trash,
                                None,
                            );
                            if resp.clicked() {
                                acts.push(Act::Show(LibView::Trash));
                            }
                            drop_target(&resp, acts, Act::Trash);
                        });
                    });
                });
            });
    }

    /// The title of the place shown, search, import, and "New note" there.
    fn library_top_bar(
        &mut self,
        ui: &mut egui::Ui,
        notebook: Option<&NotebookInfo>,
        target_dir: &Path,
        acts: &mut Vec<Act>,
    ) {
        let searching = !self.lib_ui.search.trim().is_empty();
        egui::Panel::top("lib_top")
            .show_separator_line(false)
            .frame(Frame::NONE.fill(color::desk()).inner_margin(Margin {
                left: space::XXL as i8,
                right: space::XXL as i8,
                top: space::L as i8,
                bottom: space::S as i8,
            }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(36.0);
                    match notebook {
                        _ if searching => {
                            ui.label(theme::title("Search"));
                        }
                        Some(nb) => {
                            let c = nb.color;
                            ui.label(
                                RichText::new(icon::NOTEBOOK)
                                    .size(size::TITLE)
                                    .color(Color32::from_rgb(c[0], c[1], c[2])),
                            );
                            ui.add(egui::Label::new(theme::title(&nb.name)).truncate());
                            theme::ghost_buttons(ui);
                            if ui.button(icon::PENCIL_SIMPLE).on_hover_text("Rename notebook").clicked() {
                                acts.push(Act::Dialog(Dialog::RenameDir {
                                    path: nb.path.clone(),
                                    text: nb.name.clone(),
                                }));
                            }
                            if nb.external {
                                let tip = format!("Close the notebook (the folder stays in {})", parent_name(&nb.path));
                                if ui.button(icon::X).on_hover_text(tip).clicked() {
                                    acts.push(Act::CloseNotebook(nb.path.clone()));
                                }
                            } else if ui.button(icon::TRASH).on_hover_text("Delete notebook").clicked() {
                                acts.push(Act::Dialog(Dialog::Trash { path: nb.path.clone(), label: nb.name.clone() }));
                            }
                        }
                        None => {
                            let title = match self.lib_ui.view {
                                LibView::Home => "Home",
                                LibView::Quick => "Quick notes",
                                LibView::Trash => "Trash",
                            };
                            ui.label(theme::title(title));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.lib_ui.view != LibView::Trash || notebook.is_some() || searching {
                            let new_note = theme::primary_button(format!("{}  New note", icon::NOTE_PENCIL));
                            let tip = format!(
                                "A new note in {} (Ctrl+N)",
                                place_name(notebook, self.lib_ui.section.as_deref())
                            );
                            if ui.add(new_note).on_hover_text(tip).clicked() {
                                acts.push(Act::NewNote(target_dir.to_path_buf()));
                            }
                        }
                        let menu = Id::unique("lib_import_menu");
                        let import = ui.button(format!("{}  Import  {}", icon::DOWNLOAD_SIMPLE, icon::CARET_DOWN));
                        button_menu(menu, &import, |ui| {
                            if entry(ui, icon::BOOK_OPEN, "Read a PDF...", "", true) {
                                acts.push(Act::Dialog(Dialog::OpenPdf(String::new())));
                            }
                            if entry(ui, icon::FILE_ARROW_DOWN, "Write on a PDF...", "", true) {
                                acts.push(Act::Dialog(Dialog::ImportPdf(String::new())));
                            }
                            if entry(ui, icon::NOTE_BLANK, "Import a note file...", "", true) {
                                acts.push(Act::Dialog(Dialog::Import(String::new())));
                            }
                        });
                        theme::ghost_buttons(ui);
                        if self.library_refreshing() {
                            // In the button's place, its size.
                            let side = ui.spacing().interact_size.y;
                            ui.add_sized(vec2(side, side), egui::Spinner::new().color(color::accent()))
                                .on_hover_text("Reading the library folder...");
                        } else if ui
                            .button(theme::icon(icon::ARROWS_CLOCKWISE))
                            .on_hover_text("Refresh (re-read the folder, e.g. after a sync)")
                            .clicked()
                        {
                            acts.push(Act::Refresh);
                        }
                        ui.add_space(space::S);
                        if searching && ui.button(icon::X).on_hover_text("Clear the search").clicked() {
                            self.lib_ui.search.clear();
                        }
                        ui.add(
                            TextEdit::singleline(&mut self.lib_ui.search)
                                .hint_text(format!("{}  Search notes", icon::MAGNIFYING_GLASS))
                                .margin(vec2(space::S, 6.0))
                                .desired_width(240.0),
                        );
                    });
                });
            });
    }

    fn draw_toast(&self, ctx: &egui::Context) {
        let Some(left) = self.status_at.and_then(|t| super::TOAST_FOR.checked_sub(t.elapsed())) else { return };
        if self.status.is_empty() {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("lib_toast")));
        theme::paint_toast(&painter, ctx.viewport_rect().center_bottom() - vec2(0.0, 48.0), &self.status);
        // Once more when it is gone (it does not fade).
        ctx.request_repaint_after(left);
    }

    // ------------------------------------------------------------ dialogs

    /// The library dialogs, also shown over the editor (notebook panel).
    pub(super) fn editor_dialogs(&mut self, ctx: &egui::Context) {
        if self.lib_ui.dialog.is_none() {
            return;
        }
        let target = self.library_target_dir();
        let mut acts = Vec::new();
        self.dialogs(ctx, &target, &mut acts);
        for a in acts {
            self.apply_act(a);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, target_dir: &Path, acts: &mut Vec<Act>) {
        let Some(dialog) = self.lib_ui.dialog.as_mut() else {
            return;
        };
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut close = escape;
        let picking = self.file_picker.is_open();
        // Browse was clicked: open the system's file dialog.
        let mut browse = false;
        let mut result: Option<Act> = None;

        // Name entry used by several dialogs.
        fn name_row(ui: &mut Ui, text: &mut String, hint: &str) -> bool {
            let r = ui.add(TextEdit::singleline(text).hint_text(hint).desired_width(280.0));
            r.request_focus();
            r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
        }

        // Path entry with a Browse button, which opens the system's file
        // dialog (disabled while one is open). Returns (submit, browse).
        fn path_row(ui: &mut Ui, text: &mut String, hint: &str, picking: bool) -> (bool, bool) {
            ui.horizontal(|ui| {
                let submit = name_row(ui, text, hint);
                let browse = ui
                    .add_enabled(!picking, egui::Button::new(format!("{}  Browse...", icon::FOLDER_OPEN)))
                    .on_hover_text("Choose with the system's file explorer")
                    .clicked();
                (submit, browse)
            })
            .inner
        }

        let title = match dialog {
            Dialog::NewNotebook(_) => "New notebook",
            Dialog::NewSection { .. } => "New section",
            Dialog::RenameNote { .. } | Dialog::RenameDir { .. } => "Rename",
            Dialog::Trash { .. } => "Delete",
            Dialog::DeleteForever { .. } => "Delete permanently",
            Dialog::EmptyTrash(_) => "Empty trash",
            Dialog::Import(_) => "Import a note file",
            Dialog::ImportPdf(_) => "Write on a PDF",
            Dialog::OpenPdf(_) => "Read a PDF",
            Dialog::Folder(_) => "Library folder",
        };

        egui::Window::new(title)
            .id(egui::Id::unique(super::menus::LIBRARY_DIALOG))
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                // Main button: label, destructive, action (None = disabled).
                let primary: Option<(&str, bool, Option<Act>)>;
                let mut submit = enter;
                let named = |text: &String| !text.trim().is_empty();
                match dialog {
                    Dialog::NewNotebook(text) => {
                        submit |= name_row(ui, text, "Notebook name");
                        primary = Some(("Create", false, named(text).then(|| Act::CreateNotebook(text.clone()))));
                    }
                    Dialog::NewSection { notebook, text } => {
                        submit |= name_row(ui, text, "Section name");
                        primary = Some((
                            "Create",
                            false,
                            named(text).then(|| Act::CreateSection(notebook.clone(), text.clone())),
                        ));
                    }
                    Dialog::RenameNote { path, text } => {
                        submit |= name_row(ui, text, "Title");
                        primary =
                            Some(("Rename", false, named(text).then(|| Act::RenameNote(path.clone(), text.clone()))));
                    }
                    Dialog::RenameDir { path, text } => {
                        submit |= name_row(ui, text, "Name");
                        primary =
                            Some(("Rename", false, named(text).then(|| Act::RenameDir(path.clone(), text.clone()))));
                    }
                    Dialog::Trash { path, label } => {
                        ui.label(format!("Move \"{label}\" to the library's .trash folder?"));
                        ui.label(theme::muted("You can restore it later from the Trash.").small());
                        primary = Some(("Delete", true, Some(Act::Trash(path.clone()))));
                    }
                    Dialog::DeleteForever { path, label } => {
                        ui.label(format!("Erase \"{label}\" for good?"));
                        ui.label(theme::muted("This cannot be undone.").small());
                        primary = Some(("Delete permanently", true, Some(Act::DeleteForever(path.clone()))));
                    }
                    Dialog::EmptyTrash(n) => {
                        let what = if *n == 1 { "the item".to_owned() } else { format!("all {n} items") };
                        ui.label(format!("Erase {what} in the trash for good?"));
                        ui.label(theme::muted("This cannot be undone.").small());
                        primary = Some(("Empty trash", true, Some(Act::EmptyTrash)));
                    }
                    Dialog::Import(text) => {
                        ui.label("Full path of an .ink file (any version):");
                        let (enter, clicked) = path_row(ui, text, "/path/to/notes.ink", picking);
                        (submit, browse) = (submit || enter, clicked);
                        ui.label(theme::muted("It is copied into the current notebook or section.").small());
                        primary = Some((
                            "Import",
                            false,
                            named(text).then(|| Act::Import(target_dir.to_path_buf(), text.clone())),
                        ));
                    }
                    Dialog::ImportPdf(text) => {
                        ui.label("Full path of a PDF file:");
                        let (enter, clicked) = path_row(ui, text, "/path/to/slides.pdf", picking);
                        (submit, browse) = (submit || enter, clicked);
                        ui.label(
                            theme::muted(
                                "It becomes a note in the current notebook or section, one page per PDF page.",
                            )
                            .small(),
                        );
                        ui.label(theme::muted("You can also drop a PDF onto the window.").small());
                        primary = Some((
                            "Import",
                            false,
                            named(text).then(|| Act::ImportPdf(target_dir.to_path_buf(), text.clone())),
                        ));
                    }
                    Dialog::OpenPdf(text) => {
                        ui.label("Full path of a PDF file:");
                        let (enter, clicked) = path_row(ui, text, "/path/to/book.pdf", picking);
                        (submit, browse) = (submit || enter, clicked);
                        ui.label(theme::muted("It opens to read, as it is. Import it to write on it.").small());
                        ui.label(theme::muted("You can also drop a PDF onto the window.").small());
                        let path = PathBuf::from(text.trim().trim_matches('"'));
                        primary = Some(("Open", false, named(text).then_some(Act::OpenPdf(path))));
                    }
                    Dialog::Folder(text) => {
                        ui.label("All notebooks and notes live in this folder:");
                        let (enter, clicked) = path_row(ui, text, "Folder path", picking);
                        (submit, browse) = (submit || enter, clicked);
                        ui.label(theme::muted("Existing notes are not moved; the new folder is read instead.").small());
                        primary = Some(("Use this folder", false, named(text).then(|| Act::SetRoot(text.clone()))));
                    }
                }
                ui.add_space(space::M);
                ui.horizontal(|ui| {
                    if let Some((label, danger, act)) = primary {
                        let button = if danger { theme::danger_button(label) } else { theme::primary_button(label) };
                        let enabled = act.is_some();
                        if (ui.add_enabled(enabled, button).clicked() || submit) && enabled {
                            result = act;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        let pick =
            self.lib_ui.dialog.as_mut().filter(|_| browse).and_then(|d| Some((d.pick()?, d.path_mut()?.clone())));
        if let Some(((pick, title), current)) = pick {
            self.pick_request = Some(Request { pick, title, current, field: PathField::Library });
        }
        if let Some(r) = result {
            acts.push(r);
            acts.push(Act::CloseDialog);
        } else if close {
            acts.push(Act::CloseDialog);
        }
    }

    // ------------------------------------------------------------ actions

    /// Opens a library dialog. On Windows and macOS (`file_picker::DIRECT`)
    /// the ones that ask for a path open the system's file dialog instead,
    /// and the chosen path is used at once.
    pub(super) fn show_dialog(&mut self, d: Dialog) {
        let Some((pick, title)) = d.pick().filter(|_| file_picker::DIRECT) else {
            self.lib_ui.dialog = Some(d);
            return;
        };
        if !self.file_picker.is_open() {
            let current = if let Dialog::Folder(root) = &d { root.clone() } else { String::new() };
            let target_dir = self.library_target_dir();
            self.pick_request = Some(Request { pick, title, current, field: PathField::Run { dialog: d, target_dir } });
        }
    }

    /// Folder that new notes go to: the open section or notebook, else the root.
    pub(super) fn library_target_dir(&self) -> PathBuf {
        let notebook = self.lib_ui.notebook.as_ref().and_then(|p| self.library.notebooks.iter().find(|n| &n.path == p));
        match (notebook, &self.lib_ui.section) {
            (Some(_), Some(sec)) => sec.clone(),
            (Some(nb), None) => nb.path.clone(),
            _ => self.library_root(),
        }
    }

    /// Creates an untitled note in `dir` and opens it.
    pub(super) fn new_note(&mut self, dir: &Path) {
        match library::create_note(dir, "Untitled note") {
            Ok(p) => {
                self.refresh_library();
                self.open_note(&p);
                // The paper chosen for new notes (Settings).
                if self.note_path.as_deref() == Some(p.as_path()) {
                    let s = &self.settings;
                    self.doc.page_width = s.new_page_width;
                    self.doc.page_height = s.new_page_height;
                    self.doc.page.kind = s.new_page_kind;
                    self.doc.page.spacing = s.new_page_spacing;
                    self.save_note(false);
                }
            }
            Err(e) => self.toast(format!("Cannot create note: {e}")),
        }
    }

    pub(super) fn apply_act(&mut self, a: Act) {
        // Library actions move, rename and delete files: never during a save.
        self.finish_saves();
        let root = self.library_root();
        match a {
            Act::Open(p) => self.open_note(&p),
            Act::NewNote(dir) => self.new_note(&dir),
            Act::OpenNotebook(p) => {
                self.lib_ui.notebook = Some(p);
                self.lib_ui.section = None;
                self.lib_ui.search.clear();
            }
            Act::CloseNotebook(p) => {
                self.external_notebooks.retain(|e| *e != p);
                if self.lib_ui.notebook.as_ref() == Some(&p) {
                    self.lib_ui.notebook = None;
                    self.lib_ui.section = None;
                }
                self.refresh_library();
            }
            Act::Home => self.apply_act(Act::Show(LibView::Home)),
            Act::Show(view) => {
                self.lib_ui.view = view;
                self.lib_ui.notebook = None;
                self.lib_ui.section = None;
                self.lib_ui.search.clear();
                self.refresh_library();
            }
            Act::QuickNote => self.new_note(&root),
            Act::Sort(sort) => self.settings.library_sort = sort,
            Act::SelectSection(s) => self.lib_ui.section = s,
            Act::Dialog(d) => self.show_dialog(d),
            Act::CloseDialog => self.lib_ui.dialog = None,
            Act::CreateNotebook(name) => match library::create_notebook(&root, &name) {
                Ok(p) => {
                    self.refresh_library();
                    self.lib_ui.notebook = Some(p);
                    self.lib_ui.section = None;
                }
                Err(e) => self.toast(format!("Cannot create notebook: {e}")),
            },
            Act::CreateSection(nb, name) => match library::create_section(&nb, &name) {
                Ok(p) => {
                    self.refresh_library();
                    self.lib_ui.section = Some(p);
                }
                Err(e) => self.toast(format!("Cannot create section: {e}")),
            },
            // The open note's header is kept in memory and written on save.
            Act::RenameNote(path, title) if self.note_path.as_ref() == Some(&path) => {
                self.rename_open_note(&title);
            }
            Act::RenameNote(path, title) => match library::rename_note(&path, &title) {
                Ok(new) => {
                    self.settings.moved(&path, &new);
                    self.refresh_library();
                }
                Err(e) => self.toast(format!("Rename failed: {e}")),
            },
            Act::RenameDir(path, name) => match library::rename_dir(&path, &name) {
                Ok(new) => {
                    self.settings.moved(&path, &new);
                    for nb in self.external_notebooks.iter_mut().filter(|nb| **nb == path) {
                        nb.clone_from(&new);
                    }
                    // The open note moved with its notebook / section.
                    if let Some(rel) = self.note_path.as_ref().and_then(|p| p.strip_prefix(&path).ok()) {
                        self.note_path = Some(new.join(rel));
                    }
                    if self.lib_ui.notebook.as_ref() == Some(&path) {
                        self.lib_ui.notebook = Some(new.clone());
                        self.lib_ui.section = None;
                    } else if self.lib_ui.section.as_ref() == Some(&path) {
                        self.lib_ui.section = Some(new);
                    }
                    self.refresh_library();
                }
                Err(e) => self.toast(format!("Rename failed: {e}")),
            },
            Act::Trash(path) if self.note_path.as_ref().is_some_and(|p| p.starts_with(&path)) => {
                self.toast("The open note cannot be deleted; close it first");
            }
            Act::Trash(path) => match library::trash(&root, &path) {
                Ok(()) => {
                    if self.lib_ui.notebook.as_ref() == Some(&path) {
                        self.lib_ui.notebook = None;
                        self.lib_ui.section = None;
                    } else if self.lib_ui.section.as_ref() == Some(&path) {
                        self.lib_ui.section = None;
                    }
                    self.refresh_library();
                    self.toast("Moved to the trash");
                }
                Err(e) => self.toast(format!("Delete failed: {e}")),
            },
            Act::Clip(path, cut) => self.copy_note(path, cut),
            Act::Paste(dir) => self.paste_note(dir),
            // The open note as it is shown, not as last saved.
            Act::Duplicate(path) if !self.save_if_open(&path) => {}
            Act::Duplicate(path) => match library::duplicate_note(&path) {
                Ok(_) => self.refresh_library(),
                Err(e) => self.toast(format!("Duplicate failed: {e}")),
            },
            // Already there (dropped onto its own folder): nothing to do.
            Act::MoveNote(path, dir) if path.parent() == Some(dir.as_path()) => {}
            Act::MoveNote(path, dir) => {
                // The open note is saved first and followed to its new place.
                let open = self.note_path.as_ref() == Some(&path);
                if !self.save_if_open(&path) {
                    return;
                }
                match library::move_note(&path, &dir) {
                    Ok(new) => {
                        self.settings.moved(&path, &new);
                        if open {
                            self.note_path = Some(new);
                        }
                        self.refresh_library();
                    }
                    Err(e) => self.toast(format!("Move failed: {e}")),
                }
            }
            Act::Restore(item) => match library::restore(&root, &item) {
                Ok(_) => {
                    self.refresh_library();
                    self.toast("Restored");
                }
                Err(e) => self.toast(format!("Restore failed: {e}")),
            },
            Act::DeleteForever(item) => match library::delete_forever(&root, &item) {
                Ok(()) => self.refresh_library(),
                Err(e) => self.toast(format!("Delete failed: {e}")),
            },
            Act::EmptyTrash => match library::empty_trash(&root) {
                Ok(_) => {
                    self.refresh_library();
                    self.toast("Trash emptied");
                }
                Err(e) => {
                    self.refresh_library();
                    self.toast(format!("Cannot empty the trash: {e}"));
                }
            },
            Act::Import(dir, text) => {
                let src = PathBuf::from(text.trim().trim_matches('"'));
                match library::import_file(&dir, &src) {
                    Ok(_) => {
                        self.refresh_library();
                        self.toast("Imported");
                    }
                    Err(e) => self.toast(format!("Import failed: {e}")),
                }
            }
            Act::ImportPdf(dir, text) => {
                let src = PathBuf::from(text.trim().trim_matches('"'));
                self.import_pdf(dir, src);
            }
            Act::OpenPdf(path) => {
                let ctx = self.egui_ctx.clone();
                self.open_pdf(&ctx, &path);
            }
            Act::ForgetPdf(path) => self.settings.forget_pdf(&path),
            Act::SetRoot(text) => {
                let p = PathBuf::from(text.trim());
                match std::fs::create_dir_all(&p) {
                    Ok(()) => {
                        self.settings.library_root = p.to_string_lossy().into_owned();
                        self.lib_ui.notebook = None;
                        self.lib_ui.section = None;
                        // Not the old folder's notebooks meanwhile: the spinner.
                        self.library = library::Library::empty(p);
                        self.library_loaded = false;
                        self.refresh_library();
                    }
                    Err(e) => self.toast(format!("Cannot use that folder: {e}")),
                }
            }
            Act::Refresh => self.refresh_library(),
        }
    }
}

// ------------------------------------------------------------------ views

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(space::S);
    ui.label(theme::heading(text));
    ui.add_space(space::M);
}

/// A section heading with the sort menu on its right.
fn heading_with_sort(ui: &mut Ui, text: &str, sort: LibrarySort, acts: &mut Vec<Act>) {
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        ui.label(theme::heading(text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            theme::ghost_buttons(ui);
            let menu = Id::unique(("lib_sort", text));
            let button = ui.button(format!("{}  {}  {}", icon::SORT_ASCENDING, sort.label(), icon::CARET_DOWN));
            button_menu(menu, &button, |ui| {
                for s in [LibrarySort::Manual, LibrarySort::Edited, LibrarySort::Title] {
                    let glyph = if s == sort { icon::CHECK } else { "" };
                    if entry(ui, glyph, s.label(), "", true) {
                        acts.push(Act::Sort(s));
                    }
                }
            });
        });
    });
    ui.add_space(space::M);
}

/// A large icon, a title and a hint, centred: an empty place.
fn empty_state(ui: &mut Ui, glyph: &str, title: &str, hint: &str) {
    ui.add_space(48.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(glyph).size(48.0).color(color::text_faint()));
        ui.add_space(space::M);
        ui.label(theme::heading(title));
        ui.add_space(space::XS);
        ui.label(theme::muted(hint));
    });
    ui.add_space(space::XL);
}

/// Where a new note goes, for tooltips.
fn place_name(notebook: Option<&NotebookInfo>, section: Option<&Path>) -> String {
    match notebook {
        Some(nb) => match section.and_then(|s| nb.sections.iter().find(|x| x.path == s)) {
            Some(s) => format!("\u{201c}{} \u{203a} {}\u{201d}", nb.name, s.name),
            None => format!("\u{201c}{}\u{201d}", nb.name),
        },
        None => "the quick notes".to_owned(),
    }
}

/// Notes in the chosen order.
fn sorted(mut notes: Vec<&NoteInfo>, sort: LibrarySort) -> Vec<&NoteInfo> {
    match sort {
        // The folder's own order (dragged in the notebook panel).
        LibrarySort::Manual => {}
        LibrarySort::Edited => notes.sort_by_key(|n| std::cmp::Reverse(n.meta.modified)),
        LibrarySort::Title => notes.sort_by_key(|n| n.meta.title.to_lowercase()),
    }
    notes
}

/// Big buttons for the most common starts.
fn action_tile(ui: &mut Ui, glyph: &str, title: &str, hint: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(220.0, 72.0), Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    p.rect(
        rect,
        radius::MEDIUM,
        if hovered { color::raised() } else { color::surface() },
        Stroke::new(1.0, if hovered { color::accent() } else { color::border() }),
        egui::StrokeKind::Middle,
    );
    let icon_rect = Rect::from_center_size(pos2(rect.left() + 34.0, rect.center().y), vec2(40.0, 40.0));
    p.rect_filled(icon_rect, radius::MEDIUM, color::accent_alpha(if hovered { 60 } else { 34 }));
    p.text(icon_rect.center(), Align2::CENTER_CENTER, glyph, theme::font(22.0), color::accent_hover());
    let x = rect.left() + 66.0;
    p.text(
        pos2(x, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        title,
        theme::font_strong(size::BODY),
        color::text_strong(),
    );
    p.text(pos2(x, rect.center().y + 11.0), Align2::LEFT_CENTER, hint, theme::font(size::CAPTION), color::text_muted());
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn home_view(
    ui: &mut Ui,
    lib: &library::Library,
    root: &Path,
    recent_count: usize,
    pdfs: &[String],
    acts: &mut Vec<Act>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(space::L, space::L);
        if action_tile(ui, icon::LIGHTNING, "Quick note", "Start writing right away").clicked() {
            acts.push(Act::QuickNote);
        }
        if action_tile(ui, icon::NOTEBOOK, "New notebook", "Notes grouped in sections").clicked() {
            acts.push(Act::Dialog(Dialog::NewNotebook(String::new())));
        }
        if action_tile(ui, icon::BOOK_OPEN, "Read a PDF", "Books, papers, manuals").clicked() {
            acts.push(Act::Dialog(Dialog::OpenPdf(String::new())));
        }
        if action_tile(ui, icon::FILE_ARROW_DOWN, "Write on a PDF", "Slides, papers, forms").clicked() {
            acts.push(Act::Dialog(Dialog::ImportPdf(String::new())));
        }
    });
    ui.add_space(space::XL);

    let recent = lib.recent(recent_count);
    heading(ui, "Recent");
    if recent.is_empty() {
        ui.label(theme::muted("Notes you edit show up here."));
    }
    note_grid(ui, &recent, root, true, None, acts);

    if !pdfs.is_empty() {
        ui.add_space(space::XL);
        heading(ui, "Recently read");
        recent_pdfs(ui, pdfs, acts);
    }

    ui.add_space(space::XL);
    heading(ui, "Notebooks");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(space::L, space::L);
        for nb in &lib.notebooks {
            notebook_cover(ui, nb, acts);
        }
        let (rect, _) = ui.allocate_exact_size(vec2(COVER_W, COVER_H), Sense::hover());
        if theme::add_tile(ui, rect, Id::unique("new_notebook_tile"), "New notebook").clicked() {
            acts.push(Act::Dialog(Dialog::NewNotebook(String::new())));
        }
    });
}

/// The PDFs recently opened in the reader, most recent first: a click
/// opens one again, where it was left.
fn recent_pdfs(ui: &mut Ui, pdfs: &[String], acts: &mut Vec<Act>) {
    let width = ui.available_width().min(560.0);
    ui.allocate_ui(vec2(width, 0.0), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for path in pdfs.iter().rev() {
            let p = Path::new(path);
            let name = p.file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            let row = theme::list_row(ui, icon::FILE_PDF, &name, false, theme::ROW_BUTTON);
            let forget = ui.rect_contains_pointer(row.rect)
                && theme::row_button(
                    ui,
                    row.rect,
                    0,
                    Id::unique(("forget_pdf", path)),
                    icon::X,
                    "Remove from this list",
                    false,
                )
                .clicked();
            if forget {
                acts.push(Act::ForgetPdf(path.clone()));
            } else if row.on_hover_text(path).clicked() {
                acts.push(Act::OpenPdf(p.to_path_buf()));
            }
        }
    });
}

fn quick_view(ui: &mut Ui, lib: &library::Library, root: &Path, sort: LibrarySort, acts: &mut Vec<Act>) {
    let notes = sorted(lib.quick.iter().collect(), sort);
    heading_with_sort(ui, &count(notes.len(), "note"), sort, acts);
    if notes.is_empty() {
        ui.label(theme::muted("Notes outside any notebook. Drag a note here from a notebook, or start one:"));
        ui.add_space(space::M);
    }
    note_grid(ui, &notes, root, false, Some(root), acts);
}

fn notebook_view(
    ui: &mut Ui,
    nb: &NotebookInfo,
    section: Option<&Path>,
    root: &Path,
    sort: LibrarySort,
    acts: &mut Vec<Act>,
) {
    // Section tabs, as pills.
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(space::S, space::S);
        let tint = Color32::from_rgb(nb.color[0], nb.color[1], nb.color[2]);
        if (!nb.loose.is_empty() || nb.sections.is_empty())
            && pill(ui, "General", nb.loose.len(), section.is_none(), tint).clicked()
        {
            acts.push(Act::SelectSection(None));
        }
        for s in &nb.sections {
            let resp = pill(ui, &s.name, s.notes.len(), section == Some(s.path.as_path()), tint);
            if resp.clicked() {
                acts.push(Act::SelectSection(Some(s.path.clone())));
            }
            drop_target(&resp, acts, |path| Act::MoveNote(path, s.path.clone()));
            resp.context_menu(|ui| {
                if entry(ui, icon::PENCIL_SIMPLE, "Rename section...", "", true) {
                    acts.push(Act::Dialog(Dialog::RenameDir { path: s.path.clone(), text: s.name.clone() }));
                    ui.close();
                }
                ui.separator();
                if entry(ui, icon::TRASH, "Delete section...", "", true) {
                    acts.push(Act::Dialog(Dialog::Trash { path: s.path.clone(), label: s.name.clone() }));
                    ui.close();
                }
            });
        }
        theme::ghost_buttons(ui);
        if ui.button(format!("{}  Section", icon::PLUS)).on_hover_text("New section").clicked() {
            acts.push(Act::Dialog(Dialog::NewSection { notebook: nb.path.clone(), text: String::new() }));
        }
    });
    ui.add_space(space::M);

    let (dir, notes): (&Path, Vec<&NoteInfo>) = match section.and_then(|p| nb.sections.iter().find(|s| s.path == p)) {
        Some(s) => (&s.path, s.notes.iter().collect()),
        None => (&nb.path, nb.loose.iter().collect()),
    };
    let notes = sorted(notes, sort);
    heading_with_sort(ui, &count(notes.len(), "note"), sort, acts);
    note_grid(ui, &notes, root, false, Some(dir), acts);
}

/// Everything deleted: where it was and when, restore or erase it.
fn trash_view(ui: &mut Ui, lib: &library::Library, acts: &mut Vec<Act>) {
    if lib.trash.is_empty() {
        empty_state(
            ui,
            icon::TRASH,
            "The trash is empty",
            "Deleted notes, sections and notebooks wait here until you erase them.",
        );
        return;
    }
    ui.horizontal(|ui| {
        ui.label(theme::muted(format!(
            "{}. Restoring puts an item back where it was.",
            count(lib.trash.len(), "item")
        )));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(theme::danger_button(format!("{}  Empty trash", icon::BROOM))).clicked() {
                acts.push(Act::Dialog(Dialog::EmptyTrash(lib.trash.len())));
            }
        });
    });
    ui.add_space(space::M);
    let root = &lib.root;
    egui::Frame::NONE
        .fill(color::surface())
        .corner_radius(radius::MEDIUM)
        .stroke(Stroke::new(1.0, color::border()))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (i, item) in lib.trash.iter().enumerate() {
                if i > 0 {
                    let y = ui.cursor().top();
                    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, color::border()));
                }
                trash_row(ui, item, root, acts);
            }
        });
}

fn trash_row(ui: &mut Ui, item: &library::TrashItem, root: &Path, acts: &mut Vec<Act>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, color::hover().gamma_multiply(0.5));
    }
    let p = ui.painter();
    let y = rect.center().y;
    let glyph = if item.is_folder { icon::FOLDER_SIMPLE } else { icon::NOTE_BLANK };
    p.text(
        pos2(rect.left() + space::L + 10.0, y),
        Align2::CENTER_CENTER,
        glyph,
        theme::font(size::ICON),
        color::text_muted(),
    );
    let x = rect.left() + space::L + 32.0;
    p.text(
        pos2(x, y - 8.0),
        Align2::LEFT_CENTER,
        truncate(&item.name, 60),
        theme::font_strong(size::BODY),
        color::text_strong(),
    );
    let from = match &item.origin {
        Some(o) if o.parent() == Some(root) => "From the library".to_owned(),
        Some(o) => format!("From {}", location(root, o)),
        None => "Origin unknown (restored to the library)".to_owned(),
    };
    let detail = format!("{from} \u{b7} deleted {}", when(item.deleted).to_lowercase());
    p.text(pos2(x, y + 10.0), Align2::LEFT_CENTER, detail, theme::font(size::CAPTION), color::text_muted());

    let buttons = Rect::from_min_max(pos2(rect.right() - 340.0, rect.top()), rect.max).shrink2(vec2(space::L, 10.0));
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(buttons).layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            theme::ghost_buttons(ui);
            if ui.button(RichText::new(format!("{}  Delete permanently", icon::X)).color(color::danger())).clicked() {
                acts.push(Act::Dialog(Dialog::DeleteForever { path: item.path.clone(), label: item.name.clone() }));
            }
            if ui.button(format!("{}  Restore", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
                acts.push(Act::Restore(item.path.clone()));
            }
        },
    );
}

/// A section tab: name and count, the selected one in the notebook's colour.
fn pill(ui: &mut Ui, name: &str, n: usize, selected: bool, tint: Color32) -> egui::Response {
    let text = format!("{name}  {n}");
    let galley = ui.painter().layout_no_wrap(text, theme::font(size::BODY), color::text());
    let size = galley.size() + vec2(2.0 * space::M, 12.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let (fill, stroke, fg) = if selected {
        (tint.gamma_multiply(0.35), Stroke::new(1.0, tint), color::text_strong())
    } else if resp.hovered() {
        (color::hover(), Stroke::new(1.0, color::border_strong()), color::text_strong())
    } else {
        (color::surface(), Stroke::new(1.0, color::border()), color::text())
    };
    let dropping = egui::DragAndDrop::has_payload_of_type::<DraggedNote>(ui.ctx()) && resp.contains_pointer();
    let stroke = if dropping { Stroke::new(2.0, color::accent()) } else { stroke };
    ui.painter().rect(rect, rect.height() * 0.5, fill, stroke, egui::StrokeKind::Middle);
    ui.painter().galley(rect.center() - galley.size() * 0.5, galley, fg);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A row of the sidebar: icon, name, a count; the place shown is
/// highlighted. A note dragged over it is outlined.
fn nav_row(
    ui: &mut Ui,
    glyph: &str,
    label: &str,
    n: Option<usize>,
    selected: bool,
    tint: Option<Color32>,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_HEIGHT + 4.0), Sense::click());
    let hovered = resp.hovered();
    let dropping = egui::DragAndDrop::has_payload_of_type::<DraggedNote>(ui.ctx()) && resp.contains_pointer();
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, radius::SMALL, color::accent_alpha(46));
        let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 7.0), vec2(3.0, rect.height() - 14.0));
        p.rect_filled(bar, 1.5, color::accent());
    } else if hovered || dropping {
        p.rect_filled(rect, radius::SMALL, color::hover());
    }
    if dropping {
        p.rect_stroke(rect, radius::SMALL, Stroke::new(1.5, color::accent()), egui::StrokeKind::Middle);
    }
    let y = rect.center().y;
    let icon_color = tint.unwrap_or(if selected { color::accent_hover() } else { color::text_muted() });
    p.text(
        pos2(rect.left() + space::M + 6.0, y),
        Align2::CENTER_CENTER,
        glyph,
        theme::font(size::BODY + 1.0),
        icon_color,
    );
    let count_w = match n {
        Some(n) => {
            let g = ui.painter().layout_no_wrap(n.to_string(), theme::font(size::CAPTION), color::text_faint());
            let w = g.size().x;
            ui.painter().galley(pos2(rect.right() - space::S - w, y - g.size().y * 0.5), g, color::text_faint());
            w + space::S
        }
        None => 0.0,
    };
    let left = rect.left() + space::M + 22.0;
    let text_color = if selected || hovered { color::text_strong() } else { color::text() };
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), theme::font(size::BODY), text_color);
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.right() - space::S - count_w - left).max(0.0));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos2(left, y - galley.size().y * 0.5), galley, text_color);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A dragged note released over `resp` becomes `act(path)`.
fn drop_target(resp: &egui::Response, acts: &mut Vec<Act>, act: impl FnOnce(PathBuf) -> Act) {
    if let Some(d) = resp.dnd_release_payload::<DraggedNote>() {
        acts.push(act(d.path.clone()));
    }
}

fn notebook_menu(ui: &mut Ui, nb: &NotebookInfo, acts: &mut Vec<Act>) {
    if entry(ui, icon::ARROW_SQUARE_OUT, "Open", "", true) {
        acts.push(Act::OpenNotebook(nb.path.clone()));
        ui.close();
    }
    if entry(ui, icon::FOLDER_SIMPLE_PLUS, "New section...", "", true) {
        acts.push(Act::Dialog(Dialog::NewSection { notebook: nb.path.clone(), text: String::new() }));
        ui.close();
    }
    if entry(ui, icon::PENCIL_SIMPLE, "Rename...", "", true) {
        acts.push(Act::Dialog(Dialog::RenameDir { path: nb.path.clone(), text: nb.name.clone() }));
        ui.close();
    }
    let paste = super::clipboard::note_pastable(ui.ctx());
    if entry(ui, icon::CLIPBOARD, "Paste note here", "", paste) {
        acts.push(Act::Paste(nb.path.clone()));
        ui.close();
    }
    ui.separator();
    if nb.external {
        if entry(ui, icon::X, "Close", "", true) {
            acts.push(Act::CloseNotebook(nb.path.clone()));
            ui.close();
        }
    } else if entry(ui, icon::TRASH, "Delete...", "", true) {
        acts.push(Act::Dialog(Dialog::Trash { path: nb.path.clone(), label: nb.name.clone() }));
        ui.close();
    }
}

/// The name of the folder that holds `path`, or the whole path of a drive.
fn parent_name(path: &Path) -> String {
    let parent = path.parent().unwrap_or(path);
    parent.file_name().map_or_else(|| parent.display().to_string(), |n| n.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------ cards

/// The notes as cards; with `new_in`, a "New note" tile (for that folder)
/// comes first.
fn note_grid(
    ui: &mut Ui,
    notes: &[&NoteInfo],
    root: &Path,
    show_location: bool,
    new_in: Option<&Path>,
    acts: &mut Vec<Act>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(space::L, space::L);
        if let Some(dir) = new_in {
            let (rect, _) = ui.allocate_exact_size(vec2(CARD_W, CARD_W * 1.414), Sense::hover());
            if theme::add_tile(ui, rect, Id::unique(("new_note_tile", dir)), "New note").clicked() {
                acts.push(Act::NewNote(dir.to_path_buf()));
            }
        }
        for n in notes {
            note_card(ui, n, root, show_location, acts);
        }
    });
}

fn note_menu(ui: &mut Ui, note: &NoteInfo, acts: &mut Vec<Act>) {
    if entry(ui, icon::ARROW_SQUARE_OUT, "Open", "", true) {
        acts.push(Act::Open(note.path.clone()));
        ui.close();
    }
    if entry(ui, icon::PENCIL_SIMPLE, "Rename...", "", true) {
        acts.push(Act::Dialog(Dialog::RenameNote { path: note.path.clone(), text: note.meta.title.clone() }));
        ui.close();
    }
    if entry(ui, icon::COPY, "Duplicate", "", true) {
        acts.push(Act::Duplicate(note.path.clone()));
        ui.close();
    }
    if entry(ui, icon::COPY_SIMPLE, "Copy", "", true) {
        acts.push(Act::Clip(note.path.clone(), false));
        ui.close();
    }
    if entry(ui, icon::SCISSORS, "Cut", "", true) {
        acts.push(Act::Clip(note.path.clone(), true));
        ui.close();
    }
    ui.separator();
    if entry(ui, icon::TRASH, "Delete", "", true) {
        acts.push(Act::Trash(note.path.clone()));
        ui.close();
    }
}

/// A note: page 1, title, when it was edited and its pages. On hover a "⋯"
/// menu; drag it onto a notebook, the quick notes or the trash.
fn note_card(ui: &mut Ui, note: &NoteInfo, root: &Path, show_location: bool, acts: &mut Vec<Act>) {
    let page_ratio =
        if note.meta.preview.page_w > 0.0 { note.meta.preview.page_h / note.meta.preview.page_w } else { 1.414 };
    let thumb_h = (CARD_W * page_ratio).clamp(CARD_W * 0.6, CARD_W * 1.5);

    ui.vertical(|ui| {
        ui.set_width(CARD_W);
        let (rect, resp) = ui.allocate_exact_size(vec2(CARD_W, thumb_h), Sense::click_and_drag());
        let p = ui.painter();
        p.add(theme::card_shadow().as_shape(rect, egui::CornerRadius::from(radius::SMALL)));
        paint_preview(&ui.painter_at(rect), rect, note);
        let menu = Id::unique(("note_card_menu", &note.path));
        let menu_open = egui::Popup::is_id_open(ui.ctx(), menu);
        let hovered = ui.rect_contains_pointer(rect);
        if hovered || menu_open {
            ui.painter().rect_stroke(
                rect.expand(1.0),
                radius::SMALL,
                Stroke::new(2.0, color::accent()),
                egui::StrokeKind::Middle,
            );
        }
        if resp.drag_started() {
            egui::DragAndDrop::set_payload(
                ui.ctx(),
                DraggedNote { path: note.path.clone(), title: note.meta.title.clone() },
            );
        }
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() {
            acts.push(Act::Open(note.path.clone()));
        }
        resp.context_menu(|ui| note_menu(ui, note, acts));
        if (hovered || menu_open) && !egui::DragAndDrop::has_any_payload(ui.ctx()) {
            let r = Rect::from_min_size(pos2(rect.right() - 32.0, rect.top() + 6.0), vec2(26.0, 26.0));
            let more = ui.interact(r, menu.with("button"), Sense::click()).on_hover_text("Actions");
            let fill = if more.hovered() { color::raised() } else { color::surface().gamma_multiply(0.92) };
            ui.painter().rect(r, radius::SMALL, fill, Stroke::new(1.0, color::border()), egui::StrokeKind::Middle);
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                icon::DOTS_THREE,
                theme::font(size::BODY),
                color::text_strong(),
            );
            button_menu(menu, &more, |ui| note_menu(ui, note, acts));
        }

        ui.add_space(space::XS);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add(
            egui::Label::new(
                RichText::new(&note.meta.title).font(theme::font_strong(size::BODY)).color(color::text_strong()),
            )
            .truncate(),
        );
        let pages =
            if note.meta.pages == 0 { "Infinite page".to_owned() } else { count(note.meta.pages as usize, "page") };
        ui.label(RichText::new(format!("{} · {}", when(note.meta.modified), pages)).small().color(color::text_muted()));
        if show_location {
            ui.add(
                egui::Label::new(RichText::new(location(root, &note.path)).small().color(color::text_faint()))
                    .truncate(),
            );
        }
    });
}

fn notebook_cover(ui: &mut Ui, nb: &NotebookInfo, acts: &mut Vec<Act>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(COVER_W, COVER_H), Sense::click());
    let c = nb.color;
    let base = Color32::from_rgb(c[0], c[1], c[2]);
    let p = ui.painter();
    let r = radius::MEDIUM;
    p.add(theme::card_shadow().as_shape(rect, egui::CornerRadius::from(r)));
    p.rect_filled(rect, r, base);
    // Spine.
    let spine = Rect::from_min_max(rect.min, pos2(rect.min.x + 14.0, rect.max.y));
    p.rect_filled(spine, egui::CornerRadius { nw: r as u8, sw: r as u8, ne: 0, se: 0 }, base.gamma_multiply(0.7));
    p.text(
        pos2(rect.min.x + 26.0, rect.min.y + 14.0),
        Align2::LEFT_TOP,
        truncate(&nb.name, 16),
        theme::font_strong(size::HEADING),
        Color32::WHITE,
    );
    let total: usize = nb.loose.len() + nb.sections.iter().map(|s| s.notes.len()).sum::<usize>();
    p.text(
        pos2(rect.min.x + 26.0, rect.max.y - 12.0),
        Align2::LEFT_BOTTOM,
        format!("{} · {}", count(nb.sections.len(), "section"), count(total, "note")),
        theme::font(size::CAPTION),
        Color32::from_white_alpha(210),
    );
    let dropping = egui::DragAndDrop::has_payload_of_type::<DraggedNote>(ui.ctx()) && resp.contains_pointer();
    if resp.hovered() || dropping {
        p.rect_stroke(rect.expand(1.0), r, Stroke::new(2.0, color::accent()), egui::StrokeKind::Middle);
    }
    drop_target(&resp, acts, |path| Act::MoveNote(path, nb.path.clone()));
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        acts.push(Act::OpenNotebook(nb.path.clone()));
    }
    resp.context_menu(|ui| notebook_menu(ui, nb, acts));
}

/// "1 note", "3 notes".
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// Cards' previews unused for this many frames are forgotten.
const PREVIEW_TTL_FRAMES: u64 = 600;

/// The cards' previews as triangles kept on the GPU (see `stroke_gpu`):
/// made once, not again on every frame the library is shown (each hover
/// repaints it). By note and how many times it was already shown in this
/// frame (a note can be on two cards: a batch is drawn once per frame).
#[derive(Default)]
struct PreviewCache {
    batches: HashMap<(PathBuf, u32), PreviewBatch>,
    /// Cards shown per note in `frame`.
    shown: HashMap<PathBuf, u32>,
    frame: u64,
}

struct PreviewBatch {
    /// What it was made from: the note's modified time and strokes, the
    /// card's scale and the pixels per point.
    made_for: (u64, usize, u32, u32),
    batch: Batch,
    last_used: u64,
}

/// Page 1 as stored in the note header (no document loading).
fn paint_preview(painter: &egui::Painter, rect: Rect, note: &NoteInfo) {
    let pv: &Preview = &note.meta.preview;
    painter.rect_filled(rect, radius::SMALL, color::page());
    if pv.page_w <= 0.0 {
        return;
    }
    let scale = rect.width() / pv.page_w;
    let at = |x: f32, y: f32| pos2(rect.min.x + x * scale, rect.min.y + y * scale);
    let ctx = painter.ctx();
    let (frame, ppp) = (ctx.cumulative_frame_nr(), ctx.pixels_per_point());
    let cache =
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Arc<Mutex<PreviewCache>>>(Id::unique("note_previews")).clone());
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.frame != frame {
        cache.frame = frame;
        cache.shown.clear();
        cache.batches.retain(|_, b| frame - b.last_used < PREVIEW_TTL_FRAMES);
    }
    let shown = cache.shown.entry(note.path.clone()).or_default();
    let key = (note.path.clone(), *shown);
    *shown += 1;
    let made_for = (note.meta.modified, pv.strokes.len(), scale.to_bits(), ppp.to_bits());
    let entry = cache.batches.entry(key).or_insert_with(|| PreviewBatch {
        made_for: (0, 0, 0, 0),
        batch: Batch::default(),
        last_used: frame,
    });
    entry.last_used = frame;
    if entry.made_for != made_for {
        let mut mesh = egui::epaint::Mesh::default();
        for s in &pv.strokes {
            // Full pressure: the stroke's own width.
            let pts: Vec<Point> = s.pts.iter().map(|p| Point::new(p[0], p[1], 1.0)).collect();
            crate::stroke_mesh::tessellate(&mut mesh, StrokeKind::Pen, s.color, s.width, &pts, scale, 1.0 / ppp);
        }
        entry.batch.replace(mesh);
        entry.made_for = made_for;
    }
    stroke_gpu::paint_batch(painter, &entry.batch, rect.min.to_vec2(), 1.0, Blend::Over);
    for b in &pv.bars {
        let r = Rect::from_min_size(at(b[0], b[1]), vec2(b[2] * scale, (b[3] * scale).max(1.0)));
        painter.rect_filled(r, 0.5, Color32::from_black_alpha(140));
    }
}

// ---------------------------------------------------------------- helpers

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// "Notebook › Section", or "Quick notes" for notes at the library root.
pub(super) fn location(root: &Path, note: &Path) -> String {
    let Some(rel) = note.parent().and_then(|p| p.strip_prefix(root).ok()) else {
        // Outside the library (an external notebook): its folder.
        return parent_name(note);
    };
    if rel.components().count() == 0 {
        return "Quick notes".to_owned();
    }
    rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join(" › ")
}

/// Relative time for recent changes, calendar date (UTC) for older ones.
pub(super) fn when(ts: u64) -> String {
    let now = now_secs();
    let d = now.saturating_sub(ts);
    match d {
        0..=59 => "Just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=604_799 => {
            let days = d / 86_400;
            if days == 1 { "Yesterday".into() } else { format!("{days} days ago") }
        }
        _ => {
            let (y, m, day) = civil_from_days((ts / 86_400) as i64);
            const MONTHS: [&str; 12] =
                ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{day} {} {y}", MONTHS[(m as usize - 1).min(11)])
        }
    }
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_745), (2026, 10, 19));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn sorting_and_place_names() {
        use tratto_core::storage::NoteMeta;
        let note = |title: &str, modified: u64| {
            let mut meta = NoteMeta::new(title, false);
            meta.modified = modified;
            NoteInfo { path: PathBuf::from(format!("{title}.ink")), meta }
        };
        let (a, b, c) = (note("banana", 1), note("Apple", 3), note("cherry", 2));
        let notes = vec![&a, &b, &c];
        let titles = |v: Vec<&NoteInfo>| v.iter().map(|n| n.meta.title.clone()).collect::<Vec<_>>();
        assert_eq!(titles(sorted(notes.clone(), LibrarySort::Manual)), ["banana", "Apple", "cherry"]);
        assert_eq!(titles(sorted(notes.clone(), LibrarySort::Edited)), ["Apple", "cherry", "banana"]);
        assert_eq!(titles(sorted(notes, LibrarySort::Title)), ["Apple", "banana", "cherry"]);
        assert_eq!(place_name(None, None), "the quick notes");
    }

    /// Every view drawn with a real library (a notebook with a section,
    /// notes, a quick note, something in the trash): nothing panics, and
    /// the views offer their actions.
    #[test]
    fn every_view_draws() {
        let root = std::env::temp_dir().join("tratto_library_ui_test");
        let _ = std::fs::remove_dir_all(&root);
        let nb = library::create_notebook(&root, "Work").unwrap();
        library::create_note(&nb.join("Section 1"), "Plan").unwrap();
        library::create_note(&nb, "Loose").unwrap();
        let quick = library::create_note(&root, "Groceries").unwrap();
        library::create_note(&root, "Old").map(|p| library::trash(&root, &p)).unwrap().unwrap();
        let lib = library::Library::scan(&root).unwrap();
        assert_eq!((lib.quick.len(), lib.trash.len()), (1, 1));
        let nb = lib.notebooks[0].clone();

        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut acts = Vec::new();
        crate::test_frame(&ctx, egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let pdfs = ["/papers/a.pdf".to_owned(), "b.pdf".to_owned()];
                home_view(ui, &lib, &root, 12, &pdfs, &mut acts);
                quick_view(ui, &lib, &root, LibrarySort::Edited, &mut acts);
                notebook_view(ui, &nb, None, &root, LibrarySort::Manual, &mut acts);
                notebook_view(ui, &nb, Some(&nb.sections[0].path), &root, LibrarySort::Title, &mut acts);
                trash_view(ui, &lib, &mut acts);
            });
        });
        assert!(acts.is_empty(), "nothing was clicked");
        assert_eq!(location(&root, &quick), "Quick notes");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn locations() {
        let root = Path::new("/lib");
        assert_eq!(location(root, Path::new("/lib/a.ink")), "Quick notes");
        assert_eq!(location(root, Path::new("/lib/Work/Plans/a.ink")), "Work › Plans");
        assert_eq!(location(root, Path::new("/home/me/Thesis/a.ink")), "Thesis");
    }
}
