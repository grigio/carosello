//! Pure rotate/mirror pipeline: decode → EXIF-orient → transform → encode
//! → re-inject patched EXIF. Everything works on plain byte slices and
//! `image::DynamicImage` so it can run on a worker thread with Send data
//! only (no `gdk_pixbuf::Pixbuf` / `Rc` — see AGENTS.md).
//!
//! Saved pixels are always display-oriented (EXIF orientation baked in),
//! with the JPEG Orientation tag patched back to 1, so a reload shows the
//! exact same picture without double rotation.

use std::io::Cursor;

use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, DynamicImage, ImageFormat};

use crate::media;
use crate::state::debug_log;

/// JPEG encode quality: high on purpose, every rotate re-encodes in place.
const JPEG_QUALITY: u8 = 95;

/// EXIF tag 0x0112 (Orientation), type SHORT.
const TAG_ORIENTATION: u16 = 0x0112;
/// EXIF SHORT type code.
const EXIF_TYPE_SHORT: u16 = 3;

#[derive(Clone, Copy, Debug)]
pub enum Transform {
    RotateLeft,
    RotateRight,
    /// Left ↔ right reflection (mirror across the vertical axis).
    MirrorH,
}

/// Transform image `bytes` (format chosen by `ext`, e.g. `"jpg"`) and
/// encode the result. Display-oriented pixels out, EXIF preserved for JPEG.
pub fn transform_bytes(bytes: &[u8], ext: &str, op: Transform) -> Result<Vec<u8>, String> {
    let ext = ext.to_ascii_lowercase();
    let format =
        ImageFormat::from_extension(&ext).ok_or_else(|| format!("Unsupported format .{ext}"))?;
    reject_animation(bytes, format)?;
    // Same orientation source the viewer uses, so saved pixels match the
    // screen exactly (see media::read_exif_orientation).
    let orientation = media::read_exif_orientation_bytes(bytes);
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("Cannot decode image: {e}"))?;
    let img = apply_exif_orientation(&img, orientation);
    let img = apply_transform(&img, op);
    let mut out = encode(&img, format)?;
    if format == ImageFormat::Jpeg {
        // None → drop EXIF rather than ship a stale Orientation tag.
        match extract_exif_app1(bytes).and_then(|p| patch_exif(p, orientation)) {
            Some(payload) => inject_exif_app1(&mut out, &payload),
            None => debug_log("transform: no usable EXIF to preserve"),
        }
    }
    Ok(out)
}

/// Refuse animated GIF/WebP: re-encoding would freeze them on frame one.
fn reject_animation(bytes: &[u8], format: ImageFormat) -> Result<(), String> {
    match format {
        ImageFormat::Gif => {
            let dec = GifDecoder::new(Cursor::new(bytes))
                .map_err(|e| format!("Cannot decode GIF: {e}"))?;
            // Frames decode lazily: consuming the second one proves the
            // GIF is animated without decoding the whole animation.
            let mut frames = dec.into_frames();
            let _ = frames.next();
            if frames.next().is_some() {
                return Err("Can't transform an animated GIF".into());
            }
        }
        ImageFormat::WebP => {
            let dec = WebPDecoder::new(Cursor::new(bytes))
                .map_err(|e| format!("Cannot decode WebP: {e}"))?;
            if dec.has_animation() {
                return Err("Can't transform an animated WebP".into());
            }
        }
        _ => {}
    }
    Ok(())
}

/// Bake EXIF orientation into the pixels. The match arms mirror
/// `media::apply_orientation` (gdk-pixbuf) one for one, so what we save is
/// what the viewer showed — keep both in sync.
fn apply_exif_orientation(img: &DynamicImage, orientation: u8) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate270().fliph(),
        6 => img.rotate90(),
        7 => img.rotate90().fliph(),
        8 => img.rotate270(),
        _ => img.clone(),
    }
}

fn apply_transform(img: &DynamicImage, op: Transform) -> DynamicImage {
    match op {
        // image crate rotations are clockwise, hence the names.
        Transform::RotateLeft => img.rotate270(),
        Transform::RotateRight => img.rotate90(),
        Transform::MirrorH => img.fliph(),
    }
}

fn encode(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    if format == ImageFormat::Jpeg {
        let enc = JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
        img.write_with_encoder(enc)
            .map_err(|e| format!("Cannot encode JPEG: {e}"))?;
    } else {
        img.write_to(&mut out, format)
            .map_err(|e| format!("Cannot encode image: {e}"))?;
    }
    Ok(out.into_inner())
}

