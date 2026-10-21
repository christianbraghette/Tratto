// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Markdown in text boxes: CommonMark (`pulldown-cmark`) with tables, task
//! lists and ~~strikethrough~~, and two departures that suit notes: every
//! line break is kept (as on GitHub comments or in Obsidian), and so are the
//! blank lines typed between blocks.
//!
//! [`layout`] lays the Markdown out without its marks, as the screen and the
//! PDF show it: runs of text in one style, each with where it comes from in
//! the Markdown, and the marks drawn with them (bullets' task boxes, quote
//! bars, rules, code blocks' and tables' lines). [`highlight`] styles the
//! Markdown itself with its marks dimmed, for the editor.

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontFamily, FontId, Galley, Stroke};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use tratto_core::model::{Document, RunStyle, TextBox, TextLayout, TextMark, TextRun};

use crate::theme;

/// Font size of the headings `#` to `###` (and smaller ones), times the box's.
const HEADING: [f32; 3] = [1.6, 1.35, 1.15];
/// Room for a list's bullet, per level, and a quote's indent (times the font size).
const BULLET: f32 = 1.3;
const QUOTE: f32 = 0.9;
/// Padding inside code blocks and table cells (times the font size).
const PAD: f32 = 0.35;

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// Text in one style, and where it is in the Markdown (none for what is not
/// there as it is shown, such as a line break).
#[derive(Clone, Debug)]
struct Piece {
    text: String,
    source: Option<usize>,
    style: RunStyle,
}

#[derive(Clone, Debug, PartialEq)]
enum Marker {
    Bullet,
    /// `1.`, and where the item starts in the Markdown.
    Number(String, usize),
    Check(bool),
}

#[derive(Clone, Debug)]
enum Kind {
    Text,
    Heading(usize),
    Code,
    Rule,
    /// Rows of cells; the first row is the header.
    Table(Vec<Vec<Vec<Piece>>>),
}

/// A block of the Markdown, ready to lay out.
#[derive(Clone, Debug)]
struct Block {
    kind: Kind,
    pieces: Vec<Piece>,
    /// Quotes and lists it is in.
    quotes: usize,
    lists: usize,
    /// A list item's first block: its bullet.
    marker: Option<Marker>,
    /// Where it is in the Markdown.
    start: usize,
    end: usize,
}

/// Bold, italic... as deep as the marks around the text go.
#[derive(Default)]
struct Nesting {
    bold: u32,
    italic: u32,
    strike: u32,
    link: u32,
    code_block: bool,
}

impl Nesting {
    fn style(&self) -> RunStyle {
        RunStyle {
            bold: self.bold > 0,
            italic: self.italic > 0,
            code: self.code_block,
            strike: self.strike > 0,
            underline: self.link > 0,
            code_span: false,
        }
    }
}

/// Where `shown` (the text of an event over `range`) is in the Markdown.
fn source_of(text: &str, range: &Range<usize>, shown: &str) -> Option<usize> {
    let span = text.get(range.clone())?;
    if span == shown { Some(range.start) } else { span.find(shown).map(|k| range.start + k) }
}

