// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Serializable document model. Stroke coordinates are in *page space*:
//! document units relative to the top-left corner of the stroke's page.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use emath::{Pos2, Rect, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

use crate::geometry::point_segment_distance;
use crate::pdf_pages::PdfFile;

/// A4 at 96 dpi.
pub const DEFAULT_PAGE_WIDTH: f32 = 794.0;
pub const DEFAULT_PAGE_HEIGHT: f32 = 1123.0;

/// (name, physical size, width, height) in document units (96 dpi), portrait.
pub const PAPERS: [(&str, &str, f32, f32); 5] = [
    ("A3", "297 × 420 mm", 1123.0, 1587.0),
    ("A4", "210 × 297 mm", 794.0, 1123.0),
    ("A5", "148 × 210 mm", 559.0, 794.0),
    ("Letter", "8.5 × 11 in", 816.0, 1056.0),
    ("Legal", "8.5 × 14 in", 816.0, 1344.0),
];

/// Human-readable name of a page size ("A4 portrait", "Custom", ...).
pub fn paper_label(w: f32, h: f32) -> String {
    if w >= CANVAS_SIZE && h >= CANVAS_SIZE {
        return "Infinite".to_owned();
    }
    let close = |a: f32, b: f32| (a - b).abs() < 0.5;
    for (name, _, pw, ph) in PAPERS {
        if close(w, pw) && close(h, ph) {
            return format!("{name} portrait");
        }
        if close(w, ph) && close(h, pw) {
            return format!("{name} landscape");
        }
    }
    "Custom".to_owned()
}

/// The paper colour (screen and PDF).
pub const PAPER_RGB: [u8; 3] = [255, 255, 255];

/// The opaque colour a highlighter of colour `rgba` (its alpha is the ink's
/// strength) leaves on a background `under`.
///
/// Highlighters are painted in this colour with a darken blend (each point
/// keeps the darker of the highlighter and what is under it, on screen and
/// in exported PDFs): strokes overlapping each other (or themselves) never
/// darken twice, and ink, text and the pages of a PDF under them stay
/// visible, as with a real highlighter.
pub fn highlight_on(rgba: [u8; 4], under: [u8; 3]) -> [u8; 3] {
    let a = rgba[3] as f32 / 255.0;
    let mix = |c: u8, u: u8| (u as f32 + (c as f32 - u as f32) * a).round() as u8;
    [mix(rgba[0], under[0]), mix(rgba[1], under[1]), mix(rgba[2], under[2])]
}

/// A single input sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    /// Normalised pen pressure in `0.0..=1.0` (1.0 for a mouse).
    pub pressure: f32,
}

impl Point {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self { x, y, pressure }
    }

    pub fn pos2(&self) -> Pos2 {
        Pos2::new(self.x, self.y)
    }

    pub fn distance(&self, other: &Point) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeKind {
    Pen,
    Highlighter,
}

/// A finished stroke.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stroke {
    pub id: u64,
    /// Zero-based page the stroke belongs to; points are in that page's space.
    pub page: u32,
    pub kind: StrokeKind,
    /// Unmultiplied RGBA. Highlighters carry their alpha here.
    pub color: [u8; 4],
    /// Base width in document units.
    pub width: f32,
    /// Shared: copies of the document (undo history, the last save, a save
    /// in the background) cost nothing per point. Encoded like a `Vec`.
    pub points: Arc<[Point]>,
    /// Cached `[min_x, min_y, max_x, max_y]` for culling and eraser rejection.
    pub bounds: [f32; 4],
    /// Bumped whenever the points change; invalidates cached meshes. Not saved.
    #[serde(skip)]
    pub rev: u32,
}

impl Stroke {
    pub fn new(id: u64, page: u32, kind: StrokeKind, color: [u8; 4], width: f32, points: Vec<Point>) -> Self {
        let bounds = compute_bounds(&points);
        Self { id, page, kind, color, width, points: points.into(), bounds, rev: 0 }
    }

