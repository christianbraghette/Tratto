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

use eframe::egui::collapsing_header::CollapsingState;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, Id, Layout, Rect, Sense, Ui};
use egui_phosphor::regular as icon;

use inknote_core::library::{self, NoteInfo, NotebookInfo, SectionInfo, TrashItem};

use super::library_ui::{when, Act, Dialog};
use super::menus::{button_menu, entry};
use super::InkApp;
use crate::scrollbar;
use crate::theme::{self, color, radius, size, space};
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
    /// Ask before emptying the trash.
    EmptyTrash,
}

/// A note being dragged to a new place in its group.
pub(super) struct NoteDrag {
    /// The group's folder.
    dir: PathBuf,
    path: PathBuf,
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
        let mut drop_to: Option<(PathBuf, String)> = None;
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
                        let mut tree = Tree { current: current.as_deref(), picks: &mut picks, drag: &mut drag, drop_to: None };
                        // The trash items that came from here: the notebook's
                        // notes and sections, or the library's own items.
                        let trash = &self.library.trash;
                        match &notebook {
                            Some(nb) => {
                                let inside = |o: &Path| o.starts_with(&nb.path) && o != nb.path;
                                let items: Vec<&TrashItem> =
                                    trash.iter().filter(|i| i.origin.as_deref().is_some_and(inside)).collect();
                                notebook_tree(ui, nb, &items, &mut tree);
                            }
                            None => {
                                let items: Vec<&TrashItem> = trash
                                    .iter()
                                    .filter(|i| i.origin.as_deref().map_or(true, |o| o.parent() == Some(root.as_path())))
                                    .collect();
                                quick_tree(ui, &root, &self.library.quick, &items, &mut tree);
                            }
                        }
                        drop_to = tree.drop_to;
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

        // A dragged note: a label follows the pointer, naming the group it
        // would move to. Released over another group it moves there; anywhere
        // else (but its own list) the drag just ends.
        if let Some(d) = &drag {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                let label = match &drop_to {
                    Some((_, name)) => format!("{}  {}  {name}", d.title, icon::ARROW_RIGHT),
                    None => d.title.clone(),
                };
                let ghost = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("note_drag_ghost")));
                theme::paint_toast(&ghost, p + vec2(64.0, 22.0), &label);
            }
            if ctx.input(|i| !i.pointer.any_down()) {
                if let Some((dir, _)) = drop_to {
                    picks.push(Pick::Act(Act::MoveNote(d.path.clone(), dir)));
                }
                drag = None;
            }
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
                Pick::EmptyTrash => self.lib_ui.dialog = Some(Dialog::EmptyTrash(self.library.trash.len())),
            }
        }
    }
}

/// What drawing the tree needs and collects.
struct Tree<'a> {
    current: Option<&'a Path>,
    picks: &'a mut Vec<Pick>,
    drag: &'a mut Option<NoteDrag>,
    /// The group under a note dragged out of its own group (folder, name):
    /// the note moves there when released.
    drop_to: Option<(PathBuf, String)>,
}

/// One collapsible group of notes.
struct Group<'a> {
    /// The folder of its notes.
    dir: &'a Path,
    glyph: &'a str,
    name: &'a str,
    notes: &'a [NoteInfo],
    tint: Color32,
    /// Set for a section: it can be renamed and deleted.
    section: Option<&'a SectionInfo>,
    default_open: bool,
}

/// "General" (notes directly in the notebook), one group per section, a row
/// to add a section, and the notebook's trash.
fn notebook_tree(ui: &mut Ui, nb: &NotebookInfo, trash: &[&TrashItem], t: &mut Tree) {
    let tint = Color32::from_rgb(nb.color[0], nb.color[1], nb.color[2]);
    // While a note is dragged, an empty "General" is a place to drop it.
    if !nb.loose.is_empty() || nb.sections.is_empty() || t.drag.is_some() {
        let g = Group {
            dir: &nb.path,
            glyph: icon::TRAY,
            name: "General",
            notes: &nb.loose,
            tint,
            section: None,
            default_open: true,
        };
        group(ui, &g, t);
    }
    for s in &nb.sections {
        let g = Group {
            dir: &s.path,
            glyph: icon::FOLDER_SIMPLE,
            name: &s.name,
            notes: &s.notes,
            tint,
            section: Some(s),
            default_open: false,
        };
        group(ui, &g, t);
    }
    // Quiet rows for the panel's own actions.
    //new_note_row(ui, t);
    if ghost_row(ui, icon::FOLDER_SIMPLE_PLUS, "New section").clicked() {
        let d = Dialog::NewSection { notebook: nb.path.clone(), text: String::new() };
        t.picks.push(Pick::Act(Act::Dialog(d)));
    }
    trash_group(ui, &nb.path, trash, t);
}

