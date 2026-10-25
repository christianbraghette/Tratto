// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The open note's lifecycle: opening (in the background when large), showing,
//! saving, closing, and PDFs read, imported or dropped onto the window.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};

use eframe::egui::{self, Color32, Id, Vec2, vec2};

use tratto_core::library::{self, Library};
use tratto_core::model::Document;
use tratto_core::pdf_pages::{self, PdfFile};
use tratto_core::storage::{self, NoteMeta, NoteWriter};

// `Instant` that works in the browser too (std's panics there).
use web_time::Instant;

use crate::settings::{NoteView, Sidebar};
use crate::theme;
use crate::toolbar::Tool;

use crate::saver::Saved;

use super::{Fingerprint, InkApp, Loaded, Loading, Screen, is_pdf};

/// Notes larger than this (bytes) are read in the background: notes written
/// on a PDF.
const LOAD_IN_BACKGROUND: u64 = 4 * 1024 * 1024;

/// The library folder being read in the background (see `refresh_library`).
pub(super) struct LibraryScan {
    done: mpsc::Receiver<Result<Library, String>>,
    /// Asked for again meanwhile: read once more when it is done.
    again: bool,
}

/// Why the note at `path` cannot be opened; for a damaged one, where its
/// backup is.
fn cannot_open(path: &Path, e: &storage::StorageError) -> String {
    let bak = path.with_extension("ink.bak");
    match e {
        storage::StorageError::Corrupt if tratto_core::fs::is_file(&bak) => {
            let name = bak.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            format!("Cannot open note: {e}. Its last backup is next to it: rename \u{201c}{name}\u{201d} to .ink")
        }
        _ => format!("Cannot open note: {e}"),
    }
}

/// The library at `root`, with the folders outside it shown as notebooks.
fn scan_library(root: &Path, external: &[PathBuf]) -> Result<Library, String> {
    let mut library = Library::scan(root).map_err(|e| e.to_string())?;
    for nb in external {
        library.add_notebook(nb);
    }
    Ok(library)
}

impl InkApp {
    pub(super) fn toast(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_at = Some(Instant::now());
    }

    pub(super) fn library_root(&self) -> PathBuf {
        PathBuf::from(&self.settings.library_root)
    }

    /// Re-reads the library folder on a background thread (it reads the
    /// header of every note: a large library, or one on a network drive,
    /// would hold up the window). The library shown stays until then.
    pub(super) fn refresh_library(&mut self) {
        if let Some(scan) = &mut self.library_scan {
            // It may have read the folder before the change: once more after it.
            scan.again = true;
            return;
        }
        // Gone meanwhile (deleted, renamed elsewhere): no longer shown.
        self.external_notebooks.retain(|p| tratto_core::fs::is_dir(p));
        let (root, external) = (self.library_root(), self.external_notebooks.clone());
        let (tx, done) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let work = move || {
            let _ = tx.send(scan_library(&root, &external));
            ctx.request_repaint();
        };
        match crate::background::spawn("library scan", work) {
            Ok(_) => self.library_scan = Some(LibraryScan { done, again: false }),
            Err(_) => self.refresh_library_now(),
        }
    }

    /// Re-reads the library folder now, for what needs it as it is on disk.
    pub(super) fn refresh_library_now(&mut self) {
        self.external_notebooks.retain(|p| tratto_core::fs::is_dir(p));
        let scanned = scan_library(&self.library_root(), &self.external_notebooks);
        self.library_scanned(scanned);
    }

    /// The library is being read (see `refresh_library`).
    pub(super) fn library_refreshing(&self) -> bool {
        self.library_scan.is_some()
    }

    /// Shows the library read in the background once it is ready.
    pub(super) fn poll_library(&mut self) {
        let Some(scan) = &self.library_scan else { return };
        let scanned = match scan.done.try_recv() {
            Ok(scanned) => scanned,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the scan stopped unexpectedly".to_owned()),
        };
        let again = scan.again;
        self.library_scan = None;
        self.library_scanned(scanned);
        if again {
            self.refresh_library();
        }
    }

