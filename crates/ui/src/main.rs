// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! InkNote – a small handwriting note-taking application.
//!
//! This crate is the UI (egui + wgpu). Document model, history, storage,
//! library and PDF export live in `inknote_core`.
//!
//! Module layout:
//! - `app`            application state and per-frame orchestration
//! - `chrome`         custom title bar, window buttons and resize borders
//! - `view`           page layout + page/content/screen coordinate transforms
//! - `render`         painting of page background and strokes
//! - `stroke_mesh`    anti-aliased stroke tessellation
//! - `kinetic`        scroll momentum, wheel acceleration and eased jumps
//! - `scrollbar`      large pen-friendly scrollbars
//! - `toolbar`        the floating toolbars (tools, navigation, settings)
//! - `settings`       persisted UI settings (toolbar position, tool styles)
//! - `theme`          design system: colours, sizes, fonts, shared components
//! - `input`          native pen input (Wayland tablet, Windows pen buttons)

mod app;
mod chrome;
mod input;
mod kinetic;
mod render;
mod scrollbar;
mod settings;
mod stroke_mesh;
mod theme;
mod toolbar;
mod view;

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
        // Always start maximized and windowed: don't save the window state.
        // (eframe still *loads* a saved one, see `InkApp::window_chrome`.)
        // App settings are persisted separately.
        persist_window: false,
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
            // The app draws its own title bar; switching to the system one
            // (Settings) turns decorations back on at runtime.
            .with_decorations(false)
            .with_maximized(true) // F11 toggles fullscreen at runtime
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };

    eframe::run_native(
        "InkNote",
        options,
        Box::new(|cc| Ok(Box::new(app::InkApp::new(cc)))),
    )
}
