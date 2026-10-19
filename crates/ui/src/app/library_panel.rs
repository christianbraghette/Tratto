// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The notebook panel of the editor, on the right: the sections and notes of
//! the open note's notebook, to switch notes without leaving the editor.
//!
//! * the header is the notebook's name, editable in place (renames the
//!   notebook folder; the open note moves with it)
//! * "General" holds the notes directly in the notebook, then one group per
//!   section; the open note is highlighted
//! * right-click a note, a section or the empty space for its actions
//! * drag a note within its group to reorder it (saved per folder; the
//!   default order is the order of creation)
//! * a quick note (outside any notebook) shows the quick notes instead
//!
//! Like the pages panel it is a corner notch (top right) grown into a
//! sidebar: same surface, the library button at the same position, and the
//! concave joins drawn by `toolbar::show_corners`.

use std::path::{Path, PathBuf};

use eframe::egui::{self, vec2, Align, Color32, Id, Layout, Sense, Ui};
use egui_phosphor::regular as icon;

use inknote_core::library::{self, NoteInfo, NotebookInfo};

use super::library_ui::{Act, Dialog};
use super::menus::entry;
use super::InkApp;
use crate::scrollbar;
use crate::theme::{self, color, size, space};
use crate::toolbar;

const PANEL_WIDTH: f32 = 260.0;
const PANEL_WIDTHS: std::ops::RangeInclusive<f32> = 200.0..=480.0;

/// What the panel asks for; applied after drawing.
enum Pick {
    Open(PathBuf),
    NewNote(PathBuf),
    LibraryScreen,
    Act(Act),
    /// New order of the notes of a folder.
    Reorder(PathBuf, Vec<PathBuf>),
}

/// A note being dragged to a new place in its group.
pub(super) struct NoteDrag {
    /// The group's folder.
    dir: PathBuf,
    from: usize,
    title: String,
}

