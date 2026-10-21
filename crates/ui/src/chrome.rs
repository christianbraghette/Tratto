// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Window chrome drawn by the app itself (VS Code style): a title bar with the
//! menu bar, the window title and the minimize / maximize / close buttons,
//! plus resize borders and a thin window outline.
//!
//! With the custom frame the system decorations are turned off, so moving and
//! resizing go through `ViewportCommand::StartDrag` / `BeginResize`, which hand
//! the gesture to the OS (snapping keeps working). With the native frame only
//! the menu bar is drawn, below the system title bar.
//!
//! The window buttons follow the desktop (see `desktop.rs`): which ones, on
//! which side and how they look (Windows, GNOME's Adwaita, KDE's Breeze).
//!
//! In fullscreen the bar hides and slides in when the pointer reaches the top
//! edge, like OneNote. On Windows 11 DWM rounds the corners of the normal
//! (not maximized) window, see `round_corners`.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Id, LayerId, Layout, Order, PointerButton, Rect, ResizeDirection,
    RichText, Sense, Stroke, StrokeKind, Ui, ViewportCommand, pos2, vec2,
};
use egui_phosphor::regular as icon;

use crate::desktop::{self, Button, Style};
use crate::theme::{self, color, size, space};

/// Height of the custom title bar.
pub const TITLE_BAR_HEIGHT: f32 = 32.0;
/// Width of the invisible resize borders.
const RESIZE_BORDER: f32 = 5.0;
const CLOSE_HOVER: Color32 = color::DANGER;
/// Fullscreen: how close to the top edge the pointer reveals the bar, and
/// how far below it the bar stays.
const REVEAL_EDGE: f32 = 2.0;
const KEEP_BELOW: f32 = 12.0;
/// Slide in / out time of the fullscreen bar, in seconds.
const REVEAL_TIME: f32 = 0.15;

/// DWM rounds the corners and draws the window's border (Windows 11).
static DWM_FRAME: AtomicBool = AtomicBool::new(false);

/// Window state the chrome depends on.
#[derive(Clone, Copy)]
pub struct WindowState {
    /// Draw the custom frame (system decorations are off).
    pub custom: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

impl WindowState {
    pub fn of(ctx: &egui::Context, custom: bool) -> Self {
        ctx.input(|i| Self {
            custom,
            maximized: i.viewport().maximized.unwrap_or(false),
            fullscreen: i.viewport().fullscreen.unwrap_or(false),
        })
    }

