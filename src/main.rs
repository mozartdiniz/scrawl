mod editor;
mod imageio;
mod model;
mod render;
mod resize_dialog;
mod sketch;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};

use window::ScrawlWindow;

pub const APP_ID: &str = "io.github.mozartdiniz.Scrawl";

fn main() -> glib::ExitCode {
    pretty_env_logger::init();

    // Icons are embedded at compile time by build.rs.
    gio::resources_register_include!("scrawl.gresource").expect("failed to register GResources");

    let app =
        adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();

    app.connect_startup(|app| {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display).add_resource_path("/io/github/mozartdiniz/Scrawl/icons");
        }
        setup_actions(app);
    });
    app.connect_activate(|app| ScrawlWindow::new(app).present());
    app.connect_open(|app, files, _| {
        for file in files {
            let window = ScrawlWindow::new(app);
            window.open_file(file);
            window.present();
        }
    });

    glib::set_application_name("Scrawl");
    app.run()
}

fn setup_actions(app: &adw::Application) {
    let new_window = gio::SimpleAction::new("new-window", None);
    new_window.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| ScrawlWindow::new(&app).present()
    ));
    app.add_action(&new_window);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            for window in app.windows() {
                window.close();
            }
        }
    ));
    app.add_action(&quit);

    for (action, accels) in [
        ("app.new-window", &["<Control>n"][..]),
        ("app.quit", &["<Control>q"]),
        ("win.open", &["<Control>o"]),
        ("win.save", &["<Control>s"]),
        ("win.save-as", &["<Control><Shift>s"]),
        ("win.copy", &["<Control>c"]),
        ("win.paste", &["<Control>v"]),
        ("win.undo", &["<Control>z"]),
        ("win.redo", &["<Control><Shift>z", "<Control>y"]),
        ("win.crop", &["<Control>k"]),
        ("win.duplicate", &["<Control>d"]),
        ("win.front", &["<Control><Shift>f"]),
        ("win.back", &["<Control><Shift>b"]),
        ("win.zoom-in", &["<Control>plus", "<Control>equal"]),
        ("win.zoom-out", &["<Control>minus"]),
        ("win.zoom-fit", &["<Control>0"]),
        ("win.zoom-actual", &["<Control>1"]),
        ("window.close", &["<Control>w"]),
    ] {
        app.set_accels_for_action(action, accels);
    }
}
