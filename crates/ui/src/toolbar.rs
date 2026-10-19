// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Floating pill-shaped toolbars.
//!
//! * the main toolbar (tools, styles, undo / redo) at the top or bottom centre
//! * library in a notch in the top-right corner
//! * pages panel toggle and settings in notches in the top-left and
//!   bottom-left corners, which become part of the pages panel when it is open
//!
//! Buttons show Phosphor icons with the name as a tooltip. Colours and sizes
//! come from [`crate::theme`].

use std::ops::RangeInclusive;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, Margin, Order, Pos2, Rect, Sense, Shape, Slider, Stroke,
};
use egui_phosphor::regular as icon;

use crate::scrollbar;
use crate::settings::{Settings, ToolStyle, ToolbarPosition};
use crate::theme::{self, color, radius, space};

/// Fixed distance between the toolbars and the window edge.
pub const MARGIN: f32 = space::L;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
    /// Lasso select + move.
    Select,
    /// Click to type text.
    Text,
}

pub enum ToolbarEvent {
    Undo,
    Redo,
    ToggleSettings,
    /// Back to the library.
    Library,
}

impl Tool {
    pub const ALL: [Tool; 5] = [Tool::Pen, Tool::Highlighter, Tool::Eraser, Tool::Select, Tool::Text];

    pub fn icon(self) -> &'static str {
        match self {
            Tool::Pen => icon::PEN_NIB,
            Tool::Highlighter => icon::HIGHLIGHTER,
            Tool::Eraser => icon::ERASER,
            Tool::Select => icon::LASSO,
            Tool::Text => icon::TEXT_T,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen => "Pen",
            Tool::Highlighter => "Highlighter",
            Tool::Eraser => "Eraser",
            Tool::Select => "Select (lasso)",
            Tool::Text => "Text",
        }
    }
}

const PEN_PALETTE: [[u8; 3]; 6] = [
    [20, 20, 20],
    [30, 80, 220],
    [210, 40, 40],
    [30, 150, 70],
    [240, 140, 20],
    [130, 50, 170],
];
const HIGHLIGHTER_PALETTE: [[u8; 3]; 5] = [
    [255, 235, 0],
    [120, 230, 90],
    [255, 140, 200],
    [100, 210, 255],
    [255, 170, 60],
];

/// Shows the toolbar at the top or bottom edge; returns a clicked action.
///
/// `bottom_inset` keeps it clear of the horizontal scrollbar when that is visible.
pub fn show(
    ctx: &egui::Context,
    tool: &mut Tool,
    settings: &mut Settings,
    can_undo: bool,
    can_redo: bool,
    bottom_inset: f32,
) -> Option<ToolbarEvent> {
    // Centred over the canvas, below the title bar (call after the title bar
    // and the side panels), so it never covers the menus or the pages panel.
    let free = ctx.available_rect();
    let id = egui::Id::new("floating_toolbar");
    // When the window is too narrow for the toolbar to fit between the corner
    // notches, it moves past them (size from the previous frame).
    let width = ctx.memory(|m| m.area_rect(id)).map_or(0.0, |r| r.width());
    let clear = if width + 2.0 * CORNER_ZONE > free.width() { NOTCH_HEIGHT } else { 0.0 };
    let (pivot, pos) = match settings.toolbar_position {
        ToolbarPosition::Top => {
            (Align2::CENTER_TOP, pos2(free.center().x, free.top() + MARGIN + clear))
        }
        ToolbarPosition::Bottom => (
            Align2::CENTER_BOTTOM,
            pos2(free.center().x, free.bottom() - MARGIN - bottom_inset - clear),
        ),
    };

    let mut event = None;
    egui::Area::new(id)
        .order(Order::Foreground)
        .pivot(pivot)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            theme::floating_frame().show(ui, |ui| {
                tool_buttons_style(ui);
                ui.horizontal(|ui| event = contents(ui, tool, settings, can_undo, can_redo));
            })
        });
    event
}

