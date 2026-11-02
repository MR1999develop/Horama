//! Image and Camera RAW decoding with ICC profile extraction and EXIF orientation support.

use crate::color_manager::{extract_icc_description, icc_color_space};
use crate::config::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, is_raw_file};
use crate::cr3_parser::extract_embedded_jpegs;
use image::{DynamicImage, ImageReader};
use img_parts::ImageICC;
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

/// Fast EXIF thumbnail extractor (works for JPEG, TIFF, and RAW files with standard EXIF APP1/IFD1 thumbnail).
pub fn extract_exif_thumbnail(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let exif_data = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let off_field = exif_data.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)?;
    let len_field =
        exif_data.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)?;
    let offset = off_field.value.get_uint(0)? as usize;
    let len = len_field.value.get_uint(0)? as usize;
    let tiff_buf = exif_data.buf();
    let end = offset.checked_add(len)?;
    if end <= tiff_buf.len() && len > 0 {
        let candidate = &tiff_buf[offset..end];
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
    /// Display pixels: 8-bit RGB, orientation already applied.
    pub rgb_pixels: Vec<u8>,
    /// Embedded RGB ICC profile (non-RGB profiles are never stored here).
    pub embedded_icc: Option<Vec<u8>>,
    pub embedded_icc_name: Option<String>,
    /// Set when the file carries an ICC profile that cannot be applied to RGB pixels
    /// (e.g. Gray or CMYK); the image is then treated as sRGB.
    pub ignored_icc_note: Option<String>,
    pub format_name: String,
    /// Bit depth of the decoded source data (the viewer itself works on 8-bit pixels).
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

    /// Copy of all metadata with new pixel data (avoids cloning the old pixel buffer).
    fn with_pixels(&self, width: u32, height: u32, rgb_pixels: Vec<u8>) -> DecodedImage {
        DecodedImage {
            path: self.path.clone(),
            width,
            height,
            rgb_pixels,
            embedded_icc: self.embedded_icc.clone(),
            embedded_icc_name: self.embedded_icc_name.clone(),
            ignored_icc_note: self.ignored_icc_note.clone(),
            format_name: self.format_name.clone(),
            bit_depth: self.bit_depth,
            is_raw: self.is_raw,
            file_size: self.file_size,
        }
    }

    fn remap(
        &self,
        new_w: usize,
        new_h: usize,
        map: impl Fn(usize, usize) -> (usize, usize),
    ) -> DecodedImage {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut new_pixels = vec![0u8; new_w * new_h * 3];
        for y in 0..h {
            let row = &self.rgb_pixels[y * w * 3..(y + 1) * w * 3];
            for x in 0..w {
                let (nx, ny) = map(x, y);
                let dst = (ny * new_w + nx) * 3;
                new_pixels[dst..dst + 3].copy_from_slice(&row[x * 3..x * 3 + 3]);
            }
        }
        self.with_pixels(new_w as u32, new_h as u32, new_pixels)
    }

    /// Rotate 90 degrees clockwise (exact pixel transformation).
    pub fn rotate_90_cw(&self) -> DecodedImage {
        let h = self.height as usize;
        self.remap(h, self.width as usize, |x, y| (h - 1 - y, x))
    }

    /// Rotate 90 degrees counter-clockwise (exact pixel transformation).
    pub fn rotate_90_ccw(&self) -> DecodedImage {
        let w = self.width as usize;
        self.remap(self.height as usize, w, |x, y| (y, w - 1 - x))
    }

    /// Rotate 180 degrees.
    pub fn rotate_180(&self) -> DecodedImage {
        let (w, h) = (self.width as usize, self.height as usize);
        self.remap(w, h, |x, y| (w - 1 - x, h - 1 - y))
    }

    /// Flip horizontally.
    #[allow(dead_code)]
    pub fn flip_horizontal(&self) -> DecodedImage {
        let (w, h) = (self.width as usize, self.height as usize);
        self.remap(w, h, |x, y| (w - 1 - x, y))
    }

    /// Crop image to the given rectangle; the rectangle is clamped to the image bounds.
    pub fn crop(&self, crop_x: u32, crop_y: u32, crop_w: u32, crop_h: u32) -> DecodedImage {
        let w = self.width;
        let h = self.height;
        let crop_x = if crop_x >= w { 0 } else { crop_x };
        let crop_y = if crop_y >= h { 0 } else { crop_y };
        let crop_w = crop_w.min(w - crop_x).max(1);
        let crop_h = crop_h.min(h - crop_y).max(1);

        let mut new_pixels = Vec::with_capacity(crop_w as usize * crop_h as usize * 3);
        let src_w = w as usize;
        for y in crop_y..(crop_y + crop_h) {
            let row_start = (y as usize * src_w + crop_x as usize) * 3;
            let row_end = row_start + (crop_w as usize * 3);
            new_pixels.extend_from_slice(&self.rgb_pixels[row_start..row_end]);
        }
        self.with_pixels(crop_w, crop_h, new_pixels)
    }
}

