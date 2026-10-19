// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Serializable document model. Stroke coordinates are in *page space*:
//! document units relative to the top-left corner of the stroke's page.

use std::sync::Arc;

use emath::{pos2, vec2, Pos2, Rect};
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
pub const PAPER_RGB: [u8; 3] = [252, 251, 247];

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
    pub points: Vec<Point>,
    /// Cached `[min_x, min_y, max_x, max_y]` for culling and eraser rejection.
    pub bounds: [f32; 4],
    /// Bumped whenever the points change; invalidates cached meshes. Not saved.
    #[serde(skip)]
    pub rev: u32,
}

impl Stroke {
    pub fn new(
        id: u64,
        page: u32,
        kind: StrokeKind,
        color: [u8; 4],
        width: f32,
        points: Vec<Point>,
    ) -> Self {
        let bounds = compute_bounds(&points);
        Self { id, page, kind, color, width, points, bounds, rev: 0 }
    }

    /// Moves the stroke (page space).
    pub fn translate(&mut self, dx: f32, dy: f32) {
        for p in &mut self.points {
            p.x += dx;
            p.y += dy;
        }
        self.bounds[0] += dx;
        self.bounds[2] += dx;
        self.bounds[1] += dy;
        self.bounds[3] += dy;
        self.rev = self.rev.wrapping_add(1);
    }

    /// Stroke-based eraser test: does a circle at `center` with `radius`
    /// touch any segment of this stroke?
    pub fn hit_test(&self, center: Pos2, radius: f32) -> bool {
        let reach = radius + self.width * 0.5;
        let [min_x, min_y, max_x, max_y] = self.bounds;
        if center.x < min_x - reach
            || center.x > max_x + reach
            || center.y < min_y - reach
            || center.y > max_y + reach
        {
            return false; // cheap bounding-box rejection
        }
        match self.points.as_slice() {
            [] => false,
            [p] => p.pos2().distance(center) <= reach,
            pts => pts
                .windows(2)
                .any(|w| point_segment_distance(center, w[0].pos2(), w[1].pos2()) <= reach),
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
    if points.is_empty() {
        [0.0; 4]
    } else {
        b
    }
}

/// A keyboard-typed text box. `x`/`y` is its top-left corner in page space.
///
/// `lines` is the text already wrapped to `width` (computed with egui's font
/// when the box is committed), so the screen, the thumbnails and the PDF all
/// show identical line breaks and no font metrics are needed to draw it.
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
    /// The source text (may contain explicit newlines).
    pub text: String,
    /// `text` wrapped to `width`.
    pub lines: Vec<String>,
    /// Height of one line in document units.
    pub line_h: f32,
}

impl TextBox {
    pub fn height(&self) -> f32 {
        self.lines.len().max(1) as f32 * self.line_h
    }

    /// Bounding box in page space.
    pub fn rect(&self) -> Rect {
        Rect::from_min_size(pos2(self.x, self.y), vec2(self.width, self.height()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageKind {
    Blank,
    Ruled,
    Grid,
}

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
        }
    }
}

impl Document {
    /// Builds a document from the fields of the pre-text-box file format (v2).
    pub fn migrated_from_v2(
        page_width: f32,
        page_height: f32,
        page_count: u32,
        page: PageStyle,
        strokes: Vec<Stroke>,
        next_id: u64,
    ) -> Self {
        Self { page_width, page_height, page_count, page, strokes, texts: Vec::new(), next_id, pdf: None }
    }

    /// Builds a document from the fields of the v3 / v4 file formats (no PDF).
    pub fn migrated_from_v4(
        page_width: f32,
        page_height: f32,
        page_count: u32,
        page: PageStyle,
        strokes: Vec<Stroke>,
        texts: Vec<TextBox>,
        next_id: u64,
    ) -> Self {
        Self { page_width, page_height, page_count, page, strokes, texts, next_id, pdf: None }
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
    fn highlight_mixes_with_the_paper() {
        assert_eq!(highlight_on([255, 0, 0, 255], [255, 255, 255]), [255, 0, 0]);
        assert_eq!(highlight_on([255, 0, 0, 0], [10, 20, 30]), [10, 20, 30]);
        assert_eq!(highlight_on([0, 0, 0, 128], [255, 255, 255]), [127, 127, 127]);
    }
}
