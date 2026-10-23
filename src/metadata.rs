//! EXIF metadata extraction and 256-bin RGB + Luminance histogram rendering.

use crate::image_loader::DecodedImage;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a.max(1)
}

fn extract_exif_tags_into(exif_data: &exif::Exif, rows: &mut Vec<(String, String)>) {
    let important_tags = [
        (exif::Tag::Make, "Camera Make"),
        (exif::Tag::Model, "Camera Model"),
        (exif::Tag::LensModel, "Lens"),
        (exif::Tag::DateTimeOriginal, "Date Taken"),
        (exif::Tag::ExposureTime, "Shutter Speed"),
        (exif::Tag::FNumber, "Aperture"),
        (exif::Tag::PhotographicSensitivity, "ISO"),
        (exif::Tag::FocalLength, "Focal Length"),
        (exif::Tag::FocalLengthIn35mmFilm, "Focal Length (35mm eq)"),
        (exif::Tag::ExposureBiasValue, "Exposure Bias"),
        (exif::Tag::ExposureProgram, "Exposure Program"),
        (exif::Tag::WhiteBalance, "White Balance"),
        (exif::Tag::MeteringMode, "Metering Mode"),
        (exif::Tag::Flash, "Flash"),
        (exif::Tag::Software, "Software"),
    ];

    for (tag, label) in &important_tags {
        if rows.iter().any(|(k, _)| k == *label) {
            continue;
        }
        for field in exif_data.fields() {
            if field.tag == *tag {
                let val = field
                    .display_value()
                    .with_unit(exif_data)
                    .to_string()
                    .trim_matches('"')
                    .trim()
                    .to_string();
                if !val.is_empty() {
                    rows.push((label.to_string(), val));
                    break;
                }
            }
        }
    }
}

pub fn extract_metadata(decoded: &DecodedImage) -> Vec<(String, String)> {
    let path = Path::new(&decoded.path);
    let mut rows = Vec::new();

    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| decoded.path.clone());
    rows.push(("File Name".to_string(), file_name));

    if let Some(parent) = path.parent() {
        rows.push(("Folder".to_string(), parent.to_string_lossy().to_string()));
    }

    rows.push((
        "Format".to_string(),
        if decoded.is_raw {
            format!("{} [Camera RAW]", decoded.format_name)
        } else {
            decoded.format_name.clone()
        },
    ));
    rows.push((
        "Dimensions".to_string(),
        format!("{} × {} px", decoded.width, decoded.height),
    ));

    let mp = (decoded.width as f64 * decoded.height as f64) / 1_000_000.0;
    let d = gcd(decoded.width, decoded.height);
    rows.push((
        "Megapixels".to_string(),
        format!(
            "{:.2} MP ({}:{})",
            mp,
            decoded.width / d,
            decoded.height / d
        ),
    ));

    rows.push((
        "Bit Depth".to_string(),
        format!("{}-bit per channel", decoded.bit_depth),
    ));
    rows.push(("File Size".to_string(), format_file_size(decoded.file_size)));
    rows.push((
        "Embedded ICC".to_string(),
        decoded
            .embedded_icc_name
            .clone()
            .unwrap_or_else(|| "None (Untagged)".to_string()),
    ));

    // 1. Direct container EXIF read (for JPEG/TIFF/DNG/RAW)
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        if let Ok(exif_data) = exif::Reader::new().read_from_container(&mut reader) {
            extract_exif_tags_into(&exif_data, &mut rows);
        } else {
            use std::io::{Read, Seek};
            if reader.seek(std::io::SeekFrom::Start(0)).is_ok() {
                let mut raw_buf = Vec::new();
                if reader.read_to_end(&mut raw_buf).is_ok() {
                    if let Ok(exif_data) = exif::Reader::new().read_raw(raw_buf) {
                        extract_exif_tags_into(&exif_data, &mut rows);
                    }
                }
            }
        }
    }

    // 2. Comprehensive RAW EXIF Fallback: Extract from embedded preview JPEG
    // Many camera RAW containers (CR3, CR2, NEF, ARW, RAF, ORF) store complete shooting
    // metadata (ISO, Shutter, Aperture, Lens, Focal Length) in the embedded JPEG APP1 EXIF segment.
    if decoded.is_raw {
        if let Some(jpeg_bytes) = crate::cr3_parser::extract_largest_embedded_jpeg(path) {
            let mut cur = std::io::Cursor::new(jpeg_bytes);
            if let Ok(exif_data) = exif::Reader::new().read_from_container(&mut cur) {
                extract_exif_tags_into(&exif_data, &mut rows);
            }
        }
    }

    // 3. Fallback: rawloader clean_make and clean_model if still missing
    if decoded.is_raw {
        let has_make = rows.iter().any(|(k, _)| k == "Camera Make");
        let has_model = rows.iter().any(|(k, _)| k == "Camera Model");
        if !has_make || !has_model {
            if let Ok(raw_img) = rawloader::decode_file(path) {
                if !has_make && !raw_img.clean_make.is_empty() {
                    rows.push(("Camera Make".to_string(), raw_img.clean_make));
                }
                if !has_model && !raw_img.clean_model.is_empty() {
                    rows.push(("Camera Model".to_string(), raw_img.clean_model));
                }
            }
        }
    }

    rows
}

