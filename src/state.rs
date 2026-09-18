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
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let enabled = ENABLED.get_or_init(|| std::env::var("CAROSELLO_DEBUG").is_ok());
    if *enabled {
        eprintln!("[carosello-debug] {msg}");
    }
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
}
