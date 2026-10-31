// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The library screen's views, drawn from the scanned library and reporting
//! what is done in them as [`Act`]s: home, the quick notes, a notebook, the
//! trash, the note cards and notebook covers with their menus, and the note
//! previews they show. Plus the formatting of places and dates shared with
//! the sidebar and the settings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align2, Color32, CursorIcon, Id, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use egui_phosphor::regular as icon;

use tratto_core::library::{self, NoteInfo, NotebookInfo};
use tratto_core::model::{Point, StrokeKind};
use tratto_core::storage::{Preview, now_secs};

use super::library_ui::{Act, Dialog};
use super::menus::{button_menu, entry};
use crate::settings::LibrarySort;
use crate::stroke_gpu::{self, Batch, Blend};
use crate::theme::{self, color, radius, size, space};

const CARD_W: f32 = 168.0;
const COVER_W: f32 = 168.0;
const COVER_H: f32 = 118.0;

/// A note card being dragged (onto a notebook, the quick notes or the trash).
pub(super) struct DraggedNote {
    pub(super) path: PathBuf,
    pub(super) title: String,
}

pub(super) fn heading(ui: &mut Ui, text: &str) {
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
pub(super) fn empty_state(ui: &mut Ui, glyph: &str, title: &str, hint: &str) {
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
pub(super) fn place_name(notebook: Option<&NotebookInfo>, section: Option<&Path>) -> String {
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

pub(super) fn home_view(
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

pub(super) fn quick_view(ui: &mut Ui, lib: &library::Library, root: &Path, sort: LibrarySort, acts: &mut Vec<Act>) {
    let notes = sorted(lib.quick.iter().collect(), sort);
    heading_with_sort(ui, &count(notes.len(), "note"), sort, acts);
    if notes.is_empty() {
        ui.label(theme::muted("Notes outside any notebook. Drag a note here from a notebook, or start one:"));
        ui.add_space(space::M);
    }
    note_grid(ui, &notes, root, false, Some(root), acts);
}

pub(super) fn notebook_view(
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
        let tint = notebook_tint(nb);
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
pub(super) fn trash_view(ui: &mut Ui, lib: &library::Library, acts: &mut Vec<Act>) {
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
pub(super) fn nav_row(
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
pub(super) fn drop_target(resp: &egui::Response, acts: &mut Vec<Act>, act: impl FnOnce(PathBuf) -> Act) {
    if let Some(d) = resp.dnd_release_payload::<DraggedNote>() {
        acts.push(act(d.path.clone()));
    }
}

pub(super) fn notebook_menu(ui: &mut Ui, nb: &NotebookInfo, acts: &mut Vec<Act>) {
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
pub(super) fn parent_name(path: &Path) -> String {
    let parent = path.parent().unwrap_or(path);
    parent.file_name().map_or_else(|| parent.display().to_string(), |n| n.to_string_lossy().into_owned())
}

// ------------------------------------------------------------------ cards

/// The notes as cards; with `new_in`, a "New note" tile (for that folder)
/// comes first.
pub(super) fn note_grid(
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
    let base = notebook_tint(nb);
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

/// A notebook's colour (its cover, its icon, its section pills).
pub(super) fn notebook_tint(nb: &NotebookInfo) -> Color32 {
    let [r, g, b] = nb.color;
    Color32::from_rgb(r, g, b)
}

/// "1 note", "3 notes".
pub(super) fn count(n: usize, noun: &str) -> String {
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
    /// The highlighters, drawn first with the darken blend (as on the
    /// page), and the ink over them.
    highlight: Batch,
    ink: Batch,
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
        highlight: Batch::default(),
        ink: Batch::default(),
        last_used: frame,
    });
    entry.last_used = frame;
    if entry.made_for != made_for {
        let (mut highlight, mut ink) = (egui::epaint::Mesh::default(), egui::epaint::Mesh::default());
        for s in &pv.strokes {
            let kind = preview_kind(s.color);
            let mesh = if kind == StrokeKind::Highlighter { &mut highlight } else { &mut ink };
            // Full pressure: the stroke's own width.
            let pts: Vec<Point> = s.pts.iter().map(|p| Point::new(p[0], p[1], 1.0)).collect();
            let rgba = crate::render::paint_rgba(kind, s.color);
            crate::stroke_mesh::tessellate(mesh, kind, rgba, s.width, &pts, scale, 1.0 / ppp);
        }
        entry.highlight.replace(highlight);
        entry.ink.replace(ink);
        entry.made_for = made_for;
    }
    let at_card = rect.min.to_vec2();
    stroke_gpu::paint_batch(painter, &entry.highlight, at_card, 1.0, Blend::Darken);
    stroke_gpu::paint_batch(painter, &entry.ink, at_card, 1.0, Blend::Over);
    for b in &pv.bars {
        let r = Rect::from_min_size(at(b[0], b[1]), vec2(b[2] * scale, (b[3] * scale).max(1.0)));
        painter.rect_filled(r, 0.5, Color32::from_black_alpha(140));
    }
}

// ---------------------------------------------------------------- helpers

/// What drew a stroke of the preview, which keeps only its colour: pens
/// write opaque, a highlighter's colour carries its strength (alpha).
fn preview_kind(rgba: [u8; 4]) -> StrokeKind {
    if rgba[3] < 255 { StrokeKind::Highlighter } else { StrokeKind::Pen }
}

pub(super) fn truncate(s: &str, max: usize) -> String {
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
    fn previews_tell_highlighters_from_ink_by_their_colour() {
        assert_eq!(preview_kind([255, 230, 0, 96]), StrokeKind::Highlighter);
        assert_eq!(preview_kind([20, 20, 20, 255]), StrokeKind::Pen);
    }

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
