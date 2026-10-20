// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Finding one's way in a note written on a PDF: the PDF's outline (its
//! table of contents) and the links on its pages.
//!
//! Both are read in the background (`tratto_core::pdf_pages`): the outline
//! once, the links of a page when it is first shown. They are kept while the
//! note is open; `clear` forgets them when another note is opened.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use tratto_core::pdf_pages::{self, Outline, OutlineEntry, PageLink, PageLinks, PdfFile};

/// Something read in the background.
pub enum Read<T> {
    /// Asked for, not there yet.
    Reading,
    Ready(T),
    /// Why it cannot be read.
    Failed(String),
}

pub struct PdfNav {
    /// `PdfFile::key` of the PDF everything below belongs to.
    key: Option<u64>,
    outline: Option<Read<Vec<OutlineEntry>>>,
    /// By PDF page.
    links: HashMap<u32, Read<Vec<PageLink>>>,
    outline_reply: Sender<Outline>,
    outlines: Receiver<Outline>,
    links_reply: Sender<PageLinks>,
    page_links: Receiver<PageLinks>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for PdfNav {
    fn default() -> Self {
        let (outline_reply, outlines) = mpsc::channel();
        let (links_reply, page_links) = mpsc::channel();
        Self {
            key: None,
            outline: None,
            links: HashMap::new(),
            outline_reply,
            outlines,
            links_reply,
            page_links,
            wake: None,
        }
    }
}

impl PdfNav {
    /// Takes in what has been read since the last frame. Call once per
    /// frame, before asking for anything.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        if self.wake.is_none() {
            let ctx = ctx.clone();
            self.wake = Some(Arc::new(move || ctx.request_repaint()));
        }
        for o in self.outlines.try_iter() {
            if Some(o.key) == self.key {
                self.outline = Some(match o.entries {
                    Ok(entries) => Read::Ready(entries),
                    Err(e) => Read::Failed(e.to_string()),
                });
            }
        }
        for l in self.page_links.try_iter() {
            if Some(l.key) == self.key {
                // Unreadable links are no reason to bother the reader.
                self.links.insert(l.page, Read::Ready(l.links.unwrap_or_default()));
            }
        }
    }

    /// Forgets everything (another note was opened).
    pub fn clear(&mut self) {
        self.key = None;
        self.outline = None;
        self.links.clear();
    }

    /// Starts over when `file` is not the PDF of what is kept.
    fn follow(&mut self, file: &PdfFile) {
        if self.key != Some(file.key) {
            self.clear();
            self.key = Some(file.key);
        }
    }

    fn wake(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.wake.clone().unwrap_or_else(|| Arc::new(|| {}))
    }

    /// The outline of `file`, asked for the first time it is wanted.
    pub fn outline(&mut self, file: &Arc<PdfFile>) -> &Read<Vec<OutlineEntry>> {
        self.follow(file);
        if self.outline.is_none() {
            pdf_pages::outline(file, self.outline_reply.clone(), self.wake());
        }
        self.outline.get_or_insert(Read::Reading)
    }

    /// The links on page `page` of `file`: none until they have been read
    /// (asked for the first time they are wanted).
    pub fn links(&mut self, file: &Arc<PdfFile>, page: u32) -> &[PageLink] {
        self.follow(file);
        if !self.links.contains_key(&page) {
            pdf_pages::links(file, page, self.links_reply.clone(), self.wake());
        }
        match self.links.entry(page).or_insert(Read::Reading) {
            Read::Ready(links) => links,
            _ => &[],
        }
    }
}
