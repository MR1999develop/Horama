//! Configuration, supported file extensions, folder scanning, and persistent settings.

use crate::persist;
use crate::ratings::Ratings;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use walkdir::WalkDir;

pub const APP_NAME: &str = "Horama";
/// Names used by earlier versions of the program; their settings are copied once at the
/// first start under the current name (the old files are left untouched).
const LEGACY_APP_NAMES: &[&str] = &["Horama"];
pub const MAX_RECENT_FOLDERS: usize = 10;
pub const THUMBNAIL_WIDTH: u32 = 140;
pub const THUMBNAIL_HEIGHT: u32 = 100;
pub const CACHE_FREE_RAM_RATIO: f64 = 0.65;

pub const PROFILE_EMBEDDED: &str = "Embedded Profile";
pub const PROFILE_SRGB: &str = "sRGB IEC61966-2.1";
pub const PROFILE_ADOBE_RGB: &str = "Adobe RGB (1998)";
pub const PROFILE_PROPHOTO: &str = "ProPhoto RGB (ROMM RGB)";
pub const PROFILE_DISPLAY_P3: &str = "Display P3";
pub const PROFILE_REC2020: &str = "Rec.2020";
pub const PROFILE_REC709: &str = "Rec.709";
pub const PROFILE_WIDE_GAMUT: &str = "Wide Gamut RGB";
pub const PROFILE_CIE_RGB: &str = "CIE RGB";
pub const PROFILE_APPLE_RGB: &str = "Apple RGB";
pub const PROFILE_LOAD_CUSTOM: &str = "Load Custom ICC/ICM Profile...";

pub const DEFAULT_FALLBACK_PROFILE: &str = PROFILE_SRGB;

pub const BUILTIN_PROFILES: &[&str] = &[
    PROFILE_EMBEDDED,
    PROFILE_SRGB,
    PROFILE_ADOBE_RGB,
    PROFILE_PROPHOTO,
    PROFILE_DISPLAY_P3,
    PROFILE_REC2020,
    PROFILE_REC709,
    PROFILE_WIDE_GAMUT,
    PROFILE_CIE_RGB,
    PROFILE_APPLE_RGB,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub last_folder: String,
    #[serde(default)]
    pub last_subfolder: String,
    pub recent_folders: Vec<String>,
    pub custom_profiles: Vec<String>,
    /// Ratings saved inside the settings by older versions: read once, moved to [`Ratings`]
    /// and never written back.
    #[serde(default, rename = "image_ratings", skip_serializing)]
    legacy_ratings: HashMap<String, u8>,
    /// Star ratings, kept in their own file (see [`crate::ratings`]).
    #[serde(skip)]
    pub ratings: Ratings,
    #[serde(default = "default_window_width")]
    pub window_width: u32,
    #[serde(default = "default_window_height")]
    pub window_height: u32,
    #[serde(default)]
    pub window_maximized: bool,
    #[serde(default = "default_shortcuts")]
    pub shortcuts: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub saved_aspect_ratios: Vec<String>,
    /// JPEG quality chosen at the last save (1–100); 0 = the compression of the original.
    #[serde(default)]
    pub jpeg_quality: u8,
    /// Theme of the egui interface.
    #[serde(default)]
    pub theme: ThemeChoice,
    /// Language of the egui interface: "" = the system language, otherwise "en" or "it".
    #[serde(default)]
    pub language: String,
}

/// Theme of the interface; Dark by default, as suits an image viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeChoice {
    System,
    Light,
    #[default]
    Dark,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    /// English name, to be passed through the translation.
    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "System",
            ThemeChoice::Light => "Light",
            ThemeChoice::Dark => "Dark",
        }
    }
}

