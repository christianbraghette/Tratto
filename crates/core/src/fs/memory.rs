// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file system kept in memory: the browser's (see `fs`). Built on every
//! target, so it is tested with the rest; only the web app uses it.
//!
//! The functions behave like their `std::fs` namesakes for what the app
//! does with them. Paths are absolute (`/Tratto/Notebook/Note.ink`); a
//! relative one is taken from `/`. One tree per thread: the web app has one.
//!
//! Every change is remembered by path (`take_changes`): the web app writes
//! what is at those paths now to the browser's storage, or removes it there
//! when nothing is. `load_file` and `load_dir` fill the tree without that.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct FileData {
    bytes: Vec<u8>,
    /// Seconds since the Unix epoch.
    modified: u64,
}

#[derive(Clone)]
enum Node {
    Dir,
    /// Shared with the open `File`s.
    File(Rc<RefCell<FileData>>),
}

#[derive(Default)]
struct Tree {
    /// Every directory and file but `/`, by normalised path.
    nodes: BTreeMap<PathBuf, Node>,
    changed: BTreeSet<PathBuf>,
}

thread_local! {
    static TREE: RefCell<Tree> = RefCell::new(Tree::default());
}

fn with<T>(f: impl FnOnce(&mut Tree) -> T) -> T {
    TREE.with_borrow_mut(f)
}

fn now() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn error(kind: ErrorKind, path: &Path) -> io::Error {
    io::Error::new(kind, format!("{}: {kind}", path.display()))
}

/// `path` absolute, without `.` and `..`.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in path.components() {
        match c {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

fn is_root(path: &Path) -> bool {
    path.parent().is_none()
}

impl Tree {
    fn get(&self, path: &Path) -> Option<Node> {
        if is_root(path) { Some(Node::Dir) } else { self.nodes.get(path).cloned() }
    }

    fn is_dir(&self, path: &Path) -> bool {
        matches!(self.get(path), Some(Node::Dir))
    }

    /// `path` can be made: its parent is a directory.
    fn parent_ok(&self, path: &Path) -> io::Result<()> {
        match path.parent() {
            Some(parent) if self.is_dir(parent) => Ok(()),
            _ => Err(error(ErrorKind::NotFound, path)),
        }
    }

    fn file(&self, path: &Path) -> io::Result<Rc<RefCell<FileData>>> {
        match self.get(path) {
            Some(Node::File(data)) => Ok(data),
            Some(Node::Dir) => Err(error(ErrorKind::IsADirectory, path)),
            None => Err(error(ErrorKind::NotFound, path)),
        }
    }

    /// `path` and everything under it.
    fn subtree(&self, path: &Path) -> Vec<PathBuf> {
        self.nodes.range(path.to_path_buf()..).map(|(p, _)| p).take_while(|p| p.starts_with(path)).cloned().collect()
    }

    fn put_file(&mut self, path: PathBuf, bytes: Vec<u8>, modified: u64) -> io::Result<Rc<RefCell<FileData>>> {
        self.parent_ok(&path)?;
        if self.is_dir(&path) {
            return Err(error(ErrorKind::IsADirectory, &path));
        }
        let data = Rc::new(RefCell::new(FileData { bytes, modified }));
        self.nodes.insert(path, Node::File(data.clone()));
        Ok(data)
    }
}

// ------------------------------------------------------------ functions

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = normal(path.as_ref());
    with(|t| t.file(&path).map(|d| d.borrow().bytes.clone()))
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))
}

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let path = normal(path.as_ref());
    with(|t| {
        t.put_file(path.clone(), contents.as_ref().to_vec(), now())?;
        t.changed.insert(path);
        Ok(())
    })
}

pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normal(path.as_ref());
    with(|t| {
        if t.get(&path).is_some() {
            return Err(error(ErrorKind::AlreadyExists, &path));
        }
        t.parent_ok(&path)?;
        t.nodes.insert(path.clone(), Node::Dir);
        t.changed.insert(path);
        Ok(())
    })
}

pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normal(path.as_ref());
    with(|t| {
        let mut ancestors: Vec<&Path> = path.ancestors().collect();
        ancestors.reverse();
        for dir in ancestors {
            match t.get(dir) {
                Some(Node::Dir) => {}
                Some(Node::File(_)) => return Err(error(ErrorKind::NotADirectory, dir)),
                None => {
                    t.nodes.insert(dir.to_path_buf(), Node::Dir);
                    t.changed.insert(dir.to_path_buf());
                }
            }
        }
        Ok(())
    })
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normal(path.as_ref());
    with(|t| {
        t.file(&path)?;
        t.nodes.remove(&path);
        t.changed.insert(path);
        Ok(())
    })
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normal(path.as_ref());
    with(|t| {
        match t.get(&path) {
            Some(Node::Dir) if !is_root(&path) => {}
            Some(_) => return Err(error(ErrorKind::NotADirectory, &path)),
            None => return Err(error(ErrorKind::NotFound, &path)),
        }
        for p in t.subtree(&path) {
            t.nodes.remove(&p);
        }
        t.changed.insert(path);
        Ok(())
    })
}

/// Moves a file or a directory (with what is in it). A file replaces a file
/// at `to`; a directory replaces nothing.
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (from, to) = (normal(from.as_ref()), normal(to.as_ref()));
    with(|t| {
        let node = t.get(&from).filter(|_| !is_root(&from)).ok_or_else(|| error(ErrorKind::NotFound, &from))?;
        if from == to {
            return Ok(());
        }
        t.parent_ok(&to)?;
        match (&node, t.get(&to)) {
            (_, None) | (Node::File(_), Some(Node::File(_))) => {}
            (_, Some(_)) => return Err(error(ErrorKind::AlreadyExists, &to)),
        }
        if to.starts_with(&from) {
            return Err(error(ErrorKind::InvalidInput, &to));
        }
        for p in t.subtree(&from) {
            let moved = t.nodes.remove(&p).expect("listed above");
            let rest = p.strip_prefix(&from).expect("under it");
            t.nodes.insert(if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) }, moved);
        }
        t.changed.insert(from);
        t.changed.insert(to);
        Ok(())
    })
}

/// Copies a file; returns its length.
pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let bytes = read(from)?;
    let len = bytes.len() as u64;
    write(to, bytes)?;
    Ok(len)
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    let path = normal(path.as_ref());
    with(|t| t.get(&path).is_some())
}

pub fn is_dir(path: impl AsRef<Path>) -> bool {
    let path = normal(path.as_ref());
    with(|t| t.is_dir(&path))
}

pub fn is_file(path: impl AsRef<Path>) -> bool {
    let path = normal(path.as_ref());
    with(|t| matches!(t.get(&path), Some(Node::File(_))))
}

pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let path = normal(path.as_ref());
    with(|t| match t.get(&path) {
        Some(node) => Ok(Metadata::of(&node)),
        None => Err(error(ErrorKind::NotFound, &path)),
    })
}

/// What is directly in the directory `path`, by name.
pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
    let path = normal(path.as_ref());
    with(|t| {
        if !t.is_dir(&path) {
            let kind = if t.get(&path).is_some() { ErrorKind::NotADirectory } else { ErrorKind::NotFound };
            return Err(error(kind, &path));
        }
        let entries: Vec<io::Result<DirEntry>> = t
            .nodes
            .range(path.clone()..)
            .map(|(p, _)| p)
            .take_while(|p| p.starts_with(&path))
            .filter(|p| p.parent() == Some(path.as_path()))
            .map(|p| Ok(DirEntry { path: p.clone() }))
            .collect();
        Ok(ReadDir(entries.into_iter()))
    })
}

pub struct ReadDir(std::vec::IntoIter<io::Result<DirEntry>>);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

pub struct DirEntry {
    path: PathBuf,
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn file_name(&self) -> OsString {
        self.path.file_name().map(OsString::from).unwrap_or_default()
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        metadata(&self.path)
    }
}

/// Like `std::fs::Metadata` (which has a `len` and no `is_empty` either).
#[allow(clippy::len_without_is_empty)]
#[derive(Clone, Debug)]
pub struct Metadata {
    dir: bool,
    len: u64,
    modified: u64,
}

impl Metadata {
    fn of(node: &Node) -> Self {
        match node {
            Node::Dir => Self { dir: true, len: 0, modified: 0 },
            Node::File(data) => {
                let d = data.borrow();
                Self { dir: false, len: d.bytes.len() as u64, modified: d.modified }
            }
        }
    }

