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
        if f.read_exact(&mut header).is_ok() {
            if &header[4..8] == b"ftyp" && &header[8..12] == b"crx " {
                return true;
            }
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
        } else if matches!(
            &box_type,
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl"
        ) {
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

/// Fast scanner for embedded full-size JPEG previews inside NEF/ARW/RAF/ORF/CR2 RAW files.
pub fn extract_largest_embedded_jpeg(path: &Path) -> Option<Vec<u8>> {
    if is_cr3_file(path) {
        if let Some(cr3) = extract_cr3_preview(path) {
            return Some(cr3);
        }
    }

    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    // Read up to 64 MB to find embedded preview JPEG streams
    let read_limit = len.min(64 * 1024 * 1024);
    let mut buf = vec![0u8; read_limit];
    f.read_exact(&mut buf).ok()?;

    let mut best_jpeg: Option<&[u8]> = None;
    let mut search_idx = 0;

    while search_idx + 4 < buf.len() {
        // Look for JPEG SOI + APP1/APP0/DQT marker: FF D8 FF (E0 | E1 | E2 | DB)
        if let Some(rel) = buf[search_idx..]
            .windows(3)
            .position(|w| w[0] == 0xFF && w[1] == 0xD8 && w[2] == 0xFF)
        {
            let start = search_idx + rel;
            let next_byte = buf.get(start + 3).copied().unwrap_or(0);
            if matches!(next_byte, 0xE0 | 0xE1 | 0xE2 | 0xDB | 0xEE) {
                // Search for matching EOI (FF D9) after at least 32 KB for a high-res preview
                let min_end = (start + 32_768).min(buf.len());
                if let Some(end_rel) = buf[min_end..]
                    .windows(2)
                    .position(|w| w[0] == 0xFF && w[1] == 0xD9)
                {
                    let end = min_end + end_rel + 2;
                    let candidate = &buf[start..end];
                    if best_jpeg.map_or(true, |b| candidate.len() > b.len()) {
                        // Quick check that image crate can parse header
                        if image::ImageReader::new(std::io::Cursor::new(candidate))
                            .with_guessed_format()
                            .ok()
                            .and_then(|r| r.into_dimensions().ok())
                            .map_or(false, |(w, h)| w >= 640 && h >= 480)
                        {
                            best_jpeg = Some(candidate);
                        }
                    }
                    search_idx = end;
                    continue;
                }
            }
            search_idx = start + 3;
        } else {
            break;
        }
    }

    best_jpeg.map(|s| s.to_vec())
}
