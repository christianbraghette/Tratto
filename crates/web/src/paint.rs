// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Painting a page on a canvas, bottom to top as the app does: paper, the
//! PDF page it is written on, rules, pictures, highlighters, text, pen.
//!
//! Strokes are the app's smoothed curve (`stroke_path::smoothed`, sampled
//! as densely as on screen) stroked with round caps and joins; a pen whose
//! width changes is drawn in runs of one width, as the PDF export does. Ink
//! is opaque and highlighters are in the colour they leave on the paper with
//! the darken blend, so where runs or strokes overlap nothing gets darker.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::f64::consts::TAU;
use std::sync::{Arc, mpsc};

use tratto_core::model::{self, Document, ImageData as Picture, PageKind, Stroke, StrokeKind, TextBox};
use tratto_core::pdf_pages::{self, PdfFile};
use tratto_core::stroke_path::{self, Sample};
use wasm_bindgen::{Clamped, JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};

/// Rules and grid lines, as on screen (`theme`'s `page_rule`).
const RULE_RGB: [u8; 3] = [203, 216, 232];
/// What is not there (a PDF page PDFium cannot render, a picture that cannot
/// be read): its place, greyed, as the app shows it.
const MISSING_RGB: [u8; 3] = [225, 225, 225];
/// Pictures are decoded at most this large (pixels on the longer side).
const PICTURE_MAX_SIDE: u32 = 2048;
/// Rasterised PDF pages kept (a few pages each side of the one in view).
const PDF_PAGES_KEPT: usize = 8;
/// Spacing of curve samples in canvas pixels (`stroke_mesh`'s, for 1 pixel
/// per point).
const SAMPLE_STEP_PX: f32 = 2.5;
/// Text boxes: the app's font (see `www/`), else the browser's.
const FONT_FAMILY: &str = "Inter, \"Helvetica Neue\", Arial, sans-serif";

/// What painting keeps from one page to the next.
#[derive(Default)]
pub struct Cache {
    /// Rasterised PDF pages: `(file key, page, width)`, most recently used last.
    pdf: Vec<((u64, u32, u32), HtmlCanvasElement)>,
    /// Pictures by their file's key; `None`: unreadable.
    pictures: HashMap<u64, Option<HtmlCanvasElement>>,
}

/// Paints note page `page` on `canvas`, sized to it at `scale` canvas
/// pixels per document unit. Returns what could not be shown, if anything.
pub fn page(
    doc: &Document,
    page: u32,
    canvas: &HtmlCanvasElement,
    scale: f32,
    cache: &mut Cache,
) -> Result<Option<String>, JsValue> {
    let scale = scale.max(0.01);
    // Resizing also clears the canvas and resets its state.
    canvas.set_width((doc.page_width * scale).round().max(1.0) as u32);
    canvas.set_height((doc.page_height * scale).round().max(1.0) as u32);
    let ctx: CanvasRenderingContext2d =
        canvas.get_context("2d")?.ok_or_else(|| JsValue::from_str("no 2D canvas"))?.dyn_into()?;
    let s = f64::from(scale);
    let mut missing = Vec::new();

    ctx.set_fill_style_str(&css(model::PAPER_RGB));
    ctx.fill_rect(0.0, 0.0, f64::from(canvas.width()), f64::from(canvas.height()));

    if let Some((file, source, [x, y, w, h])) = pdf_under(doc, page) {
        let rect = [f64::from(x) * s, f64::from(y) * s, f64::from(w) * s, f64::from(h) * s];
        let width = (w * scale).round().max(1.0) as u32;
        match cache.pdf_page(&file, source, width) {
            // At whole pixels, unscaled: a raster stretched by a fraction of
            // a pixel (pages rarely have whole sizes) is blurred all over.
            Ok(image) if image.width() == width => {
                ctx.draw_image_with_html_canvas_element(&image, rect[0].round(), rect[1].round())?
            }
            // Capped at `MAX_RENDER_PX`: scaled up to its place.
            Ok(image) => {
                ctx.draw_image_with_html_canvas_element_and_dw_and_dh(&image, rect[0], rect[1], rect[2], rect[3])?
            }
            Err(e) => {
                fill_rect(&ctx, rect, MISSING_RGB);
                missing.push(e);
            }
        }
    }

    if doc.ruled(page) {
        rules(&ctx, doc, s);
    }

    for img in doc.images.iter().filter(|i| i.page == page) {
        let rect = [f64::from(img.x) * s, f64::from(img.y) * s, f64::from(img.width) * s, f64::from(img.height) * s];
        match cache.picture(&img.data)? {
            Some(image) => {
                ctx.draw_image_with_html_canvas_element_and_dw_and_dh(image, rect[0], rect[1], rect[2], rect[3])?
            }
            None => {
                fill_rect(&ctx, rect, MISSING_RGB);
                missing.push("a picture cannot be read".to_owned());
            }
        }
    }

    let strokes = |kind: StrokeKind| doc.strokes.iter().filter(move |s| s.page == page && s.kind == kind);
    ctx.set_line_cap("round");
    ctx.set_line_join("round");
    ctx.set_global_composite_operation("darken")?;
    for stroke in strokes(StrokeKind::Highlighter) {
        paint_stroke(&ctx, stroke, scale);
    }
    ctx.set_global_composite_operation("source-over")?;

    for t in doc.texts.iter().filter(|t| t.page == page) {
        paint_text(&ctx, t, s);
    }

    for stroke in strokes(StrokeKind::Pen) {
        paint_stroke(&ctx, stroke, scale);
    }

    missing.dedup();
    Ok((!missing.is_empty()).then(|| missing.join("; ")))
}

