//! The note library on disk.
//!
//! ```text
//! <root>/                      library folder (default: Documents/InkNote)
//!   Quick note.ink             loose notes
//!   My notebook/               a notebook (folder)
//!     .notebook                  color=R,G,B
//!     Section 1/               a section (sub-folder)
//!       Some note.ink
//!     Loose note.ink           notes directly in a notebook ("General")
//!   .trash/                    deleted items (nothing is ever removed for good)
//! ```
//!
//! Plain files and folders on purpose: they work with any sync tool or backup,
//! one damaged note cannot affect the others, and users can see their data.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::model::Document;
use crate::storage::{self, NoteMeta};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

const NOTE_EXT: &str = "ink";
const NOTEBOOK_FILE: &str = ".notebook";
const TRASH_DIR: &str = ".trash";

const NOTEBOOK_COLORS: [[u8; 3]; 6] = [
    [66, 133, 244],
    [219, 68, 55],
    [15, 157, 88],
    [244, 160, 0],
    [171, 71, 188],
    [0, 172, 193],
];

#[derive(Clone)]
pub struct NoteInfo {
    pub path: PathBuf,
    pub meta: NoteMeta,
}

#[derive(Clone)]
pub struct SectionInfo {
    pub name: String,
    pub path: PathBuf,
    pub notes: Vec<NoteInfo>,
}

#[derive(Clone)]
pub struct NotebookInfo {
    pub name: String,
    pub path: PathBuf,
    pub color: [u8; 3],
    pub sections: Vec<SectionInfo>,
    /// Notes directly inside the notebook folder.
    pub loose: Vec<NoteInfo>,
}

#[derive(Default)]
pub struct Library {
    pub root: PathBuf,
    pub notebooks: Vec<NotebookInfo>,
    /// Notes directly in the library folder.
    pub quick: Vec<NoteInfo>,
}

pub fn default_root() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    match home {
        Some(h) => {
            let docs = h.join("Documents");
            (if docs.is_dir() { docs } else { h }).join("InkNote")
        }
        None => PathBuf::from("InkNote"),
    }
}

impl Library {
    pub fn empty(root: PathBuf) -> Self {
        Self { root, notebooks: Vec::new(), quick: Vec::new() }
    }

    /// Reads the folder structure and the meta block of every note.
    pub fn scan(root: &Path) -> io::Result<Library> {
        fs::create_dir_all(root)?;
        let mut lib = Library::empty(root.to_path_buf());

        for entry in fs::read_dir(root)?.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                lib.notebooks.push(scan_notebook(&path, name));
            } else if is_note(&path) {
                if let Some(n) = read_note(&path) {
                    lib.quick.push(n);
                }
            }
        }
        lib.notebooks.sort_by_key(|n| n.name.to_lowercase());
        sort_notes(&mut lib.quick);
        Ok(lib)
    }

    /// Every note in the library, newest first.
    pub fn all_notes(&self) -> Vec<&NoteInfo> {
        let mut all: Vec<&NoteInfo> = self.quick.iter().collect();
        for nb in &self.notebooks {
            all.extend(nb.loose.iter());
            for s in &nb.sections {
                all.extend(s.notes.iter());
            }
        }
        all.sort_by(|a, b| b.meta.modified.cmp(&a.meta.modified));
        all
    }

    /// The most recently modified notes anywhere in the library.
    pub fn recent(&self, n: usize) -> Vec<&NoteInfo> {
        let mut all = self.all_notes();
        all.truncate(n);
        all
    }
}

fn is_note(path: &Path) -> bool {
    path.is_file() && path.extension().map_or(false, |e| e == NOTE_EXT)
}

fn read_note(path: &Path) -> Option<NoteInfo> {
    storage::read_meta(path).ok().map(|meta| NoteInfo { path: path.to_path_buf(), meta })
}

fn sort_notes(notes: &mut [NoteInfo]) {
    notes.sort_by(|a, b| b.meta.modified.cmp(&a.meta.modified));
}

fn scan_notebook(path: &Path, name: String) -> NotebookInfo {
    let mut nb = NotebookInfo {
        name,
        path: path.to_path_buf(),
        color: read_color(path),
        sections: Vec::new(),
        loose: Vec::new(),
    };
    if let Ok(rd) = fs::read_dir(path) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                let mut notes: Vec<NoteInfo> = fs::read_dir(&p)
                    .map(|rd| rd.flatten().map(|e| e.path()).filter(|q| is_note(q)).filter_map(|q| read_note(&q)).collect())
                    .unwrap_or_default();
                sort_notes(&mut notes);
                nb.sections.push(SectionInfo { name, path: p, notes });
            } else if is_note(&p) {
                if let Some(n) = read_note(&p) {
                    nb.loose.push(n);
                }
            }
        }
    }
    nb.sections.sort_by_key(|s| s.name.to_lowercase());
    sort_notes(&mut nb.loose);
    nb
}

fn read_color(dir: &Path) -> [u8; 3] {
    let text = fs::read_to_string(dir.join(NOTEBOOK_FILE)).unwrap_or_default();
    for line in text.lines() {
        if let Some(v) = line.trim().strip_prefix("color=") {
            let c: Vec<u8> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            if c.len() == 3 {
                return [c[0], c[1], c[2]];
            }
        }
    }
    NOTEBOOK_COLORS[0]
}