/// Extract embedded ICC profile bytes from JPEG, PNG, WebP, or TIFF files.
pub fn extract_icc_from_bytes(bytes: &[u8], ext: &str) -> Option<Vec<u8>> {
    let ext_low = ext.to_lowercase();
    if matches!(ext_low.as_str(), "jpg" | "jpeg" | "jpe" | "jfif") {
        if let Ok(jpeg) = img_parts::jpeg::Jpeg::from_bytes(bytes.to_vec().into())
            && let Some(icc) = jpeg.icc_profile()
            && !icc.is_empty()
        {
            return Some(icc.to_vec());
        }
        return extract_jpeg_icc_from_header(bytes);
    } else if ext_low == "png" {
        if let Ok(png) = img_parts::png::Png::from_bytes(bytes.to_vec().into())
            && let Some(icc) = png.icc_profile()
            && !icc.is_empty()
        {
            return Some(icc.to_vec());
        }
    } else if ext_low == "webp" {
        if let Ok(webp) = img_parts::webp::WebP::from_bytes(bytes.to_vec().into())
            && let Some(icc) = webp.icc_profile()
            && !icc.is_empty()
        {
            return Some(icc.to_vec());
        }
    } else if matches!(ext_low.as_str(), "tif" | "tiff")
        && let Ok(mut dec) = tiff::decoder::Decoder::new(Cursor::new(bytes))
    {
        // Tag 34675 (0x8773) is InterColorProfile (ICC Profile)
        if let Ok(Some(tiff::decoder::ifd::Value::List(list))) =
            dec.find_tag(tiff::tags::Tag::Unknown(34675))
        {
            use tiff::decoder::ifd::Value;
            // Depending on the writer the bytes come back as BYTE/UNDEFINED or wider ints.
            let icc: Vec<u8> = list
                .into_iter()
                .filter_map(|v| match v {
                    Value::Byte(b) => Some(b),
                    Value::Short(b) => Some(b as u8),
                    Value::Unsigned(b) => Some(b as u8),
                    Value::UnsignedBig(b) => Some(b as u8),
                    _ => None,
                })
                .collect();
            if icc.len() >= 128 {
                return Some(icc);
            }
        }
        if let Ok(vec_u8) = dec.get_tag_u8_vec(tiff::tags::Tag::Unknown(34675))
            && vec_u8.len() >= 128
        {
            return Some(vec_u8);
        }
    }
    None
}

/// Reassembles an ICC profile from JPEG APP2 "ICC_PROFILE" segments using only the file
/// header (works on a truncated prefix of the file, which makes it cheap for thumbnails).
pub fn extract_jpeg_icc_from_header(data: &[u8]) -> Option<Vec<u8>> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut parts: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut pos = 2usize;
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            break;
        }
        let marker = data[pos + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if len < 2 || pos + 2 + len > data.len() {
            break;
        }
        let content = &data[pos + 4..pos + 2 + len];
        if marker == 0xE2 && content.len() > 14 && content.starts_with(b"ICC_PROFILE\0") {
            parts.push((content[12], content[14..].to_vec()));
        }
        pos += 2 + len;
    }
    if parts.is_empty() {
        return None;
    }
    parts.sort_by_key(|p| p.0);
    let icc: Vec<u8> = parts.into_iter().flat_map(|p| p.1).collect();
    (icc.len() >= 128).then_some(icc)
}

/// Splits an embedded profile into (usable RGB profile, note about an ignored profile).
fn classify_icc(icc: Option<Vec<u8>>) -> (Option<Vec<u8>>, Option<String>, Option<String>) {
    let Some(icc) = icc else {
        return (None, None, None);
    };
    match icc_color_space(&icc) {
        Some(cs) if cs == *b"RGB " => {
            let name = extract_icc_description(&icc);
            (Some(icc), name, None)
        }
        other => {
            let cs = other
                .map(|c| String::from_utf8_lossy(&c).trim().to_string())
                .unwrap_or_else(|| "unknown".into());
            let name = extract_icc_description(&icc).unwrap_or_else(|| "Embedded ICC".into());
            (
                None,
                None,
                Some(format!(
                    "{} ({} profile, not applicable to RGB — sRGB assumed)",
                    name, cs
                )),
            )
        }
    }
}

