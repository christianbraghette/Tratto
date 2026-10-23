// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Windows: the custom title bar on the system's window frame.
//!
//! The window keeps its decorations: Windows draws the frame (shadow,
//! rounded corners, border, the invisible resize margins), animates and
//! snaps it, and keeps it themed. While the app draws its own title bar
//! (`own_frame`) the window only tells Windows there is no caption: its
//! client area reaches up to the window's top (`WM_NCCALCSIZE`), where the
//! app's bar is. Turning the custom bar on or off only has the frame
//! measured again; the window's style never changes (each change had
//! Windows repaint its frame, unthemed, for a moment).
//!
//! The empty part of the title bar answers Windows' hit test
//! (`WM_NCHITTEST`) as the caption, and the top edge as the top border (the
//! other borders are the system's), so Windows moves and resizes the window
//! itself, with any input: mouse, touchpad, touch and pen.
//! (`ViewportCommand::StartDrag` / `BeginResize` only work while the left
//! mouse button is held: Windows ends a move at once for touch, pen and a
//! touchpad's tap and drag.) Snapping, a double click to maximize and the
//! system menu on a right click come with it.
//!
//! The window opens maximized and hidden from view (DWM cloaks it) until
//! its first frame is drawn (`open_maximized`, `reveal`): winit maximizes a
//! new window by showing it, so it appeared empty with the system's caption,
//! was hidden again and came back white for a frame.
//!
//! While the window goes in or out of full screen (`switching`) it paints
//! none of the system's frame (`WM_NCPAINT`): leaving full screen it is
//! briefly a normal window as large as the screen, and Windows painted its
//! unthemed frame around it for a frame or two. A window that swallows
//! `WM_NCPAINT` is no longer themed (the frame drawn later would be the
//! unthemed, light blue one), so once the switch is done the theme is
//! applied again.
//!
//! The chrome publishes the regions every frame (`publish_caption`,
//! `publish_border`); the window subclass reads them. Elsewhere, and if the
//! subclass cannot be installed, the chrome moves and resizes the window
//! through egui as before (`active` is false).

// The hit test is only Windows' (and its test's); elsewhere only the
// regions are kept.
#![cfg_attr(not(windows), allow(dead_code))]

use eframe::egui::{self, Rect};

/// Where the title bar can be grabbed (`None`: nowhere, e.g. while a menu is
/// open) and the widgets on it that keep their clicks; points.
#[derive(Clone, Default)]
pub struct Caption {
    pub bar: Option<Rect>,
    pub holes: Vec<Rect>,
}

/// The regions in client pixels: `[left, top, right, bottom]`.
#[derive(Clone, Default, Debug, PartialEq)]
struct Regions {
    caption: Option<[i32; 4]>,
    holes: Vec<[i32; 4]>,
    /// Height of the resize border along the top edge (the others are the
    /// system's); 0: none (maximized, fullscreen).
    border: i32,
}

// winuser.h hit test codes.
const HTCLIENT: isize = 1;
const HTCAPTION: isize = 2;
const HTTOP: isize = 12;
const HTTOPLEFT: isize = 13;
const HTTOPRIGHT: isize = 14;

fn pixels(r: Rect, ppp: f32) -> [i32; 4] {
    [r.left(), r.top(), r.right(), r.bottom()].map(|v| (v * ppp).round() as i32)
}

fn inside(r: &[i32; 4], (x, y): (i32, i32)) -> bool {
    x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
}

impl Regions {
    /// What the client pixel `p` of a window `size` pixels large is.
    fn hit(&self, p: (i32, i32), size: (i32, i32)) -> isize {
        let ((x, y), (w, _), b) = (p, size, self.border);
        if y < b {
            // The corners reach twice as far along the edge.
            return if x < 2 * b {
                HTTOPLEFT
            } else if x >= w - 2 * b {
                HTTOPRIGHT
            } else {
                HTTOP
            };
        }
        match &self.caption {
            Some(bar) if inside(bar, p) && !self.holes.iter().any(|h| inside(h, p)) => HTCAPTION,
            _ => HTCLIENT,
        }
    }
}

/// Windows moves and resizes the window (the subclass is installed).
pub fn active() -> bool {
    imp::active()
}

/// The app draws the window's title bar (the custom frame is on): the
/// window has no caption (see the module docs). Call it whenever it may
/// have changed; only a change has the frame measured again.
pub fn own_frame(on: bool) {
    imp::own_frame(on);
}

/// The window is going in or out of full screen: Windows paints none of
/// its frame meanwhile (see the module docs).
pub fn switching(on: bool) {
    imp::switching(on);
}

/// Maximizes the window, still hidden from view, before the first frame
/// (after `install`, so it is laid out without the caption).
pub fn open_maximized() {
    imp::open_maximized();
}

/// Shows the window opened by `open_maximized`: call it once a frame at
/// its maximized size is drawn (does nothing after the first call).
pub fn reveal() {
    imp::reveal();
}

