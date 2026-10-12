//! "Adjust Size…" dialog, after Preview's Image Dimensions sheet: Fit into
//! presets, width/height in several units, resolution, Scale proportionally
//! and Resample image, plus the resulting size.
//!
//! Semantics follow Preview (its binary asserts "cannot set widthInPixels
//! when _resample == NO"): with Resample off the pixel dimensions are fixed
//! and only the resolution, and therefore the print size, changes.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

pub struct SizeInfo {
    pub width: f64,
    pub height: f64,
    pub dpi: f64,
    /// Size of the file on disk, for the "(was …)" estimate.
    pub file_size: Option<u64>,
}

pub struct ResizeRequest {
    pub width: u32,
    pub height: u32,
    pub dpi: f64,
}

/// Cairo's limit on surface dimensions.
const MAX_PIXELS: f64 = 32767.0;

const PRESETS: [u32; 6] = [320, 640, 800, 1024, 1280, 1920];
const UNITS: [&str; 6] = ["pixels", "per cent", "inches", "cm", "mm", "points"];
const RES_UNITS: [&str; 2] = ["pixels/inch", "pixels/cm"];

#[derive(Clone, Copy, PartialEq)]
enum Unit {
    Pixels,
    Percent,
    Inches,
    Cm,
    Mm,
    Points,
}

impl Unit {
    fn from_index(i: u32) -> Self {
        [Self::Pixels, Self::Percent, Self::Inches, Self::Cm, Self::Mm, Self::Points][i as usize % 6]
    }

    /// Inches per one of this unit, for physical units.
    fn inches(self) -> Option<f64> {
        match self {
            Self::Inches => Some(1.0),
            Self::Cm => Some(1.0 / 2.54),
            Self::Mm => Some(1.0 / 25.4),
            Self::Points => Some(1.0 / 72.0),
            Self::Pixels | Self::Percent => None,
        }
    }

    fn digits(self) -> u32 {
        match self {
            Self::Pixels => 0,
            Self::Percent => 1,
            _ => 2,
        }
    }
}

struct State {
    info: SizeInfo,
    width: Cell<f64>,
    height: Cell<f64>,
    dpi: Cell<f64>,
    /// Set while the dialog writes its own fields, to ignore the echoes.
    updating: Cell<bool>,
}

struct Widgets {
    fit: gtk::DropDown,
    width: gtk::SpinButton,
    height: gtk::SpinButton,
    unit: gtk::DropDown,
    resolution: gtk::SpinButton,
    res_unit: gtk::DropDown,
    proportional: gtk::CheckButton,
    resample: gtk::CheckButton,
    result: gtk::Label,
    file_size: gtk::Label,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Width,
    Height,
    Resolution,
}

pub fn present(parent: &impl IsA<gtk::Widget>, info: SizeInfo, on_apply: impl Fn(ResizeRequest) + 'static) {
    let state = Rc::new(State {
        width: Cell::new(info.width),
        height: Cell::new(info.height),
        dpi: Cell::new(info.dpi),
        updating: Cell::new(false),
        info,
    });
    let w = Rc::new(build_widgets());

    let grid = gtk::Grid::builder().row_spacing(8).column_spacing(10).build();
    let label = |text: &str| gtk::Label::builder().label(text).xalign(1.0).build();
    grid.attach(&label("Fit into:"), 0, 0, 1, 1);
    grid.attach(&w.fit, 1, 0, 1, 1);
    grid.attach(&gtk::Label::builder().label("pixels").xalign(0.0).build(), 2, 0, 1, 1);
    grid.attach(&label("Width:"), 0, 1, 1, 1);
    grid.attach(&w.width, 1, 1, 1, 1);
    grid.attach(&label("Height:"), 0, 2, 1, 1);
    grid.attach(&w.height, 1, 2, 1, 1);
    grid.attach(&w.unit, 2, 1, 1, 2);
    grid.attach(&label("Resolution:"), 0, 3, 1, 1);
    grid.attach(&w.resolution, 1, 3, 1, 1);
    grid.attach(&w.res_unit, 2, 3, 1, 1);
    grid.attach(&w.proportional, 1, 4, 2, 1);
    grid.attach(&w.resample, 1, 5, 2, 1);

    let result_title = gtk::Label::builder().label("Resulting Size").xalign(0.0).build();
    result_title.add_css_class("heading");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&grid);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&result_title);
    content.append(&w.result);
    content.append(&w.file_size);

    let dialog = adw::AlertDialog::new(Some("Image Dimensions"), None);
    dialog.set_extra_child(Some(&content));
    dialog.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");

    connect(&state, &w);
    refresh(&state, &w, None);

    let state_ok = state.clone();
    dialog.choose(Some(parent), gtk::gio::Cancellable::NONE, move |response| {
        if response == "ok" {
            on_apply(ResizeRequest {
                width: state_ok.width.get().round().clamp(1.0, MAX_PIXELS) as u32,
                height: state_ok.height.get().round().clamp(1.0, MAX_PIXELS) as u32,
                dpi: state_ok.dpi.get(),
            });
        }
    });
}

