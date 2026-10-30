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

    /// Around its points (page space), without its width.
    pub fn rect(&self) -> Rect {
        let [x0, y0, x1, y1] = self.bounds;
        Rect::from_min_max(pos2(x0, y0), pos2(x1, y1))
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
    /// paper, e.g. a page added later). Always `page_count` long, except on
    /// an infinite page: there, the PDF's pages on it, top to bottom (see
    /// `canvas_slot`).
    pub pages: Vec<Option<u32>>,
    /// On an infinite page, the room between two of its PDF's pages
    /// (document units, `PDF_GAPS`).
    pub gap: f32,
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

    /// The size of each PDF page's place on an infinite page: that of the
    /// first PDF page (the others are fitted into it), as large as the pages
    /// of a note on the PDF start (see `Document::on_pdf`).
    pub fn slot_size(&self) -> Vec2 {
        let [w, h] = self.sizes.first().copied().unwrap_or([DEFAULT_PAGE_WIDTH / PT_TO_PX, DEFAULT_PAGE_HEIGHT / PT_TO_PX]);
        (vec2(w, h) * PT_TO_PX).max(vec2(1.0, 1.0))
    }

    /// On an infinite page, the place of `pages[i]`: the PDF's pages one
    /// under the other, `gap` apart, `CANVAS_PDF_INSET` from the page's
    /// corner.
    pub fn canvas_slot(&self, i: usize) -> Rect {
        let size = self.slot_size();
        let top = CANVAS_PDF_INSET + i as f32 * (size.y + self.gap);
        Rect::from_min_size(pos2(CANVAS_PDF_INSET, top), size)
    }

    /// On an infinite page, the place of a PDF page whose rows (from its top
    /// to the next one's) the height `y` is in: what is beside a page moves
    /// with it. Above the first page, none.
    pub fn canvas_row(&self, y: f32) -> Option<usize> {
        let stride = self.slot_size().y + self.gap;
        let row = ((y - CANVAS_PDF_INSET) / stride).floor();
        (row >= 0.0).then(|| (row as usize).min(self.pages.len().saturating_sub(1)))
    }

    /// On an infinite page, the band of the height `y`: from the top of the
    /// place of `pages[i]` to that of the next one, counted on past the last
    /// one (a page's own and what is beside it). Above the first, none.
    fn canvas_band(&self, y: f32) -> Option<usize> {
        let row = ((y - CANVAS_PDF_INSET) / (self.slot_size().y + self.gap)).floor();
        (row >= 0.0).then_some(row as usize)
    }
}

/// A PDF as formats 6 to 8 saved it, before the room between its pages (see
/// `storage`).
#[derive(Deserialize)]
pub(crate) struct PdfBackgroundV8 {
    sizes: Vec<[f32; 2]>,
    pages: Vec<Option<u32>>,
}

impl From<PdfBackgroundV8> for PdfBackground {
    fn from(p: PdfBackgroundV8) -> Self {
        Self { sizes: p.sizes, pages: p.pages, gap: PDF_GAP, file: None }
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
/// How far a stroke drawn along the corner of an infinite page sticks out
/// left of or above it (half its width) without being written there.
const EDGE_OVERHANG: f32 = 10.0;
/// The room left of and above the pages of a PDF on an infinite page.
const CANVAS_PDF_INSET: f32 = 48.0;
/// The room between them (`PdfBackground::gap`) to start with, and the
/// room they can have.
pub const PDF_GAP: f32 = 48.0;
pub const PDF_GAPS: std::ops::RangeInclusive<f32> = 0.0..=400.0;
/// What is on an infinite page may stick out of the place of a PDF page by
/// this much and still go back onto that page (document units).
const SLOT_SLACK: f32 = 2.0;

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
    /// An infinite page cut into pages (`as_pages`, `as_sheets`): its PDF's
    /// pages stay where they were on it, one under the other (see
    /// `pdf_places`). Not saved.
    #[serde(skip)]
    cut: Option<Cut>,
}

/// How an infinite page was cut into pages: where the first one starts on
/// it (its left edge and its top, left of and above the page's corner if
/// it is written on there), and how tall each is (document units).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cut {
    left: f32,
    top: f32,
    height: f32,
}

/// A document as formats 7 and 8 saved it, before the room between the
/// pages of its PDF (see `storage`).
#[derive(Deserialize)]
pub(crate) struct DocumentV8 {
    page_width: f32,
    page_height: f32,
    page_count: u32,
    page: PageStyle,
    strokes: Vec<Stroke>,
    texts: Vec<TextBox>,
    next_id: u64,
    pdf: Option<PdfBackgroundV8>,
    images: Vec<Image>,
}

impl From<DocumentV8> for Document {
    fn from(d: DocumentV8) -> Self {
        let DocumentV8 { page_width, page_height, page_count, page, strokes, texts, next_id, pdf, images } = d;
        let pdf = pdf.map(PdfBackground::from);
        Self { page_width, page_height, page_count, page, strokes, texts, next_id, pdf, images, cut: None }
    }
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
    pdf: Option<PdfBackgroundV8>,
}