/// Installs the subclass on the eframe window: before the first frame, so
/// that one is already laid out without the system's caption.
pub fn install(window: &impl raw_window_handle::HasWindowHandle) {
    imp::install(window);
}

/// The title bar of this frame (call every frame the bar is drawn; a frame
/// without it has no caption).
pub fn publish_caption(ctx: &egui::Context, caption: Caption) {
    // Not inside `data_mut`: the context is locked there.
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("win_frame_caption"), (frame, caption)));
}

/// The resize borders (`None`: none) of this frame; also takes the title
/// bar published this frame. Call once per frame, after the title bar.
pub fn publish_border(ctx: &egui::Context, border: Option<f32>) {
    let ppp = ctx.pixels_per_point();
    let frame = ctx.cumulative_frame_nr();
    let caption = ctx
        .data(|d| d.get_temp::<(u64, Caption)>(egui::Id::new("win_frame_caption")))
        .filter(|(nr, _)| *nr == frame)
        .map(|(_, c)| c)
        .unwrap_or_default();
    imp::set(Regions {
        caption: caption.bar.map(|r| pixels(r, ppp)),
        holes: caption.holes.iter().map(|r| pixels(*r, ppp)).collect(),
        border: border.map_or(0, |b| (b * ppp).round().max(1.0) as i32),
    });
}

