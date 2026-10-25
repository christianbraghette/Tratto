// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The file system notes live in.
//!
//! On the desktop it is the computer's: `std::fs`, re-exported. In the
//! browser there is none the app can use synchronously, so it is a tree
//! kept in memory (`memory`), with the same functions: the web app fills it
//! from the browser's private storage before it starts and writes back what
//! changed (`memory::take_changes`).
//!
//! `Path::exists`, `Path::is_dir` and `Path::is_file` always ask the
//! computer: use `exists`, `is_dir` and `is_file` from here instead.

pub mod memory;

#[cfg(target_arch = "wasm32")]
pub use memory::{
    File, OpenOptions, copy, create_dir, create_dir_all, exists, is_dir, is_file, metadata, read, read_dir,
    read_to_string, remove_dir_all, remove_file, rename, write,
};
#[cfg(not(target_arch = "wasm32"))]
pub use std::fs::{
    File, OpenOptions, copy, create_dir, create_dir_all, metadata, read, read_dir, read_to_string, remove_dir_all,
    remove_file, rename, write,
};

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

#[cfg(not(target_arch = "wasm32"))]
pub fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn is_dir(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_dir()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn is_file(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_file()
}