pub fn render_histogram_rgba(rgb_pixels: &[u8]) -> (u32, u32, Vec<u8>) {
    let w = 268u32;
    let h = 130u32;
    let mut rgba = vec![0u8; (w * h * 4) as usize];

    // Background #141414
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 0x14;
        px[1] = 0x14;
        px[2] = 0x14;
        px[3] = 0xFF;
    }

    // Subtle grid lines (#262626)
    for gx in [67u32, 134, 201] {
        for y in 0..h {
            let idx = ((y * w + gx) * 4) as usize;
            rgba[idx] = 0x26;
            rgba[idx + 1] = 0x26;
            rgba[idx + 2] = 0x26;
        }
    }
    for gy in [32u32, 65, 97] {
        for x in 0..w {
            let idx = ((gy * w + x) * 4) as usize;
            rgba[idx] = 0x26;
            rgba[idx + 1] = 0x26;
            rgba[idx + 2] = 0x26;
        }
    }

    if rgb_pixels.len() < 3 {
        return (w, h, rgba);
    }

    let mut r_bins = [0u32; 256];
    let mut g_bins = [0u32; 256];
    let mut b_bins = [0u32; 256];
    let mut y_bins = [0u32; 256];

    // Sample with step if huge image for instant responsiveness
    let total_px = rgb_pixels.len() / 3;
    let step = (total_px / 250_000).max(1);

    for i in (0..total_px).step_by(step) {
        let base = i * 3;
        let r = rgb_pixels[base] as usize;
        let g = rgb_pixels[base + 1] as usize;
        let b = rgb_pixels[base + 2] as usize;
        let lum = ((r * 77 + g * 150 + b * 29) >> 8).min(255);

        r_bins[r] += 1;
        g_bins[g] += 1;
        b_bins[b] += 1;
        y_bins[lum] += 1;
    }

    // Find 99th-percentile peak to avoid extreme clipping spikes flattening the histogram
    let mut all_vals: Vec<u32> = Vec::with_capacity(768);
    all_vals.extend_from_slice(&r_bins[1..255]);
    all_vals.extend_from_slice(&g_bins[1..255]);
    all_vals.extend_from_slice(&b_bins[1..255]);
    all_vals.sort_unstable();
    let peak = all_vals
        .get(all_vals.len().saturating_sub(8))
        .copied()
        .unwrap_or(1)
        .max(1) as f32;

    let pad_x = 6u32;
    let plot_h = (h - 10) as f32;

    let blend_pixel = |buf: &mut [u8], x: u32, y: u32, cr: u8, cg: u8, cb: u8, alpha: f32| {
        if x >= w || y >= h {
            return;
        }
        let idx = ((y * w + x) * 4) as usize;
        let inv = 1.0 - alpha;
        buf[idx] = ((buf[idx] as f32 * inv) + (cr as f32 * alpha)).clamp(0.0, 255.0) as u8;
        buf[idx + 1] = ((buf[idx + 1] as f32 * inv) + (cg as f32 * alpha)).clamp(0.0, 255.0) as u8;
        buf[idx + 2] = ((buf[idx + 2] as f32 * inv) + (cb as f32 * alpha)).clamp(0.0, 255.0) as u8;
    };

    let channels: [(&[u32; 256], (u8, u8, u8), f32); 4] = [
        (&y_bins, (215, 215, 215), 0.18),
        (&r_bins, (255, 75, 75), 0.30),
        (&g_bins, (75, 235, 110), 0.28),
        (&b_bins, (75, 165, 255), 0.32),
    ];

    for (bins, (cr, cg, cb), fill_alpha) in channels {
        let mut prev_y = h - 4;
        for bin in 0..256u32 {
            let x = pad_x + bin;
            let norm = (bins[bin as usize] as f32 / peak).clamp(0.0, 1.0);
            let bar_h = (norm * plot_h).round() as u32;
            let top_y = (h - 4).saturating_sub(bar_h);

            for y in top_y..(h - 4) {
                blend_pixel(&mut rgba, x, y, cr, cg, cb, fill_alpha);
            }

            // Draw crisp top curve line
            let y_min = top_y.min(prev_y);
            let y_max = top_y.max(prev_y);
            for y in y_min..=y_max {
                blend_pixel(&mut rgba, x, y, cr, cg, cb, 0.90);
            }
            prev_y = top_y;
        }
    }

    (w, h, rgba)
}
