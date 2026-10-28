#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cache_manager;
mod color_manager;
mod config;
mod cr3_parser;
mod disk_ops;
mod exif_orient;
mod file_watcher;
mod image_loader;
mod image_saver;
mod mcp_server;
mod metadata;
mod open_with;
mod rename;

use mcp_server::{McpCommand, run_mcp_stdio_server};
use serde_json::{json, Value};

slint::include_modules!();

use crate::cache_manager::{PreloadController, PreloadLoader};
use crate::color_manager::{rgb_to_hsl, ColorManager, ColorTransformInfo};
use crate::config::{
    normalize_path, path_key, same_path, scan_folder_and_subfolders, AppSettings, BUILTIN_PROFILES,
    DEFAULT_FALLBACK_PROFILE, PROFILE_EMBEDDED, PROFILE_LOAD_CUSTOM,
};
use crate::disk_ops::DiskQueue;
use crate::image_loader::{load_thumbnail_rgba, DecodedImage};
use crate::image_saver::{build_export_exif, generate_next_sequential_path, save_image_with_icc};
use crate::metadata::{extract_metadata, render_histogram_rgba};
#[cfg(test)]
use crate::rename::compute_batch_rename_plan;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

type StateArc = Arc<Mutex<ViewerState>>;

/// Locks the shared state. A poisoned lock (a panic while it was held) is recovered instead of
/// cascading the panic into every later callback.
fn lock_state(s: &Mutex<ViewerState>) -> std::sync::MutexGuard<'_, ViewerState> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

struct ShortcutDef {
    id: &'static str,
    name: &'static str,
    default_key: &'static str,
}

const SHORTCUT_DEFS: &[ShortcutDef] = &[
    ShortcutDef { id: "undo", name: "Undo Last Edit", default_key: "Ctrl+Z" },
    ShortcutDef { id: "redo", name: "Redo Last Edit", default_key: "Ctrl+Y" },
    ShortcutDef { id: "zoom_in", name: "Zoom In", default_key: "+" },
    ShortcutDef { id: "zoom_out", name: "Zoom Out", default_key: "-" },
    ShortcutDef { id: "zoom_fit", name: "Fit to Window", default_key: "Ctrl+0" },
    ShortcutDef { id: "zoom_100", name: "Actual Size (100%)", default_key: "Ctrl+1" },
    ShortcutDef { id: "rotate_cw", name: "Rotate Clockwise (90°)", default_key: "R" },
    ShortcutDef { id: "rotate_ccw", name: "Rotate Counter-Clockwise (90°)", default_key: "L" },
    ShortcutDef { id: "toggle_crop", name: "Crop Tool", default_key: "C" },
    ShortcutDef { id: "next_image", name: "Next Image", default_key: "Right" },
    ShortcutDef { id: "prev_image", name: "Previous Image", default_key: "Left" },
    ShortcutDef { id: "jump_forward_10", name: "Jump Forward 10 Images", default_key: "Up" },
    ShortcutDef { id: "jump_backward_10", name: "Jump Backward 10 Images", default_key: "Down" },
    ShortcutDef { id: "cycle_profile", name: "Cycle Color Profile", default_key: "P" },
    ShortcutDef { id: "open_shortcuts", name: "Keyboard Shortcuts Dialog", default_key: "S" },
    ShortcutDef { id: "rename", name: "Rename Image", default_key: "F2" },
    ShortcutDef { id: "delete", name: "Delete Image", default_key: "Delete" },
    ShortcutDef { id: "refresh", name: "Refresh Current Folder", default_key: "F5" },
    ShortcutDef { id: "open_file", name: "Open File(s)...", default_key: "Ctrl+O" },
    ShortcutDef { id: "open_folder", name: "Open Folder...", default_key: "Ctrl+Shift+O" },
    ShortcutDef { id: "save", name: "Save / Export...", default_key: "Ctrl+S" },
];


/// Looks up the action bound to a key combination. Bindings are checked in the fixed order of
/// `SHORTCUT_DEFS`, so the result is deterministic even if two actions share a combination.
fn action_for_shortcut(settings: &AppSettings, pressed: &str) -> String {
    let mut pressed_trim = pressed.trim().to_string();
    let lower = pressed_trim.to_lowercase();
    if lower.starts_with("cmd+") {
        pressed_trim = format!("ctrl+{}", &pressed_trim[4..]);
    } else if lower.starts_with("meta+") {
        pressed_trim = format!("ctrl+{}", &pressed_trim[5..]);
    }
    // Windows delivers Ctrl+letter as ASCII control codes (Ctrl+A = \u{1} ... Ctrl+Z = \u{1a}).
    let lower = pressed_trim.to_lowercase();
    for prefix in ["ctrl+shift+", "ctrl+"] {
        if lower.starts_with(prefix) && pressed_trim.len() > prefix.len() {
            let ch = pressed_trim[prefix.len()..].chars().next().unwrap_or('\0');
            if (1..=26).contains(&(ch as u32)) {
                pressed_trim = format!("{}{}", prefix, ((ch as u8) - 1 + b'a') as char);
            }
            break;
        }
    }
    let p_lower = pressed_trim.to_lowercase();

    for def in SHORTCUT_DEFS {
        let Some(combo) = settings.shortcuts.get(def.id) else { continue };
        let c_lower = combo.trim().to_lowercase();
        if c_lower.is_empty() {
            continue;
        }
        if c_lower == p_lower
            || (c_lower.len() == 1 && p_lower == format!("shift+{}", c_lower))
            || (c_lower == "+" && matches!(p_lower.as_str(), "=" | "+" | "shift+=" | "shift++"))
            || (c_lower == "-" && matches!(p_lower.as_str(), "_" | "-" | "shift+_" | "shift+-"))
        {
            return def.id.to_string();
        }
    }
    // Historical behaviour: Space also fits the image when "Fit to Window" keeps its default key.
    if p_lower == "space" {
        if let Some(fit) = settings.shortcuts.get("zoom_fit") {
            if fit.trim().eq_ignore_ascii_case("ctrl+0") {
                return "zoom_fit".to_string();
            }
        }
    }
    String::new()
}

/// Filmstrip badge kinds (the UI receives `kind()` / `text()` strings).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Badge {
    None,
    Deleted,
    Renamed,
    New,
    Modified,
}

impl Badge {
    fn kind(self) -> &'static str {
        match self {
            Badge::None => "",
            Badge::Deleted => "deleted",
            Badge::Renamed => "renamed",
            Badge::New => "new",
            Badge::Modified => "modified",
        }
    }
    fn text(self) -> &'static str {
        match self {
            Badge::None => "",
            Badge::Deleted => "🗑 Deleted",
            Badge::Renamed => "✏️ Renamed",
            Badge::New => "✨ New",
            Badge::Modified => "🔄 Modified",
        }
    }
    fn overlay_text(self) -> &'static str {
        match self {
            Badge::None => "",
            Badge::Deleted => "⚠️ Deleted from disk",
            Badge::Renamed => "✏️ Renamed",
            Badge::New => "✨ New",
            Badge::Modified => "🔄 Modified",
        }
    }
    fn from_kind(k: &str) -> Badge {
        match k {
            "deleted" => Badge::Deleted,
            "renamed" => Badge::Renamed,
            "new" => Badge::New,
            "modified" => Badge::Modified,
            _ => Badge::None,
        }
    }
}

fn set_item_badge(item: &mut FilmstripItemData, badge: Badge) {
    item.badge_type = badge.kind().into();
    item.badge_text = badge.text().into();
}

/// Shows the overlay notice for the image on the canvas.
fn show_current_badge(ui: &AppWindow, badge: Badge) {
    ui.set_current_file_deleted(badge == Badge::Deleted);
    ui.set_current_file_renamed(badge == Badge::Renamed);
    ui.set_current_file_new(badge == Badge::New);
    ui.set_current_file_modified(badge == Badge::Modified);
    ui.set_current_file_badge_text(badge.overlay_text().into());
}

#[derive(Clone)]
enum HistoryAction {
    ImageSelection {
        from_idx: usize,
        to_idx: usize,
    },
    ColorProfile {
        old_profile: String,
        new_profile: String,
    },
    Rating {
        path: String,
        old_rating: i32,
        new_rating: i32,
    },
    /// Lossless 90° rotation (no pixel copies are kept: it is undone by rotating back).
    Rotate {
        path: String,
        cw: bool,
    },
    /// Unsaved in-memory edit (crop). Undo/redo never touch the file on disk.
    ImageEdit {
        path: String,
        before: Arc<DecodedImage>,
        after: Arc<DecodedImage>,
        before_cropped: bool,
        after_cropped: bool,
        desc: String,
    },
}

/// Maximum number of undo entries that hold full image copies (crops).
const MAX_PIXEL_HISTORY: usize = 8;
const MAX_HISTORY: usize = 60;

struct ViewerState {
    settings: AppSettings,
    color_manager: ColorManager,
    preloader: PreloadController,
    disk: DiskQueue,
    thumbnail_generation: Arc<AtomicU64>,
    history_undo: Vec<HistoryAction>,
    history_redo: Vec<HistoryAction>,
    is_undoing_redoing: bool,
    /// Unsaved edits (crops), kept outside the evictable RAM cache so they can never be lost.
    edited_images: HashMap<String, Arc<DecodedImage>>,
    filter_is_cropped: bool,
    filter_is_profile_modified: bool,

    current_folder: String,
    is_files_mode: bool,
    all_folder_files: Vec<String>,
    subfolders: Vec<String>,
    selected_tree_filter: String,
    visible_files: Vec<String>,
    current_index: Option<usize>,
    selected_indices: std::collections::BTreeSet<usize>,
    selection_anchor: Option<usize>,

    current_decoded: Option<Arc<DecodedImage>>,
    /// Pixels currently on screen (monitor color space); shared with the Slint image.
    rendered: Option<SharedPixelBuffer<Rgb8Pixel>>,
    /// Pixel values in the document color space (Convert mode); `None` = source pixels.
    document_rgb: Option<Arc<Vec<u8>>>,
    /// Incremented for every display request; stale background results are discarded.
    load_request_id: u64,
    scan_generation: u64,
    display_scale: f32,
    aspect_cache: Arc<Mutex<HashMap<String, f32>>>,
    aspect_job_running: bool,
    /// Rotations queued for writing, per file.
    pending_rotations: HashMap<String, usize>,

    selected_profile: String,
    lock_profile: bool,
    lock_zoom: bool,
    is_fit_mode: bool,
    image_profile_overrides: HashMap<String, String>,

    zoom_factor: f32,
    pan_x: f32,
    pan_y: f32,
    folder_watcher: Option<crate::file_watcher::FolderWatcher>,
    expanded_folders: HashSet<String>,
    filter_search_text: String,
    filter_rating: i32,
    filter_format: String,
    filter_color_profile: String,
    filter_aspect_ratio: String,
    filter_ratings_mask: u8,

    ui_weak: Option<slint::Weak<AppWindow>>,
    self_weak: Option<Weak<Mutex<ViewerState>>>,
}

impl ViewerState {
    fn push_undo(&mut self, action: HistoryAction) {
        if self.is_undoing_redoing {
            return;
        }
        let is_pixel = matches!(action, HistoryAction::ImageEdit { .. });
        self.history_undo.push(action);
        if is_pixel {
            let pixel_entries = self.history_undo.iter().filter(|a| matches!(a, HistoryAction::ImageEdit { .. })).count();
            if pixel_entries > MAX_PIXEL_HISTORY {
                if let Some(pos) = self.history_undo.iter().position(|a| matches!(a, HistoryAction::ImageEdit { .. })) {
                    self.history_undo.remove(pos);
                }
            }
        }
        if self.history_undo.len() > MAX_HISTORY {
            self.history_undo.remove(0);
        }
        self.history_redo.clear();
    }

    fn push_redo(&mut self, action: HistoryAction) {
        self.history_redo.push(action);
        if self.history_redo.len() > MAX_HISTORY {
            self.history_redo.remove(0);
        }
    }

    fn is_cropped(&self, path: &str) -> bool {
        self.edited_images.contains_key(path)
    }

    /// Best in-memory version of an image: unsaved edit first, then the RAM cache.
    fn in_memory(&self, path: &str) -> Option<Arc<DecodedImage>> {
        if let Some(e) = self.edited_images.get(path) {
            return Some(e.clone());
        }
        self.preloader.cache.lock().ok().and_then(|mut g| g.get(path))
    }

    fn new() -> Self {
        let mut settings = AppSettings::load();
        let mut color_manager = ColorManager::new();

        let mut valid_custom = Vec::new();
        for cp in &settings.custom_profiles {
            let p = Path::new(cp);
            if p.is_file() && color_manager.register_custom_profile(p).is_some() {
                valid_custom.push(cp.clone());
            }
        }
        settings.custom_profiles = valid_custom;

        Self {
            settings,
            color_manager,
            preloader: PreloadController::new(),
            disk: DiskQueue::new(),
            thumbnail_generation: Arc::new(AtomicU64::new(1)),
            history_undo: Vec::new(),
            history_redo: Vec::new(),
            is_undoing_redoing: false,
            edited_images: HashMap::new(),
            filter_is_cropped: false,
            filter_is_profile_modified: false,
            current_folder: String::new(),
            is_files_mode: false,
            all_folder_files: Vec::new(),
            subfolders: Vec::new(),
            selected_tree_filter: "__ALL__".to_string(),
            visible_files: Vec::new(),
            current_index: None,
            selected_indices: std::collections::BTreeSet::new(),
            selection_anchor: None,
            current_decoded: None,
            rendered: None,
            document_rgb: None,
            load_request_id: 0,
            scan_generation: 0,
            display_scale: 1.0,
            aspect_cache: Arc::new(Mutex::new(HashMap::new())),
            aspect_job_running: false,
            pending_rotations: HashMap::new(),
            selected_profile: DEFAULT_FALLBACK_PROFILE.to_string(),
            lock_profile: false,
            lock_zoom: false,
            is_fit_mode: true,
            image_profile_overrides: HashMap::new(),
            zoom_factor: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            folder_watcher: None,
            expanded_folders: HashSet::new(),
            filter_search_text: String::new(),
            filter_rating: 0,
            filter_format: "ALL".to_string(),
            filter_color_profile: "ALL".to_string(),
            filter_aspect_ratio: "ALL".to_string(),
            filter_ratings_mask: 0,
            ui_weak: None,
            self_weak: None,
        }
    }
}

/// Runs `f` on the UI thread with the UI and the locked state (no-op if either is gone).
fn post_to_ui<F>(ui_weak: slint::Weak<AppWindow>, state_weak: Weak<Mutex<ViewerState>>, f: F)
where
    F: FnOnce(&AppWindow, &mut ViewerState) + Send + 'static,
{
    let _ = slint::invoke_from_event_loop(move || run_with_state(ui_weak, state_weak, f));
}

fn run_with_state<F>(ui_weak: slint::Weak<AppWindow>, state_weak: Weak<Mutex<ViewerState>>, f: F)
where
    F: FnOnce(&AppWindow, &mut ViewerState) + Send + 'static,
{
    let Some(ui) = ui_weak.upgrade() else { return };
    let Some(st_arc) = state_weak.upgrade() else { return };
    let mut guard = match st_arc.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            // The state is held further up this thread's stack (e.g. a native dialog is
            // running a nested event loop): try again shortly instead of deadlocking.
            drop(ui);
            slint::Timer::single_shot(Duration::from_millis(15), move || run_with_state(ui_weak, state_weak, f));
            return;
        }
    };
    f(&ui, &mut guard);
}

fn weak_handles(state: &ViewerState) -> Option<(slint::Weak<AppWindow>, Weak<Mutex<ViewerState>>)> {
    Some((state.ui_weak.clone()?, state.self_weak.clone()?))
}

/// Comparison key for "is inside this folder" checks (`normalize_path` drops trailing
/// separators, so the separator is appended after normalizing).
fn dir_prefix_key(dir: &str) -> String {
    let k = path_key(dir);
    if k.ends_with(std::path::MAIN_SEPARATOR) {
        k // drive root such as "C:\"
    } else {
        format!("{}{}", k, std::path::MAIN_SEPARATOR)
    }
}

fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

pub fn validate_shortcut_combo(input: &str) -> (bool, String) {
    let raw = input.trim();
    if raw.is_empty() {
        return (true, String::new());
    }

    // Direct standalone "+" or "=" key (Zoom In)
    if raw == "+" || raw == "=" {
        return (true, "+".to_string());
    }

    // Handle combinations ending in "+" like "Ctrl++" or "Ctrl+Shift++"
    let (prefix, key_override) = if raw.ends_with("++") {
        (&raw[..raw.len() - 1], Some("+"))
    } else {
        (raw, None)
    };

    let parts: Vec<&str> = prefix.split('+').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let mut has_ctrl = false;
    let mut has_shift = false;
    let mut has_alt = false;
    let mut key_part: Option<String> = key_override.map(|s| s.to_string());

    for part in parts {
        let p_lower = part.to_lowercase();
        if p_lower == "ctrl" || p_lower == "control" || p_lower == "cmd" || p_lower == "command" || p_lower == "meta" {
            has_ctrl = true;
        } else if p_lower == "shift" {
            has_shift = true;
        } else if p_lower == "alt" || p_lower == "opt" || p_lower == "option" {
            has_alt = true;
        } else {
            if key_part.is_some() {
                return (false, raw.to_string());
            }
            key_part = Some(part.to_string());
        }
    }

    let Some(k) = key_part else {
        return (false, raw.to_string());
    };

    let k_lower = k.to_lowercase();
    let canonical_key = match k_lower.as_str() {
        "f1" => "F1", "f2" => "F2", "f3" => "F3", "f4" => "F4",
        "f5" => "F5", "f6" => "F6", "f7" => "F7", "f8" => "F8",
        "f9" => "F9", "f10" => "F10", "f11" => "F11", "f12" => "F12",
        "left" | "leftarrow" => "Left",
        "right" | "rightarrow" => "Right",
        "up" | "uparrow" => "Up",
        "down" | "downarrow" => "Down",
        "delete" | "del" => "Delete",
        "backspace" | "back" => "Backspace",
        "home" => "Home",
        "end" => "End",
        "pageup" | "pgup" => "PageUp",
        "pagedown" | "pgdn" => "PageDown",
        "enter" | "return" => "Enter",
        "esc" | "escape" => "Escape",
        "space" => "Space",
        "tab" => "Tab",
        "+" | "plus" => "+",
        "-" | "minus" => "-",
        "=" => "=",
        s if s.chars().count() == 1 => {
            s
        }
        _ => return (false, raw.to_string()),
    };

    let mut result = String::new();
    if has_ctrl {
        result.push_str("Ctrl+");
    }
    if has_alt {
        result.push_str("Alt+");
    }
    if has_shift {
        result.push_str("Shift+");
    }
    if canonical_key.chars().count() == 1 && canonical_key.chars().next().unwrap().is_alphabetic() {
        result.push_str(&canonical_key.to_uppercase());
    } else {
        result.push_str(canonical_key);
    }

    (true, result)
}

fn populate_shortcuts_ui(ui: &AppWindow, settings: &AppSettings) {
    let mut items = Vec::new();
    for def in SHORTCUT_DEFS {
        let key = settings.shortcuts.get(def.id).cloned().unwrap_or_else(|| def.default_key.to_string());
        let (is_valid, _) = validate_shortcut_combo(&key);
        items.push(ShortcutItemData {
            id: def.id.into(),
            name: def.name.into(),
            key_combo: key.into(),
            is_valid,
        });
    }
    ui.set_shortcut_items(ModelRc::new(VecModel::from(items)));
}

fn rgba_to_slint_image(width: u32, height: u32, rgba: &[u8]) -> Image {
    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    buf.make_mut_bytes().copy_from_slice(rgba);
    Image::from_rgba8(buf)
}

fn rebuild_profile_combobox_list(ui: &AppWindow, state: &ViewerState) {
    let mut list: Vec<SharedString> = BUILTIN_PROFILES
        .iter()
        .map(|&s| SharedString::from(s))
        .collect();
    for (label, _) in state.color_manager.get_custom_profiles() {
        list.push(SharedString::from(label));
    }
    list.push(SharedString::from(PROFILE_LOAD_CUSTOM));
    ui.set_profile_list(ModelRc::new(VecModel::from(list)));
}

