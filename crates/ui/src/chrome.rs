// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Window chrome drawn by the app itself (VS Code style): a title bar with the
//! menu bar, the window title and the minimize / maximize / close buttons,
//! plus resize borders and a thin window outline.
//!
//! With the custom frame the system decorations are turned off, so moving and
//! resizing go through `ViewportCommand::StartDrag` / `BeginResize`, which hand
//! the gesture to the OS (snapping keeps working). On Windows the window keeps
//! the system's frame and only loses its caption (`win_frame`). With the
//! native frame only the menu bar is drawn, below the system title bar.
//!
//! The window buttons follow the desktop (see `desktop.rs`): which ones, on
//! which side and how they look (Windows, GNOME's Adwaita, KDE's Breeze).
//!
//! In fullscreen the bar hides and slides in when the pointer reaches the top
//! edge, like OneNote, or on a swipe down from the top edge (touch screens). On Windows 11 DWM rounds the corners of the normal
//! (not maximized) window, see `round_corners`.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    self, Align, Align2, CursorIcon, Id, LayerId, Layout, Order, PointerButton, Rect, ResizeDirection, RichText, Sense,
    Stroke, StrokeKind, Ui, ViewportCommand, pos2, vec2,
};
use egui_phosphor::regular as icon;

use crate::desktop::{self, Button, Style};
use crate::theme::{self, color, size, space};
use crate::win_frame;

/// Height of the custom title bar.
pub const TITLE_BAR_HEIGHT: f32 = 32.0;
/// Width of the invisible resize borders.
const RESIZE_BORDER: f32 = 5.0;
/// Fullscreen: how close to the top edge the pointer reveals the bar, and
/// how far below it the bar stays.
const REVEAL_EDGE: f32 = 2.0;
const KEEP_BELOW: f32 = 12.0;
/// Slide in / out time of the fullscreen bar, in seconds.
const REVEAL_TIME: f32 = 0.15;
/// Fullscreen: a swipe that reveals the bar starts this close to the top
/// edge and goes at least this far down.
const SWIPE_EDGE: f32 = 24.0;
const SWIPE_DOWN: f32 = 48.0;

/// DWM rounds the corners and draws the window's border (Windows 11).
static DWM_FRAME: AtomicBool = AtomicBool::new(false);

/// Window state the chrome depends on.
#[derive(Clone, Copy)]
pub struct WindowState {
    /// Draw the custom title bar (the system's is off).
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

