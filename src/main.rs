mod css;
mod media;
mod state;
mod window;
mod zoom;

use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{self, glib};
use libadwaita as adw;

const APP_ID: &str = "io.github.grigio.carosello";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    // App-level actions + accelerators: registered once so single-instance
    // re-activation and all windows share them (no per-window duplication).
    app.connect_startup(|app| {
        let quit = gtk::gio::SimpleAction::new("quit", None);
        let app_clone = app.clone();
        quit.connect_activate(move |_, _| app_clone.quit());
        app.add_action(&quit);

        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("win.open", &["<Control>o"]);
        app.set_accels_for_action("win.open-folder", &["<Control><Shift>o"]);
        app.set_accels_for_action("win.trash", &["Delete", "KP_Delete"]);
        app.set_accels_for_action(
            "win.shortcuts",
            &["<Control>question", "<Control>slash", "<Control>k"],
        );
        app.set_accels_for_action("win.about", &["F1"]);
        app.set_accels_for_action("win.close", &["<Control>w"]);
        app.set_accels_for_action(
            "win.zoom-in",
            &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
        );
        app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
        app.set_accels_for_action("win.zoom-reset", &["<Control>0", "<Control>KP_0"]);
    });

    app.connect_activate(|app| {
        // Single-window: D-Bus activation re-presents the existing window.
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        let window = window::build(app, None);
        window.present();
    });

    app.connect_command_line(|app, cmd_line| {
        // Single-instance: a second launch presents the existing window
        // instead of opening a duplicate.
        if let Some(win) = app.active_window() {
            win.present();
            return glib::ExitCode::SUCCESS.into();
        }
        let args = cmd_line.arguments();
        let start = args.get(1).map(PathBuf::from);
        let window = window::build(app, start.as_deref());
        window.present();
        glib::ExitCode::SUCCESS.into()
    });

    app.run()
}
