// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Window chrome drawn by the app itself (VS Code style): a title bar with the
//! menu bar, the window title and the minimize / maximize / close buttons,
//! plus resize borders and a thin window outline.
//!
//! With the custom frame the system decorations are turned off, so moving and
//! resizing go through `ViewportCommand::StartDrag` / `BeginResize`, which hand
//! the gesture to the OS (snapping keeps working). With the native frame only
//! the menu bar is drawn, below the system title bar.

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CursorIcon, Id, LayerId, Layout, Order,
    PointerButton, Rect, ResizeDirection, RichText, Sense, Stroke, Ui, ViewportCommand,
};
use egui_phosphor::regular as icon;

use crate::theme::{self, color, size, space};

/// Height of the custom title bar.
pub const TITLE_BAR_HEIGHT: f32 = 32.0;
const WINDOW_BUTTON_WIDTH: f32 = 46.0;
/// Width of the invisible resize borders.
const RESIZE_BORDER: f32 = 5.0;
const CLOSE_HOVER: Color32 = color::DANGER;

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
    Editable { text: &'a mut String, hint: &'a str },
}

/// Shows the top bar. Call it before any other panel so it spans the whole
/// window. `menus` adds the menu buttons (inside an `egui::menu::bar`).
/// Returns the text field's response for an editable title.
pub fn title_bar(
    ctx: &egui::Context,
    state: WindowState,
    title: Title<'_>,
    menus: impl FnOnce(&mut Ui),
) -> Option<egui::Response> {
    let controls = state.frame_controls();
    // Same height with or without the window buttons: the menu buttons need
    // it, and a lower bar would leave a gap below its painted background.
    let height = TITLE_BAR_HEIGHT;
    let mut response = None;
    egui::TopBottomPanel::top("title_bar")
        .exact_height(height)
        .show_separator_line(false)
        .frame(egui::Frame::none().fill(color::CHROME))
        .show(ctx, |ui| {
            let bar = ui.max_rect();
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
                ui.add_space(space::M);
                ui.label(RichText::new(icon::PEN_NIB).size(size::ICON).color(color::ACCENT));
                ui.add_space(space::XS);
                egui::menu::bar(ui, |ui| {
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
            if controls {
                let layout = Layout::right_to_left(Align::Center);
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bar).layout(layout), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    window_buttons(ui, state, height);
                    buttons_left = ui.min_rect().left();
                });
            }

            // Centred title, moved aside (or dropped) when the window is too narrow.
            let (min_x, max_x) = (menus_right + 16.0, buttons_left - 16.0);
            let place = |w: f32| {
                (max_x - min_x >= w).then(|| (bar.center().x - w * 0.5).clamp(min_x, max_x - w))
            };
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
                    response = editable_title(ui, bar, text, hint, &place);
                }
            }
        });
    response
}

/// Minimize, maximize / restore and close. No tooltips: the icons are
/// standard, and a tooltip would open over the editor's top-right notch.
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

    let resp = theme::inline_edit(
        ui,
        rect,
        Id::new("note_title_edit"),
        text,
        hint,
        font,
        Align::Center,
        color::TEXT_MUTED,
    );
    Some(resp.on_hover_text("Rename the note"))
}

fn window_buttons(ui: &mut Ui, state: WindowState, height: f32) {
    let ctx = ui.ctx().clone();
    if window_button(ui, icon::X, CLOSE_HOVER, height).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    let glyph = if state.maximized { icon::COPY } else { icon::SQUARE };
    let hover = color::HOVER;
    if window_button(ui, glyph, hover, height).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!state.maximized));
    }
    if window_button(ui, icon::MINUS, hover, height).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

fn window_button(ui: &mut Ui, glyph: &str, hover_fill: Color32, height: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(WINDOW_BUTTON_WIDTH, height), Sense::click());
    let mut color = color::TEXT_MUTED;
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, hover_fill);
        color = if hover_fill == CLOSE_HOVER { color::ON_ACCENT } else { color::TEXT_STRONG };
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, theme::font(15.0), color);
    resp
}

/// Resize borders along the window edges and a 1 px outline. Call it last,
/// so the borders are on top of the other floating elements.
pub fn window_frame(ctx: &egui::Context, state: WindowState) {
    if !state.frame_controls() || state.maximized {
        return;
    }
    let r = ctx.screen_rect();
    let (b, c) = (RESIZE_BORDER, RESIZE_BORDER * 2.0);
    let zones = [
        (ResizeDirection::NorthWest, Rect::from_min_size(r.left_top(), vec2(c, c)), CursorIcon::ResizeNorthWest),
        (ResizeDirection::NorthEast, Rect::from_min_size(pos2(r.right() - c, r.top()), vec2(c, c)), CursorIcon::ResizeNorthEast),
        (ResizeDirection::SouthWest, Rect::from_min_size(pos2(r.left(), r.bottom() - c), vec2(c, c)), CursorIcon::ResizeSouthWest),
        (ResizeDirection::SouthEast, Rect::from_min_size(r.right_bottom() - vec2(c, c), vec2(c, c)), CursorIcon::ResizeSouthEast),
        (ResizeDirection::North, Rect::from_min_max(pos2(r.left() + c, r.top()), pos2(r.right() - c, r.top() + b)), CursorIcon::ResizeNorth),
        (ResizeDirection::South, Rect::from_min_max(pos2(r.left() + c, r.bottom() - b), pos2(r.right() - c, r.bottom())), CursorIcon::ResizeSouth),
        (ResizeDirection::West, Rect::from_min_max(pos2(r.left(), r.top() + c), pos2(r.left() + b, r.bottom() - c)), CursorIcon::ResizeWest),
        (ResizeDirection::East, Rect::from_min_max(pos2(r.right() - b, r.top() + c), pos2(r.right(), r.bottom() - c)), CursorIcon::ResizeEast),
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
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("window_outline")));
    painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, color::BORDER_STRONG));
}