pub fn default_shortcuts() -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    m.insert("undo".to_string(), "Ctrl+Z".to_string());
    m.insert("redo".to_string(), "Ctrl+Y".to_string());
    m.insert("zoom_in".to_string(), "+".to_string());
    m.insert("zoom_out".to_string(), "-".to_string());
    m.insert("zoom_fit".to_string(), "Ctrl+0".to_string());
    m.insert("zoom_100".to_string(), "Ctrl+1".to_string());
    m.insert("rotate_cw".to_string(), "R".to_string());
    m.insert("rotate_ccw".to_string(), "L".to_string());
    m.insert("toggle_crop".to_string(), "C".to_string());
    m.insert("next_image".to_string(), "Right".to_string());
    m.insert("prev_image".to_string(), "Left".to_string());
    m.insert("jump_forward_10".to_string(), "Up".to_string());
    m.insert("jump_backward_10".to_string(), "Down".to_string());
    m.insert("cycle_profile".to_string(), "P".to_string());
    m.insert("open_shortcuts".to_string(), "S".to_string());
    m.insert("rename".to_string(), "F2".to_string());
    m.insert("delete".to_string(), "Delete".to_string());
    m.insert("refresh".to_string(), "F5".to_string());
    m.insert("open_file".to_string(), "Ctrl+O".to_string());
    m.insert("open_folder".to_string(), "Ctrl+Shift+O".to_string());
    m.insert("save".to_string(), "Ctrl+S".to_string());
    m
}

fn default_window_width() -> u32 {
    1440
}

fn default_window_height() -> u32 {
    900
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_folder: String::new(),
            last_subfolder: String::new(),
            recent_folders: Vec::new(),
            custom_profiles: Vec::new(),
            legacy_ratings: HashMap::new(),
            ratings: Ratings::in_memory(),
            window_width: 1440,
            window_height: 900,
            window_maximized: false,
            shortcuts: default_shortcuts(),
            saved_aspect_ratios: Vec::new(),
            jpeg_quality: 0,
            theme: ThemeChoice::default(),
            language: String::new(),
        }
    }
}

impl AppSettings {
    pub fn config_file_path() -> PathBuf {
        settings_path_in(&config_base_dir(), APP_NAME)
    }

    pub fn ratings_file_path() -> PathBuf {
        config_base_dir().join(APP_NAME).join("ratings.jsonl")
    }

    /// Reads the settings and the ratings; ratings found inside the settings (older versions)
    /// are moved to the ratings file.
    pub fn load() -> Self {
        let mut settings = Self::load_settings_file();
        settings.ratings = Ratings::load(Self::ratings_file_path());
        if !settings.legacy_ratings.is_empty() {
            // The settings stop holding the old ratings at their next save: a copy of the file
            // as it was is kept in case the move goes wrong.
            let path = Self::config_file_path();
            let backup = path.with_extension("json.before-ratings");
            if !backup.exists()
                && let Err(e) = fs::copy(&path, &backup) {
                    log::error!("Settings not copied to {}: {}", backup.display(), e);
                }
            let legacy = std::mem::take(&mut settings.legacy_ratings);
            let count = legacy.len();
            settings.ratings.import(legacy);
            let written = persist::flush(Duration::from_secs(10))
                && Ratings::load(Self::ratings_file_path()).len() >= settings.ratings.len();
            if written {
                settings.save();
                log::info!(
                    "{} ratings moved from the settings to their own file",
                    count
                );
            } else {
                log::error!(
                    "Ratings not written to their own file; the old ones are in {}",
                    backup.display()
                );
            }
        }
        settings
    }

