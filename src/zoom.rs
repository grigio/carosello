use gtk::gdk::prelude::PaintableExt;

use crate::state::MAX_DIM;

/// Paintable wrapper that scales its inner paintable to an explicit
/// display size through a snapshot transform.
///
/// A raw MediaStream paintable renders at its native size no matter
/// what allocation it is snapshotted into, so growing the widget
/// allocation (the image-zoom mechanism) has no visible effect on
/// video. Drawing the inner paintable at the native size it honors,
/// inside a pre-scaled transform, produces correctly scaled output
/// for any inner paintable while keeping playback live.
mod zoom_paintable {
    use std::cell::{Cell, RefCell};

    use gtk::gdk;
    use gtk::gdk::subclass::prelude::*;
    use gtk::glib;
    use gtk::glib::clone::Downgrade;
    use gtk::graphene;
    use gtk::prelude::*;

    #[derive(Default)]
    pub struct ZoomPaintableImp {
        inner: RefCell<Option<gdk::Paintable>>,
        base_w: Cell<f64>,
        base_h: Cell<f64>,
        zoom: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ZoomPaintableImp {
        const NAME: &'static str = "CaroselloZoomPaintable";
        type Type = ZoomPaintable;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for ZoomPaintableImp {}

    impl PaintableImpl for ZoomPaintableImp {
        fn intrinsic_width(&self) -> i32 {
            (self.base_w.get() * self.zoom.get()).round() as i32
        }

        fn intrinsic_height(&self) -> i32 {
            (self.base_h.get() * self.zoom.get()).round() as i32
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else {
                return;
            };
            let Some(inner) = self.inner.borrow().clone() else {
                return;
            };
            let iw = inner.intrinsic_width() as f64;
            let ih = inner.intrinsic_height() as f64;
            if iw < 1.0 || ih < 1.0 {
                return;
            }
            let dw = (self.base_w.get() * self.zoom.get()).max(1.0);
            let dh = (self.base_h.get() * self.zoom.get()).max(1.0);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                ((width - dw) / 2.0) as f32,
                ((height - dh) / 2.0) as f32,
            ));
            snapshot.scale((dw / iw) as f32, (dh / ih) as f32);
            inner.snapshot(snapshot, iw, ih);
            snapshot.restore();
        }
    }

    glib::wrapper! {
        pub struct ZoomPaintable(ObjectSubclass<ZoomPaintableImp>)
            @implements gdk::Paintable;
    }

    impl ZoomPaintable {
        pub fn new() -> Self {
            glib::Object::new()
        }

        pub fn set_inner(&self, inner: Option<gdk::Paintable>) {
            if let Some(ref p) = inner {
                let weak = Downgrade::downgrade(self);
                p.connect_invalidate_contents(move |_| {
                    if let Some(s) = weak.upgrade() {
                        s.invalidate_contents();
                    }
                });
            }
            *self.imp().inner.borrow_mut() = inner;
            self.invalidate_size();
        }

        pub fn set_view(&self, base_w: f64, base_h: f64, zoom: f64) {
            let imp = self.imp();
            imp.base_w.set(base_w.max(1.0));
            imp.base_h.set(base_h.max(1.0));
            imp.zoom.set(crate::state::clamp_zoom(zoom));
            self.invalidate_size();
        }

        pub fn view(&self) -> (f64, f64, f64) {
            let imp = self.imp();
            (imp.base_w.get(), imp.base_h.get(), imp.zoom.get())
        }
    }

