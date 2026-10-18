//! Persisted UI settings (stored through eframe's storage, not in the note file).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolbarPosition {
    Top,
    Bottom,
    //Left,
    //Right,
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
    /// Eraser radius in document units.
    pub eraser_radius: f32,
    /// Extrapolate the live stroke slightly ahead of the pen (hides latency).
    pub predict_pen: bool,
    pub file_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            toolbar_position: ToolbarPosition::Bottom,
            pen: ToolStyle { color: [20, 20, 20], width: 2.5 },
            highlighter: ToolStyle { color: [255, 235, 0], width: 14.0 },
            eraser_radius: 12.0,
            predict_pen: true,
            file_path: "notes.ink".to_owned(),
        }
    }
}