fn build_widgets() -> Widgets {
    let mut fit_labels = vec!["Custom".to_owned()];
    fit_labels.extend(PRESETS.iter().map(|n| format!("{n} × {n}")));
    let fit_refs: Vec<&str> = fit_labels.iter().map(String::as_str).collect();

    let spin = || {
        let s = gtk::SpinButton::with_range(0.01, 100_000.0, 1.0);
        s.set_width_chars(9);
        s
    };
    let unit = gtk::DropDown::from_strings(&UNITS);
    unit.set_valign(gtk::Align::Center);
    unit.set_selected(1); // per cent, like Preview
    Widgets {
        fit: gtk::DropDown::from_strings(&fit_refs),
        width: spin(),
        height: spin(),
        unit,
        resolution: spin(),
        res_unit: gtk::DropDown::from_strings(&RES_UNITS),
        proportional: gtk::CheckButton::builder().label("Scale proportionally").active(true).build(),
        resample: gtk::CheckButton::builder().label("Resample image").active(true).build(),
        result: gtk::Label::builder().xalign(0.0).build(),
        file_size: gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build(),
    }
}

fn unit(w: &Widgets) -> Unit {
    Unit::from_index(w.unit.selected())
}

/// Pixel count → displayed value in the current unit.
fn to_display(state: &State, px: f64, base: f64, unit: Unit) -> f64 {
    match unit {
        Unit::Pixels => px,
        Unit::Percent => px / base * 100.0,
        physical => px / state.dpi.get() / physical.inches().expect("physical unit"),
    }
}

/// Write the model into every field except the one being edited.
fn refresh(state: &State, w: &Widgets, skip: Option<Field>) {
    state.updating.set(true);
    let u = unit(w);
    for (field, spin, px, base) in [
        (Field::Width, &w.width, state.width.get(), state.info.width),
        (Field::Height, &w.height, state.height.get(), state.info.height),
    ] {
        spin.set_digits(u.digits());
        if skip != Some(field) {
            spin.set_value(to_display(state, px, base, u));
        }
    }
    if skip != Some(Field::Resolution) {
        let per_cm = w.res_unit.selected() == 1;
        w.resolution.set_digits(if per_cm { 2 } else { 0 });
        w.resolution.set_value(if per_cm { state.dpi.get() / 2.54 } else { state.dpi.get() });
    }

    // Without resampling the pixel grid is fixed: pixel/percent fields are
    // read-only and the aspect ratio can't change.
    let resample = w.resample.is_active();
    let pixel_unit = matches!(u, Unit::Pixels | Unit::Percent);
    w.width.set_sensitive(resample || !pixel_unit);
    w.height.set_sensitive(resample || !pixel_unit);
    w.proportional.set_sensitive(resample);
    if !resample {
        w.proportional.set_active(true);
    }

    let (pw, ph) = (state.width.get().round(), state.height.get().round());
    w.result.set_label(&format!("{pw} × {ph} pixels"));
    match state.info.file_size {
        Some(bytes) => {
            let ratio = (pw * ph) / (state.info.width * state.info.height);
            let estimate = (bytes as f64 * ratio) as u64;
            w.file_size.set_label(&format!(
                "{} (was {})",
                glib::format_size(estimate),
                glib::format_size(bytes)
            ));
        }
        None => w.file_size.set_label(&format!(
            "{:.1} × {:.1} inches at {:.0} pixels/inch",
            pw / state.dpi.get(),
            ph / state.dpi.get(),
            state.dpi.get()
        )),
    }
    state.updating.set(false);
}

