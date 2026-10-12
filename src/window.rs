//! One window per image, like Preview. Hosts the canvas and the markup
//! toolbar, and translates widget events into `Editor` calls in model
//! coordinates.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{cairo, gdk, gio, glib};

use crate::editor::{self, Document, Editor, Press, Tool};
use crate::imageio;
use crate::model::{ArrowHeads, Color, Point, Rect, Shape, Style};
use crate::{render, resize_dialog};

/// Blank space kept around the image, in widget pixels.
const MARGIN: f64 = 24.0;
/// Pointer slop for hit testing, in widget pixels.
const HIT_SLOP: f64 = 6.0;
const HANDLE_RADIUS: f64 = 5.0;
const MIN_ZOOM: f64 = 0.05;
const MAX_ZOOM: f64 = 16.0;

// Toolbar order follows Preview: Sketch, Draw, Shapes ▾, Text, …
const LEADING_TOOLS: [(Tool, &str, &str); 3] = [
    (Tool::Select, "scrawl-select-symbolic", "Select (V)"),
    (Tool::Sketch, "scrawl-sketch-symbolic", "Sketch: rough shapes snap to clean ones (S)"),
    (Tool::Pen, "scrawl-pen-symbolic", "Draw freehand (P)"),
];

/// The Shapes palette, in Preview's two-column order.
const SHAPES: [(Tool, &str, &str); 8] = [
    (Tool::Line, "scrawl-line-symbolic", "Line (L)"),
    (Tool::Arrow, "scrawl-arrow-symbolic", "Arrow (A)"),
    (Tool::Rectangle, "scrawl-rectangle-symbolic", "Rectangle (R)"),
    (Tool::RoundedRect, "scrawl-rounded-rect-symbolic", "Rounded Rectangle"),
    (Tool::Oval, "scrawl-oval-symbolic", "Oval (O)"),
    (Tool::Bubble, "scrawl-bubble-symbolic", "Speech Bubble"),
    (Tool::Star, "scrawl-star-symbolic", "Star"),
    (Tool::Polygon, "scrawl-polygon-symbolic", "Hexagon"),
];

const TRAILING_TOOLS: [(Tool, &str, &str); 4] = [
    (Tool::Text, "scrawl-text-symbolic", "Text (T)"),
    (Tool::Highlight, "scrawl-highlight-symbolic", "Highlight (H)"),
    (Tool::Redact, "scrawl-redact-symbolic", "Redact (X)"),
    (Tool::Crop, "scrawl-crop-symbolic", "Crop selection (C, apply with Enter or Ctrl+K)"),
];

const HEAD_LABELS: [&str; 4] = ["No arrow heads", "Arrow at end", "Arrow at start", "Arrows at both ends"];

/// Model ↔ widget mapping: `widget = model * scale + offset`.
#[derive(Clone, Copy)]
struct View {
    scale: f64,
    ox: f64,
    oy: f64,
}

impl View {
    fn to_model(self, x: f64, y: f64) -> Point {
        Point::new((x - self.ox) / self.scale, (y - self.oy) / self.scale)
    }

    fn to_widget(self, p: Point) -> (f64, f64) {
        (p.x * self.scale + self.ox, p.y * self.scale + self.oy)
    }

    fn rect_to_widget(self, r: Rect) -> Rect {
        let (x, y) = self.to_widget(Point::new(r.x, r.y));
        Rect::new(x, y, r.w * self.scale, r.h * self.scale)
    }
}

struct Controls {
    tools: Vec<(Tool, gtk::ToggleButton)>,
    /// Toggle for the most recently chosen shape, plus the palette that
    /// picks it.
    shape_button: gtk::ToggleButton,
    shape_tool: Cell<Tool>,
    shape_popover: gtk::Popover,
    shape_choices: Vec<(Tool, gtk::Button)>,
    stroke: gtk::ColorDialogButton,
    fill_toggle: gtk::ToggleButton,
    fill: gtk::ColorDialogButton,
    width: gtk::SpinButton,
    heads: gtk::DropDown,
    dashed: gtk::ToggleButton,
    shadow: gtk::ToggleButton,
    font_size: gtk::SpinButton,
    crop: gtk::Button,
}

struct Inner {
    /// Weak: the window owns `Inner` (see `new`), not the other way round.
    window: glib::WeakRef<adw::ApplicationWindow>,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    markup_bar: gtk::Box,
    area: gtk::DrawingArea,
    controls: Controls,
    text_popover: gtk::Popover,
    text_view: gtk::TextView,

    editor: RefCell<Option<Editor>>,
    /// `None` fits the image to the window.
    zoom: Cell<Option<f64>>,
    /// Guards control signal handlers while the toolbar mirrors the model.
    syncing: Cell<bool>,
    drag_origin: Cell<(f64, f64)>,
    editing_text: Cell<Option<u64>>,
    source: RefCell<Option<PathBuf>>,
    /// Where Ctrl+S writes. Unset until the user picks a file, so the
    /// original is never overwritten with flattened pixels by accident.
    save_target: RefCell<Option<PathBuf>>,
}

#[derive(Clone)]
pub struct ScrawlWindow {
    inner: Rc<Inner>,
}

