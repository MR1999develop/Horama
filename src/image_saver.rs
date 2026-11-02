//! Save and export images in JPEG, TIFF, PNG, WebP and BMP with embedded ICC profiles and the
//! original EXIF metadata. Files are always written atomically (temp file + rename).
//!
//! JPEG is lossy, so every new compression loses detail. To keep repeated saves from wearing
//! a photo down:
//! * a JPEG whose pixels did not change is never recompressed ([`save_jpeg_unchanged`]):
//!   its compressed data is kept byte for byte, only the ICC profile may change;
//! * an edited JPEG (crop, profile conversion) is compressed with the quantization tables and
//!   chroma subsampling of the original ([`JpegSettings`]), so the detail the original kept
//!   survives and saving again does not degrade it further in a visible way;
//! * other sources are compressed at quality 95 without chroma subsampling;
//! * the user can instead choose a quality ([`JpegCompression::Quality`]), e.g. for a
//!   smaller file.

use crate::color_manager::ColorManager;
use crate::config::{DEFAULT_FALLBACK_PROFILE, PROFILE_EMBEDDED};
use crate::disk_ops::atomic_write;
use crate::image_loader::DecodedImage;
use image::{ColorType, ImageEncoder};
use img_parts::{Bytes, ImageEXIF, ImageICC};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use tiff::encoder::{colortype, TiffEncoder};
use tiff::tags::Tag;

/// Quality used when the source is not a JPEG (nothing to copy the settings from).
const DEFAULT_JPEG_QUALITY: u8 = 95;

/// How a JPEG was compressed: quantization tables (natural order) and the luma sampling
/// factors (chroma is 1×1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JpegSettings {
    luma: [u16; 64],
    chroma: [u16; 64],
    sampling: (u8, u8),
}

/// How to compress a JPEG.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JpegCompression {
    /// With the tables and chroma sampling of the original.
    Like(Box<JpegSettings>),
    /// At a quality chosen by the user (1–100). From 90 up the color keeps full resolution
    /// (4:4:4); below, it is halved in both directions (4:2:0) as most cameras do.
    Quality(u8),
}

impl Default for JpegCompression {
    fn default() -> Self {
        JpegCompression::Quality(DEFAULT_JPEG_QUALITY)
    }
}

/// Zigzag position → natural position of a coefficient in an 8×8 block (ITU-T T.81, A.3.6).
const ZIGZAG_TO_NATURAL: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Reads the compression settings of a baseline or progressive 8-bit JPEG. Returns `None` for
/// anything else (12-bit, arithmetic coding, CMYK, malformed headers): the caller then uses
/// the default settings.
pub fn jpeg_settings_of(jpeg: &[u8]) -> Option<JpegSettings> {
    if jpeg.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut tables: [Option<[u16; 64]>; 4] = [None; 4];
    // (component id, sampling, table id) of the frame header.
    let mut components: Vec<(u8, (u8, u8), usize)> = Vec::new();
    let mut i = 2usize;
    while i + 4 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        let marker = jpeg[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break; // start of scan: all tables needed are known
        }
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        let seg = jpeg.get(i + 4..i + 2 + len)?;
        match marker {
            0xDB => {
                let mut p = 0;
                while p < seg.len() {
                    let (precision, id) = (seg[p] >> 4, (seg[p] & 0x0F) as usize);
                    if precision != 0 || id > 3 {
                        return None; // 16-bit tables: not supported by the encoder
                    }
                    let values = seg.get(p + 1..p + 65)?;
                    let mut t = [0u16; 64];
                    for (z, &v) in values.iter().enumerate() {
                        t[ZIGZAG_TO_NATURAL[z]] = u16::from(v).max(1);
                    }
                    tables[id] = Some(t);
                    p += 65;
                }
            }
            0xC0..=0xC2 => {
                if *seg.first()? != 8 {
                    return None;
                }
                let n = *seg.get(5)? as usize;
                for c in 0..n {
                    let b = seg.get(6 + c * 3..9 + c * 3)?;
                    components.push((b[0], (b[1] >> 4, b[1] & 0x0F), b[2] as usize & 3));
                }
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            _ => {}
        }
        i += 2 + len;
    }
    let luma_c = components.first()?;
    let luma = tables[luma_c.2]?;
    let chroma = match components.get(1) {
        Some(c) => {
            if c.1 != (1, 1) || components.get(2).map(|c3| c3.1 != (1, 1)).unwrap_or(false) {
                return None; // unusual layout: let the encoder choose
            }
            tables[c.2]?
        }
        None => luma, // grayscale source: the RGB copy uses the luma table for chroma too
    };
    Some(JpegSettings {
        luma,
        chroma,
        sampling: luma_c.1,
    })
}

