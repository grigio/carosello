use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use libadwaita as adw;

use crate::css;
use crate::state::{self, debug_log, format_time, is_media};
use crate::transform;
use crate::zoom::{self, ZoomPaintable};

struct AppState {
    files: Vec<PathBuf>,
    index: usize,
    image_w: i32,
    image_h: i32,
    image_gen: u64,
    /// FIFO of decoded neighbor frames (path, texture), oldest first,
    /// capped at 2 — see `prefetch_*` below. Textures, not pixbufs: the
    /// slide/drag paths and the main view all render the same GPU copy
    /// (report #4 used to upload each frame twice).
    prefetch: VecDeque<(PathBuf, gdk::Texture)>,
    /// Cancels the in-flight display decode (replaced on every
    /// `show_image`/`show_video`, after the `image_gen` bump).
    decode_cancel: Option<gio::Cancellable>,
    /// Cancels the current prefetch round (one round per navigation).
    prefetch_cancel: Option<gio::Cancellable>,
    /// Path whose decoded frame currently sits in the main `picture`.
    /// Updated only where the paintable is set, so `stash_outgoing` can
    /// pair the outgoing texture with the right path before a replace.
    shown_path: Option<PathBuf>,
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
    /// Whether the pointer position above was ever observed (`motion` or
    /// `enter` on the overlay). Distinguishes "cursor at (0,0)" from "never
    /// seen": a window opening under a stationary cursor plus a tap-to-click
    /// double-tap delivers no motion at all, and an unseen (0,0) must never
    /// be used as a zoom anchor (it scrolls to the top-left corner).
    mouse_seen: bool,
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
    /// Rotate/mirror header buttons: kept here to enable/disable them
    /// (disabled for videos, empty folder, and while a save is in flight).
    transform_btns: Vec<gtk::Button>,
    /// A transform is being computed or written to disk; blocks new
    /// transforms and Move to Trash until the write completes.
    saving: bool,
    /// A `trash_async`/`delete_async` request is in flight (blocks a
    /// second one; the delete is the no-Trash fallback of the trash).
    trashing: bool,
    /// Last state pushed to the seek bar/labels — `(ts_s, dur_s, bar_unit,
    /// playing, seeking)`; `update_seek_ui` early-outs when unchanged
    /// (report #9). `bar_unit = ts * 500 / dur` = the seek bar's own
    /// granularity, so the handle still moves smoothly at pixel steps.
    last_seek_ui: Option<(i64, i64, i64, bool, bool)>,
    /// Last layout `update_display` applied — `(dw, dh, is_video, fit_w,
    /// fit_h, zoom)`; skip re-setting identical `content_fit` /
    /// `size_request` / view values (report: lower-impact polish).
    last_layout: Option<(i32, i32, bool, f64, f64, f64)>,
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

impl AppState {
    /// Borrowed texture clone for a cached neighbor (slide/drag *peek*:
    /// the entry stays for `prefetch_take` in the main view).
    fn prefetch_get(&self, path: &Path) -> Option<gdk::Texture> {
        self.prefetch
            .iter()
            .find(|(p, _)| p.as_path() == path)
            .map(|(_, t)| t.clone())
    }

    fn prefetch_contains(&self, path: &Path) -> bool {
        self.prefetch.iter().any(|(p, _)| p.as_path() == path)
    }

    /// Consume the cached neighbor (the main view takes it for itself).
    fn prefetch_take(&mut self, path: &Path) -> Option<gdk::Texture> {
        let pos = self
            .prefetch
            .iter()
            .position(|(p, _)| p.as_path() == path)?;
        self.prefetch.remove(pos).map(|(_, t)| t)
    }

    /// Drop a cached neighbor (file trashed / transformed since decode).
    fn prefetch_drop(&mut self, path: &Path) {
        if let Some(pos) = self.prefetch.iter().position(|(p, _)| p.as_path() == path) {
            self.prefetch.remove(pos);
        }
        // Cache mutation reopens the round epoch: the transform re-show
        // calls show_image directly (no show_file entry-cancel), so the
        // guard in prefetch_neighbors must be free to start a fresh round.
        // Take + cancel: an in-flight round may hold the dropped path.
        if let Some(c) = self.prefetch_cancel.take() {
            c.cancel();
        }
    }

