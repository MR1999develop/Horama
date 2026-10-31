//! Save and export images in JPEG, TIFF, PNG, WebP and BMP with embedded ICC profiles and the
//! original EXIF metadata. Files are always written atomically (temp file + rename).

use crate::disk_ops::atomic_write;
use image::{ColorType, ImageEncoder};
use img_parts::{Bytes, ImageEXIF, ImageICC};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use tiff::encoder::{colortype, TiffEncoder};
use tiff::tags::Tag;

/// Formats the export dialog offers (all 8-bit; the viewer works on 8-bit display data).
pub const EXPORT_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "png", "tif", "tiff", "webp", "bmp"];

/// Builds an EXIF block (raw TIFF structure) for an exported image from the source file's
/// metadata: camera, lens, exposure, dates, GPS, copyright... Orientation is reset (the
/// exported pixels are already upright), pixel dimensions are updated, and maker notes,
/// thumbnails and layout tags of the original are dropped because they would be invalid.
pub fn build_export_exif(source_path: &Path, width: u32, height: u32) -> Option<Vec<u8>> {
    let exif_data = read_source_exif(source_path)?;

    // Tags that describe the original file layout or would be wrong after editing.
    const SKIP: &[u16] = &[
        0x00FE, 0x0100, 0x0101, 0x0102, 0x0103, 0x0106, 0x0111, 0x0112, 0x0115, 0x0116, 0x0117,
        0x011C, 0x0142, 0x0143, 0x0144, 0x0145, 0x014A, 0x015B, 0x0201, 0x0202, 0x02BC, 0x83BB,
        0x8773, 0x927C, 0xA002, 0xA003, 0xA005, 0x8769, 0x8825,
    ];
    let mut fields: Vec<exif::Field> = exif_data
        .fields()
        .filter(|f| f.ifd_num == exif::In::PRIMARY)
        .filter(|f| {
            let n = f.tag.number();
            let private_tiff = f.tag.context() == exif::Context::Tiff && n >= 0xC000;
            !SKIP.contains(&n) && !private_tiff
        })
        .filter(|f| !matches!(f.value, exif::Value::Unknown(..)))
        .cloned()
        .collect();
    fields.push(exif::Field {
        tag: exif::Tag::PixelXDimension,
        ifd_num: exif::In::PRIMARY,
        value: exif::Value::Long(vec![width]),
    });
    fields.push(exif::Field {
        tag: exif::Tag::PixelYDimension,
        ifd_num: exif::In::PRIMARY,
        value: exif::Value::Long(vec![height]),
    });

    let mut writer = exif::experimental::Writer::new();
    let mut seen = std::collections::HashSet::new();
    for f in &fields {
        if seen.insert((f.tag.context(), f.tag.number())) {
            writer.push_field(f);
        }
    }
    let mut out = Cursor::new(Vec::new());
    writer.write(&mut out, true).ok()?;
    Some(out.into_inner())
}

fn read_source_exif(path: &Path) -> Option<exif::Exif> {
    if let Ok(file) = File::open(path) {
        if let Ok(e) = exif::Reader::new().read_from_container(&mut BufReader::new(file)) {
            return Some(e);
        }
    }
    if crate::config::is_raw_file(path) {
        if crate::cr3_parser::is_cr3_file(path) {
            // CMT1 holds IFD0 (camera, date); good enough for exports.
            if let Some(blob) = crate::cr3_parser::extract_cr3_exif_blocks(path)
                .into_iter()
                .next()
            {
                if let Ok(e) = exif::Reader::new().read_raw(blob) {
                    return Some(e);
                }
            }
        }
        let jpeg = crate::cr3_parser::extract_largest_embedded_jpeg(path)?;
        return exif::Reader::new()
            .read_from_container(&mut Cursor::new(jpeg))
            .ok();
    }
    None
}

/// Reads the ASCII value of a TIFF-context tag from an EXIF block.
fn exif_ascii(exif_tiff: &[u8], tag_number: u16) -> Option<String> {
    let e = exif::Reader::new().read_raw(exif_tiff.to_vec()).ok()?;
    let f = e
        .fields()
        .find(|f| f.tag.number() == tag_number && f.ifd_num == exif::In::PRIMARY)?;
    match &f.value {
        exif::Value::Ascii(v) => v
            .first()
            .map(|s| String::from_utf8_lossy(s).trim().to_string())
            .filter(|s| !s.is_empty()),
        _ => None,
    }
}

