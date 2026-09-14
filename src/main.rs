mod css;
mod media;
mod state;
mod window;
mod zoom;

use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{self, glib};
use libadwaita as adw;

const APP_ID: &str = "com.github.carosello";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    app.connect_activate(|app| {
        let window = window::build(app, None);
        window.present();
    });

    app.connect_command_line(|app, cmd_line| {
        let args = cmd_line.arguments();
        let start = args.get(1).map(PathBuf::from);
        let window = window::build(app, start.as_deref());
        window.present();
        glib::ExitCode::SUCCESS.into()
    });

    app.run()
}
