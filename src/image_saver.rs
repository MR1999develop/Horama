//! Save and export images in JPEG, TIFF (8/16-bit), PNG, WebP, BMP, and DNG with embedded ICC profiles.

use image::{ColorType, ImageEncoder, ImageFormat};
use img_parts::{Bytes, ImageICC};
use std::fs::{self, File};
use std::io::{BufWriter, Cursor};
use std::path::{Path, PathBuf};
use tiff::encoder::{colortype, TiffEncoder};
use tiff::tags::Tag;

pub fn save_image_with_icc(
    dest_path: &Path,
    width: u32,
    height: u32,
    rgb_pixels: &[u8],
    icc_bytes: Option<&[u8]>,
    save_16bit_tiff: bool,
) -> Result<(), String> {
    let ext = dest_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("jpg")
        .to_lowercase();

    match ext.as_str() {
        "jpg" | "jpeg" | "jpe" => {
            let mut raw_jpeg = Vec::new();
            {
                let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut raw_jpeg, 96);
                encoder
                    .write_image(rgb_pixels, width, height, ColorType::Rgb8.into())
                    .map_err(|e| format!("JPEG encode error: {}", e))?;
            }
            if let Some(icc) = icc_bytes {
                if let Ok(mut jpeg) = img_parts::jpeg::Jpeg::from_bytes(Bytes::from(raw_jpeg.clone())) {
                    jpeg.set_icc_profile(Some(Bytes::copy_from_slice(icc)));
                    let out_file = File::create(dest_path)
                        .map_err(|e| format!("File create error: {}", e))?;
                    jpeg.encoder()
                        .write_to(BufWriter::new(out_file))
                        .map_err(|e| format!("JPEG ICC write error: {}", e))?;
                    return Ok(());
                }
            }
            fs::write(dest_path, raw_jpeg).map_err(|e| format!("Write error: {}", e))
        }
        "png" => {
            let mut raw_png = Vec::new();
            {
                let encoder = image::codecs::png::PngEncoder::new(&mut raw_png);
                encoder
                    .write_image(rgb_pixels, width, height, ColorType::Rgb8.into())
                    .map_err(|e| format!("PNG encode error: {}", e))?;
            }
            if let Some(icc) = icc_bytes {
                if let Ok(mut png) = img_parts::png::Png::from_bytes(Bytes::from(raw_png.clone())) {
                    png.set_icc_profile(Some(Bytes::copy_from_slice(icc)));
                    let out_file = File::create(dest_path)
                        .map_err(|e| format!("File create error: {}", e))?;
                    png.encoder()
                        .write_to(BufWriter::new(out_file))
                        .map_err(|e| format!("PNG ICC write error: {}", e))?;
                    return Ok(());
                }
            }
            fs::write(dest_path, raw_png).map_err(|e| format!("Write error: {}", e))
        }
        "webp" => {
            let mut raw_webp = Vec::new();
            {
                let mut cursor = Cursor::new(&mut raw_webp);
                image::write_buffer_with_format(
                    &mut cursor,
                    rgb_pixels,
                    width,
                    height,
                    ColorType::Rgb8,
                    ImageFormat::WebP,
                )
                .map_err(|e| format!("WebP encode error: {}", e))?;
            }
            if let Some(icc) = icc_bytes {
                if let Ok(mut webp) =
                    img_parts::webp::WebP::from_bytes(Bytes::from(raw_webp.clone()))
                {
                    webp.set_icc_profile(Some(Bytes::copy_from_slice(icc)));
                    let out_file = File::create(dest_path)
                        .map_err(|e| format!("File create error: {}", e))?;
                    webp.encoder()
                        .write_to(BufWriter::new(out_file))
                        .map_err(|e| format!("WebP ICC write error: {}", e))?;
                    return Ok(());
                }
            }
            fs::write(dest_path, raw_webp).map_err(|e| format!("Write error: {}", e))
        }
        "tif" | "tiff" | "dng" => {
            let out_file =
                File::create(dest_path).map_err(|e| format!("File create error: {}", e))?;
            let mut writer = BufWriter::new(out_file);
            let mut tiff_enc = TiffEncoder::new(&mut writer)
                .map_err(|e| format!("TIFF encoder init error: {}", e))?;

            if save_16bit_tiff {
                let mut img_16: Vec<u16> = Vec::with_capacity(rgb_pixels.len());
                for &b in rgb_pixels {
                    img_16.push((b as u16) * 257);
                }
                let mut img_dir = tiff_enc
                    .new_image::<colortype::RGB16>(width, height)
                    .map_err(|e| format!("TIFF 16-bit dir error: {}", e))?;
                if let Some(icc) = icc_bytes {
                    img_dir
                        .encoder()
                        .write_tag(Tag::Unknown(34675), icc)
                        .map_err(|e| format!("TIFF ICC tag error: {}", e))?;
                }
                if ext == "dng" {
                    img_dir
                        .encoder()
                        .write_tag(Tag::Unknown(50706), &[1u8, 4, 0, 0][..])
                        .ok();
                }
                img_dir
                    .write_data(&img_16)
                    .map_err(|e| format!("TIFF 16-bit write error: {}", e))?;
            } else {
                let mut img_dir = tiff_enc
                    .new_image::<colortype::RGB8>(width, height)
                    .map_err(|e| format!("TIFF 8-bit dir error: {}", e))?;
                if let Some(icc) = icc_bytes {
                    img_dir
                        .encoder()
                        .write_tag(Tag::Unknown(34675), icc)
                        .map_err(|e| format!("TIFF ICC tag error: {}", e))?;
                }
                if ext == "dng" {
                    img_dir
                        .encoder()
                        .write_tag(Tag::Unknown(50706), &[1u8, 4, 0, 0][..])
                        .ok();
                }
                img_dir
                    .write_data(rgb_pixels)
                    .map_err(|e| format!("TIFF 8-bit write error: {}", e))?;
            }
            Ok(())
        }
        "bmp" => image::save_buffer_with_format(
            dest_path,
            rgb_pixels,
            width,
            height,
            ColorType::Rgb8,
            ImageFormat::Bmp,
        )
        .map_err(|e| format!("BMP save error: {}", e)),
        _ => image::save_buffer(dest_path, rgb_pixels, width, height, ColorType::Rgb8)
            .map_err(|e| format!("Image save error: {}", e)),
    }
}

