//! Image and Camera RAW decoding with ICC profile extraction and EXIF orientation support.

use crate::color_manager::extract_icc_description;
use crate::config::{is_raw_file, THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH};
use crate::cr3_parser::extract_largest_embedded_jpeg;
use image::{DynamicImage, ImageReader};
use img_parts::ImageICC;
use std::fs::{self, File};
use std::io::{BufReader, Cursor};
use std::path::Path;

/// Fast EXIF thumbnail extractor (works for JPEG, TIFF, and RAW files with standard EXIF APP1/IFD1 thumbnail).
pub fn extract_exif_thumbnail(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif_data = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let off_field = exif_data.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)?;
    let len_field = exif_data.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)?;
    let offset = off_field.value.get_uint(0)? as usize;
    let len = len_field.value.get_uint(0)? as usize;
    let tiff_buf = exif_data.buf();
    if offset + len <= tiff_buf.len() && len > 0 {
        let candidate = &tiff_buf[offset..offset + len];
        if candidate.starts_with(&[0xFF, 0xD8]) {
            return Some(candidate.to_vec());
        }
    }
    None
}

#[derive(Clone)]
pub struct DecodedImage {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub rgb_pixels: Vec<u8>,
    pub embedded_icc: Option<Vec<u8>>,
    pub embedded_icc_name: Option<String>,
    pub format_name: String,
    pub bit_depth: u8,
    pub is_raw: bool,
    pub file_size: u64,
}

impl DecodedImage {
    pub fn memory_bytes(&self) -> usize {
        self.rgb_pixels.len()
            + self.embedded_icc.as_ref().map_or(0, |v| v.len())
            + std::mem::size_of::<Self>()
    }

    /// Rotate 90 degrees clockwise (lossless RGB buffer transformation).
    pub fn rotate_90_cw(&self) -> DecodedImage {
        let w = self.width as usize;
        let h = self.height as usize;
        let new_w = h;
        let new_h = w;
        let mut new_pixels = vec![0u8; new_w * new_h * 3];

        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 3;
                let new_x = h - 1 - y;
                let new_y = x;
                let dst_idx = (new_y * new_w + new_x) * 3;
                new_pixels[dst_idx..dst_idx + 3]
                    .copy_from_slice(&self.rgb_pixels[src_idx..src_idx + 3]);
            }
        }

        let mut rotated = self.clone();
        rotated.width = new_w as u32;
        rotated.height = new_h as u32;
        rotated.rgb_pixels = new_pixels;
        rotated
    }

    /// Rotate 90 degrees counter-clockwise (lossless RGB buffer transformation).
    pub fn rotate_90_ccw(&self) -> DecodedImage {
        let w = self.width as usize;
        let h = self.height as usize;
        let new_w = h;
        let new_h = w;
        let mut new_pixels = vec![0u8; new_w * new_h * 3];

        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 3;
                let new_x = y;
                let new_y = w - 1 - x;
                let dst_idx = (new_y * new_w + new_x) * 3;
                new_pixels[dst_idx..dst_idx + 3]
                    .copy_from_slice(&self.rgb_pixels[src_idx..src_idx + 3]);
            }
        }

        let mut rotated = self.clone();
        rotated.width = new_w as u32;
        rotated.height = new_h as u32;
        rotated.rgb_pixels = new_pixels;
        rotated
    }

    /// Rotate 180 degrees.
    #[allow(dead_code)]
    pub fn rotate_180(&self) -> DecodedImage {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut new_pixels = vec![0u8; w * h * 3];

        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 3;
                let new_x = w - 1 - x;
                let new_y = h - 1 - y;
                let dst_idx = (new_y * w + new_x) * 3;
                new_pixels[dst_idx..dst_idx + 3]
                    .copy_from_slice(&self.rgb_pixels[src_idx..src_idx + 3]);
            }
        }

        let mut rotated = self.clone();
        rotated.rgb_pixels = new_pixels;
        rotated
    }

    /// Flip horizontally.
    #[allow(dead_code)]
    pub fn flip_horizontal(&self) -> DecodedImage {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut new_pixels = vec![0u8; w * h * 3];

        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 3;
                let new_x = w - 1 - x;
                let new_y = y;
                let dst_idx = (new_y * w + new_x) * 3;
                new_pixels[dst_idx..dst_idx + 3]
                    .copy_from_slice(&self.rgb_pixels[src_idx..src_idx + 3]);
            }
        }

        let mut rotated = self.clone();
        rotated.rgb_pixels = new_pixels;
        rotated
    }

    /// Crop image to specified rectangle (crop_x, crop_y, crop_w, crop_h).
    pub fn crop(&self, mut crop_x: u32, mut crop_y: u32, mut crop_w: u32, mut crop_h: u32) -> DecodedImage {
        let w = self.width;
        let h = self.height;
        if crop_x >= w { crop_x = 0; }
        if crop_y >= h { crop_y = 0; }
        if crop_x + crop_w > w { crop_w = w.saturating_sub(crop_x); }
        if crop_y + crop_h > h { crop_h = h.saturating_sub(crop_y); }
        if crop_w == 0 { crop_w = 1; }
        if crop_h == 0 { crop_h = 1; }

        let mut new_pixels = Vec::with_capacity((crop_w * crop_h * 3) as usize);
        let src_w = w as usize;
        for y in crop_y..(crop_y + crop_h) {
            let row_start = (y as usize * src_w + crop_x as usize) * 3;
            let row_end = row_start + (crop_w as usize * 3);
            new_pixels.extend_from_slice(&self.rgb_pixels[row_start..row_end]);
        }

        let mut cropped = self.clone();
        cropped.width = crop_w;
        cropped.height = crop_h;
        cropped.rgb_pixels = new_pixels;
        cropped
    }
}