/// The quick notes (outside any notebook) and the library's own trash.
fn quick_tree(ui: &mut Ui, root: &Path, notes: &[NoteInfo], trash: &[&TrashItem], t: &mut Tree) {
    let g = Group {
        dir: root,
        glyph: icon::LIGHTNING,
        name: "Quick notes",
        notes,
        tint: color::TEXT_MUTED,
        section: None,
        default_open: true,
    };
    group(ui, &g, t);
    new_note_row(ui, t);
    trash_group(ui, root, trash, t);
}

/// A new note next to the open one (same notebook / section).
fn new_note_row(ui: &mut Ui, t: &mut Tree) {
    let Some(dir) = t.current.and_then(Path::parent) else { return };
    if ghost_row(ui, icon::NOTE_PENCIL, "New note").on_hover_text("Ctrl+N").clicked() {
        t.picks.push(Pick::NewNote(dir.to_path_buf()));
    }
}

/// A group: header (caret, icon, name, count; on hover "+" and, for a
/// section, "⋯"), then its notes. A note dragged in from another group drops
/// into it.
fn group(ui: &mut Ui, g: &Group, t: &mut Tree) {
    let id = Id::new(("panel_group", g.dir));
    let has_current = g.notes.iter().any(|n| Some(n.path.as_path()) == t.current);
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, g.default_open || has_current);
    let dragging = t.drag.is_some();
    let menu_id = id.with("menu");
    let menu_open = ui.memory(|m| m.is_popup_open(menu_id));

    let area = ui
        .scope(|ui| {
            let (row, header) = ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_HEIGHT), Sense::click());
            let show_actions = !dragging && (ui.rect_contains_pointer(row) || menu_open);
            let actions = if g.section.is_some() { 2 } else { 1 };
            let count = g.notes.len().to_string();
            let reserve = if show_actions { actions } else { 0 };
            paint_header(ui, row, g.glyph, g.name, &count, g.tint, state.openness(ui.ctx()), has_current, reserve);
            if header.clicked() {
                state.toggle(ui);
            }
            let header = header.on_hover_cursor(egui::CursorIcon::PointingHand);

            // The same actions on right-click and behind "⋯".
            header.context_menu(|ui| group_menu(ui, g, has_current, t.picks));
            if show_actions {
                let new = theme::row_button(ui, row, actions - 1, id.with("new"), icon::PLUS, "New note", false);
                if new.clicked() {
                    t.picks.push(Pick::NewNote(g.dir.to_path_buf()));
                }
                if g.section.is_some() {
                    let more = theme::row_button(ui, row, 0, id.with("more"), icon::DOTS_THREE, "Section actions", false);
                    if more.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(menu_id));
                    }
                    button_menu(ui, menu_id, &more, |ui| group_menu(ui, g, has_current, t.picks));
                }
            }

            state.show_body_indented(&header, ui, |ui| {
                if g.notes.is_empty() {
                    ui.add_space(2.0);
                    ui.label(theme::muted(if dragging { "Drop a note here" } else { "No notes" }).small());
                    ui.add_space(2.0);
                }
                note_list(ui, g.dir, g.notes, t);
            });
        })
        .response
        .rect;

    // Drop target for a note from another group.
    let from_elsewhere = t.drag.as_ref().is_some_and(|d| d.dir != g.dir);
    if from_elsewhere && ui.rect_contains_pointer(area) {
        ui.painter().rect(area, radius::SMALL, color::accent_alpha(24), egui::Stroke::new(1.0, color::ACCENT));
        t.drop_to = Some((g.dir.to_path_buf(), g.name.to_owned()));
    }
}

