//! Application state and per-frame orchestration.

use std::path::Path;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, DragValue, Event, FontId, Id, Key, Modifiers, PointerButton,
    Pos2, Rect, Sense, Slider, TouchPhase, Vec2,
};

use crate::history::{Action, History};
use crate::model::{paper_label, Document, PageKind, Point, Stroke, StrokeKind, PAPERS};
use crate::render::{self, StrokeCache};
use crate::scrollbar;
use crate::settings::{Settings, ToolbarPosition, ZoomMode};
use crate::storage;
use crate::stroke_builder::StrokeBuilder;
use crate::toolbar::{self, Tool, ToolbarEvent};
use crate::view::{Layout, View};
use crate::pen::{PenEvent, WaylandPen};

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
    page: u32,
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
    /// Content-space position at the top-left of the writing area.
    scroll: Vec2,
    vbar_grab: Option<f32>,
    hbar_grab: Option<f32>,
    /// Whether a horizontal scrollbar was shown last frame (toolbar placement).
    hbar_visible: bool,
    active: Option<ActiveStroke>,
    /// Inverse actions of strokes erased during the current eraser drag.
    erase_session: Vec<Action>,
    /// Last eraser position, in content space.
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
        // We implement zoom ourselves; keep egui's UI from zooming on Ctrl +/-.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);

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
            scroll: Vec2::ZERO,
            vbar_grab: None,
            hbar_grab: None,
            hbar_visible: false,
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
                self.ensure_pages();
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
        self.scroll = Vec2::ZERO;
    }

    /// Guarantees that every stroke lies on an existing page (e.g. after an
    /// undo that restores a stroke whose page was removed).
    fn ensure_pages(&mut self) {
        let need = self.doc.strokes.iter().map(|s| s.page + 1).max().unwrap_or(1).max(1);
        if self.doc.page_count < need {
            self.doc.page_count = need;
        }
    }

    // ------------------------------------------------------- undo / redo

    fn undo(&mut self) {
        if self.active.is_none() && self.erase_session.is_empty() {
            self.history.undo(&mut self.doc);
            self.ensure_pages();
        }
    }

    fn redo(&mut self) {
        if self.active.is_none() && self.erase_session.is_empty() {
            self.history.redo(&mut self.doc);
            self.ensure_pages();
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
                // A stroke belongs to the page it starts on; the gaps and
                // margins between pages are not writable.
                if let Some(page) = view.layout.page_at(view.screen_to_content(pos)) {
                    let mut builder =
                        StrokeBuilder::new(MIN_POINT_DIST, SMOOTH_ALPHA_MIN, SMOOTH_SPEED_REF);
                    builder.begin(to_page_point(view, page, pos, self.last_pressure));
                    self.active = Some(ActiveStroke { page, kind, color, width, builder });
                }
            }
        } else if resp.dragged() || resp.drag_stopped() {
            let pressure = self.last_pressure;
            if let Some(active) = self.active.as_mut() {
                let page = active.page;
                for pos in moves {
                    active.builder.push(to_page_point(view, page, *pos, pressure));
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
            let stroke = Stroke::new(id, a.page, a.kind, a.color, a.width, points);
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
                self.erase_along(view.screen_to_content(p));
            }
        } else if resp.dragged() || resp.drag_stopped() {
            for p in moves {
                self.erase_along(view.screen_to_content(*p));
            }
        }
        if !resp.dragged() {
            self.finish_erase();
        }
    }

    /// Erases along the path from the previous cursor position to `target`
    /// (content space). Intermediate samples prevent fast flicks from
    /// skipping over strokes.
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
        let layout = Layout {
            page_w: self.doc.page_width,
            page_h: self.doc.page_height,
            count: self.doc.page_count,
        };
        let mut hits: Vec<u64> = Vec::new();
        // The eraser circle may touch two neighbouring pages near the gap.
        for page in layout.pages_overlapping(center.y - radius, center.y + radius) {
            let local = center - layout.page_origin(page).to_vec2();
            hits.extend(
                self.doc
                    .strokes
                    .iter()
                    .filter(|s| s.page == page && s.hit_test(local, radius))
                    .map(|s| s.id),
            );
        }
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

    /// Writing-area rectangle, zoom scale and whether a horizontal scrollbar
    /// is needed. The vertical scrollbar always occupies the right edge.
    fn canvas_geometry(&self, full: Rect, layout: &Layout) -> (Rect, f32, bool) {
        let sb = scrollbar::THICKNESS;
        let w = (full.width() - sb).max(50.0);
        let mut rect = Rect::from_min_size(full.min, vec2(w, full.height()));
        let mut scale = View::scale_for(self.settings.zoom, rect, layout);
        let hbar = layout.content_w() * scale > rect.width() + 0.5;
        if hbar {
            rect = Rect::from_min_size(full.min, vec2(w, (full.height() - sb).max(50.0)));
            scale = View::scale_for(self.settings.zoom, rect, layout);
        }
        (rect, scale, hbar)
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        let layout = Layout {
            page_w: self.doc.page_width,
            page_h: self.doc.page_height,
            count: self.doc.page_count.max(1),
        };
        ui.painter().rect_filled(full, 0.0, render::PADDING_COLOR);

        // ---- geometry and zoom
        let (mut canvas, mut scale, mut hbar) = self.canvas_geometry(full, &layout);
        let idle = self.active.is_none() && self.erase_session.is_empty();

        // Ctrl + wheel / pinch zoom, anchored under the pointer.
        if idle && ui.rect_contains_pointer(canvas) {
            let zd = ui.input(|i| i.zoom_delta());
            if (zd - 1.0).abs() > 1e-4 {
                let anchor = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| canvas.contains(*p))
                    .unwrap_or_else(|| canvas.center());
                let before = View::new(canvas, self.scroll, scale, layout);
                let c = before.screen_to_content(anchor);
                let pct = (scale * zd * 100.0).round().clamp(25.0, 400.0) as u32;
                self.settings.zoom = ZoomMode::Percent(pct);
                (canvas, scale, hbar) = self.canvas_geometry(full, &layout);
                let pad = View::pad_x_for(canvas.width(), layout.content_w(), scale);
                self.scroll.x = c.x - (anchor.x - canvas.left() - pad) / scale;
                self.scroll.y = c.y - (anchor.y - canvas.top()) / scale;
            }
        }
        self.hbar_visible = hbar;

        let canvas_resp = ui.allocate_rect(canvas, Sense::drag());

        // ---- scrolling: wheel / touchpad / keys
        if idle && ui.rect_contains_pointer(full) {
            let d = ui.input(|i| i.smooth_scroll_delta);
            self.scroll -= d / scale;
        }
        if !ui.ctx().wants_keyboard_input() {
            let (pgdn, pgup, home, end) = ui.input(|i| {
                (
                    i.key_pressed(Key::PageDown),
                    i.key_pressed(Key::PageUp),
                    i.key_pressed(Key::Home),
                    i.key_pressed(Key::End),
                )
            });
            let page_len = canvas.height() / scale * 0.9;
            if pgdn {
                self.scroll.y += page_len;
            }
            if pgup {
                self.scroll.y -= page_len;
            }
            if home {
                self.scroll.y = 0.0;
            }
            if end {
                self.scroll.y = f32::MAX;
            }
        }
        let max = View::max_scroll(canvas, scale, &layout);
        self.scroll = vec2(self.scroll.x.clamp(0.0, max.x), self.scroll.y.clamp(0.0, max.y));

        // ---- scrollbars (interaction first, so the view below is final)
        let vbar_rect = Rect::from_min_max(
            pos2(canvas.right(), full.top()),
            pos2(full.right(), canvas.bottom()),
        );
        let hbar_rect = Rect::from_min_max(
            pos2(full.left(), canvas.bottom()),
            pos2(canvas.right(), full.bottom()),
        );
        let vstate = scrollbar::update(
            ui,
            Id::new("vbar"),
            vbar_rect,
            true,
            &mut self.scroll.y,
            max.y,
            canvas.height() / scale,
            layout.content_h(scale),
            &mut self.vbar_grab,
        );
        let hstate = hbar.then(|| {
            scrollbar::update(
                ui,
                Id::new("hbar"),
                hbar_rect,
                false,
                &mut self.scroll.x,
                max.x,
                canvas.width() / scale,
                layout.content_w(),
                &mut self.hbar_grab,
            )
        });

        let view = View::new(canvas, self.scroll, scale, layout);

        // ---- pen / mouse input
        self.update_pressure(ui.ctx());
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
            moves.extend(canvas_resp.interact_pointer_pos());
        }
        match self.tool {
            Tool::Eraser => self.handle_erase(&canvas_resp, &view, press, &moves),
            Tool::Pen | Tool::Highlighter => self.handle_ink(&canvas_resp, &view, press, &moves),
        }

        // ---- painting
        let painter = ui.painter_at(canvas);
        render::paint_background(&painter, &view, &self.doc.page);
        let ppp = ui.ctx().pixels_per_point();
        render::paint_strokes(&painter, &view, &self.doc.strokes, &mut self.cache, ppp);
        if let Some(a) = &self.active {
            let pts = a.builder.preview_points(self.settings.predict_pen);
            render::paint_live(&painter, &view, a.page, a.kind, a.color, a.width, &pts, ppp);
        }
        if self.tool == Tool::Eraser {
            if let Some(pos) = canvas_resp.hover_pos().or(canvas_resp.interact_pointer_pos()) {
                render::paint_eraser_cursor(&painter, pos, self.settings.eraser_radius * scale);
            }
        }

        // "Add page" button below the last page.
        let last = view.page_screen_rect(layout.count - 1);
        let btn = Rect::from_center_size(pos2(last.center().x, last.bottom() + 36.0), vec2(160.0, 36.0));
        if btn.intersects(canvas) {
            let resp = ui.interact(btn.intersect(canvas), Id::new("add_page"), Sense::click());
            let fill = if resp.hovered() { Color32::from_gray(95) } else { Color32::from_gray(75) };
            painter.rect_filled(btn, 18.0, fill);
            painter.text(
                btn.center(),
                Align2::CENTER_CENTER,
                "+ Add page",
                FontId::proportional(15.0),
                Color32::WHITE,
            );
            if resp.clicked() {
                self.doc.page_count += 1;
            }
        }

        // Scrollbars on top; the vertical one shows a page indicator while dragged.
        let center_y = self.scroll.y + canvas.height() / scale * 0.5;
        let label = format!("Page {} / {}", layout.page_index_at_y(center_y) + 1, layout.count);
        let full_painter = ui.painter();
        scrollbar::paint(full_painter, vbar_rect, true, &vstate, Some(&label));
        if let Some(h) = &hstate {
            scrollbar::paint(full_painter, hbar_rect, false, h, None);
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
                ui.heading("View");
                egui::ComboBox::from_label("Zoom")
                    .selected_text(self.settings.zoom.label())
                    .show_ui(ui, |ui| {
                        for z in ZoomMode::PRESETS {
                            ui.selectable_value(&mut self.settings.zoom, z, z.label());
                        }
                    });
                ui.small("Ctrl + mouse wheel (or pinch) also zooms.");

                ui.separator();
                ui.heading("Pages");
                egui::ComboBox::from_label("Paper size")
                    .selected_text(paper_label(self.doc.page_width, self.doc.page_height))
                    .show_ui(ui, |ui| {
                        for (name, dims, w, h) in PAPERS {
                            if ui.selectable_label(false, format!("{name}  ({dims})")).clicked() {
                                self.doc.page_width = w;
                                self.doc.page_height = h;
                            }
                        }
                    });
                ui.horizontal(|ui| {
                    ui.label("Width");
                    ui.add(
                        DragValue::new(&mut self.doc.page_width)
                            .range(200.0..=4000.0)
                            .speed(2.0)
                            .suffix(" px"),
                    );
                    ui.label("Height");
                    ui.add(
                        DragValue::new(&mut self.doc.page_height)
                            .range(200.0..=6000.0)
                            .speed(2.0)
                            .suffix(" px"),
                    );
                    if ui.button("Rotate").clicked() {
                        std::mem::swap(&mut self.doc.page_width, &mut self.doc.page_height);
                    }
                });
                ui.small("Existing ink keeps its position on the page; it is not rescaled.");
                ui.horizontal(|ui| {
                    ui.label(format!("{} page(s)", self.doc.page_count));
                    if ui.button("Add page").clicked() {
                        self.doc.page_count += 1;
                    }
                    let last = self.doc.page_count.saturating_sub(1);
                    let removable = self.doc.page_count > 1
                        && !self.doc.strokes.iter().any(|s| s.page == last);
                    if ui
                        .add_enabled(removable, egui::Button::new("Remove last page"))
                        .on_disabled_hover_text("Only an empty last page can be removed")
                        .clicked()
                    {
                        self.doc.page_count -= 1;
                    }
                });
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
                ui.small("Shortcuts: Ctrl+Z undo, Ctrl+Y redo, Ctrl+S save, F11 fullscreen, PageUp/PageDown scroll");
            });
        self.settings_open = open;
    }
}

/// Screen position -> point in the page's space, clamped to the page.
fn to_page_point(view: &View, page: u32, pos: Pos2, pressure: f32) -> Point {
    let c = view.screen_to_content(pos);
    let o = view.layout.page_origin(page);
    Point::new(
        (c.x - o.x).clamp(0.0, view.layout.page_w),
        (c.y - o.y).clamp(0.0, view.layout.page_h),
        pressure,
    )
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
        // automatically keeps pen strokes from starting underneath it. It is
        // moved inwards so it never covers the scrollbars.
        let inset = vec2(
            scrollbar::THICKNESS,
            if self.hbar_visible { scrollbar::THICKNESS } else { 0.0 },
        );
        let event = toolbar::show(
            ctx,
            &mut self.tool,
            &mut self.settings,
            self.history.can_undo(),
            self.history.can_redo(),
            inset,
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
