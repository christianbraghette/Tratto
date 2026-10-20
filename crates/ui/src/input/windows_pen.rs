// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pen side button and eraser end on Windows.
//!
//! winit 0.30 reports a Windows pen as a touch contact (position + pressure)
//! and drops everything else, so the side ("barrel") button and the inverted
//! eraser end never reach egui. This module subclasses the window and reads
//! the extra pen state straight from the `WM_POINTER*` messages with
//! `GetPointerPenInfo`:
//!
//! * `PEN_FLAG_BARREL`                    -> side button held
//! * `PEN_FLAG_INVERTED` / `PEN_FLAG_ERASER` -> eraser end in use
//! * in range but not touching             -> hover position (the app
//!   draws its pen cursor there; winit does not report pen hover)
//!
//! The subclass proc only *observes* the messages and always forwards them
//! (`DefSubclassProc`), so winit keeps working exactly as before. State is
//! published through atomics and the UI is woken with `request_repaint`.
//!
//! Set `TRATTO_DEBUG_PEN=1` to print the pen flags whenever they change.
//!
//! On other platforms `WindowsPen::start` returns `None`.

/// Pen buttons as last reported by the digitizer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PenButtons {
    /// Side button held.
    pub barrel: bool,
    /// The eraser end of the pen is in range / touching.
    pub eraser_end: bool,
}

#[cfg(windows)]
pub use imp::WindowsPen;
#[cfg(not(windows))]
pub use stub::WindowsPen;

#[cfg(not(windows))]
mod stub {
    use super::PenButtons;
    use eframe::egui;

    pub struct WindowsPen;

    impl WindowsPen {
        pub fn start(_ctx: &egui::Context, _frame: &eframe::Frame) -> Option<Self> {
            None
        }
        pub fn buttons(&self) -> PenButtons {
            PenButtons::default()
        }
        pub fn hover(&self) -> Option<(f32, f32)> {
            None
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    use eframe::egui;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};

    use super::PenButtons;

    // Message / flag values from winuser.h (defined here so they do not depend
    // on optional windows-sys feature flags).
    const WM_KILLFOCUS: u32 = 0x0008;
    const WM_POINTERUPDATE: u32 = 0x0245;
    const WM_POINTERDOWN: u32 = 0x0246;
    const WM_POINTERUP: u32 = 0x0247;
    const WM_POINTERENTER: u32 = 0x0249;
    const WM_POINTERLEAVE: u32 = 0x024A;
    const PT_PEN: i32 = 3;
    const PEN_FLAG_BARREL: u32 = 0x0000_0001;
    const PEN_FLAG_INVERTED: u32 = 0x0000_0002;
    const PEN_FLAG_ERASER: u32 = 0x0000_0004;
    const POINTER_FLAG_INCONTACT: u32 = 0x0000_0004;

    // Keep these declarations local so this module does not require the
    // optional `Win32_UI_Input_Pointer` windows-sys feature.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct PointerInfo {
        pointer_type: u32,
        pointer_id: u32,
        frame_id: u32,
        pointer_flags: u32,
        source_device: isize,
        hwnd_target: isize,
        pixel_location: Point,
        himetric_location: Point,
        pixel_location_raw: Point,
        himetric_location_raw: Point,
        time: u32,
        history_count: u32,
        input_data: i32,
        key_states: u32,
        performance_count: u64,
        button_change_type: i32,
    }

    #[repr(C)]
    struct PointerPenInfo {
        pointer_info: PointerInfo,
        pen_flags: u32,
        pen_mask: u32,
        pressure: u32,
        rotation: u32,
        tilt_x: i32,
        tilt_y: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetPointerType(pointer_id: u32, pointer_type: *mut i32) -> i32;
        fn GetPointerPenInfo(pointer_id: u32, pen_info: *mut PointerPenInfo) -> i32;
        fn ScreenToClient(hwnd: HWND, point: *mut Point) -> i32;
    }

    const SUBCLASS_ID: usize = 0x494E_4B31; // "INK1"

    static BARREL: AtomicBool = AtomicBool::new(false);
    static ERASER_END: AtomicBool = AtomicBool::new(false);
    /// The pen is in range and not touching; where (client pixels).
    static HOVERING: AtomicBool = AtomicBool::new(false);
    static HOVER_X: AtomicI32 = AtomicI32::new(0);
    static HOVER_Y: AtomicI32 = AtomicI32::new(0);
    static CTX: OnceLock<egui::Context> = OnceLock::new();

