//! Cairo renderer for the document. The same code paints the canvas and
//! produces the flattened bitmap on save, so what you see is what you export.

use gtk::{cairo, pango};

use crate::editor::Document;
use crate::model::{Annotation, Color, Point, Rect, Shape, Style};

/// Padding around a text box's background fill, as a fraction of font size.
const TEXT_PAD: f64 = 0.2;

fn set_color(cr: &cairo::Context, c: Color) {
    cr.set_source_rgba(c.r, c.g, c.b, c.a);
}

/// Paint the visible (cropped) part of the image plus all annotations.
/// The caller sets up the model→device transform.
pub fn draw_document(cr: &cairo::Context, doc: &Document) {
    let crop = doc.crop;
    cr.save().ok();
    cr.rectangle(crop.x, crop.y, crop.w, crop.h);
    cr.clip();

    let pattern = cairo::SurfacePattern::create(&doc.image);
    pattern.set_filter(cairo::Filter::Good);
    cr.set_source(&pattern).ok();
    cr.paint().ok();

    for a in &doc.annotations {
        draw_annotation(cr, a);
    }
    cr.restore().ok();
}

/// Render the document at full resolution, cropped, with every annotation
/// burnt in — Preview's `CGImageForSavingWithFlattenedEXIF` step.
pub fn flatten(doc: &Document) -> anyhow::Result<cairo::ImageSurface> {
    let crop = doc.crop;
    let surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, crop.w.round() as i32, crop.h.round() as i32)?;
    {
        let cr = cairo::Context::new(&surface)?;
        cr.translate(-crop.x, -crop.y);
        draw_document(&cr, doc);
    }
    surface.flush();
    Ok(surface)
}

pub fn draw_annotation(cr: &cairo::Context, a: &Annotation) {
    if a.style.shadow && !matches!(a.shape, Shape::Highlight(_)) {
        let offset = (a.style.width * 0.5).max(2.0);
        cr.save().ok();
        cr.translate(offset * 0.5, offset);
        draw_shape(cr, a, Some(Color::rgba(0.0, 0.0, 0.0, 0.3)));
        cr.restore().ok();
    }
    draw_shape(cr, a, None);
}

