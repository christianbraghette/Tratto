// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The library screen: recent notes, notebook covers, sections and dialogs.
//!
//! This is a child module of `app`, so it can use the editor state directly
//! (opening a note, toasts, settings) without extra plumbing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CursorIcon, FontId, Frame, Margin, Rect, RichText, Rounding,
    Sense, Shape, Stroke, TextEdit, Ui,
};

use inknote_core::library::{self, NoteInfo, NotebookInfo};
use inknote_core::storage::{now_secs, Preview};

use super::InkApp;
use crate::render::PAGE_COLOR;

const CARD_W: f32 = 168.0;
const COVER_W: f32 = 168.0;
const COVER_H: f32 = 118.0;
const RECENT_COUNT: usize = 12;
const BG: Color32 = Color32::from_gray(28);
const PANEL: Color32 = Color32::from_gray(38);

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
/// ever borrows the data immutably.
enum Act {
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
            .frame(Frame::none().fill(PANEL).inner_margin(Margin::symmetric(18.0, 12.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("InkNote").size(22.0).strong());
                    ui.add_space(14.0);
                    ui.add(
                        TextEdit::singleline(&mut self.lib_ui.search)
                            .hint_text("Search notes")
                            .desired_width(230.0),
                    );
                    if searching && ui.small_button("Clear").clicked() {
                        self.lib_ui.search.clear();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Library folder").clicked() {
                            acts.push(Act::Dialog(Dialog::Folder(self.settings.library_root.clone())));
                        }
                        if ui.button("Refresh").on_hover_text("Re-read the folder (e.g. after a sync)").clicked() {
                            acts.push(Act::Refresh);
                        }
                        if ui.button("Import").clicked() {
                            acts.push(Act::Dialog(Dialog::Import(String::new())));
                        }
                        if ui.button("+ New notebook").clicked() {
                            acts.push(Act::Dialog(Dialog::NewNotebook(String::new())));
                        }
                        let new_note = egui::Button::new(RichText::new("+ New note").strong())
                            .fill(Color32::from_rgb(40, 100, 220));
                        if ui.add(new_note).clicked() {
                            acts.push(Act::NewNote(target_dir.clone()));
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin::symmetric(24.0, 16.0)))
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
                        home_view(ui, &self.library, &root, &mut acts);
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
        egui::Area::new(egui::Id::new("lib_toast"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -24.0))
            .show(ctx, |ui| {
                Frame::none()
                    .fill(Color32::from_black_alpha(225))
                    .rounding(Rounding::same(10.0))
                    .inner_margin(Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| ui.colored_label(Color32::WHITE, &self.status));
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    // ------------------------------------------------------------ dialogs

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
                match dialog {
                    Dialog::NewNotebook(text) => {
                        let submit = name_row(ui, text, "Notebook name");
                        if ui.button("Create").clicked() || submit || enter {
                            if !text.trim().is_empty() {
                                result = Some(Act::CreateNotebook(text.clone()));
                            }
                        }
                    }
                    Dialog::NewSection { notebook, text } => {
                        let submit = name_row(ui, text, "Section name");
                        if ui.button("Create").clicked() || submit || enter {
                            if !text.trim().is_empty() {
                                result = Some(Act::CreateSection(notebook.clone(), text.clone()));
                            }
                        }
                    }
                    Dialog::RenameNote { path, text } => {
                        let submit = name_row(ui, text, "Title");
                        if ui.button("Rename").clicked() || submit || enter {
                            if !text.trim().is_empty() {
                                result = Some(Act::RenameNote(path.clone(), text.clone()));
                            }
                        }
                    }
                    Dialog::RenameDir { path, text } => {
                        let submit = name_row(ui, text, "Name");
                        if ui.button("Rename").clicked() || submit || enter {
                            if !text.trim().is_empty() {
                                result = Some(Act::RenameDir(path.clone(), text.clone()));
                            }
                        }
                    }
                    Dialog::Trash { path, label } => {
                        ui.label(format!("Move \"{label}\" to the library's .trash folder?"));
                        ui.small("Nothing is erased for good; you can copy it back from .trash.");
                        ui.horizontal(|ui| {
                            let del = egui::Button::new("Delete").fill(Color32::from_rgb(170, 50, 50));
                            if ui.add(del).clicked() || enter {
                                result = Some(Act::Trash(path.clone()));
                            }
                        });
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
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for (label, dir) in dest {
                                if dir == from {
                                    continue;
                                }
                                if ui.button(label).clicked() {
                                    result = Some(Act::MoveNote(path.clone(), dir));
                                }
                            }
                        });
                    }
                    Dialog::Import(text) => {
                        ui.label("Full path of an .ink file (any version):");
                        let submit = name_row(ui, text, "/path/to/notes.ink");
                        ui.small("It is copied into the current notebook / section.");
                        if ui.button("Import").clicked() || submit || enter {
                            if !text.trim().is_empty() {
                                result = Some(Act::Import(target_dir.to_path_buf(), text.clone()));
                            }
                        }
                    }
                    Dialog::Folder(text) => {
                        ui.label("All notebooks and notes live in this folder:");
                        let submit = name_row(ui, text, "Folder path");
                        ui.small("Existing notes are not moved; the new folder is read instead.");
                        if ui.button("Use this folder").clicked() || submit || enter {
                            if !text.trim().is_empty() {
                                result = Some(Act::SetRoot(text.clone()));
                            }
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
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
            }
            Err(e) => self.toast(format!("Cannot create note: {e}")),
        }
    }

    fn apply_act(&mut self, a: Act) {
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
            Act::RenameNote(path, title) => {
                match library::rename_note(&path, &title) {
                    Ok(()) => self.refresh_library(),
                    Err(e) => self.toast(format!("Rename failed: {e}")),
                }
            }
            Act::RenameDir(path, name) => match library::rename_dir(&path, &name) {
                Ok(new) => {
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
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(18.0).strong());
    ui.add_space(8.0);
}

fn home_view(ui: &mut Ui, lib: &library::Library, root: &Path, acts: &mut Vec<Act>) {
    let recent = lib.recent(RECENT_COUNT);
    if recent.is_empty() && lib.notebooks.is_empty() {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Your library is empty").size(22.0));
            ui.add_space(6.0);
            ui.weak("Create a note or a notebook, or import an existing .ink file.");
        });
        return;
    }

    heading(ui, "Recent");
    if recent.is_empty() {
        ui.weak("Nothing here yet.");
    }
    note_grid(ui, &recent, root, true, acts);

    ui.add_space(14.0);
    heading(ui, "Notebooks");
    if lib.notebooks.is_empty() {
        ui.weak("No notebooks yet. Use \"+ New notebook\" to group your notes in sections.");
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(16.0, 16.0);
        for nb in &lib.notebooks {
            notebook_cover(ui, nb, acts);
        }
    });

