// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The web: the pen's pressure, eraser end and side button, from the
//! browser's Pointer Events. eframe takes a pen for a mouse: the position
//! and the press still come from it, only what it leaves out comes from here.

use std::cell::Cell;
use std::rc::Rc;

use eframe::wasm_bindgen::JsCast;
use eframe::wasm_bindgen::closure::Closure;
use eframe::web_sys::{self, PointerEvent};

/// `PointerEvent::buttons` bits: the tip, the side button, the eraser end.
const TIP: u16 = 1;
const BARREL: u16 = 2;
const ERASER: u16 = 32;

/// What the last pointer event said.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PenState {
    /// The last pointer was a pen (else a mouse or a finger).
    pub pen: bool,
    /// Touching the screen, with the tip or the eraser end.
    pub down: bool,
    /// `0.0..=1.0`.
    pub pressure: f32,
    pub eraser: bool,
    pub barrel: bool,
}

impl PenState {
    fn of(e: &PointerEvent, released: bool) -> Self {
        let buttons = e.buttons();
        Self {
            pen: e.pointer_type() == "pen",
            down: !released && buttons & (TIP | ERASER) != 0,
            pressure: e.pressure(),
            eraser: buttons & ERASER != 0,
            barrel: buttons & BARREL != 0,
        }
    }
}

/// Listens to the page's pointer events while it lives.
pub struct WebPen {
    state: Rc<Cell<PenState>>,
    _listeners: Vec<Closure<dyn FnMut(PointerEvent)>>,
}

impl WebPen {
    pub fn start() -> Option<Self> {
        let window = web_sys::window()?;
        let state = Rc::new(Cell::new(PenState::default()));
        let mut listeners = Vec::new();
        for kind in ["pointerdown", "pointermove", "pointerup", "pointercancel"] {
            let released = matches!(kind, "pointerup" | "pointercancel");
            let state = state.clone();
            let listener = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
                state.set(PenState::of(&e, released));
            });
            window.add_event_listener_with_callback(kind, listener.as_ref().unchecked_ref()).ok()?;
            listeners.push(listener);
        }
        Some(Self { state, _listeners: listeners })
    }

    pub fn state(&self) -> PenState {
        self.state.get()
    }
}