/// `tint` overrides every color; used to paint the drop shadow pass.
fn draw_shape(cr: &cairo::Context, a: &Annotation, tint: Option<Color>) {
    let style = &a.style;
    let stroke = tint.unwrap_or(style.stroke);
    let fill = style.fill.map(|f| tint.unwrap_or(f));

    cr.save().ok();
    cr.set_line_width(style.width);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.set_line_cap(cairo::LineCap::Round);
    if style.dashed {
        cr.set_dash(&[style.width * 3.0, style.width * 2.0], 0.0);
    }

    match &a.shape {
        Shape::Rectangle(_)
        | Shape::RoundedRect(_)
        | Shape::Oval(_)
        | Shape::Bubble(_)
        | Shape::Star(_)
        | Shape::Polygon(_) => {
            outline_path(cr, &a.shape, a.turns);
            fill_and_stroke(cr, fill, stroke);
        }
        Shape::Highlight(r) => {
            if tint.is_none() {
                cr.set_operator(cairo::Operator::Multiply);
                set_color(cr, stroke);
                cr.rectangle(r.x, r.y, r.w, r.h);
                cr.fill().ok();
            }
        }
        Shape::Redact(r) => {
            set_color(cr, Color { a: 1.0, ..stroke });
            cr.rectangle(r.x, r.y, r.w, r.h);
            cr.fill().ok();
        }
        Shape::Line { start, end } => draw_line(cr, *start, *end, style, stroke),
        Shape::Pen(points) => draw_pen(cr, points, style, stroke),
        Shape::Text { origin, text, rotation } => {
            let (b, _) = text_frame(*origin, text, style, *rotation);
            let c = b.center();
            cr.translate(c.x, c.y);
            cr.rotate(*rotation);
            cr.translate(-c.x, -c.y);

            let layout = text_layout(cr, text, style.font_size);
            if let Some(fill) = fill {
                set_color(cr, fill);
                cr.rectangle(b.x, b.y, b.w, b.h);
                cr.fill().ok();
            }
            set_color(cr, stroke);
            cr.move_to(origin.x, origin.y);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }
    cr.restore().ok();
}

fn fill_and_stroke(cr: &cairo::Context, fill: Option<Color>, stroke: Color) {
    if let Some(fill) = fill {
        set_color(cr, fill);
        cr.fill_preserve().ok();
    }
    set_color(cr, stroke);
    cr.stroke().ok();
}

/// Build the outline of a closed shape. Shared by drawing and hit testing.
///
/// After `turns` quarter turns of the image the shape is laid out in its
/// rect as it was before them, then turned about the center — what
/// AnnotationKit's `adjustModelToCompensateForOriginalExif` does — so a
/// bubble's tail follows the image instead of snapping back to the bottom.
fn outline_path(cr: &cairo::Context, shape: &Shape, turns: u8) {
    use std::f64::consts::{FRAC_PI_2, PI, TAU};
    let turns = turns % 4;
    if let (Some(r), true) = (shape.rect(), turns != 0) {
        let mut upright = shape.clone();
        if let Some(rect) = upright.rect_mut() {
            *rect = if turns % 2 == 1 { r.transposed() } else { r };
        }
        let c = r.center();
        cr.save().ok();
        cr.translate(c.x, c.y);
        cr.rotate(turns as f64 * FRAC_PI_2);
        cr.translate(-c.x, -c.y);
        outline_path(cr, &upright, 0);
        cr.restore().ok();
        return;
    }
    match shape {
        Shape::Rectangle(r) => cr.rectangle(r.x, r.y, r.w, r.h),
        Shape::Oval(r) => ellipse_path(cr, r),
        Shape::RoundedRect(r) => rounded_rect_path(cr, r, None),
        Shape::Bubble(r) => {
            // Body takes the top part; the tail reaches the bottom-left.
            let body = Rect::new(r.x, r.y, r.w, r.h * 0.78);
            rounded_rect_path(cr, &body, Some(Point::new(r.x + r.w * 0.12, r.bottom())));
        }
        Shape::Star(r) => {
            let unit: Vec<(f64, f64)> = (0..10)
                .map(|i| {
                    let angle = -FRAC_PI_2 + i as f64 * PI / 5.0;
                    let radius = if i % 2 == 0 { 1.0 } else { 0.4 };
                    (radius * angle.cos(), radius * angle.sin())
                })
                .collect();
            polygon_path(cr, &unit, r);
        }
        Shape::Polygon(r) => {
            let unit: Vec<(f64, f64)> = (0..6)
                .map(|i| {
                    let angle = -FRAC_PI_2 + i as f64 * TAU / 6.0;
                    (angle.cos(), angle.sin())
                })
                .collect();
            polygon_path(cr, &unit, r);
        }
        _ => {}
    }
}

fn corner_radius(r: &Rect) -> f64 {
    (r.w.min(r.h) * 0.2).min(r.w.min(r.h) / 2.0)
}

/// Rounded rectangle, optionally with a speech-bubble tail leaving the
/// bottom edge towards `tail`.
fn rounded_rect_path(cr: &cairo::Context, r: &Rect, tail: Option<Point>) {
    use std::f64::consts::{FRAC_PI_2, PI};
    if r.w <= 0.0 || r.h <= 0.0 {
        return;
    }
    let rad = corner_radius(r);
    let (l, t, rt, b) = (r.x, r.y, r.right(), r.bottom());
    cr.new_sub_path();
    cr.arc(rt - rad, t + rad, rad, -FRAC_PI_2, 0.0);
    cr.arc(rt - rad, b - rad, rad, 0.0, FRAC_PI_2);
    if let Some(tip) = tail {
        let left = (l + r.w * 0.22).max(l + rad);
        let right = (l + r.w * 0.40).min(rt - rad).max(left);
        cr.line_to(right, b);
        cr.line_to(tip.x, tip.y);
        cr.line_to(left, b);
    }
    cr.arc(l + rad, b - rad, rad, FRAC_PI_2, PI);
    cr.arc(l + rad, t + rad, rad, PI, PI + FRAC_PI_2);
    cr.close_path();
}

/// Polygon given in unit coordinates, stretched so its bounds fill `r`
/// (a star's points don't reach its circle's bottom otherwise).
fn polygon_path(cr: &cairo::Context, unit: &[(f64, f64)], r: &Rect) {
    let (min_x, max_x) = unit.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
    let (min_y, max_y) = unit.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
    for (i, (x, y)) in unit.iter().enumerate() {
        let px = r.x + (x - min_x) / (max_x - min_x) * r.w;
        let py = r.y + (y - min_y) / (max_y - min_y) * r.h;
        if i == 0 {
            cr.move_to(px, py);
        } else {
            cr.line_to(px, py);
        }
    }
    cr.close_path();
}

/// Whether `p` is on a closed shape's outline (within `band`) or, when
/// filled, inside it. Uses cairo's own geometry so it matches the drawing.
pub fn outline_contains(a: &Annotation, p: Point, band: f64) -> bool {
    let Ok(cr) =
        cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1).and_then(|s| cairo::Context::new(&s))
    else {
        return false;
    };
    let filled = a.style.fill.is_some();
    outline_path(&cr, &a.shape, a.turns);
    cr.set_line_width(band * 2.0);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.in_stroke(p.x, p.y).unwrap_or(false) || (filled && cr.in_fill(p.x, p.y).unwrap_or(false))
}

