// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Smoothed stroke centreline, shared by on-screen rendering and PDF export.
//!
//! The sparse (already filtered) control points are interpolated with
//! quadratic Béziers through the segment midpoints (C1-smooth, no overshoot).
//! The pressure-dependent radius is interpolated along the same curve, so
//! width changes are smooth too. Using one curve everywhere guarantees that
//! the printed ink matches what is seen.

use emath::{Pos2, pos2};

use crate::model::{Point, StrokeKind};

/// One sample of a stroke's smoothed centreline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub x: f32,
    pub y: f32,
    /// Half of the local stroke width.
    pub r: f32,
}

#[derive(Clone, Copy)]
struct Node {
    p: Pos2,
    r: f32,
}

/// Pressure -> width multiplier.
pub fn pressure_factor(p: f32) -> f32 {
    0.35 + 0.65 * p.clamp(0.0, 1.0)
}

/// How a stroke's points become samples: everything [`smoothed`] takes
/// besides the points.
#[derive(Clone, Copy)]
struct Params {
    kind: StrokeKind,
    width: f32,
    scale: f32,
    min_radius: f32,
    step: f32,
    min_dist: f32,
}

impl Params {
    fn node(&self, p: &Point) -> Node {
        let w = match self.kind {
            StrokeKind::Pen => self.width * pressure_factor(p.pressure),
            StrokeKind::Highlighter => self.width,
        };
        Node { p: pos2(p.x * self.scale, p.y * self.scale), r: (w * self.scale * 0.5).max(self.min_radius) }
    }

    /// Adds `n` to `out` unless it is closer than `min_dist` to the sample
    /// before it (the last of `out`, else `before`).
    fn push(&self, out: &mut Vec<Sample>, before: Option<&Sample>, n: Node) {
        let last = out.last().or(before);
        if last.is_none_or(|l| (n.p - pos2(l.x, l.y)).length() >= self.min_dist) {
            out.push(Sample { x: n.p.x, y: n.p.y, r: n.r });
        }
    }
}

/// The smoothed curve of a stroke, with all coordinates and radii multiplied
/// by `scale`.
///
/// * `min_radius` – lower bound for the (scaled) radius
/// * `step`       – target spacing of samples (scaled units)
/// * `min_dist`   – samples closer than this to their predecessor are dropped
///
/// The points are interpolated with quadratic Béziers through the midpoints
/// of their segments, so each piece of the curve depends on three points.
///
/// Never returns an empty vector.
pub fn smoothed(
    kind: StrokeKind,
    width: f32,
    pts: &[Point],
    scale: f32,
    min_radius: f32,
    step: f32,
    min_dist: f32,
) -> Vec<Sample> {
    let params = Params { kind, width, scale, min_radius, step, min_dist };
    let nodes: Vec<Node> = pts.iter().map(|p| params.node(p)).collect();
    let n = nodes.len();
    let mut out = Vec::with_capacity(if n < 3 { n } else { n * 3 });
    if n < 3 {
        for node in nodes {
            params.push(&mut out, None, node);
        }
    } else {
        params.push(&mut out, None, nodes[0]);
        for i in 1..n - 1 {
            segment([nodes[i - 1], nodes[i], nodes[i + 1]], i == 1, step, |node| params.push(&mut out, None, node));
        }
        params.push(&mut out, None, nodes[n - 1]);
    }
    if out.is_empty() {
        out.push(Sample { x: 0.0, y: 0.0, r: 0.5 });
    }
    out
}

/// The curve sampled every `step` page units. Used by the PDF export.
pub fn centerline(kind: StrokeKind, width: f32, pts: &[Point], step: f32) -> Vec<Sample> {
    if pts.is_empty() {
        return Vec::new();
    }
    smoothed(kind, width, pts, 1.0, 0.0, step, 0.01)
}

/// The piece of curve of the middle point of `[a, c, b]`: a quadratic Bézier
/// from the midpoint of `a`-`c` to that of `c`-`b`, with `c` as control. Each
/// piece starts where the one before ended, so only the `first` one has its
/// start.
fn segment([prev, c, next]: [Node; 3], first: bool, step: f32, mut out: impl FnMut(Node)) {
    let a = mid(prev, c);
    let b = mid(c, next);
    let len = (c.p - a.p).length() + (b.p - c.p).length();
    let k = ((len / step).ceil() as usize).clamp(1, 32);
    for j in if first { 0 } else { 1 }..=k {
        out(bezier(a, c, b, j as f32 / k as f32));
    }
}

/// [`smoothed`] for a stroke being drawn, made as it goes: its points only
/// grow at the end (the fixed ones), followed by a few that change every
/// time (the tip: the pen's live position and a prediction). The curve as
/// far as only fixed points shape it is made once; only the rest is made
/// again each time.
pub struct LiveCurve {
    params: Params,
    /// The curve that no point added later changes.
    fixed: Vec<Sample>,
    /// The next piece of curve (see `segment`) to add to `fixed`.
    next: usize,
}

