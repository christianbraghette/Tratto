// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The two settings views:
//!
//! * **Note settings** (the gear button, left panel): what is stored in the
//!   open note (paper, pages, title) and actions on it (save, export).
//! * **Settings** (File > Settings, `Ctrl+,`): global preferences, in a
//!   floating window, on the library screen too.

use eframe::egui::{self, Align2, DragValue, RichText, Slider, Ui};
use egui_phosphor::regular as icon;
use inknote_core::history::Action;
use inknote_core::model::{paper_label, PageKind, PAPERS};

use super::library_ui::{location, when, Dialog};
use super::InkApp;
use crate::settings::ToolbarPosition;
use crate::theme::{self, color, space};

/// Note settings in the left panel: default width and resize range.
const NOTE_WIDTH: f32 = 340.0;
const NOTE_WIDTHS: std::ops::RangeInclusive<f32> = 280.0..=520.0;

// ------------------------------------------------------------- helpers

/// Label | control rows, aligned within a section.
fn rows(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([space::L, space::S]).min_col_width(110.0).show(ui, add);
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(theme::muted(label));
    ui.horizontal(add);
    ui.end_row();
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().color(color::TEXT_FAINT));
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(space::M);
    theme::caption(ui, title);
    ui.add_space(space::XS);
}

/// Paper size, size, paper style and line spacing rows (the open note, or the
/// defaults for new notes).
fn paper_rows(ui: &mut Ui, id: &str, width: &mut f32, height: &mut f32, kind: &mut PageKind, spacing: &mut f32) {
    row(ui, "Paper size", |ui| {
        egui::ComboBox::from_id_salt((id, "paper_size"))
            .selected_text(paper_label(*width, *height))
            .width(180.0)
            .show_ui(ui, |ui| {
                for (name, dims, w, h) in PAPERS {
                    if ui.selectable_label(false, format!("{name}  ({dims})")).clicked() {
                        *width = w;
                        *height = h;
                    }
                }
            });
    });
    row(ui, "Size", |ui| {
        ui.add(DragValue::new(width).range(200.0..=4000.0).speed(2.0).suffix(" px"));
        ui.label(theme::muted("×"));
        ui.add(DragValue::new(height).range(200.0..=6000.0).speed(2.0).suffix(" px"));
        if ui.button(icon::ARROWS_CLOCKWISE).on_hover_text("Rotate (swap width and height)").clicked() {
            std::mem::swap(width, height);
        }
    });
    row(ui, "Paper", |ui| {
        for (k, label) in [(PageKind::Blank, "Blank"), (PageKind::Ruled, "Ruled"), (PageKind::Grid, "Grid")] {
            ui.selectable_value(kind, k, label);
        }
    });
    row(ui, "Line spacing", |ui| {
        ui.add(Slider::new(spacing, 16.0..=80.0));
    });
}

impl InkApp {
    // ----------------------------------------------------- note settings

