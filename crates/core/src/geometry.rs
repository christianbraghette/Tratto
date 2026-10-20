// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Small geometry helpers.

use emath::Pos2;

/// Shortest distance from point `p` to the segment `a`–`b`.
pub fn point_segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return p.distance(a); // degenerate segment
    }
    // Projection parameter of p on the infinite line, clamped to the segment.
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Even-odd point-in-polygon test (the polygon is implicitly closed).
pub fn point_in_polygon(p: Pos2, poly: &[Pos2]) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_containment() {
        let sq = [Pos2::new(0.0, 0.0), Pos2::new(10.0, 0.0), Pos2::new(10.0, 10.0), Pos2::new(0.0, 10.0)];
        assert!(point_in_polygon(Pos2::new(5.0, 5.0), &sq));
        assert!(!point_in_polygon(Pos2::new(15.0, 5.0), &sq));
    }

    #[test]
    fn distance_to_segment() {
        let a = Pos2::new(0.0, 0.0);
        let b = Pos2::new(10.0, 0.0);
        assert!((point_segment_distance(Pos2::new(5.0, 3.0), a, b) - 3.0).abs() < 1e-5);
        assert!((point_segment_distance(Pos2::new(-4.0, 3.0), a, b) - 5.0).abs() < 1e-5);
        assert!((point_segment_distance(Pos2::new(1.0, 1.0), a, a) - 2.0_f32.sqrt()).abs() < 1e-5);
    }
}
