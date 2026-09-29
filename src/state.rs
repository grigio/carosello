use std::path::{Path, PathBuf};

pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "tif"];
pub const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mkv"];
pub const ZOOM_MAX: f64 = 20.0;
pub const ZOOM_MIN: f64 = 0.1;
pub const MAX_DIM: f64 = 8192.0;
/// Full cross-slide duration in microseconds (fast start, gentle stop).
pub const SLIDE_MICROS: i64 = 220_000;
/// Finger travel before a touchpad swipe locks a direction and builds the
/// incoming frame (deadzone against accidental brushes).
pub const SWIPE_LOCK_PX: f64 = 12.0;
/// Release commits past a quarter of the viewport…
pub const SWIPE_PROGRESS_COMMIT: f64 = 0.25;
/// …or past a fling velocity in the travel direction (px/ms).
pub const SWIPE_FLING_PX_PER_MS: f64 = 0.8;

/// Whether verbose logging is on (`CAROSELLO_DEBUG` set), cached on first
/// use so [`debug_log!`] is a single check when disabled.
pub fn debug_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("CAROSELLO_DEBUG").is_ok())
}

/// Log a message when `CAROSELLO_DEBUG` is set. A macro rather than a
/// `fn(&str)`, so the argument expression lives *inside* the gate: with
/// debugging off no `format!` runs and nothing allocates (the old
/// function formatted on every call — IMPROVEMENTS #6). Import like a
/// function (`use crate::state::debug_log;`) and call with `!`.
macro_rules! debug_log {
    ($e:expr) => {{
        if $crate::state::debug_enabled() {
            let msg = $e;
            eprintln!("[carosello-debug] {msg}");
        }
    }};
}
pub(crate) use debug_log;

/// Case-insensitive extension test without allocating: compares the
/// original extension against each candidate (was `to_ascii_lowercase()`
/// per check — IMPROVEMENTS, micro).
pub fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        .unwrap_or(false)
}

pub fn is_media(path: &Path) -> bool {
    // One `path.extension()` for both lists (it used to run twice).
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    IMAGE_EXTS
        .iter()
        .chain(VIDEO_EXTS.iter())
        .any(|x| ext.eq_ignore_ascii_case(x))
}

/// Config dir for user preferences (`$XDG_CONFIG_HOME/carosello`,
/// `~/.config/carosello` fallback; remapped into the sandbox by Flatpak).
fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("carosello");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".config").join("carosello");
        }
    }
    PathBuf::from("/tmp").join("carosello-config")
}

/// Parse the slide-animation pref from settings text (default: enabled).
/// Only an explicit `false` disables it; unknown values stay enabled.
fn parse_slide_enabled(text: &str) -> bool {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "slide-animation" {
                return !v.trim().eq_ignore_ascii_case("false");
            }
        }
    }
    true
}

/// Parse the two-finger-swipe pref from settings text (default: disabled).
/// Only an explicit `true` enables it; unknown values stay disabled.
fn parse_two_finger_swipe(text: &str) -> bool {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "two-finger-swipe" {
                return v.trim().eq_ignore_ascii_case("true");
            }
        }
    }
    false
}

/// Parse the video-muted pref from settings text (default: muted — a new
/// video starts silent, issue #3). Only an explicit `false` (the user
/// unmuted once and we remembered it) starts videos with audio; unknown
/// values stay muted.
fn parse_video_muted(text: &str) -> bool {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "video-muted" {
                return !v.trim().eq_ignore_ascii_case("false");
            }
        }
    }
    true
}

/// Parse all prefs from one read of the settings text: slide animation
/// (default on), two-finger swipe (default off), video muted (default on).
fn parse_prefs(text: &str) -> (bool, bool, bool) {
    (
        parse_slide_enabled(text),
        parse_two_finger_swipe(text),
        parse_video_muted(text),
    )
}

/// Read every pref with a single `read_to_string` — startup used to read
/// `settings.conf` once per pref (IMPROVEMENTS #8). Unreadable file
/// yields the defaults: slide on, two-finger off, video muted.
pub fn load_prefs_from(dir: &Path) -> (bool, bool, bool) {
    match std::fs::read_to_string(dir.join("settings.conf")) {
        Ok(text) => parse_prefs(&text),
        Err(_) => (true, false, true),
    }
}

