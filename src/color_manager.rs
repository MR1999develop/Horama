//! ICC Color Profile synthesis, parsing, system display profile detection, and qcms transformations.

use crate::config::{
    DEFAULT_FALLBACK_PROFILE, PROFILE_ADOBE_RGB, PROFILE_APPLE_RGB, PROFILE_CIE_RGB,
    PROFILE_DISPLAY_P3, PROFILE_EMBEDDED, PROFILE_PROPHOTO, PROFILE_REC2020, PROFILE_REC709,
    PROFILE_SRGB, PROFILE_WIDE_GAMUT,
};
use qcms::{DataType, Intent, Profile, Transform};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ColorTransformInfo {
    pub source_name: String,
    pub has_embedded: bool,
    pub embedded_name: Option<String>,
    pub display_name: String,
}

fn to_s15f16(val: f64) -> i32 {
    (val * 65536.0).round() as i32
}

fn xyz_tag_data(xyz: (f64, f64, f64)) -> Vec<u8> {
    let mut v = Vec::with_capacity(20);
    v.extend_from_slice(b"XYZ \x00\x00\x00\x00");
    v.extend_from_slice(&to_s15f16(xyz.0).to_be_bytes());
    v.extend_from_slice(&to_s15f16(xyz.1).to_be_bytes());
    v.extend_from_slice(&to_s15f16(xyz.2).to_be_bytes());
    v
}