/// Read EXIF orientation (1..=8) from a file or byte slice.
fn read_exif_orientation_from_bytes(bytes: &[u8]) -> u32 {
    let mut cursor = Cursor::new(bytes);
    if let Ok(exif_data) = exif::Reader::new().read_from_container(&mut cursor)
        && let Some(field) = exif_data.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        && let Some(v) = field.value.get_uint(0)
    {
        return v;
    }
    1
}

pub fn read_exif_orientation_from_file(path: &Path) -> u32 {
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        if let Ok(exif_data) = exif::Reader::new().read_from_container(&mut reader)
            && let Some(field) = exif_data.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
            && let Some(v) = field.value.get_uint(0)
        {
            return v;
        }
    }
    1
}

/// Applies an EXIF orientation (1..=8) to an image.
pub(crate) fn apply_orientation(img: DynamicImage, orientation: u32) -> DynamicImage {
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

fn decode_bytes(bytes: &[u8]) -> Option<DynamicImage> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

/// Largest sensor developed from raw data (pixels). Today's largest cameras have about 150 MP;
/// anything far above is a corrupt or hostile header.
const MAX_RAW_PIXELS: u64 = 200_000_000;
/// Peak memory of rawloader + imagepipe per sensor pixel: 16-bit raw data plus two `f32`
/// working buffers of up to 4 channels alive at the same time, plus the 8-bit output.
const RAW_DEVELOP_BYTES_PER_PIXEL: u64 = 2 + 2 * 4 * 4 + 3;

/// Only one RAW is developed at a time, so parallel preloads cannot add up their peaks.
static RAW_DEVELOP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Reads only the RAW headers and refuses sensors that are implausibly large or would not fit
/// in the free memory. A failed allocation aborts the whole program (it is not a panic that
/// `catch_unwind` could stop), so the check must happen before decoding.
fn check_raw_develop_size(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("Cannot open '{}': {}", path.display(), e))?;
    let header = rawloader::decode_dummy(&mut BufReader::new(file))
        .map_err(|e| format!("Failed to decode RAW file '{}': {}", path.display(), e))?;
    let free = {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.available_memory()
    };
    raw_develop_size_ok(header.width as u64, header.height as u64, free)
}

/// `free_bytes == 0` means "unknown" (no memory check, only the pixel limit).
fn raw_develop_size_ok(width: u64, height: u64, free_bytes: u64) -> Result<(), String> {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let pixels = width.saturating_mul(height);
    if pixels == 0 {
        return Err("RAW file declares an empty image".to_string());
    }
    if pixels > MAX_RAW_PIXELS {
        return Err(format!(
            "RAW sensor of {}×{} pixels ({} MP) exceeds the supported {} MP; the file is probably damaged",
            width,
            height,
            pixels / 1_000_000,
            MAX_RAW_PIXELS / 1_000_000
        ));
    }
    let needed = pixels.saturating_mul(RAW_DEVELOP_BYTES_PER_PIXEL);
    if free_bytes > 0 && needed > free_bytes {
        return Err(format!(
            "Not enough free memory to develop this RAW: about {:.1} GB needed, {:.1} GB free. \
             Close other programs and try again",
            needed as f64 / GB,
            free_bytes as f64 / GB
        ));
    }
    Ok(())
}

