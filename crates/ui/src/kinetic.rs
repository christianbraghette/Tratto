// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Kinetic scrolling: momentum, wheel acceleration and eased jumps.
//!
//! Three sources move the view:
//!
//! * **Wheel notches** add an impulse that decays exponentially, so a notch
//!   glides instead of jumping. Notches in quick succession in the same
//!   direction are amplified (up to [`MAX_BOOST`]), which makes long
//!   distances fast while single notches stay precise. How much is a
//!   setting, separate for the mouse wheel and the touchpad.
//! * **Flings**: releasing a pan drag keeps the release velocity (scaled by
//!   a setting) and lets it decay with lower friction.
//! * **Targets** (Page Up / Down, Home / End, jump to page) are approached with
//!   exponential easing.
//!
//! Precise touchpad deltas are *not* routed through here: they are applied
//! 1:1 because the OS already provides their inertia where it exists.
//!
//! All quantities are in content units (seconds for time). With exponential
//! decay `v(t) = v0·e^(-k·t)` the total travel is `v0 / k`, so an impulse of
//! `d·k` moves exactly `d`; [`Kinetic::step`] integrates that exactly, which
//! keeps the distance independent of the frame rate.

use std::collections::VecDeque;

use eframe::egui::{Vec2, vec2};

/// Friction of wheel glides (1/s): ~90 % of the distance in 0.25 s.
const WHEEL_FRICTION: f32 = 9.0;
/// Friction of flings (1/s): a longer, softer glide.
const FLING_FRICTION: f32 = 3.5;
/// Easing rate towards a target (1/s): ~95 % in 0.2 s.
const TARGET_RATE: f32 = 15.0;
/// Notches closer together than this (s) accelerate.
const ACCEL_WINDOW: f64 = 0.15;
/// Boost added per accelerated notch, and its upper bound, at acceleration 1.
const BOOST_STEP: f32 = 0.35;
pub const MAX_BOOST: f32 = 4.0;
/// Drag samples older than this (s) do not count towards the fling.
const FLING_WINDOW: f64 = 0.1;
/// A drag that rested this long (s) before release does not fling.
const FLING_REST: f64 = 0.05;
/// Below this speed (points/s on screen) motion stops.
const MIN_SPEED_PT: f32 = 20.0;
/// Fling speed limit (points/s on screen).
const MAX_FLING_PT: f32 = 6000.0;

#[derive(Default)]
pub struct Kinetic {
    velocity: Vec2,
    friction: f32,
    target: Option<Vec2>,
    boost: f32,
    last_notch: Option<(f64, Vec2)>,
    drag: VecDeque<(f64, Vec2)>,
}

impl Kinetic {
    /// Whether the view is still moving (the caller should keep repainting).
    pub fn is_active(&self) -> bool {
        self.velocity != Vec2::ZERO || self.target.is_some()
    }

    /// Stops all motion (e.g. when the user starts drawing or zooms).
    pub fn stop(&mut self) {
        self.velocity = Vec2::ZERO;
        self.target = None;
        self.drag.clear();
    }

    /// One discrete wheel step moving the view by `delta` content units.
    /// `accel` scales the acceleration of quick notches (`0`: none, `1`: the
    /// default).
    pub fn notch(&mut self, delta: Vec2, now: f64, accel: f32) {
        let same_dir = |a: Vec2, b: Vec2| a.dot(b) > 0.0;
        let accel = accel.max(0.0);
        self.boost = match self.last_notch {
            Some((t, prev)) if now - t < ACCEL_WINDOW && same_dir(prev, delta) => {
                (self.boost + BOOST_STEP * accel).min(1.0 + (MAX_BOOST - 1.0) * accel)
            }
            _ => 1.0,
        };
        self.last_notch = Some((now, delta));
        // Reversing direction cancels the remaining glide immediately.
        if !same_dir(self.velocity, delta) {
            self.velocity = Vec2::ZERO;
        }
        // Continue from wherever an ongoing ease would have ended.
        self.target = None;
        self.friction = WHEEL_FRICTION;
        self.velocity += delta * self.boost * WHEEL_FRICTION;
    }

    /// The view was dragged by `delta` content units (already applied).
    pub fn drag(&mut self, delta: Vec2, now: f64) {
        self.velocity = Vec2::ZERO;
        self.target = None;
        self.drag.push_back((now, delta));
        while self.drag.front().is_some_and(|(t, _)| now - t > FLING_WINDOW) {
            self.drag.pop_front();
        }
    }

    /// The drag ended; continues with the release velocity, if any, times
    /// `strength` (`0`: no fling, `1`: the default).
    pub fn release(&mut self, now: f64, scale: f32, strength: f32) {
        let samples: Vec<_> = self.drag.drain(..).filter(|(t, _)| now - t <= FLING_WINDOW).collect();
        let Some(&(last, _)) = samples.last() else {
            return;
        };
        if now - last > FLING_REST {
            return;
        }
        let first = samples[0].0;
        // Each sample is the movement of one frame, so the span starts one
        // frame before the first sample; approximated by the mean interval.
        let n = samples.len() as f64;
        let span = if n > 1.0 { (last - first) * n / (n - 1.0) } else { 1.0 / 60.0 };
        let total = samples.iter().fold(Vec2::ZERO, |acc, (_, d)| acc + *d);
        let v = total / span.max(1.0 / 240.0) as f32;
        let speed_pt = v.length() * scale;
        if speed_pt < MIN_SPEED_PT * 4.0 || strength <= 0.0 {
            return;
        }
        self.velocity = v * (MAX_FLING_PT / speed_pt).min(1.0) * strength;
        self.friction = FLING_FRICTION;
    }

