//! Application state and per-frame orchestration.

use std::path::Path;

use eframe::egui::{
    self, Align2, DragValue, Event, Key, Modifiers, PointerButton, Pos2, Sense, Slider, TouchPhase,
};

use crate::history::{Action, History};
use crate::model::{Document, PageKind, Point, StrokeKind, Stroke};
use crate::render::{self, StrokeCache};
use crate::settings::{Settings, ToolbarPosition};
use crate::storage;
use crate::stroke_builder::StrokeBuilder;
use crate::toolbar::{self, Tool, ToolbarEvent};
use crate::view::View;
use crate::wayland_pen::{PenEvent, WaylandPen};

const SETTINGS_KEY: &str = "inknote_settings";
/// Samples closer than this (document units) are treated as jitter.
const MIN_POINT_DIST: f32 = 0.6;
/// Smoothing strength at rest (0..1, lower = smoother) ...
const SMOOTH_ALPHA_MIN: f32 = 0.4;
/// ... fading to none at this per-sample distance (document units).
const SMOOTH_SPEED_REF: f32 = 6.0;
const HIGHLIGHTER_ALPHA: u8 = 96;

/// Stroke currently being drawn.
struct ActiveStroke {
    kind: StrokeKind,
    color: [u8; 4],
    width: f32,
    builder: StrokeBuilder,
}

pub struct InkApp {
    doc: Document,
    history: History,
    tool: Tool,
    settings: Settings,
    settings_open: bool,
    /// Top of the viewport in document units (>= 0).
    scroll_y: f32,
    active: Option<ActiveStroke>,
    /// Inverse actions of strokes erased during the current eraser drag.
    erase_session: Vec<Action>,
    last_erase_pos: Option<Pos2>,
    last_pressure: f32,
    status: String,
    /// Native Wayland pen source (None on X11/Windows or without tablet support).
    pen: Option<WaylandPen>,
    pen_init_done: bool,
    pen_down: bool,
    pen_pos: Pos2,
    /// Tool to restore when the pen's eraser end is lifted.
    saved_tool: Option<Tool>,
    /// Cached anti-aliased meshes of finished strokes.
    cache: StrokeCache,
}

