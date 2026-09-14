use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use libadwaita as adw;

use crate::css;
use crate::media;
use crate::state::{self, debug_log, format_time, is_media};
use crate::zoom::{self, ZoomPaintable};

struct AppState {
    files: Vec<PathBuf>,
    index: usize,
    original_pixbuf: Option<gdk_pixbuf::Pixbuf>,
    zoom: f64,
    media_file: Option<gtk::MediaFile>,
    video_view: Option<ZoomPaintable>,
    last_w: i32,
    last_h: i32,
    pending_update: bool,
    is_video: bool,
    seeking: bool,
    updating_seek_bar: bool,
    skip_volume_update: bool,
    video_gen: u64,
    video_w: i32,
    video_h: i32,
    mouse_x: f64,
    mouse_y: f64,
    play_pause_btn: Option<gtk::Button>,
    mute_btn: Option<gtk::Button>,
    seek_scale: Option<gtk::Scale>,
    position_label: Option<gtk::Label>,
    duration_label: Option<gtk::Label>,
    bottom_bar: Option<gtk::Box>,
    volume_scale: Option<gtk::Scale>,
    toast_overlay: Option<adw::ToastOverlay>,
}

pub fn build(app: &adw::Application, start: Option<&Path>) -> adw::ApplicationWindow {
    css::load_css();

    let state: Rc<RefCell<AppState>> = Rc::new(RefCell::new(AppState {
        files: Vec::new(),
        index: 0,
        original_pixbuf: None,
        zoom: 1.0,
        media_file: None,
        video_view: Some(ZoomPaintable::new()),
        last_w: 0,
        last_h: 0,
        pending_update: false,
        is_video: false,
        seeking: false,
        updating_seek_bar: false,
        skip_volume_update: false,
        video_gen: 0,
        video_w: 0,
        video_h: 0,
        mouse_x: 0.0,
        mouse_y: 0.0,
        play_pause_btn: None,
        mute_btn: None,
        seek_scale: None,
        position_label: None,
        duration_label: None,
        bottom_bar: None,
        volume_scale: None,
        toast_overlay: None,
    }));

    let mut start_index: usize = 0;
    {
        let mut s = state.borrow_mut();
        let (read_dir, target_file) = match start {
            Some(p) if p.is_file() => (p.parent().unwrap_or(Path::new(".")), Some(p.to_path_buf())),
            Some(p) => (p, None),
            None => (Path::new("."), None),
        };
        let mut files: Vec<PathBuf> = std::fs::read_dir(read_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_media(p))
            .collect();
        files.sort();
        if let Some(target) = target_file {
            start_index = files.iter().position(|f| f == &target).unwrap_or(0);
        }
        s.files = files;
    }

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Carosello")
        .default_width(900)
        .default_height(600)
        .css_classes(["carosello-window"])
        .build();

    let overlay = gtk::Overlay::new();

    // ── Toast overlay for error feedback ──
    let toast_overlay = adw::ToastOverlay::new();
    overlay.set_child(Some(&toast_overlay));

    // ── Zoomable viewport: ScrolledWindow + Picture ──
    let scrolled = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .has_frame(false)
        .propagate_natural_height(false)
        .propagate_natural_width(false)
        .css_classes(["carosello-scrolled"])
        .build();

    let picture = gtk::Picture::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();

    scrolled.set_child(Some(&picture));
    toast_overlay.set_child(Some(&scrolled));

    let empty_label = gtk::Label::builder()
        .label("No media files in current directory")
        .css_classes(["empty-label"])
        .build();
    overlay.add_overlay(&empty_label);

    // ── Top: AdwHeaderBar (GNOME HIG standard) ──
    let header_bar = adw::HeaderBar::builder()
        .css_classes(["flat", "titlebar", "controls-bg-top"])
        .build();

    // ── Top-left: fullscreen button ──
    let fullscreen_btn = gtk::Button::builder()
        .icon_name("view-fullscreen-symbolic")
        .tooltip_text("Toggle Fullscreen (F11)")
        .css_classes(["flat"])
        .build();
    {
        let w = window.clone();
        fullscreen_btn.connect_clicked(move |_| w.set_fullscreened(!w.is_fullscreen()));
    }
    header_bar.pack_start(&fullscreen_btn);

    // ── Top-right: zoom + menu ──
    let zoom_out_btn = gtk::Button::builder()
        .icon_name("zoom-out-symbolic")
        .tooltip_text("Zoom Out (Ctrl+-)")
        .css_classes(["flat"])
        .build();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        zoom_out_btn.connect_clicked(move |_| {
            zoom_by(&state, &picture, &scrolled, 1.0 / 1.25, None);
        });
    }
    header_bar.pack_end(&zoom_out_btn);

    let zoom_reset_btn = gtk::Button::builder()
        .icon_name("zoom-fit-best-symbolic")
        .tooltip_text("Reset Zoom (Ctrl+0)")
        .css_classes(["flat"])
        .build();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        zoom_reset_btn.connect_clicked(move |_| {
            zoom_to(&state, &picture, &scrolled, 1.0, None);
        });
    }
    header_bar.pack_end(&zoom_reset_btn);

    let zoom_in_btn = gtk::Button::builder()
        .icon_name("zoom-in-symbolic")
        .tooltip_text("Zoom In (Ctrl++)")
        .css_classes(["flat"])
        .build();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        zoom_in_btn.connect_clicked(move |_| {
            zoom_by(&state, &picture, &scrolled, 1.25, None);
        });
    }
    header_bar.pack_end(&zoom_in_btn);

    let section = gio::Menu::new();
    section.append(Some("About"), Some("win.about"));
    let quit_section = gio::Menu::new();
    quit_section.append(Some("Quit"), Some("app.quit"));
    let menu = gio::Menu::new();
    menu.append_section(None, &section);
    menu.append_section(None, &quit_section);

    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Menu (F10)")
        .css_classes(["flat"])
        .build();
    header_bar.pack_end(&menu_btn);

    // Wrap the header bar in a WindowHandle for drag support
    let top_handle = gtk::WindowHandle::new();
    top_handle.set_child(Some(&header_bar));
    top_handle.set_opacity(0.0);
    top_handle.set_valign(gtk::Align::Start);
    top_handle.set_vexpand(false);
    top_handle.add_css_class("fade-controls");
    overlay.add_overlay(&top_handle);

    // ── Bottom: video controls (Showtime-style layout) ──
    let bottom_outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::End)
        .halign(gtk::Align::Fill)
        .css_classes(["fade-controls"])
        .build();

    let bottom_controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(12)
        .halign(gtk::Align::Fill)
        .css_classes(["controls-bg-bottom", "osd"])
        .build();

    // Seek bar row: position label | scale | duration label
    let seek_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Fill)
        .build();

    let position_label = gtk::Label::builder()
        .label("0:00")
        .halign(gtk::Align::Start)
        .css_classes(["time-label"])
        .build();
    position_label.set_xalign(0.0);
    seek_row.append(&position_label);

    let seek_scale = gtk::Scale::builder()
        .hexpand(true)
        .draw_value(false)
        .css_classes(["seek-bar"])
        .value_pos(gtk::PositionType::Right)
        .build();
    seek_scale.set_range(0.0, 500.0);
    seek_scale.set_increments(1.0, 10.0);
    {
        let state = state.clone();
        let scale = seek_scale.clone();
        seek_scale.connect_value_changed(move |_| {
            let value = scale.value();
            let (media, duration, seeking, updating) = {
                let s = state.borrow_mut();
                (
                    s.media_file.clone(),
                    s.media_file.as_ref().map(|m| m.duration()).unwrap_or(0),
                    s.seeking,
                    s.updating_seek_bar,
                )
            };
            if let Some(media) = media {
                if duration > 0 && !seeking && !updating {
                    let ts = (value / 500.0 * duration as f64)
                        .max(0.0)
                        .min(duration as f64) as i64;
                    state.borrow_mut().seeking = true;
                    media.seek(ts);
                }
            }
        });
    }

    let click_gesture = gtk::GestureClick::new();
    click_gesture.set_button(1);
    {
        let state = state.clone();
        let scale = seek_scale.clone();
        click_gesture.connect_pressed(move |_, _, x, _| {
            let (media, duration) = {
                let s = state.borrow_mut();
                (
                    s.media_file.clone(),
                    s.media_file.as_ref().map(|m| m.duration()).unwrap_or(0),
                )
            };
            if let Some(media) = media {
                if duration > 0 {
                    let width = scale.width() as f64;
                    let value = (x / width.max(1.0) * 500.0).clamp(0.0, 500.0);
                    scale.set_value(value);
                    let ts = (value / 500.0 * duration as f64)
                        .max(0.0)
                        .min(duration as f64) as i64;
                    state.borrow_mut().seeking = true;
                    media.seek(ts);
                }
            }
        });
    }
    {
        let state = state.clone();
        click_gesture.connect_released(move |_, _, _, _| {
            let mut s = state.borrow_mut();
    s.seeking = false;
    s.updating_seek_bar = false;
        });
    }
    seek_scale.add_controller(click_gesture);

    // When seek bar is focused, intercept arrow keys for navigation
    // (GtkScale's built-in handler would otherwise consume them)
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let window = window.clone();
        let seek_key_ctrl = gtk::EventControllerKey::new();
        seek_key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        seek_key_ctrl.connect_key_pressed(move |_, key, _, _| {
            match key {
                gdk::Key::Left => {
                    nav(&state, &picture, &scrolled, &window, -1);
                    glib::Propagation::Stop
                }
                gdk::Key::Right => {
                    nav(&state, &picture, &scrolled, &window, 1);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        seek_scale.add_controller(seek_key_ctrl);
    }

    seek_row.append(&seek_scale);

    let duration_label = gtk::Label::builder()
        .label("0:00")
        .halign(gtk::Align::End)
        .css_classes(["time-label"])
        .build();
    duration_label.set_xalign(1.0);
    seek_row.append(&duration_label);

    // Button row: play/pause + volume + mute (centered)
    let btn_row = gtk::Box::builder()
        .spacing(4)
        .halign(gtk::Align::Center)
        .build();

    let play_pause_btn = gtk::Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Play/Pause (Space)")
        .css_classes(["ctrl-btn"])
        .build();
    {
        let state = state.clone();
        play_pause_btn.connect_clicked(move |btn| {
            let s = state.borrow();
            if let Some(ref media) = s.media_file {
                if media.is_playing() {
                    media.pause();
                    btn.set_icon_name("media-playback-start-symbolic");
                    btn.set_tooltip_text(Some("Play"));
                } else {
                    media.play();
                    btn.set_icon_name("media-playback-pause-symbolic");
                    btn.set_tooltip_text(Some("Pause"));
                }
            }
        });
    }

    // Volume slider
    let volume_scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::Center)
        .width_request(100)
        .draw_value(false)
        .css_classes(["volume-bar"])
        .build();
    volume_scale.set_range(0.0, 1.0);
    volume_scale.set_value(1.0);
    {
        let state = state.clone();
        volume_scale.connect_value_changed(move |scale| {
            let value = scale.value();
            let skip = state.borrow().skip_volume_update;
            if skip {
                return;
            }
            let media = state.borrow().media_file.clone();
            let btn = state.borrow().mute_btn.clone();
            if let Some(media) = media {
                let muted = value < 0.01;
                media.set_volume(value);
                media.set_muted(muted);
                if let Some(ref btn) = btn {
                    update_mute_button(btn, muted);
                }
            }
        });
    }

    let mute_btn = gtk::Button::builder()
        .icon_name("audio-volume-muted-symbolic")
        .tooltip_text("Mute/Unmute")
        .css_classes(["ctrl-btn"])
        .build();
    {
        let state = state.clone();
        let volume_scale = volume_scale.clone();
        mute_btn.connect_clicked(move |btn| {
            let (new_muted, media) = {
                let s = state.borrow_mut();
                if let Some(ref media) = s.media_file {
                    let new_muted = !media.is_muted();
                    media.set_muted(new_muted);
                    update_mute_button(btn, new_muted);
                    (new_muted, Some(media.clone()))
                } else {
                    return;
                }
            };
            // Set skip flag to prevent volume callback from re-entering
            state.borrow_mut().skip_volume_update = true;
            if new_muted {
                volume_scale.set_value(0.0);
            } else {
                if let Some(ref media) = media {
                    volume_scale.set_value(media.volume() as f64);
                }
            }
            state.borrow_mut().skip_volume_update = false;
        });
    }

    btn_row.append(&play_pause_btn);
    btn_row.append(&volume_scale);
    btn_row.append(&mute_btn);

    bottom_controls.append(&seek_row);
    bottom_controls.append(&btn_row);
    bottom_outer.append(&bottom_controls);

    // Plain overlay on purpose: video controls must NOT drag the window.
    bottom_outer.set_opacity(0.0);
    bottom_outer.set_visible(false);
    overlay.add_overlay(&bottom_outer);

    // Store references
    {
        let mut s = state.borrow_mut();
        s.play_pause_btn = Some(play_pause_btn);
        s.mute_btn = Some(mute_btn);
        s.seek_scale = Some(seek_scale);
        s.position_label = Some(position_label);
        s.duration_label = Some(duration_label);
        s.bottom_bar = Some(bottom_outer.clone());
        s.volume_scale = Some(volume_scale);
        s.toast_overlay = Some(toast_overlay.clone());
    }

    // ── Auto-hide + mouse tracking (Showtime-style) ──
    const FADE_DELAY_MS: u32 = 3000;
    const EDGE_THRESHOLD: f64 = 0.25;
    {
        let top = top_handle.clone();
        let bottom = bottom_outer.clone();
        let hide_id: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));

        let motion_ctrl = gtk::EventControllerMotion::new();
        {
            let top = top.clone();
            let bottom = bottom.clone();
            let hide_id = hide_id.clone();
            let state = state.clone();
            let window_ref = window.clone();
            motion_ctrl.connect_motion(move |_, x, y| {
                {
                    let mut s = state.borrow_mut();
                    s.mouse_x = x;
                    s.mouse_y = y;
                }
                let win_h = window_ref.height() as f64;
                let win_w = window_ref.width() as f64;
                let at_top = y < win_h * EDGE_THRESHOLD;
                let at_bottom = y > win_h * (1.0 - EDGE_THRESHOLD);
                let at_edge_x = x < 24.0 || x > win_w - 24.0;
                let in_top_zone = at_top || at_edge_x;
                let in_bottom_zone = at_bottom || at_edge_x;
                let in_any_zone = in_top_zone || in_bottom_zone;
                if in_any_zone {
                    if let Some(id) = hide_id.take() {
                        id.remove();
                    }
                    top.set_opacity(1.0);
                    if bottom.is_visible() {
                        bottom.set_opacity(1.0);
                    }
                }
                if !in_top_zone && !in_bottom_zone {
                    if let Some(id) = hide_id.take() {
                        id.remove();
                    }
                    let topc = top.clone();
                    let bottomc = bottom.clone();
                    let hide_id2 = hide_id.clone();
                    let sid = glib::timeout_add_local(
                        Duration::from_millis(FADE_DELAY_MS as u64),
                        move || {
                            topc.set_opacity(0.0);
                            bottomc.set_opacity(0.0);
                            hide_id2.set(None);
                            glib::ControlFlow::Break
                        },
                    );
                    hide_id.set(Some(sid));
                }
            });
        }
        overlay.add_controller(motion_ctrl);

        // Keep panels visible while the mouse hovers over them
        {
            let top = top_handle.clone();
            let hide_id = hide_id.clone();
            let motion_top = gtk::EventControllerMotion::new();
            {
                let top = top.clone();
                let hide_id = hide_id.clone();
                motion_top.connect_enter(move |_, _, _| {
                    if let Some(id) = hide_id.take() {
                        id.remove();
                    }
                    top.set_opacity(1.0);
                });
            }
            {
                let top = top.clone();
                motion_top.connect_motion(move |_, _, _| {
                    top.set_opacity(1.0);
                });
            }
            {
                let top = top.clone();
                let bottom = bottom_outer.clone();
                let hide_id = hide_id.clone();
                motion_top.connect_leave(move |_| {
                    if let Some(id) = hide_id.take() {
                        id.remove();
                    }
                    let topc = top.clone();
                    let bottomc = bottom.clone();
                    let hide_id2 = hide_id.clone();
                    let sid = glib::timeout_add_local(
                        Duration::from_millis(FADE_DELAY_MS as u64),
                        move || {
                            topc.set_opacity(0.0);
                            bottomc.set_opacity(0.0);
                            hide_id2.set(None);
                            glib::ControlFlow::Break
                        },
                    );
                    hide_id.set(Some(sid));
                });
            }
            top_handle.add_controller(motion_top);
        }
        {
            let bottom = bottom_outer.clone();
            let hide_id = hide_id.clone();
            let motion_bottom = gtk::EventControllerMotion::new();
            {
                let bottom = bottom.clone();
                let hide_id = hide_id.clone();
                motion_bottom.connect_enter(move |_, _, _| {
                    if let Some(id) = hide_id.take() {
                        id.remove();
                    }
                    bottom.set_opacity(1.0);
                });
            }
            {
                let bottom = bottom.clone();
                motion_bottom.connect_motion(move |_, _, _| {
                    bottom.set_opacity(1.0);
                });
            }
            {
                let bottom = bottom.clone();
                let top = top_handle.clone();
                let hide_id = hide_id.clone();
                motion_bottom.connect_leave(move |_| {
                    if let Some(id) = hide_id.take() {
                        id.remove();
                    }
                    let topc = top.clone();
                    let bottomc = bottom.clone();
                    let hide_id2 = hide_id.clone();
                    let sid = glib::timeout_add_local(
                        Duration::from_millis(FADE_DELAY_MS as u64),
                        move || {
                            topc.set_opacity(0.0);
                            bottomc.set_opacity(0.0);
                            hide_id2.set(None);
                            glib::ControlFlow::Break
                        },
                    );
                    hide_id.set(Some(sid));
                });
            }
            bottom_outer.add_controller(motion_bottom);
        }

        // When mouse leaves the window, fade out both panels quickly
        let motion_leave = gtk::EventControllerMotion::new();
        {
            motion_leave.connect_leave(move |_| {
                top.set_opacity(0.0);
                bottom.set_opacity(0.0);
            });
        }
        window.add_controller(motion_leave);
    }

    window.set_content(Some(&overlay));

    // ── Actions ──
    let about_action = gio::SimpleAction::new("about", None);
    {
        let w = window.clone();
        about_action.connect_activate(move |_, _| {
            let about = adw::AboutDialog::builder()
                .application_name("Carosello")
                .application_icon("image-x-generic")
                .developer_name("Carosello Contributors")
                .version("0.1.0")
                .copyright("© 2026 Carosello Contributors")
                .license_type(gtk::License::Gpl30)
                .website("https://github.com/grigio/carosello")
                .issue_url("https://github.com/grigio/carosello/issues")
                .developers(vec!["Carosello Contributors"])
                .build();
            about.present(Some(&w));
        });
    }
    window.add_action(&about_action);

    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let app_clone = app.clone();
        quit_action.connect_activate(move |_, _| app_clone.quit());
    }
    window.add_action(&quit_action);

    let close_action = gio::SimpleAction::new("close", None);
    {
        let w = window.clone();
        close_action.connect_activate(move |_, _| w.close());
    }
    window.add_action(&close_action);

    let has_files = !state.borrow().files.is_empty();
    scrolled.set_visible(has_files);
    empty_label.set_visible(!has_files);

    if has_files {
        state.borrow_mut().index = start_index;
        show_file(&state, &picture, &scrolled, &window);
    }

    // ── Keyboard (GNOME HIG standard shortcuts) ──
    let key_ctrl = gtk::EventControllerKey::new();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let window = window.clone();
        key_ctrl.connect_key_pressed(move |_, key, _, modifier| match key {
            // Navigation
            gdk::Key::Left => {
                nav(&state, &picture, &scrolled, &window, -1);
                glib::Propagation::Stop
            }
            gdk::Key::Right => {
                nav(&state, &picture, &scrolled, &window, 1);
                glib::Propagation::Stop
            }
            // Fullscreen
            gdk::Key::F11 | gdk::Key::f => {
                window.set_fullscreened(!window.is_fullscreen());
                glib::Propagation::Stop
            }
            // Zoom: Ctrl++ / Ctrl+- / Ctrl+0 (HIG standard)
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => {
                zoom_by(&state, &picture, &scrolled, 1.25, None);
                glib::Propagation::Stop
            }
            gdk::Key::minus | gdk::Key::underscore | gdk::Key::KP_Subtract => {
                zoom_by(&state, &picture, &scrolled, 1.0 / 1.25, None);
                glib::Propagation::Stop
            }
            gdk::Key::_0 | gdk::Key::KP_0 => {
                zoom_to(&state, &picture, &scrolled, 1.0, None);
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                let z = state.borrow().zoom;
                if (z - 1.0).abs() > 0.01 {
                    zoom_to(&state, &picture, &scrolled, 1.0, None);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            // Quit: Ctrl+Q
            gdk::Key::q if modifier.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                let app = window.application().expect("Window has no application");
                app.quit();
                glib::Propagation::Stop
            }
            // Close: Ctrl+W
            gdk::Key::w if modifier.contains(gtk::gdk::ModifierType::CONTROL_MASK) => {
                window.close();
                glib::Propagation::Stop
            }
            // Help: F1
            gdk::Key::F1 => {
                // Trigger the about action
                if let Some(action) = window.lookup_action("about") {
                    action.activate(None);
                }
                glib::Propagation::Stop
            }
            // Mute toggle
            gdk::Key::space => {
                let (new_muted, media, btn, scale) = {
                    let s = state.borrow();
                    let media = s.media_file.clone();
                    let btn = s.mute_btn.clone();
                    let scale = s.volume_scale.clone();
                    if let Some(ref media) = media {
                        let new_muted = !media.is_muted();
                        (new_muted, Some(media.clone()), btn, scale)
                    } else {
                        return glib::Propagation::Stop;
                    }
                };
                if let Some(ref media) = media {
                    media.set_muted(new_muted);
                }
                if let Some(ref btn) = btn {
                    update_mute_button(btn, new_muted);
                }
                state.borrow_mut().skip_volume_update = true;
                if let Some(ref scale) = scale {
                    if new_muted {
                        scale.set_value(0.0);
                    } else {
                        if let Some(ref media) = media {
                            scale.set_value(media.volume() as f64);
                        }
                    }
                }
                state.borrow_mut().skip_volume_update = false;
                glib::Propagation::Stop
            }
            // Video seeking: [ (backward 5s) / ] (forward 5s)
            gdk::Key::bracketleft => {
                let s = state.borrow();
                if let Some(ref media) = s.media_file {
                    if s.is_video {
                        let ts = media.timestamp();
                        let seek_to = (ts - 5_000_000).max(0);
                        media.seek(seek_to);
                    }
                }
                glib::Propagation::Stop
            }
            gdk::Key::k => {
                let s = state.borrow();
                if let Some(ref media) = s.media_file {
                    if s.is_video {
                        if media.is_playing() {
                            media.pause();
                            if let Some(ref btn) = s.play_pause_btn {
                                btn.set_icon_name("media-playback-start-symbolic");
                                btn.set_tooltip_text(Some("Play"));
                            }
                        } else {
                            media.play();
                            if let Some(ref btn) = s.play_pause_btn {
                                btn.set_icon_name("media-playback-pause-symbolic");
                                btn.set_tooltip_text(Some("Pause"));
                            }
                        }
                    }
                }
                glib::Propagation::Stop
            }
            gdk::Key::bracketright => {
                let s = state.borrow();
                if let Some(ref media) = s.media_file {
                    if s.is_video {
                        let ts = media.timestamp();
                        let dur = media.duration();
                        let seek_to = (ts + 5_000_000).min(dur);
                        media.seek(seek_to);
                    }
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
    }
    window.add_controller(key_ctrl);

    // Let Left/Right arrow keys propagate through ScrolledWindow for navigation
    let scrolled_key_ctrl = gtk::EventControllerKey::new();
    scrolled_key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    scrolled_key_ctrl.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Left || key == gdk::Key::Right {
            return glib::Propagation::Proceed;
        }
        glib::Propagation::Proceed
    });
    scrolled.add_controller(scrolled_key_ctrl);

    // ── Scroll: when zoomed → pan; when fit → horizontal navigates ──
    let scroll_ctrl = gtk::EventControllerScroll::builder()
        .flags(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::HORIZONTAL,
        )
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    {
        let state = state.clone();
        let scrolled = scrolled.clone();
        scroll_ctrl.connect_scroll(move |_, dx, dy| {
            let has_content = {
                let s = state.borrow();
                s.original_pixbuf.is_some() || s.media_file.is_some()
            };
            if !has_content {
                return glib::Propagation::Proceed;
            }
            let zoomed = state.borrow().zoom > 1.05;

            if zoomed {
                let scrolled = scrolled.clone();
                glib::idle_add_local_once(move || {
                    let hadj = scrolled.hadjustment();
                    let vadj = scrolled.vadjustment();
                    hadj.set_value((hadj.value() + dx * 20.0).clamp(
                        hadj.lower(),
                        (hadj.upper() - hadj.page_size()).max(hadj.lower()),
                    ));
                    vadj.set_value((vadj.value() + dy * 20.0).clamp(
                        vadj.lower(),
                        (vadj.upper() - vadj.page_size()).max(vadj.lower()),
                    ));
                });
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        });
    }
    scrolled.add_controller(scroll_ctrl);

    // ── Pinch-to-zoom ──
    let zoom_gesture = gtk::GestureZoom::new();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let base_zoom: Rc<Cell<f64>> = Rc::new(Cell::new(1.0));
        {
            let state = state.clone();
            let base_zoom = base_zoom.clone();
            zoom_gesture.connect_begin(move |_, _| {
                base_zoom.set(state.borrow().zoom);
            });
        }
        {
            let state = state.clone();
            let picture = picture.clone();
            let scrolled = scrolled.clone();
            let base_zoom = base_zoom.clone();
            zoom_gesture.connect_scale_changed(move |gesture, scale| {
                let target = (base_zoom.get() * scale).clamp(state::ZOOM_MIN, state::ZOOM_MAX);
                let anchor = if gesture.device().map_or(false, |d| d.has_cursor()) {
                    let s = state.borrow();
                    Some((s.mouse_x, s.mouse_y))
                } else {
                    gesture.point(None)
                };
                zoom_to(&state, &picture, &scrolled, target, anchor);
            });
        }
    }
    scrolled.add_controller(zoom_gesture);

    // ── Drag-to-pan when zoomed ──
    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let start_h: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
        let start_v: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
        {
            let scrolled = scrolled.clone();
            let start_h = start_h.clone();
            let start_v = start_v.clone();
            let state = state.clone();
            drag.connect_drag_begin(move |_, _, _| {
                if state.borrow().zoom <= 1.05 {
                    return;
                }
                start_h.set(scrolled.hadjustment().value());
                start_v.set(scrolled.vadjustment().value());
            });
        }
        {
            let scrolled = scrolled.clone();
            let start_h = start_h.clone();
            let start_v = start_v.clone();
            let state = state.clone();
            drag.connect_drag_update(move |_, off_x, off_y| {
                if state.borrow().zoom <= 1.05 {
                    return;
                }
                let hadj = scrolled.hadjustment();
                let vadj = scrolled.vadjustment();
                let nx = (start_h.get() - off_x).clamp(
                    hadj.lower(),
                    (hadj.upper() - hadj.page_size()).max(hadj.lower()),
                );
                let ny = (start_v.get() - off_y).clamp(
                    vadj.lower(),
                    (vadj.upper() - vadj.page_size()).max(vadj.lower()),
                );
                hadj.set_value(nx);
                vadj.set_value(ny);
            });
        }
        scrolled.add_controller(drag);
    }

    // ── Double-click toggles fit / 2.5x ──
    {
        let click = gtk::GestureClick::new();
        click.set_button(1);
        let state = state.clone();
        let picture_c = picture.clone();
        let scrolled_c = scrolled.clone();
        click.connect_pressed(move |_, n_press, x, y| {
            if n_press == 2 {
                let cur = state.borrow().zoom;
                if cur > 1.5 {
                    zoom_to(&state, &picture_c, &scrolled_c, 1.0, None);
                } else {
                    let pt = gtk::graphene::Point::new(x as f32, y as f32);
                    if let Some(conv) = picture_c.compute_point(&scrolled_c, &pt) {
                        zoom_to(
                            &state,
                            &picture_c,
                            &scrolled_c,
                            2.5,
                            Some((conv.x() as f64, conv.y() as f64)),
                        );
                    }
                }
            }
        });
        picture.add_controller(click);
    }

    // ── 3-finger swipe: prev/next item ──
    {
        let swipe = gtk::GestureSwipe::builder().n_points(3).build();
        let state = state.clone();
        let picture = picture.clone();
        let scrolled_s = scrolled.clone();
        let window_w = window.clone();
        swipe.connect_swipe(move |_, vx, vy| {
            if vx.abs() > vy.abs() && vx.abs() > 0.3 {
                if vx < 0.0 {
                    nav(&state, &picture, &scrolled_s, &window_w, 1);
                } else {
                    nav(&state, &picture, &scrolled_s, &window_w, -1);
                }
            }
        });
        scrolled.add_controller(swipe);
    }

    // ── Initial display ──
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        window.connect_realize(move |_| {
            let state = state.clone();
            let picture = picture.clone();
            let scrolled = scrolled.clone();
            glib::idle_add_local_once(move || update_display(&state, &picture, &scrolled));
        });
    }

    // ── Resize handling (viewport-driven) ──
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled_w = scrolled.clone();
        let window = window.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || {
            let mut s = state.borrow_mut();
            let w = scrolled_w.width();
            let h = scrolled_w.height();
            let (w, h) = if w <= 1 || h <= 1 {
                (window.width(), window.height())
            } else {
                (w, h)
            };
            let changed = s.last_w != w || s.last_h != h;
            if changed {
                s.last_w = w;
                s.last_h = h;
            }
            if !s.pending_update && !changed {
                return glib::ControlFlow::Continue;
            }
            s.pending_update = false;
            drop(s);
            update_display(&state, &picture, &scrolled_w);
            glib::ControlFlow::Continue
        });
    }

    window
}