fn ellipse_path(cr: &cairo::Context, r: &Rect) {
    if r.w <= 0.0 || r.h <= 0.0 {
        return;
    }
    let c = r.center();
    cr.save().ok();
    cr.translate(c.x, c.y);
    cr.scale(r.w / 2.0, r.h / 2.0);
    cr.new_sub_path();
    cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
    cr.restore().ok();
}

/// Arrow head length for a given line width.
pub fn head_length(width: f64) -> f64 {
    width * 4.0 + 8.0
}

fn draw_line(cr: &cairo::Context, start: Point, end: Point, style: &Style, color: Color) {
    let len = start.distance(end);
    if len < 0.5 {
        return;
    }
    let (ux, uy) = ((end.x - start.x) / len, (end.y - start.y) / len);
    let head = head_length(style.width).min(len * 0.9);

    // Stop the shaft inside the head so its round cap doesn't poke out of the tip.
    let inset = head * 0.7;
    let mut a = start;
    let mut b = end;
    if style.heads.at_start() {
        a = start.offset(ux * inset, uy * inset);
    }
    if style.heads.at_end() {
        b = end.offset(-ux * inset, -uy * inset);
    }
    set_color(cr, color);
    cr.move_to(a.x, a.y);
    cr.line_to(b.x, b.y);
    cr.stroke().ok();

    cr.set_dash(&[], 0.0);
    if style.heads.at_end() {
        arrow_head(cr, end, ux, uy, head);
    }
    if style.heads.at_start() {
        arrow_head(cr, start, -ux, -uy, head);
    }
}

/// Filled triangle with its tip at `tip`, pointing along (ux, uy).
fn arrow_head(cr: &cairo::Context, tip: Point, ux: f64, uy: f64, len: f64) {
    let half = len * 0.55;
    let base = tip.offset(-ux * len, -uy * len);
    let (nx, ny) = (-uy, ux);
    cr.move_to(tip.x, tip.y);
    cr.line_to(base.x + nx * half, base.y + ny * half);
    cr.line_to(base.x - nx * half, base.y - ny * half);
    cr.close_path();
    cr.fill().ok();
}

/// Freehand stroke smoothed with quadratic curves through segment midpoints.
fn draw_pen(cr: &cairo::Context, points: &[Point], style: &Style, color: Color) {
    set_color(cr, color);
    match points {
        [] => {}
        [p] => {
            cr.arc(p.x, p.y, style.width / 2.0, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }
        [first, rest @ ..] => {
            cr.move_to(first.x, first.y);
            let mut current = *first;
            for pair in rest.windows(2) {
                let (ctrl, next) = (pair[0], pair[1]);
                let mid = Point::new((ctrl.x + next.x) / 2.0, (ctrl.y + next.y) / 2.0);
                // Quadratic → cubic control points.
                cr.curve_to(
                    current.x + 2.0 / 3.0 * (ctrl.x - current.x),
                    current.y + 2.0 / 3.0 * (ctrl.y - current.y),
                    mid.x + 2.0 / 3.0 * (ctrl.x - mid.x),
                    mid.y + 2.0 / 3.0 * (ctrl.y - mid.y),
                    mid.x,
                    mid.y,
                );
                current = mid;
            }
            let last = rest[rest.len() - 1];
            cr.line_to(last.x, last.y);
            cr.stroke().ok();
        }
    }
}

fn text_layout(cr: &cairo::Context, text: &str, size: f64) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = pango::FontDescription::from_string("Sans");
    font.set_absolute_size(size * pango::SCALE as f64);
    layout.set_font_description(Some(&font));
    layout.set_text(text);
    layout
}

/// Size of laid-out text, measured on a scratch surface so it can be called
/// from anywhere (hit testing, handles) without a widget around.
fn text_bounds(origin: Point, text: &str, size: f64, padded: bool) -> Rect {
    let (w, h) = cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1)
        .and_then(|s| cairo::Context::new(&s))
        .map(|cr| text_layout(&cr, text, size).pixel_size())
        .unwrap_or((0, 0));
    let rect = Rect::new(origin.x, origin.y, (w as f64).max(size * 0.5), (h as f64).max(size));
    if padded {
        rect.inflate(size * TEXT_PAD)
    } else {
        rect
    }
}

/// A text box's unrotated frame (padded when it has a fill) and its
/// rotation; the frame's center is the pivot.
pub fn text_frame(origin: Point, text: &str, style: &Style, rotation: f64) -> (Rect, f64) {
    (text_bounds(origin, text, style.font_size, style.fill.is_some()), rotation)
}

