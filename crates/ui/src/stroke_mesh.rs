// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: GPL-3.0-or-later

//! Anti-aliased stroke tessellation.
//!
//! A stroke is turned into one triangle mesh:
//!
//! 1. The smoothed centreline comes from [`tratto_core::stroke_path`],
//!    sampled densely enough that curves look round at any zoom.
//! 2. A ribbon with 4 vertices per sample is built: two solid inner vertices
//!    and two fully transparent outer vertices, one `aa` (= 1 physical pixel)
//!    further out. The GPU interpolates the alpha across that fringe, which
//!    gives analytic anti-aliasing independent of MSAA.
//! 3. Round caps (half-discs with the same fringe) close both ends; a
//!    single-point stroke becomes a full disc.
//!
//! All output positions are `document_units * scale` (no translation), which
//! lets the cache reuse a mesh while scrolling.
//!
//! [`LiveMesh`] makes the same triangles for the stroke being drawn, as it
//! grows: only its end is made again each frame.

use std::f32::consts::{PI, TAU};
use std::ops::Range;

use eframe::egui::{Color32, Pos2, Vec2, epaint::Mesh, pos2, vec2};
use tratto_core::model::{Point, StrokeKind};
use tratto_core::stroke_path::{self, LiveCurve, Sample};

use crate::stroke_gpu::Batch;

/// Target spacing of generated curve samples, in physical pixels.
const SAMPLE_STEP_PX: f32 = 2.5;

/// Appends the tessellated stroke to `mesh`.
///
/// * `scale` – document units -> points
/// * `aa`    – feather width in points (`1.0 / pixels_per_point`)
pub fn tessellate(mesh: &mut Mesh, kind: StrokeKind, rgba: [u8; 4], width: f32, pts: &[Point], scale: f32, aa: f32) {
    if pts.is_empty() {
        return;
    }
    // Never thinner than one physical pixel.
    let dense = stroke_path::smoothed(kind, width, pts, scale, aa * 0.5, SAMPLE_STEP_PX * aa, 0.05 * aa);
    whole(mesh, &dense, solid(rgba), aa);
}

fn solid(rgba: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}

/// All the triangles of the curve `dense`: ribbon and round caps, or a disc.
fn whole(mesh: &mut Mesh, dense: &[Sample], solid: Color32, aa: f32) {
    if let [dot] = dense {
        let r = dot.r;
        let half_aa = aa * 0.5;
        let segs = arc_segments(r, aa) * 2;
        add_arc(mesh, at(dot), vec2(1.0, 0.0), vec2(0.0, 1.0), r - half_aa, r + half_aa, solid, TAU, segs);
        return;
    }
    ribbon(mesh, dense, 0..dense.len(), None, solid, aa * 0.5);
    caps(mesh, dense, solid, aa);
}

fn at(s: &Sample) -> Pos2 {
    pos2(s.x, s.y)
}

/// Unit direction from sample `k` to `k + 1`.
fn dir(dense: &[Sample], k: usize) -> Vec2 {
    unit(at(&dense[k + 1]) - at(&dense[k]))
}

/// The ribbon rows of samples `rows` of `dense` (at least two samples): two
/// solid inner vertices and two transparent outer ones each, one `aa`
/// apart, and the quads between the rows. With `after`, the first row is
/// also joined to the row whose vertices start there (one made before).
fn ribbon(mesh: &mut Mesh, dense: &[Sample], rows: Range<usize>, after: Option<u32>, solid: Color32, half_aa: f32) {
    let n = dense.len();
    let first = mesh.vertices.len() as u32;
    mesh.reserve_vertices(rows.len() * 4);
    mesh.reserve_triangles(rows.len() * 6);
    for i in rows.clone() {
        let n0 = perp(dir(dense, i.saturating_sub(1)));
        let n1 = perp(dir(dense, i.min(n - 2)));
        let mut nrm = unit(n0 + n1);
        if nrm == Vec2::ZERO {
            nrm = n0; // 180° reversal
        }
        // Miter scale keeps the width constant around bends (limited to 2x).
        let s = 1.0 / nrm.dot(n0).clamp(0.5, 1.0);
        let r = dense[i].r;
        let r_in = (r - half_aa).max(0.0) * s;
        let r_out = (r + half_aa) * s;
        let c = at(&dense[i]);
        mesh.colored_vertex(c + nrm * r_out, Color32::TRANSPARENT);
        mesh.colored_vertex(c + nrm * r_in, solid);
        mesh.colored_vertex(c - nrm * r_in, solid);
        mesh.colored_vertex(c - nrm * r_out, Color32::TRANSPARENT);
    }
    let starts = after.into_iter().chain((0..rows.len() as u32).map(|r| first + 4 * r));
    for (a0, c0) in starts.clone().zip(starts.skip(1)) {
        for k in 0..3u32 {
            let (a, c) = (a0 + k, c0 + k);
            mesh.add_triangle(a, a + 1, c);
            mesh.add_triangle(a + 1, c + 1, c);
        }
    }
}