/// The editor's corner buttons:
///
/// * library in a notch in the top-right corner (against the scrollbar)
/// * pages panel toggle in a notch in the top-left corner
/// * settings in a notch in the bottom-left corner
///
/// While the pages panel is open, the pages and settings buttons live in the
/// panel instead (see [`pages_button`], [`settings_button`]) at the same
/// positions, and the panel gets the same concave joins as the notches, so it
/// reads as the two notches grown into a sidebar.
///
/// Call after the side panels: `available_rect` is the canvas area then.
/// `bottom_inset` keeps the bottom notch above the horizontal scrollbar.
pub fn show_corners(
    ctx: &egui::Context,
    settings: &mut Settings,
    settings_open: bool,
    bottom_inset: f32,
) -> Option<ToolbarEvent> {
    let free = ctx.available_rect();
    let mut event = None;

    // Against the vertical scrollbar, which always occupies the right edge.
    let right = pos2(free.right() - scrollbar::THICKNESS, free.top());
    notch(ctx, "corner_notch_library", Corner::TopRight, right, |ui| {
        if ui
            .button(theme::icon(icon::BOOKS))
            .on_hover_text("Library: save and go back to your notes")
            .clicked()
        {
            event = Some(ToolbarEvent::Library);
        }
    });

    if settings.pages_panel_open {
        // Join the panel to the title bar and to the bottom edge. Above the
        // canvas, but below menus and popups (which are foreground areas).
        let layer = egui::LayerId::new(Order::Middle, egui::Id::new("pages_panel_joins"));
        let painter = ctx.layer_painter(layer);
        painter.add(fillet(free.left_top(), 1.0, 1.0, color::CHROME));
        painter.add(fillet(free.left_bottom(), 1.0, -1.0, color::CHROME));
    } else {
        notch(ctx, "corner_notch_pages", Corner::TopLeft, free.left_top(), |ui| {
            pages_button(ui, &mut settings.pages_panel_open);
        });
        let bottom_left = pos2(free.left(), free.bottom() - bottom_inset);
        notch(ctx, "corner_notch_settings", Corner::BottomLeft, bottom_left, |ui| {
            if settings_button(ui, settings_open) {
                event = Some(ToolbarEvent::ToggleSettings);
            }
        });
    }
    event
}

/// Toggles the pages panel (in its notch, or in the panel header).
pub fn pages_button(ui: &mut egui::Ui, open: &mut bool) {
    let tip = if *open { "Hide pages" } else { "Pages: jump, reorder, delete" };
    ui.toggle_value(open, theme::icon(icon::SIDEBAR_SIMPLE)).on_hover_text(tip);
}

/// Opens or closes the settings (in its notch, or in the panel footer).
/// Returns true when clicked.
pub fn settings_button(ui: &mut egui::Ui, open: bool) -> bool {
    ui.add(egui::SelectableLabel::new(open, theme::icon(icon::GEAR_SIX)))
        .on_hover_text("Settings")
        .clicked()
}

/// Margins around the buttons of a notch attached to the top edge (`top`) or
/// to the bottom edge. The pages panel uses the same ones, so its buttons sit
/// exactly where the notch buttons were.
pub fn notch_margin(top: bool) -> Margin {
    // A little more room on the attached side keeps the buttons clear of the
    // title bar (and its window buttons) and of the bottom edge.
    let (attached, free) = (space::S, 6.0);
    Margin {
        left: space::S,
        right: space::S,
        top: if top { attached } else { free },
        bottom: if top { free } else { attached },
    }
}

/// Button style of the notches, for the same buttons inside the pages panel.
pub fn notch_buttons_style(ui: &mut egui::Ui) {
    tool_buttons_style(ui);
}

/// The window corner a notch is attached to.
#[derive(Clone, Copy, PartialEq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
}