impl ScrawlWindow {
    pub fn new(app: &adw::Application) -> Self {
        let title = adw::WindowTitle::new("Scrawl", "");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));

        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Open (Ctrl+O)")
            .action_name("win.open")
            .build();
        header.pack_start(&open);

        let undo = icon_button("edit-undo-symbolic", "Undo (Ctrl+Z)", "win.undo");
        let redo = icon_button("edit-redo-symbolic", "Redo (Ctrl+Shift+Z)", "win.redo");
        let undo_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        undo_box.add_css_class("linked");
        undo_box.append(&undo);
        undo_box.append(&redo);
        header.pack_start(&undo_box);

        let menu = gio::Menu::new();
        let file_section = gio::Menu::new();
        file_section.append(Some("Save"), Some("win.save"));
        file_section.append(Some("Save As…"), Some("win.save-as"));
        file_section.append(Some("Copy Image"), Some("win.copy"));
        file_section.append(Some("New from Clipboard"), Some("win.paste"));
        menu.append_section(None, &file_section);
        let arrange = gio::Menu::new();
        arrange.append(Some("Adjust Size…"), Some("win.adjust-size"));
        arrange.append(Some("Duplicate"), Some("win.duplicate"));
        arrange.append(Some("Bring to Front"), Some("win.front"));
        arrange.append(Some("Send to Back"), Some("win.back"));
        menu.append_section(None, &arrange);
        let view_section = gio::Menu::new();
        view_section.append(Some("Zoom In"), Some("win.zoom-in"));
        view_section.append(Some("Zoom Out"), Some("win.zoom-out"));
        view_section.append(Some("Zoom to Fit"), Some("win.zoom-fit"));
        view_section.append(Some("Actual Size"), Some("win.zoom-actual"));
        menu.append_section(None, &view_section);
        let app_section = gio::Menu::new();
        app_section.append(Some("New Window"), Some("app.new-window"));
        menu.append_section(None, &app_section);
        header
            .pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).build());

        let share = icon_button("edit-copy-symbolic", "Copy image to clipboard (Ctrl+C)", "win.copy");
        header.pack_end(&share);
        let save = icon_button("document-save-symbolic", "Save (Ctrl+S)", "win.save");
        header.pack_end(&save);

        let (markup_bar, controls) = build_markup_bar();

        let area = gtk::DrawingArea::new();
        area.set_focusable(true);
        area.set_hexpand(true);
        area.set_vexpand(true);
        let scroller = gtk::ScrolledWindow::builder().child(&area).build();

        let empty = adw::StatusPage::builder()
            .icon_name("image-x-generic-symbolic")
            .title("Open an Image")
            .description("Drop an image here, open one, or paste a screenshot from the clipboard")
            .build();
        let empty_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        empty_buttons.set_halign(gtk::Align::Center);
        let open_big = gtk::Button::builder().label("Open…").action_name("win.open").build();
        open_big.add_css_class("pill");
        open_big.add_css_class("suggested-action");
        let paste_big = gtk::Button::builder().label("Paste").action_name("win.paste").build();
        paste_big.add_css_class("pill");
        empty_buttons.append(&open_big);
        empty_buttons.append(&paste_big);
        empty.set_child(Some(&empty_buttons));

        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scroller, Some("canvas"));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.add_top_bar(&markup_bar);
        toolbar_view.set_content(Some(&toasts));
        markup_bar.set_visible(false);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(1100)
            .default_height(760)
            .content(&toolbar_view)
            .build();

        let text_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(6)
            .right_margin(6)
            .build();
        let text_scroller = gtk::ScrolledWindow::builder()
            .child(&text_view)
            .min_content_width(280)
            .min_content_height(90)
            .build();
        let done = gtk::Button::builder().label("Done").halign(gtk::Align::End).build();
        done.add_css_class("suggested-action");
        let popover_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        popover_box.append(&text_scroller);
        popover_box.append(&done);
        let text_popover = gtk::Popover::builder().child(&popover_box).build();
        text_popover.set_parent(&area);
        area.connect_destroy(glib::clone!(
            #[weak]
            text_popover,
            move |_| text_popover.unparent()
        ));
        done.connect_clicked(glib::clone!(
            #[weak]
            text_popover,
            move |_| text_popover.popdown()
        ));

        let this = Self {
            inner: Rc::new(Inner {
                window: window.downgrade(),
                title,
                toasts,
                stack,
                markup_bar,
                area,
                controls,
                text_popover,
                text_view,
                editor: RefCell::new(None),
                zoom: Cell::new(None),
                syncing: Cell::new(false),
                drag_origin: Cell::new((0.0, 0.0)),
                editing_text: Cell::new(None),
                source: RefCell::new(None),
                save_target: RefCell::new(None),
            }),
        };
        // Signal handlers only hold weak references, so the window keeps the
        // state alive; it's released when the window is disposed.
        let keep_alive = this.inner.clone();
        window.connect_destroy(move |_| {
            let _ = &keep_alive;
        });
        this.setup_actions();
        this.setup_canvas();
        this.setup_controls();
        this.setup_text_editing();
        this.setup_drop();
        this.setup_close_guard();
        this.refresh();
        this
    }

    pub fn present(&self) {
        self.window().present();
    }

    fn window(&self) -> adw::ApplicationWindow {
        self.inner.window.upgrade().expect("window outlives its state")
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    fn from_weak(weak: &Weak<Inner>) -> Option<Self> {
        weak.upgrade().map(|inner| Self { inner })
    }

    /// Run `f` with a strong handle; used by every signal closure so the
    /// window state never keeps itself alive.
    fn bind<A>(&self, f: impl Fn(&Self, A) + 'static) -> impl Fn(A) + 'static {
        let weak = self.weak();
        move |args| {
            if let Some(this) = Self::from_weak(&weak) {
                f(&this, args);
            }
        }
    }

    // ---- documents ------------------------------------------------------

    pub fn open_file(&self, file: &gio::File) {
        let Some(path) = file.path() else {
            self.toast("Only local files can be opened");
            return;
        };
        match imageio::load_file(&path) {
            Ok((surface, dpi)) => self.load(surface, dpi, Some(path)),
            Err(err) => {
                log::warn!("{err:#}");
                self.toast(&format!("{err}"));
            }
        }
    }

    fn load(&self, surface: cairo::ImageSurface, dpi: Option<f64>, source: Option<PathBuf>) {
        let inner = &self.inner;
        inner.editor.replace(Some(Editor::new(Document::new(surface, dpi))));
        inner.source.replace(source);
        inner.save_target.replace(None);
        inner.zoom.set(None);
        inner.stack.set_visible_child_name("canvas");
        inner.markup_bar.set_visible(true);
        self.select_tool(Tool::Arrow);
        self.update_content_size();
        self.refresh();
        inner.area.grab_focus();
    }

    fn has_document(&self) -> bool {
        self.inner.editor.borrow().is_some()
    }

    /// Open in this window when it's empty, otherwise in a new one.
    fn target_window(&self) -> Self {
        if !self.has_document() {
            return self.clone();
        }
        let app = self
            .window()
            .application()
            .and_downcast::<adw::Application>()
            .expect("window belongs to an adw::Application");
        let window = Self::new(&app);
        window.present();
        window
    }

    fn with_editor<R>(&self, f: impl FnOnce(&mut Editor) -> R) -> Option<R> {
        self.inner.editor.borrow_mut().as_mut().map(f)
    }

    // ---- view -----------------------------------------------------------

    fn view(&self) -> Option<View> {
        let editor = self.inner.editor.borrow();
        let crop = editor.as_ref()?.doc.crop;
        let area = &self.inner.area;
        let (w, h) = (area.width() as f64, area.height() as f64);
        let scale = match self.inner.zoom.get() {
            Some(zoom) => zoom,
            // Fit, but never blow small images up beyond actual size.
            None => ((w - 2.0 * MARGIN) / crop.w).min((h - 2.0 * MARGIN) / crop.h).clamp(MIN_ZOOM, 1.0),
        };
        let ox = ((w - crop.w * scale) / 2.0).max(MARGIN) - crop.x * scale;
        let oy = ((h - crop.h * scale) / 2.0).max(MARGIN) - crop.y * scale;
        Some(View { scale, ox, oy })
    }

    fn update_content_size(&self) {
        let crop = self.inner.editor.borrow().as_ref().map(|e| e.doc.crop);
        let area = &self.inner.area;
        match (self.inner.zoom.get(), crop) {
            (Some(zoom), Some(crop)) => {
                area.set_content_width((crop.w * zoom + 2.0 * MARGIN) as i32);
                area.set_content_height((crop.h * zoom + 2.0 * MARGIN) as i32);
            }
            _ => {
                area.set_content_width(0);
                area.set_content_height(0);
            }
        }
        area.queue_draw();
    }

    fn set_zoom(&self, zoom: Option<f64>) {
        self.inner.zoom.set(zoom.map(|z| z.clamp(MIN_ZOOM, MAX_ZOOM)));
        self.update_content_size();
    }

    fn zoom_by(&self, factor: f64) {
        if let Some(view) = self.view() {
            self.set_zoom(Some(view.scale * factor));
        }
    }

    fn draw(&self, cr: &cairo::Context) {
        let Some(view) = self.view() else { return };
        let editor = self.inner.editor.borrow();
        let Some(editor) = editor.as_ref() else { return };
        let page = view.rect_to_widget(editor.doc.crop);

        // Soft page shadow and a white backdrop for transparent images.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
        cr.rectangle(page.x + 1.0, page.y + 2.0, page.w, page.h);
        cr.fill().ok();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(page.x, page.y, page.w, page.h);
        cr.fill().ok();

        cr.save().ok();
        cr.translate(view.ox, view.oy);
        cr.scale(view.scale, view.scale);
        render::draw_document(cr, &editor.doc);
        cr.restore().ok();

        if let Some(selection) = editor.crop_selection {
            let sel = view.rect_to_widget(selection);
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            cr.rectangle(page.x, page.y, page.w, page.h);
            cr.rectangle(sel.x, sel.y, sel.w, sel.h);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
            cr.fill().ok();
            cr.set_fill_rule(cairo::FillRule::Winding);
            dashed_outline(cr, sel);
        }

        if let Some(a) = editor.selected_annotation() {
            let handles = editor::handles(a);
            if let Some(corners) = render::text_corners(a) {
                // Text outlines turn with the text; a stem links the top
                // edge to the rotation knob.
                dashed_polygon(cr, &corners.map(|p| view.to_widget(p)));
                let top =
                    Point::new((corners[0].x + corners[1].x) / 2.0, (corners[0].y + corners[1].y) / 2.0);
                let (x0, y0) = view.to_widget(top);
                let (x1, y1) = view.to_widget(handles[editor::TEXT_ROTATE_HANDLE]);
                cr.set_source_rgb(0.21, 0.52, 0.89);
                cr.set_line_width(1.0);
                cr.move_to(x0, y0);
                cr.line_to(x1, y1);
                cr.stroke().ok();
            } else if !matches!(a.shape, Shape::Line { .. }) {
                dashed_outline(cr, view.rect_to_widget(render::annotation_bounds(a)));
            }
            for handle in handles {
                let (x, y) = view.to_widget(handle);
                cr.new_sub_path();
                cr.arc(x, y, HANDLE_RADIUS, 0.0, std::f64::consts::TAU);
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.fill_preserve().ok();
                cr.set_source_rgb(0.21, 0.52, 0.89);
                cr.set_line_width(1.5);
                cr.stroke().ok();
            }
        }
    }

    // ---- setup ----------------------------------------------------------

    fn setup_canvas(&self) {
        let area = &self.inner.area;
        let weak = self.weak();
        area.set_draw_func(move |_, cr, _, _| {
            if let Some(this) = Self::from_weak(&weak) {
                this.draw(cr);
            }
        });

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let f = self.bind(|this, (x, y): (f64, f64)| this.on_press(x, y));
        drag.connect_drag_begin(move |_, x, y| f((x, y)));
        let f = self.bind(|this, (dx, dy, shift): (f64, f64, bool)| this.on_drag(dx, dy, shift));
        drag.connect_drag_update(move |gesture, dx, dy| {
            let shift = gesture.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
            f((dx, dy, shift))
        });
        let f = self.bind(|this, ()| this.on_release());
        drag.connect_drag_end(move |_, _, _| f(()));
        area.add_controller(drag);

        // Double-click a text box to edit it.
        let click = gtk::GestureClick::new();
        let f = self.bind(|this, (x, y): (f64, f64)| this.on_double_click(x, y));
        click.connect_pressed(move |_, n_press, x, y| {
            if n_press == 2 {
                f((x, y))
            }
        });
        area.add_controller(click);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = self.weak();
        scroll.connect_scroll(move |controller, _, dy| {
            let ctrl = controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            match Self::from_weak(&weak) {
                Some(this) if ctrl => {
                    this.zoom_by(if dy < 0.0 { 1.1 } else { 1.0 / 1.1 });
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        area.add_controller(scroll);

        let keys = gtk::EventControllerKey::new();
        let weak = self.weak();
        keys.connect_key_pressed(move |_, key, _, state| match Self::from_weak(&weak) {
            Some(this) => this.on_key(key, state),
            None => glib::Propagation::Proceed,
        });
        area.add_controller(keys);
    }

    fn on_press(&self, x: f64, y: f64) {
        self.inner.area.grab_focus();
        let Some(view) = self.view() else { return };
        self.inner.drag_origin.set((x, y));
        let p = view.to_model(x, y);
        let press = self.with_editor(|ed| ed.press(p, HIT_SLOP / view.scale));
        self.refresh();
        if let Some(Press::EditText(id)) = press {
            self.begin_text_edit(id);
        }
    }

    fn on_drag(&self, dx: f64, dy: f64, constrain: bool) {
        let Some(view) = self.view() else { return };
        let (x, y) = self.inner.drag_origin.get();
        let p = view.to_model(x + dx, y + dy);
        self.with_editor(|ed| ed.drag_to(p, constrain));
        self.inner.area.queue_draw();
    }

    fn on_release(&self) {
        let recognized = self.with_editor(Editor::release).flatten();
        self.refresh();
        if let Some(name) = recognized {
            let toast = adw::Toast::builder()
                .title(format!("Sketch recognized as {name}"))
                .button_label("Keep Drawing")
                .action_name("win.keep-sketch")
                .timeout(4)
                .build();
            self.inner.toasts.add_toast(toast);
        }
    }

    fn on_double_click(&self, x: f64, y: f64) {
        let Some(view) = self.view() else { return };
        let p = view.to_model(x, y);
        let id = self.with_editor(|ed| {
            let id = ed.text_at(p, HIT_SLOP / view.scale)?;
            ed.start_text_edit(id);
            Some(id)
        });
        if let Some(Some(id)) = id {
            self.begin_text_edit(id);
        }
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        let step = if state.contains(gdk::ModifierType::SHIFT_MASK) { 10.0 } else { 1.0 };
        match key {
            gdk::Key::Delete | gdk::Key::BackSpace => {
                self.with_editor(Editor::delete_selected);
            }
            gdk::Key::Escape => {
                self.with_editor(|ed| {
                    if ed.crop_selection.take().is_none() {
                        ed.selected = None;
                    }
                });
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                let tool = self.with_editor(|ed| ed.tool());
                if tool == Some(Tool::Crop) {
                    self.crop();
                    return glib::Propagation::Stop;
                }
                let text = self.with_editor(|ed| {
                    let a = ed.selected_annotation()?;
                    matches!(a.shape, Shape::Text { .. }).then_some(a.id)
                });
                if let Some(Some(id)) = text {
                    self.with_editor(|ed| ed.start_text_edit(id));
                    self.begin_text_edit(id);
                }
            }
            gdk::Key::bracketleft => {
                self.with_editor(|ed| ed.rotate_selected(-15f64.to_radians())).unwrap_or(())
            }
            gdk::Key::bracketright => {
                self.with_editor(|ed| ed.rotate_selected(15f64.to_radians())).unwrap_or(())
            }
            gdk::Key::Left => self.nudge(-step, 0.0),
            gdk::Key::Right => self.nudge(step, 0.0),
            gdk::Key::Up => self.nudge(0.0, -step),
            gdk::Key::Down => self.nudge(0.0, step),
            _ => {
                let tool = match key.to_lower() {
                    gdk::Key::v => Tool::Select,
                    gdk::Key::s => Tool::Sketch,
                    gdk::Key::a => Tool::Arrow,
                    gdk::Key::l => Tool::Line,
                    gdk::Key::r => Tool::Rectangle,
                    gdk::Key::o => Tool::Oval,
                    gdk::Key::p => Tool::Pen,
                    gdk::Key::h => Tool::Highlight,
                    gdk::Key::t => Tool::Text,
                    gdk::Key::x => Tool::Redact,
                    gdk::Key::c => Tool::Crop,
                    _ => return glib::Propagation::Proceed,
                };
                self.select_tool(tool);
            }
        }
        self.refresh();
        glib::Propagation::Stop
    }

    fn nudge(&self, dx: f64, dy: f64) {
        self.with_editor(|ed| ed.nudge(dx, dy));
    }

    fn select_tool(&self, tool: Tool) {
        let c = &self.inner.controls;
        if let Some((_, button)) = c.tools.iter().find(|(t, _)| *t == tool) {
            button.set_active(true);
            return;
        }
        if !SHAPES.iter().any(|(t, ..)| *t == tool) {
            return;
        }
        self.show_shape(tool);
        if c.shape_button.is_active() {
            // Already in shape mode: no "toggled" signal, switch directly.
            self.with_editor(|ed| ed.set_tool(tool));
            self.refresh();
        } else {
            c.shape_button.set_active(true);
        }
    }

    /// Make the shapes toggle stand for `tool`.
    fn show_shape(&self, tool: Tool) {
        let c = &self.inner.controls;
        if let Some((_, icon, tooltip)) = SHAPES.iter().find(|(t, ..)| *t == tool) {
            c.shape_tool.set(tool);
            c.shape_button.set_icon_name(icon);
            c.shape_button.set_tooltip_text(Some(tooltip));
        }
    }

    fn setup_controls(&self) {
        let c = &self.inner.controls;
        for (tool, button) in &c.tools {
            let tool = *tool;
            let weak = self.weak();
            button.connect_toggled(move |button| {
                let Some(this) = Self::from_weak(&weak) else { return };
                if button.is_active() && !this.inner.syncing.get() {
                    this.with_editor(|ed| ed.set_tool(tool));
                    this.refresh();
                }
            });
        }

        let weak = self.weak();
        c.shape_button.connect_toggled(move |button| {
            let Some(this) = Self::from_weak(&weak) else { return };
            if button.is_active() && !this.inner.syncing.get() {
                let tool = this.inner.controls.shape_tool.get();
                this.with_editor(|ed| ed.set_tool(tool));
                this.refresh();
            }
        });
        for (tool, button) in &c.shape_choices {
            let tool = *tool;
            let f = self.bind(move |this, ()| {
                this.inner.controls.shape_popover.popdown();
                this.select_tool(tool);
            });
            button.connect_clicked(move |_| f(()));
        }

        let weak = self.weak();
        let restyle = move |change: Box<dyn Fn(&mut Style)>| {
            let Some(this) = Self::from_weak(&weak) else { return };
            if this.inner.syncing.get() {
                return;
            }
            this.with_editor(|ed| ed.update_style(&change));
            this.refresh();
        };
        let restyle = Rc::new(restyle);

        let r = restyle.clone();
        c.stroke.connect_rgba_notify(move |b| {
            let color = to_color(&b.rgba());
            r(Box::new(move |s| s.stroke = color));
        });
        let r = restyle.clone();
        let fill_button = c.fill.clone();
        c.fill_toggle.connect_toggled(move |b| {
            let fill = b.is_active().then(|| to_color(&fill_button.rgba()));
            fill_button.set_sensitive(b.is_active());
            r(Box::new(move |s| s.fill = fill));
        });
        let r = restyle.clone();
        let fill_toggle = c.fill_toggle.clone();
        c.fill.connect_rgba_notify(move |b| {
            if fill_toggle.is_active() {
                let color = to_color(&b.rgba());
                r(Box::new(move |s| s.fill = Some(color)));
            }
        });
        let r = restyle.clone();
        c.width.connect_value_changed(move |b| {
            let width = b.value();
            r(Box::new(move |s| s.width = width));
        });
        let r = restyle.clone();
        c.heads.connect_selected_notify(move |d| {
            let heads = ArrowHeads::ALL[d.selected() as usize % 4];
            r(Box::new(move |s| s.heads = heads));
        });
        let r = restyle.clone();
        c.dashed.connect_toggled(move |b| {
            let dashed = b.is_active();
            r(Box::new(move |s| s.dashed = dashed));
        });
        let r = restyle.clone();
        c.shadow.connect_toggled(move |b| {
            let shadow = b.is_active();
            r(Box::new(move |s| s.shadow = shadow));
        });
        let r = restyle;
        c.font_size.connect_value_changed(move |b| {
            let size = b.value();
            r(Box::new(move |s| s.font_size = size));
        });

        let f = self.bind(|this, ()| this.crop());
        c.crop.connect_clicked(move |_| f(()));
    }

    /// Mirror the editor state onto the toolbar.
    fn sync_controls(&self) {
        let state = self.inner.editor.borrow().as_ref().map(|ed| {
            let selected = ed.selected_annotation().map(Tool::for_annotation);
            (ed.tool(), ed.displayed_style(), selected, ed.crop_selection.is_some())
        });
        let Some((tool, style, selected, has_crop)) = state else { return };
        let c = &self.inner.controls;
        self.inner.syncing.set(true);

        for (t, button) in &c.tools {
            if *t == tool {
                button.set_active(true);
            }
        }
        if SHAPES.iter().any(|(t, ..)| *t == tool) {
            self.show_shape(tool);
            c.shape_button.set_active(true);
        }
        c.stroke.set_rgba(&to_rgba(style.stroke));
        c.fill_toggle.set_active(style.fill.is_some());
        c.fill.set_sensitive(style.fill.is_some());
        if let Some(fill) = style.fill {
            c.fill.set_rgba(&to_rgba(fill));
        }
        c.width.set_value(style.width);
        c.heads.set_selected(ArrowHeads::ALL.iter().position(|h| *h == style.heads).unwrap_or(0) as u32);
        c.dashed.set_active(style.dashed);
        c.shadow.set_active(style.shadow);
        c.font_size.set_value(style.font_size);

        // Only offer the attributes that apply to what's selected or about
        // to be drawn.
        let kind = selected.unwrap_or(tool);
        let closed = matches!(
            kind,
            Tool::Rectangle | Tool::RoundedRect | Tool::Oval | Tool::Bubble | Tool::Star | Tool::Polygon
        );
        let stroked = closed || matches!(kind, Tool::Line | Tool::Arrow | Tool::Pen | Tool::Sketch);
        c.stroke.set_sensitive(!matches!(kind, Tool::Select | Tool::Crop));
        c.fill_toggle.set_sensitive(closed || kind == Tool::Text);
        c.width.set_sensitive(stroked);
        c.dashed.set_sensitive(stroked);
        c.heads.set_sensitive(matches!(kind, Tool::Line | Tool::Arrow));
        c.shadow.set_sensitive(stroked || kind == Tool::Text);
        c.font_size.set_sensitive(kind == Tool::Text);
        c.crop.set_visible(tool == Tool::Crop);
        c.crop.set_sensitive(has_crop);

        self.inner.syncing.set(false);
    }

    // ---- text editing ---------------------------------------------------

    fn setup_text_editing(&self) {
        let inner = &self.inner;
        let weak = self.weak();
        inner.text_view.buffer().connect_changed(move |buffer| {
            let Some(this) = Self::from_weak(&weak) else { return };
            let Some(id) = this.inner.editing_text.get() else { return };
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            this.with_editor(|ed| ed.set_text(id, &text));
            this.inner.area.queue_draw();
        });

        let weak = self.weak();
        inner.text_popover.connect_closed(move |_| {
            let Some(this) = Self::from_weak(&weak) else { return };
            if let Some(id) = this.inner.editing_text.take() {
                this.with_editor(|ed| ed.finish_text_edit(id));
                this.refresh();
                this.inner.area.grab_focus();
            }
        });

        // Ctrl+Enter finishes; plain Enter adds a line.
        let keys = gtk::EventControllerKey::new();
        let popover = inner.text_popover.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && state.contains(gdk::ModifierType::CONTROL_MASK)
            {
                popover.popdown();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        inner.text_view.add_controller(keys);
    }

    fn begin_text_edit(&self, id: u64) {
        let Some(view) = self.view() else { return };
        let inner = &self.inner;
        let Some((text, bounds)) = inner.editor.borrow().as_ref().and_then(|ed| {
            let a = ed.doc.annotations.iter().find(|a| a.id == id)?;
            Some((ed.text_of(id), render::annotation_bounds(a)))
        }) else {
            return;
        };
        // Set the text before marking the edit active so the buffer's
        // "changed" signal doesn't write it back.
        inner.text_view.buffer().set_text(&text);
        inner.editing_text.set(Some(id));

        let r = view.rect_to_widget(bounds);
        inner.text_popover.set_pointing_to(Some(&gdk::Rectangle::new(
            r.x as i32,
            r.y as i32,
            r.w.max(1.0) as i32,
            r.h.max(1.0) as i32,
        )));
        inner.text_popover.popup();
        inner.text_view.grab_focus();
        self.refresh();
    }

    // ---- actions --------------------------------------------------------

    fn setup_actions(&self) {
        let window = self.window();
        let add = |name: &str, f: fn(&Self)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = self.weak();
            action.connect_activate(move |_, _| {
                if let Some(this) = Self::from_weak(&weak) {
                    f(&this);
                }
            });
            window.add_action(&action);
        };
        add("open", Self::open_dialog);
        add("save", Self::save);
        add("save-as", Self::save_as);
        add("copy", Self::copy);
        add("paste", Self::paste);
        add("undo", |this| this.edit(Editor::undo));
        add("redo", |this| this.edit(Editor::redo));
        add("crop", Self::crop);
        add("duplicate", |this| this.edit(Editor::duplicate_selected));
        add("front", |this| this.edit(Editor::bring_to_front));
        add("back", |this| this.edit(Editor::send_to_back));
        add("zoom-in", |this| this.zoom_by(1.25));
        add("zoom-out", |this| this.zoom_by(0.8));
        add("zoom-fit", |this| this.set_zoom(None));
        add("zoom-actual", |this| this.set_zoom(Some(1.0)));
        add("keep-sketch", |this| this.edit(Editor::keep_sketch));
        add("adjust-size", Self::adjust_size);
    }

    fn edit(&self, f: impl FnOnce(&mut Editor)) {
        self.with_editor(f);
        self.refresh();
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        if let Some(action) = self.window().lookup_action(name).and_downcast::<gio::SimpleAction>() {
            action.set_enabled(enabled);
        }
    }

    fn refresh(&self) {
        self.sync_controls();

        let state = self.inner.editor.borrow().as_ref().map(|ed| {
            (
                ed.can_undo(),
                ed.can_redo(),
                ed.selected.is_some(),
                ed.crop_selection.is_some(),
                ed.dirty,
                ed.doc.crop,
            )
        });
        let (undo, redo, selected, crop, dirty, size) = match state {
            Some((u, r, s, c, d, size)) => (u, r, s, c, d, Some(size)),
            None => (false, false, false, false, false, None),
        };
        let has_doc = size.is_some();
        for name in
            ["save", "save-as", "copy", "adjust-size", "zoom-in", "zoom-out", "zoom-fit", "zoom-actual"]
        {
            self.set_action_enabled(name, has_doc);
        }
        self.set_action_enabled("undo", undo);
        self.set_action_enabled("redo", redo);
        self.set_action_enabled("crop", crop);
        for name in ["duplicate", "front", "back"] {
            self.set_action_enabled(name, selected);
        }

        let name = self
            .inner
            .source
            .borrow()
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| if has_doc { "Untitled".into() } else { "Scrawl".into() });
        self.inner.title.set_title(&if dirty { format!("{name} — Edited") } else { name });
        self.inner.title.set_subtitle(&size.map(|r| format!("{} × {}", r.w, r.h)).unwrap_or_default());
        self.inner.area.queue_draw();
    }

    fn crop(&self) {
        if self.with_editor(Editor::apply_crop) == Some(true) {
            self.select_tool(Tool::Select);
            self.update_content_size();
        }
        self.refresh();
    }

    fn open_dialog(&self) {
        let dialog = gtk::FileDialog::builder().title("Open Image").modal(true).build();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        for suffix in ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "heic", "avif"] {
            filter.add_suffix(suffix);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&filter));

        let weak = self.weak();
        dialog.open(Some(&self.window()), gio::Cancellable::NONE, move |result| {
            let (Some(this), Ok(file)) = (Self::from_weak(&weak), result) else { return };
            this.target_window().open_file(&file);
        });
    }

    fn paste(&self) {
        let weak = self.weak();
        self.window().clipboard().read_texture_async(gio::Cancellable::NONE, move |result| {
            let Some(this) = Self::from_weak(&weak) else { return };
            match result {
                Ok(Some(texture)) => match imageio::surface_from_texture(&texture) {
                    Ok(surface) => this.target_window().load(surface, None, None),
                    Err(err) => this.toast(&format!("Could not paste: {err}")),
                },
                _ => this.toast("The clipboard has no image"),
            }
        });
    }

    fn flattened(&self) -> Option<cairo::ImageSurface> {
        let editor = self.inner.editor.borrow();
        match render::flatten(&editor.as_ref()?.doc) {
            Ok(surface) => Some(surface),
            Err(err) => {
                self.toast(&format!("Could not render image: {err}"));
                None
            }
        }
    }

    fn copy(&self) {
        let Some(surface) = self.flattened() else { return };
        match imageio::texture_from_surface(surface) {
            Ok(texture) => {
                self.window().clipboard().set_texture(&texture);
                self.toast("Copied to clipboard");
            }
            Err(err) => self.toast(&format!("Could not copy: {err}")),
        }
    }

    fn save(&self) {
        let target = self.inner.save_target.borrow().clone();
        match target {
            Some(path) => {
                self.write_to(&path);
            }
            None => self.save_as(),
        }
    }

    fn save_as(&self) {
        self.save_as_then(|_| {});
    }

    /// Ask for a destination, write, then run `after` on success.
    fn save_as_then(&self, after: impl FnOnce(&Self) + 'static) {
        if !self.has_document() {
            return;
        }
        let source = self.inner.source.borrow().clone();
        let stem = source
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Screenshot".into());
        let dialog = gtk::FileDialog::builder()
            .title("Save Annotated Image")
            .modal(true)
            .initial_name(format!("{stem}-annotated.png"))
            .build();
        if let Some(folder) = source.as_ref().and_then(|p| p.parent()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let weak = self.weak();
        dialog.save(Some(&self.window()), gio::Cancellable::NONE, move |result| {
            let (Some(this), Ok(file)) = (Self::from_weak(&weak), result) else { return };
            let Some(path) = file.path() else { return };
            if this.write_to(&path) {
                this.inner.save_target.replace(Some(path));
                after(&this);
            }
        });
    }

    fn adjust_size(&self) {
        let Some((crop, dpi)) = self.inner.editor.borrow().as_ref().map(|ed| (ed.doc.crop, ed.doc.dpi))
        else {
            return;
        };
        // Only meaningful for an untouched file; otherwise it's a guess anyway.
        let file_size =
            self.inner.source.borrow().as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
        let info =
            resize_dialog::SizeInfo { width: crop.w, height: crop.h, dpi: dpi.unwrap_or(72.0), file_size };
        let weak = self.weak();
        resize_dialog::present(&self.window(), info, move |request| {
            let Some(this) = Self::from_weak(&weak) else { return };
            let result = this.with_editor(|ed| ed.resize(request.width, request.height, request.dpi));
            if let Some(Err(err)) = result {
                this.toast(&format!("Could not resize: {err}"));
            }
            this.update_content_size();
            this.refresh();
        });
    }

    fn write_to(&self, path: &std::path::Path) -> bool {
        let Some(surface) = self.flattened() else { return false };
        let dpi = self.inner.editor.borrow().as_ref().and_then(|ed| ed.doc.dpi);
        match imageio::save(surface, path, dpi) {
            Ok(()) => {
                self.with_editor(|ed| ed.dirty = false);
                self.refresh();
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.toast(&format!("Saved {name}"));
                true
            }
            Err(err) => {
                log::warn!("save failed: {err:#}");
                self.toast(&format!("Could not save: {err}"));
                false
            }
        }
    }

    fn toast(&self, message: &str) {
        self.inner.toasts.add_toast(adw::Toast::new(message));
    }

    // ---- window plumbing ------------------------------------------------

    fn setup_drop(&self) {
        let target = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        let weak = self.weak();
        target.connect_drop(move |_, value, _, _| {
            let (Some(this), Ok(file)) = (Self::from_weak(&weak), value.get::<gio::File>()) else {
                return false;
            };
            this.target_window().open_file(&file);
            true
        });
        self.window().add_controller(target);
    }

    fn setup_close_guard(&self) {
        let weak = self.weak();
        self.window().connect_close_request(move |window| {
            let Some(this) = Self::from_weak(&weak) else { return glib::Propagation::Proceed };
            if !this.inner.editor.borrow().as_ref().is_some_and(|ed| ed.dirty) {
                return glib::Propagation::Proceed;
            }
            let dialog = adw::AlertDialog::new(
                Some("Save Changes?"),
                Some("Your annotations will be lost if you close without saving."),
            );
            dialog.add_responses(&[("cancel", "Cancel"), ("discard", "Discard"), ("save", "Save…")]);
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("save"));
            dialog.set_close_response("cancel");
            let weak = Rc::downgrade(&this.inner);
            dialog.choose(Some(window), gio::Cancellable::NONE, move |response| {
                let Some(this) = Self::from_weak(&weak) else { return };
                match response.as_str() {
                    "discard" => {
                        this.with_editor(|ed| ed.dirty = false);
                        this.window().close();
                    }
                    "save" => this.save_as_then(|this| this.window().close()),
                    _ => {}
                }
            });
            glib::Propagation::Stop
        });
    }
}

fn icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).action_name(action).build()
}

