// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Note files.
//!
//! v6 layout:
//!
//! ```text
//! "INK1" | version u32 LE (= 6) | slot_size u32 LE
//!        | meta slot A | meta slot B             (slot_size bytes each)
//!        | pdf_len u64 LE | PDF bytes             (empty unless on a PDF)
//!        | doc_len u64 LE | bincode(Document)     (the snapshot)
//!        | record | record | ...                 (the journal)
//!
//! meta slot: seq u64 LE | len u32 LE | crc32 u32 LE | bincode(NoteMeta)
//! record:    len u32 LE | crc32 u32 LE | bincode(Delta)
//! ```
//!
//! The small meta block (title, dates, page count and a light vector preview
//! of page 1) can be read without touching the document, which is what keeps
//! the library screen fast. The PDF a note is written on is stored raw, so
//! the note stays one self-contained file.
//!
//! Autosaving an open note (`NoteWriter`) does not rewrite the file: it
//! appends one record with what changed since the last save (see `journal`)
//! and writes the meta into the older of the two slots, in place. Loading
//! replays the records on the snapshot. A crash can only tear the last
//! record or the slot being written; their checksums catch that, and the
//! note opens as of the save before. Once the journal has grown as large as
//! the snapshot (at least `JOURNAL_MIN`, and a quarter of the PDF), the next
//! save writes the whole file again, which empties it.
//!
//! Whole-file writes are atomic (temp file + rename) and copy the previous
//! version to `<note>.ink.bak` at most every ten minutes, so a bug can never
//! destroy the only good copy. v5 (meta, document and PDF, no journal),
//! v4 (no document length, no PDF) and v2 / v3 files (document only) still
//! load; their first save writes v6.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::journal::{self, Delta};
use crate::model::{Document, PageStyle, Stroke, TextBox};
use crate::pdf_pages::PdfFile;

const MAGIC: &[u8; 4] = b"INK1";
const FORMAT_VERSION: u32 = 6;
/// Meta block, document and PDF; no journal.
const V5: u32 = 5;
/// Meta block first, document only (no length, no PDF).
const V4: u32 = 4;
/// Magic + version.
const HEADER_LEN: usize = 8;
/// Magic + version + meta length (v4 / v5), or + slot size (v6).
const META_PREFIX: usize = 12;
const MAX_META_LEN: u32 = 16 * 1024 * 1024;
/// seq + len + crc of a meta slot.
const SLOT_HEAD: usize = 16;
const MIN_SLOT: usize = 4096;
const MAX_SLOT: usize = 2 * MAX_META_LEN as usize;
/// len + crc of a journal record.
const RECORD_HEAD: usize = 8;
/// The journal is never compacted below this size.
const JOURNAL_MIN: u64 = 1024 * 1024;
/// Upper bound of points kept in the page-1 preview.
const PREVIEW_MAX_POINTS: usize = 3000;
const BACKUP_MIN_AGE: Duration = Duration::from_secs(600);

// ------------------------------------------------------------- metadata

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PreviewStroke {
    pub color: [u8; 4],
    pub width: f32,
    pub pts: Vec<[f32; 2]>,
}

/// Light vector rendering of page 1 for the library cards.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Preview {
    pub page_w: f32,
    pub page_h: f32,
    pub strokes: Vec<PreviewStroke>,
    /// Text lines as bars: `[x, y, width, height]` in page units.
    pub bars: Vec<[f32; 4]>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NoteMeta {
    pub title: String,
    /// The title follows the first text box until the user renames the note.
    pub auto_title: bool,
    /// Seconds since the Unix epoch.
    pub created: u64,
    pub modified: u64,
    pub pages: u32,
    pub preview: Preview,
}

impl NoteMeta {
    pub fn new(title: &str, auto_title: bool) -> Self {
        let now = now_secs();
        Self {
            title: title.to_owned(),
            auto_title,
            created: now,
            modified: now,
            pages: 1,
            preview: Preview::default(),
        }
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn make_preview(doc: &Document) -> Preview {
    let strokes: Vec<&Stroke> = doc.strokes.iter().filter(|s| s.page == 0).collect();
    let total: usize = strokes.iter().map(|s| s.points.len()).sum();
    let step = total / PREVIEW_MAX_POINTS + 1;

    let mut out = Vec::with_capacity(strokes.len());
    for s in strokes {
        let n = s.points.len();
        let pts: Vec<[f32; 2]> = s
            .points
            .iter()
            .enumerate()
            .filter(|(i, _)| i % step == 0 || *i + 1 == n)
            .map(|(_, p)| [p.x, p.y])
            .collect();
        if !pts.is_empty() {
            out.push(PreviewStroke { color: s.color, width: s.width, pts });
        }
    }

    let mut bars = Vec::new();
    for t in doc.texts.iter().filter(|t| t.page == 0) {
        for (i, line) in t.lines.iter().enumerate() {
            let chars = line.chars().count();
            if chars > 0 {
                bars.push([
                    t.x,
                    t.y + i as f32 * t.line_h + t.line_h * 0.3,
                    (chars as f32 * t.size * 0.5).min(t.width),
                    t.size * 0.5,
                ]);
            }
        }
    }
    Preview { page_w: doc.page_width, page_h: doc.page_height, strokes: out, bars }
}

/// First non-empty line of the top-most text box on page 1.
fn auto_title(doc: &Document) -> Option<String> {
    let first = doc
        .texts
        .iter()
        .filter(|t| t.page == 0)
        .min_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))?;
    let line = first.text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(48).collect())
}

