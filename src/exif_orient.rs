//! Lossless orientation handling.
//!
//! * EXIF orientation algebra (the 8 EXIF orientations form the dihedral group D4).
//! * In-place patching of the TIFF `Orientation` tag (0x0112) inside raw TIFF structures
//!   (TIFF files and the EXIF block of JPEG files), without touching image data.
//! * JPEG segment handling to read/replace the APP1 EXIF block byte-for-byte.
//! * XMP `tiff:Orientation` patching (same-length, in place) so Adobe software agrees.

pub const TAG_ORIENTATION: u16 = 0x0112;
const EXIF_PREFIX: &[u8] = b"Exif\0\0";

type Mat = [[i8; 2]; 2];

const I: Mat = [[1, 0], [0, 1]];
/// 90° clockwise rotation in y-down image coordinates: (x, y) -> (-y, x).
const R: Mat = [[0, -1], [1, 0]];
const H: Mat = [[-1, 0], [0, 1]];
const V: Mat = [[1, 0], [0, -1]];

fn mul(a: Mat, b: Mat) -> Mat {
    let mut m = [[0i8; 2]; 2];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j];
        }
    }
    m
}

/// Transform (stored -> displayed) for an EXIF orientation value.
/// Mirrors `image_loader::apply_orientation` exactly.
fn orientation_matrix(o: u16) -> Mat {
    let r2 = mul(R, R);
    let r3 = mul(r2, R);
    match o {
        2 => H,
        3 => r2,
        4 => V,
        5 => mul(H, R),
        6 => R,
        7 => mul(H, r3),
        8 => r3,
        _ => I,
    }
}

/// Returns the EXIF orientation obtained by rotating an image that currently displays with
/// orientation `o` by `quarter_turns_cw` additional 90° clockwise steps.
pub fn compose_rotation(o: u16, quarter_turns_cw: u8) -> u16 {
    let mut m = orientation_matrix(o);
    for _ in 0..(quarter_turns_cw % 4) {
        m = mul(R, m);
    }
    (1..=8).find(|&c| orientation_matrix(c) == m).unwrap_or(1)
}

#[derive(Clone, Copy)]
struct Endian {
    le: bool,
}

impl Endian {
    fn u16(&self, b: &[u8], at: usize) -> Option<u16> {
        let s: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(s)
        } else {
            u16::from_be_bytes(s)
        })
    }
    fn u32(&self, b: &[u8], at: usize) -> Option<u32> {
        let s: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(s)
        } else {
            u32::from_be_bytes(s)
        })
    }
    fn put_u16(&self, b: &mut [u8], at: usize, v: u16) {
        let bytes = if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        b[at..at + 2].copy_from_slice(&bytes);
    }
    fn put_u32(&self, b: &mut [u8], at: usize, v: u32) {
        let bytes = if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        b[at..at + 4].copy_from_slice(&bytes);
    }
    fn u16_bytes(&self, v: u16) -> [u8; 2] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
    fn u32_bytes(&self, v: u32) -> [u8; 4] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
}

fn tiff_header(buf: &[u8], base: usize) -> Result<(Endian, usize), String> {
    let hdr = buf.get(base..base + 8).ok_or("TIFF header truncated")?;
    let e = match &hdr[0..2] {
        b"II" => Endian { le: true },
        b"MM" => Endian { le: false },
        _ => return Err("Not a TIFF structure".into()),
    };
    match e.u16(hdr, 2) {
        Some(42) => {}
        Some(43) => return Err("BigTIFF is not supported".into()),
        _ => return Err("Invalid TIFF magic number".into()),
    }
    let ifd0 = e.u32(hdr, 4).ok_or("TIFF header truncated")? as usize;
    Ok((e, ifd0))
}

