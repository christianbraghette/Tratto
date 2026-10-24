// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Export PDF dialog (File > Export as PDF, `Ctrl+E`, Note settings),
//! the same dialog to print (File > Print, `Ctrl+P`; see `print`), and
//! saving a page or the selection as a PNG picture (see `snapshot`).
//!
//! * **What**: the open note (all pages, the current one or a range), its
//!   whole section, or a selection of the notebook's notes; several notes go
//!   into one PDF with a bookmark each.
//! * **How**: content, ink colours, page size and margin, document title and
//!   author, file, open afterwards.
//! * **Preview**: the exported pages as they will look, page by page.
//!
//! The choices that describe *how* to export are remembered in the settings;
//! the ones about *this* export (what, pages, title, file) start fresh.
//!
//! Printing makes the same PDF (but for the page size, which is the
//! printer's) and hands it to the system's print dialog; it asks nothing
//! about the document or the file.
//!
//! The PDF is written on a background thread from copies of the notes, so a
//! long export never freezes the editor; one export or print runs at a time.
//! A PDF in the reader is printed as it is, without the dialog.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::{
    self, Align2, Color32, DragValue, Rect, RichText, Sense, Shape, Slider, Stroke, Ui, epaint::Mesh, pos2, vec2,
};
use egui_phosphor::regular as icon;
use tratto_core::library::NoteInfo;
use tratto_core::model::{Document, PAPERS, PageKind, StrokeKind};
use tratto_core::pdf_export::{self, InkColors, PdfNote, PdfOptions};
use tratto_core::{storage, stroke_path};

use super::menus::EXPORT_WINDOW;
use super::{InkApp, PathField};
use crate::file_picker::{self, Pick, Request};
use crate::pdf_view::PdfTextures;
use crate::print::{self, Printed};
use crate::render::{self, Layer, PictureTextures};
use crate::snapshot::Snapshot;
use crate::stroke_mesh;
use crate::theme::{self, color, radius, space};

/// What to export.
#[derive(Clone, Copy, PartialEq)]
enum Scope {
    /// The open note.
    Note,
    /// Every note of the open note's section (or "General", or the quick notes).
    Section,
    /// Notes ticked in the notebook.
    Selection,
}

/// Which pages of the open note.
#[derive(Clone, Copy, PartialEq)]
enum Pages {
    All,
    Current,
    Range,
}

/// The state of an open Export (or Print) dialog.
pub(super) struct ExportDialog {
    /// Print instead of writing a file.
    print: bool,
    scope: Scope,
    pages: Pages,
    /// 1-based, inclusive.
    from: u32,
    to: u32,
    title: String,
    path: String,
    /// Notes ticked for `Scope::Selection`.
    selected: HashSet<PathBuf>,
    /// Other notes, loaded once for the preview and the export (`None` =
    /// unreadable). The open note is always taken from memory.
    loaded: HashMap<PathBuf, Option<Document>>,
    /// Index of the previewed page among the exported pages.
    preview: usize,
    /// The file was just chosen in the system's Save dialog: export now.
    export_now: bool,
}

impl ExportDialog {
    /// The file to write, chosen in the system's file dialog; `export`:
    /// write it at once (Export was clicked, `file_picker::DIRECT`).
    pub(super) fn set_path(&mut self, path: String, export: bool) {
        self.path = path;
        self.export_now = export;
    }
}

/// A PDF being written (or printed) in the background.
pub(super) struct ExportJob {
    done: Receiver<io::Result<Sent>>,
    /// Unreadable notes left out.
    skipped: usize,
}

/// Where a PDF went.
enum Sent {
    File(PathBuf),
    Printer(Printed),
}

/// A group of notes as the notebook panel shows it.
struct Group {
    name: String,
    notes: Vec<NoteInfo>,
}

/// Preview area (the page is fitted inside, keeping its proportions).
const PREVIEW_W: f32 = 300.0;
const PREVIEW_H: f32 = 400.0;

/// 1 px = 1/96 in.
fn px_to_mm(px: f32) -> f32 {
    px * 25.4 / 96.0
}

fn mm_to_px(mm: f32) -> f32 {
    mm * 96.0 / 25.4
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(theme::muted(label));
    ui.horizontal(add);
    ui.end_row();
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(space::M);
    theme::caption(ui, title);
    ui.add_space(space::XS);
}

fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([space::L, space::S]).min_col_width(110.0).show(ui, add);
}

