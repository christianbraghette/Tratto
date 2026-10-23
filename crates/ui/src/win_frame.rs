// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Windows: moving and resizing the window drawn with the custom frame.
//!
//! The empty part of the title bar answers Windows' hit test
//! (`WM_NCHITTEST`) as the caption, and the window's edges as its borders,
//! so Windows moves and resizes the window itself, with any input: mouse,
//! touchpad, touch and pen. (`ViewportCommand::StartDrag` / `BeginResize`
//! only work while the left mouse button is held: Windows ends a move at
//! once for touch, pen and a touchpad's tap and drag.) Snapping, a double
//! click to maximize and the system menu on a right click come with it.
//!
//! The chrome publishes the regions every frame (`publish_caption`,
//! `publish_border`); the window subclass reads them. Elsewhere, and if the
//! subclass cannot be installed, the chrome moves and resizes the window
//! through egui as before (`active` is false).
//!
//! While the app draws its own title bar (`own_frame`), the subclass also
//! keeps Windows from repainting the system's when the window is activated.
//! While the window goes in or out of full screen (`switching`) it paints
//! none of the system's frame at all (`WM_NCPAINT`): leaving full screen,
//! Windows painted its unthemed frame around the window for a frame or two.
//! A window that swallows `WM_NCPAINT` is no longer themed, and the
//! caption Windows then paints now and then (e.g. back from the background,
//! or the whole frame with the system title bar) is the unthemed, light
//! blue one: once the switch is done, the theme is applied again.

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
    /// Width of the resize borders; 0: none (maximized, fullscreen).
    border: i32,
}

// winuser.h hit test codes.
const HTCLIENT: isize = 1;
const HTCAPTION: isize = 2;
const HTLEFT: isize = 10;
const HTRIGHT: isize = 11;
const HTTOP: isize = 12;
const HTTOPLEFT: isize = 13;
const HTTOPRIGHT: isize = 14;
const HTBOTTOM: isize = 15;
const HTBOTTOMLEFT: isize = 16;
const HTBOTTOMRIGHT: isize = 17;

fn pixels(r: Rect, ppp: f32) -> [i32; 4] {
    [r.left(), r.top(), r.right(), r.bottom()].map(|v| (v * ppp).round() as i32)
}

fn inside(r: &[i32; 4], (x, y): (i32, i32)) -> bool {
    x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
}

impl Regions {
    /// What the client pixel `p` of a window `size` pixels large is.
    fn hit(&self, p: (i32, i32), size: (i32, i32)) -> isize {
        let ((x, y), (w, h), b) = (p, size, self.border);
        if b > 0 {
            // The corners reach twice as far along the edges.
            let (left, top, right, bottom) = (x < b, y < b, x >= w - b, y >= h - b);
            let (near_l, near_t, near_r, near_b) = (x < 2 * b, y < 2 * b, x >= w - 2 * b, y >= h - 2 * b);
            let code = if top && near_l || left && near_t {
                HTTOPLEFT
            } else if top && near_r || right && near_t {
                HTTOPRIGHT
            } else if bottom && near_l || left && near_b {
                HTBOTTOMLEFT
            } else if bottom && near_r || right && near_b {
                HTBOTTOMRIGHT
            } else if top {
                HTTOP
            } else if bottom {
                HTBOTTOM
            } else if left {
                HTLEFT
            } else if right {
                HTRIGHT
            } else {
                HTCLIENT
            };
            if code != HTCLIENT {
                return code;
            }
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

/// The app draws the window's title bar (the custom frame is on): Windows
/// does not repaint its own on activation (see the module docs).
pub fn own_frame(on: bool) {
    imp::own_frame(on);
}

/// The window is going in or out of full screen: Windows paints none of
/// its frame meanwhile (see the module docs).
pub fn switching(on: bool) {
    imp::switching(on);
}

/// Installs the hit test on the eframe window (first frame).
pub fn install(frame: &eframe::Frame) {
    imp::install(frame);
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

    const WM_NCHITTEST: u32 = 0x0084;
    const WM_NCPAINT: u32 = 0x0085;
    const WM_NCACTIVATE: u32 = 0x0086;
    const SUBCLASS_ID: usize = 0x494E_4B32; // "INK2"

    #[link(name = "user32")]
    unsafe extern "system" {
        fn ScreenToClient(hwnd: HWND, point: *mut POINT) -> i32;
        fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    }

    #[link(name = "uxtheme")]
    unsafe extern "system" {
        fn SetWindowTheme(hwnd: HWND, app_name: *const u16, id_list: *const u16) -> i32;
    }

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static OWN_FRAME: AtomicBool = AtomicBool::new(false);
    static SWITCHING: AtomicBool = AtomicBool::new(false);
    /// The window, once the subclass is installed.
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static REGIONS: Mutex<Regions> = Mutex::new(Regions { caption: None, holes: Vec::new(), border: 0 });

    pub fn active() -> bool {
        ACTIVE.load(Ordering::Relaxed)
    }

    pub fn own_frame(on: bool) {
        OWN_FRAME.store(on, Ordering::Relaxed);
    }

    pub fn switching(on: bool) {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if SWITCHING.swap(on, Ordering::Relaxed) && !on && hwnd != 0 {
            // Swallowing `WM_NCPAINT` left the window unthemed: the default
            // theme back (null names), which Windows tells it with
            // `WM_THEMECHANGED`.
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

    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if OWN_FRAME.load(Ordering::Relaxed) {
            match msg {
                // Nothing of the system's frame is painted.
                WM_NCPAINT if SWITCHING.load(Ordering::Relaxed) => return 0,
                // Activation still changes, without repainting the frame
                // (an `lparam` of -1 asks for that).
                // SAFETY: forwarding to the next window procedure.
                WM_NCACTIVATE => return unsafe { DefSubclassProc(hwnd, msg, wparam, -1) },
                _ => {}
            }
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

    pub fn install(frame: &eframe::Frame) {
        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        // SAFETY: the HWND of our own window, on its thread (the first frame).
        let ok = unsafe { SetWindowSubclass(h.hwnd.get() as HWND, Some(subclass_proc), SUBCLASS_ID, 0) != 0 };
        WINDOW.store(h.hwnd.get(), Ordering::Relaxed);
        ACTIVE.store(ok, Ordering::Relaxed);
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

    pub fn install(_frame: &eframe::Frame) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_holes_and_borders() {
        let r = Regions { caption: Some([0, 0, 800, 32]), holes: vec![[0, 0, 200, 32], [700, 0, 800, 32]], border: 5 };
        let size = (800, 600);
        assert_eq!(r.hit((400, 16), size), HTCAPTION);
        assert_eq!(r.hit((100, 16), size), HTCLIENT, "a menu");
        assert_eq!(r.hit((750, 16), size), HTCLIENT, "a window button");
        assert_eq!(r.hit((400, 300), size), HTCLIENT);
        assert_eq!(r.hit((400, 2), size), HTTOP);
        assert_eq!(r.hit((2, 300), size), HTLEFT);
        assert_eq!(r.hit((798, 598), size), HTBOTTOMRIGHT);
        assert_eq!(r.hit((8, 2), size), HTTOPLEFT, "the corner reaches along the edge");
        assert_eq!(r.hit((797, 300), size), HTRIGHT);
        // Maximized: no borders; a menu open: no caption.
        let r = Regions { border: 0, caption: None, ..r };
        assert_eq!(r.hit((400, 2), size), HTCLIENT);
    }
}