/// Reads the orientation tag from IFD0 of a TIFF structure starting at `base`.
pub fn get_tiff_orientation(buf: &[u8], base: usize) -> Option<u16> {
    let (e, ifd0) = tiff_header(buf, base).ok()?;
    let abs = base.checked_add(ifd0)?;
    let count = e.u16(buf, abs)? as usize;
    for i in 0..count {
        let ent = abs + 2 + i * 12;
        if e.u16(buf, ent)? == TAG_ORIENTATION {
            let typ = e.u16(buf, ent + 2)?;
            return match typ {
                3 => e.u16(buf, ent + 8),
                4 => e.u32(buf, ent + 8).map(|v| v as u16),
                _ => None,
            }
            .filter(|v| (1..=8).contains(v));
        }
    }
    None
}

/// Sets IFD0's orientation tag in a TIFF structure starting at `base` (offsets are relative
/// to `base`). If the tag exists as a single SHORT it is overwritten in place; otherwise a new
/// copy of IFD0 containing the tag is appended at the end of `buf` and the header is redirected
/// to it. Image data and every other offset remain untouched, so the change is lossless.
pub fn set_tiff_orientation(
    buf: &mut Vec<u8>,
    base: usize,
    orientation: u16,
) -> Result<(), String> {
    if !(1..=8).contains(&orientation) {
        return Err(format!("Invalid orientation value {}", orientation));
    }
    let (e, ifd0) = tiff_header(buf, base)?;
    let abs = base.checked_add(ifd0).ok_or("Invalid IFD0 offset")?;
    let count = e.u16(buf, abs).ok_or("IFD0 truncated")? as usize;
    let entries_end = abs + 2 + count * 12;
    if entries_end + 4 > buf.len() {
        return Err("IFD0 truncated".into());
    }

    let mut entries: Vec<[u8; 12]> = Vec::with_capacity(count + 1);
    for i in 0..count {
        let ent = abs + 2 + i * 12;
        let tag = e.u16(buf, ent).unwrap_or(0);
        if tag == TAG_ORIENTATION {
            let typ = e.u16(buf, ent + 2).unwrap_or(0);
            let cnt = e.u32(buf, ent + 4).unwrap_or(0);
            if typ == 3 && cnt == 1 {
                e.put_u16(buf, ent + 8, orientation);
                e.put_u16(buf, ent + 10, 0);
                return Ok(());
            }
            continue; // malformed entry: drop it and rebuild below
        }
        let mut raw = [0u8; 12];
        raw.copy_from_slice(&buf[ent..ent + 12]);
        entries.push(raw);
    }

    let mut new_entry = [0u8; 12];
    new_entry[0..2].copy_from_slice(&e.u16_bytes(TAG_ORIENTATION));
    new_entry[2..4].copy_from_slice(&e.u16_bytes(3));
    new_entry[4..8].copy_from_slice(&e.u32_bytes(1));
    new_entry[8..10].copy_from_slice(&e.u16_bytes(orientation));
    entries.push(new_entry);
    entries.sort_by_key(|raw| e.u16(raw, 0).unwrap_or(0));

    let next_ifd = e.u32(buf, entries_end).unwrap_or(0);
    if buf.len() % 2 == 1 {
        buf.push(0);
    }
    let new_off = buf.len() - base;
    let new_off_u32 = u32::try_from(new_off).map_err(|_| "TIFF too large to patch".to_string())?;
    let n = u16::try_from(entries.len()).map_err(|_| "Too many IFD entries".to_string())?;
    buf.extend_from_slice(&e.u16_bytes(n));
    for raw in &entries {
        buf.extend_from_slice(raw);
    }
    buf.extend_from_slice(&e.u32_bytes(next_ifd));
    e.put_u32(buf, base + 4, new_off_u32);
    Ok(())
}

