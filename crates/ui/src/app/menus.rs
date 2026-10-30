// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Menus: the title-bar menu bar (File, Edit, View, Page, Help), the canvas
//! context menu, the keyboard shortcuts for menu commands and the Help windows.
//!
//! Every entry produces a [`Command`]; commands are collected while drawing
//! (which only borrows the app immutably) and run afterwards by
//! [`InkApp::run_command`], so menus, context menu and shortcuts share one
//! implementation of each action.

use eframe::egui::{
    self, Align2, Button, Color32, Frame, Id, Key, KeyboardShortcut, Modifiers, Order, Pos2, RichText, Ui,
};
use egui_phosphor::regular as icon;
use tratto_core::history::Action;
use tratto_core::library;
use tratto_core::model::PageKind;

use super::library_ui::Dialog;
use super::search_panel::Scope;
use super::{InkApp, Screen};
use crate::chrome::{self, WindowState};
use crate::settings::{Sidebar, SidebarSide, ToolbarPosition, ZoomMode};
use crate::theme::{self, color};
use crate::toolbar::Tool;

/// Ids of the windows Escape closes (see `escape_closes_window`).
pub(super) const SETTINGS_WINDOW: &str = "settings_window";
pub(super) const UPDATES_WINDOW: &str = "updates_window";
pub(super) const SHORTCUTS_WINDOW: &str = "shortcuts_window";
pub(super) const ABOUT_WINDOW: &str = "about_window";
pub(super) const EXPORT_WINDOW: &str = "export_window";
const ESCAPE_WINDOWS: [&str; 5] = [EXPORT_WINDOW, UPDATES_WINDOW, SHORTCUTS_WINDOW, ABOUT_WINDOW, SETTINGS_WINDOW];
/// Id of the library dialogs (rename, new notebook...), which close
/// themselves on Escape.
pub(super) const LIBRARY_DIALOG: &str = "library_dialog";

/// Minimum width of every menu (title bar, context menus).
const MENU_MIN_WIDTH: f32 = 200.0;

/// Zoom levels stepped through by Zoom in / Zoom out, in percent (those the
/// view can have, see `Layout::zoom_range`).
const ZOOM_STEPS: [u32; 17] = [10, 15, 25, 33, 50, 67, 75, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400];

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
    /// Copy the selected text, else what is selected on the note.
    Copy,
    /// Cut what is selected on the note.
    Cut,
    /// Paste at a page and a point on it (or where the view is): what was
    /// copied in Tratto, else another app's picture or text.
    Paste(Option<(u32, Pos2)>),
    CopyPage(u32),
    CutPage(u32),
    /// Save a picture of that page as a PNG file.
    SavePagePng(u32),
    /// Save a picture of what is selected as a PNG file.
    SaveSelectionPng,
    /// Insert the copied pages so that the first gets this index.
    PastePages(u32),
    /// Paste the copied (or cut) note into the library's open notebook or
    /// section.
    PasteNote,
    /// Select all the text of that page.
    SelectAllText(u32),
    /// The search view on the note, its field focused.
    Find,
    /// The search view on the whole notebook, its field focused.
    FindInNotebook,
    /// The next (true) or previous search match.
    FindNext(bool),
    LibraryFolder,
    Save,
    ExportPdf,
    Print,
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
    /// A picture file, chosen in a dialog, at the same.
    InsertPicture(Option<(u32, Pos2)>),
    DeletePicture(u64),
    ZoomIn,
    ZoomOut,
    Zoom(ZoomMode),
    Fullscreen,
    /// Start or end presentation mode.
    Present,
    /// Turn the laser pointer on or off.
    Laser,
    ToolbarAt(ToolbarPosition),
    SidebarAt(SidebarSide),
    /// Global settings window.
    Settings,
    /// The open note's settings (sidebar).
    NoteSettings,
    InsertPage(u32),
    DeletePage(u32),
    PaperStyle(PageKind),
    Shortcuts,
    /// Open the updates window and ask for the latest release.
    CheckUpdates,
    About,
}

/// Presenting, in and out, through a fade: the window fades to black,
/// switches while covered, then fades back in. The switch (the window
/// changing size) is never seen.
pub(super) struct Fade {
    /// The switch, run once the window is covered.
    command: Command,
    phase: FadePhase,
}

#[derive(Clone, Copy)]
enum FadePhase {
    /// Fading out, since (s).
    Out(f64),
    /// Covered, the window switching: to full screen or not, since (s) and
    /// the frame it started in.
    Switching(bool, f64, u64),
    /// Fading back in, since (s).
    In(f64),
}

/// Fade out and in times (s), and how many frames the window is drawn
/// covered once switched (so the first ones at its new size are not seen).
const FADE_OUT: f64 = 0.12;
const FADE_IN: f64 = 0.18;
const FADE_SETTLE_FRAMES: u64 = 3;
/// A switch that does not happen in this long (s) is faded in anyway.
const FADE_TIMEOUT: f64 = 1.5;

/// Going in or out of full screen with the window growing to the screen
/// or shrinking back to its place. The window itself switches at once
/// (coming out, once shrunk): what grows and shrinks is everything drawn
/// in it, scaled, over black.
#[derive(Clone, Copy)]
pub(super) enum Grow {
    /// Switched in: drawn at the window's old place until it is full
    /// screen, since (s).
    Entering(f64),
    /// Growing to the screen, since (s).
    Growing(f64),
    /// Shrinking to the window's place, since (s); switches out when done.
    Shrinking(f64),
    /// Switched out: drawn at the window's place until it is there, since (s).
    Leaving(f64),
}