// ------------------------------------------------------------- operations

/// A name that is safe as a file or folder name on Windows and Linux.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let cleaned: String = cleaned.trim_matches(|c| c == '.' || c == ' ').chars().take(60).collect();
    if cleaned.is_empty() {
        "Untitled".to_owned()
    } else {
        cleaned
    }
}

/// `dir/stem.ext`, or `dir/stem (2).ext`, ... if that exists already.
pub fn unique_path(dir: &Path, stem: &str, ext: Option<&str>) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 { stem.to_owned() } else { format!("{stem} ({n})") };
        let file = match ext {
            Some(e) => format!("{name}.{e}"),
            None => name,
        };
        let p = dir.join(file);
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

pub fn create_note(dir: &Path, title: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = unique_path(dir, &sanitize(title), Some(NOTE_EXT));
    let mut meta = NoteMeta::new(title, true);
    storage::save_note(&path, &mut meta, &Document::default())?;
    Ok(path)
}

/// New notebook with a first section, like OneNote.
pub fn create_notebook(root: &Path, name: &str) -> Result<PathBuf> {
    let existing = fs::read_dir(root)
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0);
    let color = NOTEBOOK_COLORS[existing % NOTEBOOK_COLORS.len()];
    let path = unique_path(root, &sanitize(name), None);
    fs::create_dir_all(path.join("Section 1"))?;
    fs::write(path.join(NOTEBOOK_FILE), format!("color={},{},{}\n", color[0], color[1], color[2]))?;
    Ok(path)
}

pub fn create_section(notebook: &Path, name: &str) -> Result<PathBuf> {
    let path = unique_path(notebook, &sanitize(name), None);
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// Renames a notebook or section folder; returns the new path.
pub fn rename_dir(path: &Path, new_name: &str) -> Result<PathBuf> {
    let clean = sanitize(new_name);
    let current = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if clean == current {
        return Ok(path.to_path_buf());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let target = unique_path(parent, &clean, None);
    fs::rename(path, &target)?;
    Ok(target)
}

/// Only the title in the file header changes; the file name stays.
pub fn rename_note(path: &Path, new_title: &str) -> Result<()> {
    let title = new_title.trim().to_owned();
    storage::rewrite_meta(path, move |m| {
        m.title = title;
        m.auto_title = false;
    })?;
    Ok(())
}

pub fn duplicate_note(path: &Path) -> Result<PathBuf> {
    let (mut meta, doc) = storage::load_note(path)?;
    meta.title = format!("{} (copy)", meta.title);
    meta.auto_title = false;
    meta.created = storage::now_secs();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let target = unique_path(dir, &sanitize(&meta.title), Some(NOTE_EXT));
    storage::save_note(&target, &mut meta, &doc)?;
    Ok(target)
}

pub fn move_note(path: &Path, to_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(to_dir)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let target = unique_path(to_dir, &stem, Some(NOTE_EXT));
    fs::rename(path, &target)?;
    Ok(target)
}

/// Moves a note, notebook or section into `<root>/.trash`.
pub fn trash(root: &Path, path: &Path) -> Result<()> {
    let dir = root.join(TRASH_DIR);
    fs::create_dir_all(&dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
    let target = unique_path(&dir, &name, None);
    fs::rename(path, &target)?;
    // The rolling backup of a deleted note is not needed any more.
    let _ = fs::remove_file(path.with_extension("ink.bak"));
    Ok(())
}

/// Copies a note file from anywhere (any format version) into the library.
pub fn import_file(root: &Path, src: &Path) -> Result<PathBuf> {
    let (mut meta, doc) = storage::load_note(src)?;
    meta.auto_title = false;
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported note".into());
    meta.title = stem.clone();
    fs::create_dir_all(root)?;
    let target = unique_path(root, &sanitize(&stem), Some(NOTE_EXT));
    storage::save_note(&target, &mut meta, &doc)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_and_unique() {
        assert_eq!(sanitize("  a/b:c?  "), "a_b_c_");
        assert_eq!(sanitize("..."), "Untitled");
        let dir = std::env::temp_dir().join("inknote_unique_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.ink"), b"").unwrap();
        assert_eq!(unique_path(&dir, "x", Some("ink")), dir.join("x (2).ink"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn notebook_lifecycle() {
        let root = std::env::temp_dir().join("inknote_library_test");
        let _ = fs::remove_dir_all(&root);

        let nb = create_notebook(&root, "Work").unwrap();
        let note = create_note(&nb.join("Section 1"), "Untitled note").unwrap();
        let quick = create_note(&root, "Quick").unwrap();

        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.notebooks.len(), 1);
        assert_eq!(lib.notebooks[0].sections.len(), 1);
        assert_eq!(lib.notebooks[0].sections[0].notes.len(), 1);
        assert_eq!(lib.quick.len(), 1);
        assert_eq!(lib.recent(10).len(), 2);

        rename_note(&note, "Meeting").unwrap();
        let moved = move_note(&note, &root).unwrap();
        assert_eq!(storage::read_meta(&moved).unwrap().title, "Meeting");

        trash(&root, &quick).unwrap();
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.quick.len(), 1); // only the moved note is left
        let _ = fs::remove_dir_all(&root);
    }
}
