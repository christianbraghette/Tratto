//! InkNote – a small handwriting note-taking application.
//!
//! Module layout:
//! - `model`          serializable data model (`Point`, `Stroke`, `Document`)
//! - `geometry`       point-to-segment math used by the eraser
//! - `history`        action-based undo/redo (no document cloning)
//! - `stroke_builder` jitter filter + moving-average smoothing of raw input
//! - `view`           document <-> screen coordinate transform, centering
//! - `render`         painting of page background and strokes
//! - `settings`       persisted UI settings (toolbar position, tool styles)
//! - `toolbar`        the floating pill toolbar
//! - `wayland_pen`    native Wayland tablet (pen) input, Linux only
//! - `storage`        bincode save/load
//! - `app`            application state and per-frame orchestration

mod app;
mod geometry;
mod history;
mod model;
mod render;
mod settings;
mod storage;
mod stroke_builder;
mod toolbar;
mod view;
mod wayland_pen;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("InkNote")
            .with_decorations(true) // keep system window buttons
            .with_fullscreen(true) // F11 toggles this at runtime
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };

    eframe::run_native(
        "InkNote",
        options,
        Box::new(|cc| Ok(Box::new(app::InkApp::new(cc)))),
    )
}