    fn library_scanned(&mut self, scanned: Result<Library, String>) {
        let root = self.library_root();
        match scanned {
            // Read before the library folder was changed: a newer scan follows.
            Ok(l) if l.root != root => {}
            Ok(l) => self.library = l,
            Err(e) => {
                self.toast(format!("Cannot read library folder: {e}"));
                self.library = Library::empty(root);
            }
        }
        self.library_loaded = true;
    }

    /// Opens a path given on the command line: a PDF in the reader, a note
    /// in the editor, or a folder as a notebook in the library. A folder
    /// outside the library is shown as a notebook for this session.
    pub(super) fn open_path(&mut self, ctx: &egui::Context, path: &Path) {
        let is_note = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ink"));
        if tratto_core::fs::is_dir(path) {
            self.open_folder(path);
        } else if tratto_core::fs::is_file(path) && is_pdf(path) {
            self.open_pdf(ctx, path);
        } else if tratto_core::fs::is_file(path) && is_note {
            self.open_note(path);
        } else if !tratto_core::fs::exists(path) {
            self.toast(format!("Cannot open \u{201c}{}\u{201d}: it does not exist", path.display()));
        } else {
            self.toast(format!("Cannot open \u{201c}{}\u{201d}: not a PDF, a note or a folder", path.display()));
        }
    }

    /// Shows a folder in the library: a notebook or section of the library,
    /// else the folder as an external notebook.
    fn open_folder(&mut self, path: &Path) {
        let root = self.library_root();
        let (notebook, section) = match path.parent() {
            Some(nb) if nb.parent() == Some(root.as_path()) => (nb.to_path_buf(), Some(path.to_path_buf())),
            _ => (path.to_path_buf(), None),
        };
        // Other folders of the library (the library itself) show its home.
        if !notebook.starts_with(&root) && !self.external_notebooks.contains(&notebook) {
            self.external_notebooks.push(notebook.clone());
        }
        self.refresh_library();
        self.lib_ui.notebook = Some(notebook);
        self.lib_ui.section = section;
        self.screen = Screen::Library;
    }

    fn current_fingerprint(&self) -> Fingerprint {
        (
            self.history.version(),
            self.doc.page_width.to_bits(),
            self.doc.page_height.to_bits(),
            self.doc.page.kind as u8,
            self.doc.page.spacing.to_bits(),
        )
    }

    /// Marks the note dirty when anything changed since the last check.
    pub(super) fn sync_dirty(&mut self) {
        let fp = self.current_fingerprint();
        if fp != self.last_fp {
            self.last_fp = fp;
            self.dirty = true;
            self.last_change = Instant::now();
        }
    }

    /// Writes the open note and waits until it is on disk (with any save
    /// still being written). Returns false (and shows why) on failure.
    pub(super) fn save_note(&mut self, announce: bool) -> bool {
        let Some(path) = self.note_path.clone() else {
            return false;
        };
        self.saver.save(path, self.note_meta.clone(), self.doc.clone());
        self.dirty = false;
        let saved = self.finish_saves();
        if saved && announce {
            self.toast("Saved");
        }
        saved
    }

    /// Writes the open note first if it is `path` and changed, for what
    /// reads its file (moving or copying it). False (shown) if that failed.
    pub(super) fn save_if_open(&mut self, path: &Path) -> bool {
        self.sync_dirty();
        self.note_path.as_deref() != Some(path) || !self.dirty || self.save_note(false)
    }

    /// Saves the open note in the background (autosave). While a save is
    /// being written, the note stays dirty and is saved after it.
    pub(super) fn autosave(&mut self) {
        let Some(path) = self.note_path.clone() else { return };
        if self.saver.busy() {
            return;
        }
        // Cheap: the strokes' points are shared, not copied.
        self.saver.save(path, self.note_meta.clone(), self.doc.clone());
        self.dirty = false;
    }

    /// Takes the results of the saves finished in the background.
    pub(super) fn poll_saves(&mut self) {
        let saves = self.saver.poll();
        self.apply_saves(saves);
    }

    /// Waits for the saves still being written. False if the last one failed.
    pub(super) fn finish_saves(&mut self) -> bool {
        let saves = self.saver.wait();
        self.apply_saves(saves)
    }

