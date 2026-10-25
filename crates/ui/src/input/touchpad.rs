// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Touchpad scrolling for every scroll area: the canvas, the pages, the
//! settings, the windows.
//!
//! On Windows a touchpad reports lines like a mouse wheel (fractions of
//! one), unevenly and without inertia, and egui scrolls by them as by wheel
//! notches. Before egui sees them, [`Touchpad::process`] takes these
//! deltas out of the frame's input and runs them through a swipe of
//! [`Kinetic`]: followed like a finger, faster when quick, gliding on when
//! the fingers lift. What it moves each frame goes back in as one
//! point-sized wheel event; the stream opens with a `Start` phase, which
//! makes egui apply it as it comes (no smoothing of its own), and closes
//! with `End`.
//!
//! Mouse wheel notches, zoom (Ctrl / Cmd + wheel) and everything without
//! smooth scrolling pass through untouched.

use eframe::egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, RawInput, TouchPhase, Vec2};

use crate::kinetic::Kinetic;

/// Touchpad scrolling relative to the deltas it reports (before the
/// acceleration of quick swipes, see `kinetic`).
pub const GAIN: f32 = 0.6;
/// Where the swipe is followed (points): any place far enough from the
/// edges `Kinetic` stops at, close enough to 0 to keep `f32` precise.
const ORIGIN: Vec2 = Vec2::splat(10_000.0);
const SPACE: Vec2 = Vec2::splat(20_000.0);

#[derive(Default)]
pub struct Touchpad {
    kinetic: Kinetic,
    last: Option<f64>,
    /// A `Start` went out and no `End` yet.
    streaming: bool,
    modifiers: Modifiers,
}

impl Touchpad {
    /// Replaces the touchpad deltas in `raw` with the swipe's motion this
    /// frame. `line` is egui's line (points), `accel` the touchpad
    /// setting (acceleration and momentum). Returns whether the swipe still
    /// moves (the caller keeps repainting).
    pub fn process(&mut self, raw: &mut RawInput, line: f32, smooth: bool, accel: f32) -> bool {
        let now = raw.time.unwrap_or(self.last.unwrap_or(0.0));
        let dt = self.last.map_or(0.0, |t| (now - t).clamp(0.0, 1.0 / 30.0)) as f32;
        self.last = Some(now);
        if !smooth {
            self.kinetic.stop();
        }
        let mut swiped = false;
        raw.events.retain(|e| match e {
            Event::MouseWheel { unit, delta, modifiers, .. } if smooth && !zoom(modifiers) => {
                let swipe = match unit {
                    MouseWheelUnit::Point => true,
                    // A fast swipe may report a whole line now and then:
                    // within a swipe it is no notch.
                    MouseWheelUnit::Line => !is_wheel_notch(*unit, *delta) || self.kinetic.is_swiping(),
                    MouseWheelUnit::Page => false,
                };
                if !swipe {
                    // The mouse wheel takes over.
                    self.kinetic.stop();
                    return true;
                }
                let d = if *unit == MouseWheelUnit::Line { *delta * line } else { *delta };
                if d != Vec2::ZERO {
                    self.kinetic.swipe(d * GAIN, now, 1.0, accel);
                    self.modifiers = *modifiers;
                    swiped = true;
                }
                false
            }
            Event::PointerButton { pressed: true, .. } | Event::Touch { phase: TouchPhase::Start, .. } => {
                // A press holds the view, as a finger on the pad does.
                if !swiped {
                    self.kinetic.stop();
                }
                true
            }
            _ => true,
        });
        self.kinetic.swipe_idle(now, 1.0, accel);
        let mut at = ORIGIN;
        self.kinetic.step(&mut at, dt, SPACE, 1.0);
        let moved = at - ORIGIN;
        let wheel = |delta: Vec2, phase: TouchPhase, modifiers: Modifiers| Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta,
            phase,
            source: MouseWheelSource::Unknown,
            modifiers,
        };
        if moved != Vec2::ZERO {
            if !self.streaming {
                self.streaming = true;
                raw.events.push(wheel(Vec2::ZERO, TouchPhase::Start, self.modifiers));
            }
            raw.events.push(wheel(moved, TouchPhase::Move, self.modifiers));
        }
        let active = self.kinetic.is_active();
        if self.streaming && !active {
            self.streaming = false;
            raw.events.push(wheel(Vec2::ZERO, TouchPhase::End, self.modifiers));
        }
        active
    }
}