/// Query the active monitor ICC profile configured in the OS (Windows Color Management / Linux colord).
pub fn get_system_display_icc_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetDC(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
            fn ReleaseDC(hwnd: *mut std::ffi::c_void, hdc: *mut std::ffi::c_void) -> i32;
        }
        #[link(name = "gdi32")]
        extern "system" {
            fn GetICMProfileW(
                hdc: *mut std::ffi::c_void,
                lpcb_name: *mut u32,
                lpsz_filename: *mut u16,
            ) -> i32;
        }
        unsafe {
            let hdc = GetDC(std::ptr::null_mut());
            if !hdc.is_null() {
                let mut size: u32 = 512;
                let mut buf: Vec<u16> = vec![0u16; size as usize];
                let ok = GetICMProfileW(hdc, &mut size, buf.as_mut_ptr());
                ReleaseDC(std::ptr::null_mut(), hdc);
                if ok != 0 {
                    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    let path_str = String::from_utf16_lossy(&buf[..len]);
                    let p = PathBuf::from(path_str);
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let dirs_to_check = [
            "/Library/ColorSync/Profiles/Displays",
            "/System/Library/ColorSync/Profiles",
            "/Library/ColorSync/Profiles",
        ];
        for d in &dirs_to_check {
            let p_dir = std::path::Path::new(d);
            if p_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(p_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                        if matches!(ext.as_str(), "icc" | "icm") && path.is_file() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Generate a valid ICC v2.4 RGB matrix/TRC profile in bytes (matching Horama).
pub fn make_matrix_trc_icc(
    desc_name: &str,
    r_xyz: (f64, f64, f64),
    g_xyz: (f64, f64, f64),
    b_xyz: (f64, f64, f64),
    gamma: f64,
    srgb_curve: bool,
) -> Vec<u8> {
    let wtpt = (0.9642, 1.0000, 0.8249);
    let mut tags: Vec<([u8; 4], Vec<u8>)> = Vec::new();

    // desc
    let desc_bytes = desc_name.as_bytes();
    let mut desc_data = Vec::new();
    desc_data.extend_from_slice(b"desc\x00\x00\x00\x00");
    desc_data.extend_from_slice(&((desc_bytes.len() + 1) as u32).to_be_bytes());
    desc_data.extend_from_slice(desc_bytes);
    desc_data.extend_from_slice(&[0u8; 79]);
    tags.push((*b"desc", desc_data));

    // cprt
    let mut cprt_data = Vec::new();
    cprt_data.extend_from_slice(b"text\x00\x00\x00\x00Horama GPLv3\x00");
    tags.push((*b"cprt", cprt_data));

    // wtpt & primaries
    tags.push((*b"wtpt", xyz_tag_data(wtpt)));
    tags.push((*b"rXYZ", xyz_tag_data(r_xyz)));
    tags.push((*b"gXYZ", xyz_tag_data(g_xyz)));
    tags.push((*b"bXYZ", xyz_tag_data(b_xyz)));

    // TRC: use standard ICC v2 'curv' tag so both qcms and LittleCMS2 parse it identically
    let trc_data = if !srgb_curve {
        let gamma_val = (gamma * 256.0).round().clamp(0.0, 65535.0) as u16;
        let mut d = Vec::new();
        d.extend_from_slice(b"curv\x00\x00\x00\x00");
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(&gamma_val.to_be_bytes());
        d
    } else {
        // 1024-point exact IEC 61966-2-1 sRGB transfer curve LUT
        let n = 1024u32;
        let mut d = Vec::with_capacity(12 + (n as usize) * 2);
        d.extend_from_slice(b"curv\x00\x00\x00\x00");
        d.extend_from_slice(&n.to_be_bytes());
        for i in 0..n {
            let x = (i as f64) / ((n - 1) as f64);
            let y = if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            };
            let v = (y * 65535.0).round().clamp(0.0, 65535.0) as u16;
            d.extend_from_slice(&v.to_be_bytes());
        }
        d
    };

    tags.push((*b"rTRC", trc_data.clone()));
    tags.push((*b"gTRC", trc_data.clone()));
    tags.push((*b"bTRC", trc_data));

    let tag_count = tags.len();
    let header_size = 128usize;
    let table_size = 4 + tag_count * 12;
    let mut offset = header_size + table_size;

    let mut tag_records = Vec::new();
    let mut tag_blobs = Vec::new();

    for (sig, mut data) in tags {
        let orig_len = data.len();
        let pad = (4 - (orig_len % 4)) % 4;
        data.resize(orig_len + pad, 0);
        tag_records.push((sig, offset as u32, orig_len as u32));
        offset += data.len();
        tag_blobs.push(data);
    }

    let total_size = offset as u32;
    let mut header = vec![0u8; 128];
    header[0..4].copy_from_slice(&total_size.to_be_bytes());
    header[4..8].copy_from_slice(b"none");
    header[8..12].copy_from_slice(&0x02400000u32.to_be_bytes());
    header[12..16].copy_from_slice(b"mntr");
    header[16..20].copy_from_slice(b"RGB ");
    header[20..24].copy_from_slice(b"XYZ ");
    header[36..40].copy_from_slice(b"acsp");
    header[68..72].copy_from_slice(&to_s15f16(0.9642).to_be_bytes());
    header[72..76].copy_from_slice(&to_s15f16(1.0000).to_be_bytes());
    header[76..80].copy_from_slice(&to_s15f16(0.8249).to_be_bytes());

    let mut out = header;
    out.extend_from_slice(&(tag_count as u32).to_be_bytes());
    for (sig, off, len) in tag_records {
        out.extend_from_slice(&sig);
        out.extend_from_slice(&off.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
    }
    for blob in tag_blobs {
        out.extend_from_slice(&blob);
    }
    out
}

/// Extract human-readable ASCII description from raw ICC profile bytes (`desc` tag).
pub fn extract_icc_description(icc_bytes: &[u8]) -> Option<String> {
    if icc_bytes.len() < 132 || &icc_bytes[36..40] != b"acsp" {
        return None;
    }
    let tag_count = u32::from_be_bytes(icc_bytes[128..132].try_into().ok()?) as usize;
    let mut pos = 132usize;
    for _ in 0..tag_count {
        if pos + 12 > icc_bytes.len() {
            break;
        }
        let sig = &icc_bytes[pos..pos + 4];
        let offset = u32::from_be_bytes(icc_bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let size = u32::from_be_bytes(icc_bytes[pos + 8..pos + 12].try_into().ok()?) as usize;
        pos += 12;

        if sig == b"desc" && offset + size <= icc_bytes.len() && size >= 12 {
            let tag = &icc_bytes[offset..offset + size];
            if &tag[0..4] == b"desc" {
                let str_len = u32::from_be_bytes(tag[8..12].try_into().ok()?) as usize;
                let end = (12 + str_len).min(tag.len());
                let raw_str = &tag[12..end];
                let clean: String = raw_str
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as char)
                    .collect();
                let trimmed = clean.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            } else if &tag[0..4] == b"mluc" && size >= 28 {
                let rec_len = u32::from_be_bytes(tag[20..24].try_into().ok()?) as usize;
                let rec_off = u32::from_be_bytes(tag[24..28].try_into().ok()?) as usize;
                if rec_off + rec_len <= tag.len() {
                    let utf16_bytes = &tag[rec_off..rec_off + rec_len];
                    let u16s: Vec<u16> = utf16_bytes
                        .chunks_exact(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .take_while(|&c| c != 0)
                        .collect();
                    if let Ok(s) = String::from_utf16(&u16s) {
                        let trimmed = s.trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
            }
        }
    }
    Some("Embedded ICC".to_string())
}

#[derive(Clone)]
pub struct ColorManager {
    icc_bytes_map: HashMap<String, Vec<u8>>,
    custom_profiles: HashMap<String, String>,
    display_icc_bytes: Option<Vec<u8>>,
    display_profile_name: String,
}

impl ColorManager {
    pub fn new() -> Self {
        let mut icc_bytes_map = HashMap::new();

        icc_bytes_map.insert(
            PROFILE_SRGB.to_string(),
            make_matrix_trc_icc(
                PROFILE_SRGB,
                (0.4361, 0.2225, 0.0139),
                (0.3851, 0.7169, 0.0971),
                (0.1431, 0.0606, 0.7141),
                2.2,
                true,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_ADOBE_RGB.to_string(),
            make_matrix_trc_icc(
                PROFILE_ADOBE_RGB,
                (0.60974, 0.31111, 0.01947),
                (0.20528, 0.62567, 0.06087),
                (0.14919, 0.06322, 0.74457),
                2.19921875,
                false,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_DISPLAY_P3.to_string(),
            make_matrix_trc_icc(
                PROFILE_DISPLAY_P3,
                (0.5151, 0.2412, 0.0000),
                (0.2919, 0.6922, 0.0488),
                (0.1571, 0.0666, 0.7741),
                2.2,
                true,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_PROPHOTO.to_string(),
            make_matrix_trc_icc(
                PROFILE_PROPHOTO,
                (0.7977, 0.2880, 0.0000),
                (0.1352, 0.7119, 0.0000),
                (0.0313, 0.0001, 0.8249),
                1.8,
                false,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_REC2020.to_string(),
            make_matrix_trc_icc(
                PROFILE_REC2020,
                (0.6370, 0.2627, 0.0000),
                (0.1446, 0.6780, 0.0281),
                (0.1689, 0.0593, 0.7968),
                2.2,
                false,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_REC709.to_string(),
            make_matrix_trc_icc(
                PROFILE_REC709,
                (0.4361, 0.2225, 0.0139),
                (0.3851, 0.7169, 0.0971),
                (0.1431, 0.0606, 0.7141),
                2.4,
                false,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_WIDE_GAMUT.to_string(),
            make_matrix_trc_icc(
                PROFILE_WIDE_GAMUT,
                (0.7165, 0.2587, 0.0000),
                (0.1010, 0.7247, 0.0449),
                (0.1467, 0.0166, 0.7800),
                2.2,
                false,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_CIE_RGB.to_string(),
            make_matrix_trc_icc(
                PROFILE_CIE_RGB,
                (0.4887, 0.1762, 0.0000),
                (0.3107, 0.8130, 0.0102),
                (0.1648, 0.0108, 0.8147),
                2.2,
                false,
            ),
        );
        icc_bytes_map.insert(
            PROFILE_APPLE_RGB.to_string(),
            make_matrix_trc_icc(
                PROFILE_APPLE_RGB,
                (0.4497, 0.2446, 0.0252),
                (0.3163, 0.6720, 0.0970),
                (0.1845, 0.0833, 0.7027),
                1.8,
                false,
            ),
        );

        // Automatically detect the system monitor ICC profile (matching Python's ImageCms.get_display_profile())
        let mut display_icc_bytes = None;
        let mut display_profile_name = "sRGB (Standard)".to_string();

        if let Some(sys_path) = get_system_display_icc_path() {
            if let Ok(bytes) = fs::read(&sys_path) {
                if Profile::new_from_slice(&bytes, false).is_some() {
                    display_profile_name = extract_icc_description(&bytes).unwrap_or_else(|| {
                        sys_path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| "System Display Profile".to_string())
                    });
                    display_icc_bytes = Some(bytes);
                }
            }
        }

        Self {
            icc_bytes_map,
            custom_profiles: HashMap::new(),
            display_icc_bytes,
            display_profile_name,
        }
    }

    pub fn register_custom_profile(&mut self, path: &Path) -> Option<String> {
        let bytes = fs::read(path).ok()?;
        let _prof = Profile::new_from_slice(&bytes, false)?;
        let desc = extract_icc_description(&bytes).unwrap_or_else(|| "Custom ICC".to_string());
        let fname = path.file_name()?.to_string_lossy();
        let label = format!("Custom: {} ({})", desc, fname);
        self.icc_bytes_map.insert(label.clone(), bytes);
        self.custom_profiles
            .insert(label.clone(), path.to_string_lossy().to_string());
        Some(label)
    }

    pub fn get_custom_profiles(&self) -> Vec<(String, String)> {
        self.custom_profiles
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn get_profile_bytes(&self, profile_name: &str, embedded_icc: Option<&[u8]>) -> Vec<u8> {
        if profile_name == PROFILE_EMBEDDED {
            if let Some(emb) = embedded_icc {
                return emb.to_vec();
            }
            return self.icc_bytes_map[DEFAULT_FALLBACK_PROFILE].clone();
        }
        self.icc_bytes_map
            .get(profile_name)
            .cloned()
            .unwrap_or_else(|| self.icc_bytes_map[DEFAULT_FALLBACK_PROFILE].clone())
    }

    fn resolve_display_qcms_profile(&self) -> (Box<Profile>, String) {
        if let Some(bytes) = &self.display_icc_bytes {
            if let Some(mut p) = Profile::new_from_slice(bytes, false) {
                p.precache_output_transform();
                return (p, self.display_profile_name.clone());
            }
        }
        let mut p = Profile::new_sRGB();
        p.precache_output_transform();
        (p, "sRGB (Standard)".to_string())
    }

    fn resolve_qcms_profile(
        &self,
        profile_name: &str,
        embedded_icc: Option<&[u8]>,
    ) -> (Box<Profile>, String) {
        if profile_name == PROFILE_EMBEDDED {
            if let Some(emb) = embedded_icc {
                if let Some(mut p) = Profile::new_from_slice(emb, false) {
                    p.precache_output_transform();
                    let name =
                        extract_icc_description(emb).unwrap_or_else(|| "Embedded ICC".to_string());
                    return (p, name);
                }
            }
            let mut p = Profile::new_sRGB();
            p.precache_output_transform();
            return (p, format!("{} [Default]", DEFAULT_FALLBACK_PROFILE));
        }

        if profile_name == PROFILE_SRGB {
            let mut p = Profile::new_sRGB();
            p.precache_output_transform();
            return (p, PROFILE_SRGB.to_string());
        }

        if let Some(bytes) = self.icc_bytes_map.get(profile_name) {
            if let Some(mut p) = Profile::new_from_slice(bytes, false) {
                p.precache_output_transform();
                return (p, profile_name.to_string());
            }
        }

        let mut p = Profile::new_sRGB();
        p.precache_output_transform();
        (p, DEFAULT_FALLBACK_PROFILE.to_string())
    }

    /// Automatic color management pipeline:
    /// - If the image has an embedded ICC profile and the user selects a different target profile,
    ///   converts from Embedded -> Target -> System Monitor Display Profile.
    /// - If the image has no embedded ICC profile, assigns the selected profile (defaulting to sRGB)
    ///   and converts to the System Monitor Display Profile.
    pub fn apply_profile_with_info(
        &self,
        raw_rgb: &[u8],
        selected_profile_name: &str,
        embedded_icc: Option<&[u8]>,
    ) -> (Vec<u8>, ColorTransformInfo) {
        let embedded_name = embedded_icc.and_then(extract_icc_description);
        let has_embedded = embedded_name.is_some();

        let (orig_prof, orig_name) = self.resolve_qcms_profile(PROFILE_EMBEDDED, embedded_icc);
        let (mut target_prof, target_name) =
            self.resolve_qcms_profile(selected_profile_name, embedded_icc);
        target_prof.precache_output_transform();

        let (display_prof, display_name) = self.resolve_display_qcms_profile();

        let mut pixels = raw_rgb.to_vec();

        let source_summary = if has_embedded && selected_profile_name != PROFILE_EMBEDDED {
            // Convert mode: Original Embedded -> Target Profile -> Monitor Display Profile
            if let Some(xfm1) = Transform::new(
                &orig_prof,
                &target_prof,
                DataType::RGB8,
                Intent::Perceptual,
            ) {
                xfm1.apply(&mut pixels);
            }
            if let Some(xfm2) = Transform::new(
                &target_prof,
                &display_prof,
                DataType::RGB8,
                Intent::Perceptual,
            ) {
                xfm2.apply(&mut pixels);
            }
            format!("{} → {}", orig_name, target_name)
        } else {
            // Assign / Direct mode: Target Profile -> Monitor Display Profile
            if let Some(xfm) = Transform::new(
                &target_prof,
                &display_prof,
                DataType::RGB8,
                Intent::Perceptual,
            ) {
                xfm.apply(&mut pixels);
            }
            target_name
        };

        let info = ColorTransformInfo {
            source_name: source_summary,
            has_embedded,
            embedded_name,
            display_name,
        };

        (pixels, info)
    }

    /// Convert pixel data for saving when an image with an embedded profile is converted to a new profile.
    pub fn prepare_pixels_for_save(
        &self,
        raw_rgb: &[u8],
        selected_profile_name: &str,
        embedded_icc: Option<&[u8]>,
    ) -> Vec<u8> {
        let has_embedded = embedded_icc.and_then(extract_icc_description).is_some();
        let mut pixels = raw_rgb.to_vec();
        if has_embedded && selected_profile_name != PROFILE_EMBEDDED {
            let (orig_prof, _) = self.resolve_qcms_profile(PROFILE_EMBEDDED, embedded_icc);
            let (mut target_prof, _) =
                self.resolve_qcms_profile(selected_profile_name, embedded_icc);
            target_prof.precache_output_transform();
            if let Some(xfm) = Transform::new(
                &orig_prof,
                &target_prof,
                DataType::RGB8,
                Intent::Perceptual,
            ) {
                xfm.apply(&mut pixels);
            }
        }
        pixels
    }
}

/// Convert 8-bit RGB (0..255) to HSL: (H: 0..360, S: 0..100, L: 0..100).
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (i32, i32, i32) {
    let rf = r as f64 / 255.0;
    let gf = g as f64 / 255.0;
    let bf = b as f64 / 255.0;

    let c_max = rf.max(gf).max(bf);
    let c_min = rf.min(gf).min(bf);
    let delta = c_max - c_min;

    let l = (c_max + c_min) / 2.0;

    let (mut h, s) = if delta == 0.0 {
        (0.0, 0.0)
    } else {
        let s_val = delta / (1.0 - (2.0 * l - 1.0).abs());
        let h_val = if (c_max - rf).abs() < 1e-9 {
            60.0 * (((gf - bf) / delta).rem_euclid(6.0))
        } else if (c_max - gf).abs() < 1e-9 {
            60.0 * (((bf - rf) / delta) + 2.0)
        } else {
            60.0 * (((rf - gf) / delta) + 4.0)
        };
        (h_val, s_val)
    };

    if h < 0.0 {
        h += 360.0;
    }

    (
        (h.round() as i32) % 360,
        (s * 100.0).round() as i32,
        (l * 100.0).round() as i32,
    )
}