/// `title` as a file name: characters that file systems reject become "_".
fn file_name(title: &str) -> String {
    let name: String =
        title.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let name = name.trim().trim_end_matches('.').trim();
    if name.is_empty() { "Note".to_owned() } else { name.to_owned() }
}

/// Opens the PDF with the system's default viewer.
fn open_with_system(path: &Path) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path).spawn().map(|_| ())
}

impl InkApp {
    // ---------------------------------------------------------- the notes

    /// The groups of the open note's notebook ("General" and the sections),
    /// or the quick notes for a note outside any notebook; plus the name of
    /// the notebook (or "Quick notes").
    fn export_groups(&self) -> (String, Vec<Group>) {
        let current = self.note_path.as_deref();
        let notebook = current.and_then(|p| self.library.notebooks.iter().find(|nb| p.starts_with(&nb.path)));
        match notebook {
            Some(nb) => {
                let mut groups = Vec::new();
                if !nb.loose.is_empty() || nb.sections.is_empty() {
                    groups.push(Group { name: "General".to_owned(), notes: nb.loose.clone() });
                }
                for s in &nb.sections {
                    groups.push(Group { name: s.name.clone(), notes: s.notes.clone() });
                }
                (nb.name.clone(), groups)
            }
            None => (
                "Quick notes".to_owned(),
                vec![Group { name: "Quick notes".to_owned(), notes: self.library.quick.clone() }],
            ),
        }
    }

    /// The group that holds the open note.
    fn export_section(&self) -> Option<Group> {
        let current = self.note_path.as_deref()?;
        self.export_groups().1.into_iter().find(|g| g.notes.iter().any(|n| n.path == current))
    }

    /// The notes to export, in panel order: (path, title).
    fn export_list(&self, d: &ExportDialog) -> Vec<(PathBuf, String)> {
        let title_of = |n: &NoteInfo| {
            if Some(&n.path) == self.note_path.as_ref() { self.note_meta.title.clone() } else { n.meta.title.clone() }
        };
        match d.scope {
            Scope::Note => self.note_path.iter().map(|p| (p.clone(), self.note_meta.title.clone())).collect(),
            Scope::Section => self
                .export_section()
                .map(|g| g.notes.iter().map(|n| (n.path.clone(), title_of(n))).collect())
                .unwrap_or_default(),
            Scope::Selection => self
                .export_groups()
                .1
                .iter()
                .flat_map(|g| g.notes.iter())
                .filter(|n| d.selected.contains(&n.path))
                .map(|n| (n.path.clone(), title_of(n)))
                .collect(),
        }
    }

    /// Loads the listed notes that are not loaded yet (not the open one).
    fn load_for_export(&self, d: &mut ExportDialog, list: &[(PathBuf, String)]) {
        for (path, _) in list {
            if Some(path) != self.note_path.as_ref() && !d.loaded.contains_key(path) {
                let doc = storage::load_note(path).ok().map(|(_, mut doc)| {
                    crate::markdown::lay_out_document(&self.egui_ctx, &mut doc);
                    doc
                });
                d.loaded.insert(path.clone(), doc);
            }
        }
    }

    /// The document of a listed note (the open one from memory).
    fn export_doc<'a>(&'a self, loaded: &'a HashMap<PathBuf, Option<Document>>, path: &Path) -> Option<&'a Document> {
        if Some(path) == self.note_path.as_deref() {
            Some(&self.doc)
        } else {
            loaded.get(path).and_then(Option::as_ref)
        }
    }

    /// The characters of the text to export the PDF cannot show (see
    /// `pdf_export::unshown_chars`).
    // TODO: gone once the PDF embeds a font for every script.
    fn unshown_chars(&self, d: &ExportDialog, list: &[(PathBuf, String)]) -> Vec<char> {
        let notes: Vec<PdfNote> = (list.iter())
            .filter_map(|(p, t)| self.export_doc(&d.loaded, p).map(|doc| PdfNote { doc, title: t.clone() }))
            .collect();
        pdf_export::unshown_chars(&notes, &self.pdf_options(d, &d.title))
    }

