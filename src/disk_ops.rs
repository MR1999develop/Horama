//! Operations that modify files on disk: atomic writes, lossless rotation persistence,
//! moving files to the system trash, and a serial background queue so that concurrent
//! writes to the same file can never interleave.

use crate::exif_orient;
use crate::image_loader::apply_orientation;
use image::{DynamicImage, ImageReader};
use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Self-write registry (lets the folder watcher ignore changes made by this app)
// ---------------------------------------------------------------------------

fn registry() -> &'static Mutex<HashMap<String, Instant>> {
    static REG: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn reg_key(path: &Path) -> String {
    let s = crate::config::normalize_path(&path.to_string_lossy());
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Records that this application is writing `path` right now.
pub fn mark_self_write(path: &Path) {
    if let Ok(mut r) = registry().lock() {
        r.retain(|_, t| t.elapsed() < Duration::from_secs(30));
        r.insert(reg_key(path), Instant::now());
    }
}

/// True if `path` was written by this application within the last few seconds.
pub fn is_recent_self_write(path: &Path) -> bool {
    registry()
        .lock()
        .ok()
        .and_then(|r| r.get(&reg_key(path)).map(|t| t.elapsed() < Duration::from_secs(3)))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Atomic write
// ---------------------------------------------------------------------------

/// Writes `bytes` to `dest` atomically: the data goes to a temporary file in the same folder,
/// is flushed to disk and then renamed over the destination. If anything fails, the original
/// file is left untouched.
pub fn atomic_write(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into());
    // Unique per call, created exclusively: two writers can never share a temporary file.
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let tmp = dir.join(format!(
        ".{}.mrtmp-{}-{}",
        name,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    mark_self_write(dest);
    let result = (|| -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Ok(meta) = fs::metadata(dest) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        // On Windows a viewer/indexer may hold the file briefly: retry a few times.
        let mut last_err = None;
        for _ in 0..5 {
            match fs::rename(&tmp, dest) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(120));
                }
            }
        }
        Err(last_err.unwrap())
    })();
    mark_self_write(dest);
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Could not write '{}': {}", dest.display(), e));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Lossless rotation
// ---------------------------------------------------------------------------

/// Which formats can store a rotation without any loss of image quality.
pub fn rotation_persist_supported(path: &Path) -> bool {
    match ext_lower(path).as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" | "tif" | "tiff" | "bmp" => true,
        // Animated PNG/WebP would lose their animation: rotate on screen only.
        "png" | "webp" => !is_animated_file(path),
        _ => false,
    }
}

/// Checks the beginning of a PNG/WebP file for animation (APNG `acTL`, WebP ANIM).
fn is_animated_file(path: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    if let Ok(f) = fs::File::open(path) {
        let _ = f.take(1 << 20).read_to_end(&mut head);
    }
    match ext_lower(path).as_str() {
        "png" => exif_orient::png_is_animated(&head),
        "webp" => head.len() >= 21 && &head[12..16] == b"VP8X" && head[20] & 0x02 != 0,
        _ => false,
    }
}

fn ext_lower(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

fn rotate_dyn(img: DynamicImage, quarter_turns_cw: u8) -> DynamicImage {
    match quarter_turns_cw % 4 {
        1 => img.rotate90(),
        2 => img.rotate180(),
        3 => img.rotate270(),
        _ => img,
    }
}

fn exif_orientation_of(bytes: &[u8]) -> u16 {
    let mut cur = Cursor::new(bytes);
    exif::Reader::new()
        .read_from_container(&mut cur)
        .ok()
        .and_then(|e| e.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)))
        .map(|v| v as u16)
        .filter(|v| (1..=8).contains(v))
        .unwrap_or(1)
}