fn relative_depth(root: &str, path: &str) -> i32 {
    let p_root = Path::new(root);
    let p_path = Path::new(path);
    if let Ok(rel) = p_path.strip_prefix(p_root) {
        return rel.components().count().max(1) as i32;
    }
    let root_comps: Vec<String> = p_root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let path_comps: Vec<String> = p_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if path_comps.len() > root_comps.len() {
        let is_prefix = root_comps.iter().zip(path_comps.iter()).all(|(rc, pc)| rc == pc);
        if is_prefix {
            return (path_comps.len() - root_comps.len()) as i32;
        }
    }
    1
}

fn are_ancestors_expanded(path: &str, root_norm: &str, expanded: &std::collections::HashSet<String>) -> bool {
    let mut current = Path::new(path);
    while let Some(parent) = current.parent() {
        let norm_parent = normalize_path(&parent.to_string_lossy());
        if norm_parent.eq_ignore_ascii_case(root_norm) {
            return true;
        }
        if !expanded.iter().any(|e| e.eq_ignore_ascii_case(&norm_parent)) {
            return false;
        }
        current = parent;
    }
    true
}

fn expand_ancestors_and_subfolder(state: &mut ViewerState, subfolder: &str) {
    let norm_sub = normalize_path(subfolder);
    let norm_root = normalize_path(&state.current_folder);
    let mut cur = Path::new(&norm_sub);
    while let Some(parent) = cur.parent() {
        let p_norm = normalize_path(&parent.to_string_lossy());
        if p_norm.len() < norm_root.len() {
            break;
        }
        state.expanded_folders.insert(p_norm.clone());
        if p_norm.eq_ignore_ascii_case(&norm_root) {
            break;
        }
        cur = parent;
    }
    state.expanded_folders.insert(norm_sub);
}

fn refresh_folder_tree_ui(ui: &AppWindow, state: &ViewerState) {
    if state.current_folder.is_empty() {
        ui.set_has_subfolders(false);
        ui.set_folder_tree_items(ModelRc::new(VecModel::default()));
        return;
    }

    ui.set_has_subfolders(true);
    let root_norm = normalize_path(&state.current_folder);
    let root_name = Path::new(&root_norm)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| root_norm.clone());

    let root_has_children = !state.subfolders.is_empty();
    let root_is_expanded = state.expanded_folders.contains("__ALL__")
        || state.expanded_folders.iter().any(|e| e.eq_ignore_ascii_case(&root_norm))
        || state.expanded_folders.is_empty();

    let mut items = Vec::new();
    items.push(TreeItemData {
        label: format!("{} ({})", root_name, state.all_folder_files.len()).into(),
        path_key: "__ALL__".into(),
        depth: 0,
        selected: state.selected_tree_filter == "__ALL__",
        has_children: root_has_children,
        is_expanded: root_is_expanded,
    });

    if root_is_expanded && !state.subfolders.is_empty() {
        let root_only_count = state
            .all_folder_files
            .iter()
            .filter(|f| {
                Path::new(f)
                    .parent()
                    .map(|p| normalize_path(&p.to_string_lossy()).eq_ignore_ascii_case(&root_norm))
                    .unwrap_or(false)
            })
            .count();

        items.push(TreeItemData {
            label: format!("[Direct Files] ({})", root_only_count).into(),
            path_key: "__ROOT_ONLY__".into(),
            depth: 1,
            selected: state.selected_tree_filter == "__ROOT_ONLY__",
            has_children: false,
            is_expanded: false,
        });

        for sub in &state.subfolders {
            if !are_ancestors_expanded(sub, &root_norm, &state.expanded_folders) {
                continue;
            }

            let sub_name = Path::new(sub)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| sub.clone());
            let depth = relative_depth(&root_norm, sub);
            let prefix = format!("{}{}", sub, std::path::MAIN_SEPARATOR).to_lowercase();
            let sub_low = sub.to_lowercase();
            let count = state
                .all_folder_files
                .iter()
                .filter(|f| {
                    let fl = f.to_lowercase();
                    fl.starts_with(&prefix)
                        || Path::new(f)
                            .parent()
                            .map(|p| normalize_path(&p.to_string_lossy()).to_lowercase() == sub_low)
                            .unwrap_or(false)
                })
                .count();

            let has_children = state.subfolders.iter().any(|other| {
                if other.eq_ignore_ascii_case(sub) {
                    return false;
                }
                if let Some(par) = Path::new(other).parent() {
                    normalize_path(&par.to_string_lossy()).eq_ignore_ascii_case(sub)
                } else {
                    false
                }
            });

            let is_expanded = state.expanded_folders.iter().any(|e| e.eq_ignore_ascii_case(sub));

            items.push(TreeItemData {
                label: format!("{} ({})", sub_name, count).into(),
                path_key: sub.clone().into(),
                depth,
                selected: state.selected_tree_filter.eq_ignore_ascii_case(sub),
                has_children,
                is_expanded,
            });
        }
    }

    ui.set_folder_tree_items(ModelRc::new(VecModel::from(items)));
}

fn update_cache_status_ui(ui: &AppWindow, state: &ViewerState) {
    if let Ok(guard) = state.preloader.cache.lock() {
        let (count, used, budget) = guard.stats();
        let total = state.visible_files.len();
        let used_mb = used as f64 / (1024.0 * 1024.0);
        let budget_mb = budget as f64 / (1024.0 * 1024.0);
        ui.set_status_cache_text(
            format!(
                "RAM Cache: {} / {} ({:.0} / {:.0} MB)",
                count, total, used_mb, budget_mb
            )
            .into(),
        );
    }
}

fn update_zoom_ui(ui: &AppWindow, state: &ViewerState) {
    ui.set_zoom_factor(state.zoom_factor);
    ui.set_pan_x(state.pan_x);
    ui.set_pan_y(state.pan_y);
    ui.set_zoom_text(format!("{:.0}%", state.zoom_factor * 100.0).into());
}

fn handle_filmstrip_click(
    ui: &AppWindow,
    state: &mut ViewerState,
    idx: usize,
    is_ctrl: bool,
    is_shift: bool,
) {
    let num_items = state.visible_files.len();
    if idx >= num_items {
        return;
    }

    if is_shift {
        let anchor = state.selection_anchor.or(state.current_index).unwrap_or(0);
        let start = anchor.min(idx);
        let end = anchor.max(idx);
        if !is_ctrl {
            state.selected_indices.clear();
        }
        for i in start..=end {
            state.selected_indices.insert(i);
        }
        select_image_by_index(ui, state, idx);
    } else if is_ctrl {
        if state.selected_indices.contains(&idx) {
            if state.selected_indices.len() > 1 {
                state.selected_indices.remove(&idx);
            }
        } else {
            state.selected_indices.insert(idx);
            state.selection_anchor = Some(idx);
            select_image_by_index(ui, state, idx);
        }
    } else {
        state.selected_indices.clear();
        state.selected_indices.insert(idx);
        state.selection_anchor = Some(idx);
        select_image_by_index(ui, state, idx);
    }

    sync_filmstrip_selection_ui(ui, state);
}

fn parse_aspect_ratio_str(s: &str) -> Option<f32> {
    let clean = s.trim();
    // Task 2: Strictly require 'x' / 'X' between two numbers, reject ':' and '/'
    if clean.is_empty() || clean.contains(':') || clean.contains('/') {
        return None;
    }
    if clean.contains('x') || clean.contains('X') {
        let parts: Vec<&str> = clean.split(|c| c == 'x' || c == 'X').collect();
        if parts.len() == 2 {
            if let (Ok(w), Ok(h)) = (parts[0].trim().parse::<f32>(), parts[1].trim().parse::<f32>()) {
                if w > 0.0 && h > 0.0 {
                    return Some(w.max(h) / w.min(h));
                }
            }
        }
    }
    None
}

fn ratio_matches_target(ratio: f32, target: f32) -> bool {
    let min_r = ratio.min(target);
    let max_r = ratio.max(target);
    if max_r <= 0.0 {
        return false;
    }
    (min_r / max_r) >= 0.99
}

fn matches_aspect_ratio_filter(ratio: f32, filter: &str, saved_customs: &[String]) -> bool {
    let standard_targets = [
        ("1x1", 1.0f32),
        ("3x2", 1.50f32),
        ("4x3", 4.0f32 / 3.0f32),
        ("5x4", 1.25f32),
        ("16x9", 16.0f32 / 9.0f32),
        ("21x9", 21.0f32 / 9.0f32),
        ("65x24", 65.0f32 / 24.0f32),
    ];

    match filter {
        "ALL" | "" => true,
        "1x1" | "1:1" => ratio_matches_target(ratio, 1.0),
        "3x2" | "3:2" => ratio_matches_target(ratio, 1.50),
        "4x3" | "4:3" => ratio_matches_target(ratio, 4.0 / 3.0),
        "5x4" | "5:4" => ratio_matches_target(ratio, 1.25),
        "16x9" | "16:9" => ratio_matches_target(ratio, 16.0 / 9.0),
        "21x9" | "21:9" => ratio_matches_target(ratio, 21.0 / 9.0),
        "65x24" | "65:24" => ratio_matches_target(ratio, 65.0 / 24.0),
        "CUSTOM" | "OTHER" => {
            let matches_any_standard = standard_targets.iter().any(|(_, t)| ratio_matches_target(ratio, *t));
            if matches_any_standard {
                return false;
            }
            let matches_any_saved = saved_customs.iter().any(|s| {
                parse_aspect_ratio_str(s).map(|t| ratio_matches_target(ratio, t)).unwrap_or(false)
            });
            !matches_any_saved
        }
        saved_str => {
            if let Some(target) = parse_aspect_ratio_str(saved_str) {
                ratio_matches_target(ratio, target)
            } else {
                true
            }
        }
    }
}

/// Render a 320x320 RGBA8 buffer for the Middle-Click Magnifier Loupe at 4x current zoom.
fn render_loupe_patch_rgba(
    center_x: f32,
    center_y: f32,
    effective_mag: f32,
    width: u32,
    height: u32,
    rgb_pixels: &[u8],
) -> (u32, u32, Vec<u8>) {
    let lens_size = 320u32;
    let mut out = vec![0u8; (lens_size * lens_size * 4) as usize];
    let half = lens_size as f32 * 0.5;
    let inv_mag = 1.0 / effective_mag.max(0.05);

    for ly in 0..lens_size {
        let src_y = center_y + (ly as f32 - half + 0.5) * inv_mag;
        let iy = src_y.floor() as i32;
        for lx in 0..lens_size {
            let src_x = center_x + (lx as f32 - half + 0.5) * inv_mag;
            let ix = src_x.floor() as i32;
            let dst_idx = ((ly * lens_size + lx) * 4) as usize;

            if ix >= 0 && iy >= 0 && (ix as u32) < width && (iy as u32) < height {
                let src_idx = (((iy as u32) * width + (ix as u32)) * 3) as usize;
                out[dst_idx] = rgb_pixels[src_idx];
                out[dst_idx + 1] = rgb_pixels[src_idx + 1];
                out[dst_idx + 2] = rgb_pixels[src_idx + 2];
                out[dst_idx + 3] = 255;
            } else {
                // Dark neutral border outside image bounds
                out[dst_idx] = 0x14;
                out[dst_idx + 1] = 0x14;
                out[dst_idx + 2] = 0x14;
                out[dst_idx + 3] = 255;
            }
        }
    }

    (lens_size, lens_size, out)
}

fn show_fatal_error_dialog(title: &str, details: &str) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide_msg: Vec<u16> = std::ffi::OsStr::new(details)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let wide_title: Vec<u16> = std::ffi::OsStr::new(title)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        extern "system" {
            fn MessageBoxW(hwnd: *mut std::ffi::c_void, lpText: *const u16, lpCaption: *const u16, uType: u32) -> i32;
        }
        unsafe {
            // MB_ICONERROR (0x10) | MB_OK (0x0) | MB_TOPMOST (0x40000)
            MessageBoxW(std::ptr::null_mut(), wide_msg.as_ptr(), wide_title.as_ptr(), 0x00000010 | 0x00040000);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let esc_t = title.replace('"', "\"");
        let esc_m = details.replace('"', "\"");
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(format!("display alert \"{}\" message \"{}\" as critical", esc_t, esc_m))
            .output();
        eprintln!("{}: {}", title, details);
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        eprintln!("{}: {}", title, details);
    }
}

fn refresh_multi_selection_overview(ui: &AppWindow, state: &ViewerState) {
    let count = state.selected_indices.len();
    if count <= 1 {
        ui.set_is_multi_selection(false);
        ui.set_multi_selection_tiles(ModelRc::new(VecModel::default()));
        ui.set_status_eyedropper_text("Eyedropper: R-Click | Loupe: M-Click".into());
        return;
    }

    let total = state.visible_files.len();
    let cur_name = state
        .current_index
        .and_then(|idx| state.visible_files.get(idx))
        .and_then(|p| Path::new(p).file_name().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_default();
    ui.set_status_eyedropper_text(
        format!("Selected: {} of {} files | Active: {}", count, total, cur_name).into(),
    );

    ui.set_is_multi_selection(true);

    let mut thumb_map: HashMap<usize, (Image, bool, String, String)> = HashMap::new();
    let film_model = ui.get_filmstrip_items();
    if let Some(vec_model) = film_model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
        for &idx in &state.selected_indices {
            if let Some(item) = vec_model.row_data(idx) {
                thumb_map.insert(
                    idx,
                    (
                        item.thumbnail,
                        item.has_thumbnail,
                        item.badge_text.to_string(),
                        item.badge_type.to_string(),
                    ),
                );
            }
        }
    }

    let cw = ui.get_canvas_width().max(320.0);
    let ch = ui.get_canvas_height().max(240.0);

    // Filmstrip thumbnail size is 140x95px.
    // Minimum multi-selection preview tile is 2.5x the thumbnail dimensions:
    // 140.0 * 2.5 = 350.0 px
    // 95.0 * 2.5 = 237.5 px (~238 px)
    let base_w = 350.0f32;
    let base_h = 238.0f32;
    let gap = 12.0f32;

    let eff_w = base_w.min((cw - 24.0).max(180.0));
    let eff_h = (eff_w * (base_h / base_w)).min((ch - 24.0).max(122.0));

    let cols = (((cw - gap) / (eff_w + gap)).floor() as usize).max(1);
    let rows = (((ch - gap) / (eff_h + gap)).floor() as usize).max(1);
    let max_slots = cols * rows;

    let total_sel = state.selected_indices.len();
    let (show_count, has_ellipsis) = if total_sel <= max_slots {
        (total_sel, false)
    } else {
        (max_slots - 1, true)
    };

    let active_slots = if has_ellipsis { max_slots } else { show_count };
    let grid_cols = cols.min(active_slots);
    let grid_rows = ((active_slots + cols - 1) / cols).max(1);

    let grid_w = grid_cols as f32 * eff_w + (grid_cols.saturating_sub(1)) as f32 * gap;
    let grid_h = grid_rows as f32 * eff_h + (grid_rows.saturating_sub(1)) as f32 * gap;
    let start_x = ((cw - grid_w) * 0.5).max(6.0);
    let start_y = ((ch - grid_h) * 0.5).max(6.0);

    let sel_indices_vec: Vec<usize> = state.selected_indices.iter().copied().collect();
    let mut tiles = Vec::with_capacity(active_slots);

    for slot_idx in 0..active_slots {
        let r = slot_idx / cols;
        let c = slot_idx % cols;
        let tx = start_x + c as f32 * (eff_w + gap);
        let ty = start_y + r as f32 * (eff_h + gap);

        if has_ellipsis && slot_idx == max_slots - 1 {
            tiles.push(MultiSelectTileData {
                full_path: "".into(),
                label: "".into(),
                thumbnail: Image::default(),
                has_thumbnail: false,
                is_ellipsis: true,
                ellipsis_text: format!("+{} more", total_sel - (max_slots - 1)).into(),
                rating: 0,
                badge_text: "".into(),
                badge_type: "".into(),
                tile_x: tx,
                tile_y: ty,
                tile_w: eff_w,
                tile_h: eff_h,
                is_cropped: false,
            });
        } else if let Some(&file_idx) = sel_indices_vec.get(slot_idx) {
            if let Some(fpath) = state.visible_files.get(file_idx) {
                let fname = Path::new(fpath)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| fpath.clone());
                let (thumb, has_thumb, b_text, b_type) = thumb_map
                    .get(&file_idx)
                    .cloned()
                    .unwrap_or_else(|| (Image::default(), false, String::new(), String::new()));
                let rating = state.settings.get_rating(fpath) as i32;

                tiles.push(MultiSelectTileData {
                    full_path: fpath.clone().into(),
                    label: fname.into(),
                    thumbnail: thumb,
                    has_thumbnail: has_thumb,
                    is_ellipsis: false,
                    ellipsis_text: "".into(),
                    rating,
                    badge_text: b_text.into(),
                    badge_type: b_type.into(),
                    tile_x: tx,
                    tile_y: ty,
                    tile_w: eff_w,
                    tile_h: eff_h,
                    is_cropped: state.is_cropped(fpath),
                });
            }
        }
    }

    ui.set_multi_selection_tiles(ModelRc::new(VecModel::from(tiles)));
}

fn sync_filmstrip_selection_ui(ui: &AppWindow, state: &ViewerState) {
    let filmstrip_model = ui.get_filmstrip_items();
    let mut thumb_map: HashMap<usize, (Image, bool, String, String)> = HashMap::new();
    if let Some(vec_model) =
        filmstrip_model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
    {
        for i in 0..vec_model.row_count() {
            if let Some(mut item) = vec_model.row_data(i) {
                let should_sel = state.selected_indices.contains(&i);
                if item.selected != should_sel {
                    item.selected = should_sel;
                    vec_model.set_row_data(i, item.clone());
                }
                if should_sel {
                    thumb_map.insert(
                        i,
                        (
                            item.thumbnail,
                            item.has_thumbnail,
                            item.badge_text.to_string(),
                            item.badge_type.to_string(),
                        ),
                    );
                }
            }
        }
    }

    let count = state.selected_indices.len();
    let _total = state.visible_files.len();
    ui.set_selected_files_count(count.max(if state.current_index.is_some() { 1 } else { 0 }) as i32);

    if let Some(cur_idx) = state.current_index {
        if let Some(p) = state.visible_files.get(cur_idx) {
            ui.set_current_rating(state.settings.get_rating(p) as i32);
        }
    }

    if count > 1 {
        refresh_multi_selection_overview(ui, state);
    } else {
        ui.set_is_multi_selection(false);
        ui.set_multi_selection_tiles(ModelRc::new(VecModel::default()));
        ui.set_status_eyedropper_text("Eyedropper: R-Click | Loupe: M-Click".into());
    }
}

fn jump_images(ui: &AppWindow, state: &mut ViewerState, offset: i32) {
    if state.visible_files.is_empty() {
        return;
    }
    let cur = state.current_index.unwrap_or(0) as i32;
    let total = state.visible_files.len() as i32;
    let new_idx = (cur + offset).clamp(0, total - 1) as usize;
    select_image_by_index(ui, state, new_idx);
}

fn refresh_recent_folders_ui(ui: &AppWindow, state: &ViewerState) {
    let mut items = Vec::new();
    for folder in &state.settings.recent_folders {
        let p = Path::new(folder);
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| folder.clone());
        let parent = p
            .parent()
            .map(|pr| pr.to_string_lossy().to_string())
            .unwrap_or_default();
        let active = !state.current_folder.is_empty() && same_path(folder, &state.current_folder);
        items.push(RecentFolderData {
            name: name.into(),
            parent_path: parent.into(),
            full_path: folder.clone().into(),
            active,
        });
    }
    ui.set_recent_folders(ModelRc::new(VecModel::from(items)));
}

