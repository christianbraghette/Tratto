// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! The PDF pages under the ink of a note written on a PDF.
//!
//! Pages are rasterised in the background (`inknote_core::pdf_pages`) at the
//! size they are shown and kept as textures. Until a sharp one arrives (after
//! a zoom, or on a page scrolled into view), a small version stands in, so a
//! page is never shown empty for long. Textures not used for a while are
//! dropped.
//!
//! A texture is never released in the frame it was created or painted in:
//! egui-wgpu 0.29 destroys freed textures *before* submitting the frame, so
//! that would invalidate the frame ("Texture ... has been destroyed").
//! Replaced and evicted textures are retired and released two frames later.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, pos2, Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions};
use inknote_core::model::Document;
use inknote_core::pdf_pages::{self, PdfError, PdfFile, RenderedPage};

/// Width of the stand-in (and page preview) images, in pixels. Sharp
/// images are always wider, which is how a finished render is told apart.
const SMALL_PX: u32 = 256;
/// Sharp images are never wider than this (memory: 4096 x 5800 x 4 bytes is
/// about 95 MB for an A4 page).
const MAX_PX: u32 = 4096;
/// Frames a texture may stay unused before it is dropped.
const TTL_FRAMES: u64 = 300;
/// Frames a retired texture is kept before it is released.
const RETIRE_FRAMES: u64 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Size {
    /// For the canvas, as wide as shown.
    Sharp,
    /// Stand-in and page previews.
    Small,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    file: u64,
    page: u32,
    size: Size,
}

#[derive(Default)]
struct Entry {
    texture: Option<TextureHandle>,
    /// Width the texture was rendered for.
    width: u32,
    /// Width of a render on its way.
    pending: Option<u32>,
    failed: bool,
    last_used: u64,
}

pub struct PdfTextures {
    reply: Sender<RenderedPage>,
    results: Receiver<RenderedPage>,
    entries: HashMap<Key, Entry>,
    /// Textures no longer shown, with the frame they were retired in.
    retired: Vec<(u64, TextureHandle)>,
    frame: u64,
    /// Why PDF pages cannot be shown (PDFium missing, unreadable file), for
    /// the app to tell the user: set once per note.
    pub error: Option<PdfError>,
    reported: bool,
}

impl Default for PdfTextures {
    fn default() -> Self {
        let (reply, results) = mpsc::channel();
        Self { reply, results, entries: HashMap::new(), retired: Vec::new(), frame: 0, error: None, reported: false }
    }
}

/// The width to render for a page shown `shown` pixels wide: a little more,
/// in steps, so small zoom changes reuse the image.
fn target_width(shown: f32) -> u32 {
    let w = (shown * 1.1).ceil() as u32;
    (w.div_ceil(256) * 256).clamp(2 * SMALL_PX, MAX_PX)
}