/// Rotates the file on disk by `quarter_turns_cw` × 90° clockwise without any quality loss:
/// * JPEG / TIFF: only the EXIF/TIFF `Orientation` tag (and XMP `tiff:Orientation`) changes;
///   compressed image data and all metadata are preserved byte-for-byte.
/// * PNG / WebP / BMP: pixels are re-encoded with a lossless encoder at the original bit depth
///   (PNG keeps 16-bit and alpha); ICC profile, EXIF and text metadata are carried over.
///
/// Returns a short human-readable description of the method used.
pub fn persist_rotation(path: &Path, quarter_turns_cw: u8) -> Result<String, String> {
    let turns = quarter_turns_cw % 4;
    if turns == 0 {
        return Ok("no change".into());
    }
    let bytes = fs::read(path).map_err(|e| format!("Cannot read '{}': {}", path.display(), e))?;
    match ext_lower(path).as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" => {
            let o = exif_orient::get_jpeg_orientation(&bytes);
            let new_o = exif_orient::compose_rotation(o, turns);
            let out = exif_orient::set_jpeg_orientation(&bytes, new_o)?;
            atomic_write(path, &out)?;
            Ok("EXIF orientation updated (lossless)".into())
        }
        "tif" | "tiff" => {
            let mut buf = bytes;
            let o = exif_orient::get_tiff_orientation(&buf, 0).unwrap_or(1);
            let new_o = exif_orient::compose_rotation(o, turns);
            exif_orient::set_tiff_orientation(&mut buf, 0, new_o)?;
            exif_orient::patch_all_xmp_packets(&mut buf, new_o);
            atomic_write(path, &buf)?;
            Ok("TIFF orientation tag updated (lossless)".into())
        }
        "png" => {
            if exif_orient::png_is_animated(&bytes) {
                return Err("animated PNG: the rotation cannot be saved without losing the animation".into());
            }
            let out = reencode_png_rotated(&bytes, turns)?;
            atomic_write(path, &out)?;
            Ok("PNG re-encoded losslessly".into())
        }
        "webp" => {
            let out = reencode_webp_rotated(&bytes, turns)?;
            atomic_write(path, &out)?;
            Ok("WebP re-encoded losslessly".into())
        }
        "bmp" => {
            let img = decode(&bytes)?;
            let img = rotate_dyn(img, turns);
            let img = if img.color().has_alpha() {
                DynamicImage::ImageRgba8(img.to_rgba8())
            } else {
                DynamicImage::ImageRgb8(img.to_rgb8())
            };
            let mut out = Vec::new();
            img.write_with_encoder(image::codecs::bmp::BmpEncoder::new(&mut out))
                .map_err(|e| format!("BMP encode error: {}", e))?;
            atomic_write(path, &out)?;
            Ok("BMP re-encoded losslessly".into())
        }
        other => Err(format!("Rotation cannot be saved losslessly for '{}' files", other)),
    }
}

fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| format!("Decode error: {}", e))
}

/// EXIF with orientation reset to 1 (pixels already carry the orientation).
fn exif_with_normal_orientation(exif: &[u8]) -> Option<Vec<u8>> {
    let mut tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif).to_vec();
    exif_orient::set_tiff_orientation(&mut tiff, 0, 1).ok()?;
    Some(tiff)
}

fn reencode_png_rotated(bytes: &[u8], turns: u8) -> Result<Vec<u8>, String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use img_parts::png::{Png, PngChunk};
    use img_parts::Bytes;

    let o = exif_orientation_of(bytes);
    let img = decode(bytes)?;
    let img = rotate_dyn(apply_orientation(img, o as u32), turns);
    let mut encoded = Vec::new();
    img.write_with_encoder(PngEncoder::new_with_quality(&mut encoded, CompressionType::Best, FilterType::Adaptive))
        .map_err(|e| format!("PNG encode error: {}", e))?;

    let original = Png::from_bytes(Bytes::copy_from_slice(bytes)).map_err(|e| format!("PNG parse error: {}", e))?;
    let mut fresh = Png::from_bytes(Bytes::from(encoded)).map_err(|e| format!("PNG parse error: {}", e))?;

    const COLOR: [&[u8; 4]; 7] = [b"iCCP", b"sRGB", b"gAMA", b"cHRM", b"cICP", b"mDCV", b"cLLI"];
    const META: [&[u8; 4]; 7] = [b"pHYs", b"tEXt", b"zTXt", b"iTXt", b"tIME", b"eXIf", b"sPLT"];
    let chunks = fresh.chunks_mut();
    chunks.retain(|c| {
        let k = c.kind();
        !COLOR.iter().any(|x| **x == k) && !META.iter().any(|x| **x == k)
    });
    let mut insert_at = 1; // right after IHDR
    for c in original.chunks() {
        let k = c.kind();
        if COLOR.iter().any(|x| **x == k) {
            chunks.insert(insert_at, c.clone());
            insert_at += 1;
        }
    }
    for c in original.chunks() {
        let k = c.kind();
        if META.iter().any(|x| **x == k) {
            let chunk = if &k == b"eXIf" {
                match exif_with_normal_orientation(c.contents()) {
                    Some(t) => PngChunk::new(k, Bytes::from(t)),
                    None => continue,
                }
            } else {
                c.clone()
            };
            let at = chunks.len().saturating_sub(1); // before IEND
            chunks.insert(at, chunk);
        }
    }
    let mut out = Vec::new();
    fresh.encoder().write_to(&mut out).map_err(|e| format!("PNG write error: {}", e))?;
    Ok(out)
}