    fn debug() -> bool {
        static D: OnceLock<bool> = OnceLock::new();
        *D.get_or_init(|| std::env::var_os("TRATTO_DEBUG_PEN").is_some())
    }

    fn publish(barrel: bool, eraser_end: bool) {
        // `|`, not `||`: both must be stored.
        let changed = (BARREL.swap(barrel, Ordering::Relaxed) != barrel)
            | (ERASER_END.swap(eraser_end, Ordering::Relaxed) != eraser_end);
        if changed {
            if debug() {
                eprintln!("pen: barrel={barrel} eraser_end={eraser_end}");
            }
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint(); // wake the UI so the tool switches at once
            }
        }
    }

    /// Records the hover position (`None`: touching or out of range) and wakes
    /// the UI when it changed, so the pen cursor follows.
    fn publish_hover(at: Option<(i32, i32)>) {
        let was = HOVERING.swap(at.is_some(), Ordering::Relaxed);
        let moved = at.is_some_and(|(x, y)| {
            (HOVER_X.swap(x, Ordering::Relaxed) != x) | (HOVER_Y.swap(y, Ordering::Relaxed) != y)
        });
        if (was != at.is_some() || moved)
            && let Some(ctx) = CTX.get()
        {
            ctx.request_repaint();
        }
    }

    /// Observes pointer messages; everything is forwarded unchanged.
    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        unsafe {
            match msg {
                WM_POINTERENTER | WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP => {
                    let pointer_id = (wparam & 0xFFFF) as u32;
                    let mut kind: i32 = 0;
                    if GetPointerType(pointer_id, &mut kind) != 0 && kind == PT_PEN {
                        let mut info: PointerPenInfo = std::mem::zeroed();
                        if GetPointerPenInfo(pointer_id, &mut info) != 0 {
                            publish(
                                info.pen_flags & PEN_FLAG_BARREL != 0,
                                info.pen_flags & (PEN_FLAG_INVERTED | PEN_FLAG_ERASER) != 0,
                            );
                            let touching = info.pointer_info.pointer_flags & POINTER_FLAG_INCONTACT != 0;
                            let mut p = info.pointer_info.pixel_location;
                            let hover = (!touching && ScreenToClient(hwnd, &mut p) != 0).then_some((p.x, p.y));
                            publish_hover(hover);
                        }
                    }
                }
                WM_POINTERLEAVE => {
                    let pointer_id = (wparam & 0xFFFF) as u32;
                    let mut kind: i32 = 0;
                    if GetPointerType(pointer_id, &mut kind) != 0 && kind == PT_PEN {
                        publish(false, false);
                        publish_hover(None);
                    }
                }
                WM_KILLFOCUS => {
                    publish(false, false);
                    publish_hover(None);
                }
                _ => {}
            }
            DefSubclassProc(hwnd, msg, wparam, lparam)
        }
    }

    pub struct WindowsPen {
        hwnd: isize,
    }

    impl WindowsPen {
        /// Installs the observer on the eframe window. Must run on the UI thread
        /// (it does: `App::update`).
        pub fn start(ctx: &egui::Context, frame: &eframe::Frame) -> Option<Self> {
            let handle = frame.window_handle().ok()?;
            let RawWindowHandle::Win32(h) = handle.as_raw() else {
                return None;
            };
            let hwnd = h.hwnd.get();
            let _ = CTX.set(ctx.clone());
            // SAFETY: valid HWND of our own window, called on its owner thread.
            let ok = unsafe { SetWindowSubclass(hwnd as HWND, Some(subclass_proc), SUBCLASS_ID, 0) };
            (ok != 0).then_some(Self { hwnd })
        }

        pub fn buttons(&self) -> PenButtons {
            PenButtons { barrel: BARREL.load(Ordering::Relaxed), eraser_end: ERASER_END.load(Ordering::Relaxed) }
        }

        /// Where the pen hovers (in range, not touching), in physical pixels
        /// of the window's client area.
        pub fn hover(&self) -> Option<(f32, f32)> {
            HOVERING
                .load(Ordering::Relaxed)
                .then(|| (HOVER_X.load(Ordering::Relaxed) as f32, HOVER_Y.load(Ordering::Relaxed) as f32))
        }
    }

    impl Drop for WindowsPen {
        fn drop(&mut self) {
            // SAFETY: same window and id that were used to install it.
            unsafe {
                RemoveWindowSubclass(self.hwnd as HWND, Some(subclass_proc), SUBCLASS_ID);
            }
        }
    }
}