// ---------------------------------------------------------------- errors

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Codec(bincode::Error),
    BadMagic,
    Corrupt,
    UnsupportedVersion(u32),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Codec(e) => write!(f, "corrupt data: {e}"),
            Self::BadMagic => write!(f, "not a Tratto file"),
            Self::Corrupt => write!(f, "damaged note file"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported file version {v}"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<bincode::Error> for StorageError {
    fn from(e: bincode::Error) -> Self {
        Self::Codec(e)
    }
}

// --------------------------------------------------------------- writing

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = flate2::Crc::new();
    for p in parts {
        crc.update(p);
    }
    crc.sum()
}

/// Slots hold twice the meta they were sized for, so the preview can grow
/// a while before the whole file has to be written for a larger slot.
fn slot_size_for(meta_len: usize) -> usize {
    ((meta_len + SLOT_HEAD) * 2).next_power_of_two().max(MIN_SLOT)
}

/// `seq | len | crc | meta`; the rest of the slot is left as it is.
fn encode_slot(seq: u64, meta: &[u8]) -> Vec<u8> {
    let len = (meta.len() as u32).to_le_bytes();
    let seq = seq.to_le_bytes();
    let mut out = Vec::with_capacity(SLOT_HEAD + meta.len());
    out.extend_from_slice(&seq);
    out.extend_from_slice(&len);
    out.extend_from_slice(&crc32(&[&seq, &len, meta]).to_le_bytes());
    out.extend_from_slice(meta);
    out
}

/// `(seq, meta)` of a slot, unless it is empty or torn.
fn decode_slot(slot: &[u8]) -> Option<(u64, NoteMeta)> {
    let seq = u64::from_le_bytes(slot.get(..8)?.try_into().ok()?);
    let len = u32::from_le_bytes(slot.get(8..12)?.try_into().ok()?);
    let crc = u32::from_le_bytes(slot.get(12..16)?.try_into().ok()?);
    let meta = slot.get(SLOT_HEAD..SLOT_HEAD.checked_add(len as usize)?)?;
    if seq == 0 || crc32(&[&slot[..8], &slot[8..12], meta]) != crc {
        return None;
    }
    Some((seq, bincode::deserialize(meta).ok()?))
}

/// The two meta slots of a v6 file.
struct Slots {
    size: usize,
    /// 0: empty or torn.
    seqs: [u64; 2],
    newest: Option<NoteMeta>,
}

impl Slots {
    /// Reads the slots of the file `f`; `None` if it is not a v6 file.
    fn read(f: &mut File) -> Result<Option<Slots>, StorageError> {
        let mut head = [0u8; META_PREFIX];
        f.seek(SeekFrom::Start(0))?;
        f.read_exact(&mut head)?;
        if &head[..4] != MAGIC {
            return Err(StorageError::BadMagic);
        }
        if u32::from_le_bytes([head[4], head[5], head[6], head[7]]) != FORMAT_VERSION {
            return Ok(None);
        }
        let size = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
        if !(SLOT_HEAD..=MAX_SLOT).contains(&size) {
            return Err(StorageError::Corrupt);
        }
        let mut both = vec![0u8; 2 * size];
        f.read_exact(&mut both)?;
        let (a, b) = (decode_slot(&both[..size]), decode_slot(&both[size..]));
        let seqs = [a.as_ref().map_or(0, |s| s.0), b.as_ref().map_or(0, |s| s.0)];
        let newest = if seqs[0] >= seqs[1] { a } else { b };
        Ok(Some(Slots { size, seqs, newest: newest.map(|s| s.1) }))
    }

    fn fits(&self, meta: &[u8]) -> bool {
        SLOT_HEAD + meta.len() <= self.size
    }

    /// Writes `meta` (encoded, `fits`) over the older slot. A torn write
    /// leaves the other slot, the previous meta, in charge.
    fn write(&mut self, f: &mut File, meta: &[u8]) -> Result<(), StorageError> {
        let older = usize::from(self.seqs[1] < self.seqs[0]);
        let seq = self.seqs[0].max(self.seqs[1]) + 1;
        f.seek(SeekFrom::Start((META_PREFIX + older * self.size) as u64))?;
        f.write_all(&encode_slot(seq, meta))?;
        self.seqs[older] = seq;
        Ok(())
    }
}

