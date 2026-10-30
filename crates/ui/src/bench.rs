// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Timings of the hot paths, on a page full of handwriting. Not run by
//! default: `cargo test --release -p tratto-ui bench -- --ignored --nocapture`.

use std::time::Instant;

use eframe::egui::{self, Rect, Vec2, pos2, vec2};
use tratto_core::model::{Document, Point, Stroke, StrokeKind};
use tratto_core::storage::{NoteMeta, NoteWriter};
use tratto_core::stroke_builder::StrokeBuilder;

use crate::render::{self, Layer, StrokeCache};
use crate::view::{Layout, View};

/// A page of handwriting: lines of short pen squiggles, a few highlights.
fn written_page(doc: &mut Document, page: u32) {
    for line in 0..30 {
        let y = 60.0 + line as f32 * 34.0;
        for word in 0..60 {
            let x = 40.0 + word as f32 * 12.0;
            let pts: Vec<Point> = (0..30)
                .map(|i| {
                    let t = i as f32 / 29.0;
                    Point::new(x + t * 10.0, y + (t * 19.0 + word as f32).sin() * 6.0, 0.4 + 0.3 * t)
                })
                .collect();
            let id = doc.alloc_id();
            doc.strokes.push(Stroke::new(id, page, StrokeKind::Pen, [20, 20, 20, 255], 2.0, pts));
        }
        if line % 3 == 0 {
            let pts = (0..80).map(|i| Point::new(40.0 + i as f32 * 9.0, y, 1.0)).collect();
            let id = doc.alloc_id();
            doc.strokes.push(Stroke::new(id, page, StrokeKind::Highlighter, [255, 230, 0, 96], 14.0, pts));
        }
    }
}

fn ms(start: Instant, n: usize) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0 / n as f64
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn bench_paint_page() {
    crate::stroke_gpu::pretend_installed();
    let mut doc = Document::default();
    written_page(&mut doc, 0);
    let layout = Layout {
        page_w: doc.page_width,
        page_h: doc.page_height,
        count: 1,
        canvas: false,
        extent: egui::Vec2::ZERO,
        origin: egui::Vec2::ZERO,
    };
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 1000.0));
    let ctx = egui::Context::default();
    let mut cache = StrokeCache::default();
    let (mut paint, mut tess, mut vertices) = (0.0, 0.0, 0);
    let frames = 60;
    for frame in 0..frames + 5 {
        // Scrolling: a new position every frame.
        let view = View::new(rect, Vec2::new(0.0, 200.0 + frame as f32), 1.83, layout);
        let mut out = 0.0;
        let mut shapes = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            let t = Instant::now();
            render::paint_strokes(&painter, &view, Layer::Highlight, &doc.strokes, &mut cache, 1.0, None);
            render::paint_strokes(&painter, &view, Layer::Ink, &doc.strokes, &mut cache, 1.0, None);
            out = t.elapsed().as_secs_f64() * 1000.0;
        });
        let t = Instant::now();
        let prims = ctx.tessellate(std::mem::take(&mut shapes.shapes), 1.0);
        let tess_ms = t.elapsed().as_secs_f64() * 1000.0;
        shapes.textures_delta.clear();
        if frame >= 5 {
            paint += out;
            tess += tess_ms;
            vertices = prims
                .iter()
                .map(|p| match &p.primitive {
                    egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum::<usize>();
        }
    }
    println!(
        "paint page: app {:.2} ms + egui tessellation {:.2} ms per frame, {vertices} vertices through the CPU",
        paint / frames as f64,
        tess / frames as f64
    );
}

