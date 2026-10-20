// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saves the open note on a background thread, so that writing it never
//! holds up a frame.
//!
//! One thread and one queue: saves reach the disk in the order they were
//! asked for, each with the writer of the note open at that time (see
//! `NoteWriter`). A save gets a copy of the document, which is cheap: the
//! strokes' points are shared. [`Saver::wait`] returns once everything
//! asked for is written, for whatever must not overlap a save (closing the
//! app or the note, moving or renaming files).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui;
use tratto_core::model::Document;
use tratto_core::storage::{NoteMeta, NoteWriter};

enum Job {
    /// The writer of the note opened from now on.
    Writer(NoteWriter),
    Save { path: PathBuf, meta: NoteMeta, doc: Document },
}

/// A save that finished.
pub struct Saved {
    pub path: PathBuf,
    /// The meta as written (modified time, page count, preview and, for an
    /// auto-titled note, the title), or why the save failed.
    pub result: Result<NoteMeta, String>,
}

pub struct Saver {
    jobs: Sender<Job>,
    done: Receiver<Saved>,
    /// Saves asked for whose result has not been taken yet.
    pending: usize,
    /// Saves that could not be handed to the thread (it stopped).
    lost: Vec<Saved>,
}

impl Saver {
    /// Starts the thread; it wakes `ctx` whenever a save is done.
    pub fn new(ctx: egui::Context) -> Self {
        let (jobs, todo) = mpsc::channel::<Job>();
        let (finished, done) = mpsc::channel();
        thread::Builder::new()
            .name("note saver".to_owned())
            .spawn(move || {
                let mut writer = NoteWriter::default();
                for job in todo {
                    match job {
                        Job::Writer(w) => writer = w,
                        Job::Save { path, mut meta, doc } => {
                            let result = writer.save(&path, &mut meta, &doc).map(|()| meta).map_err(|e| e.to_string());
                            if finished.send(Saved { path, result }).is_err() {
                                return;
                            }
                            ctx.request_repaint();
                        }
                    }
                }
            })
            .expect("cannot start the thread that saves notes");
        Self { jobs, done, pending: 0, lost: Vec::new() }
    }

    /// The writer of the note opened from now on (after the saves already
    /// asked for).
    pub fn set_writer(&mut self, writer: NoteWriter) {
        let _ = self.jobs.send(Job::Writer(writer));
    }

    /// Saves `doc` to `path` in the background; `meta` is updated there (see
    /// `NoteWriter::save`) and comes back with the result.
    pub fn save(&mut self, path: PathBuf, meta: NoteMeta, doc: Document) {
        match self.jobs.send(Job::Save { path, meta, doc }) {
            Ok(()) => self.pending += 1,
            Err(mpsc::SendError(job)) => {
                if let Job::Save { path, .. } = job {
                    self.lost.push(Saved { path, result: Err("the saving thread stopped".to_owned()) });
                }
            }
        }
    }

    /// A save is being written.
    pub fn busy(&self) -> bool {
        self.pending > 0
    }

    /// The saves finished since the last call, in order.
    pub fn poll(&mut self) -> Vec<Saved> {
        let mut out = std::mem::take(&mut self.lost);
        while self.pending > 0 {
            match self.done.try_recv() {
                Ok(saved) => {
                    self.pending -= 1;
                    out.push(saved);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => self.thread_stopped(&mut out),
            }
        }
        out
    }

    /// Waits until every save asked for is written; their results, in order.
    pub fn wait(&mut self) -> Vec<Saved> {
        let mut out = std::mem::take(&mut self.lost);
        while self.pending > 0 {
            match self.done.recv() {
                Ok(saved) => {
                    self.pending -= 1;
                    out.push(saved);
                }
                Err(mpsc::RecvError) => self.thread_stopped(&mut out),
            }
        }
        out
    }

    /// The thread is gone (a bug made it panic): what it still owed failed.
    fn thread_stopped(&mut self, out: &mut Vec<Saved>) {
        for _ in 0..self.pending {
            out.push(Saved { path: PathBuf::new(), result: Err("the saving thread stopped".to_owned()) });
        }
        self.pending = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tratto_core::model::{Point, Stroke, StrokeKind};
    use tratto_core::storage;

    #[test]
    fn saves_in_order_in_the_background() {
        let path = std::env::temp_dir().join("tratto_saver_order.ink");
        let mut saver = Saver::new(egui::Context::default());
        let mut doc = Document::default();
        saver.set_writer(NoteWriter::default());
        for i in 0..5 {
            let id = doc.alloc_id();
            let pts = vec![Point::new(i as f32, 0.0, 0.5), Point::new(i as f32, 9.0, 0.5)];
            doc.strokes.push(Stroke::new(id, 0, StrokeKind::Pen, [0, 0, 0, 255], 2.0, pts));
            saver.save(path.clone(), NoteMeta::new(&format!("Version {i}"), false), doc.clone());
        }
        assert!(saver.busy() || !saver.poll().is_empty());
        let mut done = saver.poll();
        done.extend(saver.wait());
        assert!(!saver.busy());
        let titles: Vec<String> = done.iter().map(|s| s.result.as_ref().unwrap().title.clone()).collect();
        assert_eq!(titles, (0..5).map(|i| format!("Version {i}")).collect::<Vec<_>>());
        // The last save wins, written by appending to the first.
        let (meta, loaded) = storage::load_note(&path).unwrap();
        assert_eq!((meta.title.as_str(), loaded.strokes.len()), ("Version 4", 5));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("ink.bak"));
    }
}