impl InkApp {
    pub(super) fn library_panel(&mut self, ctx: &egui::Context) {
        if !self.settings.library_panel_open {
            return;
        }
        let top = toolbar::notch_margin(true);
        let current = self.note_path.clone();
        let notebook: Option<NotebookInfo> = current
            .as_ref()
            .and_then(|p| self.library.notebooks.iter().find(|nb| p.starts_with(&nb.path)))
            .cloned();

        // The name field keeps what is typed while focused; otherwise it
        // follows the notebook.
        let mut name = std::mem::take(&mut self.notebook_buf);
        if !self.notebook_focused {
            name = notebook.as_ref().map(|nb| nb.name.clone()).unwrap_or_default();
        }
        let mut name_resp = None;
        let mut open = true;
        let mut picks: Vec<Pick> = Vec::new();
        let mut drag = self.note_drag.take();
        let root = self.library_root();

        egui::SidePanel::right("library_panel")
            .resizable(true)
            .default_width(PANEL_WIDTH)
            .width_range(PANEL_WIDTHS)
            .show_separator_line(false)
            // The canvas scrollbar (same surface) sits just left of the panel,
            // so the extra right margin keeps the library button exactly
            // where it is in the notch, and the padding visually symmetric.
            .frame(egui::Frame::none().fill(color::CHROME).inner_margin(egui::Margin {
                left: top.left,
                right: top.right + scrollbar::THICKNESS,
                top: top.top,
                bottom: space::S,
            }))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    toolbar::notch_buttons_style(ui);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        toolbar::library_button(ui, &mut open);
                        if ui
                            .button(theme::icon(icon::ARROW_SQUARE_OUT))
                            .on_hover_text("Open the library screen (Ctrl+W)")
                            .clicked()
                        {
                            picks.push(Pick::LibraryScreen);
                        }
                        ui.add_space(space::XS);
                        if let Some(nb) = &notebook {
                            let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                            // The notebook's icon in its colour, then the name.
                            let tint = Color32::from_rgb(nb.color[0], nb.color[1], nb.color[2]);
                            let icon_center = egui::pos2(area.left() + 12.0, area.center().y);
                            ui.painter().text(icon_center, egui::Align2::CENTER_CENTER, icon::NOTEBOOK, theme::font(size::ICON), tint);
                            let rect = egui::Rect::from_min_max(egui::pos2(area.left() + 26.0, area.top()), area.max);
                            let resp = theme::inline_edit(
                                ui,
                                rect,
                                Id::new("notebook_name_edit"),
                                &mut name,
                                "Notebook name",
                                theme::font_strong(size::HEADING),
                                Align::Min,
                                color::TEXT_STRONG,
                            );
                            name_resp = Some(resp.on_hover_text("Rename the notebook"));
                        } else {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add_space(space::S);
                                ui.label(theme::heading("Quick notes"));
                            });
                        }
                    });
                });
                ui.add_space(space::S);

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                        theme::ghost_buttons(ui);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        // Long titles are cut with "…" instead of wrapping.
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        match &notebook {
                            Some(nb) => notebook_tree(ui, nb, current.as_deref(), &mut picks, &mut drag),
                            None => note_list(ui, &root, &self.library.quick, current.as_deref(), &mut picks, &mut drag),
                        }
                        // New note next to the open one (same notebook / section).
                        if let Some(dir) = current.as_deref().and_then(Path::parent) {
                            ui.add_space(space::S);
                            let (tile, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                            if theme::add_tile(ui, tile, Id::new("new_note_tile"), "New note here")
                                .on_hover_text("Ctrl+N")
                                .clicked()
                            {
                                picks.push(Pick::NewNote(dir.to_path_buf()));
                            }
                        }
                        // The rest of the panel: actions for the notebook.
                        let rest = ui.allocate_response(vec2(ui.available_width(), ui.available_height().max(48.0)), Sense::click());
                        let dir = notebook.as_ref().map(|nb| nb.path.clone()).unwrap_or_else(|| self.library_root());
                        rest.context_menu(|ui| {
                            if entry(ui, icon::NOTE_PENCIL, "New note", "", true) {
                                picks.push(Pick::NewNote(dir.clone()));
                                ui.close_menu();
                            }
                            if let Some(nb) = &notebook {
                                if entry(ui, icon::FOLDER_SIMPLE_PLUS, "New section...", "", true) {
                                    let d = Dialog::NewSection { notebook: nb.path.clone(), text: String::new() };
                                    picks.push(Pick::Act(Act::Dialog(d)));
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                });
            });

        // Notebook rename: Enter or a click elsewhere; Escape keeps the name.
        if let (Some(resp), Some(nb)) = (&name_resp, &notebook) {
            self.notebook_focused = resp.has_focus();
            let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            let new = name.trim();
            if resp.lost_focus() && !escape && !new.is_empty() && new != nb.name {
                picks.push(Pick::Act(Act::RenameDir(nb.path.clone(), new.to_owned())));
            }
        } else {
            self.notebook_focused = false;
        }
        self.notebook_buf = name;

        // A drag released outside every group just ends.
        if ctx.input(|i| !i.pointer.any_down()) {
            drag = None;
        }
        self.note_drag = drag;

        if !open {
            self.settings.library_panel_open = false;
        }
        for pick in picks {
            match pick {
                Pick::Open(path) => {
                    // Saves the current note first; stays put if that fails.
                    self.close_note(ctx);
                    if self.screen == super::Screen::Library {
                        self.open_note(&path);
                    }
                }
                Pick::NewNote(dir) => {
                    self.close_note(ctx);
                    if self.screen == super::Screen::Library {
                        self.new_note(&dir);
                    }
                }
                Pick::LibraryScreen => self.close_note(ctx),
                Pick::Act(act) => self.apply_act(act),
                Pick::Reorder(dir, order) => match library::set_note_order(&dir, &order) {
                    Ok(()) => self.refresh_library(),
                    Err(e) => self.toast(format!("Cannot save the order: {e}")),
                },
            }
        }
    }
}