/// Fit-to-window zoom, expressed in physical pixels per image pixel (see `display-scale`).
fn calculate_fit_zoom(ui: &AppWindow, img_width: u32, img_height: u32) -> f32 {
    let scale = ui.window().scale_factor().max(0.25);
    let cw = ui.get_canvas_width();
    let ch = ui.get_canvas_height();
    let (vw, vh) = if cw > 64.0 && ch > 64.0 {
        (cw, ch)
    } else {
        let win_size = ui.window().size();
        let w = if win_size.width > 100 { win_size.width as f32 / scale } else { 1440.0 };
        let h = if win_size.height > 100 { win_size.height as f32 / scale } else { 900.0 };
        ((w - 530.0).max(400.0), (h - 252.0).max(300.0))
    };
    let zx = (vw - 24.0).max(64.0) / (img_width.max(1) as f32);
    let zy = (vh - 24.0).max(64.0) / (img_height.max(1) as f32);
    (zx.min(zy) * scale).clamp(0.02, 32.0)
}

/// Image pixels per logical (UI) pixel.
fn view_zoom(state: &ViewerState) -> f32 {
    state.zoom_factor / state.display_scale.max(0.25)
}

/// Keeps the UI's display scale in sync with the window (moving between monitors).
fn sync_display_scale(ui: &AppWindow, state: &mut ViewerState) -> bool {
    let scale = ui.window().scale_factor().max(0.25);
    if (scale - state.display_scale).abs() > 0.001 {
        state.display_scale = scale;
        ui.set_display_scale(scale);
        return true;
    }
    false
}

/// Updates the thumbnail of the filmstrip row showing `path`. `idx_hint` is checked first;
/// if the row moved (filter, deletion…) the model is searched by path, so a late result can
/// never land on the wrong image.
fn set_thumbnail_for_path(ui: &AppWindow, idx_hint: Option<usize>, path: &str, img: Image, cropped: Option<bool>) {
    let model = ui.get_filmstrip_items();
    let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() else { return };
    let matches = |i: usize| vm.row_data(i).map(|it| same_path(it.full_path.as_str(), path)).unwrap_or(false);
    let row = match idx_hint {
        Some(i) if i < vm.row_count() && matches(i) => Some(i),
        _ => (0..vm.row_count()).find(|&i| matches(i)),
    };
    if let Some(i) = row {
        if let Some(mut item) = vm.row_data(i) {
            item.thumbnail = img;
            item.has_thumbnail = true;
            if let Some(c) = cropped {
                item.is_cropped = c;
            }
            vm.set_row_data(i, item);
        }
    }
}

/// Rebuilds the thumbnail of `path` from an in-memory image (after rotate/crop/undo).
fn refresh_thumbnail_from_memory(ui: &AppWindow, state: &ViewerState, path: &str, img: &DecodedImage) {
    if let Some((tw, th, rgba)) = crate::image_loader::create_thumbnail_from_decoded(img, Some(&state.color_manager)) {
        let idx = state.visible_files.iter().position(|f| f == path);
        set_thumbnail_for_path(ui, idx, path, rgba_to_slint_image(tw, th, &rgba), Some(state.is_cropped(path)));
    }
}

/// Reloads the thumbnail of `path` from disk in the background.
fn refresh_thumbnail_from_disk(state: &ViewerState, path: &str) {
    let Some((ui_w, st_w)) = weak_handles(state) else { return };
    let cm = state.color_manager.clone();
    let p = path.to_string();
    std::thread::spawn(move || {
        if let Some((tw, th, rgba)) = load_thumbnail_rgba(Path::new(&p), Some(&cm)) {
            post_to_ui(ui_w, st_w, move |ui, st| {
                let idx = st.visible_files.iter().position(|f| f == &p);
                if !st.is_cropped(&p) {
                    set_thumbnail_for_path(ui, idx, &p, rgba_to_slint_image(tw, th, &rgba), None);
                }
            });
        }
    });
}


// ----------------------------------------------------------------------------
// Background display pipeline (decode → color transform → histogram → metadata)
// ----------------------------------------------------------------------------

enum ProfileMode {
    /// Use exactly this profile.
    Fixed(String),
    /// Decide after decoding (lock / per-image override / embedded / default).
    Auto { lock: bool, current: String, override_profile: Option<String> },
}

struct DisplayRequest {
    req_id: u64,
    path: String,
    index: Option<usize>,
    source: Option<Arc<DecodedImage>>,
    profile_mode: ProfileMode,
    preserve_view: bool,
    need_meta: bool,
}

struct DisplayResult {
    index: Option<usize>,
    decoded: Arc<DecodedImage>,
    profile: String,
    lock_insert: bool,
    rendered: SharedPixelBuffer<Rgb8Pixel>,
    document: Option<Vec<u8>>,
    info: ColorTransformInfo,
    histogram: SharedPixelBuffer<Rgba8Pixel>,
    meta: Option<Vec<(String, String)>>,
    preserve_view: bool,
}

fn compute_display(req: DisplayRequest, cm: &ColorManager, loader: &PreloadLoader) -> Result<DisplayResult, String> {
    let decoded = match req.source {
        Some(d) => d,
        None => loader.get_or_load(&req.path)?,
    };
    let (profile, lock_insert) = match req.profile_mode {
        ProfileMode::Fixed(p) => (p, false),
        ProfileMode::Auto { lock, current, override_profile } => {
            if lock {
                (current, true)
            } else if let Some(o) = override_profile {
                (o, false)
            } else if decoded.embedded_icc_name.is_some() {
                (PROFILE_EMBEDDED.to_string(), false)
            } else {
                (DEFAULT_FALLBACK_PROFILE.to_string(), false)
            }
        }
    };
    let r = cm.render(&decoded.rgb_pixels, &profile, decoded.embedded_icc.as_deref());
    let mut rendered = SharedPixelBuffer::<Rgb8Pixel>::new(decoded.width, decoded.height);
    rendered.make_mut_bytes().copy_from_slice(&r.display);
    drop(r.display);
    // The histogram describes the document values, not the monitor-specific display values.
    let (hw, hh, hist_rgba) = render_histogram_rgba(r.document.as_deref().unwrap_or(&decoded.rgb_pixels));
    let mut histogram = SharedPixelBuffer::<Rgba8Pixel>::new(hw, hh);
    histogram.make_mut_bytes().copy_from_slice(&hist_rgba);
    let meta = req.need_meta.then(|| extract_metadata(&decoded));
    Ok(DisplayResult {
        index: req.index,
        decoded,
        profile,
        lock_insert,
        rendered,
        document: r.document,
        info: r.info,
        histogram,
        meta,
        preserve_view: req.preserve_view,
    })
}

/// Starts a display job on a worker thread. Only the most recent request is applied.
fn start_display_job(state: &mut ViewerState, mut req: DisplayRequest) {
    state.load_request_id += 1;
    req.req_id = state.load_request_id;
    let Some((ui_w, st_w)) = weak_handles(state) else { return };
    let cm = state.color_manager.clone();
    let loader = state.preloader.loader();
    let path = req.path.clone();
    let req_id = req.req_id;
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compute_display(req, &cm, &loader)))
            .unwrap_or_else(|_| Err("internal error while rendering".to_string()));
        post_to_ui(ui_w, st_w, move |ui, st| {
            if st.load_request_id != req_id {
                return; // a newer request superseded this one
            }
            match result {
                Ok(res) => apply_display_result(ui, st, res),
                Err(err) => {
                    ui.set_status_file_text(format!("Error loading {}: {}", file_name_of(&path), err).into());
                    if st.current_decoded.is_none() {
                        ui.set_has_image(false);
                        ui.set_canvas_empty_text(format!("⚠️ Cannot display this image: {}", err).into());
                    }
                }
            }
        });
    });
}

fn apply_display_result(ui: &AppWindow, state: &mut ViewerState, res: DisplayResult) {
    let decoded = res.decoded.clone();
    if res.lock_insert {
        state.image_profile_overrides.insert(decoded.path.clone(), res.profile.clone());
    }
    state.selected_profile = res.profile.clone();
    ui.set_current_profile(res.profile.clone().into());
    state.current_decoded = Some(decoded.clone());
    state.document_rgb = res.document.map(Arc::new);
    state.rendered = Some(res.rendered.clone());

    sync_display_scale(ui, state);
    ui.set_current_image(Image::from_rgb8(res.rendered));
    ui.set_image_pixel_width(decoded.width as i32);
    ui.set_image_pixel_height(decoded.height as i32);
    ui.set_has_image(true);
    ui.set_current_image_is_cropped(state.is_cropped(&decoded.path));

    if !res.preserve_view && !state.lock_zoom {
        state.zoom_factor = calculate_fit_zoom(ui, decoded.width, decoded.height);
        state.pan_x = 0.0;
        state.pan_y = 0.0;
        state.is_fit_mode = true;
    }
    update_zoom_ui(ui, state);

    let xfm_info = res.info;
    // Convert mode (embedded profile → chosen profile): the conversion keeps the colors, so
    // the picture looks the same on screen while the pixel values (and histogram) change.
    let converting = state.document_rgb.is_some();
    ui.set_has_embedded_icc(xfm_info.has_embedded);
    if let Some(emb_name) = &xfm_info.embedded_name {
        if converting {
            ui.set_embedded_badge_text(format!("Embedded: {} → Convert to {}", emb_name, state.selected_profile).into());
        } else {
            ui.set_embedded_badge_text(format!("Embedded: {}", emb_name).into());
        }
        ui.set_meta_embedded_icc(format!("Embedded ICC: {}", emb_name).into());
    } else {
        ui.set_embedded_badge_text(format!("No ICC (Default: {})", DEFAULT_FALLBACK_PROFILE).into());
        ui.set_meta_embedded_icc(format!("Embedded ICC: None (Default {})", DEFAULT_FALLBACK_PROFILE).into());
    }
    if converting {
        ui.set_meta_active_icc(
            format!("Active: Convert {} (same look on screen; pixel values & histogram change)", xfm_info.source_name).into(),
        );
    } else {
        ui.set_meta_active_icc(format!("Active Profile: {}", xfm_info.source_name).into());
    }
    ui.set_meta_display_icc(format!("Display Target: {}", xfm_info.display_name).into());
    ui.set_histogram_image(Image::from_rgba8(res.histogram));

    if let Some(meta_pairs) = res.meta {
        let meta_models: Vec<MetadataRowData> = meta_pairs
            .into_iter()
            .map(|(k, v)| MetadataRowData { key: k.into(), value: v.into() })
            .collect();
        ui.set_metadata_rows(ModelRc::new(VecModel::from(meta_models)));
    }

    let idx_num = state.current_index.map(|i| i + 1).unwrap_or(1);
    let total = state.visible_files.len();
    ui.set_status_file_text(
        format!(
            "[{}/{}] {} — {}×{} px ({}) | Profile: {}{}",
            idx_num,
            total,
            file_name_of(&decoded.path),
            decoded.width,
            decoded.height,
            decoded.format_name,
            if converting { "Convert " } else { "" },
            if converting {
                format!("{} — colors preserved on screen, pixel values converted", xfm_info.source_name)
            } else {
                xfm_info.source_name.clone()
            }
        )
        .into(),
    );
    update_cache_status_ui(ui, state);

    if let Some(index) = res.index {
        ui.invoke_ensure_filmstrip_item_visible(index as i32);
        start_neighborhood_preload(ui, state, index);
    }
}

fn start_neighborhood_preload(ui: &AppWindow, state: &ViewerState, index: usize) {
    let ui_weak = ui.as_weak();
    let cache_ref = state.preloader.cache.clone();
    let total_cnt = state.visible_files.len();
    state.preloader.preload_neighborhood(index, &state.visible_files, move || {
        let ui_c = ui_weak.clone();
        let c_ref = cache_ref.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_c.upgrade() {
                if let Ok(guard) = c_ref.lock() {
                    let (count, used, budget) = guard.stats();
                    ui.set_status_cache_text(
                        format!(
                            "RAM Cache: {} / {} ({:.0} / {:.0} MB)",
                            count,
                            total_cnt,
                            used as f64 / (1024.0 * 1024.0),
                            budget as f64 / (1024.0 * 1024.0)
                        )
                        .into(),
                    );
                }
            }
        });
    });
}

/// Re-renders the current image (profile change, edit, rename…) in the background.
fn request_render(state: &mut ViewerState, preserve_view: bool, need_meta: bool) {
    let Some(decoded) = state.current_decoded.clone() else { return };
    let req = DisplayRequest {
        req_id: 0,
        path: decoded.path.clone(),
        index: None,
        source: Some(decoded),
        profile_mode: ProfileMode::Fixed(state.selected_profile.clone()),
        preserve_view,
        need_meta,
    };
    start_display_job(state, req);
}

// ----------------------------------------------------------------------------
// Editing: lossless rotation, crop, undo/redo
// ----------------------------------------------------------------------------

/// Rotates `path` by 90° (in memory and, where possible, losslessly on disk).
fn rotate_path(ui: &AppWindow, state: &mut ViewerState, path: &str, cw: bool, record: bool) {
    // 1. In-memory version (current image, unsaved edit, or cache).
    let current_matches = state.current_decoded.as_ref().map(|d| d.path == path).unwrap_or(false);
    let in_mem = if current_matches { state.current_decoded.clone() } else { state.in_memory(path) };
    if let Some(dec) = in_mem {
        let rotated = Arc::new(if cw { dec.rotate_90_cw() } else { dec.rotate_90_ccw() });
        if state.edited_images.contains_key(path) {
            state.edited_images.insert(path.to_string(), rotated.clone());
        } else if let Ok(mut g) = state.preloader.cache.lock() {
            g.insert(path.to_string(), rotated.clone());
        }
        refresh_thumbnail_from_memory(ui, state, path, &rotated);
        if current_matches {
            state.current_decoded = Some(rotated);
            // Pixels on screen belong to the previous version until the new render arrives.
            state.rendered = None;
            state.document_rgb = None;
            request_render(state, false, true);
        }
    }
    if record {
        state.push_undo(HistoryAction::Rotate { path: path.to_string(), cw });
    }

    // 2. Persist losslessly (serial queue: rotations of the same file never interleave).
    let p = PathBuf::from(path);
    if !p.exists() {
        ui.set_status_file_text("Rotation shown only: the file is no longer on disk.".into());
        return;
    }
    if !crate::disk_ops::rotation_persist_supported(&p) {
        ui.set_status_file_text(
            format!("Rotation shown only: '{}' files are never modified (RAW/unsupported format).", p.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default()).into(),
        );
        return;
    }
    let Some((ui_w, st_w)) = weak_handles(state) else { return };
    let turns = if cw { 1 } else { 3 };
    let name = file_name_of(path);
    let path_owned = path.to_string();
    ui.set_status_file_text(format!("Saving rotation of {}…", name).into());
    *state.pending_rotations.entry(path.to_string()).or_insert(0) += 1;
    state.disk.submit(move || {
        let result = crate::disk_ops::persist_rotation(&p, turns);
        post_to_ui(ui_w, st_w, move |ui, st| {
            let left = match st.pending_rotations.get_mut(&path_owned) {
                Some(c) => {
                    *c = c.saturating_sub(1);
                    *c
                }
                None => 0,
            };
            if left == 0 {
                st.pending_rotations.remove(&path_owned);
            }
            let failed = result.is_err();
            match result {
                Ok(method) => ui.set_status_file_text(format!("Rotation saved: {} — {}", name, method).into()),
                Err(e) => {
                    ui.set_status_file_text(format!("Rotation NOT saved for {}: {}", name, e).into());
                    // The undo history no longer matches the file.
                    st.history_undo.retain(|a| !matches!(a, HistoryAction::Rotate { path, .. } if *path == path_owned));
                    st.history_redo.retain(|a| !matches!(a, HistoryAction::Rotate { path, .. } if *path == path_owned));
                    show_message_later(
                        "Rotation not saved",
                        &format!("The rotation of '{}' could not be saved:\n{}", name, e),
                        rfd::MessageLevel::Error,
                    );
                }
            }
            // Once every queued rotation of this file is done (or one failed), show exactly
            // what is on disk: this also corrects anything a preload or thumbnail job decoded
            // while the file was being rewritten.
            if left == 0 || failed {
                reconcile_with_disk(ui, st, &path_owned);
            }
        });
    });
}

/// Drops in-memory copies of `path` (unless it holds an unsaved edit) and reloads the file:
/// thumbnail, and the canvas if it is the current image.
fn reconcile_with_disk(_ui: &AppWindow, st: &mut ViewerState, path: &str) {
    if st.is_cropped(path) {
        return; // the unsaved edit in memory is authoritative
    }
    if let Ok(mut g) = st.preloader.cache.lock() {
        g.remove(path);
    }
    if let Ok(mut c) = st.aspect_cache.lock() {
        c.remove(&path_key(path));
    }
    refresh_thumbnail_from_disk(st, path);
    let is_current = st.current_decoded.as_ref().map(|d| d.path == path).unwrap_or(false);
    if is_current {
        let req = DisplayRequest {
            req_id: 0,
            path: path.to_string(),
            index: st.current_index,
            source: None,
            profile_mode: ProfileMode::Fixed(st.selected_profile.clone()),
            preserve_view: true,
            need_meta: true,
        };
        start_display_job(st, req);
    }
}

fn rotate_current_image(ui: &AppWindow, state: &mut ViewerState, cw: bool) {
    let Some(decoded) = state.current_decoded.clone() else { return };
    rotate_path(ui, state, &decoded.path.clone(), cw, true);
}

fn apply_crop(ui: &AppWindow, state: &mut ViewerState) {
    let Some(decoded) = state.current_decoded.clone() else { return };
    let path = decoded.path.clone();
    let was_cropped = state.is_cropped(&path);

    let norm_x = ui.get_crop_norm_x().clamp(0.0, 1.0);
    let norm_y = ui.get_crop_norm_y().clamp(0.0, 1.0);
    let norm_w = ui.get_crop_norm_w().clamp(0.02, 1.0);
    let norm_h = ui.get_crop_norm_h().clamp(0.02, 1.0);

    let crop_x = (norm_x * decoded.width as f32).round() as u32;
    let crop_y = (norm_y * decoded.height as f32).round() as u32;
    let crop_w = (norm_w * decoded.width as f32).round().max(1.0) as u32;
    let crop_h = (norm_h * decoded.height as f32).round().max(1.0) as u32;

    let new_arc = Arc::new(decoded.crop(crop_x, crop_y, crop_w, crop_h));
    state.push_undo(HistoryAction::ImageEdit {
        path: path.clone(),
        before: decoded.clone(),
        after: new_arc.clone(),
        before_cropped: was_cropped,
        after_cropped: true,
        desc: "Crop Image".to_string(),
    });
    state.edited_images.insert(path.clone(), new_arc.clone());
    state.current_decoded = Some(new_arc.clone());
    // Pixels on screen belong to the previous version until the new render arrives.
    state.rendered = None;
    state.document_rgb = None;
    ui.set_current_image_is_cropped(true);
    refresh_thumbnail_from_memory(ui, state, &path, &new_arc);

    // Cropping never writes to disk; use Save / Export (or "save all" on exit).
    ui.set_crop_active(false);
    ui.set_crop_norm_x(0.05);
    ui.set_crop_norm_y(0.05);
    ui.set_crop_norm_w(0.90);
    ui.set_crop_norm_h(0.90);

    request_render(state, true, true);
}

/// Restores an in-memory image state for undo/redo of an edit (never touches the disk).
fn restore_edit_state(ui: &AppWindow, state: &mut ViewerState, path: &str, image: Arc<DecodedImage>, cropped: bool) {
    // Snapshots taken before a rename still carry the old file name.
    let image = if image.path == path {
        image
    } else {
        let mut moved = (*image).clone();
        moved.path = path.to_string();
        Arc::new(moved)
    };
    if cropped {
        state.edited_images.insert(path.to_string(), image.clone());
    } else {
        state.edited_images.remove(path);
        if let Ok(mut g) = state.preloader.cache.lock() {
            g.insert(path.to_string(), image.clone());
        }
    }
    if state.current_decoded.as_ref().map(|d| d.path.as_str()) == Some(path) {
        ui.set_current_image_is_cropped(cropped);
        state.current_decoded = Some(image.clone());
        // Pixels on screen belong to the previous version until the new render arrives.
        state.rendered = None;
        state.document_rgb = None;
        request_render(state, true, true);
    }
    refresh_thumbnail_from_memory(ui, state, path, &image);
}