    impl Default for ZoomPaintable {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub use zoom_paintable::ZoomPaintable;

/// Intrinsic (native) size of the current media in pixels.
pub fn video_intrinsic(media: &gtk::MediaFile) -> Option<(f64, f64)> {
    let w = media.intrinsic_width() as f64;
    let h = media.intrinsic_height() as f64;
    if w >= 1.0 && h >= 1.0 {
        return Some((w, h));
    }
    let img = media.current_image();
    let w = img.intrinsic_width() as f64;
    let h = img.intrinsic_height() as f64;
    if w >= 1.0 && h >= 1.0 {
        Some((w, h))
    } else {
        None
    }
}

pub fn intrinsic_size(
    is_video: bool,
    video_w: i32,
    video_h: i32,
    image_w: i32,
    image_h: i32,
    media_file: &Option<gtk::MediaFile>,
) -> Option<(f64, f64)> {
    if is_video {
        if video_w > 0 && video_h > 0 {
            return Some((video_w as f64, video_h as f64));
        }
        if let Some(ref media) = media_file {
            return video_intrinsic(media);
        }
        return None;
    }
    if image_w > 0 && image_h > 0 {
        Some((image_w as f64, image_h as f64))
    } else {
        None
    }
}

/// Fit size preserving aspect ratio to fill available viewport.
pub fn fit_size(iw: f64, ih: f64, vw: f64, vh: f64) -> (f64, f64) {
    if iw < 1.0 || ih < 1.0 || vw < 1.0 || vh < 1.0 {
        return (iw.max(1.0), ih.max(1.0));
    }
    let scale = (vw / iw).min(vh / ih);
    ((iw * scale).max(1.0), (ih * scale).max(1.0))
}

pub fn display_size_for(iw: f64, ih: f64, vw: f64, vh: f64, zoom: f64) -> (i32, i32) {
    let (fw, fh) = fit_size(iw, ih, vw, vh);
    let dw = ((fw * zoom).round()).clamp(1.0, MAX_DIM) as i32;
    let dh = ((fh * zoom).round()).clamp(1.0, MAX_DIM) as i32;
    (dw.max(1), dh.max(1))
}

/// Visible content size for anchor math.
///
/// Images render through `GtkPicture:content-fit=Contain` in a viewport-sized
/// allocation, so zoom levels below fit still display at fit (shrinking is a
/// no-op). Videos render through `ZoomPaintable`, which draws centered at the
/// exact display size, so they really do shrink. Using the raw display size
/// for images therefore inflates the zoom ratio (e.g. 0.38 → 2.5 looks like
/// 6.6× instead of 2.5×) and the re-anchor clamps to a corner instead of
/// keeping the cursor point stable.
pub fn effective_display_size_for(
    iw: f64,
    ih: f64,
    vw: f64,
    vh: f64,
    zoom: f64,
    is_video: bool,
) -> (i32, i32) {
    if is_video {
        display_size_for(iw, ih, vw, vh, zoom)
    } else {
        display_size_for(iw, ih, vw, vh, zoom.max(1.0))
    }
}

/// Scroll target keeping the viewport point `anchor` stable across a zoom.
///
/// `old_scroll` is the current adjustment value, `vw` the viewport size,
/// `old_w`/`new_w` the visible content sizes (see
/// [`effective_display_size_for`]). Content smaller than the viewport is
/// centered, so the centering offsets are accounted for; an anchor in the
/// padding falls back to the viewport center. Returns `(target, max)` where
/// `max` is the new scroll range (`new_w - vw`, floored at 0).
pub fn anchor_target(old_scroll: f64, vw: f64, old_w: f64, new_w: f64, anchor: f64) -> (f64, f64) {
    let old_w = old_w.max(1.0);
    let new_w = new_w.max(1.0);
    let vw = vw.max(1.0);
    let off_old = (vw - old_w).max(0.0) / 2.0;
    let off_new = (vw - new_w).max(0.0) / 2.0;
    // Padding click: zoom to the center instead of flinging to an edge.
    let ax = if anchor < off_old || anchor > off_old + old_w {
        vw / 2.0
    } else {
        anchor
    };
    let ratio = new_w / old_w;
    let target = (old_scroll + ax - off_old) * ratio - ax + off_new;
    let max = (new_w - vw).max(0.0);
    (target, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_images_clamp_to_fit() {
        // 16:9 image in a 16:9 viewport: fit == viewport.
        let (fw, fh) = (892.0, 501.0);
        let (dw, _) = display_size_for(4032.0, 2268.0, 892.0, 501.0, 0.38);
        assert!(dw < fw as i32);
        let (ew, eh) = effective_display_size_for(4032.0, 2268.0, 892.0, 501.0, 0.38, false);
        let (fit_w, fit_h) = display_size_for(4032.0, 2268.0, 892.0, 501.0, 1.0);
        assert_eq!((ew, eh), (fit_w, fit_h));
        assert!((fw - fit_w as f64).abs() < 2.0);
        assert!((fh - fit_h as f64).abs() < 2.0);
        // Videos really do shrink.
        let (vw, _) = effective_display_size_for(4032.0, 2268.0, 892.0, 501.0, 0.38, true);
        assert_eq!(vw, dw);
    }

    #[test]
    fn test_anchor_fit_to_zoom_keeps_cursor() {
        // Fit 899x600 in a 900x600 viewport, click at (749,467), zoom 2.5x.
        let (old_w, new_w) = (899.0, 2247.0);
        let (target, max) = anchor_target(0.0, 900.0, old_w, new_w, 749.0);
        assert!((target - 1123.0).abs() < 2.0);
        assert!((max - 1347.0).abs() < 2.0);
        // Same fraction of the content stays under the cursor.
        let before = (0.0 + 749.0) / old_w;
        let after = (target + 749.0) / new_w;
        assert!((before - after).abs() < 0.001);
    }

    #[test]
    fn test_anchor_from_shrunk_image_uses_fit() {
        // Image zoomed out to 0.38 still displays at fit (~891); zooming to
        // 2.5 must behave like fit → 2.5, not 337 → 2227 (which clamps to max).
        let (fit_w, new_w) = (891.0, 2227.0);
        let (target, max) = anchor_target(0.0, 892.0, fit_w, new_w, 781.0);
        assert!(target <= max);
        assert!((target - 1171.0).abs() < 3.0);
        assert!((max - 1335.0).abs() < 3.0);
        // The old buggy ratio (337 → 2227) would land far past max.
        let buggy = (0.0 + 781.0) * (2227.0 / 337.0) - 781.0;
        assert!(buggy > max + 1000.0);
    }

    #[test]
    fn test_anchor_padding_falls_back_to_center() {
        // 300px content centered in a 900px viewport: padding click zooms centered.
        let (target, max) = anchor_target(0.0, 900.0, 300.0, 750.0, 50.0);
        let (center_target, _) = anchor_target(0.0, 900.0, 300.0, 750.0, 450.0);
        assert!((target - center_target).abs() < 0.001);
        assert_eq!(max, 0.0);
    }

    #[test]
    fn test_anchor_large_to_large_tracks() {
        // Already zoomed (1246px in a 900px viewport at scroll 0): cursor math holds.
        let (target, _) = anchor_target(0.0, 900.0, 1246.0, 2247.0, 823.0);
        assert!((target - 661.0).abs() < 3.0);
    }
}
