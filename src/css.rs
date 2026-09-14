use std::path::PathBuf;

use gtk::gdk;
use gtk::gio;

pub fn load_css() {
    let provider = gtk::CssProvider::new();
    let resource_path = "/com/github/carosello/style.css";
    let mut loaded = false;

    // 1. Try GResource from installed path (flatpak/meson)
    let install_dir = PathBuf::from("/app/share/carosello");
    let grs_path = install_dir.join("carosello-resources.gresource");
    if grs_path.exists() {
        if let Ok(res) = gio::Resource::load(&grs_path) {
            gio::resources_register(&res);
            provider.load_from_resource(resource_path);
            loaded = true;
        }
    }

    // 2. Fall back to data/style.css (cargo dev builds)
    if !loaded {
        if let Ok(cwd) = std::env::current_dir() {
            let css_path = cwd.join("data").join("style.css");
            if css_path.exists() {
                provider.load_from_path(css_path.to_str().unwrap_or("data/style.css"));
                loaded = true;
            }
        }
    }

    if !loaded {
        provider.load_from_resource(resource_path);
    }

    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("Could not get default display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