fn reencode_webp_rotated(bytes: &[u8], turns: u8) -> Result<Vec<u8>, String> {
    let meta = exif_orient::webp_meta(bytes);
    if meta.animated {
        return Err("animated WebP: the rotation cannot be saved without losing the animation".into());
    }
    let o = exif_orientation_of(bytes);
    let img = decode(bytes)?;
    let img = rotate_dyn(apply_orientation(img, o as u32), turns);
    let has_alpha = img.color().has_alpha();
    let img = if has_alpha {
        DynamicImage::ImageRgba8(img.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(img.to_rgb8())
    };
    let mut encoded = Vec::new();
    img.write_with_encoder(image::codecs::webp::WebPEncoder::new_lossless(&mut encoded))
        .map_err(|e| format!("WebP encode error: {}", e))?;
    let exif = meta.exif.as_deref().and_then(exif_with_normal_orientation);
    exif_orient::build_webp(
        &encoded,
        img.width(),
        img.height(),
        has_alpha,
        meta.icc.as_deref(),
        exif.as_deref(),
        meta.xmp.as_deref(),
    )
}

// ---------------------------------------------------------------------------
// Serial disk-write queue
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Single background thread that executes disk writes one after another, in order.
#[derive(Clone)]
pub struct DiskQueue {
    tx: Sender<Job>,
    pending: Arc<AtomicUsize>,
}

impl DiskQueue {
    pub fn new() -> Self {
        let (tx, rx) = channel::<Job>();
        let pending = Arc::new(AtomicUsize::new(0));
        let p = pending.clone();
        std::thread::Builder::new()
            .name("disk-writer".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    p.fetch_sub(1, Ordering::SeqCst);
                }
            })
            .expect("failed to start disk writer thread");
        Self { tx, pending }
    }

    /// Queues a job; jobs run strictly in submission order.
    pub fn submit<F: FnOnce() + Send + 'static>(&self, f: F) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(Box::new(f)).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    /// Waits until all queued writes are finished (or the timeout expires).
    pub fn flush(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while self.pending() > 0 {
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }
}

impl Default for DiskQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Trash / Recycle Bin
// ---------------------------------------------------------------------------