    /// Window buttons, dragging and resizing are ours (not in fullscreen,
    /// nor on the web: a browser tab has no window of its own).
    fn frame_controls(&self) -> bool {
        self.custom && !self.fullscreen && !cfg!(target_arch = "wasm32")
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
/// A swipe down from the top edge shows it until a press below it (a finger
/// leaves no pointer to stay near the bar).
fn fullscreen_reveal(ctx: &egui::Context, pinned: bool) -> f32 {
    let id = Id::unique("title_bar_reveal");
    let swipe_id = Id::unique("title_bar_swiped");
    let shown = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let mut swiped = ctx.data(|d| d.get_temp::<bool>(swipe_id)).unwrap_or(false);
    let top = ctx.viewport_rect().top();
    let (pointer, origin, down, pressed) = ctx.input(|i| {
        (i.pointer.hover_pos(), i.pointer.press_origin(), i.pointer.primary_down(), i.pointer.any_pressed())
    });
    let near = pointer.is_some_and(|p| {
        let reach = if shown { TITLE_BAR_HEIGHT + KEEP_BELOW } else { REVEAL_EDGE };
        p.y <= top + reach
    });
    if let (Some(o), Some(p), true) = (origin, pointer, down)
        && o.y <= top + SWIPE_EDGE
        && p.y - o.y >= SWIPE_DOWN
    {
        swiped = true;
    } else if pressed && origin.is_some_and(|o| o.y > top + TITLE_BAR_HEIGHT + KEEP_BELOW) {
        swiped = false;
    }
    let show = pinned || near || swiped;
    ctx.data_mut(|d| {
        d.insert_temp(id, show);
        d.insert_temp(swipe_id, swiped);
    });
    ctx.animate_bool_with_time(id, show, REVEAL_TIME)
}

/// Out of full screen: the full screen bar is hidden, at once, so going full
/// screen does not start by sliding it away.
fn hide_fullscreen_bar(ctx: &egui::Context) {
    let id = Id::unique("title_bar_reveal");
    ctx.data_mut(|d| {
        d.insert_temp(id, false);
        d.insert_temp(Id::unique("title_bar_swiped"), false);
    });
    ctx.animate_bool_with_time(id, false, 0.0);
}

/// What the title bar reports.
#[derive(Default)]
pub struct TitleBarResponse {
    /// The text field of an editable title.
    pub title: Option<egui::Response>,
    /// The full screen button was clicked: left of the window buttons (into
    /// full screen), or in full screen where they usually are (back to the
    /// window).
    pub toggle_fullscreen: bool,
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
    let shown = if state.fullscreen {
        fullscreen_reveal(ctx, pinned)
    } else {
        hide_fullscreen_bar(ctx);
        1.0
    };
    if shown <= 0.0 {
        return TitleBarResponse::default();
    }
    let mut response = TitleBarResponse::default();
    let buttons = desktop::window_buttons(ctx);
    let (_, edge) = button_size(buttons.style);
    egui::Panel::top("title_bar")
        .exact_size(height * shown)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(color::chrome()))
        .show(ui, |ui| {
            let panel = ui.max_rect();
            let bar = Rect::from_min_size(panel.min - vec2(0.0, height * (1.0 - shown)), vec2(panel.width(), height));
            let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(bar));
            ui.set_clip_rect(panel.intersect(ui.clip_rect()));
            let ui = &mut ui;
            // The background first, so every widget added later wins the hit
            // test. On Windows the system moves the window (`win_frame`).
            if controls && !win_frame::active() {
                let resp = ui.interact(bar, Id::unique("title_bar_drag"), Sense::click_and_drag());
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
                // Only the app's mark: not text to select.
                let mark = RichText::new(icon::PEN_NIB).size(size::ICON).color(color::accent());
                ui.add(egui::Label::new(mark).selectable(false));
                ui.add_space(space::XS);
                egui::MenuBar::new().ui(ui, |ui| {
                    // Menu titles: flat, muted until hovered or open.
                    theme::ghost_buttons(ui);
                    ui.visuals_mut().widgets.inactive.fg_stroke.color = color::text_muted();
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
            // Right to left: the window buttons (the last first), then left of
            // them the full screen button; in full screen only the button
            // back, where the window buttons were.
            let layout = Layout::right_to_left(Align::Center);
            ui.scope_builder(egui::UiBuilder::new().max_rect(bar).layout(layout), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(edge);
                let glyph = if state.fullscreen {
                    icon::ARROWS_IN_SIMPLE
                } else {
                    if controls {
                        window_buttons(ui, buttons.style, buttons.right.iter().rev().copied(), state, height);
                    }
                    icon::ARROWS_OUT_SIMPLE
                };
                let toggle = window_button(ui, buttons.style, glyph, false, height);
                response.toggle_fullscreen = toggle.clicked();
                buttons_left = ui.min_rect().left();
            });

            // Centred title, moved aside (or dropped) when the window is too narrow.
            let (min_x, max_x) = (menus_right + 16.0, buttons_left - 16.0);
            let place = |w: f32| (max_x - min_x >= w).then(|| (bar.center().x - w * 0.5).clamp(min_x, max_x - w));
            match title {
                Title::Text(text) => {
                    let color = color::text_muted();
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

            // Windows: the bar is grabbed outside its widgets; not while a
            // menu is open or the title edited (a click there closes them).
            let mut holes = vec![
                Rect::from_min_max(bar.min, pos2(menus_right, bar.bottom())),
                Rect::from_min_max(pos2(buttons_left, bar.top()), bar.max),
            ];
            holes.extend(response.title.as_ref().map(|r| r.rect));
            let grab = controls && !pinned;
            win_frame::publish_caption(ctx, win_frame::Caption { bar: grab.then_some(bar), holes });
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
    let text_w = ui.painter().layout_no_wrap(shown.to_owned(), font.clone(), color::text()).size().x;
    let w = (text_w + 2.0 * space::L).clamp(160.0, 420.0);
    // Narrower (but still editable) when the window is tight, hidden when it
    // does not fit at all.
    let (x, w) = match place(w) {
        Some(x) => (x, w),
        None => (place(120.0)?, 120.0),
    };
    let rect = Rect::from_min_size(pos2(x, bar.center().y - 12.0), vec2(w, 24.0));

    let resp = theme::inline_edit(
        ui,
        rect,
        Id::unique("note_title_edit"),
        text,
        hint,
        font,
        Align::Center,
        color::text_muted(),
    );
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
    let mut color = color::text_muted();
    let glyph_size = match style {
        Style::Windows => {
            if resp.hovered() {
                painter.rect_filled(rect, 0.0, if close { color::danger() } else { color::hover() });
                color = if close { color::on_accent() } else { color::text_strong() };
            }
            15.0
        }
        // Always a filled circle, a bit lighter when hovered.
        Style::Adwaita => {
            let fill = if pressed {
                color::pressed()
            } else if resp.hovered() {
                color::hover()
            } else {
                color::raised()
            };
            painter.circle_filled(rect.center(), 12.0, fill);
            color = color::text();
            13.0
        }
        // A circle only when hovered: red behind the close button.
        Style::Breeze => {
            if resp.hovered() || pressed {
                let fill = if close {
                    color::danger()
                } else if pressed {
                    color::pressed()
                } else {
                    color::hover()
                };
                painter.circle_filled(rect.center(), 10.0, fill);
                color = if close { color::on_accent() } else { color::text_strong() };
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
    let resizable = state.frame_controls() && !state.maximized;
    win_frame::publish_border(ctx, resizable.then_some(RESIZE_BORDER));
    if !resizable {
        return;
    }
    // Windows draws the frame and resizes the window itself (`win_frame`).
    // Windows 10 draws no border along the top, where the caption was.
    if win_frame::active() {
        if !DWM_FRAME.load(Ordering::Relaxed) {
            let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("window_outline")));
            let r = ctx.viewport_rect();
            let y = r.top() + 0.5;
            painter.hline(r.x_range(), y, Stroke::new(1.0, color::border_strong()));
        }
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
        egui::Area::new(Id::unique("resize_border").with(i))
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
    paint_outline(ctx);
}

/// The window's 1 px outline, unless DWM draws the (rounded) border itself.
fn paint_outline(ctx: &egui::Context) {
    if !DWM_FRAME.load(Ordering::Relaxed) {
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("window_outline")));
        let r = ctx.viewport_rect();
        painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, color::border_strong()), StrokeKind::Middle);
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

/// Windows: in fullscreen the system takes a swipe in from the screen's
/// edges for its own gestures (the top one only shows a grabber, the left
/// one the widgets), so the swipes that show the title bar and the sidebar
/// never arrived. This hands them to the app, in fullscreen only, the swipe
/// up for the taskbar too (see `show_taskbar`). Does nothing elsewhere.
pub fn own_edge_swipes(frame: &eframe::Frame) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        edge_gesture::disable_when_fullscreen(h.hwnd.get());
        dwm::WINDOW.store(h.hwnd.get(), Ordering::Relaxed);
    }
    #[cfg(not(windows))]
    let _ = frame;
}

/// Windows, fullscreen: shows the taskbar of the window's screen, as the
/// system's swipe up from the bottom edge does (taken from it by
/// `own_edge_swipes`). The taskbar is brought to the foreground, over the
/// window; a tap back on the window hides it again. Does nothing elsewhere.
pub fn show_taskbar() {
    #[cfg(windows)]
    taskbar::show(dwm::WINDOW.load(Ordering::Relaxed));
}

/// Windows: turns the window's own animations (maximize, restore) off or
/// on. Going in and out of full screen takes two steps (see
/// `Command::Fullscreen`), and animated the window visibly shrank and grew
/// back in between. Does nothing elsewhere.
pub fn window_transitions(enabled: bool) {
    #[cfg(windows)]
    {
        let hwnd = dwm::WINDOW.load(Ordering::Relaxed);
        if hwnd != 0 {
            dwm::set(hwnd, dwm::TRANSITIONS_FORCEDISABLED, u32::from(!enabled));
        }
    }
    #[cfg(not(windows))]
    let _ = enabled;
}

/// Windows: un-maximizes the window without it changing size (it keeps the
/// screen's work area), and returns its own size un-maximized (in the
/// system's workspace coordinates) for `set_normal_rect`. `None`: not done
/// (not Windows, or not maximized).
pub fn unmaximize_in_place() -> Option<[i32; 4]> {
    #[cfg(windows)]
    {
        placement::unmaximize_in_place(dwm::WINDOW.load(Ordering::Relaxed))
    }
    #[cfg(not(windows))]
    None
}

/// Windows: the size the maximized window gets when un-maximized, as
/// returned by `unmaximize_in_place`.
pub fn set_normal_rect(rect: [i32; 4]) {
    #[cfg(windows)]
    placement::set_normal_rect(dwm::WINDOW.load(Ordering::Relaxed), rect);
    #[cfg(not(windows))]
    let _ = rect;
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
    pub const TRANSITIONS_FORCEDISABLED: u32 = 3;
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }

    /// Our window, once `round_corners` has run.
    pub static HWND: AtomicIsize = AtomicIsize::new(0);
    /// Our window, on any Windows version (see `own_edge_swipes`).
    pub static WINDOW: AtomicIsize = AtomicIsize::new(0);
    /// The border colour is currently off for fullscreen.
    pub static FULLSCREEN: AtomicBool = AtomicBool::new(false);

    pub fn set(hwnd: isize, attribute: u32, value: u32) -> bool {
        // SAFETY: a valid HWND of our own window and a u32 value, as the
        // attributes expect.
        unsafe { DwmSetWindowAttribute(hwnd, attribute, (&raw const value).cast(), 4) == 0 }
    }

    /// The outline colour as a COLORREF (0x00BBGGRR).
    pub fn outline() -> u32 {
        let [r, g, b, _] = color::border_strong().to_array();
        u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16
    }
}

/// The window's placement (maximized or not, its size un-maximized), in
/// user32's terms.
#[cfg(windows)]
mod placement {
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct WindowPlacement {
        length: u32,
        flags: u32,
        show_cmd: u32,
        min_position: Point,
        max_position: Point,
        normal_position: Rect,
    }

    #[repr(C)]
    #[derive(Default)]
    struct MonitorInfo {
        size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }

    const SW_SHOWMAXIMIZED: u32 = 3;
    const SW_RESTORE: i32 = 9;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowPlacement(hwnd: isize, placement: *mut WindowPlacement) -> i32;
        fn SetWindowPlacement(hwnd: isize, placement: *const WindowPlacement) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
        fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    }

    fn get(hwnd: isize) -> Option<WindowPlacement> {
        let mut p = WindowPlacement { length: size_of::<WindowPlacement>() as u32, ..Default::default() };
        // SAFETY: our own window; `p` is a WINDOWPLACEMENT with its length set.
        (hwnd != 0 && unsafe { GetWindowPlacement(hwnd, &mut p) } != 0).then_some(p)
    }

    pub fn unmaximize_in_place(hwnd: isize) -> Option<[i32; 4]> {
        let p = get(hwnd)?;
        if p.show_cmd != SW_SHOWMAXIMIZED {
            return None;
        }
        let mut info = MonitorInfo { size: size_of::<MonitorInfo>() as u32, ..Default::default() };
        // SAFETY: our own window, on its thread; `info` has its size set.
        unsafe {
            if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) == 0 {
                return None;
            }
            let w = info.work;
            ShowWindow(hwnd, SW_RESTORE);
            SetWindowPos(hwnd, 0, w.left, w.top, w.right - w.left, w.bottom - w.top, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let n = p.normal_position;
        Some([n.left, n.top, n.right, n.bottom])
    }

    pub fn set_normal_rect(hwnd: isize, [left, top, right, bottom]: [i32; 4]) {
        let Some(mut p) = get(hwnd) else { return };
        p.normal_position = Rect { left, top, right, bottom };
        // SAFETY: our own window; `p` as read, its normal size changed.
        unsafe {
            SetWindowPlacement(hwnd, &p);
        }
    }
}

/// The taskbars: the main one (`Shell_TrayWnd`) and one per other screen
/// (`Shell_SecondaryTrayWnd`), declared here so they need no optional
/// windows-sys feature.
#[cfg(windows)]
mod taskbar {
    const MONITOR_DEFAULTTONEAREST: u32 = 2;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn FindWindowExW(parent: isize, after: isize, class: *const u16, title: *const u16) -> isize;
        fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
        fn SetForegroundWindow(hwnd: isize) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// The taskbar on the screen of `window` (the main one if none is).
    fn of(window: isize) -> Option<isize> {
        let (main, secondary) = (wide("Shell_TrayWnd"), wide("Shell_SecondaryTrayWnd"));
        // SAFETY: null-terminated class names; the handles are only compared
        // and passed back to user32.
        unsafe {
            let screen = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
            let primary = FindWindowExW(0, 0, main.as_ptr(), core::ptr::null());
            if primary != 0 && MonitorFromWindow(primary, MONITOR_DEFAULTTONEAREST) == screen {
                return Some(primary);
            }
            let mut bar = 0;
            loop {
                bar = FindWindowExW(0, bar, secondary.as_ptr(), core::ptr::null());
                if bar == 0 {
                    break;
                }
                if MonitorFromWindow(bar, MONITOR_DEFAULTTONEAREST) == screen {
                    return Some(bar);
                }
            }
            (primary != 0).then_some(primary)
        }
    }

    pub fn show(window: isize) {
        if window == 0 {
            return;
        }
        if let Some(bar) = of(window) {
            // SAFETY: a top-level window handle from FindWindowExW. Allowed:
            // the swipe made this process the one with the last input.
            unsafe { SetForegroundWindow(bar) };
        }
    }
}

/// The window's `System.EdgeGesture.DisableTouchWhenFullscreen` property,
/// set through its shell property store (`IPropertyStore`, called through
/// its vtable: windows-sys has no COM interfaces).
#[cfg(windows)]
mod edge_gesture {
    use core::ffi::c_void;

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);

    #[repr(C)]
    struct PropertyKey {
        fmtid: Guid,
        pid: u32,
    }

    /// A PROPVARIANT holding a VT_BOOL.
    #[repr(C)]
    struct PropVariant {
        vt: u16,
        reserved: [u16; 3],
        value: [u64; 2],
    }

    #[repr(C)]
    struct PropertyStoreVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut PropertyStore) -> u32,
        get_count: usize,
        get_at: usize,
        get_value: usize,
        set_value: unsafe extern "system" fn(*mut PropertyStore, *const PropertyKey, *const PropVariant) -> i32,
        commit: usize,
    }

