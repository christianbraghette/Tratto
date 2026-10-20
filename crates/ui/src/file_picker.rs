// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The system's file dialogs (Explorer, Finder, the desktop portal on
//! Linux), through rfd, for the Browse buttons next to the path boxes.
//!
//! A dialog is created on the UI thread, which macOS requires, and awaited
//! on a background thread, so the window keeps drawing while it is open.
//! Its answer comes back through [`FilePicker::poll`] and goes into the path
//! box that asked for it; the box can still be typed into, so the app works
//! where no dialog is available.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

/// Windows and macOS always have a file dialog, so a path is chosen with it
/// straight away. Elsewhere (Linux: the desktop portal may be missing) the
/// app's own path box comes first, with a Browse button.
pub const DIRECT: bool = cfg!(any(windows, target_os = "macos"));

/// What a dialog chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// An existing file (filter name, extensions).
    Open(&'static str, &'static [&'static str]),
    /// Where to write a file (filter name, extensions).
    Save(&'static str, &'static [&'static str]),
    Folder,
}

/// A dialog to show; `field` names the path box the answer goes to.
pub struct Request<F> {
    pub pick: Pick,
    pub title: &'static str,
    /// The text of the path box: the dialog starts there.
    pub current: String,
    pub field: F,
}

/// The open dialog, if any (one at a time).
pub struct FilePicker<F> {
    pending: Option<(F, Receiver<Option<PathBuf>>)>,
}

impl<F> Default for FilePicker<F> {
    fn default() -> Self {
        Self { pending: None }
    }
}

impl<F> FilePicker<F> {
    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    /// Shows the dialog over `parent`, unless one is already open.
    pub fn open<W: HasWindowHandle + HasDisplayHandle>(&mut self, ctx: &egui::Context, parent: &W, req: Request<F>) {
        if self.is_open() {
            return;
        }
        let (dir, name) = start(&req.current);
        let mut dialog = rfd::AsyncFileDialog::new().set_title(req.title).set_parent(parent);
        if let Some(dir) = dir {
            dialog = dialog.set_directory(dir);
        }
        let rx = match req.pick {
            Pick::Open(filter, ext) => wait(ctx, dialog.add_filter(filter, ext).pick_file()),
            Pick::Save(filter, ext) => {
                if let Some(name) = name {
                    dialog = dialog.set_file_name(name);
                }
                wait(ctx, dialog.add_filter(filter, ext).save_file())
            }
            Pick::Folder => wait(ctx, dialog.set_can_create_directories(true).pick_folder()),
        };
        self.pending = Some((req.field, rx));
    }

    /// The chosen path once the dialog has closed: `None` while it is open
    /// and when it was cancelled.
    pub fn poll(&mut self) -> Option<(F, PathBuf)> {
        let (_, rx) = self.pending.as_ref()?;
        match rx.try_recv() {
            Err(TryRecvError::Empty) => None,
            Ok(path) => {
                let (field, _) = self.pending.take()?;
                path.map(|p| (field, p))
            }
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                None
            }
        }
    }
}

/// Awaits the dialog on a background thread and wakes the UI when it closes.
fn wait(
    ctx: &egui::Context,
    task: impl Future<Output = Option<rfd::FileHandle>> + Send + 'static,
) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let path = pollster::block_on(task).map(|f| f.path().to_path_buf());
        let _ = tx.send(path);
        ctx.request_repaint();
    });
    rx
}

/// Where a dialog starts for the path `current`: its folder (or `current`
/// itself, if it is a folder) and its file name, for saving.
fn start(current: &str) -> (Option<PathBuf>, Option<String>) {
    let path = Path::new(current.trim().trim_matches('"'));
    if path.as_os_str().is_empty() {
        return (None, None);
    }
    if path.is_dir() {
        return (Some(path.to_path_buf()), None);
    }
    let dir = path.parent().filter(|d| d.is_dir()).map(Path::to_path_buf);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    (dir, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_next_to_the_current_path() {
        let dir = std::env::temp_dir().join("tratto_file_picker_test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Lecture 3.pdf");

        assert_eq!(start(""), (None, None));
        assert_eq!(start("  "), (None, None));
        // A folder: start inside it.
        assert_eq!(start(&dir.to_string_lossy()), (Some(dir.clone()), None));
        // A file (existing or not): its folder and its name; quotes as pasted
        // from Explorer's "Copy as path" are ignored.
        let quoted = format!("\"{}\"", file.display());
        assert_eq!(start(&quoted), (Some(dir.clone()), Some("Lecture 3.pdf".to_owned())));
        // A folder that does not exist: only the name is kept.
        let missing = dir.join("missing").join("a.pdf");
        assert_eq!(start(&missing.to_string_lossy()), (None, Some("a.pdf".to_owned())));
    }
}
