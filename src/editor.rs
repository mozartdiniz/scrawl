//! Markup controller: tools, selection, direct manipulation and undo.
//!
//! Plays the role of AnnotationKit's `AKController`. It knows nothing about
//! widgets: the window converts pointer positions into model coordinates
//! (Preview's host does the same via `convert pointInModel`) and asks the
//! controller what to do with them.

use std::collections::HashMap;

use gtk::cairo;

use crate::model::{Annotation, ArrowHeads, Color, Point, Rect, Shape, Style};
use crate::{render, sketch};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Select,
    /// Freehand stroke that snaps to a line, rectangle or oval when it
    /// looks like one.
    Sketch,
    /// Plain freehand ("Draw" in Preview).
    Pen,
    Line,
    Arrow,
    Rectangle,
    RoundedRect,
    Oval,
    Bubble,
    Star,
    Polygon,
    Highlight,
    Text,
    Redact,
    Crop,
}

impl Tool {
    /// The tool that creates a given annotation; its remembered style is
    /// updated when the user restyles that annotation.
    pub fn for_annotation(a: &Annotation) -> Tool {
        match a.shape {
            Shape::Rectangle(_) => Tool::Rectangle,
            Shape::RoundedRect(_) => Tool::RoundedRect,
            Shape::Oval(_) => Tool::Oval,
            Shape::Bubble(_) => Tool::Bubble,
            Shape::Star(_) => Tool::Star,
            Shape::Polygon(_) => Tool::Polygon,
            Shape::Highlight(_) => Tool::Highlight,
            Shape::Redact(_) => Tool::Redact,
            Shape::Line { .. } if a.style.heads == ArrowHeads::None => Tool::Line,
            Shape::Line { .. } => Tool::Arrow,
            Shape::Pen(_) => Tool::Pen,
            Shape::Text { .. } => Tool::Text,
        }
    }
}

pub struct Document {
    pub image: cairo::ImageSurface,
    pub annotations: Vec<Annotation>,
    /// Visible region of the image. Like Preview, cropping is an edit that
    /// is only applied to pixels when flattening.
    pub crop: Rect,
    /// Resolution from the file's metadata, written back on save.
    pub dpi: Option<f64>,
}

impl Document {
    pub fn new(image: cairo::ImageSurface, dpi: Option<f64>) -> Self {
        let crop = Rect::new(0.0, 0.0, image.width() as f64, image.height() as f64);
        Self { image, annotations: Vec::new(), crop, dpi }
    }
}

/// Everything an undo step needs to restore. The image is only replaced by
/// Adjust Size; surfaces are reference-counted, so keeping one per step is
/// cheap unless it actually changed.
#[derive(Clone)]
struct Snapshot {
    annotations: Vec<Annotation>,
    crop: Rect,
    dpi: Option<f64>,
    image: cairo::ImageSurface,
}

impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        self.annotations == other.annotations
            && self.crop == other.crop
            && self.dpi == other.dpi
            && self.image.to_raw_none() == other.image.to_raw_none()
    }
}

#[derive(Clone)]
enum Drag {
    None,
    Create { id: u64, start: Point },
    Move { start: Point, original: Annotation },
    Handle { handle: usize, original: Annotation },
    Crop { start: Point },
}

pub enum Press {
    Nothing,
    EditText(u64),
}

pub struct Editor {
    pub doc: Document,
    pub selected: Option<u64>,
    pub crop_selection: Option<Rect>,
    pub dirty: bool,
    tool: Tool,
    styles: HashMap<Tool, Style>,
    drag: Drag,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// State before the current drag gesture or text edit started.
    pending: Option<Snapshot>,
    text_before: Option<Snapshot>,
    /// Last sketch that was recognized: (annotation, original stroke,
    /// recognized shape), so "Keep Drawing" can undo the recognition.
    last_sketch: Option<(u64, Shape, Shape)>,
    next_id: u64,
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        // Scale default sizes to the image so a 4K screenshot doesn't get
        // hairline arrows and a thumbnail doesn't get giant text.
        let short_side = doc.crop.w.min(doc.crop.h);
        let width = (short_side / 250.0).round().clamp(2.0, 24.0);
        let font_size = (short_side / 30.0).round().clamp(14.0, 200.0);
        let base = Style { width, font_size, ..Style::default() };