/// A v6 file as written: where its parts are, and its document.
struct Written {
    slot_size: usize,
    /// Where the journal starts: the end of the snapshot.
    journal_start: u64,
    /// The end of the last record: the length of the file.
    end: u64,
    snapshot_len: u64,
    pdf_len: u64,
    /// The document the file holds (snapshot + journal).
    saved: Document,
}

/// Writes the whole note (empty journal) and returns its layout.
fn write_full(path: &Path, meta: &NoteMeta, doc: &Document) -> Result<Written, StorageError> {
    let meta_bytes = bincode::serialize(meta)?;
    let slot_size = slot_size_for(meta_bytes.len());
    let snapshot = bincode::serialize(doc)?;
    let pdf: &[u8] = doc.pdf.as_ref().and_then(|p| p.file.as_deref()).map_or(&[], |f| &f.bytes);
    let mut out = Vec::with_capacity(META_PREFIX + 2 * slot_size + 16 + pdf.len() + snapshot.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(slot_size as u32).to_le_bytes());
    out.extend_from_slice(&encode_slot(1, &meta_bytes));
    out.resize(META_PREFIX + 2 * slot_size, 0);
    out.extend_from_slice(&(pdf.len() as u64).to_le_bytes());
    out.extend_from_slice(pdf);
    out.extend_from_slice(&(snapshot.len() as u64).to_le_bytes());
    out.extend_from_slice(&snapshot);
    rolling_backup(path);
    write_atomic(path, &out)?;
    let end = out.len() as u64;
    Ok(Written {
        slot_size,
        journal_start: end,
        end,
        snapshot_len: snapshot.len() as u64,
        pdf_len: pdf.len() as u64,
        saved: doc.clone(),
    })
}

impl Written {
    /// Appends what changed since the last save and updates the meta.
    /// `Ok(false)`: the whole file has to be written instead.
    fn append(&mut self, path: &Path, meta: &NoteMeta, doc: &Document) -> Result<bool, StorageError> {
        let limit = JOURNAL_MIN.max(self.snapshot_len).max(self.pdf_len / 4);
        if self.end - self.journal_start > limit {
            return Ok(false);
        }
        let Some(delta) = journal::diff(&self.saved, doc) else { return Ok(false) };
        let meta_bytes = bincode::serialize(meta)?;
        let mut f = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        // Only append to the file as it was left (not replaced, not torn).
        if f.metadata()?.len() != self.end {
            return Ok(false);
        }
        let Some(mut slots) = Slots::read(&mut f)?.filter(|s| s.size == self.slot_size && s.fits(&meta_bytes)) else {
            return Ok(false);
        };
        if !delta.is_empty() {
            let body = bincode::serialize(&delta)?;
            let Ok(len) = u32::try_from(body.len()) else { return Ok(false) };
            let mut record = Vec::with_capacity(RECORD_HEAD + body.len());
            record.extend_from_slice(&len.to_le_bytes());
            record.extend_from_slice(&crc32(&[&body]).to_le_bytes());
            record.extend_from_slice(&body);
            f.seek(SeekFrom::Start(self.end))?;
            f.write_all(&record)?;
            self.end += record.len() as u64;
            delta.apply(&mut self.saved);
            debug_assert!(
                journal::diff(&self.saved, doc).is_some_and(|d| d.is_empty()),
                "the journal must reproduce the document"
            );
        }
        slots.write(&mut f, &meta_bytes)?;
        Ok(true)
    }
}

/// Saves the open note: appends what changed since the last save instead of
/// writing the whole file.
///
/// It writes the whole file (and with that empties the journal) when the
/// note was in an older format, the journal is due for compaction, the meta
/// outgrew its slot, a delta cannot express the change, or the file is not
/// the one it last wrote. One writer per open note: a new one (from
/// `open_note`, or `default()`) for every note opened.
#[derive(Default)]
pub struct NoteWriter(Option<Written>);