/// The blocks of the Markdown `text`.
fn blocks(text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut cur: Option<Block> = None;
    let mut nest = Nesting::default();
    let (mut quotes, mut lists): (usize, Vec<Option<u64>>) = (0, Vec::new());
    let mut marker: Option<Marker> = None;
    let mut table: Option<(Vec<Vec<Vec<Piece>>>, usize)> = None;
    let mut in_head = false;

    fn close(cur: &mut Option<Block>, out: &mut Vec<Block>) {
        if let Some(mut b) = cur.take() {
            // A code block's text ends with its last line's break.
            if let (Kind::Code, Some(last)) = (&b.kind, b.pieces.last_mut())
                && last.text.ends_with('\n')
            {
                last.text.pop();
            }
            out.push(b);
        }
    }
    let new = |kind: Kind, start: usize, quotes: usize, lists: usize, marker: &mut Option<Marker>| Block {
        kind,
        pieces: Vec::new(),
        quotes,
        lists,
        marker: marker.take(),
        start,
        end: start,
    };

    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        let mut shown: Option<(String, RunStyle)> = None;
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    close(&mut cur, &mut out);
                    cur = Some(new(Kind::Text, range.start, quotes, lists.len(), &mut marker));
                }
                Tag::Heading { level, .. } => {
                    close(&mut cur, &mut out);
                    cur = Some(new(Kind::Heading(level as usize), range.start, quotes, lists.len(), &mut marker));
                }
                Tag::CodeBlock(_) => {
                    close(&mut cur, &mut out);
                    cur = Some(new(Kind::Code, range.start, quotes, lists.len(), &mut marker));
                    nest.code_block = true;
                }
                Tag::BlockQuote(_) => {
                    close(&mut cur, &mut out);
                    quotes += 1;
                }
                Tag::List(first) => {
                    close(&mut cur, &mut out);
                    lists.push(first);
                }
                Tag::Item => {
                    close(&mut cur, &mut out);
                    marker = Some(match lists.last_mut() {
                        Some(Some(n)) => {
                            let m = Marker::Number(format!("{n}."), range.start);
                            *n += 1;
                            m
                        }
                        _ => Marker::Bullet,
                    });
                }
                Tag::Table(_) => {
                    close(&mut cur, &mut out);
                    table = Some((Vec::new(), range.start));
                }
                Tag::TableHead | Tag::TableRow => {
                    in_head = matches!(tag, Tag::TableHead);
                    if let Some((rows, _)) = &mut table {
                        rows.push(Vec::new());
                    }
                }
                Tag::TableCell => {
                    if let Some(row) = table.as_mut().and_then(|(rows, _)| rows.last_mut()) {
                        row.push(Vec::new());
                    }
                }
                Tag::Emphasis => nest.italic += 1,
                Tag::Strong => nest.bold += 1,
                Tag::Strikethrough => nest.strike += 1,
                Tag::Link { .. } => nest.link += 1,
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock => {
                    if let Some(b) = &mut cur {
                        b.end = b.end.max(range.end);
                    }
                    close(&mut cur, &mut out);
                    nest.code_block = false;
                }
                TagEnd::BlockQuote(_) => {
                    close(&mut cur, &mut out);
                    quotes = quotes.saturating_sub(1);
                }
                TagEnd::List(_) => {
                    close(&mut cur, &mut out);
                    lists.pop();
                }
                TagEnd::Item => {
                    close(&mut cur, &mut out);
                    marker = None;
                }
                TagEnd::Table => {
                    if let Some((rows, start)) = table.take() {
                        let mut b = new(Kind::Table(rows), start, quotes, lists.len(), &mut marker);
                        b.end = range.end;
                        out.push(b);
                    }
                }
                TagEnd::TableHead => in_head = false,
                TagEnd::Emphasis => nest.italic = nest.italic.saturating_sub(1),
                TagEnd::Strong => nest.bold = nest.bold.saturating_sub(1),
                TagEnd::Strikethrough => nest.strike = nest.strike.saturating_sub(1),
                TagEnd::Link => nest.link = nest.link.saturating_sub(1),
                _ => {}
            },
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) | Event::InlineMath(t) | Event::DisplayMath(t) => {
                shown = Some((t.into_string(), nest.style()));
            }
            Event::FootnoteReference(t) => shown = Some((format!("[{t}]"), nest.style())),
            Event::Code(t) => shown = Some((t.into_string(), RunStyle { code: true, code_span: true, ..nest.style() })),
            // Every line break is kept.
            Event::SoftBreak | Event::HardBreak => shown = Some(("\n".into(), nest.style())),
            Event::Rule => {
                close(&mut cur, &mut out);
                let mut b = new(Kind::Rule, range.start, quotes, lists.len(), &mut marker);
                b.end = range.end;
                out.push(b);
            }
            Event::TaskListMarker(checked) => match &mut cur {
                Some(b) if b.pieces.is_empty() => b.marker = Some(Marker::Check(checked)),
                _ => marker = Some(Marker::Check(checked)),
            },
        }
        let Some((shown, mut style)) = shown else { continue };
        let source = if shown == "\n" { None } else { source_of(text, &range, &shown) };
        if let Some(cell) = table.as_mut().and_then(|(rows, _)| rows.last_mut()).and_then(|row| row.last_mut()) {
            style.bold |= in_head;
            cell.push(Piece { text: shown, source, style });
            continue;
        }
        let b = cur.get_or_insert_with(|| new(Kind::Text, range.start, quotes, lists.len(), &mut marker));
        b.end = b.end.max(range.end);
        b.pieces.push(Piece { text: shown, source, style });
    }
    close(&mut cur, &mut out);
    out
}

/// The font of text in `style` at `size`. Italics are Inter's own; only code
/// (which has none) is slanted by egui, see [`slanted`].
pub fn font(style: RunStyle, size: f32) -> FontId {
    let family = if style.code {
        FontFamily::Monospace
    } else if style.italic {
        theme::italic_family(style.bold)
    } else if style.bold {
        theme::strong_family()
    } else {
        FontFamily::Proportional
    };
    FontId::new(size, family)
}

/// Text in `style` is slanted by egui: italic code (the monospace font has
/// no italics of its own).
pub fn slanted(style: RunStyle) -> bool {
    style.italic && style.code
}

fn format(style: RunStyle, size: f32, color: Color32) -> TextFormat {
    let line = Stroke::new((size * 0.06).max(1.0), color);
    TextFormat {
        font_id: font(style, size),
        color,
        italics: slanted(style),
        strikethrough: if style.strike { line } else { Stroke::NONE },
        underline: if style.underline { line } else { Stroke::NONE },
        background: if style.code_span { Color32::from_black_alpha(15) } else { Color32::TRANSPARENT },
        ..Default::default()
    }
}

/// Font size of a heading of `level`, times the box's.
fn heading_scale(level: usize) -> f32 {
    HEADING.get(level.saturating_sub(1)).copied().unwrap_or(1.0)
}

