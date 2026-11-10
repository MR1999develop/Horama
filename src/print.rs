//! Printing through the system: the image as shown is written as an sRGB JPEG in a folder of
//! Horama's own, then handed to the printing of the system.
//!
//! * Windows: the "print" verb of the shell opens the Print Pictures window (printer, paper,
//!   size, preview).
//! * Linux and macOS: CUPS. The printers come from `lpstat`, the job is sent with `lp`;
//!   Horama's window chooses the printer, the copies and whether to fit the page.
//!
//! The print program reads the file after Horama has handed it over, so the files are not
//! deleted at once: those older than a day go at the next print.

use crate::image_saver::{JpegCompression, save_image_with_icc};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

/// JPEG quality of the print file: no visible loss on paper.
const PRINT_JPEG_QUALITY: u8 = 95;
/// Print files are kept this long, then removed at the next print.
const KEEP_PRINT_FILES: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest wait for `lpstat` or `lp`; CUPS answers in well under a second.
const CUPS_TIMEOUT: Duration = Duration::from_secs(20);
/// Most copies of one print job.
pub const MAX_COPIES: u32 = 99;
const PRINT_FILE_PREFIX: &str = "print-";

/// How this system prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintMethod {
    /// The print window of the system (Windows).
    SystemDialog,
    /// Horama's window, then CUPS (`lp`).
    Cups,
}

/// The method of the system Horama runs on.
pub fn method() -> PrintMethod {
    if cfg!(windows) {
        PrintMethod::SystemDialog
    } else {
        PrintMethod::Cups
    }
}

/// Folder of the print files: in the user's cache, private to the user.
fn spool_dir() -> Result<PathBuf, String> {
    let dir = dirs::cache_dir()
        .ok_or("the cache folder of the user is unknown")?
        .join(crate::config::APP_NAME)
        .join("print");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create the folder {}: {}", dir.display(), e))?;
    Ok(dir)
}

/// Removes the print files of `dir` older than `keep` (only Horama's own files).
fn remove_old_print_files(dir: &Path, keep: Duration, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let ours = name
            .to_str()
            .is_some_and(|n| n.starts_with(PRINT_FILE_PREFIX) && n.ends_with(".jpg"));
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > keep);
        if ours && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A name for a new print file in `dir` that no file has yet.
fn new_print_file(dir: &Path, now: SystemTime) -> PathBuf {
    let stamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (0u32..)
        .map(|n| dir.join(format!("{PRINT_FILE_PREFIX}{stamp}-{n}.jpg")))
        .find(|p| !p.exists())
        .expect("an unused name exists")
}

/// Writes the image to print (`srgb_pixels`: 8-bit RGB in sRGB, `srgb_icc`: the sRGB profile)
/// as a new JPEG in the print folder, and returns its path.
pub fn write_print_file(
    width: u32,
    height: u32,
    srgb_pixels: &[u8],
    srgb_icc: &[u8],
) -> Result<PathBuf, String> {
    let dir = spool_dir()?;
    write_print_file_in(&dir, width, height, srgb_pixels, srgb_icc)
}

fn write_print_file_in(
    dir: &Path,
    width: u32,
    height: u32,
    srgb_pixels: &[u8],
    srgb_icc: &[u8],
) -> Result<PathBuf, String> {
    let now = SystemTime::now();
    remove_old_print_files(dir, KEEP_PRINT_FILES, now);
    let path = new_print_file(dir, now);
    save_image_with_icc(
        &path,
        width,
        height,
        srgb_pixels,
        Some(srgb_icc),
        None,
        &JpegCompression::Quality(PRINT_JPEG_QUALITY),
    )?;
    Ok(path)
}

