// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading and rasterising PDF pages, for notes written on a PDF.
//!
//! Uses PDFium (through `pdfium-render`), loaded at runtime: the library
//! (`pdfium.dll`, `libpdfium.so`, `libpdfium.dylib`) is the file named by
//! `INKNOTE_PDFIUM` if set, else the one next to the executable, else the
//! system's. Without it every
//! request fails with [`PdfError::Unavailable`] and the rest of the app works
//! as before.
//!
//! PDFium is initialised once per process and is not reentrant, so a single
//! worker thread owns it and the open documents; callers send requests and
//! get the results back on a channel (`render`) or wait for them
//! (`page_sizes`, `underlay`). Rasterising never blocks the UI thread.
//!
//! `underlay` puts PDF pages under the pages of an exported PDF as vector
//! form XObjects (text stays text), for exporting notes written on a PDF.

use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread;

use pdfium_render::prelude::*;

/// Documents kept open by the worker (the most recently used ones).
const OPEN_DOCUMENTS: usize = 4;
/// Upper bound of a rendered page, in pixels per side.
pub const MAX_RENDER_PX: u32 = 8192;

/// A PDF file in memory, shared (never copied) between the note, the
/// renderer and exports.
pub struct PdfFile {
    pub bytes: Vec<u8>,
    /// Identifies the content (a hash), for caches.
    pub key: u64,
}

impl fmt::Debug for PdfFile {
    // Not the (possibly many MB of) bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PdfFile({} bytes, key {:016x})", self.bytes.len(), self.key)
    }
}

impl PdfFile {
    pub fn new(bytes: Vec<u8>) -> Arc<Self> {
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        Arc::new(Self { key: h.finish(), bytes })
    }
}

#[derive(Clone, Debug)]
pub enum PdfError {
    /// The PDFium library could not be loaded.
    Unavailable(String),
    /// The file is not a PDF PDFium can open (damaged, password protected...).
    Unreadable(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(e) => write!(f, "PDF support is not available (the PDFium library is missing: {e})"),
            Self::Unreadable(e) => write!(f, "cannot read the PDF: {e}"),
        }
    }
}

impl std::error::Error for PdfError {}

/// A rasterised page, or a part of one.
pub struct RenderedPage {
    /// `PdfFile::key` of the document.
    pub key: u64,
    pub page: u32,
    /// The width of the whole page that was asked for.
    pub requested_width: u32,
    /// The part asked for (`[x, y, width, height]` in the pixels of the whole
    /// page at `requested_width`); `None`: the whole page.
    pub region: Option<[u32; 4]>,
    /// RGBA, `width * height * 4` bytes, or why it failed. Always opaque
    /// (rendered on white), so it is premultiplied as well. A region on the
    /// page's edge may come back smaller than asked (never past the page).
    pub image: Result<(u32, u32, Vec<u8>), PdfError>,
}

/// The height in pixels of a page of `size` (points) rendered `width`
/// pixels wide, exactly as PDFium sizes it.
pub fn page_height_px(size: [f32; 2], width: u32) -> u32 {
    let scale = width as f32 / size[0];
    (size[1] * scale).round().max(1.0) as u32
}

/// Asks for `page` of `file` rasterised `width` pixels wide (height from the
/// page's proportions): all of it (`region` `None`, both sides capped at
/// `MAX_RENDER_PX`), or only `region` (`[x, y, width, height]` in those
/// pixels; any page size). The result arrives on `reply`; `wake` is called
/// after sending it (e.g. to repaint the UI).
///
/// Regions (tiles) are what keeps zooming into large or scanned PDFs fast:
/// only what is on screen is rasterised, and the pages stay open in the
/// worker, so their decoded images are reused from one tile to the next.
pub fn render(
    file: &Arc<PdfFile>,
    page: u32,
    width: u32,
    region: Option<[u32; 4]>,
    reply: Sender<RenderedPage>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let width = if region.is_some() { width.max(1) } else { width.clamp(1, MAX_RENDER_PX) };
    let job = Job::Render { file: file.clone(), page, width, region, reply, wake };
    if let Err(mpsc::SendError(Job::Render { file, page, width, region, reply, wake })) = worker().send(job) {
        let err = PdfError::Unavailable("the PDF worker stopped".into());
        let _ = reply.send(RenderedPage { key: file.key, page, requested_width: width, region, image: Err(err) });
        wake();
    }
}