/// Builds a minimal little-endian TIFF/EXIF structure containing only the orientation tag.
pub fn minimal_exif_with_orientation(orientation: u16) -> Vec<u8> {
    let mut v = Vec::with_capacity(26);
    v.extend_from_slice(b"II");
    v.extend_from_slice(&42u16.to_le_bytes());
    v.extend_from_slice(&8u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&TAG_ORIENTATION.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&1u32.to_le_bytes());
    v.extend_from_slice(&orientation.to_le_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

/// A marker segment of a JPEG file header (everything before SOS).
#[derive(Debug, Clone, Copy)]
pub struct JpegSegment {
    pub marker: u8,
    /// Offset of the 0xFF that starts the marker.
    pub start: usize,
    /// Offset one past the end of the segment.
    pub end: usize,
}

impl JpegSegment {
    pub fn contents<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[(self.start + 4).min(self.end)..self.end]
    }
}

/// Lists the header segments of a JPEG file up to (not including) the SOS marker.
pub fn jpeg_header_segments(data: &[u8]) -> Result<Vec<JpegSegment>, String> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err("Not a JPEG file".into());
    }
    let mut segs = Vec::new();
    let mut pos = 2usize;
    loop {
        if pos >= data.len() {
            return Err("JPEG ended before image data".into());
        }
        if data[pos] != 0xFF {
            return Err("Corrupt JPEG marker structure".into());
        }
        let mut m = pos + 1;
        while m < data.len() && data[m] == 0xFF {
            m += 1;
        }
        let marker = *data.get(m).ok_or("JPEG truncated")?;
        let start = m - 1;
        match marker {
            0xDA | 0xD9 => return Ok(segs),
            0x01 | 0xD0..=0xD7 => {
                pos = m + 1;
            }
            _ => {
                let len = u16::from_be_bytes(
                    data.get(m + 1..m + 3)
                        .ok_or("JPEG truncated")?
                        .try_into()
                        .unwrap(),
                ) as usize;
                if len < 2 {
                    return Err("Invalid JPEG segment length".into());
                }
                let end = m + 1 + len;
                if end > data.len() {
                    return Err("JPEG segment exceeds file size".into());
                }
                segs.push(JpegSegment { marker, start, end });
                pos = end;
            }
        }
    }
}

fn find_exif_segment(data: &[u8], segs: &[JpegSegment]) -> Option<JpegSegment> {
    segs.iter()
        .copied()
        .find(|s| s.marker == 0xE1 && s.contents(data).starts_with(EXIF_PREFIX))
}

/// Reads the EXIF orientation of a JPEG file (1 when absent).
pub fn get_jpeg_orientation(data: &[u8]) -> u16 {
    let Ok(segs) = jpeg_header_segments(data) else {
        return 1;
    };
    let Some(seg) = find_exif_segment(data, &segs) else {
        return 1;
    };
    let tiff = &seg.contents(data)[EXIF_PREFIX.len()..];
    get_tiff_orientation(tiff, 0).unwrap_or(1)
}

/// Replaces (or inserts) the EXIF APP1 segment of a JPEG with `tiff` (raw TIFF structure).
/// The compressed image data is copied byte-for-byte.
pub fn replace_jpeg_exif(data: &[u8], tiff: &[u8]) -> Result<Vec<u8>, String> {
    let segs = jpeg_header_segments(data)?;
    let mut content = Vec::with_capacity(EXIF_PREFIX.len() + tiff.len());
    content.extend_from_slice(EXIF_PREFIX);
    content.extend_from_slice(tiff);
    let seg_len = content.len() + 2;
    if seg_len > 0xFFFF {
        return Err("EXIF block too large for a JPEG APP1 segment".into());
    }
    let (cut_start, cut_end) = match find_exif_segment(data, &segs) {
        Some(s) => (s.start, s.end),
        None => {
            // Insert after APP0 (JFIF) if it is the first segment, otherwise right after SOI.
            let at = match segs.first() {
                Some(s) if s.marker == 0xE0 => s.end,
                _ => 2,
            };
            (at, at)
        }
    };
    let mut out = Vec::with_capacity(data.len() + seg_len + 2);
    out.extend_from_slice(&data[..cut_start]);
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&(seg_len as u16).to_be_bytes());
    out.extend_from_slice(&content);
    out.extend_from_slice(&data[cut_end..]);
    Ok(out)
}