impl From<DocumentV6> for Document {
    fn from(d: DocumentV6) -> Self {
        let DocumentV6 { page_width, page_height, page_count, page, strokes, texts, next_id, pdf } = d;
        let images = Vec::new();
        DocumentV8 { page_width, page_height, page_count, page, strokes, texts, next_id, pdf, images }.into()
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

/// The place of a PDF page on an infinite page (`PdfBackground::canvas_slot`)
/// that `r` is on, if any (with `SLOT_SLACK`).
fn slot_of(pdf: &PdfBackground, r: Rect) -> Option<usize> {
    (0..pdf.pages.len().max(1)).find(|&i| pdf.canvas_slot(i).expand(SLOT_SLACK).contains_rect(r))
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
            cut: None,
        }
    }
}

impl Document {
    /// The note's page is infinite (see `CANVAS_SIZE`).
    pub fn is_canvas(&self) -> bool {
        self.page_width >= CANVAS_SIZE && self.page_height >= CANVAS_SIZE
    }

    /// Its PDF's pages are one under the other, as on an infinite page: it
    /// is one, or one cut into pages (`as_pages`, `as_sheets`).
    fn stacked(&self) -> bool {
        self.is_canvas() || self.cut.is_some()
    }

    /// The paper can be made infinite: a note of one page, or on a PDF (its
    /// pages go onto the infinite page, see `pdf_to_canvas`).
    pub fn can_be_canvas(&self) -> bool {
        self.page_count <= 1 || self.pdf.is_some()
    }

    /// The PDF pages on note page `page`, each with its place there (page
    /// space): on an infinite page all of them, one under the other, each
    /// fitted into its place (`PdfBackground::canvas_slot`); on any other
    /// page the one under it, fitted into the page.
    pub fn pdf_places(&self, page: u32) -> Vec<(u32, Rect)> {
        let Some(pdf) = &self.pdf else { return Vec::new() };
        let fit = |pdf_page: u32, slot: Rect| {
            let [x, y, w, h] = pdf.placement(pdf_page, slot.width(), slot.height())?;
            Some((pdf_page, Rect::from_min_size(slot.min + vec2(x, y), vec2(w, h))))
        };
        if self.stacked() {
            // The part of the infinite page this page is.
            let corner = match self.cut {
                Some(cut) => vec2(cut.left, cut.top + page as f32 * cut.height),
                None if page == 0 => Vec2::ZERO,
                None => return Vec::new(),
            };
            let shown = Rect::from_min_size(corner.to_pos2(), vec2(self.page_width, self.page_height));
            return (pdf.pages.iter().enumerate())
                .filter_map(|(i, p)| fit((*p)?, pdf.canvas_slot(i)))
                .filter(|(_, r)| r.intersects(shown))
                .map(|(p, r)| (p, r.translate(-corner)))
                .collect();
        }
        let whole = Rect::from_min_size(Pos2::ZERO, vec2(self.page_width, self.page_height));
        self.pdf_page(page).and_then(|p| fit(p, whole)).into_iter().collect()
    }

    /// Where PDF page `pdf_page` is in the note: the note page it is on, and
    /// its place there (the first, if it is there more than once).
    pub fn pdf_place_of(&self, pdf_page: u32) -> Option<(u32, Rect)> {
        let on = |page: u32| self.pdf_places(page).into_iter().find(|(p, _)| *p == pdf_page).map(|(_, r)| (page, r));
        if self.stacked() {
            return (0..self.page_count.max(1)).find_map(on);
        }
        on(self.pdf.as_ref()?.pages.iter().position(|p| *p == Some(pdf_page))? as u32)
    }

    /// What is on an infinite page, the places of its PDF's pages (and of
    /// the blank pages between them) included (page space); `None` when
    /// nothing is.
    pub fn canvas_used(&self) -> Option<Rect> {
        let slots = self.canvas_slots().into_iter().map(|(_, r)| r);
        self.content_bounds(0).into_iter().chain(slots).reduce(|a, b| a.union(b))
    }

    /// On an infinite page with a PDF, the places of its pages, top to
    /// bottom, each with its PDF page (`None`: a blank page); else none.
    pub fn canvas_slots(&self) -> Vec<(Option<u32>, Rect)> {
        let Some(pdf) = self.pdf.as_ref().filter(|_| self.is_canvas()) else { return Vec::new() };
        pdf.pages.iter().enumerate().map(|(i, p)| (*p, pdf.canvas_slot(i))).collect()
    }

    /// On an infinite page with a PDF: a page before `pages[index]` (past
    /// the last one, at the end), on PDF page `pdf_page` (`None`: blank).
    /// The pages below it move down a place, with what is on or beside them.
    pub fn insert_canvas_page(&mut self, index: usize, pdf_page: Option<u32>) {
        let Some(pdf) = self.pdf.as_ref().filter(|_| self.is_canvas()) else { return };
        let index = index.min(pdf.pages.len());
        let (old, stride) = (pdf.clone(), pdf.slot_size().y + pdf.gap);
        self.move_items(|page, r| {
            let below = old.canvas_band(r.center().y).is_some_and(|band| band >= index);
            (page, 1.0, vec2(0.0, if below { stride } else { 0.0 }))
        });
        self.pdf.as_mut().expect("checked above").pages.insert(index, pdf_page);
    }

