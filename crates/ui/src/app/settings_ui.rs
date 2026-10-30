// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The two settings views:
//!
//! * **Note settings** (the gear button, sidebar): what is stored in the
//!   open note (paper, pages, title) and actions on it (save, export).
//! * **Settings** (File > Settings, `Ctrl+,`): global preferences, in a
//!   floating window, on the library screen too.

use eframe::egui::{self, Align2, DragValue, RichText, Slider, Ui, vec2};
use egui_phosphor::regular as icon;
use tratto_core::history::Action;
use tratto_core::model::{CANVAS_SIZE, PAPERS, PDF_GAPS, PageKind, paper_label};

use super::InkApp;
use super::library_ui::{Dialog, location, when};
use super::menus::SETTINGS_WINDOW;
use crate::file_assoc;
use crate::settings::{BatterySaver, EraserMode, Settings, SidebarSide, ToolbarPosition};
use crate::theme::{self, ThemeMode, color, space};

/// Note settings in the sidebar: default width and resize range.
const NOTE_WIDTH: f32 = 340.0;

// ------------------------------------------------------------- helpers

/// Label | control rows, aligned within a section.
pub(super) fn rows(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([space::L, space::S]).min_col_width(110.0).show(ui, add);
}

pub(super) fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(theme::muted(label));
    ui.horizontal(add);
    ui.end_row();
}

pub(super) fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().color(color::text_faint()));
}

pub(super) fn section(ui: &mut Ui, title: &str) {
    ui.add_space(space::M);
    theme::caption(ui, title);
    ui.add_space(space::XS);
}

/// Paper size, size, paper style and line spacing rows (the open note, or the
/// defaults for new notes). The paper size can be infinite (`model::CANVAS_SIZE`)
/// if `infinite` (else that entry is there, disabled), and a size if `sized`
/// (else those entries are disabled, with `unsized_why`); an infinite page
/// has no size to set.
#[allow(clippy::too_many_arguments)]
fn paper_rows(
    ui: &mut Ui,
    id: &str,
    (width, height): (&mut f32, &mut f32),
    infinite: bool,
    (sized, unsized_why): (bool, &str),
    kind: &mut PageKind,
    spacing: &mut f32,
) {
    row(ui, "Paper size", |ui| {
        egui::ComboBox::from_id_salt((id, "paper_size"))
            .selected_text(paper_label(*width, *height))
            .width(180.0)
            .show_ui(ui, |ui| {
                for (name, dims, w, h) in PAPERS {
                    let entry = egui::Button::selectable(false, format!("{name}  ({dims})"));
                    if ui.add_enabled(sized, entry).on_disabled_hover_text(unsized_why).clicked() {
                        *width = w;
                        *height = h;
                    }
                }
                let entry = egui::Button::selectable(false, "Infinite  (grows as you write)");
                let entry = ui
                    .add_enabled(infinite, entry)
                    .on_disabled_hover_text("Only a note of one page, or written on a PDF, can have an infinite page");
                if entry.clicked() {
                    (*width, *height) = (CANVAS_SIZE, CANVAS_SIZE);
                }
            });
    });
    if *width >= CANVAS_SIZE {
        return paper_style_rows(ui, id, kind, spacing);
    }
    row(ui, "Size", |ui| {
        ui.add(DragValue::new(width).range(200.0..=4000.0).speed(2.0).suffix(" px"));
        ui.label(theme::muted("×"));
        ui.add(DragValue::new(height).range(200.0..=6000.0).speed(2.0).suffix(" px"));
        if ui.button(icon::ARROWS_CLOCKWISE).on_hover_text("Rotate (swap width and height)").clicked() {
            std::mem::swap(width, height);
        }
    });
    paper_style_rows(ui, id, kind, spacing);
}

/// Paper style and line spacing rows. The paper is a list, like the paper size: its kinds side by side are wider
/// than the note settings.
fn paper_style_rows(ui: &mut Ui, id: &str, kind: &mut PageKind, spacing: &mut f32) {
    row(ui, "Paper", |ui| {
        let name = PageKind::ALL.iter().find(|(k, _)| k == kind).map_or("", |(_, n)| n);
        egui::ComboBox::from_id_salt((id, "paper_kind")).selected_text(name).width(180.0).show_ui(ui, |ui| {
            for (k, label) in PageKind::ALL {
                ui.selectable_value(kind, k, label);
            }
        });
    });
    row(ui, "Line spacing", |ui| {
        ui.add(Slider::new(spacing, 16.0..=80.0));
    });
}