/// A group header: caret in the group's colour, icon, name (cut with "…")
/// and a count, with a hover fill. With `actions` row buttons shown, the
/// count gives way to them.
#[allow(clippy::too_many_arguments)]
fn paint_header(
    ui: &Ui,
    row: Rect,
    glyph: &str,
    name: &str,
    count: &str,
    tint: Color32,
    openness: f32,
    has_current: bool,
    actions: usize,
) {
    let hovered = ui.rect_contains_pointer(row);
    let p = ui.painter();
    if hovered {
        p.rect_filled(row, radius::SMALL, color::HOVER);
    }
    let y = row.center().y;
    let caret = if openness > 0.5 { icon::CARET_DOWN } else { icon::CARET_RIGHT };
    p.text(pos2(row.left() + 10.0, y), Align2::CENTER_CENTER, caret, theme::font(12.0), tint);
    p.text(pos2(row.left() + 22.0, y), Align2::LEFT_CENTER, glyph, theme::font(size::BODY), color::TEXT_MUTED);

    let text = if has_current || hovered { color::TEXT_STRONG } else { color::TEXT };
    let left = row.left() + 44.0;
    let count_w = if actions > 0 {
        0.0
    } else {
        ui.fonts(|f| f.layout_no_wrap(count.to_owned(), theme::font(size::CAPTION), color::TEXT_FAINT).size().x) + space::S
    };
    let right = row.right() - space::S - actions as f32 * theme::ROW_BUTTON - count_w;
    let mut job = egui::text::LayoutJob::simple_singleline(name.to_owned(), theme::font(size::BODY), text);
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - left).max(0.0));
    let galley = ui.fonts(|f| f.layout_job(job));
    let name_w = galley.size().x;
    p.galley(pos2(left, y - galley.size().y * 0.5), galley, text);
    if actions == 0 {
        let at = pos2(left + name_w + space::S, y);
        p.text(at, Align2::LEFT_CENTER, count, theme::font(size::CAPTION), color::TEXT_FAINT);
    }
}

fn group_menu(ui: &mut Ui, g: &Group, has_current: bool, picks: &mut Vec<Pick>) {
    if entry(ui, icon::NOTE_PENCIL, "New note", "", true) {
        picks.push(Pick::NewNote(g.dir.to_path_buf()));
        ui.close_menu();
    }
    let Some(s) = g.section else { return };
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
}

/// A muted full-width row with an icon, for secondary actions.
fn ghost_row(ui: &mut Ui, glyph: &str, label: &str) -> egui::Response {
    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_HEIGHT), Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    if hovered {
        p.rect_filled(row, radius::SMALL, color::HOVER);
    }
    let fg = if hovered { color::ACCENT_HOVER } else { color::TEXT_FAINT };
    let y = row.center().y;
    p.text(pos2(row.left() + 22.0, y), Align2::LEFT_CENTER, glyph, theme::font(size::BODY), fg);
    p.text(pos2(row.left() + 44.0, y), Align2::LEFT_CENTER, label, theme::font(size::BODY), fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The trash of this notebook (or of the library, for quick notes): hidden
/// when empty, closed by default. Each item can be restored or erased.
fn trash_group(ui: &mut Ui, scope: &Path, items: &[&TrashItem], t: &mut Tree) {
    if items.is_empty() {
        return;
    }
    ui.add_space(space::S);
    let id = Id::new(("panel_trash", scope));
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let (row, header) = ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_HEIGHT), Sense::click());
    let hovered = t.drag.is_none() && ui.rect_contains_pointer(row);
    let count = items.len().to_string();
    let openness = state.openness(ui.ctx());
    paint_header(ui, row, icon::TRASH, "Trash", &count, color::TEXT_FAINT, openness, false, usize::from(hovered));
    if header.clicked() {
        state.toggle(ui);
    }
    let header = header.on_hover_cursor(egui::CursorIcon::PointingHand);
    header.context_menu(|ui| {
        if entry(ui, icon::BROOM, "Empty trash...", "", true) {
            t.picks.push(Pick::EmptyTrash);
            ui.close_menu();
        }
    });
    let tip = "Empty the trash (of the whole library)";
    if hovered && theme::row_button(ui, row, 0, id.with("empty"), icon::BROOM, tip, true).clicked() {
        t.picks.push(Pick::EmptyTrash);
    }
    state.show_body_indented(&header, ui, |ui| {
        for item in items {
            trash_row(ui, item, t.picks);
        }
        ui.add_space(2.0);
        ui.label(theme::muted("Deleted notes and sections stay here until you erase them.").small());
    });
}