fn undo_last_operation(ui: &AppWindow, state: &mut ViewerState) {
    let Some(action) = state.history_undo.pop() else {
        ui.set_status_file_text("Nothing to undo".into());
        return;
    };
    state.is_undoing_redoing = true;
    state.push_redo(action.clone());

    match action {
        HistoryAction::ImageSelection { from_idx, .. } => {
            if from_idx < state.visible_files.len() {
                select_image_by_index(ui, state, from_idx);
                ui.set_status_file_text(format!("Undone image selection -> Image #{}", from_idx + 1).into());
            }
        }
        HistoryAction::ColorProfile { old_profile, .. } => {
            state.selected_profile = old_profile.clone();
            ui.set_current_profile(old_profile.clone().into());
            if let Some(curr) = state.current_decoded.as_ref().map(|d| d.path.clone()) {
                state.image_profile_overrides.insert(curr, old_profile.clone());
            }
            request_render(state, true, false);
            ui.set_status_file_text(format!("Undone color profile -> {}", old_profile).into());
        }
        HistoryAction::Rating { path, old_rating, .. } => {
            set_ratings(ui, state, &[path], old_rating as u8, false);
            ui.set_status_file_text(format!("Undone rating -> {} stars", old_rating).into());
        }
        HistoryAction::Rotate { path, cw } => {
            rotate_path(ui, state, &path, !cw, false);
            ui.set_status_file_text("Undone rotation".into());
        }
        HistoryAction::ImageEdit { path, before, before_cropped, desc, .. } => {
            restore_edit_state(ui, state, &path, before, before_cropped);
            ui.set_status_file_text(format!("Undone: {}", desc).into());
        }
    }

    state.is_undoing_redoing = false;
}

fn redo_last_operation(ui: &AppWindow, state: &mut ViewerState) {
    let Some(action) = state.history_redo.pop() else {
        ui.set_status_file_text("Nothing to redo".into());
        return;
    };
    state.is_undoing_redoing = true;
    state.history_undo.push(action.clone());
    if state.history_undo.len() > MAX_HISTORY {
        state.history_undo.remove(0);
    }

    match action {
        HistoryAction::ImageSelection { to_idx, .. } => {
            if to_idx < state.visible_files.len() {
                select_image_by_index(ui, state, to_idx);
                ui.set_status_file_text(format!("Redone image selection -> Image #{}", to_idx + 1).into());
            }
        }
        HistoryAction::ColorProfile { new_profile, .. } => {
            state.selected_profile = new_profile.clone();
            ui.set_current_profile(new_profile.clone().into());
            if let Some(curr) = state.current_decoded.as_ref().map(|d| d.path.clone()) {
                state.image_profile_overrides.insert(curr, new_profile.clone());
            }
            request_render(state, true, false);
            ui.set_status_file_text(format!("Redone color profile -> {}", new_profile).into());
        }
        HistoryAction::Rating { path, new_rating, .. } => {
            set_ratings(ui, state, &[path], new_rating as u8, false);
            ui.set_status_file_text(format!("Redone rating -> {} stars", new_rating).into());
        }
        HistoryAction::Rotate { path, cw } => {
            rotate_path(ui, state, &path, cw, false);
            ui.set_status_file_text("Redone rotation".into());
        }
        HistoryAction::ImageEdit { path, after, after_cropped, desc, .. } => {
            restore_edit_state(ui, state, &path, after, after_cropped);
            ui.set_status_file_text(format!("Redone: {}", desc).into());
        }
    }

    state.is_undoing_redoing = false;
}

/// Sets the rating of several files, updates the filmstrip and saves the settings once.
fn set_ratings(ui: &AppWindow, state: &mut ViewerState, paths: &[String], rating: u8, record: bool) {
    let rating = rating.min(5);
    for p in paths {
        let old = state.settings.get_rating(p) as i32;
        if record && old != rating as i32 {
            state.push_undo(HistoryAction::Rating { path: p.clone(), old_rating: old, new_rating: rating as i32 });
        }
        state.settings.set_rating(p, rating);
    }
    state.settings.save();

    let model = ui.get_filmstrip_items();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
        for i in 0..vm.row_count() {
            if let Some(mut item) = vm.row_data(i) {
                if paths.iter().any(|p| same_path(p, item.full_path.as_str())) {
                    item.rating = rating as i32;
                    vm.set_row_data(i, item);
                }
            }
        }
    }
    if let Some(cur) = state.current_index.and_then(|i| state.visible_files.get(i)) {
        ui.set_current_rating(state.settings.get_rating(cur) as i32);
    }
    if state.selected_indices.len() > 1 {
        refresh_multi_selection_overview(ui, state);
    }
}

fn cycle_color_profile(ui: &AppWindow, state: &mut ViewerState) {
    let mut profiles: Vec<String> = crate::config::BUILTIN_PROFILES.iter().map(|s| s.to_string()).collect();
    for (name, _) in state.color_manager.get_custom_profiles() {
        profiles.push(name);
    }
    let has_embedded = state.current_decoded.as_ref().map(|d| d.embedded_icc.is_some()).unwrap_or(false);
    if !has_embedded {
        profiles.retain(|p| p != crate::config::PROFILE_EMBEDDED);
    }
    if profiles.is_empty() {
        return;
    }

    let cur = &state.selected_profile;
    let next_profile = match profiles.iter().position(|p| p == cur) {
        Some(pos) => profiles[(pos + 1) % profiles.len()].clone(),
        None => profiles[0].clone(),
    };
    let old_p = state.selected_profile.clone();
    if old_p != next_profile {
        state.push_undo(HistoryAction::ColorProfile { old_profile: old_p, new_profile: next_profile.clone() });
    }
    state.selected_profile = next_profile.clone();
    if let Some(dec) = &state.current_decoded {
        state.image_profile_overrides.insert(dec.path.clone(), next_profile.clone());
    }
    ui.set_current_profile(next_profile.into());
    request_render(state, true, false);
}

// ----------------------------------------------------------------------------
// Selection
// ----------------------------------------------------------------------------

fn select_image_by_index(ui: &AppWindow, state: &mut ViewerState, index: usize) {
    if index >= state.visible_files.len() {
        return;
    }

    if let Some(from) = state.current_index {
        if from != index {
            state.push_undo(HistoryAction::ImageSelection { from_idx: from, to_idx: index });
        }
    }

    let path = state.visible_files[index].clone();
    state.current_index = Some(index);
    if !state.selected_indices.contains(&index) {
        state.selected_indices.clear();
        state.selected_indices.insert(index);
        state.selection_anchor = Some(index);
    }

    // Update filmstrip selection indicator & read item badge
    let mut badge = Badge::None;
    let filmstrip_model = ui.get_filmstrip_items();
    if let Some(vec_model) = filmstrip_model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
        for i in 0..vec_model.row_count() {
            if let Some(mut item) = vec_model.row_data(i) {
                let should_sel = state.selected_indices.contains(&i);
                if i == index {
                    badge = Badge::from_kind(item.badge_type.as_str());
                }
                if item.selected != should_sel {
                    item.selected = should_sel;
                    vec_model.set_row_data(i, item);
                }
            }
        }
    }
    show_current_badge(ui, badge);
    ui.set_current_rating(state.settings.get_rating(&path) as i32);
    ui.set_current_image_is_cropped(state.is_cropped(&path));

    let same_image_as_before = state.current_decoded.as_ref().map(|d| d.path == path).unwrap_or(false);
    let in_memory = state.in_memory(&path);
    if !Path::new(&path).exists() && in_memory.is_none() {
        state.current_decoded = None;
        state.load_request_id += 1; // cancel pending loads
        ui.set_has_image(false);
        ui.set_canvas_empty_text("⚠️ Image Unavailable — File was deleted from disk".into());
        ui.set_status_file_text(format!("Error: File '{}' is not available on disk.", file_name_of(&path)).into());
        return;
    }

    if in_memory.is_none() {
        // Decoding happens in the background; until it finishes, editing tools are disabled
        // (they act on `current_decoded`) while the previous picture stays on screen.
        state.current_decoded = None;
        ui.set_status_file_text(format!("Loading {}…", file_name_of(&path)).into());
    } else {
        state.current_decoded = in_memory.clone();
    }
    if !same_image_as_before {
        state.rendered = None;
        state.document_rgb = None;
    }

    let req = DisplayRequest {
        req_id: 0,
        path: path.clone(),
        index: Some(index),
        source: in_memory,
        profile_mode: ProfileMode::Auto {
            lock: state.lock_profile,
            current: state.selected_profile.clone(),
            override_profile: state.image_profile_overrides.get(&path).cloned(),
        },
        preserve_view: same_image_as_before || state.lock_zoom,
        need_meta: true,
    };
    start_display_job(state, req);
}

// ----------------------------------------------------------------------------
// Filters
// ----------------------------------------------------------------------------

/// Orientation-independent aspect ratio (long side / short side) of a file, if known without
/// heavy work. RAW files that are not cached yet return `None` (computed in the background).
fn get_image_aspect_ratio_norm(state: &ViewerState, path: &str) -> Option<f32> {
    let key = path_key(path);
    if let Some(r) = state.aspect_cache.lock().ok().and_then(|c| c.get(&key).copied()) {
        return Some(r);
    }
    let ratio_of = |w: u32, h: u32| (w > 0 && h > 0).then(|| w.max(h) as f32 / w.min(h) as f32);
    if let Some(dec) = state.edited_images.get(path) {
        return ratio_of(dec.width, dec.height);
    }
    if let Some(dec) = state.preloader.cache.lock().ok().and_then(|g| g.peek(path)) {
        return ratio_of(dec.width, dec.height);
    }
    if crate::config::is_raw_file(Path::new(path)) {
        return None;
    }
    let r = crate::image_loader::probe_dimensions(Path::new(path)).and_then(|(w, h)| ratio_of(w, h))?;
    if let Ok(mut c) = state.aspect_cache.lock() {
        c.insert(key, r);
    }
    Some(r)
}

fn get_file_color_profile_info(state: &ViewerState, path: &str) -> (String, bool) {
    if let Some(prof) = state.image_profile_overrides.get(path) {
        return (prof.clone(), true);
    }
    let dec = state.edited_images.get(path).cloned().or_else(|| state.preloader.cache.lock().ok().and_then(|g| g.peek(path)));
    if let Some(dec) = dec {
        if let Some(ref name) = dec.embedded_icc_name {
            return (name.clone(), true);
        } else if dec.embedded_icc.is_some() {
            return ("Embedded ICC".to_string(), true);
        }
    }
    (DEFAULT_FALLBACK_PROFILE.to_string(), false)
}

fn matches_format_filter(path: &str, filter: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match filter.to_uppercase().as_str() {
        "" | "ALL" => true,
        "RAW" => crate::config::is_raw_extension(&ext),
        "JPG" | "JPEG" => matches!(ext.as_str(), "jpg" | "jpeg" | "jpe" | "jfif"),
        "PNG" => ext == "png",
        "TIF" | "TIFF" => matches!(ext.as_str(), "tif" | "tiff"),
        "WEBP" => ext == "webp",
        other => ext.eq_ignore_ascii_case(other),
    }
}

/// Computes aspect ratios of RAW files (from their embedded preview) in the background, then
/// re-applies the filter.
fn start_aspect_ratio_job(state: &mut ViewerState, pending: Vec<String>) {
    if state.aspect_job_running || pending.is_empty() {
        return;
    }
    let Some((ui_w, st_w)) = weak_handles(state) else { return };
    state.aspect_job_running = true;
    let cache = state.aspect_cache.clone();
    std::thread::spawn(move || {
        for p in pending {
            let r = crate::image_loader::probe_dimensions(Path::new(&p))
                .filter(|(w, h)| *w > 0 && *h > 0)
                .map(|(w, h)| w.max(h) as f32 / w.min(h) as f32)
                .unwrap_or(f32::NAN); // NaN = unknown: excluded from ratio filters
            if let Ok(mut c) = cache.lock() {
                c.insert(path_key(&p), r);
            }
        }
        post_to_ui(ui_w, st_w, |ui, st| {
            st.aspect_job_running = false;
            if st.filter_aspect_ratio != "ALL" && !st.filter_aspect_ratio.is_empty() {
                apply_tree_filter_and_populate(ui, st);
            }
        });
    });
}

fn apply_tree_filter_and_populate(ui: &AppWindow, state: &mut ViewerState) {
    let root_norm = normalize_path(&state.current_folder);
    let filter_key = state.selected_tree_filter.clone();
    let previous_file = state.current_index.and_then(|i| state.visible_files.get(i).cloned());

    let mut visible: Vec<String> = if filter_key == "__ALL__" {
        state.all_folder_files.clone()
    } else if filter_key == "__ROOT_ONLY__" {
        state
            .all_folder_files
            .iter()
            .filter(|f| {
                Path::new(f)
                    .parent()
                    .map(|p| same_path(&p.to_string_lossy(), &root_norm))
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    } else {
        let prefix = dir_prefix_key(&filter_key);
        state
            .all_folder_files
            .iter()
            .filter(|f| {
                path_key(f).starts_with(&prefix)
                    || Path::new(f).parent().map(|p| same_path(&p.to_string_lossy(), &filter_key)).unwrap_or(false)
            })
            .cloned()
            .collect()
    };

    let search_low = state.filter_search_text.trim().to_lowercase();
    let r_mask = state.filter_ratings_mask;
    let ar_filt = state.filter_aspect_ratio.clone();
    let cp_filt = state.filter_color_profile.clone();
    let fmt_filt = state.filter_format.clone();
    let mut pending_ratios = Vec::new();

    let needs_filtering = !search_low.is_empty()
        || (r_mask != 0 && r_mask != 63)
        || (ar_filt != "ALL" && !ar_filt.is_empty())
        || (cp_filt != "ALL" && !cp_filt.is_empty())
        || (fmt_filt != "ALL" && !fmt_filt.is_empty())
        || state.filter_is_cropped
        || state.filter_is_profile_modified;

    if needs_filtering {
        let saved_aspects = state.settings.saved_aspect_ratios.clone();
        visible.retain(|fpath| {
            if !search_low.is_empty() && !file_name_of(fpath).to_lowercase().contains(&search_low) {
                return false;
            }
            if !matches_format_filter(fpath, &fmt_filt) {
                return false;
            }
            if state.filter_is_cropped && !state.is_cropped(fpath) {
                return false;
            }
            if state.filter_is_profile_modified && !state.image_profile_overrides.contains_key(fpath) {
                return false;
            }
            if r_mask != 0 && r_mask != 63 {
                let r = state.settings.get_rating(fpath);
                if (r_mask & (1 << r)) == 0 {
                    return false;
                }
            }
            if ar_filt != "ALL" && !ar_filt.is_empty() {
                match get_image_aspect_ratio_norm(state, fpath) {
                    Some(ratio) if ratio.is_finite() => {
                        if !matches_aspect_ratio_filter(ratio, &ar_filt, &saved_aspects) {
                            return false;
                        }
                    }
                    Some(_) => return false,
                    None => {
                        pending_ratios.push(fpath.clone());
                        return false;
                    }
                }
            }
            if cp_filt != "ALL" && !cp_filt.is_empty() {
                let (img_prof, has_embedded) = get_file_color_profile_info(state, fpath);
                if cp_filt.eq_ignore_ascii_case("NONE") {
                    if has_embedded {
                        return false;
                    }
                } else if !img_prof.to_lowercase().contains(&cp_filt.to_lowercase()) {
                    return false;
                }
            }
            true
        });
    }
    state.visible_files = visible;
    let pending_count = pending_ratios.len();
    start_aspect_ratio_job(state, pending_ratios);

    refresh_folder_tree_ui(ui, state);

    // Keep the current image selected if it is still visible.
    let new_index = previous_file
        .as_ref()
        .and_then(|p| state.visible_files.iter().position(|f| f == p))
        .unwrap_or(0);

    let mut film_items = Vec::with_capacity(state.visible_files.len());
    for (i, fpath) in state.visible_files.iter().enumerate() {
        let fname = file_name_of(fpath);
        let rel = Path::new(fpath)
            .strip_prefix(&root_norm)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| fname.clone());
        film_items.push(FilmstripItemData {
            label: fname.into(),
            rel_path: rel.into(),
            full_path: fpath.clone().into(),
            thumbnail: Image::default(),
            has_thumbnail: false,
            selected: i == new_index,
            badge_type: "".into(),
            badge_text: "".into(),
            rating: state.settings.get_rating(fpath) as i32,
            is_cropped: state.is_cropped(fpath),
        });
    }
    ui.set_filmstrip_items(ModelRc::new(VecModel::from(film_items)));
    ui.set_filmstrip_offset_x(0.0);

    state.selected_indices.clear();
    state.current_index = None;
    if !state.visible_files.is_empty() {
        state.selected_indices.insert(new_index);
        state.selection_anchor = Some(new_index);
        let was_undoing = state.is_undoing_redoing;
        state.is_undoing_redoing = true; // a filter change is not an undoable navigation
        select_image_by_index(ui, state, new_index);
        state.is_undoing_redoing = was_undoing;
    } else {
        state.current_decoded = None;
        state.load_request_id += 1;
        ui.set_has_image(false);
        ui.set_status_file_text(
            if pending_count > 0 {
                format!("Computing aspect ratios of {} RAW file(s)…", pending_count)
            } else {
                "No supported images found in the selected folder filter.".to_string()
            }
            .into(),
        );
    }
    sync_filmstrip_selection_ui(ui, state);
    spawn_filmstrip_thumbnail_loader(ui, state);
}

// ----------------------------------------------------------------------------
// Thumbnails
// ----------------------------------------------------------------------------

/// Generates filmstrip thumbnails with a small pool of workers that take files strictly in
/// priority order (closest to the current image first).
fn spawn_filmstrip_thumbnail_loader(ui: &AppWindow, state: &mut ViewerState) {
    let cur_idx = state.current_index.unwrap_or(0);
    let mut order: Vec<usize> = (0..state.visible_files.len()).collect();
    order.sort_by_key(|&idx| {
        let diff = (idx as isize - cur_idx as isize).unsigned_abs();
        if idx >= cur_idx { diff * 2 } else { diff * 2 + 1 }
    });

    let gen = state.thumbnail_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let files = Arc::new(state.visible_files.clone());
    let order = Arc::new(order);
    let edits: Arc<HashMap<String, Arc<DecodedImage>>> = Arc::new(state.edited_images.clone());
    let cm = Arc::new(state.color_manager.clone());
    let next = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = crossbeam_channel::unbounded::<(usize, String, u32, u32, Vec<u8>)>();
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8) - 1;

    for _ in 0..workers.max(1) {
        let (files, order, edits, cm, next, tx, gen_arc) =
            (files.clone(), order.clone(), edits.clone(), cm.clone(), next.clone(), tx.clone(), state.thumbnail_generation.clone());
        std::thread::spawn(move || loop {
            if gen_arc.load(Ordering::SeqCst) != gen {
                return;
            }
            let i = next.fetch_add(1, Ordering::SeqCst);
            let Some(&idx) = order.get(i) else { return };
            let path = &files[idx];
            let thumb = match edits.get(path) {
                Some(edit) => crate::image_loader::create_thumbnail_from_decoded(edit, Some(&cm)),
                None => load_thumbnail_rgba(Path::new(path), Some(&cm)),
            };
            if gen_arc.load(Ordering::SeqCst) != gen {
                return;
            }
            // Neutral placeholder so "Loading..." never hangs on unreadable files.
            let (tw, th, rgba) = thumb.unwrap_or_else(|| (140, 98, vec![30u8; 140 * 98 * 4]));
            if tx.send((idx, path.clone(), tw, th, rgba)).is_err() {
                return;
            }
        });
    }
    drop(tx);

    // Distributor: batches updates to the UI thread.
    let ui_weak = ui.as_weak();
    let gen_arc_dist = state.thumbnail_generation.clone();
    std::thread::spawn(move || {
        let mut count = 0;
        let mut batch = Vec::with_capacity(16);
        let mut last_flush = std::time::Instant::now();
        let flush = |to_send: Vec<(usize, String, u32, u32, Vec<u8>)>| {
            let ui_copy = ui_weak.clone();
            let gen_check = gen_arc_dist.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if gen_check.load(Ordering::SeqCst) != gen {
                    return;
                }
                if let Some(ui) = ui_copy.upgrade() {
                    for (idx, path, tw, th, rgba) in to_send {
                        set_thumbnail_for_path(&ui, Some(idx), &path, rgba_to_slint_image(tw, th, &rgba), None);
                    }
                }
            });
        };
        while let Ok(item) = rx.recv() {
            if gen_arc_dist.load(Ordering::SeqCst) != gen {
                return;
            }
            batch.push(item);
            count += 1;
            if count <= 10 || batch.len() >= 16 || last_flush.elapsed() >= Duration::from_millis(40) {
                flush(std::mem::take(&mut batch));
                last_flush = std::time::Instant::now();
            }
        }
        if !batch.is_empty() {
            flush(batch);
        }
    });
}

