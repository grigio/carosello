use std::path::Path;

pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "tif"];
pub const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mkv"];
pub const ZOOM_MAX: f64 = 20.0;
pub const ZOOM_MIN: f64 = 0.1;
pub const MAX_DIM: f64 = 20000.0;

pub fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn is_media(path: &Path) -> bool {
    has_ext(path, IMAGE_EXTS) || has_ext(path, VIDEO_EXTS)
}

pub fn clamp_zoom(z: f64) -> f64 {
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

pub fn format_time(micros: i64) -> String {
    let total_secs = micros / 1_000_000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}

pub fn debug_log(msg: &str) {
    if std::env::var("CAROUSEL_DEBUG").is_ok() {
        eprintln!("[carousel-debug] {msg}");
    }
}