/// Time (s) the window takes to grow or shrink.
const GROW_TIME: f64 = 0.25;

/// Where the canvas context menu was opened and what is under it.
pub(super) struct ContextMenu {
    pub pos: Pos2,
    pub page: Option<u32>,
    pub text: Option<u64>,
    pub on_selection: bool,
    /// The picture under it.
    pub picture: Option<u64>,
    /// The page and the point on it.
    pub at: Option<(u32, Pos2)>,
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

/// Width of the keyboard shortcuts window's list, and of its keys' column.
/// Both are fixed: keys wrap in their column and descriptions in the rest,
/// so a long row never widens the window past its title bar.
const SHORTCUTS_WIDTH: f32 = 460.0;
const SHORTCUT_KEYS_WIDTH: f32 = 180.0;

/// A key, or keys pressed together, drawn as a key cap.
fn keycap(ui: &mut Ui, keys: &str) {
    Frame::new()
        .fill(color::surface())
        .stroke(egui::Stroke::new(1.0, color::border()))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| ui.label(RichText::new(keys).monospace().size(11.5).color(color::text())));
}

/// Copying, cutting and pasting whole pages, by `page`, and a picture of
/// it (Page menu, context menu, pages panel). `enabled`: they can be;
/// `paste`: pages were copied.
pub(super) fn page_clipboard_entries(
    ui: &mut Ui,
    page: u32,
    (enabled, paste): (bool, bool),
    push: &mut dyn FnMut(&mut Ui, Command),
) {
    if entry(ui, icon::COPY, "Copy page", "", enabled) {
        push(ui, Command::CopyPage(page));
    }
    if entry(ui, icon::SCISSORS, "Cut page", "", enabled) {
        push(ui, Command::CutPage(page));
    }
    if entry(ui, icon::CLIPBOARD, "Paste page", "", enabled && paste) {
        push(ui, Command::PastePages(page + 1));
    }
    if entry(ui, icon::FILE_PNG, "Save page as PNG...", "", enabled) {
        push(ui, Command::SavePagePng(page));
    }
}