/// Extract embedded ICC profile bytes from JPEG, PNG, WebP, or TIFF files.
pub fn extract_icc_from_bytes(bytes: &[u8], ext: &str) -> Option<Vec<u8>> {
    let ext_low = ext.to_lowercase();
    if matches!(ext_low.as_str(), "jpg" | "jpeg" | "jpe" | "jfif") {
        if let Ok(jpeg) = img_parts::jpeg::Jpeg::from_bytes(bytes.to_vec().into()) {
            if let Some(icc) = jpeg.icc_profile() {
                if !icc.is_empty() {
                    return Some(icc.to_vec());
                }
            }
        }
    } else if ext_low == "png" {
        if let Ok(png) = img_parts::png::Png::from_bytes(bytes.to_vec().into()) {
            if let Some(icc) = png.icc_profile() {
                if !icc.is_empty() {
                    return Some(icc.to_vec());
                }
            }
        }
    } else if ext_low == "webp" {
        if let Ok(webp) = img_parts::webp::WebP::from_bytes(bytes.to_vec().into()) {
            if let Some(icc) = webp.icc_profile() {
                if !icc.is_empty() {
                    return Some(icc.to_vec());
                }
            }
        }
    } else if matches!(ext_low.as_str(), "tif" | "tiff") {
        if let Ok(mut dec) = tiff::decoder::Decoder::new(Cursor::new(bytes)) {
            // Tag 34675 (0x8773) is InterColorProfile (ICC Profile)
            if let Ok( Some(tiff::decoder::ifd::Value::List(list)) ) =
                dec.find_tag(tiff::tags::Tag::Unknown(34675))
            {
                let mut icc = Vec::with_capacity(list.len());
                for v in list {
                    if let tiff::decoder::ifd::Value::Byte(b) = v {
                        icc.push(b);
                    } else if let tiff::decoder::ifd::Value::Unsigned(b) = v {
                        icc.push(b as u8);
                    }
                }
                if icc.len() >= 128 {
                    return Some(icc);
                }
            }
            if let Ok(vec_u8) = dec.get_tag_u8_vec(tiff::tags::Tag::Unknown(34675)) {
                if vec_u8.len() >= 128 {
                    return Some(vec_u8);
                }
            }
        }
    }
    None
}

/// Read EXIF orientation (1..=8) from a file or byte slice.
fn read_exif_orientation_from_bytes(bytes: &[u8]) -> u32 {
    let mut cursor = Cursor::new(bytes);
    if let Ok(exif_data) = exif::Reader::new().read_from_container(&mut cursor) {
        if let Some(field) = exif_data.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
            if let Some(v) = field.value.get_uint(0) {
                return v;
            }
        }
    }
    1
}