fn update_mute_button(btn: &gtk::Button, muted: bool) {
    if muted {
        btn.set_icon_name("audio-volume-muted-symbolic");
        btn.set_tooltip_text(Some("Unmute"));
    } else {
        btn.set_icon_name("audio-volume-high-symbolic");
        btn.set_tooltip_text(Some("Mute"));
    }
}

fn schedule_update(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
) {
    {
        let mut s = state.borrow_mut();
        if s.pending_update {
            return;
        }
        s.pending_update = true;
    }
    let state = state.clone();
    let picture = picture.clone();
    let scrolled = scrolled.clone();
    glib::idle_add_local_once(move || {
        state.borrow_mut().pending_update = false;
        update_display(&state, &picture, &scrolled);
    });
}

fn zoom_by(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    factor: f64,
    anchor: Option<(f64, f64)>,
) {
    if !factor.is_finite() || (factor - 1.0).abs() < 0.0001 {
        return;
    }
    let cur = state.borrow().zoom;
    zoom_to(state, picture, scrolled, state::clamp_zoom(cur * factor), anchor);
}

fn zoom_to(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    new_zoom: f64,
    anchor: Option<(f64, f64)>,
) {
    let new_zoom = state::clamp_zoom(new_zoom);
    let (old_zoom, intrinsic, viewport) = {
        let s = state.borrow();
        if s.original_pixbuf.is_none() && s.media_file.is_none() {
            return;
        }
        let iw = s.original_pixbuf.as_ref().map(|pb| (pb.width().max(1) as f64, pb.height().max(1) as f64));
        let vid = if s.is_video {
            if s.video_w > 0 && s.video_h > 0 {
                Some((s.video_w as f64, s.video_h as f64))
            } else if let Some(ref media) = s.media_file {
                zoom::video_intrinsic(media)
            } else {
                None
            }
        } else {
            None
        };
        let intrinsic = if s.is_video { vid } else { iw };
        (s.zoom, intrinsic, {
            let w = scrolled.width() as f64;
            let h = scrolled.height() as f64;
            (w.max(1.0), h.max(1.0))
        })
    };
    if (new_zoom - old_zoom).abs() < 0.0001 {
        return;
    }
    let Some((iw, ih)) = intrinsic else {
        state.borrow_mut().zoom = new_zoom;
        schedule_update(state, picture, scrolled);
        return;
    };
    let (vw, vh) = viewport;
    let (old_dw, old_dh) = zoom::display_size_for(iw, ih, vw, vh, old_zoom);
    let (new_dw, new_dh) = zoom::display_size_for(iw, ih, vw, vh, new_zoom);
    let ratio_x = new_dw as f64 / old_dw.max(1) as f64;
    let ratio_y = new_dh as f64 / old_dh.max(1) as f64;

    let (ax, ay) = anchor.unwrap_or((vw / 2.0, vh / 2.0));
    let hadj = scrolled.hadjustment();
    let vadj = scrolled.vadjustment();
    let old_hv = hadj.value();
    let old_vv = vadj.value();

    state.borrow_mut().zoom = new_zoom;
    schedule_update(state, picture, scrolled);

    let scrolled_c = scrolled.clone();
    glib::timeout_add_local_once(Duration::from_millis(10), move || {
        let hadj = scrolled_c.hadjustment();
        let vadj = scrolled_c.vadjustment();
        let new_hv = (old_hv + ax) * ratio_x - ax;
        let new_vv = (old_vv + ay) * ratio_y - ay;
        let h_max = (new_dw as f64 - vw).max(0.0);
        let v_max = (new_dh as f64 - vh).max(0.0);
        if h_max > 0.0 {
            hadj.set_value(new_hv.clamp(0.0, h_max));
        }
        if v_max > 0.0 {
            vadj.set_value(new_vv.clamp(0.0, v_max));
        }
    });
}

