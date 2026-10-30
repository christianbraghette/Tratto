// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The library screen: a sidebar (Quick note, Home, the quick notes, the
//! notebooks, the trash) and the place chosen there: home (quick starts,
//! recent notes, notebooks), the quick notes, a notebook with its section
//! tabs, the trash; plus search and the library's dialogs. Note cards can be
//! dragged onto a notebook, a section, the quick notes or the trash.
//!
//! This file holds the screen's state, its frame (sidebar, top bar, dialogs)
//! and what its [`Act`]s do; the views themselves are in `library_views`.
//!
//! This is a child module of `app`, so it can use the editor state directly
//! (opening a note, toasts, settings) without extra plumbing.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align2, CursorIcon, Frame, Id, Margin, RichText, TextEdit, Ui, vec2};
use egui_phosphor::regular as icon;

use tratto_core::library::{self, NoteInfo, NotebookInfo};

use super::library_views::{
    DraggedNote, count, drop_target, empty_state, heading, home_view, nav_row, note_grid, notebook_menu, notebook_tint,
    notebook_view, parent_name, place_name, quick_view, trash_view,
};
use super::menus::{button_menu, entry};
use super::{InkApp, PathField};
use crate::file_picker::{self, Pick, Request};
use crate::settings::LibrarySort;
use crate::theme::{self, color, size, space};

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
                                let tint = notebook_tint(nb);
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
                            ui.label(RichText::new(icon::NOTEBOOK).size(size::TITLE).color(notebook_tint(nb)));
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