// ── JPEG APP1 / EXIF byte surgery ──────────────────────────────────────

/// First APP1 segment payload starting with `Exif\0\0` (header included).
fn extract_exif_app1(jpeg: &[u8]) -> Option<&[u8]> {
    if jpeg.get(0..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut pos = 2usize;
    loop {
        if jpeg.get(pos) != Some(&0xFF) {
            return None;
        }
        pos += 1;
        while jpeg.get(pos) == Some(&0xFF) {
            pos += 1; // fill bytes
        }
        let marker = *jpeg.get(pos)?;
        pos += 1;
        match marker {
            // Standalone markers without a length field.
            0x01 | 0xD0..=0xD7 => continue,
            // EOI/SOS: no more metadata ahead of the scan data.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let len = u16::from_be_bytes([*jpeg.get(pos)?, *jpeg.get(pos + 1)?]) as usize;
        if len < 2 {
            return None;
        }
        let payload = jpeg.get(pos + 2..pos + len)?;
        if marker == 0xE1 && payload.starts_with(b"Exif\0\0") {
            return Some(payload);
        }
        pos += len;
    }
}

/// Patch an APP1 payload in place: set IFD0 Orientation to 1 (the pixels
/// are display-oriented now) and zero the next-IFD pointer so the stale
/// pre-transform thumbnail is dropped. `None` when the structure can't be
/// parsed or Orientation can't be guaranteed to read back as 1 — the
/// caller then drops EXIF entirely (correct display beats metadata).
fn patch_exif(payload: &[u8], orientation: u8) -> Option<Vec<u8>> {
    if payload.len() < 14 || &payload[..6] != b"Exif\0\0" {
        return None;
    }
    let mut out = payload.to_vec();
    let tiff = 6usize;
    let le = match out.get(tiff..tiff + 2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    if read_u16(&out, tiff + 2, le)? != 42 {
        return None;
    }
    let ifd0 = tiff + read_u32(&out, tiff + 4, le)? as usize;
    let n = read_u16(&out, ifd0, le)? as usize;
    let entries = ifd0.checked_add(2)?;
    let mut found = false;
    for i in 0..n {
        let e = entries.checked_add(i.checked_mul(12)?)?;
        if read_u16(&out, e, le)? == TAG_ORIENTATION
            && read_u16(&out, e + 2, le)? == EXIF_TYPE_SHORT
            && read_u32(&out, e + 4, le)? == 1
        {
            write_u16(&mut out, e + 8, 1, le)?;
            found = true;
        }
    }
    // Orientation absent means "1" to every reader — but only if the file
    // also told *us* 1 (otherwise we can't prove correctness → drop EXIF).
    if orientation != 1 && !found {
        return None;
    }
    // Drop IFD1 (thumbnail): it still shows the untransformed picture.
    let next_ifd = entries.checked_add(n.checked_mul(12)?)?;
    write_u32(&mut out, next_ifd, 0, le)?;
    Some(out)
}

/// Insert APP1 right after SOI, or after a leading JFIF APP0 (convention
/// when both segments are present). Silently skips oversized payloads.
fn inject_exif_app1(jpeg: &mut Vec<u8>, payload: &[u8]) {
    let seg_len = payload.len() + 2;
    if seg_len > u16::MAX as usize || jpeg.get(0..2) != Some(&[0xFF, 0xD8]) {
        return;
    }
    let mut at = 2usize;
    if jpeg.get(2..4) == Some(&[0xFF, 0xE0]) {
        if let Some(len) = jpeg
            .get(4..6)
            .map(|s| u16::from_be_bytes([s[0], s[1]]) as usize)
        {
            if len >= 2 && 4 + len <= jpeg.len() {
                at = 4 + len;
            }
        }
    }
    let mut seg = Vec::with_capacity(seg_len);
    seg.extend_from_slice(&[0xFF, 0xE1]);
    seg.extend_from_slice(&(seg_len as u16).to_be_bytes());
    seg.extend_from_slice(payload);
    let tail = jpeg.split_off(at);
    jpeg.extend(seg);
    jpeg.extend(tail);
}

fn read_u16(b: &[u8], at: usize, le: bool) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    let a = [s[0], s[1]];
    Some(if le {
        u16::from_le_bytes(a)
    } else {
        u16::from_be_bytes(a)
    })
}

fn write_u16(b: &mut [u8], at: usize, v: u16, le: bool) -> Option<()> {
    let s = b.get_mut(at..at.checked_add(2)?)?;
    let bytes = if le { v.to_le_bytes() } else { v.to_be_bytes() };
    s.copy_from_slice(&bytes);
    Some(())
}

fn read_u32(b: &[u8], at: usize, le: bool) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    let a = [s[0], s[1], s[2], s[3]];
    Some(if le {
        u32::from_le_bytes(a)
    } else {
        u32::from_be_bytes(a)
    })
}

fn write_u32(b: &mut [u8], at: usize, v: u32, le: bool) -> Option<()> {
    let s = b.get_mut(at..at.checked_add(4)?)?;
    let bytes = if le { v.to_le_bytes() } else { v.to_be_bytes() };
    s.copy_from_slice(&bytes);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// 2×1 RGBA: red on the left, blue on the right.
    fn two_pixels() -> RgbaImage {
        RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        })
    }

    fn px(img: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        img.get_pixel(x, y).0
    }

    #[test]
    fn test_rotate_right_clockwise() {
        // [red, blue] → red on top (left element goes up in a CW turn).
        let out = apply_transform(
            &DynamicImage::ImageRgba8(two_pixels()),
            Transform::RotateRight,
        );
        let out = out.into_rgba8();
        assert_eq!((out.width(), out.height()), (1, 2));
        assert_eq!(px(&out, 0, 0), [255, 0, 0, 255]);
        assert_eq!(px(&out, 0, 1), [0, 0, 255, 255]);
    }

    #[test]
    fn test_rotate_left_counterclockwise() {
        // [red, blue] → blue on top (left element goes down in a CCW turn).
        let out = apply_transform(
            &DynamicImage::ImageRgba8(two_pixels()),
            Transform::RotateLeft,
        );
        let out = out.into_rgba8();
        assert_eq!((out.width(), out.height()), (1, 2));
        assert_eq!(px(&out, 0, 0), [0, 0, 255, 255]);
        assert_eq!(px(&out, 0, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn test_mirror_horizontal_swaps_sides() {
        let out = apply_transform(&DynamicImage::ImageRgba8(two_pixels()), Transform::MirrorH);
        let out = out.into_rgba8();
        assert_eq!((out.width(), out.height()), (2, 1));
        assert_eq!(px(&out, 0, 0), [0, 0, 255, 255]);
        assert_eq!(px(&out, 1, 0), [255, 0, 0, 255]);
    }

    /// EXIF orientation must bake exactly like the viewer's gdk-pixbuf path,
    /// otherwise autosaved files differ from what was on screen.
    #[test]
    fn test_exif_orientation_matches_viewer_pixbuf() {
        use crate::media::apply_orientation;
        use gdk_pixbuf::{Colorspace, Pixbuf};
        use gtk::glib;

        let raw: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
        ];
        let bytes = glib::Bytes::from(&raw);
        let pb = Pixbuf::from_bytes(&bytes, Colorspace::Rgb, true, 8, 3, 1, 12);
        let img = DynamicImage::ImageRgba8(
            RgbaImage::from_raw(3, 1, raw.clone()).expect("3x1 rgba buffer"),
        );

        for orientation in 1u8..=8 {
            let want = apply_orientation(&pb, orientation);
            let got = apply_exif_orientation(&img, orientation);
            assert_eq!(
                (got.width(), got.height()),
                (
                    want.width().try_into().unwrap(),
                    want.height().try_into().unwrap()
                ),
                "orientation {orientation}: dimensions"
            );
            let rgba = got.into_rgba8();
            let want_bytes = want.read_pixel_bytes();
            let want_bytes = want_bytes.as_ref();
            let stride = want.rowstride() as usize;
            for y in 0..want.height() as usize {
                for x in 0..want.width() as usize {
                    let start = y * stride + x * 4;
                    assert_eq!(
                        &rgba.get_pixel(x as u32, y as u32).0[..],
                        &want_bytes[start..start + 4],
                        "orientation {orientation}: pixel ({x},{y})"
                    );
                }
            }
        }
    }

    /// Minimal little-endian EXIF APP1 payload with a single Orientation tag.
    fn synthetic_exif_le(orientation: u16, next_ifd: u32) -> Vec<u8> {
        let mut p = b"Exif\0\0".to_vec();
        p.extend_from_slice(b"II");
        p.extend_from_slice(&42u16.to_le_bytes());
        p.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at TIFF+8
        p.extend_from_slice(&1u16.to_le_bytes()); // one entry
        p.extend_from_slice(&TAG_ORIENTATION.to_le_bytes());
        p.extend_from_slice(&EXIF_TYPE_SHORT.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes()); // count
        p.extend_from_slice(&orientation.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes()); // value field padding
        p.extend_from_slice(&next_ifd.to_le_bytes());
        p
    }

    #[test]
    fn test_patch_exif_le_sets_orientation_and_drops_thumbnail() {
        // next_ifd points at a (bogus) IFD1; patching must zero it.
        let payload = synthetic_exif_le(6, 0x100);
        let patched = patch_exif(&payload, 6).expect("patchable");
        assert_eq!(read_exif_orientation(&patched), 1);
        // Everything else untouched, only the two patched fields differ.
        let mut expect = payload.clone();
        // orientation value at: 6 (header) + 8 (tiff hdr) + 2 (count) + 8 (tag/type/count) = 24
        expect[24] = 1;
        expect[25] = 0;
        // next-IFD at 6 + 8 + 2 + 12 = 28
        expect[28..32].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(patched, expect);
    }

    #[test]
    fn test_patch_exif_orientation_one_is_idempotent() {
        let payload = synthetic_exif_le(1, 0);
        let patched = patch_exif(&payload, 1).expect("patchable");
        assert_eq!(read_exif_orientation(&patched), 1);
    }

    #[test]
    fn test_patch_exif_requires_tag_when_rotated() {
        // Orientation ≠ 1 but the tag is missing → can't prove correctness.
        let mut payload = synthetic_exif_le(6, 0);
        // Turn the tag into something else (e.g. 0x010f Make).
        payload[16] = 0x0f;
        payload[17] = 0x01;
        assert!(patch_exif(&payload, 6).is_none());
        // …but dropping EXIF is fine when the file claimed orientation 1.
        assert!(patch_exif(&payload, 1).is_some());
    }

    #[test]
    fn test_patch_exif_big_endian() {
        let mut p = b"Exif\0\0".to_vec();
        p.extend_from_slice(b"MM");
        p.extend_from_slice(&42u16.to_be_bytes());
        p.extend_from_slice(&8u32.to_be_bytes());
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&TAG_ORIENTATION.to_be_bytes());
        p.extend_from_slice(&EXIF_TYPE_SHORT.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&6u16.to_be_bytes());
        p.extend_from_slice(&0u16.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        let patched = patch_exif(&p, 6).expect("patchable");
        assert_eq!(read_exif_orientation(&patched), 1);
    }

    #[test]
    fn test_extract_and_inject_app1_roundtrip() {
        // A bare SOI/EOI JPEG… …gains an APP1 right after SOI…
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xD9];
        let payload = synthetic_exif_le(6, 0);
        inject_exif_app1(&mut jpeg, &payload);
        assert_eq!(extract_exif_app1(&jpeg), Some(payload.as_slice()));
        // …and after a leading JFIF APP0 it lands behind that segment.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xD9];
        inject_exif_app1(&mut jpeg, &payload);
        assert_eq!(&jpeg[2..4], &[0xFF, 0xE0], "APP0 stays first");
        assert_eq!(extract_exif_app1(&jpeg), Some(payload.as_slice()));
    }

    fn encode_test_jpeg(img: &DynamicImage) -> Vec<u8> {
        encode(img, ImageFormat::Jpeg).expect("jpeg encode")
    }

    /// Wrap a bare JPEG in an EXIF APP1 carrying `orientation`.
    fn jpeg_with_exif(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let payload = synthetic_exif_le(orientation, 0);
        let mut out = vec![0xFF, 0xD8];
        let seg_len = payload.len() + 2;
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&(seg_len as u16).to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn test_transform_bytes_jpeg_preserves_exif_and_resets_orientation() {
        let img = DynamicImage::ImageRgba8(two_pixels());
        let src = jpeg_with_exif(&encode_test_jpeg(&img), 6);
        // Display path: 2x1 + orientation 6 (CW) → shown as 1x2.
        // Rotate right on screen → 2x1 again.
        let out = transform_bytes(&src, "JPG", Transform::RotateRight).expect("transform");
        let dec = image::load_from_memory_with_format(&out, ImageFormat::Jpeg).expect("decode");
        assert_eq!((dec.width(), dec.height()), (2, 1));
        // EXIF survived with Orientation reset — reload won't double-rotate.
        assert!(extract_exif_app1(&out).is_some(), "EXIF preserved");
        assert_eq!(media::read_exif_orientation_bytes(&out), 1);
    }

    #[test]
    fn test_transform_bytes_png_roundtrip_has_no_stale_orientation() {
        let img = DynamicImage::ImageRgba8(two_pixels());
        let mut src = Cursor::new(Vec::new());
        img.write_to(&mut src, ImageFormat::Png)
            .expect("png encode");
        let out = transform_bytes(&src.into_inner(), "png", Transform::RotateRight).expect("t");
        // PNG carries no EXIF from the encoder → orientation defaults to 1.
        assert_eq!(media::read_exif_orientation_bytes(&out), 1);
        let dec = image::load_from_memory_with_format(&out, ImageFormat::Png).expect("decode");
        assert_eq!((dec.width(), dec.height()), (1, 2));
    }

    fn animated_gif() -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};
        let mut buf = Cursor::new(Vec::new());
        let mut enc = GifEncoder::new(&mut buf);
        let f1 = RgbaImage::from_pixel(2, 1, Rgba([255, 0, 0, 255]));
        let f2 = RgbaImage::from_pixel(2, 1, Rgba([0, 0, 255, 255]));
        enc.encode_frame(Frame::from_parts(
            f1,
            0,
            0,
            Delay::from_numer_denom_ms(1, 10),
        ))
        .expect("frame 1");
        enc.encode_frame(Frame::from_parts(
            f2,
            0,
            0,
            Delay::from_numer_denom_ms(1, 10),
        ))
        .expect("frame 2");
        drop(enc); // release the &mut buf borrow
        buf.into_inner()
    }

    #[test]
    fn test_transform_bytes_refuses_animated_gif() {
        let err = transform_bytes(&animated_gif(), "gif", Transform::RotateRight)
            .expect_err("animated GIF must be refused");
        assert!(err.contains("animated GIF"), "got: {err}");
    }

    #[test]
    fn test_transform_bytes_static_gif_roundtrips() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};
        let mut buf = Cursor::new(Vec::new());
        let mut enc = GifEncoder::new(&mut buf);
        let f = RgbaImage::from_pixel(2, 1, Rgba([255, 0, 0, 255]));
        enc.encode_frame(Frame::from_parts(
            f,
            0,
            0,
            Delay::from_numer_denom_ms(1, 10),
        ))
        .expect("frame");
        drop(enc); // release the &mut buf borrow
        let out = transform_bytes(&buf.into_inner(), "gif", Transform::RotateRight).expect("t");
        let dec = image::load_from_memory_with_format(&out, ImageFormat::Gif).expect("decode");
        assert_eq!((dec.width(), dec.height()), (1, 2));
    }

    #[test]
    fn test_transform_bytes_webp_roundtrip() {
        let img = DynamicImage::ImageRgba8(two_pixels());
        let mut src = Cursor::new(Vec::new());
        img.write_to(&mut src, ImageFormat::WebP)
            .expect("webp encode");
        let out = transform_bytes(&src.into_inner(), "webp", Transform::MirrorH).expect("t");
        let dec = image::load_from_memory_with_format(&out, ImageFormat::WebP).expect("decode");
        assert_eq!((dec.width(), dec.height()), (2, 1));
        let rgba = dec.into_rgba8();
        assert_eq!(rgba.get_pixel(0, 0).0, [0, 0, 255, 255]); // swapped
    }

    #[test]
    fn test_transform_bytes_unknown_extension_errors() {
        let err = transform_bytes(b"whatever", "xyz", Transform::RotateRight)
            .expect_err("unsupported ext");
        assert!(err.contains("Unsupported format"), "got: {err}");
    }

    /// Read the Orientation tag back out of a patched payload.
    fn read_exif_orientation(payload: &[u8]) -> u16 {
        let tiff = 6;
        let le = &payload[tiff..tiff + 2] == b"II";
        let ifd0 = tiff + read_u32(payload, tiff + 4, le).expect("ifd0") as usize;
        let n = read_u16(payload, ifd0, le).expect("count");
        for i in 0..n {
            let e = ifd0 + 2 + i as usize * 12;
            if read_u16(payload, e, le).expect("tag") == TAG_ORIENTATION {
                return read_u16(payload, e + 8, le).expect("value");
            }
        }
        panic!("orientation tag missing");
    }
}