pub fn save_image_with_icc(
    dest_path: &Path,
    width: u32,
    height: u32,
    rgb_pixels: &[u8],
    icc_bytes: Option<&[u8]>,
    exif_tiff: Option<&[u8]>,
) -> Result<(), String> {
    let ext = dest_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if rgb_pixels.len() != width as usize * height as usize * 3 {
        return Err("Internal error: pixel buffer does not match image size".into());
    }

    let encoded: Vec<u8> = match ext.as_str() {
        "jpg" | "jpeg" | "jpe" => {
            let mut raw_jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut raw_jpeg, 96)
                .write_image(rgb_pixels, width, height, ColorType::Rgb8.into())
                .map_err(|e| format!("JPEG encode error: {}", e))?;
            let mut out = raw_jpeg;
            if let Some(icc) = icc_bytes {
                let mut jpeg = img_parts::jpeg::Jpeg::from_bytes(Bytes::from(out.clone()))
                    .map_err(|e| format!("JPEG ICC error: {}", e))?;
                jpeg.set_icc_profile(Some(Bytes::copy_from_slice(icc)));
                let mut buf = Vec::new();
                jpeg.encoder()
                    .write_to(&mut buf)
                    .map_err(|e| format!("JPEG ICC write error: {}", e))?;
                out = buf;
            }
            if let Some(exif) = exif_tiff {
                if let Ok(with_exif) = crate::exif_orient::replace_jpeg_exif(&out, exif) {
                    out = with_exif;
                }
            }
            out
        }
        "png" => {
            let mut raw_png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut raw_png)
                .write_image(rgb_pixels, width, height, ColorType::Rgb8.into())
                .map_err(|e| format!("PNG encode error: {}", e))?;
            let mut png = img_parts::png::Png::from_bytes(Bytes::from(raw_png))
                .map_err(|e| format!("PNG error: {}", e))?;
            if let Some(icc) = icc_bytes {
                png.set_icc_profile(Some(Bytes::copy_from_slice(icc)));
            }
            if let Some(exif) = exif_tiff {
                png.set_exif(Some(Bytes::copy_from_slice(exif)));
            }
            let mut buf = Vec::new();
            png.encoder()
                .write_to(&mut buf)
                .map_err(|e| format!("PNG write error: {}", e))?;
            buf
        }
        "webp" => {
            let mut raw_webp = Vec::new();
            image::codecs::webp::WebPEncoder::new_lossless(&mut raw_webp)
                .write_image(rgb_pixels, width, height, ColorType::Rgb8.into())
                .map_err(|e| format!("WebP encode error: {}", e))?;
            crate::exif_orient::build_webp(
                &raw_webp, width, height, false, icc_bytes, exif_tiff, None,
            )?
        }
        "tif" | "tiff" => {
            let mut buf = Cursor::new(Vec::new());
            {
                let mut tiff_enc = TiffEncoder::new(&mut buf)
                    .map_err(|e| format!("TIFF encoder init error: {}", e))?;
                let mut img_dir = tiff_enc
                    .new_image::<colortype::RGB8>(width, height)
                    .map_err(|e| format!("TIFF dir error: {}", e))?;
                if let Some(icc) = icc_bytes {
                    img_dir
                        .encoder()
                        .write_tag(Tag::Unknown(34675), icc)
                        .map_err(|e| format!("TIFF ICC tag error: {}", e))?;
                }
                if let Some(exif) = exif_tiff {
                    // Basic descriptive tags (Make, Model, DateTime, Artist, Copyright, Software).
                    for code in [0x010Fu16, 0x0110, 0x0132, 0x013B, 0x8298, 0x0131] {
                        if let Some(v) = exif_ascii(exif, code) {
                            let _ = img_dir.encoder().write_tag(Tag::Unknown(code), v.as_str());
                        }
                    }
                }
                img_dir
                    .write_data(rgb_pixels)
                    .map_err(|e| format!("TIFF write error: {}", e))?;
            }
            buf.into_inner()
        }
        "bmp" => {
            let mut buf = Vec::new();
            image::codecs::bmp::BmpEncoder::new(&mut buf)
                .write_image(rgb_pixels, width, height, ColorType::Rgb8.into())
                .map_err(|e| format!("BMP encode error: {}", e))?;
            buf
        }
        "" => {
            return Err(
                "Please choose a file name with an extension (.jpg, .png, .tif, .webp, .bmp)"
                    .into(),
            )
        }
        other => return Err(format!("Saving as '.{}' is not supported", other)),
    };

    atomic_write(dest_path, &encoded)
}