    /// Window buttons, dragging and resizing are ours (not in fullscreen).
    fn frame_controls(&self) -> bool {
        self.custom && !self.fullscreen
    }
}

/// What the centre of the title bar shows.
pub enum Title<'a> {
    Text(&'a str),
    /// An editable title (the open note): plain text until hovered or clicked.
    Editable {
        text: &'a mut String,
        hint: &'a str,
    },
}

/// Fullscreen: how far the bar is shown (0 hidden, 1 fully in). It slides in
/// when the pointer touches the top edge and stays while the pointer is on
/// it or just below, or while `pinned` (a menu is open, the title is edited).
fn fullscreen_reveal(ctx: &egui::Context, pinned: bool) -> f32 {
    let id = Id::new("title_bar_reveal");
    let shown = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let top = ctx.viewport_rect().top();
    let pointer_y = ctx.input(|i| i.pointer.hover_pos()).map(|p| p.y);
    let near = pointer_y.is_some_and(|y| {
        let reach = if shown { TITLE_BAR_HEIGHT + KEEP_BELOW } else { REVEAL_EDGE };
        y <= top + reach
    });
    let show = pinned || near;
    ctx.data_mut(|d| d.insert_temp(id, show));
    ctx.animate_bool_with_time(id, show, REVEAL_TIME)
}

/// What the title bar reports.
#[derive(Default)]
pub struct TitleBarResponse {
    /// The text field of an editable title.
    pub title: Option<egui::Response>,
    /// Fullscreen: the button where the window buttons usually are was
    /// clicked (back to the window).
    pub exit_fullscreen: bool,
}

/// Shows the top bar in the root `ui`. Call it before any other panel so it
/// spans the whole window. `menus` adds the menu buttons (inside an
/// `egui::MenuBar`).
/// In fullscreen the bar hides until the pointer reaches the top edge;
/// `pinned` keeps it shown, and an "exit full screen" button takes the
/// place of the window buttons.
pub fn title_bar(
    ui: &mut Ui,
    state: WindowState,
    pinned: bool,
    title: Title<'_>,
    menus: impl FnOnce(&mut Ui),
) -> TitleBarResponse {
    let ctx = &ui.ctx().clone();
    let controls = state.frame_controls();
    // Same height with or without the window buttons: the menu buttons need
    // it, and a lower bar would leave a gap below its painted background.
    let height = TITLE_BAR_HEIGHT;
    // The panel grows with the reveal, the bar slides down inside it.
    let shown = if state.fullscreen { fullscreen_reveal(ctx, pinned) } else { 1.0 };
    if shown <= 0.0 {
        return TitleBarResponse::default();
    }
    let mut response = TitleBarResponse::default();
    let buttons = desktop::window_buttons(ctx);
    let (_, edge) = button_size(buttons.style);
    egui::Panel::top("title_bar")
        .exact_size(height * shown)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(color::CHROME))
        .show(ui, |ui| {
            let panel = ui.max_rect();
            let bar = Rect::from_min_size(panel.min - vec2(0.0, height * (1.0 - shown)), vec2(panel.width(), height));
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(bar));
            ui.set_clip_rect(panel.intersect(ui.clip_rect()));
            let ui = &mut ui;
            // The background first, so every widget added later wins the hit test.
            if controls {
                let resp = ui.interact(bar, Id::new("title_bar_drag"), Sense::click_and_drag());
                if resp.double_clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!state.maximized));
                } else if resp.drag_started_by(PointerButton::Primary) {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
            }

            let mut menus_right = bar.left();
            let mut buttons_left = bar.right();
            ui.horizontal(|ui| {
                ui.set_height(height);
                if controls && !buttons.left.is_empty() {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(edge);
                        window_buttons(ui, buttons.style, buttons.left.iter().copied(), state, height);
                    });
                }
                ui.add_space(space::M);
                ui.label(RichText::new(icon::PEN_NIB).size(size::ICON).color(color::ACCENT));
                ui.add_space(space::XS);
                egui::MenuBar::new().ui(ui, |ui| {
                    // Menu titles: flat, muted until hovered or open.
                    theme::ghost_buttons(ui);
                    ui.visuals_mut().widgets.inactive.fg_stroke.color = color::TEXT_MUTED;
                    ui.spacing_mut().button_padding = vec2(space::S, space::XS);
                    menus(ui);
                    // The bar fills the row, so its rect says nothing; the
                    // cursor is where the next item would go.
                    menus_right = ui.cursor().left();
                });
            });
            // In their own area spanning exactly the bar: nested in the row
            // above, the right-to-left layout would start one item spacing
            // (8 px) past the right edge, partly outside the window.
            if (controls && !buttons.right.is_empty()) || state.fullscreen {
                let layout = Layout::right_to_left(Align::Center);
                ui.scope_builder(egui::UiBuilder::new().max_rect(bar).layout(layout), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(edge);
                    if state.fullscreen {
                        let exit = window_button(ui, buttons.style, icon::CORNERS_IN, false, height);
                        response.exit_fullscreen = exit.clicked();
                    } else {
                        // Right to left: the last button first.
                        window_buttons(ui, buttons.style, buttons.right.iter().rev().copied(), state, height);
                    }
                    buttons_left = ui.min_rect().left();
                });
            }

            // Centred title, moved aside (or dropped) when the window is too narrow.
            let (min_x, max_x) = (menus_right + 16.0, buttons_left - 16.0);
            let place = |w: f32| (max_x - min_x >= w).then(|| (bar.center().x - w * 0.5).clamp(min_x, max_x - w));
            match title {
                Title::Text(text) => {
                    let color = color::TEXT_MUTED;
                    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::font(size::BODY), color);
                    if let Some(x) = place(galley.size().x) {
                        let pos = pos2(x, bar.center().y - galley.size().y * 0.5);
                        ui.painter().galley(pos, galley, color);
                    }
                }
                Title::Editable { text, hint } => {
                    response.title = editable_title(ui, bar, text, hint, &place);
                }
            }
        });
    response
}

