// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vector PDF export.
//!
//! A small hand-written PDF 1.5 writer (no external PDF crate, so there is no
//! API surface that can break): one PDF page per note page, real page size,
//! strokes as vector paths, Flate-compressed content streams.
//!
//! * Page space is y-down in 1/96 inch; a single `cm` matrix maps it to PDF
//!   points (x 0.75, y flipped), so stroke coordinates are written unchanged.
//! * Pen strokes are the same smoothed curve that is drawn on screen. Constant
//!   pressure -> one stroked path; variable pressure -> runs of segments with
//!   the same (quantised) width, joined by round caps.
//! * Highlighter strokes are a single stroked path with an `ExtGState` alpha,
//!   so a stroke crossing itself is *not* darkened twice (unlike on screen).

use std::fs;
use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::path::Path;

use flate2::{write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};

use crate::model::{Document, PageKind, Stroke, StrokeKind, TextBox};
use crate::stroke_path::{self, Sample};

/// 1 px (1/96 in) in PDF points (1/72 in).
const PX_TO_PT: f32 = 0.75;
/// Spacing of curve samples in page units.
const SAMPLE_STEP: f32 = 1.5;

/// Colours of the ink in the PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InkColors {
    /// As drawn.
    Original,
    /// Pen and text in black; highlighter in grey (so it stays see-through).
    Black,
    /// Everything converted to grey (for printing in black and white).
    Grayscale,
}

pub struct PdfOptions {
    /// Draw the ruled / grid lines of the page style.
    pub include_rules: bool,
    /// Pages to export (0-based, inclusive); `None` = all.
    pub pages: Option<RangeInclusive<u32>>,
    pub include_highlighter: bool,
    pub include_text: bool,
    /// Fill the note's page with the paper colour (otherwise white).
    pub paper_background: bool,
    pub ink: InkColors,
    /// Output page size in page units (px); the note's page is scaled to fit
    /// inside `margin` and centred. `None` = the note's own size.
    pub page_size: Option<(f32, f32)>,
    /// Margin around the scaled page, in page units (with `page_size` only).
    pub margin: f32,
    /// Document metadata.
    pub title: String,
    pub author: String,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            include_rules: true,
            pages: None,
            include_highlighter: true,
            include_text: true,
            paper_background: false,
            ink: InkColors::Original,
            page_size: None,
            margin: 0.0,
            title: "InkNote".to_owned(),
            author: String::new(),
        }
    }
}

impl PdfOptions {
    /// The note pages that end up in the PDF, in order.
    pub fn page_list(&self, doc: &Document) -> Vec<u32> {
        let count = doc.page_count.max(1);
        match &self.pages {
            None => (0..count).collect(),
            Some(r) => {
                let (a, b) = (*r.start().min(r.end()), *r.start().max(r.end()));
                (a.min(count - 1)..=b.min(count - 1)).collect()
            }
        }
    }

    /// Output page size, scale and offset of the note's page on it (page units).
    fn placement(&self, doc: &Document) -> ((f32, f32), f32, (f32, f32)) {
        let (pw, ph) = (doc.page_width, doc.page_height);
        match self.page_size {
            None => ((pw, ph), 1.0, (0.0, 0.0)),
            Some((w, h)) => {
                let m = self.margin.clamp(0.0, w.min(h) * 0.4);
                let s = ((w - 2.0 * m) / pw).min((h - 2.0 * m) / ph).max(0.01);
                ((w, h), s, ((w - pw * s) * 0.5, (h - ph * s) * 0.5))
            }
        }
    }

    /// Ink colour (0..1) for a stroke or text of colour `rgb`.
    fn ink_rgb(&self, rgb: [u8; 3], highlighter: bool) -> (f32, f32, f32) {
        let f = |v: u8| v as f32 / 255.0;
        let gray = || {
            let l = 0.299 * f(rgb[0]) + 0.587 * f(rgb[1]) + 0.114 * f(rgb[2]);
            (l, l, l)
        };
        match (self.ink, highlighter) {
            (InkColors::Original, _) => (f(rgb[0]), f(rgb[1]), f(rgb[2])),
            (InkColors::Black, false) => (0.0, 0.0, 0.0),
            (InkColors::Black, true) | (InkColors::Grayscale, _) => gray(),
        }
    }
}