    fn load_settings_file() -> Self {
        if let Some(src) = migrate_legacy_settings(&config_base_dir()) {
            log::info!("Settings copied from {}", src.display());
        }
        let path = Self::config_file_path();
        let data = match fs::read_to_string(&path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                log::error!("Settings not read from {}: {}", path.display(), e);
                return Self::default();
            }
        };
        match serde_json::from_str::<AppSettings>(&data) {
            Ok(mut parsed) => {
                let defaults = default_shortcuts();
                for (k, v) in defaults {
                    parsed.shortcuts.entry(k).or_insert(v);
                }
                parsed
            }
            Err(e) => {
                // Keep the unreadable file: the next save would otherwise overwrite it.
                let backup = path.with_extension("json.bad");
                let _ = fs::copy(&path, &backup);
                log::error!(
                    "Settings in {} are not valid ({}); defaults used, copy kept in {}",
                    path.display(),
                    e,
                    backup.display()
                );
                Self::default()
            }
        }
    }

    /// Queues the settings for writing on the background writer ([`persist`]), which writes
    /// them atomically (temp file + rename) so a crash never leaves a truncated file behind.
    pub fn save(&self) {
        match serde_json::to_string_pretty(self) {
            Ok(json) => persist::replace(Self::config_file_path(), json.into_bytes()),
            Err(e) => log::error!("Settings not saved: {}", e),
        }
    }

    pub fn add_recent_folder(&mut self, folder: &str) {
        let norm = normalize_path(folder);
        self.last_folder = norm.clone();
        self.recent_folders
            .retain(|f| f.to_lowercase() != norm.to_lowercase());
        self.recent_folders.insert(0, norm);
        if self.recent_folders.len() > MAX_RECENT_FOLDERS {
            self.recent_folders.truncate(MAX_RECENT_FOLDERS);
        }
        self.save();
    }

    pub fn remove_recent_folder(&mut self, folder: &str) {
        let norm = normalize_path(folder);
        self.recent_folders
            .retain(|f| f.to_lowercase() != norm.to_lowercase());
        self.save();
    }

    pub fn get_rating(&self, path: &str) -> u8 {
        self.ratings.get(path)
    }

    /// Sets a rating; it is written to the ratings file on its own, without [`Self::save`].
    pub fn set_rating(&mut self, path: &str, rating: u8) {
        self.ratings.set(path, rating);
    }

    /// Moves the rating of a renamed file to its new path.
    pub fn rename_rating(&mut self, old: &str, new: &str) {
        self.ratings.rename(old, new);
    }

    pub fn clear_recent_folders(&mut self) {
        self.recent_folders.clear();
        self.save();
    }
}

/// Folder that holds the program's settings folder. `HORAMA_CONFIG_DIR` replaces it (the
/// interface tests use it to keep away from the user's settings).
fn config_base_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HORAMA_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn settings_path_in(base: &Path, app_name: &str) -> PathBuf {
    base.join(app_name).join("settings.json")
}

/// If there are no settings for the current program name, copies the settings file of the
/// first previous name that has one (ratings, recent folders, shortcuts, window size).
/// Returns the file that was copied, if any.
fn migrate_legacy_settings(base: &Path) -> Option<PathBuf> {
    let dest = settings_path_in(base, APP_NAME);
    if dest.exists() {
        return None;
    }
    let src = LEGACY_APP_NAMES
        .iter()
        .map(|old| settings_path_in(base, old))
        .find(|p| p.is_file())?;
    fs::create_dir_all(dest.parent()?).ok()?;
    fs::copy(&src, &dest).ok()?;
    Some(src)
}

/// Key used to identify a file in persistent maps: case-insensitive on Windows only,
/// because Linux/macOS file names are case-sensitive.
pub fn path_key(p: &str) -> String {
    let n = normalize_path(p);
    if cfg!(windows) {
        n.to_lowercase()
    } else {
        n
    }
}

/// True if two paths refer to the same file name (case-insensitive on Windows only).
pub fn same_path(a: &str, b: &str) -> bool {
    path_key(a) == path_key(b)
}

pub const STANDARD_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "tif", "tiff", "png", "webp", "bmp", "dib", "gif", "ppm", "pgm",
    "pbm", "pnm", "ico", "tga",
];
pub const RAW_EXTENSIONS: &[&str] = &[
    "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "orf", "rw2", "raw", "pef",
    "raf", "erf", "3fr", "fff", "iiq", "mrw", "mos", "x3f", "srw",
];

/// All extensions accepted by the "Open" dialog (exactly the supported ones).
pub fn dialog_extensions() -> Vec<&'static str> {
    STANDARD_EXTENSIONS
        .iter()
        .chain(RAW_EXTENSIONS.iter())
        .copied()
        .collect()
}

pub fn normalize_path(p: &str) -> String {
    Path::new(p)
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .to_string()
}

pub fn is_raw_extension(ext: &str) -> bool {
    let e = ext.to_lowercase();
    RAW_EXTENSIONS.contains(&e.as_str())
}