/// Generates the next available sequential filename in the same directory as `original_path`.
/// E.g. `foto.jpg` -> `foto_1.jpg` (or `foto_2.jpg` if `foto_1.jpg` already exists, etc.)
pub fn generate_next_sequential_path(original_path: &Path) -> PathBuf {
    let parent = original_path.parent().unwrap_or_else(|| Path::new(""));
    let stem = original_path.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let ext = original_path.extension().and_then(|s| s.to_str()).unwrap_or("jpg");

    // Check if stem already ends with an underscore and digits, e.g. "photo_1"
    let (base_stem, start_num) = if let Some(pos) = stem.rfind('_') {
        let suffix = &stem[pos + 1..];
        if let Ok(n) = suffix.parse::<usize>() {
            (&stem[..pos], n + 1)
        } else {
            (stem, 1)
        }
    } else {
        (stem, 1)
    };

    let mut index = start_num;
    loop {
        let candidate_name = format!("{}_{}.{}", base_stem, index, ext);
        let candidate_path = parent.join(&candidate_name);
        if !candidate_path.exists() {
            return candidate_path;
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_next_sequential_path() {
        let p = Path::new("C:/nonexistent_folder_abc123/my_photo.jpg");
        let seq1 = generate_next_sequential_path(p);
        assert_eq!(seq1, Path::new("C:/nonexistent_folder_abc123/my_photo_1.jpg"));

        let p_existing_seq = Path::new("C:/nonexistent_folder_abc123/my_photo_3.jpg");
        let seq2 = generate_next_sequential_path(p_existing_seq);
        assert_eq!(seq2, Path::new("C:/nonexistent_folder_abc123/my_photo_4.jpg"));
    }
}