    /// Moves the stroke (page space).
    pub fn translate(&mut self, dx: f32, dy: f32) {
        for p in Arc::make_mut(&mut self.points) {
            p.x += dx;
            p.y += dy;
        }
        self.bounds[0] += dx;
        self.bounds[2] += dx;
        self.bounds[1] += dy;
        self.bounds[3] += dy;
        self.rev = fresh_rev();
    }

    /// The stroke with every point moved by `f` (page space) and its width
    /// times `scale` (resized, rotated).
    pub fn mapped(&self, f: impl Fn(Pos2) -> Pos2, scale: f32) -> Stroke {
        let points: Vec<Point> = self
            .points
            .iter()
            .map(|p| {
                let q = f(p.pos2());
                Point { x: q.x, y: q.y, ..*p }
            })
            .collect();
        let bounds = compute_bounds(&points);
        Stroke { points: points.into(), bounds, width: self.width * scale, rev: fresh_rev(), ..self.clone() }
    }

    /// The stroke in colour `rgb`; a highlighter keeps its strength (alpha).
    pub fn recolored(&self, rgb: [u8; 3]) -> Stroke {
        let color = [rgb[0], rgb[1], rgb[2], self.color[3]];
        Stroke { color, rev: fresh_rev(), ..self.clone() }
    }

    /// What is left of the stroke after a circular eraser at `center` with
    /// `radius` (partial erasing): its pieces outside the circle, in order;
    /// none when it is all inside. `None` if the eraser misses it.
    pub fn erased(&self, center: Pos2, radius: f32) -> Option<Vec<Vec<Point>>> {
        if !self.hit_test(center, radius) {
            return None;
        }
        // As far as the stroke's edge: the ends left are clear of the eraser.
        let reach = radius + self.width * 0.5;
        let [first, rest @ ..] = &self.points[..] else { return None };
        if rest.is_empty() {
            return Some(Vec::new());
        }
        let lerp = |a: &Point, b: &Point, t: f32| Point {
            x: a.x + (b.x - a.x) * t,
            y: a.y + (b.y - a.y) * t,
            pressure: a.pressure + (b.pressure - a.pressure) * t,
        };
        let mut pieces = Vec::new();
        let mut piece = Vec::new();
        let mut cut = false;
        if first.pos2().distance(center) > reach {
            piece.push(*first);
        }
        for w in self.points.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            match segment_in_circle(a.pos2(), b.pos2(), center, reach) {
                None => {
                    // After a cut that ended right at `a` (on the eraser's edge).
                    if piece.is_empty() {
                        piece.push(*a);
                    }
                    piece.push(*b);
                }
                Some((t0, t1)) => {
                    cut = true;
                    if t0 > 0.0 {
                        piece.push(lerp(a, b, t0));
                    }
                    if !piece.is_empty() {
                        pieces.push(std::mem::take(&mut piece));
                    }
                    if t1 < 1.0 {
                        piece.push(lerp(a, b, t1));
                        piece.push(*b);
                    }
                }
            }
        }
        if !piece.is_empty() {
            pieces.push(piece);
        }
        if !cut {
            return None;
        }
        // Crumbs left at the eraser's edge would show as dots.
        let length = |p: &Vec<Point>| p.windows(2).map(|w| w[0].distance(&w[1])).sum::<f32>();
        pieces.retain(|p| p.len() >= 2 && length(p) >= MIN_PIECE);
        Some(pieces)
    }
}

/// Pieces of an erased stroke shorter than this (document units) are dropped.
const MIN_PIECE: f32 = 1.0;

/// The part of the segment `a`-`b` inside the circle at `c` of radius `r`,
/// as the range of `t` (`a + (b - a) * t`) within `0..=1`; `None` when the
/// segment stays outside or only touches the circle.
fn segment_in_circle(a: Pos2, b: Pos2, c: Pos2, r: f32) -> Option<(f32, f32)> {
    let d: Vec2 = b - a;
    let f: Vec2 = a - c;
    let qa = d.dot(d);
    if qa <= f32::EPSILON {
        return (f.length() <= r).then_some((0.0, 1.0));
    }
    let qb = 2.0 * f.dot(d);
    let qc = f.dot(f) - r * r;
    let disc = qb * qb - 4.0 * qa * qc;
    if disc <= 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let (t0, t1) = ((-qb - sq) / (2.0 * qa), (-qb + sq) / (2.0 * qa));
    if t1 <= 0.0 || t0 >= 1.0 {
        return None;
    }
    Some((t0.max(0.0), t1.min(1.0)))
}