/// Writes `doc` to `path` (via a temp file, so a failure never truncates an
/// existing export).
pub fn export_pdf(doc: &Document, path: &Path, opts: &PdfOptions) -> io::Result<()> {
    let bytes = build_pdf(doc, opts)?;
    let tmp = path.with_extension("pdf.tmp");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, path)
}

// ------------------------------------------------------------ file layout

fn build_pdf(doc: &Document, opts: &PdfOptions) -> io::Result<Vec<u8>> {
    let page_list = opts.page_list(doc);
    let pages = page_list.len();

    // One ExtGState per distinct highlighter alpha.
    let mut alphas: Vec<u8> = doc
        .strokes
        .iter()
        .filter(|s| s.kind == StrokeKind::Highlighter && opts.include_highlighter)
        .map(|s| s.color[3])
        .collect();
    alphas.sort_unstable();
    alphas.dedup();

    // Object numbers: 1 catalog, 2 page tree, 3 resources, 4 info,
    // 5.. ExtGStates, the font, then (page, content) pairs.
    let font_obj = 5 + alphas.len();
    let first_page_obj = font_obj + 1;

    let mut pdf = Pdf::new();
    pdf.raw(b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n");

    pdf.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");

    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", first_page_obj + 2 * i)).collect();
    pdf.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));

    let gs_entries: String = alphas
        .iter()
        .enumerate()
        .map(|(i, a)| format!("/GS{a} {} 0 R ", 5 + i))
        .collect();
    pdf.obj(
        3,
        &format!("<< /ExtGState << {gs_entries}>> /Font << /F1 {font_obj} 0 R >> >>"),
    );

    let author = if opts.author.trim().is_empty() {
        String::new()
    } else {
        format!("/Author ({}) ", pdf_string(opts.author.trim()))
    };
    pdf.obj(4, &format!("<< /Title ({}) {author}/Producer (InkNote) >>", pdf_string(&opts.title)));

    for (i, a) in alphas.iter().enumerate() {
        let v = *a as f32 / 255.0;
        pdf.obj(5 + i, &format!("<< /Type /ExtGState /CA {v:.3} /ca {v:.3} >>"));
    }

    // Text boxes use the standard Helvetica (no font embedding needed).
    pdf.obj(
        font_obj,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );

    let ((out_w, out_h), _, _) = opts.placement(doc);
    let (pw, ph) = (out_w * PX_TO_PT, out_h * PX_TO_PT);
    for (p, &page) in page_list.iter().enumerate() {
        let content = page_content(doc, page, opts);
        let packed = deflate(content.as_bytes())?;
        let page_obj = first_page_obj + 2 * p;
        pdf.obj(
            page_obj,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources 3 0 R /Contents {} 0 R >>",
                num(pw, 2),
                num(ph, 2),
                page_obj + 1
            ),
        );
        pdf.stream_obj(page_obj + 1, "/Filter /FlateDecode", &packed);
    }

    Ok(pdf.finish(1, 4))
}

fn deflate(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)?;
    enc.finish()
}

/// Minimal PDF object writer that records byte offsets for the xref table.
struct Pdf {
    buf: Vec<u8>,
    /// `offsets[n - 1]` = byte offset of object `n`; objects are written in order.
    offsets: Vec<usize>,
}