/// The user typed `value` into width (`is_width`) or height.
fn edit_dimension(state: &State, w: &Widgets, is_width: bool, value: f64) {
    let (base, other_base) =
        if is_width { (state.info.width, state.info.height) } else { (state.info.height, state.info.width) };
    let (this, other) = if is_width { (&state.width, &state.height) } else { (&state.height, &state.width) };
    let proportional = w.proportional.is_active();

    match unit(w).inches() {
        None => {
            // Pixels or per cent: changes the pixel count (resample is on,
            // the fields are insensitive otherwise).
            let px = if unit(w) == Unit::Percent { base * value / 100.0 } else { value };
            this.set(px);
            if proportional {
                other.set(px * other_base / base);
            }
        }
        Some(per_unit) => {
            let inches = value * per_unit;
            if w.resample.is_active() {
                this.set(inches * state.dpi.get());
                if proportional {
                    other.set(this.get() * other_base / base);
                }
            } else {
                // Same pixels over a different print size: new resolution.
                state.dpi.set(this.get() / inches.max(1e-6));
            }
        }
    }
}

fn connect(state: &Rc<State>, w: &Rc<Widgets>) {
    for is_width in [true, false] {
        let (st, wd) = (state.clone(), w.clone());
        let spin = if is_width { &w.width } else { &w.height };
        spin.connect_value_changed(move |spin| {
            if st.updating.get() {
                return;
            }
            edit_dimension(&st, &wd, is_width, spin.value());
            st.updating.set(true);
            wd.fit.set_selected(0);
            st.updating.set(false);
            refresh(&st, &wd, Some(if is_width { Field::Width } else { Field::Height }));
        });
    }

    let (st, wd) = (state.clone(), w.clone());
    w.resolution.connect_value_changed(move |spin| {
        if st.updating.get() {
            return;
        }
        let dpi = if wd.res_unit.selected() == 1 { spin.value() * 2.54 } else { spin.value() };
        if wd.resample.is_active() {
            // Keep the print size, so the pixel count follows.
            let factor = dpi / st.dpi.get();
            st.width.set(st.width.get() * factor);
            st.height.set(st.height.get() * factor);
        }
        st.dpi.set(dpi);
        refresh(&st, &wd, Some(Field::Resolution));
    });

    let (st, wd) = (state.clone(), w.clone());
    w.fit.connect_selected_notify(move |fit| {
        if st.updating.get() || fit.selected() == 0 {
            return;
        }
        let n = PRESETS[fit.selected() as usize - 1] as f64;
        let scale = (n / st.info.width).min(n / st.info.height);
        st.width.set(st.info.width * scale);
        st.height.set(st.info.height * scale);
        st.updating.set(true);
        wd.resample.set_active(true);
        st.updating.set(false);
        refresh(&st, &wd, None);
    });

    let (st, wd) = (state.clone(), w.clone());
    w.proportional.connect_toggled(move |check| {
        if st.updating.get() {
            return;
        }
        if check.is_active() {
            st.height.set(st.width.get() * st.info.height / st.info.width);
        }
        refresh(&st, &wd, None);
    });

    let (st, wd) = (state.clone(), w.clone());
    w.resample.connect_toggled(move |check| {
        if st.updating.get() {
            return;
        }
        if !check.is_active() {
            // Back to the real pixel grid, keeping the chosen resolution.
            st.width.set(st.info.width);
            st.height.set(st.info.height);
        }
        refresh(&st, &wd, None);
    });

    for dropdown in [&w.unit, &w.res_unit] {
        let (st, wd) = (state.clone(), w.clone());
        dropdown.connect_selected_notify(move |_| refresh(&st, &wd, None));
    }
}