    #[repr(C)]
    struct PropertyStore {
        vtbl: *const PropertyStoreVtbl,
    }

    // propkey.h / propsys.h
    const IID_IPROPERTYSTORE: Guid =
        Guid(0x886D_8EEB, 0x8CF2, 0x4446, [0x8D, 0x02, 0xCD, 0xBA, 0x1D, 0xBD, 0xCF, 0x99]);
    const PKEY_EDGEGESTURE_DISABLETOUCHWHENFULLSCREEN: PropertyKey = PropertyKey {
        fmtid: Guid(0x32CE_38B2, 0x2C9A, 0x41B1, [0x9B, 0xC5, 0xB3, 0x78, 0x43, 0x94, 0xAA, 0x44]),
        pid: 2,
    };
    const VT_BOOL: u16 = 11;
    const VARIANT_TRUE: u64 = 0xFFFF;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetPropertyStoreForWindow(hwnd: isize, riid: *const Guid, ppv: *mut *mut c_void) -> i32;
    }

    pub fn disable_when_fullscreen(hwnd: isize) {
        let mut store: *mut c_void = core::ptr::null_mut();
        // SAFETY: a valid HWND of our own window, on its thread; the store
        // is used through IPropertyStore's vtable and released once.
        unsafe {
            if SHGetPropertyStoreForWindow(hwnd, &IID_IPROPERTYSTORE, &mut store) < 0 || store.is_null() {
                return;
            }
            let store = store.cast::<PropertyStore>();
            let value = PropVariant { vt: VT_BOOL, reserved: [0; 3], value: [VARIANT_TRUE, 0] };
            ((*(*store).vtbl).set_value)(store, &PKEY_EDGEGESTURE_DISABLETOUCHWHENFULLSCREEN, &value);
            ((*(*store).vtbl).release)(store);
        }
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
                exit |= title_bar(ui, state, false, Title::Text("t"), |_| {}).toggle_fullscreen;
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
    fn fullscreen_bar_shows_after_a_swipe_down_until_a_press_below() {
        let ctx = egui::Context::default();
        let mut open = false;
        let shown = |ctx: &egui::Context| ctx.data(|d| d.get_temp::<bool>(Id::unique("title_bar_reveal")));
        frame(&ctx, &mut open, vec![at(300.0, 300.0)]);
        // A finger lands on the top edge (not at it, as a mouse would be) and swipes down.
        frame(&ctx, &mut open, vec![at(300.0, 10.0), button(300.0, 10.0, true)]);
        for y in [30.0, 50.0, 70.0] {
            frame(&ctx, &mut open, vec![at(300.0, y)]);
        }
        frame(&ctx, &mut open, vec![button(300.0, 70.0, false), Event::PointerGone]);
        for _ in 0..30 {
            frame(&ctx, &mut open, vec![]);
        }
        assert_eq!(shown(&ctx), Some(true), "the swipe shows the bar");
        // A tap well below the bar hides it.
        frame(&ctx, &mut open, vec![at(300.0, 300.0), button(300.0, 300.0, true)]);
        frame(&ctx, &mut open, vec![button(300.0, 300.0, false), Event::PointerGone]);
        assert_eq!(shown(&ctx), Some(false), "a press below hides it");
    }

    #[test]
    fn a_button_left_of_the_window_buttons_goes_full_screen() {
        let ctx = egui::Context::default();
        let mut toggled = false;
        let mut run = |events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                time: Some(ctx.input(|i| i.time) + 1.0 / 60.0),
                events,
                ..Default::default()
            };
            crate::test_frame(&ctx, input, |ui| {
                let state = WindowState::of(ui.ctx(), true);
                toggled |= title_bar(ui, state, false, Title::Text("t"), |_| {}).toggle_fullscreen;
                egui::CentralPanel::default().show(ui, |_| {});
            });
        };
        let buttons = desktop::window_buttons(&ctx);
        let (width, edge) = button_size(buttons.style);
        // Right of the window buttons when they are on the left.
        let x = 800.0 - edge - width * (buttons.right.len() as f32 + 0.5);
        let y = 16.0;
        run(vec![at(x, y)]);
        run(vec![button(x, y, true)]);
        run(vec![button(x, y, false)]);
        assert!(toggled);
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