    /// What finished saves mean for the open note. False if the last failed.
    fn apply_saves(&mut self, saves: Vec<Saved>) -> bool {
        let mut ok = true;
        for saved in saves {
            let open = self.note_path.as_ref() == Some(&saved.path);
            match saved.result {
                // Only what the save derives from the document: the title may
                // have been changed since.
                Ok(meta) if open => {
                    let m = &mut self.note_meta;
                    m.modified = meta.modified;
                    m.pages = meta.pages;
                    m.preview = meta.preview;
                    if m.auto_title && meta.auto_title {
                        m.title = meta.title;
                    }
                    self.close_unsaved = false;
                    ok = true;
                }
                Ok(_) => ok = true,
                Err(e) => {
                    self.toast(format!("Save failed: {e}"));
                    // Tried again at the next autosave.
                    self.dirty |= open;
                    ok = false;
                }
            }
        }
        ok
    }

    /// Opens a note. A large one (written on a PDF) is read in the background
    /// behind a loading screen, so the window never freezes.
    pub(super) fn open_note(&mut self, path: &Path) {
        // It may be the note being saved.
        self.finish_saves();
        let large = tratto_core::fs::metadata(path).is_ok_and(|m| m.len() > LOAD_IN_BACKGROUND);
        if large {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let path = path.to_path_buf();
            self.load_in_background(format!("Opening \u{201c}{name}\u{201d}..."), move || Ok(path));
            return;
        }
        match storage::open_note(path) {
            Ok((meta, doc, writer)) => self.show_note(path, meta, doc, writer),
            Err(e) => self.toast(cannot_open(path, &e)),
        }
    }

    /// Makes a loaded note the open one, in the editor, where it was left.
    fn show_note(&mut self, path: &Path, meta: NoteMeta, doc: Document, writer: NoteWriter) {
        // A reader-only window whose PDF is written on (imported) becomes
        // the full app.
        self.standalone = None;
        self.doc = doc;
        self.note_meta = meta;
        self.saver.set_writer(writer);
        self.note_path = Some(path.to_path_buf());
        self.ensure_pages();
        self.reset_editing_state();
        // A note always opens on the pointer tool: nothing is written by
        // a stray touch before a tool is picked.
        self.pick_tool(Tool::Pointer);
        self.saved_tool = None;
        self.reader = None;
        let key = path.to_string_lossy();
        // A PDF just imported from the reader: where it was being read.
        let carried = self.carry_view.take();
        if let Some(view) = self.settings.note_view(&key).or(carried) {
            // The canvas clamps it to the note (e.g. pages deleted elsewhere).
            self.scroll = vec2(view.scroll[0], view.scroll[1]);
            self.settings.zoom = view.zoom;
        }
        self.settings.last_note = Some(key.into_owned());
        self.last_fp = self.current_fingerprint();
        self.dirty = false;
        self.screen = Screen::Editor;
    }

    /// Opens a PDF in the reader (read-only), read in the background. The
    /// open note is saved and closed first; stays put if that fails. A
    /// reader-only window keeps its PDF until the new one is read.
    pub(super) fn open_pdf(&mut self, ctx: &egui::Context, path: &Path) {
        if self.standalone.is_some() {
            self.remember_view();
        } else if self.screen == Screen::Editor {
            self.close_note(ctx);
            if self.screen == Screen::Editor {
                return;
            }
        }
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let path = path.to_path_buf();
        self.in_background(format!("Opening \u{201c}{name}\u{201d}..."), move || {
            let failed = |e: String| format!("Cannot open the PDF: {e}");
            let file = PdfFile::new(tratto_core::fs::read(&path).map_err(|e| failed(e.to_string()))?);
            let sizes = pdf_pages::page_sizes(&file).map_err(|e| failed(e.to_string()))?;
            if sizes.is_empty() {
                return Err(failed("it has no pages".into()));
            }
            Ok(Loaded::Pdf(path, Document::on_pdf(file, sizes)))
        });
    }