fn dashed_outline(cr: &cairo::Context, r: Rect) {
    cr.save().ok();
    cr.set_line_width(1.0);
    cr.rectangle(r.x.floor() + 0.5, r.y.floor() + 0.5, r.w.round(), r.h.round());
    stroke_dashed(cr);
}

fn dashed_polygon(cr: &cairo::Context, points: &[(f64, f64)]) {
    cr.save().ok();
    cr.set_line_width(1.0);
    for (i, (x, y)) in points.iter().enumerate() {
        if i == 0 {
            cr.move_to(*x, *y)
        } else {
            cr.line_to(*x, *y)
        }
    }
    cr.close_path();
    stroke_dashed(cr);
}

/// White underlay plus blue dashes, visible on light and dark images.
/// Expects a `save()` by the caller; restores it.
fn stroke_dashed(cr: &cairo::Context) {
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.stroke_preserve().ok();
    cr.set_dash(&[4.0, 4.0], 0.0);
    cr.set_source_rgb(0.21, 0.52, 0.89);
    cr.stroke().ok();
    cr.restore().ok();
}

fn to_rgba(c: Color) -> gdk::RGBA {
    gdk::RGBA::new(c.r as f32, c.g as f32, c.b as f32, c.a as f32)
}

fn to_color(c: &gdk::RGBA) -> Color {
    Color::rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64)
}