fn read_exif_orientation_from_file(path: &Path) -> u32 {
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        if let Ok(exif_data) = exif::Reader::new().read_from_container(&mut reader) {
            if let Some(field) = exif_data.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
                if let Some(v) = field.value.get_uint(0) {
                    return v;
                }
            }
        }
    }
    1
}

fn apply_orientation(img: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Decode a Camera RAW file using embedded full-size preview or pure-Rust rawloader + imagepipe.
fn decode_raw_image(path: &Path, file_size: u64) -> Result<DecodedImage, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("RAW")
        .to_uppercase();

    let file_orientation = read_exif_orientation_from_file(path);

    // 1. Try fast embedded full-size JPEG preview (works for Canon CR3/CR2, Nikon NEF, Sony ARW, Fuji RAF, etc.)
    if let Some(jpeg_bytes) = extract_largest_embedded_jpeg(path) {
        if let Ok(reader) = ImageReader::new(Cursor::new(&jpeg_bytes)).with_guessed_format() {
            if let Ok(dyn_img) = reader.decode() {
                let jpeg_orient = read_exif_orientation_from_bytes(&jpeg_bytes);
                let orient = if jpeg_orient != 1 {
                    jpeg_orient
                } else {
                    file_orientation
                };
                let oriented = apply_orientation(dyn_img, orient);
                let embedded_icc = extract_icc_from_bytes(&jpeg_bytes, "jpg");
                let embedded_icc_name = embedded_icc
                    .as_deref()
                    .and_then(extract_icc_description);
                let rgb = oriented.to_rgb8();
                let (width, height) = rgb.dimensions();
                return Ok(DecodedImage {
                    path: path.to_string_lossy().to_string(),
                    width,
                    height,
                    rgb_pixels: rgb.into_raw(),
                    embedded_icc,
                    embedded_icc_name,
                    format_name: format!("{} (RAW)", ext),
                    bit_depth: 16,
                    is_raw: true,
                    file_size,
                });
            }
        }
    }

    // 2. Full sensor demosaicing via rawloader + imagepipe
    let raw_image = rawloader::decode_file(path)
        .map_err(|e| format!("Failed to decode RAW file '{}': {}", path.display(), e))?;
    let mut pipeline =
        imagepipe::Pipeline::new_from_source(imagepipe::ImageSource::Raw(raw_image))
            .map_err(|e| format!("RAW pipeline error: {}", e))?;
    let decoded = pipeline
        .output_8bit(None)
        .map_err(|e| format!("RAW demosaic error: {}", e))?;

    let width = decoded.width as u32;
    let height = decoded.height as u32;
    let rgb_buf = image::RgbImage::from_raw(width, height, decoded.data)
        .ok_or_else(|| "Invalid RAW output buffer dimensions".to_string())?;
    let oriented = apply_orientation(DynamicImage::ImageRgb8(rgb_buf), file_orientation);
    let final_rgb = oriented.to_rgb8();
    let (w, h) = final_rgb.dimensions();

    Ok(DecodedImage {
        path: path.to_string_lossy().to_string(),
        width: w,
        height: h,
        rgb_pixels: final_rgb.into_raw(),
        embedded_icc: None,
        embedded_icc_name: None,
        format_name: format!("{} (RAW Demosaic)", ext),
        bit_depth: 16,
        is_raw: true,
        file_size,
    })
}

