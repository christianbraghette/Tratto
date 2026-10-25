// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The note library on disk.
//!
//! ```text
//! <root>/                      library folder (default: Documents/Tratto)
//!   Quick note.ink             loose notes
//!   My notebook/               a notebook (folder)
//!     .notebook                  color=R,G,B
//!     Section 1/               a section (sub-folder)
//!       Some note.ink
//!     Loose note.ink           notes directly in a notebook ("General")
//!   .trash/                    deleted items, until erased from the trash
//!     .index                     where each item came from (for restoring)
//! ```
//!
//! Plain files and folders on purpose: they work with any sync tool or backup,
//! one damaged note cannot affect the others, and users can see their data.

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};

use crate::fs;
use crate::model::Document;
use crate::pdf_pages::{self, PdfFile};
use crate::storage::{self, NoteMeta};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

const NOTE_EXT: &str = "ink";
const NOTEBOOK_FILE: &str = ".notebook";
/// Manual order of the notes in a folder: one file name per line.
const ORDER_FILE: &str = ".order";
const TRASH_DIR: &str = ".trash";

const NOTEBOOK_COLORS: [[u8; 3]; 6] =
    [[66, 133, 244], [219, 68, 55], [15, 157, 88], [244, 160, 0], [171, 71, 188], [0, 172, 193]];

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
    /// A folder outside the library, opened as a notebook (`add_notebook`).
    pub external: bool,
}

#[derive(Default)]
pub struct Library {
    pub root: PathBuf,
    pub notebooks: Vec<NotebookInfo>,
    /// Notes directly in the library folder.
    pub quick: Vec<NoteInfo>,
    /// What is in `.trash`, most recently deleted first.
    pub trash: Vec<TrashItem>,
}

#[cfg(target_arch = "wasm32")]
pub fn default_root() -> PathBuf {
    // The browser's storage (see `fs`): the library is all there is.
    PathBuf::from("/Tratto")
}

#[cfg(not(target_arch = "wasm32"))]
pub fn default_root() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    match home {
        Some(h) => {
            let docs = h.join("Documents");
            (if docs.is_dir() { docs } else { h }).join("Tratto")
        }
        None => PathBuf::from("Tratto"),
    }
}

impl Library {
    pub fn empty(root: PathBuf) -> Self {
        Self { root, notebooks: Vec::new(), quick: Vec::new(), trash: Vec::new() }
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
            if fs::is_dir(&path) {
                lib.notebooks.push(scan_notebook(&path, name));
            } else if is_note(&path)
                && let Some(n) = read_note(&path)
            {
                lib.quick.push(n);
            }
        }
        lib.notebooks.sort_by_key(|n| n.name.to_lowercase());
        sort_notes(root, &mut lib.quick);
        lib.trash = list_trash(root);
        Ok(lib)
    }

    /// Shows `path`, a folder outside the library folder, as a notebook.
    /// A folder of the library is listed already and is left alone.
    pub fn add_notebook(&mut self, path: &Path) {
        if path.parent() == Some(self.root.as_path()) || self.notebooks.iter().any(|n| n.path == path) {
            return;
        }
        // A drive's root (D:\) has no name of its own.
        let name = Some(file_name(path)).filter(|n| !n.is_empty()).unwrap_or_else(|| path.display().to_string());
        let mut nb = scan_notebook(path, name);
        nb.external = true;
        self.notebooks.push(nb);
        self.notebooks.sort_by_key(|n| n.name.to_lowercase());
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
        all.sort_by_key(|n| std::cmp::Reverse(n.meta.modified));
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
    fs::is_file(path) && path.extension().is_some_and(|e| e == NOTE_EXT)
}

fn read_note(path: &Path) -> Option<NoteInfo> {
    storage::read_meta(path).ok().map(|meta| NoteInfo { path: path.to_path_buf(), meta })
}

/// Notes of a folder in their order: the manual order saved for the folder
/// (by dragging in the notebook panel) first, then the remaining notes in the
/// order they were created.
fn sort_notes(dir: &Path, notes: &mut [NoteInfo]) {
    let order = read_order(dir);
    let rank = |n: &NoteInfo| {
        let name = n.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        order.iter().position(|o| *o == name).unwrap_or(usize::MAX)
    };
    notes.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then(a.meta.created.cmp(&b.meta.created)).then_with(|| a.path.cmp(&b.path))
    });
}