impl InkApp {
    // ----------------------------------------------------- note settings

    /// The note settings view of the sidebar.
    pub(super) fn note_settings_view(&mut self, ui: &mut egui::Ui) {
        let side = self.settings.sidebar_side;
        Self::side_panel(side, "sidebar_settings")
            .resizable(false)
            .exact_size(NOTE_WIDTH)
            .show_separator_line(false)
            .frame(Self::view_frame(true))
            .show(ui, |ui| {
                Self::view_divider(ui, true, side);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.note_settings(ui));
            });
    }

    fn note_settings(&mut self, ui: &mut Ui) {
        self.page_settings(ui);
        self.note_info(ui);
    }

    /// The page size, the paper and the pages of the open note (an infinite
    /// page is the only one).
    fn page_settings(&mut self, ui: &mut Ui) {
        let (was_canvas, before) = (self.doc.is_canvas(), (self.doc.page_width, self.doc.page_height));
        // An infinite page on a PDF becomes pages again only if all on it
        // has a page to go to.
        let on_pdf = self.doc.pdf.is_some();
        let sized = !(was_canvas && on_pdf) || self.doc.can_leave_canvas();
        let why = "Move what is beside the PDF's pages onto them first: it would have no page";
        section(ui, "Page");
        rows(ui, "note_page", |ui| {
            let d = &mut self.doc;
            let infinite = d.can_be_canvas();
            let size = (&mut d.page_width, &mut d.page_height);
            paper_rows(ui, "note", size, infinite, (sized, why), &mut d.page.kind, &mut d.page.spacing);
        });
        if on_pdf && self.doc.is_canvas() != was_canvas {
            let picked = (self.doc.page_width, self.doc.page_height);
            (self.doc.page_width, self.doc.page_height) = before;
            self.pdf_paper_switched(picked);
        }
        if self.doc.is_canvas() {
            if let Some(gap) = self.doc.pdf.as_ref().map(|pdf| pdf.gap) {
                self.pdf_gap_rows(ui, gap);
                hint(ui, "The PDF's pages are on it, one under the other; what is on or beside a page moves with it.");
            } else {
                hint(ui, "An infinite page grows right and down as you write on it.");
            }
            return;
        }
        hint(ui, "Changing the size keeps existing ink where it is; it is not rescaled.");

        section(ui, "Pages");
        rows(ui, "note_pages", |ui| {
            row(ui, "Pages", |ui| {
                ui.label(format!("{}", self.doc.page_count));
                if ui.button("Add").clicked() {
                    self.insert_page(self.doc.page_count);
                }
                let last = self.doc.page_count.saturating_sub(1);
                let d = &self.doc;
                let empty = !d.strokes.iter().any(|s| s.page == last)
                    && !d.texts.iter().any(|t| t.page == last)
                    && !d.images.iter().any(|i| i.page == last)
                    && self.edit.as_ref().is_none_or(|e| e.page != last);
                let removable = self.doc.page_count > 1 && empty;
                if ui
                    .add_enabled(removable, egui::Button::new("Remove last"))
                    .on_disabled_hover_text("Only an empty last page can be removed")
                    .clicked()
                {
                    let index = self.doc.page_count - 1;
                    self.before_page_change();
                    self.history.exec(&mut self.doc, Action::RemovePage { index });
                }
            });
        });
    }

    /// A note on a PDF made one infinite page, or pages again (of the size
    /// `picked`): what is on it goes along, its PDF's pages too (see
    /// `Document::pdf_to_canvas`). One undo step.
    fn pdf_paper_switched(&mut self, picked: (f32, f32)) {
        self.before_page_change();
        let mut doc = self.doc.clone();
        if doc.is_canvas() {
            if !doc.canvas_to_pdf_pages() {
                return;
            }
            (doc.page_width, doc.page_height) = picked;
        } else {
            doc.pdf_to_canvas();
        }
        crate::markdown::lay_out_document(&self.egui_ctx, &mut doc);
        self.history.exec(&mut self.doc, Action::Swap(Box::new(doc)));
        self.scroll = egui::Vec2::ZERO;
        self.canvas_centre = true;
        self.kinetic.stop();
    }

    /// The room between the PDF's pages on an infinite page (`gap`). The
    /// pages move while it is dragged; one undo step once it is let go.
    fn pdf_gap_rows(&mut self, ui: &mut Ui, gap: f32) {
        let mut value = gap;
        let mut resp = None;
        rows(ui, "note_pdf_gap", |ui| {
            row(ui, "Page spacing", |ui| {
                resp = Some(ui.add(Slider::new(&mut value, PDF_GAPS).step_by(4.0).suffix(" px")));
            });
        });
        if value != gap {
            if self.gap_drag.is_none() {
                self.before_page_change();
                self.gap_drag = Some(self.doc.clone());
            }
            self.doc.set_pdf_gap(value);
        }
        if resp.is_some_and(|r| !r.dragged())
            && let Some(before) = self.gap_drag.take()
        {
            let after = std::mem::replace(&mut self.doc, before);
            self.history.exec(&mut self.doc, Action::Swap(Box::new(after)));
        }
    }

    /// The title, dates and place of the open note.
    fn note_info(&mut self, ui: &mut Ui) {
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
                self.open_export_dialog(ui.ctx(), false);
            }
        });
        hint(
            ui,
            &format!(
                "Notes save automatically {} s after each change (File > Settings).",
                fmt_secs(self.settings.autosave_secs)
            ),
        );
    }

    // --------------------------------------------------- global settings

    /// File > Settings: global preferences in a floating window.
    pub(super) fn global_settings_window(&mut self, ctx: &egui::Context) {
        if !self.global_settings_open {
            return;
        }
        // Large: most of the window, what does not fit scrolls.
        let screen = ctx.viewport_rect();
        let size = vec2((screen.width() * 0.9).min(540.0), screen.height() * 0.85);
        let shown = theme::window(SETTINGS_WINDOW, "Settings")
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .fixed_size(size)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.global_settings(ui));
            });
        if shown.is_some_and(|w| theme::window_close_button(&w.response)) {
            self.global_settings_open = false;
        }
    }

    fn global_settings(&mut self, ui: &mut Ui) {
        let mut change_folder = false;
        let mut reset = false;
        // Some(true): register for PDFs, Some(false): unregister.
        let mut pdf_reader = None;
        let standalone = self.standalone.is_some();
        // Sliders filling their column.
        ui.spacing_mut().slider_width = 220.0;
        let vsync_now = self.frame_limit.vsync();
        let vsync_forced = self.frame_limit.vsync_forced();
        let saving_power = self.saving_power(ui.ctx());
        let fps_overridden = self.frame_limit.overridden();
        let s = &mut self.settings;

        if file_assoc::SUPPORTED {
            section(ui, "PDF reader");
            let registered = file_assoc::is_registered();
            rows(ui, "global_pdf_reader", |ui| {
                row(ui, "Open PDFs", |ui| {
                    if registered {
                        ui.label(format!("{}  With Tratto", icon::CHECK));
                        if ui.button("Stop").on_hover_text("Tratto no longer opens PDFs").clicked() {
                            pdf_reader = Some(false);
                        }
                    } else if ui.button(format!("{}  Use Tratto", icon::BOOK_OPEN)).clicked() {
                        pdf_reader = Some(true);
                    }
                });
            });
            hint(ui, "A PDF opened from the system gets a window of its own, for reading.");
        }

        appearance_section(ui, s);
        canvas_section(ui, s);
        pen_section(ui, s);
        display_section(ui, s, FrameRate { vsync_now, vsync_forced, saving_power, fps_overridden });

        section(ui, "New notes");
        rows(ui, "global_new_notes", |ui| {
            let size = (&mut s.new_page_width, &mut s.new_page_height);
            paper_rows(ui, "new", size, true, (true, ""), &mut s.new_page_kind, &mut s.new_page_spacing);
        });
        hint(ui, "Used for notes created from now on. Each note keeps its own paper (Note settings).");

        section(ui, "Saving");
        rows(ui, "global_saving", |ui| {
            row(ui, "Autosave after", |ui| {
                ui.add(Slider::new(&mut s.autosave_secs, 1.0..=30.0).step_by(1.0).suffix(" s"));
            });
        });
        hint(ui, "Notes are written this long after the last change; a backup copy is kept.");

        // A reader-only window has no library.
        if !standalone {
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
                    ui.add(DragValue::new(&mut s.recent_count).range(4..=48))
                        .on_hover_text("Shown on the library screen");
                });
            });
        }

        ui.add_space(space::L);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(format!("{}  Reset to defaults", icon::ARROW_COUNTER_CLOCKWISE)).clicked() {
                reset = true;
            }
            ui.label(RichText::new("Keeps the library folder and the panels.").small().color(color::text_faint()));
        });

        match pdf_reader.map(|on| (on, if on { file_assoc::register() } else { file_assoc::unregister() })) {
            Some((true, Ok(()))) => {
                self.toast(file_assoc::REGISTER_HINT);
                file_assoc::open_default_apps();
            }
            Some((false, Ok(()))) => self.toast("Tratto no longer opens PDFs"),
            Some((_, Err(e))) => self.toast(e),
            None => {}
        }
        if change_folder {
            self.show_dialog(Dialog::Folder(self.settings.library_root.clone()));
        }
        if reset {
            let old = std::mem::take(&mut self.settings);
            self.settings.library_root = old.library_root;
            self.settings.sidebar = old.sidebar;
            self.settings.sidebar_last = old.sidebar_last;
        }
    }
}