/// The note title as a borderless text field: a hover fill shows that it can
/// be edited; while typing it gets the input look of the other text fields.
fn editable_title(
    ui: &mut Ui,
    bar: Rect,
    text: &mut String,
    hint: &str,
    place: &dyn Fn(f32) -> Option<f32>,
) -> Option<egui::Response> {
    let font = theme::font(size::BODY);
    let shown = if text.is_empty() { hint } else { text.as_str() };
    let text_w = ui.painter().layout_no_wrap(shown.to_owned(), font.clone(), color::TEXT).size().x;
    let w = (text_w + 2.0 * space::L).clamp(160.0, 420.0);
    // Narrower (but still editable) when the window is tight, hidden when it
    // does not fit at all.
    let (x, w) = match place(w) {
        Some(x) => (x, w),
        None => (place(120.0)?, 120.0),
    };
    let rect = Rect::from_min_size(pos2(x, bar.center().y - 12.0), vec2(w, 24.0));

    let resp =
        theme::inline_edit(ui, rect, Id::new("note_title_edit"), text, hint, font, Align::Center, color::TEXT_MUTED);
    Some(resp.on_hover_text("Rename the note"))
}

/// The buttons in `order`, as the desktop draws them. No tooltips: the
/// icons are standard, and a tooltip would open over the editor's top-right
/// notch.
fn window_buttons(ui: &mut Ui, style: Style, order: impl Iterator<Item = Button>, state: WindowState, height: f32) {
    let ctx = ui.ctx().clone();
    for button in order {
        if window_button(ui, style, glyph(style, button, state.maximized), button == Button::Close, height).clicked() {
            ctx.send_viewport_cmd(match button {
                Button::Minimize => ViewportCommand::Minimized(true),
                Button::Maximize => ViewportCommand::Maximized(!state.maximized),
                Button::Close => ViewportCommand::Close,
            });
        }
    }
}

/// Width of one window button, and the space between the outermost one and
/// the window's edge.
fn button_size(style: Style) -> (f32, f32) {
    match style {
        Style::Windows => (46.0, 0.0),
        Style::Adwaita => (30.0, 3.0),
        Style::Breeze => (28.0, 2.0),
    }
}

fn glyph(style: Style, button: Button, maximized: bool) -> &'static str {
    match (button, style) {
        (Button::Close, _) => icon::X,
        (Button::Minimize, Style::Breeze) => icon::CARET_DOWN,
        (Button::Minimize, _) => icon::MINUS,
        (Button::Maximize, Style::Breeze) => {
            if maximized {
                icon::DIAMOND
            } else {
                icon::CARET_UP
            }
        }
        (Button::Maximize, _) => {
            if maximized {
                icon::COPY
            } else {
                icon::SQUARE
            }
        }
    }
}

fn window_button(ui: &mut Ui, style: Style, glyph: &str, close: bool, height: f32) -> egui::Response {
    let (width, _) = button_size(style);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    let painter = ui.painter();
    let mut color = color::TEXT_MUTED;
    let glyph_size = match style {
        Style::Windows => {
            if resp.hovered() {
                painter.rect_filled(rect, 0.0, if close { CLOSE_HOVER } else { color::HOVER });
                color = if close { color::ON_ACCENT } else { color::TEXT_STRONG };
            }
            15.0
        }
        // Always a filled circle, a bit lighter when hovered.
        Style::Adwaita => {
            let fill = if pressed {
                color::PRESSED
            } else if resp.hovered() {
                color::HOVER
            } else {
                color::RAISED
            };
            painter.circle_filled(rect.center(), 12.0, fill);
            color = color::TEXT;
            13.0
        }
        // A circle only when hovered: red behind the close button.
        Style::Breeze => {
            if resp.hovered() || pressed {
                let fill = if close {
                    CLOSE_HOVER
                } else if pressed {
                    color::PRESSED
                } else {
                    color::HOVER
                };
                painter.circle_filled(rect.center(), 10.0, fill);
                color = if close { color::ON_ACCENT } else { color::TEXT_STRONG };
            }
            14.0
        }
    };
    painter.text(rect.center(), Align2::CENTER_CENTER, glyph, theme::font(glyph_size), color);
    resp
}