/// Inserting a page before or after `page` and deleting it (Page menu,
/// context menu, pages panel); `delete`: it can be deleted.
pub(super) fn page_insert_entries(ui: &mut Ui, page: u32, delete: bool, push: &mut dyn FnMut(&mut Ui, Command)) {
    if entry(ui, icon::ARROW_LINE_UP, "Insert page before", "", true) {
        push(ui, Command::InsertPage(page));
    }
    if entry(ui, icon::ARROW_LINE_DOWN, "Insert page after", "", true) {
        push(ui, Command::InsertPage(page + 1));
    }
    if entry(ui, icon::TRASH, "Delete page", "", delete) {
        push(ui, Command::DeletePage(page));
    }
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
        let fullscreen = ctx.input(|i| i.viewport().fullscreen == Some(true));
        // The window's animations back on once a switch in or out of full
        // screen is done (and has had a moment to settle), or after a while.
        if let Some((want_fullscreen, want_maximized, since)) = self.chrome.switching {
            let (maximized, now) = ctx.input(|i| (i.viewport().maximized == Some(true), i.time));
            let done = fullscreen == want_fullscreen && (maximized || !want_maximized);
            // Maximized again: its own size un-maximized back (see
            // `Command::Fullscreen`).
            if done
                && maximized
                && let Some(rect) = self.chrome.normal_rect.take()
            {
                chrome::set_normal_rect(rect);
            }
            if done && now - since > 0.5 || now - since > 3.0 {
                chrome::window_transitions(true);
                crate::win_frame::switching(false);
                self.chrome.switching = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        // Windows: the window keeps the system's frame, only its caption goes
        // (`win_frame`). Elsewhere the system decorations are off while the
        // custom title bar is used. In fullscreen they are on: the window has
        // no frame there anyway, and winit's undecorated window starts its
        // client area 1 px below the screen top (room for the drop shadow),
        // so the top row would miss the pointer that reveals the bar.
        crate::win_frame::own_frame(self.settings.custom_title_bar);
        // Leaving full screen they are off already (see `Command::Fullscreen`).
        let leaving = matches!(self.chrome.switching, Some((false, ..)));
        let decorations = cfg!(windows) || !self.settings.custom_title_bar || fullscreen && !leaving;
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
        if self.presenting.is_some() {
            // Presenting: no title bar. The note was closed meanwhile: done.
            if self.screen != Screen::Editor {
                cmds.push(Command::Present);
            }
        } else if self.screen == Screen::Editor {
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
            if bar.toggle_fullscreen {
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
            if bar.toggle_fullscreen {
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
        self.poll_png();
        self.help_windows(ctx);
        self.updates_window(ctx);
        self.global_settings_window(ctx);
        chrome::window_frame(ctx, self.window_state(ctx));
        for cmd in std::mem::take(&mut self.pending) {
            self.run_command(ctx, cmd);
        }
        self.fade_step(ctx);
        self.grow_step(ctx);
    }

    /// Going in or out of full screen by itself (not presenting): starts
    /// the window growing, or shrinking. Returns whether the switch waits
    /// for it (coming out, or with one already running).
    fn grow_first(&mut self, ctx: &egui::Context) -> bool {
        let (fullscreen, inner, now) =
            ctx.input(|i| (i.viewport().fullscreen.unwrap_or(false), i.viewport().inner_rect, i.time));
        if !fullscreen {
            self.chrome.windowed = inner;
        }
        if self.chrome.fade_running || self.chrome.windowed.is_none() {
            return false;
        }
        if self.chrome.grow.is_some() {
            return true;
        }
        ctx.request_repaint();
        if fullscreen {
            self.chrome.grow = Some(Grow::Shrinking(now));
            true
        } else {
            self.chrome.grow = Some(Grow::Entering(now));
            false
        }
    }

    /// Advances the grow and scales everything drawn this frame to it.
    fn grow_step(&mut self, ctx: &egui::Context) {
        let Some(grow) = self.chrome.grow else { return };
        let (now, fullscreen, inner) =
            ctx.input(|i| (i.time, i.viewport().fullscreen.unwrap_or(false), i.viewport().inner_rect));
        let screen = ctx.viewport_rect();
        // The window's place out of full screen, in the window as it is now,
        // and everything fitted into it against its top right corner, so it
        // grows from the right border (the whole window when it is there).
        let place = match (self.chrome.windowed, inner) {
            (Some(w), Some(inner)) => w.translate(-inner.min.to_vec2()),
            _ => screen,
        };
        let s = (place.width() / screen.width()).min(place.height() / screen.height());
        let small = Align2::RIGHT_TOP.align_size_within_rect(screen.size() * s, place);
        let there = (place.size() - screen.size()).length() < 1.0;
        let mut done = false;
        let at = match grow {
            Grow::Entering(since) => {
                if fullscreen && !there {
                    self.chrome.grow = Some(Grow::Growing(now));
                }
                done = now - since > FADE_TIMEOUT;
                small
            }
            Grow::Growing(since) => {
                let t = ((now - since) / GROW_TIME) as f32;
                done = t >= 1.0;
                small.lerp_towards(&screen, egui::emath::easing::cubic_out(t.min(1.0)))
            }
            Grow::Shrinking(since) => {
                let t = ((now - since) / GROW_TIME) as f32;
                if t >= 1.0 {
                    // Shrunk: the switch, now.
                    self.chrome.fade_running = true;
                    self.run_command(ctx, Command::Fullscreen);
                    self.chrome.fade_running = false;
                    self.chrome.grow = Some(Grow::Leaving(now));
                }
                screen.lerp_towards(&small, egui::emath::easing::cubic_in_out(t.min(1.0)))
            }
            Grow::Leaving(since) => {
                done = !fullscreen && there || now - since > FADE_TIMEOUT;
                small
            }
        };
        let transform = if done {
            self.chrome.grow = None;
            egui::emath::TSTransform::IDENTITY
        } else {
            ctx.request_repaint();
            let s = at.width() / screen.width();
            egui::emath::TSTransform::new(at.min.to_vec2() - screen.min.to_vec2() * s, s)
        };
        for layer in ctx.memory(|m| m.layer_ids().collect::<Vec<_>>()) {
            ctx.set_transform_layer(layer, transform);
        }
    }

    /// Starts going in or out of presenting (`cmd`) through a fade, unless
    /// one is running. Returns whether `cmd` waits for it.
    fn fade_first(&mut self, ctx: &egui::Context, cmd: Command) -> bool {
        if self.chrome.fade_running {
            return false;
        }
        if self.chrome.fade.is_none() {
            let now = ctx.input(|i| i.time);
            self.chrome.fade = Some(Fade { command: cmd, phase: FadePhase::Out(now) });
            ctx.request_repaint();
        }
        true
    }

    /// Advances the fade and paints its cover over everything.
    fn fade_step(&mut self, ctx: &egui::Context) {
        let Some(fade) = self.chrome.fade.as_ref() else { return };
        // Not inside `input`: the context is locked there.
        let frame = ctx.cumulative_frame_nr();
        let (now, fullscreen) = ctx.input(|i| (i.time, i.viewport().fullscreen.unwrap_or(false)));
        let (phase, command) = (fade.phase, fade.command);
        let ease = |t: f64| egui::emath::easing::cubic_in_out(t.clamp(0.0, 1.0) as f32);
        let cover = match phase {
            FadePhase::Out(since) => {
                let t = (now - since) / FADE_OUT;
                if t >= 1.0 {
                    // Covered: the switch, now.
                    self.chrome.fade_running = true;
                    self.run_command(ctx, command);
                    self.chrome.fade_running = false;
                    let target = self.chrome.switching.map_or(fullscreen, |(want, ..)| want);
                    if let Some(f) = self.chrome.fade.as_mut() {
                        f.phase = FadePhase::Switching(target, now, frame);
                    }
                    1.0
                } else {
                    ease(t)
                }
            }
            FadePhase::Switching(target, since, from) => {
                let switched = fullscreen == target && frame >= from + FADE_SETTLE_FRAMES;
                if (switched || now - since > FADE_TIMEOUT)
                    && let Some(f) = self.chrome.fade.as_mut()
                {
                    f.phase = FadePhase::In(now);
                }
                1.0
            }
            FadePhase::In(since) => {
                let t = (now - since) / FADE_IN;
                if t >= 1.0 {
                    self.chrome.fade = None;
                    return;
                }
                1.0 - ease(t)
            }
        };
        let layer = egui::LayerId::new(Order::Debug, Id::unique("present_fade"));
        ctx.layer_painter(layer).rect_filled(ctx.viewport_rect(), 0.0, Color32::BLACK.gamma_multiply(cover));
        ctx.request_repaint();
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
            // A PDF in the reader prints as it is.
            push(entry(ui, icon::PRINTER, "Print...", &shortcut(&ctx, cmd, Key::P), editor), ui, Command::Print);
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
                let side = self.settings.sidebar_side;
                push(
                    check(ui, side == SidebarSide::Left, "Sidebar on the left", ""),
                    ui,
                    Command::SidebarAt(SidebarSide::Left),
                );
                push(
                    check(ui, side == SidebarSide::Right, "Sidebar on the right", ""),
                    ui,
                    Command::SidebarAt(SidebarSide::Right),
                );
                ui.separator();
            }
            push(
                entry(ui, icon::ARROWS_OUT_SIMPLE, "Full screen", &shortcut(&ctx, Modifiers::NONE, Key::F11), true),
                ui,
                Command::Fullscreen,
            );
            if editor {
                push(
                    entry(ui, icon::PRESENTATION, "Present", &shortcut(&ctx, Modifiers::NONE, Key::F5), true),
                    ui,
                    Command::Present,
                );
            }
        });

        if editor && !self.reading() {
            ui.menu_button("Page", |ui| {
                open = true;
                // An infinite page is the only one: nothing to insert, move or delete.
                if !self.doc.is_canvas() {
                    self.page_entries(ui, self.current_page, &mut |ui, c| push(true, ui, c));
                    ui.separator();
                }
                let kind = self.doc.page.kind;
                for (k, label) in PageKind::ALL {
                    push(check(ui, kind == k, &format!("{label} paper"), ""), ui, Command::PaperStyle(k));
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
        if self.standalone.is_none()
            && !self.reading()
            && entry(
                ui,
                icon::FILE_MAGNIFYING_GLASS,
                "Find in notebook...",
                &shortcut(&ctx, cmd | Modifiers::SHIFT, Key::F),
                true,
            )
        {
            push(ui, Command::FindInNotebook);
        }
    }

    /// Copying the selected text (the reader; a note has it with the ink
    /// entries) and selecting all of `page`'s (Edit menu and context menu).
    fn text_entries(&self, ui: &mut Ui, page: u32, push: &mut dyn FnMut(&mut Ui, Command)) {
        let ctx = ui.ctx().clone();
        let cmd = Modifiers::COMMAND;
        if self.reading() && entry(ui, icon::COPY, "Copy", &shortcut(&ctx, cmd, Key::C), self.text_sel.is_some()) {
            push(ui, Command::Copy);
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
        self.clipboard_entries(ui, None, push);
        ui.separator();
        if entry(ui, icon::SELECTION_ALL, "Select all on page", &shortcut(&ctx, cmd, Key::A), true) {
            push(ui, Command::SelectAll(self.current_page));
        }
        let selected = self.selection.is_some() || self.picture.is_some();
        if entry(ui, icon::TRASH, "Delete selection", &shortcut(&ctx, Modifiers::NONE, Key::Delete), selected) {
            push(ui, Command::DeleteSelection);
        }
        if entry(ui, icon::SELECTION_SLASH, "Deselect", &shortcut(&ctx, Modifiers::NONE, Key::Escape), selected) {
            push(ui, Command::Deselect);
        }
        ui.separator();
        if entry(ui, icon::IMAGE, "Insert picture...", "", true) {
            push(ui, Command::InsertPicture(None));
        }
    }

    /// Cut, copy and paste (Edit menu and context menu); pasted at `at`, a
    /// page and a point on it, if given.
    fn clipboard_entries(&self, ui: &mut Ui, at: Option<(u32, Pos2)>, push: &mut dyn FnMut(&mut Ui, Command)) {
        self.cut_copy_entries(ui, push);
        let ctx = ui.ctx().clone();
        let label = if at.is_some() { "Paste here" } else { "Paste" };
        if entry(ui, icon::CLIPBOARD, label, &shortcut(&ctx, Modifiers::COMMAND, Key::V), true) {
            push(ui, Command::Paste(at));
        }
    }

    /// Cut and copy (the context menu has its own "Paste here").
    fn cut_copy_entries(&self, ui: &mut Ui, push: &mut dyn FnMut(&mut Ui, Command)) {
        let ctx = ui.ctx().clone();
        let cmd = Modifiers::COMMAND;
        if entry(ui, icon::SCISSORS, "Cut", &shortcut(&ctx, cmd, Key::X), self.can_cut()) {
            push(ui, Command::Cut);
        }
        if entry(ui, icon::COPY, "Copy", &shortcut(&ctx, cmd, Key::C), self.can_copy()) {
            push(ui, Command::Copy);
        }
    }

    /// Insert / delete entries, then the clipboard ones, for `page` (Page
    /// menu).
    fn page_entries(&self, ui: &mut Ui, page: u32, push: &mut dyn FnMut(&mut Ui, Command)) {
        page_insert_entries(ui, page, true, push);
        ui.separator();
        page_clipboard_entries(ui, page, (true, self.clipboard.has_pages()), push);
    }

    // -------------------------------------------------------- context menu

    /// The context menu on something: a selection (of ink or of text), a
    /// picture or a text box.
    fn item_menu(&self, ui: &mut Ui, menu: &ContextMenu, push: &mut dyn FnMut(&mut Ui, Command)) {
        let mut groups = 0;
        let mut group = |ui: &mut Ui| {
            if groups > 0 {
                ui.separator();
            }
            groups += 1;
        };
        if self.text_sel.is_some() || menu.on_selection || menu.picture.is_some() {
            group(ui);
            self.cut_copy_entries(ui, push);
            // Not on a picture: it is a picture already.
            if menu.on_selection && menu.picture.is_none() && entry(ui, icon::FILE_PNG, "Save as PNG...", "", true) {
                push(ui, Command::SaveSelectionPng);
            }
        }
        if let Some(id) = menu.picture {
            group(ui);
            if entry(ui, icon::TRASH, "Delete picture", "", true) {
                push(ui, Command::DeletePicture(id));
            }
        }
        if let Some(id) = menu.text {
            group(ui);
            if entry(ui, icon::TEXT_T, "Edit text", "", true) {
                push(ui, Command::EditText(id));
            }
            if entry(ui, icon::TRASH, "Delete text box", "", true) {
                push(ui, Command::DeleteText(id));
            }
        }
        if menu.on_selection {
            group(ui);
            let ctx = ui.ctx().clone();
            if entry(ui, icon::TRASH, "Delete selection", &shortcut(&ctx, Modifiers::NONE, Key::Delete), true) {
                push(ui, Command::DeleteSelection);
            }
            if entry(ui, icon::SELECTION_SLASH, "Deselect", &shortcut(&ctx, Modifiers::NONE, Key::Escape), true) {
                push(ui, Command::Deselect);
            }
        }
    }

    /// Draws the canvas context menu, if open. Call before the canvas: a
    /// press outside closes the menu and is not passed on to the canvas.
    pub(super) fn context_menu_ui(&mut self, ctx: &egui::Context) {
        let Some(menu) = self.context_menu.as_ref() else { return };
        let mut cmds = Vec::new();
        let mut push = |ui: &mut Ui, c: Command| {
            cmds.push(c);
            ui.close();
        };
        let area = egui::Area::new(Id::unique("canvas_context_menu"))
            .order(Order::Foreground)
            .fixed_pos(menu.pos)
            .show(ctx, |ui| {
                Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(220.0);
                    // Flat rows, like egui's own menus.
                    ui.visuals_mut().button_frame = false;
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    // Presenting: the laser and the way out (there is no title bar).
                    if self.presenting.is_some() {
                        let ctx = ui.ctx().clone();
                        let key = |k| shortcut(&ctx, Modifiers::NONE, k);
                        if check(ui, self.laser, "Laser pointer", &key(Key::L)) {
                            push(ui, Command::Laser);
                        }
                        ui.separator();
                        if entry(ui, icon::X, "End presentation", &key(Key::Escape), true) {
                            push(ui, Command::Present);
                        }
                        return;
                    }
                    // The reader: only its text; a note: copying a selection first.
                    if self.reading() {
                        self.text_entries(ui, menu.page.unwrap_or(self.current_page), &mut push);
                        return;
                    }
                    // On something (a selection, a picture, a text box): what
                    // can be done with it; elsewhere: the page's entries.
                    let on_item = self.text_sel.is_some() || menu.on_selection || menu.picture.is_some();
                    if on_item || menu.text.is_some() {
                        self.item_menu(ui, menu, &mut push);
                        return;
                    }
                    if entry(ui, icon::ARROW_U_UP_LEFT, "Undo", "", self.history.can_undo()) {
                        push(ui, Command::Undo);
                    }
                    if entry(ui, icon::ARROW_U_UP_RIGHT, "Redo", "", self.history.can_redo()) {
                        push(ui, Command::Redo);
                    }
                    if let Some(page) = menu.page {
                        let canvas = self.doc.is_canvas();
                        let on_page = if canvas { "" } else { " on page" };
                        ui.separator();
                        if entry(ui, icon::SELECTION_ALL, &format!("Select all{on_page}"), "", true) {
                            push(ui, Command::SelectAll(page));
                        }
                        if entry(ui, icon::TEXT_AA, &format!("Select all text{on_page}"), "", true) {
                            push(ui, Command::SelectAllText(page));
                        }
                        ui.separator();
                        let ctx = ui.ctx().clone();
                        let paste = shortcut(&ctx, Modifiers::COMMAND, Key::V);
                        if entry(ui, icon::CLIPBOARD, "Paste here", &paste, true) {
                            push(ui, Command::Paste(menu.at));
                        }
                        if entry(ui, icon::IMAGE, "Insert picture here...", "", true) {
                            push(ui, Command::InsertPicture(menu.at));
                        }
                        ui.separator();
                        if canvas {
                            if entry(ui, icon::FILE_PNG, "Save page as PNG...", "", true) {
                                push(ui, Command::SavePagePng(page));
                            }
                            return;
                        }
                        page_clipboard_entries(ui, page, (true, self.clipboard.has_pages()), &mut push);
                        ui.separator();
                        page_insert_entries(ui, page, true, &mut push);
                    }
                });
            });

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
        let note_paste = self.clipboard.note().is_some();
        // The paste key, even without text on the clipboard.
        let paste_key = !editor && std::mem::take(&mut self.pen_input.paste);
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
            // The library: a note copied or cut is pasted into the open
            // notebook or section.
            let paste_event = i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
            if !editor && !typing && note_paste && (paste_key || paste_event) {
                out.push(Command::PasteNote);
            }
            if editor {
                if i.consume_key(cmd, Key::W) {
                    out.push(Command::CloseNote);
                }
                if i.consume_key(cmd, Key::P) {
                    out.push(Command::Print);
                }
                // Before Ctrl+F, which would also match Ctrl+Shift+F.
                if i.consume_key(cmd | Modifiers::SHIFT, Key::F) {
                    out.push(Command::FindInNotebook);
                } else if i.consume_key(cmd, Key::F) {
                    out.push(Command::Find);
                }
                if i.consume_key(Modifiers::SHIFT, Key::F3) {
                    out.push(Command::FindNext(false));
                } else if i.consume_key(Modifiers::NONE, Key::F3) {
                    out.push(Command::FindNext(true));
                }
                // Presenting: F5 or Escape ends it, L turns the laser on or off.
                // Escape last: what is selected is dropped first (see
                // `escape_for_overlay`).
                let presenting = self.presenting.is_some();
                let selected = self.text_sel.is_some() || self.selection.is_some() || self.picture.is_some();
                if i.consume_key(Modifiers::NONE, Key::F5)
                    || presenting
                        && !selected
                        && self.context_menu.is_none()
                        && !self.escape_for_overlay
                        && i.consume_key(Modifiers::NONE, Key::Escape)
                {
                    out.push(Command::Present);
                }
                if presenting && !typing && i.consume_key(Modifiers::NONE, Key::L) {
                    out.push(Command::Laser);
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
                    // egui turns Ctrl+C / Ctrl+X into copy / cut events. Ctrl+V
                    // is the canvas's (see `pictures::picture_input`).
                    let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy)) || i.consume_key(cmd, Key::C);
                    if copy && self.can_copy() {
                        out.push(Command::Copy);
                    }
                    let cut = i.events.iter().any(|e| matches!(e, egui::Event::Cut)) || i.consume_key(cmd, Key::X);
                    if cut && self.can_cut() {
                        out.push(Command::Cut);
                    }
                    if self.text_sel.is_some()
                        && self.context_menu.is_none()
                        && !self.escape_for_overlay
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
        // Presenting, in and out, through a fade.
        if cmd == Command::Present && self.fade_first(ctx, cmd) {
            return;
        }
        // Full screen by itself: the window grows to it and shrinks back.
        if cmd == Command::Fullscreen && self.grow_first(ctx) {
            return;
        }
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
            Command::Copy => self.copy(ctx),
            Command::Cut if editor => self.cut(ctx),
            Command::Paste(at) if editor => self.paste(at, false),
            Command::CopyPage(page) if editor => self.copy_page(ctx, page),
            Command::CutPage(page) if editor => self.cut_page(ctx, page),
            Command::SavePagePng(page) if editor => self.save_page_png(page),
            Command::SaveSelectionPng if editor => self.save_selection_png(),
            Command::PastePages(index) if editor => self.paste_pages_here(index),
            Command::PasteNote if !editor => {
                let dir = self.library_target_dir();
                self.paste_note(dir);
            }
            Command::SelectAllText(page) if editor => self.select_all_text(page),
            Command::Find if editor => self.find(Scope::Note),
            // A PDF in the reader has no notebook: it is searched by itself.
            Command::FindInNotebook if editor => self.find(Scope::Notebook),
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
            Command::ExportPdf if editor && !self.reading() => self.open_export_dialog(ctx, false),
            Command::Print if editor => self.open_export_dialog(ctx, true),
            Command::CloseNote if editor => self.close_note(ctx),
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::SelectAll(page) if editor => self.select_all_on_page(page),
            Command::DeleteSelection => self.delete_selection(),
            Command::InsertPicture(at) if editor => self.ask_picture_file(at),
            Command::DeletePicture(id) => self.delete_picture(id),
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
            Command::ZoomIn | Command::ZoomOut => {
                let now = (self.view_scale * 100.0).round() as u32;
                let range = self.layout().zoom_range();
                let mut steps = ZOOM_STEPS.iter().copied().filter(|s| range.contains(s));
                let next = if cmd == Command::ZoomIn {
                    steps.find(|&s| s > now).unwrap_or(*range.end())
                } else {
                    steps.rev().find(|&s| s < now).unwrap_or(*range.start())
                };
                self.zoom_request = Some(next as f32 / now.max(1) as f32);
            }
            Command::Zoom(z) => *self.zoom_mut() = z,
            Command::Present => match self.presenting.take() {
                Some(p) => {
                    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    if p.entered_fullscreen && fullscreen {
                        self.run_command(ctx, Command::Fullscreen);
                    }
                }
                None if !editor => {}
                None => {
                    // Nothing being typed, selected or open over the page.
                    self.commit_edit(ctx);
                    self.clear_selection();
                    self.context_menu = None;
                    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    if !fullscreen {
                        self.run_command(ctx, Command::Fullscreen);
                    }
                    self.presenting = Some(super::Presentation {
                        entered_fullscreen: !fullscreen,
                        seen_fullscreen: fullscreen,
                        zoom: ZoomMode::FitPage,
                    });
                    // From the top of the page being read, at once: easing there
                    // while the zoom changes swoops across the pages.
                    self.kinetic.stop();
                    // An infinite page is presented from where it is.
                    if !self.doc.is_canvas() {
                        self.scroll = egui::vec2(0.0, self.layout().page_stop(self.current_page));
                    }
                    crate::toolbar::hide_presenting_bar(ctx);
                }
            },
            Command::Laser => self.laser = !self.laser,
            Command::Fullscreen => {
                // Leaving full screen ends presenting too.
                self.presenting = None;
                // A maximized window does not stay full screen (winit maximizes
                // it again on the next change of its style), so it is
                // un-maximized first and maximized again after. On Windows that
                // happens in place: un-maximized at its maximized size, it
                // comes back from full screen at that size too, not shrinking
                // and growing back; its own size un-maximized is put back after
                // (see `window_chrome`). Its animations are off meanwhile.
                let (fs, maximized, now) = ctx.input(|i| {
                    (i.viewport().fullscreen.unwrap_or(false), i.viewport().maximized.unwrap_or(false), i.time)
                });
                chrome::window_transitions(false);
                crate::win_frame::switching(true);
                if fs {
                    let maximize = std::mem::take(&mut self.chrome.maximize_after_fullscreen);
                    self.chrome.switching = Some((false, maximize, now));
                    // The system frame (on in full screen, see `window_chrome`) off
                    // first: else the window comes back with it, unthemed, for a
                    // moment.
                    if self.settings.custom_title_bar && !cfg!(windows) {
                        self.chrome.decorations = Some(false);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    if maximize {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                    }
                } else {
                    self.chrome.switching = Some((true, false, now));
                    self.chrome.maximize_after_fullscreen = maximized;
                    if maximized {
                        self.chrome.normal_rect = chrome::unmaximize_in_place();
                        if self.chrome.normal_rect.is_none() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                        }
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                }
            }
            Command::ToolbarAt(p) => self.settings.toolbar_position = p,
            Command::SidebarAt(side) => self.settings.sidebar_side = side,
            Command::Settings => self.global_settings_open = true,
            Command::NoteSettings if editor => self.settings.sidebar = Sidebar::Settings,
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
        self.about_window(ctx);
        self.shortcuts_window(ctx);
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.help.about {
            return;
        }
        let shown = theme::window(ABOUT_WINDOW, "About Tratto")
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(icon::PEN_NIB).size(40.0).color(color::accent()));
                    ui.label(theme::title("Tratto"));
                    ui.label(theme::muted(format!("Version {}", crate::update::CURRENT)));
                    ui.add_space(10.0);
                    ui.label(env!("CARGO_PKG_DESCRIPTION"));
                    ui.add_space(20.0);
                    ui.label("Copyright © 2026 Christian Braghette");
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Free software under the GNU AGPL v3 or later.")
                            .small()
                            .color(color::text_muted()),
                    );
                    ui.label(
                        RichText::new("Commercial licenses are also available.").small().color(color::text_muted()),
                    );
                    ui.add_space(5.0);
                    ui.hyperlink_to("Source code and license", env!("CARGO_PKG_REPOSITORY"));
                    ui.add_space(15.0);
                    ui.label(
                        RichText::new("Font: Inter (OFL) · Icons: Phosphor (MIT)").small().color(color::text_faint()),
                    );
                });
            });
        if shown.is_some_and(|w| theme::window_close_button(&w.response)) {
            self.help.about = false;
        }
    }

    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.help.shortcuts {
            return;
        }
        let shown = theme::window(SHORTCUTS_WINDOW, "Keyboard shortcuts")
            .collapsible(false)
            // Above the notches and the toolbar.
            .order(egui::Order::Foreground)
            .resizable(false)
            // Pinned: a window never shrinks below the size it remembers.
            .min_width(SHORTCUTS_WIDTH)
            .max_width(SHORTCUTS_WIDTH)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let c = Modifiers::COMMAND;
                let (none, shift) = (Modifiers::NONE, Modifiers::SHIFT);
                let key = |m, k| shortcut(ctx, m, k);
                let or = |a: String, b: String| format!("{a} / {b}");
                // Groups of (keys, what); keys split by " / " are alternatives.
                let groups: [(&str, Vec<(String, &str)>); 6] = [
                    (
                        "Notes",
                        vec![
                            (key(c, Key::N), "New note"),
                            (key(c, Key::S), "Save now"),
                            (key(c, Key::E), "Export as PDF"),
                            (key(c, Key::P), "Print"),
                            (key(c, Key::W), "Close the note"),
                            (key(c, Key::O), "Open a PDF to read"),
                        ],
                    ),
                    (
                        "Editing",
                        vec![
                            (key(c, Key::Z), "Undo"),
                            (or(key(c, Key::Y), key(c | shift, Key::Z)), "Redo"),
                            (key(c, Key::A), "Select all on the page"),
                            (key(none, Key::Delete), "Delete the selection"),
                            (key(none, Key::Escape), "Deselect"),
                            (key(c, Key::X), "Cut the selection"),
                            (key(c, Key::C), "Copy the selection or the selected text"),
                            (key(c, Key::V), "Paste (at the pointer, over a page)"),
                        ],
                    ),
                    (
                        "Text boxes",
                        vec![
                            (or(key(c, Key::B), key(c, Key::I)), "Bold / italic"),
                            (key(c, Key::K), "Link"),
                            ("Enter".to_owned(), "Next list item"),
                            (or(key(none, Key::Tab), key(shift, Key::Tab)), "Nest / unnest an item"),
                        ],
                    ),
                    (
                        "View",
                        vec![
                            (or(key(c, Key::Plus), key(c, Key::Minus)), "Zoom in / out"),
                            (key(c, Key::Num0), "Actual size"),
                            ("Ctrl+Wheel / Pinch".to_owned(), "Zoom at the pointer"),
                            ("Alt+Wheel / Shift+Wheel".to_owned(), "Scroll sideways"),
                            ("PgUp / PgDn".to_owned(), "Scroll by a screen"),
                            ("Home / End".to_owned(), "First / last page"),
                            (key(none, Key::F11), "Full screen"),
                            (or(key(none, Key::F5), key(none, Key::Escape)), "Present / end presenting"),
                            (key(none, Key::L), "Laser pointer, while presenting"),
                        ],
                    ),
                    (
                        "Search and PDF reader",
                        vec![
                            (key(c, Key::F), "Find in the note"),
                            (key(c | shift, Key::F), "Find in the notebook"),
                            (or(key(none, Key::F3), key(shift, Key::F3)), "Next / previous match"),
                            ("Left / Right / Space".to_owned(), "Previous / next page"),
                            ("Alt+Left / Backspace".to_owned(), "Back after a link"),
                        ],
                    ),
                    (
                        "App",
                        vec![
                            ("Right click".to_owned(), "Context menu"),
                            (key(c, Key::Comma), "Settings"),
                            (key(c, Key::Q), "Exit"),
                        ],
                    ),
                ];
                // As wide as the window: the scroll bar at its edge, floating
                // over the margin on the right.
                let max_h = (ctx.viewport_rect().height() * 0.7).max(240.0);
                egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                    ui.set_max_width(ui.available_width() - theme::space::M);
                    for (n, (title, rows)) in groups.iter().enumerate() {
                        if n > 0 {
                            ui.add_space(theme::space::M);
                        }
                        theme::caption(ui, title);
                        ui.add_space(theme::space::XS);
                        ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.y = theme::space::S;
                            for (keys, what) in rows {
                                ui.horizontal(|ui| {
                                    let keys_layout =
                                        egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(SHORTCUT_KEYS_WIDTH, 0.0),
                                        keys_layout,
                                        |ui| {
                                            ui.set_width(SHORTCUT_KEYS_WIDTH);
                                            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                                            for (k, alt) in keys.split(" / ").enumerate() {
                                                if k > 0 {
                                                    ui.label(theme::muted("/"));
                                                }
                                                keycap(ui, alt);
                                            }
                                        },
                                    );
                                    ui.add_space(theme::space::L - ui.spacing().item_spacing.x);
                                    ui.add(egui::Label::new(*what).wrap());
                                });
                            }
                        });
                    }
                });
            });
        if shown.is_some_and(|w| theme::window_close_button(&w.response)) {
            self.help.shortcuts = false;
        }
    }

    /// Escape closes the window on top (settings, help, updates, export),
    /// before the editor would take it. Not while typing or with a popup
    /// open: those take it first.
    pub(super) fn escape_closes_window(&mut self, ctx: &egui::Context) {
        // The last window on top. Not egui's top layer itself: a dropdown's
        // popup stays at the top of its order after it closes.
        if let Some(top) = ctx.memory(|m| m.areas().top_layer_id(Order::Foreground))
            && ESCAPE_WINDOWS.iter().chain([&LIBRARY_DIALOG]).any(|w| top.id == Id::unique(w))
        {
            self.top_window = Some(top.id);
        }
        // A field typed in, a menu, or the context menu (above every window)
        // close first.
        if ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx) || self.context_menu.is_some() {
            return;
        }
        // A library dialog closes itself.
        if self.top_window == Some(Id::unique(LIBRARY_DIALOG)) && self.lib_ui.dialog.is_some() {
            return;
        }
        let is_open = |app: &Self, w: &str| match w {
            SETTINGS_WINDOW => app.global_settings_open,
            UPDATES_WINDOW => app.help.updates,
            SHORTCUTS_WINDOW => app.help.shortcuts,
            ABOUT_WINDOW => app.help.about,
            EXPORT_WINDOW => app.export_dialog.is_some(),
            _ => false,
        };
        // The one on top if still open, else any.
        let top = ESCAPE_WINDOWS.iter().find(|w| self.top_window == Some(Id::unique(w)));
        let Some(&window) =
            top.filter(|w| is_open(self, w)).or_else(|| ESCAPE_WINDOWS.iter().find(|w| is_open(self, w)))
        else {
            return;
        };
        if !ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            return;
        }
        match window {
            SETTINGS_WINDOW => self.global_settings_open = false,
            UPDATES_WINDOW => self.help.updates = false,
            SHORTCUTS_WINDOW => self.help.shortcuts = false,
            ABOUT_WINDOW => self.help.about = false,
            EXPORT_WINDOW => self.export_dialog = None,
            _ => {}
        }
    }
}
