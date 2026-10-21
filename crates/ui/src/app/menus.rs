// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Menus: the title-bar menu bar (File, Edit, View, Page, Help), the canvas
//! context menu, the keyboard shortcuts for menu commands and the Help windows.
//!
//! Every entry produces a [`Command`]; commands are collected while drawing
//! (which only borrows the app immutably) and run afterwards by
//! [`InkApp::run_command`], so menus, context menu and shortcuts share one
//! implementation of each action.

use eframe::egui::{self, Align2, Button, Frame, Id, Key, KeyboardShortcut, Modifiers, Order, Pos2, RichText, Ui};
use egui_phosphor::regular as icon;
use tratto_core::history::Action;
use tratto_core::library;
use tratto_core::model::PageKind;

use super::library_ui::Dialog;
use super::{InkApp, Screen};
use crate::chrome::{self, WindowState};
use crate::settings::{LeftPanel, ToolbarPosition, ZoomMode};
use crate::theme::{self, color};
use crate::toolbar::Tool;

/// Minimum width of every menu (title bar, context menus).
const MENU_MIN_WIDTH: f32 = 200.0;

/// Zoom levels stepped through by Zoom in / Zoom out, in percent.
const ZOOM_STEPS: [u32; 15] = [25, 33, 50, 67, 75, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Command {
    NewNote,
    QuickNote,
    NewNotebook,
    Import,
    ImportPdf,
    /// A PDF to read (the reader).
    OpenPdf,
    /// The PDF in the reader becomes a note to write on.
    WriteOnPdf,
    /// Copy the selected text.
    CopyText,
    /// Select all the text of that page.
    SelectAllText(u32),
    /// The search view, its field focused.
    Find,
    /// The next (true) or previous search match.
    FindNext(bool),
    LibraryFolder,
    Save,
    ExportPdf,
    CloseNote,
    Quit,
    Undo,
    Redo,
    /// Select everything on that page.
    SelectAll(u32),
    DeleteSelection,
    Deselect,
    EditText(u64),
    DeleteText(u64),
    Tool(Tool),
    ZoomIn,
    ZoomOut,
    Zoom(ZoomMode),
    Fullscreen,
    ToolbarAt(ToolbarPosition),
    /// Global settings window.
    Settings,
    /// The open note's settings (left panel).
    NoteSettings,
    InsertPage(u32),
    DeletePage(u32),
    PaperStyle(PageKind),
    Shortcuts,
    /// Open the updates window and ask for the latest release.
    CheckUpdates,
    About,
}

/// Where the canvas context menu was opened and what is under it.
pub(super) struct ContextMenu {
    pub pos: Pos2,
    pub page: Option<u32>,
    pub text: Option<u64>,
    pub on_selection: bool,
}

/// Help windows that can be open.
#[derive(Default)]
pub(super) struct HelpWindows {
    pub shortcuts: bool,
    pub about: bool,
    pub updates: bool,
}

fn shortcut(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    ctx.format_shortcut(&KeyboardShortcut::new(modifiers, key))
}

/// One menu entry: icon, label and shortcut. Returns true when clicked.
pub(super) fn entry(ui: &mut Ui, glyph: &str, label: &str, shortcut: &str, enabled: bool) -> bool {
    // All menus share a minimum width, and widen to fit the label and the
    // shortcut instead of wrapping.
    ui.set_min_width(MENU_MIN_WIDTH);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    // Icon column of a fixed width: entries without an icon get an invisible
    // one, so all labels line up. PLACEHOLDER takes the button's text colour
    // (normal, hovered, disabled).
    let font = egui::TextStyle::Button.resolve(ui.style());
    let format = |color| egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = egui::text::LayoutJob::default();
    let (icon_glyph, icon_color) =
        if glyph.is_empty() { (icon::CHECK, egui::Color32::TRANSPARENT) } else { (glyph, egui::Color32::PLACEHOLDER) };
    job.append(icon_glyph, 0.0, format(icon_color));
    job.append(label, 10.0, format(egui::Color32::PLACEHOLDER));
    ui.add_enabled(enabled, Button::new(job).shortcut_text(shortcut)).clicked()
}

/// A menu opened by a button (a "⋯"), drawn exactly like egui's own menus:
/// a click on `button` opens or closes it, any other click closes it.
pub(super) fn button_menu(id: Id, button: &egui::Response, add_contents: impl FnOnce(&mut Ui)) {
    egui::Popup::menu(button).id(id).show(add_contents);
}

/// A checkable / radio entry (shows a check mark when `on`).
fn check(ui: &mut Ui, on: bool, label: &str, shortcut: &str) -> bool {
    let glyph = if on { icon::CHECK } else { "" };
    entry(ui, glyph, label, shortcut, true)
}

impl InkApp {
    fn window_state(&self, ctx: &egui::Context) -> WindowState {
        WindowState::of(ctx, self.settings.custom_title_bar)
    }

    fn window_title(&self) -> String {
        match self.screen {
            Screen::Editor => format!("{} - Tratto", self.note_meta.title),
            Screen::Library => "Tratto".to_owned(),
        }
    }

    /// Title bar, window frame, OS title and decorations: once per frame,
    /// before the screen's own panels.
    pub(super) fn window_chrome(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        // eframe restores a window state saved by earlier versions before the
        // first frame. A restored fullscreen is reported by winit but not
        // applied to the window, so the app would draw its fullscreen layout
        // in a normal window and the first F11 would do nothing; a restored
        // size overrides the maximized start. Start windowed and maximized.
        if !self.chrome.checked {
            self.chrome.checked = true;
            let (fullscreen, maximized) =
                ctx.input(|i| (i.viewport().fullscreen == Some(true), i.viewport().maximized == Some(true)));
            if fullscreen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            if fullscreen || !maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
        // System decorations off while the custom title bar is used. In
        // fullscreen they are on: the window has no frame there anyway, and
        // winit's undecorated window starts its client area 1 px below the
        // screen top (room for the drop shadow), so the top row would miss
        // the pointer that reveals the bar.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen == Some(true));
        let decorations = !self.settings.custom_title_bar || fullscreen;
        if self.chrome.decorations != Some(decorations) {
            self.chrome.decorations = Some(decorations);
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(decorations));
        }
        let title = self.window_title();
        if self.chrome.os_title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.chrome.os_title = title;
        }

        let mut cmds = Vec::new();
        let state = self.window_state(ctx);
        if self.screen == Screen::Editor {
            // The note title is editable in place. While the field has focus
            // it keeps what is typed; otherwise it follows the note.
            let mut buf = std::mem::take(&mut self.chrome.title_buf);
            if !self.chrome.title_focused {
                buf = self.note_meta.title.clone();
            }
            // A PDF in the reader keeps its file's name.
            let name = self.note_meta.title.clone();
            let field = if self.reading() {
                chrome::Title::Text(&name)
            } else {
                chrome::Title::Editable { text: &mut buf, hint: "Untitled note" }
            };
            let mut menu_open = false;
            // Fullscreen: keep the bar shown while a menu is open or the title is edited.
            let pinned = self.chrome.menu_open || self.chrome.title_focused;
            let bar = chrome::title_bar(ui, state, pinned, field, |ui| menu_open = self.menu_bar(ui, &mut cmds));
            self.chrome.menu_open = menu_open;
            if bar.exit_fullscreen {
                cmds.push(Command::Fullscreen);
            }
            let resp = bar.title;
            self.chrome.title_focused = resp.as_ref().is_some_and(|r| r.has_focus());
            // Enter or a click elsewhere renames; Escape keeps the old title.
            if resp.is_some_and(|r| r.lost_focus()) && !ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.rename_open_note(&buf);
            }
            self.chrome.title_buf = buf;
        } else {
            let mut menu_open = false;
            let title = chrome::Title::Text("Tratto");
            let bar = chrome::title_bar(ui, state, self.chrome.menu_open, title, |ui| {
                menu_open = self.menu_bar(ui, &mut cmds)
            });
            self.chrome.menu_open = menu_open;
            if bar.exit_fullscreen {
                cmds.push(Command::Fullscreen);
            }
        }
        self.pending.extend(cmds);
    }

    /// Renames the open note: the title in memory, then the file on disk.
    pub(super) fn rename_open_note(&mut self, title: &str) {
        let title = title.trim();
        if title.is_empty() || title == self.note_meta.title {
            return;
        }
        self.note_meta.title = title.to_owned();
        // A title chosen by the user no longer follows the first text box.
        self.note_meta.auto_title = false;
        if !self.save_note(false) {
            return;
        }
        if let Some(path) = self.note_path.clone() {
            match library::rename_note_file(&path, title) {
                Ok(new) => {
                    self.settings.moved(&path, &new);
                    self.note_path = Some(new);
                }
                Err(e) => self.toast(format!("Renamed, but the file keeps its name: {e}")),
            }
        }
        self.refresh_library();
    }

    /// Runs the queued commands, draws the help windows and the resize
    /// borders: once per frame, after the screen.
    pub(super) fn finish_chrome(&mut self, ctx: &egui::Context) {
        self.poll_export();
        self.help_windows(ctx);
        self.updates_window(ctx);
        self.global_settings_window(ctx);
        chrome::window_frame(ctx, self.window_state(ctx));
        for cmd in std::mem::take(&mut self.pending) {
            self.run_command(ctx, cmd);
        }
    }

    // ------------------------------------------------------------ menu bar

    /// The menus of the title bar; returns whether one of them is open.
    /// (egui's `menu_button` returns `None` while a menu stays open, so the
    /// contents, which only run while a menu is shown, set the flag.)
    fn menu_bar(&self, ui: &mut Ui, cmds: &mut Vec<Command>) -> bool {
        let mut open = false;
        let ctx = ui.ctx().clone();
        let editor = self.screen == Screen::Editor;
        let cmd = Modifiers::COMMAND;
        let mut push = |clicked: bool, ui: &mut Ui, c: Command| {
            if clicked {
                cmds.push(c);
                ui.close();
            }
        };

        ui.menu_button("File", |ui| {
            open = true;
            if self.standalone.is_some() {
                self.reader_file_menu(ui, &mut |ui, c| push(true, ui, c));
                return;
            }
            push(entry(ui, icon::NOTE_PENCIL, "New note", &shortcut(&ctx, cmd, Key::N), true), ui, Command::NewNote);
            push(
                entry(ui, icon::LIGHTNING, "Quick note", &shortcut(&ctx, cmd | Modifiers::SHIFT, Key::N), true),
                ui,
                Command::QuickNote,
            );
            push(entry(ui, icon::NOTEBOOK, "New notebook...", "", true), ui, Command::NewNotebook);
            push(entry(ui, icon::DOWNLOAD_SIMPLE, "Import note file...", "", true), ui, Command::Import);
            push(entry(ui, icon::BOOK_OPEN, "Open PDF...", &shortcut(&ctx, cmd, Key::O), true), ui, Command::OpenPdf);
            push(entry(ui, icon::FILE_ARROW_DOWN, "Write on a PDF...", "", true), ui, Command::ImportPdf);
            ui.separator();
            // A PDF in the reader is not a note: nothing to save or export.
            let note = editor && !self.reading();
            push(entry(ui, icon::FLOPPY_DISK, "Save", &shortcut(&ctx, cmd, Key::S), note), ui, Command::Save);
            push(
                entry(ui, icon::FILE_PDF, "Export as PDF...", &shortcut(&ctx, cmd, Key::E), note),
                ui,
                Command::ExportPdf,
            );
            let close = if self.reading() { "Close PDF" } else { "Close note" };
            push(entry(ui, icon::BOOKS, close, &shortcut(&ctx, cmd, Key::W), editor), ui, Command::CloseNote);
            ui.separator();
            push(entry(ui, icon::FOLDER_OPEN, "Library folder...", "", true), ui, Command::LibraryFolder);
            push(
                entry(ui, icon::GEAR_SIX, "Settings...", &shortcut(&ctx, cmd, Key::Comma), true),
                ui,
                Command::Settings,
            );
            ui.separator();
            push(entry(ui, icon::SIGN_OUT, "Exit", &shortcut(&ctx, cmd, Key::Q), true), ui, Command::Quit);
        });

        // The reader changes nothing: its Edit menu only copies and finds,
        // and there is no Page menu.
        if editor {
            ui.menu_button("Edit", |ui| {
                open = true;
                self.edit_entries(ui, &mut |ui, c| push(true, ui, c));
            });
        }

        ui.menu_button("View", |ui| {
            open = true;
            if editor {
                push(
                    entry(ui, icon::MAGNIFYING_GLASS_PLUS, "Zoom in", &shortcut(&ctx, cmd, Key::Plus), true),
                    ui,
                    Command::ZoomIn,
                );
                push(
                    entry(ui, icon::MAGNIFYING_GLASS_MINUS, "Zoom out", &shortcut(&ctx, cmd, Key::Minus), true),
                    ui,
                    Command::ZoomOut,
                );
                let zoom = self.settings.zoom;
                push(
                    check(ui, zoom == ZoomMode::Percent(100), "Actual size", &shortcut(&ctx, cmd, Key::Num0)),
                    ui,
                    Command::Zoom(ZoomMode::Percent(100)),
                );
                push(check(ui, zoom == ZoomMode::FitWidth, "Fit width", ""), ui, Command::Zoom(ZoomMode::FitWidth));
                push(check(ui, zoom == ZoomMode::FitPage, "Fit page", ""), ui, Command::Zoom(ZoomMode::FitPage));
                ui.separator();
                let pos = self.settings.toolbar_position;
                push(
                    check(ui, pos == ToolbarPosition::Top, "Toolbar at top", ""),
                    ui,
                    Command::ToolbarAt(ToolbarPosition::Top),
                );
                push(
                    check(ui, pos == ToolbarPosition::Bottom, "Toolbar at bottom", ""),
                    ui,
                    Command::ToolbarAt(ToolbarPosition::Bottom),
                );
                ui.separator();
            }
            push(
                entry(ui, icon::CORNERS_OUT, "Full screen", &shortcut(&ctx, Modifiers::NONE, Key::F11), true),
                ui,
                Command::Fullscreen,
            );
        });

        if editor && !self.reading() {
            ui.menu_button("Page", |ui| {
                open = true;
                self.page_entries(ui, self.current_page, &mut |ui, c| push(true, ui, c));
                ui.separator();
                let kind = self.doc.page.kind;
                for (k, label) in
                    [(PageKind::Blank, "Blank paper"), (PageKind::Ruled, "Ruled paper"), (PageKind::Grid, "Grid paper")]
                {
                    push(check(ui, kind == k, label, ""), ui, Command::PaperStyle(k));
                }
                ui.separator();
                push(entry(ui, icon::GEAR_SIX, "Note settings...", "", true), ui, Command::NoteSettings);
            });
        }

        ui.menu_button("Help", |ui| {
            open = true;
            push(entry(ui, icon::KEYBOARD, "Keyboard shortcuts", "", true), ui, Command::Shortcuts);
            push(entry(ui, icon::DOWNLOAD_SIMPLE, "Check for updates...", "", true), ui, Command::CheckUpdates);
            ui.separator();
            push(entry(ui, icon::INFO, "About Tratto", "", true), ui, Command::About);
        });
        open
    }

    /// The File menu of a reader-only window: other PDFs, writing on this
    /// one (the window becomes the full app), closing.
    fn reader_file_menu(&self, ui: &mut Ui, push: &mut dyn FnMut(&mut Ui, Command)) {
        let ctx = ui.ctx().clone();
        let cmd = Modifiers::COMMAND;
        if entry(ui, icon::BOOK_OPEN, "Open PDF...", &shortcut(&ctx, cmd, Key::O), true) {
            push(ui, Command::OpenPdf);
        }
        if entry(ui, icon::FILE_ARROW_DOWN, "Write on this PDF", "", self.reading()) {
            push(ui, Command::WriteOnPdf);
        }
        ui.separator();
        if entry(ui, icon::GEAR_SIX, "Settings...", &shortcut(&ctx, cmd, Key::Comma), true) {
            push(ui, Command::Settings);
        }
        ui.separator();
        if entry(ui, icon::X, "Close", &shortcut(&ctx, cmd, Key::W), true) {
            push(ui, Command::Quit);
        }
    }

    /// Undo / redo, the selection entries and the text ones (Edit menu).
    fn edit_entries(&self, ui: &mut Ui, push: &mut dyn FnMut(&mut Ui, Command)) {
        let ctx = ui.ctx().clone();
        let cmd = Modifiers::COMMAND;
        if !self.reading() {
            self.ink_entries(ui, push);
            ui.separator();
        }
        self.text_entries(ui, self.current_page, push);
        ui.separator();
        if entry(ui, icon::MAGNIFYING_GLASS, "Find...", &shortcut(&ctx, cmd, Key::F), true) {
            push(ui, Command::Find);
        }
    }

    /// Copying the selected text and selecting all of `page`'s (Edit menu and
    /// context menu).
    fn text_entries(&self, ui: &mut Ui, page: u32, push: &mut dyn FnMut(&mut Ui, Command)) {
        let ctx = ui.ctx().clone();
        let cmd = Modifiers::COMMAND;
        if entry(ui, icon::COPY, "Copy", &shortcut(&ctx, cmd, Key::C), self.text_sel.is_some()) {
            push(ui, Command::CopyText);
        }
        let all = if self.reading() { shortcut(&ctx, cmd, Key::A) } else { String::new() };
        if entry(ui, icon::TEXT_AA, "Select all text on page", &all, true) {
            push(ui, Command::SelectAllText(page));
        }
    }

    /// Undo / redo and the ink selection entries.
    fn ink_entries(&self, ui: &mut Ui, push: &mut dyn FnMut(&mut Ui, Command)) {
        let ctx = ui.ctx().clone();
        let cmd = Modifiers::COMMAND;
        if entry(ui, icon::ARROW_U_UP_LEFT, "Undo", &shortcut(&ctx, cmd, Key::Z), self.history.can_undo()) {
            push(ui, Command::Undo);
        }
        if entry(ui, icon::ARROW_U_UP_RIGHT, "Redo", &shortcut(&ctx, cmd, Key::Y), self.history.can_redo()) {
            push(ui, Command::Redo);
        }
        ui.separator();
        if entry(ui, icon::SELECTION_ALL, "Select all on page", &shortcut(&ctx, cmd, Key::A), true) {
            push(ui, Command::SelectAll(self.current_page));
        }
        let selected = self.selection.is_some();
        if entry(ui, icon::TRASH, "Delete selection", &shortcut(&ctx, Modifiers::NONE, Key::Delete), selected) {
            push(ui, Command::DeleteSelection);
        }
        if entry(ui, icon::SELECTION_SLASH, "Deselect", &shortcut(&ctx, Modifiers::NONE, Key::Escape), selected) {
            push(ui, Command::Deselect);
        }
    }

    /// Insert / delete entries for `page` (Page menu and context menu).
    fn page_entries(&self, ui: &mut Ui, page: u32, push: &mut dyn FnMut(&mut Ui, Command)) {
        if entry(ui, icon::ARROW_LINE_UP, "Insert page before", "", true) {
            push(ui, Command::InsertPage(page));
        }
        if entry(ui, icon::ARROW_LINE_DOWN, "Insert page after", "", true) {
            push(ui, Command::InsertPage(page + 1));
        }
        if entry(ui, icon::TRASH, &format!("Delete page {}", page + 1), "", true) {
            push(ui, Command::DeletePage(page));
        }
    }

    // -------------------------------------------------------- context menu

    /// Draws the canvas context menu, if open. Call before the canvas: a
    /// press outside closes the menu and is not passed on to the canvas.
    pub(super) fn context_menu_ui(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.context_menu.as_ref() else { return };
        let mut cmds = Vec::new();
        let mut push = |ui: &mut Ui, c: Command| {
            cmds.push(c);
            ui.close();
        };
        let area = egui::Area::new(Id::new("canvas_context_menu")).order(Order::Foreground).fixed_pos(menu.pos).show(
            ctx,
            |ui| {
                Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    // Flat rows, like egui's own menus.
                    ui.visuals_mut().button_frame = false;
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    // The reader: only its text; a note: copying a selection first.
                    if self.reading() {
                        self.text_entries(ui, menu.page.unwrap_or(self.current_page), &mut push);
                        return;
                    }
                    if self.text_sel.is_some() {
                        let ctx = ui.ctx().clone();
                        if entry(ui, icon::COPY, "Copy", &shortcut(&ctx, Modifiers::COMMAND, Key::C), true) {
                            push(ui, Command::CopyText);
                        }
                        ui.separator();
                    }
                    if let Some(id) = menu.text {
                        if entry(ui, icon::TEXT_T, "Edit text", "", true) {
                            push(ui, Command::EditText(id));
                        }
                        if entry(ui, icon::TRASH, "Delete text box", "", true) {
                            push(ui, Command::DeleteText(id));
                        }
                        ui.separator();
                    }
                    if menu.on_selection {
                        let ctx = ui.ctx().clone();
                        if entry(
                            ui,
                            icon::TRASH,
                            "Delete selection",
                            &shortcut(&ctx, Modifiers::NONE, Key::Delete),
                            true,
                        ) {
                            push(ui, Command::DeleteSelection);
                        }
                        if entry(
                            ui,
                            icon::SELECTION_SLASH,
                            "Deselect",
                            &shortcut(&ctx, Modifiers::NONE, Key::Escape),
                            true,
                        ) {
                            push(ui, Command::Deselect);
                        }
                        ui.separator();
                    }
                    if entry(ui, icon::ARROW_U_UP_LEFT, "Undo", "", self.history.can_undo()) {
                        push(ui, Command::Undo);
                    }
                    if entry(ui, icon::ARROW_U_UP_RIGHT, "Redo", "", self.history.can_redo()) {
                        push(ui, Command::Redo);
                    }
                    ui.separator();
                    for t in Tool::ALL {
                        let glyph = if self.tool == t { icon::CHECK } else { t.icon() };
                        if entry(ui, glyph, t.label(), "", true) {
                            push(ui, Command::Tool(t));
                        }
                    }
                    if let Some(page) = menu.page {
                        ui.separator();
                        if entry(ui, icon::SELECTION_ALL, "Select all on page", "", true) {
                            push(ui, Command::SelectAll(page));
                        }
                        if entry(ui, icon::TEXT_AA, "Select all text on page", "", true) {
                            push(ui, Command::SelectAllText(page));
                        }
                        self.page_entries(ui, page, &mut push);
                    }
                });
            },
        );

        let rect = area.response.rect;
        let (pressed_outside, escape) = ctx.input(|i| {
            (
                i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| !rect.contains(p)),
                i.key_pressed(Key::Escape),
            )
        });
        if !cmds.is_empty() || pressed_outside || escape {
            self.context_menu = None;
            self.swallow_press = pressed_outside;
        }
        self.pending.extend(cmds);
    }

    // ----------------------------------------------------------- shortcuts

    /// Shortcuts of menu commands that are not handled by the editor itself.
    pub(super) fn menu_shortcuts(&mut self, ctx: &egui::Context) {
        let editor = self.screen == Screen::Editor;
        let standalone = self.standalone.is_some();
        let typing = self.edit.is_some() || ctx.egui_wants_keyboard_input();
        let cmd = Modifiers::COMMAND;
        let found: Vec<Command> = ctx.input_mut(|i| {
            let mut out = Vec::new();
            if i.consume_key(Modifiers::NONE, Key::F11) {
                out.push(Command::Fullscreen);
            }
            if i.consume_key(cmd, Key::Q) {
                out.push(Command::Quit);
            }
            // Before Ctrl+N, which would also match Ctrl+Shift+N. A
            // reader-only window makes no notes.
            if !standalone {
                if i.consume_key(cmd | Modifiers::SHIFT, Key::N) {
                    out.push(Command::QuickNote);
                } else if i.consume_key(cmd, Key::N) {
                    out.push(Command::NewNote);
                }
            }
            if i.consume_key(cmd, Key::Comma) {
                out.push(Command::Settings);
            }
            if i.consume_key(cmd, Key::O) {
                out.push(Command::OpenPdf);
            }
            if editor {
                if i.consume_key(cmd, Key::W) {
                    out.push(Command::CloseNote);
                }
                if i.consume_key(cmd, Key::F) {
                    out.push(Command::Find);
                }
                if i.consume_key(Modifiers::SHIFT, Key::F3) {
                    out.push(Command::FindNext(false));
                } else if i.consume_key(Modifiers::NONE, Key::F3) {
                    out.push(Command::FindNext(true));
                }
                if !typing {
                    // The reader selects text; a note, its ink.
                    if i.consume_key(cmd, Key::A) {
                        out.push(if self.reading() {
                            Command::SelectAllText(self.current_page)
                        } else {
                            Command::SelectAll(self.current_page)
                        });
                    }
                    // egui turns Ctrl+C into a copy event.
                    let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy)) || i.consume_key(cmd, Key::C);
                    if copy && self.text_sel.is_some() {
                        out.push(Command::CopyText);
                    }
                    if self.text_sel.is_some()
                        && self.context_menu.is_none()
                        && i.consume_key(Modifiers::NONE, Key::Escape)
                    {
                        out.push(Command::Deselect);
                    }
                    if i.consume_key(cmd, Key::Plus) || i.consume_key(cmd, Key::Equals) {
                        out.push(Command::ZoomIn);
                    }
                    if i.consume_key(cmd, Key::Minus) {
                        out.push(Command::ZoomOut);
                    }
                    if i.consume_key(cmd, Key::Num0) {
                        out.push(Command::Zoom(ZoomMode::Percent(100)));
                    }
                }
            }
            out
        });
        self.pending.extend(found);
    }

    // ------------------------------------------------------------ commands

    pub(super) fn run_command(&mut self, ctx: &egui::Context, cmd: Command) {
        let editor = self.screen == Screen::Editor;
        match cmd {
            // A reader-only window: other PDFs open in it, closing closes it,
            // and nothing of the library is there.
            Command::OpenPdf if self.standalone.is_some() => self.show_dialog(Dialog::OpenPdf(String::new())),
            Command::CloseNote if self.standalone.is_some() => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::NewNote
            | Command::QuickNote
            | Command::NewNotebook
            | Command::Import
            | Command::ImportPdf
            | Command::LibraryFolder
                if self.standalone.is_some() => {}
            Command::WriteOnPdf => self.import_open_pdf(),
            Command::CopyText => self.copy_text(ctx),
            Command::SelectAllText(page) if editor => self.select_all_text(page),
            Command::Find if editor => self.find(),
            Command::FindNext(forward) if editor => self.find_next(forward),
            Command::NewNote => {
                let dir = match (&self.note_path, editor) {
                    (Some(p), true) => p.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| self.library_root()),
                    _ => self.library_target_dir(),
                };
                if editor {
                    self.close_note(ctx);
                }
                if self.screen == Screen::Library {
                    self.new_note(&dir);
                }
            }
            Command::QuickNote => {
                if editor {
                    self.close_note(ctx);
                }
                if self.screen == Screen::Library {
                    let root = self.library_root();
                    self.new_note(&root);
                }
            }
            Command::NewNotebook | Command::Import | Command::ImportPdf | Command::OpenPdf | Command::LibraryFolder => {
                if editor {
                    self.close_note(ctx);
                }
                if self.screen == Screen::Library {
                    self.show_dialog(match cmd {
                        Command::NewNotebook => Dialog::NewNotebook(String::new()),
                        Command::Import => Dialog::Import(String::new()),
                        Command::ImportPdf => Dialog::ImportPdf(String::new()),
                        Command::OpenPdf => Dialog::OpenPdf(String::new()),
                        _ => Dialog::Folder(self.settings.library_root.clone()),
                    });
                }
            }
            Command::Save if editor => {
                self.save_note(true);
            }
            Command::ExportPdf if editor && !self.reading() => self.open_export_dialog(),
            Command::CloseNote if editor => self.close_note(ctx),
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::SelectAll(page) if editor => self.select_all_on_page(page),
            Command::DeleteSelection => self.delete_selection(),
            Command::Deselect => {
                self.clear_selection();
                self.text_sel = None;
            }
            Command::EditText(id) => {
                if self.edit.is_some() {
                    self.commit_edit(ctx);
                }
                self.tool = Tool::Text;
                self.begin_edit(id);
            }
            Command::DeleteText(id) => {
                if self.edit.as_ref().is_some_and(|e| e.id == id) {
                    self.edit = None;
                }
                self.history.exec(&mut self.doc, Action::RemoveText { id });
            }
            Command::Tool(t) => self.pick_tool(t),
            Command::ZoomIn | Command::ZoomOut => {
                let now = (self.view_scale * 100.0).round() as u32;
                let next = if cmd == Command::ZoomIn {
                    ZOOM_STEPS.iter().copied().find(|&s| s > now).unwrap_or(400)
                } else {
                    ZOOM_STEPS.iter().rev().copied().find(|&s| s < now).unwrap_or(25)
                };
                self.zoom_request = Some(next as f32 / now.max(1) as f32);
            }
            Command::Zoom(z) => self.settings.zoom = z,
            Command::Fullscreen => {
                // On Windows, going fullscreen straight from a maximized window
                // resizes the window without a resize event, so egui keeps
                // drawing at the maximized size. Un-maximize first and
                // maximize again after leaving fullscreen.
                let (fs, maximized) =
                    ctx.input(|i| (i.viewport().fullscreen.unwrap_or(false), i.viewport().maximized.unwrap_or(false)));
                if fs {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    if std::mem::take(&mut self.chrome.maximize_after_fullscreen) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                    }
                } else {
                    self.chrome.maximize_after_fullscreen = maximized;
                    if maximized {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                }
            }
            Command::ToolbarAt(p) => self.settings.toolbar_position = p,
            Command::Settings => self.global_settings_open = true,
            Command::NoteSettings if editor => self.settings.left_panel = LeftPanel::Settings,
            Command::InsertPage(i) if editor => self.insert_page(i),
            Command::DeletePage(i) if editor => self.delete_page(i),
            Command::PaperStyle(k) if editor => self.doc.page.kind = k,
            Command::Shortcuts => self.help.shortcuts = true,
            Command::CheckUpdates => self.check_for_updates(ctx),
            Command::About => self.help.about = true,
            // Editor commands that arrive after the note was closed.
            _ => {}
        }
    }

    // --------------------------------------------------------- help windows

    fn help_windows(&mut self, ctx: &egui::Context) {
        let mut open = self.help.about;
        egui::Window::new("About Tratto")
            .open(&mut open)
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(icon::PEN_NIB).size(40.0).color(color::ACCENT));
                    ui.label(theme::title("Tratto"));
                    ui.label(theme::muted(format!("Version {}", crate::update::CURRENT)));
                    ui.add_space(10.0);
                    ui.label(env!("CARGO_PKG_DESCRIPTION"));
                    ui.add_space(20.0);
                    ui.label("Copyright © 2026 Christian Braghette");
                    ui.add_space(10.0);
                    ui.label(RichText::new("Free software under the GNU AGPL v3 or later.").small().color(color::TEXT_MUTED));
                    ui.label(RichText::new("Commercial licenses are also available.").small().color(color::TEXT_MUTED));
                    ui.add_space(5.0);
                    ui.hyperlink_to("Source code and license", env!("CARGO_PKG_REPOSITORY"));
                    ui.add_space(15.0);
                    ui.label(
                        RichText::new("Font: Inter (OFL) · Icons: Phosphor (MIT)").small().color(color::TEXT_FAINT),
                    );
                });
            });
        self.help.about = open;

        let mut open = self.help.shortcuts;
        egui::Window::new("Keyboard shortcuts")
            .open(&mut open)
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let c = Modifiers::COMMAND;
                let none = Modifiers::NONE;
                let rows = [
                    (shortcut(ctx, c, Key::N), "New note"),
                    (shortcut(ctx, c, Key::S), "Save now"),
                    (shortcut(ctx, c, Key::E), "Export as PDF"),
                    (shortcut(ctx, c, Key::W), "Close note"),
                    (shortcut(ctx, c, Key::Z), "Undo"),
                    (format!("{} / {}", shortcut(ctx, c, Key::Y), shortcut(ctx, c | Modifiers::SHIFT, Key::Z)), "Redo"),
                    (shortcut(ctx, c, Key::A), "Select all on the current page"),
                    (shortcut(ctx, none, Key::Delete), "Delete the selection"),
                    (shortcut(ctx, none, Key::Escape), "Deselect"),
                    (format!("{} / {}", shortcut(ctx, c, Key::Plus), shortcut(ctx, c, Key::Minus)), "Zoom in / out"),
                    (shortcut(ctx, c, Key::Num0), "Actual size"),
                    ("Ctrl + wheel, pinch".to_owned(), "Zoom around the pointer"),
                    ("Page Up / Page Down".to_owned(), "Scroll by one screen (a page in the PDF reader)"),
                    (format!("{} / {}", shortcut(ctx, c, Key::B), shortcut(ctx, c, Key::I)), "Text box: bold / italic"),
                    (shortcut(ctx, c, Key::K), "Text box: link"),
                    ("Tab / Shift + Tab".to_owned(), "Text box: nest / unnest a list item"),
                    ("Enter".to_owned(), "Text box: next list item (on an empty one: end the list)"),
                    (shortcut(ctx, c, Key::O), "Open a PDF to read"),
                    (shortcut(ctx, c, Key::C), "Copy the selected text"),
                    (shortcut(ctx, c, Key::F), "Search the text"),
                    (
                        format!("{} / {}", shortcut(ctx, none, Key::F3), shortcut(ctx, Modifiers::SHIFT, Key::F3)),
                        "Next / previous match",
                    ),
                    ("Left / Right, Space".to_owned(), "PDF reader: previous / next page"),
                    ("Alt + Left, Backspace".to_owned(), "PDF reader: back after following a link"),
                    ("Home / End".to_owned(), "First / last page"),
                    (shortcut(ctx, none, Key::F11), "Full screen"),
                    ("Right click".to_owned(), "Context menu"),
                    (shortcut(ctx, c, Key::Comma), "Settings"),
                    (shortcut(ctx, c, Key::Q), "Exit"),
                ];
                egui::Grid::new("shortcuts_grid").num_columns(2).spacing([24.0, 6.0]).striped(true).show(ui, |ui| {
                    for (keys, what) in rows {
                        ui.monospace(keys);
                        ui.label(what);
                        ui.end_row();
                    }
                });
            });
        self.help.shortcuts = open;
    }
}
