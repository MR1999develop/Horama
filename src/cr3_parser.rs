//! Fast Canon CR3 (ISOBMFF) and general RAW embedded JPEG preview extractor.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const CANON_PRVW_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x02, 0x4e,
];

pub fn is_cr3_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("cr3") {
            return true;
        }
    }
    if let Ok(mut f) = File::open(path) {
        let mut header = [0u8; 16];
        if f.read_exact(&mut header).is_ok()
            && &header[4..8] == b"ftyp"
            && &header[8..12] == b"crx "
        {
            return true;
        }
    }
    false
}

pub fn extract_cr3_preview(path: &Path) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    let file_size = f.metadata().ok()?.len();

    let mut box_header = [0u8; 8];
    f.read_exact(&mut box_header).ok()?;
    let size = u32::from_be_bytes(box_header[0..4].try_into().ok()?) as u64;
    if &box_header[4..8] != b"ftyp" {
        return None;
    }

    let first_pos = if size > 0 { size } else { 8 };
    find_preview_in_boxes(&mut f, first_pos, file_size, 0)
}

fn find_preview_in_boxes(
    f: &mut File,
    start_pos: u64,
    end_pos: u64,
    depth: usize,
) -> Option<Vec<u8>> {
    if depth > 8 {
        return None;
    }

    let mut pos = start_pos;
    while pos + 8 <= end_pos {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut header = [0u8; 8];
        if f.read_exact(&mut header).is_err() {
            break;
        }
        let mut box_size = u32::from_be_bytes(header[0..4].try_into().ok()?) as u64;
        let box_type = [header[4], header[5], header[6], header[7]];

        let header_len = if box_size == 1 {
            let mut large = [0u8; 8];
            f.read_exact(&mut large).ok()?;
            box_size = u64::from_be_bytes(large);
            16u64
        } else if box_size == 0 {
            box_size = end_pos - pos;
            8u64
        } else {
            8u64
        };

        if box_size < header_len || pos + box_size > end_pos {
            break;
        }

        let data_pos = pos + header_len;
        let data_size = box_size - header_len;

        if &box_type == b"uuid" && data_size >= 16 {
            let mut uuid = [0u8; 16];
            f.read_exact(&mut uuid).ok()?;
            if uuid == CANON_PRVW_UUID && data_size >= 36 {
                let mut prvw_hdr = [0u8; 20];
                f.read_exact(&mut prvw_hdr).ok()?;
                let jpeg_len = u32::from_be_bytes(prvw_hdr[16..20].try_into().ok()?) as u64;
                if jpeg_len > 0 && jpeg_len <= data_size - 36 {
                    let mut jpeg_bytes = vec![0u8; jpeg_len as usize];
                    f.read_exact(&mut jpeg_bytes).ok()?;
                    if jpeg_bytes.starts_with(&[0xFF, 0xD8]) {
                        return Some(jpeg_bytes);
                    }
                }
            }
            if let Some(res) =
                find_preview_in_boxes(f, data_pos + 16, data_pos + data_size, depth + 1)
            {
                return Some(res);
            }
        } else if matches!(&box_type, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl") {
            if let Some(res) = find_preview_in_boxes(f, data_pos, data_pos + data_size, depth + 1) {
                return Some(res);
            }
        } else if &box_type == b"PRVW" && data_size < 50_000_000 {
            let mut prvw_data = vec![0u8; data_size as usize];
            f.read_exact(&mut prvw_data).ok()?;
            if let Some(idx) = prvw_data.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) {
                return Some(prvw_data[idx..].to_vec());
            }
        }

        pos += box_size;
    }

    None
}

/// Collects the EXIF TIFF structures stored by Canon in the CMT1 (IFD0), CMT2 (Exif IFD)
/// and CMT4 (GPS) boxes of a CR3 file.
pub fn extract_cr3_exif_blocks(path: &Path) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Ok(mut f) = File::open(path) else {
        return out;
    };
    let Ok(meta) = f.metadata() else { return out };
    collect_cmt_boxes(&mut f, 0, meta.len(), 0, &mut out);
    out
}

fn collect_cmt_boxes(f: &mut File, start: u64, end: u64, depth: usize, out: &mut Vec<Vec<u8>>) {
    if depth > 6 || out.len() >= 3 {
        return;
    }
    let mut pos = start;
    while pos + 8 <= end {
        if f.seek(SeekFrom::Start(pos)).is_err() {
            return;
        }
        let mut header = [0u8; 8];
        if f.read_exact(&mut header).is_err() {
            return;
        }
        let mut size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let kind = [header[4], header[5], header[6], header[7]];
        let mut hdr_len = 8u64;
        if size == 1 {
            let mut large = [0u8; 8];
            if f.read_exact(&mut large).is_err() {
                return;
            }
            size = u64::from_be_bytes(large);
            hdr_len = 16;
        } else if size == 0 {
            size = end - pos;
        }
        if size < hdr_len || pos + size > end {
            return;
        }
        let data_start = pos + hdr_len;
        let data_len = size - hdr_len;
        match &kind {
            b"CMT1" | b"CMT2" | b"CMT4" if data_len < 16 * 1024 * 1024 => {
                let mut blob = vec![0u8; data_len as usize];
                if f.read_exact(&mut blob).is_ok()
                    && (blob.starts_with(b"II*\0") || blob.starts_with(b"MM\0*"))
                {
                    out.push(blob);
                }
            }
            b"moov" => collect_cmt_boxes(f, data_start, data_start + data_len, depth + 1, out),
            b"uuid" if data_len > 16 => {
                collect_cmt_boxes(f, data_start + 16, data_start + data_len, depth + 1, out)
            }
            _ => {}
        }
        pos += size;
    }
}