// ----------------------------------------------------------------------------
// Folder scanning (background) and filesystem synchronization
// ----------------------------------------------------------------------------

type ScanDone = Box<dyn FnOnce(&AppWindow, &mut ViewerState, Result<usize, String>) + Send>;

/// Scans the current folder on a worker thread and repopulates the UI when done.
/// `keep_file` is re-selected afterwards if still present; `restore_sub` restores a tree filter.
fn start_folder_scan(ui: &AppWindow, state: &mut ViewerState, keep_file: Option<String>, restore_sub: Option<String>, on_done: Option<ScanDone>) {
    let Some((ui_w, st_w)) = weak_handles(state) else { return };
    state.scan_generation += 1;
    let gen = state.scan_generation;
    let folder = normalize_path(&state.current_folder);
    ui.set_status_file_text(format!("Scanning {}…", folder).into());
    std::thread::spawn(move || {
        let (files, subdirs) = scan_folder_and_subfolders(&folder);
        post_to_ui(ui_w, st_w, move |ui, st| {
            if st.scan_generation != gen {
                return;
            }
            st.all_folder_files = files;
            st.subfolders = subdirs;
            if let Some(sub) = restore_sub {
                let valid = sub == "__ALL__"
                    || sub == "__ROOT_ONLY__"
                    || st.subfolders.iter().any(|s| same_path(s, &sub));
                if valid {
                    st.selected_tree_filter = sub.clone();
                    if sub != "__ALL__" && sub != "__ROOT_ONLY__" {
                        expand_ancestors_and_subfolder(st, &sub);
                    }
                }
            }
            if st.selected_tree_filter != "__ALL__" && st.selected_tree_filter != "__ROOT_ONLY__" {
                let filt = st.selected_tree_filter.clone();
                if !st.subfolders.iter().any(|s| same_path(s, &filt)) {
                    st.selected_tree_filter = "__ALL__".to_string();
                }
            }
            // Re-select the previously shown file after a refresh.
            if let Some(k) = &keep_file {
                st.current_index = None;
                st.visible_files.clear();
                st.visible_files.push(k.clone());
                st.current_index = Some(0);
            } else {
                st.current_index = None;
            }
            apply_tree_filter_and_populate(ui, st);
            update_cache_status_ui(ui, st);
            let count = st.visible_files.len();
            if let Some(cb) = on_done {
                cb(ui, st, Ok(count));
            }
        });
    });
}

fn refresh_current_folder_content(ui: &AppWindow, state: &mut ViewerState) {
    if state.is_files_mode {
        state.visible_files.retain(|f| Path::new(f).exists());
        state.all_folder_files.retain(|f| Path::new(f).exists());
        apply_tree_filter_and_populate(ui, state);
        ui.set_status_file_text(format!("Refreshed: {} files available", state.visible_files.len()).into());
        return;
    }
    if state.current_folder.is_empty() {
        ui.set_status_file_text("No folder loaded to refresh.".into());
        return;
    }
    let norm = normalize_path(&state.current_folder);
    if !Path::new(&norm).is_dir() {
        ui.set_status_file_text(format!("Folder not found: {}", norm).into());
        return;
    }
    let cur_file = state.current_index.and_then(|idx| state.visible_files.get(idx).cloned());
    // Decoded images may be stale after a refresh; unsaved edits are kept separately.
    if let Ok(mut guard) = state.preloader.cache.lock() {
        guard.clear();
    }
    start_folder_scan(
        ui,
        state,
        cur_file,
        None,
        Some(Box::new(|ui: &AppWindow, st: &mut ViewerState, res: Result<usize, String>| {
            if let Ok(n) = res {
                ui.set_status_file_text(format!("Refreshed: {} images found across {} subfolder(s)", n, st.subfolders.len()).into());
            }
        })),
    );
}

/// True if `path` is (or contains) a known subfolder of the current tree.
fn is_known_folder(state: &ViewerState, path: &str) -> bool {
    let prefix = dir_prefix_key(path);
    state.subfolders.iter().any(|s| same_path(s, path) || path_key(s).starts_with(&prefix))
        || state.all_folder_files.iter().any(|f| path_key(f).starts_with(&prefix))
}

fn handle_filesystem_sync(ui: &AppWindow, state: &mut ViewerState, actions: Vec<crate::file_watcher::FileSyncAction>) {
    use crate::file_watcher::FileSyncAction;
    let model = ui.get_filmstrip_items();
    let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() else {
        return;
    };
    // An atomic write of a file we did not know yet (e.g. "Save As" into this folder) is
    // reported as a modification of the destination: treat it as a new file.
    let actions: Vec<FileSyncAction> = actions
        .into_iter()
        .map(|a| match a {
            FileSyncAction::Modified(p) if !state.all_folder_files.iter().any(|f| same_path(f, &p)) => FileSyncAction::Created(p),
            other => other,
        })
        .collect();
    let cur_path = state.current_index.and_then(|idx| state.visible_files.get(idx).cloned());
    let mut tree_changed = false;
    let mut folder_structure_changed = false;

    let set_badge_for = |path: &str, badge: Badge| {
        for i in 0..vm.row_count() {
            if let Some(mut item) = vm.row_data(i) {
                if same_path(item.full_path.as_str(), path) {
                    set_item_badge(&mut item, badge);
                    vm.set_row_data(i, item);
                }
            }
        }
    };

    for action in actions {
        match action {
            FileSyncAction::Removed(removed_path) => {
                if Path::new(&removed_path).exists() {
                    // e.g. a case-only rename on Windows, or a file restored immediately.
                    continue;
                }
                if is_known_folder(state, &removed_path) {
                    folder_structure_changed = true;
                    continue;
                }
                set_badge_for(&removed_path, Badge::Deleted);
                if cur_path.as_deref().is_some_and(|cp| same_path(cp, &removed_path)) {
                    show_current_badge(ui, Badge::Deleted);
                    ui.set_status_file_text(format!("Current image: {} [Deleted from disk]", file_name_of(&removed_path)).into());
                }
            }
            FileSyncAction::Restored(restored_path) => {
                for i in 0..vm.row_count() {
                    if let Some(mut item) = vm.row_data(i) {
                        if same_path(item.full_path.as_str(), &restored_path) && item.badge_type == "deleted" {
                            set_item_badge(&mut item, Badge::None);
                            vm.set_row_data(i, item);
                        }
                    }
                }
                if cur_path.as_deref().is_some_and(|cp| same_path(cp, &restored_path)) {
                    show_current_badge(ui, Badge::None);
                }
            }
            FileSyncAction::Renamed { old_path, new_path } => {
                if is_known_folder(state, &old_path) {
                    folder_structure_changed = true;
                    continue;
                }
                let renamed_here = migrate_renamed_path(ui, state, &old_path, &new_path, true);
                if !renamed_here && !state.is_files_mode && !state.all_folder_files.iter().any(|f| same_path(f, &new_path)) {
                    // Renamed from a file we did not track (e.g. unsupported extension).
                    state.all_folder_files.push(new_path.clone());
                    tree_changed = true;
                }
            }
            FileSyncAction::Created(created_path) => {
                if state.is_files_mode {
                    continue;
                }
                let norm_cre = normalize_path(&created_path);
                if state.all_folder_files.iter().any(|f| same_path(f, &norm_cre)) {
                    // Re-created (e.g. restored from the Recycle Bin).
                    set_badge_for(&norm_cre, Badge::None);
                    continue;
                }
                state.all_folder_files.push(norm_cre.clone());
                state.all_folder_files.sort_by_key(|a| a.to_lowercase());
                tree_changed = true;

                let root_norm = normalize_path(&state.current_folder);
                let should_show = match state.selected_tree_filter.as_str() {
                    "__ALL__" => true,
                    "__ROOT_ONLY__" => Path::new(&norm_cre).parent().map(|p| same_path(&p.to_string_lossy(), &root_norm)).unwrap_or(false),
                    sub => path_key(&norm_cre).starts_with(&dir_prefix_key(sub)),
                };
                if should_show {
                    let new_fname = file_name_of(&norm_cre);
                    let rel = Path::new(&norm_cre)
                        .strip_prefix(&root_norm)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| new_fname.clone());
                    state.visible_files.push(norm_cre.clone());
                    vm.push(FilmstripItemData {
                        label: new_fname.into(),
                        rel_path: rel.into(),
                        full_path: norm_cre.clone().into(),
                        thumbnail: Image::default(),
                        has_thumbnail: false,
                        selected: false,
                        badge_type: Badge::New.kind().into(),
                        badge_text: Badge::New.text().into(),
                        rating: state.settings.get_rating(&norm_cre) as i32,
                        is_cropped: false,
                    });
                    refresh_thumbnail_from_disk(state, &norm_cre);
                }
            }
            FileSyncAction::Modified(modified_path) => {
                let norm_mod = normalize_path(&modified_path);
                if !Path::new(&norm_mod).exists() || crate::disk_ops::is_recent_self_write(Path::new(&norm_mod)) {
                    // Our own write (rotation / save): memory is already up to date.
                    continue;
                }
                if let Ok(mut guard) = state.preloader.cache.lock() {
                    guard.remove(&norm_mod);
                }
                if let Ok(mut c) = state.aspect_cache.lock() {
                    c.remove(&path_key(&norm_mod));
                }
                set_badge_for(&norm_mod, Badge::Modified);
                if !state.is_cropped(&norm_mod) {
                    refresh_thumbnail_from_disk(state, &norm_mod);
                }

                if cur_path.as_deref().is_some_and(|cp| same_path(cp, &norm_mod)) {
                    show_current_badge(ui, Badge::Modified);
                    if state.is_cropped(&norm_mod) {
                        ui.set_status_file_text("The file changed on disk; your unsaved crop is kept in memory.".into());
                    } else {
                        let idx = state.current_index;
                        let req = DisplayRequest {
                            req_id: 0,
                            path: norm_mod.clone(),
                            index: idx,
                            source: None,
                            profile_mode: ProfileMode::Fixed(state.selected_profile.clone()),
                            preserve_view: true,
                            need_meta: true,
                        };
                        start_display_job(state, req);
                    }
                }
            }
            FileSyncAction::FolderStructureChanged => {
                folder_structure_changed = true;
            }
        }
    }

    if folder_structure_changed {
        refresh_current_folder_content(ui, state);
    } else if tree_changed {
        refresh_folder_tree_ui(ui, state);
    }
}

/// Moves every piece of per-file state from `old_path` to `new_path` after a rename
/// (filmstrip row, lists, cache, unsaved edits, profile overrides, rating, current image).
/// Returns true if the file was known.
fn migrate_renamed_path(ui: &AppWindow, state: &mut ViewerState, old_path: &str, new_path: &str, show_badge: bool) -> bool {
    let mut known = false;
    let model = ui.get_filmstrip_items();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
        for i in 0..vm.row_count() {
            if let Some(mut item) = vm.row_data(i) {
                if same_path(item.full_path.as_str(), old_path) {
                    item.full_path = new_path.into();
                    item.label = file_name_of(new_path).into();
                    if show_badge {
                        set_item_badge(&mut item, Badge::Renamed);
                    }
                    vm.set_row_data(i, item);
                }
            }
        }
    }
    for f in state.visible_files.iter_mut().chain(state.all_folder_files.iter_mut()) {
        if same_path(f, old_path) {
            *f = new_path.to_string();
            known = true;
        }
    }
    if let Ok(mut guard) = state.preloader.cache.lock() {
        guard.rename_key(old_path, new_path);
    }
    if let Some(edit) = state.edited_images.remove(old_path) {
        let mut moved = (*edit).clone();
        moved.path = new_path.to_string();
        state.edited_images.insert(new_path.to_string(), Arc::new(moved));
    }
    if let Some(p) = state.image_profile_overrides.remove(old_path) {
        state.image_profile_overrides.insert(new_path.to_string(), p);
    }
    if let Ok(mut c) = state.aspect_cache.lock() {
        if let Some(r) = c.remove(&path_key(old_path)) {
            c.insert(path_key(new_path), r);
        }
    }
    state.settings.rename_rating(old_path, new_path);
    for action in state.history_undo.iter_mut().chain(state.history_redo.iter_mut()) {
        match action {
            HistoryAction::Rating { path, .. } | HistoryAction::Rotate { path, .. } | HistoryAction::ImageEdit { path, .. } => {
                if same_path(path, old_path) {
                    *path = new_path.to_string();
                }
            }
            _ => {}
        }
    }

    let is_current = state.current_decoded.as_ref().map(|d| same_path(&d.path, old_path)).unwrap_or(false);
    if is_current {
        if let Some(dec) = state.current_decoded.clone() {
            let mut new_dec = (*dec).clone();
            new_dec.path = new_path.to_string();
            state.current_decoded = Some(Arc::new(new_dec));
        }
        if show_badge {
            show_current_badge(ui, Badge::Renamed);
        }
        request_render(state, true, true);
    }
    if known {
        state.settings.save();
    }
    known
}

fn start_folder_watcher(state: &mut ViewerState, folder: &str) {
    state.folder_watcher = None;
    let Some((ui_w, st_w)) = weak_handles(state) else { return };
    match crate::file_watcher::FolderWatcher::start(folder, move |actions| {
        post_to_ui(ui_w.clone(), st_w.clone(), move |ui, st| handle_filesystem_sync(ui, st, actions));
    }) {
        Ok(watcher) => state.folder_watcher = Some(watcher),
        Err(err) => eprintln!("Warning: Failed to start folder watcher: {}", err),
    }
}

fn open_specific_files(ui: &AppWindow, state: &mut ViewerState, files: Vec<String>) {
    if files.is_empty() {
        return;
    }
    let parent_dir = Path::new(&files[0])
        .parent()
        .map(|p| normalize_path(&p.to_string_lossy()))
        .unwrap_or_default();

    state.is_files_mode = true;
    state.scan_generation += 1; // cancel any running folder scan
    state.current_folder = parent_dir.clone();
    if !parent_dir.is_empty() {
        state.settings.add_recent_folder(&parent_dir);
    }
    ui.set_folder_path(format!("{} selected file(s)", files.len()).into());
    refresh_recent_folders_ui(ui, state);
    if let Ok(mut guard) = state.preloader.cache.lock() {
        guard.clear();
    }
    state.all_folder_files = files;
    state.subfolders = Vec::new();
    state.selected_tree_filter = "__ALL__".to_string();
    state.current_index = None;
    apply_tree_filter_and_populate(ui, state);
    if !parent_dir.is_empty() {
        start_folder_watcher(state, &parent_dir);
    }
}

/// Opens a folder: the scan runs in the background; `on_done` is called once the filmstrip
/// is populated (or immediately with an error).
fn open_folder_path(ui: &AppWindow, state: &mut ViewerState, folder: &str, restore_sub: Option<String>, on_done: Option<ScanDone>) {
    let norm = normalize_path(folder);
    if !Path::new(&norm).is_dir() {
        let msg = format!("Folder does not exist: {}", norm);
        ui.set_status_file_text(msg.clone().into());
        if let Some(cb) = on_done {
            cb(ui, state, Err(msg));
        }
        return;
    }

    let folder_changed = !same_path(&state.settings.last_folder, &norm);
    state.is_files_mode = false;
    state.current_folder = norm.clone();
    state.settings.add_recent_folder(&norm);
    state.settings.last_folder = norm.clone();
    if folder_changed {
        state.settings.last_subfolder = "__ALL__".to_string();
    }
    state.settings.save();
    ui.set_folder_path(norm.clone().into());
    refresh_recent_folders_ui(ui, state);

    if let Ok(mut guard) = state.preloader.cache.lock() {
        guard.clear();
    }

    state.filter_search_text.clear();
    state.filter_rating = 0;
    state.filter_ratings_mask = 0;
    ui.set_filter_search_text("".into());
    ui.invoke_clear_ratings();

    state.selected_tree_filter = "__ALL__".to_string();
    state.expanded_folders.clear();
    state.expanded_folders.insert("__ALL__".to_string());
    state.expanded_folders.insert(norm.clone());
    state.all_folder_files.clear();
    state.subfolders.clear();
    state.visible_files.clear();
    state.current_index = None;
    ui.set_filmstrip_items(ModelRc::new(VecModel::default()));

    start_folder_watcher(state, &norm);
    start_folder_scan(ui, state, None, restore_sub, on_done);
}

/// Convert viewport mouse coordinates (mx, my) into image pixel coordinates (ix, iy).
fn viewport_to_image_coords(mx: f32, my: f32, vw: f32, vh: f32, state: &ViewerState) -> Option<(u32, u32)> {
    let decoded = state.current_decoded.as_ref()?;
    let z = view_zoom(state);
    let img_w = decoded.width as f32 * z;
    let img_h = decoded.height as f32 * z;
    let img_x0 = (vw - img_w) * 0.5 + state.pan_x;
    let img_y0 = (vh - img_h) * 0.5 + state.pan_y;

    let rel_x = (mx - img_x0) / z;
    let rel_y = (my - img_y0) / z;

    if rel_x < 0.0 || rel_y < 0.0 || rel_x >= decoded.width as f32 || rel_y >= decoded.height as f32 {
        return None;
    }

    Some((
        (rel_x.floor() as u32).min(decoded.width.saturating_sub(1)),
        (rel_y.floor() as u32).min(decoded.height.saturating_sub(1)),
    ))
}

/// Returns the on-screen pixels if they match the current image (a render may still be pending).
fn current_rendered_bytes(state: &ViewerState) -> Option<&[u8]> {
    let dec = state.current_decoded.as_ref()?;
    let buf = state.rendered.as_ref()?;
    (buf.width() == dec.width && buf.height() == dec.height).then(|| buf.as_bytes())
}


// ----------------------------------------------------------------------------
// Helpers used by the UI callbacks
// ----------------------------------------------------------------------------

/// Shows a message box from a fresh event-loop turn, so it never runs while the state lock is
/// held (native dialogs spin a nested event loop).
fn show_message_later(title: &str, text: &str, level: rfd::MessageLevel) {
    let (title, text) = (title.to_string(), text.to_string());
    let _ = slint::invoke_from_event_loop(move || {
        rfd::MessageDialog::new()
            .set_level(level)
            .set_title(&title)
            .set_description(&text)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    });
}

fn selected_or_current(state: &ViewerState) -> Vec<usize> {
    if state.selected_indices.is_empty() {
        state.current_index.into_iter().collect()
    } else {
        state.selected_indices.iter().copied().collect()
    }
}

fn dialog_start_dir(state: &ViewerState) -> Option<PathBuf> {
    [&state.current_folder, &state.settings.last_folder]
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .find(|p| p.is_dir())
}

/// Profile an image is shown with when it is not the current one (for "save all").
fn effective_profile_for(state: &ViewerState, path: &str, decoded: &DecodedImage) -> String {
    if let Some(p) = state.image_profile_overrides.get(path) {
        return p.clone();
    }
    if state.current_decoded.as_ref().map(|d| d.path == path).unwrap_or(false) {
        return state.selected_profile.clone();
    }
    if decoded.embedded_icc.is_some() {
        PROFILE_EMBEDDED.to_string()
    } else {
        DEFAULT_FALLBACK_PROFILE.to_string()
    }
}

