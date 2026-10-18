//! Floating pill-shaped toolbar.

use std::ops::RangeInclusive;

use eframe::egui::{self, vec2, Align2, Color32, Margin, Order, Rounding, Sense, Slider, Stroke};

use crate::settings::{Settings, ToolStyle, ToolbarPosition};

/// Fixed distance between the toolbar and the window edge.
pub const MARGIN: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
}

pub enum ToolbarEvent {
    Undo,
    Redo,
    ToggleSettings,
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

/// Shows the toolbar at its configured position; returns a clicked action.
pub fn show(
    ctx: &egui::Context,
    tool: &mut Tool,
    settings: &mut Settings,
    can_undo: bool,
    can_redo: bool,
) -> Option<ToolbarEvent> {
    // (anchor, offset from that anchor, horizontal layout?)
    let (anchor, offset, horizontal) = match settings.toolbar_position {
        ToolbarPosition::Top => (Align2::CENTER_TOP, vec2(0.0, MARGIN), true),
        ToolbarPosition::Bottom => (Align2::CENTER_BOTTOM, vec2(0.0, -MARGIN), true),
        //ToolbarPosition::Left => (Align2::LEFT_CENTER, vec2(MARGIN, 0.0), false),
        //ToolbarPosition::Right => (Align2::RIGHT_CENTER, vec2(-MARGIN, 0.0), false),
    };

    let mut event = None;
    egui::Area::new(egui::Id::new("floating_toolbar"))
        .order(Order::Foreground)
        .anchor(anchor, offset)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .rounding(Rounding::same(26.0))
                .inner_margin(Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    if horizontal {
                        ui.horizontal(|ui| {
                            event = contents(ui, tool, settings, can_undo, can_redo);
                        });
                    } else {
                        ui.vertical(|ui| {
                            event = contents(ui, tool, settings, can_undo, can_redo);
                        });
                    }
                });
        });
    event
}

fn contents(
    ui: &mut egui::Ui,
    tool: &mut Tool,
    settings: &mut Settings,
    can_undo: bool,
    can_redo: bool,
) -> Option<ToolbarEvent> {
    let mut event = None;

    ui.selectable_value(tool, Tool::Pen, "Pen");
    ui.selectable_value(tool, Tool::Highlighter, "Highlighter");
    ui.selectable_value(tool, Tool::Eraser, "Eraser");
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
    }
    ui.separator();

    if ui.add_enabled(can_undo, egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked() {
        event = Some(ToolbarEvent::Undo);
    }
    if ui.add_enabled(can_redo, egui::Button::new("Redo")).on_hover_text("Ctrl+Y").clicked() {
        event = Some(ToolbarEvent::Redo);
    }
    ui.separator();
    if ui.button("Settings").clicked() {
        event = Some(ToolbarEvent::ToggleSettings);
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
