use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
    image_w: i32,
    image_h: i32,
    image_gen: u64,
    prefetch: HashMap<PathBuf, gdk_pixbuf::Pixbuf>,
    zoom: f64,
    media_file: Option<gtk::MediaFile>,
    video_view: ZoomPaintable,
    pending_update: bool,
    is_video: bool,
    seeking: bool,
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
    window_title: Option<adw::WindowTitle>,
    empty_status: Option<adw::StatusPage>,
    main_overlay: Option<gtk::Overlay>,
    slide_fixed: Option<gtk::Fixed>,
    slide_gen: u64,
    sliding: bool,
    /// End timestamp (monotonic µs) of the last locked touchpad drag, so
    /// the discrete swipe signal of the same gesture isn't double-counted.
    last_drag_us: i64,
    /// Active interactive touchpad drag (finger down, frames following).
    drag: Option<DragSt>,
    /// User pref: cross-slide between items (Preferences… switch).
    slide_enabled: bool,
    /// User pref: two-finger swipe navigates (instead of three-finger).
    /// Off by default; touchpad drags and discrete swipes gate on it.
    two_finger_swipe: bool,
}

/// One interactive swipe drag: the outgoing frame plus the incoming
/// frame (once locked and built) travel with the finger on the stage.
struct DragSt {
    stage: gtk::Fixed,
    old_pic: gtk::Picture,
    new_pic: Option<gtk::Picture>,
    obx: f64,
    oby: f64,
    nbx: f64,
    nby: f64,
    w: i32,
    h: i32,
    vw: f64,
    vh: f64,
    dir: i32,
    new_index: usize,
    /// Raw accumulated finger travel (px, left negative).
    accum: f64,
    /// Current clamped stage offset applied to both frames.
    ox: f64,
    locked: bool,
    /// No sibling in the locked direction: drag with resistance, release
    /// always snaps back.
    at_edge: bool,
    /// Incoming frame placed and pre-scaled (images at lock, videos when
    /// the pipeline reports a size). Without it the drag only tracks the
    /// outgoing frame and the release cuts instantly.
    visual: bool,
    ready: bool,
    is_video: bool,
    /// Recent (monotonic µs, accumulated px) samples for fling velocity.
    samples: Vec<(i64, f64)>,
    gen: u64,
}