impl Pdf {
    fn new() -> Self {
        Self { buf: Vec::new(), offsets: Vec::new() }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn begin(&mut self, num: usize) {
        debug_assert_eq!(num, self.offsets.len() + 1, "objects must be written in order");
        self.offsets.push(self.buf.len());
        self.buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
    }

    fn obj(&mut self, num: usize, body: &str) {
        self.begin(num);
        self.buf.extend_from_slice(body.as_bytes());
        self.buf.extend_from_slice(b"\nendobj\n");
    }

    fn stream_obj(&mut self, num: usize, dict_extra: &str, data: &[u8]) {
        self.begin(num);
        self.buf
            .extend_from_slice(format!("<< /Length {} {dict_extra} >>\nstream\n", data.len()).as_bytes());
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(b"\nendstream\nendobj\n");
    }

    fn finish(mut self, root: usize, info: usize) -> Vec<u8> {
        let xref_pos = self.buf.len();
        let count = self.offsets.len() + 1;
        let mut table = format!("xref\n0 {count}\n0000000000 65535 f \n");
        for off in &self.offsets {
            table.push_str(&format!("{off:010} 00000 n \n")); // exactly 20 bytes per entry
        }
        table.push_str(&format!(
            "trailer\n<< /Size {count} /Root {root} 0 R /Info {info} 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n"
        ));
        self.buf.extend_from_slice(table.as_bytes());
        self.buf
    }
}

// ------------------------------------------------------- page content

fn page_content(doc: &Document, page: u32, opts: &PdfOptions) -> String {
    let mut c = String::with_capacity(32 * 1024);
    // Note page space (y down, px) -> PDF space (y up, pt), scaled and
    // centred on the output page.
    let ((_, out_h), s, (ox, oy)) = opts.placement(doc);
    let a = PX_TO_PT * s;
    c.push_str(&format!(
        "q\n{} 0 0 -{} {} {} cm\n",
        num(a, 5),
        num(a, 5),
        num(ox * PX_TO_PT, 3),
        num((out_h - oy) * PX_TO_PT, 3),
    ));

    if opts.paper_background {
        // The paper colour of the screen (252, 251, 247).
        c.push_str(&format!(
            "0.988 0.984 0.969 rg\n0 0 {} {} re f\n",
            num(doc.page_width, 2),
            num(doc.page_height, 2)
        ));
    }
    if opts.include_rules && doc.page.kind != PageKind::Blank {
        write_rules(&mut c, doc);
    }
    if opts.include_text {
        for t in doc.texts.iter().filter(|t| t.page == page) {
            write_text(&mut c, t, opts);
        }
    }
    for s in doc.strokes.iter().filter(|s| s.page == page) {
        if s.kind == StrokeKind::Highlighter && !opts.include_highlighter {
            continue;
        }
        write_stroke(&mut c, s, opts);
    }
    c.push_str("Q\n");
    c
}

fn write_rules(c: &mut String, doc: &Document) {
    let spacing = doc.page.spacing.max(8.0);
    // Same light blue as on screen (200, 214, 230).
    c.push_str("0.784 0.839 0.902 RG\n1 w\n0 J\n");
    let mut y = spacing;
    while y < doc.page_height {
        c.push_str(&format!("0 {y} m {w} {y} l S\n", y = num(y, 2), w = num(doc.page_width, 2)));
        y += spacing;
    }
    if doc.page.kind == PageKind::Grid {
        let mut x = spacing;
        while x < doc.page_width {
            c.push_str(&format!("{x} 0 m {x} {h} l S\n", x = num(x, 2), h = num(doc.page_height, 2)));
            x += spacing;
        }
    }
}

/// Text box: one `Tj` per stored line, so the PDF breaks lines exactly like the
/// screen. Only characters of the WinAnsi (Latin-1 like) set can be printed;
/// anything else becomes `?`.
fn write_text(c: &mut String, t: &TextBox, opts: &PdfOptions) {
    if t.lines.iter().all(|l| l.trim().is_empty()) {
        return;
    }
    let (r, g, b) = opts.ink_rgb(t.color, false);
    c.push_str(&format!(
        "q\n{} {} {} rg\nBT\n/F1 {} Tf\n",
        num(r, 3),
        num(g, 3),
        num(b, 3),
        num(t.size, 2)
    ));
    for (i, line) in t.lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let baseline = t.y + i as f32 * t.line_h + t.line_h * 0.78;
        // The page matrix is y-down; this text matrix flips it back so glyphs are upright.
        c.push_str(&format!(
            "1 0 0 -1 {} {} Tm\n({}) Tj\n",
            num(t.x, 2),
            num(baseline, 2),
            pdf_string(line)
        ));
    }
    c.push_str("ET\nQ\n");
}

fn winansi(ch: char) -> u8 {
    match ch {
        '€' => 0x80,
        '…' => 0x85,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        c if (c as u32) < 0x20 => b' ',
        c if (0x80..=0x9F).contains(&(c as u32)) => b'?',
        c if (c as u32) <= 0xFF => c as u32 as u8,
        _ => b'?',
    }
}

/// Escapes a line as a PDF literal string (non-ASCII bytes as octal).
fn pdf_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let b = winansi(ch);
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out
}

