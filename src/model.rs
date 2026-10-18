//! Serializable document model. Coordinates are in *document units*:
//! x in `0..page_width`, y in `0..∞` (top of the page downwards).

use eframe::egui::Pos2;
use serde::{Deserialize, Serialize};

use crate::geometry::point_segment_distance;

pub const DEFAULT_PAGE_WIDTH: f32 = 800.0;

/// A single input sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    /// Normalised pen pressure in `0.0..=1.0` (1.0 for a mouse).
    pub pressure: f32,
}

impl Point {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self { x, y, pressure }
    }

    pub fn pos2(&self) -> Pos2 {
        Pos2::new(self.x, self.y)
    }

    pub fn distance(&self, other: &Point) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeKind {
    Pen,
    Highlighter,
}

/// A finished stroke.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stroke {
    pub id: u64,
    pub kind: StrokeKind,
    /// Unmultiplied RGBA. Highlighters carry their alpha here.
    pub color: [u8; 4],
    /// Base width in document units.
    pub width: f32,
    pub points: Vec<Point>,
    /// Cached `[min_x, min_y, max_x, max_y]` for culling and eraser rejection.
    pub bounds: [f32; 4],
}

impl Stroke {
    pub fn new(id: u64, kind: StrokeKind, color: [u8; 4], width: f32, points: Vec<Point>) -> Self {
        let bounds = compute_bounds(&points);
        Self { id, kind, color, width, points, bounds }
    }

    /// Stroke-based eraser test: does a circle at `center` with `radius`
    /// touch any segment of this stroke?
    pub fn hit_test(&self, center: Pos2, radius: f32) -> bool {
        let reach = radius + self.width * 0.5;
        let [min_x, min_y, max_x, max_y] = self.bounds;
        if center.x < min_x - reach
            || center.x > max_x + reach
            || center.y < min_y - reach
            || center.y > max_y + reach
        {
            return false; // cheap bounding-box rejection
        }
        match self.points.as_slice() {
            [] => false,
            [p] => p.pos2().distance(center) <= reach,
            pts => pts
                .windows(2)
                .any(|w| point_segment_distance(center, w[0].pos2(), w[1].pos2()) <= reach),
        }
    }
}

fn compute_bounds(points: &[Point]) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in points {
        b[0] = b[0].min(p.x);
        b[1] = b[1].min(p.y);
        b[2] = b[2].max(p.x);
        b[3] = b[3].max(p.y);
    }
    if points.is_empty() {
        [0.0; 4]
    } else {
        b
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageKind {
    Blank,
    Ruled,
    Grid,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PageStyle {
    pub kind: PageKind,
    /// Distance between rules/grid lines in document units.
    pub spacing: f32,
}

impl Default for PageStyle {
    fn default() -> Self {
        Self { kind: PageKind::Ruled, spacing: 36.0 }
    }
}

/// The whole note.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub page_width: f32,
    pub page: PageStyle,
    /// Draw order: later strokes are painted on top.
    pub strokes: Vec<Stroke>,
    next_id: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            page_width: DEFAULT_PAGE_WIDTH,
            page: PageStyle::default(),
            strokes: Vec::new(),
            next_id: 1,
        }
    }
}

impl Document {
    /// Allocates a document-unique stroke id.
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}
