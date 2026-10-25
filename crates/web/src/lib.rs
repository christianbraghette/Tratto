// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tratto's web reader: `.ink` notes and PDFs shown in the browser, with
//! the app's own code compiled to WebAssembly.
//!
//! - a note is read by `tratto_core::storage` (the snapshot, then its
//!   journal), exactly as the app opens it
//! - a PDF becomes a note written on it (`Document::on_pdf`), as the app
//!   imports one, so both are shown the same way
//! - a page is painted on a canvas (`paint`) with the curve the app draws
//!   and exports (`stroke_path`), in the app's order and colours
//! - PDF pages are rasterised by PDFium (`pdf_pages`), here its WASM build:
//!   the page binds it with `initialize_pdfium_render` before opening a PDF
//!   (see `www/main.js`)
//! - "Export as PDF" is the app's vector export (`pdf_export`)
//!
//! Text boxes are drawn from their stored lines, without their Markdown
//! styles: laying them out takes the app's fonts and egui (as page previews
//! and PDF exports of notes not opened in the app also do).

mod paint;

use std::path::Path;

use tratto_core::model::Document;
use tratto_core::pdf_export::{self, PdfNote, PdfOptions};
use tratto_core::pdf_pages::{self, PdfFile};
use tratto_core::storage;
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

/// A note or a PDF, open in the reader.
#[wasm_bindgen]
pub struct Reader {
    title: String,
    note: bool,
    doc: Document,
    cache: paint::Cache,
}

#[wasm_bindgen]
impl Reader {
    /// Opens the file `bytes`, called `name`: a note (`.ink`) or a PDF,
    /// told apart by their contents.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>, name: &str) -> Result<Reader, JsError> {
        let (title, note, doc) = open(bytes, name).map_err(|e| JsError::new(&e))?;
        Ok(Reader { title, note, doc, cache: paint::Cache::default() })
    }

    /// The note's title, or the PDF's file name.
    #[wasm_bindgen(getter)]
    pub fn title(&self) -> String {
        self.title.clone()
    }

    /// A note (else a PDF).
    #[wasm_bindgen(getter, js_name = isNote)]
    pub fn is_note(&self) -> bool {
        self.note
    }

    #[wasm_bindgen(getter, js_name = pageCount)]
    pub fn page_count(&self) -> u32 {
        self.doc.page_count.max(1)
    }

    /// Size of every page, in document units (1/96 in: CSS pixels).
    #[wasm_bindgen(getter, js_name = pageWidth)]
    pub fn page_width(&self) -> f32 {
        self.doc.page_width
    }

    #[wasm_bindgen(getter, js_name = pageHeight)]
    pub fn page_height(&self) -> f32 {
        self.doc.page_height
    }

    /// Paints `page` on `canvas`, `scale` canvas pixels per document unit
    /// (the canvas is sized to the page). Returns what could not be shown
    /// (the PDF page under it, a picture), if anything.
    pub fn render(&mut self, page: u32, canvas: &HtmlCanvasElement, scale: f32) -> Result<Option<String>, JsError> {
        if page >= self.page_count() {
            return Err(JsError::new(&format!("no page {}", page + 1)));
        }
        paint::page(&self.doc, page, canvas, scale, &mut self.cache).map_err(|e| JsError::new(&js_message(&e)))
    }

    /// The whole note as a PDF, the app's export with its default options
    /// (a PDF it is written on goes under its pages).
    #[wasm_bindgen(js_name = exportPdf)]
    pub fn export_pdf(&self) -> Result<Vec<u8>, JsError> {
        let note = PdfNote { doc: &self.doc, title: self.title.clone() };
        let opts = PdfOptions { title: self.title.clone(), ..PdfOptions::default() };
        pdf_export::export_bytes(&[note], &opts).map_err(|e| JsError::new(&e.to_string()))
    }
}

/// `(title, is a note, document)` of the file `bytes` called `name`.
fn open(bytes: Vec<u8>, name: &str) -> Result<(String, bool, Document), String> {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned());
    if bytes.starts_with(b"INK1") {
        let (meta, doc) = storage::load_note_bytes(&bytes).map_err(|e| e.to_string())?;
        let title = if meta.title.is_empty() { stem.unwrap_or_default() } else { meta.title };
        return Ok((title, true, doc));
    }
    // The header may follow some junk (PDF readers allow 1024 bytes of it).
    if !bytes[..bytes.len().min(1024)].windows(5).any(|w| w == b"%PDF-") {
        return Err("not a Tratto note (.ink) or a PDF".into());
    }
    let file = PdfFile::new(bytes);
    let sizes = pdf_pages::page_sizes(&file).map_err(|e| e.to_string())?;
    Ok((stem.unwrap_or_else(|| "PDF".into()), false, Document::on_pdf(file, sizes)))
}

/// The message of a JavaScript exception (canvas calls).
fn js_message(e: &JsValue) -> String {
    e.as_string()
        .or_else(|| e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())))
        .unwrap_or_else(|| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tratto_core::model::{Point, Stroke, StrokeKind};
    use tratto_core::storage::NoteMeta;

    #[test]
    fn notes_open_from_their_bytes() {
        let mut doc = Document::default();
        doc.page_count = 3;
        let id = doc.alloc_id();
        let points = vec![Point::new(1.0, 2.0, 0.5), Point::new(30.0, 40.0, 0.7)];
        doc.strokes.push(Stroke::new(id, 2, StrokeKind::Pen, [0, 0, 0, 255], 3.0, points));
        let path = std::env::temp_dir().join("tratto_web_open.ink");
        storage::save_note(&path, &mut NoteMeta::new("Lecture 4", false), &doc).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let (title, note, opened) = open(bytes, "renamed.ink").unwrap();
        assert_eq!((title.as_str(), note, opened.page_count), ("Lecture 4", true, 3));
        assert_eq!(opened.strokes[0].points.len(), 2);
    }

    #[test]
    fn other_files_are_refused() {
        assert!(open(b"hello".to_vec(), "a.txt").is_err());
        assert!(open(b"INK1 cut short".to_vec(), "a.ink").is_err());
    }
}