    pub fn is_dir(&self) -> bool {
        self.dir
    }

    pub fn is_file(&self) -> bool {
        !self.dir
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(UNIX_EPOCH + Duration::from_secs(self.modified))
    }
}

// ----------------------------------------------------------------- files

/// An open file: reads and writes go to the file's bytes in the tree.
pub struct File {
    path: PathBuf,
    data: Rc<RefCell<FileData>>,
    pos: u64,
    write: bool,
}

impl File {
    pub fn open(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).open(path)
    }

    pub fn create(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().write(true).create(true).truncate(true).open(path)
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(Metadata::of(&Node::File(self.data.clone())))
    }

    pub fn sync_all(&self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let data = self.data.borrow();
        let start = (self.pos as usize).min(data.bytes.len());
        let n = buf.len().min(data.bytes.len() - start);
        buf[..n].copy_from_slice(&data.bytes[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.write {
            return Err(error(ErrorKind::PermissionDenied, &self.path));
        }
        let mut data = self.data.borrow_mut();
        let start = self.pos as usize;
        if data.bytes.len() < start {
            data.bytes.resize(start, 0);
        }
        let overlap = buf.len().min(data.bytes.len() - start);
        data.bytes[start..start + overlap].copy_from_slice(&buf[..overlap]);
        data.bytes.extend_from_slice(&buf[overlap..]);
        data.modified = now();
        self.pos += buf.len() as u64;
        with(|t| t.changed.insert(self.path.clone()));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let len = self.data.borrow().bytes.len() as i128;
        let at = match pos {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(d) => len + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        self.pos = u64::try_from(at).map_err(|_| error(ErrorKind::InvalidInput, &self.path))?;
        Ok(self.pos)
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    create: bool,
    truncate: bool,
}

impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read(&mut self, on: bool) -> &mut Self {
        self.read = on;
        self
    }

    pub fn write(&mut self, on: bool) -> &mut Self {
        self.write = on;
        self
    }

    pub fn create(&mut self, on: bool) -> &mut Self {
        self.create = on;
        self
    }

    pub fn truncate(&mut self, on: bool) -> &mut Self {
        self.truncate = on;
        self
    }

    pub fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        let path = normal(path.as_ref());
        with(|t| {
            let data = match t.file(&path) {
                Ok(data) => {
                    if self.truncate && self.write {
                        let mut d = data.borrow_mut();
                        d.bytes.clear();
                        d.modified = now();
                        t.changed.insert(path.clone());
                    }
                    data
                }
                Err(e) if e.kind() == ErrorKind::NotFound && self.create && self.write => {
                    t.changed.insert(path.clone());
                    t.put_file(path.clone(), Vec::new(), now())?
                }
                Err(e) => return Err(e),
            };
            Ok(File { path, data, pos: 0, write: self.write })
        })
    }
}

// --------------------------------------------------- the browser's storage

/// Puts a file read from the browser's storage in the tree (its parent is
/// made), not as a change.
pub fn load_file(path: impl AsRef<Path>, bytes: Vec<u8>, modified: u64) {
    let path = normal(path.as_ref());
    if let Some(parent) = path.parent() {
        load_dir(parent);
    }
    with(|t| {
        let _ = t.put_file(path, bytes, modified);
    });
}

/// Puts a directory (and its parents) read from the browser's storage in
/// the tree, not as a change.
pub fn load_dir(path: impl AsRef<Path>) {
    let path = normal(path.as_ref());
    with(|t| {
        for dir in path.ancestors().filter(|d| !is_root(d)) {
            t.nodes.entry(dir.to_path_buf()).or_insert(Node::Dir);
        }
    });
}

/// The paths changed since the last call: for each, what is there now
/// (`entries`) goes to the browser's storage, or it is removed there.
pub fn take_changes() -> Vec<PathBuf> {
    with(|t| std::mem::take(&mut t.changed).into_iter().collect())
}

/// Remembers `path` as changed again (writing it to the browser failed).
pub fn mark_changed(path: impl AsRef<Path>) {
    let path = normal(path.as_ref());
    with(|t| t.changed.insert(path));
}

/// What is at `path` now: the path and everything under it, each with its
/// bytes (a file) or `None` (a directory), parents first. Empty: nothing is.
pub fn entries(path: impl AsRef<Path>) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let path = normal(path.as_ref());
    with(|t| {
        t.subtree(&path)
            .into_iter()
            .map(|p| {
                let bytes = match &t.nodes[&p] {
                    Node::Dir => None,
                    Node::File(data) => Some(data.borrow().bytes.clone()),
                };
                (p, bytes)
            })
            .collect()
    })
}

