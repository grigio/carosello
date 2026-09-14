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
            imp.zoom.set(zoom.clamp(0.05, 50.0));
            self.invalidate_size();
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
    original_pixbuf: &Option<gdk_pixbuf::Pixbuf>,
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
    original_pixbuf
        .as_ref()
        .map(|pb| (pb.width().max(1) as f64, pb.height().max(1) as f64))
}

/// Fit size preserving aspect ratio, never upscaling small media.
pub fn fit_size(iw: f64, ih: f64, vw: f64, vh: f64) -> (f64, f64) {
    if iw < 1.0 || ih < 1.0 || vw < 1.0 || vh < 1.0 {
        return (iw.max(1.0), ih.max(1.0));
    }
    let scale = (vw / iw).min(vh / ih).min(1.0);
    ((iw * scale).max(1.0), (ih * scale).max(1.0))
}

pub fn display_size_for(iw: f64, ih: f64, vw: f64, vh: f64, zoom: f64) -> (i32, i32) {
    let (fw, fh) = fit_size(iw, ih, vw, vh);
    let dw = ((fw * zoom).round()).clamp(1.0, MAX_DIM) as i32;
    let dh = ((fh * zoom).round()).clamp(1.0, MAX_DIM) as i32;
    (dw.max(1), dh.max(1))
}