/// Windows: opens the Print Pictures window of the system on `file`.
#[cfg(windows)]
#[expect(unsafe_code, reason = "ShellExecuteExW, CoInitializeEx")]
pub fn open_system_print_dialog(file: &Path) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct ShellExecuteInfoW {
        cbSize: u32,
        fMask: u32,
        hwnd: *mut c_void,
        lpVerb: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShow: i32,
        hInstApp: *mut c_void,
        lpIDList: *mut c_void,
        lpClass: *const u16,
        hkeyClass: *mut c_void,
        dwHotKey: u32,
        hIconOrMonitor: *mut c_void,
        hProcess: *mut c_void,
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
        fn CoUninitialize();
    }
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    // The call returns once the print window has started; errors come back, not as a box.
    const SEE_MASK_NOASYNC: u32 = 0x100;
    const SEE_MASK_FLAG_NO_UI: u32 = 0x400;
    const SW_SHOWNORMAL: i32 = 1;

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide(std::ffi::OsStr::new("print"));
    let file_w = wide(file.as_os_str());
    let mut info = ShellExecuteInfoW {
        cbSize: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        hwnd: std::ptr::null_mut(),
        lpVerb: verb.as_ptr(),
        lpFile: file_w.as_ptr(),
        lpParameters: std::ptr::null(),
        lpDirectory: std::ptr::null(),
        nShow: SW_SHOWNORMAL,
        hInstApp: std::ptr::null_mut(),
        lpIDList: std::ptr::null_mut(),
        lpClass: std::ptr::null(),
        hkeyClass: std::ptr::null_mut(),
        dwHotKey: 0,
        hIconOrMonitor: std::ptr::null_mut(),
        hProcess: std::ptr::null_mut(),
    };
    // SAFETY: COM is initialised on this worker thread for the shell call and released below
    // only if this call succeeded (S_OK or S_FALSE); `info` and the UTF-16 strings it points to
    // live until ShellExecuteExW returns, and no process handle is requested.
    unsafe {
        let com = CoInitializeEx(
            std::ptr::null_mut(),
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        );
        let ok = ShellExecuteExW(&mut info) != 0;
        let error = std::io::Error::last_os_error();
        if com >= 0 {
            CoUninitialize();
        }
        if ok {
            Ok(())
        } else {
            Err(format!("Windows cannot print this file: {}", error))
        }
    }
}

#[cfg(not(windows))]
pub fn open_system_print_dialog(_file: &Path) -> Result<(), String> {
    Err("the print window of the system exists only on Windows".to_string())
}

/// The printers CUPS knows, with the default one if set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Printers {
    pub names: Vec<String>,
    pub default: Option<String>,
}

/// A CUPS program, by absolute path: never looked up in the `PATH`.
fn cups_program(name: &str) -> PathBuf {
    Path::new("/usr/bin").join(name)
}

/// Runs `program` with `args` (a worker thread waits here) and returns its output; after
/// `timeout` the program is stopped. Its output is a few lines, well within the pipe buffers.
fn run_with_timeout(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {}", program.display(), e))?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not answer within {} seconds",
                    program.display(),
                    timeout.as_secs()
                ));
            }
            Err(e) => return Err(format!("{} failed: {}", program.display(), e)),
        }
    }
    child
        .wait_with_output()
        .map_err(|e| format!("{} failed: {}", program.display(), e))
}

/// Destinations listed by `lpstat -e`, one per line.
fn parse_destinations(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The default destination from `lpstat -d` ("system default destination: NAME").
fn parse_default(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("system default destination:"))
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
}

/// The printers of CUPS.
pub fn list_printers() -> Result<Printers, String> {
    let lpstat = cups_program("lpstat");
    let out = run_with_timeout(&lpstat, &["-e".into()], CUPS_TIMEOUT)?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let names = parse_destinations(&String::from_utf8_lossy(&out.stdout));
    // No default printer is not an error: the first one is proposed instead.
    let default = run_with_timeout(&lpstat, &["-d".into()], CUPS_TIMEOUT)
        .ok()
        .and_then(|o| parse_default(&String::from_utf8_lossy(&o.stdout)))
        .filter(|d| names.contains(d));
    Ok(Printers { names, default })
}