    /// On an infinite page with a PDF: page `pages[index]` deleted with what
    /// is on it or beside it (its band, `canvas_band`), the pages below it
    /// moving up a place with what is on or beside them: the infinite page
    /// is that much shorter. The only page is only emptied. False if there
    /// is no such page.
    pub fn delete_canvas_page(&mut self, index: usize) -> bool {
        let Some(pdf) = self.pdf.as_ref().filter(|pdf| self.is_canvas() && index < pdf.pages.len()) else {
            return false;
        };
        let (old, stride) = (pdf.clone(), pdf.slot_size().y + pdf.gap);
        let band = |r: Rect| old.canvas_band(r.center().y);
        self.strokes.retain(|s| band(s.rect()) != Some(index));
        self.texts.retain(|t| band(t.rect()) != Some(index));
        self.images.retain(|i| band(i.rect()) != Some(index));
        if old.pages.len() > 1 {
            self.move_items(|page, r| {
                let below = band(r).is_some_and(|b| b > index);
                (page, 1.0, vec2(0.0, if below { -stride } else { 0.0 }))
            });
            self.pdf.as_mut().expect("checked above").pages.remove(index);
        }
        true
    }

    /// On an infinite page with a PDF: page `pages[index]` as a note page of
    /// its own, as large as its place, with what is on it or beside it (its
    /// band; beside it is outside the page): to copy it or make a picture
    /// of it.
    pub fn canvas_page(&self, index: usize) -> Option<Document> {
        let pdf = self.pdf.as_ref().filter(|pdf| self.is_canvas() && index < pdf.pages.len())?;
        let slot = pdf.canvas_slot(index);
        let on = |r: Rect| pdf.canvas_band(r.center().y) == Some(index);
        let up = -slot.min.to_vec2();
        let mut doc = Document {
            page_width: slot.width(),
            page_height: slot.height(),
            page_count: 1,
            strokes: Vec::new(),
            texts: Vec::new(),
            images: Vec::new(),
            pdf: Some(PdfBackground { pages: vec![pdf.pages[index]], ..pdf.clone() }),
            ..self.clone()
        };
        for s in self.strokes.iter().filter(|s| on(s.rect())) {
            let mut s = s.clone();
            s.translate(up.x, up.y);
            doc.strokes.push(s);
        }
        let texts = self.texts.iter().filter(|t| on(t.rect()));
        doc.texts.extend(texts.map(|t| TextBox { x: t.x + up.x, y: t.y + up.y, ..t.clone() }));
        let images = self.images.iter().filter(|i| on(i.rect()));
        doc.images.extend(images.map(|i| Image { x: i.x + up.x, y: i.y + up.y, ..i.clone() }));
        Some(doc)
    }

    /// On an infinite page with a PDF: a page pasted before `pages[index]`
    /// (`insert_canvas_page`), on PDF page `pdf_page`, with `strokes`,
    /// `texts` and `images` (page space of that page, as `canvas_page`
    /// makes them) on it, as new ones.
    pub fn paste_canvas_page(
        &mut self,
        index: usize,
        pdf_page: Option<u32>,
        (strokes, texts, images): (&[Stroke], &[TextBox], &[Image]),
    ) {
        let Some(len) = self.pdf.as_ref().filter(|_| self.is_canvas()).map(|pdf| pdf.pages.len()) else { return };
        let index = index.min(len);
        self.insert_canvas_page(index, pdf_page);
        let by = self.pdf.as_ref().expect("checked above").canvas_slot(index).min.to_vec2();
        for s in strokes {
            let mut s = s.clone();
            (s.id, s.page) = (self.alloc_id(), 0);
            s.translate(by.x, by.y);
            self.strokes.push(s);
        }
        for t in texts {
            let text = TextBox { id: self.alloc_id(), page: 0, x: t.x + by.x, y: t.y + by.y, ..t.clone() };
            self.texts.push(text);
        }
        for i in images {
            let image = Image { id: self.alloc_id(), page: 0, x: i.x + by.x, y: i.y + by.y, ..i.clone() };
            self.images.push(image);
        }
    }

    /// A note on a PDF made one infinite page: its PDF's pages go one under
    /// the other (`PdfBackground::canvas_slot`), each with what is on its
    /// note page, which keeps its place on the PDF. A note page of another
    /// size than those places (its paper was changed) is scaled into its
    /// place as its PDF page is. Pages without a PDF page keep their place
    /// too, empty.
    pub fn pdf_to_canvas(&mut self) {
        let Some(pdf) = &self.pdf else { return };
        if self.is_canvas() {
            return;
        }
        let page = Rect::from_min_size(Pos2::ZERO, vec2(self.page_width, self.page_height));
        // For each note page: what its points are scaled by, and moved by.
        let moves: Vec<(f32, Vec2)> = (0..self.page_count as usize)
            .map(|i| {
                let slot = pdf.canvas_slot(i);
                let fitted = |r: Rect, pdf_page: u32| {
                    let [x, y, w, h] = pdf.placement(pdf_page, r.width(), r.height())?;
                    Some(Rect::from_min_size(r.min + vec2(x, y), vec2(w, h)))
                };
                let on_pdf = pdf.pages.get(i).copied().flatten().and_then(|p| Some((fitted(page, p)?, fitted(slot, p)?)));
                // Without a PDF page: the page fitted into its place.
                let (from, to) = on_pdf.unwrap_or_else(|| {
                    let k = (slot.width() / page.width()).min(slot.height() / page.height());
                    (page, Rect::from_center_size(slot.center(), page.size() * k))
                });
                let k = to.width() / from.width().max(f32::EPSILON);
                (k, to.min.to_vec2() - from.min.to_vec2() * k)
            })
            .collect();
        let at = |page: u32| moves.get(page as usize).copied().unwrap_or((1.0, Vec2::ZERO));
        self.move_items(|page, _| {
            let (k, by) = at(page);
            (0, k, by)
        });
        self.pdf.as_mut().expect("checked above").pages.resize(self.page_count as usize, None);
        (self.page_width, self.page_height, self.page_count) = (CANVAS_SIZE, CANVAS_SIZE, 1);
    }