impl InkApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
            .unwrap_or_default();

        let mut app = Self {
            doc: Document::default(),
            history: History::default(),
            tool: Tool::Pen,
            settings,
            settings_open: false,
            scroll_y: 0.0,
            active: None,
            erase_session: Vec::new(),
            last_erase_pos: None,
            last_pressure: 1.0,
            status: String::new(),
            pen: None,
            pen_init_done: false,
            pen_down: false,
            pen_pos: Pos2::ZERO,
            saved_tool: None,
            cache: StrokeCache::default(),
        };
        if Path::new(&app.settings.file_path).exists() {
            app.load();
        }
        app
    }

    // ---------------------------------------------------------------- files

    fn save_file(&mut self) {
        let path = self.settings.file_path.clone();
        self.status = match storage::save_document(Path::new(&path), &self.doc) {
            Ok(()) => format!("Saved to {path}"),
            Err(e) => format!("Save failed: {e}"),
        };
    }

    fn load(&mut self) {
        let path = self.settings.file_path.clone();
        match storage::load_document(Path::new(&path)) {
            Ok(doc) => {
                self.doc = doc;
                self.reset_editing_state();
                self.status = format!("Loaded {path}");
            }
            Err(e) => self.status = format!("Load failed: {e}"),
        }
    }

    fn new_document(&mut self) {
        self.doc = Document::default();
        self.reset_editing_state();
        self.status = "New document".into();
    }

    fn reset_editing_state(&mut self) {
        self.cache.clear();
        self.history.clear();
        self.active = None;
        self.erase_session.clear();
        self.last_erase_pos = None;
        self.scroll_y = 0.0;
    }

    // ------------------------------------------------------- undo / redo

    fn undo(&mut self) {
        if self.active.is_none() && self.erase_session.is_empty() {
            self.history.undo(&mut self.doc);
        }
    }

    fn redo(&mut self) {
        if self.active.is_none() && self.erase_session.is_empty() {
            self.history.redo(&mut self.doc);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // Order matters: the Shift variant must be consumed before plain Ctrl+Z.
        let (redo_shift, redo_y, undo, save, fullscreen) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::Y),
                i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::NONE, Key::F11),
            )
        });
        if redo_shift || redo_y {
            self.redo();
        } else if undo {
            self.undo();
        }
        if save {
            self.save_file();
        }
        if fullscreen {
            let is_fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fs));
        }
    }

    // ------------------------------------------------------------- input

    /// Tracks pen pressure from touch/pen events (mouse stays at 1.0).
    fn update_pressure(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            for e in &i.events {
                if let Event::Touch { phase, force, .. } = e {
                    match phase {
                        TouchPhase::End | TouchPhase::Cancel => self.last_pressure = 1.0,
                        _ => {
                            if let Some(f) = force {
                                if *f > 0.0 {
                                    self.last_pressure = f.clamp(0.0, 1.0);
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Converts native pen events into regular egui pointer events and pressure.
    /// Called from `raw_input_hook`, i.e. before egui processes the frame.
    fn inject_pen_events(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(pen) = self.pen.as_ref() else { return };
        let events: Vec<PenEvent> = pen.try_iter().collect();
        let zoom = ctx.zoom_factor();

        for ev in events {
            match ev {
                PenEvent::Motion { x, y, pressure, down, eraser } => {
                    let pos = Pos2::new(x / zoom, y / zoom);
                    self.pen_pos = pos;
                    raw_input.events.push(Event::PointerMoved(pos));
                    if down != self.pen_down {
                        self.pen_down = down;
                        raw_input.events.push(Event::PointerButton {
                            pos,
                            button: PointerButton::Primary,
                            pressed: down,
                            modifiers: raw_input.modifiers,
                        });
                    }
                    if down {
                        self.last_pressure = pressure.clamp(0.0, 1.0);
                    }
                    self.apply_pen_eraser(eraser);
                }
                PenEvent::Leave => {
                    if self.pen_down {
                        self.pen_down = false;
                        raw_input.events.push(Event::PointerButton {
                            pos: self.pen_pos,
                            button: PointerButton::Primary,
                            pressed: false,
                            modifiers: raw_input.modifiers,
                        });
                    }
                    raw_input.events.push(Event::PointerGone);
                    self.apply_pen_eraser(false);
                    self.last_pressure = 1.0;
                }
                // Barrel button = undo.
                PenEvent::BarrelPressed => self.undo(),
            }
        }
    }

    /// Flipping the pen over switches to the eraser; flipping back restores the tool.
    fn apply_pen_eraser(&mut self, eraser: bool) {
        if eraser {
            if self.saved_tool.is_none() && self.tool != Tool::Eraser {
                self.saved_tool = Some(self.tool);
                self.tool = Tool::Eraser;
            }
        } else if let Some(t) = self.saved_tool.take() {
            self.tool = t;
        }
    }

    fn stroke_params(&self) -> Option<(StrokeKind, [u8; 4], f32)> {
        match self.tool {
            Tool::Pen => {
                let s = self.settings.pen;
                Some((StrokeKind::Pen, [s.color[0], s.color[1], s.color[2], 255], s.width))
            }
            Tool::Highlighter => {
                let s = self.settings.highlighter;
                Some((
                    StrokeKind::Highlighter,
                    [s.color[0], s.color[1], s.color[2], HIGHLIGHTER_ALPHA],
                    s.width,
                ))
            }
            Tool::Eraser => None,
        }
    }

    fn handle_ink(
        &mut self,
        resp: &egui::Response,
        view: &View,
        press: Option<Pos2>,
        moves: &[Pos2],
    ) {
        if resp.drag_started_by(PointerButton::Primary) {
            if let (Some(pos), Some((kind, color, width))) =
                (press.or(resp.interact_pointer_pos()), self.stroke_params())
            {
                // Only start inside the writable page, not in the padding.
                if view.page_rect().contains(pos) {
                    let mut builder = StrokeBuilder::new(MIN_POINT_DIST, SMOOTH_ALPHA_MIN, SMOOTH_SPEED_REF);
                    builder.begin(to_doc_point(view, pos, self.last_pressure));
                    self.active = Some(ActiveStroke { kind, color, width, builder });
                }
            }
        } else if resp.dragged() || resp.drag_stopped() {
            let pressure = self.last_pressure;
            if let Some(active) = self.active.as_mut() {
                for pos in moves {
                    active.builder.push(to_doc_point(view, *pos, pressure));
                }
            }
        }
        if self.active.is_some() && !resp.dragged() {
            self.commit_active();
        }
    }

    fn commit_active(&mut self) {
        if let Some(a) = self.active.take() {
            let points = a.builder.finish();
            if points.is_empty() {
                return;
            }
            let id = self.doc.alloc_id();
            let stroke = Stroke::new(id, a.kind, a.color, a.width, points);
            let index = self.doc.strokes.len();
            self.history
                .exec(&mut self.doc, Action::AddStroke { index, stroke });
        }
    }

    fn handle_erase(
        &mut self,
        resp: &egui::Response,
        view: &View,
        press: Option<Pos2>,
        moves: &[Pos2],
    ) {
        if resp.drag_started_by(PointerButton::Primary) {
            self.last_erase_pos = None;
            if let Some(p) = press.or(resp.interact_pointer_pos()) {
                self.erase_along(view.to_doc(p));
            }
        } else if resp.dragged() || resp.drag_stopped() {
            for p in moves {
                self.erase_along(view.to_doc(*p));
            }
        }
        if !resp.dragged() {
            self.finish_erase();
        }
    }

    /// Erases along the path from the previous cursor position to `target`.
    /// Intermediate samples prevent fast flicks from skipping over strokes.
    fn erase_along(&mut self, target: Pos2) {
        let radius = self.settings.eraser_radius;
        let from = self.last_erase_pos.unwrap_or(target);
        let step = (radius * 0.5).max(1.0);
        let steps = ((from.distance(target) / step).ceil() as usize).max(1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let c = from + (target - from) * t;
            self.erase_at(c, radius);
        }
        self.last_erase_pos = Some(target);
    }

    fn erase_at(&mut self, center: Pos2, radius: f32) {
        let hits: Vec<u64> = self
            .doc
            .strokes
            .iter()
            .filter(|s| s.hit_test(center, radius))
            .map(|s| s.id)
            .collect();
        for id in hits {
            let inverse = Action::RemoveStroke { id }.apply(&mut self.doc);
            self.erase_session.push(inverse);
        }
    }

    /// Records everything erased during one drag as a single undo step.
    fn finish_erase(&mut self) {
        self.last_erase_pos = None;
        if self.erase_session.is_empty() {
            return;
        }
        let inverses = std::mem::take(&mut self.erase_session);
        // Undo must re-insert in the reverse order of removal.
        self.history
            .push_undo(Action::Batch(inverses.into_iter().rev().collect()));
    }

    // ---------------------------------------------------------------- UI

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, Sense::drag());

        // Infinite vertical scrolling (mouse wheel / touchpad / touch scroll).
        let scale = View::scale_for(rect.width(), self.doc.page_width);
        if response.hovered() && self.active.is_none() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            self.scroll_y = (self.scroll_y - dy / scale).max(0.0);
        }
        let view = View::new(rect, self.scroll_y, self.doc.page_width);

        self.update_pressure(ui.ctx());

        // All pointer positions of this frame (higher fidelity than one per frame).
        let press = ui.input(|i| i.pointer.press_origin());
        let mut moves: Vec<Pos2> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::PointerMoved(p) => Some(*p),
                    _ => None,
                })
                .collect()
        });
        if moves.is_empty() {
            moves.extend(response.interact_pointer_pos());
        }

        match self.tool {
            Tool::Eraser => self.handle_erase(&response, &view, press, &moves),
            Tool::Pen | Tool::Highlighter => self.handle_ink(&response, &view, press, &moves),
        }

        // ---- painting
        let painter = ui.painter_at(rect);
        render::paint_background(&painter, &view, &self.doc.page);

        let ppp = ui.ctx().pixels_per_point();
        render::paint_strokes(&painter, &view, &self.doc.strokes, &mut self.cache, ppp);
        if let Some(a) = &self.active {
            let pts = a.builder.preview_points(self.settings.predict_pen);
            render::paint_live(&painter, &view, a.kind, a.color, a.width, &pts, ppp);
        }
        if self.tool == Tool::Eraser {
            if let Some(pos) = response.hover_pos().or(response.interact_pointer_pos()) {
                render::paint_eraser_cursor(&painter, pos, self.settings.eraser_radius * view.scale);
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("Toolbar position");
                ui.horizontal(|ui| {
                    for (pos, label) in [
                        (ToolbarPosition::Top, "Top"),
                        (ToolbarPosition::Bottom, "Bottom"),
                        //(ToolbarPosition::Left, "Left"),
                        //(ToolbarPosition::Right, "Right"),
                    ] {
                        ui.selectable_value(&mut self.settings.toolbar_position, pos, label);
                    }
                });

                ui.checkbox(
                    &mut self.settings.predict_pen,
                    "Predict pen tip (lower perceived latency)",
                );

                ui.separator();
                ui.heading("Page");
                ui.horizontal(|ui| {
                    for (kind, label) in [
                        (PageKind::Blank, "Blank"),
                        (PageKind::Ruled, "Ruled"),
                        (PageKind::Grid, "Grid"),
                    ] {
                        ui.selectable_value(&mut self.doc.page.kind, kind, label);
                    }
                });
                ui.add(Slider::new(&mut self.doc.page.spacing, 16.0..=80.0).text("Line spacing"));
                ui.horizontal(|ui| {
                    ui.label("Page width");
                    // Note: shrinking the page does not move existing strokes.
                    ui.add(
                        DragValue::new(&mut self.doc.page_width)
                            .range(400.0..=1600.0)
                            .speed(5.0),
                    );
                });

                ui.separator();
                ui.heading("File");
                ui.text_edit_singleline(&mut self.settings.file_path);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save_file();
                    }
                    if ui.button("Open").clicked() {
                        self.load();
                    }
                    if ui.button("New").clicked() {
                        self.new_document();
                    }
                });
                if !self.status.is_empty() {
                    ui.label(&self.status);
                }
                if self.pen.is_some() {
                    ui.small("Native Wayland pen input: active (pressure, eraser end, barrel button = undo)");
                }
                ui.small("Shortcuts: Ctrl+Z undo, Ctrl+Y redo, Ctrl+S save, F11 fullscreen");
            });
        self.settings_open = open;
    }
}

/// Screen position -> document point, clamped to the page.
fn to_doc_point(view: &View, pos: Pos2, pressure: f32) -> Point {
    let d = view.to_doc(pos);
    Point::new(d.x.clamp(0.0, view.page_width), d.y.max(0.0), pressure)
}

impl eframe::App for InkApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The Wayland display handle is only available through `Frame`, so the
        // native pen source is started lazily on the first frame.
        if !self.pen_init_done {
            self.pen_init_done = true;
            self.pen = WaylandPen::start(ctx, frame);
        }

        self.handle_shortcuts(ctx);

        // The toolbar is an Area above the canvas, so egui's hit-testing
        // automatically keeps pen strokes from starting underneath it.
        let event = toolbar::show(
            ctx,
            &mut self.tool,
            &mut self.settings,
            self.history.can_undo(),
            self.history.can_redo(),
        );
        match event {
            Some(ToolbarEvent::Undo) => self.undo(),
            Some(ToolbarEvent::Redo) => self.redo(),
            Some(ToolbarEvent::ToggleSettings) => self.settings_open = !self.settings_open,
            None => {}
        }
        self.settings_window(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| self.canvas_ui(ui));
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.inject_pen_events(ctx, raw_input);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }
}