fn read_order(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join(ORDER_FILE))
        .map(|t| t.lines().map(|l| l.trim().to_owned()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// Saves the manual order of the notes in `dir` (the notes' paths, in order).
pub fn set_note_order(dir: &Path, notes: &[PathBuf]) -> Result<()> {
    let names: Vec<String> =
        notes.iter().filter_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())).collect();
    fs::write(dir.join(ORDER_FILE), names.join("\n") + "\n")?;
    Ok(())
}

fn scan_notebook(path: &Path, name: String) -> NotebookInfo {
    let mut nb = NotebookInfo {
        name,
        path: path.to_path_buf(),
        color: read_color(path),
        sections: Vec::new(),
        loose: Vec::new(),
        external: false,
    };
    if let Ok(rd) = fs::read_dir(path) {
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if fs::is_dir(&p) {
                let mut notes: Vec<NoteInfo> = fs::read_dir(&p)
                    .map(|rd| {
                        rd.flatten().map(|e| e.path()).filter(|q| is_note(q)).filter_map(|q| read_note(&q)).collect()
                    })
                    .unwrap_or_default();
                sort_notes(&p, &mut notes);
                nb.sections.push(SectionInfo { name, path: p, notes });
            } else if is_note(&p)
                && let Some(n) = read_note(&p)
            {
                nb.loose.push(n);
            }
        }
    }
    nb.sections.sort_by_key(|s| s.name.to_lowercase());
    sort_notes(path, &mut nb.loose);
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
    let cleaned: String =
        name.trim().chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let cleaned: String = cleaned.trim_matches(|c| c == '.' || c == ' ').chars().take(60).collect();
    if cleaned.is_empty() { "Untitled".to_owned() } else { cleaned }
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
        if !fs::exists(&p) {
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
    let existing = fs::read_dir(root).map(|rd| rd.flatten().filter(|e| fs::is_dir(e.path())).count()).unwrap_or(0);
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

/// Where `path` goes when renamed to `stem` (already sanitized), or `None`
/// if the name does not change. A change of case only keeps the name as it
/// is: on case-insensitive file systems (Windows, macOS) the old name would
/// otherwise count as taken and give "Name (2)".
fn rename_target(path: &Path, stem: &str, ext: Option<&str>) -> Option<PathBuf> {
    let current = match ext {
        Some(_) => path.file_stem(),
        None => path.file_name(),
    };
    let current = current.map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if stem == current {
        return None;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if stem.to_lowercase() == current.to_lowercase() {
        let file = match ext {
            Some(e) => format!("{stem}.{e}"),
            None => stem.to_owned(),
        };
        return Some(parent.join(file));
    }
    Some(unique_path(parent, stem, ext))
}

/// Renames a notebook or section folder; returns the new path.
pub fn rename_dir(path: &Path, new_name: &str) -> Result<PathBuf> {
    let Some(target) = rename_target(path, &sanitize(new_name), None) else {
        return Ok(path.to_path_buf());
    };
    fs::rename(path, &target)?;
    Ok(target)
}

/// Sets the note's title (header and file name); returns the new path.
pub fn rename_note(path: &Path, new_title: &str) -> Result<PathBuf> {
    let title = new_title.trim().to_owned();
    storage::rewrite_meta(path, |m| {
        m.title = title.clone();
        m.auto_title = false;
    })?;
    rename_note_file(path, &title)
}

/// Renames the note's file after `title` (the header is left alone), along
/// with its rolling backup and its place in the folder's manual order.
/// Returns the new path.
pub fn rename_note_file(path: &Path, title: &str) -> Result<PathBuf> {
    let Some(target) = rename_target(path, &sanitize(title), Some(NOTE_EXT)) else {
        return Ok(path.to_path_buf());
    };
    fs::rename(path, &target)?;
    let bak = path.with_extension("ink.bak");
    if fs::exists(&bak) {
        let _ = fs::rename(&bak, target.with_extension("ink.bak"));
    }
    if let Some(dir) = path.parent() {
        let name = |p: &Path| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let (old, new) = (name(path), name(&target));
        let order = read_order(dir);
        if order.contains(&old) {
            let order: Vec<PathBuf> = order.into_iter().map(|o| dir.join(if o == old { &new } else { &o })).collect();
            set_note_order(dir, &order)?;
        }
    }
    Ok(target)
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

/// A copy of the note `path` in `to_dir` (copy and paste): its own title
/// there, "(copy)" added in its own folder. Returns the copy's path.
pub fn copy_note(path: &Path, to_dir: &Path) -> Result<PathBuf> {
    if path.parent() == Some(to_dir) {
        return duplicate_note(path);
    }
    let (mut meta, doc) = storage::load_note(path)?;
    meta.created = storage::now_secs();
    fs::create_dir_all(to_dir)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let target = unique_path(to_dir, &stem, Some(NOTE_EXT));
    storage::save_note(&target, &mut meta, &doc)?;
    Ok(target)
}

pub fn move_note(path: &Path, to_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(to_dir)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let target = unique_path(to_dir, &stem, Some(NOTE_EXT));
    move_path(path, &target)?;
    let bak = path.with_extension("ink.bak");
    if fs::exists(&bak) {
        let _ = move_path(&bak, &target.with_extension("ink.bak"));
    }
    Ok(target)
}

/// Moves the file or folder `from` to `to`, also onto another drive (a
/// notebook outside the library), where it is copied and then removed.
fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(e) = copy_path(from, to) {
                // `to` is a new name: only the partial copy is there.
                let _ = remove_path(to);
                return Err(e);
            }
            remove_path(from)
        }
        moved => moved,
    }
}