impl Corner {
    /// Mirroring of the top-left shape: (x, y), each 1.0 or -1.0.
    fn signs(self) -> (f32, f32) {
        match self {
            Corner::TopLeft => (1.0, 1.0),
            Corner::TopRight => (-1.0, 1.0),
            Corner::BottomLeft => (1.0, -1.0),
        }
    }
}

/// Radius of the notch's free corner (same as the floating toolbar).
const NOTCH_ROUNDING: f32 = radius::LARGE;
/// Height of a notch with one button, and the width kept free for the
/// notches (plus the scrollbar) at each side of the floating toolbar.
const NOTCH_HEIGHT: f32 = 48.0;
const CORNER_ZONE: f32 = 80.0;
/// Radius of the concave joins between the notch and the window edges.
const NOTCH_FILLET: f32 = 10.0;

/// A tab attached to the top or bottom edge, flush with the side edge, with
/// its outer corner at `corner`.
fn notch(
    ctx: &egui::Context,
    id: &str,
    at: Corner,
    corner: Pos2,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let pivot = match at {
        Corner::TopLeft => Align2::LEFT_TOP,
        Corner::TopRight => Align2::RIGHT_TOP,
        Corner::BottomLeft => Align2::LEFT_BOTTOM,
    };
    egui::Area::new(egui::Id::new(id))
        .order(Order::Foreground)
        .pivot(pivot)
        .fixed_pos(corner)
        .show(ctx, |ui| {
            // Reserve the background slot first so it is painted below the buttons.
            let background = ui.painter().add(Shape::Noop);
            let rect = egui::Frame::none()
                .inner_margin(notch_margin(at != Corner::BottomLeft))
                .show(ui, |ui| {
                    tool_buttons_style(ui);
                    ui.horizontal(add_contents);
                })
                .response
                .rect;
            // Same surface as the title bar it hangs from.
            ui.painter().set(background, notch_shape(rect, at, color::CHROME));
        });
}

/// The notch outline as one polygon: the body with its free corner rounded,
/// plus concave fillets that blend it into the two window edges.
///
/// The outline is not convex, but every point is visible from the window
/// corner, its first vertex; epaint fans the fill out from the first vertex,
/// so it is filled correctly, and a single shape has no anti-aliasing seams.
fn notch_shape(rect: Rect, at: Corner, fill: Color32) -> Shape {
    let (big, small) = (NOTCH_ROUNDING, NOTCH_FILLET);
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    // Outline of the top-left notch, clockwise. The edges along the window
    // border reach 1 px past it, so their anti-aliasing fringe is not visible.
    let mut pts = vec![pos2(l - 1.0, t - 1.0), pos2(r + small, t - 1.0)];
    arc(&mut pts, pos2(r + small, t + small), small, -90.0, -180.0);
    arc(&mut pts, pos2(r - big, b - big), big, 0.0, 90.0);
    arc(&mut pts, pos2(l + small, b + small), small, -90.0, -180.0);
    pts.push(pos2(l - 1.0, b + small));

    let (sx, sy) = at.signs();
    for p in &mut pts {
        if sx < 0.0 {
            p.x = l + r - p.x;
        }
        if sy < 0.0 {
            p.y = t + b - p.y;
        }
    }
    Shape::convex_polygon(clockwise(pts), fill, Stroke::NONE)
}

/// A single concave join at `corner`, filling towards +x·`sx` and +y·`sy`
/// (like the fillets of a notch, without the notch).
fn fillet(corner: Pos2, sx: f32, sy: f32, fill: Color32) -> Shape {
    let r = NOTCH_FILLET;
    let mut pts = vec![pos2(-1.0, -1.0), pos2(r, -1.0)];
    arc(&mut pts, pos2(r, r), r, -90.0, -180.0);
    pts.push(pos2(-1.0, r));
    let pts = pts.into_iter().map(|p| corner + vec2(p.x * sx, p.y * sy)).collect();
    Shape::convex_polygon(clockwise(pts), fill, Stroke::NONE)
}