/// "General" (notes directly in the notebook), then one group per section.
fn notebook_tree(
    ui: &mut Ui,
    nb: &NotebookInfo,
    current: Option<&Path>,
    picks: &mut Vec<Pick>,
    drag: &mut Option<NoteDrag>,
) {
    let contains = |notes: &[NoteInfo]| notes.iter().any(|n| Some(n.path.as_path()) == current);
    let tint = Color32::from_rgb(nb.color[0], nb.color[1], nb.color[2]);

    if !nb.loose.is_empty() || nb.sections.is_empty() {
        let header = group_label(icon::TRAY, "General", nb.loose.len(), contains(&nb.loose));
        let group = egui::CollapsingHeader::new(header)
            .id_salt(("general", &nb.path))
            .default_open(true)
            .icon(move |ui, openness, resp| tree_icon(ui, openness, resp, tint))
            .show(ui, |ui| {
                if nb.loose.is_empty() {
                    ui.label(theme::muted("No notes").small());
                }
                note_list(ui, &nb.path, &nb.loose, current, picks, drag);
            });
        group.header_response.context_menu(|ui| {
            if entry(ui, icon::NOTE_PENCIL, "New note here", "", true) {
                picks.push(Pick::NewNote(nb.path.clone()));
                ui.close_menu();
            }
        });
    }

    for s in &nb.sections {
        let has_current = contains(&s.notes);
        let header = group_label(icon::FOLDER_SIMPLE, &s.name, s.notes.len(), has_current);
        let group = egui::CollapsingHeader::new(header)
            .id_salt(&s.path)
            .default_open(has_current)
            .icon(move |ui, openness, resp| tree_icon(ui, openness, resp, tint))
            .show(ui, |ui| {
                if s.notes.is_empty() {
                    ui.label(theme::muted("No notes").small());
                }
                note_list(ui, &s.path, &s.notes, current, picks, drag);
            });
        group.header_response.context_menu(|ui| {
            if entry(ui, icon::NOTE_PENCIL, "New note here", "", true) {
                picks.push(Pick::NewNote(s.path.clone()));
                ui.close_menu();
            }
            if entry(ui, icon::PENCIL_SIMPLE, "Rename section...", "", true) {
                let d = Dialog::RenameDir { path: s.path.clone(), text: s.name.clone() };
                picks.push(Pick::Act(Act::Dialog(d)));
                ui.close_menu();
            }
            ui.separator();
            let resp = ui.add_enabled_ui(!has_current, |ui| entry(ui, icon::TRASH, "Delete section...", "", true));
            if resp.inner {
                let d = Dialog::Trash { path: s.path.clone(), label: s.name.clone() };
                picks.push(Pick::Act(Act::Dialog(d)));
                ui.close_menu();
            }
            if has_current {
                resp.response.on_hover_text("It contains the open note");
            }
        });
    }
}