/// Moves a file to the system trash (Recycle Bin on Windows, Finder Trash on macOS,
/// freedesktop.org Trash on Linux). Never deletes permanently.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("'{}' does not exist", path.display()));
    }
    imp::move_to_trash(path)
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[repr(C)]
    struct SHFILEOPSTRUCTW {
        hwnd: *mut c_void,
        w_func: u32,
        p_from: *const u16,
        p_to: *const u16,
        f_flags: u16,
        f_any_operations_aborted: i32,
        h_name_mappings: *mut c_void,
        lpsz_progress_title: *const u16,
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHFileOperationW(lp: *mut SHFILEOPSTRUCTW) -> i32;
    }

    const FO_DELETE: u32 = 3;
    const FOF_SILENT: u16 = 0x0004;
    const FOF_NOCONFIRMATION: u16 = 0x0010;
    const FOF_ALLOWUNDO: u16 = 0x0040;
    const FOF_NOERRORUI: u16 = 0x0400;
    /// Ask before deleting permanently (drives without a Recycle Bin, files too large for it).
    const FOF_WANTNUKEWARNING: u16 = 0x4000;

    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut s = abs.to_string_lossy().to_string();
        if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
            s = format!(r"\\{}", unc); // \\?\UNC\server\share -> \\server\share
        } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
            s = stripped.to_string();
        }
        let mut wide: Vec<u16> = std::ffi::OsStr::new(&s).encode_wide().collect();
        wide.push(0);
        wide.push(0); // double-null terminated list
        let mut op = SHFILEOPSTRUCTW {
            hwnd: std::ptr::null_mut(),
            w_func: FO_DELETE,
            p_from: wide.as_ptr(),
            p_to: std::ptr::null(),
            f_flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING,
            f_any_operations_aborted: 0,
            h_name_mappings: std::ptr::null_mut(),
            lpsz_progress_title: std::ptr::null(),
        };
        let rc = unsafe { SHFileOperationW(&mut op) };
        if rc != 0 {
            return Err(format!("Recycle Bin operation failed (code {:#x})", rc));
        }
        if op.f_any_operations_aborted != 0 || path.exists() {
            return Err("Recycle Bin operation was aborted".into());
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        let abs = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let esc = abs.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("tell application \"Finder\" to delete POSIX file \"{}\"", esc);
        let out = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .map_err(|e| format!("Cannot run osascript: {}", e))?;
        if out.status.success() && !path.exists() {
            Ok(())
        } else {
            Err(format!("Finder could not move the file to the Trash: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    //! freedesktop.org Trash specification 1.0.
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::{Path, PathBuf};

    fn uid() -> Option<u32> {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find(|l| l.starts_with("Uid:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
    }

    fn home_trash() -> Option<PathBuf> {
        if let Ok(x) = std::env::var("XDG_DATA_HOME") {
            if !x.is_empty() {
                return Some(PathBuf::from(x).join("Trash"));
            }
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/Trash"))
    }

    fn mount_root(path: &Path) -> PathBuf {
        let dev = fs::metadata(path).map(|m| m.dev()).ok();
        let mut cur = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        while let Some(parent) = cur.parent() {
            match fs::metadata(parent) {
                Ok(m) if Some(m.dev()) == dev => cur = parent.to_path_buf(),
                _ => break,
            }
        }
        cur
    }

    fn percent_encode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{:02X}", b));
            }
        }
        out
    }

    fn deletion_date() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // Civil-from-days (Howard Hinnant)
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
    }

    fn try_trash_dir(trash: &Path, abs: &Path, info_path_value: &str) -> Result<(), std::io::Error> {
        let files = trash.join("files");
        let info = trash.join("info");
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&files)?;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&info)?;
        let name = abs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
            _ => (name.clone(), String::new()),
        };
        for n in 0..10_000 {
            let candidate = if n == 0 { name.clone() } else { format!("{} ({}){}", stem, n, ext) };
            let info_file = info.join(format!("{}.trashinfo", candidate));
            let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(&info_file) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            if files.join(&candidate).exists() {
                drop(f);
                let _ = fs::remove_file(&info_file);
                continue;
            }
            write!(f, "[Trash Info]\nPath={}\nDeletionDate={}\n", info_path_value, deletion_date())?;
            drop(f);
            if let Err(e) = fs::rename(abs, files.join(&candidate)) {
                let _ = fs::remove_file(&info_file);
                return Err(e);
            }
            return Ok(());
        }
        Err(std::io::Error::other("no free name in trash"))
    }

    pub fn move_to_trash(path: &Path) -> Result<(), String> {
        let abs = fs::canonicalize(path).map_err(|e| e.to_string())?;
        let abs_str = abs.to_string_lossy().to_string();
        let mut errors = Vec::new();
        if let Some(home) = home_trash() {
            match try_trash_dir(&home, &abs, &percent_encode(&abs_str)) {
                Ok(()) => return Ok(()),
                Err(e) => errors.push(format!("{}: {}", home.display(), e)),
            }
        }
        // File lives on another file system: use $topdir/.Trash-$uid with a relative path.
        if let Some(uid) = uid() {
            let root = mount_root(&abs);
            let trash = root.join(format!(".Trash-{}", uid));
            let rel = abs.strip_prefix(&root).map(|p| p.to_string_lossy().to_string()).unwrap_or(abs_str.clone());
            match try_trash_dir(&trash, &abs, &percent_encode(&rel)) {
                Ok(()) => return Ok(()),
                Err(e) => errors.push(format!("{}: {}", trash.display(), e)),
            }
        }
        Err(format!("Could not move the file to the Trash ({})", errors.join("; ")))
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn date_format_looks_valid() {
            let d = super::deletion_date();
            assert_eq!(d.len(), 19);
            assert_eq!(&d[4..5], "-");
            assert_eq!(&d[10..11], "T");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mriv-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn pattern_rgb(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| Rgb([(x * 40) as u8, (y * 60) as u8, ((x + y) * 7) as u8]))
    }

    #[test]
    fn jpeg_rotation_changes_only_metadata() {
        let d = tmpdir("jpg");
        let p = d.join("a.jpg");
        DynamicImage::ImageRgb8(pattern_rgb(6, 4)).save(&p).unwrap();
        let before = image::open(&p).unwrap().to_rgb8();
        persist_rotation(&p, 1).unwrap();
        let bytes = fs::read(&p).unwrap();
        assert_eq!(exif_orient::get_jpeg_orientation(&bytes), 6);
        // Raw pixels identical (no recompression), orientation applied on load.
        assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgb8().into_raw(), before.clone().into_raw());
        let shown = crate::image_loader::load_image(&p).unwrap();
        assert_eq!((shown.width, shown.height), (4, 6));
        // Undo returns to orientation 1.
        persist_rotation(&p, 3).unwrap();
        assert_eq!(exif_orient::get_jpeg_orientation(&fs::read(&p).unwrap()), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn png_rotation_is_pixel_exact_and_keeps_16bit_alpha() {
        let d = tmpdir("png");
        let p = d.join("a.png");
        let img = image::ImageBuffer::<Rgba<u16>, Vec<u16>>::from_fn(5, 3, |x, y| {
            Rgba([x as u16 * 1000 + 1, y as u16 * 2000 + 3, 12345, (x * 100 + y) as u16])
        });
        DynamicImage::ImageRgba16(img.clone()).save(&p).unwrap();
        persist_rotation(&p, 1).unwrap();
        let back = image::open(&p).unwrap();
        assert_eq!(back.color(), image::ColorType::Rgba16);
        let expected = DynamicImage::ImageRgba16(img).rotate90();
        assert_eq!(back.to_rgba16().into_raw(), expected.to_rgba16().into_raw());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tiff_and_webp_and_bmp_rotation() {
        let d = tmpdir("misc");
        let src = pattern_rgb(4, 2);

        let t = d.join("a.tif");
        DynamicImage::ImageRgb8(src.clone()).save(&t).unwrap();
        persist_rotation(&t, 2).unwrap();
        assert_eq!(exif_orient::get_tiff_orientation(&fs::read(&t).unwrap(), 0), Some(3));
        let shown = crate::image_loader::load_image(&t).unwrap();
        let expected = DynamicImage::ImageRgb8(src.clone()).rotate180().to_rgb8().into_raw();
        assert_eq!(shown.rgb_pixels, expected);

        let w = d.join("a.webp");
        DynamicImage::ImageRgba8(RgbaImage::from_fn(4, 2, |x, y| Rgba([x as u8 * 50, y as u8 * 90, 7, 200])))
            .save(&w)
            .unwrap();
        {
            // Add EXIF (raw TIFF layout, as libwebp/ExifTool write it).
            let src = fs::read(&w).unwrap();
            let make = exif::Field { tag: exif::Tag::Make, ifd_num: exif::In::PRIMARY, value: exif::Value::Ascii(vec![b"WebCam".to_vec()]) };
            let mut wr = exif::experimental::Writer::new();
            wr.push_field(&make);
            let mut ex = Cursor::new(Vec::new());
            wr.write(&mut ex, true).unwrap();
            let out = exif_orient::build_webp(&src, 4, 2, true, None, Some(ex.get_ref()), Some(b"<x:xmpmeta/>")).unwrap();
            fs::write(&w, out).unwrap();
        }
        let before = image::open(&w).unwrap().to_rgba8();
        persist_rotation(&w, 3).unwrap();
        let after = image::open(&w).unwrap().to_rgba8();
        assert_eq!(after.into_raw(), DynamicImage::ImageRgba8(before).rotate270().to_rgba8().into_raw());
        let e = exif::Reader::new().read_from_container(&mut Cursor::new(fs::read(&w).unwrap())).expect("EXIF kept");
        assert!(e.get_field(exif::Tag::Make, exif::In::PRIMARY).unwrap().display_value().to_string().contains("WebCam"));
        let meta = exif_orient::webp_meta(&fs::read(&w).unwrap());
        assert_eq!(meta.xmp.as_deref(), Some(&b"<x:xmpmeta/>"[..]), "XMP kept");
        assert!(!meta.animated);

        let b = d.join("a.bmp");
        DynamicImage::ImageRgb8(src.clone()).save(&b).unwrap();
        persist_rotation(&b, 1).unwrap();
        assert_eq!(
            image::open(&b).unwrap().to_rgb8().into_raw(),
            DynamicImage::ImageRgb8(src).rotate90().to_rgb8().into_raw()
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn animated_png_rotation_is_refused() {
        let d = tmpdir("apng");
        let p = d.join("a.png");
        DynamicImage::ImageRgb8(pattern_rgb(3, 2)).save(&p).unwrap();
        // Insert an acTL chunk right after IHDR (8 signature + 25 IHDR bytes).
        let mut bytes = fs::read(&p).unwrap();
        let mut actl = vec![0, 0, 0, 8];
        actl.extend_from_slice(b"acTL");
        actl.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 0]);
        actl.extend_from_slice(&[0, 0, 0, 0]); // CRC (not checked by the detector)
        bytes.splice(33..33, actl);
        fs::write(&p, &bytes).unwrap();
        assert!(!rotation_persist_supported(&p));
        assert!(persist_rotation(&p, 1).is_err());
        assert_eq!(fs::read(&p).unwrap(), bytes, "file untouched");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn concurrent_atomic_writes_never_mix() {
        let d = tmpdir("atomic-par");
        let p = d.join("f.bin");
        let a = vec![b'a'; 300_000];
        let b = vec![b'b'; 300_000];
        let handles: Vec<_> = [a.clone(), b.clone()]
            .into_iter()
            .map(|data| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        atomic_write(&p, &data).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let out = fs::read(&p).unwrap();
        assert!(out == a || out == b, "content is one complete version");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_replaces_and_cleans_up() {
        let d = tmpdir("atomic");
        let p = d.join("f.bin");
        fs::write(&p, b"old").unwrap();
        atomic_write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no temp files left behind");
        assert!(is_recent_self_write(&p));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn disk_queue_runs_in_order() {
        let q = DiskQueue::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        for i in 0..20 {
            let l = log.clone();
            q.submit(move || l.lock().unwrap().push(i));
        }
        assert!(q.flush(Duration::from_secs(5)));
        assert_eq!(*log.lock().unwrap(), (0..20).collect::<Vec<_>>());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_trash_moves_file_and_writes_info() {
        let d = tmpdir("trash");
        let data_home = d.join("xdg");
        std::env::set_var("XDG_DATA_HOME", &data_home);
        let p = d.join("photo one.jpg");
        fs::write(&p, b"x").unwrap();
        move_to_trash(&p).unwrap();
        assert!(!p.exists());
        assert!(data_home.join("Trash/files/photo one.jpg").exists());
        let info = fs::read_to_string(data_home.join("Trash/info/photo one.jpg.trashinfo")).unwrap();
        assert!(info.contains("Path=") && info.contains("photo%20one.jpg"));
        // Same name again gets a unique suffix.
        fs::write(&p, b"y").unwrap();
        move_to_trash(&p).unwrap();
        assert!(data_home.join("Trash/files/photo one (1).jpg").exists());
        std::env::remove_var("XDG_DATA_HOME");
        let _ = fs::remove_dir_all(&d);
    }
}