/// Compresses RGB pixels to JPEG.
fn encode_jpeg(
    rgb: &[u8],
    width: u32,
    height: u32,
    compression: &JpegCompression,
) -> Result<Vec<u8>, String> {
    use jpeg_encoder::{ColorType as JColor, Encoder, QuantizationTableType, SamplingFactor};
    let (w, h) = (
        u16::try_from(width).map_err(|_| "image too wide for JPEG (max 65535 px)".to_string())?,
        u16::try_from(height).map_err(|_| "image too tall for JPEG (max 65535 px)".to_string())?,
    );
    let mut out = Vec::new();
    let quality = match compression {
        JpegCompression::Quality(q) => (*q).clamp(1, 100),
        JpegCompression::Like(_) => DEFAULT_JPEG_QUALITY,
    };
    let mut enc = Encoder::new(&mut out, quality);
    enc.set_optimized_huffman_tables(true);
    let sampling = match compression {
        JpegCompression::Like(l) => match l.sampling {
            (2, 1) => SamplingFactor::F_2_1,
            (1, 2) => SamplingFactor::F_1_2,
            (2, 2) => SamplingFactor::F_2_2,
            _ => SamplingFactor::F_1_1,
        },
        JpegCompression::Quality(q) if *q < 90 => SamplingFactor::F_2_2,
        JpegCompression::Quality(_) => SamplingFactor::F_1_1, // full color resolution
    };
    enc.set_sampling_factor(sampling);
    if let JpegCompression::Like(l) = compression {
        enc.set_quantization_tables(
            QuantizationTableType::Custom(Box::new(l.luma)),
            QuantizationTableType::Custom(Box::new(l.chroma)),
        );
    }
    enc.encode(rgb, w, h, JColor::Rgb)
        .map_err(|e| format!("JPEG encode error: {}", e))?;
    Ok(out)
}

/// Saves a JPEG whose pixels did not change without recompressing it: the compressed data and
/// the EXIF metadata of `source` are kept byte for byte. With `icc`, the ICC profile is
/// replaced (a profile assigned to the image). Saving over the source with nothing to change
/// writes nothing.
pub fn save_jpeg_unchanged(source: &Path, dest: &Path, icc: Option<&[u8]>) -> Result<(), String> {
    let bytes =
        std::fs::read(source).map_err(|e| format!("Cannot read '{}': {}", source.display(), e))?;
    let out = match icc {
        Some(icc) => {
            let mut jpeg = img_parts::jpeg::Jpeg::from_bytes(Bytes::from(bytes.clone()))
                .map_err(|e| format!("JPEG ICC error: {}", e))?;
            jpeg.set_icc_profile(Some(Bytes::copy_from_slice(icc)));
            let mut buf = Vec::new();
            jpeg.encoder()
                .write_to(&mut buf)
                .map_err(|e| format!("JPEG ICC write error: {}", e))?;
            buf
        }
        None => bytes.clone(),
    };
    let same_file = crate::config::same_path(&source.to_string_lossy(), &dest.to_string_lossy());
    if same_file && out == bytes {
        return Ok(());
    }
    atomic_write(dest, &out)
}

