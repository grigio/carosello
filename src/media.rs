use std::path::Path;

use gtk::gdk;
use gtk::gdk_pixbuf;

pub fn read_exif_orientation(path: &Path) -> u8 {
    let Ok(file) = std::fs::File::open(path) else {
        return 1;
    };
    orientation_from_reader(&mut std::io::BufReader::new(file))
}

/// Same as [`read_exif_orientation`] but from in-memory bytes (worker
/// threads read the file once and parse it there).
pub fn read_exif_orientation_bytes(bytes: &[u8]) -> u8 {
    orientation_from_reader(&mut std::io::BufReader::new(std::io::Cursor::new(bytes)))
}

fn orientation_from_reader<R: std::io::BufRead + std::io::Seek>(reader: &mut R) -> u8 {
    let Ok(exif) = exif::Reader::new().read_from_container(reader) else {
        return 1;
    };
    let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) else {
        return 1;
    };
    match &field.value {
        exif::Value::Short(v) if !v.is_empty() => v[0] as u8,
        _ => 1,
    }
}

pub fn apply_orientation(pixbuf: &gdk_pixbuf::Pixbuf, orientation: u8) -> gdk_pixbuf::Pixbuf {
    match orientation {
        2 => pixbuf.flip(true).unwrap_or_else(|| pixbuf.clone()),
        3 => pixbuf
            .rotate_simple(gdk_pixbuf::PixbufRotation::Upsidedown)
            .unwrap_or_else(|| pixbuf.clone()),
        4 => pixbuf.flip(false).unwrap_or_else(|| pixbuf.clone()),
        5 => pixbuf
            .rotate_simple(gdk_pixbuf::PixbufRotation::Counterclockwise)
            .and_then(|pb| pb.flip(true))
            .unwrap_or_else(|| pixbuf.clone()),
        6 => pixbuf
            .rotate_simple(gdk_pixbuf::PixbufRotation::Clockwise)
            .unwrap_or_else(|| pixbuf.clone()),
        7 => pixbuf
            .rotate_simple(gdk_pixbuf::PixbufRotation::Clockwise)
            .and_then(|pb| pb.flip(true))
            .unwrap_or_else(|| pixbuf.clone()),
        8 => pixbuf
            .rotate_simple(gdk_pixbuf::PixbufRotation::Counterclockwise)
            .unwrap_or_else(|| pixbuf.clone()),
        _ => pixbuf.clone(),
    }
}

pub fn texture_for_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf) -> gdk::Texture {
    gdk::Texture::for_pixbuf(pixbuf)
}
