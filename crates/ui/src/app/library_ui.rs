// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The library screen: recent notes, notebook covers, sections and dialogs.
//!
//! This is a child module of `app`, so it can use the editor state directly
//! (opening a note, toasts, settings) without extra plumbing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CursorIcon, Frame, Margin, Rect, RichText, Rounding, Sense,
    Shape, Stroke, TextEdit, Ui,
};
use egui_phosphor::regular as icon;

use inknote_core::library::{self, NoteInfo, NotebookInfo};
use inknote_core::storage::{now_secs, Preview};

use super::menus::entry;
use super::InkApp;
use crate::theme::{self, color, radius, size, space};

const CARD_W: f32 = 168.0;
const COVER_W: f32 = 168.0;
const COVER_H: f32 = 118.0;

// ------------------------------------------------------------------ state

/// Modal dialogs of the library.
pub enum Dialog {
    NewNotebook(String),
    NewSection { notebook: PathBuf, text: String },
    RenameNote { path: PathBuf, text: String },
    RenameDir { path: PathBuf, text: String },
    Trash { path: PathBuf, label: String },
    Move { path: PathBuf },
    Import(String),
    Folder(String),
}

#[derive(Default)]
pub struct LibraryUi {
    pub search: String,
    /// Open notebook (None = home).
    pub notebook: Option<PathBuf>,
    /// Selected section tab inside the notebook (None = "General").
    pub section: Option<PathBuf>,
    pub dialog: Option<Dialog>,
}

/// Everything the UI can ask for; applied after drawing, so drawing only
/// ever borrows the data immutably. Also used by the editor's notebook panel.
pub(super) enum Act {
    Open(PathBuf),
    NewNote(PathBuf),
    OpenNotebook(PathBuf),
    Home,
    SelectSection(Option<PathBuf>),
    Dialog(Dialog),
    CloseDialog,
    CreateNotebook(String),
    CreateSection(PathBuf, String),
    RenameNote(PathBuf, String),
    RenameDir(PathBuf, String),
    Trash(PathBuf),
    Duplicate(PathBuf),
    MoveNote(PathBuf, PathBuf),
    Import(PathBuf, String),
    SetRoot(String),
    Refresh,
}

// ----------------------------------------------------------------- screen