/// A PDF page to put under a page of an exported PDF.
pub struct Underlay {
    /// The page of the exported PDF.
    pub page: u32,
    pub file: Arc<PdfFile>,
    pub source_page: u32,
    /// Where it goes: `[left, bottom, width, height]` in PDF points (y up).
    pub rect: [f32; 4],
}

/// Puts the `pages` under the content of the PDF `pdf` (bytes of a whole
/// file) and returns the new file. Blocks until done.
pub fn underlay(pdf: Vec<u8>, pages: Vec<Underlay>) -> Result<Vec<u8>, PdfError> {
    let (reply, answer) = mpsc::channel();
    worker()
        .send(Job::Underlay { pdf, pages, reply })
        .map_err(|_| PdfError::Unavailable("the PDF worker stopped".into()))?;
    answer.recv().map_err(|_| PdfError::Unavailable("the PDF worker stopped".into()))?
}

/// The size of every page of a PDF, in points (1/72 in), rotation applied.
/// Blocks until the worker answers (opening a PDF is quick).
pub fn page_sizes(file: &Arc<PdfFile>) -> Result<Vec<[f32; 2]>, PdfError> {
    let (reply, answer) = mpsc::channel();
    worker()
        .send(Job::Sizes { file: file.clone(), reply })
        .map_err(|_| PdfError::Unavailable("the PDF worker stopped".into()))?;
    answer.recv().map_err(|_| PdfError::Unavailable("the PDF worker stopped".into()))?
}

// ------------------------------------------------------------------ worker

/// Pages kept open in the worker (their decoded images are cached by PDFium).
const OPEN_PAGES: usize = 8;

enum Job {
    Sizes { file: Arc<PdfFile>, reply: Sender<Result<Vec<[f32; 2]>, PdfError>> },
    Render {
        file: Arc<PdfFile>,
        page: u32,
        width: u32,
        region: Option<[u32; 4]>,
        reply: Sender<RenderedPage>,
        wake: Arc<dyn Fn() + Send + Sync>,
    },
    Underlay { pdf: Vec<u8>, pages: Vec<Underlay>, reply: Sender<Result<Vec<u8>, PdfError>> },
}

impl Job {
    /// The same render asked for again (the older one need not be done).
    fn same_render(&self, other: &Job) -> bool {
        match (self, other) {
            (
                Job::Render { file: a, page: pa, width: wa, region: ra, .. },
                Job::Render { file: b, page: pb, width: wb, region: rb, .. },
            ) => a.key == b.key && pa == pb && wa == wb && ra == rb,
            _ => false,
        }
    }
}

fn worker() -> &'static Sender<Job> {
    static WORKER: OnceLock<Sender<Job>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = thread::Builder::new().name("pdfium".into()).spawn(move || run(rx));
        tx
    })
}

/// Where PDFium is looked for first: the executable's folder.
fn library_next_to_exe() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    Some(Pdfium::pdfium_platform_library_name_at_path(&dir))
}

fn bind() -> Result<Pdfium, PdfError> {
    let chosen = std::env::var_os("INKNOTE_PDFIUM").map(PathBuf::from).or_else(library_next_to_exe);
    let bindings = match chosen.filter(|p| p.exists()) {
        Some(path) => Pdfium::bind_to_library(path),
        None => Pdfium::bind_to_system_library(),
    };
    bindings.map(Pdfium::new).map_err(|e| PdfError::Unavailable(e.to_string()))
}