/// Decode a Camera RAW file using the embedded full-size preview or pure-Rust rawloader + imagepipe.
fn decode_raw_image(path: &Path, file_size: u64) -> Result<DecodedImage, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("RAW")
        .to_uppercase();

    let file_orientation = read_exif_orientation_from_file(path);

    // 1. Embedded full-size JPEG previews (CR3/CR2/NEF/ARW/RAF...): try candidates largest first.
    for jpeg_bytes in extract_embedded_jpegs(path, 3) {
        let Some(dyn_img) = decode_bytes(&jpeg_bytes) else {
            continue;
        };
        let jpeg_orient = read_exif_orientation_from_bytes(&jpeg_bytes);
        let orient = if jpeg_orient != 1 {
            jpeg_orient
        } else {
            file_orientation
        };
        let oriented = apply_orientation(dyn_img, orient);
        let (embedded_icc, embedded_icc_name, ignored_icc_note) =
            classify_icc(extract_icc_from_bytes(&jpeg_bytes, "jpg"));
        let rgb = oriented.to_rgb8();
        let (width, height) = rgb.dimensions();
        return Ok(DecodedImage {
            path: path.to_string_lossy().to_string(),
            width,
            height,
            rgb_pixels: rgb.into_raw(),
            embedded_icc,
            embedded_icc_name,
            ignored_icc_note,
            format_name: format!("{} (RAW, embedded preview)", ext),
            bit_depth: 8,
            is_raw: true,
            file_size,
        });
    }

    // 2. Full sensor demosaicing via rawloader + imagepipe: size checked before any large
    // allocation, one at a time (each needs several GB for a large sensor).
    check_raw_develop_size(path)?;
    let _one_at_a_time = RAW_DEVELOP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let raw_image = rawloader::decode_file(path)
        .map_err(|e| format!("Failed to decode RAW file '{}': {}", path.display(), e))?;
    let sensor_bits = raw_image
        .whitelevels
        .iter()
        .copied()
        .max()
        .map(|w| (32 - (w as u32).leading_zeros()) as u8)
        .unwrap_or(12);
    let mut pipeline = imagepipe::Pipeline::new_from_source(imagepipe::ImageSource::Raw(raw_image))
        .map_err(|e| format!("RAW pipeline error: {}", e))?;
    let decoded = pipeline
        .output_8bit(None)
        .map_err(|e| format!("RAW demosaic error: {}", e))?;

    // imagepipe already applies the sensor orientation (ops::transform), so it must not be
    // applied a second time here.
    let width = decoded.width as u32;
    let height = decoded.height as u32;
    let rgb_buf = image::RgbImage::from_raw(width, height, decoded.data)
        .ok_or_else(|| "Invalid RAW output buffer dimensions".to_string())?;

    Ok(DecodedImage {
        path: path.to_string_lossy().to_string(),
        width,
        height,
        rgb_pixels: rgb_buf.into_raw(),
        embedded_icc: None,
        embedded_icc_name: None,
        ignored_icc_note: None,
        format_name: format!("{} (RAW Demosaic)", ext),
        bit_depth: sensor_bits,
        is_raw: true,
        file_size,
    })
}

fn load_image_inner(path: &Path) -> Result<DecodedImage, String> {
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

    let (embedded_icc, embedded_icc_name, ignored_icc_note) =
        classify_icc(extract_icc_from_bytes(&bytes, &ext));
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
        ignored_icc_note,
        format_name: fmt_str,
        bit_depth,
        is_raw: false,
        file_size,
    })
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown decoder panic".to_string()
    }
}

/// Load and decode any supported standard or RAW image file.
/// Decoder panics on malformed files are caught and reported as errors.
pub fn load_image(path: &Path) -> Result<DecodedImage, String> {
    match catch_unwind(AssertUnwindSafe(|| load_image_inner(path))) {
        Ok(r) => r,
        Err(p) => Err(format!(
            "Decoder crashed on '{}': {}",
            path.display(),
            panic_message(p)
        )),
    }
}

/// Converts a decoded thumbnail to color-managed RGBA8.
fn finish_thumbnail(
    img: DynamicImage,
    emb_icc: Option<Vec<u8>>,
    cm: Option<&crate::color_manager::ColorManager>,
) -> (u32, u32, Vec<u8>) {
    let thumb = img.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    let rgb = thumb.to_rgb8();
    let (w, h) = rgb.dimensions();
    let (usable_icc, _, _) = classify_icc(emb_icc);
    let final_rgb = if let Some(mgr) = cm {
        let prof = if usable_icc.is_some() {
            crate::config::PROFILE_EMBEDDED
        } else {
            crate::config::DEFAULT_FALLBACK_PROFILE
        };
        let (managed, _) = mgr.apply_profile_with_info(rgb.as_raw(), prof, usable_icc.as_deref());
        managed
    } else {
        rgb.into_raw()
    };
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for [r, g, b] in final_rgb.as_chunks::<3>().0 {
        rgba.extend_from_slice(&[*r, *g, *b, 255]);
    }
    (w, h, rgba)
}

