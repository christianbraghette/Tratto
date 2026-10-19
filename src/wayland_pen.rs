//! Native pen/tablet input on Linux/Wayland (`zwp_tablet_v2`).
//!
//! winit 0.30 never binds the tablet protocol, so under Wayland a stylus
//! produces no events at all. This module:
//!
//! 1. wraps winit's live `wl_display` (`Backend::from_foreign_display`) so we
//!    share the *same* connection – the compositor only sends tablet events to
//!    the client that owns the surface,
//! 2. binds `zwp_tablet_manager_v2` on a private event queue,
//! 3. dispatches that queue on a background thread and forwards one
//!    [`PenEvent`] per protocol `frame` through a channel,
//! 4. is drained from `InkApp::raw_input_hook`, which turns the events into
//!    ordinary egui pointer events plus real pressure.
//!
//! Coordinates are surface-local logical pixels, i.e. egui points (divided by
//! egui's zoom factor on the consumer side).
//!
//! On every other OS, and on X11, `WaylandPen::start` returns `None` and the
//! normal winit input path is used unchanged.

/// One normalised pen sample (already grouped by the protocol's `frame`).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum PenEvent {
    /// Pen moved, hovered, touched down or lifted.
    Motion {
        x: f32,
        y: f32,
        /// `0.0..=1.0`
        pressure: f32,
        /// Tip is in contact with the surface.
        down: bool,
        /// The eraser end of the pen is in use.
        eraser: bool,
    },
    /// Pen left proximity of the tablet.
    Leave,
}

#[cfg(target_os = "linux")]
pub use imp::WaylandPen;
#[cfg(not(target_os = "linux"))]
pub use stub::WaylandPen;

#[cfg(not(target_os = "linux"))]
mod stub {
    use super::PenEvent;
    use eframe::egui;

    pub struct WaylandPen;

    impl WaylandPen {
        pub fn start(_ctx: &egui::Context, _frame: &eframe::Frame) -> Option<Self> {
            None
        }
        pub fn try_iter(&self) -> std::iter::Empty<PenEvent> {
            std::iter::empty()
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender, TryIter};
    use std::sync::Arc;

    use eframe::egui;
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    use wayland_client::{
        backend::{Backend, ObjectId},
        delegate_noop, event_created_child,
        globals::{registry_queue_init, GlobalListContents},
        protocol::{wl_callback::WlCallback, wl_registry::WlRegistry, wl_seat::WlSeat},
        Connection, Dispatch, Proxy, QueueHandle, WEnum,
    };
    use wayland_protocols::wp::tablet::zv2::client::{
        zwp_tablet_manager_v2::ZwpTabletManagerV2,
        zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
        zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
        zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
        zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
        zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
        zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
        zwp_tablet_v2::ZwpTabletV2,
    };

    use super::PenEvent;

    /// Linux `BTN_STYLUS` (first barrel button).
    const BTN_STYLUS: u32 = 0x14b;
    /// `zwp_tablet_tool_v2::pressure` is normalised over this range.
    const PRESSURE_MAX: f32 = 65535.0;

    pub struct WaylandPen {
        rx: Receiver<PenEvent>,
        stop: Arc<AtomicBool>,
        conn: Connection,
        qh: QueueHandle<State>,
    }

    impl WaylandPen {
        /// Returns `None` when not running on Wayland or when the compositor
        /// has no tablet support – the caller then keeps winit's input path.
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
            let manager: ZwpTabletManagerV2 = globals.bind(&qh, 1..=1, ()).ok()?;
            // Announces tablets/tools through `Dispatch<ZwpTabletSeatV2>` below.
            let _tablet_seat = manager.get_tablet_seat(&seat, &qh, ());

            let (tx, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));

            let mut state = State { tools: HashMap::new(), tx, ctx: ctx.clone() };
            let stop_thread = stop.clone();
            std::thread::Builder::new()
                .name("wayland-pen".into())
                .spawn(move || {
                    let _keep_alive = (seat, manager, _tablet_seat);
                    while !stop_thread.load(Ordering::Relaxed) {
                        if queue.blocking_dispatch(&mut state).is_err() {
                            break;
                        }
                    }
                })
                .ok()?;

            Some(Self { rx, stop, conn, qh })
        }

