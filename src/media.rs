//! EXIF helpers shared by the display decode path and the transform
//! pipeline. Pure byte-level code — safe to run on a worker thread
//! (no GTK types, see AGENTS.md).

/// Read the EXIF Orientation tag (1..=8, default 1) from in-memory file
/// bytes (workers read the file once and parse it there).
///
/// Anything outside `1..=8` — a missing tag, a wrong value type, `0`,
/// `9+`, a truncated payload, no EXIF at all — reads as `1`, which is
/// both the spec's default and the only value every reader agrees on.
/// The rest of the pipeline relies on that: `transform::decode_frame`
/// and `transform_bytes` hand the result straight to
/// `apply_exif_orientation`, and the save path writes it back as the
/// file's Orientation, so an out-of-range value leaking through would be
/// baked into the saved EXIF.
pub fn read_exif_orientation_bytes(bytes: &[u8]) -> u8 {
    orientation_from_reader(&mut std::io::BufReader::new(std::io::Cursor::new(bytes)))
}

/// Read the EXIF Orientation tag from a seekable stream — the twin of
/// [`read_exif_orientation_bytes`] for callers that already have a file
/// open (the startup dimension probe): same parser, no need to buffer the
/// whole file just to learn the orientation.
pub fn read_exif_orientation<R: std::io::BufRead + std::io::Seek>(reader: &mut R) -> u8 {
    orientation_from_reader(reader)
}

fn orientation_from_reader<R: std::io::BufRead + std::io::Seek>(reader: &mut R) -> u8 {
    let Ok(exif) = exif::Reader::new().read_from_container(reader) else {
        return 1;
    };
    let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) else {
        return 1;
    };
    match &field.value {
        // Exactly one SHORT in 1..=8 — a wrong count (spec says 1) or an
        // out-of-range value is a malformed tag, not a transform.
        exif::Value::Short(v) if v.len() == 1 && (1..=8).contains(&v[0]) => v[0] as u8,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bare JPEG framing (SOI + APP0 + EOI). The EXIF reader only walks
    /// container segments, it never decodes image data.
    fn bare_jpeg() -> Vec<u8> {
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xD9]
    }

    /// `Exif\0\0` + a minimal little-endian TIFF/IFD0 with a single
    /// Orientation entry, parameterised so tests can forge a wrong value
    /// type or count. Mirrors `synthetic_exif_le` in transform.rs (kept
    /// local: the two files' builders serve different shapes).
    fn exif_payload(value: u16, type_: u16, count: u32) -> Vec<u8> {
        let mut p = b"Exif\0\0".to_vec();
        p.extend_from_slice(b"II");
        p.extend_from_slice(&42u16.to_le_bytes());
        p.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at TIFF+8
        p.extend_from_slice(&1u16.to_le_bytes()); // one entry
        p.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
        p.extend_from_slice(&type_.to_le_bytes());
        p.extend_from_slice(&count.to_le_bytes());
        // SHORT occupies the low 2 of the 4 value bytes; LONG uses all 4.
        if type_ == 4 {
            p.extend_from_slice(&u32::from(value).to_le_bytes());
        } else {
            p.extend_from_slice(&value.to_le_bytes());
            p.extend_from_slice(&0u16.to_le_bytes());
        }
        p.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        p
    }

    /// JPEG with APP1 carrying `payload`, right after SOI.
    fn jpeg_with_app1(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    fn orientation_of(value: u16) -> u8 {
        read_exif_orientation_bytes(&jpeg_with_app1(&exif_payload(value, 3, 1)))
    }

    /// Every legal orientation must survive the round trip: this value
    /// feeds *both* the pixels on screen and the tag written back on
    /// save, so an off-by-one rotates the picture the wrong way twice.
    #[test]
    fn test_orientations_1_to_8_round_trip() {
        for value in 1u16..=8 {
            assert_eq!(orientation_of(value), value as u8, "orientation {value}");
        }
    }

    /// Out-of-range values fall back to 1 (the documented contract), not
    /// to a truncated `u16 as u8`.
    #[test]
    fn test_out_of_range_orientation_defaults_to_one() {
        assert_eq!(orientation_of(0), 1, "0 is not a valid orientation");
        assert_eq!(orientation_of(9), 1, "9+ is not a valid orientation");
        assert_eq!(orientation_of(u16::MAX), 1, "truncation must not leak");
    }

    /// A SHORT-tagged Orientation of the right shape but the wrong type
    /// or count is "no orientation" to every reader.
    #[test]
    fn test_wrong_value_type_defaults_to_one() {
        // LONG (4) instead of SHORT (3).
        let long = jpeg_with_app1(&exif_payload(6, 4, 1));
        assert_eq!(read_exif_orientation_bytes(&long), 1, "LONG value");
        // SHORT but count 2 (so the value field holds two entries).
        let count2 = jpeg_with_app1(&exif_payload(6, 3, 2));
        assert_eq!(read_exif_orientation_bytes(&count2), 1, "count != 1");
    }

    /// Files with no usable EXIF read as 1 — the cases that must never
    /// panic, because they run on a worker thread with no way to report
    /// anything but a `Result`.
    #[test]
    fn test_missing_or_malformed_exif_defaults_to_one() {
        assert_eq!(read_exif_orientation_bytes(&[]), 1, "empty input");
        assert_eq!(read_exif_orientation_bytes(&bare_jpeg()), 1, "no APP1");
        // JPEG with an EXIF-less APP1 (JFIF).
        assert_eq!(
            read_exif_orientation_bytes(&jpeg_with_app1(b"JFIF\0")),
            1,
            "APP1 without Exif header"
        );
        // Random bytes: no SOI, no TIFF header.
        let noise: Vec<u8> = (0u8..=255).collect();
        assert_eq!(read_exif_orientation_bytes(&noise), 1, "noise");
        // Truncated *inside* the APP1/TIFF data: mid-marker, mid-header,
        // mid-entry. (Trailing bytes are a different story — see below.)
        let full = jpeg_with_app1(&exif_payload(6, 3, 1));
        for cut in [1usize, 6, 14, 22, 30] {
            assert_eq!(
                read_exif_orientation_bytes(&full[..cut]),
                1,
                "truncated at {cut}"
            );
        }
        // Losing only trailing bytes (EOI) leaves the EXIF payload
        // intact, so it still parses: truncation is not assumed to fail,
        // only *structural* damage is.
        assert_eq!(read_exif_orientation_bytes(&full[..full.len() - 1]), 6);
    }
}
