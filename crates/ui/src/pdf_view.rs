// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The PDF pages under the ink of a note written on a PDF.
//!
//! Pages are rasterised in the background (`tratto_core::pdf_pages`):
//!
//! * a small image of every page shown (also the page previews), drawn first
//!   so a page is never empty;
//! * when a page is shown larger, tiles of `TILE` pixels at a zoom *level*
//!   (the page's width in pixels, exactly as shown, so a pixel of a tile is a
//!   pixel of the screen: scaled tiles break thin strokes up): only the tiles
//!   on screen are rendered, nearest to the centre first, a few at a time.
//!   Small tiles keep large (e.g. scanned) PDFs fast at any zoom: nothing off
//!   screen is rasterised and no single upload stalls a frame.
//!
//! While zooming, the level is the width at the zoom the page is made at
//! (see `zoom`), so it changes only when the zoom ends; until then the tiles
//! there are stretched, and those of the previous level stay until the new
//! ones have arrived. Finished images become textures a few per frame, as
//! they are (no per-pixel conversion).
//!
//! A texture is never released in the frame it was created or painted in:
//! egui-wgpu 0.29 destroys freed textures *before* submitting the frame, so
//! that would invalidate the frame ("Texture ... has been destroyed").
//! Replaced and evicted textures are retired and released two frames later.

use std::collections::{HashMap, VecDeque};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{self, Color32, ColorImage, Painter, Pos2, Rect, TextureHandle, TextureOptions, Vec2, pos2, vec2};
use tratto_core::model::Document;
use tratto_core::pdf_pages::{self, PdfError, PdfFile, RenderedPage};