/// Load and decode any supported standard or RAW image file.
pub fn load_image(path: &Path) -> Result<DecodedImage, String> {
    let file_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    if is_raw_file(path) {
        return decode_raw_image(path, file_size);
    }

    let bytes = fs::read(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();

    let embedded_icc = extract_icc_from_bytes(&bytes, &ext);
    let embedded_icc_name = embedded_icc.as_deref().and_then(extract_icc_description);
    let orientation = read_exif_orientation_from_bytes(&bytes);

    let reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| format!("Format guess error: {}", e))?;
    let fmt_str = reader
        .format()
        .map(|f| format!("{:?}", f).to_uppercase())
        .unwrap_or_else(|| ext.to_uppercase());

    let dyn_img = reader
        .decode()
        .map_err(|e| format!("Decode error on {}: {}", path.display(), e))?;

    let bit_depth = match &dyn_img {
        DynamicImage::ImageLuma16(_)
        | DynamicImage::ImageLumaA16(_)
        | DynamicImage::ImageRgb16(_)
        | DynamicImage::ImageRgba16(_) => 16,
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => 32,
        _ => 8,
    };

    let oriented = apply_orientation(dyn_img, orientation);
    let rgb = oriented.to_rgb8();
    let (width, height) = rgb.dimensions();

    Ok(DecodedImage {
        path: path.to_string_lossy().to_string(),
        width,
        height,
        rgb_pixels: rgb.into_raw(),
        embedded_icc,
        embedded_icc_name,
        format_name: fmt_str,
        bit_depth,
        is_raw: false,
        file_size,
    })
}