fn write_stroke(c: &mut String, s: &Stroke, opts: &PdfOptions) {
    let samples = stroke_path::centerline(s.kind, s.width, &s.points, SAMPLE_STEP);
    if samples.is_empty() {
        return;
    }
    let rgb = [s.color[0], s.color[1], s.color[2]];
    let (r, g, b) = opts.ink_rgb(rgb, s.kind == StrokeKind::Highlighter);
    c.push_str(&format!(
        "q\n{r} {g} {b} RG {r} {g} {b} rg\n1 J 1 j\n",
        r = num(r, 3),
        g = num(g, 3),
        b = num(b, 3)
    ));

    if s.kind == StrokeKind::Highlighter {
        c.push_str(&format!("/GS{} gs\n", s.color[3]));
    }

    if samples.len() == 1 {
        dot(c, samples[0]);
    } else if s.kind == StrokeKind::Highlighter || is_uniform(&samples) {
        // One path, constant width.
        let mean_r = samples.iter().map(|p| p.r).sum::<f32>() / samples.len() as f32;
        c.push_str(&format!("{} w\n", num((mean_r * 2.0).max(0.3), 2)));
        polyline(c, &samples);
        c.push_str("S\n");
    } else {
        variable_width(c, &samples);
    }
    c.push_str("Q\n");
}

fn is_uniform(samples: &[Sample]) -> bool {
    let r0 = samples[0].r;
    samples.iter().all(|p| (p.r - r0).abs() <= 0.03 * r0.max(0.5))
}

fn polyline(c: &mut String, samples: &[Sample]) {
    c.push_str(&format!("{} {} m\n", num(samples[0].x, 2), num(samples[0].y, 2)));
    for p in &samples[1..] {
        c.push_str(&format!("{} {} l\n", num(p.x, 2), num(p.y, 2)));
    }
}

/// Pressure-sensitive stroke: consecutive segments of (quantised) equal width
/// form one stroked path; round caps make the joins between runs seamless.
fn variable_width(c: &mut String, samples: &[Sample]) {
    let mut current = f32::NAN;
    let mut open = false;
    for i in 0..samples.len() - 1 {
        let (a, b) = (samples[i], samples[i + 1]);
        let w = ((a.r + b.r).max(0.3) * 4.0).round() / 4.0; // diameter, 0.25 px steps
        if w != current {
            if open {
                c.push_str("S\n");
            }
            c.push_str(&format!("{} w\n{} {} m\n", num(w, 2), num(a.x, 2), num(a.y, 2)));
            current = w;
            open = true;
        }
        c.push_str(&format!("{} {} l\n", num(b.x, 2), num(b.y, 2)));
    }
    if open {
        c.push_str("S\n");
    }
}

/// Filled circle (four Béziers) for single-point strokes.
fn dot(c: &mut String, p: Sample) {
    let r = p.r.max(0.15);
    let k = 0.552_284_7 * r;
    let (x, y) = (p.x, p.y);
    c.push_str(&format!(
        "{} {} m\n{} {} {} {} {} {} c\n{} {} {} {} {} {} c\n{} {} {} {} {} {} c\n{} {} {} {} {} {} c\nf\n",
        num(x + r, 2), num(y, 2),
        num(x + r, 2), num(y + k, 2), num(x + k, 2), num(y + r, 2), num(x, 2), num(y + r, 2),
        num(x - k, 2), num(y + r, 2), num(x - r, 2), num(y + k, 2), num(x - r, 2), num(y, 2),
        num(x - r, 2), num(y - k, 2), num(x - k, 2), num(y - r, 2), num(x, 2), num(y - r, 2),
        num(x + k, 2), num(y - r, 2), num(x + r, 2), num(y - k, 2), num(x + r, 2), num(y, 2),
    ));
}