        pub fn try_iter(&self) -> TryIter<'_, PenEvent> {
            self.rx.try_iter()
        }
    }

    impl Drop for WaylandPen {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            // Round-trip request wakes the blocked dispatch thread so it can exit.
            let _ = self.conn.display().sync(&self.qh, ());
            let _ = self.conn.flush();
        }
    }

    // ------------------------------------------------------------------
    // Protocol state
    // ------------------------------------------------------------------

    #[derive(Default, Clone, Copy, PartialEq, Eq)]
    enum ToolKind {
        #[default]
        Pen,
        Eraser,
        /// Finger / mouse / lens tools are left to the normal input path.
        Ignore,
    }

    #[derive(Default)]
    struct ToolState {
        kind: ToolKind,
        x: f64,
        y: f64,
        pressure: f32,
        in_proximity: bool,
        down: bool,
        dirty: bool,
        left: bool,
    }

    struct State {
        tools: HashMap<ObjectId, ToolState>,
        tx: Sender<PenEvent>,
        ctx: egui::Context,
    }

    impl Dispatch<ZwpTabletToolV2, ()> for State {
        fn event(
            state: &mut Self,
            proxy: &ZwpTabletToolV2,
            event: zwp_tablet_tool_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            use zwp_tablet_tool_v2::{Event as E, Type};
            let id = proxy.id();

            // Frame is handled separately: it needs `state.tx` and `state.ctx`.
            if let E::Frame { .. } = event {
                let Some(t) = state.tools.get_mut(&id) else { return };
                let mut sent = false;
                if t.left {
                    t.left = false;
                    t.dirty = false;
                    if t.kind != ToolKind::Ignore {
                        sent = state.tx.send(PenEvent::Leave).is_ok();
                    }
                } else if t.dirty && t.in_proximity && t.kind != ToolKind::Ignore {
                    t.dirty = false;
                    sent = state
                        .tx
                        .send(PenEvent::Motion {
                            x: t.x as f32,
                            y: t.y as f32,
                            pressure: t.pressure,
                            down: t.down,
                            eraser: t.kind == ToolKind::Eraser,
                        })
                        .is_ok();
                }
                if sent {
                    state.ctx.request_repaint();
                }
                return;
            }

            let t = state.tools.entry(id.clone()).or_default();
            match event {
                E::Type { tool_type } => {
                    t.kind = match tool_type {
                        WEnum::Value(Type::Eraser) => ToolKind::Eraser,
                        WEnum::Value(
                            Type::Pen | Type::Pencil | Type::Brush | Type::Airbrush,
                        ) => ToolKind::Pen,
                        _ => ToolKind::Ignore,
                    };
                }
                E::ProximityIn { .. } => {
                    t.in_proximity = true;
                    t.dirty = true;
                }
                E::ProximityOut => {
                    t.in_proximity = false;
                    t.down = false;
                    t.left = true;
                }
                E::Down { .. } => {
                    t.down = true;
                    t.dirty = true;
                }
                E::Up => {
                    t.down = false;
                    t.dirty = true;
                }
                E::Motion { x, y } => {
                    t.x = x;
                    t.y = y;
                    t.dirty = true;
                }
                E::Pressure { pressure } => {
                    t.pressure = (pressure as f32 / PRESSURE_MAX).clamp(0.0, 1.0);
                    t.dirty = true;
                }
                E::Button { button, state: btn_state, .. } => {
                    let pressed = matches!(
                        btn_state,
                        WEnum::Value(zwp_tablet_tool_v2::ButtonState::Pressed)
                    );
                    if t.kind != ToolKind::Ignore && pressed && button == BTN_STYLUS {
                        let _ = state.tx.send(PenEvent::BarrelPressed);
                        state.ctx.request_repaint();
                    }
                }
                E::Removed => {
                    state.tools.remove(&id);
                    proxy.destroy();
                }
                _ => {}
            }
        }
    }

    impl Dispatch<ZwpTabletSeatV2, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpTabletSeatV2,
            _: zwp_tablet_seat_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }

        // Objects created by events must be declared so wayland-client knows
        // which Rust type to attach to them.
        event_created_child!(State, ZwpTabletSeatV2, [
            zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
            zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
            zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ())
        ]);
    }

    // Tablet pads (the button boxes) are not used, but their child objects must
    // still be routed somewhere or wayland-client would panic.
    impl Dispatch<ZwpTabletPadV2, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpTabletPadV2,
            _: zwp_tablet_pad_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }

        event_created_child!(State, ZwpTabletPadV2, [
            zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ())
        ]);
    }

    impl Dispatch<ZwpTabletPadGroupV2, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpTabletPadGroupV2,
            _: zwp_tablet_pad_group_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }

        event_created_child!(State, ZwpTabletPadGroupV2, [
            zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
            zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ())
        ]);
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
    delegate_noop!(State: ignore ZwpTabletManagerV2);
    delegate_noop!(State: ignore ZwpTabletV2);
    delegate_noop!(State: ignore ZwpTabletPadRingV2);
    delegate_noop!(State: ignore ZwpTabletPadStripV2);
}