/// Resize borders along the window edges and a 1 px outline. Call it last,
/// so the borders are on top of the other floating elements.
pub fn window_frame(ctx: &egui::Context, state: WindowState) {
    sync_dwm_border(state.fullscreen);
    if !state.frame_controls() || state.maximized {
        return;
    }
    let r = ctx.viewport_rect();
    let (b, c) = (RESIZE_BORDER, RESIZE_BORDER * 2.0);
    let zones = [
        (ResizeDirection::NorthWest, Rect::from_min_size(r.left_top(), vec2(c, c)), CursorIcon::ResizeNorthWest),
        (
            ResizeDirection::NorthEast,
            Rect::from_min_size(pos2(r.right() - c, r.top()), vec2(c, c)),
            CursorIcon::ResizeNorthEast,
        ),
        (
            ResizeDirection::SouthWest,
            Rect::from_min_size(pos2(r.left(), r.bottom() - c), vec2(c, c)),
            CursorIcon::ResizeSouthWest,
        ),
        (
            ResizeDirection::SouthEast,
            Rect::from_min_size(r.right_bottom() - vec2(c, c), vec2(c, c)),
            CursorIcon::ResizeSouthEast,
        ),
        (
            ResizeDirection::North,
            Rect::from_min_max(pos2(r.left() + c, r.top()), pos2(r.right() - c, r.top() + b)),
            CursorIcon::ResizeNorth,
        ),
        (
            ResizeDirection::South,
            Rect::from_min_max(pos2(r.left() + c, r.bottom() - b), pos2(r.right() - c, r.bottom())),
            CursorIcon::ResizeSouth,
        ),
        (
            ResizeDirection::West,
            Rect::from_min_max(pos2(r.left(), r.top() + c), pos2(r.left() + b, r.bottom() - c)),
            CursorIcon::ResizeWest,
        ),
        (
            ResizeDirection::East,
            Rect::from_min_max(pos2(r.right() - b, r.top() + c), pos2(r.right(), r.bottom() - c)),
            CursorIcon::ResizeEast,
        ),
    ];
    for (i, (dir, rect, cursor)) in zones.into_iter().enumerate() {
        egui::Area::new(Id::new("resize_border").with(i))
            .order(Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                let resp = ui.allocate_response(rect.size(), Sense::drag());
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(cursor);
                }
                if resp.drag_started_by(PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(dir));
                }
            });
    }
    // DWM draws the (rounded) border itself when it can.
    if !DWM_FRAME.load(Ordering::Relaxed) {
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("window_outline")));
        painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, color::BORDER_STRONG), StrokeKind::Middle);
    }
}

/// Windows 11: has DWM round the window's corners (it only does so by itself
/// for windows with system decorations) and draw its 1 px border in the
/// outline colour (off in fullscreen, see `sync_dwm_border`). DWM keeps
/// maximized and fullscreen windows square. Does
/// nothing elsewhere; on Windows 10 the app keeps drawing its own outline.
pub fn round_corners(frame: &eframe::Frame) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        let hwnd = h.hwnd.get();
        if dwm::set(hwnd, dwm::WINDOW_CORNER_PREFERENCE, dwm::CORNER_ROUND) {
            dwm::HWND.store(hwnd, Ordering::Relaxed);
            dwm::set(hwnd, dwm::BORDER_COLOR, dwm::outline());
            DWM_FRAME.store(true, Ordering::Relaxed);
        }
    }
    #[cfg(not(windows))]
    let _ = frame;
}

/// Windows 11: DWM keeps drawing the border colour on the borderless
/// fullscreen window, so it is turned off there and back on when leaving.
fn sync_dwm_border(fullscreen: bool) {
    #[cfg(windows)]
    {
        if !DWM_FRAME.load(Ordering::Relaxed) || dwm::FULLSCREEN.swap(fullscreen, Ordering::Relaxed) == fullscreen {
            return;
        }
        let color = if fullscreen { dwm::COLOR_NONE } else { dwm::outline() };
        dwm::set(dwm::HWND.load(Ordering::Relaxed), dwm::BORDER_COLOR, color);
    }
    #[cfg(not(windows))]
    let _ = fullscreen;
}

#[cfg(windows)]
mod dwm {
    use std::sync::atomic::{AtomicBool, AtomicIsize};

    use crate::theme::color;