/// Paints `frames` frames of `doc`'s first page, the zoom of each frame from
/// `scale`; the milliseconds of `paint_strokes` per frame.
fn paint_frames(doc: &Document, frames: usize, scale: impl Fn(usize) -> f32) -> f64 {
    crate::stroke_gpu::pretend_installed();
    let layout = Layout {
        page_w: doc.page_width,
        page_h: doc.page_height,
        count: doc.page_count,
        canvas: false,
        extent: egui::Vec2::ZERO,
        origin: egui::Vec2::ZERO,
    };
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 1000.0));
    let ctx = egui::Context::default();
    let mut cache = StrokeCache::default();
    let mut total = 0.0;
    for frame in 0..frames + 5 {
        let view = View::new(rect, Vec2::new(0.0, 200.0 + frame as f32), scale(frame), layout);
        let mut out = 0.0;
        let mut shapes = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            let t = Instant::now();
            render::paint_strokes(&painter, &view, Layer::Highlight, &doc.strokes, &mut cache, 1.0, None);
            render::paint_strokes(&painter, &view, Layer::Ink, &doc.strokes, &mut cache, 1.0, None);
            out = t.elapsed().as_secs_f64() * 1000.0;
        });
        shapes.textures_delta.clear();
        if frame >= 5 {
            total += out;
        }
    }
    total / frames as f64
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn bench_large_note() {
    // Every page as full as the one of `bench_paint_page`; one is shown.
    let mut doc = Document::default();
    for page in 0..50 {
        written_page(&mut doc, page);
    }
    doc.page_count = 50;
    let ms = paint_frames(&doc, 60, |_| 1.83);
    println!("paint one page of a {}-stroke note: {ms:.3} ms per frame", doc.strokes.len());
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn bench_zoom() {
    // A pinch: a new zoom on every frame.
    let mut doc = Document::default();
    written_page(&mut doc, 0);
    let ms = paint_frames(&doc, 60, |frame| 1.5 + frame as f32 * 0.01);
    println!("zooming a full page: {ms:.3} ms per frame");
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn bench_live_stroke() {
    let layout = Layout {
        page_w: 794.0,
        page_h: 1123.0,
        count: 1,
        canvas: false,
        extent: egui::Vec2::ZERO,
        origin: egui::Vec2::ZERO,
    };
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 1000.0));
    let view = View::new(rect, Vec2::ZERO, 1.83, layout);
    let ctx = egui::Context::default();
    crate::stroke_gpu::pretend_installed();
    let mut live = render::LiveStroke::default();
    let mut builder = StrokeBuilder::new(0.6, 0.4, 6.0);
    builder.begin(Point::new(10.0, 10.0, 0.5));
    let samples = 1500;
    let (mut last, mut total) = (0.0, 0.0);
    for i in 1..=samples {
        let t = i as f32;
        builder.push(Point::new(10.0 + t * 0.5, 300.0 + (t * 0.05).sin() * 200.0, 0.5));
        let start = Instant::now();
        crate::test_frame(&ctx, egui::RawInput::default(), |ui| {
            let tip = builder.tip(true);
            let points = (builder.points(), &tip[..]);
            live.paint(ui.painter(), &view, 0, StrokeKind::Pen, [0, 0, 0, 255], 2.0, points, 1.0);
        });
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        if i > samples - 100 {
            last += ms;
        }
    }
    println!("live stroke of {samples} samples: {:.3} ms per frame at its end, {:.0} ms in all", last / 100.0, total);
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn bench_autosave() {
    let mut doc = Document::default();
    doc.page_count = 20;
    for page in 0..20 {
        written_page(&mut doc, page);
    }
    let path = std::env::temp_dir().join("tratto_bench_autosave.ink");
    let mut saver = crate::saver::Saver::new(egui::Context::default());
    saver.set_writer(NoteWriter::default());
    saver.save(path.clone(), NoteMeta::new("Bench", false), doc.clone());
    saver.wait();
    let n = 20;
    let (mut ui_thread, start) = (0.0, Instant::now());
    for i in 0..n {
        let id = doc.alloc_id();
        let pts = (0..30).map(|k| Point::new(k as f32, i as f32, 0.5)).collect();
        doc.strokes.push(Stroke::new(id, 3, StrokeKind::Pen, [0, 0, 0, 255], 2.0, pts));
        // What the UI thread does: copy the document and hand it over.
        let t = Instant::now();
        saver.save(path.clone(), NoteMeta::new("Bench", false), doc.clone());
        ui_thread += t.elapsed().as_secs_f64() * 1000.0;
        saver.wait();
    }
    println!(
        "autosave of {} strokes: {:.3} ms on the UI thread, {:.2} ms in the background",
        doc.strokes.len(),
        ui_thread / n as f64,
        ms(start, n) - ui_thread / n as f64
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("ink.bak"));
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn bench_page_changes() {
    crate::stroke_gpu::pretend_installed();
    let mut doc = Document::default();
    written_page(&mut doc, 0);
    let layout = Layout {
        page_w: doc.page_width,
        page_h: doc.page_height,
        count: 1,
        canvas: false,
        extent: egui::Vec2::ZERO,
        origin: egui::Vec2::ZERO,
    };
    let view =
        View::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 1000.0)), Vec2::new(0.0, 200.0), 1.83, layout);
    let ctx = egui::Context::default();
    let mut cache = StrokeCache::default();
    let frame = |doc: &Document, cache: &mut StrokeCache| {
        let t = Instant::now();
        crate::test_frame(&ctx, egui::RawInput::default(), |ui| {
            render::paint_strokes(ui.painter(), &view, Layer::Ink, &doc.strokes, cache, 1.0, None);
        });
        t.elapsed().as_secs_f64() * 1000.0
    };
    let first = frame(&doc, &mut cache);
    let n = 20;
    let (mut erase, mut write, mut undo) = (0.0, 0.0, 0.0);
    for i in 0..n {
        doc.strokes.remove(100 + i);
        erase += frame(&doc, &mut cache);
        let id = doc.alloc_id();
        let pts = (0..30).map(|k| Point::new(k as f32, i as f32, 0.5)).collect();
        doc.strokes.push(Stroke::new(id, 0, StrokeKind::Pen, [0, 0, 0, 255], 2.0, pts));
        write += frame(&doc, &mut cache);
        let undone = doc.strokes.pop();
        undo += frame(&doc, &mut cache);
        doc.strokes.extend(undone);
        frame(&doc, &mut cache);
    }
    println!(
        "page changes: first build {first:.1} ms, a stroke erased {:.2} ms, a stroke written {:.3} ms, \
         undone {:.3} ms",
        erase / n as f64,
        write / n as f64,
        undo / n as f64
    );
}
