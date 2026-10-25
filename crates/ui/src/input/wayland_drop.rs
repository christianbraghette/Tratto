// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Files dropped onto the window on Linux/Wayland (`wl_data_device`).
//!
//! winit 0.30 does not implement drag and drop on Wayland, so a file dragged
//! onto the window gives egui no hovered or dropped files. Like
//! `wayland_pen`, this module:
//!
//! 1. wraps winit's live `wl_display`, so the drags over winit's surface are
//!    reported to us too,
//! 2. gets a `wl_data_device` for the seat on a private event queue,
//!    dispatched on a background thread,
//! 3. accepts drags that carry `text/uri-list` (files from a file manager),
//!    reads the list on drop and sends a [`DropEvent`] through a channel,
//! 4. is drained from `InkApp::raw_input_hook`, which turns the events into
//!    egui's `hovered_files` / `dropped_files`.
//!
//! On every other OS, and on X11, `WaylandDrop::start` returns `None`: winit
//! reports dropped files itself.

use std::path::{Path, PathBuf};

/// A file dropped onto the window, for egui (which only has a private one).
#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct DroppedPath(pub PathBuf);

impl eframe::egui::DroppedFile for DroppedPath {
    fn path(&self) -> &Path {
        &self.0
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }

    /// Never made on the web (the browser drops files of its own).
    #[cfg(target_arch = "wasm32")]
    fn bytes_async(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + '_>> {
        Box::pin(std::future::ready(Err("not a file of the browser".to_owned())))
    }
}

/// A drag of files over the window.
#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum DropEvent {
    /// Files are dragged over the window (their paths are known on drop).
    Enter,
    /// The drag left the window or was cancelled.
    Leave,
    /// The files were dropped.
    Drop(Vec<PathBuf>),
}

#[cfg(target_os = "linux")]
pub use imp::WaylandDrop;
#[cfg(not(target_os = "linux"))]
pub use stub::WaylandDrop;

/// The local paths of a `text/uri-list` (RFC 2483): one URI per line, `#`
/// starts a comment. `file://` URIs (with an empty or any host) are
/// percent-decoded; other schemes are skipped. Paths as bytes: on Linux they
/// need not be UTF-8.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn uri_list_paths(list: &str) -> Vec<Vec<u8>> {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|uri| {
            let rest = uri.strip_prefix("file://")?;
            // Skip the host ("", "localhost" or the machine's name).
            let path = &rest[rest.find('/')?..];
            Some(percent_decode(path))
        })
        .collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if let [b'%', hi, lo, ..] = bytes[i..]
            && let (Some(hi), Some(lo)) = (hex(hi), hex(lo))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(not(target_os = "linux"))]
mod stub {
    use super::DropEvent;
    use eframe::egui;

    pub struct WaylandDrop;

    impl WaylandDrop {
        pub fn start(_ctx: &egui::Context, _frame: &eframe::Frame) -> Option<Self> {
            None
        }
        pub fn try_iter(&self) -> std::iter::Empty<DropEvent> {
            std::iter::empty()
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::io::Read;
    use std::os::fd::AsFd;
    use std::os::unix::ffi::OsStringExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender, TryIter};

    use eframe::egui;
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    use wayland_client::{
        Connection, Dispatch, Proxy, QueueHandle,
        backend::{Backend, ObjectId},
        delegate_noop, event_created_child,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{
            wl_callback::WlCallback,
            wl_data_device::{self, WlDataDevice},
            wl_data_device_manager::{DndAction, WlDataDeviceManager},
            wl_data_offer::{self, WlDataOffer},
            wl_registry::WlRegistry,
            wl_seat::WlSeat,
        },
    };

    use super::{DropEvent, uri_list_paths};

    const URI_LIST: &str = "text/uri-list";

    pub struct WaylandDrop {
        rx: Receiver<DropEvent>,
        stop: Arc<AtomicBool>,
        conn: Connection,
        qh: QueueHandle<State>,
    }

    impl WaylandDrop {
        /// Returns `None` when not running on Wayland or when the compositor
        /// has no data device manager.
        pub fn start(ctx: &egui::Context, frame: &eframe::Frame) -> Option<Self> {
            let handle = frame.display_handle().ok()?;
            let RawDisplayHandle::Wayland(h) = handle.as_raw() else {
                return None;
            };

            // SAFETY: the display belongs to the live eframe window and
            // outlives this object (which is dropped together with the app).
            let backend = unsafe { Backend::from_foreign_display(h.display.as_ptr().cast()) };
            let conn = Connection::from_backend(backend);

            let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
            let qh = queue.handle();
            let seat: WlSeat = globals.bind(&qh, 1..=1, ()).ok()?;
            // Version 3 adds drag actions; version 1 compositors still drop.
            let manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).ok()?;
            let device = manager.get_data_device(&seat, &qh, ());

            let (tx, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));

            let mut state =
                State { tx, ctx: ctx.clone(), conn: conn.clone(), mime_types: HashMap::new(), current: None };
            let stop_thread = stop.clone();
            std::thread::Builder::new()
                .name("wayland-drop".into())
                .spawn(move || {
                    let _keep_alive = (seat, manager, device);
                    while !stop_thread.load(Ordering::Relaxed) {
                        if queue.blocking_dispatch(&mut state).is_err() {
                            break;
                        }
                    }
                })
                .ok()?;

