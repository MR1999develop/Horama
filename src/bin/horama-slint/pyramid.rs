//! Reduced copies of the displayed image, so zooming out never shows aliasing.
//!
//! The canvas lets Slint scale the image with a bilinear filter, which only looks at the four
//! nearest pixels: shrinking a 6000-pixel-wide photo to 600 pixels skips most of the image and
//! produces jagged edges and moiré. The pyramid holds copies halved again and again with a 2×2
//! box filter (every source pixel counts), and the canvas shows the smallest copy that is still
//! at least as large as the screen needs, so Slint never shrinks by more than 2×.
//!
//! Averaging is done in linear light (sRGB transfer curve): averaging gamma-encoded values
//! would darken fine light-on-dark detail. The pixels are already converted to the monitor
//! profile, whose curve is close to sRGB on common displays; the error of that approximation is
//! far below the error of not filtering at all.

use slint::{Rgb8Pixel, SharedPixelBuffer};
use std::sync::OnceLock;

/// No reduced level is made below this size (longest side, pixels).
const MIN_LEVEL_SIDE: u32 = 256;

/// Full-size image (level 0) and its successive halvings.
#[derive(Clone)]
pub struct Pyramid {
    levels: Vec<SharedPixelBuffer<Rgb8Pixel>>,
}

impl Pyramid {
    /// Builds the reduced levels of `full`. Call it on a worker thread: for a 24 MP image it
    /// takes a few tens of milliseconds.
    pub fn build(full: SharedPixelBuffer<Rgb8Pixel>) -> Self {
        let mut levels = vec![full];
        loop {
            let last = levels.last().expect("level 0 exists");
            if last.width().max(last.height()) / 2 < MIN_LEVEL_SIDE {
                break;
            }
            let next = halve(last);
            levels.push(next);
        }
        Pyramid { levels }
    }

    #[cfg(test)]
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    pub fn level(&self, index: usize) -> &SharedPixelBuffer<Rgb8Pixel> {
        &self.levels[index.min(self.levels.len() - 1)]
    }

    /// Level to show when one image pixel covers `screen_px_per_image_px` physical screen
    /// pixels: the smallest level that is still at least as large as needed.
    pub fn level_for_zoom(&self, screen_px_per_image_px: f32) -> usize {
        level_for_zoom(screen_px_per_image_px, self.levels.len())
    }
}

fn level_for_zoom(zoom: f32, level_count: usize) -> usize {
    if !(zoom.is_finite() && zoom > 0.0) || zoom >= 1.0 || level_count == 0 {
        return 0;
    }
    // Level k is 2^-k of the full size; it is large enough while 2^-k >= zoom.
    // The small tolerance keeps exactly 50%, 25%… on the matching level despite rounding.
    let k = (1.0 / zoom).log2() + 1e-4;
    (k.floor() as usize).min(level_count - 1)
}

/// sRGB 8-bit value → linear light, scaled to 0..=65535.
fn to_linear_lut() -> &'static [u16; 256] {
    static LUT: OnceLock<[u16; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u16; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let c = i as f64 / 255.0;
            let lin = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            *v = (lin * 65535.0).round() as u16;
        }
        t
    })
}

/// Linear light (0..=65535, indexed by value >> 4) → sRGB 8-bit value.
fn to_srgb_lut() -> &'static [u8; 4096] {
    static LUT: OnceLock<[u8; 4096]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 4096];
        for (i, v) in t.iter_mut().enumerate() {
            // Centre of the bucket, so the round trip of every 8-bit value is exact.
            let lin = (i as f64 + 0.5) / 4096.0;
            let c = if lin <= 0.003_130_8 {
                lin * 12.92
            } else {
                1.055 * lin.powf(1.0 / 2.4) - 0.055
            };
            *v = (c * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        t
    })
}

/// Half-size copy with a 2×2 box filter in linear light. Odd sizes round up; the last row or
/// column then averages the pixels that exist.
fn halve(src: &SharedPixelBuffer<Rgb8Pixel>) -> SharedPixelBuffer<Rgb8Pixel> {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (dw, dh) = (sw.div_ceil(2), sh.div_ceil(2));
    let lin = to_linear_lut();
    let enc = to_srgb_lut();
    let s = src.as_bytes();
    let mut dst = SharedPixelBuffer::<Rgb8Pixel>::new(dw as u32, dh as u32);
    let d = dst.make_mut_bytes();
    for y in 0..dh {
        let y0 = 2 * y;
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..dw {
            let x0 = 2 * x;
            let x1 = (x0 + 1).min(sw - 1);
            let o = (y * dw + x) * 3;
            for c in 0..3 {
                let sum = lin[s[(y0 * sw + x0) * 3 + c] as usize] as u32
                    + lin[s[(y0 * sw + x1) * 3 + c] as usize] as u32
                    + lin[s[(y1 * sw + x0) * 3 + c] as usize] as u32
                    + lin[s[(y1 * sw + x1) * 3 + c] as usize] as u32;
                d[o + c] = enc[((sum / 4) >> 4) as usize];
            }
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> SharedPixelBuffer<Rgb8Pixel> {
        let mut b = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
        let bytes = b.make_mut_bytes();
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 3) as usize;
                bytes[o..o + 3].copy_from_slice(&f(x, y));
            }
        }
        b
    }

    #[test]
    fn every_8bit_value_survives_the_linear_round_trip() {
        let (lin, enc) = (to_linear_lut(), to_srgb_lut());
        for v in 0..=255u8 {
            assert_eq!(enc[(lin[v as usize] >> 4) as usize], v, "value {}", v);
        }
    }

    #[test]
    fn flat_areas_keep_their_value_and_odd_sizes_round_up() {
        let h = halve(&image(5, 3, |_, _| [10, 128, 250]));
        assert_eq!((h.width(), h.height()), (3, 2));
        assert!(h.as_bytes().chunks(3).all(|p| p == [10, 128, 250]));
    }

    #[test]
    fn fine_detail_is_averaged_in_linear_light() {
        // One-pixel black and white stripes: the right half-size result is mid grey in light
        // (sRGB ≈ 188), not the gamma-encoded mean 128, and certainly not black or white.
        let h = halve(&image(
            4,
            4,
            |x, _| if x % 2 == 0 { [0; 3] } else { [255; 3] },
        ));
        for p in h.as_bytes().chunks(3) {
            assert!((186..=190).contains(&p[0]), "got {}", p[0]);
        }
    }

    #[test]
    fn levels_stop_near_the_minimum_side_and_zoom_picks_the_level() {
        let p = Pyramid::build(image(4000, 3000, |_, _| [0; 3]));
        // 4000 → 2000 → 1000 → 500 → 250 would be under 256: stop at 500.
        assert_eq!(p.level_count(), 4);
        assert_eq!(p.level(3).width(), 500);
        assert_eq!(p.level_for_zoom(2.0), 0);
        assert_eq!(p.level_for_zoom(1.0), 0);
        assert_eq!(p.level_for_zoom(0.6), 0);
        assert_eq!(p.level_for_zoom(0.5), 1);
        assert_eq!(p.level_for_zoom(0.3), 1);
        assert_eq!(p.level_for_zoom(0.25), 2);
        assert_eq!(p.level_for_zoom(0.01), 3);
        assert_eq!(p.level_for_zoom(f32::NAN), 0);
    }

    #[test]
    fn small_images_have_only_the_full_level() {
        let p = Pyramid::build(image(300, 200, |_, _| [0; 3]));
        assert_eq!(p.level_count(), 1);
        assert_eq!(p.level_for_zoom(0.1), 0);
    }
}