pub fn is_standard_extension(ext: &str) -> bool {
    let e = ext.to_lowercase();
    STANDARD_EXTENSIONS.contains(&e.as_str())
}

pub fn is_supported_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        is_standard_extension(ext) || is_raw_extension(ext)
    } else {
        false
    }
}

pub fn is_raw_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        is_raw_extension(ext)
    } else {
        false
    }
}

#[allow(dead_code)]
pub fn scan_folder_recursive(folder: &str) -> Vec<String> {
    let root = Path::new(folder);
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || e.file_name()
                    .to_str()
                    .map(|s| !s.starts_with('.'))
                    .unwrap_or(true)
        })
        .flatten()
    {
        if entry.file_type().is_file() && is_supported_file(entry.path()) {
            files.push(normalize_path(&entry.path().to_string_lossy()));
        }
    }
    files.sort_by_key(|a| a.to_lowercase());
    files
}

/// Hierarchical depth-first path comparator.
/// Compares two subdirectory paths component-by-component so that children stay directly under their parent.
pub fn compare_paths_hierarchical(root: &str, a: &str, b: &str) -> std::cmp::Ordering {
    let p_root = Path::new(root);
    let p_a = Path::new(a);
    let p_b = Path::new(b);

    let root_comps: Vec<String> = p_root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let a_comps: Vec<String> = p_a
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let b_comps: Vec<String> = p_b
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();

    let rel_a: &[String] = if a_comps.len() >= root_comps.len()
        && root_comps.iter().zip(a_comps.iter()).all(|(r, a)| r == a)
    {
        &a_comps[root_comps.len()..]
    } else {
        &a_comps[..]
    };

    let rel_b: &[String] = if b_comps.len() >= root_comps.len()
        && root_comps.iter().zip(b_comps.iter()).all(|(r, b)| r == b)
    {
        &b_comps[root_comps.len()..]
    } else {
        &b_comps[..]
    };

    for (ca, cb) in rel_a.iter().zip(rel_b.iter()) {
        let ord = ca.cmp(cb);
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    rel_a.len().cmp(&rel_b.len())
}

/// Discover all subdirectories under root_folder for the Folder Tree view.
#[allow(dead_code)]
pub fn discover_subfolders(root_folder: &str, all_files: &[String]) -> Vec<String> {
    let root = Path::new(root_folder);
    let norm_root = normalize_path(root_folder);
    let mut subdirs: HashSet<String> = HashSet::new();

    for f in all_files {
        let mut curr = Path::new(f).parent();
        while let Some(p) = curr {
            let norm_p = normalize_path(&p.to_string_lossy());
            if norm_p.to_lowercase() == norm_root.to_lowercase() || norm_p.len() <= norm_root.len()
            {
                break;
            }
            subdirs.insert(norm_p);
            curr = p.parent();
        }
    }

    for entry in WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || e.file_name()
                    .to_str()
                    .map(|s| !s.starts_with('.'))
                    .unwrap_or(true)
        })
        .flatten()
    {
        if entry.file_type().is_dir() {
            subdirs.insert(normalize_path(&entry.path().to_string_lossy()));
        }
    }

    let mut sorted: Vec<String> = subdirs.into_iter().collect();
    sorted.sort_by(|a, b| compare_paths_hierarchical(&norm_root, a, b));
    sorted
}