    /// The note settings view of the left panel.
    pub(super) fn note_settings_view(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left_panel_settings")
            .resizable(true)
            .default_width(NOTE_WIDTH)
            .width_range(NOTE_WIDTHS)
            .show_separator_line(false)
            .frame(Self::view_frame())
            .show(ctx, |ui| {
                Self::view_divider(ui);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.note_settings(ui));
            });
    }

    fn note_settings(&mut self, ui: &mut Ui) {
        section(ui, "Page");
        rows(ui, "note_page", |ui| {
            let d = &mut self.doc;
            paper_rows(ui, "note", &mut d.page_width, &mut d.page_height, &mut d.page.kind, &mut d.page.spacing);
        });
        hint(ui, "Changing the size keeps existing ink where it is; it is not rescaled.");

        section(ui, "Pages");
        rows(ui, "note_pages", |ui| {
            row(ui, "Pages", |ui| {
                ui.label(format!("{}", self.doc.page_count));
                if ui.button("Add").clicked() {
                    self.insert_page(self.doc.page_count);
                }
                let last = self.doc.page_count.saturating_sub(1);
                let removable = self.doc.page_count > 1 && !self.doc.strokes.iter().any(|s| s.page == last);
                if ui
                    .add_enabled(removable, egui::Button::new("Remove last"))
                    .on_disabled_hover_text("Only an empty last page can be removed")
                    .clicked()
                {
                    let index = self.doc.page_count - 1;
                    self.clear_selection();
                    self.history.exec(&mut self.doc, Action::RemovePage { index });
                }
            });
        });

        section(ui, "Note");
        let root = self.library_root();
        rows(ui, "note_info", |ui| {
            row(ui, "Title", |ui| {
                ui.label(RichText::new(&self.note_meta.title).strong());
            });
            row(ui, "Created", |ui| {
                ui.label(when(self.note_meta.created));
            });
            row(ui, "Modified", |ui| {
                ui.label(when(self.note_meta.modified));
            });
            if let Some(path) = &self.note_path {
                row(ui, "Location", |ui| {
                    ui.label(location(&root, path)).on_hover_text(path.display().to_string());
                });
            }
        });
        hint(ui, "Rename the note from the title bar.");
        ui.add_space(space::S);
        ui.horizontal(|ui| {
            if ui.button(format!("{}  Save now", icon::FLOPPY_DISK)).on_hover_text("Ctrl+S").clicked() {
                self.save_note(true);
            }
            if ui
                .add(theme::primary_button(format!("{}  Export PDF...", icon::FILE_PDF)))
                .on_hover_text("Ctrl+E")
                .clicked()
            {
                self.open_export_dialog();
            }
        });
        hint(ui, &format!(
            "Notes save automatically {} s after each change (File > Settings).",
            fmt_secs(self.settings.autosave_secs)
        ));
    }

    // --------------------------------------------------- global settings

    /// File > Settings: global preferences in a floating window.
    pub(super) fn global_settings_window(&mut self, ctx: &egui::Context) {
        if !self.global_settings_open {
            return;
        }
        let mut open = true;
        let max_h = ctx.screen_rect().height() * 0.75;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .default_width(460.0)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| self.global_settings(ui));
            });
        self.global_settings_open = open;
    }

    fn global_settings(&mut self, ui: &mut Ui) {
        let mut change_folder = false;
        let mut reset = false;
        let s = &mut self.settings;

        section(ui, "Appearance");
        rows(ui, "global_appearance", |ui| {
            row(ui, "Interface size", |ui| {
                ui.add(
                    Slider::new(&mut s.ui_scale, 0.75..=1.5)
                        .step_by(0.05)
                        .custom_formatter(|v, _| format!("{:.0} %", v * 100.0)),
                )
                .on_hover_text("Applied when you release the slider");
            });
            row(ui, "Title bar", |ui| {
                ui.checkbox(&mut s.custom_title_bar, "Custom, with the menus")
                    .on_hover_text("Off: the system title bar, with the menus below it");
            });
            row(ui, "Toolbar", |ui| {
                for (pos, label) in [(ToolbarPosition::Top, "Top"), (ToolbarPosition::Bottom, "Bottom")] {
                    ui.selectable_value(&mut s.toolbar_position, pos, label);
                }
            });
        });

        section(ui, "Canvas");
        rows(ui, "global_canvas", |ui| {
            row(ui, "Smooth scrolling", |ui| {
                ui.checkbox(&mut s.smooth_scroll, "Gliding wheel, flick to pan, animated page jumps");
            });
            row(ui, "Scroll speed", |ui| {
                ui.add(Slider::new(&mut s.scroll_speed, 0.5..=3.0).step_by(0.1).suffix("×"));
            });
        });

        section(ui, "Pen and input");
        ui.checkbox(&mut s.predict_pen, "Predict the pen tip (lower perceived latency)");
        ui.checkbox(&mut s.pressure, "Pressure sensitivity (line width follows the pen pressure)");
        ui.checkbox(&mut s.side_button_eraser, "Pen side button erases while held")
            .on_hover_text("The eraser end of the pen always erases");
        rows(ui, "global_pen", |ui| {
            row(ui, "Eraser size", |ui| {
                ui.add(Slider::new(&mut s.eraser_radius, 4.0..=60.0));
            });
        });

        section(ui, "New notes");
        rows(ui, "global_new_notes", |ui| {
            paper_rows(ui, "new", &mut s.new_page_width, &mut s.new_page_height, &mut s.new_page_kind, &mut s.new_page_spacing);
        });
        hint(ui, "Used for notes created from now on. Each note keeps its own paper (Note settings).");

        section(ui, "Saving");
        rows(ui, "global_saving", |ui| {
            row(ui, "Autosave after", |ui| {
                ui.add(Slider::new(&mut s.autosave_secs, 1.0..=30.0).step_by(1.0).suffix(" s"));
            });
        });
        hint(ui, "Notes are written this long after the last change; a backup copy is kept.");

        section(ui, "Library");
        rows(ui, "global_library", |ui| {
            row(ui, "Folder", |ui| {
                ui.add(egui::Label::new(theme::muted(&s.library_root)).truncate()).on_hover_text(&s.library_root);
            });
            row(ui, "", |ui| {
                if ui.button(format!("{}  Change...", icon::FOLDER_OPEN)).clicked() {
                    change_folder = true;
                }
            });
            row(ui, "Recent notes", |ui| {
                ui.add(DragValue::new(&mut s.recent_count).range(4..=48)).on_hover_text("Shown on the library screen");
            });
        });

        ui.add_space(space::L);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(format!("{}  Reset to defaults", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
                reset = true;
            }
            ui.label(RichText::new("Keeps the library folder and the panels.").small().color(color::TEXT_FAINT));
        });

        if change_folder {
            self.lib_ui.dialog = Some(Dialog::Folder(self.settings.library_root.clone()));
        }
        if reset {
            let old = std::mem::take(&mut self.settings);
            self.settings.library_root = old.library_root;
            self.settings.left_panel = old.left_panel;
            self.settings.library_panel_open = old.library_panel_open;
        }
    }
}

/// "2", "2.5".
fn fmt_secs(s: f32) -> String {
    if s.fract() == 0.0 { format!("{s:.0}") } else { format!("{s:.1}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn seconds_format() {
        assert_eq!(fmt_secs(2.0), "2");
        assert_eq!(fmt_secs(2.5), "2.5");
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert!(s.ui_scale == 1.0 && s.scroll_speed == 1.0 && s.smooth_scroll && s.pressure);
        assert!(s.autosave_secs >= 1.0 && s.recent_count >= 4);
        assert!(s.new_page_width > 0.0 && s.new_page_height > 0.0);
    }
}