/// Corners of a text box after rotation, clockwise from top-left.
pub fn text_corners(a: &Annotation) -> Option<[Point; 4]> {
    let Shape::Text { origin, text, rotation } = &a.shape else { return None };
    let (frame, rotation) = text_frame(*origin, text, &a.style, *rotation);
    let c = frame.center();
    Some(frame.corners().map(|p| p.rotate_around(c, rotation)))
}

/// Model-space bounding box, including stroke width and arrow heads.
pub fn annotation_bounds(a: &Annotation) -> Rect {
    let half = a.style.width / 2.0;
    if let Some(r) = a.shape.rect() {
        return r;
    }
    match &a.shape {
        Shape::Line { start, end } => {
            let extra = if a.style.heads == crate::model::ArrowHeads::None {
                half
            } else {
                head_length(a.style.width) * 0.55
            };
            Rect::from_points(*start, *end).inflate(extra)
        }
        Shape::Pen(points) => Rect::bounding(points).unwrap_or_default().inflate(half),
        Shape::Text { .. } => Rect::bounding(&text_corners(a).unwrap_or_default()).unwrap_or_default(),
        _ => unreachable!("box shapes handled above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArrowHeads, Color};

    fn sample_document() -> Document {
        let image = cairo::ImageSurface::create(cairo::Format::ARgb32, 640, 400).unwrap();
        {
            let cr = cairo::Context::new(&image).unwrap();
            cr.set_source_rgb(0.93, 0.95, 0.98);
            cr.paint().unwrap();
            cr.set_source_rgb(0.2, 0.2, 0.25);
            cr.move_to(40.0, 380.0);
            cr.set_font_size(18.0);
            cr.show_text("Some screenshot text to highlight").unwrap();
        }
        let mut doc = Document::new(image, None);
        let style = Style { width: 4.0, font_size: 28.0, ..Style::default() };
        let shapes = [
            (
                Shape::Line { start: Point::new(40.0, 40.0), end: Point::new(220.0, 140.0) },
                Style { heads: ArrowHeads::End, shadow: true, ..style.clone() },
            ),
            (
                Shape::Line { start: Point::new(40.0, 180.0), end: Point::new(220.0, 180.0) },
                Style { heads: ArrowHeads::Both, dashed: true, ..style.clone() },
            ),
            (Shape::Rectangle(Rect::new(260.0, 30.0, 150.0, 90.0)), style.clone()),
            (
                Shape::Oval(Rect::new(440.0, 30.0, 160.0, 90.0)),
                Style { fill: Some(Color::rgba(0.2, 0.6, 1.0, 0.4)), ..style.clone() },
            ),
            (
                Shape::Pen(vec![
                    Point::new(260.0, 200.0),
                    Point::new(300.0, 160.0),
                    Point::new(340.0, 220.0),
                    Point::new(380.0, 160.0),
                    Point::new(420.0, 210.0),
                ]),
                style.clone(),
            ),
            (
                Shape::Highlight(Rect::new(36.0, 362.0, 300.0, 24.0)),
                Style { stroke: Color::YELLOW, ..style.clone() },
            ),
            (
                Shape::Redact(Rect::new(460.0, 160.0, 140.0, 40.0)),
                Style { stroke: Color::BLACK, ..style.clone() },
            ),
            (Shape::RoundedRect(Rect::new(240.0, 250.0, 90.0, 60.0)), style.clone()),
            (
                Shape::Bubble(Rect::new(345.0, 235.0, 115.0, 85.0)),
                Style { fill: Some(Color::WHITE), ..style.clone() },
            ),
            (
                Shape::Star(Rect::new(475.0, 225.0, 70.0, 70.0)),
                Style { fill: Some(Color::YELLOW), ..style.clone() },
            ),
            (Shape::Polygon(Rect::new(552.0, 230.0, 60.0, 64.0)), style.clone()),
            (
                Shape::Text { origin: Point::new(40.0, 240.0), text: "Click here!".into(), rotation: -0.2 },
                Style { fill: Some(Color::WHITE), ..style },
            ),
        ];
        for (id, (shape, style)) in shapes.into_iter().enumerate() {
            doc.annotations.push(Annotation { id: id as u64, shape, style, turns: 0 });
        }
        doc
    }

    #[test]
    fn flatten_respects_crop_and_burns_in_annotations() {
        let mut doc = sample_document();
        doc.crop = Rect::new(20.0, 20.0, 600.0, 370.0);
        let surface = flatten(&doc).unwrap();
        assert_eq!((surface.width(), surface.height()), (600, 370));

        // Optional visual check: SCRAWL_RENDER_OUT=/tmp/out.png cargo test
        if let Ok(path) = std::env::var("SCRAWL_RENDER_OUT") {
            crate::imageio::save(surface, std::path::Path::new(&path), Some(144.0)).unwrap();
        }
    }
}
