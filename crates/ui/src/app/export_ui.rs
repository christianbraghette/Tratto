// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Export PDF dialog (File > Export as PDF, `Ctrl+E`, Note settings).
//!
//! The choices that describe *how* to export (content, ink, page size,
//! margin, author, open afterwards) are remembered in the settings; the ones
//! about *this* export (pages, title, file) start fresh each time.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align2, DragValue, RichText, Slider, Ui};
use egui_phosphor::regular as icon;
use inknote_core::model::PAPERS;
use inknote_core::pdf_export::{self, InkColors, PdfOptions};

use super::InkApp;
use crate::theme::{self, color, space};

/// Which pages to export.
#[derive(Clone, Copy, PartialEq)]
enum Pages {
    All,
    Current,
    Range,
}

/// The state of an open Export dialog.
pub(super) struct ExportDialog {
    pages: Pages,
    /// 1-based, inclusive.
    from: u32,
    to: u32,
    title: String,
    path: String,
}

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
    let name: String = title
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
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
    /// Opens the dialog for the open note.
    pub(super) fn open_export_dialog(&mut self) {
        let Some(note) = &self.note_path else { return };
        let count = self.doc.page_count.max(1);
        self.export_dialog = Some(ExportDialog {
            pages: Pages::All,
            from: 1,
            to: count,
            title: self.note_meta.title.clone(),
            // Next to the note, named after its title.
            path: note
                .with_file_name(format!("{}.pdf", file_name(&self.note_meta.title)))
                .to_string_lossy()
                .into_owned(),
        });
    }

    pub(super) fn export_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.export_dialog.take() else { return };
        let mut open = true;
        let mut cancel = false;
        let mut export = false;
        let max_h = ctx.screen_rect().height() * 0.75;
        egui::Window::new("Export PDF")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                    self.export_options(ui, &mut dialog);
                });
                ui.add_space(space::M);
                ui.horizontal(|ui| {
                    let label = format!("{}  Export", icon::FILE_PDF);
                    let ready = !dialog.path.trim().is_empty();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter)) && !ui.ctx().wants_keyboard_input();
                    if (ui.add_enabled(ready, theme::primary_button(label)).clicked() || enter) && ready {
                        export = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if export {
            self.run_export(&dialog);
        } else if open && !cancel {
            self.export_dialog = Some(dialog);
        }
    }

    fn export_options(&mut self, ui: &mut Ui, d: &mut ExportDialog) {
        let count = self.doc.page_count.max(1);
        let current = self.current_page + 1;
        let s = &mut self.settings;

        section(ui, "Pages");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut d.pages, Pages::All, format!("All ({count})"));
            ui.selectable_value(&mut d.pages, Pages::Current, format!("Current ({current})"));
            ui.selectable_value(&mut d.pages, Pages::Range, "Range");
            if d.pages == Pages::Range {
                ui.add(DragValue::new(&mut d.from).range(1..=count));
                ui.label(theme::muted("to"));
                ui.add(DragValue::new(&mut d.to).range(1..=count));
            }
        });

        section(ui, "Content");
        ui.checkbox(&mut s.pdf_rules, "Ruled lines and grid");
        ui.checkbox(&mut s.pdf_highlighter, "Highlighter strokes");
        ui.checkbox(&mut s.pdf_text, "Text boxes");
        ui.checkbox(&mut s.pdf_paper_background, "Paper colour as background (otherwise white)");

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
                let resp = ui
                    .add_enabled(fixed, slider)
                    .on_disabled_hover_text("Only with a page size other than the note's");
                if resp.changed() {
                    s.pdf_margin = mm_to_px(mm);
                }
            });
        });
        ui.label(RichText::new("With another page size, each page is scaled to fit and centred.").small().color(color::TEXT_FAINT));

        section(ui, "Document");
        grid(ui, "export_document", |ui| {
            row(ui, "Title", |ui| {
                ui.add(egui::TextEdit::singleline(&mut d.title).desired_width(260.0));
            });
            row(ui, "Author", |ui| {
                ui.add(egui::TextEdit::singleline(&mut s.pdf_author).hint_text("Optional").desired_width(260.0));
            });
        });

        section(ui, "File");
        ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(f32::INFINITY));
        ui.checkbox(&mut s.pdf_open_after, "Open the PDF after exporting");
    }

    fn run_export(&mut self, d: &ExportDialog) {
        let count = self.doc.page_count.max(1);
        let s = &self.settings;
        let pages = match d.pages {
            Pages::All => None,
            Pages::Current => Some(self.current_page..=self.current_page),
            Pages::Range => {
                let (a, b) = (d.from.clamp(1, count) - 1, d.to.clamp(1, count) - 1);
                Some(a.min(b)..=a.max(b))
            }
        };
        let opts = PdfOptions {
            include_rules: s.pdf_rules,
            pages,
            include_highlighter: s.pdf_highlighter,
            include_text: s.pdf_text,
            paper_background: s.pdf_paper_background,
            ink: s.pdf_ink,
            page_size: s.pdf_page_size,
            margin: if s.pdf_page_size.is_some() { s.pdf_margin } else { 0.0 },
            title: if d.title.trim().is_empty() { self.note_meta.title.clone() } else { d.title.trim().to_owned() },
            author: s.pdf_author.clone(),
        };
        let mut path = PathBuf::from(d.path.trim().trim_matches('"'));
        if path.extension().is_none() {
            path.set_extension("pdf");
        }
        match pdf_export::export_pdf(&self.doc, &path, &opts) {
            Ok(()) => {
                self.toast(format!("Exported PDF to {}", path.display()));
                if self.settings.pdf_open_after {
                    if let Err(e) = open_with_system(&path) {
                        self.toast(format!("Exported, but cannot open it: {e}"));
                    }
                }
            }
            Err(e) => self.toast(format!("PDF export failed: {e}")),
        }
    }
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
