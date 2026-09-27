mod css;
mod media;
mod state;
mod transform;
mod window;
mod zoom;

use std::path::PathBuf;

use gtk::prelude::*;
use gtk::{self, gio, glib};
use libadwaita as adw;

use crate::state::debug_log;

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
        let args = cmd_line.arguments();
        let cwd = cmd_line.cwd();
        // A file manager expands `Exec=carosello %U` to a plain path when
        // FUSE maps it and to a `file://` URI otherwise, so the argument is
        // resolved with `File::for_commandline_arg` — the conversion GLib
        // itself uses for HANDLES_OPEN. `PathBuf::from(arg)` would keep the
        // `file://` prefix and open an empty window instead.
        let start: Option<PathBuf> = args
            .iter()
            .skip(1)
            .find(|a| !a.to_string_lossy().starts_with('-'))
            .and_then(|a| {
                let file = match &cwd {
                    Some(cwd) => gio::File::for_commandline_arg_and_cwd(a, cwd),
                    None => gio::File::for_commandline_arg(a),
                };
                file.path()
            });
        debug_log!(format!(
            "command-line: {} arg(s) -> {}",
            args.len().saturating_sub(1),
            start
                .as_ref()
                .map_or_else(|| "none".into(), |p| p.display().to_string())
        ));

        // Single-instance: re-activation raises the existing window, and a
        // path handed to us (file manager double-click, `gio open`) *replaces*
        // what it shows instead of leaving the current item up.
        if let Some(win) = app.active_window() {
            if let Some(path) = &start {
                // URI, not path: percent-encoding keeps non-UTF-8 names
                // lossless inside the `s` action parameter. The `win.` prefix
                // is how GtkApplicationWindow publishes its own action group
                // into the widget muxer (`gtk_application_window_init`).
                let uri = gio::File::for_path(path).uri();
                if let Err(err) =
                    win.activate_action("win.open-path", Some(&glib::Variant::from(uri.as_str())))
                {
                    debug_log!(format!("command-line: open-path failed: {err}"));
                }
            }
            win.present();
            return glib::ExitCode::SUCCESS.into();
        }
        let window = window::build(app, start.as_deref());
        window.present();
        glib::ExitCode::SUCCESS.into()
    });

    app.run()
}