pub fn load_prefs() -> (bool, bool, bool) {
    load_prefs_from(&config_dir())
}

// Single-pref readers for the settings tests (the app reads all prefs at
// once via `load_prefs` — keeping them out of the binary avoids dead code).
#[cfg(test)]
/// Whether the slide animation is enabled (default true when unset or
/// unreadable). Read from `dir/settings.conf` (tests).
pub fn load_slide_enabled_from(dir: &Path) -> bool {
    load_prefs_from(dir).0
}

#[cfg(test)]
/// Whether two-finger (instead of three-finger) swipe is enabled
/// (default false when unset or unreadable).
pub fn load_two_finger_swipe_from(dir: &Path) -> bool {
    load_prefs_from(dir).1
}

#[cfg(test)]
/// Whether videos start muted (default true when unset or unreadable;
/// false once the user has unmuted and we remembered it).
pub fn load_video_muted_from(dir: &Path) -> bool {
    load_prefs_from(dir).2
}

/// Update a single `key = bool` line, preserving all other lines.
/// Missing files start from the header comment; missing keys append.
fn update_setting_to(dir: &Path, key: &str, enabled: bool) {
    let path = dir.join("settings.conf");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut found = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            lines.push(line.to_string());
            continue;
        }
        if let Some((k, _)) = line.split_once('=') {
            if k.trim() == key {
                lines.push(format!("{key} = {enabled}"));
                found = true;
                continue;
            }
        }
        lines.push(line.to_string());
    }
    if !found {
        if lines.is_empty() {
            lines.push("# Carosello preferences".to_string());
        }
        lines.push(format!("{key} = {enabled}"));
    }
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

/// Persist the slide-animation pref (best effort: failures are ignored so
/// toggling never errors).
pub fn save_slide_enabled_to(dir: &Path, enabled: bool) {
    update_setting_to(dir, "slide-animation", enabled);
}

pub fn save_slide_enabled(enabled: bool) {
    save_slide_enabled_to(&config_dir(), enabled);
}

/// Persist the two-finger-swipe pref (best effort, preserves other keys).
pub fn save_two_finger_swipe_to(dir: &Path, enabled: bool) {
    update_setting_to(dir, "two-finger-swipe", enabled);
}

pub fn save_two_finger_swipe(enabled: bool) {
    save_two_finger_swipe_to(&config_dir(), enabled);
}

/// Persist the video-muted pref (best effort, preserves other keys):
/// whatever the user picked for this video is what the next one opens
/// with (issue #3).
pub fn save_video_muted_to(dir: &Path, muted: bool) {
    update_setting_to(dir, "video-muted", muted);
}

pub fn save_video_muted(muted: bool) {
    save_video_muted_to(&config_dir(), muted);
}

