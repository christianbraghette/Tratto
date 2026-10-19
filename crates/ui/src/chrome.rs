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
    self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Id, LayerId, Layout, Order,
    PointerButton, Rect, ResizeDirection, RichText, Sense, Stroke, Ui, ViewportCommand,
};
use egui_phosphor::regular as icon;

/// Height of the custom title bar.
pub const TITLE_BAR_HEIGHT: f32 = 32.0;
/// Height of the bar when it only holds the menus (native frame, fullscreen).
const MENU_BAR_HEIGHT: f32 = 26.0;
const WINDOW_BUTTON_WIDTH: f32 = 46.0;
/// Width of the invisible resize borders.
const RESIZE_BORDER: f32 = 5.0;
const CLOSE_HOVER: Color32 = Color32::from_rgb(196, 43, 28);
const ACCENT: Color32 = Color32::from_rgb(40, 120, 255);

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

/// Shows the top bar. Call it before any other panel so it spans the whole
/// window. `menus` adds the menu buttons (inside an `egui::menu::bar`).
pub fn title_bar(ctx: &egui::Context, state: WindowState, title: &str, menus: impl FnOnce(&mut Ui)) {
    let controls = state.frame_controls();
    let height = if controls { TITLE_BAR_HEIGHT } else { MENU_BAR_HEIGHT };
    egui::TopBottomPanel::top("title_bar")
        .exact_height(height)
        .show_separator_line(false)
        .frame(egui::Frame::none().fill(ctx.style().visuals.window_fill()))
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
                ui.add_space(10.0);
                ui.label(RichText::new(icon::PEN_NIB).size(16.0).color(ACCENT));
                ui.add_space(2.0);
                egui::menu::bar(ui, menus);
                menus_right = ui.min_rect().right();
                if controls {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        window_buttons(ui, state, height);
                        buttons_left = ui.min_rect().left();
                    });
                }
            });

            // Centred title, moved aside (or dropped) when the window is too narrow.
            let color = ui.visuals().weak_text_color();
            let galley = ui.painter().layout_no_wrap(title.to_owned(), FontId::proportional(13.0), color);
            let (min_x, max_x) = (menus_right + 16.0, buttons_left - 16.0);
            let w = galley.size().x;
            if max_x - min_x >= w {
                let x = (bar.center().x - w * 0.5).clamp(min_x, max_x - w);
                let pos = pos2(x, bar.center().y - galley.size().y * 0.5);
                ui.painter().galley(pos, galley, color);
            }
        });
}

fn window_buttons(ui: &mut Ui, state: WindowState, height: f32) {
    let ctx = ui.ctx().clone();
    if window_button(ui, icon::X, CLOSE_HOVER, height).on_hover_text("Close").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    let (glyph, tip) = if state.maximized { (icon::COPY, "Restore") } else { (icon::SQUARE, "Maximize") };
    let hover = ui.visuals().widgets.hovered.weak_bg_fill;
    if window_button(ui, glyph, hover, height).on_hover_text(tip).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!state.maximized));
    }
    if window_button(ui, icon::MINUS, hover, height).on_hover_text("Minimize").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

fn window_button(ui: &mut Ui, glyph: &str, hover_fill: Color32, height: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(WINDOW_BUTTON_WIDTH, height), Sense::click());
    let mut color = ui.visuals().text_color();
    if resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, hover_fill);
        if hover_fill == CLOSE_HOVER {
            color = Color32::WHITE;
        }
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(15.0), color);
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
    painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, Color32::from_gray(70)));
}
