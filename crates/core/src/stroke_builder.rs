// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turns raw pointer samples into a smooth control polygon with minimal latency.
//!
//! * **Spatial filter** – samples closer than `min_dist` to the last accepted
//!   one are dropped (kills sub-pixel jitter).
//! * **Velocity-adaptive smoothing** – an exponential filter whose strength
//!   depends on pen speed: slow, careful writing is smoothed strongly
//!   (`alpha_min`), fast strokes pass almost unfiltered (`alpha -> 1`), so the
//!   line does not lag behind the pen. This is a much better latency/jitter
//!   trade-off than a fixed moving average.
//! * **Live tip + prediction** – `preview_points` appends the newest raw
//!   sample (so the drawn line always reaches the pen) and optionally a short
//!   extrapolated segment that hides display latency. Neither is ever stored.
//!
//! The final curve interpolation happens in `stroke_mesh`.

use crate::model::Point;

/// Prediction horizon in samples, and its hard cap in document units.
const PREDICT_SAMPLES: f32 = 1.0;
const PREDICT_MAX: f32 = 12.0;
/// Below this per-sample speed no prediction is made (avoids jitter).
const PREDICT_MIN_SPEED: f32 = 1.5;

pub struct StrokeBuilder {
    min_dist: f32,
    alpha_min: f32,
    speed_ref: f32,
    out: Vec<Point>,
    smoothed: Point,
    last_raw: Point,
    prev_raw: Point,
    latest: Point,
}

impl StrokeBuilder {
    /// * `min_dist`  – jitter filter (document units)
    /// * `alpha_min` – smoothing factor at rest (0..1, lower = smoother)
    /// * `speed_ref` – per-sample distance at which smoothing is fully off
    pub fn new(min_dist: f32, alpha_min: f32, speed_ref: f32) -> Self {
        let z = Point::default();
        Self {
            min_dist,
            alpha_min,
            speed_ref: speed_ref.max(0.1),
            out: Vec::new(),
            smoothed: z,
            last_raw: z,
            prev_raw: z,
            latest: z,
        }
    }

    pub fn begin(&mut self, p: Point) {
        self.out.push(p);
        self.smoothed = p;
        self.last_raw = p;
        self.prev_raw = p;
        self.latest = p;
    }

    pub fn push(&mut self, p: Point) {
        self.latest = p;
        let d = p.distance(&self.last_raw);
        if d < self.min_dist {
            return;
        }
        let speed = (d / self.speed_ref).clamp(0.0, 1.0);
        let alpha = self.alpha_min + (1.0 - self.alpha_min) * speed;
        let a_p = alpha.max(0.5);
        self.smoothed.x += (p.x - self.smoothed.x) * alpha;
        self.smoothed.y += (p.y - self.smoothed.y) * alpha;
        self.smoothed.pressure += (p.pressure - self.smoothed.pressure) * a_p;

        self.prev_raw = self.last_raw;
        self.last_raw = p;
        self.out.push(self.smoothed);
    }

    /// Extrapolated pen position a short time ahead, if the pen moves fast enough.
    fn predicted_tip(&self) -> Option<Point> {
        let vx = self.last_raw.x - self.prev_raw.x;
        let vy = self.last_raw.y - self.prev_raw.y;
        let speed = (vx * vx + vy * vy).sqrt();
        if speed < PREDICT_MIN_SPEED {
            return None;
        }
        let k = (PREDICT_SAMPLES * speed).min(PREDICT_MAX) / speed;
        Some(Point::new(
            self.latest.x + vx * k,
            self.latest.y + vy * k,
            self.latest.pressure,
        ))
    }

    /// Control points for live rendering: smoothed path + live tip (+ prediction).
    pub fn preview_points(&self, predict: bool) -> Vec<Point> {
        let mut v = self.out.clone();
        if v.last().is_none_or(|o| o.distance(&self.latest) > 0.05) {
            v.push(self.latest);
        }
        if predict {
            if let Some(t) = self.predicted_tip() {
                v.push(t);
            }
        }
        v
    }

    /// Final control points to store (ends exactly where the pen lifted).
    pub fn finish(mut self) -> Vec<Point> {
        if self.out.last().is_none_or(|o| o.distance(&self.latest) > 0.05) {
            self.out.push(self.latest);
        }
        self.out
    }
}