/// A character of a laid out job: its piece, and where it is in the Markdown.
type JobChar = (char, usize, Option<usize>);

/// Lays out `pieces` wrapped at `wrap`, in their styles at `size`; also each
/// character of the job with its piece and its place in the Markdown.
fn lay_out_pieces(ctx: &egui::Context, pieces: &[Piece], size: f32, wrap: f32) -> (Arc<Galley>, Vec<JobChar>) {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    let mut chars = Vec::new();
    for (k, piece) in pieces.iter().enumerate() {
        job.append(&piece.text, 0.0, format(piece.style, size, Color32::BLACK));
        chars.extend(piece.text.char_indices().map(|(b, c)| (c, k, piece.source.map(|s| s + b))));
    }
    (ctx.fonts_mut(|f| f.layout_job(job)), chars)
}

/// The runs of `galley` (made by [`lay_out_pieces`]) placed at `left`, `top`:
/// one per piece and row. A row may leave out the space it was wrapped at,
/// so glyphs are matched to their characters. The rows as shown go to
/// `rows`, the first after `prefix`.
#[allow(clippy::too_many_arguments)]
fn runs_of(
    out: &mut TextLayout,
    rows: &mut Vec<String>,
    galley: &Galley,
    chars: &[JobChar],
    pieces: &[Piece],
    size: f32,
    (left, top): (f32, f32),
    prefix: &str,
) {
    let mut next = 0;
    for (r, placed) in galley.rows.iter().enumerate() {
        let mut shown = if r == 0 { prefix.to_owned() } else { " ".repeat(prefix.chars().count()) };
        let mut run: Option<(TextRun, usize)> = None;
        for g in &placed.row.glyphs {
            while next < chars.len() && chars[next].0 != g.chr {
                next += 1;
            }
            let Some(&(_, piece, source)) = chars.get(next) else { break };
            next += 1;
            shown.push(g.chr);
            let x = left + placed.pos.x + g.pos.x;
            match &mut run {
                Some((run, k)) if *k == piece => {
                    run.text.push(g.chr);
                    run.width = x + g.advance_width - run.x;
                }
                _ => {
                    if let Some((done, _)) = run.take() {
                        out.runs.push(done);
                    }
                    let new = TextRun {
                        text: g.chr.to_string(),
                        source,
                        x,
                        baseline: top + placed.pos.y + g.pos.y,
                        top: top + placed.pos.y,
                        line_h: placed.rect().height(),
                        width: g.advance_width,
                        size,
                        ascent: g.font_ascent,
                        style: pieces[piece].style,
                    };
                    run = Some((new, piece));
                }
            }
        }
        if let Some((done, _)) = run {
            out.runs.push(done);
        }
        rows.push(shown);
    }
}