/// Ctrl / Cmd + wheel zooms (see `canvas`).
fn zoom(m: &Modifiers) -> bool {
    m.ctrl || m.command || m.mac_cmd
}

/// A wheel event from a mouse wheel's notches rather than a touchpad. Both
/// arrive as lines on Windows: a notch is whole lines, a touchpad sends
/// fractions (so does a high-resolution wheel, which then counts as a
/// touchpad). Pixel deltas come from touchpads.
pub fn is_wheel_notch(unit: MouseWheelUnit, delta: Vec2) -> bool {
    let whole = |v: f32| (v - v.round()).abs() < 1e-3;
    unit == MouseWheelUnit::Line && delta != Vec2::ZERO && whole(delta.x) && whole(delta.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    fn line(delta: f32, time: f64) -> RawInput {
        let event = Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(0.0, delta),
            phase: TouchPhase::Move,
            source: MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        };
        RawInput { time: Some(time), events: vec![event], ..Default::default() }
    }

    /// Runs frames at 120 Hz for `frames`, with `delta` lines in each of the
    /// first `swipe`; the total point wheel motion out, and the phases.
    fn run(delta: f32, swipe: usize, frames: usize) -> (f32, Vec<TouchPhase>) {
        let mut t = Touchpad::default();
        let (mut total, mut phases) = (0.0, Vec::new());
        for i in 0..frames {
            let time = i as f64 / 120.0;
            let mut raw =
                if i < swipe { line(delta, time) } else { RawInput { time: Some(time), ..Default::default() } };
            t.process(&mut raw, 40.0, true, 1.0);
            for e in raw.events {
                match e {
                    Event::MouseWheel { unit: MouseWheelUnit::Point, delta, phase, .. } => {
                        total += delta.y;
                        phases.push(phase);
                    }
                    other => panic!("left in: {other:?}"),
                }
            }
        }
        (total, phases)
    }

    #[test]
    fn swipe_becomes_one_point_stream() {
        let (total, phases) = run(0.25, 20, 600);
        assert_eq!(phases.first(), Some(&TouchPhase::Start));
        assert_eq!(phases.last(), Some(&TouchPhase::End));
        assert!(phases[1..phases.len() - 1].iter().all(|p| *p == TouchPhase::Move));
        // 20 deltas of 10 points at gain 0.6 is 120; quick enough to be
        // accelerated and to glide on.
        assert!(total > 200.0, "{total}");
    }

    #[test]
    fn slow_swipe_moves_as_reported() {
        // 0.25 points a frame, 30 points a second: no acceleration, no glide.
        let (total, _) = run(1.0 / 160.0, 120, 600);
        assert!((total - 120.0 * 0.25 * GAIN).abs() < 0.5, "{total}");
    }

    #[test]
    fn wheel_notches_and_zoom_pass_through() {
        let mut t = Touchpad::default();
        let mut raw = line(1.0, 0.0);
        t.process(&mut raw, 40.0, true, 1.0);
        assert_eq!(raw.events.len(), 1);
        let mut raw = line(0.3, 0.0);
        if let Event::MouseWheel { modifiers, .. } = &mut raw.events[0] {
            modifiers.ctrl = true;
        }
        t.process(&mut raw, 40.0, true, 1.0);
        assert!(matches!(raw.events[..], [Event::MouseWheel { unit: MouseWheelUnit::Line, .. }]));
        let mut raw = line(0.3, 0.0);
        t.process(&mut raw, 40.0, false, 1.0);
        assert!(matches!(raw.events[..], [Event::MouseWheel { unit: MouseWheelUnit::Line, .. }]));
    }
}