    /// The infinite page of a note on a PDF can be made pages again: all
    /// that is on it is on the place of one of its PDF's pages.
    pub fn can_leave_canvas(&self) -> bool {
        let Some(pdf) = self.pdf.as_ref().filter(|_| self.is_canvas()) else { return false };
        let on_a_slot = |r: Rect| slot_of(pdf, r).is_some();
        self.strokes.iter().all(|s| on_a_slot(s.rect()))
            && self.texts.iter().all(|t| on_a_slot(t.rect()))
            && self.images.iter().all(|i| on_a_slot(i.rect()))
    }

    /// The infinite page of a note on a PDF made pages again, one per place
    /// of a PDF page and as large, each with what is on that place: the
    /// reverse of `pdf_to_canvas`. False, and nothing changes, when
    /// something is outside those places (see `can_leave_canvas`).
    pub fn canvas_to_pdf_pages(&mut self) -> bool {
        if !self.can_leave_canvas() {
            return false;
        }
        let pdf = self.pdf.as_mut().expect("can_leave_canvas checked it");
        if pdf.pages.is_empty() {
            pdf.pages.push(None);
        }
        let pdf = pdf.clone();
        self.move_items(|_, r| {
            let slot = slot_of(&pdf, r).expect("can_leave_canvas checked it");
            (slot as u32, 1.0, -pdf.canvas_slot(slot).min.to_vec2())
        });
        let size = pdf.slot_size();
        (self.page_width, self.page_height, self.page_count) = (size.x, size.y, pdf.pages.len() as u32);
        true
    }

    /// On an infinite page with a PDF: the room between the PDF's pages is
    /// `gap` (clamped to `PDF_GAPS`). What is on a page, or beside it, moves
    /// with it; what is above the first page stays.
    pub fn set_pdf_gap(&mut self, gap: f32) {
        let Some(pdf) = self.pdf.as_ref().filter(|_| self.is_canvas()) else { return };
        let gap = gap.clamp(*PDF_GAPS.start(), *PDF_GAPS.end());
        let old = pdf.clone();
        let new = PdfBackground { gap, ..old.clone() };
        self.move_items(|page, r| {
            let by = old.canvas_row(r.center().y).map_or(0.0, |i| new.canvas_slot(i).top() - old.canvas_slot(i).top());
            (page, 1.0, vec2(0.0, by))
        });
        self.pdf.as_mut().expect("checked above").gap = gap;
    }

    /// Moves every stroke, text box and picture: `f` takes its page and its
    /// place there, and gives its new page, the scale of its points (around
    /// the page's corner) and how far they move after it.
    fn move_items(&mut self, mut f: impl FnMut(u32, Rect) -> (u32, f32, Vec2)) {
        let at = |p: Pos2, k: f32, by: Vec2| pos2(p.x * k + by.x, p.y * k + by.y);
        for s in &mut self.strokes {
            let (page, k, by) = f(s.page, s.rect());
            if k == 1.0 {
                s.translate(by.x, by.y);
            } else {
                *s = s.mapped(|p| at(p, k, by), k);
            }
            s.page = page;
        }
        for t in &mut self.texts {
            let (page, k, by) = f(t.page, t.rect());
            let corner = at(pos2(t.x, t.y), k, by);
            (t.page, t.x, t.y) = (page, corner.x, corner.y);
            if k != 1.0 {
                (t.width, t.size, t.line_h) = (t.width * k, t.size * k, t.line_h * k);
                // Laid out again at the new size (see `TextBox`).
                t.layout = None;
            }
        }
        for i in &mut self.images {
            let (page, k, by) = f(i.page, i.rect());
            let corner = at(pos2(i.x, i.y), k, by);
            (i.page, i.x, i.y, i.width, i.height) = (page, corner.x, corner.y, i.width * k, i.height * k);
        }
    }

    /// What is on page `page`, as a rectangle in page space (strokes with
    /// their width, text boxes, pictures); `None` when nothing is.
    pub fn content_bounds(&self, page: u32) -> Option<Rect> {
        let strokes = self.strokes.iter().filter(|s| s.page == page).map(|s| s.rect().expand(s.width * 0.5));
        let texts = self.texts.iter().filter(|t| t.page == page).map(TextBox::rect);
        let images = self.images.iter().filter(|i| i.page == page).map(Image::rect);
        strokes.chain(texts).chain(images).reduce(|a, b| a.union(b))
    }

    /// The part of an infinite page that makes a page of it (exporting,
    /// printing, the library's preview): from its corner (or from what is
    /// left of or above it) to what is on it (its PDF's pages too), with
    /// some paper around, at least a page of the default size.
    pub fn canvas_area(&self) -> Rect {
        let margin = vec2(CANVAS_MARGIN, CANVAS_MARGIN);
        let used = self.canvas_used();
        // Paper left of or above the corner only if something is there (not
        // just half the width of a stroke drawn along the edge).
        let before = |v: f32| if v < -EDGE_OVERHANG { v - CANVAS_MARGIN } else { 0.0 };
        let min = used.map_or(Pos2::ZERO, |c| pos2(before(c.min.x), before(c.min.y))).max(pos2(-CANVAS_SIZE, -CANVAS_SIZE));
        let max = used.map_or(Pos2::ZERO, |c| c.max + margin);
        let max = max.max(pos2(DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT)).min(pos2(CANVAS_SIZE, CANVAS_SIZE));
        Rect::from_min_max(min, max)
    }

