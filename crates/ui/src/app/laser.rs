// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! The laser pointer of the pointer tool, to present from a note: a red dot
//! in place of the system pointer and, while pressed, a trail that fades out
//! behind it. It writes nothing and is never saved.
//!
//! With the laser on, dragging on the paper draws the trail instead of
//! moving around; the wheel, the keys, a finger and a drag off the paper
//! still do.

use eframe::egui::{self, Color32, Mesh, Painter, Pos2, Response, Shape, Vec2};

use crate::toolbar::Tool;
use crate::view::View;

use super::InkApp;

/// Seconds a point of the trail takes to fade out.
const FADE: f64 = 0.6;
const RED: [u8; 3] = [235, 35, 35];
/// Radius of the dot and of its glow, width of the trail and of the glow
/// around it (points).
const DOT: f32 = 5.0;
const GLOW: f32 = 13.0;
const TRAIL: f32 = 4.5;
const GLOW_WIDTH: f32 = 9.0;
/// Curve points between two points of the trail, at least `SPACING`
/// (points) apart.
const SUBDIVIDE: usize = 4;
const SPACING: f32 = 4.0;
/// Smoothing passes over the trail's points (see `relaxed`).
const RELAX: usize = 2;

/// A point of the trail, in content space so it stays on the page when the
/// view moves.
pub(super) struct LaserPoint {
    pos: Pos2,
    time: f64,
    /// The first point of a press: not joined to the one before.
    start: bool,
}