/// Reads at most `limit` bytes from the start of a file.
fn read_prefix(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let f = File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(limit).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn load_thumbnail_inner(
    path: &Path,
    cm: Option<&crate::color_manager::ColorManager>,
) -> Option<(u32, u32, Vec<u8>)> {
    let file_orientation = read_exif_orientation_from_file(path);
    let is_jpeg = matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "jpe" | "jfif")
    );

    // 1. Instant path: embedded EXIF thumbnail. The thumbnail JPEG has no EXIF of its own,
    //    so the orientation and ICC profile of the main file are used.
    if let Some(thumb_bytes) = extract_exif_thumbnail(path)
        && let Some(img) = decode_bytes(&thumb_bytes)
    {
        let emb_icc = if is_jpeg {
            read_prefix(path, 1024 * 1024).and_then(|p| extract_jpeg_icc_from_header(&p))
        } else {
            None
        };
        let oriented = apply_orientation(img, file_orientation);
        return Some(finish_thumbnail(oriented, emb_icc, cm));
    }

    // 2. RAW embedded preview path
    if is_raw_file(path) {
        for jpeg_bytes in extract_embedded_jpegs(path, 2) {
            let Some(img) = decode_bytes(&jpeg_bytes) else {
                continue;
            };
            let jpeg_orient = read_exif_orientation_from_bytes(&jpeg_bytes);
            let orient = if jpeg_orient != 1 {
                jpeg_orient
            } else {
                file_orientation
            };
            let emb_icc = extract_icc_from_bytes(&jpeg_bytes, "jpg");
            return Some(finish_thumbnail(
                apply_orientation(img, orient),
                emb_icc,
                cm,
            ));
        }
    }

    // 3. Fallback: full decode (PNG, WebP, BMP, or images without embedded thumbnails)
    let decoded = load_image(path).ok()?;
    let emb_icc = decoded.embedded_icc.clone();
    let rgb_img = image::RgbImage::from_raw(decoded.width, decoded.height, decoded.rgb_pixels)?;
    Some(finish_thumbnail(
        DynamicImage::ImageRgb8(rgb_img),
        emb_icc,
        cm,
    ))
}

/// Ultra-fast thumbnail loader returning (width, height, rgba8_pixels) for the bottom filmstrip.
/// Uses embedded EXIF IFD1 thumbnails where available (sub-millisecond decode),
/// embedded RAW JPEG previews, or falls back to full image decode.
pub fn load_thumbnail_rgba(
    path: &Path,
    cm: Option<&crate::color_manager::ColorManager>,
) -> Option<(u32, u32, Vec<u8>)> {
    catch_unwind(AssertUnwindSafe(|| load_thumbnail_inner(path, cm)))
        .ok()
        .flatten()
}

/// Creates a filmstrip thumbnail from an in-memory DecodedImage (used e.g. after rotation).
pub fn create_thumbnail_from_decoded(
    decoded: &DecodedImage,
    cm: Option<&crate::color_manager::ColorManager>,
) -> Option<(u32, u32, Vec<u8>)> {
    let rgb_img =
        image::RgbImage::from_raw(decoded.width, decoded.height, decoded.rgb_pixels.clone())?;
    Some(finish_thumbnail(
        DynamicImage::ImageRgb8(rgb_img),
        decoded.embedded_icc.clone(),
        cm,
    ))
}

