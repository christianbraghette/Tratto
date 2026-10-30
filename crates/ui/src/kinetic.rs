// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Kinetic scrolling: momentum, wheel acceleration and eased jumps.
//!
//! Four sources move the view:
//!
//! * **Wheel notches** add an impulse that decays exponentially, so a notch
//!   glides instead of jumping. Notches in quick succession in the same
//!   direction are amplified (up to [`MAX_BOOST`]), which makes long
//!   distances fast while single notches stay precise. How much is a
//!   setting.
//! * **Flings**: releasing a pan drag keeps the release velocity (scaled by
//!   a setting) and lets it decay with lower friction.
//! * **Swipes** on a touchpad behave like a finger on the screen: the view
//!   follows the stream of small deltas (eased over a few milliseconds, as
//!   the OS delivers them unevenly), faster the faster the swipe, and glides
//!   on with the release velocity once the deltas stop (fingers lifted).
//!   Where the OS adds its own inertia the stream just fades out, too slow
//!   to fling again.
//! * **Targets** (Page Up / Down, Home / End, jump to page) are approached with
//!   exponential easing.
//!
//! All quantities are in content units (seconds for time). With exponential
//! decay `v(t) = v0·e^(-k·t)` the total travel is `v0 / k`, so an impulse of
//! `d·k` moves exactly `d`; [`Kinetic::step`] integrates that exactly, which
//! keeps the distance independent of the frame rate.

use std::collections::VecDeque;

use eframe::egui::{Rect, Vec2};

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
/// Rate (1/s) at which swipe deltas are caught up with: ~90 % in 40 ms.
const SWIPE_RATE: f32 = 60.0;
/// No swipe delta for this long (s): the fingers were lifted.
const SWIPE_LIFT: f64 = 0.04;
/// Swipes slower than this (points/s on screen) are neither accelerated nor
/// glide on...
const SWIPE_SLOW_PT: f32 = 250.0;
/// ...from this speed on they get the full extra gain, at acceleration 1.
const SWIPE_FAST_PT: f32 = 2500.0;
const SWIPE_BOOST: f32 = 1.5;

#[derive(Default)]
pub struct Kinetic {
    velocity: Vec2,
    friction: f32,
    target: Option<Vec2>,
    boost: f32,
    last_notch: Option<(f64, Vec2)>,
    drag: VecDeque<(f64, Vec2)>,
    /// Recent touchpad deltas, as reported (before acceleration).
    swipe: VecDeque<(f64, Vec2)>,
    /// The acceleration of the latest swipe delta.
    swipe_gain: f32,
    /// Swipe distance not yet applied to the view.
    pending: Vec2,
}

impl Kinetic {
    /// Whether the view is still moving (the caller should keep repainting).
    pub fn is_active(&self) -> bool {
        self.velocity != Vec2::ZERO || self.target.is_some() || self.pending != Vec2::ZERO || self.is_swiping()
    }

    /// Whether a touchpad swipe is in progress (deltas still streaming).
    pub fn is_swiping(&self) -> bool {
        !self.swipe.is_empty()
    }

    /// Stops all motion (e.g. when the user starts drawing or zooms).
    pub fn stop(&mut self) {
        self.velocity = Vec2::ZERO;
        self.target = None;
        self.drag.clear();
        self.swipe.clear();
        self.pending = Vec2::ZERO;
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
        let rested = self.drag.back().is_none_or(|(last, _)| now - last > FLING_REST);
        let v = sample_velocity(&self.drag, now);
        self.drag.clear();
        if !rested && let Some(v) = v {
            self.fling(v, scale, strength, MIN_SPEED_PT * 4.0);
        }
    }

    /// One touchpad delta moving the view by `delta` content units, eased in
    /// over the next frames. `accel` scales how much faster quick swipes go
    /// (`0`: not at all, `1`: the default).
    pub fn swipe(&mut self, delta: Vec2, now: f64, scale: f32, accel: f32) {
        // Fingers on the pad hold the view, as on a touch screen.
        self.velocity = Vec2::ZERO;
        self.target = None;
        if self.swipe.back().is_some_and(|(_, prev)| prev.dot(delta) < 0.0) {
            // Reversed: the speed so far is the other way.
            self.swipe.clear();
            self.pending = Vec2::ZERO;
        }
        self.swipe.push_back((now, delta));
        while self.swipe.front().is_some_and(|(t, _)| now - t > FLING_WINDOW) {
            self.swipe.pop_front();
        }
        let speed_pt = sample_velocity(&self.swipe, now).map_or(0.0, |v| v.length() * scale);
        let fast = ((speed_pt - SWIPE_SLOW_PT) / (SWIPE_FAST_PT - SWIPE_SLOW_PT)).clamp(0.0, 1.0);
        self.swipe_gain = 1.0 + accel.max(0.0) * SWIPE_BOOST * fast;
        self.pending += delta * self.swipe_gain;
    }

