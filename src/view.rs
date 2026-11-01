//! Geometry of the image view: zoom limits and steps, fit-to-window, zoom around the cursor,
//! and conversion from view coordinates to image pixels.
//!
//! Units: `zoom` is physical screen pixels per image pixel (100% = one image pixel per
//! screen pixel, whatever the Windows scaling); view sizes, cursor positions and pan are in
//! logical (UI) pixels; `scale` is physical pixels per logical pixel. No UI toolkit here.

/// Smallest and largest zoom (2% and 3200%).
pub const MIN_ZOOM: f32 = 0.02;
pub const MAX_ZOOM: f32 = 32.0;
/// Factor of the zoom in / zoom out commands.
pub const ZOOM_STEP: f32 = 1.25;
/// Factor of one mouse-wheel notch.
pub const WHEEL_STEP: f32 = 1.18;
/// Empty border left around the image by "fit to window" (logical pixels).
const FIT_MARGIN: f32 = 24.0;

pub fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// Zoom that shows the whole `img_w`×`img_h` image in a `view_w`×`view_h` view.
pub fn fit_zoom(view_w: f32, view_h: f32, img_w: u32, img_h: u32, scale: f32) -> f32 {
    let zx = (view_w - FIT_MARGIN).max(64.0) / img_w.max(1) as f32;
    let zy = (view_h - FIT_MARGIN).max(64.0) / img_h.max(1) as f32;
    clamp_zoom(zx.min(zy) * scale.max(0.25))
}

/// Image pixels per logical pixel for a given zoom.
pub fn logical_zoom(zoom: f32, scale: f32) -> f32 {
    zoom / scale.max(0.25)
}

/// New zoom and pan after zooming by `factor` with the cursor at (`mx`, `my`) in a
/// `vw`×`vh` view: the image point under the cursor stays under the cursor.
pub fn zoom_at_cursor(
    zoom: f32,
    pan: (f32, f32),
    factor: f32,
    cursor: (f32, f32),
    view: (f32, f32),
) -> (f32, (f32, f32)) {
    let new_zoom = clamp_zoom(zoom * factor);
    let ratio = new_zoom / zoom;
    let (cx, cy) = (cursor.0 - view.0 * 0.5, cursor.1 - view.1 * 0.5);
    let pan = (cx - (cx - pan.0) * ratio, cy - (cy - pan.1) * ratio);
    (new_zoom, pan)
}

/// Image pixel under the view point (`mx`, `my`), if the point is on the image. The image is
/// centred in the `vw`×`vh` view and moved by `pan`; `lz` is the [`logical_zoom`].
pub fn view_to_image(
    point: (f32, f32),
    view: (f32, f32),
    img: (u32, u32),
    lz: f32,
    pan: (f32, f32),
) -> Option<(u32, u32)> {
    let (w, h) = (img.0 as f32, img.1 as f32);
    let x0 = (view.0 - w * lz) * 0.5 + pan.0;
    let y0 = (view.1 - h * lz) * 0.5 + pan.1;
    let (rx, ry) = ((point.0 - x0) / lz, (point.1 - y0) / lz);
    if !(rx >= 0.0 && ry >= 0.0 && rx < w && ry < h) {
        return None; // also rejects NaN
    }
    Some((
        (rx.floor() as u32).min(img.0.saturating_sub(1)),
        (ry.floor() as u32).min(img.1.saturating_sub(1)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_leaves_a_margin_and_respects_display_scale() {
        // 6000×4000 in a 1224×824 view: the width limits, 1200 px wide image.
        let z = fit_zoom(1224.0, 824.0, 6000, 4000, 1.0);
        assert!((z - 0.2).abs() < 1e-6);
        // With 150% Windows scaling the same view holds 1.5× more physical pixels.
        assert!((fit_zoom(1224.0, 824.0, 6000, 4000, 1.5) - 0.3).abs() < 1e-6);
        // Tiny image: limited by MAX_ZOOM.
        assert_eq!(fit_zoom(1000.0, 1000.0, 1, 1, 1.0), MAX_ZOOM);
        assert_eq!(clamp_zoom(f32::NAN), 1.0);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let view = (800.0, 600.0);
        let img = (4000, 3000);
        let (zoom, pan, scale) = (0.25f32, (30.0f32, -20.0f32), 1.0);
        let cursor = (610.0, 140.0);
        let before = view_to_image(cursor, view, img, logical_zoom(zoom, scale), pan).unwrap();
        let (z2, pan2) = zoom_at_cursor(zoom, pan, 2.0, cursor, view);
        assert_eq!(z2, 0.5);
        let after = view_to_image(cursor, view, img, logical_zoom(z2, scale), pan2).unwrap();
        assert!(before.0.abs_diff(after.0) <= 1 && before.1.abs_diff(after.1) <= 1);
        // At the limit the zoom stops and the pan does not drift.
        let (z3, pan3) = zoom_at_cursor(MAX_ZOOM, pan, 2.0, cursor, view);
        assert_eq!((z3, pan3), (MAX_ZOOM, pan));
    }

    #[test]
    fn points_outside_the_image_have_no_pixel() {
        let view = (100.0, 100.0);
        // 10×10 image at 1:1, centred: occupies 45..55.
        assert_eq!(
            view_to_image((45.0, 45.0), view, (10, 10), 1.0, (0.0, 0.0)),
            Some((0, 0))
        );
        assert_eq!(
            view_to_image((54.9, 54.9), view, (10, 10), 1.0, (0.0, 0.0)),
            Some((9, 9))
        );
        assert_eq!(
            view_to_image((55.0, 50.0), view, (10, 10), 1.0, (0.0, 0.0)),
            None
        );
        assert_eq!(
            view_to_image((44.9, 50.0), view, (10, 10), 1.0, (0.0, 0.0)),
            None
        );
        assert_eq!(
            view_to_image((f32::NAN, 50.0), view, (10, 10), 1.0, (0.0, 0.0)),
            None
        );
    }
}