/// What `lp` is asked to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CupsJob {
    /// One of [`Printers::names`].
    pub printer: String,
    /// 1 to [`MAX_COPIES`].
    pub copies: u32,
    pub fit_to_page: bool,
    /// Name of the job in the print queue.
    pub title: String,
}

/// Arguments of `lp` for `job` on `file`. The printer must be one CUPS listed: a name that
/// could be read as an option is refused.
fn lp_args(job: &CupsJob, file: &Path) -> Result<Vec<OsString>, String> {
    if job.printer.is_empty() || job.printer.starts_with('-') {
        return Err(format!("'{}' is not a printer name", job.printer));
    }
    let mut args: Vec<OsString> = vec![
        "-d".into(),
        job.printer.clone().into(),
        "-n".into(),
        job.copies.clamp(1, MAX_COPIES).to_string().into(),
        "-t".into(),
        job.title.clone().into(),
    ];
    if job.fit_to_page {
        args.extend(["-o".into(), "fit-to-page".into()]);
    }
    args.push("--".into());
    args.push(file.as_os_str().to_owned());
    Ok(args)
}

/// Sends `file` to CUPS; returns what `lp` answered ("request id is …").
pub fn print_with_cups(job: &CupsJob, file: &Path) -> Result<String, String> {
    let out = run_with_timeout(&cups_program("lp"), &lp_args(job, file)?, CUPS_TIMEOUT)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("horama-print-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn printers_are_read_from_lpstat() {
        let names = parse_destinations("Office_Laser\n\nPhoto-Inkjet \n");
        assert_eq!(names, ["Office_Laser", "Photo-Inkjet"]);
        assert_eq!(
            parse_default("system default destination: Photo-Inkjet\n").as_deref(),
            Some("Photo-Inkjet")
        );
        assert_eq!(parse_default("no system default destination\n"), None);
    }

    #[test]
    fn lp_gets_separate_arguments_and_no_option_as_printer() {
        let job = CupsJob {
            printer: "Photo".into(),
            copies: 500,
            fit_to_page: true,
            title: "a b.jpg".into(),
        };
        let args = lp_args(&job, Path::new("/tmp/-x.jpg")).unwrap();
        let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        assert_eq!(
            args,
            [
                "-d",
                "Photo",
                "-n",
                "99",
                "-t",
                "a b.jpg",
                "-o",
                "fit-to-page",
                "--",
                "/tmp/-x.jpg"
            ]
        );
        let no_fit = CupsJob {
            fit_to_page: false,
            copies: 0,
            ..job.clone()
        };
        let args = lp_args(&no_fit, Path::new("f.jpg")).unwrap();
        assert!(!args.iter().any(|a| a == "fit-to-page"));
        assert!(args.iter().any(|a| a == "1"));
        let hostile = CupsJob {
            printer: "-oraw".into(),
            ..job
        };
        assert!(lp_args(&hostile, Path::new("f.jpg")).is_err());
    }

    #[test]
    fn print_files_are_new_and_old_ones_are_removed() {
        let dir = tmpdir("spool");
        let pixels = vec![200u8; 4 * 3 * 3];
        let srgb = crate::color_manager::ColorManager::new()
            .get_profile_bytes(crate::config::PROFILE_SRGB, None);
        let a = write_print_file_in(&dir, 4, 3, &pixels, &srgb).unwrap();
        let b = write_print_file_in(&dir, 4, 3, &pixels, &srgb).unwrap();
        assert_ne!(a, b);
        let img = image::open(&a).unwrap();
        assert_eq!((img.width(), img.height()), (4, 3));
        assert!(
            crate::image_loader::extract_icc_from_bytes(&std::fs::read(&a).unwrap(), "jpg")
                .is_some()
        );
        // A day later both are old; other files of the folder are left alone.
        std::fs::write(dir.join("other.jpg"), b"x").unwrap();
        let later = SystemTime::now() + KEEP_PRINT_FILES + Duration::from_secs(60);
        remove_old_print_files(&dir, KEEP_PRINT_FILES, later);
        assert!(!a.exists() && !b.exists());
        assert!(dir.join("other.jpg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
