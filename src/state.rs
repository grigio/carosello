use std::path::{Path, PathBuf};

pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "tif"];
pub const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mkv"];
pub const ZOOM_MAX: f64 = 20.0;
pub const ZOOM_MIN: f64 = 0.1;
pub const MAX_DIM: f64 = 8192.0;

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
            // Leading zeros affect width tie-break so IMG02 < IMG2 < IMG10 stays stable.
            let n = buf.parse::<u64>().unwrap_or(u64::MAX);
            NatPart::Num(n, buf.len())
        } else {
            NatPart::Str(buf.to_ascii_lowercase())
        }
    }
}

/// List media files in `dir`, sorted naturally by filename.
pub fn collect_media(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_media(p))
        .collect();
    files.sort_by(|a, b| {
        let an = a.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let bn = b.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        natural_key(an).cmp(&natural_key(bn))
    });
    files
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
}
