// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web app: the same `InkApp`, run by eframe's web runner in the page's
//! canvas (`<canvas id="tratto">`, see `crates/ui/web/index.html`), drawn
//! with WebGPU where the browser has it, else WebGL 2.
//!
//! A spike: the editor on a new note, kept in memory. What the desktop
//! needs the system for (the library's folders, file dialogs, the
//! clipboard's pictures, printing, updates) is not there yet.

use eframe::wasm_bindgen::JsCast;
use eframe::web_sys;

use crate::{app, stroke_gpu};

/// The id of the canvas the app runs in.
const CANVAS_ID: &str = "tratto";

/// Starts the app in the page's canvas.
pub fn start() {
    let options = eframe::WebOptions::default();
    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window().and_then(|w| w.document()).expect("a page");
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
                    let mut app = app::InkApp::new(cc, false, None, false);
                    app.scratch_note();
                    Ok(Box::new(app))
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
