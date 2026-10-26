// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Turns raw pointer samples into a smooth control polygon with minimal latency.
//!
//! * **Spatial filter** – samples closer than `min_dist` to the last accepted
//!   one are dropped (kills sub-pixel jitter).
//! * **Velocity-adaptive smoothing** – an exponential filter whose strength
//!   depends on pen speed: slow, careful writing is smoothed strongly
//!   (`alpha_min`), fast strokes pass almost unfiltered (`alpha -> 1`), so the
//!   line does not lag behind the pen. This is a much better latency/jitter
//!   trade-off than a fixed moving average.
//! * **Live tip + prediction** – `tip` gives the newest raw
//!   sample (so the drawn line always reaches the pen) and optionally a short
//!   extrapolated segment that hides display latency. Neither is ever stored.
//!   The prediction follows the direction of the last few samples, not of
//!   the last two (one jittery sample would send it astray), and it shrinks
//!   as the pen turns: small handwriting turns at almost every sample, and a
//!   straight guess there overshoots every curve and snaps back next frame.
//!
//! The final curve interpolation happens in `stroke_mesh`.

use std::collections::VecDeque;

use crate::model::Point;

/// Prediction horizon in samples, and its hard cap in document units.
const PREDICT_SAMPLES: f32 = 1.0;
const PREDICT_MAX: f32 = 6.0;
/// Per-sample speeds over which the prediction grows from nothing to all of
/// it (below, the pen is too slow to need one and its jitter would show).
const PREDICT_SPEED: (f32, f32) = (1.0, 3.0);
/// The pen turning by this much (the cosine of the angle between its last
/// two steps) or more between two samples: no prediction. It is all there
/// on a straight line, and shrinks in between.
const PREDICT_TURN_COS: f32 = 0.5;
/// Raw samples the prediction looks back on.
const HISTORY: usize = 4;

pub struct StrokeBuilder {
    min_dist: f32,
    alpha_min: f32,
    speed_ref: f32,
    out: Vec<Point>,
    smoothed: Point,
    last_raw: Point,
    /// The last accepted raw samples, oldest first (at most `HISTORY`).
    recent: VecDeque<Point>,
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
            recent: VecDeque::with_capacity(HISTORY),
            latest: z,
        }
    }

    pub fn begin(&mut self, p: Point) {
        self.out.push(p);
        self.smoothed = p;
        self.last_raw = p;
        self.recent.clear();
        self.recent.push_back(p);
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

        self.last_raw = p;
        if self.recent.len() == HISTORY {
            self.recent.pop_front();
        }
        self.recent.push_back(p);
        self.out.push(self.smoothed);
    }

    /// Extrapolated pen position a short time ahead, if the pen moves fast
    /// enough and fairly straight (see the module docs).
    fn predicted_tip(&self) -> Option<Point> {
        let steps: Vec<(f32, f32)> =
            self.recent.iter().zip(self.recent.iter().skip(1)).map(|(a, b)| (b.x - a.x, b.y - a.y)).collect();
        let [.., (ax, ay), (bx, by)] = steps[..] else { return None };
        // The newest steps count most.
        let (mut vx, mut vy, mut total) = (0.0, 0.0, 0.0);
        for (i, (x, y)) in steps.iter().enumerate() {
            let w = (i + 1) as f32;
            (vx, vy, total) = (vx + x * w, vy + y * w, total + w);
        }
        let (vx, vy) = (vx / total, vy / total);
        let speed = (vx * vx + vy * vy).sqrt();
        let fast = ((speed - PREDICT_SPEED.0) / (PREDICT_SPEED.1 - PREDICT_SPEED.0)).clamp(0.0, 1.0);
        let (la, lb) = ((ax * ax + ay * ay).sqrt(), (bx * bx + by * by).sqrt());
        let turn_cos = if la * lb > 0.0 { (ax * bx + ay * by) / (la * lb) } else { 1.0 };
        let straight = ((turn_cos - PREDICT_TURN_COS) / (1.0 - PREDICT_TURN_COS)).clamp(0.0, 1.0);
        let length = (PREDICT_SAMPLES * speed).min(PREDICT_MAX) * fast * straight;
        if length < 0.05 {
            return None;
        }
        let k = length / speed;
        Some(Point::new(self.latest.x + vx * k, self.latest.y + vy * k, self.latest.pressure))
    }

    /// The smoothed points so far. They never change: points are only
    /// added after them.
    pub fn points(&self) -> &[Point] {
        &self.out
    }

    /// What live rendering draws after [`Self::points`]: the live tip
    /// (+ prediction). At most two points, different every time.
    pub fn tip(&self, predict: bool) -> Vec<Point> {
        let mut v = Vec::with_capacity(2);
        if self.out.last().is_none_or(|o| o.distance(&self.latest) > 0.05) {
            v.push(self.latest);
        }
        if predict && let Some(t) = self.predicted_tip() {
            v.push(t);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A builder fed `points`, and its predicted tip.
    fn predicted(points: &[(f32, f32)]) -> Option<Point> {
        let mut b = StrokeBuilder::new(0.3, 0.4, 6.0);
        let mut pts = points.iter().map(|&(x, y)| Point::new(x, y, 0.5));
        b.begin(pts.next().unwrap());
        pts.for_each(|p| b.push(p));
        b.predicted_tip()
    }

    #[test]
    fn a_straight_fast_line_is_predicted_ahead() {
        let tip = predicted(&[(0.0, 0.0), (4.0, 0.0), (8.0, 0.0), (12.0, 0.0)]).unwrap();
        assert!((tip.x - 16.0).abs() < 1e-3 && tip.y.abs() < 1e-3, "{tip:?}");
        // Never far ahead, however fast.
        let tip = predicted(&[(0.0, 0.0), (40.0, 0.0), (80.0, 0.0), (120.0, 0.0)]).unwrap();
        assert!((tip.x - 120.0 - PREDICT_MAX).abs() < 1e-3);
    }

    #[test]
    fn small_tight_curves_and_slow_strokes_get_little_or_none() {
        // Small handwriting: a zigzag turning at every sample.
        assert!(predicted(&[(0.0, 0.0), (2.0, 2.0), (4.0, 0.0), (6.0, 2.0)]).is_none());
        // A curve turning by 30 degrees a sample: shorter than its steps.
        let arc: Vec<(f32, f32)> = (0..4)
            .map(|i| (i as f32 * std::f32::consts::FRAC_PI_6).cos() * 6.0)
            .zip((0..4).map(|i| (i as f32 * std::f32::consts::FRAC_PI_6).sin() * 6.0))
            .collect();
        let tip = predicted(&arc).unwrap();
        let (last, step) = (arc[3], 6.0 * 2.0 * (std::f32::consts::FRAC_PI_6 / 2.0).sin());
        assert!(((tip.x - last.0).powi(2) + (tip.y - last.1).powi(2)).sqrt() < step * 0.8);
        // Slow: nothing.
        assert!(predicted(&[(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.5, 0.0)]).is_none());
    }

    #[test]
    fn one_jittery_sample_does_not_swing_it_around() {
        // Going right, the last step a little up: still mostly right.
        let tip = predicted(&[(0.0, 0.0), (4.0, 0.0), (8.0, 0.0), (12.0, 0.0), (16.0, -1.5)]).unwrap();
        assert!(tip.x > 16.0 && tip.y > -1.5 - 1.0, "{tip:?}");
    }
}