    /// Shows a PDF read for the reader, where it was left.
    fn show_pdf(&mut self, path: &Path, doc: Document) {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.doc = doc;
        self.note_meta = NoteMeta::new(&name, false);
        self.saver.set_writer(NoteWriter::default());
        self.note_path = None;
        self.reader = Some(path.to_path_buf());
        self.ensure_pages();
        self.reset_editing_state();
        let key = path.to_string_lossy().into_owned();
        if let Some(view) = self.settings.note_view(&key) {
            self.scroll = vec2(view.scroll[0], view.scroll[1]);
            self.settings.zoom = view.zoom;
        }
        // The note settings change a note; a PDF is read as it is.
        if self.settings.sidebar == Sidebar::Settings {
            self.settings.sidebar = Sidebar::Pages;
        }
        self.settings.pdf_opened(&key);
        if let Some(s) = &mut self.standalone {
            s.pdfs.retain(|p| *p != key);
            s.pdfs.push(key.clone());
        }
        self.settings.last_note = Some(key);
        self.last_fp = self.current_fingerprint();
        self.dirty = false;
        self.screen = Screen::Editor;
    }

    /// Turns the PDF in the reader into a note to write on, in the notebook
    /// or section shown in the library, and opens it where it was being read.
    pub(super) fn import_open_pdf(&mut self) {
        let Some(src) = self.reader.clone() else { return };
        self.remember_view();
        let scroll = self.kinetic.target().unwrap_or(self.scroll);
        self.carry_view = Some(NoteView { scroll: [scroll.x, scroll.y], zoom: self.settings.zoom });
        let dir = self.library_target_dir();
        self.import_pdf(dir, src);
    }

    /// Remembers where the open note (or PDF in the reader) is left (scroll
    /// and zoom), and that it is the open one.
    pub(super) fn remember_view(&mut self) {
        let open = self.note_path.as_ref().or(self.reader.as_ref());
        let Some(path) = open.map(|p| p.to_string_lossy().into_owned()) else { return };
        // The place the view is gliding to, if it is.
        let scroll = self.kinetic.target().unwrap_or(self.scroll);
        let view = NoteView { scroll: [scroll.x, scroll.y], zoom: self.settings.zoom };
        self.settings.set_note_view(&path, view);
        self.settings.last_note = Some(path);
    }

    /// Runs `find` (e.g. importing a PDF, which returns the new note) and
    /// reads the note it names on a background thread; the note opens when
    /// it is ready. `label` is shown on the loading screen meanwhile.
    fn load_in_background(&mut self, label: String, find: impl FnOnce() -> Result<PathBuf, String> + Send + 'static) {
        self.in_background(label, move || {
            find().and_then(|path| match storage::open_note(&path) {
                Ok((meta, doc, writer)) => Ok(Loaded::Note(path, meta, doc, writer)),
                Err(e) => Err(cannot_open(&path, &e)),
            })
        });
    }