/// Makes the polygon clockwise on screen (as epaint prefers), keeping the
/// first vertex first (the fill fans out from it).
fn clockwise(mut pts: Vec<Pos2>) -> Vec<Pos2> {
    let n = pts.len();
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    if area < 0.0 {
        pts[1..].reverse();
    }
    pts
}

/// Appends a circular arc from angle `from` to `to` (degrees, y down).
fn arc(pts: &mut Vec<Pos2>, center: Pos2, radius: f32, from: f32, to: f32) {
    const SEGMENTS: usize = 8;
    for i in 0..=SEGMENTS {
        let a = (from + (to - from) * i as f32 / SEGMENTS as f32).to_radians();
        pts.push(center + radius * vec2(a.cos(), a.sin()));
    }
}

/// Square icon buttons without a fill at rest, shared by all toolbars.
fn tool_buttons_style(ui: &mut egui::Ui) {
    theme::ghost_buttons(ui);
    let s = ui.spacing_mut();
    s.item_spacing = vec2(space::XS, space::XS);
    s.button_padding = vec2(7.0, 5.0);
    s.interact_size = vec2(32.0, 32.0);
}

fn contents(
    ui: &mut egui::Ui,
    tool: &mut Tool,
    settings: &mut Settings,
    can_undo: bool,
    can_redo: bool,
) -> Option<ToolbarEvent> {
    let mut event = None;

    for t in Tool::ALL {
        ui.selectable_value(tool, t, theme::icon(t.icon())).on_hover_text(t.label());
    }
    ui.separator();

    ui.spacing_mut().slider_width = 100.0;
    match *tool {
        Tool::Pen => style_controls(ui, &mut settings.pen, &PEN_PALETTE, 0.5..=12.0),
        Tool::Highlighter => {
            style_controls(ui, &mut settings.highlighter, &HIGHLIGHTER_PALETTE, 4.0..=40.0)
        }
        Tool::Eraser => {
            ui.add(Slider::new(&mut settings.eraser_radius, 4.0..=60.0).show_value(false))
                .on_hover_text("Eraser size");
        }
        Tool::Select => {
            ui.label(theme::muted("Lasso, then drag"));
        }
        Tool::Text => style_controls(ui, &mut settings.text, &PEN_PALETTE, 8.0..=96.0),
    }
    ui.separator();

    let undo = egui::Button::new(theme::icon(icon::ARROW_U_UP_LEFT));
    if ui.add_enabled(can_undo, undo).on_hover_text("Undo (Ctrl+Z)").clicked() {
        event = Some(ToolbarEvent::Undo);
    }
    let redo = egui::Button::new(theme::icon(icon::ARROW_U_UP_RIGHT));
    if ui.add_enabled(can_redo, redo).on_hover_text("Redo (Ctrl+Y)").clicked() {
        event = Some(ToolbarEvent::Redo);
    }
    event
}

fn style_controls(
    ui: &mut egui::Ui,
    style: &mut ToolStyle,
    palette: &[[u8; 3]],
    range: RangeInclusive<f32>,
) {
    for c in palette {
        if swatch(ui, *c, style.color == *c).clicked() {
            style.color = *c;
        }
    }
    egui::color_picker::color_edit_button_srgb(ui, &mut style.color);
    ui.add(Slider::new(&mut style.width, range).show_value(false))
        .on_hover_text("Thickness");
}

fn swatch(ui: &mut egui::Ui, rgb: [u8; 3], selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 32.0), Sense::click());
    let painter = ui.painter();
    let c = rect.center();
    // The thin ring keeps dark colours visible on the dark toolbar.
    painter.circle(c, 8.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]), Stroke::new(1.0, color::BORDER_STRONG));
    if selected {
        painter.circle_stroke(c, 11.0, Stroke::new(2.0, color::ACCENT));
    } else if resp.hovered() {
        painter.circle_stroke(c, 11.0, Stroke::new(1.5, color::TEXT_FAINT));
    }
    resp
}
