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

use std::f32::consts::{PI, TAU};

use eframe::egui::{epaint::Mesh, pos2, vec2, Color32, Pos2, Vec2};
use tratto_core::model::{Point, StrokeKind};
use tratto_core::stroke_path;

/// Target spacing of generated curve samples, in physical pixels.
const SAMPLE_STEP_PX: f32 = 2.5;

/// Appends the tessellated stroke to `mesh`.
///
/// * `scale` – document units -> points
/// * `aa`    – feather width in points (`1.0 / pixels_per_point`)
pub fn tessellate(
    mesh: &mut Mesh,
    kind: StrokeKind,
    rgba: [u8; 4],
    width: f32,
    pts: &[Point],
    scale: f32,
    aa: f32,
) {
    if pts.is_empty() {
        return;
    }
    let solid = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
    let half_aa = aa * 0.5;

    // Never thinner than one physical pixel.
    let dense = stroke_path::smoothed(kind, width, pts, scale, half_aa, SAMPLE_STEP_PX * aa, 0.05 * aa);
    let n = dense.len();
    let at = |i: usize| pos2(dense[i].x, dense[i].y);

    if n == 1 {
        let r = dense[0].r;
        add_arc(mesh, at(0), vec2(1.0, 0.0), vec2(0.0, 1.0), r - half_aa, r + half_aa, solid, TAU, arc_segments(r, aa) * 2);
        return;
    }

    // Unit direction of every segment.
    let dirs: Vec<Vec2> = (0..n - 1).map(|i| unit(at(i + 1) - at(i))).collect();

    // ---- ribbon
    let base = mesh.vertices.len() as u32;
    mesh.reserve_vertices(n * 4);
    mesh.reserve_triangles((n - 1) * 6);
    for i in 0..n {
        let d_prev = dirs[i.saturating_sub(1)];
        let d_next = dirs[i.min(n - 2)];
        let n0 = perp(d_prev);
        let n1 = perp(d_next);
        let mut nrm = unit(n0 + n1);
        if nrm == Vec2::ZERO {
            nrm = n0; // 180° reversal
        }
        // Miter scale keeps the width constant around bends (limited to 2x).
        let s = 1.0 / nrm.dot(n0).clamp(0.5, 1.0);
        let r = dense[i].r;
        let r_in = (r - half_aa).max(0.0) * s;
        let r_out = (r + half_aa) * s;
        let c = at(i);
        mesh.colored_vertex(c + nrm * r_out, Color32::TRANSPARENT);
        mesh.colored_vertex(c + nrm * r_in, solid);
        mesh.colored_vertex(c - nrm * r_in, solid);
        mesh.colored_vertex(c - nrm * r_out, Color32::TRANSPARENT);
    }
    for i in 0..(n - 1) as u32 {
        for k in 0..3u32 {
            let a = base + 4 * i + k;
            let b = a + 1;
            let c = base + 4 * (i + 1) + k;
            let d = c + 1;
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(b, d, c);
        }
    }

    // ---- round caps
    let (r0, r1) = (dense[0].r, dense[n - 1].r);
    add_arc(mesh, at(0), perp(dirs[0]), -dirs[0], r0 - half_aa, r0 + half_aa, solid, PI, arc_segments(r0, aa));
    add_arc(mesh, at(n - 1), perp(dirs[n - 2]), dirs[n - 2], r1 - half_aa, r1 + half_aa, solid, PI, arc_segments(r1, aa));
}

fn unit(v: Vec2) -> Vec2 {
    let l = v.length();
    if l > 1e-6 {
        v / l
    } else {
        Vec2::ZERO
    }
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
        let pts: Vec<Point> = (0..20)
            .map(|i| Point::new(i as f32 * 5.0, (i as f32 * 0.7).sin() * 10.0, 0.3 + 0.03 * i as f32))
            .collect();
        for kind in [StrokeKind::Pen, StrokeKind::Highlighter] {
            let mut m = Mesh::default();
            tessellate(&mut m, kind, [0, 0, 0, 255], 3.0, &pts, 1.0, 1.0);
            assert!(!m.is_empty() && m.is_valid());
        }
        let mut dot = Mesh::default();
        tessellate(&mut dot, StrokeKind::Pen, [0, 0, 0, 255], 3.0, &pts[..1], 1.0, 1.0);
        assert!(!dot.is_empty() && dot.is_valid());
    }
}