    /// Ends the swipe once its deltas have stopped coming (the fingers were
    /// lifted): the view glides on with the release velocity times
    /// `strength` (`0`: no glide). Call every frame.
    pub fn swipe_idle(&mut self, now: f64, scale: f32, strength: f32) {
        let Some(&(last, _)) = self.swipe.back() else {
            return;
        };
        if now - last < SWIPE_LIFT {
            return;
        }
        let v = sample_velocity(&self.swipe, last);
        self.swipe.clear();
        if let Some(v) = v {
            self.fling(v * self.swipe_gain, scale, strength, SWIPE_SLOW_PT);
        }
    }

    /// Glides on at `v` times `strength`, if faster than `min_pt` (points/s
    /// on screen).
    fn fling(&mut self, v: Vec2, scale: f32, strength: f32, min_pt: f32) {
        let speed_pt = v.length() * scale;
        if speed_pt < min_pt || strength <= 0.0 {
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

    /// The view's coordinates moved by `by` (an infinite page grew left or
    /// up): a target is where it was.
    pub fn shift(&mut self, by: Vec2) {
        if let Some(t) = self.target.as_mut() {
            *t += by;
        }
    }

    /// The pending target, if an ease is running.
    pub fn target(&self) -> Option<Vec2> {
        self.target
    }

    /// Advances by `dt` seconds. Moves `scroll` and keeps it within `bounds`
    /// (see `View::scroll_bounds`).
    pub fn step(&mut self, scroll: &mut Vec2, dt: f32, bounds: Rect, scale: f32) {
        let clamp = |v: Vec2| v.clamp(bounds.min.to_vec2(), bounds.max.to_vec2());
        if self.pending != Vec2::ZERO {
            let k = 1.0 - (-SWIPE_RATE * dt).exp();
            let moved = if self.pending.length() * scale < 0.5 { self.pending } else { self.pending * k };
            let before = *scroll;
            *scroll = clamp(*scroll + moved);
            self.pending -= moved;
            // At an edge the rest of the swipe has nowhere to go.
            for d in 0..2 {
                if (*scroll)[d] != before[d] + moved[d] {
                    self.pending[d] = 0.0;
                }
            }
        }
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

/// The velocity (content units/s) of the movement `samples` record, over
/// the last [`FLING_WINDOW`] before `end`.
fn sample_velocity(samples: &VecDeque<(f64, Vec2)>, end: f64) -> Option<Vec2> {
    let recent: Vec<_> = samples.iter().filter(|(t, _)| end - t <= FLING_WINDOW).collect();
    let (first, last) = (recent.first()?.0, recent.last()?.0);
    // Each sample is the movement since the previous one, so the span starts
    // one interval before the first sample; approximated by the mean one.
    let n = recent.len() as f64;
    let span = if n > 1.0 { (last - first) * n / (n - 1.0) } else { 1.0 / 60.0 };
    let total = recent.iter().fold(Vec2::ZERO, |acc, (_, d)| acc + *d);
    Some(total / span.max(1.0 / 240.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    use eframe::egui::{Pos2, vec2};

    const MAX: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1e6, 1e6));

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
        let max = Rect::from_min_max(Pos2::ZERO, Pos2::new(0.0, 300.0));
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

    /// A swipe of `n` deltas of `d`, one every 1/120 s, then idle frames.
    fn swipe(k: &mut Kinetic, s: &mut Vec2, n: usize, d: f32, accel: f32, strength: f32) {
        let dt = 1.0 / 120.0;
        for i in 0..n {
            k.swipe(vec2(0.0, d), i as f64 * dt, 1.0, accel);
            k.step(s, dt as f32, MAX, 1.0);
        }
        for i in n..n + 600 {
            k.swipe_idle(i as f64 * dt, 1.0, strength);
            k.step(s, dt as f32, MAX, 1.0);
        }
    }

    #[test]
    fn slow_swipe_follows_the_fingers() {
        let mut k = Kinetic::default();
        let mut s = Vec2::ZERO;
        // 1 point per event: 120 points/s, below any acceleration or fling.
        swipe(&mut k, &mut s, 30, 1.0, 1.0, 1.0);
        assert!(!k.is_active());
        assert!((s.y - 30.0).abs() < 0.5, "{}", s.y);
    }

    #[test]
    fn fast_swipe_accelerates_and_glides() {
        let travel = |accel: f32, strength: f32| {
            let mut k = Kinetic::default();
            let mut s = Vec2::ZERO;
            swipe(&mut k, &mut s, 12, 30.0, accel, strength);
            assert!(!k.is_active());
            s.y
        };
        let plain = travel(0.0, 0.0);
        assert!((plain - 360.0).abs() < 0.5, "{plain}");
        assert!(travel(1.0, 0.0) > plain * 1.5, "accelerated");
        assert!(travel(0.0, 1.0) > plain * 2.0, "glides on after the lift");
    }

    #[test]
    fn touching_the_pad_stops_the_glide() {
        let mut k = Kinetic::default();
        let mut s = Vec2::ZERO;
        k.notch(vec2(0.0, 100.0), 0.0, 1.0);
        k.swipe(vec2(0.0, 1.0), 0.01, 1.0, 1.0);
        assert_eq!(k.velocity, Vec2::ZERO);
        k.swipe_idle(0.2, 1.0, 1.0);
        run(&mut k, &mut s);
        assert!(!k.is_active() && s.y <= 1.0);
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