/// A `rev` no stroke has had: cached meshes of the old points or colour
/// are never taken for the new ones, even after an undo.
fn fresh_rev() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Stroke {
    /// Stroke-based eraser test: does a circle at `center` with `radius`
    /// touch any segment of this stroke?
    pub fn hit_test(&self, center: Pos2, radius: f32) -> bool {
        let reach = radius + self.width * 0.5;
        let [min_x, min_y, max_x, max_y] = self.bounds;
        if center.x < min_x - reach || center.x > max_x + reach || center.y < min_y - reach || center.y > max_y + reach
        {
            return false; // cheap bounding-box rejection
        }
        match &self.points[..] {
            [] => false,
            [p] => p.pos2().distance(center) <= reach,
            pts => pts.windows(2).any(|w| point_segment_distance(center, w[0].pos2(), w[1].pos2()) <= reach),
        }
    }
}

fn compute_bounds(points: &[Point]) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in points {
        b[0] = b[0].min(p.x);
        b[1] = b[1].min(p.y);
        b[2] = b[2].max(p.x);
        b[3] = b[3].max(p.y);
    }
    if points.is_empty() { [0.0; 4] } else { b }
}

/// A keyboard-typed text box. `x`/`y` is its top-left corner in page space.
///
/// `text` is Markdown. `lines` is the text as shown (without the Markdown
/// marks) wrapped to `width`, computed with egui's font when the box is
/// committed, for what draws it roughly (page previews). `layout` is the text
/// laid out in its styles, which the screen and the PDF draw; it is not
/// saved, the UI (which has the fonts) makes it again after loading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    pub id: u64,
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// Font size in document units.
    pub size: f32,
    pub color: [u8; 3],
    /// The source text, Markdown (may contain explicit newlines).
    pub text: String,
    /// `text` as shown, wrapped to `width`.
    pub lines: Vec<String>,
    /// Height of one line in document units (on average, with headings).
    pub line_h: f32,
    #[serde(skip)]
    pub layout: Option<Arc<TextLayout>>,
}

impl TextBox {
    pub fn height(&self) -> f32 {
        match &self.layout {
            Some(l) => l.height,
            None => self.lines.len().max(1) as f32 * self.line_h,
        }
    }

    /// Bounding box in page space.
    pub fn rect(&self) -> Rect {
        Rect::from_min_size(pos2(self.x, self.y), vec2(self.width, self.height()))
    }
}

/// A text box's Markdown laid out: runs of text in one style, placed in the
/// box, and the marks drawn with them. Document units, from the box's
/// top-left corner.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextLayout {
    pub runs: Vec<TextRun>,
    pub marks: Vec<TextMark>,
    pub height: f32,
}

/// Text of one style on one line.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub text: String,
    /// Where `text` starts in the box's Markdown (a byte offset); none for
    /// what the Markdown only stands for (a list's bullet).
    pub source: Option<usize>,
    /// Left end, and the baseline.
    pub x: f32,
    pub baseline: f32,
    /// Top of its line, and the line's height.
    pub top: f32,
    pub line_h: f32,
    pub width: f32,
    /// Font size, and the font's ascent at that size.
    pub size: f32,
    pub ascent: f32,
    pub style: RunStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunStyle {
    pub bold: bool,
    pub italic: bool,
    /// Code: in a monospace font.
    pub code: bool,
    pub strike: bool,
    /// A link's text.
    pub underline: bool,
    /// Code within a line, on a grey background of its own (a code block
    /// has one for the whole block, a [`TextMark::Shade`]).
    pub code_span: bool,
}

