//! Shape recognition for the Sketch tool — Preview's "intelligent sketch"
//! (`_akIntelligentSketchButton`): draw a rough shape in one stroke and it
//! is replaced by a clean line, rectangle or oval. Anything that doesn't
//! match stays a freehand drawing.

use crate::model::{distance_to_segment, Point, Rect, Shape};

/// Mean deviation (relative to the shape's size) above which a stroke is
/// left as freehand.
const MAX_ERROR: f64 = 0.18;

pub fn recognize(points: &[Point]) -> Option<Shape> {
    if points.len() < 4 {
        return None;
    }
    let bbox = Rect::bounding(points)?;
    let diag = bbox.w.hypot(bbox.h);
    if diag < 12.0 {
        return None;
    }
    let (first, last) = (points[0], points[points.len() - 1]);
    let span = first.distance(last);
    let travelled: f64 = points.windows(2).map(|w| w[0].distance(w[1])).sum();

    // A line: the pen barely travelled further than the straight distance,
    // and no point strays far from the segment.
    if span > 0.0 && travelled / span < 1.2 {
        let max_dev = points.iter().map(|p| distance_to_segment(*p, first, last)).fold(0.0, f64::max);
        if max_dev < span * 0.08 {
            return Some(Shape::Line { start: first, end: last });
        }
    }

    // A closed shape: the ends meet after going around something.
    let closed = span < diag * 0.3 && travelled > diag * 1.8;
    if !closed || bbox.w < 8.0 || bbox.h < 8.0 {
        return None;
    }
    let n = points.len() as f64;
    let c = bbox.center();
    let (rx, ry) = (bbox.w / 2.0, bbox.h / 2.0);

    // Ellipse fit: how far each point is from the inscribed ellipse, in
    // radius units.
    let ellipse_error = points
        .iter()
        .map(|p| ((((p.x - c.x) / rx).powi(2) + ((p.y - c.y) / ry).powi(2)).sqrt() - 1.0).abs())
        .sum::<f64>()
        / n;

    // Rectangle fit: how far each point is from the nearest bbox edge, in
    // half-size units so both errors are comparable.
    let rect_error = points
        .iter()
        .map(|p| {
            (p.x - bbox.x)
                .abs()
                .min((bbox.right() - p.x).abs())
                .min((p.y - bbox.y).abs())
                .min((bbox.bottom() - p.y).abs())
        })
        .sum::<f64>()
        / n
        / rx.min(ry);

    if ellipse_error.min(rect_error) > MAX_ERROR {
        None
    } else if rect_error < ellipse_error {
        Some(Shape::Rectangle(bbox))
    } else {
        Some(Shape::Oval(bbox))
    }
}

/// Human-readable name for the recognized shape, shown in a toast.
pub fn name(shape: &Shape) -> &'static str {
    match shape {
        Shape::Line { .. } => "line",
        Shape::Rectangle(_) => "rectangle",
        Shape::Oval(_) => "oval",
        _ => "shape",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    /// Deterministic wobble so the strokes look hand-drawn.
    fn wobble(i: usize, amount: f64) -> f64 {
        ((i as f64 * 1.7).sin() + (i as f64 * 0.6).cos()) * amount / 2.0
    }

    #[test]
    fn rough_circle_becomes_oval() {
        let points: Vec<Point> = (0..=60)
            .map(|i| {
                let t = i as f64 / 60.0 * TAU;
                let r = 50.0 + wobble(i, 4.0);
                Point::new(100.0 + r * t.cos(), 100.0 + r * 0.7 * t.sin())
            })
            .collect();
        assert!(matches!(recognize(&points), Some(Shape::Oval(_))));
    }

    #[test]
    fn rough_box_becomes_rectangle() {
        let corners = [(10.0, 10.0), (150.0, 12.0), (148.0, 90.0), (12.0, 88.0), (11.0, 14.0)];
        let mut points = Vec::new();
        for pair in corners.windows(2) {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            for step in 0..15 {
                let t = step as f64 / 15.0;
                let i = points.len();
                points.push(Point::new(
                    x0 + (x1 - x0) * t + wobble(i, 2.0),
                    y0 + (y1 - y0) * t + wobble(i + 3, 2.0),
                ));
            }
        }
        assert!(matches!(recognize(&points), Some(Shape::Rectangle(_))));
    }

    #[test]
    fn shaky_straight_stroke_becomes_line() {
        let points: Vec<Point> = (0..30).map(|i| Point::new(i as f64 * 5.0, 40.0 + wobble(i, 3.0))).collect();
        let Some(Shape::Line { start, end }) = recognize(&points) else { panic!("not a line") };
        assert_eq!((start, end), (points[0], points[29]));
    }

    #[test]
    fn scribble_stays_freehand() {
        let points: Vec<Point> =
            (0..40).map(|i| Point::new(i as f64 * 4.0, if i % 2 == 0 { 0.0 } else { 40.0 })).collect();
        assert!(recognize(&points).is_none());
    }
}