/// Lays out the Markdown `text` of a box `width` wide at font size `size`
/// (document units), as it is shown: the marks gone, in the styles they
/// make. Also the text as shown, line by line (for what draws it roughly).
pub fn layout(ctx: &egui::Context, text: &str, size: f32, width: f32) -> (TextLayout, Vec<String>) {
    let mut out = TextLayout::default();
    let mut rows = Vec::new();
    let plain = ctx.fonts_mut(|f| f.layout_no_wrap("Hg".into(), font(RunStyle::default(), size), Color32::BLACK));
    let line_h = plain.rect.height();
    let (plain_baseline, plain_ascent) = plain
        .rows
        .first()
        .and_then(|r| r.row.glyphs.first())
        .map_or((line_h * 0.78, line_h * 0.78), |g| (g.pos.y, g.font_ascent));
    let pad = size * PAD;
    let mut y = 0.0;
    // Where the text of the block before ends.
    let mut done = 0;
    for (n, block) in blocks(text).into_iter().enumerate() {
        // The blank lines typed before the block.
        let start = block.start.max(done);
        let breaks = text[done..start].matches('\n').count();
        let blank = if n == 0 { breaks } else { breaks.saturating_sub(1) };
        y += blank as f32 * line_h;
        rows.extend(std::iter::repeat_n(String::new(), blank));
        done = block.start + text[block.start..block.end.max(block.start)].trim_end().len();
        let top = y;

        let quote_x = block.quotes as f32 * QUOTE * size;
        let left = (quote_x + block.lists as f32 * BULLET * size).min(width * 0.6);
        let avail = (width - left).max(size * 2.0);
        match &block.kind {
            Kind::Rule => {
                let mid = y + line_h * 0.5;
                out.marks.push(TextMark::Line { x0: left, y0: mid, x1: width, y1: mid });
                rows.push(String::new());
                y += line_h;
            }
            Kind::Text | Kind::Heading(_) => {
                let (sz, bold) = match block.kind {
                    Kind::Heading(level) => (size * heading_scale(level), true),
                    _ => (size, false),
                };
                let pieces: Vec<Piece> = block
                    .pieces
                    .iter()
                    .map(|p| Piece { style: RunStyle { bold: p.style.bold || bold, ..p.style }, ..p.clone() })
                    .collect();
                let (galley, chars) = lay_out_pieces(ctx, &pieces, sz, avail);
                let prefix =
                    marker(&mut out, ctx, text, &block, &galley, (left, y), size, sz, (plain_baseline, plain_ascent));
                runs_of(&mut out, &mut rows, &galley, &chars, &pieces, sz, (left, y), &prefix);
                y += galley.rect.height().max(line_h * sz / size);
            }
            Kind::Code => {
                let (galley, chars) = lay_out_pieces(ctx, &block.pieces, size, avail - 2.0 * pad);
                let height = galley.rect.height().max(line_h) + 2.0 * pad;
                out.marks.push(TextMark::Shade { x: left, top: y, width: width - left, height });
                let prefix = marker(
                    &mut out,
                    ctx,
                    text,
                    &block,
                    &galley,
                    (left, y + pad),
                    size,
                    size,
                    (plain_baseline, plain_ascent),
                );
                runs_of(&mut out, &mut rows, &galley, &chars, &block.pieces, size, (left + pad, y + pad), &prefix);
                y += height;
            }
            Kind::Table(table) => {
                let columns = table.iter().map(Vec::len).max().unwrap_or(0).max(1);
                let col_w = avail / columns as f32;
                let first_row = rows.len();
                for (r, row) in table.iter().enumerate() {
                    let mut height = line_h + 2.0 * pad;
                    let mut cells = Vec::new();
                    for (c, cell) in row.iter().enumerate() {
                        let (galley, chars) = lay_out_pieces(ctx, cell, size, col_w - 2.0 * pad);
                        height = height.max(galley.rect.height() + 2.0 * pad);
                        let x = left + c as f32 * col_w + pad;
                        let mut cell_rows = Vec::new();
                        runs_of(&mut out, &mut cell_rows, &galley, &chars, cell, size, (x, y + pad), "");
                        cells.push(cell_rows.join(" "));
                    }
                    rows.push(cells.join(" | "));
                    // The line under the header is stronger: drawn twice.
                    let bottom = y + height;
                    out.marks.push(TextMark::Line { x0: left, y0: bottom, x1: left + avail, y1: bottom });
                    if r == 0 {
                        out.marks.push(TextMark::Line { x0: left, y0: bottom, x1: left + avail, y1: bottom });
                    }
                    y = bottom;
                }
                out.marks.push(TextMark::Line { x0: left, y0: top, x1: left + avail, y1: top });
                for c in 0..=columns {
                    let x = left + c as f32 * col_w;
                    out.marks.push(TextMark::Line { x0: x, y0: top, x1: x, y1: y });
                }
                if rows.len() == first_row {
                    rows.push(String::new());
                }
            }
        }
        for k in 0..block.quotes {
            let x = (k as f32 * QUOTE + 0.15) * size;
            out.marks.push(TextMark::Quote { x, top, bottom: y });
        }
    }
    // Blank lines typed at the end.
    let trailing = text[done.min(text.len())..].matches('\n').count();
    y += trailing.saturating_sub(1) as f32 * line_h;
    rows.extend(std::iter::repeat_n(String::new(), trailing.saturating_sub(1)));
    out.height = y.max(line_h);
    (out, rows)
}

/// A list item's bullet, number or task box, left of the block's text at
/// `left`, `top` (`size` the box's font size, `sz` the block's); returns
/// what the first row as shown starts with. `text` is the Markdown.
#[allow(clippy::too_many_arguments)]
fn marker(
    out: &mut TextLayout,
    ctx: &egui::Context,
    text: &str,
    block: &Block,
    galley: &Galley,
    (left, top): (f32, f32),
    size: f32,
    sz: f32,
    (plain_baseline, plain_ascent): (f32, f32),
) -> String {
    let Some(marker) = &block.marker else { return String::new() };
    let first = galley.rows.first();
    let line_h = first.map_or(plain_baseline / 0.78, |r| r.rect().height());
    let (baseline, ascent) = first
        .and_then(|r| r.row.glyphs.first().map(|g| (r.pos.y + g.pos.y, g.font_ascent)))
        .unwrap_or((plain_baseline, plain_ascent));
    let x = left - BULLET * size;
    let mut bullet = |text: &str, source: Option<usize>| {
        let style = RunStyle::default();
        let width = ctx.fonts_mut(|f| f.layout_no_wrap(text.into(), font(style, sz), Color32::BLACK).rect.width());
        let (text, baseline, size) = (text.to_owned(), top + baseline, sz);
        out.runs.push(TextRun { text, source, x, baseline, top, line_h, width, size, ascent, style });
    };
    match marker {
        Marker::Bullet => {
            bullet("•", None);
            "• ".into()
        }
        Marker::Number(num, at) => {
            // Where the number is in the Markdown, if it is there as shown
            // (CommonMark numbers a list on from its first item).
            let rest = text.get(*at..).unwrap_or_default();
            let lead = rest.len() - rest.trim_start().len();
            bullet(num, rest.trim_start().starts_with(num.as_str()).then_some(at + lead));
            format!("{num} ")
        }
        Marker::Check(checked) => {
            let s = sz * 0.75;
            out.marks.push(TextMark::Check { x, top: top + (line_h - s) * 0.5, size: s, checked: *checked });
            "☐ ".into()
        }
    }
}