            Some(Self { rx, stop, conn, qh })
        }

        pub fn try_iter(&self) -> TryIter<'_, DropEvent> {
            self.rx.try_iter()
        }
    }

    impl Drop for WaylandDrop {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            // Round-trip request wakes the blocked dispatch thread so it can exit.
            let _ = self.conn.display().sync(&self.qh, ());
            let _ = self.conn.flush();
        }
    }

    struct State {
        tx: Sender<DropEvent>,
        ctx: egui::Context,
        conn: Connection,
        /// The MIME types each offer announced (they come before `enter`).
        mime_types: HashMap<ObjectId, Vec<String>>,
        /// The offer dragged over the window, and whether it carries files.
        current: Option<(WlDataOffer, bool)>,
    }

    impl State {
        fn send(&self, event: DropEvent) {
            if self.tx.send(event).is_ok() {
                self.ctx.request_repaint();
            }
        }

        fn forget(&mut self, offer: &WlDataOffer) {
            self.mime_types.remove(&offer.id());
            offer.destroy();
        }
    }

    /// Reads the dropped URI list and sends the paths. The dragging client
    /// writes into the pipe once our `receive` request has reached it, so
    /// this runs on its own thread, not on the dispatching one.
    fn receive_files(offer: WlDataOffer, state: &State) {
        let Ok((read, write)) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) else {
            offer.destroy();
            return;
        };
        offer.receive(URI_LIST.into(), write.as_fd());
        drop(write);
        let _ = state.conn.flush();
        let (tx, ctx, conn) = (state.tx.clone(), state.ctx.clone(), state.conn.clone());
        let spawned = std::thread::Builder::new().name("wayland-drop-read".into()).spawn(move || {
            let mut list = Vec::new();
            let read_ok = std::fs::File::from(read).read_to_end(&mut list).is_ok();
            let list = String::from_utf8_lossy(&list);
            let paths = uri_list_paths(&list).into_iter().map(|p| OsString::from_vec(p).into()).collect();
            if read_ok && offer.version() >= 3 {
                offer.finish();
            }
            offer.destroy();
            let _ = conn.flush();
            if tx.send(DropEvent::Drop(paths)).is_ok() {
                ctx.request_repaint();
            }
        });
        if spawned.is_err() {
            state.send(DropEvent::Leave);
        }
    }

    impl Dispatch<WlDataDevice, ()> for State {
        fn event(
            state: &mut Self,
            _: &WlDataDevice,
            event: wl_data_device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            use wl_data_device::Event as E;
            match event {
                E::Enter { serial, id: Some(offer), .. } => {
                    let files = state.mime_types.get(&offer.id()).is_some_and(|m| m.iter().any(|m| m == URI_LIST));
                    if files {
                        offer.accept(serial, Some(URI_LIST.into()));
                        if offer.version() >= 3 {
                            offer.set_actions(DndAction::Copy, DndAction::Copy);
                        }
                        state.send(DropEvent::Enter);
                    } else {
                        offer.accept(serial, None);
                    }
                    if let Some((old, _)) = state.current.replace((offer, files)) {
                        state.forget(&old);
                    }
                }
                E::Leave => {
                    if let Some((offer, files)) = state.current.take() {
                        state.forget(&offer);
                        if files {
                            state.send(DropEvent::Leave);
                        }
                    }
                }
                E::Drop => match state.current.take() {
                    Some((offer, true)) => {
                        state.mime_types.remove(&offer.id());
                        receive_files(offer, state);
                    }
                    Some((offer, false)) => state.forget(&offer),
                    None => {}
                },
                // The clipboard is winit's (smithay-clipboard): its offers
                // to this device are not needed.
                E::Selection { id: Some(offer) } => state.forget(&offer),
                _ => {}
            }
        }

        event_created_child!(State, WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ())
        ]);
    }

    impl Dispatch<WlDataOffer, ()> for State {
        fn event(
            state: &mut Self,
            offer: &WlDataOffer,
            event: wl_data_offer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_data_offer::Event::Offer { mime_type } = event {
                state.mime_types.entry(offer.id()).or_default().push(mime_type);
            }
        }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    delegate_noop!(State: ignore WlSeat);
    delegate_noop!(State: ignore WlCallback);
    delegate_noop!(State: ignore WlDataDeviceManager);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_give_local_paths() {
        let list = "# from Files\r\nfile:///home/me/My%20Notes/a.pdf\r\nfile://localhost/tmp/b.pdf\r\n\
                    https://example.com/c.pdf\r\nfile:///tmp/%E2%82%AC%zz.pdf\r\n";
        let paths = uri_list_paths(list);
        assert_eq!(
            paths,
            vec![b"/home/me/My Notes/a.pdf".to_vec(), b"/tmp/b.pdf".to_vec(), "/tmp/€%zz.pdf".as_bytes().to_vec(),]
        );
    }

    #[test]
    fn non_utf8_escapes_are_kept_as_bytes() {
        assert_eq!(uri_list_paths("file:///tmp/%FF.pdf"), vec![b"/tmp/\xFF.pdf".to_vec()]);
    }
}
