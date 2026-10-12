//! Annotation object model, in image-pixel ("model") coordinates.
//!
//! Mirrors the split Preview uses: AnnotationKit keeps a live list of
//! annotation objects above the image, and the bitmap is only touched on
//! save, when everything is flattened (see PREVIEW_RESEARCH.md).

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn offset(self, dx: f64, dy: f64) -> Self {
        Self::new(self.x + dx, self.y + dy)
    }

    pub fn distance(self, other: Point) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }

    /// Rotate by `angle` radians (clockwise on screen, y points down).
    pub fn rotate_around(self, center: Point, angle: f64) -> Self {
        let (sin, cos) = angle.sin_cos();
        let (dx, dy) = (self.x - center.x, self.y - center.y);
        Self::new(center.x + dx * cos - dy * sin, center.y + dx * sin + dy * cos)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Normalized rectangle spanning two arbitrary corners.
    pub fn from_points(a: Point, b: Point) -> Self {
        Self::new(a.x.min(b.x), a.y.min(b.y), (a.x - b.x).abs(), (a.y - b.y).abs())
    }

    pub fn bounding(points: &[Point]) -> Option<Self> {
        let first = *points.first()?;
        let (mut min, mut max) = (first, first);
        for p in points {
            min = Point::new(min.x.min(p.x), min.y.min(p.y));
            max = Point::new(max.x.max(p.x), max.y.max(p.y));
        }
        Some(Self::from_points(min, max))
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }

    /// Grow (or shrink, with a negative `d`) on every side.
    pub fn inflate(&self, d: f64) -> Self {
        Self::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }

    pub fn intersect(&self, other: &Rect) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        (r > x && b > y).then(|| Self::new(x, y, r - x, b - y))
    }

    /// Top-left, top-right, bottom-right, bottom-left. Corner `i` is
    /// opposite corner `(i + 2) % 4`.
    pub fn corners(&self) -> [Point; 4] {
        [
            Point::new(self.x, self.y),
            Point::new(self.right(), self.y),
            Point::new(self.right(), self.bottom()),
            Point::new(self.x, self.bottom()),
        ]
    }

    pub fn clamp_point(&self, p: Point) -> Point {
        Point::new(p.x.clamp(self.x, self.right()), p.y.clamp(self.y, self.bottom()))
    }
}

pub fn distance_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    p.distance(Point::new(a.x + t * dx, a.y + t * dy))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const fn rgba(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }

    // Same defaults as the macOS system palette Preview draws with.
    pub const RED: Color = Color::rgba(1.0, 0.231, 0.188, 1.0);
    pub const YELLOW: Color = Color::rgba(1.0, 0.8, 0.0, 1.0);
    pub const BLACK: Color = Color::rgba(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Color = Color::rgba(1.0, 1.0, 1.0, 1.0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowHeads {
    None,
    End,
    Start,
    Both,
}

impl ArrowHeads {
    pub const ALL: [ArrowHeads; 4] = [Self::None, Self::End, Self::Start, Self::Both];

    pub fn at_start(self) -> bool {
        matches!(self, Self::Start | Self::Both)
    }

    pub fn at_end(self) -> bool {
        matches!(self, Self::End | Self::Both)
    }
}

/// The attributes AnnotationKit exposes on its markup toolbar: color, fill,
/// line width, dashed, shadow, arrow heads and font.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub stroke: Color,
    pub fill: Option<Color>,
    pub width: f64,
    pub dashed: bool,
    pub shadow: bool,
    pub heads: ArrowHeads,
    pub font_size: f64,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            stroke: Color::RED,
            fill: None,
            width: 4.0,
            dashed: false,
            shadow: false,
            heads: ArrowHeads::None,
            font_size: 28.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rectangle(Rect),
    RoundedRect(Rect),
    Oval(Rect),
    /// Speech bubble with its tail at the bottom-left of the rect.
    Bubble(Rect),
    /// Five-pointed star fitted to the rect.
    Star(Rect),
    /// Hexagon (pointy top) fitted to the rect.
    Polygon(Rect),
    /// Marker-style rectangle composited with a multiply blend.
    Highlight(Rect),
    /// Opaque box; becomes real redaction once flattened on save.
    Redact(Rect),
    /// A line, or an arrow when `Style::heads` is not `None`.
    Line {
        start: Point,
        end: Point,
    },
    /// Freehand stroke ("Doodle" / "Drawing" in AnnotationKit).
    Pen(Vec<Point>),
    /// Text box anchored at its top-left corner, rotated by `rotation`
    /// radians around its center.
    Text {
        origin: Point,
        text: String,
        rotation: f64,
    },
}

impl Shape {
    /// The defining rectangle of box-like shapes (everything dragged out
    /// corner to corner).
    pub fn rect(&self) -> Option<Rect> {
        match self {
            Shape::Rectangle(r)
            | Shape::RoundedRect(r)
            | Shape::Oval(r)
            | Shape::Bubble(r)
            | Shape::Star(r)
            | Shape::Polygon(r)
            | Shape::Highlight(r)
            | Shape::Redact(r) => Some(*r),
            _ => None,
        }
    }

    pub fn rect_mut(&mut self) -> Option<&mut Rect> {
        match self {
            Shape::Rectangle(r)
            | Shape::RoundedRect(r)
            | Shape::Oval(r)
            | Shape::Bubble(r)
            | Shape::Star(r)
            | Shape::Polygon(r)
            | Shape::Highlight(r)
            | Shape::Redact(r) => Some(r),
            _ => None,
        }
    }

    /// Scale about the image origin; used when the image is resampled.
    pub fn scale(&mut self, sx: f64, sy: f64) {
        let scale = |p: &mut Point| *p = Point::new(p.x * sx, p.y * sy);
        if let Some(r) = self.rect_mut() {
            *r = Rect::new(r.x * sx, r.y * sy, r.w * sx, r.h * sy);
            return;
        }
        match self {
            Shape::Line { start, end } => {
                scale(start);
                scale(end);
            }
            Shape::Pen(points) => points.iter_mut().for_each(scale),
            Shape::Text { origin, .. } => scale(origin),
            _ => unreachable!("box shapes handled above"),
        }
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        if let Some(r) = self.rect_mut() {
            r.x += dx;
            r.y += dy;
            return;
        }
        match self {
            Shape::Line { start, end } => {
                *start = start.offset(dx, dy);
                *end = end.offset(dx, dy);
            }
            Shape::Pen(points) => {
                for p in points {
                    *p = p.offset(dx, dy);
                }
            }
            Shape::Text { origin, .. } => *origin = origin.offset(dx, dy),
            _ => unreachable!("box shapes handled above"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: u64,
    pub shape: Shape,
    pub style: Style,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_from_points_normalizes() {
        let r = Rect::from_points(Point::new(10.0, 20.0), Point::new(2.0, 5.0));
        assert_eq!(r, Rect::new(2.0, 5.0, 8.0, 15.0));
    }

    #[test]
    fn intersect_disjoint_is_none() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(a.intersect(&Rect::new(20.0, 20.0, 5.0, 5.0)).is_none());
        assert_eq!(a.intersect(&Rect::new(5.0, 5.0, 10.0, 10.0)), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
    }

    #[test]
    fn segment_distance() {
        let (a, b) = (Point::new(0.0, 0.0), Point::new(10.0, 0.0));
        assert_eq!(distance_to_segment(Point::new(5.0, 3.0), a, b), 3.0);
        assert_eq!(distance_to_segment(Point::new(13.0, 4.0), a, b), 5.0);
    }
}