impl PdfTextures {
    /// Takes the finished renders, retires textures unused for a while and
    /// releases the ones retired long enough ago. Call at the start of every
    /// frame, before anything is painted.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        let frame = self.frame;
        self.retired.retain(|(at, _)| frame - at < RETIRE_FRAMES);
        for r in self.results.try_iter() {
            let size = if r.requested_width <= SMALL_PX { Size::Small } else { Size::Sharp };
            let Some(e) = self.entries.get_mut(&Key { file: r.key, page: r.page, size }) else { continue };
            // A newer size is on its way (zooming): skip this one, unless
            // there is nothing to show yet.
            let stale = e.pending.is_some_and(|w| w != r.requested_width);
            if stale && e.texture.is_some() {
                continue;
            }
            if e.pending == Some(r.requested_width) {
                e.pending = None;
            }
            match r.image {
                Ok((w, h, rgba)) => {
                    let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    let name = format!("pdf-{:x}-{}-{w}", r.key, r.page);
                    let new = ctx.load_texture(name, image, TextureOptions::LINEAR);
                    if let Some(old) = e.texture.replace(new) {
                        self.retired.push((frame, old));
                    }
                    e.width = r.requested_width;
                }
                Err(err) => {
                    e.failed = true;
                    if !std::mem::replace(&mut self.reported, true) {
                        self.error = Some(err);
                    }
                }
            }
        }
        let unused: Vec<Key> = self
            .entries
            .iter()
            .filter(|(_, e)| e.pending.is_none() && frame - e.last_used >= TTL_FRAMES)
            .map(|(k, _)| *k)
            .collect();
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
        self.error = None;
        self.reported = false;
    }

    /// The texture of a page at `size`, asking for a new render when the
    /// one there is too small (or much too large) for `want` pixels.
    fn texture(&mut self, ctx: &egui::Context, file: &Arc<PdfFile>, page: u32, size: Size, want: u32) -> Option<TextureHandle> {
        let key = Key { file: file.key, page, size };
        let e = self.entries.entry(key).or_default();
        e.last_used = self.frame;
        let outdated = e.texture.is_none() || e.width < want * 9 / 10 || e.width > want * 2;
        if outdated && !e.failed && e.pending != Some(want) {
            e.pending = Some(want);
            let ctx = ctx.clone();
            pdf_pages::render(file, page, want, self.reply.clone(), Arc::new(move || ctx.request_repaint()));
        }
        e.texture.clone()
    }

    /// Paints the PDF page under note page `page` (if it has one) into
    /// `page_rect`, the page on screen. `sharp` false: page previews (small
    /// images only).
    pub fn paint(&mut self, painter: &Painter, doc: &Document, page: u32, page_rect: Rect, sharp: bool) {
        let Some(pdf) = doc.pdf.as_ref() else { return };
        let (Some(file), Some(pdf_page)) = (pdf.file.as_ref(), doc.pdf_page(page)) else { return };
        let Some([x, y, w, h]) = pdf.placement(pdf_page, doc.page_width, doc.page_height) else { return };
        let k = page_rect.width() / doc.page_width;
        let rect = Rect::from_min_size(page_rect.min + egui::vec2(x, y) * k, egui::vec2(w, h) * k);
        if !painter.clip_rect().intersects(rect) {
            return;
        }
        let ctx = painter.ctx().clone();
        let small = self.texture(&ctx, file, pdf_page, Size::Small, SMALL_PX);
        let shown = if sharp {
            let want = target_width(rect.width() * ctx.pixels_per_point());
            self.texture(&ctx, file, pdf_page, Size::Sharp, want).or(small)
        } else {
            small
        };
        if let Some(t) = shown {
            painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, RawInput, TextureId};

    /// A finished render of page 0 at the stand-in size.
    fn arrives(tex: &PdfTextures, key: u64) {
        let image = Ok((1, 1, vec![255; 4]));
        tex.reply.send(RenderedPage { key, page: 0, requested_width: SMALL_PX, image }).unwrap();
    }

    /// Runs one frame and returns the textures it frees.
    fn frame(ctx: &egui::Context, run: impl FnMut(&egui::Context)) -> Vec<TextureId> {
        ctx.run(RawInput::default(), run).textures_delta.free
    }

    #[test]
    fn textures_outlive_the_frame_they_are_made_or_painted_in() {
        let ctx = egui::Context::default();
        let mut tex = PdfTextures::default();
        let key = Key { file: 7, page: 0, size: Size::Small };
        tex.entries.insert(key, Entry { pending: Some(SMALL_PX), ..Entry::default() });

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

        // Painted, then everything cleared in the same frame (another note
        // opened): nothing is freed in that frame either.
        let mut painted = None;
        let freed = frame(&ctx, |ctx| {
            tex.begin_frame(ctx);
            let t = tex.entries[&key].texture.clone().unwrap();
            let rect = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
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
    fn render_widths_come_in_steps() {
        assert_eq!(target_width(10.0), 2 * SMALL_PX);
        assert_eq!(target_width(1000.0), 1280);
        assert_eq!(target_width(1010.0), 1280); // a small zoom reuses it
        assert_eq!(target_width(20_000.0), MAX_PX);
    }
}