    // dwmapi.h, declared here so they need no optional windows-sys feature.
    pub const WINDOW_CORNER_PREFERENCE: u32 = 33;
    pub const BORDER_COLOR: u32 = 34;
    pub const CORNER_ROUND: u32 = 2;
    pub const COLOR_NONE: u32 = 0xFFFF_FFFE;
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }

    /// Our window, once `round_corners` has run.
    pub static HWND: AtomicIsize = AtomicIsize::new(0);
    /// The border colour is currently off for fullscreen.
    pub static FULLSCREEN: AtomicBool = AtomicBool::new(false);

    pub fn set(hwnd: isize, attribute: u32, value: u32) -> bool {
        // SAFETY: a valid HWND of our own window and a u32 value, as the
        // attributes expect.
        unsafe { DwmSetWindowAttribute(hwnd, attribute, (&raw const value).cast(), 4) == 0 }
    }

    /// The outline colour as a COLORREF (0x00BBGGRR).
    pub fn outline() -> u32 {
        let [r, g, b, _] = color::BORDER_STRONG.to_array();
        u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton, RawInput, ViewportInfo};

    /// One frame (1/60 s) of a fullscreen 800 x 600 window with `events`;
    /// `menu_open` is set to whether the menu is open.
    fn frame(ctx: &egui::Context, menu_open: &mut bool, events: Vec<Event>) {
        let mut input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            time: Some(ctx.input(|i| i.time) + 1.0 / 60.0),
            events,
            ..Default::default()
        };
        let info = ViewportInfo { fullscreen: Some(true), ..Default::default() };
        input.viewports.insert(egui::ViewportId::ROOT, info);
        let pinned = *menu_open;
        crate::test_frame(ctx, input, |ui| {
            let state = WindowState::of(ui.ctx(), true);
            let mut open = false;
            title_bar(ui, state, pinned, Title::Text("t"), |ui| {
                // As the app does: the contents run only while the menu is shown.
                ui.menu_button("File", |ui| {
                    open = true;
                    ui.label("entry");
                });
            });
            egui::CentralPanel::default().show(ui, |_| {});
            *menu_open = open;
        });
    }

    fn at(x: f32, y: f32) -> Event {
        Event::PointerMoved(pos2(x, y))
    }

    fn button(x: f32, y: f32, pressed: bool) -> Event {
        Event::PointerButton { pos: pos2(x, y), button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    #[test]
    fn fullscreen_bar_has_an_exit_button_at_the_window_buttons() {
        let ctx = egui::Context::default();
        let mut exit = false;
        let mut run = |events: Vec<Event>| {
            let mut input = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                time: Some(ctx.input(|i| i.time) + 1.0 / 60.0),
                events,
                ..Default::default()
            };
            let info = ViewportInfo { fullscreen: Some(true), ..Default::default() };
            input.viewports.insert(egui::ViewportId::ROOT, info);
            crate::test_frame(&ctx, input, |ui| {
                let state = WindowState::of(ui.ctx(), true);
                exit |= title_bar(ui, state, false, Title::Text("t"), |_| {}).exit_fullscreen;
                egui::CentralPanel::default().show(ui, |_| {});
            });
        };
        // Where the close button usually is: the right end of the bar.
        let (width, edge) = button_size(desktop::window_buttons(&ctx).style);
        let (x, y) = (800.0 - edge - width * 0.5, 16.0);
        for _ in 0..30 {
            run(vec![at(x, 0.0)]);
        }
        run(vec![at(x, y)]);
        run(vec![button(x, y, true)]);
        run(vec![button(x, y, false)]);
        assert!(exit);
    }

    #[test]
    fn fullscreen_bar_stays_while_a_menu_is_open() {
        let ctx = egui::Context::default();
        let mut open = false;
        // Reveal: the pointer at the top edge, wait for the slide.
        for _ in 0..30 {
            frame(&ctx, &mut open, vec![at(60.0, 0.0)]);
        }
        // Click "File" (just right of the app icon).
        let file = (60.0, 16.0);
        frame(&ctx, &mut open, vec![at(file.0, file.1)]);
        frame(&ctx, &mut open, vec![button(file.0, file.1, true)]);
        frame(&ctx, &mut open, vec![button(file.0, file.1, false)]);
        assert!(open, "the menu opens");
        // Into the dropdown, well below the bar.
        for _ in 0..30 {
            frame(&ctx, &mut open, vec![at(70.0, 60.0)]);
        }
        assert!(open, "the menu stays open");
        // A pen lifts off: the pointer is gone.
        for _ in 0..30 {
            frame(&ctx, &mut open, vec![Event::PointerGone]);
        }
        assert!(open, "the menu stays open without a pointer");
    }
}