    fn pdf_options(&self, d: &ExportDialog, title: &str) -> PdfOptions {
        let count = self.doc.page_count.max(1);
        let s = &self.settings;
        let pages = match (d.scope, d.pages) {
            (Scope::Note, Pages::Current) => Some(self.current_page..=self.current_page),
            (Scope::Note, Pages::Range) => {
                let (a, b) = (d.from.clamp(1, count) - 1, d.to.clamp(1, count) - 1);
                Some(a.min(b)..=a.max(b))
            }
            _ => None,
        };
        // The printer's paper is the page size.
        let page_size = if d.print { None } else { s.pdf_page_size };
        PdfOptions {
            include_rules: s.pdf_rules,
            pages,
            include_highlighter: s.pdf_highlighter,
            include_text: s.pdf_text,
            include_pdf_pages: s.pdf_pages,
            ink: s.pdf_ink,
            page_size,
            margin: if page_size.is_some() { s.pdf_margin } else { 0.0 },
            title: title.to_owned(),
            author: s.pdf_author.clone(),
        }
    }

    /// Default title of the PDF for a scope.
    fn scope_title(&self, scope: Scope) -> String {
        match scope {
            Scope::Note => self.note_meta.title.clone(),
            Scope::Section => self.export_section().map(|g| g.name).unwrap_or_else(|| self.note_meta.title.clone()),
            Scope::Selection => self.export_groups().0,
        }
    }

    /// Sets the title and the file name to the defaults of the scope.
    fn apply_scope_defaults(&self, d: &mut ExportDialog) {
        d.title = self.scope_title(d.scope);
        if let Some(note) = &self.note_path {
            // Next to the note, named after the title.
            d.path = note.with_file_name(format!("{}.pdf", file_name(&d.title))).to_string_lossy().into_owned();
        }
        d.preview = 0;
    }

    // ------------------------------------------------------------ dialog

    /// Opens the dialog for the open note: to export it, or to `print` it.
    /// A PDF in the reader is printed at once.
    pub(super) fn open_export_dialog(&mut self, ctx: &egui::Context, print: bool) {
        if print && self.reading() {
            return self.print_pdf(ctx);
        }
        if self.note_path.is_none() {
            return;
        }
        // The notebook as it is on disk now.
        self.refresh_library_now();
        let count = self.doc.page_count.max(1);
        let section: HashSet<PathBuf> =
            self.export_section().map(|g| g.notes.into_iter().map(|n| n.path).collect()).unwrap_or_default();
        let mut d = ExportDialog {
            print,
            scope: Scope::Note,
            pages: Pages::All,
            from: 1,
            to: count,
            title: String::new(),
            path: String::new(),
            selected: section,
            loaded: HashMap::new(),
            preview: 0,
            export_now: false,
        };
        self.apply_scope_defaults(&mut d);
        self.export_dialog = Some(d);
    }

    pub(super) fn export_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.export_dialog.take() else { return };
        let list = self.export_list(&dialog);
        self.load_for_export(&mut dialog, &list);

