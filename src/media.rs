//! EXIF helpers shared by the display decode path and the transform
//! pipeline. Pure byte-level code — safe to run on a worker thread
//! (no GTK types, see AGENTS.md).

/// Read the EXIF Orientation tag (1..=8, default 1) from in-memory file
/// bytes (workers read the file once and parse it there).
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