/// Compact number formatting (trailing zeros trimmed) to keep streams small.
fn num(v: f32, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    let t = s.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-" {
        "0".to_owned()
    } else {
        t.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Point, TextBox};

    fn sample_doc() -> Document {
        let mut doc = Document::default();
        doc.page_count = 2;
        let pen: Vec<Point> = (0..30)
            .map(|i| Point::new(20.0 + i as f32 * 8.0, 50.0 + (i as f32 * 0.4).sin() * 20.0, 0.2 + i as f32 * 0.02))
            .collect();
        let id = doc.alloc_id();
        doc.strokes.push(Stroke::new(id, 0, StrokeKind::Pen, [10, 20, 200, 255], 3.0, pen.clone()));
        let id = doc.alloc_id();
        doc.strokes.push(Stroke::new(id, 1, StrokeKind::Highlighter, [255, 235, 0, 96], 14.0, pen));
        let id = doc.alloc_id();
        doc.strokes.push(Stroke::new(id, 1, StrokeKind::Pen, [0, 0, 0, 255], 3.0, vec![Point::new(5.0, 5.0, 1.0)]));
        doc.texts.push(TextBox {
            id: 99,
            page: 0,
            x: 40.0,
            y: 200.0,
            width: 300.0,
            size: 18.0,
            color: [0, 0, 0],
            text: "Città (test) \\ €".into(),
            lines: vec!["Città (test) \\ €".into()],
            line_h: 22.0,
        });
        doc
    }

    #[test]
    fn structure_is_consistent() {
        let bytes = build_pdf(&sample_doc(), &PdfOptions::default()).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.5"));
        assert!(bytes.ends_with(b"%%EOF\n"));

        // startxref must point at the "xref" keyword.
        let text = String::from_utf8_lossy(&bytes);
        let pos: usize = text
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(&bytes[pos..pos + 4], b"xref");

        // Every xref entry must point at "<n> 0 obj".
        let table = &text[pos..];
        for (i, line) in table.lines().skip(3).take_while(|l| l.ends_with(" n ")).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            let expect = format!("{} 0 obj", i + 1);
            assert_eq!(&bytes[off..off + expect.len()], expect.as_bytes());
        }
    }

    fn pdf_text(opts: &PdfOptions) -> String {
        String::from_utf8_lossy(&build_pdf(&sample_doc(), opts).unwrap()).into_owned()
    }

    #[test]
    fn page_ranges() {
        let doc = sample_doc();
        let all = PdfOptions::default();
        assert_eq!(all.page_list(&doc), vec![0, 1]);
        let one = PdfOptions { pages: Some(1..=1), ..Default::default() };
        assert_eq!(one.page_list(&doc), vec![1]);
        // Out of range and reversed ranges are clamped / ordered.
        let wild = PdfOptions { pages: Some(5..=0), ..Default::default() };
        assert_eq!(wild.page_list(&doc), vec![0, 1]);
        assert!(pdf_text(&one).contains("/Count 1 "));
    }

    #[test]
    fn fits_onto_another_page_size() {
        let doc = sample_doc();
        // US Letter, 0.5 in margins.
        let opts = PdfOptions { page_size: Some((816.0, 1056.0)), margin: 48.0, ..Default::default() };
        let ((w, h), s, (ox, oy)) = opts.placement(&doc);
        assert_eq!((w, h), (816.0, 1056.0));
        assert!(s < 1.0 && ox >= 47.9 && oy >= 47.9);
        // Centred: equal space left and right, top and bottom.
        assert!((w - doc.page_width * s - 2.0 * ox).abs() < 0.01);
        assert!((h - doc.page_height * s - 2.0 * oy).abs() < 0.01);
        assert!(pdf_text(&opts).contains("/MediaBox [0 0 612 792]"));
    }

    #[test]
    fn ink_colours_and_filters() {
        let black = PdfOptions { ink: InkColors::Black, ..Default::default() };
        assert_eq!(black.ink_rgb([10, 20, 200], false), (0.0, 0.0, 0.0));
        let (r, g, b) = black.ink_rgb([255, 235, 0], true);
        assert!(r == g && g == b && r > 0.5, "highlighter stays light grey");
        let gray = PdfOptions { ink: InkColors::Grayscale, ..Default::default() };
        let (r, g, b) = gray.ink_rgb([10, 20, 200], false);
        assert!(r == g && g == b);

        let no_hl = PdfOptions { include_highlighter: false, ..Default::default() };
        assert!(!pdf_text(&no_hl).contains("/ExtGState <<  /GS"));
        assert!(pdf_text(&PdfOptions::default()).contains("/GS96"));
        assert!(!pdf_text(&no_hl).contains("/GS96 gs"));
    }

    #[test]
    fn pdf_string_escapes() {
        assert_eq!(pdf_string("a(b)\\"), "a\\(b\\)\\\\");
        assert_eq!(pdf_string("è"), "\\350");
        assert_eq!(pdf_string("€"), "\\200");
        assert_eq!(pdf_string("漢"), "?");
    }

    #[test]
    fn number_format() {
        assert_eq!(num(1.500, 2), "1.5");
        assert_eq!(num(2.0, 2), "2");
        assert_eq!(num(-0.001, 2), "-0");
        assert_eq!(num(0.0, 3), "0");
    }
}