/// Saves `decoded` to `dest` with color profile `profile`; the format follows the extension.
///
/// * `edited`: the pixels differ from the file (crop).
/// * `jpeg_quality`: 0 = compress like the original (and do not recompress a JPEG whose
///   pixels did not change), 1–100 = the quality chosen by the user.
pub fn export_image(
    cm: &ColorManager,
    decoded: &DecodedImage,
    profile: &str,
    dest: &Path,
    edited: bool,
    jpeg_quality: u8,
) -> Result<(), String> {
    let source = Path::new(&decoded.path);
    let embedded = decoded.embedded_icc.as_deref();
    let unchanged_pixels = !edited && !cm.converts_on_save(profile, embedded);
    if jpeg_quality == 0 && unchanged_pixels && is_jpeg(source) && is_jpeg(dest) {
        // Same pixels: keep the compressed data; write a profile only if another one was
        // assigned to the image.
        let own_profile = if decoded.embedded_icc_name.is_some() {
            PROFILE_EMBEDDED
        } else {
            DEFAULT_FALLBACK_PROFILE
        };
        let icc = (profile != own_profile).then(|| cm.get_profile_bytes(profile, embedded));
        return save_jpeg_unchanged(source, dest, icc.as_deref());
    }
    let compression = if jpeg_quality > 0 {
        JpegCompression::Quality(jpeg_quality)
    } else {
        // Recompressed JPEGs reuse the tables of the original, so repeated saves do not wear it.
        is_jpeg(source)
            .then(|| std::fs::read(source).ok())
            .flatten()
            .and_then(|b| jpeg_settings_of(&b))
            .map(|s| JpegCompression::Like(Box::new(s)))
            .unwrap_or_default()
    };
    let pixels = cm.prepare_pixels_for_save(&decoded.rgb_pixels, profile, embedded);
    let icc = cm.get_profile_bytes(profile, embedded);
    let exif = build_export_exif(source, decoded.width, decoded.height);
    save_image_with_icc(
        dest,
        decoded.width,
        decoded.height,
        &pixels,
        Some(&icc),
        exif.as_deref(),
        &compression,
    )
}