        let mut styles = HashMap::new();
        for tool in [
            Tool::Select,
            Tool::Sketch,
            Tool::Pen,
            Tool::Line,
            Tool::Rectangle,
            Tool::RoundedRect,
            Tool::Oval,
            Tool::Bubble,
            Tool::Star,
            Tool::Polygon,
            Tool::Text,
            Tool::Crop,
        ] {
            styles.insert(tool, base.clone());
        }
        styles.insert(Tool::Arrow, Style { heads: ArrowHeads::End, ..base.clone() });
        styles.insert(Tool::Highlight, Style { stroke: Color::YELLOW, ..base.clone() });
        styles.insert(Tool::Redact, Style { stroke: Color::BLACK, ..base });

        Self {
            doc,
            selected: None,
            crop_selection: None,
            dirty: false,
            tool: Tool::Arrow,
            styles,
            drag: Drag::None,
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            text_before: None,
            last_sketch: None,
            next_id: 1,
        }
    }

    // ---- tools & style -------------------------------------------------

    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: Tool) {
        if tool != Tool::Select {
            self.selected = None;
        }
        if tool != Tool::Crop {
            self.crop_selection = None;
        }
        self.tool = tool;
    }

    /// Style shown on the toolbar: the selection's, else the active tool's.
    pub fn displayed_style(&self) -> Style {
        match self.selected_annotation() {
            Some(a) => a.style.clone(),
            None => self.styles[&self.tool].clone(),
        }
    }

    /// Restyle the selection (undoable) and remember the change as the
    /// default for its tool, the way Preview's toolbar behaves.
    pub fn update_style(&mut self, change: impl Fn(&mut Style)) {
        let Some(id) = self.selected else {
            change(self.styles.get_mut(&self.tool).expect("every tool has a style"));
            return;
        };
        let before = self.snapshot();
        let Some(a) = self.annotation_mut(id) else { return };
        change(&mut a.style);
        let tool = Tool::for_annotation(a);
        change(self.styles.get_mut(&tool).expect("every tool has a style"));
        self.commit(before);
    }

    // ---- lookup ---------------------------------------------------------

    pub fn selected_annotation(&self) -> Option<&Annotation> {
        self.selected.and_then(|id| self.doc.annotations.iter().find(|a| a.id == id))
    }

    fn annotation_mut(&mut self, id: u64) -> Option<&mut Annotation> {
        self.doc.annotations.iter_mut().find(|a| a.id == id)
    }

    fn replace(&mut self, annotation: Annotation) {
        if let Some(a) = self.annotation_mut(annotation.id) {
            *a = annotation;
        }
    }

    /// Topmost annotation under `p`; `tol` is the hit slop in model units.
    pub fn annotation_at(&self, p: Point, tol: f64) -> Option<u64> {
        self.doc.annotations.iter().rev().find(|a| hit_test(a, p, tol)).map(|a| a.id)
    }

    pub fn text_at(&self, p: Point, tol: f64) -> Option<u64> {
        self.doc
            .annotations
            .iter()
            .rev()
            .find(|a| matches!(a.shape, Shape::Text { .. }) && hit_test(a, p, tol))
            .map(|a| a.id)
    }

    // ---- pointer interaction -------------------------------------------

    pub fn press(&mut self, p: Point, tol: f64) -> Press {
        self.drag = Drag::None;

        if self.tool == Tool::Crop {
            self.crop_selection = None;
            self.drag = Drag::Crop { start: self.doc.crop.clamp_point(p) };
            return Press::Nothing;
        }

        // The selection's handles and body win in every tool, so a freshly
        // drawn arrow can be adjusted without switching tools.
        if let Some(selected) = self.selected_annotation().cloned() {
            if let Some(handle) = handle_at(&selected, p, tol) {
                self.begin_change();
                self.drag = Drag::Handle { handle, original: selected };
                return Press::Nothing;
            }
            if hit_test(&selected, p, tol) {
                if self.tool == Tool::Text && matches!(selected.shape, Shape::Text { .. }) {
                    self.start_text_edit(selected.id);
                    return Press::EditText(selected.id);
                }
                self.begin_change();
                self.drag = Drag::Move { start: p, original: selected };
                return Press::Nothing;
            }
        }

        if self.tool == Tool::Select {
            self.selected = self.annotation_at(p, tol);
            if let Some(original) = self.selected_annotation().cloned() {
                self.begin_change();
                self.drag = Drag::Move { start: p, original };
            }
            return Press::Nothing;
        }

        let before = self.snapshot();
        let shape = match self.tool {
            Tool::Rectangle => Shape::Rectangle(Rect::from_points(p, p)),
            Tool::RoundedRect => Shape::RoundedRect(Rect::from_points(p, p)),
            Tool::Oval => Shape::Oval(Rect::from_points(p, p)),
            Tool::Bubble => Shape::Bubble(Rect::from_points(p, p)),
            Tool::Star => Shape::Star(Rect::from_points(p, p)),
            Tool::Polygon => Shape::Polygon(Rect::from_points(p, p)),
            Tool::Highlight => Shape::Highlight(Rect::from_points(p, p)),
            Tool::Redact => Shape::Redact(Rect::from_points(p, p)),
            Tool::Line | Tool::Arrow => Shape::Line { start: p, end: p },
            Tool::Pen | Tool::Sketch => Shape::Pen(vec![p]),
            Tool::Text => Shape::Text { origin: p, text: String::new(), rotation: 0.0 },
            Tool::Select | Tool::Crop => unreachable!("handled above"),
        };
        let style = self.styles[&self.tool].clone();
        let id = self.add(shape, style);
        self.selected = Some(id);

        if self.tool == Tool::Text {
            self.text_before = Some(before);
            return Press::EditText(id);
        }
        self.pending = Some(before);
        self.drag = Drag::Create { id, start: p };
        Press::Nothing
    }

    /// Pointer moved to `p` during a drag. `constrain` (Shift) keeps boxes
    /// square and snaps lines to 45°.
    pub fn drag_to(&mut self, p: Point, constrain: bool) {
        match self.drag.clone() {
            Drag::None => {}
            Drag::Create { id, start } => {
                let Some(a) = self.annotation_mut(id) else { return };
                if let Some(r) = a.shape.rect_mut() {
                    let end = if constrain { square(start, p) } else { p };
                    *r = Rect::from_points(start, end);
                    return;
                }
                match &mut a.shape {
                    Shape::Line { end, .. } => *end = if constrain { snap_45(start, p) } else { p },
                    Shape::Pen(points) if points.last().is_none_or(|last| last.distance(p) >= 1.0) => {
                        points.push(p);
                    }
                    _ => {}
                }
            }
            Drag::Move { start, mut original, .. } => {
                original.shape.translate(p.x - start.x, p.y - start.y);
                self.replace(original);
            }
            Drag::Handle { handle, original, .. } => {
                self.replace(drag_handle(&original, handle, p, constrain));
            }
            Drag::Crop { start } => {
                let end = self.doc.crop.clamp_point(p);
                self.crop_selection = Some(Rect::from_points(start, end));
            }
        }
    }

    /// Finish the gesture. Returns the recognized shape's name when a
    /// sketch was turned into a clean shape.
    pub fn release(&mut self) -> Option<&'static str> {
        let mut recognized = None;
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::None => {}
            Drag::Create { id, .. } => {
                // A click without a drag leaves nothing worth keeping.
                if self.doc.annotations.iter().any(|a| a.id == id && is_degenerate(a)) {
                    self.doc.annotations.retain(|a| a.id != id);
                    self.selected = None;
                } else if self.tool == Tool::Sketch {
                    recognized = self.recognize_sketch(id);
                }
                self.end_change();
            }
            Drag::Move { .. } | Drag::Handle { .. } => self.end_change(),
            Drag::Crop { .. } => {
                if self.crop_selection.is_some_and(|r| r.w < 4.0 || r.h < 4.0) {
                    self.crop_selection = None;
                }
            }
        }
        recognized
    }

    fn recognize_sketch(&mut self, id: u64) -> Option<&'static str> {
        let a = self.annotation_mut(id)?;
        let Shape::Pen(points) = &a.shape else { return None };
        let shape = sketch::recognize(points)?;
        let stroke = std::mem::replace(&mut a.shape, shape.clone());
        self.last_sketch = Some((id, stroke, shape.clone()));
        Some(sketch::name(&shape))
    }

    /// Undo the last recognition, keeping the hand-drawn stroke instead
    /// (Preview offers the same choice in a palette after sketching).
    pub fn keep_sketch(&mut self) {
        let Some((id, stroke, recognized)) = self.last_sketch.take() else { return };
        let before = self.snapshot();
        match self.annotation_mut(id) {
            Some(a) if a.shape == recognized => a.shape = stroke,
            _ => return,
        }
        self.commit(before);
    }

    /// Adjust Size: resample to `width`×`height` pixels (of the visible,
    /// cropped area) and record `dpi`. Annotations scale with the image so
    /// they keep their place.
    pub fn resize(&mut self, width: u32, height: u32, dpi: f64) -> anyhow::Result<()> {
        let before = self.snapshot();
        let crop = self.doc.crop;
        let (sx, sy) = (width as f64 / crop.w, height as f64 / crop.h);
        if width as f64 != crop.w || height as f64 != crop.h {
            let old = &self.doc.image;
            let new_w = (old.width() as f64 * sx).round().max(1.0) as i32;
            let new_h = (old.height() as f64 * sy).round().max(1.0) as i32;
            let image = cairo::ImageSurface::create(cairo::Format::ARgb32, new_w, new_h)?;
            {
                let cr = cairo::Context::new(&image)?;
                cr.scale(new_w as f64 / old.width() as f64, new_h as f64 / old.height() as f64);
                let pattern = cairo::SurfacePattern::create(old);
                pattern.set_filter(cairo::Filter::Best);
                pattern.set_extend(cairo::Extend::Pad); // no dark fringe at the edges
                cr.set_source(&pattern)?;
                cr.paint()?;
            }
            self.doc.image = image;
            let (w, h) = (width as f64, height as f64);
            let x = (crop.x * sx).round().min(new_w as f64 - w).max(0.0);
            let y = (crop.y * sy).round().min(new_h as f64 - h).max(0.0);
            self.doc.crop = Rect::new(x, y, w, h);

            let s = (sx * sy).sqrt();
            for a in &mut self.doc.annotations {
                a.shape.scale(sx, sy);
                a.style.width = (a.style.width * s).max(0.5);
                a.style.font_size = (a.style.font_size * s).max(1.0);
            }
            self.crop_selection = None;
        }
        self.doc.dpi = Some(dpi);
        self.commit(before);
        Ok(())
    }

    // ---- text editing ---------------------------------------------------

    pub fn start_text_edit(&mut self, id: u64) {
        self.selected = Some(id);
        self.text_before = Some(self.snapshot());
    }

    pub fn text_of(&self, id: u64) -> String {
        self.doc
            .annotations
            .iter()
            .find_map(|a| match &a.shape {
                Shape::Text { text, .. } if a.id == id => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    pub fn set_text(&mut self, id: u64, new_text: &str) {
        if let Some(Annotation { shape: Shape::Text { text, .. }, .. }) = self.annotation_mut(id) {
            *text = new_text.to_owned();
        }
    }

    /// Close the edit as one undo step; empty text boxes are dropped.
    pub fn finish_text_edit(&mut self, id: u64) {
        if self.text_of(id).trim().is_empty() {
            self.doc.annotations.retain(|a| a.id != id);
            if self.selected == Some(id) {
                self.selected = None;
            }
        }
        if let Some(before) = self.text_before.take() {
            self.commit(before);
        }
    }

    // ---- commands -------------------------------------------------------

    pub fn delete_selected(&mut self) -> bool {
        let Some(id) = self.selected.take() else { return false };
        let before = self.snapshot();
        self.doc.annotations.retain(|a| a.id != id);
        self.commit(before);
        true
    }

    pub fn duplicate_selected(&mut self) {
        let Some(mut copy) = self.selected_annotation().cloned() else { return };
        let before = self.snapshot();
        let offset = (self.doc.crop.w.min(self.doc.crop.h) * 0.02).max(8.0);
        copy.shape.translate(offset, offset);
        let id = self.add(copy.shape, copy.style);
        self.selected = Some(id);
        self.commit(before);
    }

    /// Rotate the selected text by `delta` radians.
    pub fn rotate_selected(&mut self, delta: f64) {
        let Some(id) = self.selected else { return };
        let before = self.snapshot();
        if let Some(Annotation { shape: Shape::Text { rotation, .. }, .. }) = self.annotation_mut(id) {
            *rotation = normalize_angle(*rotation + delta);
        }
        self.commit(before);
    }

    pub fn nudge(&mut self, dx: f64, dy: f64) {
        let Some(id) = self.selected else { return };
        let before = self.snapshot();
        if let Some(a) = self.annotation_mut(id) {
            a.shape.translate(dx, dy);
        }
        self.commit(before);
    }

    pub fn bring_to_front(&mut self) {
        self.reorder(|list, i| {
            let a = list.remove(i);
            list.push(a);
        });
    }

    pub fn send_to_back(&mut self) {
        self.reorder(|list, i| {
            let a = list.remove(i);
            list.insert(0, a);
        });
    }

    fn reorder(&mut self, op: impl Fn(&mut Vec<Annotation>, usize)) {
        let Some(id) = self.selected else { return };
        let Some(index) = self.doc.annotations.iter().position(|a| a.id == id) else { return };
        let before = self.snapshot();
        op(&mut self.doc.annotations, index);
        self.commit(before);
    }

    /// Apply the crop selection (Preview's ⌘K). Returns whether it cropped.
    pub fn apply_crop(&mut self) -> bool {
        let Some(selection) = self.crop_selection.take() else { return false };
        let Some(crop) = selection.intersect(&self.doc.crop) else { return false };
        let before = self.snapshot();
        self.doc.crop = Rect::new(crop.x.round(), crop.y.round(), crop.w.round(), crop.h.round());
        self.commit(before);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            let current = self.snapshot();
            self.restore(previous);
            self.redo.push(current);
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let current = self.snapshot();
            self.restore(next);
            self.undo.push(current);
        }
    }

    // ---- undo plumbing --------------------------------------------------

    fn add(&mut self, shape: Shape, style: Style) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.doc.annotations.push(Annotation { id, shape, style });
        id
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            annotations: self.doc.annotations.clone(),
            crop: self.doc.crop,
            dpi: self.doc.dpi,
            image: self.doc.image.clone(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.doc.annotations = snapshot.annotations;
        self.doc.crop = snapshot.crop;
        self.doc.dpi = snapshot.dpi;
        self.doc.image = snapshot.image;
        self.crop_selection = None;
        if self.selected_annotation().is_none() {
            self.selected = None;
        }
        self.dirty = true;
    }

    fn begin_change(&mut self) {
        self.pending = Some(self.snapshot());
    }

    fn end_change(&mut self) {
        if let Some(before) = self.pending.take() {
            self.commit(before);
        }
    }

    /// Record `before` as an undo step if anything actually changed.
    fn commit(&mut self, before: Snapshot) {
        if before != self.snapshot() {
            self.undo.push(before);
            self.redo.clear();
            self.dirty = true;
        }
    }
}

// ---- geometry helpers -----------------------------------------------------

fn square(start: Point, p: Point) -> Point {
    let side = (p.x - start.x).abs().max((p.y - start.y).abs());
    Point::new(start.x + side.copysign(p.x - start.x), start.y + side.copysign(p.y - start.y))
}

fn snap_45(start: Point, p: Point) -> Point {
    let step = std::f64::consts::FRAC_PI_4;
    let angle = ((p.y - start.y).atan2(p.x - start.x) / step).round() * step;
    let len = start.distance(p);
    Point::new(start.x + len * angle.cos(), start.y + len * angle.sin())
}

/// Map to (-π, π] so 0 means upright and the value stays readable.
fn normalize_angle(angle: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let a = angle.rem_euclid(TAU);
    if a > PI {
        a - TAU
    } else {
        a
    }
}

fn is_degenerate(a: &Annotation) -> bool {
    if let Some(r) = a.shape.rect() {
        return r.w < 2.0 && r.h < 2.0;
    }
    match &a.shape {
        Shape::Line { start, end } => start.distance(*end) < 2.0,
        Shape::Pen(points) => points.len() < 2,
        _ => false,
    }
}

/// Resize handles in model coordinates.
pub fn handles(a: &Annotation) -> Vec<Point> {
    if let Some(r) = a.shape.rect() {
        return r.corners().to_vec();
    }
    match &a.shape {
        Shape::Line { start, end } => vec![*start, *end],
        Shape::Pen(points) => Rect::bounding(points).map(|r| r.corners().to_vec()).unwrap_or_default(),
        // Bottom-right corner scales the font; the knob above the top edge
        // rotates.
        Shape::Text { .. } => {
            let Some([tl, tr, br, _]) = render::text_corners(a) else { return Vec::new() };
            vec![br, rotation_knob(tl, tr, a.style.font_size)]
        }
        _ => unreachable!("box shapes handled above"),
    }
}

/// Where the rotation handle sits: above the middle of the top edge,
/// perpendicular to it, so it follows the text as it turns.
fn rotation_knob(tl: Point, tr: Point, font_size: f64) -> Point {
    let gap = (font_size * 0.8).max(12.0);
    let mid = Point::new((tl.x + tr.x) / 2.0, (tl.y + tr.y) / 2.0);
    let len = tl.distance(tr).max(f64::EPSILON);
    // Unit normal pointing away from the text (up, when unrotated).
    let (nx, ny) = ((tr.y - tl.y) / len, -(tr.x - tl.x) / len);
    mid.offset(nx * gap, ny * gap)
}

/// Index of the rotation handle in `handles()` for text.
pub const TEXT_ROTATE_HANDLE: usize = 1;

fn handle_at(a: &Annotation, p: Point, tol: f64) -> Option<usize> {
    handles(a).iter().position(|h| h.distance(p) <= tol * 1.5)
}

fn drag_handle(original: &Annotation, handle: usize, p: Point, constrain: bool) -> Annotation {
    let mut a = original.clone();
    if let Some(r) = a.shape.rect_mut() {
        let anchor = r.corners()[(handle + 2) % 4];
        let end = if constrain { square(anchor, p) } else { p };
        *r = Rect::from_points(anchor, end);
        return a;
    }
    match &mut a.shape {
        Shape::Line { start, end } => {
            if handle == 0 {
                *start = if constrain { snap_45(*end, p) } else { p };
            } else {
                *end = if constrain { snap_45(*start, p) } else { p };
            }
        }
        Shape::Pen(points) => {
            let Some(from) = Rect::bounding(points) else { return a };
            let anchor = from.corners()[(handle + 2) % 4];
            let to = Rect::from_points(anchor, p);
            let sx = if from.w > 0.0 { to.w / from.w } else { 1.0 };
            let sy = if from.h > 0.0 { to.h / from.h } else { 1.0 };
            for pt in points.iter_mut() {
                *pt = Point::new(to.x + (pt.x - from.x) * sx, to.y + (pt.y - from.y) * sy);
            }
        }
        Shape::Text { origin, text, rotation } => {
            let (frame, angle) = render::text_frame(*origin, text, &original.style, *rotation);
            let center = frame.center();
            if handle == TEXT_ROTATE_HANDLE {
                // The knob sits straight above the center at rotation 0.
                let mut turned = (p.y - center.y).atan2(p.x - center.x) + std::f64::consts::FRAC_PI_2;
                if constrain {
                    let step = 15f64.to_radians();
                    turned = (turned / step).round() * step;
                }
                *rotation = normalize_angle(turned);
            } else {
                // Measure the drag in the text's own (unrotated) frame.
                let local = p.rotate_around(center, -angle);
                let scale = ((local.y - frame.y) / frame.h.max(1.0)).max(0.1);
                a.style.font_size = (original.style.font_size * scale).clamp(6.0, 1000.0);
            }
        }
        _ => unreachable!("box shapes handled above"),
    }
    a
}

pub fn hit_test(a: &Annotation, p: Point, tol: f64) -> bool {
    let band = tol + a.style.width / 2.0;
    match &a.shape {
        Shape::Rectangle(_)
        | Shape::RoundedRect(_)
        | Shape::Oval(_)
        | Shape::Bubble(_)
        | Shape::Star(_)
        | Shape::Polygon(_) => render::outline_contains(&a.shape, a.style.fill.is_some(), p, band),
        Shape::Highlight(r) | Shape::Redact(r) => r.inflate(tol).contains(p),
        Shape::Line { start, end } => crate::model::distance_to_segment(p, *start, *end) <= band,
        Shape::Pen(points) => match points.as_slice() {
            [single] => single.distance(p) <= band,
            _ => points.windows(2).any(|s| crate::model::distance_to_segment(p, s[0], s[1]) <= band),
        },
        Shape::Text { origin, text, rotation } => {
            let (frame, angle) = render::text_frame(*origin, text, &a.style, *rotation);
            frame.inflate(tol).contains(p.rotate_around(frame.center(), -angle))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        let image = cairo::ImageSurface::create(cairo::Format::ARgb32, 400, 300).unwrap();
        Editor::new(Document::new(image, None))
    }

    fn draw(ed: &mut Editor, tool: Tool, from: Point, to: Point) {
        ed.set_tool(tool);
        ed.press(from, 4.0);
        ed.drag_to(to, false);
        ed.release();
    }

    #[test]
    fn drawing_an_arrow_adds_an_undoable_annotation() {
        let mut ed = editor();
        draw(&mut ed, Tool::Arrow, Point::new(10.0, 10.0), Point::new(100.0, 50.0));
        assert_eq!(ed.doc.annotations.len(), 1);
        assert_eq!(ed.doc.annotations[0].style.heads, ArrowHeads::End);
        assert!(ed.dirty);

        ed.undo();
        assert!(ed.doc.annotations.is_empty());
        ed.redo();
        assert_eq!(ed.doc.annotations.len(), 1);
    }

    #[test]
    fn click_without_drag_creates_nothing() {
        let mut ed = editor();
        ed.set_tool(Tool::Rectangle);
        ed.press(Point::new(10.0, 10.0), 4.0);
        ed.release();
        assert!(ed.doc.annotations.is_empty());
        assert!(!ed.can_undo());
    }

    #[test]
    fn selected_shape_can_be_moved_from_any_tool() {
        let mut ed = editor();
        draw(&mut ed, Tool::Rectangle, Point::new(10.0, 10.0), Point::new(110.0, 60.0));
        // Still in the rectangle tool: grabbing the selected shape's edge moves it.
        ed.press(Point::new(50.0, 10.0), 4.0);
        ed.drag_to(Point::new(70.0, 30.0), false);
        ed.release();
        assert_eq!(ed.doc.annotations.len(), 1);
        assert_eq!(ed.doc.annotations[0].shape, Shape::Rectangle(Rect::new(30.0, 30.0, 100.0, 50.0)));

        // Away from the selection, the tool draws a new shape.
        ed.press(Point::new(200.0, 200.0), 4.0);
        ed.drag_to(Point::new(250.0, 250.0), false);
        ed.release();
        assert_eq!(ed.doc.annotations.len(), 2);
    }

    #[test]
    fn select_tool_moves_and_resizes() {
        let mut ed = editor();
        draw(&mut ed, Tool::Rectangle, Point::new(10.0, 10.0), Point::new(110.0, 60.0));
        ed.set_tool(Tool::Select);

        ed.press(Point::new(60.0, 10.0), 4.0);
        ed.drag_to(Point::new(70.0, 20.0), false);
        ed.release();
        assert_eq!(ed.doc.annotations[0].shape, Shape::Rectangle(Rect::new(20.0, 20.0, 100.0, 50.0)));

        // Bottom-right handle.
        ed.press(Point::new(120.0, 70.0), 4.0);
        ed.drag_to(Point::new(220.0, 120.0), false);
        ed.release();
        assert_eq!(ed.doc.annotations[0].shape, Shape::Rectangle(Rect::new(20.0, 20.0, 200.0, 100.0)));
        assert!(ed.can_undo());
    }

    #[test]
    fn shift_snaps_lines_to_45_degrees() {
        let mut ed = editor();
        ed.set_tool(Tool::Line);
        ed.press(Point::new(0.0, 0.0), 4.0);
        ed.drag_to(Point::new(100.0, 8.0), true);
        ed.release();
        let Shape::Line { end, .. } = ed.doc.annotations[0].shape else { panic!() };
        assert!(end.y.abs() < 1e-9);
    }

    #[test]
    fn crop_is_undoable() {
        let mut ed = editor();
        draw(&mut ed, Tool::Crop, Point::new(50.0, 40.0), Point::new(250.0, 240.0));
        assert!(ed.apply_crop());
        assert_eq!(ed.doc.crop, Rect::new(50.0, 40.0, 200.0, 200.0));
        ed.undo();
        assert_eq!(ed.doc.crop, Rect::new(0.0, 0.0, 400.0, 300.0));
    }

    #[test]
    fn empty_text_is_discarded_without_undo_step() {
        let mut ed = editor();
        ed.set_tool(Tool::Text);
        let Press::EditText(id) = ed.press(Point::new(20.0, 20.0), 4.0) else { panic!() };
        ed.finish_text_edit(id);
        assert!(ed.doc.annotations.is_empty());
        assert!(!ed.can_undo());

        let Press::EditText(id) = ed.press(Point::new(20.0, 20.0), 4.0) else { panic!() };
        ed.set_text(id, "Hello");
        ed.finish_text_edit(id);
        assert_eq!(ed.text_of(id), "Hello");
        ed.undo();
        assert!(ed.doc.annotations.is_empty());
    }

    fn text_rotation(ed: &Editor) -> f64 {
        match ed.doc.annotations[0].shape {
            Shape::Text { rotation, .. } => rotation,
            _ => panic!("not text"),
        }
    }

    fn add_text(ed: &mut Editor) -> u64 {
        ed.set_tool(Tool::Text);
        let Press::EditText(id) = ed.press(Point::new(100.0, 100.0), 4.0) else { panic!() };
        ed.set_text(id, "Rotate me");
        ed.finish_text_edit(id);
        id
    }

    #[test]
    fn dragging_the_knob_rotates_text_and_shift_snaps() {
        let mut ed = editor();
        add_text(&mut ed);
        let a = ed.doc.annotations[0].clone();
        let knob = handles(&a)[TEXT_ROTATE_HANDLE];
        let center = render::text_corners(&a).map(|c| Rect::bounding(&c).unwrap().center()).unwrap();

        // Drag the knob to the right of the center: a quarter turn clockwise.
        ed.press(knob, 4.0);
        ed.drag_to(center.offset(50.0, 3.0), true);
        ed.release();
        assert!((text_rotation(&ed) - std::f64::consts::FRAC_PI_2).abs() < 1e-9);

        ed.undo();
        assert_eq!(text_rotation(&ed), 0.0);
    }

    #[test]
    fn rotated_text_hits_along_its_rotated_frame() {
        let mut ed = editor();
        add_text(&mut ed);
        ed.rotate_selected(std::f64::consts::FRAC_PI_2);
        let a = ed.doc.annotations[0].clone();
        let [tl, tr, br, _] = render::text_corners(&a).unwrap();
        let center = Point::new((tl.x + br.x) / 2.0, (tl.y + br.y) / 2.0);
        // Vertical now: a point well above the center along the text hits,
        // a point the same distance to the side does not.
        let half_len = tl.distance(tr) / 2.0 - 2.0;
        assert!(hit_test(&a, center.offset(0.0, -half_len), 0.0));
        assert!(!hit_test(&a, center.offset(half_len, 0.0), 0.0));
    }

    #[test]
    fn sketched_circle_snaps_to_oval_and_can_be_kept_as_drawn() {
        let mut ed = editor();
        ed.set_tool(Tool::Sketch);
        let at = |i: usize| {
            let t = i as f64 / 40.0 * std::f64::consts::TAU;
            Point::new(200.0 + 60.0 * t.cos(), 150.0 + 40.0 * t.sin())
        };
        ed.press(at(0), 4.0);
        for i in 1..=40 {
            ed.drag_to(at(i), false);
        }
        assert_eq!(ed.release(), Some("oval"));
        assert!(matches!(ed.doc.annotations[0].shape, Shape::Oval(_)));

        ed.keep_sketch();
        assert!(matches!(ed.doc.annotations[0].shape, Shape::Pen(_)));
        ed.undo();
        assert!(matches!(ed.doc.annotations[0].shape, Shape::Oval(_)));
    }

    #[test]
    fn new_shapes_hit_on_their_outline() {
        let mut ed = editor();
        for tool in [Tool::RoundedRect, Tool::Bubble, Tool::Star, Tool::Polygon] {
            draw(&mut ed, tool, Point::new(100.0, 100.0), Point::new(200.0, 200.0));
            let a = ed.doc.annotations.last().unwrap().clone();
            assert!(!hit_test(&a, Point::new(150.0, 150.0), 2.0), "{tool:?}: unfilled center misses");
            ed.update_style(|s| s.fill = Some(Color::WHITE));
            let a = ed.doc.annotations.last().unwrap().clone();
            assert!(hit_test(&a, Point::new(150.0, 150.0), 2.0), "{tool:?}: filled center hits");
        }
    }

    #[test]
    fn resize_scales_image_crop_and_annotations_undoably() {
        let mut ed = editor();
        draw(&mut ed, Tool::Rectangle, Point::new(100.0, 100.0), Point::new(200.0, 150.0));
        draw(&mut ed, Tool::Crop, Point::new(0.0, 0.0), Point::new(200.0, 300.0));
        ed.apply_crop();

        ed.resize(100, 150, 144.0).unwrap();
        assert_eq!((ed.doc.image.width(), ed.doc.image.height()), (200, 150));
        assert_eq!(ed.doc.crop, Rect::new(0.0, 0.0, 100.0, 150.0));
        assert_eq!(ed.doc.annotations[0].shape, Shape::Rectangle(Rect::new(50.0, 50.0, 50.0, 25.0)));
        assert_eq!(ed.doc.dpi, Some(144.0));

        ed.undo();
        assert_eq!(ed.doc.image.width(), 400);
        assert_eq!(ed.doc.dpi, None);
    }

    #[test]
    fn resolution_only_change_keeps_pixels() {
        let mut ed = editor();
        ed.resize(400, 300, 300.0).unwrap();
        assert_eq!(ed.doc.image.width(), 400);
        assert_eq!(ed.doc.dpi, Some(300.0));
        assert!(ed.can_undo());
    }

    #[test]
    fn restyling_selection_is_undoable_and_becomes_default() {
        let mut ed = editor();
        draw(&mut ed, Tool::Oval, Point::new(10.0, 10.0), Point::new(80.0, 80.0));
        ed.update_style(|s| s.stroke = Color::BLACK);
        assert_eq!(ed.doc.annotations[0].style.stroke, Color::BLACK);
        assert_eq!(ed.styles[&Tool::Oval].stroke, Color::BLACK);
        ed.undo();
        assert_eq!(ed.doc.annotations[0].style.stroke, Color::RED);
    }
}
