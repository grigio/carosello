use gtk::gdk;

pub fn load_css() {
    let provider = gtk::CssProvider::new();

    // Try GResource first (meson/flatpak install), fall back to filesystem (cargo dev)
    let resource_path = "/com/github/carosello/style.css";
    if let Some(cwd) = std::env::current_dir().ok() {
        let css_path = cwd.join("data").join("style.css");
        if css_path.exists() {
            provider.load_from_path(css_path.to_str().unwrap_or("data/style.css"));
        } else {
            provider.load_from_resource(resource_path);
        }
    } else {
        provider.load_from_resource(resource_path);
    }

    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("Could not get default display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
