//! Small geometry helpers.

use eframe::egui::Pos2;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_to_segment() {
        let a = Pos2::new(0.0, 0.0);
        let b = Pos2::new(10.0, 0.0);
        assert!((point_segment_distance(Pos2::new(5.0, 3.0), a, b) - 3.0).abs() < 1e-5);
        assert!((point_segment_distance(Pos2::new(-4.0, 3.0), a, b) - 5.0).abs() < 1e-5);
        assert!((point_segment_distance(Pos2::new(1.0, 1.0), a, a) - 2.0_f32.sqrt()).abs() < 1e-5);
    }
}