/// What a text box draws besides its text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextMark {
    /// A thin line: a horizontal rule (`---`), a table's grid.
    Line { x0: f32, y0: f32, x1: f32, y1: f32 },
    /// A light grey area: a code block.
    Shade { x: f32, top: f32, width: f32, height: f32 },
    /// The bar left of a quote (`> `).
    Quote { x: f32, top: f32, bottom: f32 },
    /// A task's box (`- [ ]`, `- [x]`), `size` wide and high.
    Check { x: f32, top: f32, size: f32, checked: bool },
}

/// The paper. New kinds go at the end: files store the variant's index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageKind {
    Blank,
    Ruled,
    Grid,
    /// A dot where the lines of the grid would cross.
    Dotted,
}

impl PageKind {
    /// Every kind, with its name, in the order menus list them.
    pub const ALL: [(PageKind, &'static str); 4] = [
        (PageKind::Blank, "Blank"),
        (PageKind::Ruled, "Ruled"),
        (PageKind::Grid, "Grid"),
        (PageKind::Dotted, "Dotted"),
    ];
}

/// Radius of the dots of dotted paper, in document units (PDF, pictures;
/// the screen keeps them about this large at any zoom).
pub const DOT_RADIUS: f32 = 1.2;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageStyle {
    pub kind: PageKind,
    /// Distance between rules/grid lines in document units.
    pub spacing: f32,
}

impl Default for PageStyle {
    fn default() -> Self {
        Self { kind: PageKind::Ruled, spacing: 36.0 }
    }
}

/// A PDF the note is written on: its pages are drawn under the ink of the
/// note's pages.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PdfBackground {
    /// Size of every page of the PDF, in points (1/72 in).
    pub sizes: Vec<[f32; 2]>,
    /// For every page of the note, the PDF page under it (`None`: plain
    /// paper, e.g. a page added later). Always `page_count` long.
    pub pages: Vec<Option<u32>>,
    /// The PDF itself. Not part of the encoded document: the file stores it
    /// raw after it (see `storage`).
    #[serde(skip)]
    pub file: Option<Arc<PdfFile>>,
}

impl PdfBackground {
    /// Where PDF page `pdf_page` sits on a note page of `page_w` x `page_h`:
    /// scaled to fit and centred, as `(x, y, width, height)` in page units.
    pub fn placement(&self, pdf_page: u32, page_w: f32, page_h: f32) -> Option<[f32; 4]> {
        let [w, h] = *self.sizes.get(pdf_page as usize)?;
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let s = (page_w / w).min(page_h / h);
        let (dw, dh) = (w * s, h * s);
        Some([(page_w - dw) * 0.5, (page_h - dh) * 0.5, dw, dh])
    }
}

/// Side of an infinite page, in document units.
///
/// An infinite page is a paper size: a note of one page this large, square,
/// written on from its top-left corner like any page, which the view shows
/// only as far as it is written on (it grows as it is, like OneNote's).
/// Over 25 m of paper each way, more than anyone writes, while every
/// coordinate stays precise in an `f32`. Nothing else marks it (see
/// `Document::is_canvas`), so the file format is that of any note. Other
/// paper sizes stay far below it.
pub const CANVAS_SIZE: f32 = 100_000.0;
/// Paper right of and below what is on an infinite page when it is made a
/// page of its own (`canvas_area`).
const CANVAS_MARGIN: f32 = 48.0;

/// The whole note: `page_count` pages of `page_width` x `page_height`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub page_width: f32,
    pub page_height: f32,
    pub page_count: u32,
    pub page: PageStyle,
    /// Draw order: later strokes are painted on top.
    pub strokes: Vec<Stroke>,
    /// Typed text boxes.
    pub texts: Vec<TextBox>,
    next_id: u64,
    /// The PDF the note is written on, if any.
    pub pdf: Option<PdfBackground>,
    /// Pictures, in draw order (under the ink and the text).
    pub images: Vec<Image>,
}