    /// Runs `load` on a background thread behind the loading screen (showing
    /// `label`); what it read opens when it is ready.
    fn in_background(&mut self, label: String, load: impl FnOnce() -> Result<Loaded, String> + Send + 'static) {
        let (tx, done) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let spawned = crate::background::spawn("load-note", move || {
            let _ = tx.send(load());
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => self.loading = Some(Loading { label, done }),
            Err(e) => self.toast(format!("Cannot open note: {e}")),
        }
    }

    /// Opens the note loaded in the background once it is ready.
    pub(super) fn poll_loading(&mut self) {
        let Some(loading) = &self.loading else { return };
        let loaded = match loading.done.try_recv() {
            Ok(loaded) => loaded,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Cannot open note: the loading stopped unexpectedly".into()),
        };
        self.loading = None;
        match loaded {
            Ok(Loaded::Note(path, meta, doc, writer)) => {
                self.refresh_library();
                self.show_note(&path, meta, doc, writer);
            }
            Ok(Loaded::Pdf(path, doc)) => self.show_pdf(&path, doc),
            Err(e) => {
                // A reader-only window with nothing to show becomes the full app.
                if self.screen == Screen::Library {
                    self.standalone = None;
                }
                self.carry_view = None;
                self.refresh_library();
                self.toast(e);
            }
        }
    }

    /// Imports a PDF into `dir` in the background and opens the new note.
    pub(super) fn import_pdf(&mut self, dir: PathBuf, src: PathBuf) {
        let name = src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.load_in_background(format!("Importing \u{201c}{name}\u{201d}..."), move || {
            library::import_pdf(&dir, &src).map_err(|e| format!("Cannot import the PDF: {e}"))
        });
    }

    /// Saves and returns to the library. Stays in the editor if saving fails,
    /// so nothing typed or drawn can be lost. A PDF in the reader has
    /// nothing to save.
    pub(super) fn close_note(&mut self, ctx: &egui::Context) {
        if self.reader.is_none() {
            if self.edit.is_some() {
                self.commit_edit(ctx);
            }
            self.sync_dirty();
            if self.dirty && !self.save_note(false) {
                return;
            }
        }
        self.remember_view();
        // Closed by hand: the app starts in the library next time.
        self.settings.last_note = None;
        self.note_path = None;
        self.reader = None;
        self.context_menu = None;
        self.export_dialog = None;
        self.doc = Document::default();
        self.saver.set_writer(NoteWriter::default());
        self.reset_editing_state();
        self.screen = Screen::Library;
        self.refresh_library();
    }

    /// A PDF dropped onto the window opens in the reader. While a file is
    /// dragged over the window, a hint says so.
    pub(super) fn dropped_pdf(&mut self, ctx: &egui::Context) {
        let (hovering, dropped) = ctx.input(|i| {
            let hovering = i.raw.hovered_files.iter().any(|f| f.path.as_deref().is_none_or(is_pdf));
            let dropped = i.raw.dropped_files.iter().map(|f| f.path()).find(|p| is_pdf(p)).map(Path::to_path_buf);
            (hovering, dropped)
        });
        if hovering {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("pdf_drop_hint")));
            let screen = ctx.viewport_rect();
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(120));
            theme::paint_toast(&painter, screen.center(), "Drop the PDF to read it");
        }
        if let Some(src) = dropped {
            self.open_pdf(ctx, &src);
        }
    }

    /// The web: hands the note file `path` to the browser as a download
    /// (through `web_files::DOWNLOADS`, see there).
    #[cfg(target_arch = "wasm32")]
    pub(super) fn download_note(&mut self, path: &Path) {
        let Some(name) = path.file_name() else { return };
        tratto_core::fs::memory::load_dir(crate::web_files::DOWNLOADS);
        let to = Path::new(crate::web_files::DOWNLOADS).join(name);
        match tratto_core::fs::copy(path, &to) {
            Ok(_) => self.toast(format!("Downloading {}", name.to_string_lossy())),
            Err(e) => self.toast(format!("Download failed: {e}")),
        }
    }

    fn reset_editing_state(&mut self) {
        self.cache.clear();
        self.thumb_cache.clear();
        self.pdf_textures.clear();
        self.pdf_nav.clear();
        self.nav_back.clear();
        self.outline_open.clear();
        self.page_drag = None;
        self.picture = None;
        self.picture_drag = None;
        self.picture_textures.clear();
        self.edit = None;
        self.text_press = None;
        self.text_layer.clear();
        self.text_sel = None;
        self.text_gesture = None;
        self.search.reset();
        self.history.clear();
        self.active = None;
        self.erase_session.clear();
        self.last_erase_pos = None;
        self.scroll = Vec2::ZERO;
        self.kinetic.stop();
    }

    /// Guarantees that every stroke lies on an existing page (e.g. after an
    /// undo that restores a stroke whose page was removed).
    pub(super) fn ensure_pages(&mut self) {
        let d = &self.doc;
        let pages = d.strokes.iter().map(|s| s.page).chain(d.texts.iter().map(|t| t.page));
        let need = pages.chain(d.images.iter().map(|i| i.page)).max().map_or(1, |p| p + 1);
        if self.doc.page_count < need {
            self.doc.page_count = need;
            // Plain paper under the pages added (the list is one per page).
            if let Some(pdf) = &mut self.doc.pdf {
                pdf.pages.resize(need as usize, None);
            }
        }
    }
}