/// Theme, interface size, title bar, toolbar and sidebar places.
fn appearance_section(ui: &mut Ui, s: &mut Settings) {
    section(ui, "Appearance");
    rows(ui, "global_appearance", |ui| {
        row(ui, "Theme", |ui| {
            egui::ComboBox::from_id_salt("theme_mode").selected_text(s.theme.label()).show_ui(ui, |ui| {
                for mode in ThemeMode::ALL {
                    ui.selectable_value(&mut s.theme, mode, mode.label());
                }
            });
        });
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
        row(ui, "Sidebar", |ui| {
            for (side, label) in [(SidebarSide::Left, "Left"), (SidebarSide::Right, "Right")] {
                ui.selectable_value(&mut s.sidebar_side, side, label);
            }
            ui.label(theme::muted("Right: out from under a left hand"));
        });
    });
}

/// Scrolling, the minimap and what opens at startup.
fn canvas_section(ui: &mut Ui, s: &mut Settings) {
    section(ui, "Canvas");
    rows(ui, "global_canvas", |ui| {
        row(ui, "Smooth scrolling", |ui| {
            ui.checkbox(&mut s.smooth_scroll, "Gliding wheel, flick to pan, animated page jumps");
        });
        row(ui, "Scroll speed", |ui| {
            ui.add(Slider::new(&mut s.scroll_speed, 0.5..=3.0).step_by(0.1).suffix("×"));
        });
        let smooth = s.smooth_scroll;
        let off = "Needs smooth scrolling";
        row(ui, "Mouse wheel", |ui| {
            ui.add_enabled(smooth, strength_slider(&mut s.mouse_accel))
                .on_hover_text("Acceleration: quick notches scroll further")
                .on_disabled_hover_text(off);
        });
        row(ui, "Touchpad", |ui| {
            ui.add_enabled(smooth, strength_slider(&mut s.touchpad_accel))
                .on_hover_text("Acceleration and momentum: a quick swipe scrolls further and glides on")
                .on_disabled_hover_text(off);
        });
        row(ui, "Touch", |ui| {
            ui.add_enabled(smooth, strength_slider(&mut s.touch_fling))
                .on_hover_text("Momentum: how far the view glides after a flick (finger, or a pen or mouse drag)")
                .on_disabled_hover_text(off);
        });
        row(ui, "Minimap", |ui| {
            ui.checkbox(&mut s.minimap, "Dragging the scroll bar shows the note in small");
        });
        row(ui, "At startup", |ui| {
            ui.checkbox(&mut s.reopen_last_note, "Reopen the note that was open, where you left it")
                .on_hover_text("Every note opens where it was left; this also reopens the last one when Tratto starts");
        });
    });
}