/// Empties the tree (tests).
#[cfg(test)]
fn clear() {
    with(|t| *t = Tree::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &str) -> Vec<String> {
        let mut n: Vec<String> =
            read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        n.sort();
        n
    }

    #[test]
    fn files_and_directories() {
        clear();
        assert!(write("/a/b.txt", "x").is_err(), "no parent");
        create_dir_all("/a/b").unwrap();
        write("/a/note.ink", b"hello").unwrap();
        write("a/other.ink", b"!").unwrap();
        assert_eq!(read("/a/./b/../note.ink").unwrap(), b"hello");
        assert_eq!(names("/a"), ["b", "note.ink", "other.ink"]);
        assert!(is_dir("/a/b") && is_file("/a/note.ink") && !exists("/a/nope"));
        assert_eq!(metadata("/a/note.ink").unwrap().len(), 5);
        assert!(read_dir("/a/note.ink").is_err() && read("/a/b").is_err());
        assert!(create_dir("/a/b").is_err());

        rename("/a", "/c").unwrap();
        assert!(!exists("/a/note.ink") && is_dir("/c/b"));
        assert_eq!(read_to_string("/c/note.ink").unwrap(), "hello");
        rename("/c/other.ink", "/c/note.ink").unwrap();
        assert_eq!(read("/c/note.ink").unwrap(), b"!");
        assert!(rename("/c", "/c/b/inside").is_err());

        copy("/c/note.ink", "/c/b/copy.ink").unwrap();
        remove_file("/c/note.ink").unwrap();
        assert!(remove_file("/c/b").is_err());
        assert_eq!(names("/c"), ["b"]);
        remove_dir_all("/c").unwrap();
        assert!(!exists("/c/b/copy.ink") && names("/").is_empty());
    }

    #[test]
    fn files_are_read_written_and_appended_in_place() {
        clear();
        let mut f = File::create("/n.ink").unwrap();
        f.write_all(b"0123456789").unwrap();
        let mut f = OpenOptions::new().read(true).write(true).open("/n.ink").unwrap();
        f.seek(SeekFrom::Start(2)).unwrap();
        f.write_all(b"ab").unwrap();
        f.seek(SeekFrom::Start(f.metadata().unwrap().len())).unwrap();
        f.write_all(b"XY").unwrap();
        assert_eq!(read("/n.ink").unwrap(), b"01ab456789XY");
        let mut back = [0u8; 4];
        let mut r = File::open("/n.ink").unwrap();
        r.seek(SeekFrom::End(-4)).unwrap();
        r.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"89XY");
        assert!(r.write(b"no").is_err(), "opened for reading");
        assert!(File::open("/missing").is_err());
    }

    #[test]
    fn changes_are_remembered_by_path() {
        clear();
        load_file("/Tratto/Saved.ink", b"old".to_vec(), 5);
        assert!(take_changes().is_empty(), "what was loaded is no change");
        assert!(is_dir("/Tratto"));
        create_dir_all("/Tratto/Book/Section").unwrap();
        write("/Tratto/Book/Section/New.ink", b"new").unwrap();
        rename("/Tratto/Saved.ink", "/Tratto/Book/Saved.ink").unwrap();
        let changed = take_changes();
        for p in ["/Tratto/Book", "/Tratto/Book/Section/New.ink", "/Tratto/Saved.ink", "/Tratto/Book/Saved.ink"] {
            assert!(changed.contains(&PathBuf::from(p)), "{p} in {changed:?}");
        }
        assert!(entries("/Tratto/Saved.ink").is_empty());
        let book = entries("/Tratto/Book");
        assert_eq!(book[0], (PathBuf::from("/Tratto/Book"), None));
        assert!(book.contains(&(PathBuf::from("/Tratto/Book/Saved.ink"), Some(b"old".to_vec()))));
        assert!(take_changes().is_empty());
    }
}
