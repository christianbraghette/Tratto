//! Anti-aliased stroke tessellation.
//!
//! A stroke is turned into one triangle mesh:
//!
//! 1. The sparse (already filtered) control points are interpolated with
//!    quadratic Béziers through the segment midpoints (C1-smooth, no
//!    overshoot), sampled densely enough that curves look round at any zoom.
//! 2. A ribbon with 4 vertices per sample is built: two solid inner vertices
//!    and two fully transparent outer vertices, one `aa` (= 1 physical pixel)
//!    further out. The GPU interpolates the alpha across that fringe, which
//!    gives analytic anti-aliasing independent of MSAA.
//! 3. Round caps (half-discs with the same fringe) close both ends; a
//!    single-point stroke becomes a full disc.
//!
//! The pressure-dependent radius is interpolated along the same curve, so
//! width changes are smooth too.
//!
//! All output positions are `document_units * scale` (no translation), which
//! lets the cache reuse a mesh while scrolling.

use std::f32::consts::{PI, TAU};

use eframe::egui::{epaint::Mesh, pos2, vec2, Color32, Pos2, Vec2};

use crate::model::{Point, StrokeKind};

/// Target spacing of generated curve samples, in physical pixels.
const SAMPLE_STEP_PX: f32 = 2.5;

#[derive(Clone, Copy)]
struct Node {
    p: Pos2,
    r: f32,
}

/// Pressure -> width multiplier.
pub fn pressure_factor(p: f32) -> f32 {
    0.35 + 0.65 * p.clamp(0.0, 1.0)
}

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

    let nodes: Vec<Node> = pts
        .iter()
        .map(|p| {
            let w = match kind {
                StrokeKind::Pen => width * pressure_factor(p.pressure),
                StrokeKind::Highlighter => width,
            };
            Node {
                p: pos2(p.x * scale, p.y * scale),
                // Never thinner than one physical pixel.
                r: (w * scale * 0.5).max(half_aa),
            }
        })
        .collect();

    let dense = dedup(smooth(&nodes, SAMPLE_STEP_PX * aa), 0.05 * aa);
    let n = dense.len();

    if n == 1 {
        let r = dense[0].r;
        add_arc(mesh, dense[0].p, vec2(1.0, 0.0), vec2(0.0, 1.0), r - half_aa, r + half_aa, solid, TAU, arc_segments(r, aa) * 2);
        return;
    }

    // Unit direction of every segment.
    let dirs: Vec<Vec2> = dense.windows(2).map(|w| unit(w[1].p - w[0].p)).collect();

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
        let c = dense[i].p;
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
    let first = dense[0];
    add_arc(mesh, first.p, perp(dirs[0]), -dirs[0], first.r - half_aa, first.r + half_aa, solid, PI, arc_segments(first.r, aa));
    let last = dense[n - 1];
    add_arc(mesh, last.p, perp(dirs[n - 2]), dirs[n - 2], last.r - half_aa, last.r + half_aa, solid, PI, arc_segments(last.r, aa));
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
        if out.last().map_or(true, |l| (n.p - l.p).length() >= min_dist) {
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
        p: pos2(
            a.p.x * wa + c.p.x * wc + b.p.x * wb,
            a.p.y * wa + c.p.y * wc + b.p.y * wb,
        ),
        r: a.r * wa + c.r * wc + b.r * wb,
    }
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