/// Pixel dimensions of an image file without decoding it (header only where possible).
/// For RAW files the size of the embedded preview is used.
pub fn probe_dimensions(path: &Path) -> Option<(u32, u32)> {
    if !is_raw_file(path) {
        if let Ok(sz) = imagesize::size(path)
            && sz.width > 0
            && sz.height > 0
        {
            return Some((sz.width as u32, sz.height as u32));
        }
        return ImageReader::open(path)
            .ok()?
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok();
    }
    let jpeg = crate::cr3_parser::extract_largest_embedded_jpeg(path)?;
    ImageReader::new(Cursor::new(&jpeg))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_develop_limits() {
        const GB: u64 = 1024 * 1024 * 1024;
        // 45 MP camera with plenty of memory.
        assert!(raw_develop_size_ok(8192, 5464, 16 * GB).is_ok());
        // Same file on a machine with 1 GB free: refused with a clear reason.
        let e = raw_develop_size_ok(8192, 5464, GB).unwrap_err();
        assert!(e.contains("Not enough free memory"), "{}", e);
        // Unknown free memory: only the pixel limit applies.
        assert!(raw_develop_size_ok(8192, 5464, 0).is_ok());
        // Corrupt header: far beyond any sensor.
        let e = raw_develop_size_ok(40_000, 40_000, 1024 * GB).unwrap_err();
        assert!(e.contains("exceeds the supported"), "{}", e);
        assert!(raw_develop_size_ok(0, 4000, 16 * GB).is_err());
        assert!(raw_develop_size_ok(u64::MAX, u64::MAX, 16 * GB).is_err());
    }

    fn sample(width: u32, height: u32, rgb_pixels: Vec<u8>) -> DecodedImage {
        DecodedImage {
            path: "test.png".to_string(),
            width,
            height,
            rgb_pixels,
            embedded_icc: None,
            embedded_icc_name: None,
            ignored_icc_note: None,
            format_name: "PNG".to_string(),
            bit_depth: 8,
            is_raw: false,
            file_size: 100,
        }
    }

    #[test]
    fn test_decoded_image_rotations() {
        let img = sample(
            2,
            3,
            vec![
                10, 20, 30, 40, 50, 60, //
                70, 80, 90, 11, 22, 33, //
                44, 55, 66, 77, 88, 99,
            ],
        );

        let cw = img.rotate_90_cw();
        assert_eq!((cw.width, cw.height), (3, 2));
        // Top-left after CW rotation is the old bottom-left pixel.
        assert_eq!(&cw.rgb_pixels[0..3], &[44, 55, 66]);

        let full_rot = img
            .rotate_90_cw()
            .rotate_90_cw()
            .rotate_90_cw()
            .rotate_90_cw();
        assert_eq!(full_rot.rgb_pixels, img.rgb_pixels);

        let back = img.rotate_90_cw().rotate_90_ccw();
        assert_eq!(back.rgb_pixels, img.rgb_pixels);

        let rot180_twice = img.rotate_180().rotate_180();
        assert_eq!(rot180_twice.rgb_pixels, img.rgb_pixels);
        assert_eq!(
            img.rotate_180().rgb_pixels,
            img.rotate_90_cw().rotate_90_cw().rgb_pixels
        );

        // Must match the image crate's rotations used for files on disk.
        let dynimg = DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(2, 3, img.rgb_pixels.clone()).unwrap(),
        );
        assert_eq!(dynimg.rotate90().to_rgb8().into_raw(), cw.rgb_pixels);
        assert_eq!(
            dynimg.rotate270().to_rgb8().into_raw(),
            img.rotate_90_ccw().rgb_pixels
        );
    }

    #[test]
    fn test_decoded_image_crop() {
        let img = sample(
            3,
            3,
            vec![
                1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 5, 6, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 9,
            ],
        );
        let cropped = img.crop(1, 1, 1, 1);
        assert_eq!((cropped.width, cropped.height), (1, 1));
        assert_eq!(cropped.rgb_pixels, vec![5, 5, 5]);

        let top_left = img.crop(0, 0, 2, 2);
        assert_eq!(
            top_left.rgb_pixels,
            vec![1, 1, 1, 2, 2, 2, 4, 4, 4, 5, 5, 5]
        );

        // Out-of-range sizes are clamped instead of overflowing.
        let clamped = img.crop(2, 2, u32::MAX, u32::MAX);
        assert_eq!((clamped.width, clamped.height), (1, 1));
        assert_eq!(clamped.rgb_pixels, vec![9, 9, 9]);
    }

    #[test]
    fn corrupt_file_does_not_panic() {
        let dir = std::env::temp_dir().join(format!("mriv-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["bad.jpg", "bad.png", "bad.cr2", "bad.nef", "bad.tif"] {
            let p = dir.join(name);
            std::fs::write(
                &p,
                [0xFFu8, 0xD8, 0xFF, 0xE1, 0x00, 0x02, 1, 2, 3, 4, 5, 6, 7],
            )
            .unwrap();
            assert!(load_image(&p).is_err());
            let _ = load_thumbnail_rgba(&p, None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gray_icc_profile_is_not_used_as_rgb() {
        let mut gray = crate::color_manager::make_matrix_trc_icc(
            "Gray Test",
            (0.4361, 0.2225, 0.0139),
            (0.3851, 0.7169, 0.0971),
            (0.1431, 0.0606, 0.7141),
            2.2,
            false,
        );
        gray[16..20].copy_from_slice(b"GRAY");
        let (usable, name, note) = classify_icc(Some(gray));
        assert!(usable.is_none() && name.is_none());
        assert!(note.unwrap().contains("GRAY"));
    }
}
