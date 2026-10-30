// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

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
//! * Highlighter strokes are a single stroked path in the colour they leave
//!   on the paper (`model::highlight_on`), with the Darken blend mode: each
//!   point keeps the darker of the highlighter and what is under it, so
//!   overlapping highlighters never darken and nothing is covered (as on
//!   screen). Page order: highlighters, rules, text, pen strokes, on the
//!   white page.
//! * Notes written on a PDF: the PDF's pages go under the pages as vector
//!   form XObjects (`pdf_pages::underlay`, PDFium), so their text stays
//!   text; such pages get no rules.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::path::Path;

use flate2::{Compression, write::ZlibEncoder};
use serde::{Deserialize, Serialize};

use crate::images;
use crate::model::{
    self, Document, ImageData, PageKind, RunStyle, Stroke, StrokeKind, TextBox, TextLayout, TextMark, TextRun,
};
use crate::pdf_pages::{self, Underlay};
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
    /// Notes written on a PDF: put the PDF's pages under them.
    pub include_pdf_pages: bool,
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
            include_pdf_pages: true,
            ink: InkColors::Original,
            page_size: None,
            margin: 0.0,
            title: "Tratto".to_owned(),
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
    pub fn placement(&self, doc: &Document) -> ((f32, f32), f32, (f32, f32)) {
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
    pub fn ink_rgb(&self, rgb: [u8; 3], highlighter: bool) -> (f32, f32, f32) {
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

    /// Colour (0..1) a stroke is printed in: the ink colour, and for a
    /// highlighter the opaque colour it leaves on the background.
    pub fn stroke_rgb(&self, s: &Stroke) -> (f32, f32, f32) {
        let highlighter = s.kind == StrokeKind::Highlighter;
        let (r, g, b) = self.ink_rgb([s.color[0], s.color[1], s.color[2]], highlighter);
        if !highlighter {
            return (r, g, b);
        }
        let byte = |v: f32| (v * 255.0).round() as u8;
        let [r, g, b] = model::highlight_on([byte(r), byte(g), byte(b), s.color[3]], model::PAPER_RGB);
        (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }
}

/// One note of a multi-note export.
pub struct PdfNote<'a> {
    pub doc: &'a Document,
    /// Its bookmark in the PDF.
    pub title: String,
}

/// Writes several notes, one after the other, to one PDF (`opts.pages` only
/// applies to a single note). With more than one note the PDF gets a
/// bookmark per note. Via a temp file, so a failure never truncates an
/// existing export.
pub fn export_notes(notes: &[PdfNote], path: &Path, opts: &PdfOptions) -> io::Result<()> {
    let bytes = export_bytes(notes, opts)?;
    let tmp = path.with_extension("pdf.tmp");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, path)
}

/// The PDF `export_notes` writes, as bytes (to print it).
pub fn export_bytes(notes: &[PdfNote], opts: &PdfOptions) -> io::Result<Vec<u8>> {
    let bytes = build_pdf(notes, opts)?;
    let under = underlays(notes, opts);
    if under.is_empty() { Ok(bytes) } else { pdf_pages::underlay(bytes, under).map_err(io::Error::other) }
}

/// The PDF pages to put under the exported pages (notes written on a PDF),
/// placed like the note pages they belong to: one under each, or (an
/// infinite page) all of them where they are on it.
fn underlays(notes: &[PdfNote], opts: &PdfOptions) -> Vec<Underlay> {
    let mut out = Vec::new();
    if !opts.include_pdf_pages {
        return out;
    }
    for (p, &(note, page)) in export_pages(notes, opts).iter().enumerate() {
        let doc = notes[note].doc;
        let Some(file) = doc.pdf.as_ref().and_then(|pdf| pdf.file.clone()) else { continue };
        let ((_, out_h), s, (ox, oy)) = opts.placement(doc);
        for (source, r) in doc.pdf_places(page) {
            // Page units (y down) to PDF points (y up) on the output page.
            let (left, top) = ((ox + r.left() * s) * PX_TO_PT, (oy + r.top() * s) * PX_TO_PT);
            let (width, height) = (r.width() * s * PX_TO_PT, r.height() * s * PX_TO_PT);
            out.push(Underlay {
                page: p as u32,
                file: file.clone(),
                source_page: source,
                rect: [left, out_h * PX_TO_PT - top - height, width, height],
            });
        }
    }
    out
}

