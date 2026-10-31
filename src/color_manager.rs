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
use std::sync::{Arc, Mutex};

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

/// Query the ICC profile assigned to the monitor that contains `screen_point`
/// (physical screen coordinates; `None` = primary monitor).
///
/// * Windows: Windows Color Management (per monitor).
/// * Linux: colord (`colormgr`), default profile of the first display device.
/// * macOS / others: not detected; displays are treated as sRGB.
pub fn get_system_display_icc_path(screen_point: Option<(i32, i32)>) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return win_display::profile_path(screen_point);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = screen_point;
        return linux_display::profile_path();
    }
    #[allow(unreachable_code)]
    {
        let _ = screen_point;
        None
    }
}

#[cfg(target_os = "windows")]
mod win_display {
    // Win32 structures keep their SDK names so they can be checked against the documentation.
    #![allow(clippy::upper_case_acronyms)]
    use std::ffi::c_void;
    use std::path::PathBuf;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct POINT {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct MONITORINFOEXW {
        cb_size: u32,
        rc_monitor: [i32; 4],
        rc_work: [i32; 4],
        dw_flags: u32,
        sz_device: [u16; 32],
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
        fn MonitorFromPoint(pt: POINT, flags: u32) -> *mut c_void;
        fn GetMonitorInfoW(hmonitor: *mut c_void, lpmi: *mut MONITORINFOEXW) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn GetICMProfileW(hdc: *mut c_void, lpcb_name: *mut u32, lpsz_filename: *mut u16) -> i32;
        fn CreateDCW(
            driver: *const u16,
            device: *const u16,
            port: *const u16,
            devmode: *const c_void,
        ) -> *mut c_void;
        fn DeleteDC(hdc: *mut c_void) -> i32;
    }

    const MONITOR_DEFAULTTONEAREST: u32 = 2;

    unsafe fn icm_profile(hdc: *mut c_void) -> Option<PathBuf> {
        let mut size: u32 = 1024;
        let mut buf: Vec<u16> = vec![0u16; size as usize];
        if GetICMProfileW(hdc, &mut size, buf.as_mut_ptr()) == 0 {
            if size as usize > buf.len() {
                buf = vec![0u16; size as usize];
                if GetICMProfileW(hdc, &mut size, buf.as_mut_ptr()) == 0 {
                    return None;
                }
            } else {
                return None;
            }
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let p = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
        p.is_file().then_some(p)
    }

    pub fn profile_path(screen_point: Option<(i32, i32)>) -> Option<PathBuf> {
        unsafe {
            if let Some((x, y)) = screen_point {
                let hmon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
                if !hmon.is_null() {
                    let mut mi: MONITORINFOEXW = std::mem::zeroed();
                    mi.cb_size = std::mem::size_of::<MONITORINFOEXW>() as u32;
                    if GetMonitorInfoW(hmon, &mut mi) != 0 {
                        let hdc = CreateDCW(
                            mi.sz_device.as_ptr(),
                            mi.sz_device.as_ptr(),
                            std::ptr::null(),
                            std::ptr::null(),
                        );
                        if !hdc.is_null() {
                            let p = icm_profile(hdc);
                            DeleteDC(hdc);
                            if p.is_some() {
                                return p;
                            }
                        }
                    }
                }
            }
            let hdc = GetDC(std::ptr::null_mut());
            if hdc.is_null() {
                return None;
            }
            let p = icm_profile(hdc);
            ReleaseDC(std::ptr::null_mut(), hdc);
            p
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux_display {
    use std::path::PathBuf;
    use std::process::Command;

    fn run(args: &[&str]) -> Option<String> {
        let out = Command::new("colormgr").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn value_after(line: &str, key: &str) -> Option<String> {
        line.trim_start()
            .strip_prefix(key)
            .map(|v| v.trim().to_string())
    }

    /// Default profile of the first colord display device (if colord is installed).
    pub fn profile_path() -> Option<PathBuf> {
        let devices = run(&["get-devices-by-kind", "display"])?;
        let ids: Vec<String> = devices
            .lines()
            .filter_map(|l| value_after(l, "Device ID:"))
            .collect();
        for id in ids {
            if let Some(info) = run(&["device-get-default-profile", &id]) {
                for line in info.lines() {
                    if let Some(f) = value_after(line, "Filename:") {
                        let p = PathBuf::from(f);
                        if p.is_file() {
                            return Some(p);
                        }
                    }
                }
            }
        }
        None
    }
}

/// Returns the 4-byte data color space signature of an ICC profile (e.g. `b"RGB "`, `b"GRAY"`, `b"CMYK"`).
pub fn icc_color_space(icc: &[u8]) -> Option<[u8; 4]> {
    if icc.len() < 128 || &icc[36..40] != b"acsp" {
        return None;
    }
    icc[16..20].try_into().ok()
}

fn hash_bytes(b: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
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

type TransformCache = Arc<Mutex<HashMap<(String, String), Option<Arc<Transform>>>>>;

/// Holds the profile library and the current display profile. Clones share the transform
/// cache, so a clone handed to a worker thread benefits from transforms built elsewhere.
#[derive(Clone)]
pub struct ColorManager {
    icc_bytes_map: HashMap<String, Vec<u8>>,
    custom_profiles: HashMap<String, String>,
    display_icc_bytes: Option<Vec<u8>>,
    display_profile_name: String,
    display_key: String,
    transform_cache: TransformCache,
}

/// Result of rendering an image for the screen.
pub struct RenderedPixels {
    /// Pixels converted to the monitor profile, ready to be shown.
    pub display: Vec<u8>,
    /// Pixels in the *document* color space when it differs from the source pixels
    /// (Convert mode: embedded profile → selected profile). `None` means the source pixels
    /// are already the document values.
    pub document: Option<Vec<u8>>,
    pub info: ColorTransformInfo,
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

        let mut cm = Self {
            icc_bytes_map,
            custom_profiles: HashMap::new(),
            display_icc_bytes: None,
            display_profile_name: "sRGB (Standard)".to_string(),
            display_key: "display:srgb".to_string(),
            transform_cache: Arc::new(Mutex::new(HashMap::new())),
        };
        cm.refresh_display_profile(None);
        cm
    }

    /// Re-detects the monitor profile for the monitor containing `screen_point`.
    /// Returns true when the display profile changed.
    pub fn refresh_display_profile(&mut self, screen_point: Option<(i32, i32)>) -> bool {
        let detected = get_system_display_icc_path(screen_point).and_then(|p| {
            let bytes = fs::read(&p).ok()?;
            Profile::new_from_slice(&bytes, false)?;
            let name = extract_icc_description(&bytes).unwrap_or_else(|| {
                p.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "System Display Profile".to_string())
            });
            Some((bytes, name))
        });
        let (bytes, name, key) = match detected {
            Some((b, n)) => {
                let k = format!("display:{:016x}", hash_bytes(&b));
                (Some(b), n, k)
            }
            None => (
                None,
                "sRGB (Standard)".to_string(),
                "display:srgb".to_string(),
            ),
        };
        if key == self.display_key {
            return false;
        }
        self.display_icc_bytes = bytes;
        self.display_profile_name = name;
        self.display_key = key;
        true
    }

    pub fn display_profile_name(&self) -> &str {
        &self.display_profile_name
    }

    pub fn register_custom_profile(&mut self, path: &Path) -> Option<String> {
        let bytes = fs::read(path).ok()?;
        let _prof = Profile::new_from_slice(&bytes, false)?;
        let desc = extract_icc_description(&bytes).unwrap_or_else(|| "Custom ICC".to_string());
        let fname = path.file_name()?.to_string_lossy();
        let label = format!("Custom: {} ({})", desc, fname);
        self.icc_bytes_map.insert(label.clone(), bytes);
        if let Ok(mut cache) = self.transform_cache.lock() {
            cache.clear();
        }
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
            if let Some(emb) = embedded_icc.filter(|e| {
                icc_color_space(e) == Some(*b"RGB ") && Profile::new_from_slice(e, false).is_some()
            }) {
                return emb.to_vec();
            }
            return self.icc_bytes_map[DEFAULT_FALLBACK_PROFILE].clone();
        }
        self.icc_bytes_map
            .get(profile_name)
            .cloned()
            .unwrap_or_else(|| self.icc_bytes_map[DEFAULT_FALLBACK_PROFILE].clone())
    }

    /// Key identifying the profile a name resolves to (used for the transform cache).
    fn profile_key(&self, profile_name: &str, embedded_icc: Option<&[u8]>) -> String {
        if profile_name == PROFILE_EMBEDDED {
            return match embedded_icc {
                Some(emb) => format!("emb:{:016x}", hash_bytes(emb)),
                None => "builtin:srgb".to_string(),
            };
        }
        if profile_name == PROFILE_SRGB || !self.icc_bytes_map.contains_key(profile_name) {
            return "builtin:srgb".to_string();
        }
        format!("named:{}", profile_name)
    }

    fn display_name_for(&self, profile_name: &str, embedded_icc: Option<&[u8]>) -> String {
        if profile_name == PROFILE_EMBEDDED {
            return match embedded_icc.filter(|e| Profile::new_from_slice(e, false).is_some()) {
                Some(emb) => {
                    extract_icc_description(emb).unwrap_or_else(|| "Embedded ICC".to_string())
                }
                None => format!("{} [Default]", DEFAULT_FALLBACK_PROFILE),
            };
        }
        if self.icc_bytes_map.contains_key(profile_name) {
            profile_name.to_string()
        } else {
            DEFAULT_FALLBACK_PROFILE.to_string()
        }
    }

    fn build_profile(
        &self,
        key: &str,
        profile_name: &str,
        embedded_icc: Option<&[u8]>,
    ) -> Box<Profile> {
        let parsed = if key.starts_with("emb:") {
            embedded_icc.and_then(|e| Profile::new_from_slice(e, false))
        } else if key.starts_with("named:") {
            self.icc_bytes_map
                .get(profile_name)
                .and_then(|b| Profile::new_from_slice(b, false))
        } else if key.starts_with("display:") && key != "display:srgb" {
            self.display_icc_bytes
                .as_ref()
                .and_then(|b| Profile::new_from_slice(b, false))
        } else {
            None
        };
        let mut p = parsed.unwrap_or_else(Profile::new_sRGB);
        p.precache_output_transform();
        p
    }

    /// Returns a cached transform between two profiles (None when no conversion is needed
    /// or qcms cannot build it).
    #[allow(clippy::too_many_arguments)]
    fn transform(
        &self,
        src_key: &str,
        src_name: &str,
        src_emb: Option<&[u8]>,
        dst_key: &str,
        dst_name: &str,
        dst_emb: Option<&[u8]>,
    ) -> Option<Arc<Transform>> {
        let same = src_key == dst_key || (src_key == "builtin:srgb" && dst_key == "display:srgb");
        if same {
            return None;
        }
        let cache_key = (src_key.to_string(), dst_key.to_string());
        if let Ok(cache) = self.transform_cache.lock() {
            if let Some(t) = cache.get(&cache_key) {
                return t.clone();
            }
        }
        let src = self.build_profile(src_key, src_name, src_emb);
        let dst = self.build_profile(dst_key, dst_name, dst_emb);
        let t = Transform::new(&src, &dst, DataType::RGB8, Intent::Perceptual).map(Arc::new);
        if let Ok(mut cache) = self.transform_cache.lock() {
            if cache.len() > 64 {
                cache.clear();
            }
            cache.insert(cache_key, t.clone());
        }
        t
    }

    fn apply_transform(t: &Transform, pixels: &mut [u8]) {
        use rayon::prelude::*;
        const CHUNK: usize = 3 * 64 * 1024;
        if pixels.len() > CHUNK * 2 {
            pixels.par_chunks_mut(CHUNK).for_each(|c| t.apply(c));
        } else {
            t.apply(pixels);
        }
    }

    /// Automatic color management pipeline:
    /// - Tagged image with a different target profile selected (Convert mode):
    ///   Embedded -> Target (document values) -> Monitor profile.
    /// - Otherwise (Assign mode): the selected profile is assigned to the pixels
    ///   (default sRGB for untagged images) and converted to the monitor profile.
    pub fn render(
        &self,
        raw_rgb: &[u8],
        selected_profile_name: &str,
        embedded_icc: Option<&[u8]>,
    ) -> RenderedPixels {
        let embedded_icc = embedded_icc.filter(|e| icc_color_space(e) == Some(*b"RGB "));
        let embedded_name = embedded_icc.and_then(extract_icc_description);
        let has_embedded = embedded_name.is_some();
        let display_key = self.display_key.clone();
        let display_name = self.display_profile_name.clone();

        let target_key = self.profile_key(selected_profile_name, embedded_icc);
        let target_name = self.display_name_for(selected_profile_name, embedded_icc);

        let (document, source_summary) =
            if has_embedded && selected_profile_name != PROFILE_EMBEDDED {
                let emb_key = self.profile_key(PROFILE_EMBEDDED, embedded_icc);
                let emb_name = self.display_name_for(PROFILE_EMBEDDED, embedded_icc);
                let mut doc = raw_rgb.to_vec();
                if let Some(t) = self.transform(
                    &emb_key,
                    PROFILE_EMBEDDED,
                    embedded_icc,
                    &target_key,
                    selected_profile_name,
                    embedded_icc,
                ) {
                    Self::apply_transform(&t, &mut doc);
                }
                (Some(doc), format!("{} → {}", emb_name, target_name))
            } else {
                (None, target_name)
            };

        let mut display = document.clone().unwrap_or_else(|| raw_rgb.to_vec());
        if let Some(t) = self.transform(
            &target_key,
            selected_profile_name,
            embedded_icc,
            &display_key,
            "",
            None,
        ) {
            Self::apply_transform(&t, &mut display);
        }

        RenderedPixels {
            display,
            document,
            info: ColorTransformInfo {
                source_name: source_summary,
                has_embedded,
                embedded_name,
                display_name,
            },
        }
    }

    /// Convenience wrapper returning only the display pixels and the info.
    pub fn apply_profile_with_info(
        &self,
        raw_rgb: &[u8],
        selected_profile_name: &str,
        embedded_icc: Option<&[u8]>,
    ) -> (Vec<u8>, ColorTransformInfo) {
        let r = self.render(raw_rgb, selected_profile_name, embedded_icc);
        (r.display, r.info)
    }

    /// Convert pixel data for saving when an image with an embedded profile is converted to a new profile.
    pub fn prepare_pixels_for_save(
        &self,
        raw_rgb: &[u8],
        selected_profile_name: &str,
        embedded_icc: Option<&[u8]>,
    ) -> Vec<u8> {
        let embedded_icc = embedded_icc.filter(|e| icc_color_space(e) == Some(*b"RGB "));
        let has_embedded = embedded_icc.and_then(extract_icc_description).is_some();
        let mut pixels = raw_rgb.to_vec();
        if has_embedded && selected_profile_name != PROFILE_EMBEDDED {
            let emb_key = self.profile_key(PROFILE_EMBEDDED, embedded_icc);
            let target_key = self.profile_key(selected_profile_name, embedded_icc);
            if let Some(t) = self.transform(
                &emb_key,
                PROFILE_EMBEDDED,
                embedded_icc,
                &target_key,
                selected_profile_name,
                embedded_icc,
            ) {
                Self::apply_transform(&t, &mut pixels);
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
