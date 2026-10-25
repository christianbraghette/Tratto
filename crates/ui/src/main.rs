// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tratto – a low-latency handwriting note-taking and PDF annotation app.
//!
//! This crate is the UI (egui + wgpu). Document model, history, storage,
//! library and PDF export live in `tratto_core`.
//!
//! Module layout:
//! - `app`            application state and per-frame orchestration
//! - `background`     work off the frame: a thread (desktop), or at once (web)
//! - `chrome`         custom title bar, window buttons and resize borders
//! - `view`           page layout + page/content/screen coordinate transforms
//! - `win_frame`      Windows: moving and resizing the window with any input
//! - `render`         painting of page background and strokes
//! - `pdf_view`       the PDF pages under the ink (background-rendered textures)
//! - `pdf_nav`        a PDF's outline and links
//! - `stroke_gpu`     strokes kept on the GPU (wgpu pipelines: ink, darkening highlighters)
//! - `stroke_mesh`    anti-aliased stroke tessellation
//! - `kinetic`        scroll momentum, wheel acceleration and eased jumps
//! - `scrollbar`      large pen-friendly scrollbars
//! - `toolbar`        the floating toolbars (tools, navigation, settings)
//! - `settings`       persisted UI settings (toolbar position, tool styles)
//! - `theme`          design system: colours, sizes, fonts, shared components
//! - `input`          native pen input (Wayland tablet, Windows pen buttons), multi-finger gestures
//! - `desktop`        the desktop's window button layout (Linux portal)
//! - `file_assoc`     Tratto as the system's PDF reader (file associations)
//! - `file_picker`    the system's file dialogs, off the UI thread
//! - `frame_limit`    frame rate cap when vsync is off
//! - `power`          whether the computer runs on battery (battery saver)
//! - `print`          printing through the system's print dialog
//! - `saver`          saves the open note on a background thread
//! - `snapshot`       a picture of what was copied, for other apps
//! - `update`         updates from the GitHub releases
//! - `web`            the web app: the same app in a browser's canvas
//! - `web_files`      the web: files picked, dropped and downloaded
//! - `web_storage`    the web: notes in the browser's private storage

// A GUI program on Windows: started from Explorer or a shortcut, no console
// window opens next to it. See `attach_console` for output in a terminal.
#![windows_subsystem = "windows"]

mod app;
mod background;
#[cfg(test)]
mod bench;
mod chrome;
mod desktop;
mod file_assoc;
mod file_picker;
mod frame_limit;
mod input;
mod kinetic;
mod markdown;
mod pdf_nav;
mod pdf_view;
mod power;
mod print;
mod render;
mod saver;
mod scrollbar;
mod settings;
mod snapshot;
mod stroke_gpu;
mod stroke_mesh;
mod theme;
mod toolbar;
mod update;
mod view;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
mod web_files;
#[cfg(target_arch = "wasm32")]
mod web_storage;
mod win_frame;

#[cfg(any(test, not(target_arch = "wasm32")))]
use eframe::egui;
#[cfg(not(target_arch = "wasm32"))]
use eframe::egui_wgpu::{self, wgpu};

/// The app icon: the title bar's pen nib (see assets/icon).
#[cfg(not(target_arch = "wasm32"))]
const ICON_PNG: &[u8] = include_bytes!("../assets/icon/icon.png");

/// eframe's app id: names the folder of the saved settings.
#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
const USAGE: &str = "Usage: tratto [--reader] [PATH]
       tratto --register-pdf | --unregister-pdf

  PATH              a PDF to read, a note (.ink) to open, or a folder to
                    open as a notebook (a folder outside the library is
                    shown for this session)
  --reader          a PDF opens in a window of its own, for reading only
                    (how the system opens PDFs with Tratto)
  --register-pdf    makes Tratto the app that opens PDFs, for this user
  --unregister-pdf  takes Tratto off the apps that open PDFs";

#[cfg(not(target_arch = "wasm32"))]
/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Run {
    /// The app, with the path to open (absolute) and whether a PDF opens in
    /// the reader-only window.
    App {
        open: Option<std::path::PathBuf>,
        reader: bool,
    },
    /// `--register-pdf` (true) / `--unregister-pdf` (false), then quit.
    RegisterPdf(bool),
    Help,
}