/// Whether `p` has a JPEG extension.
pub fn is_jpeg(p: &Path) -> bool {
    matches!(
        p.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "jpe" | "jfif")
    )
}

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
    if let Ok(file) = File::open(path)
        && let Ok(e) = exif::Reader::new().read_from_container(&mut BufReader::new(file)) {
            return Some(e);
        }
    if crate::config::is_raw_file(path) {
        if crate::cr3_parser::is_cr3_file(path) {
            // CMT1 holds IFD0 (camera, date); good enough for exports.
            if let Some(blob) = crate::cr3_parser::extract_cr3_exif_blocks(path)
                .into_iter()
                .next()
                && let Ok(e) = exif::Reader::new().read_raw(blob) {
                    return Some(e);
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
    jpeg: &JpegCompression,
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
            let mut out = encode_jpeg(rgb_pixels, width, height, jpeg)?;
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
            if let Some(exif) = exif_tiff
                && let Ok(with_exif) = crate::exif_orient::replace_jpeg_exif(&out, exif) {
                    out = with_exif;
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
            save_image_with_icc(
                &out,
                4,
                8,
                &[50u8; 4 * 8 * 3],
                Some(&icc),
                Some(&exif),
                &JpegCompression::default(),
            )
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
        assert!(save_image_with_icc(
            &dir.join("x.dng"),
            1,
            1,
            &[0, 0, 0],
            None,
            None,
            &JpegCompression::default()
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A photo-like test image: smooth gradients, fine stripes and saturated color edges.
    fn test_image(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let stripes = if (x / 2 + y / 3) % 2 == 0 { 40 } else { 0 };
                v.push(((x * 255 / w) as u8).saturating_add(stripes));
                v.push((y * 255 / h) as u8);
                v.push(if (x / 16 + y / 16) % 2 == 0 { 230 } else { 20 });
            }
        }
        v
    }

    fn decode_rgb(bytes: &[u8]) -> Vec<u8> {
        image::load_from_memory(bytes).unwrap().to_rgb8().into_raw()
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        if mse == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    }

    #[test]
    fn jpeg_settings_round_trip_through_the_encoder() {
        let rgb = test_image(64, 48);
        let first = encode_jpeg(&rgb, 64, 48, &JpegCompression::default()).unwrap();
        let s = jpeg_settings_of(&first).expect("settings of our own JPEG");
        assert_eq!(s.sampling, (1, 1));
        let again = encode_jpeg(
            &decode_rgb(&first),
            64,
            48,
            &JpegCompression::Like(Box::new(s.clone())),
        )
        .unwrap();
        assert_eq!(
            jpeg_settings_of(&again),
            Some(s),
            "same tables and sampling"
        );
        assert!(jpeg_settings_of(b"not a jpeg").is_none());
        assert!(jpeg_settings_of(&first[..20]).is_none(), "truncated header");
    }

    #[test]
    fn saving_again_with_the_source_settings_does_not_wear_the_image() {
        let (w, h) = (96, 64);
        let original = test_image(w, h);
        let gen1 = encode_jpeg(&original, w, h, &JpegCompression::default()).unwrap();
        let settings = jpeg_settings_of(&gen1).unwrap();
        let mut current = gen1.clone();
        for _ in 0..10 {
            current = encode_jpeg(
                &decode_rgb(&current),
                w,
                h,
                &JpegCompression::Like(Box::new(settings.clone())),
            )
            .unwrap();
        }
        let drift = psnr(&decode_rgb(&gen1), &decode_rgb(&current));
        assert!(
            drift > 40.0,
            "10 more saves changed the image too much: {:.1} dB",
            drift
        );
    }

    #[test]
    fn chosen_quality_trades_size_for_detail() {
        let (w, h) = (96, 64);
        let rgb = test_image(w, h);
        let high = encode_jpeg(&rgb, w, h, &JpegCompression::Quality(98)).unwrap();
        let low = encode_jpeg(&rgb, w, h, &JpegCompression::Quality(60)).unwrap();
        assert!(
            low.len() < high.len(),
            "{} vs {} bytes",
            low.len(),
            high.len()
        );
        assert!(psnr(&rgb, &decode_rgb(&high)) > psnr(&rgb, &decode_rgb(&low)));
        assert_eq!(jpeg_settings_of(&high).unwrap().sampling, (1, 1));
        assert_eq!(jpeg_settings_of(&low).unwrap().sampling, (2, 2));
        // Out-of-range values are clamped, not rejected.
        assert!(encode_jpeg(&rgb, w, h, &JpegCompression::Quality(0)).is_ok());
    }

    #[test]
    fn unchanged_jpeg_keeps_its_bytes() {
        let dir = std::env::temp_dir().join(format!("horama-unchanged-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.jpg");
        std::fs::write(
            &src,
            encode_jpeg(&test_image(32, 32), 32, 32, &JpegCompression::default()).unwrap(),
        )
        .unwrap();
        let before = std::fs::read(&src).unwrap();
        let mtime = std::fs::metadata(&src).unwrap().modified().unwrap();

        // Over itself, nothing to change: not even rewritten.
        save_jpeg_unchanged(&src, &src, None).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), before);
        assert_eq!(std::fs::metadata(&src).unwrap().modified().unwrap(), mtime);

        // Copy with an assigned profile: same pixels, new ICC.
        let copy = dir.join("b.jpg");
        let icc = crate::color_manager::ColorManager::new()
            .get_profile_bytes(crate::config::PROFILE_ADOBE_RGB, None);
        save_jpeg_unchanged(&src, &copy, Some(&icc)).unwrap();
        let out = std::fs::read(&copy).unwrap();
        assert_eq!(decode_rgb(&out), decode_rgb(&before));
        let jpeg = img_parts::jpeg::Jpeg::from_bytes(Bytes::from(out)).unwrap();
        assert_eq!(jpeg.icc_profile().as_deref(), Some(&icc[..]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