pub fn clamp_zoom(z: f64) -> f64 {
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Release decision for an interactive swipe: commit when the dragged
/// progress passes a quarter of the viewport, or the finger flings past
/// the velocity threshold in the travel direction (`dir`: +1 next slides
/// left so the offset velocity is negative, and vice versa).
pub fn fling_complete(progress: f64, velocity: f64, dir: i32) -> bool {
    if progress >= SWIPE_PROGRESS_COMMIT {
        return true;
    }
    if dir > 0 {
        velocity < -SWIPE_FLING_PX_PER_MS
    } else {
        velocity > SWIPE_FLING_PX_PER_MS
    }
}

/// Settle duration scaled by remaining travel (a full slide is 220ms,
/// a short snap-back is much quicker).
pub fn settle_micros(remaining: f64, total: f64) -> i64 {
    if total < 1.0 {
        return SLIDE_MICROS;
    }
    ((SLIDE_MICROS as f64 * (remaining / total)).round() as i64).clamp(60_000, SLIDE_MICROS)
}

/// Release velocity over the recent samples (px/ms, signed like the
/// accumulated offset: negative travels left/next).
pub fn fling_velocity(samples: &[(i64, f64)]) -> f64 {
    if samples.len() < 2 {
        return 0.0;
    }
    let (t0, x0) = samples[0];
    let (t1, x1) = samples[samples.len() - 1];
    let dt_ms = (t1 - t0) as f64 / 1000.0;
    if dt_ms < 1.0 {
        return 0.0;
    }
    (x1 - x0) / dt_ms
}

/// Index to show after removing `pos` from a list that now has `new_len`
/// items. Keeps the viewer on the next sibling instead of resetting to 0:
/// the item after the removed one slides into the same index, removing the
/// last item falls back to the new last item, removing an item before the
/// current one shifts the index left. Empty list yields 0.
pub fn index_after_removal(new_len: usize, index: usize, pos: usize) -> usize {
    if new_len == 0 {
        return 0;
    }
    let mut idx = index;
    if pos < idx {
        idx -= 1;
    }
    idx.min(new_len - 1)
}

pub fn format_time(micros: i64) -> String {
    let total_secs = micros / 1_000_000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}

/// Compare filenames with embedded numbers numerically so `IMG2` sorts
/// before `IMG10` (plain byte-wise `sort()` does not).
fn natural_key(name: &str) -> Vec<NatPart> {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut in_digit = false;
    for ch in name.chars() {
        let is_digit = ch.is_ascii_digit();
        if buf.is_empty() {
            in_digit = is_digit;
            buf.push(ch);
        } else if is_digit == in_digit {
            buf.push(ch);
        } else {
            parts.push(NatPart::from_buf(&buf, in_digit));
            buf.clear();
            buf.push(ch);
            in_digit = is_digit;
        }
    }
    if !buf.is_empty() {
        parts.push(NatPart::from_buf(&buf, in_digit));
    }
    parts
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NatPart {
    Num(u64, usize),
    Str(String),
}

impl NatPart {
    fn from_buf(buf: &str, in_digit: bool) -> Self {
        if in_digit {
            // `NatPart`'s derived `Ord` compares (value, digit count), so
            // the value wins and the length only breaks exact ties:
            // IMG2 < IMG02 (leading zeros lose) and IMG2 < IMG10. The
            // sort tests below pin this — a comment claiming the
            // opposite used to live here.
            // (A run too long for u64 saturates to u64::MAX, so such
            // names all tie and fall through to the length tie-break.)
            let n = buf.parse::<u64>().unwrap_or(u64::MAX);
            NatPart::Num(n, buf.len())
        } else {
            NatPart::Str(buf.to_ascii_lowercase())
        }
    }
}

/// Sort media paths naturally by filename (`IMG2` before `IMG10`).
/// `sort_by_cached_key` builds each key once (n) instead of once per
/// comparison (n log n × 2 key builds — IMPROVEMENTS #5).
pub fn sort_media_paths(files: &mut [PathBuf]) {
    files.sort_by_cached_key(|p| {
        natural_key(p.file_name().and_then(|n| n.to_str()).unwrap_or_default())
    });
}

/// List media files in `dir`, sorted naturally by filename.
pub fn collect_media(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            debug_log!(format!(
                "collect_media: read_dir({}) failed: {err}",
                dir.display()
            ));
            return Vec::new();
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_media(p))
        .collect();
    sort_media_paths(&mut files);
    debug_log!(format!(
        "collect_media: {} media in {}",
        files.len(),
        dir.display()
    ));
    files
}

/// Display size of `path` in pixels, read from **headers only** (no pixel
/// decode) — cheap enough to run synchronously while the window is still
/// unmapped, which is what lets the initial window fit the first item
/// with no visible 900×600 → real-size jump (see
/// `window::fit_window_to_media`).
///
/// EXIF Orientation is applied the way the display decode applies it, so
/// a phone portrait (stored landscape + Orientation 6) probes as
/// portrait. Videos return `None`: their intrinsic size only exists once
/// the media pipeline prepares, so they take the deferred fit instead.
pub fn probe_dimensions(path: &Path) -> Option<(i32, i32)> {
    if !has_ext(path, IMAGE_EXTS) {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut source = std::io::BufReader::new(file);
    let reader = image::ImageReader::new(&mut source)
        .with_guessed_format()
        .ok()?;
    // Header parse: SOF/IHDR/… for the size, nothing else is read.
    let (w, h) = reader.into_dimensions().ok()?;
    // `reader` was consumed, so the `&mut source` borrow is over: rewind
    // past the sniffed prefix before walking the APP1 segments.
    std::io::Seek::rewind(&mut source).ok()?;
    let orientation = crate::media::read_exif_orientation(&mut source);
    let (w, h) = display_dims(w, h, orientation);
    debug_log!(format!(
        "probe: {} -> {w}x{h} (orientation {orientation})",
        path.display()
    ));
    Some((w, h))
}

/// Stored size × EXIF Orientation → display size: orientations 5..=8
/// transpose, the rest keep the axes. Saturates instead of overflowing on
/// absurd headers — the caller clamps to the screen anyway.
fn display_dims(w: u32, h: u32, orientation: u8) -> (i32, i32) {
    let (w, h) = if (5..=8).contains(&orientation) {
        (h, w)
    } else {
        (w, h)
    };
    let clamp = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    (clamp(w), clamp(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_has_ext_case_insensitive() {
        assert!(has_ext(&PathBuf::from("photo.JPG"), IMAGE_EXTS));
        assert!(has_ext(&PathBuf::from("photo.jpg"), IMAGE_EXTS));
        assert!(has_ext(&PathBuf::from("photo.Jpg"), IMAGE_EXTS));
        assert!(has_ext(&PathBuf::from("video.MP4"), VIDEO_EXTS));
    }

    #[test]
    fn test_has_ext_no_match() {
        assert!(!has_ext(&PathBuf::from("document.pdf"), IMAGE_EXTS));
        assert!(!has_ext(&PathBuf::from("archive.zip"), VIDEO_EXTS));
        assert!(!has_ext(&PathBuf::from("noext"), IMAGE_EXTS));
    }

    #[test]
    fn test_is_media() {
        assert!(is_media(&PathBuf::from("photo.jpg")));
        assert!(is_media(&PathBuf::from("video.mp4")));
        assert!(is_media(&PathBuf::from("image.webp")));
        assert!(!is_media(&PathBuf::from("document.pdf")));
    }

    #[test]
    fn test_clamp_zoom() {
        assert_eq!(clamp_zoom(0.5), 0.5);
        assert_eq!(clamp_zoom(1.0), 1.0);
        assert_eq!(clamp_zoom(10.0), 10.0);
        assert_eq!(clamp_zoom(0.01), ZOOM_MIN);
        assert_eq!(clamp_zoom(100.0), ZOOM_MAX);
        assert_eq!(clamp_zoom(-5.0), ZOOM_MIN);
    }

    #[test]
    fn test_format_time() {
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(1_000_000), "0:01");
        assert_eq!(format_time(60_000_000), "1:00");
        assert_eq!(format_time(90_500_000), "1:30");
        assert_eq!(format_time(3_661_000_000), "61:01");
    }

    #[test]
    fn test_natural_sort() {
        let mut names = vec!["IMG10.jpg", "IMG2.jpg", "IMG1.jpg", "img02.jpg"];
        names.sort_by_key(|a| natural_key(a));
        assert_eq!(
            names,
            vec!["IMG1.jpg", "IMG2.jpg", "img02.jpg", "IMG10.jpg"]
        );
    }

    #[test]
    fn test_collect_media_empty_missing() {
        let files = collect_media(Path::new("/nonexistent-dir-xyz"));
        assert!(files.is_empty());
    }

    /// `is_file() && is_media()` is the rule the drop handler now mirrors
    /// (IMPROVEMENTS §1.5): only *regular* files with a media extension
    /// get a viewer slot. A dangling symlink named `*.jpg` must not — its
    /// extension lies and `is_file()` says no — while a symlink *to* a
    /// real image does, because stat follows the link.
    #[test]
    fn test_collect_media_skips_non_regular_and_non_media() {
        let dir = std::env::temp_dir().join(format!("carosello-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("real.jpg"), b"jpeg").unwrap();
        std::fs::write(dir.join("notes.txt"), b"text").unwrap();
        std::os::unix::fs::symlink(dir.join("missing-target"), dir.join("dangling.jpg")).unwrap();
        std::os::unix::fs::symlink(dir.join("real.jpg"), dir.join("link.jpg")).unwrap();

        let mut got = collect_media(&dir);
        got.sort();
        assert_eq!(
            got,
            vec![dir.join("link.jpg"), dir.join("real.jpg")],
            "regular media files only (dangling symlink excluded)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_slide_enabled_defaults_on() {
        let dir = std::env::temp_dir().join("carosello-pref-missing-xyz");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_slide_enabled_from(&dir));
        assert!(parse_slide_enabled(""));
        assert!(parse_slide_enabled("# comment\n"));
    }

    #[test]
    fn test_slide_enabled_roundtrip() {
        let dir = std::env::temp_dir().join(format!("carosello-pref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save_slide_enabled_to(&dir, false);
        assert!(!load_slide_enabled_from(&dir));
        save_slide_enabled_to(&dir, true);
        assert!(load_slide_enabled_from(&dir));
        // Unknown values and comments fall back to enabled.
        std::fs::write(dir.join("settings.conf"), "# hi\nslide-animation = maybe\n").unwrap();
        assert!(load_slide_enabled_from(&dir));
        std::fs::write(dir.join("settings.conf"), "slide-animation=FALSE\n").unwrap();
        assert!(!load_slide_enabled_from(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_two_finger_swipe_defaults_off() {
        let dir = std::env::temp_dir().join("carosello-pref-2f-missing-xyz");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!load_two_finger_swipe_from(&dir));
        assert!(!parse_two_finger_swipe(""));
        assert!(!parse_two_finger_swipe("# comment\n"));
        assert!(!parse_two_finger_swipe("two-finger-swipe = maybe\n"));
        assert!(parse_two_finger_swipe("two-finger-swipe = true\n"));
        assert!(parse_two_finger_swipe("two-finger-swipe=TRUE\n"));
        assert!(!parse_two_finger_swipe("two-finger-swipe = false\n"));
    }

    #[test]
    fn test_two_finger_swipe_roundtrip() {
        let dir = std::env::temp_dir().join(format!("carosello-pref-2f-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save_two_finger_swipe_to(&dir, true);
        assert!(load_two_finger_swipe_from(&dir));
        save_two_finger_swipe_to(&dir, false);
        assert!(!load_two_finger_swipe_from(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_prefs_preserve_each_other() {
        let dir = std::env::temp_dir().join(format!("carosello-pref-both-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save_slide_enabled_to(&dir, false);
        save_two_finger_swipe_to(&dir, true);
        assert!(!load_slide_enabled_from(&dir));
        assert!(load_two_finger_swipe_from(&dir));
        // Toggling one must not clobber the other.
        save_slide_enabled_to(&dir, true);
        assert!(load_slide_enabled_from(&dir));
        assert!(load_two_finger_swipe_from(&dir));
        save_two_finger_swipe_to(&dir, false);
        assert!(load_slide_enabled_from(&dir));
        assert!(!load_two_finger_swipe_from(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_prefs_reads_all_at_once() {
        let dir = std::env::temp_dir().join(format!("carosello-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Missing file → defaults: slide on, two-finger off, video muted.
        assert_eq!(load_prefs_from(&dir), (true, false, true));
        save_slide_enabled_to(&dir, false);
        save_two_finger_swipe_to(&dir, true);
        save_video_muted_to(&dir, false);
        assert_eq!(load_prefs_from(&dir), (false, true, false));
        // The *_from delegates must report exactly the same triple.
        assert_eq!(
            (
                load_slide_enabled_from(&dir),
                load_two_finger_swipe_from(&dir),
                load_video_muted_from(&dir)
            ),
            load_prefs_from(&dir)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_video_muted_defaults_on() {
        let dir = std::env::temp_dir().join("carosello-video-muted-missing-xyz");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_video_muted_from(&dir));
        assert!(parse_video_muted(""));
        assert!(parse_video_muted("# comment\n"));
        assert!(parse_video_muted("video-muted = maybe\n"));
        // Unmuted only on an explicit false (the remembered unmute).
        assert!(!parse_video_muted("video-muted = false\n"));
        assert!(!parse_video_muted("video-muted=FALSE\n"));
        assert!(parse_video_muted("video-muted = true\n"));
    }

    #[test]
    fn test_video_muted_roundtrip() {
        let dir = std::env::temp_dir().join(format!("carosello-vmute-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save_video_muted_to(&dir, false);
        assert!(!load_video_muted_from(&dir));
        save_video_muted_to(&dir, true);
        assert!(load_video_muted_from(&dir));
        // Remembering the mute choice must not clobber the other prefs.
        save_slide_enabled_to(&dir, false);
        save_video_muted_to(&dir, false);
        assert!(!load_slide_enabled_from(&dir));
        assert!(!load_video_muted_from(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_index_after_removal_next_slides_in() {
        // Removing the current item keeps the index: the next sibling
        // slides into place (no reset to 0).
        assert_eq!(index_after_removal(3, 1, 1), 1);
        assert_eq!(index_after_removal(3, 0, 0), 0);
    }

    #[test]
    fn test_index_after_removal_last_falls_back() {
        // Removing the last item shows the new last item.
        assert_eq!(index_after_removal(3, 3, 3), 2);
        assert_eq!(index_after_removal(1, 1, 1), 0);
    }

    #[test]
    fn test_index_after_removal_before_shifts_left() {
        // Removing an item before the current one shifts the index left.
        assert_eq!(index_after_removal(3, 2, 0), 1);
        // Removing an item after the current one changes nothing.
        assert_eq!(index_after_removal(3, 0, 2), 0);
    }

    #[test]
    fn test_index_after_removal_empty() {
        assert_eq!(index_after_removal(0, 0, 0), 0);
    }

    #[test]
    fn test_fling_complete_by_progress() {
        assert!(fling_complete(0.3, 0.0, 1));
        assert!(fling_complete(0.3, 0.0, -1));
        assert!(fling_complete(1.0, 0.0, 1));
    }

    #[test]
    fn test_fling_complete_by_velocity() {
        // Next travels left: fast negative velocity commits.
        assert!(fling_complete(0.0, -2.0, 1));
        assert!(!fling_complete(0.0, 2.0, 1));
        // Previous travels right: mirror image.
        assert!(fling_complete(0.0, 2.0, -1));
        assert!(!fling_complete(0.0, -2.0, -1));
        // Wrong-direction fling never commits on its own.
        assert!(!fling_complete(0.1, 0.5, 1));
    }

    #[test]
    fn test_settle_micros_scaled() {
        assert_eq!(settle_micros(900.0, 900.0), SLIDE_MICROS);
        assert_eq!(settle_micros(450.0, 900.0), SLIDE_MICROS / 2);
        // Short snap-backs bottom out instead of flashing by.
        assert_eq!(settle_micros(1.0, 900.0), 60_000);
        assert_eq!(settle_micros(0.0, 900.0), 60_000);
        // Degenerate viewport falls back to the full duration.
        assert_eq!(settle_micros(10.0, 0.0), SLIDE_MICROS);
    }

    #[test]
    fn test_fling_velocity_samples() {
        assert_eq!(fling_velocity(&[]), 0.0);
        assert_eq!(fling_velocity(&[(0, 0.0)]), 0.0);
        // 200px over 100ms travelling left.
        assert_eq!(fling_velocity(&[(0, 0.0), (100_000, -200.0)]), -2.0);
        // Sub-millisecond spans yield zero instead of exploding.
        assert_eq!(fling_velocity(&[(0, 0.0), (500, -200.0)]), 0.0);
    }

    #[test]
    fn test_display_dims_transposes_only_rotated_orientations() {
        // 5..=8 are the transposing orientations (a phone portrait is
        // stored landscape + Orientation 6), everything else is axis-safe.
        assert_eq!(display_dims(4032, 3024, 1), (4032, 3024));
        assert_eq!(display_dims(4032, 3024, 3), (4032, 3024));
        assert_eq!(display_dims(4032, 3024, 6), (3024, 4032));
        assert_eq!(display_dims(4032, 3024, 8), (3024, 4032));
        // Out-of-range junk reads as orientation 1 upstream, and a
        // degenerate header must saturate rather than wrap.
        assert_eq!(display_dims(4032, 3024, 0), (4032, 3024));
        assert_eq!(display_dims(u32::MAX, u32::MAX, 6), (i32::MAX, i32::MAX));
    }

    #[test]
    fn test_probe_dimensions_reads_image_header() {
        let dir = std::env::temp_dir().join(format!("carosello-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("probe.png");
        image::DynamicImage::new_rgb8(7, 5)
            .save(&png)
            .expect("write png");
        assert_eq!(probe_dimensions(&png), Some((7, 5)));
        // Videos and anything unreadable probe as "unknown": the window
        // then keeps its default size (a video takes the deferred fit).
        assert_eq!(probe_dimensions(&dir.join("clip.mp4")), None);
        assert_eq!(probe_dimensions(&dir.join("missing.jpg")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