/// Lays out `t`'s Markdown into its layout, its shown lines and their
/// height (see `TextBox`).
pub fn lay_out_box(ctx: &egui::Context, t: &mut TextBox) {
    let (layout, rows) = layout(ctx, &t.text, t.size, t.width);
    t.line_h = layout.height / rows.len().max(1) as f32;
    t.lines = rows;
    t.layout = Some(Arc::new(layout));
}

/// Lays out the text boxes of a document that are not (just loaded: the
/// layouts are not saved). What is saved stays as it is. Needs the fonts:
/// call it during a frame.
pub fn lay_out_document(ctx: &egui::Context, doc: &mut Document) {
    for t in doc.texts.iter_mut().filter(|t| t.layout.is_none()) {
        let (layout, _) = layout(ctx, &t.text, t.size, t.width);
        t.layout = Some(Arc::new(layout));
    }
}

/// The Markdown `text` for the editor: in the styles it makes, its marks
/// dimmed. `size` is the font size, in points.
pub fn highlight(text: &str, size: f32, color: Color32) -> LayoutJob {
    /// How a byte of the Markdown is shown.
    #[derive(Clone, Copy, PartialEq)]
    struct Look {
        style: RunStyle,
        scale: f32,
        /// Text, not a mark.
        text: bool,
    }
    let mut looks = vec![Look { style: RunStyle::default(), scale: 1.0, text: false }; text.len()];
    let mut nest = Nesting::default();
    let mut heading = false;
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        let mut shown = None;
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = true;
                for look in &mut looks[range.clone()] {
                    look.scale = heading_scale(level as usize);
                }
            }
            Event::End(TagEnd::Heading(_)) => heading = false,
            Event::Start(Tag::CodeBlock(_)) => nest.code_block = true,
            Event::End(TagEnd::CodeBlock) => nest.code_block = false,
            Event::Start(Tag::Emphasis) => nest.italic += 1,
            Event::End(TagEnd::Emphasis) => nest.italic = nest.italic.saturating_sub(1),
            Event::Start(Tag::Strong) => nest.bold += 1,
            Event::End(TagEnd::Strong) => nest.bold = nest.bold.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => nest.strike += 1,
            Event::End(TagEnd::Strikethrough) => nest.strike = nest.strike.saturating_sub(1),
            Event::Start(Tag::Link { .. }) => nest.link += 1,
            Event::End(TagEnd::Link) => nest.link = nest.link.saturating_sub(1),
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => shown = Some((t, nest.style())),
            Event::Code(t) => shown = Some((t, RunStyle { code: true, code_span: true, ..nest.style() })),
            _ => {}
        }
        if let Some((t, mut style)) = shown
            && let Some(at) = source_of(text, &range, &t)
        {
            style.bold |= heading;
            for look in &mut looks[at..at + t.len()] {
                look.style = style;
                look.text = true;
            }
        }
    }

    let dim = color.gamma_multiply(0.4);
    let mut job = LayoutJob::default();
    let mut from = 0;
    let chars: Vec<usize> = text.char_indices().map(|(b, _)| b).chain([text.len()]).collect();
    for w in chars.windows(2) {
        let (at, next) = (w[0], w[1]);
        if next == text.len() || looks[next] != looks[at] {
            let look = looks[at];
            let ink = if look.text { color } else { dim };
            let style = if look.text { look.style } else { RunStyle::default() };
            job.append(&text[from..next], 0.0, format(style, size * look.scale, ink));
            from = next;
        }
    }
    job
}

// ---- Writing: what the editor's keys do to the Markdown

/// The Markdown after an edit, and the selection in it (byte offsets).
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub text: String,
    pub sel: Range<usize>,
}

/// Where the line holding byte `at` starts and ends.
fn line_at(text: &str, at: usize) -> Range<usize> {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    start..end
}