/// The notes of one folder; drag a row to reorder them.
fn note_list(
    ui: &mut Ui,
    dir: &Path,
    notes: &[NoteInfo],
    current: Option<&Path>,
    picks: &mut Vec<Pick>,
    drag: &mut Option<NoteDrag>,
) {
    let mut rects = Vec::with_capacity(notes.len());
    for (i, n) in notes.iter().enumerate() {
        let resp = note_row(ui, n, current, picks);
        if resp.drag_started() {
            *drag = Some(NoteDrag { dir: dir.to_path_buf(), from: i, title: n.meta.title.clone() });
        }
        rects.push(resp.rect);
    }
    let Some(d) = drag.as_ref().filter(|d| d.dir == dir) else { return };
    let (Some(p), Some(first)) = (ui.input(|i| i.pointer.interact_pos()), rects.first()) else { return };
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);

    // Where it would land: an accent line between the rows.
    let slot = rects.iter().filter(|r| r.center().y < p.y).count();
    let y = if slot < rects.len() { rects[slot].top() - 1.0 } else { rects[rects.len() - 1].bottom() + 1.0 };
    let line = egui::Rect::from_min_max(egui::pos2(first.left(), y - 1.0), egui::pos2(first.right(), y + 1.0));
    ui.painter().rect_filled(line, 1.0, color::ACCENT);

    // Scroll the list near its edges; a label follows the pointer.
    let clip = ui.clip_rect();
    if p.y < clip.top() + 24.0 {
        ui.scroll_with_delta(vec2(0.0, 10.0));
    } else if p.y > clip.bottom() - 24.0 {
        ui.scroll_with_delta(vec2(0.0, -10.0));
    }
    let ghost = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("note_drag_ghost")));
    theme::paint_toast(&ghost, p + vec2(64.0, 22.0), &d.title);

    if ui.input(|i| !i.pointer.any_down()) {
        let to = if slot > d.from { slot - 1 } else { slot };
        if to != d.from {
            let mut order: Vec<PathBuf> = notes.iter().map(|n| n.path.clone()).collect();
            let moved = order.remove(d.from);
            order.insert(to, moved);
            picks.push(Pick::Reorder(dir.to_path_buf(), order));
        }
        *drag = None;
    }
}

/// One note; the open note is highlighted. Right-click for its actions.
fn note_row(ui: &mut Ui, note: &NoteInfo, current: Option<&Path>, picks: &mut Vec<Pick>) -> egui::Response {
    let is_current = Some(note.path.as_path()) == current;
    let resp = theme::list_row(ui, icon::NOTE_BLANK, &note.meta.title, is_current);
    if resp.clicked() && !is_current {
        picks.push(Pick::Open(note.path.clone()));
    }
    resp.context_menu(|ui| {
        if entry(ui, icon::ARROW_SQUARE_OUT, "Open", "", !is_current) {
            picks.push(Pick::Open(note.path.clone()));
            ui.close_menu();
        }
        if entry(ui, icon::PENCIL_SIMPLE, "Rename...", "", true) {
            let d = Dialog::RenameNote { path: note.path.clone(), text: note.meta.title.clone() };
            picks.push(Pick::Act(Act::Dialog(d)));
            ui.close_menu();
        }
        if entry(ui, icon::COPY, "Duplicate", "", true) {
            picks.push(Pick::Act(Act::Duplicate(note.path.clone())));
            ui.close_menu();
        }
        ui.separator();
        let resp = ui.add_enabled_ui(!is_current, |ui| entry(ui, icon::TRASH, "Delete...", "", true));
        if resp.inner {
            let d = Dialog::Trash { path: note.path.clone(), label: note.meta.title.clone() };
            picks.push(Pick::Act(Act::Dialog(d)));
            ui.close_menu();
        }
        if is_current {
            resp.response.on_hover_text("This is the open note");
        }
    });
    resp
}

/// A group header: icon and name, then the number of notes, muted.
fn group_label(glyph: &str, name: &str, count: usize, has_current: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let main = egui::TextFormat {
        font_id: theme::font(size::BODY),
        color: if has_current { color::TEXT_STRONG } else { color::TEXT },
        ..Default::default()
    };
    job.append(&format!("{glyph}  {name}"), 0.0, main);
    let faint = egui::TextFormat { font_id: theme::font(size::CAPTION), color: color::TEXT_FAINT, ..Default::default() };
    job.append(&count.to_string(), 8.0, faint);
    job
}

/// Disclosure triangle in the notebook's colour.
fn tree_icon(ui: &mut Ui, openness: f32, resp: &egui::Response, tint: Color32) {
    let glyph = if openness > 0.5 { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    ui.painter().text(resp.rect.center(), egui::Align2::CENTER_CENTER, glyph, theme::font(12.0), tint);
}