    if !lib.quick.is_empty() {
        ui.add_space(14.0);
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
        if ui.button("<  Library").clicked() {
            acts.push(Act::Home);
        }
        let c = nb.color;
        ui.label(RichText::new(&nb.name).size(22.0).strong().color(Color32::from_rgb(c[0], c[1], c[2])));
        if ui.small_button("Rename").clicked() {
            acts.push(Act::Dialog(Dialog::RenameDir { path: nb.path.clone(), text: nb.name.clone() }));
        }
        if ui.small_button("Delete").clicked() {
            acts.push(Act::Dialog(Dialog::Trash { path: nb.path.clone(), label: nb.name.clone() }));
        }
    });
    ui.add_space(10.0);

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
                if ui.button("Rename section").clicked() {
                    acts.push(Act::Dialog(Dialog::RenameDir { path: s.path.clone(), text: s.name.clone() }));
                    ui.close_menu();
                }
                if ui.button("Delete section").clicked() {
                    acts.push(Act::Dialog(Dialog::Trash { path: s.path.clone(), label: s.name.clone() }));
                    ui.close_menu();
                }
            });
        }
        if ui.button("+ Section").clicked() {
            acts.push(Act::Dialog(Dialog::NewSection { notebook: nb.path.clone(), text: String::new() }));
        }
    });
    ui.add_space(12.0);

    let notes: Vec<&NoteInfo> = match section {
        Some(p) => nb.sections.iter().find(|s| &s.path == p).map(|s| s.notes.iter().collect()).unwrap_or_default(),
        None => nb.loose.iter().collect(),
    };
    if notes.is_empty() {
        ui.weak("No notes here yet. Use \"+ New note\".");
    }
    note_grid(ui, &notes, root, false, acts);
}

