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

// A GUI program on Windows: started from Explorer or a shortcut, no console
// window opens next to it. See `attach_console` for output in a terminal.
#![windows_subsystem = "windows"]

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

/// The app icon: the title bar's pen nib (see assets/icon).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon/icon.png");

/// Started from a terminal (`cargo run`, PowerShell), print there: a GUI
/// program has no console of its own, so it borrows its parent's.
#[cfg(windows)]
fn attach_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // Declared here so it does not need an optional windows-sys feature.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // Fails (harmlessly) when there is no parent console, e.g. from Explorer.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() -> eframe::Result<()> {
    #[cfg(windows)]
    attach_console();

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
            .with_icon(eframe::icon_data::from_png_bytes(ICON_PNG).expect("the bundled icon is a valid PNG"))
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

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_icon_decodes() {
        let icon = eframe::icon_data::from_png_bytes(super::ICON_PNG).unwrap();
        assert_eq!((icon.width, icon.height), (256, 256));
    }
}
