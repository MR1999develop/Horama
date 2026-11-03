//! Fast Canon CR3 (ISOBMFF), Fujifilm RAF and general RAW embedded JPEG preview extractor.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The top-level `uuid` box of a CR3 that holds the PRVW box (a JPEG of about 1620×1080),
/// after 8 bytes of its own.
const CANON_PRVW_UUID: [u8; 16] = [
    0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16,
];

pub fn is_cr3_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && ext.eq_ignore_ascii_case("cr3")
    {
        return true;
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

/// The JPEG preview of a Fujifilm RAF, where its header says it is (big-endian offset and
/// length at bytes 84 and 88). A RAF holds only this preview, at full size.
pub fn extract_raf_preview(path: &Path) -> Option<Vec<u8>> {
    let mut f = File::open(path).ok()?;
    let mut header = [0u8; 92];
    f.read_exact(&mut header).ok()?;
    if !header.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let offset = u32::from_be_bytes(header[84..88].try_into().ok()?);
    let len = u32::from_be_bytes(header[88..92].try_into().ok()?);
    let file_size = f.metadata().ok()?.len();
    if len < 4 || u64::from(offset) + u64::from(len) > file_size {
        return None;
    }
    f.seek(SeekFrom::Start(u64::from(offset))).ok()?;
    let mut jpeg = vec![0u8; len as usize];
    f.read_exact(&mut jpeg).ok()?;
    let end = jpeg_stream_end(&jpeg, 0)?;
    jpeg.truncate(end);
    Some(jpeg)
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
            let skip = if uuid == CANON_PRVW_UUID { 16 + 8 } else { 16 };
            if data_size >= skip
                && let Some(res) =
                    find_preview_in_boxes(f, data_pos + skip, data_pos + data_size, depth + 1)
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

/// Previews live near the start of RAW files; the search reads at most this much.
const PREVIEW_SEARCH_LIMIT: u64 = 64 * 1024 * 1024;

/// Finds embedded JPEG previews (at least 640×480) in a RAW file, largest first.
/// At most `max` candidates are returned.
pub fn extract_embedded_jpegs(path: &Path, max: usize) -> Vec<Vec<u8>> {
    // A RAF says where its only preview is: nothing to search.
    if let Some(raf) = extract_raf_preview(path) {
        return vec![raf];
    }
    let Some(buf) = read_start(path, PREVIEW_SEARCH_LIMIT) else {
        return Vec::new();
    };
    let mut found = scan_jpegs(&buf);
    found.retain(|j| j.width >= 640 && j.height >= 480);
    found.sort_by_key(|j| std::cmp::Reverse(j.pixels()));
    found
        .into_iter()
        .take(max)
        .map(|j| buf[j.start..j.end].to_vec())
        .collect()
}

/// The smallest embedded JPEG preview of a RAW file with at least `long`×`short` pixels (in
/// either orientation): what a thumbnail needs, decoded many times faster than the full-size
/// preview. The CR3 PRVW box and the RAF preview are read directly; otherwise the start of the
/// file is searched, then more of it only if nothing large enough turns up.
pub fn extract_preview_at_least(path: &Path, long: u32, short: u32) -> Option<Vec<u8>> {
    if let Some(raf) = extract_raf_preview(path) {
        return Some(raf);
    }
    if is_cr3_file(path)
        && let Some(prvw) = extract_cr3_preview(path)
    {
        return Some(prvw);
    }
    for limit in [1024 * 1024, 16 * 1024 * 1024, PREVIEW_SEARCH_LIMIT] {
        let buf = read_start(path, limit)?;
        let smallest = scan_jpegs(&buf)
            .into_iter()
            .filter(|j| j.width.max(j.height) >= long && j.width.min(j.height) >= short)
            .min_by_key(|j| j.pixels());
        if let Some(j) = smallest {
            return Some(buf[j.start..j.end].to_vec());
        }
        if (buf.len() as u64) < limit {
            break; // the whole file was searched
        }
    }
    None
}

/// The first `limit` bytes of a file (at most [`PREVIEW_SEARCH_LIMIT`]).
fn read_start(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    File::open(path)
        .ok()?
        .take(limit.min(PREVIEW_SEARCH_LIMIT))
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

/// A complete JPEG stream found in a buffer.
struct FoundJpeg {
    width: u32,
    height: u32,
    start: usize,
    end: usize,
}

impl FoundJpeg {
    fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// Every complete JPEG stream in `buf` whose size can be read, in file order.
fn scan_jpegs(buf: &[u8]) -> Vec<FoundJpeg> {
    let mut found = Vec::new();
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
        if matches!(next_byte, 0xE0 | 0xE1 | 0xE2 | 0xDB | 0xEE | 0xC0 | 0xC4)
            && let Some(end) = jpeg_stream_end(buf, start)
        {
            let candidate = &buf[start..end];
            if let Some((width, height)) = image::ImageReader::new(std::io::Cursor::new(candidate))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok())
            {
                found.push(FoundJpeg {
                    width,
                    height,
                    start,
                    end,
                });
            }
            search_idx = end;
            continue;
        }
        search_idx = start + 3;
    }
    found
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
    fn raf_preview_is_read_where_the_header_says() {
        let dir = std::env::temp_dir().join(format!("mriv-raf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("fake.raf");
        let preview = jpeg_bytes(800, 600);
        let mut data = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        data.resize(84, 0);
        data.extend_from_slice(&200u32.to_be_bytes());
        // The declared length may run past the end of the stream: it is cut at its EOI.
        data.extend_from_slice(&(preview.len() as u32 + 10).to_be_bytes());
        data.resize(200, 0);
        data.extend_from_slice(&preview);
        data.extend_from_slice(&[0u8; 10]);
        // A larger JPEG later in the file is not the preview and is not searched for.
        data.extend_from_slice(&jpeg_bytes(1200, 800));
        std::fs::write(&p, &data).unwrap();
        assert_eq!(extract_embedded_jpegs(&p, 3), vec![preview]);
        // A header pointing past the end of the file falls back to the search.
        data[84..88].copy_from_slice(&u32::MAX.to_be_bytes());
        std::fs::write(&p, &data).unwrap();
        assert_eq!(extract_embedded_jpegs(&p, 3).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// An ISOBMFF box: big-endian size, type, data.
    fn mp4_box(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut b = ((data.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn cr3_prvw_preview_is_found_for_the_thumbnail() {
        let dir = std::env::temp_dir().join(format!("mriv-prvw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("fake.cr3");
        let prvw = jpeg_bytes(400, 300);
        let full = jpeg_bytes(1600, 1200);
        // The layout of a Canon CR3: ftyp, moov, uuid { 8 bytes, PRVW { 16 bytes, JPEG } }, mdat.
        let mut prvw_data = vec![0u8; 16];
        prvw_data.extend_from_slice(&prvw);
        let mut uuid_data = CANON_PRVW_UUID.to_vec();
        uuid_data.extend_from_slice(&[0u8; 8]);
        uuid_data.extend_from_slice(&mp4_box(b"PRVW", &prvw_data));
        let mut data = mp4_box(b"ftyp", b"crx \0\0\0\x01crx isom");
        data.extend_from_slice(&mp4_box(b"moov", &[0u8; 40]));
        data.extend_from_slice(&mp4_box(b"uuid", &uuid_data));
        data.extend_from_slice(&mp4_box(b"mdat", &full));
        std::fs::write(&p, &data).unwrap();
        assert_eq!(extract_cr3_preview(&p), Some(prvw.clone()));
        assert_eq!(extract_preview_at_least(&p, 280, 200), Some(prvw));
        // The main image still comes from the full-size preview.
        assert_eq!(extract_embedded_jpegs(&p, 1), vec![full]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thumbnail_preview_is_the_smallest_large_enough() {
        let dir = std::env::temp_dir().join(format!("mriv-small-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("fake.arw");
        let portrait = jpeg_bytes(300, 400);
        let mut data = vec![0x11u8; 2000];
        for jpeg in [
            jpeg_bytes(160, 120),
            jpeg_bytes(1600, 1200),
            portrait.clone(),
            jpeg_bytes(640, 480),
        ] {
            data.extend_from_slice(&jpeg);
            data.extend_from_slice(&[0x22u8; 500]);
        }
        std::fs::write(&p, &data).unwrap();
        // 160×120 is too small; a portrait 300×400 is enough for 280×200.
        assert_eq!(extract_preview_at_least(&p, 280, 200), Some(portrait));
        assert_eq!(extract_preview_at_least(&p, 4000, 3000), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