// ------------------------------------------------------------------ cards

fn note_grid(ui: &mut Ui, notes: &[&NoteInfo], root: &Path, show_location: bool, acts: &mut Vec<Act>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(16.0, 18.0);
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
        paint_preview(&ui.painter_at(rect), rect, &note.meta.preview);
        let border = if resp.hovered() {
            Stroke::new(2.0, Color32::from_rgb(70, 140, 255))
        } else {
            Stroke::new(1.0, Color32::from_gray(70))
        };
        ui.painter().rect_stroke(rect, 2.0, border);
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);

        if resp.clicked() {
            acts.push(Act::Open(note.path.clone()));
        }
        resp.context_menu(|ui| {
            if ui.button("Open").clicked() {
                acts.push(Act::Open(note.path.clone()));
                ui.close_menu();
            }
            if ui.button("Rename").clicked() {
                acts.push(Act::Dialog(Dialog::RenameNote { path: note.path.clone(), text: note.meta.title.clone() }));
                ui.close_menu();
            }
            if ui.button("Duplicate").clicked() {
                acts.push(Act::Duplicate(note.path.clone()));
                ui.close_menu();
            }
            if ui.button("Move to...").clicked() {
                acts.push(Act::Dialog(Dialog::Move { path: note.path.clone() }));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Delete").clicked() {
                acts.push(Act::Dialog(Dialog::Trash { path: note.path.clone(), label: note.meta.title.clone() }));
                ui.close_menu();
            }
        });

        ui.add(egui::Label::new(RichText::new(&note.meta.title).strong()).truncate());
        let pages = if note.meta.pages == 1 { "1 page".to_owned() } else { format!("{} pages", note.meta.pages) };
        ui.weak(format!("{} · {}", when(note.meta.modified), pages));
        if show_location {
            ui.weak(RichText::new(location(root, &note.path)).size(11.0));
        }
    });
}

fn notebook_cover(ui: &mut Ui, nb: &NotebookInfo, acts: &mut Vec<Act>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(COVER_W, COVER_H), Sense::click());
    let c = nb.color;
    let base = Color32::from_rgb(c[0], c[1], c[2]);
    let p = ui.painter();
    p.rect_filled(rect.translate(vec2(2.0, 3.0)), 8.0, Color32::from_black_alpha(90));
    p.rect_filled(rect, 8.0, base);
    // Spine.
    let spine = Rect::from_min_max(rect.min, pos2(rect.min.x + 14.0, rect.max.y));
    p.rect_filled(spine, Rounding { nw: 8.0, sw: 8.0, ne: 0.0, se: 0.0 }, base.gamma_multiply(0.7));
    p.text(
        pos2(rect.min.x + 26.0, rect.min.y + 14.0),
        Align2::LEFT_TOP,
        truncate(&nb.name, 16),
        FontId::proportional(17.0),
        Color32::WHITE,
    );
    let total: usize = nb.loose.len() + nb.sections.iter().map(|s| s.notes.len()).sum::<usize>();
    p.text(
        pos2(rect.min.x + 26.0, rect.max.y - 12.0),
        Align2::LEFT_BOTTOM,
        format!("{} sections · {} notes", nb.sections.len(), total),
        FontId::proportional(12.0),
        Color32::from_white_alpha(210),
    );
    if resp.hovered() {
        p.rect_stroke(rect, 8.0, Stroke::new(2.0, Color32::WHITE));
    }
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if resp.clicked() {
        acts.push(Act::OpenNotebook(nb.path.clone()));
    }
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            acts.push(Act::OpenNotebook(nb.path.clone()));
            ui.close_menu();
        }
        if ui.button("Rename").clicked() {
            acts.push(Act::Dialog(Dialog::RenameDir { path: nb.path.clone(), text: nb.name.clone() }));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Delete").clicked() {
            acts.push(Act::Dialog(Dialog::Trash { path: nb.path.clone(), label: nb.name.clone() }));
            ui.close_menu();
        }
    });
}

/// Page 1 as stored in the note header (no document loading).
fn paint_preview(painter: &egui::Painter, rect: Rect, pv: &Preview) {
    painter.rect_filled(rect, 2.0, PAGE_COLOR);
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
fn location(root: &Path, note: &Path) -> String {
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
fn when(ts: u64) -> String {
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