/// The round caps of the curve `dense` (at least two samples).
fn caps(mesh: &mut Mesh, dense: &[Sample], solid: Color32, aa: f32) {
    let n = dense.len();
    let half_aa = aa * 0.5;
    let (d0, d1) = (dir(dense, 0), dir(dense, n - 2));
    let (r0, r1) = (dense[0].r, dense[n - 1].r);
    add_arc(mesh, at(&dense[0]), perp(d0), -d0, r0 - half_aa, r0 + half_aa, solid, PI, arc_segments(r0, aa));
    add_arc(mesh, at(&dense[n - 1]), perp(d1), d1, r1 - half_aa, r1 + half_aa, solid, PI, arc_segments(r1, aa));
}

/// The stroke being drawn, tessellated as it grows: the same triangles as
/// [`tessellate`], but the ribbon of the part of the curve that no longer
/// changes (see `LiveCurve`) is made once and kept in a [`Batch`] that only
/// grows (so only its new triangles are uploaded). Each frame makes just the
/// end of the ribbon and the two caps.
pub struct LiveMesh {
    curve: LiveCurve,
    solid: Color32,
    aa: f32,
    /// The ribbon rows of the fixed samples made so far (row `r` is
    /// vertices `4 r ..`).
    fixed: Batch,
    rows: usize,
}

impl LiveMesh {
    /// The parameters of [`tessellate`].
    pub fn new(kind: StrokeKind, rgba: [u8; 4], width: f32, scale: f32, aa: f32) -> Self {
        Self {
            curve: LiveCurve::new(kind, width, scale, aa * 0.5, SAMPLE_STEP_PX * aa, 0.05 * aa),
            solid: solid(rgba),
            aa,
            fixed: Batch::default(),
            rows: 0,
        }
    }

    /// Brings the stroke to `fixed` points (which only grow) followed by `tip`
    /// (see `StrokeBuilder`). Returns the triangles made for this frame only;
    /// the others are in [`Self::fixed`].
    pub fn update(&mut self, fixed: &[Point], tip: &[Point]) -> Mesh {
        let (solid, aa) = (self.solid, self.aa);
        let (done, rest) = self.curve.update(fixed, tip);
        let mut mesh = Mesh::default();
        if done.len() < 3 {
            let all: Vec<Sample> = done.iter().copied().chain(rest).collect();
            if !all.is_empty() {
                whole(&mut mesh, &all, solid, aa);
            }
            return mesh;
        }
        // A row's vertices depend on the samples on both sides: the rows of
        // all fixed samples but the last are final.
        let last = done.len() - 2;
        if self.rows <= last {
            let (from, after) = (self.rows, self.rows.checked_sub(1).map(|r| 4 * r as u32));
            self.fixed.append(|m| ribbon(m, done, from..last + 1, after, solid, aa * 0.5));
            self.rows = last + 1;
        }
        // The rest of the ribbon from the last kept row on (made again, joining
        // it), with the samples on either side, then the caps.
        let end: Vec<Sample> = done[last - 1..].iter().copied().chain(rest).collect();
        ribbon(&mut mesh, &end, 1..end.len(), None, solid, aa * 0.5);
        let (d0, d1) = (dir(done, 0), dir(&end, end.len() - 2));
        let half_aa = aa * 0.5;
        let (first, tip_end) = (done[0], end[end.len() - 1]);
        add_arc(&mut mesh, at(&first), perp(d0), -d0, first.r - half_aa, first.r + half_aa, solid, PI, arc_segments(first.r, aa));
        add_arc(
            &mut mesh,
            at(&tip_end),
            perp(d1),
            d1,
            tip_end.r - half_aa,
            tip_end.r + half_aa,
            solid,
            PI,
            arc_segments(tip_end.r, aa),
        );
        mesh
    }

    /// The triangles kept from frame to frame.
    pub fn fixed(&self) -> &Batch {
        &self.fixed
    }
}

