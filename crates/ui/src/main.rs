// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tratto – a low-latency handwriting note-taking and PDF annotation app.
//!
//! This crate is the UI (egui + wgpu). Document model, history, storage,
//! library and PDF export live in `tratto_core`.
//!
//! Module layout:
//! - `app`            application state and per-frame orchestration
//! - `chrome`         custom title bar, window buttons and resize borders
//! - `view`           page layout + page/content/screen coordinate transforms
//! - `render`         painting of page background and strokes
//! - `pdf_view`       the PDF pages under the ink (background-rendered textures)
//! - `stroke_gpu`     strokes kept on the GPU (wgpu pipelines: ink, darkening highlighters)
//! - `stroke_mesh`    anti-aliased stroke tessellation
//! - `kinetic`        scroll momentum, wheel acceleration and eased jumps
//! - `scrollbar`      large pen-friendly scrollbars
//! - `toolbar`        the floating toolbars (tools, navigation, settings)
//! - `settings`       persisted UI settings (toolbar position, tool styles)
//! - `theme`          design system: colours, sizes, fonts, shared components
//! - `input`          native pen input (Wayland tablet, Windows pen buttons)
//! - `saver`          saves the open note on a background thread
//! - `update`         updates from the GitHub releases

// A GUI program on Windows: started from Explorer or a shortcut, no console
// window opens next to it. See `attach_console` for output in a terminal.
#![windows_subsystem = "windows"]

mod app;
#[cfg(test)]
mod bench;
mod chrome;
mod desktop;
mod file_picker;
mod frame_limit;
mod input;
mod kinetic;
mod pdf_nav;
mod pdf_view;
mod render;
mod saver;
mod scrollbar;
mod settings;
mod stroke_gpu;
mod stroke_mesh;
mod theme;
mod toolbar;
mod update;
mod view;

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};

/// The app icon: the title bar's pen nib (see assets/icon).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon/icon.png");

/// eframe's app id: names the folder of the saved settings.
const APP_ID: &str = "Tratto";

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
    update::remove_leftovers();

    // Low-latency presentation by default: no vsync wait and at most one frame
    // queued, frames capped (File > Settings, see `frame_limit`). Vsync (saves
    // power, adds latency) is a setting too, read here because the present
    // mode is fixed when the window is created; TRATTO_VSYNC=1 forces it on.
    // TRATTO_MSAA=4 additionally enables 4x MSAA (strokes are already
    // anti-aliased analytically, so this is rarely needed).
    let vsync = std::env::var_os("TRATTO_VSYNC").is_some() || app::saved_settings().is_some_and(|s| s.vsync);
    let msaa: u16 = std::env::var("TRATTO_MSAA").ok().and_then(|v| v.parse().ok()).unwrap_or(1);

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        // Always start maximized and windowed: don't save the window state.
        // (eframe still *loads* a saved one, see `InkApp::window_chrome`.)
        // App settings are persisted separately.
        persist_window: false,
        multisampling: msaa,
        // The default setup already asks for the high-performance GPU.
        wgpu_options: egui_wgpu::WgpuConfiguration::default().with_surface_config(egui_wgpu::SurfaceConfig {
            present_mode: if vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync },
            desired_maximum_frame_latency: Some(1),
        }),
        viewport: egui::ViewportBuilder::default()
            .with_title("Tratto")
            .with_icon(eframe::icon_data::from_png_bytes(ICON_PNG).expect("the bundled icon is a valid PNG"))
            // The app draws its own title bar; switching to the system one
            // (Settings) turns decorations back on at runtime.
            .with_decorations(false)
            .with_maximized(true) // F11 toggles fullscreen at runtime
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        APP_ID,
        options,
        Box::new(move |cc| {
            if let Some(render_state) = cc.wgpu_render_state.as_ref() {
                stroke_gpu::install(render_state, msaa.into());
            }
            Ok(Box::new(app::InkApp::new(cc, vsync)))
        }),
    );
    // "Restart now" after an update: the window is closed and saved.
    update::restart_if_requested();
    result
}

/// One egui frame for tests: runs `ui` and returns the textures it frees
/// (egui asserts that the texture changes of every frame are handled).
#[cfg(test)]
fn test_frame(ctx: &egui::Context, input: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) -> Vec<egui::TextureId> {
    let mut out = ctx.run_ui(input, ui);
    let freed = out.textures_delta.free.iter().copied().collect();
    out.textures_delta.clear();
    freed
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_icon_decodes() {
        let icon = eframe::icon_data::from_png_bytes(super::ICON_PNG).unwrap();
        assert_eq!((icon.width, icon.height), (256, 256));
    }
}