/// The PDF page under note page `page`: its file, its page and where it
/// goes (`[x, y, width, height]`, page units).
fn pdf_under(doc: &Document, page: u32) -> Option<(Arc<PdfFile>, u32, [f32; 4])> {
    let pdf = doc.pdf.as_ref()?;
    let source = doc.pdf_page(page)?;
    let rect = pdf.placement(source, doc.page_width, doc.page_height)?;
    Some((pdf.file.clone()?, source, rect))
}

impl Cache {
    /// Page `page` of `file` rasterised `width` pixels wide.
    fn pdf_page(&mut self, file: &Arc<PdfFile>, page: u32, width: u32) -> Result<HtmlCanvasElement, String> {
        let key = (file.key, page, width);
        if let Some(i) = self.pdf.iter().position(|(k, _)| *k == key) {
            let entry = self.pdf.remove(i);
            self.pdf.push(entry);
            return Ok(self.pdf.last().expect("just pushed").1.clone());
        }
        // In WebAssembly the answer is on the channel when `render` returns.
        let (reply, answer) = mpsc::channel();
        pdf_pages::render(file, page, width, None, reply, Arc::new(|| {}));
        let rendered = answer.try_recv().map_err(|_| "PDFium did not answer".to_owned())?;
        let (w, h, rgba) = rendered.image.map_err(|e| e.to_string())?;
        let image = to_canvas(w, h, &rgba).map_err(|e| crate::js_message(&e))?;
        if self.pdf.len() >= PDF_PAGES_KEPT {
            self.pdf.remove(0);
        }
        self.pdf.push((key, image.clone()));
        Ok(image)
    }

    /// The picture `data`, decoded; `None` if it cannot be read.
    fn picture(&mut self, data: &Picture) -> Result<Option<&HtmlCanvasElement>, JsValue> {
        let image = match self.pictures.entry(data.key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(match data.decode(PICTURE_MAX_SIDE) {
                Ok(rgba) => Some(to_canvas(rgba.width(), rgba.height(), rgba.as_raw())?),
                Err(_) => None,
            }),
        };
        Ok(image.as_ref())
    }
}

/// A canvas holding `rgba` (unmultiplied, row by row).
fn to_canvas(width: u32, height: u32, rgba: &[u8]) -> Result<HtmlCanvasElement, JsValue> {
    let document = web_sys::window().and_then(|w| w.document()).ok_or_else(|| JsValue::from_str("no document"))?;
    let canvas: HtmlCanvasElement = document.create_element("canvas")?.dyn_into()?;
    canvas.set_width(width);
    canvas.set_height(height);
    let ctx: CanvasRenderingContext2d =
        canvas.get_context("2d")?.ok_or_else(|| JsValue::from_str("no 2D canvas"))?.dyn_into()?;
    let pixels = ImageData::new_with_u8_clamped_array_and_sh(Clamped(rgba), width, height)?;
    ctx.put_image_data(&pixels, 0.0, 0.0)?;
    Ok(canvas)
}

fn css([r, g, b]: [u8; 3]) -> String {
    format!("rgb({r}, {g}, {b})")
}

fn fill_rect(ctx: &CanvasRenderingContext2d, [x, y, w, h]: [f64; 4], rgb: [u8; 3]) {
    ctx.set_fill_style_str(&css(rgb));
    ctx.fill_rect(x, y, w, h);
}

/// The rules (and the grid), one screen pixel wide at any zoom, as in the app.
fn rules(ctx: &CanvasRenderingContext2d, doc: &Document, s: f64) {
    let spacing = f64::from(doc.page.spacing.max(8.0));
    let (w, h) = (f64::from(doc.page_width), f64::from(doc.page_height));
    ctx.set_stroke_style_str(&css(RULE_RGB));
    ctx.set_line_width(web_sys::window().map_or(1.0, |w| w.device_pixel_ratio()));
    ctx.begin_path();
    let mut y = spacing;
    while y < h {
        ctx.move_to(0.0, y * s);
        ctx.line_to(w * s, y * s);
        y += spacing;
    }
    if doc.page.kind == PageKind::Grid {
        let mut x = spacing;
        while x < w {
            ctx.move_to(x * s, 0.0);
            ctx.line_to(x * s, h * s);
            x += spacing;
        }
    }
    ctx.stroke();
}