impl NoteWriter {
    /// Saves `doc` to `path`. `meta` is updated first (modified time, page
    /// count, preview and, for auto-titled notes, the title).
    pub fn save(&mut self, path: &Path, meta: &mut NoteMeta, doc: &Document) -> Result<(), StorageError> {
        refresh_meta(meta, doc);
        if let Some(written) = self.0.as_mut() {
            match written.append(path, meta, doc) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => {
                    self.0 = None;
                    return Err(e);
                }
            }
        }
        self.0 = None;
        self.0 = Some(write_full(path, meta, doc)?);
        Ok(())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let tmp = path.with_extension("ink.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Keeps one previous version, refreshed at most every `BACKUP_MIN_AGE`.
fn rolling_backup(path: &Path) {
    if !path.exists() {
        return;
    }
    let bak = path.with_extension("ink.bak");
    let stale = fs::metadata(&bak)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map_or(true, |e| e >= BACKUP_MIN_AGE))
        .unwrap_or(true);
    if stale {
        let _ = fs::copy(path, &bak);
    }
}

/// Modified time, page count, preview and, for auto-titled notes, the title.
fn refresh_meta(meta: &mut NoteMeta, doc: &Document) {
    meta.modified = now_secs();
    meta.pages = doc.page_count;
    meta.preview = make_preview(doc);
    if meta.auto_title {
        if let Some(t) = auto_title(doc) {
            meta.title = t;
        }
    }
}

/// Writes the whole note (for an open note, `NoteWriter` appends instead).
/// `meta` is updated first, see `NoteWriter::save`.
pub fn save_note(path: &Path, meta: &mut NoteMeta, doc: &Document) -> Result<(), StorageError> {
    refresh_meta(meta, doc);
    write_full(path, meta, doc).map(drop)
}

// --------------------------------------------------------------- reading

/// Layout of the document of version-3 and version-4 files.
#[derive(Deserialize)]
struct DocumentV4 {
    page_width: f32,
    page_height: f32,
    page_count: u32,
    page: PageStyle,
    strokes: Vec<Stroke>,
    texts: Vec<TextBox>,
    next_id: u64,
}

impl From<DocumentV4> for Document {
    fn from(d: DocumentV4) -> Self {
        Document::migrated_from_v4(d.page_width, d.page_height, d.page_count, d.page, d.strokes, d.texts, d.next_id)
    }
}

/// The meta block of a v4 / v5 file and where it ends.
fn meta_block(bytes: &[u8]) -> Result<(NoteMeta, usize), StorageError> {
    if bytes.len() < META_PREFIX {
        return Err(StorageError::Corrupt);
    }
    let len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let end = META_PREFIX + len as usize;
    if len > MAX_META_LEN || end > bytes.len() {
        return Err(StorageError::Corrupt);
    }
    Ok((bincode::deserialize(&bytes[META_PREFIX..end])?, end))
}

/// Layout of a version-2 file.
#[derive(Deserialize)]
struct DocumentV2 {
    page_width: f32,
    page_height: f32,
    page_count: u32,
    page: PageStyle,
    strokes: Vec<Stroke>,
    next_id: u64,
}

fn legacy_meta(path: &Path, doc: &Document) -> NoteMeta {
    let title = path.file_stem().map_or_else(|| "Imported note".to_owned(), |s| s.to_string_lossy().into_owned());
    let mtime = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or_else(now_secs, |d| d.as_secs());
    NoteMeta {
        title,
        auto_title: false,
        created: mtime,
        modified: mtime,
        pages: doc.page_count,
        preview: make_preview(doc),
    }
}

/// `len` bytes from `at`, if the file has them.
fn part(bytes: &[u8], at: usize, len: u64) -> Result<&[u8], StorageError> {
    usize::try_from(len)
        .ok()
        .and_then(|len| at.checked_add(len))
        .and_then(|end| bytes.get(at..end))
        .ok_or(StorageError::Corrupt)
}

/// The part `len u64 LE | bytes` at `at`.
fn sized_part(bytes: &[u8], at: usize) -> Result<&[u8], StorageError> {
    let len = u64::from_le_bytes(part(bytes, at, 8)?.try_into().expect("8 bytes"));
    part(bytes, at + 8, len)
}

/// Applies the journal records from `at` on and returns where the intact
/// ones end. A torn record (a crash while appending) ends the journal.
fn replay(bytes: &[u8], mut at: usize, doc: &mut Document) -> Result<usize, StorageError> {
    while let Some(head) = bytes.get(at..).and_then(|rest| rest.get(..RECORD_HEAD)) {
        let len = u32::from_le_bytes(head[..4].try_into().expect("4 bytes")) as usize;
        let crc = u32::from_le_bytes(head[4..].try_into().expect("4 bytes"));
        let Some(body) = bytes.get(at + RECORD_HEAD..).and_then(|rest| rest.get(..len)) else { break };
        if crc32(&[body]) != crc {
            break;
        }
        // An intact record that does not decode is no crash: the note is not
        // opened (so not overwritten either) rather than silently cut short.
        let delta: Delta = bincode::deserialize(body)?;
        delta.apply(doc);
        at += RECORD_HEAD + len;
    }
    Ok(at)
}

/// A v6 file; with `writer`, also the layout and document to append to.
fn read_v6(path: &Path, bytes: &[u8], writer: bool) -> Result<(NoteMeta, Document, Option<Written>), StorageError> {
    let head = part(bytes, 0, META_PREFIX as u64)?;
    let slot_size = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
    if !(SLOT_HEAD..=MAX_SLOT).contains(&slot_size) {
        return Err(StorageError::Corrupt);
    }
    let slots = part(bytes, META_PREFIX, 2 * slot_size as u64)?;
    let newest = [decode_slot(&slots[..slot_size]), decode_slot(&slots[slot_size..])]
        .into_iter()
        .flatten()
        .max_by_key(|s| s.0)
        .map(|s| s.1);
    let mut at = META_PREFIX + 2 * slot_size;
    let pdf = sized_part(bytes, at)?;
    at += 8 + pdf.len();
    let snapshot = sized_part(bytes, at)?;
    at += 8 + snapshot.len();
    let mut doc: Document = bincode::deserialize(snapshot)?;
    if let Some(p) = doc.pdf.as_mut() {
        if pdf.is_empty() {
            return Err(StorageError::Corrupt);
        }
        p.file = Some(PdfFile::new(pdf.to_vec()));
    }
    let end = replay(bytes, at, &mut doc)?;
    // Both slots torn: rebuilt from the document, written at the next save.
    let meta = newest.unwrap_or_else(|| legacy_meta(path, &doc));
    let written = writer.then(|| Written {
        slot_size,
        journal_start: at as u64,
        end: end as u64,
        snapshot_len: snapshot.len() as u64,
        pdf_len: pdf.len() as u64,
        saved: doc.clone(),
    });
    Ok((meta, doc, written))
}

/// Reads a note, with the writer that saves it from now on.
pub fn open_note(path: &Path) -> Result<(NoteMeta, Document, NoteWriter), StorageError> {
    read_note(path, true).map(|(meta, doc, written)| (meta, doc, NoteWriter(written)))
}

pub fn load_note(path: &Path) -> Result<(NoteMeta, Document), StorageError> {
    read_note(path, false).map(|(meta, doc, _)| (meta, doc))
}

fn read_note(path: &Path, writer: bool) -> Result<(NoteMeta, Document, Option<Written>), StorageError> {
    let bytes = fs::read(path)?;
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(StorageError::BadMagic);
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version == FORMAT_VERSION {
        return read_v6(path, &bytes, writer);
    }
    read_legacy(path, &bytes, version).map(|(meta, doc)| (meta, doc, None))
}

fn read_legacy(path: &Path, bytes: &[u8], version: u32) -> Result<(NoteMeta, Document), StorageError> {
    match version {
        V5 => {
            let (meta, end) = meta_block(bytes)?;
            let rest = &bytes[end..];
            if rest.len() < 8 {
                return Err(StorageError::Corrupt);
            }
            let doc_len = u64::from_le_bytes(rest[..8].try_into().expect("8 bytes"));
            let doc_end = usize::try_from(doc_len).ok().and_then(|l| l.checked_add(8)).filter(|&e| e <= rest.len());
            let doc_end = doc_end.ok_or(StorageError::Corrupt)?;
            let mut doc: Document = bincode::deserialize(&rest[8..doc_end])?;
            if let Some(pdf) = doc.pdf.as_mut() {
                let raw = &rest[doc_end..];
                if raw.is_empty() {
                    return Err(StorageError::Corrupt);
                }
                pdf.file = Some(PdfFile::new(raw.to_vec()));
            }
            Ok((meta, doc))
        }
        V4 => {
            let (meta, end) = meta_block(bytes)?;
            let doc: DocumentV4 = bincode::deserialize(&bytes[end..])?;
            Ok((meta, doc.into()))
        }
        3 => {
            let doc: Document = bincode::deserialize::<DocumentV4>(&bytes[HEADER_LEN..])?.into();
            Ok((legacy_meta(path, &doc), doc))
        }
        2 => {
            let d: DocumentV2 = bincode::deserialize(&bytes[HEADER_LEN..])?;
            let doc = Document::migrated_from_v2(
                d.page_width,
                d.page_height,
                d.page_count,
                d.page,
                d.strokes,
                d.next_id,
            );
            Ok((legacy_meta(path, &doc), doc))
        }
        v => Err(StorageError::UnsupportedVersion(v)),
    }
}

/// Reads only the meta block (a few KB), not the document.
pub fn read_meta(path: &Path) -> Result<NoteMeta, StorageError> {
    let mut f = File::open(path)?;
    let mut head = [0u8; META_PREFIX];
    if f.read_exact(&mut head).is_ok() && &head[..4] == MAGIC {
        let version = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        if version == FORMAT_VERSION {
            if let Some(meta) = Slots::read(&mut f)?.and_then(|s| s.newest) {
                return Ok(meta);
            }
        } else if version == V5 || version == V4 {
            let len = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
            if len > MAX_META_LEN {
                return Err(StorageError::Corrupt);
            }
            let mut buf = vec![0u8; len as usize];
            f.read_exact(&mut buf)?;
            return Ok(bincode::deserialize(&buf)?);
        }
    }
    // Older formats carry no meta block (and a v6 file whose slots are both
    // torn has none left): load the document once.
    load_note(path).map(|(m, _)| m)
}

/// Changes the meta block (e.g. the title) without re-encoding the document.
/// A v6 file gets it written into a slot in place; v4 and v5 files keep
/// their version. Older files are converted.
pub fn rewrite_meta(path: &Path, change: impl FnOnce(&mut NoteMeta)) -> Result<(), StorageError> {
    let mut f = OpenOptions::new().read(true).write(true).open(path)?;
    if let Some(mut slots) = Slots::read(&mut f).ok().flatten() {
        if let Some(mut meta) = slots.newest.take() {
            change(&mut meta);
            let bytes = bincode::serialize(&meta)?;
            if slots.fits(&bytes) {
                return slots.write(&mut f, &bytes);
            }
            // Outgrew the slots: the whole file, with larger ones.
            drop(f);
            let (_, doc) = load_note(path)?;
            return write_full(path, &meta, &doc).map(drop);
        }
    }
    drop(f);
    let bytes = fs::read(path)?;
    let version = (bytes.len() >= META_PREFIX && &bytes[..4] == MAGIC)
        .then(|| u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]))
        .filter(|&v| v == V5 || v == V4);
    let Some(version) = version else {
        let (mut meta, doc) = load_note(path)?;
        change(&mut meta);
        return write_full(path, &meta, &doc).map(drop);
    };
    let (mut meta, end) = meta_block(&bytes)?;
    change(&mut meta);
    let new_meta = bincode::serialize(&meta)?;
    let mut out = Vec::with_capacity(META_PREFIX + new_meta.len() + bytes.len() - end);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(new_meta.len() as u32).to_le_bytes());
    out.extend_from_slice(&new_meta);
    out.extend_from_slice(&bytes[end..]);
    write_atomic(path, &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Point, StrokeKind, TextBox};

    fn sample_doc() -> Document {
        let mut doc = Document::default();
        let id = doc.alloc_id();
        doc.strokes.push(Stroke::new(
            id,
            0,
            StrokeKind::Highlighter,
            [255, 230, 0, 96],
            14.0,
            vec![Point::new(1.0, 2.0, 0.5), Point::new(30.0, 40.0, 0.7)],
        ));
        doc.texts.push(TextBox {
            id: 50,
            page: 0,
            x: 10.0,
            y: 100.0,
            width: 300.0,
            size: 18.0,
            color: [0, 0, 0],
            text: "\n  Shopping list\nmilk".into(),
            lines: vec!["".into(), "  Shopping list".into(), "milk".into()],
            line_h: 22.0,
        });
        doc
    }

    #[test]
    fn roundtrip_with_meta_and_preview() {
        let doc = sample_doc();
        let path = std::env::temp_dir().join("tratto_roundtrip_v4.ink");
        let mut meta = NoteMeta::new("Untitled note", true);
        save_note(&path, &mut meta, &doc).unwrap();
        assert_eq!(meta.title, "Shopping list"); // auto title from the first text line

        let (m, d) = load_note(&path).unwrap();
        assert_eq!(m.title, "Shopping list");
        assert_eq!(d.strokes[0].points, doc.strokes[0].points);
        assert_eq!(d.texts.len(), 1);

        let only_meta = read_meta(&path).unwrap();
        assert_eq!(only_meta.pages, 1);
        assert_eq!(only_meta.preview.strokes.len(), 1);
        assert!(!only_meta.preview.bars.is_empty());

        rewrite_meta(&path, |m| {
            m.title = "Renamed".into();
            m.auto_title = false;
        })
        .unwrap();
        let (m2, d2) = load_note(&path).unwrap();
        assert_eq!(m2.title, "Renamed");
        assert_eq!(d2.texts.len(), 1);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("ink.bak"));
    }

    /// A document in the v3 / v4 layout.
    #[derive(Serialize)]
    struct OldDocument<'a> {
        page_width: f32,
        page_height: f32,
        page_count: u32,
        page: PageStyle,
        strokes: &'a [Stroke],
        texts: &'a [TextBox],
        next_id: u64,
    }

    fn old_layout(doc: &Document) -> Vec<u8> {
        let old = OldDocument {
            page_width: doc.page_width,
            page_height: doc.page_height,
            page_count: doc.page_count,
            page: doc.page,
            strokes: &doc.strokes,
            texts: &doc.texts,
            next_id: 100,
        };
        bincode::serialize(&old).unwrap()
    }

    #[test]
    fn v4_files_still_load_and_keep_their_version() {
        let doc = sample_doc();
        let path = std::env::temp_dir().join("tratto_legacy_v4.ink");
        let meta = bincode::serialize(&NoteMeta::new("Old note", false)).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&V4.to_le_bytes());
        bytes.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&meta);
        bytes.extend_from_slice(&old_layout(&doc));
        fs::write(&path, bytes).unwrap();

        let (meta, loaded) = load_note(&path).unwrap();
        assert_eq!(meta.title, "Old note");
        assert_eq!((loaded.strokes.len(), loaded.texts.len()), (1, 1));
        assert!(loaded.pdf.is_none());
        // Renaming rewrites only the meta block: still a valid v4 file.
        rewrite_meta(&path, |m| m.title = "Renamed".into()).unwrap();
        assert_eq!(fs::read(&path).unwrap()[4..8], V4.to_le_bytes());
        let (meta, loaded) = load_note(&path).unwrap();
        assert_eq!((meta.title.as_str(), loaded.strokes.len()), ("Renamed", 1));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn notes_on_a_pdf_keep_it() {
        let path = std::env::temp_dir().join("tratto_on_pdf.ink");
        let file = PdfFile::new(b"%PDF-1.5 pretend".to_vec());
        let mut doc = Document::on_pdf(file.clone(), vec![[612.0, 792.0], [792.0, 612.0]]);
        let id = doc.alloc_id();
        doc.strokes.push(sample_doc().strokes[0].clone());
        doc.strokes[0].id = id;
        let mut meta = NoteMeta::new("Slides", false);
        save_note(&path, &mut meta, &doc).unwrap();

        let (meta, loaded) = load_note(&path).unwrap();
        assert_eq!((meta.title.as_str(), loaded.page_count), ("Slides", 2));
        // US Letter in document units.
        assert!((loaded.page_width - 816.0).abs() < 0.01 && (loaded.page_height - 1056.0).abs() < 0.01);
        let pdf = loaded.pdf.as_ref().unwrap();
        assert_eq!(pdf.pages, [Some(0), Some(1)]);
        let stored = pdf.file.as_ref().unwrap();
        assert_eq!((stored.bytes.as_slice(), stored.key), (file.bytes.as_slice(), file.key));
        assert_eq!(loaded.strokes.len(), 1);
        // The title can still be changed without re-encoding.
        rewrite_meta(&path, |m| m.title = "Lecture".into()).unwrap();
        let (meta, again) = load_note(&path).unwrap();
        assert_eq!(meta.title, "Lecture");
        assert_eq!(again.pdf.unwrap().file.unwrap().bytes, file.bytes);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("ink.bak"));
    }

    fn same(a: &Document, b: &Document) -> bool {
        bincode::serialize(a).unwrap() == bincode::serialize(b).unwrap()
    }

    fn pen(doc: &mut Document, points: usize) -> Stroke {
        let id = doc.alloc_id();
        let pts = (0..points).map(|i| Point::new(i as f32, (i % 7) as f32, 0.5)).collect();
        Stroke::new(id, 0, StrokeKind::Pen, [0, 0, 0, 255], 2.0, pts)
    }

    fn remove(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("ink.bak"));
    }

    #[test]
    fn autosaves_append_only_the_changes() {
        let path = std::env::temp_dir().join("tratto_journal_append.ink");
        let pdf = PdfFile::new(vec![b'%'; 200_000]);
        let mut doc = Document::on_pdf(pdf.clone(), vec![[612.0, 792.0]; 3]);
        let mut meta = NoteMeta::new("Lecture", false);
        let mut writer = NoteWriter::default();
        writer.save(&path, &mut meta, &doc).unwrap();
        let full = fs::metadata(&path).unwrap().len();
        assert!(full > 200_000);

        // Each save adds a small record, not the PDF again.
        let mut len = full;
        for _ in 0..5 {
            let s = pen(&mut doc, 20);
            doc.strokes.push(s);
            writer.save(&path, &mut meta, &doc).unwrap();
            let now = fs::metadata(&path).unwrap().len();
            assert!(now > len && now - len < 1024, "grew by {}", now - len);
            len = now;
        }
        // Erase, reinsert in the middle, new page, other paper.
        let gone = doc.strokes.remove(2);
        writer.save(&path, &mut meta, &doc).unwrap();
        doc.strokes.insert(1, gone);
        crate::history::Action::InsertPage { index: 0, pdf_page: None }.apply(&mut doc);
        doc.page.spacing = 50.0;
        writer.save(&path, &mut meta, &doc).unwrap();
        assert_eq!(writer.0.as_ref().unwrap().journal_start, full);

        let (m, loaded, mut reopened) = open_note(&path).unwrap();
        assert!(same(&loaded, &doc));
        assert_eq!(loaded.pdf.as_ref().unwrap().file.as_ref().unwrap().key, pdf.key);
        assert_eq!((m.title.as_str(), m.pages), ("Lecture", 4));
        assert_eq!(read_meta(&path).unwrap().preview.strokes.len(), 0); // page 1 is now blank

        // Renaming writes a slot in place; the reopened writer keeps appending.
        let before = len_of(&path);
        rewrite_meta(&path, |m| m.title = "Physics".into()).unwrap();
        assert_eq!(len_of(&path), before);
        assert_eq!(read_meta(&path).unwrap().title, "Physics");
        let mut meta = m;
        meta.title = "Physics".into();
        let s = pen(&mut doc, 5);
        doc.strokes.push(s);
        reopened.save(&path, &mut meta, &doc).unwrap();
        assert_eq!(reopened.0.as_ref().unwrap().journal_start, full);
        let (m, loaded) = load_note(&path).unwrap();
        assert!(same(&loaded, &doc));
        assert_eq!(m.title, "Physics");
        remove(&path);
    }

    fn len_of(path: &Path) -> u64 {
        fs::metadata(path).unwrap().len()
    }

    #[test]
    fn a_torn_record_or_slot_falls_back_to_the_previous_save() {
        let path = std::env::temp_dir().join("tratto_journal_torn.ink");
        let mut doc = Document::default();
        let mut meta = NoteMeta::new("Torn", false);
        let mut writer = NoteWriter::default();
        let s = pen(&mut doc, 10);
        doc.strokes.push(s);
        writer.save(&path, &mut meta, &doc).unwrap();
        let s = pen(&mut doc, 10);
        doc.strokes.push(s);
        writer.save(&path, &mut meta, &doc).unwrap();
        let before = doc.clone();
        let s = pen(&mut doc, 10);
        doc.strokes.push(s);
        meta.title = "Torn twice".into();
        writer.save(&path, &mut meta, &doc).unwrap();

        // A crash in the middle of the last append.
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(len_of(&path) - 3).unwrap();
        drop(file);
        let (_, loaded, mut writer) = open_note(&path).unwrap();
        assert!(same(&loaded, &before));

        // A crash in the middle of the last slot write: the older slot wins.
        let mut f = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let slots = Slots::read(&mut f).unwrap().unwrap();
        let newest = usize::from(slots.seqs[1] > slots.seqs[0]);
        f.seek(SeekFrom::Start((META_PREFIX + newest * slots.size + SLOT_HEAD) as u64)).unwrap();
        f.write_all(b"garbage").unwrap();
        drop(f);
        assert_eq!(read_meta(&path).unwrap().title, "Torn");

        // The next save notices the torn tail and writes the whole file.
        writer.save(&path, &mut meta, &doc).unwrap();
        let (m, loaded) = load_note(&path).unwrap();
        assert!(same(&loaded, &doc));
        assert_eq!(m.title, "Torn twice");
        let w = writer.0.as_ref().unwrap();
        assert_eq!(w.end, w.journal_start);
        remove(&path);
    }

    #[test]
    fn a_long_journal_is_compacted() {
        let path = std::env::temp_dir().join("tratto_journal_compact.ink");
        let mut doc = Document::default();
        let mut meta = NoteMeta::new("Long", false);
        let mut writer = NoteWriter::default();
        writer.save(&path, &mut meta, &doc).unwrap();
        let mut compacted = false;
        for _ in 0..12 {
            // ~120 KB per save, deleted again so the snapshot stays small.
            doc.strokes.clear();
            let s = pen(&mut doc, 10_000);
            doc.strokes.push(s);
            writer.save(&path, &mut meta, &doc).unwrap();
            let w = writer.0.as_ref().unwrap();
            assert!(w.end - w.journal_start <= JOURNAL_MIN + 200_000);
            compacted |= w.end == w.journal_start;
        }
        assert!(compacted);
        assert!(same(&load_note(&path).unwrap().1, &doc));
        remove(&path);
    }

    #[test]
    fn the_writer_never_appends_to_a_file_replaced_behind_it() {
        let path = std::env::temp_dir().join("tratto_journal_replaced.ink");
        let mut doc = Document::default();
        let mut meta = NoteMeta::new("Mine", false);
        let mut writer = NoteWriter::default();
        writer.save(&path, &mut meta, &doc).unwrap();

        // Another note is written to the same path (e.g. restored from the trash).
        let mut other = Document::default();
        let s = pen(&mut other, 30);
        other.strokes.push(s);
        save_note(&path, &mut NoteMeta::new("Other", false), &other).unwrap();

        let s = pen(&mut doc, 5);
        doc.strokes.push(s);
        writer.save(&path, &mut meta, &doc).unwrap();
        let (m, loaded) = load_note(&path).unwrap();
        assert!(same(&loaded, &doc));
        assert_eq!(m.title, "Mine");
        remove(&path);
    }

    #[test]
    fn v5_files_still_load_and_are_converted_on_save() {
        let doc = sample_doc();
        let path = std::env::temp_dir().join("tratto_legacy_v5.ink");
        let meta = bincode::serialize(&NoteMeta::new("Old", false)).unwrap();
        let body = bincode::serialize(&doc).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&V5.to_le_bytes());
        bytes.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&meta);
        bytes.extend_from_slice(&(body.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&body);
        fs::write(&path, bytes).unwrap();

        assert_eq!(read_meta(&path).unwrap().title, "Old");
        let (mut meta, mut doc, mut writer) = open_note(&path).unwrap();
        assert!(writer.0.is_none());
        doc.page_count = 2;
        writer.save(&path, &mut meta, &doc).unwrap();
        assert_eq!(fs::read(&path).unwrap()[4..8], FORMAT_VERSION.to_le_bytes());
        assert!(same(&load_note(&path).unwrap().1, &doc));
        remove(&path);
    }

    #[test]
    fn v3_files_still_load() {
        let doc = sample_doc();
        let path = std::env::temp_dir().join("tratto_legacy_v3.ink");
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&old_layout(&doc));
        fs::write(&path, bytes).unwrap();

        let (meta, loaded) = load_note(&path).unwrap();
        assert_eq!(loaded.strokes.len(), 1);
        assert_eq!(meta.title, "tratto_legacy_v3");
        assert_eq!(read_meta(&path).unwrap().pages, 1);
        let _ = fs::remove_file(&path);
    }
}
