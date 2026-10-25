// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web: files in and out of the browser, for the app's path-based file
//! handling (see `file_picker`).
//!
//! - **In** (a file picked or dropped): read into the file system in memory
//!   under `TMP` and handed to the app by that path. It is not kept in the
//!   browser's storage; what the app makes of it (an imported note) is.
//! - **Out** (an exported PDF, a saved picture): the app writes it under
//!   `DOWNLOADS`; `web_storage` then hands it to the browser as a download
//!   instead of keeping it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use eframe::egui;
use eframe::wasm_bindgen::closure::Closure;
use eframe::wasm_bindgen::{JsCast, JsValue};
use eframe::web_sys;
use tratto_core::fs::memory;
use wasm_bindgen_futures::{JsFuture, spawn_local};

/// Where picked and dropped files are read to (never stored).
pub const TMP: &str = "/tmp";
/// Where files to download are written.
pub const DOWNLOADS: &str = "/Downloads";

/// Files at `path` are not kept in the browser's storage.
pub fn transient(path: &Path) -> bool {
    path.starts_with(TMP) || path.starts_with(DOWNLOADS)
}

/// A name that is a single path component.
fn file_name(name: &str) -> String {
    let name: String = name.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    if name.trim().is_empty() { "file".to_owned() } else { name }
}

/// Puts `bytes`, a file called `name`, under `TMP`; its path.
pub fn into_tmp(name: &str, bytes: Vec<u8>) -> PathBuf {
    let path = Path::new(TMP).join(file_name(name));
    memory::load_file(&path, bytes, tratto_core::storage::now_secs());
    path
}

/// Opens the browser's file picker for a file with one of `extensions`; the
/// path it is read to (`into_tmp`) goes to `reply`, `None` if cancelled.
pub fn pick(ctx: &egui::Context, extensions: &[&str], reply: Sender<Option<PathBuf>>) {
    let reply = Rc::new(RefCell::new(Some(reply)));
    let answer = {
        let (reply, ctx) = (reply.clone(), ctx.clone());
        move |path: Option<PathBuf>| {
            if let Some(reply) = reply.borrow_mut().take() {
                let _ = reply.send(path);
                ctx.request_repaint();
            }
        }
    };
    let input = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.create_element("input").ok())
        .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok());
    let Some(input) = input else { return answer(None) };
    input.set_type("file");
    input.set_accept(&extensions.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(","));

    let changed = {
        let (input, answer) = (input.clone(), answer.clone());
        Closure::<dyn FnMut()>::new(move || {
            let Some(file) = input.files().and_then(|f| f.get(0)) else { return answer(None) };
            let answer = answer.clone();
            spawn_local(async move { answer(read(&file).await.ok()) });
        })
    };
    let cancelled = Closure::<dyn FnMut()>::new(move || answer(None));
    input.set_onchange(Some(changed.as_ref().unchecked_ref()));
    let _ = input.add_event_listener_with_callback("cancel", cancelled.as_ref().unchecked_ref());
    // Freed with the page: one small pair per file picked.
    changed.forget();
    cancelled.forget();
    input.click();
}

async fn read(file: &web_sys::File) -> Result<PathBuf, JsValue> {
    let bytes = js_sys::Uint8Array::new(&JsFuture::from(file.array_buffer()).await?).to_vec();
    Ok(into_tmp(&file.name(), bytes))
}

/// Where a file to save goes: under `DOWNLOADS`, named after `current` (the
/// path the app suggests) with the first of `extensions`.
pub fn download_path(current: &str, extensions: &[&str]) -> PathBuf {
    memory::load_dir(DOWNLOADS);
    let current = Path::new(current.trim().trim_matches('"'));
    let stem = current.file_stem().map_or_else(|| "Tratto".to_owned(), |s| file_name(&s.to_string_lossy()));
    let name = match extensions.first() {
        Some(ext) => format!("{stem}.{ext}"),
        None => current.file_name().map_or(stem, |n| file_name(&n.to_string_lossy())),
    };
    Path::new(DOWNLOADS).join(name)
}

/// Hands `bytes` to the browser as a download called `name`.
pub fn download(name: &str, bytes: &[u8]) -> Result<(), JsValue> {
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no page")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let link: web_sys::HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
    link.set_href(&url);
    link.set_download(name);
    link.click();
    // After the browser has started the download.
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000);
    }
    Ok(())
}

/// The web: files dropped onto the page, read into `TMP`. eframe reports
/// them with only their names (the browser reads them asynchronously): they
/// are taken out of the frame's input, read, and reported by their path in
/// a later frame, as the desktop reports them.
#[derive(Default)]
pub struct Drops {
    read: Rc<RefCell<Vec<PathBuf>>>,
}

impl Drops {
    pub fn process(&self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        for file in std::mem::take(&mut raw.dropped_files) {
            let (read, ctx) = (self.read.clone(), ctx.clone());
            spawn_local(async move {
                let name = file.path().to_string_lossy().into_owned();
                if let Ok(bytes) = file.bytes_async().await {
                    read.borrow_mut().push(into_tmp(&name, bytes));
                    ctx.request_repaint();
                }
            });
        }
        for path in self.read.borrow_mut().drain(..) {
            let file: egui::DroppedFileHandle = std::sync::Arc::new(crate::input::wayland_drop::DroppedPath(path));
            raw.dropped_files.push(file);
        }
    }
}
