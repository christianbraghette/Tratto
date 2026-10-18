//! Turns raw pointer samples into a smooth polyline with minimal latency.
//!
//! 1. Spatial filter: samples closer than `min_dist` to the last accepted
//!    sample are dropped (removes sub-pixel jitter).
//! 2. Moving average over the last `window` accepted samples (x, y, pressure).
//!    A small window (3) keeps the lag to about one sample.
//! 3. The first sample is kept exactly; `finish` appends the exact last sample
//!    so strokes start and end where the pen did.

use std::collections::VecDeque;

use crate::model::Point;

pub struct StrokeBuilder {
    min_dist: f32,
    window: usize,
    tail: VecDeque<Point>,
    out: Vec<Point>,
    last_accepted: Option<Point>,
    latest: Option<Point>,
}

impl StrokeBuilder {
    pub fn new(min_dist: f32, window: usize) -> Self {
        Self {
            min_dist,
            window: window.max(1),
            tail: VecDeque::new(),
            out: Vec::new(),
            last_accepted: None,
            latest: None,
        }
    }

    pub fn begin(&mut self, p: Point) {
        self.tail.push_back(p);
        self.out.push(p);
        self.last_accepted = Some(p);
        self.latest = Some(p);
    }

    pub fn push(&mut self, p: Point) {
        self.latest = Some(p);
        if let Some(last) = self.last_accepted {
            if last.distance(&p) < self.min_dist {
                return;
            }
        }
        self.last_accepted = Some(p);
        self.tail.push_back(p);
        while self.tail.len() > self.window {
            self.tail.pop_front();
        }
        self.out.push(self.average());
    }

    fn average(&self) -> Point {
        let n = self.tail.len() as f32;
        let (x, y, pr) = self
            .tail
            .iter()
            .fold((0.0, 0.0, 0.0), |a, p| (a.0 + p.x, a.1 + p.y, a.2 + p.pressure));
        Point::new(x / n, y / n, pr / n)
    }

    /// Smoothed points so far (for live rendering).
    pub fn points(&self) -> &[Point] {
        &self.out
    }

    pub fn finish(mut self) -> Vec<Point> {
        if let Some(l) = self.latest {
            if self.out.last().map_or(true, |o| o.distance(&l) > 0.05) {
                self.out.push(l);
            }
        }
        self.out
    }
}