/// A list item's or a quote's start: its indent (spaces), its marks (`- `,
/// `1. `, `- [x] `, `> `), and the marks the next item starts with.
fn item_marks(line: &str) -> Option<(usize, &str, String)> {
    let spaces = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[spaces..];
    if rest.starts_with("> ") {
        return Some((spaces, &rest[..2], "> ".into()));
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(item) = rest.strip_prefix(bullet) {
            if ["[ ] ", "[x] ", "[X] "].iter().any(|task| item.starts_with(task)) {
                return Some((spaces, &rest[..6], format!("{bullet}[ ] ")));
            }
            return Some((spaces, &rest[..2], bullet.into()));
        }
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let delim = rest.get(digits..digits + 2)?;
    if (1..=9).contains(&digits) && (delim == ". " || delim == ") ") {
        let n: u64 = rest[..digits].parse().ok()?;
        return Some((spaces, &rest[..digits + 2], format!("{}{delim}", n + 1)));
    }
    None
}

/// Enter in a list or a quote: the new line starts as the next item (a
/// task unchecked, the next number). Enter on an item left empty ends it: a
/// nested one goes up a level, others lose their marks. `None`: a plain
/// line break.
pub fn enter(text: &str, sel: Range<usize>) -> Option<Edit> {
    if !sel.is_empty() {
        return None;
    }
    let at = sel.start;
    let line = line_at(text, at);
    let (spaces, marks, next) = item_marks(&text[line.clone()])?;
    let content = line.start + spaces + marks.len();
    if at < content {
        return None;
    }
    if text[content..line.end].trim().is_empty() {
        if spaces > 0 {
            return indent(text, sel, true);
        }
        let text = format!("{}{}", &text[..line.start], &text[line.end..]);
        return Some(Edit { text, sel: line.start..line.start });
    }
    let insert = format!("\n{}{next}", " ".repeat(spaces));
    let cursor = at + insert.len();
    Some(Edit { text: format!("{}{insert}{}", &text[..at], &text[at..]), sel: cursor..cursor })
}

/// Tab (`outdent`: Shift+Tab) on list items: the items of the selected lines
/// go a level in (out). A level is as deep as the text of the item above
/// (CommonMark: two spaces under `- `, three under `1. `). `None`: the first
/// selected line is no item, or cannot go further.
pub fn indent(text: &str, sel: Range<usize>, outdent: bool) -> Option<Edit> {
    let first = line_at(text, sel.start);
    let mut last = line_at(text, sel.end);
    // A selection ending at a line's start leaves that line out.
    if sel.end > sel.start && sel.end == last.start && last.start > first.start {
        last = line_at(text, last.start - 1);
    }
    let (spaces, _, _) = item_marks(&text[first.clone()])?;
    // The items above, nearest first: (indent, indent of their text).
    let above = text[..first.start]
        .lines()
        .rev()
        .filter_map(|l| item_marks(l).map(|(s, m, _)| (s, s + m.len())))
        .collect::<Vec<_>>();
    let target = if outdent {
        if spaces == 0 {
            return None;
        }
        above.iter().find(|(s, _)| *s < spaces).map_or(0, |(s, _)| *s)
    } else {
        let (_, inner) = above.iter().find(|(s, _)| *s <= spaces)?;
        if *inner <= spaces {
            return None;
        }
        *inner
    };
    let delta = target as isize - spaces as isize;

    // Each selected line that is an item moves by `delta`; the selection
    // moves with the text.
    let mut out = String::from(&text[..first.start]);
    let mut moved = Vec::new();
    for (k, line) in text[first.start..last.end].split('\n').enumerate() {
        if k > 0 {
            out.push('\n');
        }
        let own = line.len() - line.trim_start_matches(' ').len();
        let shift = if item_marks(line).is_some() { (own as isize + delta).max(0) - own as isize } else { 0 };
        moved.push(shift);
        if shift >= 0 {
            out.push_str(&" ".repeat(shift as usize));
            out.push_str(line);
        } else {
            out.push_str(&line[(-shift) as usize..]);
        }
    }
    out.push_str(&text[last.end..]);

    // Where a byte of the old text is now.
    let map = |pos: usize| -> usize {
        let mut old = first.start;
        let mut total = 0isize;
        for (line, shift) in text[first.start..last.end].split('\n').zip(&moved) {
            let end = old + line.len();
            if pos <= end {
                let into = (pos - old) as isize;
                return (old as isize + total + (into + shift).max(0)) as usize;
            }
            total += shift;
            old = end + 1;
        }
        (pos as isize + total) as usize
    };
    Some(Edit { text: out, sel: map(sel.start)..map(sel.end) })
}

/// Ctrl+B / Ctrl+I: the selection made bold (`**`) or italic (`*`), or back
/// to plain if it is. With nothing selected, the word at the cursor; out of
/// any word, a pair of marks for what is typed next.
pub fn emphasis(text: &str, sel: Range<usize>, mark: &str) -> Edit {
    let word = |c: char| c.is_alphanumeric() || c == '\'';
    let (mut a, mut b) = (sel.start, sel.end);
    if a == b {
        a = text[..a].char_indices().rev().take_while(|(_, c)| word(*c)).last().map_or(a, |(i, _)| i);
        b += text[b..].char_indices().take_while(|(_, c)| word(*c)).last().map_or(0, |(i, c)| i + c.len_utf8());
    }
    // Marks never go next to a space.
    a += text[a..b].len() - text[a..b].trim_start().len();
    b -= text[a..b].len() - text[a..b].trim_end().len();
    if a == b {
        let text = format!("{}{mark}{mark}{}", &text[..a], &text[a..]);
        return Edit { text, sel: a + mark.len()..a + mark.len() };
    }
    let star = mark.as_bytes()[0];
    let before = text.as_bytes()[..a].iter().rev().take_while(|c| **c == star).count();
    let after = text.as_bytes()[b..].iter().take_while(|c| **c == star).count();
    // Bold is two marks, italic one (or three, with bold).
    let marked = if mark.len() == 2 { before >= 2 && after >= 2 } else { before % 2 == 1 && after % 2 == 1 };
    let m = mark.len();
    if marked {
        let text = format!("{}{}{}", &text[..a - m], &text[a..b], &text[b + m..]);
        Edit { text, sel: a - m..b - m }
    } else {
        let text = format!("{}{mark}{}{mark}{}", &text[..a], &text[a..b], &text[b..]);
        Edit { text, sel: a + m..b + m }
    }
}

/// Ctrl+K: the selection made a link's text, the cursor where its address
/// goes; a selected address becomes the address, the cursor on the text.
pub fn link(text: &str, sel: Range<usize>) -> Edit {
    let picked = &text[sel.clone()];
    let address = ["http://", "https://", "www."].iter().any(|p| picked.starts_with(p));
    let (made, cursor) = if address {
        (format!("[]({picked})"), sel.start + 1)
    } else {
        (format!("[{picked}]()"), sel.start + picked.len() + 3)
    };
    Edit { text: format!("{}{made}{}", &text[..sel.start], &text[sel.end..]), sel: cursor..cursor }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `edit` on `text`, where `|` marks the selection (one `|`: the cursor;
    /// two: from the first to the second); the result the same way.
    fn typed(text: &str, edit: impl Fn(&str, Range<usize>) -> Option<Edit>) -> Option<String> {
        let a = text.find('|').unwrap();
        let b = text[a + 1..].find('|').map_or(a, |k| a + k);
        let plain = text.replacen('|', "", 2);
        edit(&plain, a..b).map(|e| {
            let mut s = e.text.clone();
            if e.sel.is_empty() {
                s.insert(e.sel.start, '|');
            } else {
                s.insert(e.sel.end, '|');
                s.insert(e.sel.start, '|');
            }
            s
        })
    }

    #[test]
    fn enter_continues_lists_and_quotes() {
        assert_eq!(typed("- milk|", enter).unwrap(), "- milk\n- |");
        assert_eq!(typed("  * a|", enter).unwrap(), "  * a\n  * |");
        assert_eq!(typed("9. nine|", enter).unwrap(), "9. nine\n10. |");
        assert_eq!(typed("- [x] done|", enter).unwrap(), "- [x] done\n- [ ] |");
        assert_eq!(typed("> said|", enter).unwrap(), "> said\n> |");
        assert_eq!(typed("- spl|it", enter).unwrap(), "- spl\n- |it");
        assert_eq!(typed("plain|", enter), None);
        assert_eq!(typed("-| item", enter), None, "inside the marks");
    }

    #[test]
    fn enter_on_an_empty_item_ends_it() {
        assert_eq!(typed("- a\n- |", enter).unwrap(), "- a\n|");
        assert_eq!(typed("- a\n  - |", enter).unwrap(), "- a\n- |", "a nested one goes up");
    }

    #[test]
    fn tab_nests_items_under_the_one_above() {
        let tab = |t: &str, s| indent(t, s, false);
        let untab = |t: &str, s| indent(t, s, true);
        assert_eq!(typed("- a\n- b|", tab).unwrap(), "- a\n  - b|");
        assert_eq!(typed("1. a\n- b|", tab).unwrap(), "1. a\n   - b|", "under a number: three spaces");
        assert_eq!(typed("- a\n  - b|", tab), None, "no deeper than one level below");
        assert_eq!(typed("- a|", tab), None, "nothing above");
        assert_eq!(typed("- a\n  - b|", untab).unwrap(), "- a\n- b|");
        assert_eq!(typed("- a\n|- b\n- c|", tab).unwrap(), "- a\n  |- b\n  - c|");
        assert_eq!(typed("- a\n|- b\n\n- c|", tab).unwrap(), "- a\n  |- b\n\n  - c|", "blank lines stay");
        assert_eq!(typed("plain|", tab), None);
    }

    #[test]
    fn bold_and_italic_toggle() {
        let bold = |t: &str, s| Some(emphasis(t, s, "**"));
        let italic = |t: &str, s| Some(emphasis(t, s, "*"));
        assert_eq!(typed("a |word| b", bold).unwrap(), "a **|word|** b");
        assert_eq!(typed("a **|word|** b", bold).unwrap(), "a |word| b");
        assert_eq!(typed("a wo|rd b", italic).unwrap(), "a *|word|* b", "the word at the cursor");
        assert_eq!(typed("a |word |b", bold).unwrap(), "a **|word|** b", "not around the space");
        assert_eq!(typed("a | b", bold).unwrap(), "a **|** b");
        assert_eq!(typed("**|word|**", italic).unwrap(), "***|word|***", "italic in bold");
        assert_eq!(typed("***|word|***", italic).unwrap(), "**|word|**");
    }

    #[test]
    fn links() {
        let l = |t: &str, s| Some(link(t, s));
        assert_eq!(typed("see |site| now", l).unwrap(), "see [site](|) now");
        assert_eq!(typed("|https://x.y|", l).unwrap(), "[|](https://x.y)");
    }

    /// `text` laid out at font size 16 in a box 400 wide.
    fn lay_out(text: &str) -> (TextLayout, Vec<String>) {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        // Fonts are there once a frame has begun.
        let mut laid_out = None;
        crate::test_frame(&ctx, egui::RawInput::default(), |ui| laid_out = Some(layout(ui.ctx(), text, 16.0, 400.0)));
        laid_out.unwrap()
    }

    fn run<'a>(layout: &'a TextLayout, text: &str) -> &'a TextRun {
        layout.runs.iter().find(|r| r.text == text).unwrap_or_else(|| panic!("no run {text:?}"))
    }

    #[test]
    fn inline_styles() {
        let (l, rows) = lay_out("a **b** *c* ~~d~~ `e` [f](http://x.y) ***g***");
        assert_eq!(rows, ["a b c d e f g"]);
        assert!(run(&l, "b").style.bold);
        assert!(run(&l, "c").style.italic);
        assert!(run(&l, "d").style.strike);
        assert!(run(&l, "e").style.code && run(&l, "e").style.code_span);
        assert!(run(&l, "f").style.underline);
        assert!(run(&l, "g").style.bold && run(&l, "g").style.italic);
        // CommonMark: no emphasis inside words with `_`, none unclosed.
        assert_eq!(lay_out("snake_case_name and 2 * 3 and **open").1, ["snake_case_name and 2 * 3 and **open"]);
    }

    #[test]
    fn line_breaks_and_blank_lines_are_kept() {
        let (l, rows) = lay_out("one\ntwo\n\n\nthree");
        assert_eq!(rows, ["one", "two", "", "", "three"]);
        let (two, three) = (run(&l, "two"), run(&l, "three"));
        // Two blank lines between them: three lines down.
        let step = two.top - run(&l, "one").top;
        assert!((three.top - two.top - 3.0 * step).abs() < 0.5, "{} {}", three.top, two.top);
    }

    #[test]
    fn blocks_are_laid_out() {
        let text =
            "# Hi\n- **bold** item\n  - deeper\n1. first\n2. second\n- [x] done\n> quoted\n\n---\n```\nlet x = 1;\n```";
        let (l, rows) = lay_out(text);
        assert_eq!(
            rows,
            ["Hi", "• bold item", "• deeper", "1. first", "2. second", "☐ done", "quoted", "", "", "let x = 1;"]
        );
        assert!(run(&l, "Hi").size > 16.0 && run(&l, "Hi").style.bold);
        assert!(run(&l, "deeper").x > run(&l, "bold").x, "a nested item is indented");
        assert!(run(&l, "let x = 1;").style.code);
        let marks = |f: fn(&TextMark) -> bool| l.marks.iter().filter(|m| f(m)).count();
        assert_eq!(marks(|m| matches!(m, TextMark::Check { checked: true, .. })), 1);
        assert_eq!(marks(|m| matches!(m, TextMark::Quote { .. })), 1);
        assert_eq!(marks(|m| matches!(m, TextMark::Shade { .. })), 1);
        assert_eq!(marks(|m| matches!(m, TextMark::Line { .. })), 1);
    }

    #[test]
    fn tables() {
        let (l, rows) = lay_out("| a | b |\n|---|---|\n| 1 | **2** |");
        assert_eq!(rows, ["a | b", "1 | 2"]);
        assert!(run(&l, "a").style.bold, "the header is bold");
        assert!(run(&l, "2").style.bold);
        assert!(run(&l, "b").x > run(&l, "a").x && run(&l, "1").top > run(&l, "a").top);
        assert!(l.marks.iter().filter(|m| matches!(m, TextMark::Line { .. })).count() >= 6, "the grid");
    }

    /// Every run's text is its Markdown's, where it says it is.
    #[test]
    fn runs_keep_their_source() {
        let text = "# T\nsome **bold** and `code`, [a link](http://x.y)\n- item\n| h | i |\n|--|--|\n| c | d |";
        let (l, _) = lay_out(text);
        for r in &l.runs {
            if let Some(at) = r.source {
                assert_eq!(&text[at..at + r.text.len()], r.text);
            }
        }
        assert!(l.runs.iter().filter(|r| r.source.is_some()).count() >= 8);
    }

    #[test]
    fn the_editor_dims_the_marks() {
        let color = Color32::WHITE;
        let job = highlight("# Title\n**b** x", 16.0, color);
        let section = |s: &str| {
            job.sections
                .iter()
                .find(|sec| &job.text[sec.byte_range.start.0..sec.byte_range.end.0] == s)
                .unwrap_or_else(|| panic!("{s:?}"))
        };
        assert_ne!(section("# ").format.color, color);
        assert_eq!(section("Title").format.color, color);
        assert!(section("Title").format.font_id.size > 16.0);
        assert_ne!(section("**").format.color, color);
        assert_eq!(section("b").format.font_id.family, theme::strong_family());
    }
}