/// The documents and pages the worker keeps open, most recently used last.
/// A page is always closed before its document.
struct Open<'a> {
    pdfium: &'a Pdfium,
    documents: Vec<(u64, PdfDocument<'a>)>,
    pages: Vec<(u64, u32, PdfPage<'a>)>,
}

impl<'a> Open<'a> {
    fn document(&mut self, file: &PdfFile) -> Result<&PdfDocument<'a>, PdfError> {
        if let Some(i) = self.documents.iter().position(|(k, _)| *k == file.key) {
            let entry = self.documents.remove(i);
            self.documents.push(entry);
        } else {
            let doc = self
                .pdfium
                .load_pdf_from_byte_vec(file.bytes.clone(), None)
                .map_err(|e| PdfError::Unreadable(e.to_string()))?;
            if self.documents.len() >= OPEN_DOCUMENTS {
                let (old, _) = self.documents[0];
                self.pages.retain(|(k, _, _)| *k != old);
                self.documents.remove(0);
            }
            self.documents.push((file.key, doc));
        }
        Ok(&self.documents.last().expect("just opened").1)
    }

    fn page(&mut self, file: &PdfFile, index: u32) -> Result<&PdfPage<'a>, PdfError> {
        if let Some(i) = self.pages.iter().position(|(k, p, _)| *k == file.key && *p == index) {
            let entry = self.pages.remove(i);
            self.pages.push(entry);
        } else {
            let pdf_index = PdfPageIndex::try_from(index).map_err(|_| PdfError::Unreadable(format!("no page {}", index + 1)))?;
            let page = self.document(file)?.pages().get(pdf_index).map_err(|e| PdfError::Unreadable(e.to_string()))?;
            if self.pages.len() >= OPEN_PAGES {
                self.pages.remove(0);
            }
            self.pages.push((file.key, index, page));
        }
        Ok(&self.pages.last().expect("just opened").2)
    }
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        // Pages before their documents.
        self.pages.clear();
    }
}

fn run(jobs: Receiver<Job>) {
    let pdfium = bind();
    let mut open = pdfium.as_ref().ok().map(|pdfium| Open { pdfium, documents: Vec::new(), pages: Vec::new() });

    while let Ok(mut job) = jobs.recv() {
        // A burst of requests (scrolling, zooming): a render asked for again
        // later in the queue is only done then.
        let mut pending: Vec<Job> = jobs.try_iter().collect();
        loop {
            if !pending.iter().any(|later| job.same_render(later)) {
                match open.as_mut() {
                    Some(open) => handle(open, job),
                    None => fail(job, pdfium.as_ref().err().cloned().expect("not bound")),
                }
            }
            if pending.is_empty() {
                break;
            }
            job = pending.remove(0);
        }
    }
}

/// Answers `job` with `err` (PDFium is not available).
fn fail(job: Job, err: PdfError) {
    match job {
        Job::Sizes { reply, .. } => {
            let _ = reply.send(Err(err));
        }
        Job::Render { file, page, width, region, reply, wake } => {
            let _ = reply.send(RenderedPage { key: file.key, page, requested_width: width, region, image: Err(err) });
            wake();
        }
        Job::Underlay { reply, .. } => {
            let _ = reply.send(Err(err));
        }
    }
}

fn handle(open: &mut Open, job: Job) {
    match job {
        Job::Sizes { file, reply } => {
            let _ = reply.send(open.document(&file).and_then(sizes));
        }
        Job::Render { file, page, width, region, reply, wake } => {
            let image = open.page(&file, page).and_then(|p| rasterise(p, width, region));
            let _ = reply.send(RenderedPage { key: file.key, page, requested_width: width, region, image });
            wake();
        }
        Job::Underlay { pdf, pages, reply } => {
            let _ = reply.send(underlay_pages(open.pdfium, pdf, &pages));
        }
    }
}