/// Encodes `decoded` with `profile` to `dest` (color conversion, ICC and EXIF included).
fn export_image(cm: &ColorManager, decoded: &DecodedImage, profile: &str, dest: &Path) -> Result<(), String> {
    let pixels = cm.prepare_pixels_for_save(&decoded.rgb_pixels, profile, decoded.embedded_icc.as_deref());
    let icc = cm.get_profile_bytes(profile, decoded.embedded_icc.as_deref());
    let exif = build_export_exif(Path::new(&decoded.path), decoded.width, decoded.height);
    save_image_with_icc(dest, decoded.width, decoded.height, &pixels, Some(&icc), exif.as_deref())
}

fn unsaved_edits_prompt(ui: &AppWindow, count: usize) {
    ui.set_exit_confirm_text(
        format!("There are {} image(s) with unsaved modifications (crop).\nDo you want to save before exiting?", count).into(),
    );
    ui.set_exit_confirm_dialog_visible(true);
}

fn store_window_geometry(ui: &AppWindow, st: &mut ViewerState) {
    st.settings.last_subfolder = st.selected_tree_filter.clone();
    let is_max = ui.window().is_maximized();
    st.settings.window_maximized = is_max;
    if !is_max {
        let sz = ui.window().size();
        if sz.width >= 1024 && sz.height >= 680 {
            st.settings.window_width = sz.width;
            st.settings.window_height = sz.height;
        }
    }
}

/// Removes deleted files from every list and from the filmstrip (rows are removed in place,
/// so the other thumbnails stay loaded).
fn remove_files_from_view(ui: &AppWindow, st: &mut ViewerState, removed: &[String]) {
    if removed.is_empty() {
        return;
    }
    let first_idx = st
        .visible_files
        .iter()
        .position(|f| removed.iter().any(|r| same_path(r, f)))
        .unwrap_or(0);
    let is_removed = |f: &String| removed.iter().any(|r| same_path(r, f));
    st.all_folder_files.retain(|f| !is_removed(f));
    st.visible_files.retain(|f| !is_removed(f));
    if let Ok(mut guard) = st.preloader.cache.lock() {
        for f in removed {
            guard.remove(f);
        }
    }
    for f in removed {
        st.edited_images.remove(f);
        st.image_profile_overrides.remove(f);
        st.settings.set_rating(f, 0);
    }
    st.settings.save();
    let keep = |a: &HistoryAction| match a {
        HistoryAction::ImageSelection { .. } => false,
        HistoryAction::Rating { path, .. } | HistoryAction::Rotate { path, .. } | HistoryAction::ImageEdit { path, .. } => {
            !removed.iter().any(|r| same_path(r, path))
        }
        HistoryAction::ColorProfile { .. } => true,
    };
    st.history_undo.retain(keep);
    st.history_redo.retain(keep);

    let model = ui.get_filmstrip_items();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
        for i in (0..vm.row_count()).rev() {
            if vm.row_data(i).map(|it| removed.iter().any(|r| same_path(r, it.full_path.as_str()))).unwrap_or(false) {
                vm.remove(i);
            }
        }
    }

    st.selected_indices.clear();
    st.current_index = None;
    if st.visible_files.is_empty() {
        st.current_decoded = None;
        st.rendered = None;
        st.document_rgb = None;
        st.load_request_id += 1;
        ui.set_has_image(false);
        ui.set_is_multi_selection(false);
        ui.set_multi_selection_tiles(ModelRc::new(VecModel::default()));
        show_current_badge(ui, Badge::None);
    } else {
        let new_idx = first_idx.min(st.visible_files.len() - 1);
        st.selected_indices.insert(new_idx);
        st.selection_anchor = Some(new_idx);
        select_image_by_index(ui, st, new_idx);
    }
    refresh_folder_tree_ui(ui, st);
    sync_filmstrip_selection_ui(ui, st);
    update_cache_status_ui(ui, st);
}

fn reveal_in_file_manager(path_str: &str) {
    let p = Path::new(path_str);
    if !p.exists() {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        extern "system" {
            fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
        }
        // Let Explorer bring its window to the front (ASFW_ANY).
        unsafe {
            AllowSetForegroundWindow(0xFFFF_FFFF);
        }
        // explorer.exe needs the /select argument as one token: /select,"C:\path\file.jpg"
        let win_path = path_str.replace('/', "\\");
        let _ = std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", win_path))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path_str).spawn();
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Some(parent) = p.parent() {
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }
}

fn enable_windows_dpi_awareness() {
    #[cfg(target_os = "windows")]
    unsafe {
        // Per-Monitor V2 DPI awareness for crisp rendering on high-DPI displays.
        type SetProcessDpiAwarenessContextFn = unsafe extern "system" fn(isize) -> i32;
        type SetProcessDpiAwareFn = unsafe extern "system" fn() -> i32;
        extern "system" {
            fn GetModuleHandleA(lpModuleName: *const u8) -> *mut std::ffi::c_void;
            fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const u8) -> *mut std::ffi::c_void;
        }
        let user32 = GetModuleHandleA(b"user32.dll\0".as_ptr());
        if !user32.is_null() {
            let set_ctx = GetProcAddress(user32, b"SetProcessDpiAwarenessContext\0".as_ptr());
            if !set_ctx.is_null() {
                let func: SetProcessDpiAwarenessContextFn = std::mem::transmute(set_ctx);
                func(-4); // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
            } else {
                let set_dpi = GetProcAddress(user32, b"SetProcessDPIAware\0".as_ptr());
                if !set_dpi.is_null() {
                    let func: SetProcessDpiAwareFn = std::mem::transmute(set_dpi);
                    func();
                }
            }
        }
    }
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "An unexpected internal error occurred.".to_string()
        };
        let location = panic_info
            .location()
            .map(|loc| format!("File: {}\nLine: {}, Column: {}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "Location: unknown".to_string());

        // Worker threads catch their panics (corrupt files, decoder bugs) and report them as
        // normal errors; only a panic on the UI thread is fatal.
        if std::thread::current().name() != Some("main") {
            eprintln!("Worker thread panic: {} ({})", payload, location.replace('\n', " "));
            return;
        }
        let msg = format!(
            "Horama encountered an unhandled error and needs to close.\n\n\
             Error Details:\n{}\n\n{}\n\n\
             If this error persists, please check your files or settings.",
            payload, location
        );
        show_fatal_error_dialog("Horama - Application Error", &msg);
    }));
}

/// Applies a minimum-rating filter coming from MCP (`-1` = unrated only, `0` = all).
fn apply_min_rating_filter(ui: &AppWindow, st: &mut ViewerState, r: i32) {
    st.filter_rating = r;
    if r == 0 {
        ui.invoke_clear_ratings();
        st.filter_ratings_mask = 0;
    } else if r < 0 {
        ui.set_filter_r0(true);
        ui.set_filter_r1(false);
        ui.set_filter_r2(false);
        ui.set_filter_r3(false);
        ui.set_filter_r4(false);
        ui.set_filter_r5(false);
        st.filter_ratings_mask = 1;
    } else {
        let r = r.min(5);
        ui.set_filter_r0(false);
        ui.set_filter_r1(1 >= r);
        ui.set_filter_r2(2 >= r);
        ui.set_filter_r3(3 >= r);
        ui.set_filter_r4(4 >= r);
        ui.set_filter_r5(5 >= r);
        st.filter_ratings_mask = (r..=5).fold(0u8, |m, k| m | (1 << k));
    }
}

fn handle_mcp_command(ui: &AppWindow, state_arc: &StateArc, cmd: McpCommand) {
    let mut st = lock_state(state_arc);
    let st = &mut *st;
    match cmd {
        McpCommand::OpenFolder { path, resp } => {
            let shown = path.clone();
            open_folder_path(
                ui,
                st,
                &path,
                None,
                Some(Box::new(move |_ui: &AppWindow, _st: &mut ViewerState, res: Result<usize, String>| {
                    let _ = resp.send(res.map(|n| format!("Opened folder '{}' with {} visible images", shown, n)));
                })),
            );
        }
        McpCommand::SelectImage { filename, resp } => {
            let wanted = filename.trim();
            let wanted_low = wanted.to_lowercase();
            // 1. exact file name (case-insensitive), 2. full path, 3. path suffix at a separator.
            let found = st
                .visible_files
                .iter()
                .position(|f| file_name_of(f).to_lowercase() == wanted_low)
                .or_else(|| st.visible_files.iter().position(|f| same_path(f, wanted)))
                .or_else(|| {
                    let suffix = format!("{}{}", std::path::MAIN_SEPARATOR, normalize_path(wanted)).to_lowercase();
                    st.visible_files.iter().position(|f| f.to_lowercase().ends_with(&suffix))
                });
            if let Some(idx) = found {
                st.selected_indices.clear();
                st.selected_indices.insert(idx);
                st.selection_anchor = Some(idx);
                st.is_fit_mode = true;
                ui.set_is_multi_selection(false);
                select_image_by_index(ui, st, idx);
                sync_filmstrip_selection_ui(ui, st);
                let _ = resp.send(Ok(format!("Selected image: {}", st.visible_files[idx])));
            } else {
                let _ = resp.send(Err(format!("Image '{}' not found in current folder", filename)));
            }
        }
        McpCommand::RotateImage { direction, resp } => {
            let Some(dec) = st.current_decoded.clone() else {
                let _ = resp.send(Err("No image loaded (or it is still loading)".to_string()));
                return;
            };
            let turns: u8 = match direction.trim().to_lowercase().as_str() {
                "cw" | "90" => 1,
                "180" => 2,
                "ccw" | "-90" | "270" => 3,
                _ => {
                    let _ = resp.send(Err(format!("Invalid rotation direction: '{}'", direction)));
                    return;
                }
            };
            match turns {
                1 => rotate_current_image(ui, st, true),
                2 => {
                    rotate_current_image(ui, st, true);
                    rotate_current_image(ui, st, true);
                }
                _ => rotate_current_image(ui, st, false),
            }
            let how = if crate::disk_ops::rotation_persist_supported(Path::new(&dec.path)) {
                "the file is being updated losslessly (JPEG/TIFF orientation tag, lossless re-encode for PNG/WebP/BMP)"
            } else {
                "shown on screen only (RAW/unsupported formats are never modified)"
            };
            let _ = resp.send(Ok(format!("Image rotated {}°; {}", turns as u32 * 90, how)));
        }
        McpCommand::SetZoom { mode, factor, resp } => match mode.as_str() {
            "fit" => {
                if let Some(decoded) = st.current_decoded.clone() {
                    st.zoom_factor = calculate_fit_zoom(ui, decoded.width, decoded.height);
                    st.pan_x = 0.0;
                    st.pan_y = 0.0;
                    st.is_fit_mode = true;
                    update_zoom_ui(ui, st);
                    let _ = resp.send(Ok("Zoom set to fit window".to_string()));
                } else {
                    let _ = resp.send(Err("No image loaded".to_string()));
                }
            }
            "100" => {
                st.zoom_factor = 1.0;
                st.pan_x = 0.0;
                st.pan_y = 0.0;
                st.is_fit_mode = false;
                update_zoom_ui(ui, st);
                let _ = resp.send(Ok("Zoom set to 100%".to_string()));
            }
            "custom" => {
                if let Some(f) = factor.filter(|f| f.is_finite() && *f > 0.0) {
                    st.zoom_factor = f.clamp(0.02, 32.0);
                    st.is_fit_mode = false;
                    update_zoom_ui(ui, st);
                    let _ = resp.send(Ok(format!("Zoom set to {:.0}%", st.zoom_factor * 100.0)));
                } else {
                    let _ = resp.send(Err("Missing or invalid 'factor' parameter for custom zoom".to_string()));
                }
            }
            _ => {
                let _ = resp.send(Err(format!("Unknown zoom mode: '{}'", mode)));
            }
        },
        McpCommand::SetRating { rating, resp } => {
            let current_path = st.current_index.and_then(|idx| st.visible_files.get(idx)).cloned();
            if let Some(path) = current_path {
                set_ratings(ui, st, std::slice::from_ref(&path), rating.clamp(0, 5) as u8, true);
                let _ = resp.send(Ok(format!("Rating set to {} stars for {}", rating, path)));
            } else {
                let _ = resp.send(Err("No active image to rate".to_string()));
            }
        }
        McpCommand::FilterImages { query, min_rating, format, resp } => {
            if let Some(q) = query {
                st.filter_search_text = q.clone();
                ui.set_filter_search_text(q.into());
            }
            if let Some(r) = min_rating {
                apply_min_rating_filter(ui, st, r);
            }
            if let Some(fmt) = format {
                st.filter_format = fmt.trim().to_uppercase();
            }
            apply_tree_filter_and_populate(ui, st);
            let _ = resp.send(Ok(format!("Filters applied: {} images matching criteria", st.visible_files.len())));
        }
        McpCommand::GetCurrentImageInfo { resp } => {
            let current_path = st.current_index.and_then(|idx| st.visible_files.get(idx)).cloned();
            if let Some(ref path) = current_path {
                let (w, h) = st.current_decoded.as_ref().map(|d| (d.width, d.height)).unwrap_or((0, 0));
                let mut meta_map = serde_json::Map::new();
                if let Some(ref dec) = st.current_decoded {
                    for (k, v) in extract_metadata(dec) {
                        meta_map.insert(k, Value::String(v));
                    }
                }
                let info = json!({
                    "path": path,
                    "filename": file_name_of(path),
                    "width": w,
                    "height": h,
                    "loading": st.current_decoded.is_none(),
                    "rating": st.settings.get_rating(path),
                    "profile": st.selected_profile,
                    "unsaved_crop": st.is_cropped(path),
                    "metadata": meta_map,
                });
                let _ = resp.send(Ok(info));
            } else {
                let _ = resp.send(Err("No image currently loaded".to_string()));
            }
        }
        McpCommand::ListImages { resp } => {
            let _ = resp.send(Ok(st.visible_files.clone()));
        }
    }
}

// ----------------------------------------------------------------------------
// main
// ----------------------------------------------------------------------------

