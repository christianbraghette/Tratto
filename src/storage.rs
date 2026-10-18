//! Binary persistence: `MAGIC | format version (u32 LE) | bincode(Document)`.
//! Writes go to a temp file first and are renamed, so a crash never leaves a
//! half-written note behind.

use std::{fmt, fs, io, path::Path};

use crate::model::Document;

const MAGIC: &[u8; 4] = b"INK1";
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: usize = 8;

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Codec(bincode::Error),
    BadMagic,
    UnsupportedVersion(u32),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Codec(e) => write!(f, "corrupt data: {e}"),
            Self::BadMagic => write!(f, "not an InkNote file"),
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

pub fn save_document(path: &Path, doc: &Document) -> Result<(), StorageError> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + 1024);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&bincode::serialize(doc)?);

    let tmp = path.with_extension("ink.tmp");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_document(path: &Path) -> Result<Document, StorageError> {
    let bytes = fs::read(path)?;
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(StorageError::BadMagic);
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if version != FORMAT_VERSION {
        return Err(StorageError::UnsupportedVersion(version));
    }
    Ok(bincode::deserialize(&bytes[HEADER_LEN..])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Point, Stroke, StrokeKind};

    #[test]
    fn roundtrip() {
        let mut doc = Document::default();
        let id = doc.alloc_id();
        doc.strokes.push(Stroke::new(
            id,
            StrokeKind::Highlighter,
            [255, 230, 0, 96],
            14.0,
            vec![Point::new(1.0, 2.0, 0.5), Point::new(30.0, 40.0, 0.7)],
        ));
        let path = std::env::temp_dir().join("inknote_roundtrip_test.ink");
        save_document(&path, &doc).unwrap();
        let loaded = load_document(&path).unwrap();
        assert_eq!(loaded.strokes.len(), 1);
        assert_eq!(loaded.strokes[0].points, doc.strokes[0].points);
        let _ = fs::remove_file(path);
    }
}