fn underlay_pages(pdfium: &Pdfium, pdf: Vec<u8>, pages: &[Underlay]) -> Result<Vec<u8>, PdfError> {
    let failed = |e: PdfiumError| PdfError::Unreadable(e.to_string());
    let page_index = |p: u32| PdfPageIndex::try_from(p).map_err(|_| PdfError::Unreadable(format!("no page {}", p + 1)));
    // The source PDFs (each once) and their pages, opened before the output:
    // a page copied into it must outlive it.
    let mut sources: Vec<(u64, PdfDocument)> = Vec::new();
    for u in pages {
        if !sources.iter().any(|(k, _)| *k == u.file.key) {
            let doc = pdfium.load_pdf_from_byte_vec(u.file.bytes.clone(), None).map_err(failed)?;
            sources.push((u.file.key, doc));
        }
    }
    let source_pages = pages
        .iter()
        .map(|u| {
            let (_, doc) = sources.iter().find(|(k, _)| *k == u.file.key).expect("opened above");
            doc.pages().get(page_index(u.source_page)?).map_err(failed)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = pdfium.load_pdf_from_byte_vec(pdf, None).map_err(failed)?;
    for (u, source) in pages.iter().zip(&source_pages) {
        let quarter_turns = match source.rotation().unwrap_or(PdfPageRenderRotation::None) {
            PdfPageRenderRotation::None => 0,
            PdfPageRenderRotation::Degrees90 => 1,
            PdfPageRenderRotation::Degrees180 => 2,
            PdfPageRenderRotation::Degrees270 => 3,
        };
        let mut form = source.objects().copy_into_x_object_form_object(&mut out).map_err(failed)?;
        // The form spans the page's visible area: its crop box, else its
        // media box (unrotated, in the page's own coordinates).
        let area = source.boundaries().crop().or_else(|_| source.boundaries().media()).map_err(failed)?;
        let q = area.bounds.to_quad_points();
        let bounds = [q.left().value, q.bottom().value, q.right().value, q.top().value];
        let [a, b, c, d, e, f] = fit_matrix(bounds, quarter_turns, u.rect);
        form.reset_matrix(PdfMatrix::new(a, b, c, d, e, f)).map_err(failed)?;
        let mut page = out.pages().get(page_index(u.page)?).map_err(failed)?;
        // Index 0: under everything already on the page.
        page.objects_mut().insert_object_at_index(0, form).map_err(failed)?;
    }
    out.save_to_bytes().map_err(failed)
}

/// The matrix `[a b c d e f]` that turns a page's content (its `bounds`,
/// `[left, bottom, right, top]`) clockwise by `quarter_turns` x 90 degrees, as
/// the page's /Rotate asks, and fits it into `rect` (`[left, bottom, width,
/// height]`).
fn fit_matrix(bounds: [f32; 4], quarter_turns: u8, rect: [f32; 4]) -> [f32; 6] {
    // Clockwise in y-up coordinates: (x, y) -> (x cos + y sin, -x sin + y cos).
    let (sin, cos) = match quarter_turns % 4 {
        0 => (0.0, 1.0),
        1 => (1.0, 0.0),
        2 => (0.0, -1.0),
        _ => (-1.0, 0.0),
    };
    let (a, b, c, d) = (cos, -sin, sin, cos);
    let [l, bt, r, t] = bounds;
    let corners = [(l, bt), (r, bt), (l, t), (r, t)].map(|(x, y)| (a * x + c * y, b * x + d * y));
    let min_x = corners.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let max_x = corners.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
    let min_y = corners.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_y = corners.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
    let sx = rect[2] / (max_x - min_x).max(1e-3);
    let sy = rect[3] / (max_y - min_y).max(1e-3);
    [sx * a, sy * b, sx * c, sy * d, rect[0] - sx * min_x, rect[1] - sy * min_y]
}

fn sizes(doc: &PdfDocument) -> Result<Vec<[f32; 2]>, PdfError> {
    let pages = doc.pages();
    (0..pages.len())
        .map(|i| {
            let page = pages.get(i).map_err(|e| PdfError::Unreadable(e.to_string()))?;
            Ok([page.width().value, page.height().value])
        })
        .collect()
}

/// Renders the page `width` pixels wide: all of it, or only `region`.
fn rasterise(page: &PdfPage, width: u32, region: Option<[u32; 4]>) -> Result<(u32, u32, Vec<u8>), PdfError> {
    let unreadable = |e: PdfiumError| PdfError::Unreadable(e.to_string());
    let config = PdfRenderConfig::new()
        .set_clear_color(PdfColor::WHITE)
        .set_target_width(width as Pixels)
        .render_form_data(true)
        .render_annotations(true);
    let Some([x, y, w, h]) = region else {
        let config = config.set_maximum_height(MAX_RENDER_PX as Pixels);
        let bitmap = page.render_with_config(&config).map_err(unreadable)?;
        return Ok((bitmap.width() as u32, bitmap.height() as u32, bitmap.as_rgba_bytes()));
    };
    // The page is laid out at its full size, shifted so the region's corner
    // is the bitmap's; only the bitmap is drawn. Never past the page's edge:
    // the bitmap would keep whatever it held there.
    let page_h = page_height_px([page.width().value, page.height().value], width);
    let (w, h) = (w.min(width.saturating_sub(x)), h.min(page_h.saturating_sub(y)));
    if w == 0 || h == 0 {
        return Err(PdfError::Unreadable("the region is outside the page".into()));
    }
    let mut bitmap = PdfBitmap::empty(w as Pixels, h as Pixels, PdfBitmapFormat::BGRA).map_err(unreadable)?;
    let config = config.set_origin(-(x as Pixels), -(y as Pixels));
    page.render_into_bitmap_with_config(&mut bitmap, &config).map_err(unreadable)?;
    Ok((w, h, bitmap.as_rgba_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, Point, Stroke, StrokeKind};
    use crate::pdf_export::{self, PdfNote, PdfOptions};

    /// Uses the PDFium of the repository (crates/ui/pdfium, see its README);
    /// skipped when it has not been fetched.
    fn pdfium_available() -> bool {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/pdfium");
        let lib = ["win-x64/bin/pdfium.dll", "linux-x64/lib/libpdfium.so", "mac-arm64/lib/libpdfium.dylib"]
            .iter()
            .map(|p| dir.join(p))
            .find(|p| p.exists());
        match lib {
            Some(p) => {
                // SAFETY: set before the worker starts; tests read nothing else from it.
                unsafe { std::env::set_var("INKNOTE_PDFIUM", p) };
                true
            }
            None => {
                eprintln!("PDFium not fetched: skipped");
                false
            }
        }
    }

    #[test]
    fn sizes_and_rendering_of_an_exported_note() {
        if !pdfium_available() {
            return;
        }
        // A two-page A4 note with a thick black stroke on page 2.
        let mut doc = Document::default();
        doc.page_count = 2;
        let id = doc.alloc_id();
        let pts = (0..50).map(|i| Point::new(100.0 + i as f32 * 10.0, 500.0, 1.0)).collect();
        doc.strokes.push(Stroke::new(id, 1, StrokeKind::Pen, [0, 0, 0, 255], 40.0, pts));
        let path = std::env::temp_dir().join("inknote_pdf_pages_test.pdf");
        let notes = [PdfNote { doc: &doc, title: "t".into() }];
        pdf_export::export_notes(&notes, &path, &PdfOptions::default()).unwrap();
        let file = PdfFile::new(std::fs::read(&path).unwrap());
        let _ = std::fs::remove_file(&path);

        let sizes = page_sizes(&file).unwrap();
        assert_eq!(sizes.len(), 2);
        // A4 in points (794 x 1123 px at 96 dpi).
        assert!((sizes[0][0] - 595.5).abs() < 1.0 && (sizes[0][1] - 842.25).abs() < 1.0, "{sizes:?}");

        let (tx, rx) = mpsc::channel();
        render(&file, 1, 400, None, tx, Arc::new(|| {}));
        let page = rx.recv().unwrap();
        let (w, h, rgba) = page.image.unwrap();
        assert_eq!(w, 400);
        assert!((h as f32 - 400.0 * 1123.0 / 794.0).abs() <= 1.0);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // The stroke (page y 500 of 1123) is dark across the middle row.
        let y = (500.0 / 1123.0 * h as f32) as u32;
        let px = |x: u32| &rgba[((y * w + x) * 4) as usize..][..3];
        assert!(px(w / 2).iter().all(|&c| c < 60), "ink: {:?}", px(w / 2));
        assert!(px(5).iter().all(|&c| c > 200), "paper: {:?}", px(5));

        // Not a PDF.
        let junk = PdfFile::new(b"not a pdf".to_vec());
        assert!(matches!(page_sizes(&junk), Err(PdfError::Unreadable(_))));
    }

    #[test]
    fn importing_a_pdf_makes_a_note_to_write_on() {
        if !pdfium_available() {
            return;
        }
        let dir = std::env::temp_dir().join("inknote_import_pdf_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A three-page US Letter PDF.
        let mut doc = Document::default();
        (doc.page_count, doc.page_width, doc.page_height) = (3, 816.0, 1056.0);
        let src = dir.join("Lecture 3.pdf");
        let notes = [PdfNote { doc: &doc, title: "t".into() }];
        pdf_export::export_notes(&notes, &src, &PdfOptions::default()).unwrap();

        let note = crate::library::import_pdf(&dir, &src).unwrap();
        assert_eq!(note.file_name().unwrap(), "Lecture 3.ink");
        let (meta, loaded) = crate::storage::load_note(&note).unwrap();
        assert_eq!((meta.title.as_str(), loaded.page_count), ("Lecture 3", 3));
        assert!((loaded.page_width - 816.0).abs() < 0.5 && (loaded.page_height - 1056.0).abs() < 0.5);
        assert_eq!(loaded.pdf_page(2), Some(2));
        // The stored PDF still renders.
        let file = loaded.pdf.unwrap().file.unwrap();
        let (tx, rx) = mpsc::channel();
        render(&file, 2, 300, None, tx, Arc::new(|| {}));
        assert!(rx.recv().unwrap().image.is_ok());

        let not_pdf = dir.join("notes.pdf");
        std::fs::write(&not_pdf, b"hello").unwrap();
        assert!(crate::library::import_pdf(&dir, &not_pdf).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotated_pages_fit_their_rect() {
        let apply = |m: [f32; 6], (x, y): (f32, f32)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        // Upright: bounds onto the rect.
        let m = fit_matrix([0.0, 0.0, 100.0, 200.0], 0, [10.0, 20.0, 50.0, 100.0]);
        assert_eq!(apply(m, (0.0, 0.0)), (10.0, 20.0));
        assert_eq!(apply(m, (100.0, 200.0)), (60.0, 120.0));
        // A quarter turn clockwise: the source's bottom-left corner ends up
        // top-left, its top-left corner top-right.
        let m = fit_matrix([0.0, 0.0, 100.0, 200.0], 1, [10.0, 20.0, 200.0, 100.0]);
        assert_eq!(apply(m, (0.0, 0.0)), (10.0, 120.0));
        assert_eq!(apply(m, (0.0, 200.0)), (210.0, 120.0));
        assert_eq!(apply(m, (100.0, 0.0)), (10.0, 20.0));
    }

    #[test]
    fn exports_keep_the_pdf_under_the_ink() {
        if !pdfium_available() {
            return;
        }
        // The "PDF": an A4 page with a thick black line at y 500, x 100..600.
        let mut source = Document::default();
        let id = source.alloc_id();
        let line = (0..=50).map(|i| Point::new(100.0 + i as f32 * 10.0, 500.0, 1.0)).collect();
        source.strokes.push(Stroke::new(id, 0, StrokeKind::Pen, [0, 0, 0, 255], 40.0, line));
        let path = std::env::temp_dir().join("inknote_underlay_source.pdf");
        pdf_export::export_notes(&[PdfNote { doc: &source, title: "s".into() }], &path, &PdfOptions::default()).unwrap();
        let file = PdfFile::new(std::fs::read(&path).unwrap());
        let sizes = page_sizes(&file).unwrap();

        // Written on it: a yellow highlighter crossing the line (x 300, y
        // 300..700) and a red pen line at y 900.
        let mut note = Document::on_pdf(file, sizes);
        let id = note.alloc_id();
        let band = (0..=40).map(|i| Point::new(300.0, 300.0 + i as f32 * 10.0, 1.0)).collect();
        note.strokes.push(Stroke::new(id, 0, StrokeKind::Highlighter, [255, 235, 0, 96], 60.0, band));
        let id = note.alloc_id();
        let red = (0..=50).map(|i| Point::new(100.0 + i as f32 * 10.0, 900.0, 1.0)).collect();
        note.strokes.push(Stroke::new(id, 0, StrokeKind::Pen, [220, 0, 0, 255], 20.0, red));
        let opts = PdfOptions { paper_background: true, ..PdfOptions::default() };
        pdf_export::export_notes(&[PdfNote { doc: &note, title: "n".into() }], &path, &opts).unwrap();
        let exported = PdfFile::new(std::fs::read(&path).unwrap());
        let _ = std::fs::remove_file(&path);
        // The PDF page went in as vector content (a form XObject), not an image.
        let has = |needle: &[u8]| exported.bytes.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"/Form") && !has(b"/Image"));

        // Rendered 1:1 (794 px wide A4).
        let (tx, rx) = mpsc::channel();
        render(&exported, 0, 794, None, tx, Arc::new(|| {}));
        let (w, _, rgba) = rx.recv().unwrap().image.unwrap();
        let px = |x: u32, y: u32| -> [u8; 3] { rgba[((y * w + x) * 4) as usize..][..3].try_into().unwrap() };
        let dark = |p: [u8; 3]| p.iter().all(|&c| c < 60);
        assert!(dark(px(150, 500)), "the PDF's line: {:?}", px(150, 500));
        assert!(dark(px(300, 500)), "still black under the highlighter: {:?}", px(300, 500));
        let [r, g, b] = px(300, 350);
        assert!(r > 200 && g > 200 && b < 200, "highlighter on white: {:?}", [r, g, b]);
        let [r, g, b] = px(450, 900);
        assert!(r > 150 && g < 80 && b < 80, "pen ink: {:?}", [r, g, b]);
        // No paper fill over the PDF page (it would hide it), so white here.
        assert!(px(700, 200).iter().all(|&c| c > 250), "{:?}", px(700, 200));
    }

    #[test]
    fn tiles_match_the_whole_page() {
        if !pdfium_available() {
            return;
        }
        // A4 with a diagonal black stroke, so every part of the page differs.
        let mut doc = Document::default();
        let id = doc.alloc_id();
        let line = (0..=100).map(|i| Point::new(50.0 + i as f32 * 7.0, 60.0 + i as f32 * 10.0, 1.0)).collect();
        doc.strokes.push(Stroke::new(id, 0, StrokeKind::Pen, [0, 0, 0, 255], 12.0, line));
        let path = std::env::temp_dir().join("inknote_tiles_test.pdf");
        pdf_export::export_notes(&[PdfNote { doc: &doc, title: "t".into() }], &path, &PdfOptions::default()).unwrap();
        let file = PdfFile::new(std::fs::read(&path).unwrap());
        let _ = std::fs::remove_file(&path);
        let size = page_sizes(&file).unwrap()[0];

        let width = 1000;
        let get = |region| {
            let (tx, rx) = mpsc::channel();
            render(&file, 0, width, region, tx, Arc::new(|| {}));
            let r = rx.recv().unwrap();
            assert_eq!(r.region, region);
            r.image.unwrap()
        };
        let (w, h, whole) = get(None);
        assert_eq!((w, h), (width, page_height_px(size, width)));

        // An inner tile and one over the bottom-right corner (clamped to the
        // page) hold the whole page's pixels.
        for [x, y, tw, th] in [[300, 400, 256, 256], [900, h - 100, 256, 256]] {
            let (rw, rh, tile) = get(Some([x, y, tw, th]));
            assert_eq!((rw, rh), (tw.min(w - x), th.min(h - y)));
            for row in 0..rh {
                let a = ((y + row) * w + x) as usize * 4;
                let b = (row * rw) as usize * 4;
                let n = rw as usize * 4;
                // Equal but for antialiasing rounding (a step or two on edges).
                let off = whole[a..a + n].iter().zip(&tile[b..b + n]).position(|(p, q)| p.abs_diff(*q) > 3);
                assert!(off.is_none(), "row {row} of the tile at {x},{y} differs at byte {off:?}");
            }
        }
        // Outside the page: an error, not garbage.
        let (tx, rx) = mpsc::channel();
        render(&file, 0, width, Some([w + 10, 0, 64, 64]), tx, Arc::new(|| {}));
        assert!(rx.recv().unwrap().image.is_err());
    }

    /// Timings on a real (e.g. scanned) PDF: `INKNOTE_BENCH_PDF=<file>
    /// cargo test -p inknote-core --release bench_tiles -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_tiles() {
        let Some(path) = std::env::var_os("INKNOTE_BENCH_PDF") else { return };
        if !pdfium_available() {
            return;
        }
        let file = PdfFile::new(std::fs::read(path).unwrap());
        let sizes = page_sizes(&file).unwrap();
        let time = |page: u32, width: u32, region: Option<[u32; 4]>| {
            let start = std::time::Instant::now();
            let (tx, rx) = mpsc::channel();
            render(&file, page, width, region, tx, Arc::new(|| {}));
            rx.recv().unwrap().image.unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        };
        let width = 4096;
        println!("page 1 whole at {width}px: {:.0} ms (cold), {:.0} ms (again)", time(0, width, None), time(0, width, None));
        let h = page_height_px(sizes[1], width);
        println!("page 2 at {width}x{h}px, 512px tiles:");
        let first = time(1, width, Some([1024, 1024, 512, 512]));
        let rest: Vec<f64> = (0..6).map(|i| time(1, width, Some([512 * (i % 3), 512 * (i / 3), 512, 512]))).collect();
        println!("  first tile {first:.0} ms (cold), next six {:.0?} ms", rest);
        println!("  a screenful (~12 tiles) after the first: ~{:.0} ms", rest.iter().sum::<f64>() / 6.0 * 12.0);
    }
}