/// The markup toolbar: tools on the left, attributes of the current
/// selection (or of the next shape) on the right — Preview's layout.
fn build_markup_bar() -> (gtk::Box, Controls) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("toolbar");

    let tools_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tools_box.add_css_class("linked");
    let mut tools = Vec::new();
    let mut group: Option<gtk::ToggleButton> = None;
    let mut add_toggle = |icon: &str, tooltip: &str| {
        let button = gtk::ToggleButton::builder().icon_name(icon).tooltip_text(tooltip).build();
        button.set_group(group.as_ref());
        group.get_or_insert_with(|| button.clone());
        tools_box.append(&button);
        button
    };
    for (tool, icon, tooltip) in LEADING_TOOLS {
        tools.push((tool, add_toggle(icon, tooltip)));
    }

    // Shapes: a toggle for the current shape and a ▾ that opens the palette.
    let (first_shape, first_icon, first_tooltip) = SHAPES[1];
    let shape_button = add_toggle(first_icon, first_tooltip);
    let palette = gtk::Grid::builder().row_spacing(4).column_spacing(4).build();
    let mut shape_choices = Vec::new();
    for (i, (tool, icon, tooltip)) in SHAPES.into_iter().enumerate() {
        let button = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).build();
        button.add_css_class("flat");
        palette.attach(&button, (i % 2) as i32, (i / 2) as i32, 1, 1);
        shape_choices.push((tool, button));
    }
    let shape_popover = gtk::Popover::builder().child(&palette).build();
    let shape_menu = gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text("Shapes")
        .popover(&shape_popover)
        .build();
    tools_box.append(&shape_menu);

    for (tool, icon, tooltip) in TRAILING_TOOLS {
        tools.push((tool, add_toggle(icon, tooltip)));
    }
    bar.append(&tools_box);

    let adjust_size = gtk::Button::builder()
        .icon_name("scrawl-resize-symbolic")
        .tooltip_text("Adjust Size…")
        .action_name("win.adjust-size")
        .build();
    bar.append(&adjust_size);
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let color_dialog = gtk::ColorDialog::builder().with_alpha(true).build();
    let stroke = gtk::ColorDialogButton::new(Some(color_dialog.clone()));
    stroke.set_tooltip_text(Some("Color"));
    bar.append(&stroke);

    let fill_toggle = gtk::ToggleButton::builder().label("Fill").tooltip_text("Fill shape").build();
    let fill = gtk::ColorDialogButton::new(Some(color_dialog));
    fill.set_tooltip_text(Some("Fill color"));
    fill.set_rgba(&to_rgba(Color::WHITE));
    let fill_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    fill_box.add_css_class("linked");
    fill_box.append(&fill_toggle);
    fill_box.append(&fill);
    bar.append(&fill_box);

    let width = gtk::SpinButton::with_range(1.0, 100.0, 1.0);
    width.set_tooltip_text(Some("Line thickness"));
    bar.append(&gtk::Label::new(Some("Width")));
    bar.append(&width);

    let heads = gtk::DropDown::from_strings(&HEAD_LABELS);
    heads.set_tooltip_text(Some("Arrow heads"));
    bar.append(&heads);

    let dashed = gtk::ToggleButton::builder().label("Dashed").tooltip_text("Draw line dashed").build();
    let shadow = gtk::ToggleButton::builder().label("Shadow").tooltip_text("Draw with shadow").build();
    bar.append(&dashed);
    bar.append(&shadow);

    let font_size = gtk::SpinButton::with_range(6.0, 1000.0, 1.0);
    font_size.set_tooltip_text(Some("Font size"));
    bar.append(&gtk::Label::new(Some("Size")));
    bar.append(&font_size);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    let crop = gtk::Button::builder().label("Crop").tooltip_text("Crop to selection (Ctrl+K)").build();
    crop.add_css_class("suggested-action");
    bar.append(&crop);

    let controls = Controls {
        tools,
        shape_button,
        shape_tool: Cell::new(first_shape),
        shape_popover,
        shape_choices,
        stroke,
        fill_toggle,
        fill,
        width,
        heads,
        dashed,
        shadow,
        font_size,
        crop,
    };
    (bar, controls)
}
