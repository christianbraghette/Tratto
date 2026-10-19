// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! InkNote core: everything that does not depend on the UI toolkit.
//!
//! - `model`          serializable data model (`Point`, `Stroke`, `Document`)
//! - `geometry`       point-to-segment math used by the eraser
//! - `history`        action-based undo/redo (no document cloning)
//! - `stroke_builder` jitter filter + velocity-adaptive smoothing + prediction
//! - `stroke_path`    smoothed stroke centreline shared by rendering and export
//! - `storage`        `.ink` files: a snapshot plus an append-only journal
//! - `journal`        the changes between two saves (journal records)
//! - `library`        notebooks / sections / notes as folders and files
//! - `pdf_export`     vector PDF export (hand-written PDF writer)
//! - `pdf_pages`      reading and rasterising PDF pages to write on (PDFium)

pub mod geometry;
pub mod history;
mod journal;
pub mod library;
pub mod model;
pub mod pdf_export;
pub mod pdf_pages;
pub mod storage;
pub mod stroke_builder;
pub mod stroke_path;
