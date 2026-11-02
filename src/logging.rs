//! Diagnostic log written to a file, so problems in the release build (which has no console)
//! leave a trace.
//!
//! * Location: `<config dir>/Horama/logs/horama.log` (`%APPDATA%\Horama\logs` on Windows).
//! * Rotation: when the file exceeds [`MAX_LOG_BYTES`] it becomes `horama.1.log`, the previous
//!   one `horama.2.log`, and so on up to [`KEPT_OLD_LOGS`] files; older ones are deleted.
//! * Level: `info` by default; the `HORAMA_LOG` environment variable (`error`, `warn`, `info`,
//!   `debug`, `trace`) changes it at start-up.
//! * Every line: UTC timestamp with milliseconds, level, thread, module, message.
//!
//! Messages never contain image data or settings content, only file paths and error causes.

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Size at which the current log file is rotated.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Number of rotated files kept next to the current one.
pub const KEPT_OLD_LOGS: u32 = 3;
const LOG_FILE_STEM: &str = "horama";
const LEVEL_ENV_VAR: &str = "HORAMA_LOG";

struct FileLogger {
    level: LevelFilter,
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            SystemTime::now(),
            record.level(),
            std::thread::current().name().unwrap_or("unnamed"),
            record.target(),
            &record.args().to_string(),
        );
        // Debug builds also print to the console.
        if cfg!(debug_assertions) {
            eprint!("{}", line);
        }
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        if guard
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0)
            >= MAX_LOG_BYTES
        {
            *guard = None; // close before renaming (required on Windows)
            rotate(&self.path, KEPT_OLD_LOGS);
        }
        if guard.is_none() {
            *guard = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock()
            && let Some(f) = guard.as_mut()
        {
            let _ = f.flush();
        }
    }
}

/// Installs the file logger. Call once, at the start of `main`, before any other thread starts.
/// Returns the log file path, or `None` if no log directory could be created (the program then
/// runs without a log).
pub fn init(app_name: &str) -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join(app_name).join("logs");
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.log", LOG_FILE_STEM));
    let level = std::env::var(LEVEL_ENV_VAR)
        .ok()
        .and_then(|v| v.trim().parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::Info);
    let logger = FileLogger {
        level,
        path: path.clone(),
        file: Mutex::new(None),
    };
    log::set_boxed_logger(Box::new(logger)).ok()?;
    log::set_max_level(level);
    Some(path)
}

/// `horama.log` → `horama.1.log` → … → `horama.<keep>.log`; the oldest is deleted.
fn rotate(path: &Path, keep: u32) {
    let numbered = |n: u32| path.with_file_name(format!("{}.{}.log", LOG_FILE_STEM, n));
    let _ = fs::remove_file(numbered(keep));
    for n in (1..keep).rev() {
        let _ = fs::rename(numbered(n), numbered(n + 1));
    }
    let _ = fs::rename(path, numbered(1));
}

/// One log line: `2026-10-31T10:05:03.123Z INFO  [main] horama::config: message`.
fn format_line(now: SystemTime, level: Level, thread: &str, target: &str, message: &str) -> String {
    // Messages are single-line so the file can be read and filtered line by line.
    let message = message.replace(['\r', '\n'], " ");
    format!(
        "{} {:<5} [{}] {}: {}\n",
        utc_timestamp(now),
        level,
        thread,
        target,
        message
    )
}

/// ISO 8601 UTC timestamp with milliseconds, without external date libraries.
fn utc_timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (y, m, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        day,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// Days since 1970-01-01 → (year, month, day) in the proleptic Gregorian calendar
/// (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_are_utc_iso8601() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        // 2026-10-31T09:05:03.250Z
        let t = UNIX_EPOCH + Duration::from_millis(1_793_437_503_250);
        assert_eq!(utc_timestamp(t), "2026-10-31T09:05:03.250Z");
        // Leap day.
        let t = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(utc_timestamp(t), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn lines_are_single_line_with_fixed_fields() {
        let line = format_line(
            UNIX_EPOCH,
            Level::Warn,
            "main",
            "horama::disk_ops",
            "a\nb\r\nc",
        );
        assert_eq!(
            line,
            "1970-01-01T00:00:00.000Z WARN  [main] horama::disk_ops: a b  c\n"
        );
    }

    #[test]
    fn rotation_keeps_a_bounded_number_of_files() {
        let dir = std::env::temp_dir().join(format!("horama-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("horama.log");
        for round in 0..5 {
            fs::write(&path, format!("round {}", round)).unwrap();
            rotate(&path, 3);
        }
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["horama.1.log", "horama.2.log", "horama.3.log"]);
        assert_eq!(
            fs::read_to_string(dir.join("horama.1.log")).unwrap(),
            "round 4"
        );
        assert_eq!(
            fs::read_to_string(dir.join("horama.3.log")).unwrap(),
            "round 2"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