/// Width of the whole-page stand-in (and page preview) images, in pixels.
const SMALL_PX: u32 = 512;
/// Side of a tile, in pixels.
const TILE: u32 = 512;
/// The largest zoom level (page width in pixels); past it tiles are stretched.
const MAX_LEVEL: f32 = 16384.0;
/// Renders asked for and not back yet, at most.
const MAX_IN_FLIGHT: usize = 6;
/// Finished images turned into textures per frame, at most.
const UPLOADS_PER_FRAME: usize = 6;
/// Seconds an image may stay unused before it is dropped (by time, not
/// frames: at a high frame rate tiles scrolled past would be rendered again
/// after a moment). One shown in the last frame is still on screen and never
/// goes, however long ago that frame was (the window only repaints on a
/// change).
const TTL_WHOLE: f64 = 30.0;
const TTL_TILE: f64 = 10.0;
/// Bytes of textures kept at most: past it, the images unused the longest
/// go first (never one shown in the last frame).
const TEXTURE_BUDGET: usize = 256 << 20;
/// Frames a retired texture is kept before it is released.
const RETIRE_FRAMES: u64 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Part {
    Whole,
    Tile { level: u32, tx: u32, ty: u32 },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Key {
    file: u64,
    page: u32,
    part: Part,
}

#[derive(Default)]
struct Entry {
    texture: Option<TextureHandle>,
    /// Size of the texture in pixels.
    size: [u32; 2],
    pending: bool,
    failed: bool,
    /// The frame and the time (egui's, seconds) it was last shown in.
    last_used: u64,
    used_at: f64,
}

impl Entry {
    fn bytes(&self) -> usize {
        if self.texture.is_some() { self.size[0] as usize * self.size[1] as usize * 4 } else { 0 }
    }
}

/// The zoom level of a page shown with tiles.
struct PageLevel {
    current: u32,
    /// Kept on screen until the tiles of `current` are all there.
    previous: Option<u32>,
}

pub struct PdfTextures {
    reply: Sender<RenderedPage>,
    results: Receiver<RenderedPage>,
    entries: HashMap<Key, Entry>,
    levels: HashMap<(u64, u32), PageLevel>,
    /// Finished renders waiting to become textures.
    backlog: VecDeque<RenderedPage>,
    in_flight: usize,
    /// Textures no longer shown, with the frame they were retired in.
    retired: Vec<(u64, TextureHandle)>,
    frame: u64,
    /// The time of this frame (egui's, seconds).
    now: f64,
    /// Why PDF pages cannot be shown (PDFium missing, unreadable file), for
    /// the app to tell the user: set once per note.
    pub error: Option<PdfError>,
    reported: bool,
}

impl Default for PdfTextures {
    fn default() -> Self {
        let (reply, results) = mpsc::channel();
        Self {
            reply,
            results,
            entries: HashMap::new(),
            levels: HashMap::new(),
            backlog: VecDeque::new(),
            in_flight: 0,
            retired: Vec::new(),
            frame: 0,
            now: 0.0,
            error: None,
            reported: false,
        }
    }
}

/// The zoom level for a page shown `shown` pixels wide: that width, whole
/// pixels.
fn level_for(shown: f32) -> u32 {
    shown.round().clamp(SMALL_PX as f32, MAX_LEVEL) as u32
}

/// The tiles (columns, rows) of a `width` x `height` pixel page that cover
/// `[x0, x1) x [y0, y1)` (pixels of the page), or `None` if that is empty.
fn tiles_over(width: u32, height: u32, x: [f32; 2], y: [f32; 2]) -> Option<(RangeInclusive<u32>, RangeInclusive<u32>)> {
    let (x0, x1) = (x[0].max(0.0), x[1].min(width as f32));
    let (y0, y1) = (y[0].max(0.0), y[1].min(height as f32));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let col = |v: f32| (v as u32) / TILE;
    Some((col(x0)..=col(x1.ceil() - 1.0), col(y0)..=col(y1.ceil() - 1.0)))
}

impl PdfTextures {
    /// Turns finished renders into textures (a few per frame), retires
    /// images unused for a while (or past the budget) and releases the ones
    /// retired long enough ago. Call at the start of every frame, before
    /// anything is painted.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        self.now = ctx.input(|i| i.time);
        let (frame, now) = (self.frame, self.now);
        self.retired.retain(|(at, _)| frame - at < RETIRE_FRAMES);

        for r in self.results.try_iter() {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.backlog.push_back(r);
        }
        for _ in 0..UPLOADS_PER_FRAME {
            let Some(r) = self.backlog.pop_front() else { break };
            let part = match r.region {
                None => Part::Whole,
                Some([x, y, ..]) => Part::Tile { level: r.requested_width, tx: x / TILE, ty: y / TILE },
            };
            // Not wanted any more (scrolled away, another note): dropped.
            let Some(e) = self.entries.get_mut(&Key { file: r.key, page: r.page, part }) else { continue };
            e.pending = false;
            match r.image {
                Ok((w, h, rgba)) => {
                    // Opaque RGBA is exactly egui's (premultiplied) pixels.
                    let image = ColorImage::from_rgba_premultiplied([w as usize, h as usize], &rgba);
                    let name = format!("pdf-{:x}-{}-{part:?}", r.key, r.page);
                    let new = ctx.load_texture(name, image, TextureOptions::LINEAR);
                    if let Some(old) = e.texture.replace(new) {
                        self.retired.push((frame, old));
                    }
                    e.size = [w, h];
                }
                Err(err) => {
                    e.failed = true;
                    if !std::mem::replace(&mut self.reported, true) {
                        self.error = Some(err);
                    }
                }
            }
        }
        if !self.backlog.is_empty() {
            ctx.request_repaint();
        }

        let mut unused: Vec<Key> = self
            .entries
            .iter()
            .filter(|(k, e)| {
                let ttl = if k.part == Part::Whole { TTL_WHOLE } else { TTL_TILE };
                let on_screen = e.last_used + 1 >= frame;
                !e.pending && !on_screen && now - e.used_at >= ttl
            })
            .map(|(k, _)| *k)
            .collect();
        // Over the budget (zoomed far into a large PDF): the least recently
        // shown go too.
        let mut bytes: usize = self.entries.values().map(Entry::bytes).sum();
        if bytes > TEXTURE_BUDGET {
            let mut old: Vec<(&Key, &Entry)> =
                self.entries.iter().filter(|(_, e)| e.bytes() > 0 && e.last_used + 1 < frame).collect();
            old.sort_by(|a, b| a.1.used_at.total_cmp(&b.1.used_at));
            for (k, e) in old {
                if bytes <= TEXTURE_BUDGET {
                    break;
                }
                bytes -= e.bytes();
                unused.push(*k);
            }
        }
        for k in unused {
            if let Some(t) = self.entries.remove(&k).and_then(|e| e.texture) {
                self.retired.push((frame, t));
            }
        }
    }

    /// Forgets every page (another note was opened). Their textures may have
    /// been painted this frame, so they are retired, not dropped.
    pub fn clear(&mut self) {
        let frame = self.frame;
        self.retired.extend(self.entries.drain().filter_map(|(_, e)| e.texture).map(|t| (frame, t)));
        self.levels.clear();
        self.backlog.clear();
        self.error = None;
        self.reported = false;
    }

    fn request(&mut self, ctx: &egui::Context, file: &Arc<PdfFile>, page: u32, width: u32, region: Option<[u32; 4]>) {
        self.in_flight += 1;
        let ctx = ctx.clone();
        pdf_pages::render(file, page, width, region, self.reply.clone(), Arc::new(move || ctx.request_repaint()));
    }

    /// The whole-page stand-in of a page, asked for if needed.
    fn whole(&mut self, ctx: &egui::Context, file: &Arc<PdfFile>, page: u32) -> Option<TextureHandle> {
        let key = Key { file: file.key, page, part: Part::Whole };
        let e = self.entries.entry(key).or_default();
        (e.last_used, e.used_at) = (self.frame, self.now);
        if e.texture.is_none() && !e.pending && !e.failed {
            e.pending = true;
            self.request(ctx, file, page, SMALL_PX, None);
        }
        self.entries[&key].texture.clone()
    }

    /// The level to draw a page at, given the one its size asks for, and
    /// the level before, kept until the tiles of the new one are there.
    fn level(&mut self, file: u64, page: u32, target: u32) -> (u32, Option<u32>) {
        let st = self.levels.entry((file, page)).or_insert(PageLevel { current: target, previous: None });
        if st.current != target {
            st.previous = Some(st.current);
            st.current = target;
        }
        (st.current, st.previous)
    }

    /// Paints the tiles of `level` over `visible` (screen) of a page drawn
    /// in `rect`; with `fetch`, asks for the missing ones. Returns whether
    /// every tile there is shown.
    #[allow(clippy::too_many_arguments)]
    fn paint_tiles(
        &mut self,
        painter: &Painter,
        file: &Arc<PdfFile>,
        page: u32,
        size: [f32; 2],
        rect: Rect,
        visible: Rect,
        level: u32,
        fetch: bool,
    ) -> bool {
        let (w, h) = (level, pdf_pages::page_height_px(size, level));
        // Level pixels per screen point: at the level shown, exactly the
        // screen's, with the page's corner on a pixel, so tiles are drawn
        // pixel for pixel (no resampling blur).
        let ppp = painter.ctx().pixels_per_point();
        let k = if (w as f32 - rect.width() * ppp).abs() <= 1.0 { ppp } else { w as f32 / rect.width() };
        let full = rect;
        let rect = Rect::from_min_size((rect.min.to_vec2() * ppp).round().to_pos2() / ppp, rect.size());
        let to_px = |a: f32, b: f32, o: f32| [(a - o) * k, (b - o) * k];
        let Some((cols, rows)) = tiles_over(
            w,
            h,
            to_px(visible.min.x, visible.max.x, rect.min.x),
            to_px(visible.min.y, visible.max.y, rect.min.y),
        ) else {
            return true;
        };
        let centre = visible.center();
        let mut missing = Vec::new();
        let mut complete = true;
        for ty in rows.clone() {
            for tx in cols.clone() {
                let key = Key { file: file.key, page, part: Part::Tile { level, tx, ty } };
                let e = match self.entries.get_mut(&key) {
                    Some(e) => e,
                    None if fetch => self.entries.entry(key).or_default(),
                    None => {
                        complete = false;
                        continue;
                    }
                };
                (e.last_used, e.used_at) = (self.frame, self.now);
                let origin = rect.min + vec2((tx * TILE) as f32, (ty * TILE) as f32) / k;
                match &e.texture {
                    Some(t) => {
                        let tile = Rect::from_min_size(origin, vec2(e.size[0] as f32, e.size[1] as f32) / k);
                        // The tiles on the page's edges end exactly on them:
                        // snapping the corner and rounding the level's height
                        // can leave them short of the edge (what is under the
                        // page shows) or past it (their last, half blank, row
                        // shows past the page's border).
                        let (mut shown, mut uv) = (tile, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
                        let at = |p: Pos2| ((p - tile.min) / tile.size()).clamp(Vec2::ZERO, Vec2::splat(1.0));
                        if tx == 0 {
                            (shown.min.x, uv.min.x) = (full.min.x, at(full.min).x);
                        }
                        if ty == 0 {
                            (shown.min.y, uv.min.y) = (full.min.y, at(full.min).y);
                        }
                        if (tx + 1) * TILE >= w {
                            (shown.max.x, uv.max.x) = (full.max.x, at(full.max).x);
                        }
                        if (ty + 1) * TILE >= h {
                            (shown.max.y, uv.max.y) = (full.max.y, at(full.max).y);
                        }
                        painter.image(t.id(), shown, uv, Color32::WHITE);
                    }
                    None => {
                        complete = false;
                        if fetch && !e.pending && !e.failed {
                            let mid = origin + vec2(TILE as f32, TILE as f32) / (2.0 * k);
                            missing.push((mid.distance_sq(centre), tx, ty));
                        }
                    }
                }
            }
        }
        // Nearest to the middle of the view first, a few at a time.
        missing.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, tx, ty) in missing {
            if self.in_flight >= MAX_IN_FLIGHT {
                break;
            }
            let key = Key { file: file.key, page, part: Part::Tile { level, tx, ty } };
            self.entries.get_mut(&key).expect("entry made above").pending = true;
            self.request(painter.ctx(), file, page, level, Some([tx * TILE, ty * TILE, TILE, TILE]));
        }
        complete
    }

    /// Paints the PDF page under note page `page` (if it has one) into
    /// `page_rect`, the page on screen. `sharp` false: page previews (the
    /// small image only). `made`: the zoom the page is made at over the
    /// zoom shown (see `zoom`), the tiles' level is for that zoom.
    pub fn paint(&mut self, painter: &Painter, doc: &Document, page: u32, page_rect: Rect, sharp: bool, made: f32) {
        let Some(pdf) = doc.pdf.as_ref() else { return };
        let (Some(file), Some(pdf_page)) = (pdf.file.as_ref(), doc.pdf_page(page)) else { return };
        let Some([x, y, w, h]) = pdf.placement(pdf_page, doc.page_width, doc.page_height) else { return };
        let Some(&size) = pdf.sizes.get(pdf_page as usize) else { return };
        let k = page_rect.width() / doc.page_width;
        let rect = Rect::from_min_size(page_rect.min + vec2(x, y) * k, vec2(w, h) * k);
        let visible = painter.clip_rect().intersect(rect);
        if !visible.is_positive() {
            return;
        }
        let ctx = painter.ctx().clone();
        if let Some(t) = self.whole(&ctx, file, pdf_page) {
            painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        let shown = rect.width() * ctx.pixels_per_point() * made;
        if !sharp || shown <= SMALL_PX as f32 * 1.05 {
            return;
        }
        let (current, previous) = self.level(file.key, pdf_page, level_for(shown));
        // The previous level covers the gaps until the new tiles are there.
        if let Some(prev) = previous {
            self.paint_tiles(painter, file, pdf_page, size, rect, visible, prev, false);
        }
        let complete = self.paint_tiles(painter, file, pdf_page, size, rect, visible, current, true);
        if complete && let Some(st) = self.levels.get_mut(&(file.key, pdf_page)) {
            st.previous = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{RawInput, TextureId};

    #[test]
    fn levels_are_the_width_shown() {
        assert_eq!(level_for(10.0), SMALL_PX);
        assert_eq!(level_for(1024.0), 1024);
        assert_eq!(level_for(1100.4), 1100);
        assert_eq!(level_for(100_000.0), 16384);
    }

    #[test]
    fn only_the_tiles_on_screen() {
        // A 2048 x 2896 page, 4 x 6 tiles.
        assert_eq!(tiles_over(2048, 2896, [0.0, 2048.0], [0.0, 2896.0]), Some((0..=3, 0..=5)));
        assert_eq!(tiles_over(2048, 2896, [600.0, 1100.0], [10.0, 20.0]), Some((1..=2, 0..=0)));
        // Exactly on a tile edge: not the next tile.
        assert_eq!(tiles_over(2048, 2896, [0.0, 512.0], [0.0, 512.0]), Some((0..=0, 0..=0)));
        // Clamped to the page; nothing when off it.
        assert_eq!(tiles_over(2048, 2896, [-50.0, 100.0], [2800.0, 4000.0]), Some((0..=0, 5..=5)));
        assert_eq!(tiles_over(2048, 2896, [3000.0, 4000.0], [0.0, 10.0]), None);
    }

    #[test]
    fn a_new_level_keeps_the_previous_until_its_tiles_are_there() {
        let mut tex = PdfTextures::default();
        assert_eq!(tex.level(1, 0, 1024), (1024, None));
        // The new level, the old one kept to cover the gaps.
        assert_eq!(tex.level(1, 0, 2048), (2048, Some(1024)));
        assert_eq!(tex.level(1, 0, 2048), (2048, Some(1024)));
    }

    /// A finished render of the stand-in of page 0.
    fn arrives(tex: &PdfTextures, key: u64) {
        let image = Ok((1, 1, vec![255; 4]));
        tex.reply.send(RenderedPage { key, page: 0, requested_width: SMALL_PX, region: None, image }).unwrap();
    }

    /// Runs one frame and returns the textures it frees.
    fn frame(ctx: &egui::Context, mut run: impl FnMut(&egui::Context)) -> Vec<TextureId> {
        crate::test_frame(ctx, RawInput::default(), |ui| run(ui.ctx()))
    }

    #[test]
    fn textures_outlive_the_frame_they_are_made_or_painted_in() {
        let ctx = egui::Context::default();
        let mut tex = PdfTextures::default();
        let key = Key { file: 7, page: 0, part: Part::Whole };
        tex.entries.insert(key, Entry { pending: true, ..Entry::default() });

        // Two renders of the same page in one frame: the first texture is
        // replaced right away, but not freed in that frame.
        arrives(&tex, 7);
        arrives(&tex, 7);
        let mut first = None;
        let freed = frame(&ctx, |ctx| {
            tex.begin_frame(ctx);
            first = tex.retired.first().map(|(_, t)| t.id());
        });
        let first = first.expect("the first texture was replaced");
        assert!(!freed.contains(&first));
        // Opaque RGBA went to egui as it is.
        assert_eq!(tex.entries[&key].size, [1, 1]);

        // Painted, then everything cleared in the same frame (another note
        // opened): nothing is freed in that frame either.
        let mut painted = None;
        let freed = frame(&ctx, |ctx| {
            tex.begin_frame(ctx);
            let t = tex.entries[&key].texture.clone().unwrap();
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0));
            ctx.layer_painter(egui::LayerId::background()).image(t.id(), rect, rect, Color32::WHITE);
            painted = Some(t.id());
            tex.clear();
        });
        let painted = painted.unwrap();
        assert!(!freed.contains(&painted) && !freed.contains(&first));

        // A couple of frames later both are released.
        let mut freed = Vec::new();
        for _ in 0..RETIRE_FRAMES + 1 {
            freed.extend(frame(&ctx, |ctx| tex.begin_frame(ctx)));
        }
        assert!(freed.contains(&first) && freed.contains(&painted), "{freed:?}");
        assert!(tex.retired.is_empty());
    }

    #[test]
    fn over_the_budget_the_least_recently_shown_go() {
        let ctx = egui::Context::default();
        let mut tex = PdfTextures { frame: 10, ..PdfTextures::default() };
        // Three tiles of 144 MB each (as far as the budget can tell).
        let texture = ctx.load_texture("t", ColorImage::filled([1, 1], Color32::WHITE), TextureOptions::LINEAR);
        let size = [6000, 6000];
        for (tx, last_used, used_at) in [(0, 1, 1.0), (1, 2, 2.0), (2, 10, 3.0)] {
            let key = Key { file: 1, page: 0, part: Part::Tile { level: 1024, tx, ty: 0 } };
            let entry = Entry { texture: Some(texture.clone()), size, last_used, used_at, ..Entry::default() };
            tex.entries.insert(key, entry);
        }
        let at = RawInput { time: Some(5.0), ..RawInput::default() };
        crate::test_frame(&ctx, at, |ui| tex.begin_frame(ui.ctx()));
        // The two oldest go; the one shown in the last frame stays.
        let left: Vec<u32> = tex
            .entries
            .keys()
            .map(|k| match k.part {
                Part::Tile { tx, .. } => tx,
                Part::Whole => unreachable!(),
            })
            .collect();
        assert_eq!(left, [2]);
        assert_eq!(tex.retired.len(), 2);
    }

    #[test]
    fn what_is_on_screen_stays_after_a_pause() {
        let ctx = egui::Context::default();
        let mut tex = PdfTextures { frame: 10, ..PdfTextures::default() };
        let texture = ctx.load_texture("t", ColorImage::filled([1, 1], Color32::WHITE), TextureOptions::LINEAR);
        // Shown in the last frame (10), and in an older one (9).
        for (tx, last_used) in [(0, 10), (1, 9)] {
            let key = Key { file: 1, page: 0, part: Part::Tile { level: 1024, tx, ty: 0 } };
            let entry = Entry { texture: Some(texture.clone()), size: [1, 1], last_used, ..Entry::default() };
            tex.entries.insert(key, entry);
        }
        // The next frame comes long after.
        let at = RawInput { time: Some(TTL_WHOLE * 10.0), ..RawInput::default() };
        crate::test_frame(&ctx, at, |ui| tex.begin_frame(ui.ctx()));
        let left: Vec<u32> = tex
            .entries
            .keys()
            .map(|k| match k.part {
                Part::Tile { tx, .. } => tx,
                Part::Whole => unreachable!(),
            })
            .collect();
        assert_eq!(left, [0], "the tile on screen stays, the one scrolled past goes");
    }

    #[test]
    fn uploads_are_spread_over_frames() {
        let ctx = egui::Context::default();
        let mut tex = PdfTextures::default();
        for tx in 0..10 {
            let part = Part::Tile { level: 1024, tx, ty: 0 };
            tex.entries.insert(Key { file: 3, page: 0, part }, Entry { pending: true, ..Entry::default() });
            let image = Ok((1, 1, vec![255; 4]));
            let region = Some([tx * TILE, 0, TILE, TILE]);
            tex.reply.send(RenderedPage { key: 3, page: 0, requested_width: 1024, region, image }).unwrap();
        }
        let shown = |tex: &PdfTextures| tex.entries.values().filter(|e| e.texture.is_some()).count();
        crate::test_frame(&ctx, RawInput::default(), |ui| tex.begin_frame(ui.ctx()));
        assert_eq!(shown(&tex), UPLOADS_PER_FRAME);
        crate::test_frame(&ctx, RawInput::default(), |ui| tex.begin_frame(ui.ctx()));
        assert_eq!(shown(&tex), 10);
    }
}