fn nav(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
    dir: i32,
) {
    let new_index = {
        let s = state.borrow();
        let idx = s.index as i32 + dir;
        if idx >= 0 && (idx as usize) < s.files.len() {
            idx as usize
        } else {
            return;
        }
    };
    {
        let mut s = state.borrow_mut();
        s.index = new_index;
        s.zoom = 1.0;
    }
    scrolled.hadjustment().set_value(0.0);
    scrolled.vadjustment().set_value(0.0);
    show_file(state, picture, scrolled, window);
}

fn show_file(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
) {
    let (path, idx, total) = {
        let s = state.borrow();
        if s.files.is_empty() {
            return;
        }
        (s.files[s.index].clone(), s.index, s.files.len())
    };

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    window.set_title(Some(&format!("{} — {}/{}", name, idx + 1, total)));

    if state::has_ext(&path, state::VIDEO_EXTS) {
        show_video(state, picture, scrolled, &path);
    } else {
        show_image(state, picture, scrolled, &path);
    }
}

fn show_image(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    path: &Path,
) {
    let mut s = state.borrow_mut();
    if let Some(ref old) = s.media_file {
        old.pause();
    }
    s.video_gen = s.video_gen.wrapping_add(1);
    s.media_file = None;
    s.zoom = 1.0;
    s.is_video = false;

    if let Some(ref h) = s.bottom_bar {
        h.set_visible(false);
    }

    match gdk_pixbuf::Pixbuf::from_file(path) {
        Ok(pixbuf) => {
            let orientation = media::read_exif_orientation(path);
            let pixbuf = media::apply_orientation(&pixbuf, orientation);
            let tex = media::texture_for_pixbuf(&pixbuf);
            s.original_pixbuf = Some(pixbuf);
            drop(s);
            scrolled.set_child(Some(picture));
            picture.set_paintable(Some(&tex));
            picture.set_content_fit(gtk::ContentFit::Contain);
            scrolled.hadjustment().set_value(0.0);
            scrolled.vadjustment().set_value(0.0);
            schedule_update(state, picture, scrolled);
        }
        Err(e) => {
            debug_log(&format!("Failed to load: {}", e));
            s.original_pixbuf = None;
            let none: Option<&gdk::Texture> = None;
            picture.set_paintable(none);
            // Show toast on error (GNOME HIG feedback pattern)
            if let Some(ref overlay) = s.toast_overlay {
                let toast = adw::Toast::new(&format!("Failed to load {}", path.file_name().unwrap_or_default().to_string_lossy()));
                toast.set_timeout(3);
                overlay.add_toast(toast);
            }
        }
    }
}