/// The (note, page) pairs that become the PDF's pages, in order.
pub fn export_pages(notes: &[PdfNote], opts: &PdfOptions) -> Vec<(usize, u32)> {
    if let [one] = notes {
        return opts.page_list(one.doc).into_iter().map(|p| (0, p)).collect();
    }
    notes.iter().enumerate().flat_map(|(i, n)| (0..n.doc.page_count.max(1)).map(move |p| (i, p))).collect()
}

/// The characters of the exported pages' text boxes the PDF cannot show,
/// each once, in the order met: its fonts (the PDF's standard ones) cover
/// Western European text only, the others come out as "?".
pub fn unshown_chars(notes: &[PdfNote], opts: &PdfOptions) -> Vec<char> {
    let mut out = Vec::new();
    for (note, page) in export_pages(notes, opts) {
        for t in notes[note].doc.texts.iter().filter(|t| t.page == page) {
            for ch in t.text.chars().filter(|&c| c != '?' && winansi(c) == b'?') {
                if !out.contains(&ch) {
                    out.push(ch);
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------ file layout

fn build_pdf(notes: &[PdfNote], opts: &PdfOptions) -> io::Result<Vec<u8>> {
    let page_list = export_pages(notes, opts);
    let pages = page_list.len();

    // Object numbers: 1 catalog, 2 page tree, 3 resources, 4 info, 5 the
    // highlighters' graphics state, 6.. the fonts, then (page, content)
    // pairs, then (several notes only) the outline root and one bookmark per
    // note, then the pictures (each with its transparency, if it has some).
    let gs_obj = 5;
    let font_obj = 6;
    let first_page_obj = font_obj + FONTS.len();
    let outline_obj = first_page_obj + 2 * pages;
    let outlined = notes.len() > 1;
    let bookmarks = (0..notes.len()).filter(|n| page_list.iter().any(|&(m, _)| m == *n)).count();
    let image_obj = outline_obj + if outlined { 1 + bookmarks } else { 0 };

    // The pictures on the pages exported, each file once: `/Im<n>`.
    let mut pictures: HashMap<u64, usize> = HashMap::new();
    let mut xobjects: Vec<XObject> = Vec::new();
    for &(note, page) in &page_list {
        for img in notes[note].doc.images.iter().filter(|i| i.page == page) {
            if !pictures.contains_key(&img.data.key)
                && let Some(x) = XObject::of(&img.data)?
            {
                pictures.insert(img.data.key, xobjects.len());
                xobjects.push(x);
            }
        }
    }
    let mut next = image_obj;
    let numbers: Vec<usize> = xobjects
        .iter()
        .map(|x| {
            let n = next;
            next += 1 + usize::from(x.mask.is_some());
            n
        })
        .collect();

    let mut pdf = Pdf::new();
    pdf.raw(b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n");

    if outlined {
        pdf.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /Outlines {outline_obj} 0 R /PageMode /UseOutlines >>"));
    } else {
        pdf.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    }

    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", first_page_obj + 2 * i)).collect();
    pdf.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));

    let fonts: Vec<String> = (0..FONTS.len()).map(|i| format!("/F{} {} 0 R", i + 1, font_obj + i)).collect();
    let images: Vec<String> = numbers.iter().enumerate().map(|(i, n)| format!("/Im{i} {n} 0 R")).collect();
    pdf.obj(
        3,
        &format!(
            "<< /ExtGState << /GSH {gs_obj} 0 R >> /Font << {} >> /XObject << {} >> >>",
            fonts.join(" "),
            images.join(" ")
        ),
    );

    let author = if opts.author.trim().is_empty() {
        String::new()
    } else {
        format!("/Author ({}) ", pdf_string(opts.author.trim()))
    };
    pdf.obj(4, &format!("<< /Title ({}) {author}/Producer (Tratto) >>", pdf_string(&opts.title)));
    pdf.obj(gs_obj, "<< /Type /ExtGState /BM /Darken >>");

    // Text boxes use the standard fonts (no font embedding needed).
    for (i, name) in FONTS.iter().enumerate() {
        pdf.obj(
            font_obj + i,
            &format!("<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding >>"),
        );
    }

    for (p, &(note, page)) in page_list.iter().enumerate() {
        let doc = notes[note].doc;
        let ((out_w, out_h), _, _) = opts.placement(doc);
        let (pw, ph) = (out_w * PX_TO_PT, out_h * PX_TO_PT);
        let content = page_content(doc, page, opts, &pictures);
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

    if outlined {
        // Bookmarks: each note points at its first exported page.
        let firsts: Vec<(usize, usize)> = (0..notes.len())
            .filter_map(|n| page_list.iter().position(|&(m, _)| m == n).map(|p| (n, first_page_obj + 2 * p)))
            .collect();
        let items: Vec<usize> = (0..firsts.len()).map(|i| outline_obj + 1 + i).collect();
        pdf.obj(
            outline_obj,
            &format!(
                "<< /Type /Outlines /First {} 0 R /Last {} 0 R /Count {} >>",
                items[0],
                items[items.len() - 1],
                items.len()
            ),
        );
        for (i, &(n, page_obj)) in firsts.iter().enumerate() {
            let prev = if i > 0 { format!("/Prev {} 0 R ", items[i - 1]) } else { String::new() };
            let next = if i + 1 < items.len() { format!("/Next {} 0 R ", items[i + 1]) } else { String::new() };
            pdf.obj(
                items[i],
                &format!(
                    "<< /Title ({}) /Parent {outline_obj} 0 R {prev}{next}/Dest [{page_obj} 0 R /Fit] >>",
                    pdf_string(&notes[n].title)
                ),
            );
        }
    }

    for (x, &n) in xobjects.iter().zip(&numbers) {
        let mask = x.mask.as_ref().map_or(String::new(), |_| format!(" /SMask {} 0 R", n + 1));
        pdf.stream_obj(n, &format!("{}{mask}", x.dict), &x.data);
        if let Some(alpha) = &x.mask {
            let dict = format!(
                "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode",
                x.size[0], x.size[1]
            );
            pdf.stream_obj(n + 1, &dict, alpha);
        }
    }

    Ok(pdf.finish(1, 4))
}

/// A picture as a PDF image: a JPEG as it is (grey or colour), anything
/// else its pixels, compressed, with its transparency as a mask if it has
/// some.
struct XObject {
    /// The stream's dictionary entries, without `/Length`.
    dict: String,
    data: Vec<u8>,
    size: [u32; 2],
    /// The transparency, compressed.
    mask: Option<Vec<u8>>,
}

impl XObject {
    /// `None`: a picture that cannot be read (it is left out).
    fn of(img: &ImageData) -> io::Result<Option<XObject>> {
        let size = [img.width, img.height];
        let head = |space: &str, filter: &str| {
            format!(
                "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /{space} /BitsPerComponent 8 /Filter /{filter}",
                img.width, img.height
            )
        };
        if img.is_jpeg() {
            let space = match images::jpeg_channels(&img.bytes) {
                Some(1) => Some("DeviceGray"),
                Some(3) => Some("DeviceRGB"),
                _ => None,
            };
            if let Some(space) = space {
                return Ok(Some(XObject { dict: head(space, "DCTDecode"), data: img.bytes.clone(), size, mask: None }));
            }
        }
        let Ok(pixels) = img.decode(u32::MAX) else { return Ok(None) };
        let size = [pixels.width(), pixels.height()];
        let rgb: Vec<u8> = pixels.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let alpha: Vec<u8> = pixels.pixels().map(|p| p[3]).collect();
        let mask = if alpha.iter().any(|a| *a < 255) { Some(deflate(&alpha)?) } else { None };
        let dict = format!(
            "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode",
            size[0], size[1]
        );
        Ok(Some(XObject { dict, data: deflate(&rgb)?, size, mask }))
    }
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
        self.buf.extend_from_slice(format!("<< /Length {} {dict_extra} >>\nstream\n", data.len()).as_bytes());
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

/// The drawing of note page `page`; `pictures` names the pictures' images
/// (`/Im<n>`) by their files' keys.
fn page_content(doc: &Document, page: u32, opts: &PdfOptions, pictures: &HashMap<u64, usize>) -> String {
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
    // Only the page: what crosses onto the next one (a sheet of an infinite
    // page, see `Document::as_sheets`) is cut off at its edge, not drawn in
    // the margin.
    c.push_str(&format!("0 0 {} {} re W n\n", num(doc.page_width, 2), num(doc.page_height, 2)));

    // Bottom to top, as on screen: rules, pictures, highlighters, text, pen
    // strokes.
    let strokes = |kind: StrokeKind| doc.strokes.iter().filter(move |s| s.page == page && s.kind == kind);
    if opts.include_rules && doc.ruled(page) {
        // Not over the PDF's pages on it (an infinite page): they go under
        // everything (see `underlays`), and cover the rules on screen.
        let holes = if opts.include_pdf_pages { doc.pdf_places(page) } else { Vec::new() };
        if holes.is_empty() {
            write_rules(&mut c, doc);
        } else {
            let re = |r: emath::Rect| {
                let [x, y, w, h] = [r.left(), r.top(), r.width(), r.height()].map(|v| num(v, 2));
                format!(
                    "{x} {y} {w} {h} re
"
                )
            };
            c.push_str(
                "q
",
            );
            c.push_str(&re(emath::Rect::from_min_size(
                emath::Pos2::ZERO,
                emath::vec2(doc.page_width, doc.page_height),
            )));
            for (_, r) in holes {
                c.push_str(&re(r));
            }
            // Even-odd: the page without the PDF's pages.
            c.push_str(
                "W* n
",
            );
            write_rules(&mut c, doc);
            c.push_str(
                "Q
",
            );
        }
    }
    for img in doc.images.iter().filter(|i| i.page == page) {
        if let Some(n) = pictures.get(&img.data.key) {
            // The image's unit square, upright (the page space is y down).
            let (w, h) = (num(img.width, 2), num(img.height, 2));
            c.push_str(&format!("q\n{w} 0 0 -{h} {} {} cm\n/Im{n} Do\nQ\n", num(img.x, 2), num(img.y + img.height, 2)));
        }
    }
    if opts.include_highlighter {
        for s in strokes(StrokeKind::Highlighter) {
            write_stroke(&mut c, s, opts);
        }
    }
    if opts.include_text {
        for t in doc.texts.iter().filter(|t| t.page == page) {
            write_text(&mut c, t, opts);
        }
    }
    for s in strokes(StrokeKind::Pen) {
        write_stroke(&mut c, s, opts);
    }
    c.push_str("Q\n");
    c
}

fn write_rules(c: &mut String, doc: &Document) {
    let spacing = doc.page.spacing.max(8.0);
    if doc.page.kind == PageKind::Dotted {
        // Same blue grey as on screen (160, 176, 198): each dot a line of no
        // length with round caps.
        c.push_str(&format!("0.627 0.690 0.776 RG\n{} w\n1 J\n", num(2.0 * model::DOT_RADIUS, 2)));
        let mut y = spacing;
        while y < doc.page_height {
            let mut x = spacing;
            while x < doc.page_width {
                let (dx, dy) = (num(x, 2), num(y, 2));
                c.push_str(&format!("{dx} {dy} m {dx} {dy} l S\n"));
                x += spacing;
            }
            y += spacing;
        }
        return;
    }
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

/// The standard fonts text boxes use, `/F1` to `/F5`: regular, bold, italic,
/// bold italic, code.
const FONTS: [&str; 5] = ["Helvetica", "Helvetica-Bold", "Helvetica-Oblique", "Helvetica-BoldOblique", "Courier"];

/// The font (`/F<n>`) of a run of text.
fn font_of(style: RunStyle) -> usize {
    match style {
        RunStyle { code: true, .. } => 5,
        RunStyle { bold: true, italic: true, .. } => 4,
        RunStyle { italic: true, .. } => 3,
        RunStyle { bold: true, .. } => 2,
        _ => 1,
    }
}

/// Text box: its Markdown laid out as on screen (one `Tj` per run of one
/// style, at the run's place, with the rules, quote bars and task boxes), or
/// without a layout one `Tj` per stored line. Either way the PDF breaks
/// lines exactly like the screen. Only characters of the WinAnsi (Latin-1
/// like) set can be printed; anything else becomes `?`.
fn write_text(c: &mut String, t: &TextBox, opts: &PdfOptions) {
    if let Some(layout) = &t.layout {
        return write_layout(c, t, layout, opts);
    }
    if t.lines.iter().all(|l| l.trim().is_empty()) {
        return;
    }
    let (r, g, b) = opts.ink_rgb(t.color, false);
    c.push_str(&format!("q\n{} {} {} rg\nBT\n/F1 {} Tf\n", num(r, 3), num(g, 3), num(b, 3), num(t.size, 2)));
    for (i, line) in t.lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let baseline = t.y + i as f32 * t.line_h + t.line_h * 0.78;
        // The page matrix is y-down; this text matrix flips it back so glyphs are upright.
        c.push_str(&format!("1 0 0 -1 {} {} Tm\n({}) Tj\n", num(t.x, 2), num(baseline, 2), pdf_string(line)));
    }
    c.push_str("ET\nQ\n");
}

fn write_layout(c: &mut String, t: &TextBox, layout: &TextLayout, opts: &PdfOptions) {
    let (r, g, b) = opts.ink_rgb(t.color, false);
    let (r, g, b) = (num(r, 3), num(g, 3), num(b, 3));
    c.push_str(&format!("q\n{r} {g} {b} rg {r} {g} {b} RG\n"));
    let at = |x: f32, y: f32| format!("{} {}", num(t.x + x, 2), num(t.y + y, 2));
    // Code's grey background, under its text: code blocks', then code
    // within lines.
    let spans: Vec<&TextRun> = layout.runs.iter().filter(|r| r.style.code_span).collect();
    let shades = layout.marks.iter().filter(|m| matches!(m, TextMark::Shade { .. }));
    c.push_str("q\n0.94 0.94 0.94 rg\n");
    for mark in shades {
        if let TextMark::Shade { x, top, width, height } = *mark {
            c.push_str(&format!("{} {} {} re f\n", at(x, top), num(width, 2), num(height, 2)));
        }
    }
    for run in spans {
        let (x, y) = (run.x - 2.0, run.top + run.line_h * 0.08);
        c.push_str(&format!("{} {} {} re f\n", at(x, y), num(run.width + 4.0, 2), num(run.line_h * 0.84, 2)));
    }
    c.push_str("Q\n");
    for mark in &layout.marks {
        match *mark {
            TextMark::Line { x0, y0, x1, y1 } => {
                c.push_str(&format!("{} w {} m {} l S\n", num(t.size * 0.05, 2), at(x0, y0), at(x1, y1)));
            }
            TextMark::Shade { .. } => {}
            TextMark::Quote { x, top, bottom } => {
                let w = (t.size * 0.18).max(1.0);
                c.push_str(&format!("{} {} {} re f\n", at(x, top), num(w, 2), num(bottom - top, 2)));
            }
            TextMark::Check { x, top, size, checked } => {
                let s = num(size, 2);
                c.push_str(&format!("{} w {} {s} {s} re S\n", num(size * 0.09, 2), at(x, top)));
                if checked {
                    let tick =
                        [(0.22, 0.52), (0.42, 0.72), (0.78, 0.28)].map(|(u, v)| at(x + u * size, top + v * size));
                    c.push_str(&format!("{} m {} l {} l S\n", tick[0], tick[1], tick[2]));
                }
            }
        }
    }
    c.push_str("BT\n");
    for run in &layout.runs {
        if run.text.trim().is_empty() {
            continue;
        }
        let font = font_of(run.style);
        // The page matrix is y-down; this text matrix flips it back so glyphs are upright.
        c.push_str(&format!("/F{font} {} Tf\n1 0 0 -1 {} Tm\n", num(run.size, 2), at(run.x, run.baseline)));
        c.push_str(&format!("({}) Tj\n", pdf_string(&run.text)));
    }
    c.push_str("ET\n");
    // Struck and underlined runs.
    for run in layout.runs.iter().filter(|r| r.style.strike || r.style.underline) {
        let y = if run.style.strike { run.baseline - run.size * 0.3 } else { run.baseline + run.size * 0.12 };
        c.push_str(&format!("{} w {} m {} l S\n", num(run.size * 0.06, 2), at(run.x, y), at(run.x + run.width, y)));
    }
    c.push_str("Q\n");
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
    let (r, g, b) = opts.stroke_rgb(s);
    c.push_str(&format!("q\n{r} {g} {b} RG {r} {g} {b} rg\n1 J 1 j\n", r = num(r, 3), g = num(g, 3), b = num(b, 3)));
    if s.kind == StrokeKind::Highlighter {
        c.push_str("/GSH gs\n");
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
        num(x + r, 2),
        num(y, 2),
        num(x + r, 2),
        num(y + k, 2),
        num(x + k, 2),
        num(y + r, 2),
        num(x, 2),
        num(y + r, 2),
        num(x - k, 2),
        num(y + r, 2),
        num(x - r, 2),
        num(y + k, 2),
        num(x - r, 2),
        num(y, 2),
        num(x - r, 2),
        num(y - k, 2),
        num(x - k, 2),
        num(y - r, 2),
        num(x, 2),
        num(y - r, 2),
        num(x + k, 2),
        num(y - r, 2),
        num(x + r, 2),
        num(y - k, 2),
        num(x + r, 2),
        num(y, 2),
    ));
}

/// Compact number formatting (trailing zeros trimmed) to keep streams small.
fn num(v: f32, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    let t = s.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() || t == "-" { "0".to_owned() } else { t.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Point, TextBox};
    use crate::pdf_pages::PdfFile;

    #[test]
    fn an_infinite_page_on_a_pdf_exports_all_of_the_pdf_where_it_is() {
        let mut doc = Document::on_pdf(PdfFile::new(Vec::new()), vec![[600.0, 800.0]; 3]);
        doc.pdf_to_canvas();
        let page = doc.as_pages();
        let notes = [PdfNote { doc: &page, title: String::new() }];
        let under = underlays(&notes, &PdfOptions::default());
        assert_eq!(under.iter().map(|u| (u.page, u.source_page)).collect::<Vec<_>>(), [(0, 0), (0, 1), (0, 2)]);
        // One under the other on the one exported page (y up: lower bottoms).
        assert!(under[0].rect[1] > under[1].rect[1] && under[1].rect[1] > under[2].rect[1]);
        let none = PdfOptions { include_pdf_pages: false, ..PdfOptions::default() };
        assert!(underlays(&notes, &none).is_empty());
    }

    #[test]
    fn the_rules_of_an_infinite_page_stay_under_its_pdf_pages() {
        let mut doc = Document::on_pdf(PdfFile::new(Vec::new()), vec![[600.0, 800.0]; 2]);
        doc.page.kind = PageKind::Ruled;
        doc.pdf_to_canvas();
        let page = doc.as_pages();
        let opts = PdfOptions::default();
        let content = page_content(&page, 0, &opts, &HashMap::new());
        // The page with a hole per PDF page, then the rules, inside it.
        let clip = content.find("W* n").expect("the rules are clipped");
        assert_eq!(
            content[..clip]
                .matches(
                    " re
"
                )
                .count(),
            3
        );
        assert!(content[clip..].contains(
            " l S
"
        ));
        // Without the PDF's pages, nothing to keep them off.
        let bare = PdfOptions { include_pdf_pages: false, ..opts };
        assert!(!page_content(&page, 0, &bare, &HashMap::new()).contains("W* n"));
    }

    #[test]
    fn what_crosses_onto_the_next_sheet_is_cut_off_at_the_page() {
        let doc = sample_doc();
        let content = page_content(&doc, 0, &PdfOptions::default(), &HashMap::new());
        let clip = format!("0 0 {} {} re W n", num(doc.page_width, 2), num(doc.page_height, 2));
        let at = content.find(&clip).expect("clipped to the page");
        assert!(at < content.find(" l S").unwrap_or(usize::MAX), "before anything is drawn");
    }

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
            layout: None,
        });
        doc
    }

    #[test]
    fn text_the_fonts_cannot_show_is_found() {
        let mut doc = sample_doc();
        assert!(unshown_chars(&[PdfNote { doc: &doc, title: "A".into() }], &PdfOptions::default()).is_empty());
        let mut far = doc.texts[0].clone();
        far.id = 100;
        far.page = 1;
        far.text = "Łódź? 東京 Łódź".into();
        doc.texts.push(far);
        let note = [PdfNote { doc: &doc, title: "A".into() }];
        assert_eq!(unshown_chars(&note, &PdfOptions::default()), ['Ł', 'ź', '東', '京']);
        // Only the pages exported count.
        let first = PdfOptions { pages: Some(0..=0), ..PdfOptions::default() };
        assert!(unshown_chars(&note, &first).is_empty());
    }

    #[test]
    fn dotted_paper_is_dots() {
        let mut doc = Document::default();
        doc.page = crate::model::PageStyle { kind: PageKind::Dotted, spacing: 100.0 };
        let mut c = String::new();
        write_rules(&mut c, &doc);
        // A4 (794 x 1123): 7 x 11 dots, each a round-capped line of no length.
        assert_eq!(c.matches(" l S").count(), 7 * 11);
        assert!(c.contains("1 J") && c.contains("100 100 m 100 100 l S"), "{c}");
    }

    #[test]
    fn structure_is_consistent() {
        let doc = sample_doc();
        let bytes = build_pdf(&[PdfNote { doc: &doc, title: "A".into() }], &PdfOptions::default()).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.5"));
        assert!(bytes.ends_with(b"%%EOF\n"));

        // startxref must point at the "xref" keyword.
        let text = String::from_utf8_lossy(&bytes);
        let pos: usize = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert_eq!(&bytes[pos..pos + 4], b"xref");

        // Every xref entry must point at "<n> 0 obj".
        // Sliced from the bytes: `text` is lossy, so its offsets can differ.
        let table = String::from_utf8_lossy(&bytes[pos..]);
        for (i, line) in table.lines().skip(3).take_while(|l| l.ends_with(" n ")).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            let expect = format!("{} 0 obj", i + 1);
            assert_eq!(&bytes[off..off + expect.len()], expect.as_bytes());
        }
    }

    /// A box laid out from Markdown prints its runs in their fonts, at their
    /// places, and its marks.
    #[test]
    fn laid_out_text_uses_its_fonts_and_marks() {
        let mut doc = sample_doc();
        let run = |text: &str, x: f32, style: RunStyle| TextRun {
            text: text.into(),
            source: None,
            x,
            baseline: 16.0,
            top: 0.0,
            line_h: 20.0,
            width: 30.0,
            size: 16.0,
            ascent: 15.0,
            style,
        };
        let layout = TextLayout {
            runs: vec![
                run("bold", 0.0, RunStyle { bold: true, ..Default::default() }),
                run("code", 40.0, RunStyle { code: true, ..Default::default() }),
                run("gone", 80.0, RunStyle { strike: true, ..Default::default() }),
            ],
            marks: vec![
                TextMark::Check { x: 0.0, top: 22.0, size: 12.0, checked: true },
                TextMark::Line { x0: 0.0, y0: 50.0, x1: 300.0, y1: 50.0 },
            ],
            height: 60.0,
        };
        doc.texts[0].layout = Some(std::sync::Arc::new(layout));
        let c = page_content(&doc, 0, &PdfOptions::default(), &HashMap::new());
        assert!(c.contains("/F2 16 Tf\n1 0 0 -1 40 216 Tm\n(bold) Tj"), "bold, at the run's baseline");
        assert!(c.contains("/F5 16 Tf\n1 0 0 -1 80 216 Tm\n(code) Tj"), "code in Courier");
        assert!(c.contains(" re S\n"), "the task's box");
        assert!(c.contains("40 250 m 340 250 l S"), "the rule");
        assert!(!c.contains("Città"), "the stored lines are not printed too");
        let fonts = String::from_utf8_lossy(
            &build_pdf(&[PdfNote { doc: &doc, title: "A".into() }], &PdfOptions::default()).unwrap(),
        )
        .into_owned();
        assert!(FONTS.iter().all(|f| fonts.contains(&format!("/BaseFont /{f} "))));
    }

    /// Pictures become images, each file once: a JPEG as it is, a PNG with
    /// transparency as pixels and a mask; the file stays consistent.
    #[test]
    fn pictures_are_embedded_once() {
        use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
        let encode = |img: DynamicImage, format| {
            let mut out = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut out), format).unwrap();
            ImageData::from_file(out).unwrap()
        };
        let png = encode(DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, Rgba([9, 8, 7, 100]))), ImageFormat::Png);
        let jpeg = encode(DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([1, 2, 3]))), ImageFormat::Jpeg);
        let mut doc = sample_doc();
        for (n, data) in [&png, &png, &jpeg].into_iter().enumerate() {
            let id = doc.alloc_id();
            let (x, y, width, height, data) = (n as f32 * 50.0, 10.0, 40.0, 20.0, data.clone());
            doc.images.push(model::Image { id, page: 0, x, y, width, height, data });
        }
        let bytes = build_pdf(&[PdfNote { doc: &doc, title: "A".into() }], &PdfOptions::default()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text.matches("/Subtype /Image").count(), 3, "the PNG, its mask, the JPEG");
        assert_eq!(text.matches("/SMask").count(), 1);
        assert_eq!(text.matches("/DCTDecode").count(), 1);
        let pictures = HashMap::from([(png.key, 0), (jpeg.key, 1)]);
        let c = page_content(&doc, 0, &PdfOptions::default(), &pictures);
        assert_eq!(c.matches("/Im0 Do").count(), 2);
        assert!(c.contains("q\n40 0 0 -20 0 30 cm\n/Im0 Do"), "upright, at its place");

        // Every xref entry points at its object.
        let pos: usize = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        let table = String::from_utf8_lossy(&bytes[pos..]);
        for (i, line) in table.lines().skip(3).take_while(|l| l.ends_with(" n ")).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            let expect = format!("{} 0 obj", i + 1);
            assert_eq!(&bytes[off..off + expect.len()], expect.as_bytes());
        }
    }

    fn pdf_text(opts: &PdfOptions) -> String {
        let doc = sample_doc();
        let notes = [PdfNote { doc: &doc, title: opts.title.clone() }];
        String::from_utf8_lossy(&build_pdf(&notes, opts).unwrap()).into_owned()
    }

    #[test]
    fn several_notes_with_bookmarks() {
        let (a, b) = (sample_doc(), Document::default());
        let notes = [PdfNote { doc: &a, title: "First".into() }, PdfNote { doc: &b, title: "Second".into() }];
        let opts = PdfOptions::default();
        // 2 + 1 pages; a page range only applies to a single note.
        assert_eq!(export_pages(&notes, &opts).len(), 3);
        let ranged = PdfOptions { pages: Some(0..=0), ..Default::default() };
        assert_eq!(export_pages(&notes, &ranged).len(), 3);
        let text = String::from_utf8_lossy(&build_pdf(&notes, &opts).unwrap()).into_owned();
        assert!(text.contains("/Count 3 "));
        assert!(text.contains("/Type /Outlines") && text.contains("(First)") && text.contains("(Second)"));
        assert!(text.contains("/PageMode /UseOutlines"));
        // A single note has no bookmarks.
        assert!(!pdf_text(&opts).contains("/Outlines"));
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)] // a reversed range on purpose
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

        // Highlighters: opaque, in the colour they leave on the background
        // (no transparency, so overlapping strokes cannot darken).
        let opts = PdfOptions::default();
        let hl = Stroke::new(0, 0, StrokeKind::Highlighter, [255, 235, 0, 96], 14.0, Vec::new());
        let (r, g, b) = opts.stroke_rgb(&hl);
        let [br, bg, bb] = model::PAPER_RGB.map(|v| v as f32 / 255.0);
        assert!(r <= br + 1e-3 && b < bb && g < bg, "between the background and the ink");
        let colour = format!("{} {} {} RG", num(r, 3), num(g, 3), num(b, 3));
        let doc = sample_doc();
        let rules = PdfOptions { include_rules: true, ..Default::default() };
        let page = page_content(&doc, 1, &rules, &HashMap::new());
        let at = |what: &str| page.find(what).unwrap_or_else(|| panic!("{what} missing"));
        // Over the rules (and pictures), darkening them; under the pen ink (black).
        assert!(at("0.784 0.839 0.902 RG") < at(&colour) && at(&colour) < at("0 0 0 RG"));
        assert!(pdf_text(&opts).contains("/BM /Darken"));
        assert!(page.contains("/GSH gs"));
        let no_hl = PdfOptions { include_highlighter: false, ..Default::default() };
        assert!(!page_content(&doc, 1, &no_hl, &HashMap::new()).contains(&colour));
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
