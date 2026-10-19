//! Note files.
//!
//! v4 layout:
//!
//! ```text
//! "INK1" | version u32 LE (= 4) | meta_len u32 LE | bincode(NoteMeta) | bincode(Document)
//! ```
//!
//! The small meta block (title, dates, page count and a light vector preview
//! of page 1) can be read without touching the document, which is what keeps
//! the library screen fast. v2 / v3 files (document only) still load.
//!
//! Writes are atomic (temp file + rename) and the previous version is copied
//! to `<note>.ink.bak` at most every ten minutes, so an autosave bug or a
//! crash can never destroy the only good copy.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{Document, PageStyle, Stroke};

const MAGIC: &[u8; 4] = b"INK1";
const FORMAT_VERSION: u32 = 4;
/// Magic + version.
const HEADER_LEN: usize = 8;
/// Magic + version + meta length.
const META_PREFIX: usize = 12;
const MAX_META_LEN: u32 = 16 * 1024 * 1024;
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
            Self::BadMagic => write!(f, "not an InkNote file"),
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

fn encode(meta: &NoteMeta, doc: &Document) -> Result<Vec<u8>, StorageError> {
    let meta_bytes = bincode::serialize(meta)?;
    let doc_bytes = bincode::serialize(doc)?;
    let mut out = Vec::with_capacity(META_PREFIX + meta_bytes.len() + doc_bytes.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&meta_bytes);
    out.extend_from_slice(&doc_bytes);
    Ok(out)
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

/// Saves the note. `meta` is updated first (modified time, page count,
/// preview and, for auto-titled notes, the title).
pub fn save_note(path: &Path, meta: &mut NoteMeta, doc: &Document) -> Result<(), StorageError> {
    meta.modified = now_secs();
    meta.pages = doc.page_count;
    meta.preview = make_preview(doc);
    if meta.auto_title {
        if let Some(t) = auto_title(doc) {
            meta.title = t;
        }
    }
    let bytes = encode(meta, doc)?;
    rolling_backup(path);
    write_atomic(path, &bytes)
}

// --------------------------------------------------------------- reading

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

pub fn load_note(path: &Path) -> Result<(NoteMeta, Document), StorageError> {
    let bytes = fs::read(path)?;
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(StorageError::BadMagic);
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    match version {
        FORMAT_VERSION => {
            if bytes.len() < META_PREFIX {
                return Err(StorageError::Corrupt);
            }
            let len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            let end = META_PREFIX + len as usize;
            if len > MAX_META_LEN || end > bytes.len() {
                return Err(StorageError::Corrupt);
            }
            let meta: NoteMeta = bincode::deserialize(&bytes[META_PREFIX..end])?;
            let doc: Document = bincode::deserialize(&bytes[end..])?;
            Ok((meta, doc))
        }
        3 => {
            let doc: Document = bincode::deserialize(&bytes[HEADER_LEN..])?;
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
            let len = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
            if len > MAX_META_LEN {
                return Err(StorageError::Corrupt);
            }
            let mut buf = vec![0u8; len as usize];
            f.read_exact(&mut buf)?;
            return Ok(bincode::deserialize(&buf)?);
        }
    }
    // Older formats carry no meta block: load the document once.
    load_note(path).map(|(m, _)| m)
}

/// Changes the meta block (e.g. the title) without re-encoding the document.
pub fn rewrite_meta(path: &Path, change: impl FnOnce(&mut NoteMeta)) -> Result<(), StorageError> {
    let bytes = fs::read(path)?;
    let is_v4 = bytes.len() >= META_PREFIX
        && &bytes[..4] == MAGIC
        && u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) == FORMAT_VERSION;
    if !is_v4 {
        let (mut meta, doc) = load_note(path)?;
        change(&mut meta);
        return write_atomic(path, &encode(&meta, &doc)?);
    }
    let len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let end = META_PREFIX + len as usize;
    if len > MAX_META_LEN || end > bytes.len() {
        return Err(StorageError::Corrupt);
    }
    let mut meta: NoteMeta = bincode::deserialize(&bytes[META_PREFIX..end])?;
    change(&mut meta);
    let new_meta = bincode::serialize(&meta)?;
    let mut out = Vec::with_capacity(META_PREFIX + new_meta.len() + bytes.len() - end);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
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
        let path = std::env::temp_dir().join("inknote_roundtrip_v4.ink");
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

    #[test]
    fn v3_files_still_load() {
        let doc = sample_doc();
        let path = std::env::temp_dir().join("inknote_legacy_v3.ink");
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&bincode::serialize(&doc).unwrap());
        fs::write(&path, bytes).unwrap();

        let (meta, loaded) = load_note(&path).unwrap();
        assert_eq!(loaded.strokes.len(), 1);
        assert_eq!(meta.title, "inknote_legacy_v3");
        assert_eq!(read_meta(&path).unwrap().pages, 1);
        let _ = fs::remove_file(&path);
    }
}