/// Generates the next available sequential filename in the same directory as `original_path`.
/// E.g. `foto.jpg` -> `foto_1.jpg` (or `foto_2.jpg` if `foto_1.jpg` already exists, etc.).
/// Only short numeric suffixes (up to 3 digits) are treated as an existing sequence, so names
/// like `IMG_20240101.jpg` become `IMG_20240101_1.jpg`.
pub fn generate_next_sequential_path(original_path: &Path) -> PathBuf {
    let parent = original_path.parent().unwrap_or_else(|| Path::new(""));
    let stem = original_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let ext = original_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("jpg");

    let (base_stem, start_num) = match stem.rfind('_') {
        Some(pos) if (1..=3).contains(&(stem.len() - pos - 1)) => {
            match stem[pos + 1..].parse::<usize>() {
                Ok(n) => (&stem[..pos], n + 1),
                Err(_) => (stem, 1),
            }
        }
        _ => (stem, 1),
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
        assert_eq!(
            seq1,
            Path::new("C:/nonexistent_folder_abc123/my_photo_1.jpg")
        );

        let p_existing_seq = Path::new("C:/nonexistent_folder_abc123/my_photo_3.jpg");
        let seq2 = generate_next_sequential_path(p_existing_seq);
        assert_eq!(
            seq2,
            Path::new("C:/nonexistent_folder_abc123/my_photo_4.jpg")
        );

        let dated = Path::new("C:/nonexistent_folder_abc123/IMG_20240101.jpg");
        assert_eq!(
            generate_next_sequential_path(dated),
            Path::new("C:/nonexistent_folder_abc123/IMG_20240101_1.jpg")
        );
    }

    #[test]
    fn export_keeps_exif_and_icc() {
        let dir = std::env::temp_dir().join(format!("mriv-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Source JPEG with EXIF (Make + Orientation 6).
        let src = dir.join("src.jpg");
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            8,
            4,
            image::Rgb([10, 20, 30]),
        ));
        let mut jpg = Vec::new();
        img.write_to(&mut Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
            .unwrap();
        let make = exif::Field {
            tag: exif::Tag::Make,
            ifd_num: exif::In::PRIMARY,
            value: exif::Value::Ascii(vec![b"TestCam".to_vec()]),
        };
        let orient = exif::Field {
            tag: exif::Tag::Orientation,
            ifd_num: exif::In::PRIMARY,
            value: exif::Value::Short(vec![6]),
        };
        let mut w = exif::experimental::Writer::new();
        w.push_field(&make);
        w.push_field(&orient);
        let mut ex = Cursor::new(Vec::new());
        w.write(&mut ex, true).unwrap();
        std::fs::write(
            &src,
            crate::exif_orient::replace_jpeg_exif(&jpg, ex.get_ref()).unwrap(),
        )
        .unwrap();

        let exif = build_export_exif(&src, 4, 8).expect("exif");
        let icc = crate::color_manager::ColorManager::new()
            .get_profile_bytes(crate::config::PROFILE_ADOBE_RGB, None);
        for ext in ["jpg", "png", "webp", "tif"] {
            let out = dir.join(format!("out.{}", ext));
            save_image_with_icc(&out, 4, 8, &vec![50u8; 4 * 8 * 3], Some(&icc), Some(&exif))
                .unwrap();
            let decoded = crate::image_loader::load_image(&out).unwrap();
            assert_eq!(
                (decoded.width, decoded.height),
                (4, 8),
                "{}: orientation must not be re-applied",
                ext
            );
            assert!(decoded.embedded_icc.is_some(), "{}: ICC missing", ext);
            if ext != "tif" {
                let e = exif::Reader::new()
                    .read_from_container(&mut BufReader::new(File::open(&out).unwrap()))
                    .unwrap();
                let m = e.get_field(exif::Tag::Make, exif::In::PRIMARY).unwrap();
                assert!(m.display_value().to_string().contains("TestCam"), "{}", ext);
                assert!(e
                    .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                    .is_none());
            }
        }
        assert!(save_image_with_icc(&dir.join("x.dng"), 1, 1, &[0, 0, 0], None, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