pub fn build(app: &adw::Application, start: Option<&Path>) -> adw::ApplicationWindow {
    css::load_css();

    let state: Rc<RefCell<AppState>> = Rc::new(RefCell::new(AppState {
        files: Vec::new(),
        index: 0,
        original_pixbuf: None,
        image_w: 0,
        image_h: 0,
        image_gen: 0,
        prefetch: HashMap::new(),
        zoom: 1.0,
        media_file: None,
        video_view: ZoomPaintable::new(),
        pending_update: false,
        is_video: false,
        seeking: false,
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
        window_title: None,
        empty_status: None,
        main_overlay: None,
        slide_fixed: None,
        slide_gen: 0,
        sliding: false,
        last_drag_us: 0,
        drag: None,
        slide_enabled: state::load_slide_enabled(),
        two_finger_swipe: state::load_two_finger_swipe(),
    }));

    let mut start_index: usize = 0;
    {
        let mut s = state.borrow_mut();
        let (read_dir, target_file) = match start {
            Some(p) if p.is_file() => (p.parent().unwrap_or(Path::new(".")), Some(p.to_path_buf())),
            Some(p) => (p, None),
            None => (Path::new("."), None),
        };
        let files = state::collect_media(read_dir);
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
    scrolled.update_property(&[gtk::accessible::Property::Label("Image and video view")]);
    scrolled.set_accessible_role(gtk::AccessibleRole::Group);

    let picture = gtk::Picture::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();

    scrolled.set_child(Some(&picture));
    toast_overlay.set_child(Some(&scrolled));

    let empty_status = adw::StatusPage::builder()
        .icon_name("system-file-manager-symbolic")
        .title("No Images Found")
        .description("Drag an image or video here or open a file")
        .build();
    empty_status.add_css_class("compact");
    let open_btn_status = gtk::Button::builder()
        .label("Open…")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    empty_status.set_child(Some(&open_btn_status));
    {
        let w = window.clone();
        open_btn_status.connect_clicked(move |_| {
            gio::prelude::ActionGroupExt::activate_action(&w, "open", None);
        });
    }
    overlay.add_overlay(&empty_status);

    // ── Top: AdwHeaderBar with AdwWindowTitle (GNOME HIG) ──
    let window_title = adw::WindowTitle::builder().title("Carosello").build();
    let header_bar = adw::HeaderBar::builder()
        .title_widget(&window_title)
        .css_classes(["flat", "titlebar", "controls-bg-top", "osd"])
        .build();
    state.borrow_mut().window_title = Some(window_title.clone());
    state.borrow_mut().empty_status = Some(empty_status.clone());

    // ── Top-left: fullscreen button ──
    let fullscreen_btn = gtk::Button::builder()
        .icon_name("view-fullscreen-symbolic")
        .tooltip_text("Toggle Fullscreen (F11)")
        .css_classes(["flat"])
        .build();
    fullscreen_btn.update_property(&[gtk::accessible::Property::Label("Toggle Fullscreen")]);
    fullscreen_btn.set_accessible_role(gtk::AccessibleRole::Button);
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
    zoom_out_btn.update_property(&[gtk::accessible::Property::Label("Zoom Out")]);
    zoom_out_btn.set_accessible_role(gtk::AccessibleRole::Button);
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
    zoom_reset_btn.update_property(&[gtk::accessible::Property::Label("Reset Zoom")]);
    zoom_reset_btn.set_accessible_role(gtk::AccessibleRole::Button);
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
    zoom_in_btn.update_property(&[gtk::accessible::Property::Label("Zoom In")]);
    zoom_in_btn.set_accessible_role(gtk::AccessibleRole::Button);
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
    section.append(Some("Open…"), Some("win.open"));
    section.append(Some("Open Folder…"), Some("win.open-folder"));
    section.append(Some("Move to Trash"), Some("win.trash"));
    section.append(Some("Preferences…"), Some("win.preferences"));
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

    // Wrap the header bar in a WindowHandle for drag support (overlay, transparent, no push)
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
        .css_classes(["time-label", "caption", "monospace", "numeric"])
        .build();
    position_label.set_xalign(0.0);
    // a11y: time labels should be numeric and respect text scaling via caption
    position_label.update_property(&[gtk::accessible::Property::Label("Played time")]);
    seek_row.append(&position_label);

    let seek_scale = gtk::Scale::builder()
        .hexpand(true)
        .draw_value(false)
        .css_classes(["seek-bar"])
        .value_pos(gtk::PositionType::Right)
        .build();
    seek_scale.update_property(&[gtk::accessible::Property::Label("Seek position")]);
    seek_scale.set_accessible_role(gtk::AccessibleRole::Slider);
    seek_scale.set_range(0.0, 500.0);
    seek_scale.set_increments(1.0, 10.0);
    // User-driven seeks via change-value (only fires for interaction, not for
    // programmatic set_value from timestamp notifies — no feedback loop).
    // Native click-to-seek is kept; no custom click math (RTL/padding safe).
    {
        let state = state.clone();
        seek_scale.connect_change_value(move |_, _, value| {
            let (media, duration) = {
                let s = state.borrow();
                (
                    s.media_file.clone(),
                    s.media_file.as_ref().map(|m| m.duration()).unwrap_or(0),
                )
            };
            if let Some(media) = media {
                if duration > 0 {
                    let ts = (value / 500.0 * duration as f64).clamp(0.0, duration as f64) as i64;
                    state.borrow_mut().seeking = true;
                    media.seek(ts);
                }
            }
            glib::Propagation::Proceed
        });
    }

    // When seek bar is focused, intercept arrow keys for navigation
    // (GtkScale's built-in handler would otherwise consume them)
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let window = window.clone();
        let seek_key_ctrl = gtk::EventControllerKey::new();
        seek_key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
        seek_key_ctrl.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Left => {
                nav(&state, &picture, &scrolled, &window, -1);
                glib::Propagation::Stop
            }
            gdk::Key::Right => {
                nav(&state, &picture, &scrolled, &window, 1);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        seek_scale.add_controller(seek_key_ctrl);
    }

    seek_row.append(&seek_scale);

    let duration_label = gtk::Label::builder()
        .label("0:00")
        .halign(gtk::Align::End)
        .css_classes(["time-label", "caption", "monospace", "numeric"])
        .build();
    duration_label.set_xalign(1.0);
    duration_label.update_property(&[gtk::accessible::Property::Label("Total duration")]);
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
    play_pause_btn.update_property(&[gtk::accessible::Property::Label("Play/Pause")]);
    play_pause_btn.set_accessible_role(gtk::AccessibleRole::Button);
    {
        let state = state.clone();
        play_pause_btn.connect_clicked(move |_| {
            toggle_play_pause(&state);
        });
    }

    // Volume slider (HIG: slider with value label, centered)
    let volume_scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::Center)
        .width_request(100)
        .draw_value(false)
        .css_classes(["volume-bar"])
        .build();
    volume_scale.update_property(&[gtk::accessible::Property::Label("Volume")]);
    volume_scale.set_accessible_role(gtk::AccessibleRole::Slider);
    volume_scale.set_range(0.0, 1.0);
    volume_scale.set_value(1.0);
    {
        let state = state.clone();
        volume_scale.connect_value_changed(move |scale| {
            let value = scale.value();
            if state.borrow().skip_volume_update {
                return;
            }
            let (media, btn) = {
                let s = state.borrow();
                (s.media_file.clone(), s.mute_btn.clone())
            };
            if let Some(media) = media {
                let muted = value < 0.01;
                media.set_volume(value);
                media.set_muted(muted);
                if let Some(btn) = btn {
                    update_mute_button(&btn, muted);
                }
            }
        });
    }

    let mute_btn = gtk::Button::builder()
        .icon_name("audio-volume-muted-symbolic")
        .tooltip_text("Mute/Unmute (M)")
        .css_classes(["ctrl-btn"])
        .build();
    mute_btn.update_property(&[gtk::accessible::Property::Label("Mute")]);
    mute_btn.set_accessible_role(gtk::AccessibleRole::Button);
    {
        let state = state.clone();
        mute_btn.connect_clicked(move |_| {
            let new_muted = state
                .borrow()
                .media_file
                .as_ref()
                .map(|m| !m.is_muted())
                .unwrap_or(false);
            set_muted_state(&state, new_muted);
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
        s.main_overlay = Some(overlay.clone());
    }

    // ── Auto-hide + mouse tracking (Showtime-style, overlay transparent, no push) ──
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
            let menu = menu_btn.clone();
            motion_ctrl.connect_motion(move |_, x, y| {
                {
                    let mut s = state.borrow_mut();
                    s.mouse_x = x;
                    s.mouse_y = y;
                }
                let win_h = window_ref.height() as f64;
                let at_top = y < win_h * EDGE_THRESHOLD;
                let at_bottom = y > win_h * (1.0 - EDGE_THRESHOLD);
                let in_top_zone = at_top;
                let in_bottom_zone = at_bottom;
                let in_any_zone = in_top_zone || in_bottom_zone;
                if in_any_zone {
                    cancel_hide(&hide_id);
                    top.set_opacity(1.0);
                    if bottom.is_visible() {
                        bottom.set_opacity(1.0);
                    }
                }
                if !in_top_zone && !in_bottom_zone {
                    arm_hide(&top, &bottom, &hide_id, FADE_DELAY_MS, &menu);
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
                    cancel_hide(&hide_id);
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
                let menu = menu_btn.clone();
                motion_top.connect_leave(move |_| {
                    arm_hide(&top, &bottom, &hide_id, FADE_DELAY_MS, &menu);
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
                    cancel_hide(&hide_id);
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
                let menu = menu_btn.clone();
                motion_bottom.connect_leave(move |_| {
                    arm_hide(&top, &bottom, &hide_id, FADE_DELAY_MS, &menu);
                });
            }
            bottom_outer.add_controller(motion_bottom);
        }

        // While the menu popup is open the panel stays visible.
        {
            let top = top_handle.clone();
            let bottom = bottom_outer.clone();
            let hide_id = hide_id.clone();
            menu_btn.connect_active_notify(move |btn| {
                if btn.is_active() {
                    cancel_hide(&hide_id);
                    top.set_opacity(1.0);
                    if bottom.is_visible() {
                        bottom.set_opacity(1.0);
                    }
                } else {
                    arm_hide(&top, &bottom, &hide_id, FADE_DELAY_MS, btn);
                }
            });
        }

        // When mouse leaves the window, fade out both panels quickly
        let motion_leave = gtk::EventControllerMotion::new();
        {
            let menuc = menu_btn.clone();
            motion_leave.connect_leave(move |_| {
                if menuc.is_active() {
                    return;
                }
                top.set_opacity(0.0);
                bottom.set_opacity(0.0);
            });
        }
        window.add_controller(motion_leave);
    }

    window.set_content(Some(&overlay));

    // ── Drag and drop: load files dropped on the window ──
    {
        let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let window_clone = window.clone();
        let empty_status_clone = empty_status.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            if let Ok(file_list) = value.get::<gdk::FileList>() {
                let gfiles = file_list.files();
                if let Some(first) = gfiles.first() {
                    debug_log(&format!(
                        "drop: uri={} has_local_path={}",
                        first.uri(),
                        first.path().is_some()
                    ));
                }
                // Local paths, split into files and dropped folders. Stat can
                // fail in the sandbox while GIO still reads, so trust the
                // extension as a last resort before giving up.
                let mut paths: Vec<PathBuf> = Vec::new();
                let mut dirs: Vec<PathBuf> = Vec::new();
                for p in gfiles.iter().filter_map(|f| f.path()) {
                    if p.is_dir() {
                        dirs.push(p);
                    } else if p.is_file() || is_media(&p) {
                        paths.push(p);
                    }
                }
                if paths.is_empty() && dirs.is_empty() {
                    return false;
                }
                // Dropped folders define their own scope (the portal exports
                // a dropped folder's subtree, so this works for sandboxed
                // gvfs drops too): dropped media files first, then each
                // folder's listing in drop order.
                let via_portal = paths
                    .first()
                    .or(dirs.first())
                    .map(|p| is_doc_portal_path(p))
                    .unwrap_or(false);
                let mut files: Vec<PathBuf> =
                    paths.iter().filter(|p| is_media(p)).cloned().collect();
                for dir in &dirs {
                    files.extend(collect_dir_media(dir));
                }
                if !via_portal && dirs.is_empty() {
                    // Plain local file drop: browse the whole parent folder.
                    let read_dir = paths[0].parent().unwrap_or(Path::new(".")).to_path_buf();
                    let listed = collect_dir_media(&read_dir);
                    if !listed.is_empty() {
                        files = listed;
                    }
                    // Else keep just the dropped files (set above).
                }
                // If directory listing failed (e.g. sandbox), fall back to dropped files only
                if files.is_empty() {
                    files = paths.iter().filter(|p| is_media(p)).cloned().collect();
                }
                if files.is_empty() {
                    return false;
                }
                let start_index = paths
                    .first()
                    .and_then(|p| files.iter().position(|f| f == p))
                    .unwrap_or(0);
                cancel_slide(&state);
                {
                    let mut s = state.borrow_mut();
                    s.files = files;
                    s.index = start_index;
                    s.zoom = 1.0;
                }
                scrolled.hadjustment().set_value(0.0);
                scrolled.vadjustment().set_value(0.0);
                scrolled.set_visible(true);
                empty_status_clone.set_visible(false);
                show_file(&state, &picture, &scrolled, &window_clone);
                return true;
            }
            false
        });
        overlay.add_controller(drop_target);
    }

    // ── Actions ──
    let about_action = gio::SimpleAction::new("about", None);
    {
        let w = window.clone();
        about_action.connect_activate(move |_, _| {
            let about = adw::AboutDialog::builder()
                .application_name("Carosello")
                .application_icon("io.github.grigio.carosello")
                .developer_name("Carosello Contributors")
                .version(env!("CARGO_PKG_VERSION"))
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

    let preferences_action = gio::SimpleAction::new("preferences", None);
    {
        let state = state.clone();
        let window = window.clone();
        preferences_action.connect_activate(move |_, _| {
            show_preferences(&state, &window);
        });
    }
    window.add_action(&preferences_action);

    let close_action = gio::SimpleAction::new("close", None);
    {
        let w = window.clone();
        close_action.connect_activate(move |_, _| w.close());
    }
    window.add_action(&close_action);

    // Open file via portal (GtkFileDialog) — HIG blocker
    let open_action = gio::SimpleAction::new("open", None);
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let window_clone = window.clone();
        open_action.connect_activate(move |_, _| {
            let dialog = gtk::FileDialog::builder()
                .title("Open Image or Video")
                .modal(true)
                .build();
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            let img_filter = gtk::FileFilter::new();
            img_filter.set_name(Some("Images and Videos"));
            for pat in [
                "*.jpg", "*.jpeg", "*.png", "*.webp", "*.gif", "*.bmp", "*.tiff", "*.tif", "*.mp4",
                "*.webm", "*.mkv",
            ] {
                img_filter.add_pattern(pat);
                img_filter.add_pattern(&pat.to_ascii_uppercase());
            }
            filters.append(&img_filter);
            let all_filter = gtk::FileFilter::new();
            all_filter.set_name(Some("All Files"));
            all_filter.add_pattern("*");
            filters.append(&all_filter);
            dialog.set_filters(Some(&filters));
            dialog.set_default_filter(Some(&img_filter));
            let state = state.clone();
            let picture = picture.clone();
            let scrolled = scrolled.clone();
            let window = window_clone.clone();
            dialog.open(Some(&window_clone), None::<&gio::Cancellable>, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        let read_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                        let mut files = state::collect_media(&read_dir);
                        if files.is_empty() {
                            files = vec![path.clone()];
                            if !is_media(&path) {
                                if let Some(ref ov) = state.borrow().toast_overlay {
                                    let toast = adw::Toast::new("Unsupported file type");
                                    ov.add_toast(toast);
                                }
                                return;
                            }
                        }
                        let start_index = files.iter().position(|f| f == &path).unwrap_or(0);
                        cancel_slide(&state);
                        {
                            let mut s = state.borrow_mut();
                            s.files = files;
                            s.index = start_index;
                            s.zoom = 1.0;
                        }
                        scrolled.hadjustment().set_value(0.0);
                        scrolled.vadjustment().set_value(0.0);
                        if let Some(ref st) = state.borrow().empty_status.clone() {
                            st.set_visible(false);
                        }
                        scrolled.set_visible(true);
                        show_file(&state, &picture, &scrolled, &window);
                    }
                }
            });
        });
    }
    window.add_action(&open_action);

    // Open folder via portal (GtkFileDialog select_folder). Unlike picking a
    // single file, the portal exports the whole directory, so sibling
    // navigation works even inside the Flatpak sandbox.
    let open_folder_action = gio::SimpleAction::new("open-folder", None);
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let window_clone = window.clone();
        open_folder_action.connect_activate(move |_, _| {
            let dialog = gtk::FileDialog::builder()
                .title("Open Folder")
                .modal(true)
                .build();
            let state = state.clone();
            let picture = picture.clone();
            let scrolled = scrolled.clone();
            let window = window_clone.clone();
            dialog.select_folder(Some(&window_clone), None::<&gio::Cancellable>, move |res| {
                if let Ok(file) = res {
                    if let Some(dir) = file.path() {
                        let files = state::collect_media(&dir);
                        if files.is_empty() {
                            show_toast(&state, "No images or videos in this folder");
                            return;
                        }
                        cancel_slide(&state);
                        {
                            let mut s = state.borrow_mut();
                            s.files = files;
                            s.index = 0;
                            s.zoom = 1.0;
                        }
                        reset_scroll(&scrolled);
                        if let Some(ref st) = state.borrow().empty_status.clone() {
                            st.set_visible(false);
                        }
                        scrolled.set_visible(true);
                        show_file(&state, &picture, &scrolled, &window);
                    }
                }
            });
        });
    }
    window.add_action(&open_folder_action);

    // Shortcuts dialog — HIG §Keyboard
    let shortcuts_action = gio::SimpleAction::new("shortcuts", None);
    {
        let w = window.clone();
        shortcuts_action.connect_activate(move |_, _| {
            // Prefer AdwShortcutsDialog if available, fall back to AlertDialog
            // Using AlertDialog keeps compatibility with libadwaita 1.5 bindings
            let dlg = adw::AlertDialog::builder()
                .heading("Keyboard Shortcuts")
                .body("Navigation:\n  ← / →, Page Up / Down  —  Previous / Next\n  Home / End  —  First / Last\n  3-finger swipe (2-finger if enabled in Preferences)  —  Previous / Next\n\nZoom:\n  Ctrl + + / −  —  Zoom In / Out\n  Ctrl + 0  —  Reset Zoom\n  Ctrl + Scroll  —  Zoom\n  Pinch  —  Zoom\n  Double-click  —  Toggle 2.5×\n  Drag  —  Pan when zoomed\n\nView:\n  F11 / F  —  Fullscreen\n  Esc  —  Reset Zoom\n\nVideo:\n  Space / K  —  Play / Pause\n  M  —  Mute\n  [ / ]  —  Seek 5 s\n  Click seek bar  —  Seek\n\nFile:\n  Del  —  Move to Trash\n\nApplication:\n  Ctrl + O  —  Open File\n  Ctrl + Shift + O  —  Open Folder\n  Ctrl + Q  —  Quit\n  Ctrl + W  —  Close\n  Ctrl + ? / Ctrl + K  —  This Help\n  F1  —  About")
                .build();
            dlg.add_response("close", "Close");
            dlg.set_close_response("close");
            dlg.present(Some(&w));
        });
    }
    window.add_action(&shortcuts_action);

    // Zoom actions for discoverability via app.set_accels
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let zoom_in = gio::SimpleAction::new("zoom-in", None);
        zoom_in.connect_activate(move |_, _| zoom_by(&state, &picture, &scrolled, 1.25, None));
        window.add_action(&zoom_in);
    }
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let zoom_out = gio::SimpleAction::new("zoom-out", None);
        zoom_out
            .connect_activate(move |_, _| zoom_by(&state, &picture, &scrolled, 1.0 / 1.25, None));
        window.add_action(&zoom_out);
    }
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let zoom_reset = gio::SimpleAction::new("zoom-reset", None);
        zoom_reset.connect_activate(move |_, _| zoom_to(&state, &picture, &scrolled, 1.0, None));
        window.add_action(&zoom_reset);
    }

    // Move current file to Trash (Del). Index is kept so the next sibling
    // slides into place instead of resetting to the first item.
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let window_c = window.clone();
        let trash = gio::SimpleAction::new("trash", None);
        trash.connect_activate(move |_, _| {
            trash_current(&state, &picture, &scrolled, &window_c);
        });
        window.add_action(&trash);
    }

    // Accelerators are registered once in main::connect_startup (single-instance
    // safe); window actions here are discoverable via those accels.

    let has_files = !state.borrow().files.is_empty();
    scrolled.set_visible(has_files);
    empty_status.set_visible(!has_files);

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
            // Navigation: Left/Right, PageUp/PageDown, Home/End (HIG)
            gdk::Key::Left | gdk::Key::Page_Up => {
                nav(&state, &picture, &scrolled, &window, -1);
                glib::Propagation::Stop
            }
            gdk::Key::Right | gdk::Key::Page_Down => {
                nav(&state, &picture, &scrolled, &window, 1);
                glib::Propagation::Stop
            }
            gdk::Key::Home => {
                let len = state.borrow().files.len();
                if len > 0 {
                    cancel_slide(&state);
                    {
                        let mut s = state.borrow_mut();
                        s.index = 0;
                        s.zoom = 1.0;
                    }
                    scrolled.hadjustment().set_value(0.0);
                    scrolled.vadjustment().set_value(0.0);
                    show_file(&state, &picture, &scrolled, &window);
                }
                glib::Propagation::Stop
            }
            gdk::Key::End => {
                let len = state.borrow().files.len();
                if len > 0 {
                    cancel_slide(&state);
                    {
                        let mut s = state.borrow_mut();
                        s.index = len - 1;
                        s.zoom = 1.0;
                    }
                    scrolled.hadjustment().set_value(0.0);
                    scrolled.vadjustment().set_value(0.0);
                    show_file(&state, &picture, &scrolled, &window);
                }
                glib::Propagation::Stop
            }
            // Fullscreen
            gdk::Key::F11 | gdk::Key::f => {
                window.set_fullscreened(!window.is_fullscreen());
                glib::Propagation::Stop
            }
            // Zoom: Ctrl++ / Ctrl+- / Ctrl+0 (HIG standard, gated behind Ctrl)
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add
                if modifier.contains(gdk::ModifierType::CONTROL_MASK) =>
            {
                zoom_by(&state, &picture, &scrolled, 1.25, None);
                glib::Propagation::Stop
            }
            gdk::Key::minus | gdk::Key::underscore | gdk::Key::KP_Subtract
                if modifier.contains(gdk::ModifierType::CONTROL_MASK) =>
            {
                zoom_by(&state, &picture, &scrolled, 1.0 / 1.25, None);
                glib::Propagation::Stop
            }
            gdk::Key::_0 | gdk::Key::KP_0 if modifier.contains(gdk::ModifierType::CONTROL_MASK) => {
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
            // Help: F1
            gdk::Key::F1 => {
                // Trigger the about action
                if let Some(action) = window.lookup_action("about") {
                    action.activate(None);
                }
                glib::Propagation::Stop
            }
            // Shortcuts dialog: Ctrl+K (HIG) — must be before K play/pause
            gdk::Key::k if modifier.contains(gdk::ModifierType::CONTROL_MASK) => {
                gio::prelude::ActionGroupExt::activate_action(&window, "shortcuts", None);
                glib::Propagation::Stop
            }
            // Play/Pause: Space / K — only consume when video, so Space still
            // activates focused buttons on images.
            gdk::Key::space => {
                if state.borrow().is_video && state.borrow().media_file.is_some() {
                    toggle_play_pause(&state);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            gdk::Key::k => {
                if state.borrow().is_video && state.borrow().media_file.is_some() {
                    toggle_play_pause(&state);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
            // Mute toggle: m — only consume when video.
            gdk::Key::m => {
                let new_muted = state.borrow().media_file.as_ref().map(|m| !m.is_muted());
                if let Some(new_muted) = new_muted {
                    if state.borrow().is_video {
                        set_muted_state(&state, new_muted);
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
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
            // Ctrl+O / Ctrl+? handled via app accels (win.open / win.shortcuts).
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

    // ── Scroll: zoomed → pan; fit + two-finger mode → swipe-navigate ──
    // A 2-finger touchpad motion arrives as smooth scroll, not TouchpadSwipe
    // (libinput reserves swipe gestures for 3+ fingers), so in two-finger
    // mode the scroll deltas drive the same interactive drag machinery as
    // the touchpad phases above: begin on first scroll, feed dx, settle on
    // gesture end (or after a short idle — not every backend emits end).
    let scroll_ctrl = gtk::EventControllerScroll::builder()
        .flags(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::HORIZONTAL,
        )
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let scroll_ctrl_c = scroll_ctrl.clone();
        // Settles a scroll-driven drag now: drop a pending idle settle,
        // then end the drag if one is still active.
        let settle_id: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        let settle_now: Rc<dyn Fn()> = {
            let state = state.clone();
            let picture = picture.clone();
            let scrolled = scrolled.clone();
            let window = window.clone();
            let settle_id = settle_id.clone();
            Rc::new(move || {
                if let Some(id) = settle_id.take() {
                    id.remove();
                }
                if state.borrow().drag.is_some() {
                    debug_log("scroll-swipe: settle");
                    drag_end(&state, &picture, &scrolled, &window, false);
                }
            })
        };
        {
            let state = state.clone();
            let picture = picture.clone();
            let scrolled = scrolled.clone();
            scroll_ctrl.connect_scroll_begin(move |_| {
                let eligible = {
                    let s = state.borrow();
                    s.two_finger_swipe && s.zoom <= 1.05 && s.drag.is_none()
                };
                if eligible {
                    drag_begin(&state, &picture, &scrolled);
                }
            });
        }
        {
            let settle_now = settle_now.clone();
            scroll_ctrl.connect_scroll_end(move |_| {
                settle_now();
            });
        }
        scroll_ctrl.connect_scroll(move |_, dx, dy| {
            let has_content = {
                let s = state.borrow();
                (s.image_w > 0 && s.image_h > 0) || s.media_file.is_some()
            };
            if !has_content {
                return glib::Propagation::Proceed;
            }
            // Ctrl+scroll stays a zoom gesture (handled below).
            let is_ctrl = scroll_ctrl_c
                .current_event()
                .map(|e| e.modifier_state().contains(gdk::ModifierType::CONTROL_MASK))
                .unwrap_or(false);
            if is_ctrl {
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

            if !state.borrow().two_finger_swipe {
                return glib::Propagation::Proceed;
            }
            // Lazily begin for backends that skip scroll-begin.
            if state.borrow().drag.is_none()
                && matches!(
                    drag_begin(&state, &picture, &scrolled),
                    glib::Propagation::Stop
                )
            {
                debug_log(&format!("scroll-swipe: begin (dx={dx:.1})"));
            }
            if state.borrow().drag.is_none() {
                // Begin declined (no frame, animations off…): stay out.
                return glib::Propagation::Proceed;
            }
            if dx.is_finite() && dx != 0.0 {
                // Unmirror to finger motion (see touchpad_natural_scroll):
                // the drag tracks fingers like TouchpadSwipe does.
                let finger_dx = if touchpad_natural_scroll() { -dx } else { dx };
                drag_update(&state, finger_dx);
            }
            // Re-arm the idle settle: continuous motion keeps feeding one
            // drag (one navigation per swipe), a pause settles it.
            if let Some(id) = settle_id.take() {
                id.remove();
            }
            {
                let settle_now = settle_now.clone();
                settle_id.set(Some(glib::timeout_add_local_once(
                    Duration::from_millis(120),
                    move || settle_now(),
                )));
            }
            glib::Propagation::Stop
        });
    }
    scrolled.add_controller(scroll_ctrl);

    // ── Ctrl+Scroll zoom (HIG) ──
    let zoom_scroll_ctrl = gtk::EventControllerScroll::builder()
        .flags(gtk::EventControllerScrollFlags::VERTICAL)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        let ctrl = zoom_scroll_ctrl.clone();
        zoom_scroll_ctrl.connect_scroll(move |_, _, dy| {
            let is_ctrl = ctrl
                .current_event()
                .map(|e| e.modifier_state().contains(gdk::ModifierType::CONTROL_MASK))
                .unwrap_or(false);
            if !is_ctrl {
                return glib::Propagation::Proceed;
            }
            let factor = if dy < 0.0 { 1.1 } else { 1.0 / 1.1 };
            zoom_by(&state, &picture, &scrolled, factor, None);
            glib::Propagation::Stop
        });
    }
    scrolled.add_controller(zoom_scroll_ctrl);

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
                let anchor = if gesture.device().is_some_and(|d| d.has_cursor()) {
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
                    debug_log(&format!(
                        "dblclick: press=({x:.0},{y:.0}) pic={}x{} scrolled={}x{}",
                        picture_c.width(),
                        picture_c.height(),
                        scrolled_c.width(),
                        scrolled_c.height()
                    ));
                    if let Some(conv) = picture_c.compute_point(&scrolled_c, &pt) {
                        debug_log(&format!(
                            "dblclick: anchor=({:.0},{:.0})",
                            conv.x(),
                            conv.y()
                        ));
                        zoom_to(
                            &state,
                            &picture_c,
                            &scrolled_c,
                            2.5,
                            Some((conv.x() as f64, conv.y() as f64)),
                        );
                    } else {
                        // Coordinate conversion can fail before first layout:
                        // still zoom (centered) instead of ignoring the click.
                        debug_log("dblclick: compute_point failed, center fallback");
                        zoom_to(&state, &picture_c, &scrolled_c, 2.5, None);
                    }
                }
            }
        });
        picture.add_controller(click);
    }

    // ── Touchpad drag: both items follow the finger ──
    // Raw TouchpadSwipe phases (GestureSwipe only fires after release).
    // Finger count comes from Preferences (2 when enabled, else 3);
    // pinch passes through, and 2-finger scroll is translated into this
    // same drag path by the scroll handler below (most systems report
    // two fingers as scroll, not TouchpadSwipe).
    {
        let pad = gtk::EventControllerLegacy::new();
        pad.set_propagation_phase(gtk::PropagationPhase::Capture);
        let state_c = state.clone();
        let picture_c = picture.clone();
        let scrolled_c = scrolled.clone();
        let window_c = window.clone();
        pad.connect_event(move |_, event| {
            handle_touchpad(&state_c, &picture_c, &scrolled_c, &window_c, event)
        });
        scrolled.add_controller(pad);
    }

    // ── Swipe: prev/next item (touchscreens; touchpad drags
    // consumed interactively above are skipped via last_drag_us) ──
    // `n-points` is construct-only, so register one discrete swipe per
    // finger count and gate in the handler on the live pref: 2-finger
    // acts only when enabled, 3-finger only when disabled.
    for n_points in [2u32, 3u32] {
        let swipe = gtk::GestureSwipe::builder().n_points(n_points).build();
        let state = state.clone();
        let picture = picture.clone();
        let scrolled_s = scrolled.clone();
        let window_w = window.clone();
        swipe.connect_swipe(move |_, vx, vy| {
            if state.borrow().two_finger_swipe != (n_points == 2) {
                return;
            }
            if n_points == 2 && state.borrow().zoom > 1.05 {
                // Zoomed: two fingers pan/pinch, they never navigate.
                return;
            }
            let fresh = glib::monotonic_time() - state.borrow().last_drag_us > 150_000;
            if !fresh {
                return;
            }
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

    // ── Resize handling: viewport notifies cover drag/tile/maximize/fullscreen
    // (all change allocation). GtkWidget exposes no width/height GObject
    // properties, so connect_notify on them would never fire; the scrolled
    // viewport's page size always tracks the visible size instead, so the
    // adjustments' `changed` signal is the reliable resize hook. Zoom's own
    // set_value emits value-changed only, so this never self-triggers, and
    // schedule_update coalesces bursts while update_display is idempotent.
    for adj in [scrolled.hadjustment(), scrolled.vadjustment()] {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled_w = scrolled.clone();
        adj.connect_changed(move |_| {
            schedule_update(&state, &picture, &scrolled_w);
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

/// Single place for play/pause icon state.
fn sync_play_button(btn: &gtk::Button, playing: bool) {
    if playing {
        btn.set_icon_name("media-playback-pause-symbolic");
        btn.set_tooltip_text(Some("Pause (Space)"));
    } else {
        btn.set_icon_name("media-playback-start-symbolic");
        btn.set_tooltip_text(Some("Play (Space)"));
    }
}

fn toggle_play_pause(state: &Rc<RefCell<AppState>>) {
    // (Copied out: pause()/play() synchronously emit playing notifies
    // whose handlers borrow state — no borrow may be held here.)
    let (media, btn, is_video) = {
        let s = state.borrow();
        (s.media_file.clone(), s.play_pause_btn.clone(), s.is_video)
    };
    if let (Some(media), Some(btn)) = (media, btn) {
        if is_video {
            if media.is_playing() {
                media.pause();
                sync_play_button(&btn, false);
            } else {
                media.play();
                sync_play_button(&btn, true);
            }
        }
    }
}

/// Single place for mute + volume-slider sync (avoids the three divergent copies).
fn set_muted_state(state: &Rc<RefCell<AppState>>, muted: bool) {
    let (media, btn, scale) = {
        let s = state.borrow();
        (
            s.media_file.clone(),
            s.mute_btn.clone(),
            s.volume_scale.clone(),
        )
    };
    if let Some(ref media) = media {
        // Unmuting with the volume at zero stays silent (volume 0 + muted
        // false). This happens after dragging the slider to 0, which sets
        // volume 0 and muted, then pressing M / the mute button: restoring
        // the slider to media.volume() keeps it at 0. Restore full volume.
        if !muted && media.volume() < 0.01 {
            media.set_volume(1.0);
        }
        media.set_muted(muted);
    }
    if let Some(btn) = btn {
        update_mute_button(&btn, muted);
    }
    state.borrow_mut().skip_volume_update = true;
    if let Some(scale) = scale {
        if muted {
            scale.set_value(0.0);
        } else if let Some(ref media) = media {
            scale.set_value(media.volume());
        }
    }
    state.borrow_mut().skip_volume_update = false;
}

/// Refresh seek bar + time labels + play icon from the current media position.
/// Called from timestamp/duration/playing notifies (signal-driven, no polling).
fn update_seek_ui(state: &Rc<RefCell<AppState>>) {
    let (media, seeking, scale, pos, dur, play_btn, is_vid) = {
        let s = state.borrow();
        (
            s.media_file.clone(),
            s.seeking,
            s.seek_scale.clone(),
            s.position_label.clone(),
            s.duration_label.clone(),
            s.play_pause_btn.clone(),
            s.is_video,
        )
    };
    let Some(media) = media else { return };
    if !is_vid {
        return;
    }
    let ts = media.timestamp();
    let dur_val = media.duration();
    if !seeking {
        if let Some(scale) = scale {
            if dur_val > 0 {
                scale.set_value((ts as f64 / dur_val as f64) * 500.0);
            }
        }
    }
    if let Some(label) = pos {
        label.set_text(&format_time(ts));
    }
    if let Some(label) = dur {
        if dur_val > 0 {
            label.set_text(&format_time(dur_val));
        }
    }
    if let Some(btn) = play_btn {
        sync_play_button(&btn, media.is_playing());
    }
}

fn show_toast(state: &Rc<RefCell<AppState>>, msg: &str) {
    if let Some(overlay) = state.borrow().toast_overlay.clone() {
        let toast = adw::Toast::new(msg);
        toast.set_timeout(3);
        overlay.add_toast(toast);
    }
}

/// True when `path` lives under the Flatpak document portal
/// (`$XDG_RUNTIME_DIR/doc/…`). The portal exports only the explicitly
/// opened file, so the containing directory always lists a single item
/// and sibling navigation is impossible from such paths.
fn is_doc_portal_path(path: &Path) -> bool {
    let doc_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(|r| PathBuf::from(r).join("doc"))
        .unwrap_or_else(|_| PathBuf::from("/run/user/1000/doc"));
    path.starts_with(&doc_dir)
}

/// List media directly inside `dir`: plain read_dir first, gvfs-daemon
/// enumeration when the sandbox blocks readdir.
fn collect_dir_media(dir: &Path) -> Vec<PathBuf> {
    let listed = state::collect_media(dir);
    if !listed.is_empty() {
        return listed;
    }
    let fallback = collect_media_gio(&gio::File::for_path(dir));
    debug_log(&format!(
        "drop GIO fallback: {} siblings for {}",
        fallback.len(),
        dir.display()
    ));
    fallback
}
/// Sibling media via GIO enumeration (gvfs/sandbox fallback).
/// `std::fs::read_dir` can fail inside the sandbox even when the dropped
/// file itself is readable (fuse mounts, portal-adjacent paths), while
/// enumeration via the gvfs daemon (org.gtk.vfs) still succeeds. Trusts
/// the enumerator's file type instead of re-statting (stat can hit the
/// same sandbox wall). Returns local paths only; pure remote URIs
/// (no FUSE path) yield nothing and callers fall back to dropped files.
fn collect_media_gio(parent: &gio::File) -> Vec<PathBuf> {
    let Ok(enumerator) = parent.enumerate_children(
        "standard::name,standard::type",
        gio::FileQueryInfoFlags::NONE,
        None::<&gio::Cancellable>,
    ) else {
        debug_log(&format!(
            "collect_media_gio: enumerate({}) failed",
            parent.uri()
        ));
        return Vec::new();
    };
    let mut files = Vec::new();
    while let Ok(Some(info)) = enumerator.next_file(None::<&gio::Cancellable>) {
        if info.file_type() != gio::FileType::Regular {
            continue;
        }
        let name = info.name();
        let name_str = name.to_string_lossy();
        if !state::is_media(Path::new(name_str.as_ref())) {
            continue;
        }
        if let Some(path) = parent.child(name_str.as_ref()).path() {
            files.push(path);
        }
    }
    state::sort_media_paths(&mut files);
    files
}

fn reset_scroll(scrolled: &gtk::ScrolledWindow) {
    scrolled.hadjustment().set_value(0.0);
    scrolled.vadjustment().set_value(0.0);
}

// ── Auto-hide helpers (opacity-only; CSS transparencies untouched) ──
fn cancel_hide(hide_id: &Rc<Cell<Option<glib::SourceId>>>) {
    if let Some(id) = hide_id.take() {
        id.remove();
    }
}

fn arm_hide(
    top: &gtk::WindowHandle,
    bottom: &gtk::Box,
    hide_id: &Rc<Cell<Option<glib::SourceId>>>,
    delay_ms: u32,
    menu: &gtk::MenuButton,
) {
    cancel_hide(hide_id);
    let topc = top.clone();
    let bottomc = bottom.clone();
    let hide_id2 = hide_id.clone();
    let menuc = menu.clone();
    let sid = glib::timeout_add_local(Duration::from_millis(delay_ms as u64), move || {
        // While the menu popup is open the panel stays visible (no hide under it).
        if menuc.is_active() {
            hide_id2.set(None);
            return glib::ControlFlow::Break;
        }
        // Opacity-only fade; widgets stay in place so overlay layout/push is unchanged.
        topc.set_opacity(0.0);
        bottomc.set_opacity(0.0);
        hide_id2.set(None);
        glib::ControlFlow::Break
    });
    hide_id.set(Some(sid));
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
    zoom_to(
        state,
        picture,
        scrolled,
        state::clamp_zoom(cur * factor),
        anchor,
    );
}

/// Stillness snapshot for `zoom_to`'s re-anchor poll: both adjustment
/// ranges, both values and the picture's size request. As long as this
/// does not move, layout has caught up with the applied anchor.
#[derive(Clone, Copy, PartialEq)]
struct AnchorSnap {
    h_upper: f64,
    h_page: f64,
    v_upper: f64,
    v_page: f64,
    h_value: f64,
    v_value: f64,
    pic_w: i32,
    pic_h: i32,
}

fn zoom_to(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    new_zoom: f64,
    anchor: Option<(f64, f64)>,
) {
    let mut new_zoom = state::clamp_zoom(new_zoom);
    let (old_zoom, intrinsic, viewport, is_video) = {
        let s = state.borrow();
        let has_image = s.image_w > 0 && s.image_h > 0;
        if !has_image && s.media_file.is_none() {
            return;
        }
        let iw = if has_image {
            Some((s.image_w as f64, s.image_h as f64))
        } else {
            None
        };
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
        let is_video = s.is_video;
        (
            s.zoom,
            intrinsic,
            {
                let w = scrolled.width() as f64;
                let h = scrolled.height() as f64;
                (w.max(1.0), h.max(1.0))
            },
            is_video,
        )
    };
    // Images render at fit when zoom < 1 (Contain in a viewport-sized
    // allocation ignores the smaller size request), so sub-fit levels are a
    // no-op that only corrupts the next anchor ratio. Fit is the minimum.
    if !is_video {
        new_zoom = new_zoom.max(1.0);
    }
    if (new_zoom - old_zoom).abs() < 0.0001 {
        return;
    }
    let Some((iw, ih)) = intrinsic else {
        state.borrow_mut().zoom = new_zoom;
        schedule_update(state, picture, scrolled);
        return;
    };
    let (vw, vh) = viewport;
    let (old_dw, old_dh) = zoom::effective_display_size_for(iw, ih, vw, vh, old_zoom, is_video);
    let (new_dw, new_dh) = zoom::effective_display_size_for(iw, ih, vw, vh, new_zoom, is_video);

    let (ax, ay) = anchor.unwrap_or((vw / 2.0, vh / 2.0));
    let hadj = scrolled.hadjustment();
    let vadj = scrolled.vadjustment();
    let old_hv = hadj.value();
    let old_vv = vadj.value();

    state.borrow_mut().zoom = new_zoom;
    schedule_update(state, picture, scrolled);

    // Re-anchor scroll after layout. Applying only synchronously races
    // layout: the adjustments still hold the old (often zero) range, so
    // `set_value` clamps to the top-left and the intent is lost — that is
    // exactly the "double-click zooms into the top-left corner" symptom.
    // Applying from inside an adjustments `changed` emission is no better:
    // the value is recorded but the running layout pass never repositions
    // the child (observed: v=1050 while `compute_point` stays at (0,0)).
    // So the anchor is applied only from outside layout — immediately
    // (already correct when the range is final, e.g. an incremental pinch
    // step) and then from a bounded poll — re-applied idempotently from the
    // captured origin until the range, the value and the picture size have
    // held still for a few ticks. A late resize (async video decode, EXIF
    // rotation) resets that stability and gets re-anchored, while a forced
    // clamp (portrait, where the target is unreachable) still settles.
    // Rapid successive zooms never fight: the stale guard drops every
    // superseded attempt.
    let apply_anchor = {
        let state = state.clone();
        move |hadj: &gtk::Adjustment, vadj: &gtk::Adjustment| {
            if (state.borrow().zoom - new_zoom).abs() > 0.0001 {
                return; // superseded by a newer zoom
            }
            let (new_hv, h_max) = zoom::anchor_target(old_hv, vw, old_dw as f64, new_dw as f64, ax);
            let (new_vv, v_max) = zoom::anchor_target(old_vv, vh, old_dh as f64, new_dh as f64, ay);
            let want_h = new_hv.clamp(0.0, h_max);
            let want_v = new_vv.clamp(0.0, v_max);
            // Only nudge GTK when the value is off target: a persistent
            // delta means the range was still too small, so retrying picks
            // the target up once layout has grown the upper bound.
            if h_max > 0.0 && (hadj.value() - want_h).abs() > 0.5 {
                hadj.set_value(want_h);
                debug_log(&format!("zoom-anchor: h -> {want_h:.0}"));
            }
            if v_max > 0.0 && (vadj.value() - want_v).abs() > 0.5 {
                vadj.set_value(want_v);
                debug_log(&format!("zoom-anchor: v -> {want_v:.0}"));
            }
        }
    };
    apply_anchor(&hadj, &vadj);
    {
        let picture = picture.clone();
        let mut ticks: u32 = 0;
        let mut stable: u32 = 0;
        let mut prev: Option<AnchorSnap> = None;
        glib::timeout_add_local(Duration::from_millis(16), move || {
            ticks += 1;
            apply_anchor(&hadj, &vadj);
            let snap = AnchorSnap {
                h_upper: hadj.upper(),
                h_page: hadj.page_size(),
                v_upper: vadj.upper(),
                v_page: vadj.page_size(),
                h_value: hadj.value(),
                v_value: vadj.value(),
                pic_w: picture.width(),
                pic_h: picture.height(),
            };
            stable = if prev == Some(snap) { stable + 1 } else { 0 };
            prev = Some(snap);
            if stable >= 4 {
                debug_log(&format!(
                    "zoom-settled: h={:.0} v={:.0} (t={}ms)",
                    hadj.value(),
                    vadj.value(),
                    ticks * 16,
                ));
                return glib::ControlFlow::Break;
            }
            if ticks >= 40 {
                debug_log("zoom-anchor: deadline, stop re-anchoring");
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }
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
    // Rapid navigation mid-slide cuts instantly so spam stays fast.
    if state.borrow().sliding {
        cancel_slide(state);
    }
    // Adjacent steps cross-slide; anything else cuts instantly.
    if dir != 0 && try_slide_to(state, picture, scrolled, window, new_index, dir) {
        return;
    }
    {
        let mut s = state.borrow_mut();
        s.index = new_index;
        s.zoom = 1.0;
    }
    reset_scroll(scrolled);
    show_file(state, picture, scrolled, window);
}

/// Outgoing frame for a slide: paintable + fit + size.
struct OldFrame {
    tex: gdk::Paintable,
    fit: gtk::ContentFit,
    ow: i32,
    oh: i32,
}

/// Capture the outgoing frame before switching. Images share their
/// immutable Texture; videos get a frozen wrapper around the paused
/// MediaFile so the shared live view can move on to the new item.
/// Sized exactly like the current allocation (`w`/`h` fallback).
fn old_frame(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    w: i32,
    h: i32,
) -> Option<OldFrame> {
    let (paintable, fit, was_video, old_media, old_view) = {
        let s = state.borrow();
        (
            picture.paintable(),
            picture.content_fit(),
            s.is_video,
            s.media_file.clone(),
            s.video_view.view(),
        )
    };
    let paintable = paintable?;
    let (tex, fit): (gdk::Paintable, gtk::ContentFit) = if was_video {
        if let Some(old_media) = old_media {
            let zp = ZoomPaintable::new();
            zp.set_inner(Some(old_media.upcast()));
            zp.set_view(old_view.0, old_view.1, old_view.2);
            (zp.upcast(), gtk::ContentFit::Fill)
        } else {
            (paintable, fit)
        }
    } else {
        (paintable, fit)
    };
    let (pw, ph) = (picture.width(), picture.height());
    let (ow, oh) = if pw > 1 && ph > 1 { (pw, ph) } else { (w, h) };
    Some(OldFrame { tex, fit, ow, oh })
}

/// Opaque black stage holding the outgoing frame, added above the content
/// (below the floating video controls). GtkFixed positions children
/// absolutely, so per-frame moves never renegotiate sizes (no measure
/// warnings, no squash); the window clips the off-screen travel.
struct StagePieces {
    overlay: gtk::Overlay,
    stage: gtk::Fixed,
    old_pic: gtk::Picture,
    obx: f64,
    oby: f64,
    gen: u64,
}

fn build_stage(
    state: &Rc<RefCell<AppState>>,
    old: &OldFrame,
    w: i32,
    h: i32,
) -> Option<StagePieces> {
    let overlay = { state.borrow().main_overlay.clone() };
    let overlay = overlay?;
    let stage = gtk::Fixed::new();
    stage.set_hexpand(true);
    stage.set_vexpand(true);
    stage.set_halign(gtk::Align::Fill);
    stage.set_valign(gtk::Align::Fill);
    stage.set_can_target(false);
    stage.add_css_class("carosello-scrolled");

    let old_pic = gtk::Picture::builder()
        .paintable(&old.tex)
        .content_fit(old.fit)
        .width_request(old.ow)
        .height_request(old.oh)
        .can_shrink(true)
        .build();
    old_pic.set_can_target(false);
    let obx = ((w - old.ow) as f64) / 2.0;
    let oby = ((h - old.oh) as f64) / 2.0;
    stage.put(&old_pic, obx, oby);
    overlay.add_overlay(&stage);
    // Keep floating video controls above the sliding frames.
    if let Some(bottom) = state.borrow().bottom_bar.clone() {
        if bottom.parent().is_some() {
            overlay.remove_overlay(&bottom);
            overlay.add_overlay(&bottom);
        }
    }
    let gen = {
        let mut s = state.borrow_mut();
        s.slide_gen = s.slide_gen.wrapping_add(1);
        s.slide_fixed = Some(stage.clone());
        s.sliding = true;
        s.slide_gen
    };
    Some(StagePieces {
        overlay,
        stage,
        old_pic,
        obx,
        oby,
        gen,
    })
}

/// Cross-slide to `new_index`: the outgoing frame and the incoming frame
/// (fully scaled before it moves) travel together — outgoing 0→∓W while
/// incoming ±W→0 over ~220ms. The main view switches underneath via the
/// regular `show_file` path, so steady-state behavior is unchanged; the
/// overlay stage is purely visual and removed at the end.
///
/// Images animate only when the frame is immediately ready (prefetch hit)
/// and fall back to an instant cut otherwise — never slide a placeholder
/// in, never decode twice. Videos slide once prepared with a known size
/// (2s timeout reveals the main view). Returns true when the switch was
/// taken over.
fn try_slide_to(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
    new_index: usize,
    dir: i32,
) -> bool {
    let (vw, vh) = viewport_size(scrolled);
    if vw < 1.0 || vh < 1.0 || !animations_enabled(state) {
        return false;
    }
    let w = vw as i32;
    let h = vh as i32;
    if state.borrow().zoom > 1.05 {
        return false;
    }
    let Some(old) = old_frame(state, picture, w, h) else {
        return false;
    };
    let path = { state.borrow().files.get(new_index).cloned() };
    let Some(path) = path else { return false };
    let new_is_video = state::has_ext(&path, state::VIDEO_EXTS);

    // Incoming images must be ready now (peek, don't consume: the main
    // fast path still takes the cache entry for itself). Prefetch stores
    // EXIF-oriented pixels, so use them as-is (re-applying orientation
    // would rotate portrait shots twice and glitch the transition).
    let new_image: Option<(gdk::Paintable, i32, i32)> = if !new_is_video {
        // (Two statements: the borrow must end before the pixels are used.)
        let cached = state.borrow().prefetch.get(&path).cloned();
        let Some(raw) = cached else {
            return false;
        };
        let (iw, ih) = (raw.width().max(1), raw.height().max(1));
        let tex: gdk::Paintable = media::texture_for_pixbuf(&raw).upcast();
        Some((tex, iw, ih))
    } else {
        None
    };

    // Commit the switch underneath first: title, controls, pipelines and
    // the main paintable all follow the regular path.
    {
        let mut s = state.borrow_mut();
        s.index = new_index;
        s.zoom = 1.0;
    }
    reset_scroll(scrolled);
    show_file(state, picture, scrolled, window);

    let Some(pieces) = build_stage(state, &old, w, h) else {
        return true;
    };
    let my_gen = pieces.gen;
    let StagePieces {
        overlay,
        stage,
        old_pic,
        obx,
        oby,
        ..
    } = pieces;

    if let Some((tex, iw, ih)) = new_image {
        let (dw, dh) = zoom::display_size_for(iw as f64, ih as f64, vw, vh, 1.0);
        let new_pic = gtk::Picture::builder()
            .paintable(&tex)
            .content_fit(gtk::ContentFit::Contain)
            .width_request(dw)
            .height_request(dh)
            .can_shrink(true)
            .build();
        new_pic.set_can_target(false);
        let nbx = ((w - dw) as f64) / 2.0;
        let nby = ((h - dh) as f64) / 2.0;
        stage.put(&new_pic, nbx + direction_offset(dir, w), nby);
        let ctx = SlideCtx {
            state: state.clone(),
            overlay,
            stage,
            old_pic,
            obx,
            oby,
            w,
            h,
            vw,
            vh,
            dir,
            my_gen,
            new_index,
        };
        start_slide_tick(
            &ctx,
            &new_pic,
            nbx,
            nby,
            0.0,
            -direction_offset(dir, w),
            state::SLIDE_MICROS,
        );
    } else {
        let ctx = SlideCtx {
            state: state.clone(),
            overlay,
            stage,
            old_pic,
            obx,
            oby,
            w,
            h,
            vw,
            vh,
            dir,
            my_gen,
            new_index,
        };
        preload_slide_video(&ctx, &path);
    }
    true
}

/// Signed off-screen origin for the incoming frame: right of stage for
/// next, left of stage for previous.
fn direction_offset(dir: i32, w: i32) -> f64 {
    if dir > 0 {
        w as f64
    } else {
        -(w as f64)
    }
}

/// Everything a running slide needs: stage widgets, geometry bases and the
/// generation/index guards. Bundled so helpers stay under the argument
/// limit and can't drift out of sync.
#[derive(Clone)]
struct SlideCtx {
    state: Rc<RefCell<AppState>>,
    overlay: gtk::Overlay,
    stage: gtk::Fixed,
    old_pic: gtk::Picture,
    obx: f64,
    oby: f64,
    w: i32,
    h: i32,
    vw: f64,
    vh: f64,
    dir: i32,
    my_gen: u64,
    new_index: usize,
}

/// Incoming video for a slide: its own muted pipeline + wrapper, traveling
/// once prepared with a known intrinsic size (then pre-scaled like the
/// main path). Weak refs only, so abandoning never leaks a pipeline.
fn preload_slide_video(ctx: &SlideCtx, path: &Path) {
    let media = gtk::MediaFile::for_filename(path);
    media.set_loop(true);
    media.set_muted(true);
    media.set_volume(1.0);
    let wrap = ZoomPaintable::new();
    wrap.set_inner(Some(media.clone().upcast()));
    let new_pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Fill)
        .width_request(ctx.w)
        .height_request(ctx.h)
        .can_shrink(true)
        .build();
    let paintable: gdk::Paintable = wrap.clone().upcast();
    new_pic.set_paintable(Some(&paintable));
    new_pic.set_can_target(false);
    ctx.stage
        .put(&new_pic, direction_offset(ctx.dir, ctx.w), 0.0);

    let started: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let size_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    // (Two statements: each lookup must finish before the mutable borrow
    // below, else RefCell panics.)
    let weak_media = media.downgrade();
    let weak_wrap = wrap.downgrade();
    let maybe_start = {
        let ctx = ctx.clone();
        let new_pic = new_pic.clone();
        let started = started.clone();
        let size_handler = size_handler.clone();
        move || {
            if started.get() {
                return;
            }
            let Some(m) = weak_media.upgrade() else {
                return;
            };
            let Some((sw, sh)) = zoom::video_intrinsic(&m) else {
                return;
            };
            // Superseded (rapid nav, Home/End, trash): reveal, never slide stale frames.
            {
                let s = ctx.state.borrow();
                if s.slide_gen != ctx.my_gen || s.index != ctx.new_index {
                    return;
                }
            }
            let (dw, dh) = zoom::display_size_for(sw, sh, ctx.vw, ctx.vh, 1.0);
            let (fw, fh) = zoom::fit_size(sw, sh, ctx.vw, ctx.vh);
            if let Some(wp) = weak_wrap.upgrade() {
                wp.set_view(fw, fh, 1.0);
            }
            new_pic.set_size_request(dw, dh);
            let nbx = ((ctx.w - dw) as f64) / 2.0;
            let nby = ((ctx.h - dh) as f64) / 2.0;
            ctx.stage
                .move_(&new_pic, nbx + direction_offset(ctx.dir, ctx.w), nby);
            started.set(true);
            if let Some(hid) = size_handler.borrow_mut().take() {
                m.disconnect(hid);
            }
            // Both frames placed and pre-scaled: travel together.
            start_slide_tick(
                &ctx,
                &new_pic,
                nbx,
                nby,
                0.0,
                -direction_offset(ctx.dir, ctx.w),
                state::SLIDE_MICROS,
            );
        }
    };
    // NOTE: the outgoing origin (obx/oby) is captured by the caller from
    // the live allocation; the tick below moves both frames together.
    {
        let maybe_start_c = maybe_start.clone();
        let id = media.connect_invalidate_size(move |_| {
            maybe_start_c();
        });
        *size_handler.borrow_mut() = Some(id);
    }
    {
        let maybe_start_c = maybe_start.clone();
        let weak_play = media.downgrade();
        media.connect_prepared_notify(move |_| {
            if let Some(m) = weak_play.upgrade() {
                m.play();
            }
            maybe_start_c();
        });
        if media.is_prepared() {
            media.play();
            maybe_start();
        }
    }
    {
        let ctx = ctx.clone();
        media.connect_error_notify(move |_| {
            abandon_slide(&ctx.state, ctx.my_gen);
        });
    }
    // Stuck pipeline (slow mount, missing codec with no error yet):
    // reveal the main view instead of covering it forever.
    {
        let ctx = ctx.clone();
        let started = started.clone();
        glib::timeout_add_local_once(Duration::from_millis(2000), move || {
            if !started.get() {
                abandon_slide(&ctx.state, ctx.my_gen);
            }
        });
    }
}

/// Animate both frames from `from_dx` to `to_dx` over `dur` microseconds
/// once both are placed (incoming pre-scaled). A fire-and-forget slide
/// runs 0→∓W in 220ms; a drag release settles from the finger offset over
/// a distance-scaled duration. Steady-state widgets are untouched.
fn start_slide_tick(
    ctx: &SlideCtx,
    new_pic: &gtk::Picture,
    nbx: f64,
    nby: f64,
    from_dx: f64,
    to_dx: f64,
    dur: i64,
) {
    let start = glib::monotonic_time();
    let dur = dur.max(1) as f64;
    let off = direction_offset(ctx.dir, ctx.w);
    let (obx, oby) = (ctx.obx, ctx.oby);
    let ctx_c = ctx.clone();
    let (old_c, new_c) = (ctx.old_pic.clone(), new_pic.clone());
    let stage_c = ctx.stage.clone();
    ctx.stage.add_tick_callback(move |_, _| {
        let now = glib::monotonic_time();
        let t = ((now - start) as f64 / dur).clamp(0.0, 1.0);
        // Ease-out cubic: fast start, gentle stop. Keeps perceived nav fast.
        let eased = 1.0 - (1.0 - t).powi(3);
        let dx = from_dx + (to_dx - from_dx) * eased;
        stage_c.move_(&old_c, obx + dx, oby);
        stage_c.move_(&new_c, nbx + off + dx, nby);
        if t >= 1.0 {
            finish_slide(&ctx_c.state, &ctx_c.overlay, &stage_c, ctx_c.my_gen);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

/// Remove a stale slide stage without touching a newer slide's state.
fn abandon_slide(state: &Rc<RefCell<AppState>>, my_gen: u64) {
    let s = state.borrow();
    if s.slide_gen != my_gen {
        return;
    }
    let (overlay, fixed) = (s.main_overlay.clone(), s.slide_fixed.clone());
    drop(s);
    if let (Some(overlay), Some(fixed)) = (overlay, fixed) {
        if fixed.parent().is_some() {
            overlay.remove_overlay(&fixed);
        }
    }
    let mut s = state.borrow_mut();
    if s.slide_gen == my_gen {
        s.slide_fixed = None;
        s.sliding = false;
    }
}

fn finish_slide(
    state: &Rc<RefCell<AppState>>,
    overlay: &gtk::Overlay,
    stage: &gtk::Fixed,
    my_gen: u64,
) {
    if stage.parent().is_some() {
        overlay.remove_overlay(stage);
    }
    let mut s = state.borrow_mut();
    if s.slide_gen == my_gen && s.slide_fixed.as_ref().is_some_and(|f| f == stage) {
        s.slide_fixed = None;
        s.sliding = false;
    }
}

/// Remove any running slide or drag overlay immediately (rapid-nav fast
/// path and all instant-switch paths: Home/End, trash, open, drop).
fn cancel_slide(state: &Rc<RefCell<AppState>>) {
    let (overlay, fixed) = {
        let mut s = state.borrow_mut();
        s.sliding = false;
        s.slide_gen = s.slide_gen.wrapping_add(1);
        s.drag = None;
        (s.main_overlay.clone(), s.slide_fixed.take())
    };
    if let (Some(overlay), Some(fixed)) = (overlay, fixed) {
        if fixed.parent().is_some() {
            overlay.remove_overlay(&fixed);
        }
    }
}

/// Instant index switch following the regular path (drag releases that
/// commit without an animation, e.g. unready incoming video).
fn commit_index(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
    new_index: usize,
) {
    {
        let mut s = state.borrow_mut();
        s.index = new_index;
        s.zoom = 1.0;
    }
    reset_scroll(scrolled);
    show_file(state, picture, scrolled, window);
}

/// Preferences dialog (HIG §Settings): switches persisted to the config
/// file so they survive restarts (Flatpak included).
fn show_preferences(state: &Rc<RefCell<AppState>>, window: &adw::ApplicationWindow) {
    let dialog = adw::PreferencesWindow::builder()
        .title("Preferences")
        .transient_for(window)
        .modal(true)
        .build();
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("emblem-system-symbolic")
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Transitions")
        .description("Slide animation between items")
        .build();
    let row = adw::SwitchRow::builder()
        .title("Slide animation")
        .subtitle("Animate transitions between items")
        .active(state.borrow().slide_enabled)
        .build();
    {
        let state = state.clone();
        row.connect_active_notify(move |r| {
            let enabled = r.is_active();
            state.borrow_mut().slide_enabled = enabled;
            state::save_slide_enabled(enabled);
        });
    }
    group.add(&row);
    page.add(&group);
    let nav_group = adw::PreferencesGroup::builder()
        .title("Navigation")
        .description("Swipe between items")
        .build();
    let fingers_row = adw::SwitchRow::builder()
        .title("Two-finger swipe")
        .subtitle("Use two fingers instead of three to switch items")
        .active(state.borrow().two_finger_swipe)
        .build();
    {
        let state = state.clone();
        fingers_row.connect_active_notify(move |r| {
            let enabled = r.is_active();
            state.borrow_mut().two_finger_swipe = enabled;
            state::save_two_finger_swipe(enabled);
        });
    }
    nav_group.add(&fingers_row);
    page.add(&nav_group);
    dialog.add(&page);
    dialog.present();
}

fn animations_enabled(state: &Rc<RefCell<AppState>>) -> bool {
    if !state.borrow().slide_enabled {
        return false;
    }
    gtk::Settings::default()
        .map(|s| s.property::<bool>("gtk-enable-animations"))
        .unwrap_or(true)
}

/// Finger count for swipe navigation from Preferences (2 when the
/// two-finger switch is on, else the default 3).
fn swipe_fingers(state: &Rc<RefCell<AppState>>) -> u32 {
    if state.borrow().two_finger_swipe {
        2
    } else {
        3
    }
}

/// Touchpad natural-scroll pref. Scroll deltas follow scroll direction
/// (mirrored vs finger motion when natural-scroll is on) while
/// TouchpadSwipe deltas are raw finger motion — the scroll-fed drag
/// unmirrors via this so both paths agree. Missing schema (non-GNOME)
/// degrades to unmirrored rather than failing.
fn touchpad_natural_scroll() -> bool {
    use std::cell::OnceCell;
    thread_local! {
        static SETTINGS: OnceCell<Option<gio::Settings>> = const { OnceCell::new() };
    }
    SETTINGS.with(|cell| {
        let settings = cell.get_or_init(|| {
            let present = gio::SettingsSchemaSource::default().is_some_and(|src| {
                src.lookup("org.gnome.desktop.peripherals.touchpad", false)
                    .is_some()
            });
            present.then(|| gio::Settings::new("org.gnome.desktop.peripherals.touchpad"))
        });
        settings
            .as_ref()
            .map(|s| s.boolean("natural-scroll"))
            .unwrap_or(false)
    })
}

/// Continuous touchpad swipes: while the fingers move, both the
/// outgoing and the incoming frame travel with them; release commits past
/// a quarter of the viewport (or on fling) and snaps back otherwise.
/// Gtk.GestureSwipe only fires after release, so raw TouchpadSwipe phases
/// drive the interaction; the swipe handler skips gestures consumed here
/// (see last_drag_us).
fn handle_touchpad(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
    event: &gdk::Event,
) -> glib::Propagation {
    if event.event_type() != gdk::EventType::TouchpadSwipe {
        return glib::Propagation::Proceed;
    }
    let Some(tp) = event.downcast_ref::<gdk::TouchpadEvent>() else {
        return glib::Propagation::Proceed;
    };
    if tp.n_fingers() != swipe_fingers(state) {
        return glib::Propagation::Proceed;
    }
    debug_log(&format!(
        "touchpad swipe: n={} phase={:?}",
        tp.n_fingers(),
        tp.gesture_phase()
    ));
    match tp.gesture_phase() {
        gdk::TouchpadGesturePhase::Begin => drag_begin(state, picture, scrolled),
        gdk::TouchpadGesturePhase::Update => {
            let (dx, _) = tp.deltas();
            drag_update(state, dx)
        }
        gdk::TouchpadGesturePhase::End => drag_end(state, picture, scrolled, window, false),
        gdk::TouchpadGesturePhase::Cancel => drag_end(state, picture, scrolled, window, true),
        _ => glib::Propagation::Proceed,
    }
}

/// Finger touched down: snapshot the outgoing frame onto a stage. The
/// direction (and the incoming frame) locks on first significant travel.
fn drag_begin(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
) -> glib::Propagation {
    let (vw, vh) = viewport_size(scrolled);
    if vw < 1.0 || vh < 1.0 || !animations_enabled(state) || state.borrow().zoom > 1.05 {
        // Zoomed: leave it to the discrete swipe (instant cut when zoomed).
        return glib::Propagation::Proceed;
    }
    let active = { state.borrow().sliding || state.borrow().drag.is_some() };
    if active {
        cancel_slide(state);
    }
    let w = vw as i32;
    let h = vh as i32;
    let Some(old) = old_frame(state, picture, w, h) else {
        return glib::Propagation::Proceed;
    };
    let Some(pieces) = build_stage(state, &old, w, h) else {
        return glib::Propagation::Proceed;
    };
    // (Index first: the struct assignment below holds a mutable borrow.)
    let index = state.borrow().index;
    state.borrow_mut().drag = Some(DragSt {
        stage: pieces.stage,
        old_pic: pieces.old_pic,
        new_pic: None,
        obx: pieces.obx,
        oby: pieces.oby,
        nbx: 0.0,
        nby: 0.0,
        w,
        h,
        vw,
        vh,
        dir: 0,
        new_index: index,
        accum: 0.0,
        ox: 0.0,
        locked: false,
        at_edge: false,
        visual: true,
        ready: false,
        is_video: false,
        samples: Vec::new(),
        gen: pieces.gen,
    });
    glib::Propagation::Stop
}

/// Finger moved: accumulate travel, lock a direction past the deadzone and
/// slide both frames. Returns Stop for the owned gesture stream.
fn drag_update(state: &Rc<RefCell<AppState>>, dx: f64) -> glib::Propagation {
    if state.borrow().drag.is_none() {
        // Begin didn't pass the gates: stay out of the way.
        return glib::Propagation::Proceed;
    }
    if !dx.is_finite() || dx == 0.0 {
        return glib::Propagation::Stop;
    }
    // Track under one borrow; widget moves after it ends.
    let locked_now = {
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else {
            return glib::Propagation::Stop;
        };
        d.accum += dx;
        let now = glib::monotonic_time();
        d.samples.push((now, d.accum));
        while d.samples.len() > 2 && now - d.samples[0].0 > 120_000 {
            d.samples.remove(0);
        }
        if !d.locked && d.accum.abs() >= state::SWIPE_LOCK_PX {
            d.locked = true;
            d.dir = if d.accum < 0.0 { 1 } else { -1 };
            true
        } else {
            d.locked
        }
    };
    if locked_now {
        drag_lock(state);
    }
    // (Fresh borrows: the RefMut above ended before any widget call.)
    let (stage, old_pic, new_pic, obx, oby, nbx, nby, off, visual) = {
        let s = state.borrow();
        let Some(d) = s.drag.as_ref() else {
            return glib::Propagation::Stop;
        };
        if !d.locked {
            return glib::Propagation::Stop;
        }
        (
            d.stage.clone(),
            d.old_pic.clone(),
            d.new_pic.clone(),
            d.obx,
            d.oby,
            d.nbx,
            d.nby,
            direction_offset(d.dir, d.w),
            d.visual,
        )
    };
    if !visual {
        return glib::Propagation::Stop;
    }
    let ox = {
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else {
            return glib::Propagation::Stop;
        };
        let w = d.w as f64;
        d.ox = if d.at_edge {
            (d.accum * 0.3).clamp(-w, w)
        } else {
            d.accum.clamp(-w, w)
        };
        d.ox
    };
    stage.move_(&old_pic, obx + ox, oby);
    if let Some(new_pic) = new_pic {
        stage.move_(&new_pic, nbx + off + ox, nby);
    }
    glib::Propagation::Stop
}

/// Lock the drag direction on first significant travel: bounds-check the
/// sibling and build its (pre-scaled) frame. Runs once per drag.
fn drag_lock(state: &Rc<RefCell<AppState>>) {
    let (dir, index, len) = {
        let s = state.borrow();
        let Some(d) = s.drag.as_ref() else { return };
        (d.dir, s.index, s.files.len())
    };
    // Sibling in the travel direction: swiping toward `dir` reveals the
    // item on that side. `dir` itself stays the travel direction, so
    // frames keep following the finger and fling/progress math is untouched.
    let target = index as i32 + dir;
    if target < 0 || (target as usize) >= len {
        // No sibling: rubber-band with resistance, release snaps back.
        // An invisible placeholder keeps the settle-back path uniform.
        let blank = gtk::Picture::new();
        blank.set_can_target(false);
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else { return };
        if !d.locked || d.new_pic.is_some() {
            return;
        }
        d.stage.put(&blank, 0.0, 0.0);
        d.new_pic = Some(blank);
        d.nbx = 0.0;
        d.nby = 0.0;
        d.at_edge = true;
        d.ready = true;
        return;
    }
    let new_index = target as usize;
    // Geometry snapshot for the incoming frame (viewport is stable enough
    // mid-drag that re-querying widgets is unnecessary).
    let (path, vw, vh) = {
        let s = state.borrow();
        let Some(d) = s.drag.as_ref() else { return };
        if d.new_pic.is_some() {
            return; // already built (repeated lock call)
        }
        (s.files[new_index].clone(), d.vw, d.vh)
    };
    let new_is_video = state::has_ext(&path, state::VIDEO_EXTS);
    // Peek before mutating (borrow discipline).
    let have_frame = new_is_video || state.borrow().prefetch.contains_key(&path);
    {
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else { return };
        d.new_index = new_index;
        if !have_frame {
            // Uncached: track blind, the release cuts instantly.
            d.visual = false;
            return;
        }
    }
    if new_is_video {
        drag_lock_video(state, &path, vw, vh, new_index);
        return;
    }
    // Image: use the prefetched frame as-is (prefetch stores EXIF-oriented
    // pixels; re-applying orientation would rotate portrait shots twice).
    // (Two statements: the clone must finish before pixels are used.)
    let raw = state.borrow().prefetch.get(&path).cloned();
    let Some(raw) = raw else {
        // Evicted between lock and decode: track blind, release cuts.
        if let Some(d) = state.borrow_mut().drag.as_mut() {
            d.visual = false;
        }
        return;
    };
    let (iw, ih) = (raw.width().max(1), raw.height().max(1));
    let tex: gdk::Paintable = media::texture_for_pixbuf(&raw).upcast();
    let (dw, dh) = zoom::display_size_for(iw as f64, ih as f64, vw, vh, 1.0);
    let (stage, w, vh, dir) = {
        let s = state.borrow();
        let Some(d) = s.drag.as_ref() else { return };
        (d.stage.clone(), d.w, d.vh, d.dir)
    };
    let new_pic = gtk::Picture::builder()
        .paintable(&tex)
        .content_fit(gtk::ContentFit::Contain)
        .width_request(dw)
        .height_request(dh)
        .can_shrink(true)
        .build();
    new_pic.set_can_target(false);
    let nbx = ((w - dw) as f64) / 2.0;
    let nby = (vh - dh as f64) / 2.0;
    stage.put(&new_pic, nbx + direction_offset(dir, w), nby);
    {
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else { return };
        // Re-check: a concurrent release may have cleared the drag.
        if !d.locked || d.new_pic.is_some() {
            return;
        }
        d.new_pic = Some(new_pic);
        d.nbx = nbx;
        d.nby = nby;
        d.ready = true;
    }
}

/// Incoming video for a drag: its own muted pipeline, marked ready (and
/// repositioned at the live finger offset) once sized. Weak refs only.
fn drag_lock_video(state: &Rc<RefCell<AppState>>, path: &Path, vw: f64, vh: f64, new_index: usize) {
    let media = gtk::MediaFile::for_filename(path);
    media.set_loop(true);
    media.set_muted(true);
    media.set_volume(1.0);
    let wrap = ZoomPaintable::new();
    wrap.set_inner(Some(media.clone().upcast()));
    let (stage, w, h, dir, gen) = {
        let s = state.borrow();
        let Some(d) = s.drag.as_ref() else { return };
        (d.stage.clone(), d.w, d.h, d.dir, d.gen)
    };
    let new_pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Fill)
        .width_request(w)
        .height_request(h)
        .can_shrink(true)
        .build();
    let paintable: gdk::Paintable = wrap.clone().upcast();
    new_pic.set_paintable(Some(&paintable));
    new_pic.set_can_target(false);
    stage.put(&new_pic, direction_offset(dir, w), 0.0);
    {
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else { return };
        if !d.locked || d.new_pic.is_some() {
            return;
        }
        d.new_pic = Some(new_pic.clone());
        d.is_video = true;
    }
    let started: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let size_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let weak_media = media.downgrade();
    let weak_wrap = wrap.downgrade();
    let maybe_ready = {
        let state = state.clone();
        let stage = stage.clone();
        let new_pic = new_pic.clone();
        let started = started.clone();
        let size_handler = size_handler.clone();
        move || {
            if started.get() {
                return;
            }
            let Some(m) = weak_media.upgrade() else {
                return;
            };
            let Some((sw, sh)) = zoom::video_intrinsic(&m) else {
                return;
            };
            {
                let s = state.borrow();
                if s.slide_gen != gen || s.index == new_index {
                    // Superseded, or main already committed: never touch.
                    return;
                }
                let Some(d) = s.drag.as_ref() else {
                    return;
                };
                if !d.locked {
                    return;
                }
            }
            let (dw, dh) = zoom::display_size_for(sw, sh, vw, vh, 1.0);
            let (fw, fh) = zoom::fit_size(sw, sh, vw, vh);
            if let Some(wp) = weak_wrap.upgrade() {
                wp.set_view(fw, fh, 1.0);
            }
            new_pic.set_size_request(dw, dh);
            let nbx = ((w - dw) as f64) / 2.0;
            let nby = ((h - dh) as f64) / 2.0;
            // Reposition at the live finger offset (the drag kept moving).
            let ox = state.borrow().drag.as_ref().map(|d| d.ox).unwrap_or(0.0);
            stage.move_(&new_pic, nbx + direction_offset(dir, w) + ox, nby);
            started.set(true);
            if let Some(hid) = size_handler.borrow_mut().take() {
                m.disconnect(hid);
            }
            let mut s = state.borrow_mut();
            if s.slide_gen == gen {
                if let Some(d) = s.drag.as_mut() {
                    d.nbx = nbx;
                    d.nby = nby;
                    d.ready = true;
                }
            }
        }
    };
    {
        let maybe_ready_c = maybe_ready.clone();
        let id = media.connect_invalidate_size(move |_| {
            maybe_ready_c();
        });
        *size_handler.borrow_mut() = Some(id);
    }
    {
        let maybe_ready_c = maybe_ready.clone();
        let weak_play = media.downgrade();
        media.connect_prepared_notify(move |_| {
            if let Some(m) = weak_play.upgrade() {
                m.play();
            }
            maybe_ready_c();
        });
        if media.is_prepared() {
            media.play();
            maybe_ready();
        }
    }
    {
        let state = state.clone();
        media.connect_error_notify(move |_| {
            // Release will reveal the main view; mark unready now.
            let mut s = state.borrow_mut();
            if s.slide_gen == gen {
                if let Some(d) = s.drag.as_mut() {
                    d.ready = false;
                    d.visual = false;
                }
            }
        });
    }
}

/// Finger lifted (or gesture cancelled): commit past a quarter of the
/// viewport (or on fling) with a distance-scaled settle, else snap back.
fn drag_end(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
    cancelled: bool,
) -> glib::Propagation {
    let drag = state.borrow_mut().drag.take();
    let Some(d) = drag else {
        return glib::Propagation::Proceed;
    };
    if !d.locked {
        // Micro-gesture: reveal, no navigation.
        abandon_slide(state, d.gen);
        return glib::Propagation::Stop;
    }
    state.borrow_mut().last_drag_us = glib::monotonic_time();
    let w_f = d.w as f64;
    let progress = d.ox.abs() / w_f.max(1.0);
    let velocity = state::fling_velocity(&d.samples);
    let commit = !d.at_edge && !cancelled && state::fling_complete(progress, velocity, d.dir);
    if commit {
        commit_index(state, picture, scrolled, window, d.new_index);
    }
    if !d.visual || (commit && d.is_video && !d.ready) {
        // Nothing (yet) to travel with: reveal the main view instantly.
        abandon_slide(state, d.gen);
        return glib::Propagation::Stop;
    }
    let Some(new_pic) = d.new_pic else {
        abandon_slide(state, d.gen);
        return glib::Propagation::Stop;
    };
    let overlay = { state.borrow().main_overlay.clone() };
    let Some(overlay) = overlay else {
        abandon_slide(state, d.gen);
        return glib::Propagation::Stop;
    };
    let to_dx = if commit {
        -direction_offset(d.dir, d.w)
    } else {
        0.0
    };
    let dur = state::settle_micros((to_dx - d.ox).abs(), w_f);
    // SlideCtx carries what the tick needs; geometry comes from the drag.
    let ctx = SlideCtx {
        state: state.clone(),
        overlay,
        stage: d.stage,
        old_pic: d.old_pic,
        obx: d.obx,
        oby: d.oby,
        w: d.w,
        h: 0,
        vw: 0.0,
        vh: 0.0,
        dir: d.dir,
        my_gen: d.gen,
        new_index: d.new_index,
    };
    // Re-register: abandon_slide/finish_slide key off slide_fixed + gen.
    {
        let mut s = state.borrow_mut();
        if s.slide_gen == d.gen {
            s.slide_fixed = Some(ctx.stage.clone());
            s.sliding = true;
        } else {
            return glib::Propagation::Stop;
        }
    }
    start_slide_tick(&ctx, &new_pic, d.nbx, d.nby, d.ox, to_dx, dur);
    glib::Propagation::Stop
}

/// Move the current file to Trash and advance to the next sibling.
///
/// The index is NOT reset: after removal the next file slides into the
/// same index (or the previous one if the last item was trashed).
fn trash_current(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
) {
    let path = {
        let s = state.borrow();
        if s.files.is_empty() {
            return;
        }
        s.files[s.index].clone()
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    // GIO trash (goes to Trash, not permanent delete). Needs write access
    // to the containing directory (Flatpak: --filesystem=host:rw).
    let file = gio::File::for_path(&path);
    if let Err(e) = file.trash(None::<&gio::Cancellable>) {
        debug_log(&format!("trash failed for {}: {e}", path.display()));
        if is_doc_portal_path(&path) {
            show_toast(
                state,
                "Cannot move to Trash from sandbox portal — use the Files app",
            );
        } else {
            show_toast(state, &format!("Cannot move {name} to Trash"));
        }
        return;
    }

    // (Two statements: the position lookup must finish before the mutable
    // borrow below, else RefCell panics.)
    let pos = state.borrow().files.iter().position(|f| f == &path);
    cancel_slide(state);
    {
        let mut s = state.borrow_mut();
        if let Some(pos) = pos {
            s.files.remove(pos);
            s.prefetch.remove(&path);
            s.index = state::index_after_removal(s.files.len(), s.index, pos);
            s.zoom = 1.0;
        }
    }
    reset_scroll(scrolled);
    show_file(state, picture, scrolled, window);
    if !state.borrow().files.is_empty() {
        show_toast(state, &format!("Moved {name} to Trash"));
    }
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
            // Show empty state
            if let Some(ref st) = s.empty_status {
                st.set_visible(true);
            }
            scrolled.set_visible(false);
            window.set_title(Some("Carosello"));
            if let Some(ref wt) = s.window_title {
                wt.set_title("Carosello");
                wt.set_subtitle("");
            }
            return;
        }
        (s.files[s.index].clone(), s.index, s.files.len())
    };

    // Ensure content visible, empty hidden
    if let Some(ref st) = state.borrow().empty_status.clone() {
        st.set_visible(false);
    }
    scrolled.set_visible(true);

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    // HIG: WindowTitle shows filename + counter as subtitle, WM title is just filename
    window.set_title(Some(&name));
    if let Some(ref wt) = state.borrow().window_title.clone() {
        wt.set_title(&name);
        wt.set_subtitle(&format!("{} of {}", idx + 1, total));
    }

    // Single file via the sandbox document portal: siblings are not visible
    // by design, so explain how to browse the whole folder instead.
    if total == 1 && is_doc_portal_path(&path) {
        show_toast(
            state,
            "Single file from sandbox portal — use Open Folder to browse all",
        );
    }

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
    // Pause outside the borrow: pause() synchronously emits playing
    // notifies whose handlers borrow state (update_seek_ui) — pausing
    // while mutably borrowed panics.
    let old_media = state.borrow().media_file.clone();
    if let Some(ref old) = old_media {
        old.pause();
    }
    {
        let mut s = state.borrow_mut();
        s.video_gen = s.video_gen.wrapping_add(1);
        s.media_file = None;
        s.zoom = 1.0;
        s.is_video = false;
        s.image_gen = s.image_gen.wrapping_add(1);
        s.image_w = 0;
        s.image_h = 0;
        s.original_pixbuf = None;
    }

    if let Some(ref h) = state.borrow().bottom_bar {
        h.set_visible(false);
    }

    let path_buf = path.to_path_buf();
    let gen = state.borrow().image_gen;

    // Fast path: prefetched neighbor already decoded.
    // (Two statements: the RefMut from the remove must drop before the
    // body borrows state again, else RefCell panics.)
    let cached = state.borrow_mut().prefetch.remove(&path_buf);
    if let Some(cached) = cached {
        let w = cached.width().max(1);
        let h = cached.height().max(1);
        let tex = media::texture_for_pixbuf(&cached);
        {
            let mut s = state.borrow_mut();
            if s.image_gen != gen {
                return;
            }
            s.image_w = w;
            s.image_h = h;
            // Drop CPU pixels after GPU upload; dimensions retained for zoom.
            s.original_pixbuf = None;
        }
        scrolled.set_child(Some(picture));
        picture.set_paintable(Some(&tex));
        picture.set_content_fit(gtk::ContentFit::Contain);
        reset_scroll(scrolled);
        schedule_update(state, picture, scrolled);
        prefetch_neighbors(state);
        return;
    }

    // Async decode via GIO (never blocks the UI thread on large files).
    // Stale results are discarded via image_gen. EXIF read is a tiny header
    // parse kept synchronous after decode.
    let state_c = state.clone();
    let picture_c = picture.clone();
    let scrolled_c = scrolled.clone();
    let path_c = path_buf.clone();
    let file = gio::File::for_path(&path_buf);
    file.read_async(
        glib::Priority::DEFAULT,
        None::<&gio::Cancellable>,
        move |res| {
            let Ok(stream) = res else {
                if state_c.borrow().image_gen != gen {
                    return;
                }
                picture_c.set_paintable(None::<&gdk::Texture>);
                show_toast(
                    &state_c,
                    &format!(
                        "Failed to load {}",
                        path_c.file_name().unwrap_or_default().to_string_lossy()
                    ),
                );
                return;
            };
            let state_c2 = state_c.clone();
            let picture_c2 = picture_c.clone();
            let scrolled_c2 = scrolled_c.clone();
            gdk_pixbuf::Pixbuf::from_stream_async(&stream, None::<&gio::Cancellable>, move |res| {
                if state_c2.borrow().image_gen != gen {
                    return;
                }
                match res {
                    Ok(raw) => {
                        let orientation = media::read_exif_orientation(&path_c);
                        let pixbuf = media::apply_orientation(&raw, orientation);
                        let w = pixbuf.width().max(1);
                        let h = pixbuf.height().max(1);
                        let tex = media::texture_for_pixbuf(&pixbuf);
                        {
                            let mut s = state_c2.borrow_mut();
                            s.image_w = w;
                            s.image_h = h;
                            s.original_pixbuf = None;
                        }
                        scrolled_c2.set_child(Some(&picture_c2));
                        picture_c2.set_paintable(Some(&tex));
                        picture_c2.set_content_fit(gtk::ContentFit::Contain);
                        reset_scroll(&scrolled_c2);
                        schedule_update(&state_c2, &picture_c2, &scrolled_c2);
                        prefetch_neighbors(&state_c2);
                    }
                    Err(e) => {
                        debug_log(&format!("Failed to load: {}", e));
                        picture_c2.set_paintable(None::<&gdk::Texture>);
                        show_toast(
                            &state_c2,
                            &format!(
                                "Failed to load {}",
                                path_c.file_name().unwrap_or_default().to_string_lossy()
                            ),
                        );
                    }
                }
            });
        },
    );
}

/// Decode next/prev images in the background so navigation feels instant.
/// Bounded to 2 entries; GIO-async so the UI never blocks.
fn prefetch_neighbors(state: &Rc<RefCell<AppState>>) {
    let (files, index) = {
        let s = state.borrow();
        (s.files.clone(), s.index)
    };
    if files.is_empty() {
        return;
    }
    let mut targets = Vec::new();
    if index + 1 < files.len() {
        targets.push(files[index + 1].clone());
    }
    if index > 0 {
        targets.push(files[index - 1].clone());
    }
    for target in targets {
        if !state::has_ext(&target, state::IMAGE_EXTS) {
            continue;
        }
        if state.borrow().prefetch.contains_key(&target) {
            continue;
        }
        let state_c = state.clone();
        let file = gio::File::for_path(&target);
        file.read_async(glib::Priority::LOW, None::<&gio::Cancellable>, move |res| {
            let Ok(stream) = res else { return };
            let state_c2 = state_c.clone();
            gdk_pixbuf::Pixbuf::from_stream_async(&stream, None::<&gio::Cancellable>, move |res| {
                let Ok(raw) = res else { return };
                let orientation = media::read_exif_orientation(&target);
                let pixbuf = media::apply_orientation(&raw, orientation);
                let mut s = state_c2.borrow_mut();
                if s.prefetch.contains_key(&target) {
                    return;
                }
                while s.prefetch.len() >= 2 {
                    if let Some(k) = s.prefetch.keys().next().cloned() {
                        s.prefetch.remove(&k);
                    } else {
                        break;
                    }
                }
                s.prefetch.insert(target, pixbuf);
            });
        });
    }
}

fn viewport_size(scrolled: &gtk::ScrolledWindow) -> (f64, f64) {
    let mut vw = scrolled.width() as f64;
    let mut vh = scrolled.height() as f64;
    if vw < 1.0 || vh < 1.0 {
        if let Some(root) = scrolled.root() {
            // Try to fall back to toplevel window size (available window space)
            if let Some(win) = root.downcast_ref::<gtk::Window>() {
                let w = win.width() as f64;
                let h = win.height() as f64;
                if w >= 1.0 && h >= 1.0 {
                    // Window includes decorations/header overlay; approximate viewport
                    // by using window size. Scrolled is full-window overlay, so this is accurate.
                    vw = w;
                    vh = h;
                }
            }
        }
    }
    (vw, vh)
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
        s.image_w,
        s.image_h,
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
    let (vw, vh) = viewport_size(scrolled);
    if vw < 1.0 || vh < 1.0 {
        return;
    }

    let (dw, dh) = zoom::display_size_for(iw, ih, vw, vh, zoom);
    debug_log(&format!(
        "update_display: is_video={is_video} intrinsic={iw:.0}x{ih:.0} viewport={vw:.0}x{vh:.0} zoom={zoom:.2} display={dw}x{dh}"
    ));
    if is_video {
        let zp = state.borrow().video_view.clone();
        let (fw, fh) = zoom::fit_size(iw, ih, vw, vh);
        zp.set_view(fw, fh, zoom);
        picture.set_content_fit(gtk::ContentFit::Fill);
        picture.set_size_request(dw, dh);
        if picture.paintable().is_none() {
            let zp_paintable: gdk::Paintable = zp.clone().upcast();
            picture.set_paintable(Some(&zp_paintable));
        }
    } else {
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_size_request(dw, dh);
        // Paintable is set at load time; no pixbuf retained (memory: GPU copy only).
    }
}

fn show_video(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    path: &Path,
) {
    // Pause outside the borrow: pause() synchronously emits playing
    // notifies whose handlers borrow state (update_seek_ui) — pausing
    // while mutably borrowed panics.
    let old_media = state.borrow().media_file.clone();
    if let Some(ref old) = old_media {
        old.pause();
    }
    let mut s = state.borrow_mut();
    s.original_pixbuf = None;
    s.image_w = 0;
    s.image_h = 0;
    // Invalidate any in-flight async image load.
    s.image_gen = s.image_gen.wrapping_add(1);
    s.zoom = 1.0;
    s.is_video = true;
    s.seeking = false;
    s.video_gen = s.video_gen.wrapping_add(1);
    s.video_w = 0;
    s.video_h = 0;
    // gen kept for size-discovery guard below.

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

    state.borrow_mut().media_file = Some(media.clone());

    // Delay play() until the GStreamer pipeline is prepared
    {
        let media_clone = media.clone();
        media.connect_prepared_notify(move |_| {
            media_clone.play();
        });
        if media.is_prepared() {
            media.play();
        }
    }

    let zp = state.borrow().video_view.clone();
    zp.set_inner(Some(media.clone().upcast()));
    scrolled.set_child(Some(picture));
    picture.set_content_fit(gtk::ContentFit::Fill);
    let zp_paintable: gdk::Paintable = zp.clone().upcast();
    picture.set_paintable(Some(&zp_paintable));
    scrolled.hadjustment().set_value(0.0);
    scrolled.vadjustment().set_value(0.0);
    schedule_update(state, picture, scrolled);

    // When the video's intrinsic size becomes known, re-fit.
    // Size discovery disconnects after the first hit (no per-frame work).
    {
        let state_c = state.clone();
        let picture_c = picture.clone();
        let scrolled_c = scrolled.clone();
        let media_c = media.clone();
        let size_handler = Rc::new(RefCell::new(None::<glib::SignalHandlerId>));
        let size_handler_c = size_handler.clone();
        let id = media.connect_invalidate_size(move |_| {
            if let Some((w, h)) = zoom::video_intrinsic(&media_c) {
                let (wi, hi) = (w as i32, h as i32);
                let changed = {
                    let mut s = state_c.borrow_mut();
                    if (wi, hi) != (s.video_w, s.video_h) {
                        s.video_w = wi;
                        s.video_h = hi;
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    debug_log(&format!("video: size discovered {wi}x{hi}"));
                    schedule_update(&state_c, &picture_c, &scrolled_c);
                    // Size known: stop listening (one-shot).
                    if let Some(hid) = size_handler_c.borrow_mut().take() {
                        media_c.disconnect(hid);
                    }
                }
            } else {
                schedule_update(&state_c, &picture_c, &scrolled_c);
            }
        });
        *size_handler.borrow_mut() = Some(id);
    }

    // Signal-driven seek UI: timestamp/duration/playing notifies replace the
    // old 200ms poll (idle even when paused). Seeking flag clears on seek-done.
    {
        let state_c = state.clone();
        media.connect_timestamp_notify(move |_| {
            update_seek_ui(&state_c);
        });
    }
    {
        let state_c = state.clone();
        media.connect_duration_notify(move |_| {
            update_seek_ui(&state_c);
        });
    }
    {
        let state_c = state.clone();
        media.connect_playing_notify(move |_| {
            update_seek_ui(&state_c);
        });
    }
    {
        let state_c = state.clone();
        media.connect_seeking_notify(move |m| {
            if !m.is_seeking() {
                state_c.borrow_mut().seeking = false;
            }
            update_seek_ui(&state_c);
        });
    }
    // Error feedback (previously silent black frame).
    {
        let state_c = state.clone();
        media.connect_error_notify(move |m| {
            if let Some(err) = m.error() {
                debug_log(&format!("video error: {err:?}"));
                show_toast(&state_c, "Failed to play video (missing codec?)");
            }
        });
    }
    update_seek_ui(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_media_gio_matches_read_dir() {
        let dir = std::env::temp_dir().join(format!("carosello-gio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["IMG2.jpg", "IMG10.jpg", "img01.jpg", "note.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let via_fs = state::collect_media(&dir);
        let via_gio = collect_media_gio(&gio::File::for_path(&dir));
        assert_eq!(via_gio, via_fs);
        let names: Vec<String> = via_gio
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["img01.jpg", "IMG2.jpg", "IMG10.jpg"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_collect_media_gio_missing_dir_empty() {
        let missing = std::env::temp_dir().join("carosello-gio-nonexistent-xyz");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(collect_media_gio(&gio::File::for_path(&missing)).is_empty());
    }
}