fn copy_path(from: &Path, to: &Path) -> io::Result<()> {
    if !fs::is_dir(from) {
        return fs::copy(from, to).map(drop);
    }
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        copy_path(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    if fs::is_dir(path) { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Moves a note, notebook or section into `<root>/.trash`, remembering where
/// it was (for `restore`).
pub fn trash(root: &Path, path: &Path) -> Result<()> {
    let dir = root.join(TRASH_DIR);
    fs::create_dir_all(&dir)?;
    let target = if is_note(path) {
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "note".into());
        unique_path(&dir, &stem, Some(NOTE_EXT))
    } else {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
        unique_path(&dir, &name, None)
    };
    move_path(path, &target)?;
    // The rolling backup of a deleted note is not needed any more.
    let _ = fs::remove_file(path.with_extension("ink.bak"));
    let mut index = read_trash_index(root);
    let origin = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    index.push(TrashEntry { name: file_name(&target), origin, deleted: storage::now_secs() });
    write_trash_index(root, &index)?;
    Ok(())
}

// ------------------------------------------------------------------ trash

/// Where the items of the trash came from: one line per item,
/// `name \t deleted (secs) \t original path relative to the root`.
const TRASH_INDEX: &str = ".index";

/// Something in the trash.
#[derive(Clone)]
pub struct TrashItem {
    /// Where it is now, inside `.trash`.
    pub path: PathBuf,
    /// The note's title, or the folder's name.
    pub name: String,
    /// A folder (notebook or section) rather than a note.
    pub is_folder: bool,
    /// Where it was, if known (items trashed before the index existed are not).
    pub origin: Option<PathBuf>,
    /// Seconds since the Unix epoch.
    pub deleted: u64,
}

struct TrashEntry {
    name: String,
    origin: PathBuf,
    deleted: u64,
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

fn read_trash_index(root: &Path) -> Vec<TrashEntry> {
    let text = fs::read_to_string(root.join(TRASH_DIR).join(TRASH_INDEX)).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            let (name, deleted, origin) = (parts.next()?, parts.next()?, parts.next()?);
            Some(TrashEntry { name: name.to_owned(), deleted: deleted.parse().ok()?, origin: PathBuf::from(origin) })
        })
        .collect()
}

fn write_trash_index(root: &Path, index: &[TrashEntry]) -> Result<()> {
    let text: String =
        index.iter().map(|e| format!("{}\t{}\t{}\n", e.name, e.deleted, e.origin.to_string_lossy())).collect();
    fs::write(root.join(TRASH_DIR).join(TRASH_INDEX), text)?;
    Ok(())
}

/// Drops the index line of a trash item that left the trash.
fn forget_trashed(root: &Path, item: &Path) -> Result<()> {
    let name = file_name(item);
    let mut index = read_trash_index(root);
    let before = index.len();
    index.retain(|e| e.name != name);
    if index.len() != before {
        write_trash_index(root, &index)?;
    }
    Ok(())
}

/// The trash's items, most recently deleted first.
pub fn list_trash(root: &Path) -> Vec<TrashItem> {
    let dir = root.join(TRASH_DIR);
    let index = read_trash_index(root);
    let Ok(rd) = fs::read_dir(&dir) else { return Vec::new() };
    let mut items: Vec<TrashItem> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| !file_name(p).starts_with('.') && (fs::is_dir(p) || is_note(p)))
        .map(|path| {
            let entry = index.iter().find(|e| e.name == file_name(&path));
            let is_folder = fs::is_dir(&path);
            let name = if is_folder {
                file_name(&path)
            } else {
                storage::read_meta(&path).map(|m| m.title).unwrap_or_else(|_| file_name(&path))
            };
            let deleted = entry.map(|e| e.deleted).unwrap_or_else(|| {
                fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs())
            });
            TrashItem { name, is_folder, origin: entry.map(|e| root.join(&e.origin)), deleted, path }
        })
        .collect();
    items.sort_by(|a, b| b.deleted.cmp(&a.deleted).then_with(|| a.name.cmp(&b.name)));
    items
}