/// A text box's stored lines (its text as shown, wrapped), placed as the PDF
/// export places them.
fn paint_text(ctx: &CanvasRenderingContext2d, t: &TextBox, s: f64) {
    ctx.set_fill_style_str(&css(t.color));
    ctx.set_font(&format!("{}px {FONT_FAMILY}", f64::from(t.size) * s));
    for (i, line) in t.lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let baseline = t.y + i as f32 * t.line_h + t.line_h * 0.78;
        let _ = ctx.fill_text(line, f64::from(t.x) * s, f64::from(baseline) * s);
    }
}

fn paint_stroke(ctx: &CanvasRenderingContext2d, stroke: &Stroke, scale: f32) {
    if stroke.points.is_empty() {
        return;
    }
    let [r, g, b, a] = match stroke.kind {
        StrokeKind::Highlighter => {
            let [r, g, b] = model::highlight_on(stroke.color, model::PAPER_RGB);
            [r, g, b, 255]
        }
        StrokeKind::Pen => stroke.color,
    };
    let color = format!("rgba({r}, {g}, {b}, {})", f32::from(a) / 255.0);
    ctx.set_fill_style_str(&color);
    ctx.set_stroke_style_str(&color);
    // Never thinner than one canvas pixel, as on screen.
    let samples = stroke_path::smoothed(stroke.kind, stroke.width, &stroke.points, scale, 0.5, SAMPLE_STEP_PX, 0.05);
    for run in runs(stroke.kind, &samples) {
        match run {
            Run::Dot(p) => {
                ctx.begin_path();
                let _ = ctx.arc(f64::from(p.x), f64::from(p.y), f64::from(p.r), 0.0, TAU);
                ctx.fill();
            }
            Run::Line { width, points } => {
                ctx.set_line_width(f64::from(width));
                ctx.begin_path();
                ctx.move_to(f64::from(points[0].x), f64::from(points[0].y));
                for p in &points[1..] {
                    ctx.line_to(f64::from(p.x), f64::from(p.y));
                }
                ctx.stroke();
            }
        }
    }
}

/// A piece of a stroke to draw.
#[derive(Debug, PartialEq)]
enum Run<'a> {
    /// A single-point stroke: a disc.
    Dot(Sample),
    /// A line of one `width` through `points` (at least two).
    Line { width: f32, points: &'a [Sample] },
}

/// How the curve `samples` is drawn, as the PDF export draws it: one line of
/// the mean width for a highlighter or a pen of even pressure, else runs of
/// segments whose width (quantised) is the same, each run starting where the
/// one before ended.
fn runs(kind: StrokeKind, samples: &[Sample]) -> Vec<Run<'_>> {
    match samples {
        [] => Vec::new(),
        [dot] => vec![Run::Dot(*dot)],
        _ if kind == StrokeKind::Highlighter || is_uniform(samples) => {
            let mean_r = samples.iter().map(|p| p.r).sum::<f32>() / samples.len() as f32;
            vec![Run::Line { width: mean_r * 2.0, points: samples }]
        }
        _ => {
            let width = |i: usize| ((samples[i].r + samples[i + 1].r) * 4.0).round() / 4.0;
            let mut out = Vec::new();
            let mut start = 0;
            for i in 1..samples.len() - 1 {
                if width(i) != width(start) {
                    out.push(Run::Line { width: width(start), points: &samples[start..=i] });
                    start = i;
                }
            }
            out.push(Run::Line { width: width(start), points: &samples[start..] });
            out
        }
    }
}

fn is_uniform(samples: &[Sample]) -> bool {
    let r0 = samples[0].r;
    samples.iter().all(|p| (p.r - r0).abs() <= 0.03 * r0.max(0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(x: f32, r: f32) -> Sample {
        Sample { x, y: 0.0, r }
    }

    #[test]
    fn a_pen_of_changing_width_is_drawn_in_joined_runs() {
        let samples = [sample(0.0, 1.0), sample(1.0, 1.0), sample(2.0, 2.0), sample(3.0, 2.0), sample(4.0, 2.0)];
        let runs = runs(StrokeKind::Pen, &samples);
        let widths: Vec<f32> = runs
            .iter()
            .map(|r| match r {
                Run::Line { width, .. } => *width,
                Run::Dot(_) => panic!("a dot"),
            })
            .collect();
        assert_eq!(widths, [2.0, 3.0, 4.0]);
        // Every segment drawn once, each run starting where the last ended.
        let segments: usize =
            runs.iter().map(|r| if let Run::Line { points, .. } = r { points.len() - 1 } else { 0 }).sum();
        assert_eq!(segments, samples.len() - 1);
    }

    #[test]
    fn highlighters_and_dots_are_one_piece() {
        let samples = [sample(0.0, 5.0), sample(1.0, 7.0)];
        assert_eq!(runs(StrokeKind::Highlighter, &samples), [Run::Line { width: 12.0, points: &samples[..] }]);
        assert_eq!(runs(StrokeKind::Pen, &samples[..1]), [Run::Dot(samples[0])]);
    }
}