/// Ultra-fast thumbnail loader returning (width, height, rgba8_pixels) for the bottom filmstrip.
/// Uses embedded EXIF IFD1 thumbnails where available (sub-millisecond decode),
/// embedded RAW JPEG previews, or falls back to full image decode.
pub fn load_thumbnail_rgba(
    path: &Path,
    cm: Option<&crate::color_manager::ColorManager>,
) -> Option<(u32, u32, Vec<u8>)> {
    // 1. Instant path: Try embedded EXIF thumbnail (JPEG/TIFF/RAW with standard EXIF)
    if let Some(thumb_bytes) = extract_exif_thumbnail(path) {
        if let Ok(reader) = ImageReader::new(Cursor::new(&thumb_bytes)).with_guessed_format() {
            if let Ok(img) = reader.decode() {
                let orient = read_exif_orientation_from_bytes(&thumb_bytes);
                let emb_icc = extract_icc_from_bytes(&thumb_bytes, "jpg");
                let oriented = apply_orientation(img, orient);
                let thumb = oriented.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
                let rgb = thumb.to_rgb8();
                let (w, h) = rgb.dimensions();

                let final_rgb = if let Some(mgr) = cm {
                    let prof = if emb_icc.is_some() {
                        crate::config::PROFILE_EMBEDDED
                    } else {
                        crate::config::DEFAULT_FALLBACK_PROFILE
                    };
                    let (managed, _) =
                        mgr.apply_profile_with_info(rgb.as_raw(), prof, emb_icc.as_deref());
                    managed
                } else {
                    rgb.into_raw()
                };

                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for px in final_rgb.chunks_exact(3) {
                    rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
                return Some((w, h, rgba));
            }
        }
    }

    // 2. RAW embedded preview path
    if is_raw_file(path) {
        if let Some(jpeg_bytes) = extract_largest_embedded_jpeg(path) {
            if let Ok(reader) = ImageReader::new(Cursor::new(&jpeg_bytes)).with_guessed_format() {
                if let Ok(img) = reader.decode() {
                    let orient = read_exif_orientation_from_bytes(&jpeg_bytes);
                    let emb_icc = extract_icc_from_bytes(&jpeg_bytes, "jpg");
                    let oriented = apply_orientation(img, orient);
                    let thumb = oriented.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
                    let rgb = thumb.to_rgb8();
                    let (w, h) = rgb.dimensions();

                    let final_rgb = if let Some(mgr) = cm {
                        let prof = if emb_icc.is_some() {
                            crate::config::PROFILE_EMBEDDED
                        } else {
                            crate::config::DEFAULT_FALLBACK_PROFILE
                        };
                        let (managed, _) =
                            mgr.apply_profile_with_info(rgb.as_raw(), prof, emb_icc.as_deref());
                        managed
                    } else {
                        rgb.into_raw()
                    };

                    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                    for px in final_rgb.chunks_exact(3) {
                        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                    }
                    return Some((w, h, rgba));
                }
            }
        }
    }

    // 3. Fallback: Full decode (for PNG, WebP, BMP, or images without embedded thumbnails)
    let decoded = load_image(path).ok()?;
    let emb_icc = decoded.embedded_icc;
    let rgb_img =
        image::RgbImage::from_raw(decoded.width, decoded.height, decoded.rgb_pixels)?;
    let dyn_img = DynamicImage::ImageRgb8(rgb_img);
    let thumb = dyn_img.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    let rgb = thumb.to_rgb8();
    let (w, h) = rgb.dimensions();

    let final_rgb = if let Some(mgr) = cm {
        let prof = if emb_icc.is_some() {
            crate::config::PROFILE_EMBEDDED
        } else {
            crate::config::DEFAULT_FALLBACK_PROFILE
        };
        let (managed, _) = mgr.apply_profile_with_info(rgb.as_raw(), prof, emb_icc.as_deref());
        managed
    } else {
        rgb.into_raw()
    };

    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in final_rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Some((w, h, rgba))
}

/// Creates a fast filmstrip thumbnail from an in-memory DecodedImage (used e.g. after rotation).
pub fn create_thumbnail_from_decoded(decoded: &DecodedImage) -> Option<(u32, u32, Vec<u8>)> {
    let rgb_img = image::RgbImage::from_raw(decoded.width, decoded.height, decoded.rgb_pixels.clone())?;
    let dyn_img = DynamicImage::ImageRgb8(rgb_img);
    let thumb = dyn_img.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    let (w, h) = (thumb.width(), thumb.height());
    let rgb = thumb.to_rgb8();
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in rgb.as_raw().chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Some((w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoded_image_rotations() {
        // Create a 2x3 test image with unique RGB values per pixel
        let img = DecodedImage {
            path: "test.png".to_string(),
            width: 2,
            height: 3,
            rgb_pixels: vec![
                10, 20, 30,  40, 50, 60,   // y=0: (0,0), (1,0)
                70, 80, 90,  11, 22, 33,   // y=1: (0,1), (1,1)
                44, 55, 66,  77, 88, 99,   // y=2: (0,2), (1,2)
            ],
            embedded_icc: None,
            embedded_icc_name: None,
            format_name: "PNG".to_string(),
            bit_depth: 8,
            is_raw: false,
            file_size: 100,
        };

        // 90° CW should produce 3x2 dimensions
        let cw = img.rotate_90_cw();
        assert_eq!(cw.width, 3);
        assert_eq!(cw.height, 2);

        // 4x 90° CW should restore original dimensions and pixel content
        let full_rot = img.rotate_90_cw().rotate_90_cw().rotate_90_cw().rotate_90_cw();
        assert_eq!(full_rot.width, img.width);
        assert_eq!(full_rot.height, img.height);
        assert_eq!(full_rot.rgb_pixels, img.rgb_pixels);

        // 90° CW then 90° CCW should cancel out
        let back = img.rotate_90_cw().rotate_90_ccw();
        assert_eq!(back.rgb_pixels, img.rgb_pixels);

        // 180° rotation twice should cancel out
        let rot180_twice = img.rotate_180().rotate_180();
        assert_eq!(rot180_twice.rgb_pixels, img.rgb_pixels);
    }

    #[test]
    fn test_decoded_image_crop() {
        let img = DecodedImage {
            path: "test_crop.png".to_string(),
            width: 3,
            height: 3,
            rgb_pixels: vec![
                1, 1, 1,   2, 2, 2,   3, 3, 3,
                4, 4, 4,   5, 5, 5,   6, 6, 6,
                7, 7, 7,   8, 8, 8,   9, 9, 9,
            ],
            embedded_icc: None,
            embedded_icc_name: None,
            format_name: "PNG".to_string(),
            bit_depth: 8,
            is_raw: false,
            file_size: 100,
        };

        // Crop center 1x1 (x=1, y=1, w=1, h=1)
        let cropped = img.crop(1, 1, 1, 1);
        assert_eq!(cropped.width, 1);
        assert_eq!(cropped.height, 1);
        assert_eq!(cropped.rgb_pixels, vec![5, 5, 5]);

        // Crop 2x2 top-left (x=0, y=0, w=2, h=2)
        let top_left = img.crop(0, 0, 2, 2);
        assert_eq!(top_left.width, 2);
        assert_eq!(top_left.height, 2);
        assert_eq!(top_left.rgb_pixels, vec![
            1, 1, 1,  2, 2, 2,
            4, 4, 4,  5, 5, 5,
        ]);
    }
}
