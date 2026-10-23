// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Native pen and drag-and-drop input: Wayland tablet and drop events turned
//! into egui events, pen pressure, and the pen's eraser end and side button.

use std::sync::Arc;

use eframe::egui::{self, Event, PointerButton, Pos2, TouchPhase};

use crate::input::wayland_drop::{DropEvent, DroppedPath};
use crate::input::wayland_pen::PenEvent;
use crate::toolbar::Tool;

use super::InkApp;

impl InkApp {
    /// Tracks pen pressure from touch/pen events (mouse stays at 1.0).
    pub(super) fn update_pressure(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            for e in &i.events {
                if let Event::Touch { phase, force, .. } = e {
                    match phase {
                        TouchPhase::End | TouchPhase::Cancel => self.pen_input.pressure = 1.0,
                        _ => {
                            if let Some(f) = force
                                && *f > 0.0
                            {
                                self.pen_input.pressure = f.clamp(0.0, 1.0);
                            }
                        }
                    }
                }
            }
        });
    }

    /// Wayland: reports the files dragged over and dropped onto the window
    /// as winit does elsewhere. While dragged their paths are unknown.
    pub(super) fn inject_drop_events(&mut self, raw_input: &mut egui::RawInput) {
        let Some(wl_drop) = self.pen_input.drop.as_ref() else { return };
        for ev in wl_drop.try_iter() {
            match ev {
                DropEvent::Enter => self.pen_input.drop_hover = true,
                DropEvent::Leave => self.pen_input.drop_hover = false,
                DropEvent::Drop(paths) => {
                    self.pen_input.drop_hover = false;
                    raw_input
                        .dropped_files
                        .extend(paths.into_iter().map(|path| Arc::new(DroppedPath(path)) as egui::DroppedFileHandle));
                }
            }
        }
        if self.pen_input.drop_hover {
            raw_input.hovered_files.push(egui::HoveredFile { path: None, mime: "text/uri-list".to_owned() });
        }
    }

    /// Converts native pen events into regular egui pointer events and pressure.
    /// Called from `raw_input_hook`, i.e. before egui processes the frame.
    pub(super) fn inject_pen_events(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(pen) = self.pen_input.wayland.as_ref() else { return };
        let events: Vec<PenEvent> = pen.try_iter().collect();
        let zoom = ctx.zoom_factor();
        // As of the last frame (the raw input has no modifier state).
        let modifiers = ctx.input(|i| i.modifiers);

        for ev in events {
            match ev {
                PenEvent::Motion { x, y, pressure, down, eraser } => {
                    let pos = Pos2::new(x / zoom, y / zoom);
                    self.pen_input.pos = pos;
                    self.pen_input.hover = (!down).then_some(pos);
                    raw_input.events.push(Event::PointerMoved(pos));
                    if down != self.pen_input.down {
                        self.pen_input.down = down;
                        raw_input.events.push(Event::PointerButton {
                            pos,
                            button: PointerButton::Primary,
                            pressed: down,
                            modifiers,
                        });
                    }
                    if down {
                        self.pen_input.pressure = pressure.clamp(0.0, 1.0);
                    }
                    self.pen_input.eraser = eraser;
                }
                PenEvent::Leave => {
                    self.pen_input.hover = None;
                    if self.pen_input.down {
                        self.pen_input.down = false;
                        raw_input.events.push(Event::PointerButton {
                            pos: self.pen_input.pos,
                            button: PointerButton::Primary,
                            pressed: false,
                            modifiers,
                        });
                    }
                    raw_input.events.push(Event::PointerGone);
                    self.pen_input.eraser = false;
                    self.pen_input.pressure = 1.0;
                }
            }
        }
    }

    /// Switches to the eraser while it is requested (pen eraser end or a held
    /// side button) and restores the previous tool afterwards.
    /// Pressure for new points: the pen's, brought towards full by the
    /// pressure strength, or full when pressure sensitivity is off (Settings).
    pub(super) fn pen_pressure(&self) -> f32 {
        self.applied_pressure(self.pen_input.pressure)
    }

    /// The pressure `raw` (the pen's) as the settings make it, see `pen_pressure`.
    pub(super) fn applied_pressure(&self, raw: f32) -> f32 {
        if self.settings.pressure {
            let strength = self.settings.pressure_strength.clamp(0.0, 1.0);
            1.0 - strength * (1.0 - raw)
        } else {
            1.0
        }
    }

    pub(super) fn apply_pen_eraser(&mut self, eraser: bool) {
        if eraser {
            if self.saved_tool.is_none() && self.tool != Tool::Eraser {
                self.saved_tool = Some(self.tool);
                self.tool = Tool::Eraser;
            }
        } else if let Some(t) = self.saved_tool.take() {
            self.tool = t;
        }
    }
}