fn red(alpha: f32) -> Color32 {
    let [r, g, b] = RED;
    Color32::from_rgba_unmultiplied(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

impl InkApp {
    /// The laser is on and points: the pointer tool on a note, not a finger
    /// moving around.
    pub(super) fn lasering(&self) -> bool {
        self.laser && self.tool == Tool::Pointer && !self.reading() && !self.finger_nav
    }

    /// Adds where the pointer went this frame (`moves`) to the trail while
    /// it is pressed on the canvas, and drops what has faded.
    pub(super) fn laser_input(&mut self, ctx: &egui::Context, resp: &Response, view: &View, moves: &[Pos2], now: f64) {
        self.laser_trail.retain(|p| now - p.time < FADE);
        let pressed = self.lasering() && !self.panning && resp.is_pointer_button_down_on();
        if pressed {
            let mut start = !self.laser_down;
            let at = if moves.is_empty() { resp.interact_pointer_pos().into_iter().collect() } else { moves.to_vec() };
            for p in at {
                let pos = view.screen_to_content(p);
                if start || self.laser_trail.last().is_none_or(|l| l.pos != pos) {
                    self.laser_trail.push(LaserPoint { pos, time: now, start });
                    start = false;
                }
            }
        }
        self.laser_down = pressed;
        if !self.laser_trail.is_empty() {
            ctx.request_repaint();
        }
    }

    /// The trail, fading and thinning with age, and the dot at `pointer`.
    pub(super) fn paint_laser(&self, painter: &Painter, view: &View, pointer: Option<Pos2>, now: f64) {
        // Each press its own ribbon: a wide soft glow under a bright core
        // with a hot centre, both smoothed through the points.
        let mut glow = Mesh::default();
        let mut core = Mesh::default();
        let mut centre = Mesh::default();
        let mut from = 0;
        for i in 1..=self.laser_trail.len() {
            if i == self.laser_trail.len() || self.laser_trail[i].start {
                let press: Vec<(Pos2, f32)> = self.laser_trail[from..i]
                    .iter()
                    .map(|p| (view.content_to_screen(p.pos), (1.0 - (now - p.time) / FADE) as f32))
                    .collect();
                let path = smooth(&relaxed(spaced(&press)));
                // Overlaps (a sharp turn) of a see-through strip show as
                // darker spots: the core stays opaque until late in its fade.
                let solid = |t: f32| (t / 0.4).min(1.0);
                ribbon(&mut glow, &path, 0.0, GLOW_WIDTH, |t| red(0.16 * solid(t)));
                ribbon(&mut core, &path, TRAIL / 2.0, 1.6, |t| red(solid(t)));
                ribbon(&mut centre, &path, TRAIL / 5.0, 1.4, |t| hot(0.85 * solid(t)));
                from = i;
            }
        }
        for mesh in [glow, core, centre] {
            if !mesh.is_empty() {
                painter.add(Shape::mesh(mesh));
            }
        }
        if let Some(p) = pointer {
            painter.circle_filled(p, GLOW, red(0.12));
            painter.circle_filled(p, GLOW * 0.65, red(0.25));
            painter.circle_filled(p, DOT, red(1.0));
            painter.circle_filled(p, DOT * 0.45, hot(0.9));
        }
    }
}

/// The centre of the beam: nearly white, a touch of red.
fn hot(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(255, 215, 210, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

/// `points` with those closer than `SPACING` to the one before dropped
/// (the last one kept): the pointer reports points a pixel apart, whose
/// jitter would fold the strip over itself.
fn spaced(points: &[(Pos2, f32)]) -> Vec<(Pos2, f32)> {
    let mut out: Vec<(Pos2, f32)> = Vec::with_capacity(points.len());
    for (i, &p) in points.iter().enumerate() {
        let several = out.len() > 1;
        match out.last_mut() {
            Some(last) if last.0.distance(p.0) < SPACING => {
                // The end of the trail is where the pointer is: keep it.
                if i == points.len() - 1 && several {
                    *last = p;
                }
            }
            _ => out.push(p),
        }
    }
    out
}

/// `points` evened out: each but the ends moved towards its neighbours, a
/// few times. The pointer moves by whole pixels; this irons out the zigzag
/// that would otherwise ripple along the trail's edges.
fn relaxed(mut points: Vec<(Pos2, f32)>) -> Vec<(Pos2, f32)> {
    for _ in 0..RELAX {
        for i in 1..points.len().saturating_sub(1) {
            let (a, b) = (points[i - 1].0, points[i + 1].0);
            let p = points[i].0;
            points[i].0 = (a.to_vec2() * 0.25 + p.to_vec2() * 0.5 + b.to_vec2() * 0.25).to_pos2();
        }
    }
    points
}

/// `points` (screen position, freshness 1 → 0) through a Catmull-Rom curve,
/// so a quick drag draws a curve and not a polygon.
fn smooth(points: &[(Pos2, f32)]) -> Vec<(Pos2, f32)> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let at = |i: isize| points[i.clamp(0, points.len() as isize - 1) as usize];
    let mut out = Vec::with_capacity(points.len() * SUBDIVIDE);
    for i in 0..points.len() as isize - 1 {
        let (p0, p1, p2, p3) = (at(i - 1).0, at(i).0, at(i + 1).0, at(i + 2).0);
        let (t1, t2) = (at(i).1, at(i + 1).1);
        for k in 0..SUBDIVIDE {
            let s = k as f32 / SUBDIVIDE as f32;
            let (s2, s3) = (s * s, s * s * s);
            let p = 0.5
                * (2.0 * p1.to_vec2()
                    + (p2 - p0) * s
                    + (2.0 * p0.to_vec2() - 5.0 * p1.to_vec2() + 4.0 * p2.to_vec2() - p3.to_vec2()) * s2
                    + (3.0 * p1.to_vec2() - p0.to_vec2() - 3.0 * p2.to_vec2() + p3.to_vec2()) * s3);
            out.push((p.to_pos2(), t1 + (t2 - t1) * s));
        }
    }
    out.push(*points.last().unwrap());
    out
}

/// Adds to `mesh` a strip along `path`: `half` wide each side of it in
/// `color(t)`, then `feather` more fading to nothing (soft edges). Older
/// parts (smaller t) are thinner, so the trail tapers to its tail. Both
/// ends are rounded.
fn ribbon(mesh: &mut Mesh, path: &[(Pos2, f32)], half: f32, feather: f32, color: impl Fn(f32) -> Color32) {
    if path.len() < 2 {
        return;
    }
    let width = |t: f32| {
        let taper = 0.35 + 0.65 * t.clamp(0.0, 1.0);
        (half * taper, half * taper + feather * taper.max(0.6))
    };
    let mut normal = Vec2::ZERO;
    for (i, &(p, t)) in path.iter().enumerate() {
        let (a, b) = (path[i.saturating_sub(1)].0, path[(i + 1).min(path.len() - 1)].0);
        let d = b - a;
        if d.length_sq() > 1e-6 {
            normal = d.normalized().rot90();
        }
        let (inner, outer) = width(t);
        let (c, clear) = (color(t.clamp(0.0, 1.0)), Color32::TRANSPARENT);
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(p - normal * outer, clear);
        mesh.colored_vertex(p - normal * inner, c);
        mesh.colored_vertex(p + normal * inner, c);
        mesh.colored_vertex(p + normal * outer, clear);
        if i > 0 {
            let prev = base - 4;
            for col in 0..3 {
                let (a, b, c, d) = (prev + col, prev + col + 1, base + col, base + col + 1);
                mesh.add_triangle(a, b, c);
                mesh.add_triangle(b, d, c);
            }
        }
    } // The caps: a half disc beyond each end, away from the trail.
    let last = path.len() - 1;
    for (end, before) in [(0, 1), (last, last - 1)] {
        let (p, t) = path[end];
        let out = (p - path[before].0).normalized();
        let (inner, outer) = width(t);
        cap(mesh, p, out, inner, outer, color(t.clamp(0.0, 1.0)));
    }
}

/// Steps of a cap's half circle.
const CAP_STEPS: usize = 10;

/// A half disc at `p` bulging towards `out`: `inner` in `c`, then fading to
/// nothing at `outer`.
fn cap(mesh: &mut Mesh, p: Pos2, out: Vec2, inner: f32, outer: f32, c: Color32) {
    let side = out.rot90();
    let centre = mesh.vertices.len() as u32;
    mesh.colored_vertex(p, c);
    for k in 0..=CAP_STEPS {
        let a = std::f32::consts::PI * k as f32 / CAP_STEPS as f32;
        let dir = side * a.cos() + out * a.sin();
        mesh.colored_vertex(p + dir * inner, c);
        mesh.colored_vertex(p + dir * outer, Color32::TRANSPARENT);
        if k > 0 {
            // Inner and outer vertices of the step before and of this one.
            let k = 2 * k as u32;
            let (i0, o0, i1, o1) = (centre + k - 1, centre + k, centre + k + 1, centre + k + 2);
            mesh.add_triangle(centre, i0, i1);
            mesh.add_triangle(i0, o0, i1);
            mesh.add_triangle(o0, o1, i1);
        }
    }
}