        let mut cancel = false;
        let mut export = std::mem::take(&mut dialog.export_now);
        let picking = self.file_picker.is_open();
        let max_h = (ctx.viewport_rect().height() * 0.75).max(300.0);
        let shown = theme::window(EXPORT_WINDOW, if dialog.print { "Print" } else { "Export PDF" })
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(440.0);
                        egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                            self.export_options(ui, &mut dialog);
                        });
                    });
                    // No separator: in a row it would take all the height it is
                    // offered and stretch the window; the preview's page and its
                    // shadow set the columns apart.
                    ui.add_space(space::XL);
                    ui.vertical(|ui| {
                        ui.set_width(PREVIEW_W);
                        self.export_preview(ui, &mut dialog, &list);
                    });
                });
                ui.add_space(space::M);
                let unshown = self.unshown_chars(&dialog, &list);
                if !unshown.is_empty() {
                    let some: String = unshown.iter().take(12).map(|c| format!("{c} ")).collect();
                    let more = if unshown.len() > 12 { "..." } else { "" };
                    let text = format!(
                        "{}  Some characters cannot be shown in the PDF and come out as \u{201c}?\u{201d}: {some}{more}",
                        icon::WARNING
                    );
                    ui.add(egui::Label::new(RichText::new(text).color(color::danger())).wrap());
                    ui.add_space(space::S);
                }
                ui.horizontal(|ui| {
                    // With the system's Save (or Print) dialog next, the button says so.
                    let label = if dialog.print {
                        format!("{}  Print...", icon::PRINTER)
                    } else {
                        format!("{}  Export{}", icon::FILE_PDF, if file_picker::DIRECT { "..." } else { "" })
                    };
                    let running = self.export_job.is_some();
                    let has_path = dialog.print || file_picker::DIRECT || !dialog.path.trim().is_empty();
                    let ready = has_path && !list.is_empty() && !running && !picking;
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter)) && !ui.ctx().egui_wants_keyboard_input();
                    let resp = ui.add_enabled(ready, theme::primary_button(label)).on_disabled_hover_text(if running {
                        "Another export or print is still running"
                    } else {
                        "Nothing to export"
                    });
                    if (resp.clicked() || enter) && ready {
                        if file_picker::DIRECT && !dialog.print {
                            self.pick_request = Some(Request {
                                pick: Pick::Save("PDF", &["pdf"]),
                                title: "Export PDF",
                                current: dialog.path.clone(),
                                field: PathField::ExportTo,
                            });
                        } else {
                            export = true;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if running {
                        ui.spinner();
                        ui.label(theme::muted(if dialog.print { "Printing..." } else { "Exporting..." }));
                    }
                });
            });
        cancel |= shown.is_some_and(|w| theme::window_close_button(&w.response));
        if export {
            self.run_export(ctx, &dialog, &list);
        } else if !cancel {
            self.export_dialog = Some(dialog);
        }
    }

    fn export_options(&mut self, ui: &mut Ui, d: &mut ExportDialog) {
        let count = self.doc.page_count.max(1);
        let current = self.current_page + 1;

        section(ui, if d.print { "Print" } else { "Export" });
        let before = d.scope;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut d.scope, Scope::Note, "This note");
            let section_name = self.export_section().map(|g| g.name).unwrap_or_default();
            ui.selectable_value(&mut d.scope, Scope::Section, "This section")
                .on_hover_text(format!("Every note of \"{section_name}\", in the panel's order"));
            ui.selectable_value(&mut d.scope, Scope::Selection, "Selected notes");
        });
        if d.scope != before {
            self.apply_scope_defaults(d);
        }
        match d.scope {
            Scope::Note => {
                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    ui.label(theme::muted("Pages"));
                    ui.selectable_value(&mut d.pages, Pages::All, format!("All ({count})"));
                    ui.selectable_value(&mut d.pages, Pages::Current, format!("Current ({current})"));
                    ui.selectable_value(&mut d.pages, Pages::Range, "Range");
                    if d.pages == Pages::Range {
                        ui.add(DragValue::new(&mut d.from).range(1..=count));
                        ui.label(theme::muted("to"));
                        ui.add(DragValue::new(&mut d.to).range(1..=count));
                    }
                });
            }
            Scope::Section => {
                let n = self.export_section().map_or(0, |g| g.notes.len());
                ui.label(theme::muted(format!("{n} notes, in the order of the notebook panel.")).small());
            }
            Scope::Selection => self.note_checklist(ui, d),
        }

        let on_pdf = self.doc.pdf.is_some() || d.loaded.values().flatten().any(|doc| doc.pdf.is_some());
        let s = &mut self.settings;
        section(ui, "Content");
        if on_pdf {
            ui.checkbox(&mut s.pdf_pages, "PDF pages under the ink")
                .on_hover_text("Notes written on a PDF: its pages, as they are (text stays selectable)");
        }
        ui.checkbox(&mut s.pdf_rules, "Ruled lines and grid");
        ui.checkbox(&mut s.pdf_highlighter, "Highlighter strokes");
        ui.checkbox(&mut s.pdf_text, "Text boxes");

        section(ui, "Appearance");
        grid(ui, "export_appearance", |ui| {
            row(ui, "Ink colours", |ui| {
                for (ink, label) in [
                    (InkColors::Original, "As drawn"),
                    (InkColors::Black, "Black"),
                    (InkColors::Grayscale, "Grayscale"),
                ] {
                    ui.selectable_value(&mut s.pdf_ink, ink, label);
                }
            });
            if d.print {
                return;
            }
            row(ui, "Page size", |ui| {
                let label = match s.pdf_page_size {
                    None => "Same as the note".to_owned(),
                    Some((w, h)) => PAPERS
                        .iter()
                        .find(|p| (p.2 - w).abs() < 0.5 && (p.3 - h).abs() < 0.5)
                        .map(|p| p.0.to_owned())
                        .unwrap_or_else(|| format!("{w:.0} × {h:.0} px")),
                };
                egui::ComboBox::from_id_salt("export_page_size").selected_text(label).width(200.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.pdf_page_size, None, "Same as the note");
                    for (name, dims, w, h) in PAPERS {
                        ui.selectable_value(&mut s.pdf_page_size, Some((w, h)), format!("{name}  ({dims})"));
                    }
                });
            });
            row(ui, "Margin", |ui| {
                let mut mm = px_to_mm(s.pdf_margin).round();
                let fixed = s.pdf_page_size.is_some();
                let slider = Slider::new(&mut mm, 0.0..=40.0).step_by(1.0).suffix(" mm");
                let resp =
                    ui.add_enabled(fixed, slider).on_disabled_hover_text("Only with a page size other than the note's");
                if resp.changed() {
                    s.pdf_margin = mm_to_px(mm);
                }
            });
        });
        let fitted = if d.print {
            "Each page is scaled to fit the printer's paper and centred, turned to match it."
        } else {
            "With another page size, each page is scaled to fit and centred."
        };
        ui.label(RichText::new(fitted).small().color(color::text_faint()));
        if d.print {
            return;
        }

        section(ui, "Document");
        grid(ui, "export_document", |ui| {
            row(ui, "Title", |ui| {
                ui.add(egui::TextEdit::singleline(&mut d.title).desired_width(260.0));
            });
            row(ui, "Author", |ui| {
                ui.add(egui::TextEdit::singleline(&mut s.pdf_author).hint_text("Optional").desired_width(260.0));
            });
        });
        if d.scope != Scope::Note {
            ui.label(RichText::new("Each note gets a bookmark in the PDF.").small().color(color::text_faint()));
        }

        section(ui, "File");
        // On Windows and macOS the file is chosen when exporting.
        let picking = self.file_picker.is_open();
        if !file_picker::DIRECT {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let browse = ui
                    .add_enabled(!picking, egui::Button::new(format!("{}  Browse...", icon::FOLDER_OPEN)))
                    .on_hover_text("Choose with the system's file explorer");
                if browse.clicked() {
                    self.pick_request = Some(Request {
                        pick: Pick::Save("PDF", &["pdf"]),
                        title: "Export PDF",
                        current: d.path.clone(),
                        field: PathField::Export,
                    });
                }
                ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(f32::INFINITY));
            });
        }
        ui.checkbox(&mut s.pdf_open_after, "Open the PDF after exporting");
    }

    /// The notebook's notes with a tick each, grouped like the panel.
    fn note_checklist(&self, ui: &mut Ui, d: &mut ExportDialog) {
        let (_, groups) = self.export_groups();
        let all: Vec<PathBuf> = groups.iter().flat_map(|g| g.notes.iter().map(|n| n.path.clone())).collect();
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            let n = all.iter().filter(|p| d.selected.contains(*p)).count();
            ui.label(theme::muted(format!("{n} of {} notes", all.len())));
            theme::ghost_buttons(ui);
            if ui.small_button("All").clicked() {
                d.selected.extend(all.iter().cloned());
            }
            if ui.small_button("None").clicked() {
                d.selected.clear();
            }
        });
        egui::Frame::NONE.fill(color::input()).corner_radius(radius::SMALL).inner_margin(space::S).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("export_checklist").max_height(200.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for g in &groups {
                    // The group's tick selects or clears all its notes.
                    let paths: Vec<&PathBuf> = g.notes.iter().map(|n| &n.path).collect();
                    let ticked = paths.iter().filter(|p| d.selected.contains(**p)).count();
                    let mut all_on = !paths.is_empty() && ticked == paths.len();
                    let label = RichText::new(format!("{}  {}", icon::FOLDER_SIMPLE, g.name)).strong();
                    let resp = ui
                        .add(egui::Checkbox::new(&mut all_on, label).indeterminate(ticked > 0 && ticked < paths.len()));
                    if resp.changed() {
                        for p in &paths {
                            if all_on {
                                d.selected.insert((*p).clone());
                            } else {
                                d.selected.remove(*p);
                            }
                        }
                    }
                    ui.indent(("export_group", &g.name), |ui| {
                        for n in &g.notes {
                            let mut on = d.selected.contains(&n.path);
                            if ui.checkbox(&mut on, &n.meta.title).changed() {
                                if on {
                                    d.selected.insert(n.path.clone());
                                } else {
                                    d.selected.remove(&n.path);
                                }
                            }
                        }
                    });
                }
            });
        });
    }

    // ----------------------------------------------------------- preview

    fn export_preview(&mut self, ui: &mut Ui, d: &mut ExportDialog, list: &[(PathBuf, String)]) {
        // The PDF pages' and pictures' textures, lent for the preview (the
        // rest only reads).
        let mut textures = (std::mem::take(&mut self.pdf_textures), std::mem::take(&mut self.picture_textures));
        self.preview_pages(ui, d, list, &mut textures);
        (self.pdf_textures, self.picture_textures) = textures;
    }

    fn preview_pages(
        &self,
        ui: &mut Ui,
        d: &mut ExportDialog,
        list: &[(PathBuf, String)],
        textures: &mut (PdfTextures, PictureTextures),
    ) {
        theme::caption(ui, "Preview");
        ui.add_space(space::XS);
        let docs: Vec<(&Document, &str)> =
            list.iter().filter_map(|(p, t)| self.export_doc(&d.loaded, p).map(|doc| (doc, t.as_str()))).collect();
        let notes: Vec<PdfNote> = docs.iter().map(|(doc, t)| PdfNote { doc, title: (*t).to_owned() }).collect();
        let opts = self.pdf_options(d, &d.title);
        let pages = pdf_export::export_pages(&notes, &opts);

        let (area, _) = ui.allocate_exact_size(vec2(PREVIEW_W, PREVIEW_H), Sense::hover());
        if pages.is_empty() {
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                "Nothing to export",
                theme::font(theme::size::BODY),
                color::text_faint(),
            );
            return;
        }
        d.preview = d.preview.min(pages.len() - 1);
        let (note, page) = pages[d.preview];
        paint_page(ui, area, notes[note].doc, page, &opts, textures);

        ui.add_space(space::S);
        ui.horizontal(|ui| {
            theme::ghost_buttons(ui);
            if ui.add_enabled(d.preview > 0, egui::Button::new(icon::CARET_LEFT)).clicked() {
                d.preview -= 1;
            }
            ui.label(format!("Page {} of {}", d.preview + 1, pages.len()));
            if ui.add_enabled(d.preview + 1 < pages.len(), egui::Button::new(icon::CARET_RIGHT)).clicked() {
                d.preview += 1;
            }
        });
        if notes.len() > 1 {
            let label = format!("{} · page {}", notes[note].title, page + 1);
            ui.add(egui::Label::new(theme::muted(label).small()).truncate());
        }
        let missing = list.len() - docs.len();
        if missing > 0 {
            ui.label(
                RichText::new(format!("{missing} note(s) cannot be read and will be skipped."))
                    .small()
                    .color(color::danger()),
            );
        }
    }

    /// Starts writing the PDF on a background thread, from copies of the
    /// notes: the open note can be edited (and saved) meanwhile. Printing,
    /// the thread then shows the system's print dialog and prints it.
    fn run_export(&mut self, ctx: &egui::Context, d: &ExportDialog, list: &[(PathBuf, String)]) {
        let docs: Vec<(Document, String)> = list
            .iter()
            .filter_map(|(p, t)| self.export_doc(&d.loaded, p).map(|doc| (doc.clone(), t.clone())))
            .collect();
        let title = if d.title.trim().is_empty() { self.scope_title(d.scope) } else { d.title.trim().to_owned() };
        let opts = self.pdf_options(d, &title);
        let mut path = PathBuf::from(d.path.trim().trim_matches('"'));
        if path.extension().is_none() {
            path.set_extension("pdf");
        }
        let skipped = list.len() - docs.len();

        let (tx, done) = mpsc::channel();
        let (ctx, to_printer) = (ctx.clone(), d.print);
        let spawned = thread::Builder::new().name("pdf-export".into()).spawn(move || {
            let notes: Vec<PdfNote> = docs.iter().map(|(doc, t)| PdfNote { doc, title: t.clone() }).collect();
            let sent = if to_printer {
                pdf_export::export_bytes(&notes, &opts)
                    .and_then(|pdf| print::print(pdf, &title).map_err(io::Error::other))
                    .map(Sent::Printer)
            } else {
                pdf_export::export_notes(&notes, &path, &opts).map(|()| Sent::File(path))
            };
            let _ = tx.send(sent);
            // Wake the UI to show the result.
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => {
                self.export_job = Some(ExportJob { done, skipped });
                self.toast(if to_printer { "Preparing to print..." } else { "Exporting PDF..." });
            }
            Err(e) => self.toast(format!("PDF export failed: {e}")),
        }
    }

    /// Prints the PDF open in the reader, as it is.
    fn print_pdf(&mut self, ctx: &egui::Context) {
        if self.export_job.is_some() {
            return self.toast("Another export or print is still running");
        }
        let Some(file) = self.doc.pdf.as_ref().and_then(|p| p.file.clone()) else { return };
        let title = self.note_meta.title.clone();
        let (tx, done) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = thread::Builder::new().name("pdf-print".into()).spawn(move || {
            let _ = tx.send(print::print(file.bytes.clone(), &title).map(Sent::Printer).map_err(io::Error::other));
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => self.export_job = Some(ExportJob { done, skipped: 0 }),
            Err(e) => self.toast(format!("Cannot print: {e}")),
        }
    }

    /// Reports a finished background export (once per frame).
    pub(super) fn poll_export(&mut self) {
        let Some(job) = &self.export_job else { return };
        let result = match job.done.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(io::Error::other("the export stopped unexpectedly")),
        };
        let Some(ExportJob { skipped, .. }) = self.export_job.take() else { return };
        let skipped = if skipped > 0 { format!(" ({skipped} unreadable note(s) skipped)") } else { String::new() };
        match result {
            Ok(Sent::File(path)) => {
                self.toast(format!("Exported PDF to {}{skipped}", path.display()));
                if self.settings.pdf_open_after
                    && let Err(e) = open_with_system(&path)
                {
                    self.toast(format!("Exported, but cannot open it: {e}"));
                }
            }
            Ok(Sent::Printer(Printed::Sent(Some(n)))) => {
                self.toast(format!("Sent {n} {} to the printer{skipped}", if n == 1 { "page" } else { "pages" }));
            }
            Ok(Sent::Printer(Printed::Sent(None))) => self.toast(format!("Sent to the printer{skipped}")),
            Ok(Sent::Printer(Printed::Cancelled)) => {}
            Ok(Sent::Printer(Printed::Opened)) => self.toast("Opened in Preview: print it from there (Cmd+P)"),
            Err(e) => self.toast(format!("Cannot print or export: {e}")),
        }
    }
}