fn unit(v: Vec2) -> Vec2 {
    let l = v.length();
    if l > 1e-6 { v / l } else { Vec2::ZERO }
}

fn perp(d: Vec2) -> Vec2 {
    vec2(-d.y, d.x)
}

fn arc_segments(r: f32, aa: f32) -> usize {
    (((r / aa).sqrt() * 2.0).ceil()).clamp(4.0, 24.0) as usize
}

/// Fan + feather band sweeping `sweep` radians from direction `n` towards `dir`.
#[allow(clippy::too_many_arguments)]
fn add_arc(
    mesh: &mut Mesh,
    c: Pos2,
    n: Vec2,
    dir: Vec2,
    r_in: f32,
    r_out: f32,
    solid: Color32,
    sweep: f32,
    segs: usize,
) {
    let r_in = r_in.max(0.0);
    let center = mesh.vertices.len() as u32;
    mesh.colored_vertex(c, solid);
    let first = center + 1;
    for k in 0..=segs {
        let th = sweep * k as f32 / segs as f32;
        let v = n * th.cos() + dir * th.sin();
        mesh.colored_vertex(c + v * r_in, solid);
        mesh.colored_vertex(c + v * r_out, Color32::TRANSPARENT);
    }
    for k in 0..segs as u32 {
        let i0 = first + 2 * k;
        let o0 = i0 + 1;
        let i1 = i0 + 2;
        let o1 = i0 + 3;
        mesh.add_triangle(center, i0, i1);
        mesh.add_triangle(i0, o0, i1);
        mesh.add_triangle(i1, o0, o1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meshes_are_valid() {
        let pts: Vec<Point> =
            (0..20).map(|i| Point::new(i as f32 * 5.0, (i as f32 * 0.7).sin() * 10.0, 0.3 + 0.03 * i as f32)).collect();
        for kind in [StrokeKind::Pen, StrokeKind::Highlighter] {
            let mut m = Mesh::default();
            tessellate(&mut m, kind, [0, 0, 0, 255], 3.0, &pts, 1.0, 1.0);
            assert!(!m.is_empty() && m.is_valid());
        }
        let mut dot = Mesh::default();
        tessellate(&mut dot, StrokeKind::Pen, [0, 0, 0, 255], 3.0, &pts[..1], 1.0, 1.0);
        assert!(!dot.is_empty() && dot.is_valid());
    }

    /// The triangles of a mesh, as their corners, in a canonical order.
    fn triangles(meshes: &[&Mesh]) -> Vec<[(u32, u32, [u8; 4]); 3]> {
        let mut out: Vec<_> = meshes
            .iter()
            .flat_map(|m| {
                m.indices.chunks(3).map(|t| {
                    let mut c = [0, 1, 2].map(|k| {
                        let v = m.vertices[t[k] as usize];
                        (v.pos.x.to_bits(), v.pos.y.to_bits(), v.color.to_array())
                    });
                    c.sort();
                    c
                })
            })
            .collect();
        out.sort();
        out
    }

    /// While a stroke is drawn, the live mesh (kept + this frame's) has
    /// exactly the triangles of the whole stroke tessellated at once.
    #[test]
    fn the_live_mesh_is_the_whole_mesh() {
        let pts: Vec<Point> = (0..80)
            .map(|i| {
                let t = i as f32;
                Point::new(t * 4.0, (t * 0.3).sin() * 20.0 - if i > 60 { t * 3.0 } else { 0.0 }, 0.3 + 0.008 * t)
            })
            .collect();
        for kind in [StrokeKind::Pen, StrokeKind::Highlighter] {
            let mut live = LiveMesh::new(kind, [10, 20, 30, 255], 3.0, 1.7, 1.0);
            for m in 1..=pts.len() {
                let tip: Vec<Point> = (0..m % 3).map(|k| Point::new(400.0, 9.0 * k as f32, 0.6)).collect();
                let now = live.update(&pts[..m], &tip);
                let all: Vec<Point> = pts[..m].iter().chain(&tip).copied().collect();
                let mut whole = Mesh::default();
                tessellate(&mut whole, kind, [10, 20, 30, 255], 3.0, &all, 1.7, 1.0);
                assert!(now.is_valid() && live.fixed().mesh().is_valid());
                assert_eq!(triangles(&[live.fixed().mesh(), &now]), triangles(&[&whole]), "after {m} points");
            }
        }
    }
}