fn update_display(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
) {
    let (zoom, is_video) = {
        let s = state.borrow();
        (state::clamp_zoom(s.zoom), s.is_video)
    };

    let s = state.borrow();
    let Some((iw, ih)) = zoom::intrinsic_size(
        is_video,
        s.video_w,
        s.video_h,
        &s.original_pixbuf,
        &s.media_file,
    ) else {
        debug_log(&format!(
            "update_display: intrinsic unknown (is_video={})",
            is_video
        ));
        return;
    };
    drop(s);

    if is_video {
        let mut s = state.borrow_mut();
        if s.video_w <= 0 || s.video_h <= 0 {
            s.video_w = iw as i32;
            s.video_h = ih as i32;
        }
    }
    let vw = scrolled.width() as f64;
    let vh = scrolled.height() as f64;
    if vw < 1.0 || vh < 1.0 {
        return;
    }

    let (dw, dh) = zoom::display_size_for(iw, ih, vw, vh, zoom);
    debug_log(&format!(
        "update_display: is_video={is_video} intrinsic={iw:.0}x{ih:.0} viewport={vw:.0}x{vh:.0} zoom={zoom:.2} display={dw}x{dh}"
    ));
    if is_video {
        let zp = state.borrow().video_view.clone();
        if let Some(ref zp) = zp {
            let (fw, fh) = zoom::fit_size(iw, ih, vw, vh);
            zp.set_view(fw, fh, zoom);
        }
        picture.set_content_fit(gtk::ContentFit::Fill);
        picture.set_size_request(dw, dh);
        if picture.paintable().is_none() {
            if let Some(ref zp) = state.borrow().video_view.clone() {
                let zp_paintable: gdk::Paintable = zp.clone().upcast();
                picture.set_paintable(Some(&zp_paintable));
            }
        }
    } else {
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_size_request(dw, dh);
        if picture.paintable().is_none() {
            if let Some(tex) = state
                .borrow()
                .original_pixbuf
                .as_ref()
                .map(media::texture_for_pixbuf)
            {
                picture.set_paintable(Some(&tex));
            }
        }
    }
}