impl InkApp {
    pub(super) fn library_screen(&mut self, ctx: &egui::Context) {
        // Drop selections that no longer exist (renamed / deleted / synced away).
        if let Some(nb) = self.lib_ui.notebook.clone() {
            match self.library.notebooks.iter().find(|n| n.path == nb) {
                None => {
                    self.lib_ui.notebook = None;
                    self.lib_ui.section = None;
                }
                Some(n) => {
                    if let Some(sec) = &self.lib_ui.section {
                        if !n.sections.iter().any(|s| &s.path == sec) {
                            self.lib_ui.section = None;
                        }
                    }
                }
            }
        }

        let mut acts: Vec<Act> = Vec::new();
        let root = self.library_root();
        let notebook = self
            .lib_ui
            .notebook
            .as_ref()
            .and_then(|p| self.library.notebooks.iter().find(|n| &n.path == p))
            .cloned();
        let target_dir = self.library_target_dir();
        let searching = !self.lib_ui.search.trim().is_empty();

        egui::TopBottomPanel::top("lib_top")
            .frame(Frame::none().fill(color::CHROME).inner_margin(Margin::symmetric(space::XXL, space::M)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(theme::title("Library"));
                    ui.add_space(space::XL);
                    ui.add(
                        TextEdit::singleline(&mut self.lib_ui.search)
                            .hint_text(format!("{}  Search notes", icon::MAGNIFYING_GLASS))
                            .margin(vec2(space::S, 6.0))
                            .desired_width(260.0),
                    );
                    theme::ghost_buttons(ui);
                    if searching && ui.button(icon::X).on_hover_text("Clear the search").clicked() {
                        self.lib_ui.search.clear();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let new_note = theme::primary_button(format!("{}  New note", icon::NOTE_PENCIL));
                        if ui.add(new_note).on_hover_text("Ctrl+N").clicked() {
                            acts.push(Act::NewNote(target_dir.clone()));
                        }
                        if ui.button(format!("{}  New notebook", icon::NOTEBOOK)).clicked() {
                            acts.push(Act::Dialog(Dialog::NewNotebook(String::new())));
                        }
                        ui.add_space(space::S);
                        if ui.button(theme::icon(icon::DOWNLOAD_SIMPLE)).on_hover_text("Import a note file").clicked() {
                            acts.push(Act::Dialog(Dialog::Import(String::new())));
                        }
                        if ui.button(theme::icon(icon::ARROWS_CLOCKWISE)).on_hover_text("Refresh (re-read the folder, e.g. after a sync)").clicked() {
                            acts.push(Act::Refresh);
                        }
                        if ui.button(theme::icon(icon::FOLDER_OPEN)).on_hover_text("Library folder").clicked() {
                            acts.push(Act::Dialog(Dialog::Folder(self.settings.library_root.clone())));
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(color::DESK).inner_margin(Margin::symmetric(space::XXL, space::XL)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if searching {
                        let q = self.lib_ui.search.trim().to_lowercase();
                        let hits: Vec<&NoteInfo> = self
                            .library
                            .all_notes()
                            .into_iter()
                            .filter(|n| n.meta.title.to_lowercase().contains(&q))
                            .collect();
                        heading(ui, &format!("Search results ({})", hits.len()));
                        if hits.is_empty() {
                            ui.weak("No note has that title.");
                        }
                        note_grid(ui, &hits, &root, true, &mut acts);
                    } else if let Some(nb) = &notebook {
                        notebook_view(ui, nb, &self.lib_ui.section, &root, &mut acts);
                    } else {
                        home_view(ui, &self.library, &root, self.settings.recent_count, &mut acts);
                    }
                });
            });

        self.dialogs(ctx, &root, &target_dir, &mut acts);
        self.draw_toast(ctx);
        for a in acts {
            self.apply_act(a);
        }
    }

    fn draw_toast(&self, ctx: &egui::Context) {
        let Some(t) = self.status_at else { return };
        if self.status.is_empty() || t.elapsed() >= Duration::from_millis(3500) {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("lib_toast")));
        theme::paint_toast(&painter, ctx.screen_rect().center_bottom() - vec2(0.0, 48.0), &self.status);
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    // ------------------------------------------------------------ dialogs

    /// The library dialogs, also shown over the editor (notebook panel).
    pub(super) fn editor_dialogs(&mut self, ctx: &egui::Context) {
        if self.lib_ui.dialog.is_none() {
            return;
        }
        let (root, target) = (self.library_root(), self.library_target_dir());
        let mut acts = Vec::new();
        self.dialogs(ctx, &root, &target, &mut acts);
        for a in acts {
            self.apply_act(a);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, root: &Path, target_dir: &Path, acts: &mut Vec<Act>) {
        let Some(dialog) = self.lib_ui.dialog.as_mut() else { return };
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut close = escape;
        let mut result: Option<Act> = None;

        // Name entry used by several dialogs.
        fn name_row(ui: &mut Ui, text: &mut String, hint: &str) -> bool {
            let r = ui.add(TextEdit::singleline(text).hint_text(hint).desired_width(280.0));
            r.request_focus();
            r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
        }

        let title = match dialog {
            Dialog::NewNotebook(_) => "New notebook",
            Dialog::NewSection { .. } => "New section",
            Dialog::RenameNote { .. } | Dialog::RenameDir { .. } => "Rename",
            Dialog::Trash { .. } => "Delete",
            Dialog::Move { .. } => "Move note to",
            Dialog::Import(_) => "Import a note file",
            Dialog::Folder(_) => "Library folder",
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                // Main button: label, destructive, action (None = disabled).
                let mut primary: Option<(&str, bool, Option<Act>)> = None;
                let mut submit = enter;
                let named = |text: &String| !text.trim().is_empty();
                match dialog {
                    Dialog::NewNotebook(text) => {
                        submit |= name_row(ui, text, "Notebook name");
                        primary = Some(("Create", false, named(text).then(|| Act::CreateNotebook(text.clone()))));
                    }
                    Dialog::NewSection { notebook, text } => {
                        submit |= name_row(ui, text, "Section name");
                        primary = Some(("Create", false, named(text).then(|| Act::CreateSection(notebook.clone(), text.clone()))));
                    }
                    Dialog::RenameNote { path, text } => {
                        submit |= name_row(ui, text, "Title");
                        primary = Some(("Rename", false, named(text).then(|| Act::RenameNote(path.clone(), text.clone()))));
                    }
                    Dialog::RenameDir { path, text } => {
                        submit |= name_row(ui, text, "Name");
                        primary = Some(("Rename", false, named(text).then(|| Act::RenameDir(path.clone(), text.clone()))));
                    }
                    Dialog::Trash { path, label } => {
                        ui.label(format!("Move \"{label}\" to the library's .trash folder?"));
                        ui.label(theme::muted("Nothing is erased for good; you can copy it back from .trash.").small());
                        primary = Some(("Delete", true, Some(Act::Trash(path.clone()))));
                    }
                    Dialog::Move { path } => {
                        let from = path.parent().map(Path::to_path_buf).unwrap_or_default();
                        let mut dest: Vec<(String, PathBuf)> = vec![("Library (quick notes)".into(), root.to_path_buf())];
                        for nb in &self.library.notebooks {
                            dest.push((nb.name.clone(), nb.path.clone()));
                            for s in &nb.sections {
                                dest.push((format!("{} › {}", nb.name, s.name), s.path.clone()));
                            }
                        }
                        ui.visuals_mut().button_frame = false;
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for (label, dir) in dest {
                                if dir == from {
                                    continue;
                                }
                                if entry(ui, icon::FOLDER_SIMPLE, &label, "", true) {
                                    result = Some(Act::MoveNote(path.clone(), dir));
                                }
                            }
                        });
                        ui.visuals_mut().button_frame = true;
                        submit = false;
                    }
                    Dialog::Import(text) => {
                        ui.label("Full path of an .ink file (any version):");
                        submit |= name_row(ui, text, "/path/to/notes.ink");
                        ui.label(theme::muted("It is copied into the current notebook or section.").small());
                        primary = Some(("Import", false, named(text).then(|| Act::Import(target_dir.to_path_buf(), text.clone()))));
                    }
                    Dialog::Folder(text) => {
                        ui.label("All notebooks and notes live in this folder:");
                        submit |= name_row(ui, text, "Folder path");
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

        if let Some(r) = result {
            acts.push(r);
            acts.push(Act::CloseDialog);
        } else if close {
            acts.push(Act::CloseDialog);
        }
    }

    // ------------------------------------------------------------ actions

    /// Folder that new notes go to: the open section or notebook, else the root.
    pub(super) fn library_target_dir(&self) -> PathBuf {
        let notebook = self
            .lib_ui
            .notebook
            .as_ref()
            .and_then(|p| self.library.notebooks.iter().find(|n| &n.path == p));
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
        let root = self.library_root();
        match a {
            Act::Open(p) => self.open_note(&p),
            Act::NewNote(dir) => self.new_note(&dir),
            Act::OpenNotebook(p) => {
                self.lib_ui.notebook = Some(p);
                self.lib_ui.section = None;
                self.lib_ui.search.clear();
            }
            Act::Home => {
                self.lib_ui.notebook = None;
                self.lib_ui.section = None;
            }
            Act::SelectSection(s) => self.lib_ui.section = s,
            Act::Dialog(d) => self.lib_ui.dialog = Some(d),
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
            Act::RenameNote(path, title) => {
                match library::rename_note(&path, &title) {
                    Ok(()) => self.refresh_library(),
                    Err(e) => self.toast(format!("Rename failed: {e}")),
                }
            }
            Act::RenameDir(path, name) => match library::rename_dir(&path, &name) {
                Ok(new) => {
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
                    self.toast("Moved to .trash");
                }
                Err(e) => self.toast(format!("Delete failed: {e}")),
            },
            Act::Duplicate(path) => match library::duplicate_note(&path) {
                Ok(_) => self.refresh_library(),
                Err(e) => self.toast(format!("Duplicate failed: {e}")),
            },
            Act::MoveNote(path, dir) => match library::move_note(&path, &dir) {
                Ok(_) => self.refresh_library(),
                Err(e) => self.toast(format!("Move failed: {e}")),
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
            Act::SetRoot(text) => {
                let p = PathBuf::from(text.trim());
                match std::fs::create_dir_all(&p) {
                    Ok(()) => {
                        self.settings.library_root = p.to_string_lossy().into_owned();
                        self.lib_ui.notebook = None;
                        self.lib_ui.section = None;
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

fn home_view(ui: &mut Ui, lib: &library::Library, root: &Path, recent_count: usize, acts: &mut Vec<Act>) {
    let recent = lib.recent(recent_count);
    if recent.is_empty() && lib.notebooks.is_empty() {
        ui.add_space(80.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(icon::BOOK_OPEN_TEXT).size(56.0).color(color::TEXT_FAINT));
            ui.add_space(space::M);
            ui.label(theme::title("Your library is empty"));
            ui.add_space(space::XS);
            ui.label(theme::muted("Create a note or a notebook, or import an existing .ink file."));
        });
        return;
    }

    heading(ui, "Recent");
    if recent.is_empty() {
        ui.label(theme::muted("Nothing here yet."));
    }
    note_grid(ui, &recent, root, true, acts);

    ui.add_space(space::XL);
    heading(ui, "Notebooks");
    if lib.notebooks.is_empty() {
        ui.label(theme::muted("No notebooks yet. Use \"New notebook\" to group your notes in sections."));
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(space::L, space::L);
        for nb in &lib.notebooks {
            notebook_cover(ui, nb, acts);
        }
    });

    if !lib.quick.is_empty() {
        ui.add_space(space::XL);
        heading(ui, "Quick notes");
        let quick: Vec<&NoteInfo> = lib.quick.iter().collect();
        note_grid(ui, &quick, root, false, acts);
    }
}

fn notebook_view(
    ui: &mut Ui,
    nb: &NotebookInfo,
    section: &Option<PathBuf>,
    root: &Path,
    acts: &mut Vec<Act>,
) {
    ui.horizontal(|ui| {
        theme::ghost_buttons(ui);
        if ui.button(format!("{}  Library", icon::CARET_LEFT)).clicked() {
            acts.push(Act::Home);
        }
        ui.add_space(space::S);
        let c = nb.color;
        ui.label(RichText::new(icon::NOTEBOOK).size(size::TITLE).color(Color32::from_rgb(c[0], c[1], c[2])));
        ui.label(theme::title(&nb.name));
        ui.add_space(space::S);
        if ui.button(icon::PENCIL_SIMPLE).on_hover_text("Rename notebook").clicked() {
            acts.push(Act::Dialog(Dialog::RenameDir { path: nb.path.clone(), text: nb.name.clone() }));
        }
        if ui.button(icon::TRASH).on_hover_text("Delete notebook").clicked() {
            acts.push(Act::Dialog(Dialog::Trash { path: nb.path.clone(), label: nb.name.clone() }));
        }
    });
    ui.add_space(space::L);

    // Section tabs.
    ui.horizontal_wrapped(|ui| {
        let general_count = nb.loose.len();
        let label = if general_count > 0 || nb.sections.is_empty() {
            Some(format!("General ({general_count})"))
        } else {
            None
        };
        if let Some(label) = label {
            if ui.selectable_label(section.is_none(), label).clicked() {
                acts.push(Act::SelectSection(None));
            }
        }
        for s in &nb.sections {
            let selected = section.as_ref() == Some(&s.path);
            let resp = ui.selectable_label(selected, format!("{} ({})", s.name, s.notes.len()));
            if resp.clicked() {
                acts.push(Act::SelectSection(Some(s.path.clone())));
            }
            resp.context_menu(|ui| {
                if entry(ui, icon::PENCIL_SIMPLE, "Rename section", "", true) {
                    acts.push(Act::Dialog(Dialog::RenameDir { path: s.path.clone(), text: s.name.clone() }));
                    ui.close_menu();
                }
                if entry(ui, icon::TRASH, "Delete section", "", true) {
                    acts.push(Act::Dialog(Dialog::Trash { path: s.path.clone(), label: s.name.clone() }));
                    ui.close_menu();
                }
            });
        }
        theme::ghost_buttons(ui);
        if ui.button(format!("{}  Section", icon::PLUS)).clicked() {
            acts.push(Act::Dialog(Dialog::NewSection { notebook: nb.path.clone(), text: String::new() }));
        }
    });
    ui.add_space(space::L);

    let notes: Vec<&NoteInfo> = match section {
        Some(p) => nb.sections.iter().find(|s| &s.path == p).map(|s| s.notes.iter().collect()).unwrap_or_default(),
        None => nb.loose.iter().collect(),
    };
    if notes.is_empty() {
        ui.label(theme::muted("No notes here yet. Use \"New note\"."));
    }
    note_grid(ui, &notes, root, false, acts);
}

// ------------------------------------------------------------------ cards

fn note_grid(ui: &mut Ui, notes: &[&NoteInfo], root: &Path, show_location: bool, acts: &mut Vec<Act>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(space::L, space::L);
        for n in notes {
            note_card(ui, n, root, show_location, acts);
        }
    });
}

fn note_card(ui: &mut Ui, note: &NoteInfo, root: &Path, show_location: bool, acts: &mut Vec<Act>) {
    let page_ratio = if note.meta.preview.page_w > 0.0 {
        note.meta.preview.page_h / note.meta.preview.page_w
    } else {
        1.414
    };
    let thumb_h = (CARD_W * page_ratio).clamp(CARD_W * 0.6, CARD_W * 1.5);

    ui.vertical(|ui| {
        ui.set_width(CARD_W);
        let (rect, resp) = ui.allocate_exact_size(vec2(CARD_W, thumb_h), Sense::click());
        let p = ui.painter();
        p.add(theme::card_shadow().as_shape(rect, Rounding::same(radius::SMALL)));
        paint_preview(&ui.painter_at(rect), rect, &note.meta.preview);
        if resp.hovered() {
            ui.painter().rect_stroke(rect.expand(1.0), radius::SMALL, Stroke::new(2.0, color::ACCENT));
        }
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);

        if resp.clicked() {
            acts.push(Act::Open(note.path.clone()));
        }
        resp.context_menu(|ui| {
            if entry(ui, icon::ARROW_SQUARE_OUT, "Open", "", true) {
                acts.push(Act::Open(note.path.clone()));
                ui.close_menu();
            }
            if entry(ui, icon::PENCIL_SIMPLE, "Rename", "", true) {
                acts.push(Act::Dialog(Dialog::RenameNote { path: note.path.clone(), text: note.meta.title.clone() }));
                ui.close_menu();
            }
            if entry(ui, icon::COPY, "Duplicate", "", true) {
                acts.push(Act::Duplicate(note.path.clone()));
                ui.close_menu();
            }
            if entry(ui, icon::FOLDER_SIMPLE, "Move to...", "", true) {
                acts.push(Act::Dialog(Dialog::Move { path: note.path.clone() }));
                ui.close_menu();
            }
            ui.separator();
            if entry(ui, icon::TRASH, "Delete", "", true) {
                acts.push(Act::Dialog(Dialog::Trash { path: note.path.clone(), label: note.meta.title.clone() }));
                ui.close_menu();
            }
        });

        ui.add_space(space::XS);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add(egui::Label::new(RichText::new(&note.meta.title).font(theme::font_strong(size::BODY)).color(color::TEXT_STRONG)).truncate());
        let pages = count(note.meta.pages as usize, "page");
        ui.label(RichText::new(format!("{} · {}", when(note.meta.modified), pages)).small().color(color::TEXT_MUTED));
        if show_location {
            ui.add(egui::Label::new(RichText::new(location(root, &note.path)).small().color(color::TEXT_FAINT)).truncate());
        }
    });
}

fn notebook_cover(ui: &mut Ui, nb: &NotebookInfo, acts: &mut Vec<Act>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(COVER_W, COVER_H), Sense::click());
    let c = nb.color;
    let base = Color32::from_rgb(c[0], c[1], c[2]);
    let p = ui.painter();
    let r = radius::MEDIUM;
    p.add(theme::card_shadow().as_shape(rect, Rounding::same(r)));
    p.rect_filled(rect, r, base);
    // Spine.
    let spine = Rect::from_min_max(rect.min, pos2(rect.min.x + 14.0, rect.max.y));
    p.rect_filled(spine, Rounding { nw: r, sw: r, ne: 0.0, se: 0.0 }, base.gamma_multiply(0.7));
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
    if resp.hovered() {
        p.rect_stroke(rect.expand(1.0), r, Stroke::new(2.0, color::ACCENT));
    }
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        acts.push(Act::OpenNotebook(nb.path.clone()));
    }
    resp.context_menu(|ui| {
        if entry(ui, icon::ARROW_SQUARE_OUT, "Open", "", true) {
            acts.push(Act::OpenNotebook(nb.path.clone()));
            ui.close_menu();
        }
        if entry(ui, icon::PENCIL_SIMPLE, "Rename", "", true) {
            acts.push(Act::Dialog(Dialog::RenameDir { path: nb.path.clone(), text: nb.name.clone() }));
            ui.close_menu();
        }
        ui.separator();
        if entry(ui, icon::TRASH, "Delete", "", true) {
            acts.push(Act::Dialog(Dialog::Trash { path: nb.path.clone(), label: nb.name.clone() }));
            ui.close_menu();
        }
    });
}

/// "1 note", "3 notes".
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// Page 1 as stored in the note header (no document loading).
fn paint_preview(painter: &egui::Painter, rect: Rect, pv: &Preview) {
    painter.rect_filled(rect, radius::SMALL, color::PAGE);
    if pv.page_w <= 0.0 {
        return;
    }
    let scale = rect.width() / pv.page_w;
    let at = |x: f32, y: f32| pos2(rect.min.x + x * scale, rect.min.y + y * scale);
    for s in &pv.strokes {
        let color = Color32::from_rgba_unmultiplied(s.color[0], s.color[1], s.color[2], s.color[3]);
        let w = (s.width * scale).max(0.8);
        let pts: Vec<egui::Pos2> = s.pts.iter().map(|p| at(p[0], p[1])).collect();
        if pts.len() == 1 {
            painter.circle_filled(pts[0], w * 0.5, color);
        } else {
            painter.add(Shape::line(pts, Stroke::new(w, color)));
        }
    }
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
    let rel = note.parent().and_then(|p| p.strip_prefix(root).ok());
    match rel {
        Some(r) if r.components().count() > 0 => r
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" › "),
        _ => "Quick notes".to_owned(),
    }
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
    fn locations() {
        let root = Path::new("/lib");
        assert_eq!(location(root, Path::new("/lib/a.ink")), "Quick notes");
        assert_eq!(location(root, Path::new("/lib/Work/Plans/a.ink")), "Work › Plans");
    }
}
