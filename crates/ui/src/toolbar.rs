// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Floating pill-shaped toolbars.
//!
//! * the main toolbar (tools, styles, undo / redo) at the top or bottom centre
//! * navigation (library, pages panel) in a notch in the top-left corner
//! * settings in a notch in the top-right corner
//!
//! Buttons show Phosphor icons (see [`install_icon_font`]) with the name as a
//! tooltip.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, Margin, Order, Pos2, Rect, RichText, Rounding, Sense, Shape,
    Slider, Stroke,
};
use egui_phosphor::regular as icon;

use crate::scrollbar;
use crate::settings::{Settings, ToolStyle, ToolbarPosition};

/// Fixed distance between the toolbars and the window edge.
pub const MARGIN: f32 = 16.0;

/// Glyph size of the toolbar icons, in points.
const ICON_SIZE: f32 = 20.0;

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

/// Adds the icon font as a fallback of the proportional family, so icon
/// glyphs can be mixed with normal text.
pub fn install_icon_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

fn icon_text(glyph: &str) -> RichText {
    RichText::new(glyph).size(ICON_SIZE)
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
    let (anchor, offset) = match settings.toolbar_position {
        ToolbarPosition::Top => (Align2::CENTER_TOP, vec2(0.0, MARGIN)),
        ToolbarPosition::Bottom => (Align2::CENTER_BOTTOM, vec2(0.0, -(MARGIN + bottom_inset))),
    };

    let mut event = None;
    egui::Area::new(egui::Id::new("floating_toolbar"))
        .order(Order::Foreground)
        .anchor(anchor, offset)
        .show(ctx, |ui| pill(ui, |ui| event = contents(ui, tool, settings, can_undo, can_redo)));
    event
}

/// Library and pages buttons in a notch in the top-left corner, settings in
/// one in the top-right corner of the free area (call after the side panels,
/// so the notches hang from their edge instead of covering them).
pub fn show_corners(
    ctx: &egui::Context,
    settings: &mut Settings,
    settings_open: bool,
) -> Option<ToolbarEvent> {
    let free = ctx.available_rect();
    let mut event = None;

    notch(ctx, "corner_notch_left", Side::Left, free.left_top(), |ui| {
        if ui
            .button(icon_text(icon::BOOKS))
            .on_hover_text("Library: save and go back to your notes")
            .clicked()
        {
            event = Some(ToolbarEvent::Library);
        }
        ui.toggle_value(&mut settings.pages_panel_open, icon_text(icon::SIDEBAR_SIMPLE))
            .on_hover_text("Pages: jump, reorder, delete");
    });

    // Against the vertical scrollbar, which always occupies the right edge.
    let right = Pos2::new(free.right() - scrollbar::THICKNESS, free.top());
    notch(ctx, "corner_notch_right", Side::Right, right, |ui| {
        if ui
            .add(egui::SelectableLabel::new(settings_open, icon_text(icon::GEAR_SIX)))
            .on_hover_text("Settings")
            .clicked()
        {
            event = Some(ToolbarEvent::ToggleSettings);
        }
    });
    event
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
}

/// Radius of the notch's free bottom corner.
const NOTCH_ROUNDING: f32 = 14.0;
/// Radius of the concave joins between the notch and the window edges.
const NOTCH_FILLET: f32 = 10.0;

/// A tab that hangs from the top edge, flush with the side edge at `corner`
/// (its top-left corner for [`Side::Left`], top-right for [`Side::Right`]).
fn notch(
    ctx: &egui::Context,
    id: &str,
    side: Side,
    corner: Pos2,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let pivot = match side {
        Side::Left => Align2::LEFT_TOP,
        Side::Right => Align2::RIGHT_TOP,
    };
    egui::Area::new(egui::Id::new(id))
        .order(Order::Foreground)
        .pivot(pivot)
        .fixed_pos(corner)
        .show(ctx, |ui| {
            // Reserve the background slot first so it is painted below the buttons.
            let background = ui.painter().add(Shape::Noop);
            let rect = egui::Frame::none()
                .inner_margin(Margin { left: 10.0, right: 10.0, top: 6.0, bottom: 8.0 })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    ui.spacing_mut().button_padding = vec2(8.0, 6.0);
                    ui.horizontal(add_contents);
                })
                .response
                .rect;
            let fill = ui.visuals().window_fill();
            ui.painter().set(background, notch_shape(rect, side, fill));
        });
}

/// The notch outline as one polygon: the body with its free bottom corner
/// rounded, plus concave fillets that blend it into the top edge and the side
/// edge.
///
/// The outline is not convex, but every point is visible from the window
/// corner, its first vertex; epaint fans the fill out from the first vertex,
/// so it is filled correctly, and a single shape has no anti-aliasing seams.
fn notch_shape(rect: Rect, side: Side, fill: Color32) -> Shape {
    let (big, small) = (NOTCH_ROUNDING, NOTCH_FILLET);
    let (l, r, t, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    // Outline of the left notch, clockwise. The edges along the window border
    // reach 1 px past it, so their anti-aliasing fringe is not visible.
    let mut pts = vec![pos2(l - 1.0, t - 1.0), pos2(r + small, t - 1.0)];
    arc(&mut pts, pos2(r + small, t + small), small, -90.0, -180.0);
    arc(&mut pts, pos2(r - big, b - big), big, 0.0, 90.0);
    arc(&mut pts, pos2(l + small, b + small), small, -90.0, -180.0);
    pts.push(pos2(l - 1.0, b + small));

    if side == Side::Right {
        // Mirror; that reverses the winding, so restore clockwise order
        // while keeping the corner first.
        for p in &mut pts {
            p.x = l + r - p.x;
        }
        pts[1..].reverse();
    }
    Shape::convex_polygon(pts, fill, Stroke::NONE)
}

/// Appends a circular arc from angle `from` to `to` (degrees, y down).
fn arc(pts: &mut Vec<Pos2>, center: Pos2, radius: f32, from: f32, to: f32) {
    const SEGMENTS: usize = 8;
    for i in 0..=SEGMENTS {
        let a = (from + (to - from) * i as f32 / SEGMENTS as f32).to_radians();
        pts.push(center + radius * vec2(a.cos(), a.sin()));
    }
}

/// The rounded floating frame of the main toolbar.
fn pill(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::popup(ui.style())
        .rounding(Rounding::same(26.0))
        .inner_margin(Margin::symmetric(10.0, 6.0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            ui.spacing_mut().button_padding = vec2(8.0, 6.0);
            ui.horizontal(add_contents);
        });
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
        ui.selectable_value(tool, t, icon_text(t.icon())).on_hover_text(t.label());
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
            ui.weak("Lasso, then drag");
        }
        Tool::Text => style_controls(ui, &mut settings.text, &PEN_PALETTE, 8.0..=96.0),
    }
    ui.separator();

    let undo = egui::Button::new(icon_text(icon::ARROW_U_UP_LEFT));
    if ui.add_enabled(can_undo, undo).on_hover_text("Undo (Ctrl+Z)").clicked() {
        event = Some(ToolbarEvent::Undo);
    }
    let redo = egui::Button::new(icon_text(icon::ARROW_U_UP_RIGHT));
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
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 9.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    if selected {
        painter.circle_stroke(rect.center(), 11.0, Stroke::new(2.0, ui.visuals().selection.bg_fill));
    } else if resp.hovered() {
        painter.circle_stroke(rect.center(), 11.0, Stroke::new(1.0, Color32::GRAY));
    }
    resp
}