    /// Store a decoded neighbor, evicting the oldest first (FIFO — with
    /// only 2 slots an arbitrary key could evict the fresh true neighbor,
    /// report #3.3).
    fn prefetch_store(&mut self, path: &Path, tex: gdk::Texture) {
        if self.prefetch_contains(path) {
            return;
        }
        while self.prefetch.len() >= 2 {
            self.prefetch.pop_front();
        }
        self.prefetch.push_back((path.to_path_buf(), tex));
    }
}

pub fn build(app: &adw::Application, start: Option<&Path>) -> adw::ApplicationWindow {
    css::load_css();

    // Both prefs from a single settings read (report #8: was two reads).
    let (slide_enabled, two_finger_swipe) = state::load_prefs();

    let state: Rc<RefCell<AppState>> = Rc::new(RefCell::new(AppState {
        files: Vec::new(),
        index: 0,
        image_w: 0,
        image_h: 0,
        image_gen: 0,
        prefetch: VecDeque::new(),
        decode_cancel: None,
        prefetch_cancel: None,
        shown_path: None,
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
        mouse_seen: false,
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
        slide_enabled,
        two_finger_swipe,
        transform_btns: Vec::new(),
        saving: false,
        trashing: false,
        last_seek_ui: None,
        last_layout: None,
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

    // ── Top-right: transforms, zoom + menu (visual order, left → right) ──
    // NOTE: AdwHeaderBar::pack_end PREPENDS to the end box, so the final
    // look is the reverse of the packing order. Widgets are created here in
    // visual order and packed in reverse at the end of this section, so the
    // header reads [rotate-left][rotate-right][mirror][zoom-out]
    // [zoom-reset][zoom-in][menu][window controls].
    let rotate_left_btn = gtk::Button::builder()
        .icon_name("object-rotate-left-symbolic")
        .tooltip_text("Rotate Left")
        .css_classes(["flat"])
        .build();
    rotate_left_btn.update_property(&[gtk::accessible::Property::Label("Rotate Left")]);
    rotate_left_btn.set_accessible_role(gtk::AccessibleRole::Button);
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        rotate_left_btn.connect_clicked(move |_| {
            run_transform(
                &state,
                &picture,
                &scrolled,
                transform::Transform::RotateLeft,
            );
        });
    }

    let rotate_right_btn = gtk::Button::builder()
        .icon_name("object-rotate-right-symbolic")
        .tooltip_text("Rotate Right")
        .css_classes(["flat"])
        .build();
    rotate_right_btn.update_property(&[gtk::accessible::Property::Label("Rotate Right")]);
    rotate_right_btn.set_accessible_role(gtk::AccessibleRole::Button);
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        rotate_right_btn.connect_clicked(move |_| {
            run_transform(
                &state,
                &picture,
                &scrolled,
                transform::Transform::RotateRight,
            );
        });
    }

    let mirror_btn = gtk::Button::builder()
        .icon_name("object-flip-horizontal-symbolic")
        .tooltip_text("Mirror Horizontally")
        .css_classes(["flat"])
        .build();
    mirror_btn.update_property(&[gtk::accessible::Property::Label("Mirror Horizontally")]);
    mirror_btn.set_accessible_role(gtk::AccessibleRole::Button);
    {
        let state = state.clone();
        let picture = picture.clone();
        let scrolled = scrolled.clone();
        mirror_btn.connect_clicked(move |_| {
            run_transform(&state, &picture, &scrolled, transform::Transform::MirrorH);
        });
    }

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
            debug_log!("zoom-reset: button");
            zoom_to(&state, &picture, &scrolled, 1.0, None);
        });
    }

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

    // Transform buttons live in state so sync_transform_buttons() can flip
    // their sensitivity (video / empty folder / save in flight).
    state.borrow_mut().transform_btns = vec![
        rotate_left_btn.clone(),
        rotate_right_btn.clone(),
        mirror_btn.clone(),
    ];

    // Pack in REVERSE visual order (pack_end prepends to the end box, and
    // the window controls are appended by libadwaita last).
    header_bar.pack_end(&menu_btn);
    header_bar.pack_end(&zoom_in_btn);
    header_bar.pack_end(&zoom_reset_btn);
    header_bar.pack_end(&zoom_out_btn);
    header_bar.pack_end(&mirror_btn);
    header_bar.pack_end(&rotate_right_btn);
    header_bar.pack_end(&rotate_left_btn);

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
        // Monotonic µs when `hide_id` was last armed (report #7 throttle).
        let hide_armed_us: Rc<Cell<i64>> = Rc::new(Cell::new(0));

        let motion_ctrl = gtk::EventControllerMotion::new();
        {
            let top = top.clone();
            let bottom = bottom.clone();
            let hide_id = hide_id.clone();
            let hide_armed_us = hide_armed_us.clone();
            let state = state.clone();
            let window_ref = window.clone();
            let menu = menu_btn.clone();
            motion_ctrl.connect_motion(move |_, x, y| {
                {
                    let mut s = state.borrow_mut();
                    s.mouse_x = x;
                    s.mouse_y = y;
                    s.mouse_seen = true;
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
                    arm_hide(
                        &top,
                        &bottom,
                        &hide_id,
                        &hide_armed_us,
                        FADE_DELAY_MS,
                        &menu,
                    );
                }
            });
        }
        // `enter` must record the cursor too: a window that opens under a
        // stationary cursor (or a tap-to-click double-tap, which generates
        // no motion) only ever delivers `enter`. Without this, mouse_x/mouse_y
        // stayed (0,0) and the double-click/pinch anchor landed top-left.
        {
            let state = state.clone();
            motion_ctrl.connect_enter(move |_, x, y| {
                let mut s = state.borrow_mut();
                s.mouse_x = x;
                s.mouse_y = y;
                s.mouse_seen = true;
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
                let hide_armed_us = hide_armed_us.clone();
                let menu = menu_btn.clone();
                motion_top.connect_leave(move |_| {
                    arm_hide(
                        &top,
                        &bottom,
                        &hide_id,
                        &hide_armed_us,
                        FADE_DELAY_MS,
                        &menu,
                    );
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
                let hide_armed_us = hide_armed_us.clone();
                let menu = menu_btn.clone();
                motion_bottom.connect_leave(move |_| {
                    arm_hide(
                        &top,
                        &bottom,
                        &hide_id,
                        &hide_armed_us,
                        FADE_DELAY_MS,
                        &menu,
                    );
                });
            }
            bottom_outer.add_controller(motion_bottom);
        }

        // While the menu popup is open the panel stays visible.
        {
            let top = top_handle.clone();
            let bottom = bottom_outer.clone();
            let hide_id = hide_id.clone();
            let hide_armed_us = hide_armed_us.clone();
            menu_btn.connect_active_notify(move |btn| {
                if btn.is_active() {
                    cancel_hide(&hide_id);
                    top.set_opacity(1.0);
                    if bottom.is_visible() {
                        bottom.set_opacity(1.0);
                    }
                } else {
                    arm_hide(&top, &bottom, &hide_id, &hide_armed_us, FADE_DELAY_MS, btn);
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
                    debug_log!(format!(
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
                .body("Navigation:\n  ← / →, Page Up / Down  —  Previous / Next\n  Home / End  —  First / Last\n  3-finger swipe (2-finger if enabled in Preferences)  —  Previous / Next\n\nZoom:\n  Ctrl + + / −  —  Zoom In / Out\n  Ctrl + 0  —  Reset Zoom\n  Ctrl + Scroll  —  Zoom\n  Pinch  —  Zoom\n  Double-click  —  Toggle 2.5×\n  Drag  —  Pan when zoomed\n\nView:\n  F11 / F  —  Fullscreen\n  Esc  —  Exit Fullscreen, else Reset Zoom\n\nVideo:\n  Space / K  —  Play / Pause\n  M  —  Mute\n  [ / ]  —  Seek 5 s\n  Click seek bar  —  Seek\n\nFile:\n  Del  —  Move to Trash (or delete it when there is no Trash)\n\nApplication:\n  Ctrl + O  —  Open File\n  Ctrl + Shift + O  —  Open Folder\n  Ctrl + Q  —  Quit\n  Ctrl + W  —  Close\n  Ctrl + ? / Ctrl + K  —  This Help\n  F1  —  About")
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
        zoom_reset.connect_activate(move |_, _| {
            debug_log!("zoom-reset: accel action");
            zoom_to(&state, &picture, &scrolled, 1.0, None);
        });
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
    } else {
        // Transform buttons start disabled in an empty folder (show_file
        // only runs when there is a file).
        sync_transform_buttons(&state);
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
                debug_log!("zoom-reset: key Ctrl+0");
                zoom_to(&state, &picture, &scrolled, 1.0, None);
                glib::Propagation::Stop
            }
            // Escape: leave fullscreen first (standard viewer behaviour),
            // otherwise reset the zoom, otherwise let it bubble (e.g. dialogs).
            gdk::Key::Escape => {
                if window.is_fullscreen() {
                    debug_log!("fullscreen-exit: key Escape");
                    window.set_fullscreened(false);
                    return glib::Propagation::Stop;
                }
                let z = state.borrow().zoom;
                if (z - 1.0).abs() > 0.01 {
                    debug_log!("zoom-reset: key Escape");
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
                    debug_log!("scroll-swipe: settle");
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
                debug_log!(format!("scroll-swipe: begin (dx={dx:.1})"));
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
                debug_log!(format!(
                    "pinch: base={:.2} scale={scale:.3} -> target={target:.2}",
                    base_zoom.get()
                ));
                let (mx, my, seen) = {
                    let s = state.borrow();
                    (s.mouse_x, s.mouse_y, s.mouse_seen)
                };
                // (0,0) is the stale/never-seen marker for either source (see
                // the double-click handler): prefer the source that is sane
                // for this device kind, fall back to the other one.
                let sane = |p: (f64, f64)| !(p.0 == 0.0 && p.1 == 0.0);
                let cursor = seen.then_some((mx, my)).filter(|p| sane(*p));
                let point = gesture.point(None).filter(|p| sane(*p));
                let anchor = if gesture.device().is_some_and(|d| d.has_cursor()) {
                    cursor.or(point)
                } else {
                    point.or(cursor)
                };
                zoom_to(&state, &picture, &scrolled, target, anchor);
            });
        }
    }
    scrolled.add_controller(zoom_gesture);

    // ── Drag-to-pan when zoomed ──
    // Incremental deltas (`off − prev`), never an absolute `start − off`
    // origin snapshotted at press time: the press that *triggers* a
    // double-click zoom runs while the adjustments still hold the fit
    // range — layout grows them later and the anchor poller only positions
    // the scroll ~100 ms after — so that origin reads (0,0), and any finger
    // micro-motion while the button is still down then slammed the view
    // back to the top-left corner right after `zoom-settled` ("zooms
    // correctly, then immediately switches to top-left"; touchpad taps and
    // trackpad presses always carry such motion, a mechanical mouse
    // double-click does not). Shifting by the per-update delta only moves
    // the view by real finger motion, so the just-applied anchor survives.
    // Diagnose with CAROSELLO_DEBUG=1: `pan-begin` / `pan-update` around a
    // `zoom-settled`.
    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        // Previous drag offset (GtkGestureDrag offsets are relative to the
        // drag start): delta = off − prev. Reset on every begin so a stale
        // origin from an earlier drag can't turn the first update into a
        // jump.
        let prev: Rc<Cell<(f64, f64)>> = Rc::new(Cell::new((0.0, 0.0)));
        {
            let prev = prev.clone();
            let state = state.clone();
            drag.connect_drag_begin(move |_, _, _| {
                prev.set((0.0, 0.0));
                let z = state.borrow().zoom;
                if z > 1.05 {
                    debug_log!(format!("pan-begin: zoom={z:.2}"));
                }
            });
        }
        {
            let scrolled = scrolled.clone();
            let prev = prev.clone();
            let state = state.clone();
            drag.connect_drag_update(move |_, off_x, off_y| {
                // Track the finger even while below the pan threshold, so a
                // zoom starting mid-hold only pans the motion since here.
                let (px, py) = prev.get();
                prev.set((off_x, off_y));
                if state.borrow().zoom <= 1.05 {
                    return;
                }
                let dx = off_x - px;
                let dy = off_y - py;
                if dx == 0.0 && dy == 0.0 {
                    return;
                }
                let hadj = scrolled.hadjustment();
                let vadj = scrolled.vadjustment();
                let old_h = hadj.value();
                let old_v = vadj.value();
                let nx = (old_h - dx).clamp(
                    hadj.lower(),
                    (hadj.upper() - hadj.page_size()).max(hadj.lower()),
                );
                let ny = (old_v - dy).clamp(
                    vadj.lower(),
                    (vadj.upper() - vadj.page_size()).max(vadj.lower()),
                );
                debug_log!(format!(
                    "pan-update: d=({dx:+.1},{dy:+.1}) h {old_h:.0}->{nx:.0} v {old_v:.0}->{ny:.0}"
                ));
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
        click.connect_pressed(move |gesture, n_press, x, y| {
            if n_press != 2 {
                return;
            }
            let cur = state.borrow().zoom;
            if cur > 1.5 {
                debug_log!(format!("dblclick-toggle: fit (cur={cur:.2})"));
                zoom_to(&state, &picture_c, &scrolled_c, 1.0, None);
                return;
            }
            // Two anchor sources, cross-checked:
            //  - press coords (picture-local, converted to viewport space):
            //    reliable for touch, but stale/`(0,0)` for a pointer device
            //   that hasn't moved (documented Wayland per-device quirk);
            //  - the motion/`enter`-tracked cursor: reliable for pointer
            //    devices, but (0,0) until the pointer ever moves or enters —
            //    a window opening under a stationary cursor followed by a
            //    tap-to-click double-tap (no motion at all) used to zoom
            //    into the top-left corner this way.
            // Pick the source preferred for this device kind, reject (0,0) as
            // the stale marker on either, use the other one, and only then
            // fall back to the center — never to a corner. The log line shows
            // both sources plus which won (CAROSELLO_DEBUG=1).
            let has_cursor = gesture.device().is_some_and(|d| d.has_cursor());
            let dev_name = gesture
                .device()
                .map(|d| d.name().to_string())
                .unwrap_or_else(|| "<none>".into());
            let (mx, my, seen) = {
                let s = state.borrow();
                (s.mouse_x, s.mouse_y, s.mouse_seen)
            };
            let press_vp = picture_c
                .compute_point(&scrolled_c, &gtk::graphene::Point::new(x as f32, y as f32))
                .map(|p| (p.x() as f64, p.y() as f64));
            let sane = |p: (f64, f64)| !(p.0 == 0.0 && p.1 == 0.0);
            let cursor = seen.then_some((mx, my)).filter(|p| sane(*p));
            let press = press_vp.filter(|p| sane(*p));
            let (anchor, src) = if has_cursor {
                match (cursor, press) {
                    (c @ Some(_), _) => (c, "cursor"),
                    (_, p @ Some(_)) => (p, "press"),
                    _ => (None, "center"),
                }
            } else {
                match (press, cursor) {
                    (p @ Some(_), _) => (p, "press"),
                    (_, c @ Some(_)) => (c, "cursor"),
                    _ => (None, "center"),
                }
            };
            let fmt = |p: Option<(f64, f64)>| match p {
                Some((a, b)) => format!("({a:.0},{b:.0})"),
                None => "-".into(),
            };
            debug_log!(format!(
                "dblclick: dev={dev_name} has_cursor={has_cursor} press=({x:.0},{y:.0})\u{2192}vp={} cursor=({mx:.0},{my:.0}) seen={seen} -> anchor {src} {} pic={}x{} scrolled={}x{}",
                fmt(press_vp),
                fmt(anchor),
                picture_c.width(),
                picture_c.height(),
                scrolled_c.width(),
                scrolled_c.height()
            ));
            // Coordinate conversion can fail before first layout: `anchor` is
            // then None and the zoom still happens (centered), never silently
            // dropped and never aimed at a corner.
            zoom_to(&state, &picture_c, &scrolled_c, 2.5, anchor);
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
/// Early-outs when nothing visible would change: the formatted second, the
/// seek bar's own 500-step position, play state and the seeking flag
/// (report #9 — this ran the full set on every GStreamer position tick).
fn update_seek_ui(state: &Rc<RefCell<AppState>>) {
    let (media, seeking, is_vid) = {
        let s = state.borrow();
        (s.media_file.clone(), s.seeking, s.is_video)
    };
    let Some(media) = media else { return };
    if !is_vid {
        return;
    }
    let ts = media.timestamp();
    let dur_val = media.duration();
    let playing = media.is_playing();
    let bar_unit = if dur_val > 0 { ts * 500 / dur_val } else { 0 };
    let key = (
        ts / 1_000_000,
        dur_val / 1_000_000,
        bar_unit,
        playing,
        seeking,
    );
    if state.borrow().last_seek_ui == Some(key) {
        return;
    }
    state.borrow_mut().last_seek_ui = Some(key);

    let (scale, pos, dur, play_btn) = {
        let s = state.borrow();
        (
            s.seek_scale.clone(),
            s.position_label.clone(),
            s.duration_label.clone(),
            s.play_pause_btn.clone(),
        )
    };
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
        sync_play_button(&btn, playing);
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
    debug_log!(format!(
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
        debug_log!(format!(
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
    armed_us: &Rc<Cell<i64>>,
    delay_ms: u32,
    menu: &gtk::MenuButton,
) {
    // Re-arm only when no timer is pending or the pending one is about to
    // fire (armed ≥ delay − 500 ms ago): motion events used to destroy and
    // recreate a glib source on every event — ~120 create/destroys per
    // second at 120 Hz (report #7). While an early timer runs the panels
    // simply hide a bit sooner; the ≥ delay − 500 ms check then pushes the
    // deadline back out for as long as the pointer keeps moving.
    // (`Option<SourceId>` isn't `Copy` and `Cell` has no `borrow`, so the
    // pending id is inspected by take/put-back — single-threaded, with
    // nothing reentrant in between.)
    if let Some(id) = hide_id.take() {
        let armed = armed_us.get();
        let fresh =
            armed != 0 && glib::monotonic_time() - armed < (i64::from(delay_ms) - 500) * 1000;
        hide_id.set(Some(id));
        if fresh {
            return;
        }
    }
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
    armed_us.set(glib::monotonic_time());
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
        debug_log!(format!(
            "zoom_to: {old_zoom:.2} -> {new_zoom:.2} (intrinsic unknown yet)"
        ));
        state.borrow_mut().zoom = new_zoom;
        schedule_update(state, picture, scrolled);
        return;
    };
    let (vw, vh) = viewport;
    // Past the zoom where fit × zoom reaches MAX_DIM, display sizes stop
    // growing (display_size_for clamps) while `state.zoom` would keep
    // climbing to ZOOM_MAX: a dead range of no-op anchor math (report,
    // "zoom saturates silently" — ~9.1× in a 900 px viewport). Cap at the
    // level where the larger fit edge hits MAX_DIM, floored at fit and
    // never above ZOOM_MAX.
    let (fw, fh) = zoom::fit_size(iw, ih, vw, vh);
    let max_zoom = (state::MAX_DIM / fw.max(fh).max(1.0)).clamp(1.0, state::ZOOM_MAX);
    new_zoom = new_zoom.min(max_zoom);
    if !is_video {
        new_zoom = new_zoom.max(1.0);
    }
    if (new_zoom - old_zoom).abs() < 0.0001 {
        return;
    }
    let (old_dw, old_dh) = zoom::effective_display_size_for(iw, ih, vw, vh, old_zoom, is_video);
    let (new_dw, new_dh) = zoom::effective_display_size_for(iw, ih, vw, vh, new_zoom, is_video);

    let (ax, ay) = anchor.unwrap_or((vw / 2.0, vh / 2.0));
    let hadj = scrolled.hadjustment();
    let vadj = scrolled.vadjustment();
    let old_hv = hadj.value();
    let old_vv = vadj.value();

    debug_log!(format!(
        "zoom_to: {old_zoom:.2} -> {new_zoom:.2} anchor={} ({ax:.0},{ay:.0})",
        if anchor.is_some() { "pt" } else { "center" }
    ));
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
                debug_log!(format!("zoom-anchor: h -> {want_h:.0}"));
            }
            if v_max > 0.0 && (vadj.value() - want_v).abs() > 0.5 {
                vadj.set_value(want_v);
                debug_log!(format!("zoom-anchor: v -> {want_v:.0}"));
            }
        }
    };
    apply_anchor(&hadj, &vadj);
    {
        let picture = picture.clone();
        let state_poll = state.clone();
        let mut ticks: u32 = 0;
        let mut stable: u32 = 0;
        let mut prev: Option<AnchorSnap> = None;
        glib::timeout_add_local(Duration::from_millis(16), move || {
            // Superseded by a newer zoom: stop instead of burning the full
            // 40-tick budget — pinch fires scale_changed per frame, so
            // overlapping pollers used to stack (report #7).
            if (state_poll.borrow().zoom - new_zoom).abs() > 0.0001 {
                return glib::ControlFlow::Break;
            }
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
                debug_log!(format!(
                    "zoom-settled: h={:.0} v={:.0} (t={}ms)",
                    hadj.value(),
                    vadj.value(),
                    ticks * 16,
                ));
                return glib::ControlFlow::Break;
            }
            if ticks >= 40 {
                debug_log!("zoom-anchor: deadline, stop re-anchoring");
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
    debug_log!(format!("nav: reset zoom to fit (index={new_index})"));
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
    // the finished texture, and the incoming frame + the main view share
    // that single GPU copy (report #4: one upload, not two).
    let new_image: Option<(gdk::Paintable, i32, i32)> = if !new_is_video {
        // (Two statements: the borrow must end before the texture is used.)
        let cached = state.borrow().prefetch_get(&path);
        let Some(tex) = cached else {
            return false;
        };
        let (iw, ih) = (tex.width().max(1), tex.height().max(1));
        Some((tex.upcast(), iw, ih))
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
    debug_log!(format!("try_slide: reset zoom to fit (index={new_index})"));
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

/// Weak mirror of `SlideCtx` for `MediaFile` signal closures (report
/// #2b): the state stays a std `Weak` and the widgets glib `WeakRef`s,
/// so a slide/drag pipeline can never keep the stage — or the whole
/// `AppState` — alive. Scalars are copied; `upgrade` rebuilds a strong
/// ctx only while everything is still around.
#[derive(Clone)]
struct SlideCtxWeak {
    state: std::rc::Weak<RefCell<AppState>>,
    overlay: glib::WeakRef<gtk::Overlay>,
    stage: glib::WeakRef<gtk::Fixed>,
    old_pic: glib::WeakRef<gtk::Picture>,
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

impl SlideCtxWeak {
    fn new(ctx: &SlideCtx) -> Self {
        Self {
            state: Rc::downgrade(&ctx.state),
            overlay: ctx.overlay.downgrade(),
            stage: ctx.stage.downgrade(),
            old_pic: ctx.old_pic.downgrade(),
            obx: ctx.obx,
            oby: ctx.oby,
            w: ctx.w,
            h: ctx.h,
            vw: ctx.vw,
            vh: ctx.vh,
            dir: ctx.dir,
            my_gen: ctx.my_gen,
            new_index: ctx.new_index,
        }
    }

    fn upgrade(&self) -> Option<SlideCtx> {
        Some(SlideCtx {
            state: self.state.upgrade()?,
            overlay: self.overlay.upgrade()?,
            stage: self.stage.upgrade()?,
            old_pic: self.old_pic.upgrade()?,
            obx: self.obx,
            oby: self.oby,
            w: self.w,
            h: self.h,
            vw: self.vw,
            vh: self.vh,
            dir: self.dir,
            my_gen: self.my_gen,
            new_index: self.new_index,
        })
    }
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
    // Every strong capture is weak here (report #2b): media → these
    // handlers → maybe_start → new_pic → wrap → media used to pin a
    // muted *playing, looping* pipeline plus the whole slide stage.
    let maybe_start = {
        let ctx_w = SlideCtxWeak::new(ctx);
        let new_pic_w = new_pic.downgrade();
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
            let Some(ctx) = ctx_w.upgrade() else {
                return;
            };
            let Some(new_pic) = new_pic_w.upgrade() else {
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
        let ctx_w = SlideCtxWeak::new(ctx);
        media.connect_error_notify(move |_| {
            if let Some(c) = ctx_w.upgrade() {
                abandon_slide(&c.state, c.my_gen);
            }
        });
    }
    // Stuck pipeline (slow mount, missing codec with no error yet):
    // reveal the main view instead of covering it forever. This one-shot
    // timeout may hold a strong ctx for its bounded 2 s (by design).
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
    debug_log!(format!(
        "commit_index: reset zoom to fit (index={new_index})"
    ));
    reset_scroll(scrolled);
    show_file(state, picture, scrolled, window);
}
/// Open the preferences dialog: slide animation + two-finger swipe, written to a
/// file so they survive restarts (Flatpak included).
fn show_preferences(state: &Rc<RefCell<AppState>>, window: &adw::ApplicationWindow) {
    // AdwPreferencesWindow is deprecated since libadwaita 1.6 (the version
    // meson.build requires); its dialog replacement takes the parent at
    // present() time and has no modal flag (dialogs always are).
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
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
    dialog.present(Some(window));
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
    debug_log!(format!(
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
    let z = state.borrow().zoom;
    if vw < 1.0 || vh < 1.0 || !animations_enabled(state) || z > 1.05 {
        // Zoomed: leave it to the discrete swipe (instant cut when zoomed).
        debug_log!(format!(
            "drag-begin: decline vp={vw:.0}x{vh:.0} zoom={z:.2} anims={}",
            animations_enabled(state)
        ));
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
    let have_frame = new_is_video || state.borrow().prefetch_contains(&path);
    {
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else { return };
        d.new_index = new_index;
        if !have_frame {
            // Uncached: track blind, the release cuts instantly.
            debug_log!(format!(
                "drag-lock: no frame for {} -> blind cut",
                path.display()
            ));
            d.visual = false;
            return;
        }
    }
    if new_is_video {
        drag_lock_video(state, &path, vw, vh, new_index);
        return;
    }
    // Image: the prefetched texture as-is (already EXIF-oriented; the main
    // view takes the same entry on commit — one upload, report #4).
    // (Two statements: the clone must finish before the texture is used.)
    let cached = state.borrow().prefetch_get(&path);
    let Some(tex) = cached else {
        // Evicted between lock and decode: track blind, release cuts.
        debug_log!(format!(
            "drag-lock: evicted {} -> blind cut",
            path.display()
        ));
        if let Some(d) = state.borrow_mut().drag.as_mut() {
            d.visual = false;
        }
        return;
    };
    let (iw, ih) = (tex.width().max(1), tex.height().max(1));
    let tex: gdk::Paintable = tex.upcast();
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
    // Weak captures (report #2b): media → this handler → new_pic → wrap
    // → media, and → state → drag → new_pic, are both real cycles.
    let maybe_ready = {
        let state_w = Rc::downgrade(state);
        let stage_w = stage.downgrade();
        let new_pic_w = new_pic.downgrade();
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
            let Some(state) = state_w.upgrade() else {
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
            let Some(stage) = stage_w.upgrade() else {
                return;
            };
            let Some(new_pic) = new_pic_w.upgrade() else {
                return;
            };
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
        let state_w = Rc::downgrade(state);
        media.connect_error_notify(move |_| {
            // Release will reveal the main view; mark unready now.
            let Some(state) = state_w.upgrade() else {
                return;
            };
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
        debug_log!("drag-end: micro-gesture, no nav");
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
        debug_log!(format!(
            "drag-end: instant reveal (visual={} commit={} video={} ready={})",
            d.visual, commit, d.is_video, d.ready
        ));
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
///
/// When the filesystem has no Trash at all (`G_IO_ERROR_NOT_SUPPORTED`:
/// remote gvfs mounts such as sftp, system-internal mounts like `/tmp`,
/// the document portal) the file is deleted in place instead — see
/// [`delete_current`].
fn trash_current(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
) {
    // An in-flight transform write would recreate a trashed file at its old
    // path (GIO replace works on the original path), so make people wait.
    if state.borrow().saving {
        show_toast(state, "Wait for the save to finish first");
        return;
    }
    // One request at a time: results arrive asynchronously now.
    if state.borrow().trashing {
        return;
    }
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

    // GIO trash (goes to Trash, not permanent delete), async so a
    // network/portal path can't freeze the UI (report #8). Needs write
    // access to the containing directory (Flatpak: --filesystem=host:rw).
    let file = gio::File::for_path(&path);
    state.borrow_mut().trashing = true;
    let state_c = state.clone();
    let picture_c = picture.clone();
    let scrolled_c = scrolled.clone();
    let window_c = window.clone();
    let path_c = path.clone();
    file.trash_async(
        glib::Priority::DEFAULT,
        None::<&gio::Cancellable>,
        move |res| {
            state_c.borrow_mut().trashing = false;
            match res {
                Ok(()) => finish_removal(
                    &state_c,
                    &picture_c,
                    &scrolled_c,
                    &window_c,
                    &path_c,
                    &format!("Moved {name} to Trash"),
                ),
                Err(e) => {
                    debug_log!(format!("trash failed for {}: {e}", path_c.display()));
                    // "There is no Trash here" — verified to be
                    // G_IO_ERROR_NOT_SUPPORTED on a remote sftp mount
                    // ("Operation not supported") and on /tmp ("Trashing
                    // on system internal mounts is not supported").
                    if matches!(e.kind(), Some(gio::IOErrorEnum::NotSupported)) {
                        delete_current(&state_c, &picture_c, &scrolled_c, &window_c, path_c, name);
                        return;
                    }
                    if is_doc_portal_path(&path_c) {
                        show_toast(
                            &state_c,
                            "Cannot move to Trash from sandbox portal — use the Files app",
                        );
                    } else {
                        show_toast(&state_c, &format!("Cannot move {name} to Trash"));
                    }
                }
            }
        },
    );
}

/// Delete the current file permanently — the fallback used when the
/// filesystem offers no Trash ([`trash_current`], `NOT_SUPPORTED`).
///
/// Reuses the `trashing` guard so a second Del cannot race the first,
/// and ends in the same [`finish_removal`] tail as a successful trash.
fn delete_current(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
    path: PathBuf,
    name: String,
) {
    let file = gio::File::for_path(&path);
    state.borrow_mut().trashing = true;
    let state_c = state.clone();
    let picture_c = picture.clone();
    let scrolled_c = scrolled.clone();
    let window_c = window.clone();
    file.delete_async(
        glib::Priority::DEFAULT,
        None::<&gio::Cancellable>,
        move |res| {
            state_c.borrow_mut().trashing = false;
            match res {
                Ok(()) => finish_removal(
                    &state_c,
                    &picture_c,
                    &scrolled_c,
                    &window_c,
                    &path,
                    &format!("Deleted {name} (no Trash here)"),
                ),
                Err(e) => {
                    debug_log!(format!("delete failed for {}: {e}", path.display()));
                    if is_doc_portal_path(&path) {
                        show_toast(
                            &state_c,
                            "Cannot delete from sandbox portal — use the Files app",
                        );
                    } else {
                        show_toast(&state_c, &format!("Cannot delete {name}"));
                    }
                }
            }
        },
    );
}

/// Shared tail of a successful trash/delete: drop the entry from the
/// list, reindex, show the next sibling and toast `msg`.
///
/// The index is NOT reset (see [`trash_current`]).
fn finish_removal(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
    path: &Path,
    msg: &str,
) {
    // (Two statements: the position lookup must finish before the
    // mutable borrow below, else RefCell panics.)
    let pos = state.borrow().files.iter().position(|f| f == path);
    cancel_slide(state);
    {
        let mut s = state.borrow_mut();
        if let Some(pos) = pos {
            s.files.remove(pos);
            s.prefetch_drop(path);
            s.index = state::index_after_removal(s.files.len(), s.index, pos);
            s.zoom = 1.0;
        }
    }
    reset_scroll(scrolled);
    show_file(state, picture, scrolled, window);
    if !state.borrow().files.is_empty() {
        show_toast(state, msg);
    }
}

/// Worker result of a transform: encoded bytes plus the original file mode
/// (GIO's atomic replace recreates the file, so permissions are restored).
type TransformResult = Result<(Vec<u8>, Option<std::fs::Permissions>), String>;

/// Transform buttons are enabled only while showing a real image with no
/// save in flight (disabled for videos, empty folder, and mid-save).
fn sync_transform_buttons(state: &Rc<RefCell<AppState>>) {
    let (buttons, enabled) = {
        let s = state.borrow();
        (
            s.transform_btns.clone(),
            !s.files.is_empty() && !s.is_video && !s.saving,
        )
    };
    for btn in buttons {
        btn.set_sensitive(enabled);
    }
}

/// Rotate/mirror the current image and autosave it in place.
///
/// Read/decode/transform/encode happens on a worker thread using only Send
/// data (no Pixbuf, no Rc — AGENTS.md); the result comes back through an
/// `Arc<Mutex<…>>` slot polled on the main context (glib has no channel
/// and `idle_add` closures must be Send). The file write itself is GIO
/// async, so gvfs/portal paths keep working.
fn run_transform(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    op: transform::Transform,
) {
    let path = {
        let s = state.borrow();
        if s.files.is_empty() || s.is_video || s.saving {
            return;
        }
        s.files[s.index].clone()
    };
    debug_log!(format!("transform: {op:?} {}", path.display()));
    cancel_slide(state);
    {
        let mut s = state.borrow_mut();
        s.saving = true;
    }
    sync_transform_buttons(state);

    let slot: Arc<Mutex<Option<TransformResult>>> = Arc::new(Mutex::new(None));
    {
        let slot = slot.clone();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            let result = (|| -> TransformResult {
                let mode = std::fs::metadata(&worker_path)
                    .ok()
                    .map(|m| m.permissions());
                let bytes = std::fs::read(&worker_path)
                    .map_err(|e| format!("Cannot read {}: {e}", worker_path.display()))?;
                let ext = worker_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                let out = transform::transform_bytes(&bytes, ext, op)?;
                Ok((out, mode))
            })();
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(result);
            }
        });
    }

    // Poll the slot every frame; ~30 s safety net if the worker never
    // reports back (in release a worker panic aborts the process anyway).
    let state_c = state.clone();
    let picture_c = picture.clone();
    let scrolled_c = scrolled.clone();
    let path_c = path.clone();
    let mut ticks: u32 = 0;
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let taken = match slot.lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
        if let Some(result) = taken {
            finish_transform(&state_c, &picture_c, &scrolled_c, &path_c, result);
            return glib::ControlFlow::Break;
        }
        ticks += 1;
        if ticks >= 1_875 {
            debug_log!("transform: worker never reported back, giving up");
            show_toast(&state_c, "Transform timed out");
            state_c.borrow_mut().saving = false;
            sync_transform_buttons(&state_c);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// Worker finished: write the result over the original file (GIO async,
/// then restore permissions), refresh the view if it is still current.
fn finish_transform(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    path: &Path,
    result: TransformResult,
) {
    let (bytes, mode) = match result {
        Ok(ok) => ok,
        Err(msg) => {
            debug_log!(format!("transform failed for {}: {msg}", path.display()));
            let hint = if is_doc_portal_path(path) {
                " — use Open Folder so the file is writable"
            } else {
                ""
            };
            show_toast(state, &format!("{msg}{hint}"));
            state.borrow_mut().saving = false;
            sync_transform_buttons(state);
            return;
        }
    };

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let file = gio::File::for_path(path);
    let state_c = state.clone();
    let picture_c = picture.clone();
    let scrolled_c = scrolled.clone();
    let path_c = path.to_path_buf();
    file.replace_contents_async(
        bytes,
        None,
        false,
        gio::FileCreateFlags::NONE,
        None::<&gio::Cancellable>,
        move |res| {
            match res {
                Ok(_) => {
                    debug_log!(format!("transform: saved {}", path_c.display()));
                    // GIO replaced the file: put the original mode back.
                    if let Some(mode) = mode {
                        let _ = std::fs::set_permissions(&path_c, mode);
                    }
                    {
                        let mut s = state_c.borrow_mut();
                        s.saving = false;
                        // Drop pixels decoded from the pre-transform file.
                        s.prefetch_drop(&path_c);
                    }
                    sync_transform_buttons(&state_c);
                    // Refresh only if this file is still on screen.
                    let current = {
                        let s = state_c.borrow();
                        s.files.get(s.index).cloned()
                    };
                    if current.as_deref() == Some(path_c.as_path()) {
                        show_image(&state_c, &picture_c, &scrolled_c, &path_c);
                    }
                }
                Err((_, e)) => {
                    debug_log!(format!("transform: save failed for {}: {e}", path_c.display()));
                    if is_doc_portal_path(&path_c) {
                        show_toast(
                            &state_c,
                            "Cannot save to sandbox portal — use Open Folder so the file is writable",
                        );
                    } else {
                        show_toast(&state_c, &format!("Cannot save {name}"));
                    }
                    state_c.borrow_mut().saving = false;
                    sync_transform_buttons(&state_c);
                }
            }
        },
    );
}

fn show_file(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    window: &adw::ApplicationWindow,
) {
    // A new item is coming: supersede the old round — taking its token
    // also opens a fresh epoch, letting the dispatch below (or the call
    // at the end of this function) start the new round.
    cancel_prefetch(state);
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
            sync_transform_buttons(state);
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

    // After dispatch, so is_video reflects the item just shown.
    sync_transform_buttons(state);
    // Guarantee a round for the new index: image shows already started
    // one (epoch guard → no-op here); videos and error paths otherwise
    // never prefetch their neighbors at all.
    prefetch_neighbors(state);
}

/// Attach `picture` to `scrolled` only when not already there — the
/// child never changes, so re-issuing `set_child` on every show was pure
/// container churn (report: lower-impact polish).
fn ensure_picture_child(scrolled: &gtk::ScrolledWindow, picture: &gtk::Picture) {
    let attached = scrolled
        .child()
        .is_some_and(|w| w == picture.clone().upcast::<gtk::Widget>());
    if !attached {
        scrolled.set_child(Some(picture));
    }
}

/// Drop + cancel the in-flight display decode. Called right after the
/// `image_gen` bump, so the stale round's poller exits on its generation
/// guard and its worker stops before read/decode (report #1's pipeline
/// with report #3-style cancellation).
fn cancel_decode(state: &Rc<RefCell<AppState>>) {
    let old = state.borrow_mut().decode_cancel.take();
    if let Some(c) = old {
        c.cancel();
    }
}

/// Drop + cancel the current prefetch round (`show_file` entry; a new
/// round also supersedes in `prefetch_neighbors`).
fn cancel_prefetch(state: &Rc<RefCell<AppState>>) {
    let old = state.borrow_mut().prefetch_cancel.take();
    if let Some(c) = old {
        c.cancel();
    }
}

/// Worker for one read+decode round: plain `Send` data only (Pixbuf is
/// `!Send` — see AGENTS.md). Checks the round's `Cancellable` before the
/// read, before the decode and before publishing, so a superseded round
/// stores nothing and its poller can retire early (report #1 + #3).
fn spawn_frame_worker(
    cancel: gio::Cancellable,
    path: PathBuf,
) -> Arc<Mutex<Option<Result<transform::Decoded, String>>>> {
    let slot: Arc<Mutex<Option<Result<transform::Decoded, String>>>> = Arc::new(Mutex::new(None));
    let slot_c = slot.clone();
    std::thread::spawn(move || {
        let result = (|| -> Result<transform::Decoded, String> {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let bytes =
                std::fs::read(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            transform::decode_frame(&bytes)
        })();
        if cancel.is_cancelled() {
            return;
        }
        if let Ok(mut guard) = slot_c.lock() {
            *guard = Some(result);
        }
    });
    slot
}

/// Wrap decoded RGBA rows in a GPU texture. Main thread only
/// (`MemoryTexture` asserts it) and zero-copy: `Bytes::from_owned` takes
/// the worker's `Vec` as-is.
fn frame_texture(frame: transform::Decoded) -> gdk::Texture {
    let stride = frame.w as usize * 4;
    gdk::MemoryTexture::new(
        frame.w,
        frame.h,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(frame.rgba),
        stride,
    )
    .upcast()
}

/// Park the texture currently shown on `picture` in the prefetch cache
/// under the path it belongs to (`shown_path`), right before the paintable
/// is replaced with `incoming`.
///
/// The outgoing frame was decoded moments ago; keeping it turns the common
/// back-and-forth into a prefetch *hit*. Without it, navigating back means
/// a fresh worker decode — if the finger/keys get there first,
/// `drag_lock`/`try_slide_to` fall back to the instant cut ("sometimes the
/// slide transition is skipped": landing on an item and swiping back
/// before its neighbor round finished decoding). The neighbor round then
/// skips the stashed path (`prefetch_contains`), so the FIFO cap of 2
/// still holds: [outgoing, other-neighbor].
///
/// Skipped when: nothing is shown yet (startup), the paintable isn't a
/// `gdk::Texture` (video wrapper / already cleared), the same path is
/// being re-shown (transform re-show — never cache pre-transform pixels),
/// the old path left `files` (trash), or a transform save is in flight
/// (same rationale as `prefetch_neighbors`).
fn stash_outgoing(state: &Rc<RefCell<AppState>>, picture: &gtk::Picture, incoming: &Path) {
    let Some(old) = state.borrow().shown_path.clone() else {
        return;
    };
    let usable = {
        let s = state.borrow();
        old != *incoming
            && !s.saving
            && state::has_ext(&old, state::IMAGE_EXTS)
            && s.files.iter().any(|f| f == &old)
    };
    if !usable {
        return;
    }
    let Some(tex) = picture
        .paintable()
        .and_then(|p| p.downcast_ref::<gdk::Texture>().cloned())
    else {
        return;
    };
    debug_log!(format!(
        "stash: {} ({}x{}) for back-nav",
        old.display(),
        tex.width(),
        tex.height()
    ));
    state.borrow_mut().prefetch_store(&old, tex);
}

/// Worker delivered a decoded frame: wrap it and put it on screen.
fn present_image(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    frame: transform::Decoded,
    gen: u64,
    path: &Path,
) {
    if state.borrow().image_gen != gen {
        return;
    }
    let tex = frame_texture(frame);
    let (w, h) = (tex.width().max(1), tex.height().max(1));
    {
        let mut s = state.borrow_mut();
        if s.image_gen != gen {
            return;
        }
        s.image_w = w;
        s.image_h = h;
    }
    stash_outgoing(state, picture, path);
    ensure_picture_child(scrolled, picture);
    picture.set_paintable(Some(&tex));
    state.borrow_mut().shown_path = Some(path.to_path_buf());
    picture.set_content_fit(gtk::ContentFit::Contain);
    reset_scroll(scrolled);
    schedule_update(state, picture, scrolled);
    prefetch_neighbors(state);
}

/// Read + decode + EXIF-orient the display frame on a worker thread
/// (report #1: gdk-pixbuf's "async" stream decode, the synchronous EXIF
/// file read and the orientation transpose all ran on the main thread).
/// The worker returns plain bytes; the main context only builds the
/// texture. The slot is polled every frame (glib has no channel and
/// `idle_add` closures must be `Send` — same pattern as the transform
/// worker), with `image_gen` + `Cancellable` discarding stale rounds.
fn spawn_decode(
    state: &Rc<RefCell<AppState>>,
    picture: &gtk::Picture,
    scrolled: &gtk::ScrolledWindow,
    path: &Path,
    gen: u64,
) {
    let cancel = gio::Cancellable::new();
    state.borrow_mut().decode_cancel = Some(cancel.clone());
    let slot = spawn_frame_worker(cancel.clone(), path.to_path_buf());

    let state_c = state.clone();
    let picture_c = picture.clone();
    let scrolled_c = scrolled.clone();
    let path_c = path.to_path_buf();
    let mut ticks: u32 = 0;
    glib::timeout_add_local(Duration::from_millis(16), move || {
        // Superseded (a newer show bumped `image_gen`, or the round was
        // cancelled): drop the result without touching the widgets.
        if state_c.borrow().image_gen != gen || cancel.is_cancelled() {
            return glib::ControlFlow::Break;
        }
        let taken = match slot.lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
        if let Some(result) = taken {
            match result {
                Ok(frame) => present_image(&state_c, &picture_c, &scrolled_c, frame, gen, &path_c),
                Err(msg) => {
                    debug_log!(format!("Failed to load {}: {msg}", path_c.display()));
                    // Keep the still-valid frame we were showing instead of
                    // dropping it with the clear below.
                    stash_outgoing(&state_c, &picture_c, &path_c);
                    picture_c.set_paintable(None::<&gdk::Texture>);
                    state_c.borrow_mut().shown_path = None;
                    show_toast(
                        &state_c,
                        &format!(
                            "Failed to load {}",
                            path_c.file_name().unwrap_or_default().to_string_lossy()
                        ),
                    );
                }
            }
            return glib::ControlFlow::Break;
        }
        ticks += 1;
        if ticks >= 1_875 {
            debug_log!("decode: worker never reported back, giving up");
            show_toast(&state_c, "Image load timed out");
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
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
    }
    // Release the last video's pipeline: the wrapper paintable kept it
    // alive even while browsing photos (report #2 — never cleared).
    // Clone out first: set_inner's invalidate notifies run synchronously.
    let video_view = state.borrow().video_view.clone();
    video_view.set_inner(None);
    debug_log!(format!(
        "show_image: reset zoom to fit ({})",
        path.display()
    ));

    if let Some(ref h) = state.borrow().bottom_bar {
        h.set_visible(false);
    }

    let path_buf = path.to_path_buf();
    let gen = state.borrow().image_gen;
    // Generation bumped above: any in-flight decode is stale now.
    cancel_decode(state);

    // Fast path: prefetched neighbor already decoded.
    // (Two statements: the take must drop its borrow before the body
    // borrows state again, else RefCell panics.)
    let cached = state.borrow_mut().prefetch_take(&path_buf);
    if let Some(tex) = cached {
        let (w, h) = (tex.width().max(1), tex.height().max(1));
        {
            let mut s = state.borrow_mut();
            if s.image_gen != gen {
                return;
            }
            s.image_w = w;
            s.image_h = h;
        }
        ensure_picture_child(scrolled, picture);
        stash_outgoing(state, picture, &path_buf);
        picture.set_paintable(Some(&tex));
        state.borrow_mut().shown_path = Some(path_buf.clone());
        picture.set_content_fit(gtk::ContentFit::Contain);
        reset_scroll(scrolled);
        schedule_update(state, picture, scrolled);
        prefetch_neighbors(state);
        return;
    }

    spawn_decode(state, picture, scrolled, &path_buf, gen);
    // Start the neighbor round NOW, in parallel with the display decode.
    // Waiting for present_image serialized it behind the current file's
    // decode (decode current → *then* neighbors), so the next swipe
    // almost always beat the round — the forward-cut cascade. The epoch
    // guard makes present_image's own call a no-op afterwards.
    prefetch_neighbors(state);
}

/// Decode next/prev images in the background so navigation feels instant.
/// Bounded to 2 entries with FIFO eviction; one round per navigation:
/// the previous round's `GCancellable` is cancelled first, results
/// re-check the target is still a neighbor, and the neighbor paths are
/// read straight from the borrow — no per-nav `files.clone()` (report
/// #3). Decode runs on a worker thread (#1) and the cache stores the
/// finished texture, shared by slide/drag/main (#4).
fn prefetch_neighbors(state: &Rc<RefCell<AppState>>) {
    // While a transform save is in flight, a decode started now could land
    // around the atomic replace and cache pre-transform pixels. Navigation
    // just falls back to a normal async load instead.
    if state.borrow().saving {
        return;
    }
    // Epoch guard: `prefetch_cancel = Some(...)` means a round for this
    // show already runs (started by show_image / show_file below / the
    // fast path). Never cancel it here mid-decode and spawn duplicates —
    // show_file's entry-cancel and prefetch_drop reopen the epoch.
    if state.borrow().prefetch_cancel.is_some() {
        debug_log!("prefetch: round already active, skip");
        return;
    }
    let targets: Vec<PathBuf> = {
        let s = state.borrow();
        if s.files.is_empty() {
            return;
        }
        let mut targets = Vec::with_capacity(2);
        if s.index + 1 < s.files.len() {
            targets.push(s.files[s.index + 1].clone());
        }
        if s.index > 0 {
            targets.push(s.files[s.index - 1].clone());
        }
        targets
    };
    // Supersede the previous round (report #3.1).
    cancel_prefetch(state);
    debug_log!(format!(
        "prefetch: round start ({})",
        targets
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let cancel = gio::Cancellable::new();
    state.borrow_mut().prefetch_cancel = Some(cancel.clone());

    for target in targets {
        if !state::has_ext(&target, state::IMAGE_EXTS) {
            continue;
        }
        if state.borrow().prefetch_contains(&target) {
            continue;
        }
        let slot = spawn_frame_worker(cancel.clone(), target.clone());
        let state_c = state.clone();
        let round = cancel.clone();
        let path_c = target;
        let mut ticks: u32 = 0;
        glib::timeout_add_local(Duration::from_millis(16), move || {
            if round.is_cancelled() {
                return glib::ControlFlow::Break;
            }
            let taken = match slot.lock() {
                Ok(mut guard) => guard.take(),
                Err(_) => None,
            };
            if let Some(result) = taken {
                // Re-check the target is still a neighbor: fast back-and-
                // forth nav can move the index twice inside one round
                // (report #3.2 — stale decodes used to evict fresh ones).
                let still = {
                    let s = state_c.borrow();
                    let i = s.index;
                    (i + 1 < s.files.len() && s.files[i + 1] == path_c)
                        || (i > 0 && s.files[i - 1] == path_c)
                };
                match result {
                    Ok(frame) if still => {
                        let tex = frame_texture(frame);
                        state_c.borrow_mut().prefetch_store(&path_c, tex);
                    }
                    Ok(_) => {}
                    Err(msg) => {
                        debug_log!(format!("prefetch {}: {msg}", path_c.display()));
                    }
                }
                return glib::ControlFlow::Break;
            }
            ticks += 1;
            if ticks >= 1_875 {
                debug_log!(format!("prefetch: worker silent for {}", path_c.display()));
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
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
        debug_log!(format!(
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
    let (fit_w, fit_h) = if is_video {
        zoom::fit_size(iw, ih, vw, vh)
    } else {
        (0.0, 0.0)
    };
    debug_log!(format!(
        "update_display: is_video={is_video} intrinsic={iw:.0}x{ih:.0} viewport={vw:.0}x{vh:.0} zoom={zoom:.2} display={dw}x{dh}"
    ));
    // Skip when every output would be set to the value it already has
    // (report: lower-impact polish) — unless the video still needs its
    // paintable attached, which the key alone can't cover.
    let needs_paintable = is_video && picture.paintable().is_none();
    if !needs_paintable {
        let key = (dw, dh, is_video, fit_w, fit_h, zoom);
        let mut s = state.borrow_mut();
        if s.last_layout == Some(key) {
            return;
        }
        s.last_layout = Some(key);
    }
    if is_video {
        let zp = state.borrow().video_view.clone();
        zp.set_view(fit_w, fit_h, zoom);
        picture.set_content_fit(gtk::ContentFit::Fill);
        picture.set_size_request(dw, dh);
        if picture.paintable().is_none() {
            let zp_paintable: gdk::Paintable = zp.clone().upcast();
            picture.set_paintable(Some(&zp_paintable));
            // Nothing to stash (the paintable was empty); just re-pair the
            // bookkeeping with the file now on screen.
            let cur = {
                let s = state.borrow();
                s.files.get(s.index).cloned()
            };
            if let Some(p) = cur {
                state.borrow_mut().shown_path = Some(p);
            }
        }
    } else {
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_size_request(dw, dh);
        // Paintable is set at load time; only the GPU copy is retained.
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
    s.image_w = 0;
    s.image_h = 0;
    // Invalidate any in-flight async image load.
    s.image_gen = s.image_gen.wrapping_add(1);
    s.zoom = 1.0;
    s.is_video = true;
    debug_log!("show_video: reset zoom to fit");
    s.seeking = false;
    // The seek early-out key is per-media (report #9).
    s.last_seek_ui = None;
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

    // Image decode in flight? Its generation was bumped above — cancel it.
    cancel_decode(state);

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

    // Delay play() until the GStreamer pipeline is prepared. Weak
    // self-capture: a strong one is a permanent refcycle (the handler
    // only drops at finalize, and it keeps the object alive — report #2a).
    {
        let weak_media = media.downgrade();
        media.connect_prepared_notify(move |_| {
            if let Some(m) = weak_media.upgrade() {
                m.play();
            }
        });
        if media.is_prepared() {
            media.play();
        }
    }

    let zp = state.borrow().video_view.clone();
    zp.set_inner(Some(media.clone().upcast()));
    ensure_picture_child(scrolled, picture);
    picture.set_content_fit(gtk::ContentFit::Fill);
    let zp_paintable: gdk::Paintable = zp.clone().upcast();
    // Keep a still-displayed outgoing *image* for back-nav (the video
    // paintable itself is not stashed — not a Texture).
    stash_outgoing(state, picture, path);
    picture.set_paintable(Some(&zp_paintable));
    state.borrow_mut().shown_path = Some(path.to_path_buf());
    scrolled.hadjustment().set_value(0.0);
    scrolled.vadjustment().set_value(0.0);
    schedule_update(state, picture, scrolled);

    // When the video's intrinsic size becomes known, re-fit.
    // Size discovery disconnects after the first hit (no per-frame work).
    // All captures weak (report #2a): media → this handler → media (and
    // → state/widgets → video paintable → media) must not keep the
    // pipeline alive after the window is gone.
    {
        let weak_state = Rc::downgrade(state);
        let weak_pic = picture.downgrade();
        let weak_scrolled = scrolled.downgrade();
        let weak_media = media.downgrade();
        let size_handler = Rc::new(RefCell::new(None::<glib::SignalHandlerId>));
        let size_handler_c = size_handler.clone();
        let id = media.connect_invalidate_size(move |_| {
            let (Some(state_c), Some(picture_c), Some(scrolled_c), Some(media_c)) = (
                weak_state.upgrade(),
                weak_pic.upgrade(),
                weak_scrolled.upgrade(),
                weak_media.upgrade(),
            ) else {
                return;
            };
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
                    debug_log!(format!("video: size discovered {wi}x{hi}"));
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
    // old 200ms poll (idle even when paused). Seeking flag clears on
    // seek-done. Weak `state` captures (report #2a): state → media_file →
    // these handlers → state is exactly the cycle that leaked the widget
    // graph plus a paused pipeline after every video ever shown.
    {
        let weak_state = Rc::downgrade(state);
        media.connect_timestamp_notify(move |_| {
            if let Some(s) = weak_state.upgrade() {
                update_seek_ui(&s);
            }
        });
    }
    {
        let weak_state = Rc::downgrade(state);
        media.connect_duration_notify(move |_| {
            if let Some(s) = weak_state.upgrade() {
                update_seek_ui(&s);
            }
        });
    }
    {
        let weak_state = Rc::downgrade(state);
        media.connect_playing_notify(move |_| {
            if let Some(s) = weak_state.upgrade() {
                update_seek_ui(&s);
            }
        });
    }
    {
        let weak_state = Rc::downgrade(state);
        media.connect_seeking_notify(move |m| {
            let Some(s) = weak_state.upgrade() else {
                return;
            };
            if !m.is_seeking() {
                s.borrow_mut().seeking = false;
            }
            update_seek_ui(&s);
        });
    }
    // Error feedback (previously silent black frame).
    {
        let weak_state = Rc::downgrade(state);
        media.connect_error_notify(move |m| {
            if let Some(err) = m.error() {
                debug_log!(format!("video error: {err:?}"));
                if let Some(s) = weak_state.upgrade() {
                    show_toast(&s, "Failed to play video (missing codec?)");
                }
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
