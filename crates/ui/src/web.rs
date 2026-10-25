// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web app: the same `InkApp`, run by eframe's web runner in the page's
//! canvas (`<canvas id="tratto">`, see `crates/ui/web/index.html`), drawn
//! with WebGPU where the browser has it, else WebGL 2.
//!
//! The library lives in the browser's private storage (`web_storage`),
//! loaded before the app starts. What the desktop needs the system for
//! (file dialogs, the clipboard's pictures, printing, updates) is not there
//! yet.

use eframe::wasm_bindgen::JsCast;
use eframe::web_sys;

use crate::{app, stroke_gpu, web_storage};

/// The id of the canvas the app runs in.
const CANVAS_ID: &str = "tratto";

/// The page is shown full screen.
pub fn fullscreen() -> bool {
    web_sys::window().and_then(|w| w.document()).is_some_and(|d| d.fullscreen_element().is_some())
}

/// Asks the browser to show the page full screen, or to stop. It does so
/// only just after a click or a key press (as when the user asks for it),
/// and Escape always leaves full screen; `fullscreen` says what happened.
pub fn set_fullscreen(on: bool) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    if on {
        if let Some(page) = document.document_element() {
            let _ = page.request_fullscreen();
        }
    } else {
        document.exit_fullscreen();
    }
}

/// Starts the app in the page's canvas.
pub fn start() {
    let options = eframe::WebOptions::default();
    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window().and_then(|w| w.document()).expect("a page");
        // The notes first: the app reads its library as it starts.
        if let Err(e) = web_storage::load().await {
            web_sys::console::warn_2(&"Tratto: the browser's storage cannot be read, notes are not kept:".into(), &e);
        }
        web_storage::keep_saved();
        let canvas = document
            .get_element_by_id(CANVAS_ID)
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("the page has the app's canvas");
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(|cc| {
                    if let Some(render_state) = cc.wgpu_render_state.as_ref() {
                        stroke_gpu::install(render_state, 1);
                    }
                    Ok(Box::new(app::InkApp::new(cc, false, None, false)))
                }),
            )
            .await;
        // Shown in the page: the app could not start (no WebGPU nor WebGL 2).
        let status = document.get_element_by_id("status");
        match started {
            Ok(()) => {
                if let Some(status) = status {
                    status.remove();
                }
            }
            Err(e) => {
                if let Some(status) = status {
                    status.set_text_content(Some(&format!("Tratto cannot start: {e:?}")));
                }
            }
        }
    });
}