impl LiveCurve {
    /// The parameters of [`smoothed`].
    pub fn new(kind: StrokeKind, width: f32, scale: f32, min_radius: f32, step: f32, min_dist: f32) -> Self {
        Self { params: Params { kind, width, scale, min_radius, step, min_dist }, fixed: Vec::new(), next: 1 }
    }

    /// The curve through `fixed` and then `tip`, exactly as [`smoothed`]
    /// makes it, as the samples no later point changes and the others.
    /// Between calls, `fixed` may only have grown at the end.
    pub fn update(&mut self, fixed: &[Point], tip: &[Point]) -> (&[Sample], Vec<Sample>) {
        let params = self.params;
        let m = fixed.len();
        if m < 3 {
            // Too short to have pieces that cannot change.
            let all: Vec<Point> = fixed.iter().chain(tip).copied().collect();
            let rest = if all.is_empty() { Vec::new() } else { smoothed_with(&params, &all) };
            return (&[], rest);
        }
        // The pieces of the middle points up to m - 2 only use fixed points.
        if self.fixed.is_empty() {
            params.push(&mut self.fixed, None, params.node(&fixed[0]));
        }
        while self.next <= m - 2 {
            let i = self.next;
            let nodes = [params.node(&fixed[i - 1]), params.node(&fixed[i]), params.node(&fixed[i + 1])];
            let out = &mut self.fixed;
            segment(nodes, i == 1, params.step, |node| params.push(out, None, node));
            self.next += 1;
        }
        // The rest: the pieces from m - 1 on, then the last point.
        let nodes: Vec<Node> = fixed[m - 2..].iter().chain(tip).map(|p| params.node(p)).collect();
        let before = self.fixed.last();
        let mut rest = Vec::new();
        for i in 1..nodes.len() - 1 {
            segment([nodes[i - 1], nodes[i], nodes[i + 1]], false, params.step, |node| {
                params.push(&mut rest, before, node);
            });
        }
        params.push(&mut rest, before, nodes[nodes.len() - 1]);
        (&self.fixed, rest)
    }
}

fn smoothed_with(p: &Params, pts: &[Point]) -> Vec<Sample> {
    smoothed(p.kind, p.width, pts, p.scale, p.min_radius, p.step, p.min_dist)
}

fn mid(a: Node, b: Node) -> Node {
    Node { p: pos2((a.p.x + b.p.x) * 0.5, (a.p.y + b.p.y) * 0.5), r: (a.r + b.r) * 0.5 }
}

fn bezier(a: Node, c: Node, b: Node, t: f32) -> Node {
    let u = 1.0 - t;
    let (wa, wc, wb) = (u * u, 2.0 * u * t, t * t);
    Node {
        p: pos2(a.p.x * wa + c.p.x * wc + b.p.x * wb, a.p.y * wa + c.p.y * wc + b.p.y * wb),
        r: a.r * wa + c.r * wc + b.r * wb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drawing point by point, with zero to two tip points each time, the
    /// live curve is always exactly the whole curve.
    #[test]
    fn the_live_curve_is_the_whole_curve() {
        for kind in [StrokeKind::Pen, StrokeKind::Highlighter] {
            let pts: Vec<Point> = (0..120)
                .map(|i| {
                    let t = i as f32;
                    // Fast and slow parts, a pause (repeated points), a reversal.
                    let x = if (40..50).contains(&i) { 80.0 } else { t * (1.0 + (t * 0.3).sin()) };
                    Point::new(x, (t * 0.2).sin() * 30.0 - if i > 90 { t } else { 0.0 }, 0.3 + (t * 0.1).cos().abs() * 0.6)
                })
                .collect();
            let mut live = LiveCurve::new(kind, 3.0, 1.7, 0.5, 2.5, 0.05);
            for m in 0..=pts.len() {
                let tip: Vec<Point> = (0..m % 3).map(|k| Point::new(500.0 + k as f32 * 7.0, 3.0, 0.8)).collect();
                let (fixed, rest) = live.update(&pts[..m], &tip);
                let got: Vec<Sample> = fixed.iter().copied().chain(rest).collect();
                let all: Vec<Point> = pts[..m].iter().chain(&tip).copied().collect();
                let whole = if all.is_empty() { Vec::new() } else { smoothed(kind, 3.0, &all, 1.7, 0.5, 2.5, 0.05) };
                assert_eq!(got, whole, "after {m} points");
            }
        }
    }

    #[test]
    fn centerline_follows_endpoints() {
        let pts: Vec<Point> = (0..10).map(|i| Point::new(i as f32 * 4.0, 0.0, 0.5)).collect();
        let s = centerline(StrokeKind::Pen, 2.0, &pts, 1.0);
        assert!(s.len() >= pts.len());
        assert_eq!((s[0].x, s[0].y), (0.0, 0.0));
        assert_eq!(s.last().unwrap().x, 36.0);
        assert!(centerline(StrokeKind::Pen, 2.0, &[], 1.0).is_empty());
    }
}