/// A document as format 6 saved it, before pictures (see `storage`).
#[derive(Deserialize)]
pub(crate) struct DocumentV6 {
    page_width: f32,
    page_height: f32,
    page_count: u32,
    page: PageStyle,
    strokes: Vec<Stroke>,
    texts: Vec<TextBox>,
    next_id: u64,
    pdf: Option<PdfBackground>,
}

impl From<DocumentV6> for Document {
    fn from(d: DocumentV6) -> Self {
        let DocumentV6 { page_width, page_height, page_count, page, strokes, texts, next_id, pdf } = d;
        Self { page_width, page_height, page_count, page, strokes, texts, next_id, pdf, images: Vec::new() }
    }
}

/// A picture placed on a page: its top-left corner `x`, `y` and its size, in
/// page space. Its file is shared by its copies (undo, the saved document).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub id: u64,
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub data: Arc<ImageData>,
}

impl Image {
    /// Its place on its page.
    pub fn rect(&self) -> Rect {
        Rect::from_min_size(pos2(self.x, self.y), vec2(self.width, self.height))
    }
}

/// A picture's file: PNG or JPEG bytes, its size in pixels, and `key`, a
/// hash of the bytes that names the file (the journal refers to it by it).
#[derive(Clone, Serialize, Deserialize)]
pub struct ImageData {
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

impl ImageData {
    pub fn new(width: u32, height: u32, bytes: Vec<u8>) -> Self {
        Self { key: fnv1a(&bytes), width, height, bytes }
    }

    /// A JPEG (else a PNG).
    pub fn is_jpeg(&self) -> bool {
        self.bytes.starts_with(&[0xFF, 0xD8])
    }
}

/// The same file: the same key (comparing the bytes would be slow).
impl PartialEq for ImageData {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.width == other.width && self.height == other.height
    }
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageData({:x}, {}x{}, {} bytes)", self.key, self.width, self.height, self.bytes.len())
    }
}

/// 64-bit FNV-1a: a hash that stays the same across builds (saved keys).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// Points (1/72 in) to document units (1/96 in).
pub const PT_TO_PX: f32 = 96.0 / 72.0;

impl Default for Document {
    fn default() -> Self {
        Self {
            page_width: DEFAULT_PAGE_WIDTH,
            page_height: DEFAULT_PAGE_HEIGHT,
            page_count: 1,
            page: PageStyle::default(),
            strokes: Vec::new(),
            texts: Vec::new(),
            next_id: 1,
            pdf: None,
            images: Vec::new(),
        }
    }
}

impl Document {
    /// The note's page is infinite (see `CANVAS_SIZE`).
    pub fn is_canvas(&self) -> bool {
        self.page_width >= CANVAS_SIZE && self.page_height >= CANVAS_SIZE
    }

    /// The paper can be made infinite: a note of one page, not on a PDF
    /// (its pages have their size).
    pub fn can_be_canvas(&self) -> bool {
        self.page_count <= 1 && self.pdf.is_none()
    }