#[cfg(windows)]
mod imp {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};

    use super::{HTCLIENT, Regions};

    const WM_NCCALCSIZE: u32 = 0x0083;
    const WM_NCHITTEST: u32 = 0x0084;
    const WM_NCPAINT: u32 = 0x0085;
    const GWL_STYLE: i32 = -16;
    const WS_CAPTION: i32 = 0x00C0_0000;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    const SWP_NOOWNERZORDER: u32 = 0x0200;
    const SUBCLASS_ID: usize = 0x494E_4B32; // "INK2"

    #[link(name = "user32")]
    unsafe extern "system" {
        fn ScreenToClient(hwnd: HWND, point: *mut POINT) -> i32;
        fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
        fn GetWindowLongW(hwnd: HWND, index: i32) -> i32;
        fn IsZoomed(hwnd: HWND) -> i32;
        // As declared in `chrome` (the same function, the same signature).
        fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    }

    // As declared in `chrome`.
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }
    const DWMWA_CLOAK: u32 = 13;
    const SW_MAXIMIZE: i32 = 3;

    #[link(name = "uxtheme")]
    unsafe extern "system" {
        fn SetWindowTheme(hwnd: HWND, app_name: *const u16, id_list: *const u16) -> i32;
    }

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static OWN_FRAME: AtomicBool = AtomicBool::new(false);
    static SWITCHING: AtomicBool = AtomicBool::new(false);
    /// Opened cloaked, not shown yet.
    static CLOAKED: AtomicBool = AtomicBool::new(false);
    /// The window, once the subclass is installed.
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static REGIONS: Mutex<Regions> = Mutex::new(Regions { caption: None, holes: Vec::new(), border: 0 });

    pub fn active() -> bool {
        ACTIVE.load(Ordering::Relaxed)
    }

    pub fn own_frame(on: bool) {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if OWN_FRAME.swap(on, Ordering::Relaxed) != on && hwnd != 0 {
            let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
            // SAFETY: our own window, on its thread; it only measures its
            // frame again.
            unsafe { SetWindowPos(hwnd, 0, 0, 0, 0, 0, flags) };
        }
    }

    fn cloak(hwnd: isize, on: bool) -> bool {
        let value = i32::from(on);
        // SAFETY: our own window and a BOOL, as the attribute expects.
        unsafe { DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, (&raw const value).cast(), 4) == 0 }
    }

    pub fn open_maximized() {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if hwnd == 0 {
            return;
        }
        // Not cloaked (Windows 7): maximized as it is shown, as before.
        CLOAKED.store(cloak(hwnd, true), Ordering::Relaxed);
        // SAFETY: our own window, on its thread.
        unsafe { ShowWindow(hwnd, SW_MAXIMIZE) };
    }

    pub fn reveal() {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if CLOAKED.swap(false, Ordering::Relaxed) && hwnd != 0 {
            cloak(hwnd, false);
        }
    }

    pub fn switching(on: bool) {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if SWITCHING.swap(on, Ordering::Relaxed) && !on && hwnd != 0 {
            // The default theme back (null names); Windows tells the window
            // with `WM_THEMECHANGED`.
            // SAFETY: our own window, on its thread.
            unsafe { SetWindowTheme(hwnd as HWND, std::ptr::null(), std::ptr::null()) };
        }
    }

    pub fn set(regions: Regions) {
        if let Ok(mut r) = REGIONS.lock()
            && *r != regions
        {
            *r = regions;
        }
    }

    /// The client area Windows measured with its frame (`client`, of a
    /// window whose top-left corner is `window`), without the caption: up to
    /// the window's top or, maximized, the screen's (the window reaches past
    /// it by its frame, as wide as the side borders).
    fn without_caption(client: &mut RECT, window: (i32, i32), maximized: bool) {
        let frame = client.left - window.0;
        client.top = if maximized { window.1 + frame } else { window.1 };
    }

    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if msg == WM_NCPAINT && SWITCHING.load(Ordering::Relaxed) {
            return 0;
        }
        // The system's frame without its caption, while the window has one
        // (not in full screen).
        // SAFETY: our own window.
        let captioned = msg == WM_NCCALCSIZE && unsafe { GetWindowLongW(hwnd, GWL_STYLE) } & WS_CAPTION == WS_CAPTION;
        if captioned && OWN_FRAME.load(Ordering::Relaxed) {
            // SAFETY: `lparam` is an NCCALCSIZE_PARAMS (`wparam` set) or a
            // RECT: either way it starts with the window's rectangle, which
            // becomes the client's.
            let client = unsafe { &mut *(lparam as *mut RECT) };
            let window = (client.left, client.top);
            // SAFETY: forwarding to the next window procedure (Windows
            // measures its frame).
            let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            // SAFETY: our own window.
            let maximized = unsafe { IsZoomed(hwnd) != 0 };
            without_caption(client, window, maximized);
            return result;
        }
        // SAFETY: forwarding to the next window procedure, as the subclass API asks.
        let default = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        if msg != WM_NCHITTEST || default != HTCLIENT {
            return default;
        }
        // Screen coordinates, signed 16 bits each (other monitors may be negative).
        let mut p = POINT { x: (lparam & 0xFFFF) as i16 as i32, y: ((lparam >> 16) & 0xFFFF) as i16 as i32 };
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        // SAFETY: our own window; the out-parameters are valid.
        let placed = unsafe { ScreenToClient(hwnd, &mut p) != 0 && GetClientRect(hwnd, &mut rect) != 0 };
        match REGIONS.lock() {
            Ok(r) if placed => r.hit((p.x, p.y), (rect.right, rect.bottom)),
            _ => default,
        }
    }

    pub fn install(window: &impl HasWindowHandle) {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        // SAFETY: the HWND of our own window, on its thread.
        let ok = unsafe { SetWindowSubclass(h.hwnd.get() as HWND, Some(subclass_proc), SUBCLASS_ID, 0) != 0 };
        if ok {
            WINDOW.store(h.hwnd.get(), Ordering::Relaxed);
            // The caption may be off already: measured again.
            let on = OWN_FRAME.swap(false, Ordering::Relaxed);
            own_frame(on);
        }
        ACTIVE.store(ok, Ordering::Relaxed);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_caption_goes_the_borders_stay() {
            // Windows' client area: 8 px borders, a 31 px caption.
            let mut c = RECT { left: 108, top: 139, right: 892, bottom: 692 };
            without_caption(&mut c, (100, 100), false);
            assert_eq!((c.left, c.top, c.right, c.bottom), (108, 100, 892, 692));
            // Maximized at -8, -8: up to the screen's top.
            let mut c = RECT { left: 0, top: 23, right: 1920, bottom: 1032 };
            without_caption(&mut c, (-8, -8), true);
            assert_eq!(c.top, 0);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Regions;

    pub fn active() -> bool {
        false
    }

    pub fn own_frame(_on: bool) {}

    pub fn switching(_on: bool) {}

    pub fn set(_regions: Regions) {}

    pub fn install(_window: &impl raw_window_handle::HasWindowHandle) {}

    pub fn open_maximized() {}

    pub fn reveal() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_holes_and_top_border() {
        let r = Regions { caption: Some([0, 0, 800, 32]), holes: vec![[0, 0, 200, 32], [700, 0, 800, 32]], border: 5 };
        let size = (800, 600);
        assert_eq!(r.hit((400, 16), size), HTCAPTION);
        assert_eq!(r.hit((100, 16), size), HTCLIENT, "a menu");
        assert_eq!(r.hit((750, 16), size), HTCLIENT, "a window button");
        assert_eq!(r.hit((400, 300), size), HTCLIENT);
        assert_eq!(r.hit((400, 2), size), HTTOP);
        assert_eq!(r.hit((8, 2), size), HTTOPLEFT, "the corner reaches along the edge");
        assert_eq!(r.hit((795, 3), size), HTTOPRIGHT);
        // The other edges are the system's (outside the client area).
        assert_eq!(r.hit((2, 300), size), HTCLIENT);
        assert_eq!(r.hit((798, 598), size), HTCLIENT);
        // Maximized: no borders; a menu open: no caption.
        let r = Regions { border: 0, caption: None, ..r };
        assert_eq!(r.hit((400, 2), size), HTCLIENT);
    }
}