/// Sets the EXIF orientation of a JPEG file losslessly (only metadata bytes change).
pub fn set_jpeg_orientation(data: &[u8], orientation: u16) -> Result<Vec<u8>, String> {
    let segs = jpeg_header_segments(data)?;
    let mut out = match find_exif_segment(data, &segs) {
        Some(seg) => {
            let mut tiff = seg.contents(data)[EXIF_PREFIX.len()..].to_vec();
            set_tiff_orientation(&mut tiff, 0, orientation)?;
            replace_jpeg_exif(data, &tiff)?
        }
        None => {
            if orientation == 1 {
                data.to_vec()
            } else {
                replace_jpeg_exif(data, &minimal_exif_with_orientation(orientation))?
            }
        }
    };
    // Keep XMP (APP1 "http://ns.adobe.com/xap/1.0/") in agreement.
    if let Ok(segs) = jpeg_header_segments(&out) {
        for s in segs.iter().filter(|s| s.marker == 0xE1) {
            let (a, b) = (s.start + 4, s.end);
            if out[a..b].starts_with(b"http://ns.adobe.com/xap/1.0/") {
                patch_xmp_orientation(&mut out[a..b], orientation);
            }
        }
    }
    Ok(out)
}

/// Rewrites `tiff:Orientation` inside an XMP packet in place (single-digit values only,
/// so the packet length never changes). Returns true if something was changed.
pub fn patch_xmp_orientation(xmp: &mut [u8], orientation: u16) -> bool {
    if !(1..=8).contains(&orientation) {
        return false;
    }
    let digit = b'0' + orientation as u8;
    let mut changed = false;
    let key = b"tiff:Orientation";
    let mut i = 0;
    while i + key.len() < xmp.len() {
        if &xmp[i..i + key.len()] == key {
            let mut j = i + key.len();
            // attribute form: tiff:Orientation="6"   element form: <tiff:Orientation>6<
            while j < xmp.len() && matches!(xmp[j], b' ' | b'=' | b'"' | b'\'' | b'>') {
                j += 1;
            }
            if j < xmp.len()
                && xmp[j].is_ascii_digit()
                && !xmp.get(j + 1).is_some_and(|c| c.is_ascii_digit())
            {
                xmp[j] = digit;
                changed = true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    changed
}

/// Patches every XMP packet (`<x:xmpmeta ... </x:xmpmeta>`) found in `buf`.
pub fn patch_all_xmp_packets(buf: &mut [u8], orientation: u16) -> bool {
    let open = b"<x:xmpmeta";
    let close = b"</x:xmpmeta>";
    let mut changed = false;
    let mut pos = 0;
    while let Some(rel) = find(&buf[pos..], open) {
        let start = pos + rel;
        let Some(rel_end) = find(&buf[start..], close) else {
            break;
        };
        let end = start + rel_end + close.len();
        changed |= patch_xmp_orientation(&mut buf[start..end], orientation);
        pos = end;
    }
    changed
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// WebP / PNG containers
// ---------------------------------------------------------------------------

const EXIF_HEADER: &[u8] = b"Exif\0\0";

/// Chunks of a WebP (RIFF) file as (fourcc, payload).
pub fn webp_chunks(data: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return None;
    }
    let riff_end = (u32::from_le_bytes(data[4..8].try_into().ok()?) as usize + 8).min(data.len());
    let mut out = Vec::new();
    let mut pos = 12;
    while pos + 8 <= riff_end {
        let id: [u8; 4] = data[pos..pos + 4].try_into().ok()?;
        let len = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().ok()?) as usize;
        let start = pos + 8;
        let end = start.checked_add(len)?;
        if end > riff_end {
            return None;
        }
        out.push((id, &data[start..end]));
        pos = end + (len & 1);
    }
    Some(out)
}

/// Metadata of a WebP file.
#[derive(Default, Debug)]
pub struct WebpMeta {
    pub icc: Option<Vec<u8>>,
    /// Raw TIFF structure (an "Exif\0\0" prefix, written by some tools, is removed).
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub animated: bool,
}

pub fn webp_meta(data: &[u8]) -> WebpMeta {
    let mut m = WebpMeta::default();
    for (id, payload) in webp_chunks(data).unwrap_or_default() {
        match &id {
            b"VP8X" => m.animated |= payload.first().map(|f| f & 0x02 != 0).unwrap_or(false),
            b"ANIM" | b"ANMF" => m.animated = true,
            b"ICCP" => m.icc = Some(payload.to_vec()),
            b"EXIF" => {
                let t = payload.strip_prefix(EXIF_HEADER).unwrap_or(payload);
                if t.len() >= 8 {
                    m.exif = Some(t.to_vec());
                }
            }
            b"XMP " => m.xmp = Some(payload.to_vec()),
            _ => {}
        }
    }
    m
}

/// Rebuilds a still WebP from the image chunks of `encoded` (VP8L, or ALPH+VP8) plus the
/// given metadata, with a correct VP8X header (flags and canvas size). EXIF is stored as a
/// raw TIFF structure, as the WebP specification and libwebp do.
pub fn build_webp(
    encoded: &[u8],
    width: u32,
    height: u32,
    has_alpha: bool,
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let chunks = webp_chunks(encoded).ok_or("invalid WebP produced by the encoder")?;
    let image_chunks: Vec<_> = chunks
        .into_iter()
        .filter(|(id, _)| matches!(id, b"VP8L" | b"VP8 " | b"ALPH"))
        .collect();
    if image_chunks.is_empty() {
        return Err("WebP encoder produced no image data".into());
    }
    if width == 0 || height == 0 || width > 1 << 24 || height > 1 << 24 {
        return Err("invalid WebP canvas size".into());
    }
    let mut body: Vec<u8> = b"WEBP".to_vec();
    let mut push = |id: &[u8; 4], payload: &[u8]| {
        body.extend_from_slice(id);
        body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        body.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            body.push(0);
        }
    };
    let needs_vp8x = icc.is_some() || exif.is_some() || xmp.is_some();
    if needs_vp8x {
        let mut flags = 0u8;
        if icc.is_some() {
            flags |= 0x20;
        }
        if has_alpha {
            flags |= 0x10;
        }
        if exif.is_some() {
            flags |= 0x08;
        }
        if xmp.is_some() {
            flags |= 0x04;
        }
        let mut vp8x = vec![flags, 0, 0, 0];
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        push(b"VP8X", &vp8x);
    }
    if let Some(icc) = icc {
        push(b"ICCP", icc);
    }
    for (id, payload) in &image_chunks {
        push(id, payload);
    }
    if let Some(exif) = exif {
        push(b"EXIF", exif);
    }
    if let Some(xmp) = xmp {
        push(b"XMP ", xmp);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// True for animated PNG (APNG): an `acTL` chunk before the image data.
/// Works on a prefix of the file (returns false if the prefix ends before IDAT).
pub fn png_is_animated(data: &[u8]) -> bool {
    if data.len() < 8 || &data[0..8] != b"\x89PNG\r\n\x1a\n" {
        return false;
    }
    let mut pos = 8;
    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let kind = &data[pos + 4..pos + 8];
        if kind == b"acTL" {
            return true;
        }
        if kind == b"IDAT" || kind == b"IEND" {
            return false;
        }
        pos = match pos.checked_add(12 + len) {
            Some(p) => p,
            None => return false,
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    fn sample() -> DynamicImage {
        let mut img = RgbImage::new(3, 2);
        for (i, px) in img.pixels_mut().enumerate() {
            *px = image::Rgb([i as u8 * 10, 0, 0]);
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn rotation_composition_matches_pixel_operations() {
        let base = sample();
        for o in 1..=8u16 {
            let shown = crate::image_loader::apply_orientation(base.clone(), o as u32);
            for k in 0..4u8 {
                let mut expected = shown.clone();
                for _ in 0..k {
                    expected = expected.rotate90();
                }
                let o2 = compose_rotation(o, k);
                let got = crate::image_loader::apply_orientation(base.clone(), o2 as u32);
                assert_eq!(
                    got.to_rgb8().into_raw(),
                    expected.to_rgb8().into_raw(),
                    "o={} k={}",
                    o,
                    k
                );
                assert_eq!(
                    (got.width(), got.height()),
                    (expected.width(), expected.height())
                );
            }
        }
    }

    #[test]
    fn tiff_orientation_inplace_and_append() {
        let mut t = minimal_exif_with_orientation(1);
        set_tiff_orientation(&mut t, 0, 6).unwrap();
        assert_eq!(get_tiff_orientation(&t, 0), Some(6));
        assert_eq!(t.len(), 26, "in-place update must not grow the buffer");

        // IFD0 without orientation (one unrelated tag, big endian): must append a new IFD0.
        let mut b = Vec::new();
        b.extend_from_slice(b"MM");
        b.extend_from_slice(&42u16.to_be_bytes());
        b.extend_from_slice(&8u32.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&0x010Fu16.to_be_bytes()); // Make
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(&4u32.to_be_bytes());
        b.extend_from_slice(b"ABC\0");
        b.extend_from_slice(&0u32.to_be_bytes());
        set_tiff_orientation(&mut b, 0, 8).unwrap();
        assert_eq!(get_tiff_orientation(&b, 0), Some(8));
        let off = u32::from_be_bytes(b[4..8].try_into().unwrap()) as usize;
        assert_eq!(u16::from_be_bytes(b[off..off + 2].try_into().unwrap()), 2);
        assert_eq!(
            &b[off + 2..off + 4],
            &0x010Fu16.to_be_bytes(),
            "entries must stay sorted"
        );
    }

    #[test]
    fn jpeg_orientation_roundtrip_preserves_image_data() {
        let img = sample();
        let mut jpg = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut jpg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        assert_eq!(get_jpeg_orientation(&jpg), 1);
        let rotated = set_jpeg_orientation(&jpg, 6).unwrap();
        assert_eq!(get_jpeg_orientation(&rotated), 6);
        let again = set_jpeg_orientation(&rotated, 3).unwrap();
        assert_eq!(get_jpeg_orientation(&again), 3);
        assert_eq!(again.len(), rotated.len(), "second update must be in place");
        // Entropy-coded data is untouched: everything after the EXIF segment is identical.
        let tail = |d: &[u8]| {
            let segs = jpeg_header_segments(d).unwrap();
            let last = segs.last().unwrap().end;
            d[last..].to_vec()
        };
        assert_eq!(tail(&jpg), tail(&again));
        // Still decodes, with identical pixels.
        let a = image::load_from_memory(&jpg).unwrap().to_rgb8();
        let b = image::load_from_memory(&again).unwrap().to_rgb8();
        assert_eq!(a.into_raw(), b.into_raw());
    }

    #[test]
    fn xmp_patch_is_same_length() {
        let mut x = br#"<x:xmpmeta><rdf:Description tiff:Orientation="1"/></x:xmpmeta>"#.to_vec();
        let len = x.len();
        assert!(patch_all_xmp_packets(&mut x, 6));
        assert_eq!(x.len(), len);
        assert!(String::from_utf8_lossy(&x).contains(r#"tiff:Orientation="6""#));
        let mut y = b"<x:xmpmeta><tiff:Orientation>8</tiff:Orientation></x:xmpmeta>".to_vec();
        assert!(patch_all_xmp_packets(&mut y, 3));
        assert!(String::from_utf8_lossy(&y).contains("<tiff:Orientation>3<"));
    }
}