/// Pen prediction and pressure, the side button and the eraser.
fn pen_section(ui: &mut Ui, s: &mut Settings) {
    section(ui, "Pen and input");
    rows(ui, "global_pen", |ui| {
        row(ui, "Prediction", |ui| {
            ui.checkbox(&mut s.predict_pen, "Predict the pen tip").on_hover_text("Lower perceived latency");
        });
        row(ui, "Pressure", |ui| {
            ui.checkbox(&mut s.pressure, "Line width follows the pen pressure");
        });
        row(ui, "Pressure strength", |ui| {
            let slider = Slider::new(&mut s.pressure_strength, 0.1..=1.0)
                .step_by(0.05)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0));
            ui.add_enabled(s.pressure, slider)
                .on_hover_text("How much the pressure thins the line; lower it if the lines come out too thin")
                .on_disabled_hover_text("Needs pressure sensitivity");
        });
        row(ui, "Side button", |ui| {
            ui.checkbox(&mut s.side_button_eraser, "Erases while held")
                .on_hover_text("The eraser end of the pen always erases");
        });
        row(ui, "Eraser size", |ui| {
            ui.add(Slider::new(&mut s.eraser_radius, 4.0..=60.0));
        });
        row(ui, "Eraser erases", |ui| {
            egui::ComboBox::from_id_salt("eraser_mode").selected_text(s.eraser_mode.label()).show_ui(ui, |ui| {
                for mode in EraserMode::ALL {
                    ui.selectable_value(&mut s.eraser_mode, mode, mode.label());
                }
            });
        });
    });
}

