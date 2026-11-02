//! Rotation shown on screen only, for files Horama never rewrites: RAW and the other formats
//! without a lossless rotation, and animated PNG/WebP.
//!
//! The quarter turns are kept per file while the program runs; the image, its thumbnail, the
//! comparison and a saved copy show them, the file stays as it is.

use super::i18n::trf;
use super::{HoramaApp, browse};
use crate::config::path_key;
use crate::image_loader::DecodedImage;
use std::sync::Arc;

/// `d` turned by `turns` quarter turns clockwise (the same image for 0).
pub(super) fn turned(d: Arc<DecodedImage>, turns: u8) -> Arc<DecodedImage> {
    match turns % 4 {
        1 => Arc::new(d.rotate_90_cw()),
        2 => Arc::new(d.rotate_180()),
        3 => Arc::new(d.rotate_90_ccw()),
        _ => d,
    }
}

impl HoramaApp {
    /// Quarter turns clockwise shown for `path`.
    pub(super) fn view_turns_of(&self, path: &str) -> u8 {
        self.view_turns.get(&path_key(path)).copied().unwrap_or(0)
    }

    /// Turns `paths` on screen only and shows them again.
    pub(super) fn rotate_on_screen(&mut self, paths: &[String], clockwise: bool) {
        for p in paths {
            let key = path_key(p);
            let t = (self.view_turns.get(&key).copied().unwrap_or(0)
                + if clockwise { 1 } else { 3 })
                % 4;
            if t == 0 {
                self.view_turns.remove(&key);
            } else {
                self.view_turns.insert(key, t);
            }
        }
        self.reload_current();
        let names: Vec<String> = paths.iter().map(|p| browse::file_name_of(p)).collect();
        self.notify(
            trf(
                "Rotation shown only: {} (this format is never modified).",
                &[&names.join(", ")],
            ),
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            path: "x".into(),
            width: w,
            height: h,
            rgb_pixels: (0..w * h * 3).map(|v| v as u8).collect(),
            embedded_icc: None,
            embedded_icc_name: None,
            ignored_icc_note: None,
            format_name: String::new(),
            bit_depth: 8,
            is_raw: true,
            file_size: 0,
        })
    }

    #[test]
    fn quarter_turns_swap_the_sides_and_four_turns_change_nothing() {
        let d = img(4, 2);
        let one = turned(d.clone(), 1);
        assert_eq!((one.width, one.height), (2, 4));
        assert_eq!(turned(d.clone(), 2).rgb_pixels.len(), d.rgb_pixels.len());
        let back = turned(turned(turned(one, 1), 1), 1);
        assert_eq!(back.rgb_pixels, d.rgb_pixels);
        assert!(Arc::ptr_eq(&turned(d.clone(), 4), &d));
    }
}