#[cfg(not(target_arch = "wasm32"))]
/// Reads the arguments (after the program's name); `Err` with the message
/// to print and quit.
fn parse_args(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Run, String> {
    let (mut open, mut reader) = (None, false);
    for arg in args {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(Run::Help),
            Some("--register-pdf") => return Ok(Run::RegisterPdf(true)),
            Some("--unregister-pdf") => return Ok(Run::RegisterPdf(false)),
            Some("--reader") => reader = true,
            Some(flag) if flag.starts_with("--") => return Err(format!("tratto: unknown option {flag}\n\n{USAGE}")),
            _ if open.is_some() => return Err(format!("tratto: only one path can be opened\n\n{USAGE}")),
            _ => {
                let path = std::path::absolute(&arg).map_err(|e| format!("tratto: {}: {e}", arg.to_string_lossy()))?;
                open = Some(path);
            }
        }
    }
    Ok(Run::App { open, reader })
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    #[cfg(windows)]
    attach_console();
    let (open, reader) = match parse_args(std::env::args_os().skip(1)) {
        Ok(Run::App { open, reader }) => (open, reader),
        Ok(Run::Help) => {
            println!("{USAGE}");
            return Ok(());
        }
        Ok(Run::RegisterPdf(on)) => {
            let done = if on { file_assoc::register() } else { file_assoc::unregister() };
            match done {
                Ok(()) if on => {
                    println!("Tratto is registered for PDFs. {}", file_assoc::REGISTER_HINT);
                    file_assoc::open_default_apps();
                }
                Ok(()) => println!("Tratto no longer opens PDFs."),
                Err(e) => {
                    eprintln!("tratto: {e}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    update::remove_leftovers();

    // Low-latency presentation by default: no vsync wait and at most one frame
    // queued, frames capped (File > Settings, see `frame_limit`). Vsync (saves
    // power, adds latency) is a setting too, read here for the window to start
    // with it; it changes at runtime (`InkApp::present_mode`, also for the
    // battery saver). TRATTO_VSYNC=1 forces it on.
    // TRATTO_MSAA=4 additionally enables 4x MSAA (strokes are already
    // anti-aliased analytically, so this is rarely needed).
    let saved = app::saved_settings();
    let vsync = frame_limit::env_vsync() || saved.as_ref().is_some_and(|s| s.vsync);
    let msaa: u16 = std::env::var("TRATTO_MSAA").ok().and_then(|v| v.parse().ok()).unwrap_or(1);

    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default().with_surface_config(egui_wgpu::SurfaceConfig {
        present_mode: if vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync },
        desired_maximum_frame_latency: Some(1),
    });
    // The integrated GPU: pages and strokes are light work for it, and on a
    // laptop with two GPUs the discrete one would stay powered while Tratto
    // is open (egui asks for it by default). WGPU_POWER_PREF=high overrides.
    // Windows: DirectX 12. With Vulkan the drivers take the fullscreen window
    // for an exclusive one, and the screen flashes black whenever another
    // window comes over it (Alt+Tab, the taskbar, the system's gestures).
    // Another backend can be chosen in the settings; WGPU_BACKEND overrides.
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        let power = wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::LowPower);
        setup.power_preference = power;
        if wgpu::Backends::from_env().is_none() {
            if cfg!(windows) {
                setup.instance_descriptor.backends = wgpu::Backends::DX12;
            }
            if let Some(chosen) = saved.and_then(|s| s.graphics_backend.backend()) {
                // The automatic backends stay as a fallback, for a choice the
                // system lacks (otherwise the window would never open).
                setup.instance_descriptor.backends |= chosen.into();
                setup.native_adapter_selector =
                    Some(std::sync::Arc::new(move |adapters, surface| pick_adapter(adapters, surface, chosen, power)));
            }
        }
    }

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        // Always start maximized and windowed: don't save the window state.
        // (eframe still *loads* a saved one, see `InkApp::window_chrome`.)
        // App settings are persisted separately.
        persist_window: false,
        multisampling: msaa,
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_title("Tratto")
            .with_icon(eframe::icon_data::from_png_bytes(ICON_PNG).expect("the bundled icon is a valid PNG"))
            // The app draws its own title bar; switching to the system one
            // (Settings) turns decorations back on at runtime. On Windows the
            // window keeps them, and only loses its caption (see `win_frame`).
            .with_decorations(cfg!(windows))
            // F11 toggles fullscreen at runtime. On Windows maximized out of
            // sight before the first frame instead (`win_frame::open_maximized`).
            .with_maximized(!cfg!(windows))
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
            Ok(Box::new(app::InkApp::new(cc, vsync, open, reader)))
        }),
    );
    // "Restart now" after an update: the window is closed and saved.
    update::restart_if_requested();
    result
}

#[cfg(not(target_arch = "wasm32"))]
/// The GPU to draw with when a backend was chosen in the settings: one that
/// can draw the window, on the `chosen` backend if there is one, the
/// integrated or the discrete GPU as `power` prefers.
fn pick_adapter(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    chosen: wgpu::Backend,
    power: wgpu::PowerPreference,
) -> Result<wgpu::Adapter, String> {
    let wanted = match power {
        wgpu::PowerPreference::HighPerformance => wgpu::DeviceType::DiscreteGpu,
        _ => wgpu::DeviceType::IntegratedGpu,
    };
    adapters
        .iter()
        .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
        .max_by_key(|a| {
            let info = a.get_info();
            (info.backend == chosen, info.device_type == wanted, info.device_type != wgpu::DeviceType::Cpu)
        })
        .cloned()
        .ok_or_else(|| "no GPU can draw the window".to_owned())
}

/// The web: started by the page (`web::start`).
#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
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

    #[test]
    fn command_line() {
        use super::Run;
        let args =
            |a: &[&str]| super::parse_args(a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>().into_iter());
        assert_eq!(args(&[]), Ok(Run::App { open: None, reader: false }));
        let Ok(Run::App { open: Some(doc), reader: true }) = args(&["--reader", "paper.pdf"]) else { panic!() };
        assert!(doc.is_absolute() && doc.ends_with("paper.pdf"));
        assert_eq!(args(&["--help"]), Ok(Run::Help));
        assert_eq!(args(&["--unregister-pdf"]), Ok(Run::RegisterPdf(false)));
        assert!(args(&["a.pdf", "b.pdf"]).is_err());
        assert!(args(&["--nope"]).is_err());
    }
}