/// Puts a trash item back where it was (its folder is created again if it is
/// gone; without a known origin it goes to the root). A name taken meanwhile
/// gets " (2)". Returns the new path.
pub fn restore(root: &Path, item: &Path) -> Result<PathBuf> {
    let origin = read_trash_index(root)
        .into_iter()
        .find(|e| e.name == file_name(item))
        .map(|e| root.join(e.origin))
        .unwrap_or_else(|| root.join(file_name(item)));
    let parent = origin.parent().unwrap_or(root);
    fs::create_dir_all(parent)?;
    let target = if fs::is_dir(item) {
        unique_path(parent, &file_name(&origin), None)
    } else {
        let stem = origin.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        unique_path(parent, &stem, Some(NOTE_EXT))
    };
    move_path(item, &target)?;
    forget_trashed(root, item)?;
    Ok(target)
}

/// Erases a trash item for good.
pub fn delete_forever(root: &Path, item: &Path) -> Result<()> {
    // Only ever inside the trash.
    if item.parent() != Some(root.join(TRASH_DIR).as_path()) {
        return Err("not in the trash".into());
    }
    if fs::is_dir(item) {
        fs::remove_dir_all(item)?;
    } else {
        fs::remove_file(item)?;
    }
    forget_trashed(root, item)
}

/// Erases everything in the trash; returns how many items were erased.
pub fn empty_trash(root: &Path) -> Result<usize> {
    let items = list_trash(root);
    for item in &items {
        delete_forever(root, &item.path)?;
    }
    Ok(items.len())
}

