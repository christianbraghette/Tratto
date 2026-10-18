//! InkNote – a small handwriting note-taking application.
//!
//! Module layout:
//! - `model`          serializable data model (`Point`, `Stroke`, `Document`)
//! - `geometry`       point-to-segment math used by the eraser
//! - `history`        action-based undo/redo (no document cloning)
//! - `stroke_builder` jitter filter + velocity-adaptive smoothing + prediction
//! - `stroke_mesh`    anti-aliased, curve-interpolated stroke tessellation
//! - `view`           page layout + page/content/screen coordinate transforms
//! - `scrollbar`      large pen-friendly scrollbars
//! - `render`         painting of page background and strokes
//! - `settings`       persisted UI settings (toolbar position, tool styles)
//! - `toolbar`        the floating pill toolbar
//! - `wayland_pen`    native Wayland tablet (pen) input, Linux only
//! - `pdf_export`    vector PDF export (hand-written PDF writer)
//! - `storage`        bincode save/load
//! - `app`            application state and per-frame orchestration

mod app;
mod geometry;
mod history;
mod model;
mod pdf_export;
mod render;
mod scrollbar;
mod settings;
mod storage;
mod stroke_builder;
mod stroke_mesh;
mod toolbar;
mod view;
mod pen;

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};

fn main() -> eframe::Result<()> {
    // Low-latency presentation by default: no vsync wait and at most one frame
    // queued. Set INKNOTE_VSYNC=1 to go back to vsync (saves power, adds latency).
    // INKNOTE_MSAA=4 additionally enables 4x MSAA (strokes are already
    // anti-aliased analytically, so this is rarely needed).
    let vsync = std::env::var_os("INKNOTE_VSYNC").is_some();
    let msaa: u16 = std::env::var("INKNOTE_MSAA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        vsync,
        multisampling: msaa,
        wgpu_options: egui_wgpu::WgpuConfiguration {
            present_mode: if vsync {
                wgpu::PresentMode::AutoVsync
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
            desired_maximum_frame_latency: Some(1),
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        },
        viewport: egui::ViewportBuilder::default()
            .with_title("InkNote")
            .with_decorations(true) // keep system window buttons
            .with_maximized(true)
            //.with_fullscreen(true) // F11 toggles this at runtime
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };

    eframe::run_native(
        "InkNote",
        options,
        Box::new(|cc| Ok(Box::new(app::InkApp::new(cc)))),
    )
}