impl InkApp {
    // ------------------------------------------------------------- PNG

    /// Asks where to save a picture of page `page` (as it looks: paper,
    /// rules, the PDF under it), then saves it.
    pub(super) fn save_page_png(&mut self, page: u32) {
        let snap = Snapshot::of_note_page(&self.doc, page);
        self.ask_png_path(snap, &format!("page {}", page + 1));
    }

    /// Asks where to save a picture of what is selected (transparent around
    /// it), then saves it.
    pub(super) fn save_selection_png(&mut self) {
        let Some(content) = self.selected_content() else { return };
        self.ask_png_path(Snapshot::of_content(&content), "selection");
    }

    /// The system's Save dialog for a picture, next to the note and named
    /// after it and `what`; `snap` is saved where it says.
    fn ask_png_path(&mut self, snap: Snapshot, what: &str) {
        if self.file_picker.is_open() {
            return;
        }
        let stem = self.note_path.as_deref().and_then(Path::file_stem).map(|s| s.to_string_lossy().into_owned());
        let name = format!("{} - {what}.png", file_name(stem.as_deref().unwrap_or("Note")));
        let current = self.note_path.as_ref().map(|p| p.with_file_name(&name)).unwrap_or_else(|| PathBuf::from(&name));
        self.pick_request = Some(Request {
            pick: Pick::Save("PNG image", &["png"]),
            title: "Save as PNG",
            current: current.to_string_lossy().into_owned(),
            field: PathField::Png(Box::new(snap)),
        });
    }