/// Creates a note written on the PDF at `src` in `dir` (one page per PDF
/// page, titled after the file) and returns its path.
pub fn import_pdf(dir: &Path, src: &Path) -> Result<PathBuf> {
    let file = PdfFile::new(fs::read(src)?);
    let sizes = pdf_pages::page_sizes(&file)?;
    if sizes.is_empty() {
        return Err("the PDF has no pages".into());
    }
    let title = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "PDF".into());
    let mut meta = NoteMeta::new(&title, false);
    fs::create_dir_all(dir)?;
    let target = unique_path(dir, &sanitize(&title), Some(NOTE_EXT));
    storage::save_note(&target, &mut meta, &Document::on_pdf(file, sizes))?;
    Ok(target)
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
    fn notes_in_creation_or_manual_order() {
        let dir = std::env::temp_dir().join("tratto_order_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let note = |name: &str, created: u64| {
            let mut meta = NoteMeta::new(name, false);
            meta.created = created;
            meta.modified = 1000 - created; // newest-modified first would reverse them
            NoteInfo { path: dir.join(format!("{name}.ink")), meta }
        };
        let mut notes = vec![note("c", 3), note("a", 1), note("b", 2)];
        sort_notes(&dir, &mut notes);
        let names = |n: &[NoteInfo]| n.iter().map(|n| n.meta.title.clone()).collect::<Vec<_>>();
        assert_eq!(names(&notes), ["a", "b", "c"]);

        // Manual order; notes it does not list follow in creation order.
        set_note_order(&dir, &[dir.join("c.ink"), dir.join("a.ink")]).unwrap();
        sort_notes(&dir, &mut notes);
        assert_eq!(names(&notes), ["c", "a", "b"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_and_unique() {
        assert_eq!(sanitize("  a/b:c?  "), "a_b_c_");
        assert_eq!(sanitize("..."), "Untitled");
        let dir = std::env::temp_dir().join("tratto_unique_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.ink"), b"").unwrap();
        assert_eq!(unique_path(&dir, "x", Some("ink")), dir.join("x (2).ink"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn notebook_lifecycle() {
        let root = std::env::temp_dir().join("tratto_library_test");
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

        let note = rename_note(&note, "Meeting").unwrap();
        assert_eq!(note.file_name().unwrap(), "Meeting.ink");
        let moved = move_note(&note, &root).unwrap();
        assert_eq!(storage::read_meta(&moved).unwrap().title, "Meeting");

        trash(&root, &quick).unwrap();
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.quick.len(), 1); // only the moved note is left

        // A change of case renames in place instead of giving "work (2)".
        let renamed = rename_dir(&nb, "work").unwrap();
        assert_eq!(renamed.file_name().unwrap(), "work");
        let section = rename_dir(&renamed.join("Section 1"), "Ideas").unwrap();
        assert!(section.is_dir() && !renamed.join("Section 1").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copying_a_note_into_another_folder() {
        let root = std::env::temp_dir().join("tratto_copy_note_test");
        let _ = fs::remove_dir_all(&root);
        let nb = create_notebook(&root, "Work").unwrap();
        let note = create_note(&root, "Plan").unwrap();

        // Elsewhere: the same title, the original left where it was.
        let copy = copy_note(&note, &nb).unwrap();
        assert_eq!((copy.parent(), note.is_file()), (Some(nb.as_path()), true));
        assert_eq!(storage::read_meta(&copy).unwrap().title, "Plan");
        // Again there: a name of its own.
        let again = copy_note(&note, &nb).unwrap();
        assert_ne!(again, copy);
        // In its own folder: a duplicate.
        let dup = copy_note(&note, &root).unwrap();
        assert_eq!(storage::read_meta(&dup).unwrap().title, "Plan (copy)");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn external_notebook() {
        let base = std::env::temp_dir().join("tratto_external_test");
        let _ = fs::remove_dir_all(&base);
        let (root, outside) = (base.join("lib"), base.join("Thesis"));
        let inside = create_notebook(&root, "Work").unwrap();
        create_note(&outside.join("Chapter 1"), "Draft").unwrap();
        create_note(&outside, "Ideas").unwrap();

        let mut lib = Library::scan(&root).unwrap();
        lib.add_notebook(&outside);
        lib.add_notebook(&outside);
        lib.add_notebook(&inside);
        let names: Vec<_> = lib.notebooks.iter().map(|n| (n.name.as_str(), n.external)).collect();
        assert_eq!(names, [("Thesis", true), ("Work", false)]);
        let thesis = &lib.notebooks[0];
        assert_eq!((thesis.loose.len(), thesis.sections[0].notes.len()), (1, 1));
        assert_eq!(lib.all_notes().len(), 2);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn trash_restore_and_erase() {
        let root = std::env::temp_dir().join("tratto_trash_test");
        let _ = fs::remove_dir_all(&root);
        let nb = create_notebook(&root, "Work").unwrap();
        let section = nb.join("Section 1");
        let note = create_note(&section, "Plan").unwrap();
        rename_note(&note, "Plan v2").unwrap();
        let note = section.join("Plan v2.ink");

        trash(&root, &note).unwrap();
        trash(&root, &section).unwrap();
        let items = list_trash(&root);
        assert_eq!(items.len(), 2);
        let n = items.iter().find(|i| !i.is_folder).unwrap();
        assert_eq!(n.name, "Plan v2");
        assert_eq!(n.origin.as_deref(), Some(note.as_path()));
        assert!(n.path.extension().is_some_and(|e| e == "ink"));

        // The note goes back even though its section is in the trash too.
        assert_eq!(restore(&root, &n.path).unwrap(), note);
        assert!(note.exists());
        let sec = list_trash(&root).into_iter().find(|i| i.is_folder).unwrap();
        // Its old name is taken now (by the folder re-created for the note).
        assert_eq!(restore(&root, &sec.path).unwrap(), nb.join("Section 1 (2)"));
        assert!(list_trash(&root).is_empty());

        trash(&root, &note).unwrap();
        assert!(delete_forever(&root, &note).is_err()); // not in the trash
        assert_eq!(empty_trash(&root).unwrap(), 1);
        assert!(list_trash(&root).is_empty() && !note.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn renaming_a_note_keeps_its_order_and_backup() {
        let dir = std::env::temp_dir().join("tratto_rename_note_test");
        let _ = fs::remove_dir_all(&dir);
        let a = create_note(&dir, "A").unwrap();
        let b = create_note(&dir, "B").unwrap();
        fs::write(b.with_extension("ink.bak"), b"old").unwrap();
        set_note_order(&dir, &[b.clone(), a.clone()]).unwrap();

        let c = rename_note(&b, "C: draft").unwrap();
        assert_eq!(c, dir.join("C_ draft.ink"));
        assert!(!b.exists() && c.with_extension("ink.bak").exists());
        assert_eq!(storage::read_meta(&c).unwrap().title, "C: draft");
        assert_eq!(read_order(&dir), ["C_ draft.ink", "A.ink"]);

        // Another note's name is never overwritten.
        assert_eq!(rename_note(&c, "A").unwrap(), dir.join("A (2).ink"));
        let _ = fs::remove_dir_all(&dir);
    }
}