    /// An infinite page as a page as large as what is on it (`canvas_area`),
    /// for what needs a page of a sensible size; any other note as it is.
    pub fn as_pages(&self) -> std::borrow::Cow<'_, Document> {
        if !self.is_canvas() {
            return std::borrow::Cow::Borrowed(self);
        }
        let area = self.canvas_area();
        let cut = Cut { left: area.left(), top: area.top(), height: area.height() };
        std::borrow::Cow::Owned(self.cut_into(area.width(), cut, 1))
    }

    /// An infinite page cut into sheets to export or print, as OneNote
    /// prints a page (see `sheets`); any other note as it is.
    pub fn as_sheets(&self) -> std::borrow::Cow<'_, Document> {
        if !self.is_canvas() {
            return std::borrow::Cow::Borrowed(self);
        }
        let (width, cut, count) = self.sheets();
        std::borrow::Cow::Owned(self.cut_into(width, cut, count))
    }

    /// The pages `as_sheets` makes.
    pub fn sheet_count(&self) -> u32 {
        if self.is_canvas() { self.sheets().2 } else { self.page_count.max(1) }
    }

    /// The sheet of `as_sheets` the height `y` of an infinite page is on.
    pub fn sheet_at(&self, y: f32) -> u32 {
        let (_, cut, count) = self.sheets();
        (((y - cut.top) / cut.height).floor().max(0.0) as u32).min(count - 1)
    }

    /// How an infinite page is cut into sheets: as wide as what is on it
    /// (`canvas_area`), one under the other down to its end. With a PDF on
    /// it, each as tall as a PDF page and the room after it, cut halfway
    /// between two (a PDF page a sheet, with what is beside it); else of
    /// the shape of the default paper (A4).
    fn sheets(&self) -> (f32, Cut, u32) {
        let area = self.canvas_area();
        let width = area.width();
        let count = |top: f32, height: f32, bottom: f32| ((bottom - top) / height - 1e-3).ceil().max(1.0) as u32;
        match self.pdf.as_ref().filter(|pdf| !pdf.pages.is_empty()) {
            Some(pdf) => {
                let height = pdf.slot_size().y + pdf.gap.max(1.0);
                let used = self.canvas_used().unwrap_or(area);
                // Halfway up the room above the first one; higher if
                // something is written there.
                let top = (CANVAS_PDF_INSET - pdf.gap * 0.5).max(0.0).min(used.top());
                (width, Cut { left: area.left(), top, height }, count(top, height, used.bottom()))
            }
            None => {
                let height = width * DEFAULT_PAGE_HEIGHT / DEFAULT_PAGE_WIDTH;
                let top = area.top();
                (width, Cut { left: area.left(), top, height }, count(top, height, area.bottom()))
            }
        }
    }

    /// An infinite page cut into `count` pages `width` wide (see `Cut`).
    /// What crosses from one to the next is on both, cut off at their
    /// edges (the copies keep its id: such a document is only drawn).
    fn cut_into(&self, width: f32, cut: Cut, count: u32) -> Document {
        let last = count.max(1) - 1;
        let row = |y: f32| (((y - cut.top) / cut.height).floor().max(0.0) as u32).min(last);
        let rows = |r: Rect| row(r.top())..=row(r.bottom());
        // From the infinite page to page `page` of the cut.
        let by = |page: u32| -vec2(cut.left, cut.top + page as f32 * cut.height);
        let mut doc = Document { strokes: Vec::new(), texts: Vec::new(), images: Vec::new(), ..self.clone() };
        for s in &self.strokes {
            for page in rows(s.rect().expand(s.width * 0.5)) {
                let mut s = s.clone();
                s.translate(by(page).x, by(page).y);
                s.page = page;
                doc.strokes.push(s);
            }
        }
        for t in &self.texts {
            for page in rows(t.rect()) {
                doc.texts.push(TextBox { page, x: t.x + by(page).x, y: t.y + by(page).y, ..t.clone() });
            }
        }
        for i in &self.images {
            for page in rows(i.rect()) {
                doc.images.push(Image { page, x: i.x + by(page).x, y: i.y + by(page).y, ..i.clone() });
            }
        }
        (doc.page_width, doc.page_height, doc.page_count) = (width, cut.height, last + 1);
        doc.cut = Some(cut);
        doc
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
            pdf: Some(PdfBackground { sizes, pages: (0..count).map(Some).collect(), gap: PDF_GAP, file: Some(file) }),
            ..Self::default()
        }
    }

    /// Note page `page` gets the rules (or grid) of the paper: not a page
    /// written on a PDF page, which is its own paper. An infinite page does,
    /// under the PDF's pages on it (they cover them, see `pdf_places`).
    pub fn ruled(&self, page: u32) -> bool {
        self.page.kind != PageKind::Blank && self.pdf_page(page).is_none()
    }

    /// The PDF page under note page `page`, if any; on an infinite page
    /// there are several (see `pdf_places`), none is "under" it.
    pub fn pdf_page(&self, page: u32) -> Option<u32> {
        if self.stacked() {
            return None;
        }
        self.pdf.as_ref()?.pages.get(page as usize).copied().flatten()
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

    /// A note on a PDF of three pages (the last one landscape), with a
    /// stroke on its second page and a picture on its third.
    fn note_on_pdf() -> Document {
        let mut doc = Document::on_pdf(PdfFile::new(Vec::new()), vec![[600.0, 800.0], [600.0, 800.0], [800.0, 600.0]]);
        let mut s = line(2.0);
        s.translate(100.0, 200.0);
        (s.id, s.page) = (doc.alloc_id(), 1);
        doc.strokes.push(s);
        let data = Arc::new(ImageData::new(1, 1, Vec::new()));
        let id = doc.alloc_id();
        doc.images.push(Image { id, page: 2, x: 50.0, y: 60.0, width: 40.0, height: 30.0, data });
        doc
    }

    #[test]
    fn a_note_on_a_pdf_becomes_an_infinite_page_and_back() {
        let pages = note_on_pdf();
        let mut doc = pages.clone();
        assert!(doc.can_be_canvas());
        doc.pdf_to_canvas();
        assert!(doc.is_canvas() && doc.page_count == 1);
        // The PDF's pages one under the other, each in its place.
        let places = doc.pdf_places(0);
        assert_eq!(places.iter().map(|(p, _)| *p).collect::<Vec<_>>(), [0, 1, 2]);
        let pdf = doc.pdf.as_ref().unwrap();
        for (i, (_, r)) in places.iter().enumerate() {
            assert!(pdf.canvas_slot(i).expand(0.01).contains_rect(*r));
        }
        assert!(places[0].1.bottom() < places[1].1.top(), "one under the other");
        // What was on a page is where that page went: nothing moves on the PDF.
        let slot = pdf.canvas_slot(1).min.to_vec2();
        assert_eq!(doc.strokes[0].page, 0);
        assert_eq!(doc.strokes[0].points[0].pos2(), pos2(100.0, 200.0) + slot);
        assert_eq!(doc.images[0].rect().min, pos2(50.0, 60.0) + pdf.canvas_slot(2).min.to_vec2());
        assert_eq!(doc.pdf_place_of(1).map(|(page, r)| (page, r.min)), Some((0, pos2(0.0, 0.0) + slot)));
        // The infinite page goes as far as its PDF.
        assert!(doc.canvas_used().unwrap().contains_rect(places[2].1));

        assert!(doc.can_leave_canvas());
        assert!(doc.canvas_to_pdf_pages());
        assert_eq!((doc.page_width, doc.page_height, doc.page_count), (pages.page_width, pages.page_height, 3));
        assert_eq!(doc.strokes[0].page, 1);
        assert_eq!(doc.strokes[0].points[0].pos2(), pos2(100.0, 200.0));
        assert_eq!((doc.images[0].page, doc.images[0].rect()), (2, pages.images[0].rect()));
        assert_eq!(doc.pdf_page(2), Some(2));
    }

    #[test]
    fn ink_beside_the_pdf_has_no_page_to_go_back_to() {
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        let mut s = line(2.0);
        s.translate(5000.0, 100.0);
        doc.strokes.push(s);
        assert!(!doc.can_leave_canvas());
        let before = (doc.page_count, doc.strokes[1].points[0]);
        assert!(!doc.canvas_to_pdf_pages());
        assert!(doc.is_canvas());
        assert_eq!((doc.page_count, doc.strokes[1].points[0]), before, "nothing changed");
    }

    #[test]
    fn a_page_of_another_size_is_scaled_into_its_place() {
        let mut doc = note_on_pdf();
        // Twice as large: the PDF page is twice as large on it, and so is the stroke.
        (doc.page_width, doc.page_height) = (doc.page_width * 2.0, doc.page_height * 2.0);
        doc.strokes[0].page = 0;
        let width = doc.strokes[0].width;
        doc.pdf_to_canvas();
        let (_, place) = doc.pdf_place_of(0).unwrap();
        let p = doc.strokes[0].points[0].pos2();
        assert!((p - (place.min + vec2(50.0, 100.0))).length() < 0.01, "{p:?} on {place:?}");
        assert!((doc.strokes[0].width - width * 0.5).abs() < 1e-4);
    }

    #[test]
    fn an_infinite_page_on_a_pdf_made_a_page_keeps_the_pdf_where_it_was() {
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        let page = doc.as_pages();
        assert!(!page.is_canvas());
        assert_eq!(page.pdf_places(0), doc.pdf_places(0));
        // As large as the PDF's pages on it.
        assert!(page.page_height >= doc.pdf_places(0)[2].1.bottom());
        assert!(!page.ruled(0) && page.pdf_page(0).is_none());
    }

    #[test]
    fn a_pdf_saved_before_the_room_between_its_pages_gets_the_usual_room() {
        let mut doc = note_on_pdf();
        // Format 8: the same without the room (an unmistakable value here).
        doc.pdf.as_mut().unwrap().gap = 1234.5;
        let mut bytes = bincode::serialize(&doc).unwrap();
        let at = bytes.windows(4).position(|w| w == 1234.5f32.to_le_bytes()).unwrap();
        bytes.drain(at..at + 4);
        let old: Document = bincode::deserialize::<DocumentV8>(&bytes).unwrap().into();
        assert_eq!(old.pdf.as_ref().unwrap().gap, PDF_GAP);
        assert_eq!(old.pdf.unwrap().pages, doc.pdf.unwrap().pages);
        assert_eq!(old.images.len(), 1);
    }

    #[test]
    fn more_room_between_the_pdf_pages_moves_what_is_on_them_and_beside_them() {
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        // Beside the third page, and above the first.
        let third = doc.pdf.as_ref().unwrap().canvas_slot(2);
        let mut beside = line(2.0);
        beside.translate(third.right() + 300.0, third.center().y);
        doc.strokes.push(beside);
        let mut above = line(2.0);
        above.translate(10.0, 5.0);
        doc.strokes.push(above);
        let (on, aside, top) = (doc.strokes[0].points[0], doc.strokes[1].points[0], doc.strokes[2].points[0]);
        doc.set_pdf_gap(PDF_GAP + 100.0);
        let pdf = doc.pdf.as_ref().unwrap();
        assert_eq!(pdf.gap, PDF_GAP + 100.0);
        assert_eq!(pdf.canvas_slot(2).top() - third.top(), 200.0);
        // On the second page: 100 lower; beside the third: 200; above: as it was.
        assert_eq!(doc.strokes[0].points[0].y - on.y, 100.0);
        assert_eq!(doc.strokes[1].points[0].y - aside.y, 200.0);
        assert_eq!(doc.strokes[2].points[0], top);
        // Still on the PDF's pages: they can be pages again.
        doc.strokes.truncate(1);
        assert!(doc.canvas_to_pdf_pages());
        assert_eq!(doc.strokes[0].points[0].pos2(), pos2(100.0, 200.0));
        // Not past what can be set.
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        doc.set_pdf_gap(-5.0);
        assert_eq!(doc.pdf.unwrap().gap, 0.0);
    }

    #[test]
    fn an_infinite_page_is_exported_on_sheets_of_the_paper_s_shape() {
        let mut doc = Document { page_width: CANVAS_SIZE, page_height: CANVAS_SIZE, ..Document::default() };
        // Down to 2.5 A4 pages, and a stroke across the first cut.
        let cut = DEFAULT_PAGE_HEIGHT;
        let mut across = line(2.0);
        across.points = vec![Point::new(10.0, cut - 20.0, 1.0), Point::new(10.0, cut + 20.0, 1.0)].into();
        across.bounds = compute_bounds(&across.points);
        doc.strokes.push(across);
        let mut low = line(2.0);
        low.translate(0.0, cut * 2.5);
        doc.strokes.push(low);
        assert_eq!(doc.sheet_count(), 3);
        let sheets = doc.as_sheets();
        assert!(!sheets.is_canvas());
        assert_eq!((sheets.page_width, sheets.page_height, sheets.page_count), (DEFAULT_PAGE_WIDTH, cut, 3));
        // On both sheets it crosses, each part where it is on that sheet.
        let parts: Vec<(u32, f32)> = sheets.strokes.iter().map(|s| (s.page, s.points[0].y)).collect();
        assert_eq!(parts[..2], [(0, cut - 20.0), (1, -20.0)]);
        assert_eq!(parts[2], (2, cut * 0.5));
        assert_eq!(doc.sheet_at(cut * 1.5), 1);
        assert_eq!(doc.sheet_at(-5.0), 0);
        assert_eq!(doc.sheet_at(1e9), 2);
        // A note with pages is exported as it is.
        assert!(matches!(Document::default().as_sheets(), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn an_infinite_page_on_a_pdf_is_exported_a_pdf_page_a_sheet() {
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        let pdf = doc.pdf.clone().unwrap();
        let sheets = doc.as_sheets();
        assert_eq!(sheets.page_count, 3);
        assert_eq!(sheets.page_height, pdf.slot_size().y + pdf.gap);
        for page in 0..3 {
            // Each sheet has its PDF page, whole, halfway down the room around it.
            let places = sheets.pdf_places(page);
            assert_eq!(places.len(), 1, "sheet {page}");
            let (p, r) = places[0];
            assert_eq!(p, page);
            assert!(r.top() >= 0.0 && r.bottom() <= sheets.page_height, "{r:?}");
            assert_eq!(sheets.pdf_place_of(page).map(|(sheet, _)| sheet), Some(page));
        }
        // The stroke of the second page is on the second sheet, where it is on its PDF page.
        let (_, second) = sheets.pdf_places(1)[0];
        let s = sheets.strokes.iter().find(|s| s.page == 1).unwrap();
        assert_eq!(s.points[0].pos2(), second.min + vec2(100.0, 200.0));
        assert_eq!(doc.sheet_at(pdf.canvas_slot(2).center().y), 2);
    }

    #[test]
    fn a_blank_page_between_the_pdf_pages_makes_room_for_itself() {
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        let pdf = doc.pdf.clone().unwrap();
        let stride = pdf.slot_size().y + pdf.gap;
        // The stroke is on the second PDF page, the picture on the third.
        let (stroke, picture) = (doc.strokes[0].points[0], doc.images[0].y);
        doc.insert_canvas_page(1, None);
        assert_eq!(doc.pdf.as_ref().unwrap().pages, [Some(0), None, Some(1), Some(2)]);
        assert_eq!(doc.strokes[0].points[0].y, stroke.y + stride, "moved down with its page");
        assert_eq!(doc.images[0].y, picture + stride);
        // The PDF's second page is a place further down, the stroke still on it.
        let (_, second) = doc.pdf_place_of(1).unwrap();
        assert_eq!(doc.strokes[0].points[0].pos2(), second.min + vec2(100.0, 200.0));
        assert!(doc.canvas_used().unwrap().contains_rect(doc.pdf.as_ref().unwrap().canvas_slot(3)));
        // Exported as a sheet of its own, and a page of its own back on pages.
        assert_eq!(doc.sheet_count(), 4);
        let mut pages = doc.clone();
        assert!(pages.canvas_to_pdf_pages());
        assert_eq!((pages.page_count, pages.pdf_page(1), pages.strokes[0].page), (4, None, 2));

        // Taken away again: all back where it was.
        assert!(doc.delete_canvas_page(1));
        assert_eq!(doc.pdf.as_ref().unwrap().pages, [Some(0), Some(1), Some(2)]);
        assert_eq!((doc.strokes[0].points[0], doc.images[0].y), (stroke, picture));
    }

    #[test]
    fn deleting_a_page_of_an_infinite_page_deletes_its_band_and_shortens_it() {
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        // At the end: nothing moves.
        let before = doc.strokes[0].points[0];
        doc.insert_canvas_page(99, None);
        assert_eq!(doc.pdf.as_ref().unwrap().pages, [Some(0), Some(1), Some(2), None]);
        assert_eq!(doc.strokes[0].points[0], before);
        // Beside the second PDF page, in its band: goes with it.
        let pdf = doc.pdf.clone().unwrap();
        let mut beside = line(2.0);
        beside.translate(pdf.canvas_slot(1).right() + 500.0, pdf.canvas_slot(1).center().y);
        doc.strokes.push(beside);
        let (picture, used) = (doc.images[0].y, doc.canvas_used().unwrap().bottom());
        assert!(doc.delete_canvas_page(1));
        assert_eq!(doc.pdf.as_ref().unwrap().pages, [Some(0), Some(2), None]);
        assert!(doc.strokes.is_empty(), "the stroke on it and the one beside it");
        let stride = pdf.slot_size().y + pdf.gap;
        assert_eq!(doc.images[0].y, picture - stride, "the third page's picture moved up");
        assert_eq!(doc.canvas_used().unwrap().bottom(), used - stride);
        assert!(!doc.delete_canvas_page(9));
        // The only page left is only emptied.
        for _ in 0..2 {
            assert!(doc.delete_canvas_page(0));
        }
        assert!(doc.delete_canvas_page(0));
        assert_eq!(doc.pdf.as_ref().unwrap().pages, [None]);
    }

    #[test]
    fn a_page_of_an_infinite_page_is_copied_and_pasted_with_what_is_on_it() {
        let mut doc = note_on_pdf();
        doc.pdf_to_canvas();
        let page = doc.canvas_page(1).unwrap();
        assert_eq!((page.page_count, page.page_width, page.page_height), (1, 600.0 * PT_TO_PX, 800.0 * PT_TO_PX));
        assert_eq!(page.pdf_page(0), Some(1));
        assert_eq!(page.strokes.len(), 1);
        assert_eq!(page.strokes[0].points[0].pos2(), pos2(100.0, 200.0), "where it is on its page");
        assert!(doc.canvas_page(3).is_none());
        // Pasted after the last page: a copy, with new ids, on its PDF page.
        let ids: Vec<u64> = doc.strokes.iter().map(|s| s.id).collect();
        doc.paste_canvas_page(3, Some(1), (&page.strokes, &page.texts, &page.images));
        assert_eq!(doc.pdf.as_ref().unwrap().pages, [Some(0), Some(1), Some(2), Some(1)]);
        let pasted = doc.strokes.last().unwrap();
        assert!(!ids.contains(&pasted.id) && pasted.page == 0);
        let (_, place) = doc.pdf_places(0)[3];
        assert_eq!(pasted.points[0].pos2(), place.min + vec2(100.0, 200.0));
    }

    #[test]
    fn what_is_left_of_and_above_an_infinite_page_s_corner_is_exported_too() {
        let mut doc = Document { page_width: CANVAS_SIZE, page_height: CANVAS_SIZE, ..Document::default() };
        let mut s = line(2.0);
        s.translate(-500.0, -300.0);
        s.id = doc.alloc_id();
        doc.strokes.push(s);
        let area = doc.canvas_area();
        // From before it (and some paper), to at least a default page.
        assert_eq!(area.min, pos2(-500.0 - 1.0 - CANVAS_MARGIN, -300.0 - 1.0 - CANVAS_MARGIN));
        assert_eq!(area.max, pos2(DEFAULT_PAGE_WIDTH, DEFAULT_PAGE_HEIGHT));
        // Cut from there: on the first sheet, as far in as it is from that corner.
        let sheets = doc.as_sheets();
        assert_eq!(sheets.page_width, area.width());
        let p = sheets.strokes[0].points[0].pos2();
        assert_eq!(p, pos2(-500.0, -300.0) - area.min.to_vec2());
        assert_eq!(doc.sheet_at(-300.0), 0);
        let page = doc.as_pages();
        assert_eq!(page.strokes[0].points[0].pos2(), p);
    }
}