fn main() -> Result<(), slint::PlatformError> {
    enable_windows_dpi_awareness();
    install_panic_hook();

    let ui = AppWindow::new()?;
    let state: StateArc = Arc::new(Mutex::new(ViewerState::new()));

    {
        let mut st = lock_state(&state);
        st.ui_weak = Some(ui.as_weak());
        st.self_weak = Some(Arc::downgrade(&state));
        st.display_scale = ui.window().scale_factor().max(0.25);
        ui.set_display_scale(st.display_scale);
        rebuild_profile_combobox_list(&ui, &st);
        populate_shortcuts_ui(&ui, &st.settings);
        refresh_recent_folders_ui(&ui, &st);

        let (w, h, is_max) = (
            st.settings.window_width.max(1024),
            st.settings.window_height.max(680),
            st.settings.window_maximized,
        );
        ui.window().set_size(slint::PhysicalSize::new(w, h));
        if is_max {
            ui.window().set_maximized(true);
        }
        let saved_aspects: Vec<SharedString> = st.settings.saved_aspect_ratios.iter().map(|s| SharedString::from(s.as_str())).collect();
        ui.set_saved_custom_aspect_ratios(ModelRc::new(VecModel::from(saved_aspects)));

        let last = st.settings.last_folder.clone();
        if !last.is_empty() {
            if Path::new(&last).is_dir() {
                let sub = Some(st.settings.last_subfolder.clone()).filter(|s| !s.is_empty());
                open_folder_path(&ui, &mut st, &last, sub, None);
            } else {
                ui.set_folder_path(last.into());
            }
        }
    }

    // Deferred refinement once the window layout pass is complete.
    let _layout_timer = {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::SingleShot, Duration::from_millis(60), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            sync_display_scale(&ui, &mut st);
            if !st.lock_zoom {
                if let Some(dec) = st.current_decoded.clone() {
                    st.zoom_factor = calculate_fit_zoom(&ui, dec.width, dec.height);
                    st.pan_x = 0.0;
                    st.pan_y = 0.0;
                    update_zoom_ui(&ui, &st);
                }
            }
        });
        timer
    };

    // --- Open File(s) ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_open_files_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let start_dir = dialog_start_dir(&lock_state(&state));
            let mut dlg = rfd::FileDialog::new()
                .set_title("Open Images")
                .add_filter("Supported Images & RAW", &crate::config::dialog_extensions())
                .add_filter("All Files (*.*)", &["*"]);
            if let Some(dir) = start_dir {
                dlg = dlg.set_directory(dir);
            }
            if let Some(files) = dlg.pick_files() {
                let file_paths: Vec<String> = files
                    .into_iter()
                    .map(|p| normalize_path(&p.to_string_lossy()))
                    .filter(|p| crate::config::is_supported_file(Path::new(p)))
                    .collect();
                if !file_paths.is_empty() {
                    open_specific_files(&ui, &mut lock_state(&state), file_paths);
                }
            }
        });
    }

    // --- Open Folder ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_open_folder_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let start_dir = dialog_start_dir(&lock_state(&state));
            let mut dlg = rfd::FileDialog::new().set_title("Select Image Folder");
            if let Some(dir) = start_dir {
                dlg = dlg.set_directory(dir);
            }
            if let Some(folder) = dlg.pick_folder() {
                open_folder_path(&ui, &mut lock_state(&state), &folder.to_string_lossy(), None, None);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_refresh_current_folder(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            refresh_current_folder_content(&ui, &mut lock_state(&state));
        });
    }

    // --- Save / Export Image with ICC & EXIF ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_image_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            // Collect what is needed, then release the lock before any dialog is shown.
            let (decoded, profile, cm, disk, weak) = {
                let st = lock_state(&state);
                let Some(decoded) = st.current_decoded.clone() else {
                    ui.set_status_file_text("Nothing to save (no image loaded yet).".into());
                    return;
                };
                (decoded, st.selected_profile.clone(), st.color_manager.clone(), st.disk.clone(), weak_handles(&st))
            };
            let Some((ui_w, st_w)) = weak else { return };

            let orig_path = Path::new(&decoded.path);
            let stem = orig_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "exported_image".to_string());
            let mut dlg = rfd::FileDialog::new()
                .set_title("Save / Export Image with Color Profile")
                .set_file_name(format!("{}.jpg", stem))
                .add_filter("JPEG Image (*.jpg, *.jpeg)", &["jpg", "jpeg", "jpe"])
                .add_filter("TIFF 8-bit (*.tif, *.tiff)", &["tif", "tiff"])
                .add_filter("PNG Image (*.png)", &["png"])
                .add_filter("WebP Image, lossless (*.webp)", &["webp"])
                .add_filter("BMP Image (*.bmp)", &["bmp"]);
            if let Some(parent) = orig_path.parent().filter(|p| p.is_dir()) {
                dlg = dlg.set_directory(parent);
            }
            let Some(mut dest) = dlg.save_file() else { return };

            let ext = dest.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext.is_empty() {
                dest.set_extension("jpg");
            } else if !crate::image_saver::EXPORT_EXTENSIONS.contains(&ext.as_str()) {
                rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Error)
                    .set_title("Save Error")
                    .set_description(format!(
                        "'.{}' is not a supported export format.\nSupported: {}",
                        ext,
                        crate::image_saver::EXPORT_EXTENSIONS.join(", ")
                    ))
                    .set_buttons(rfd::MessageButtons::Ok)
                    .show();
                return;
            }
            // The native save dialog already asks before overwriting, except when the
            // extension was added above.
            if dest.exists() && ext.is_empty() {
                let confirm = rfd::MessageDialog::new()
                    .set_title("Confirm Overwrite")
                    .set_description(format!("The file '{}' already exists.\n\nDo you want to overwrite it?", dest.display()))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();
                if confirm != rfd::MessageDialogResult::Yes {
                    return;
                }
            }

            let dest_str = normalize_path(&dest.to_string_lossy());
            let src_path = decoded.path.clone();
            ui.set_status_file_text(format!("Saving {}…", dest.display()).into());
            // Through the serial disk queue: never concurrent with a rotation of the same file.
            disk.submit(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| export_image(&cm, &decoded, &profile, &dest)))
                    .unwrap_or_else(|_| Err("internal error while encoding".to_string()));
                post_to_ui(ui_w, st_w, move |ui, st| match result {
                    Ok(()) => {
                        // The edit is saved: it is no longer an unsaved change, and its undo
                        // snapshots would no longer match the files.
                        st.edited_images.remove(&src_path);
                        st.history_undo.retain(|a| !matches!(a, HistoryAction::ImageEdit { path, .. } if *path == src_path));
                        st.history_redo.retain(|a| !matches!(a, HistoryAction::ImageEdit { path, .. } if *path == src_path));
                        if let Ok(mut g) = st.preloader.cache.lock() {
                            g.remove(&dest_str);
                        }
                        set_thumbnail_for_path(ui, None, &src_path, Image::default(), Some(false));
                        // Show the source as it is on disk now (overwritten, or untouched when
                        // the edit was exported to another file).
                        if st.current_decoded.as_ref().map(|d| same_path(&d.path, &src_path)).unwrap_or(false) {
                            ui.set_current_image_is_cropped(false);
                        }
                        reconcile_with_disk(ui, st, &src_path);
                        ui.set_status_file_text(format!("Saved '{}' with ICC profile '{}'", dest_str, profile).into());
                    }
                    Err(e) => {
                        ui.set_status_file_text(format!("Save failed: {}", e).into());
                        show_message_later("Save Error", &format!("Failed to save image:\n{}", e), rfd::MessageLevel::Error);
                    }
                });
            });
        });
    }

    // --- Color Profile Selection ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_profile_selected(move |val| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let chosen = val.to_string();

            if chosen == PROFILE_LOAD_CUSTOM {
                let picked = rfd::FileDialog::new()
                    .set_title("Load Custom ICC / ICM Color Profile")
                    .add_filter("ICC Color Profile (*.icc, *.icm)", &["icc", "icm"])
                    .pick_file();
                let mut st = lock_state(&state);
                if let Some(icc_path) = picked {
                    if let Some(label) = st.color_manager.register_custom_profile(&icc_path) {
                        let p_str = icc_path.to_string_lossy().to_string();
                        if !st.settings.custom_profiles.contains(&p_str) {
                            st.settings.custom_profiles.push(p_str);
                            st.settings.save();
                        }
                        rebuild_profile_combobox_list(&ui, &st);
                        let old = st.selected_profile.clone();
                        if old != label {
                            st.push_undo(HistoryAction::ColorProfile { old_profile: old, new_profile: label.clone() });
                        }
                        st.selected_profile = label.clone();
                        ui.set_current_profile(label.clone().into());
                        if let Some(curr) = st.current_decoded.as_ref().map(|d| d.path.clone()) {
                            st.image_profile_overrides.insert(curr, label);
                        }
                        request_render(&mut st, true, false);
                        return;
                    }
                    ui.set_status_file_text(format!("'{}' is not a valid RGB ICC profile.", icc_path.display()).into());
                }
                ui.set_current_profile(st.selected_profile.clone().into());
                return;
            }

            let mut st = lock_state(&state);
            let old_p = st.selected_profile.clone();
            if old_p != chosen {
                st.push_undo(HistoryAction::ColorProfile { old_profile: old_p, new_profile: chosen.clone() });
            }
            st.selected_profile = chosen.clone();
            if let Some(curr) = st.current_decoded.as_ref().map(|d| d.path.clone()) {
                st.image_profile_overrides.insert(curr, chosen);
            }
            request_render(&mut st, true, false);
        });
    }

    // --- Lock Color Profile & Lock Zoom Toggles ---
    {
        let state = state.clone();
        ui.on_lock_profile_toggled(move |checked| lock_state(&state).lock_profile = checked);
    }
    {
        let state = state.clone();
        ui.on_lock_zoom_toggled(move |checked| lock_state(&state).lock_zoom = checked);
    }

    // --- Zoom Controls (zoom_factor = physical screen pixels per image pixel) ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_in_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.is_fit_mode = false;
            st.zoom_factor = (st.zoom_factor * 1.25).clamp(0.02, 32.0);
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_out_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.is_fit_mode = false;
            st.zoom_factor = (st.zoom_factor / 1.25).clamp(0.02, 32.0);
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_100_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.is_fit_mode = false;
            st.zoom_factor = 1.0;
            st.pan_x = 0.0;
            st.pan_y = 0.0;
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_fit_clicked(move |_vw, _vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.is_fit_mode = true;
            if let Some(dec) = st.current_decoded.clone() {
                st.zoom_factor = calculate_fit_zoom(&ui, dec.width, dec.height);
                st.pan_x = 0.0;
                st.pan_y = 0.0;
                update_zoom_ui(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_set_factor(move |factor| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if !factor.is_finite() || factor <= 0.0 {
                return;
            }
            let mut st = lock_state(&state);
            st.is_fit_mode = false;
            st.zoom_factor = factor.clamp(0.02, 32.0);
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_window_resized(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            sync_display_scale(&ui, &mut st);
            if st.selected_indices.len() > 1 {
                refresh_multi_selection_overview(&ui, &st);
            } else if let Some(decoded) = st.current_decoded.clone() {
                if st.is_fit_mode && !st.lock_zoom {
                    st.zoom_factor = calculate_fit_zoom(&ui, decoded.width, decoded.height);
                    st.pan_x = 0.0;
                    st.pan_y = 0.0;
                    update_zoom_ui(&ui, &st);
                }
            }
        });
    }

    // --- Prev / Next / Filmstrip Navigation ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_prev_image_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            if st.visible_files.is_empty() {
                return;
            }
            let curr = st.current_index.unwrap_or(0);
            let prev = if curr == 0 { st.visible_files.len() - 1 } else { curr - 1 };
            st.selected_indices.clear();
            select_image_by_index(&ui, &mut st, prev);
            sync_filmstrip_selection_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_next_image_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            if st.visible_files.is_empty() {
                return;
            }
            let next = (st.current_index.unwrap_or(0) + 1) % st.visible_files.len();
            st.selected_indices.clear();
            select_image_by_index(&ui, &mut st, next);
            sync_filmstrip_selection_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_filmstrip_item_clicked(move |idx, is_ctrl, is_shift| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if idx >= 0 {
                handle_filmstrip_click(&ui, &mut lock_state(&state), idx as usize, is_ctrl, is_shift);
            }
        });
    }

    // --- Filmstrip Context Menu Callbacks ---
    {
        let ui_weak = ui.as_weak();
        ui.on_request_open_with_apps(move |file_path| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let handlers = crate::open_with::get_handlers_for_file(file_path.as_str());
            let items: Vec<OpenWithAppItem> = handlers
                .into_iter()
                .map(|h| {
                    let (icon, has_icon) = match h.icon_rgba {
                        Some(rgba) => (rgba_to_slint_image(h.icon_width, h.icon_height, &rgba), true),
                        None => (Image::default(), false),
                    };
                    OpenWithAppItem { id: h.id, name: h.name.into(), icon, has_icon }
                })
                .collect();
            ui.set_open_with_apps(ModelRc::new(VecModel::from(items)));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_with_app_clicked(move |app_id, file_path| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Err(e) = crate::open_with::launch_handler(file_path.as_str(), app_id) {
                ui.set_status_file_text(format!("Failed to open in app: {}", e).into());
            }
        });
    }
    ui.on_open_with_dialog_clicked(move |file_path| {
        crate::open_with::launch_open_with_dialog(file_path.as_str());
    });
    ui.on_open_containing_folder_explorer(move |file_path| reveal_in_file_manager(file_path.as_str()));
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rate_selected_images(move |rating| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            let paths: Vec<String> = selected_or_current(&st).into_iter().filter_map(|i| st.visible_files.get(i).cloned()).collect();
            set_ratings(&ui, &mut st, &paths, rating.clamp(0, 5) as u8, true);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rate_filmstrip_item(move |idx, rating| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if idx < 0 {
                return;
            }
            let mut st = lock_state(&state);
            let idx = idx as usize;
            let targets: Vec<usize> = if st.selected_indices.contains(&idx) && st.selected_indices.len() > 1 {
                st.selected_indices.iter().copied().collect()
            } else {
                vec![idx]
            };
            let paths: Vec<String> = targets.into_iter().filter_map(|i| st.visible_files.get(i).cloned()).collect();
            set_ratings(&ui, &mut st, &paths, rating.clamp(0, 5) as u8, true);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_delete_selected_images(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let (files, disk, weak) = {
                let st = lock_state(&state);
                let files: Vec<String> = selected_or_current(&st).into_iter().filter_map(|i| st.visible_files.get(i).cloned()).collect();
                (files, st.disk.clone(), weak_handles(&st))
            };
            let Some((ui_w, st_w)) = weak else { return };
            if files.is_empty() {
                return;
            }
            let what = if files.len() == 1 { format!("'{}'", file_name_of(&files[0])) } else { format!("{} files", files.len()) };
            let confirm = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("Move to Recycle Bin")
                .set_description(format!("Move {} to the Recycle Bin / Trash?", what))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if confirm != rfd::MessageDialogResult::Yes {
                return;
            }
            ui.set_status_file_text(format!("Moving {} to the Recycle Bin…", what).into());
            // Queued after any pending rotation of these files, so nothing writes them back.
            disk.submit(move || {
                let mut removed = Vec::new();
                let mut failed = Vec::new();
                for f in files {
                    match crate::disk_ops::move_to_trash(Path::new(&f)) {
                        Ok(()) => removed.push(f),
                        Err(e) => failed.push(format!("{}: {}", file_name_of(&f), e)),
                    }
                }
                post_to_ui(ui_w, st_w, move |ui, st| {
                    remove_files_from_view(ui, st, &removed);
                    if failed.is_empty() {
                        ui.set_status_file_text(format!("Moved {} file(s) to the Recycle Bin", removed.len()).into());
                    } else {
                        ui.set_status_file_text(format!("{} file(s) could not be moved to the Recycle Bin", failed.len()).into());
                        show_message_later(
                            "Delete",
                            &format!("These files were NOT deleted:\n\n{}", failed.join("\n")),
                            rfd::MessageLevel::Error,
                        );
                    }
                });
            });
        });
    }

    // --- Rename ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_request_rename(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = lock_state(&state);
            let indices: Vec<usize> = selected_or_current(&st).into_iter().filter(|&i| i < st.visible_files.len()).collect();
            if indices.is_empty() {
                return;
            }
            let target = st
                .current_index
                .filter(|cur| indices.contains(cur))
                .unwrap_or(indices[0]);
            let p = Path::new(&st.visible_files[target]);
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Image");
            ui.set_rename_dialog_visible(true);
            ui.set_rename_file_count(indices.len() as i32);
            ui.set_rename_input_text(stem.into());
            if indices.len() == 1 {
                let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{}", e)).unwrap_or_default();
                ui.set_rename_extension_text(ext.into());
            } else {
                ui.set_rename_extension_text("Original extensions preserved".into());
            }
            ui.set_rename_error_text("".into());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_confirm_rename(move |new_name| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            let files: Vec<String> = selected_or_current(&st).into_iter().filter_map(|i| st.visible_files.get(i).cloned()).collect();
            if files.is_empty() {
                ui.set_rename_dialog_visible(false);
                return;
            }
            let plan = match crate::rename::compute_batch_rename_plan(&files, new_name.as_str()) {
                Ok(p) => p,
                Err(err_msg) => {
                    ui.set_rename_error_text(err_msg.into());
                    return;
                }
            };
            if plan.is_empty() {
                ui.set_rename_dialog_visible(false);
                return;
            }
            // Let pending rotations finish first: they write to the old names.
            if !st.disk.flush(Duration::from_secs(10)) {
                ui.set_rename_error_text("Files are still being written; try again in a moment.".into());
                return;
            }
            if let Err(e) = crate::rename::execute_batch_rename(&plan) {
                ui.set_rename_error_text(e.into());
                return;
            }
            // A target can be another selected file's old name ("A → X (1)", "X (1) → X (2)"):
            // migrate through unique placeholders so every entry is rewritten exactly once.
            let moves: Vec<&(String, String)> = plan.iter().filter(|(o, n)| o != n).collect();
            let placeholders: Vec<String> = (0..moves.len()).map(|i| format!("\u{1}mr-rename-{}\u{1}", i)).collect();
            for ((old_f, _), ph) in moves.iter().map(|m| (&m.0, &m.1)).zip(&placeholders) {
                migrate_renamed_path(&ui, &mut st, old_f, ph, false);
            }
            for ((_, new_f), ph) in moves.iter().map(|m| (&m.0, &m.1)).zip(&placeholders) {
                migrate_renamed_path(&ui, &mut st, ph, new_f, true);
            }
            st.settings.save();
            if let Some(cur) = st.current_index.and_then(|i| st.visible_files.get(i)).cloned() {
                ui.set_status_file_text(format!("Renamed: {}", cur).into());
            }
            if st.selected_indices.len() > 1 {
                refresh_multi_selection_overview(&ui, &st);
            }
            ui.set_rename_dialog_visible(false);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_multi_select_tile_clicked(move |full_path| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            if let Some(target_idx) = st.visible_files.iter().position(|f| f.as_str() == full_path.as_str()) {
                st.selected_indices.clear();
                st.selected_indices.insert(target_idx);
                st.selection_anchor = Some(target_idx);
                st.is_fit_mode = true;
                ui.set_is_multi_selection(false);
                ui.set_multi_selection_tiles(ModelRc::new(VecModel::default()));
                select_image_by_index(&ui, &mut st, target_idx);
                sync_filmstrip_selection_ui(&ui, &st);
            }
        });
    }

    // --- Folder Tree & Recent Folders ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_tree_item_clicked(move |key| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            let key = key.to_string();
            st.selected_tree_filter = key.clone();
            st.settings.last_subfolder = key;
            st.settings.save();
            apply_tree_filter_and_populate(&ui, &mut st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_tree_item_toggle_expand(move |key| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            let key = key.to_string();
            let norm_root = normalize_path(&st.current_folder);
            let root_expanded = st.expanded_folders.contains("__ALL__")
                || st.expanded_folders.iter().any(|e| same_path(e, &norm_root))
                || st.expanded_folders.is_empty();
            if key == "__ALL__" {
                if root_expanded {
                    st.expanded_folders.remove("__ALL__");
                    st.expanded_folders.retain(|e| !same_path(e, &norm_root));
                } else {
                    st.expanded_folders.insert("__ALL__".to_string());
                    st.expanded_folders.insert(norm_root);
                }
            } else {
                let norm_k = normalize_path(&key);
                let existing = st.expanded_folders.iter().find(|e| same_path(e, &norm_k)).cloned();
                match existing {
                    Some(ex) => {
                        st.expanded_folders.remove(&ex);
                    }
                    None => {
                        st.expanded_folders.insert(norm_k);
                    }
                }
            }
            refresh_folder_tree_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_undo_action(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            undo_last_operation(&ui, &mut lock_state(&state));
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_redo_action(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            redo_last_operation(&ui, &mut lock_state(&state));
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_jump_forward_10(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            jump_images(&ui, &mut lock_state(&state), 10);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_jump_backward_10(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            jump_images(&ui, &mut lock_state(&state), -10);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_cycle_profile(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            cycle_color_profile(&ui, &mut lock_state(&state));
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rotate_cw(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            rotate_current_image(&ui, &mut lock_state(&state), true);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rotate_ccw(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            rotate_current_image(&ui, &mut lock_state(&state), false);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_apply_crop(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            apply_crop(&ui, &mut lock_state(&state));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_apply_custom_aspect_ratio(move |text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text = text.trim();
            if let Some(r) = parse_aspect_ratio_str(text) {
                if r > 0.01 && r < 100.0 {
                    ui.invoke_set_aspect_ratio(format!("Custom ({text})").into(), r);
                    ui.set_crop_custom_dialog_visible(false);
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_custom_aspect_ratio(move |text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text_trim = text.trim().to_string();
            if parse_aspect_ratio_str(&text_trim).is_some() {
                let mut st = lock_state(&state);
                if !st.settings.saved_aspect_ratios.iter().any(|s| s.eq_ignore_ascii_case(&text_trim)) {
                    st.settings.saved_aspect_ratios.push(text_trim);
                    st.settings.save();
                    let model: Vec<SharedString> = st.settings.saved_aspect_ratios.iter().map(|s| SharedString::from(s.as_str())).collect();
                    ui.set_saved_custom_aspect_ratios(ModelRc::new(VecModel::from(model)));
                }
                ui.set_crop_custom_dialog_visible(false);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_delete_custom_aspect_ratio(move |text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text_trim = text.trim();
            let mut st = lock_state(&state);
            st.settings.saved_aspect_ratios.retain(|s| !s.eq_ignore_ascii_case(text_trim));
            st.settings.save();
            let model: Vec<SharedString> = st.settings.saved_aspect_ratios.iter().map(|s| SharedString::from(s.as_str())).collect();
            ui.set_saved_custom_aspect_ratios(ModelRc::new(VecModel::from(model)));
        });
    }

    // --- Exit handling ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_request_exit(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let count = lock_state(&state).edited_images.len();
            if count > 0 {
                unsaved_edits_prompt(&ui, count);
            } else {
                ui.invoke_exit_app();
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_exit_app(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = lock_state(&state);
                store_window_geometry(&ui, &mut st);
                st.settings.save();
                if st.disk.pending() > 0 {
                    ui.set_status_file_text("Finishing pending file operations…".into());
                }
            }
            // Pending rotations are flushed after the event loop ends (see below).
            let _ = slint::quit_event_loop();
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_all_modified_and_exit(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let (jobs, cm, disk, weak) = {
                let st = lock_state(&state);
                let jobs: Vec<(String, Arc<DecodedImage>, String)> = st
                    .edited_images
                    .iter()
                    .map(|(p, d)| (p.clone(), d.clone(), effective_profile_for(&st, p, d)))
                    .collect();
                (jobs, st.color_manager.clone(), st.disk.clone(), weak_handles(&st))
            };
            let Some((ui_w, st_w)) = weak else { return };
            ui.set_exit_confirm_dialog_visible(false);
            ui.set_status_file_text(format!("Saving {} modified image(s)…", jobs.len()).into());
            disk.submit(move || {
                let mut saved = Vec::new();
                let mut failed = Vec::new();
                for (path, decoded, profile) in jobs {
                    let dest = generate_next_sequential_path(Path::new(&path));
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| export_image(&cm, &decoded, &profile, &dest)))
                        .unwrap_or_else(|_| Err("internal error while encoding".to_string()));
                    match res {
                        Ok(()) => saved.push(path),
                        Err(e) => failed.push(format!("{}: {}", file_name_of(&path), e)),
                    }
                }
                post_to_ui(ui_w, st_w, move |ui, st| {
                    for p in &saved {
                        st.edited_images.remove(p);
                    }
                    if failed.is_empty() {
                        // Exit from a fresh event-loop turn: the exit handler needs the state lock,
                        // which is held while this closure runs.
                        let ui_weak = ui.as_weak();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.invoke_exit_app();
                            }
                        });
                    } else {
                        ui.set_status_file_text(format!("{} image(s) could not be saved — the application stays open.", failed.len()).into());
                        show_message_later(
                            "Save Error",
                            &format!("These images could not be saved; nothing was lost, the application stays open:\n\n{}", failed.join("\n")),
                            rfd::MessageLevel::Error,
                        );
                    }
                });
            });
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_recent_folder_clicked(move |folder| {
            let Some(ui) = ui_weak.upgrade() else { return };
            open_folder_path(&ui, &mut lock_state(&state), &folder, None, None);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_recent_folder_remove(move |folder| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.settings.remove_recent_folder(&folder);
            refresh_recent_folders_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_recent_folders_clear(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.settings.clear_recent_folders();
            refresh_recent_folders_ui(&ui, &st);
        });
    }
    // Double click: 100% (1 image pixel = 1 screen pixel) centered on the cursor / back to fit.
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_double_clicked(move |mx, my, vw, vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            let Some(decoded) = st.current_decoded.clone() else { return };
            if (st.zoom_factor - 1.0).abs() < 0.01 && !st.is_fit_mode {
                st.zoom_factor = calculate_fit_zoom(&ui, decoded.width, decoded.height);
                st.pan_x = 0.0;
                st.pan_y = 0.0;
                st.is_fit_mode = true;
            } else {
                let scale_ratio = 1.0 / st.zoom_factor.max(0.0001);
                let cx = mx - vw * 0.5;
                let cy = my - vh * 0.5;
                st.pan_x = cx - (cx - st.pan_x) * scale_ratio;
                st.pan_y = cy - (cy - st.pan_y) * scale_ratio;
                st.zoom_factor = 1.0;
                st.is_fit_mode = false;
            }
            update_zoom_ui(&ui, &st);
        });
    }
    // --- Filters ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_filter_changed_advanced(move |search_text, ratings_mask, aspect_filter, profile_filter, f_cropped, f_prof_mod| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.filter_search_text = search_text.to_string();
            st.filter_ratings_mask = (ratings_mask & 63) as u8;
            st.filter_aspect_ratio = aspect_filter.to_string();
            st.filter_color_profile = profile_filter.to_string();
            st.filter_is_cropped = f_cropped;
            st.filter_is_profile_modified = f_prof_mod;
            apply_tree_filter_and_populate(&ui, &mut st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_filter_clear_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.filter_search_text.clear();
            st.filter_rating = 0;
            st.filter_ratings_mask = 0;
            st.filter_aspect_ratio = "ALL".to_string();
            st.filter_color_profile = "ALL".to_string();
            st.filter_format = "ALL".to_string();
            st.filter_is_cropped = false;
            st.filter_is_profile_modified = false;
            apply_tree_filter_and_populate(&ui, &mut st);
        });
    }

    // --- Keyboard shortcuts ---
    {
        let state = state.clone();
        ui.on_get_action_for_shortcut(move |pressed| action_for_shortcut(&lock_state(&state).settings, &pressed).into());
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_update_shortcut_key(move |idx, new_key| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if idx < 0 {
                return;
            }
            let model = ui.get_shortcut_items();
            if let Some(mut item) = model.row_data(idx as usize) {
                let (valid, _) = validate_shortcut_combo(&new_key);
                item.key_combo = new_key;
                item.is_valid = valid;
                model.set_row_data(idx as usize, item);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_reset_shortcuts_to_default(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.settings.shortcuts = crate::config::default_shortcuts();
            st.settings.save();
            populate_shortcuts_ui(&ui, &st.settings);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_shortcuts_and_close(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_shortcut_items();
            let mut entries: Vec<(usize, String, String)> = Vec::new(); // (row, id, canonical)
            let mut invalid = false;
            for i in 0..model.row_count() {
                if let Some(item) = model.row_data(i) {
                    let (valid, canonical) = validate_shortcut_combo(&item.key_combo);
                    if valid {
                        entries.push((i, item.id.to_string(), canonical));
                    } else {
                        invalid = true;
                    }
                }
            }
            // The same key may not trigger two different actions.
            let mut duplicates: Vec<usize> = Vec::new();
            for (a, (row_a, _, combo_a)) in entries.iter().enumerate() {
                if combo_a.is_empty() {
                    continue;
                }
                if entries.iter().enumerate().any(|(b, (_, _, combo_b))| a != b && combo_b.eq_ignore_ascii_case(combo_a)) {
                    duplicates.push(*row_a);
                }
            }
            if invalid || !duplicates.is_empty() {
                for row in &duplicates {
                    if let Some(mut item) = model.row_data(*row) {
                        item.is_valid = false;
                        model.set_row_data(*row, item);
                    }
                }
                ui.set_status_file_text(
                    if duplicates.is_empty() {
                        "Shortcuts not saved: fix the invalid key combinations (marked in red).".to_string()
                    } else {
                        "Shortcuts not saved: the same key is assigned to more than one action (marked in red).".to_string()
                    }
                    .into(),
                );
                // The dialog closes itself after this callback: reopen it so the user can fix it.
                let ui_weak = ui.as_weak();
                slint::Timer::single_shot(Duration::from_millis(1), move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        ui.set_shortcuts_dialog_visible(true);
                    }
                });
                return;
            }
            let mut st = lock_state(&state);
            for (_, id, canonical) in entries {
                if canonical.is_empty() {
                    st.settings.shortcuts.remove(&id);
                } else {
                    st.settings.shortcuts.insert(id, canonical);
                }
            }
            st.settings.save();
            populate_shortcuts_ui(&ui, &st.settings);
            ui.set_status_file_text("Keyboard shortcuts saved.".into());
        });
    }

    // --- Canvas: wheel zoom, pan, eyedropper, loupe ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_wheel(move |delta_y, mx, my, vw, vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            let old_zoom = st.zoom_factor;
            let factor = if delta_y > 0.0 { 1.18 } else { 1.0 / 1.18 };
            let new_zoom = (old_zoom * factor).clamp(0.02, 32.0);
            let scale_ratio = new_zoom / old_zoom;
            // Zoom anchored around the cursor position (pan is in logical pixels).
            let cx = mx - vw * 0.5;
            let cy = my - vh * 0.5;
            st.pan_x = cx - (cx - st.pan_x) * scale_ratio;
            st.pan_y = cy - (cy - st.pan_y) * scale_ratio;
            st.zoom_factor = new_zoom;
            st.is_fit_mode = false;
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_pan(move |dx, dy| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            st.pan_x += dx;
            st.pan_y += dy;
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_inspect_pixel(move |mx, my, vw, vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = lock_state(&state);
            let Some(decoded) = st.current_decoded.clone() else { return };

            let hud_x = (mx + 22.0).clamp(8.0, (vw - 296.0).max(8.0));
            let hud_y = (my + 22.0).clamp(8.0, (vh - 126.0).max(8.0));
            ui.set_eyedropper_x(hud_x);
            ui.set_eyedropper_y(hud_y);

            let Some((ix, iy)) = viewport_to_image_coords(mx, my, vw, vh, &st) else { return };
            let idx = ((iy as usize * decoded.width as usize) + ix as usize) * 3;
            // Numbers: document values (what the file means in the active profile).
            // Swatch: the color actually shown on this monitor.
            let doc: &[u8] = st.document_rgb.as_deref().map(|v| v.as_slice()).unwrap_or(&decoded.rgb_pixels);
            let Some(px) = doc.get(idx..idx + 3) else { return };
            let (r, g, b) = (px[0], px[1], px[2]);
            let swatch = current_rendered_bytes(&st).and_then(|s| s.get(idx..idx + 3)).map(|s| (s[0], s[1], s[2])).unwrap_or((r, g, b));
            let (h, s, l) = rgb_to_hsl(r, g, b);
            ui.set_eyedropper_color(Color::from_rgb_u8(swatch.0, swatch.1, swatch.2));
            ui.set_eyedropper_xy_text(format!("XY: ({}, {})", ix, iy).into());
            ui.set_eyedropper_rgb_text(format!("RGB: {:3}, {:3}, {:3}", r, g, b).into());
            ui.set_eyedropper_hsl_text(format!("HSL: {:3}°, {:3}%, {:3}%", h, s, l).into());
            ui.set_status_eyedropper_text(format!("XY:({},{}) RGB:({},{},{}) HSL:({}°,{}%,{}%)", ix, iy, r, g, b, h, s, l).into());
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_inspect_loupe(move |mx, my, vw, vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = lock_state(&state);
            let Some(decoded) = st.current_decoded.clone() else { return };

            // Center the 332x356 loupe overlay over the cursor.
            let lx = (mx - 166.0).clamp(4.0, (vw - 336.0).max(4.0));
            let ly = (my - 166.0).clamp(4.0, (vh - 360.0).max(4.0));
            ui.set_loupe_x(lx);
            ui.set_loupe_y(ly);

            let z = view_zoom(&st);
            let img_x0 = (vw - decoded.width as f32 * z) * 0.5 + st.pan_x;
            let img_y0 = (vh - decoded.height as f32 * z) * 0.5 + st.pan_y;
            let center_x = (mx - img_x0) / z;
            let center_y = (my - img_y0) / z;

            // 4× the current on-screen magnification.
            let effective_mag = (z * 4.0).clamp(0.20, 128.0);
            let pixels = current_rendered_bytes(&st).unwrap_or(&decoded.rgb_pixels);
            let (pw, ph, patch_rgba) = render_loupe_patch_rgba(center_x, center_y, effective_mag, decoded.width, decoded.height, pixels);
            ui.set_loupe_image(rgba_to_slint_image(pw, ph, &patch_rgba));
            let ix = center_x.floor().clamp(0.0, decoded.width.saturating_sub(1) as f32) as u32;
            let iy = center_y.floor().clamp(0.0, decoded.height.saturating_sub(1) as f32) as u32;
            ui.set_loupe_badge_text(format!("4× ({:.0}%) [{}, {}]", st.zoom_factor * 400.0, ix, iy).into());
        });
    }

    // --- MCP server (stdio) ---
    let args: Vec<String> = std::env::args().collect();
    let is_mcp_enabled = args.iter().any(|a| a == "--mcp" || a == "-mcp") || std::env::var("HORAMA_MCP").is_ok();
    let _mcp_timer = if is_mcp_enabled {
        let (mcp_tx, mcp_rx) = crossbeam_channel::unbounded::<McpCommand>();
        std::thread::spawn(move || run_mcp_stdio_server(mcp_tx));
        let timer = slint::Timer::default();
        let ui_weak = ui.as_weak();
        let state = state.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(20), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            while let Ok(cmd) = mcp_rx.try_recv() {
                handle_mcp_command(&ui, &state, cmd);
            }
        });
        Some(timer)
    } else {
        None
    };

    // --- Periodic window tracking: geometry, DPI scale and (Windows) per-monitor ICC ---
    let _win_timer = {
        let timer = slint::Timer::default();
        let ui_weak = ui.as_weak();
        let state = state.clone();
        let mut last_center: Option<(i32, i32)> = None;
        let mut ticks: u32 = 0;
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = lock_state(&state);
            ticks = ticks.wrapping_add(1);

            if sync_display_scale(&ui, &mut st) {
                if let Some(dec) = st.current_decoded.clone() {
                    if st.is_fit_mode && !st.lock_zoom {
                        st.zoom_factor = calculate_fit_zoom(&ui, dec.width, dec.height);
                        st.pan_x = 0.0;
                        st.pan_y = 0.0;
                    }
                }
                update_zoom_ui(&ui, &st);
            }

            // The monitor profile can differ between screens: re-detect it when the window moves.
            let pos = ui.window().position();
            let size = ui.window().size();
            let center = (pos.x + size.width as i32 / 2, pos.y + size.height as i32 / 2);
            if cfg!(target_os = "windows") && last_center != Some(center) {
                if st.color_manager.refresh_display_profile(Some(center)) {
                    ui.set_status_file_text(format!("Display profile: {}", st.color_manager.display_profile_name()).into());
                    request_render(&mut st, true, false);
                    spawn_filmstrip_thumbnail_loader(&ui, &mut st);
                }
            }
            last_center = Some(center);

            // Window geometry is saved every 2 seconds at most.
            if ticks % 4 == 0 {
                let before = (st.settings.window_maximized, st.settings.window_width, st.settings.window_height);
                store_window_geometry(&ui, &mut st);
                if before != (st.settings.window_maximized, st.settings.window_width, st.settings.window_height) {
                    st.settings.save();
                }
            }
        });
        timer
    };

    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.window().on_close_requested(move || -> slint::CloseRequestResponse {
            let count = lock_state(&state).edited_images.len();
            if count > 0 {
                if let Some(ui) = ui_weak.upgrade() {
                    unsaved_edits_prompt(&ui, count);
                }
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                slint::CloseRequestResponse::HideWindow
            }
        });
    }

    let res = ui.run();
    {
        let mut st = lock_state(&state);
        store_window_geometry(&ui, &mut st);
        st.settings.save();
        st.folder_watcher = None;
        // Never quit in the middle of a lossless rotation write.
        if !st.disk.flush(Duration::from_secs(10)) {
            eprintln!("Warning: some file operations did not finish before exit.");
        }
    }
    if let Err(ref e) = res {
        let msg = format!("Horama encountered a UI runtime error:\n\n{}", e);
        show_fatal_error_dialog("Horama - Runtime Error", &msg);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_manager::extract_icc_description;
    use crate::config::{PROFILE_ADOBE_RGB, PROFILE_PROPHOTO, PROFILE_SRGB};

    #[test]
    fn rename_plan_is_reexported_for_ui() {
        let files = vec![normalize_path("C:/photos/my_image.jpg")];
        let plan = compute_batch_rename_plan(&files, "vacation").unwrap();
        assert_eq!(plan[0].1, normalize_path("C:/photos/vacation.jpg"));
    }

    #[test]
    fn test_icc_generation_and_description() {
        let cm = ColorManager::new();
        let srgb_bytes = cm.get_profile_bytes(PROFILE_SRGB, None);
        assert!(srgb_bytes.len() >= 128);
        assert_eq!(extract_icc_description(&srgb_bytes).as_deref(), Some(PROFILE_SRGB));
        let adobe_bytes = cm.get_profile_bytes(PROFILE_ADOBE_RGB, None);
        assert_eq!(extract_icc_description(&adobe_bytes).as_deref(), Some(PROFILE_ADOBE_RGB));
        let test_px = vec![200u8, 100, 50, 50, 150, 220, 255, 255, 255, 128, 128, 128];
        let (out_srgb, _) = cm.apply_profile_with_info(&test_px, PROFILE_SRGB, None);
        assert_eq!(out_srgb.len(), test_px.len());
    }

    #[test]
    fn test_automatic_assign_and_convert_pipeline() {
        let cm = ColorManager::new();
        let sample_rgb = vec![200u8, 80, 40, 50, 180, 220];
        let (out_untagged, info_untagged) = cm.apply_profile_with_info(&sample_rgb, PROFILE_ADOBE_RGB, None);
        assert_eq!(out_untagged.len(), sample_rgb.len());
        assert!(!info_untagged.has_embedded);
        assert_eq!(info_untagged.source_name, PROFILE_ADOBE_RGB);

        let emb_bytes = cm.get_profile_bytes(PROFILE_ADOBE_RGB, None);
        let (out_conv, info_conv) = cm.apply_profile_with_info(&sample_rgb, PROFILE_PROPHOTO, Some(&emb_bytes));
        assert_eq!(out_conv.len(), sample_rgb.len());
        assert!(info_conv.has_embedded);
        assert!(info_conv.source_name.contains("→"));
        // Convert mode exposes document values different from the source pixels.
        let r = cm.render(&sample_rgb, PROFILE_PROPHOTO, Some(&emb_bytes));
        assert!(r.document.is_some());
    }

    #[test]
    fn test_rgb_to_hsl_and_loupe_patch() {
        assert_eq!(rgb_to_hsl(255, 0, 0), (0, 100, 50));
        assert_eq!(rgb_to_hsl(0, 255, 0), (120, 100, 50));
        assert_eq!(rgb_to_hsl(0, 0, 255), (240, 100, 50));
        let rgb = vec![128u8; 16 * 16 * 3];
        let (pw, ph, rgba) = render_loupe_patch_rgba(8.0, 8.0, 4.0, 16, 16, &rgb);
        assert_eq!((pw, ph), (320, 320));
        assert_eq!(rgba.len(), 320 * 320 * 4);
    }

    #[test]
    fn badge_roundtrip() {
        for b in [Badge::None, Badge::Deleted, Badge::Renamed, Badge::New, Badge::Modified] {
            assert_eq!(Badge::from_kind(b.kind()), b);
        }
    }

    #[test]
    fn test_validate_shortcut_combo() {
        assert_eq!(validate_shortcut_combo("c"), (true, "C".to_string()));
        assert_eq!(validate_shortcut_combo("up"), (true, "Up".to_string()));
        assert_eq!(validate_shortcut_combo("F2"), (true, "F2".to_string()));
        assert_eq!(validate_shortcut_combo("delete"), (true, "Delete".to_string()));
        assert_eq!(validate_shortcut_combo("+"), (true, "+".to_string()));
        assert_eq!(validate_shortcut_combo("="), (true, "+".to_string()));
        assert_eq!(validate_shortcut_combo("ctrl++"), (true, "Ctrl++".to_string()));
        assert_eq!(validate_shortcut_combo("cmd+z"), (true, "Ctrl+Z".to_string()));
        assert_eq!(validate_shortcut_combo("ctrl+shift+o"), (true, "Ctrl+Shift+O".to_string()));
        assert_eq!(validate_shortcut_combo(""), (true, "".to_string()));
        assert!(!validate_shortcut_combo("asdfgh").0);
        assert!(!validate_shortcut_combo("ctrl+").0);
        assert!(!validate_shortcut_combo("ctrl+a+b").0);
    }

    #[test]
    fn shortcut_lookup_is_deterministic_and_handles_control_codes() {
        let mut settings = AppSettings::default();
        settings.shortcuts.clear();
        settings.shortcuts.insert("undo".to_string(), "Ctrl+Z".to_string());
        settings.shortcuts.insert("next_image".to_string(), "Right".to_string());
        assert_eq!(action_for_shortcut(&settings, "Ctrl+Z"), "undo");
        assert_eq!(action_for_shortcut(&settings, "Ctrl+\u{1a}"), "undo");
        assert_eq!(action_for_shortcut(&settings, "cmd+z"), "undo");
        assert_eq!(action_for_shortcut(&settings, "Right"), "next_image");
        assert_eq!(action_for_shortcut(&settings, "space"), "");
        settings.shortcuts.insert("zoom_fit".to_string(), "Ctrl+0".to_string());
        assert_eq!(action_for_shortcut(&settings, "space"), "zoom_fit");
    }

    #[test]
    fn pixel_history_is_capped() {
        let mut state = ViewerState::new();
        let img = Arc::new(DecodedImage {
            path: "a.jpg".into(),
            width: 1,
            height: 1,
            rgb_pixels: vec![0, 0, 0],
            embedded_icc: None,
            embedded_icc_name: None,
            ignored_icc_note: None,
            format_name: "JPEG".into(),
            bit_depth: 8,
            is_raw: false,
            file_size: 0,
        });
        for _ in 0..(MAX_PIXEL_HISTORY + 5) {
            state.push_undo(HistoryAction::ImageEdit {
                path: "a.jpg".into(),
                before: img.clone(),
                after: img.clone(),
                before_cropped: false,
                after_cropped: true,
                desc: "Crop".into(),
            });
        }
        let pixel = state.history_undo.iter().filter(|a| matches!(a, HistoryAction::ImageEdit { .. })).count();
        assert_eq!(pixel, MAX_PIXEL_HISTORY);
        state.edited_images.insert("a.jpg".into(), img);
        assert!(state.is_cropped("a.jpg"));
    }

    #[test]
    fn format_filter_matches_extensions() {
        assert!(matches_format_filter("x/a.JPG", "JPG"));
        assert!(matches_format_filter("x/a.jpeg", "jpeg"));
        assert!(matches_format_filter("x/a.cr3", "RAW"));
        assert!(!matches_format_filter("x/a.png", "RAW"));
        assert!(matches_format_filter("x/a.tif", "TIFF"));
        assert!(matches_format_filter("x/a.png", "ALL"));
    }

    #[test]
    fn test_aspect_ratio_filters() {
        let empty: Vec<String> = Vec::new();
        assert!(matches_aspect_ratio_filter(6000.0 / 4000.0, "3x2", &empty));
        assert!(!matches_aspect_ratio_filter(6000.0 / 4000.0, "16x9", &empty));
        assert!(matches_aspect_ratio_filter(1920.0 / 1080.0, "16x9", &empty));
        assert!(matches_aspect_ratio_filter(1.0, "1x1", &empty));
        assert!(matches_aspect_ratio_filter(2500.0 / 1200.0, "OTHER", &empty));
        let saved = vec!["18x9".to_string()];
        assert!(matches_aspect_ratio_filter(2.0, "18x9", &saved));
        assert!(!matches_aspect_ratio_filter(2.0, "OTHER", &saved));
    }

    #[test]
    fn test_sequential_save_filename_generation() {
        let seq1 = generate_next_sequential_path(Path::new("C:/photos/vacation.jpg"));
        assert_eq!(seq1, Path::new("C:/photos/vacation_1.jpg"));
        let seq2 = generate_next_sequential_path(Path::new("C:/photos/vacation_1.jpg"));
        assert_eq!(seq2, Path::new("C:/photos/vacation_2.jpg"));
    }
}