/// Returns the end offset (one past EOI) of the JPEG stream starting at `start` (which must
/// point at an SOI marker), by walking the marker structure. Embedded thumbnails inside APP
/// segments are skipped by segment length, so their EOI markers are never mistaken for the end.
pub fn jpeg_stream_end(buf: &[u8], start: usize) -> Option<usize> {
    if buf.get(start..start + 2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut pos = start + 2;
    loop {
        // Expect a marker.
        if *buf.get(pos)? != 0xFF {
            return None;
        }
        while *buf.get(pos + 1)? == 0xFF {
            pos += 1;
        }
        let marker = *buf.get(pos + 1)?;
        match marker {
            0xD9 => return Some(pos + 2),
            0x01 | 0xD0..=0xD7 => pos += 2,
            0xDA => {
                let len = u16::from_be_bytes([*buf.get(pos + 2)?, *buf.get(pos + 3)?]) as usize;
                pos += 2 + len;
                // Entropy-coded data: 0xFF is followed by 0x00 (stuffing) or RSTn inside the scan.
                loop {
                    let rel = buf.get(pos..)?.iter().position(|&b| b == 0xFF)?;
                    pos += rel;
                    let next = *buf.get(pos + 1)?;
                    if next == 0x00 || (0xD0..=0xD7).contains(&next) || next == 0xFF {
                        pos += if next == 0xFF { 1 } else { 2 };
                        continue;
                    }
                    break; // a real marker: continue the outer loop (next scan, DHT, EOI...)
                }
            }
            _ => {
                let len = u16::from_be_bytes([*buf.get(pos + 2)?, *buf.get(pos + 3)?]) as usize;
                if len < 2 {
                    return None;
                }
                pos += 2 + len;
            }
        }
    }
}

/// Finds embedded JPEG previews (at least 640×480) in a RAW file, largest first.
/// At most `max` candidates are returned. For CR3 files the PRVW box is tried first.
pub fn extract_embedded_jpegs(path: &Path, max: usize) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    if is_cr3_file(path) {
        if let Some(cr3) = extract_cr3_preview(path) {
            out.push(cr3);
        }
    }

    let Ok(mut f) = File::open(path) else {
        return out;
    };
    let Ok(meta) = f.metadata() else { return out };
    // Previews live near the start of RAW files; cap the read at 64 MB.
    let read_limit = (meta.len() as usize).min(64 * 1024 * 1024);
    let mut buf = vec![0u8; read_limit];
    if f.read_exact(&mut buf).is_err() {
        return out;
    }

    let mut found: Vec<(u64, usize, usize)> = Vec::new();
    let mut search_idx = 0;
    while search_idx + 4 < buf.len() {
        let Some(rel) = buf[search_idx..]
            .windows(3)
            .position(|w| w[0] == 0xFF && w[1] == 0xD8 && w[2] == 0xFF)
        else {
            break;
        };
        let start = search_idx + rel;
        let next_byte = buf.get(start + 3).copied().unwrap_or(0);
        if matches!(next_byte, 0xE0 | 0xE1 | 0xE2 | 0xDB | 0xEE | 0xC0 | 0xC4) {
            if let Some(end) = jpeg_stream_end(&buf, start) {
                let candidate = &buf[start..end];
                if let Some((w, h)) = image::ImageReader::new(std::io::Cursor::new(candidate))
                    .with_guessed_format()
                    .ok()
                    .and_then(|r| r.into_dimensions().ok())
                {
                    if w >= 640 && h >= 480 {
                        found.push((w as u64 * h as u64, start, end));
                    }
                }
                search_idx = end;
                continue;
            }
        }
        search_idx = start + 3;
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, s, e) in found {
        if out.len() >= max {
            break;
        }
        out.push(buf[s..e].to_vec());
    }
    out
}

/// Largest embedded JPEG preview of a RAW file (see [`extract_embedded_jpegs`]).
pub fn extract_largest_embedded_jpeg(path: &Path) -> Option<Vec<u8>> {
    extract_embedded_jpegs(path, 1).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 77])
        }));
        let mut v = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut v), image::ImageFormat::Jpeg)
            .unwrap();
        v
    }

    #[test]
    fn stream_end_skips_embedded_thumbnail() {
        // Large JPEG whose APP1 contains a complete small JPEG (fake EXIF thumbnail).
        let big = jpeg_bytes(700, 500);
        let thumb = jpeg_bytes(16, 16);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&thumb);
        let mut composed = big[..2].to_vec();
        composed.extend_from_slice(&[0xFF, 0xE1]);
        composed.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        composed.extend_from_slice(&app1);
        composed.extend_from_slice(&big[2..]);
        assert_eq!(jpeg_stream_end(&composed, 0), Some(composed.len()));
    }

    #[test]
    fn scanner_finds_largest_preview_in_container() {
        let dir = std::env::temp_dir().join(format!("mriv-cr3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("fake.nef");
        let mut data = vec![0x11u8; 5000];
        data.extend_from_slice(&jpeg_bytes(800, 600));
        data.extend_from_slice(&[0x22u8; 3000]);
        let big = jpeg_bytes(1200, 800);
        data.extend_from_slice(&big);
        data.extend_from_slice(&[0x33u8; 100]);
        std::fs::write(&p, &data).unwrap();
        let found = extract_embedded_jpegs(&p, 3);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], big);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