/// A trash item: on hover restore and erase buttons, its origin and date as
/// a tooltip; the same actions on right-click.
fn trash_row(ui: &mut Ui, item: &TrashItem, picks: &mut Vec<Pick>) {
    let glyph = if item.is_folder { icon::FOLDER_SIMPLE } else { icon::NOTE_BLANK };
    let resp = theme::list_row(ui, glyph, &item.name, false, 2.0 * theme::ROW_BUTTON);
    let id = Id::new(("trash_item", &item.path));
    let erase = || Pick::Act(Act::Dialog(Dialog::DeleteForever { path: item.path.clone(), label: item.name.clone() }));
    let restore = || Pick::Act(Act::Restore(item.path.clone()));
    if ui.rect_contains_pointer(resp.rect) {
        let tip = "Restore to where it was";
        if theme::row_button(ui, resp.rect, 1, id.with("restore"), icon::ARROW_COUNTER_CLOCKWISE, tip, false).clicked() {
            picks.push(restore());
            return;
        }
        if theme::row_button(ui, resp.rect, 0, id.with("erase"), icon::X, "Delete permanently", true).clicked() {
            picks.push(erase());
            return;
        }
    }
    let mut hint = format!("Deleted {}", when(item.deleted));
    if let Some(from) = item.origin.as_ref().and_then(|o| o.parent()).and_then(|p| p.file_name()) {
        hint = format!("From \"{}\" · {hint}", from.to_string_lossy());
    }
    resp.on_hover_text(hint).context_menu(|ui| {
        if entry(ui, icon::ARROW_COUNTER_CLOCKWISE, "Restore", "", true) {
            picks.push(restore());
            ui.close_menu();
        }
        ui.separator();
        if entry(ui, icon::X, "Delete permanently...", "", true) {
            picks.push(erase());
            ui.close_menu();
        }
    });
}

/// The notes of one folder; drag a row to reorder them, or onto another
/// group to move it there.
fn note_list(ui: &mut Ui, dir: &Path, notes: &[NoteInfo], t: &mut Tree) {
    let mut rects = Vec::with_capacity(notes.len());
    for (i, n) in notes.iter().enumerate() {
        let resp = note_row(ui, n, t.current, t.picks);
        if resp.drag_started() {
            *t.drag = Some(NoteDrag { dir: dir.to_path_buf(), from: i, title: n.meta.title.clone(), path: n.path.clone() });
        }
        rects.push(resp.rect);
    }
    let Some(d) = t.drag.as_ref().filter(|d| d.dir == dir) else { return };
    let (Some(p), Some(first), Some(last)) = (ui.input(|i| i.pointer.interact_pos()), rects.first(), rects.last()) else {
        return;
    };
    // Reordering only while the pointer is over the list itself; elsewhere
    // the note may be on its way to another group.
    let half = vec2(0.0, theme::ROW_HEIGHT * 0.5);
    if !Rect::from_min_max(first.min - half, last.max + half).contains(p) {
        return;
    }

    // Where it would land: an accent line between the rows.
    let slot = rects.iter().filter(|r| r.center().y < p.y).count();
    let y = if slot < rects.len() { rects[slot].top() - 1.0 } else { last.bottom() + 1.0 };
    let line = Rect::from_min_max(pos2(first.left(), y - 1.0), pos2(first.right(), y + 1.0));
    ui.painter().rect_filled(line, 1.0, color::ACCENT);

    if ui.input(|i| !i.pointer.any_down()) {
        let to = if slot > d.from { slot - 1 } else { slot };
        if to != d.from {
            let mut order: Vec<PathBuf> = notes.iter().map(|n| n.path.clone()).collect();
            let moved = order.remove(d.from);
            order.insert(to, moved);
            t.picks.push(Pick::Reorder(dir.to_path_buf(), order));
        }
    }
}

/// One note; the open note is highlighted. On hover a delete button (not
/// for the open note); right-click for all its actions.
fn note_row(ui: &mut Ui, note: &NoteInfo, current: Option<&Path>, picks: &mut Vec<Pick>) -> egui::Response {
    let is_current = Some(note.path.as_path()) == current;
    let actions = if is_current { 0.0 } else { theme::ROW_BUTTON };
    let resp = theme::list_row(ui, icon::NOTE_BLANK, &note.meta.title, is_current, actions);
    let trash = || Pick::Act(Act::Dialog(Dialog::Trash { path: note.path.clone(), label: note.meta.title.clone() }));
    let dragging = ui.ctx().dragged_id().is_some();
    if !is_current && !dragging && ui.rect_contains_pointer(resp.rect) {
        let id = Id::new(("note_trash", &note.path));
        if theme::row_button(ui, resp.rect, 0, id, icon::TRASH, "Delete", true).clicked() {
            picks.push(trash());
            return resp;
        }
    }
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
        if entry(ui, icon::ARROW_BEND_UP_RIGHT, "Move to...", "", true) {
            picks.push(Pick::Act(Act::Dialog(Dialog::Move { path: note.path.clone() })));
            ui.close_menu();
        }
        ui.separator();
        let resp = ui.add_enabled_ui(!is_current, |ui| entry(ui, icon::TRASH, "Delete...", "", true));
        if resp.inner {
            picks.push(trash());
            ui.close_menu();
        }
        if is_current {
            resp.response.on_hover_text("This is the open note");
        }
    });
    resp
}