/// How the frame rate is decided now, for the Display section.
struct FrameRate {
    /// The window presents with vsync now.
    vsync_now: bool,
    /// `TRATTO_VSYNC` sets vsync.
    vsync_forced: bool,
    saving_power: bool,
    /// `TRATTO_FPS` sets the limit.
    fps_overridden: bool,
}

/// Vsync, the battery saver and the frame rate limit.
fn display_section(ui: &mut Ui, s: &mut Settings, rate: FrameRate) {
    let FrameRate { vsync_now, vsync_forced, saving_power, fps_overridden } = rate;
    section(ui, "Display");
    rows(ui, "global_display", |ui| {
        row(ui, "Vsync", |ui| {
            ui.add_enabled(!vsync_forced, egui::Checkbox::new(&mut s.vsync, "Wait for the display"))
                .on_hover_text("Saves power, but the ink trails the pen a little more")
                .on_disabled_hover_text("Set by the TRATTO_VSYNC environment variable");
        });
        row(ui, "Battery saver", |ui| {
            egui::ComboBox::from_id_salt("battery_saver")
                .selected_text(s.battery_saver.label())
                .show_ui(ui, |ui| {
                    for mode in BatterySaver::ALL {
                        ui.selectable_value(&mut s.battery_saver, mode, mode.label());
                    }
                })
                .response
                .on_hover_text(format!(
                    "Waits for the display to save power. Low: on battery at {}% or less",
                    crate::power::LOW_PERCENT
                ));
        });
        row(ui, "Frame rate limit", |ui| {
            ui.add_enabled_ui(!vsync_now && !fps_overridden, |ui| {
                egui::ComboBox::from_id_salt("fps_limit").selected_text(fps_label(s.fps_limit)).show_ui(ui, |ui| {
                    for fps in FPS_LIMITS {
                        ui.selectable_value(&mut s.fps_limit, fps, fps_label(fps));
                    }
                });
            })
            .response
            .on_disabled_hover_text(if vsync_now {
                "With vsync the display sets the frame rate"
            } else {
                "Set by the TRATTO_FPS environment variable"
            });
        });
    });
    if saving_power && !s.vsync {
        hint(ui, "The battery saver is on: Tratto waits for the display.");
    } else {
        hint(ui, "A lower limit uses less GPU while you write or scroll.");
    }
}

/// Scroll acceleration or momentum: off, the default (1×), up to double.
fn strength_slider(value: &mut f32) -> Slider<'_> {
    Slider::new(value, 0.0..=2.0)
        .step_by(0.1)
        .custom_formatter(|v, _| if v == 0.0 { "Off".to_owned() } else { format!("{v:.1}×") })
}

/// The frame rate limits offered (`0`: none).
const FPS_LIMITS: [u32; 7] = [60, 90, 120, 144, 165, 240, 0];

/// "165 fps", "No limit".
fn fps_label(fps: u32) -> String {
    if fps == 0 { "No limit".to_owned() } else { format!("{fps} fps") }
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