fn show_video(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    path: &Path,
) {
    let mut s = state.borrow_mut();
    if let Some(ref old) = s.media_file {
        old.pause();
    }
    s.original_pixbuf = None;
    s.zoom = 1.0;
    s.is_video = true;
    s.seeking = false;
    s.video_gen = s.video_gen.wrapping_add(1);
    s.video_w = 0;
    s.video_h = 0;
    let gen = s.video_gen;

    // Reset play/pause button
    if let Some(ref btn) = s.play_pause_btn {
        btn.set_icon_name("media-playback-pause-symbolic");
        btn.set_tooltip_text(Some("Pause"));
    }
    // Reset mute button and volume scale
    if let Some(ref btn) = s.mute_btn {
        update_mute_button(btn, true);
    }
    let volume_scale = s.volume_scale.clone();
    drop(s);

    state.borrow_mut().skip_volume_update = true;
    if let Some(ref scale) = volume_scale {
        scale.set_value(1.0);
    }
    state.borrow_mut().skip_volume_update = false;

    // Show video controls
    {
        let s = state.borrow_mut();
        if let Some(ref h) = s.bottom_bar {
            h.set_visible(true);
        }
    }

    let media = gtk::MediaFile::for_filename(path);
    media.set_loop(true);
    media.set_muted(true);
    media.set_volume(1.0);
    media.play();

    state.borrow_mut().media_file = Some(media.clone());

    let zp = state.borrow().video_view.clone();
    if let Some(ref zp) = zp {
        zp.set_inner(Some(media.clone().upcast()));
    }
    scrolled.set_child(Some(picture));
    picture.set_content_fit(gtk::ContentFit::Fill);
    if let Some(ref zp) = zp {
        let zp_paintable: gdk::Paintable = zp.clone().upcast();
        picture.set_paintable(Some(&zp_paintable));
    }
    scrolled.hadjustment().set_value(0.0);
    scrolled.vadjustment().set_value(0.0);
    schedule_update(state, picture, scrolled);

    // When the video's intrinsic size becomes known, re-fit.
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        media.connect_invalidate_size(move |_| {
            debug_log("video: invalidate-size");
            schedule_update(&state, &picture, &scrolled);
        });
    }

    // TEMP-DIAG hook (removed before release): programmatic zoom.
    if std::env::var("CAROSELLO_DEBUG_ZOOM").is_ok() {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        glib::timeout_add_local_once(Duration::from_millis(2500), move || {
            zoom_to(&state, &picture, &scrolled, 2.5, None);
        });
    }

    // Seek bar update timer (generation-guarded so old timers die on nav).
    let state_clone = state.clone();
    let picture_clone = picture.clone();
    let scrolled_clone = scrolled.clone();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        // Size poll
        let size_changed = {
            let (media_opt, stored, gen_now, is_vid) = {
                let s = state_clone.borrow();
                (
                    s.media_file.clone(),
                    (s.video_w, s.video_h),
                    s.video_gen,
                    s.is_video,
                )
            };
            if gen_now != gen || !is_vid {
                return glib::ControlFlow::Break;
            }
            match media_opt.as_ref().and_then(zoom::video_intrinsic) {
                Some((w, h)) => {
                    let (wi, hi) = (w as i32, h as i32);
                    if (wi, hi) != stored {
                        let mut s = state_clone.borrow_mut();
                        if s.video_gen == gen && s.is_video {
                            s.video_w = wi;
                            s.video_h = hi;
                            debug_log(&format!(
                                "video: size discovered {wi}x{hi} prepared={}",
                                media_opt.as_ref().map(|m| m.is_prepared()).unwrap_or(false)
                            ));
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }
                None => false,
            }
        };
        if size_changed {
            schedule_update(&state_clone, &picture_clone, &scrolled_clone);
        }
        let (
            media_opt,
            seeking,
            seek_scale_opt,
            position_label_opt,
            duration_label_opt,
            play_pause_btn_opt,
            video_gen_now,
            is_vid,
        ) = {
            let s = state_clone.borrow();
            (
                s.media_file.clone(),
                s.seeking,
                s.seek_scale.clone(),
                s.position_label.clone(),
                s.duration_label.clone(),
                s.play_pause_btn.clone(),
                s.video_gen,
                s.is_video,
            )
        };
        if video_gen_now != gen || !is_vid {
            return glib::ControlFlow::Break;
        }
        if let Some(media) = media_opt {
            let ts = media.timestamp();
            let dur = media.duration();
            if !seeking {
                if let Some(ref scale) = seek_scale_opt {
                    if dur > 0 {
                        state_clone.borrow_mut().updating_seek_bar = true;
                        scale.set_value((ts as f64 / dur as f64) * 500.0);
                        state_clone.borrow_mut().updating_seek_bar = false;
                    }
                }
            }
            if let Some(ref label) = position_label_opt {
                label.set_text(&format_time(ts));
            }
            if let Some(ref label) = duration_label_opt {
                if dur > 0 {
                    label.set_text(&format_time(dur));
                }
            }
            if let Some(ref btn) = play_pause_btn_opt {
                if media.is_playing() {
                    btn.set_icon_name("media-playback-pause-symbolic");
                } else {
                    btn.set_icon_name("media-playback-start-symbolic");
                }
            }
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}