    /// Tells how the pictures being saved went.
    pub(super) fn poll_png(&mut self) {
        let mut done = Vec::new();
        self.png_jobs.retain(|job| match job.try_recv() {
            Ok(r) => {
                done.push(r);
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push(Err("the picture could not be made".to_owned()));
                false
            }
        });
        for r in done {
            match r {
                Ok(path) => self.toast(format!("Saved {}", path.display())),
                Err(e) => self.toast(format!("Cannot save the picture: {e}")),
            }
        }
    }
}

/// One exported page as the PDF will show it, fitted into `area`: the output
/// page (white), the note's page scaled and centred on it; then, as in the
/// PDF, paper (or the PDF page it is written on), highlighters, rules, text
/// boxes and pen strokes with the chosen ink colours.
fn paint_page(
    ui: &Ui,
    area: Rect,
    doc: &Document,
    page: u32,
    opts: &PdfOptions,
    (textures, pictures): &mut (PdfTextures, PictureTextures),
) {
    let ((out_w, out_h), s, (ox, oy)) = opts.placement(doc);
    let k = (area.width() / out_w).min(area.height() / out_h);
    let sheet = Rect::from_center_size(area.center(), vec2(out_w * k, out_h * k));
    let p = ui.painter_at(sheet.expand(8.0));
    p.add(theme::card_shadow().as_shape(sheet, 2.0));
    p.rect_filled(sheet, 0.0, Color32::WHITE);

    let f = s * k;
    let origin = sheet.min + vec2(ox * k, oy * k);
    let at = |x: f32, y: f32| pos2(origin.x + x * f, origin.y + y * f);
    let paper = Rect::from_min_max(at(0.0, 0.0), at(doc.page_width, doc.page_height));
    let p = ui.painter_at(paper);
    let on_pdf = opts.include_pdf_pages && doc.pdf_page(page).is_some();
    if on_pdf {
        textures.paint(&p, doc, page, paper, true);
    }
    let rgb = |(r, g, b): (f32, f32, f32)| Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8);
    let paint_strokes = |kind: StrokeKind| {
        for st in doc.strokes.iter().filter(|st| st.page == page && st.kind == kind) {
            let ink = rgb(opts.stroke_rgb(st));
            let samples = stroke_path::centerline(st.kind, st.width, &st.points, 2.0);
            let width = |r: f32| (r * 2.0 * f).max(0.6);
            match samples.as_slice() {
                [] => {}
                [one] => {
                    p.circle_filled(at(one.x, one.y), width(one.r) * 0.5, ink);
                }
                many => {
                    let r = many.iter().map(|s| s.r).sum::<f32>() / many.len() as f32;
                    let pts = many.iter().map(|s| at(s.x, s.y)).collect();
                    p.add(Shape::line(pts, Stroke::new(width(r), ink)));
                }
            }
        }
    };
    // As in the PDF: rules, pictures, highlighters, text, pen.
    if opts.include_rules && doc.ruled(page) {
        let spacing = doc.page.spacing.max(8.0);
        if spacing * f >= 2.5 {
            let rule = Stroke::new(0.6, color::page_rule());
            let mut y = spacing;
            while y < doc.page_height {
                p.line_segment([at(0.0, y), at(doc.page_width, y)], rule);
                y += spacing;
            }
            if doc.page.kind == PageKind::Grid {
                let mut x = spacing;
                while x < doc.page_width {
                    p.line_segment([at(x, 0.0), at(x, doc.page_height)], rule);
                    x += spacing;
                }
            }
        }
    }
    pictures.paint_page(&p, &doc.images, page, origin, f, &[]);
    // Highlighters darken what is under them, as in the PDF.
    if opts.include_highlighter {
        let mut mesh = Mesh::default();
        let aa = 1.0 / ui.ctx().pixels_per_point();
        for st in doc.strokes.iter().filter(|st| st.page == page && st.kind == StrokeKind::Highlighter) {
            let (r, g, b) = opts.stroke_rgb(st);
            let rgba = [r, g, b].map(|v| (v * 255.0).round() as u8);
            stroke_mesh::tessellate(&mut mesh, st.kind, [rgba[0], rgba[1], rgba[2], 255], st.width, &st.points, f, aa);
        }
        mesh.translate(origin.to_vec2());
        render::paint_mesh(&p, Layer::Highlight, mesh);
    }
    if opts.include_text {
        for t in doc.texts.iter().filter(|t| t.page == page) {
            render::paint_text_box(&p, at(t.x, t.y), f, t, rgb(opts.ink_rgb(t.color, false)));
        }
    }
    paint_strokes(StrokeKind::Pen);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_from_titles() {
        assert_eq!(file_name("Test 1"), "Test 1");
        assert_eq!(file_name("a/b: c?"), "a_b_ c_");
        assert_eq!(file_name("  ...  "), "Note");
        assert_eq!(file_name("Notes."), "Notes");
    }

    #[test]
    fn millimetres() {
        assert!((px_to_mm(96.0) - 25.4).abs() < 1e-4);
        assert!((mm_to_px(px_to_mm(37.0)) - 37.0).abs() < 1e-3);
    }
}
