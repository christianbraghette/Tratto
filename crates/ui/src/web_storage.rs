// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web: notes in the browser's private storage (the origin private file
//! system, OPFS), which the page reads and writes only asynchronously.
//!
//! The app's file system is a tree in memory (`tratto_core::fs::memory`):
//! `load` fills it from the storage before the app starts, `keep_saved`
//! then writes back what changed every `FLUSH_EVERY` and when the page is
//! hidden. `/` is the storage's root; the library is `/Tratto`.
//!
//! A changed file is written whole (a note appended to is written again).

use std::path::{Component, Path};

use eframe::wasm_bindgen::closure::Closure;
use eframe::wasm_bindgen::{JsCast, JsValue};
use eframe::web_sys::{
    self, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions,
    FileSystemHandle, FileSystemHandleKind, FileSystemRemoveOptions, FileSystemWritableFileStream,
};
use tratto_core::fs::memory;
use wasm_bindgen_futures::{JsFuture, spawn_local};

/// How often changes are written to the storage, in milliseconds.
const FLUSH_EVERY: i32 = 1000;

async fn root() -> Result<FileSystemDirectoryHandle, JsValue> {
    let storage = web_sys::window().ok_or("no window")?.navigator().storage();
    JsFuture::from(storage.get_directory()).await?.dyn_into()
}

/// Reads everything in the storage into the tree. Asks the browser to keep
/// the storage (not to clear it when space runs low).
pub async fn load() -> Result<(), JsValue> {
    let root = root().await?;
    if let Some(storage) = web_sys::window().map(|w| w.navigator().storage())
        && let Ok(asked) = storage.persist()
    {
        let _ = JsFuture::from(asked).await;
    }
    load_dir(&root, Path::new("/")).await
}

async fn load_dir(dir: &FileSystemDirectoryHandle, path: &Path) -> Result<(), JsValue> {
    memory::load_dir(path);
    let entries = dir.values();
    loop {
        let next = JsFuture::from(entries.next()?).await?;
        if js_sys::Reflect::get(&next, &"done".into())?.is_truthy() {
            return Ok(());
        }
        let handle: FileSystemHandle = js_sys::Reflect::get(&next, &"value".into())?.dyn_into()?;
        let child = path.join(handle.name());
        match handle.kind() {
            FileSystemHandleKind::Directory => {
                Box::pin(load_dir(handle.unchecked_ref::<FileSystemDirectoryHandle>(), &child)).await?
            }
            _ => {
                let file: web_sys::File =
                    JsFuture::from(handle.unchecked_ref::<FileSystemFileHandle>().get_file()).await?.dyn_into()?;
                let bytes = js_sys::Uint8Array::new(&JsFuture::from(file.array_buffer()).await?).to_vec();
                memory::load_file(&child, bytes, (file.last_modified() / 1000.0) as u64);
            }
        }
    }
}

/// Writes the changes to the storage from now on, in the background.
pub fn keep_saved() {
    let window = web_sys::window().expect("a page");
    let tick = Closure::<dyn FnMut()>::new(|| spawn_local(flush()));
    let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(tick.as_ref().unchecked_ref(), FLUSH_EVERY);
    // The page may be closed next: what changed goes now.
    let hidden = Closure::<dyn FnMut()>::new(|| {
        if web_sys::window().and_then(|w| w.document()).is_some_and(|d| d.hidden()) {
            spawn_local(flush());
        }
    });
    if let Some(document) = window.document() {
        let _ = document.add_event_listener_with_callback("visibilitychange", hidden.as_ref().unchecked_ref());
    }
    // For as long as the page lives.
    tick.forget();
    hidden.forget();
}

/// Writes what changed since the last flush. A path that cannot be written
/// is tried again at the next one.
async fn flush() {
    let changes = memory::take_changes();
    if changes.is_empty() {
        return;
    }
    let Ok(root) = root().await else {
        changes.iter().for_each(memory::mark_changed);
        return;
    };
    for path in changes {
        if let Err(e) = write_path(&root, &path).await {
            web_sys::console::warn_2(&format!("Tratto: cannot save {} to the browser:", path.display()).into(), &e);
            memory::mark_changed(&path);
        }
    }
}

/// Makes the storage hold at `path` what the tree holds there now.
async fn write_path(root: &FileSystemDirectoryHandle, path: &Path) -> Result<(), JsValue> {
    let entries = memory::entries(path);
    if entries.is_empty() {
        return remove(root, path).await;
    }
    for (p, bytes) in entries {
        match bytes {
            None => drop(dir(root, &p, true).await?),
            Some(bytes) => write_file(root, &p, &bytes).await?,
        }
    }
    Ok(())
}

/// The names from `/` to `path`.
fn names(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The directory `path`, made (with its parents) if `create`.
async fn dir(
    root: &FileSystemDirectoryHandle,
    path: &Path,
    create: bool,
) -> Result<FileSystemDirectoryHandle, JsValue> {
    let options = FileSystemGetDirectoryOptions::new();
    options.set_create(create);
    let mut at = root.clone();
    for name in names(path) {
        at = JsFuture::from(at.get_directory_handle_with_options(&name, &options)).await?.dyn_into()?;
    }
    Ok(at)
}

async fn write_file(root: &FileSystemDirectoryHandle, path: &Path, bytes: &[u8]) -> Result<(), JsValue> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return Ok(()) };
    let options = FileSystemGetFileOptions::new();
    options.set_create(true);
    let dir = dir(root, parent, true).await?;
    let file: FileSystemFileHandle =
        JsFuture::from(dir.get_file_handle_with_options(&name.to_string_lossy(), &options)).await?.dyn_into()?;
    let stream: FileSystemWritableFileStream = JsFuture::from(file.create_writable()).await?.dyn_into()?;
    JsFuture::from(stream.write_with_u8_array(bytes)?).await?;
    JsFuture::from(stream.close()).await?;
    Ok(())
}

/// Removes `path` (with what is in it); nothing to do if it is not there.
async fn remove(root: &FileSystemDirectoryHandle, path: &Path) -> Result<(), JsValue> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return Ok(()) };
    let Ok(dir) = dir(root, parent, false).await else { return Ok(()) };
    let options = FileSystemRemoveOptions::new();
    options.set_recursive(true);
    match JsFuture::from(dir.remove_entry_with_options(&name.to_string_lossy(), &options)).await {
        Err(e) if e.dyn_ref::<web_sys::DomException>().is_some_and(|e| e.name() == "NotFoundError") => Ok(()),
        done => done.map(drop),
    }
}
