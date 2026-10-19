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
    /// Left panel with page previews.
    pub pages_panel_open: bool,
    pub pdf_path: String,
    /// Draw the ruled/grid lines into the exported PDF.
    pub pdf_rules: bool,
    pub file_path: String,
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
            pages_panel_open: false,
            pdf_path: "notes.pdf".to_owned(),
            pdf_rules: true,
            file_path: "notes.ink".to_owned(),
        }
    }
}