/// Single-pass high-speed scanner that collects all supported image files and subdirectories simultaneously.
pub fn scan_folder_and_subfolders(folder: &str) -> (Vec<String>, Vec<String>) {
    let root = Path::new(folder);
    if !root.is_dir() {
        return (Vec::new(), Vec::new());
    }
    let norm_root = normalize_path(folder);
    let mut files = Vec::new();
    let mut subdirs: HashSet<String> = HashSet::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || e.file_name()
                    .to_str()
                    .map(|s| !s.starts_with('.'))
                    .unwrap_or(true)
        })
        .flatten()
    {
        let p = entry.path();
        if entry.file_type().is_dir() {
            let norm_p = normalize_path(&p.to_string_lossy());
            if norm_p.to_lowercase() != norm_root.to_lowercase() && norm_p.len() > norm_root.len() {
                subdirs.insert(norm_p);
            }
        } else if entry.file_type().is_file() && is_supported_file(p) {
            files.push(normalize_path(&p.to_string_lossy()));
        }
    }

    files.sort_by_key(|a| a.to_lowercase());

    let mut sorted_subdirs: Vec<String> = subdirs.into_iter().collect();
    sorted_subdirs.sort_by(|a, b| compare_paths_hierarchical(&norm_root, a, b));

    (files, sorted_subdirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hierarchical_path_sorting() {
        let root = "C:\\Photos";
        let mut paths = vec![
            "C:\\Photos\\2025\\Trip".to_string(),
            "C:\\Photos\\2024".to_string(),
            "C:\\Photos\\2024\\Vacation\\Beach".to_string(),
            "C:\\Photos\\2024\\Vacation".to_string(),
            "C:\\Photos\\2025".to_string(),
        ];
        paths.sort_by(|a, b| compare_paths_hierarchical(root, a, b));
        assert_eq!(
            paths,
            vec![
                "C:\\Photos\\2024",
                "C:\\Photos\\2024\\Vacation",
                "C:\\Photos\\2024\\Vacation\\Beach",
                "C:\\Photos\\2025",
                "C:\\Photos\\2025\\Trip",
            ]
        );
    }

    #[test]
    fn scan_works_inside_hidden_root_but_skips_hidden_children() {
        let root = std::env::temp_dir().join(format!(".mriv-hidden-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        for f in [
            "a.jpg",
            "sub/b.png",
            ".cache/c.jpg",
            "notes.txt",
            ".a.jpg.mrtmp-1",
        ] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        let (files, subs) = scan_folder_and_subfolders(&root.to_string_lossy());
        let names: Vec<String> = files
            .iter()
            .map(|f| {
                Path::new(f)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["a.jpg".to_string(), "b.png".to_string()]);
        assert_eq!(subs.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_keys_follow_platform_case_rules() {
        assert!(same_path("C:/Photos/A.JPG", "C:/Photos/A.JPG"));
        assert_eq!(same_path("/photos/A.jpg", "/photos/a.jpg"), cfg!(windows));
        assert!(is_supported_file(Path::new("x.CR3")));
        assert!(is_raw_file(Path::new("x.nef")));
        assert!(!is_supported_file(Path::new("x.jpg.mrtmp-12")));
        assert!(dialog_extensions().contains(&"jpg") && dialog_extensions().contains(&"cr3"));
    }

    #[test]
    fn ratings_are_set_in_memory_and_renamed() {
        let mut s = AppSettings::default();
        s.set_rating("/p/a.jpg", 4);
        assert_eq!(s.get_rating("/p/a.jpg"), 4);
        s.rename_rating("/p/a.jpg", "/p/b.jpg");
        assert_eq!(s.get_rating("/p/a.jpg"), 0);
        assert_eq!(s.get_rating("/p/b.jpg"), 4);
        s.set_rating("/p/b.jpg", 9);
        assert_eq!(s.get_rating("/p/b.jpg"), 5);
        s.set_rating("/p/b.jpg", 0);
        assert!(s.ratings.is_empty());
    }

    #[test]
    fn legacy_settings_are_copied_once() {
        let base = std::env::temp_dir().join(format!("horama-cfg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let old = settings_path_in(&base, "Horama");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, b"{\"old\":1}").unwrap();

        assert_eq!(migrate_legacy_settings(&base), Some(old.clone()));
        let new = settings_path_in(&base, APP_NAME);
        assert_eq!(fs::read(&new).unwrap(), b"{\"old\":1}");
        assert!(old.is_file(), "the old settings are left in place");

        // Settings under the new name are never overwritten.
        fs::write(&new, b"{}").unwrap();
        assert_eq!(migrate_legacy_settings(&base), None);
        assert_eq!(fs::read(&new).unwrap(), b"{}");
        let _ = fs::remove_dir_all(&base);
    }
}