    /// What is on page `page`, as a rectangle in page space (strokes with
    /// their width, text boxes, pictures); `None` when nothing is.
    pub fn content_bounds(&self, page: u32) -> Option<Rect> {
        let strokes = self.strokes.iter().filter(|s| s.page == page).map(|s| {
            let [x0, y0, x1, y1] = s.bounds;
            Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)).expand(s.width * 0.5)
        });
        let texts = self.texts.iter().filter(|t| t.page == page).map(TextBox::rect);
        let images = self.images.iter().filter(|i| i.page == page).map(Image::rect);
        strokes.chain(texts).chain(images).reduce(|a, b| a.union(b))
    }

    /// The part of an infinite page that makes a page of it (exporting,
    /// printing, the library's preview): from its corner to what is on it,
    /// with some paper right and below, at least a page of the default size.
    pub fn canvas_area(&self) -> Rect {
        let written = self.content_bounds(0).map_or(pos2(0.0, 0.0), |c| c.max + vec2(CANVAS_MARGIN, CANVAS_MARGIN));
        let max = written.max(pos2(DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT)).min(pos2(CANVAS_SIZE, CANVAS_SIZE));
        Rect::from_min_max(pos2(0.0, 0.0), max)
    }

    /// An infinite page as a page as large as what is on it (`canvas_area`),
    /// for what needs a page of a sensible size; any other note as it is.
    pub fn as_pages(&self) -> std::borrow::Cow<'_, Document> {
        if !self.is_canvas() {
            return std::borrow::Cow::Borrowed(self);
        }
        let area = self.canvas_area();
        let mut doc = self.clone();
        (doc.page_width, doc.page_height) = (area.width(), area.height());
        std::borrow::Cow::Owned(doc)
    }

    /// A note written on `file`: one page per PDF page, the note's pages as
    /// large as the first PDF page (others are fitted into them), blank
    /// paper (the PDF has its own lines).
    pub fn on_pdf(file: Arc<PdfFile>, sizes: Vec<[f32; 2]>) -> Self {
        let [w, h] = sizes.first().copied().unwrap_or([DEFAULT_PAGE_WIDTH / PT_TO_PX, DEFAULT_PAGE_HEIGHT / PT_TO_PX]);
        let count = sizes.len().max(1) as u32;
        Self {
            page_width: w * PT_TO_PX,
            page_height: h * PT_TO_PX,
            page_count: count,
            page: PageStyle { kind: PageKind::Blank, ..PageStyle::default() },
            pdf: Some(PdfBackground { sizes, pages: (0..count).map(Some).collect(), file: Some(file) }),
            ..Self::default()
        }
    }

    /// The PDF page under note page `page`, if any.
    pub fn pdf_page(&self, page: u32) -> Option<u32> {
        self.pdf.as_ref()?.pages.get(page as usize).copied().flatten()
    }

    /// Note page `page` gets the rules (or grid) of the paper: not a page
    /// written on a PDF page, which is its own paper.
    pub fn ruled(&self, page: u32) -> bool {
        self.page.kind != PageKind::Blank && self.pdf_page(page).is_none()
    }

    /// Allocates a document-unique stroke id.
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The id `alloc_id` hands out next (the journal records it).
    pub(crate) fn next_id(&self) -> u64 {
        self.next_id
    }

    pub(crate) fn set_next_id(&mut self, id: u64) {
        self.next_id = id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_only_on_pages_without_a_pdf_page() {
        let mut doc = Document::on_pdf(PdfFile::new(Vec::new()), vec![[100.0, 100.0]]);
        doc.page.kind = PageKind::Ruled;
        // A page added after the PDF's.
        doc.page_count = 2;
        doc.pdf.as_mut().unwrap().pages.push(None);
        assert!(!doc.ruled(0) && doc.ruled(1));
        doc.page.kind = PageKind::Blank;
        assert!(!doc.ruled(1));
    }

    /// Shared points are encoded exactly like the `Vec` the format was
    /// defined with: files do not change.
    #[test]
    fn shared_points_encode_like_a_vec() {
        #[derive(Serialize)]
        struct Written {
            id: u64,
            page: u32,
            kind: StrokeKind,
            color: [u8; 4],
            width: f32,
            points: Vec<Point>,
            bounds: [f32; 4],
        }
        let points = vec![Point::new(1.0, 2.0, 0.5), Point::new(3.0, 4.0, 0.25)];
        let stroke = Stroke::new(7, 2, StrokeKind::Pen, [1, 2, 3, 255], 2.5, points.clone());
        let written = Written {
            id: 7,
            page: 2,
            kind: StrokeKind::Pen,
            color: [1, 2, 3, 255],
            width: 2.5,
            points,
            bounds: stroke.bounds,
        };
        let bytes = bincode::serialize(&stroke).unwrap();
        assert_eq!(bytes, bincode::serialize(&written).unwrap());
        let back: Stroke = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.points, stroke.points);
    }

    fn line(width: f32) -> Stroke {
        let points = (0..=10).map(|i| Point::new(i as f32 * 10.0, 0.0, 1.0)).collect();
        Stroke::new(1, 0, StrokeKind::Pen, [0, 0, 0, 255], width, points)
    }

    #[test]
    fn erasing_the_middle_leaves_two_pieces() {
        let s = line(2.0);
        let pieces = s.erased(pos2(50.0, 0.0), 9.0).unwrap();
        assert_eq!(pieces.len(), 2);
        // Cut where the eraser's edge meets the stroke's edge.
        assert!((pieces[0].last().unwrap().x - 40.0).abs() < 1e-3);
        assert!((pieces[1][0].x - 60.0).abs() < 1e-3);
        assert_eq!((pieces[0][0].x, pieces[1].last().unwrap().x), (0.0, 100.0));
    }

    #[test]
    fn erasing_an_end_or_all_of_it() {
        let s = line(2.0);
        let end = s.erased(pos2(100.0, 0.0), 9.0).unwrap();
        assert_eq!(end.len(), 1);
        assert!((end[0].last().unwrap().x - 90.0).abs() < 1e-3);
        assert!(s.erased(pos2(50.0, 0.0), 80.0).unwrap().is_empty());
        // Missed: nothing to do.
        assert!(s.erased(pos2(50.0, 30.0), 9.0).is_none());
    }

    #[test]
    fn erasing_between_samples_cuts_the_segment() {
        let s = Stroke::new(
            1,
            0,
            StrokeKind::Pen,
            [0; 4],
            0.0,
            vec![Point::new(0.0, 0.0, 0.2), Point::new(100.0, 0.0, 1.0)],
        );
        let pieces = s.erased(pos2(50.0, 0.0), 10.0).unwrap();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].len(), 2);
        assert!((pieces[0][1].x - 40.0).abs() < 1e-3);
        // The pressure follows along the segment.
        assert!((pieces[1][0].pressure - 0.68).abs() < 1e-3);
    }

    #[test]
    fn mapped_and_recolored_strokes_are_new_to_the_caches() {
        let s = line(2.0);
        let m = s.mapped(|p| pos2(p.x * 2.0, p.y + 5.0), 2.0);
        assert_eq!((m.width, m.bounds), (4.0, [0.0, 5.0, 200.0, 5.0]));
        assert_ne!(m.rev, s.rev);
        let mut h = line(10.0);
        h.color = [255, 235, 0, 96];
        let r = h.recolored([10, 20, 30]);
        assert_eq!(r.color, [10, 20, 30, 96]);
        assert_ne!(r.rev, m.rev);
    }

    #[test]
    fn an_infinite_page_is_made_a_page_as_large_as_what_is_on_it() {
        let mut doc = Document { page_width: CANVAS_SIZE, page_height: CANVAS_SIZE, ..Document::default() };
        assert!(doc.is_canvas() && !Document::default().is_canvas());
        // Empty: a page of the default size.
        assert_eq!(doc.canvas_area().max, pos2(DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT));
        let mut s = line(2.0);
        s.translate(1000.0, 2000.0);
        doc.strokes.push(s);
        // From the corner to the stroke (and its width), and some paper.
        let area = doc.canvas_area();
        assert_eq!(area.min, pos2(0.0, 0.0));
        assert!(area.contains_rect(Rect::from_min_max(pos2(999.0, 1999.0), pos2(1101.0, 2001.0))));
        let page = doc.as_pages();
        assert!(!page.is_canvas());
        assert_eq!((page.page_width, page.page_height), (area.width(), area.height()));
        assert_eq!(page.strokes[0].points[0].pos2(), pos2(1000.0, 2000.0), "nothing moves");
        // A note with pages stays as it is.
        assert!(matches!(Document::default().as_pages(), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn highlight_mixes_with_the_paper() {
        assert_eq!(highlight_on([255, 0, 0, 255], [255, 255, 255]), [255, 0, 0]);
        assert_eq!(highlight_on([255, 0, 0, 0], [10, 20, 30]), [10, 20, 30]);
        assert_eq!(highlight_on([0, 0, 0, 128], [255, 255, 255]), [127, 127, 127]);
    }
}