    /// Eases towards `target` (unclamped; the caller clamps every frame).
    pub fn ease_to(&mut self, target: Vec2) {
        self.velocity = Vec2::ZERO;
        self.target = Some(target);
    }

    /// The pending target, if an ease is running.
    pub fn target(&self) -> Option<Vec2> {
        self.target
    }

    /// Advances by `dt` seconds. Moves `scroll` and keeps it within `0..=max`.
    pub fn step(&mut self, scroll: &mut Vec2, dt: f32, max: Vec2, scale: f32) {
        let clamp = |v: Vec2| vec2(v.x.clamp(0.0, max.x), v.y.clamp(0.0, max.y));
        if let Some(t) = self.target {
            let t = clamp(t);
            let k = 1.0 - (-TARGET_RATE * dt).exp();
            *scroll += (t - *scroll) * k;
            if ((t - *scroll).length() * scale) < 0.5 {
                *scroll = t;
                self.target = None;
            } else {
                self.target = Some(t);
            }
            return;
        }
        if self.velocity == Vec2::ZERO {
            return;
        }
        let decay = (-self.friction * dt).exp();
        let moved = self.velocity * ((1.0 - decay) / self.friction);
        let before = *scroll;
        *scroll = clamp(*scroll + moved);
        self.velocity *= decay;
        // Hitting an edge ends the motion along that axis.
        for d in 0..2 {
            if (*scroll)[d] != before[d] + moved[d] {
                self.velocity[d] = 0.0;
            }
        }
        if self.velocity.length() * scale < MIN_SPEED_PT {
            self.velocity = Vec2::ZERO;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: Vec2 = Vec2::new(1e6, 1e6);

    fn run(k: &mut Kinetic, scroll: &mut Vec2) {
        for _ in 0..600 {
            k.step(scroll, 1.0 / 120.0, MAX, 1.0);
        }
    }

    #[test]
    fn single_notch_travels_its_distance() {
        let mut k = Kinetic::default();
        let mut s = vec2(0.0, 500.0);
        k.notch(vec2(0.0, 100.0), 0.0, 1.0);
        run(&mut k, &mut s);
        assert!(!k.is_active());
        // Stops a little short (minimum speed), never overshoots.
        assert!(s.y > 597.0 && s.y <= 600.0, "{}", s.y);
    }

    #[test]
    fn fast_notches_accelerate_slow_ones_do_not() {
        let mut fast = Kinetic::default();
        let mut slow = Kinetic::default();
        let (mut sf, mut ss) = (Vec2::ZERO, Vec2::ZERO);
        for i in 0..5 {
            fast.notch(vec2(0.0, 100.0), i as f64 * 0.05, 1.0);
            slow.notch(vec2(0.0, 100.0), i as f64 * 0.5, 1.0);
        }
        run(&mut fast, &mut sf);
        run(&mut slow, &mut ss);
        assert!(sf.y > ss.y * 1.5, "fast {} slow {}", sf.y, ss.y);
        assert!(slow.boost == 1.0 && fast.boost > 2.0);
    }

    #[test]
    fn acceleration_scales_the_boost() {
        let boost_after = |accel: f32| {
            let mut k = Kinetic::default();
            for i in 0..20 {
                k.notch(vec2(0.0, 100.0), i as f64 * 0.05, accel);
            }
            k.boost
        };
        assert_eq!(boost_after(0.0), 1.0, "off: every notch moves the same");
        assert_eq!(boost_after(1.0), MAX_BOOST);
        assert_eq!(boost_after(2.0), 1.0 + (MAX_BOOST - 1.0) * 2.0);
    }

    #[test]
    fn reversing_cancels_the_glide() {
        let mut k = Kinetic::default();
        let mut s = vec2(0.0, 1000.0);
        k.notch(vec2(0.0, 100.0), 0.0, 1.0);
        k.step(&mut s, 0.01, MAX, 1.0);
        k.notch(vec2(0.0, -100.0), 0.02, 1.0);
        assert_eq!(k.boost, 1.0);
        assert!(k.velocity.y < 0.0);
    }

    #[test]
    fn fling_continues_and_stops_at_edges() {
        let mut k = Kinetic::default();
        let mut s = vec2(0.0, 0.0);
        for i in 0..6 {
            k.drag(vec2(0.0, 20.0), i as f64 / 60.0);
        }
        k.release(5.0 / 60.0 + 0.005, 1.0, 1.0);
        assert!(k.is_active());
        let max = vec2(0.0, 300.0);
        for _ in 0..600 {
            k.step(&mut s, 1.0 / 120.0, max, 1.0);
        }
        assert_eq!(s.y, 300.0);
        assert!(!k.is_active());
    }

    #[test]
    fn fling_strength_scales_the_glide() {
        let glide = |strength: f32| {
            let mut k = Kinetic::default();
            let mut s = Vec2::ZERO;
            for i in 0..6 {
                k.drag(vec2(0.0, 20.0), i as f64 / 60.0);
            }
            k.release(5.0 / 60.0 + 0.005, 1.0, strength);
            run(&mut k, &mut s);
            s.y
        };
        assert_eq!(glide(0.0), 0.0, "off: no fling");
        assert!(glide(2.0) > glide(1.0) * 1.8);
    }

    #[test]
    fn resting_before_release_does_not_fling() {
        let mut k = Kinetic::default();
        k.drag(vec2(0.0, 20.0), 0.0);
        k.release(0.2, 1.0, 1.0);
        assert!(!k.is_active());
    }

    #[test]
    fn ease_reaches_clamped_target() {
        let mut k = Kinetic::default();
        let mut s = Vec2::ZERO;
        k.ease_to(vec2(0.0, f32::MAX));
        run(&mut k, &mut s);
        assert_eq!(s.y, 1e6);
        assert!(!k.is_active());
    }
}
