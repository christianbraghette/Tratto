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
#[derive(Clone, Copy, Debug)]
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

/// The smoothed curve of a stroke, with all coordinates and radii multiplied
/// by `scale`.
///
/// * `min_radius` – lower bound for the (scaled) radius
/// * `step`       – target spacing of samples (scaled units)
/// * `min_dist`   – samples closer than this to their predecessor are dropped
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
    let nodes: Vec<Node> = pts
        .iter()
        .map(|p| {
            let w = match kind {
                StrokeKind::Pen => width * pressure_factor(p.pressure),
                StrokeKind::Highlighter => width,
            };
            Node { p: pos2(p.x * scale, p.y * scale), r: (w * scale * 0.5).max(min_radius) }
        })
        .collect();
    dedup(smooth(&nodes, step), min_dist).into_iter().map(|n| Sample { x: n.p.x, y: n.p.y, r: n.r }).collect()
}

/// The curve sampled every `step` page units. Used by the PDF export.
pub fn centerline(kind: StrokeKind, width: f32, pts: &[Point], step: f32) -> Vec<Sample> {
    if pts.is_empty() {
        return Vec::new();
    }
    smoothed(kind, width, pts, 1.0, 0.0, step, 0.01)
}

/// Quadratic Bézier through segment midpoints, using the points as controls.
fn smooth(nodes: &[Node], step: f32) -> Vec<Node> {
    let n = nodes.len();
    if n < 3 {
        return nodes.to_vec();
    }
    let mut out = Vec::with_capacity(n * 3);
    out.push(nodes[0]);
    for i in 1..n - 1 {
        let a = mid(nodes[i - 1], nodes[i]);
        let c = nodes[i];
        let b = mid(nodes[i], nodes[i + 1]);
        let len = (c.p - a.p).length() + (b.p - c.p).length();
        let k = ((len / step).ceil() as usize).clamp(1, 32);
        // The curve of segment i starts where the previous one ended.
        let start = if i == 1 { 0 } else { 1 };
        for j in start..=k {
            out.push(bezier(a, c, b, j as f32 / k as f32));
        }
    }
    out.push(nodes[n - 1]);
    out
}

fn dedup(nodes: Vec<Node>, min_dist: f32) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    for n in nodes {
        if out.last().is_none_or(|l| (n.p - l.p).length() >= min_dist) {
            out.push(n);
        }
    }
    if out.is_empty() {
        out.push(Node { p: Pos2::ZERO, r: 0.5 });
    }
    out
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
