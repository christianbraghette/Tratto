// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Persisted UI settings (stored through eframe's storage, not in the note file).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolbarPosition {
    Top,
    /// Also the migration target for the removed Left / Right positions
    /// that may still be present in saved settings.
    #[serde(alias = "Left", alias = "Right")]
    Bottom,
}

/// What the left panel of the editor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeftPanel {
    Closed,
    Pages,
    Settings,
}

impl LeftPanel {
    /// Shows `view`, or closes the panel if `view` is already shown.
    pub fn toggle(&mut self, view: LeftPanel) {
        *self = if *self == view { LeftPanel::Closed } else { view };
    }
}

/// How the pages are scaled in the window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ZoomMode {
    /// Page width (plus margins) fills the window.
    FitWidth,
    /// A whole page fits in the window.
    FitPage,
    /// Fixed zoom; 100 % = 1 document unit per screen point.
    Percent(u32),
}

impl ZoomMode {
    pub const PRESETS: [ZoomMode; 9] = [
        ZoomMode::FitWidth,
        ZoomMode::FitPage,
        ZoomMode::Percent(50),
        ZoomMode::Percent(75),
        ZoomMode::Percent(100),
        ZoomMode::Percent(125),
        ZoomMode::Percent(150),
        ZoomMode::Percent(200),
        ZoomMode::Percent(300),
    ];

    pub fn label(&self) -> String {
        match self {
            ZoomMode::FitWidth => "Fit width".into(),
            ZoomMode::FitPage => "Fit page".into(),
            ZoomMode::Percent(p) => format!("{p}%"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ToolStyle {
    pub color: [u8; 3],
    /// Width in document units.
    pub width: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub toolbar_position: ToolbarPosition,
    pub pen: ToolStyle,
    pub highlighter: ToolStyle,
    /// Text boxes: `width` is the font size (document units).
    pub text: ToolStyle,
    /// Eraser radius in document units.
    pub eraser_radius: f32,
    /// Extrapolate the live stroke slightly ahead of the pen (hides latency).
    pub predict_pen: bool,
    pub zoom: ZoomMode,
    /// Left panel: page previews or settings.
    pub left_panel: LeftPanel,
    /// Right panel with the notebook hierarchy.
    pub library_panel_open: bool,
    /// Draw the ruled/grid lines into the exported PDF.
    pub pdf_rules: bool,
    /// Folder that holds all notebooks and notes.
    pub library_root: String,
    /// Draw our own title bar with the menus (system decorations off).
    pub custom_title_bar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            toolbar_position: ToolbarPosition::Bottom,
            pen: ToolStyle { color: [20, 20, 20], width: 2.5 },
            highlighter: ToolStyle { color: [255, 235, 0], width: 14.0 },
            text: ToolStyle { color: [20, 20, 20], width: 18.0 },
            eraser_radius: 12.0,
            predict_pen: true,
            zoom: ZoomMode::Percent(100),
            left_panel: LeftPanel::Closed,
            library_panel_open: false,
            pdf_rules: true,
            library_root: inknote_core::library::default_root().to_string_lossy().into_owned(),
            custom_title_bar: true,
        }
    }
}
