#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cache_manager;
mod color_manager;
mod config;
mod cr3_parser;
mod file_watcher;
mod image_loader;
mod image_saver;
mod mcp_server;
mod metadata;
mod open_with;

use mcp_server::{McpCommand, run_mcp_stdio_server};
use serde_json::{json, Value};

slint::include_modules!();

use crate::cache_manager::PreloadController;
use crate::color_manager::{rgb_to_hsl, ColorManager};
use crate::config::{
    normalize_path, scan_folder_and_subfolders, AppSettings, BUILTIN_PROFILES,
    DEFAULT_FALLBACK_PROFILE, PROFILE_EMBEDDED, PROFILE_LOAD_CUSTOM,
};
use crate::image_loader::{load_thumbnail_rgba, DecodedImage};
use crate::image_saver::{generate_next_sequential_path, save_image_with_icc};
use crate::metadata::{extract_metadata, render_histogram_rgba};
use rayon::prelude::*;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, SharedPixelBuffer, SharedString, VecModel};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
    ImageEdit {
        path: String,
        before: Arc<DecodedImage>,
        after: Arc<DecodedImage>,
        before_cropped: bool,
        after_cropped: bool,
        saved_to_disk: bool,
        desc: String,
    },
}

struct ViewerState {
    settings: AppSettings,
    color_manager: ColorManager,
    preloader: PreloadController,
    thumbnail_generation: Arc<AtomicU64>,
    history_undo: Vec<HistoryAction>,
    history_redo: Vec<HistoryAction>,
    is_undoing_redoing: bool,
    cropped_files: std::collections::HashSet<String>,
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
    rendered_rgb: Vec<u8>,

    selected_profile: String,
    lock_profile: bool,
    lock_zoom: bool,
    is_fit_mode: bool,
    image_profile_overrides: HashMap<String, String>,

    zoom_factor: f32,
    pan_x: f32,
    pan_y: f32,
    folder_watcher: Option<crate::file_watcher::FolderWatcher>,
    expanded_folders: std::collections::HashSet<String>,
    filter_search_text: String,
    filter_rating: i32,
    filter_format: String,
    filter_color_profile: String,
    filter_aspect_ratio: String,
    filter_ratings_mask: u8,
}

impl ViewerState {
    fn push_undo(&mut self, action: HistoryAction) {
        if self.is_undoing_redoing {
            return;
        }
        self.history_undo.push(action);
        if self.history_undo.len() > 60 {
            self.history_undo.remove(0);
        }
        self.history_redo.clear();
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
            thumbnail_generation: Arc::new(AtomicU64::new(1)),
            history_undo: Vec::new(),
            history_redo: Vec::new(),
            is_undoing_redoing: false,
            cropped_files: std::collections::HashSet::new(),
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
            rendered_rgb: Vec::new(),
            selected_profile: DEFAULT_FALLBACK_PROFILE.to_string(),
            lock_profile: false,
            lock_zoom: false,
            is_fit_mode: true,
            image_profile_overrides: HashMap::new(),
            zoom_factor: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            folder_watcher: None,
            expanded_folders: std::collections::HashSet::new(),
            filter_search_text: String::new(),
            filter_rating: 0,
            filter_format: "ALL".to_string(),
            filter_color_profile: "ALL".to_string(),
            filter_aspect_ratio: "ALL".to_string(),
            filter_ratings_mask: 0,
        }
    }
}

fn rgb_to_slint_image(width: u32, height: u32, rgb: &[u8]) -> Image {
    let mut buf = SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
    buf.make_mut_bytes().copy_from_slice(rgb);
    Image::from_rgb8(buf)
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

fn refresh_recent_folders_ui(ui: &AppWindow, state: &ViewerState) {
    let mut items = Vec::new();
    let norm_curr = normalize_path(&state.current_folder).to_lowercase();
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
        let active = !norm_curr.is_empty() && folder.to_lowercase() == norm_curr;
        items.push(RecentFolderData {
            name: name.into(),
            parent_path: parent.into(),
            full_path: folder.clone().into(),
            active,
        });
    }
    ui.set_recent_folders(ModelRc::new(VecModel::from(items)));
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

fn calculate_fit_zoom(ui: &AppWindow, img_width: u32, img_height: u32) -> f32 {
    let cw = ui.get_canvas_width();
    let ch = ui.get_canvas_height();
    let (vw, vh) = if cw > 64.0 && ch > 64.0 {
        (cw, ch)
    } else {
        let win_size = ui.window().size();
        let w = if win_size.width > 100 {
            win_size.width as f32
        } else {
            1440.0
        };
        let h = if win_size.height > 100 {
            win_size.height as f32
        } else {
            900.0
        };
        ((w - 530.0).max(400.0), (h - 252.0).max(300.0))
    };
    let zx = (vw - 24.0).max(64.0) / (img_width.max(1) as f32);
    let zy = (vh - 24.0).max(64.0) / (img_height.max(1) as f32);
    zx.min(zy).clamp(0.02, 32.0)
}

fn render_current_image_and_panels(ui: &AppWindow, state: &mut ViewerState, preserve_view: bool) {
    let Some(decoded) = state.current_decoded.clone() else {
        ui.set_has_image(false);
        return;
    };

    let (transformed_rgb, xfm_info) = state.color_manager.apply_profile_with_info(
        &decoded.rgb_pixels,
        &state.selected_profile,
        decoded.embedded_icc.as_deref(),
    );

    state.rendered_rgb = transformed_rgb;

    let slint_img = rgb_to_slint_image(decoded.width, decoded.height, &state.rendered_rgb);
    ui.set_current_image(slint_img);
    ui.set_image_pixel_width(decoded.width as i32);
    ui.set_image_pixel_height(decoded.height as i32);
    ui.set_has_image(true);

    if !preserve_view && !state.lock_zoom {
        state.zoom_factor = calculate_fit_zoom(ui, decoded.width, decoded.height);
        state.pan_x = 0.0;
        state.pan_y = 0.0;
        state.is_fit_mode = true;
    }
    update_zoom_ui(ui, state);

    // Update Embedded ICC badge
    ui.set_has_embedded_icc(xfm_info.has_embedded);
    if let Some(emb_name) = &xfm_info.embedded_name {
        ui.set_embedded_badge_text(format!("Embedded: {}", emb_name).into());
        ui.set_meta_embedded_icc(format!("Embedded ICC: {}", emb_name).into());
    } else {
        ui.set_embedded_badge_text(
            format!("No ICC (Default: {})", DEFAULT_FALLBACK_PROFILE).into(),
        );
        ui.set_meta_embedded_icc(
            format!("Embedded ICC: None (Default {})", DEFAULT_FALLBACK_PROFILE).into(),
        );
    }

    ui.set_meta_active_icc(format!("Active Profile: {}", xfm_info.source_name).into());
    ui.set_meta_display_icc(format!("Display Target: {}", xfm_info.display_name).into());

    // Render Histogram
    let (hw, hh, hist_rgba) = render_histogram_rgba(&state.rendered_rgb);
    ui.set_histogram_image(rgba_to_slint_image(hw, hh, &hist_rgba));

    // Populate EXIF & Image Metadata rows
    let meta_pairs = extract_metadata(&decoded);
    let meta_models: Vec<MetadataRowData> = meta_pairs
        .into_iter()
        .map(|(k, v)| MetadataRowData {
            key: k.into(),
            value: v.into(),
        })
        .collect();
    ui.set_metadata_rows(ModelRc::new(VecModel::from(meta_models)));

    // Status bar text
    let idx_num = state.current_index.map(|i| i + 1).unwrap_or(1);
    let total = state.visible_files.len();
    let fname = Path::new(&decoded.path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| decoded.path.clone());
    ui.set_status_file_text(
        format!(
            "[{}/{}] {} — {}×{} px ({}) | Profile: {}",
            idx_num, total, fname, decoded.width, decoded.height, decoded.format_name, xfm_info.source_name
        )
        .into(),
    );
    update_cache_status_ui(ui, state);
}

fn rotate_current_image(ui: &AppWindow, state: &mut ViewerState, cw: bool) {
    let Some(decoded) = state.current_decoded.clone() else { return };

    let was_cropped = state.cropped_files.contains(&decoded.path);
    let before_image = decoded.clone();

    let new_dec = if cw {
        decoded.rotate_90_cw()
    } else {
        decoded.rotate_90_ccw()
    };
    let new_arc = Arc::new(new_dec);
    state.push_undo(HistoryAction::ImageEdit {
        path: decoded.path.clone(),
        before: before_image,
        after: new_arc.clone(),
        before_cropped: was_cropped,
        after_cropped: was_cropped,
        saved_to_disk: true,
        desc: if cw { "Rotate 90° CW".to_string() } else { "Rotate 90° CCW".to_string() },
    });
    state.current_decoded = Some(new_arc.clone());
    if let Ok(mut guard) = state.preloader.cache.lock() {
        guard.insert(decoded.path.clone(), new_arc.clone());
    }

    // Task 7: Update filmstrip thumbnail immediately with rotated image
    if let Some(cur_idx) = state.current_index {
        if let Some((tw, th, rgba)) = crate::image_loader::create_thumbnail_from_decoded(&new_arc) {
            let model = ui.get_filmstrip_items();
            if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
                if let Some(mut item) = vm.row_data(cur_idx) {
                    item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                    item.has_thumbnail = true;
                    vm.set_row_data(cur_idx, item);
                }
            }
        }
    }

    // Task 8: Auto-save rotated image to disk automatically without asking confirmation
    let path_to_save = new_arc.path.clone();
    let dec_to_save = new_arc.clone();
    std::thread::spawn(move || {
        let p = Path::new(&path_to_save);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if matches!(ext.as_str(), "jpg" | "jpeg" | "jpe" | "png" | "tiff" | "tif" | "webp" | "bmp") {
            let _ = crate::image_saver::save_image_with_icc(
                p,
                dec_to_save.width,
                dec_to_save.height,
                &dec_to_save.rgb_pixels,
                dec_to_save.embedded_icc.as_deref(),
                false,
            );
        }
    });

    render_current_image_and_panels(ui, state, false);
}

fn apply_crop(ui: &AppWindow, state: &mut ViewerState) {
    let Some(decoded) = state.current_decoded.clone() else { return };

    let was_cropped = state.cropped_files.contains(&decoded.path);
    let before_image = decoded.clone();

    let norm_x = ui.get_crop_norm_x().clamp(0.0, 1.0);
    let norm_y = ui.get_crop_norm_y().clamp(0.0, 1.0);
    let norm_w = ui.get_crop_norm_w().clamp(0.02, 1.0);
    let norm_h = ui.get_crop_norm_h().clamp(0.02, 1.0);

    let crop_x = (norm_x * decoded.width as f32).round() as u32;
    let crop_y = (norm_y * decoded.height as f32).round() as u32;
    let crop_w = (norm_w * decoded.width as f32).round().max(1.0) as u32;
    let crop_h = (norm_h * decoded.height as f32).round().max(1.0) as u32;

    let cropped_dec = decoded.crop(crop_x, crop_y, crop_w, crop_h);
    let new_arc = Arc::new(cropped_dec);
    state.push_undo(HistoryAction::ImageEdit {
        path: decoded.path.clone(),
        before: before_image,
        after: new_arc.clone(),
        before_cropped: was_cropped,
        after_cropped: true,
        saved_to_disk: false,
        desc: "Crop Image".to_string(),
    });
    state.current_decoded = Some(new_arc.clone());
    state.cropped_files.insert(decoded.path.clone());
    ui.set_current_image_is_cropped(true);

    if let Ok(mut guard) = state.preloader.cache.lock() {
        guard.insert(decoded.path.clone(), new_arc.clone());
    }

    // Task 7: Update filmstrip thumbnail immediately with cropped image and crop badge
    if let Some(cur_idx) = state.current_index {
        if let Some((tw, th, rgba)) = crate::image_loader::create_thumbnail_from_decoded(&new_arc) {
            let model = ui.get_filmstrip_items();
            if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
                if let Some(mut item) = vm.row_data(cur_idx) {
                    item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                    item.has_thumbnail = true;
                    item.is_cropped = true;
                    vm.set_row_data(cur_idx, item);
                }
            }
        }
    }

    // Note: Cropping does NOT auto-save to disk; user saves manually if desired

    ui.set_crop_active(false);
    ui.set_crop_norm_x(0.05);
    ui.set_crop_norm_y(0.05);
    ui.set_crop_norm_w(0.90);
    ui.set_crop_norm_h(0.90);

    render_current_image_and_panels(ui, state, true);
}

fn undo_last_operation(ui: &AppWindow, state: &mut ViewerState) {
    let Some(action) = state.history_undo.pop() else {
        ui.set_status_file_text("Nothing to undo".into());
        return;
    };
    state.is_undoing_redoing = true;

    match action.clone() {
        HistoryAction::ImageSelection { from_idx, to_idx: _ } => {
            state.history_redo.push(action);
            if state.history_redo.len() > 60 { state.history_redo.remove(0); }
            if from_idx < state.visible_files.len() {
                select_image_by_index(ui, state, from_idx);
                ui.set_status_file_text(format!("Undone image selection -> Image #{}", from_idx + 1).into());
            }
        }
        HistoryAction::ColorProfile { old_profile, new_profile: _ } => {
            state.history_redo.push(action);
            if state.history_redo.len() > 60 { state.history_redo.remove(0); }
            state.selected_profile = old_profile.clone();
            ui.set_current_profile(old_profile.clone().into());
            if let Some(curr) = state.current_decoded.as_ref().map(|d| d.path.clone()) {
                state.image_profile_overrides.insert(curr, old_profile.clone());
            }
            render_current_image_and_panels(ui, state, true);
            ui.set_status_file_text(format!("Undone color profile -> {}", old_profile).into());
        }
        HistoryAction::Rating { path, old_rating, new_rating: _ } => {
            state.history_redo.push(action);
            if state.history_redo.len() > 60 { state.history_redo.remove(0); }
            state.settings.set_rating(&path, old_rating as u8);
            let _ = state.settings.save();
            ui.set_current_rating(old_rating);
            sync_filmstrip_selection_ui(ui, state);
            ui.set_status_file_text(format!("Undone rating -> {} stars", old_rating).into());
        }
        HistoryAction::ImageEdit {
            path,
            before,
            after: _,
            before_cropped,
            after_cropped: _,
            saved_to_disk,
            desc,
        } => {
            state.history_redo.push(action);
            if state.history_redo.len() > 60 { state.history_redo.remove(0); }

            if before_cropped {
                state.cropped_files.insert(path.clone());
            } else {
                state.cropped_files.remove(&path);
            }
            ui.set_current_image_is_cropped(before_cropped);

            if let Ok(mut guard) = state.preloader.cache.lock() {
                guard.insert(path.clone(), before.clone());
            }

            if state.current_decoded.as_ref().map(|d| d.path.as_str()) == Some(&path) {
                state.current_decoded = Some(before.clone());
                render_current_image_and_panels(ui, state, true);
            }

            if let Some(cur_idx) = state.visible_files.iter().position(|f| f == &path) {
                if let Some((tw, th, rgba)) = crate::image_loader::create_thumbnail_from_decoded(&before) {
                    let model = ui.get_filmstrip_items();
                    if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
                        if let Some(mut item) = vm.row_data(cur_idx) {
                            item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                            item.has_thumbnail = true;
                            item.is_cropped = before_cropped;
                            vm.set_row_data(cur_idx, item);
                        }
                    }
                }
            }

            if saved_to_disk {
                let p_save = path.clone();
                let img_save = before.clone();
                std::thread::spawn(move || {
                    let p = Path::new(&p_save);
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    if matches!(ext.as_str(), "jpg" | "jpeg" | "jpe" | "png" | "tiff" | "tif" | "webp" | "bmp") {
                        let _ = crate::image_saver::save_image_with_icc(
                            p,
                            img_save.width,
                            img_save.height,
                            &img_save.rgb_pixels,
                            img_save.embedded_icc.as_deref(),
                            false,
                        );
                    }
                });
            }

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

    match action.clone() {
        HistoryAction::ImageSelection { from_idx: _, to_idx } => {
            state.history_undo.push(action);
            if state.history_undo.len() > 60 { state.history_undo.remove(0); }
            if to_idx < state.visible_files.len() {
                select_image_by_index(ui, state, to_idx);
                ui.set_status_file_text(format!("Redone image selection -> Image #{}", to_idx + 1).into());
            }
        }
        HistoryAction::ColorProfile { old_profile: _, new_profile } => {
            state.history_undo.push(action);
            if state.history_undo.len() > 60 { state.history_undo.remove(0); }
            state.selected_profile = new_profile.clone();
            ui.set_current_profile(new_profile.clone().into());
            if let Some(curr) = state.current_decoded.as_ref().map(|d| d.path.clone()) {
                state.image_profile_overrides.insert(curr, new_profile.clone());
            }
            render_current_image_and_panels(ui, state, true);
            ui.set_status_file_text(format!("Redone color profile -> {}", new_profile).into());
        }
        HistoryAction::Rating { path, old_rating: _, new_rating } => {
            state.history_undo.push(action);
            if state.history_undo.len() > 60 { state.history_undo.remove(0); }
            state.settings.set_rating(&path, new_rating as u8);
            let _ = state.settings.save();
            ui.set_current_rating(new_rating);
            sync_filmstrip_selection_ui(ui, state);
            ui.set_status_file_text(format!("Redone rating -> {} stars", new_rating).into());
        }
        HistoryAction::ImageEdit {
            path,
            before: _,
            after,
            before_cropped: _,
            after_cropped,
            saved_to_disk,
            desc,
        } => {
            state.history_undo.push(action);
            if state.history_undo.len() > 60 { state.history_undo.remove(0); }

            if after_cropped {
                state.cropped_files.insert(path.clone());
            } else {
                state.cropped_files.remove(&path);
            }
            ui.set_current_image_is_cropped(after_cropped);

            if let Ok(mut guard) = state.preloader.cache.lock() {
                guard.insert(path.clone(), after.clone());
            }

            if state.current_decoded.as_ref().map(|d| d.path.as_str()) == Some(&path) {
                state.current_decoded = Some(after.clone());
                render_current_image_and_panels(ui, state, true);
            }

            if let Some(cur_idx) = state.visible_files.iter().position(|f| f == &path) {
                if let Some((tw, th, rgba)) = crate::image_loader::create_thumbnail_from_decoded(&after) {
                    let model = ui.get_filmstrip_items();
                    if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
                        if let Some(mut item) = vm.row_data(cur_idx) {
                            item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                            item.has_thumbnail = true;
                            item.is_cropped = after_cropped;
                            vm.set_row_data(cur_idx, item);
                        }
                    }
                }
            }

            if saved_to_disk {
                let p_save = path.clone();
                let img_save = after.clone();
                std::thread::spawn(move || {
                    let p = Path::new(&p_save);
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                    if matches!(ext.as_str(), "jpg" | "jpeg" | "jpe" | "png" | "tiff" | "tif" | "webp" | "bmp") {
                        let _ = crate::image_saver::save_image_with_icc(
                            p,
                            img_save.width,
                            img_save.height,
                            &img_save.rgb_pixels,
                            img_save.embedded_icc.as_deref(),
                            false,
                        );
                    }
                });
            }

            ui.set_status_file_text(format!("Redone: {}", desc).into());
        }
    }

    state.is_undoing_redoing = false;
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

fn cycle_color_profile(ui: &AppWindow, state: &mut ViewerState) {
    let mut profiles: Vec<String> = crate::config::BUILTIN_PROFILES
        .iter()
        .map(|s| s.to_string())
        .collect();
    for (name, _) in state.color_manager.get_custom_profiles() {
        profiles.push(name);
    }
    if profiles.is_empty() {
        return;
    }
    let has_embedded = state
        .current_decoded
        .as_ref()
        .map(|d| d.embedded_icc.is_some())
        .unwrap_or(false);
    if !has_embedded {
        profiles.retain(|p| p != crate::config::PROFILE_EMBEDDED);
    }

    let cur = &state.selected_profile;
    let next_profile = if let Some(pos) = profiles.iter().position(|p| p == cur) {
        profiles[(pos + 1) % profiles.len()].clone()
    } else {
        profiles[0].clone()
    };
    let old_p = state.selected_profile.clone();
    if old_p != next_profile && !state.is_undoing_redoing {
        state.push_undo(HistoryAction::ColorProfile { old_profile: old_p, new_profile: next_profile.clone() });
    }
    state.selected_profile = next_profile.clone();
    if let Some(dec) = &state.current_decoded {
        state.image_profile_overrides.insert(dec.path.clone(), next_profile.clone());
    }
    ui.set_current_profile(next_profile.into());
    render_current_image_and_panels(ui, state, false);
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
                    is_cropped: state.cropped_files.contains(fpath),
                });
            }
        }
    }

    ui.set_multi_selection_tiles(ModelRc::new(VecModel::from(tiles)));
}

fn handle_filmstrip_click(
    ui: &AppWindow,
    state_arc: &Arc<Mutex<ViewerState>>,
    idx: usize,
    is_ctrl: bool,
    is_shift: bool,
) {
    let mut state = state_arc.lock().unwrap();
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
        select_image_by_index(ui, &mut state, idx);
    } else if is_ctrl {
        if state.selected_indices.contains(&idx) {
            if state.selected_indices.len() > 1 {
                state.selected_indices.remove(&idx);
            }
        } else {
            state.selected_indices.insert(idx);
            state.selection_anchor = Some(idx);
            select_image_by_index(ui, &mut state, idx);
        }
    } else {
        state.selected_indices.clear();
        state.selected_indices.insert(idx);
        state.selection_anchor = Some(idx);
        select_image_by_index(ui, &mut state, idx);
    }

    sync_filmstrip_selection_ui(ui, &state);
}

fn select_image_by_index(ui: &AppWindow, state: &mut ViewerState, index: usize) {
    if index >= state.visible_files.len() {
        return;
    }

    let old_idx = state.current_index;
    if !state.is_undoing_redoing {
        if let Some(from) = old_idx {
            if from != index {
                state.push_undo(HistoryAction::ImageSelection { from_idx: from, to_idx: index });
            }
        }
    }

    let path = state.visible_files[index].clone();
    state.current_index = Some(index);
    if state.selected_indices.is_empty() || !state.selected_indices.contains(&index) {
        state.selected_indices.clear();
        state.selected_indices.insert(index);
        state.selection_anchor = Some(index);
    }

    // Update filmstrip selection indicator & read item badge
    let mut badge_type = String::new();
    let filmstrip_model = ui.get_filmstrip_items();
    if let Some(vec_model) = filmstrip_model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
    {
        for i in 0..vec_model.row_count() {
            if let Some(mut item) = vec_model.row_data(i) {
                let should_sel = state.selected_indices.contains(&i);
                if i == index {
                    badge_type = item.badge_type.to_string();
                }
                if item.selected != should_sel {
                    item.selected = should_sel;
                    vec_model.set_row_data(i, item);
                }
            }
        }
    }

    // Set image overlay notices based on current selection
    if badge_type == "deleted" {
        ui.set_current_file_deleted(true);
        ui.set_current_file_renamed(false);
        ui.set_current_file_new(false);
        ui.set_current_file_modified(false);
        ui.set_current_file_badge_text("⚠️ Deleted from disk".into());
    } else if badge_type == "renamed" {
        ui.set_current_file_deleted(false);
        ui.set_current_file_renamed(true);
        ui.set_current_file_new(false);
        ui.set_current_file_modified(false);
        ui.set_current_file_badge_text("✏️ Renamed".into());
    } else if badge_type == "new" {
        ui.set_current_file_deleted(false);
        ui.set_current_file_renamed(false);
        ui.set_current_file_new(true);
        ui.set_current_file_modified(false);
        ui.set_current_file_badge_text("✨ New".into());
    } else if badge_type == "modified" {
        ui.set_current_file_deleted(false);
        ui.set_current_file_renamed(false);
        ui.set_current_file_new(false);
        ui.set_current_file_modified(true);
        ui.set_current_file_badge_text("🔄 Modified".into());
    } else {
        ui.set_current_file_deleted(false);
        ui.set_current_file_renamed(false);
        ui.set_current_file_new(false);
        ui.set_current_file_modified(false);
        ui.set_current_file_badge_text("".into());
    }

    // Check if file exists on disk or is cached in RAM
    let file_exists = Path::new(&path).exists();
    let cached_in_ram = state
        .preloader
        .cache
        .lock()
        .ok()
        .and_then(|g| g.get(&path));

    ui.set_current_image_is_cropped(state.cropped_files.contains(&path));

    if !file_exists && cached_in_ram.is_none() {
        state.current_decoded = None;
        ui.set_has_image(false);
        ui.set_canvas_empty_text("⚠️ Image Unavailable — File was deleted from disk".into());
        let fname = Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        ui.set_status_file_text(format!("Error: File '{}' is not available on disk.", fname).into());
        return;
    }

    match state.preloader.get_or_load(&path) {
        Ok(decoded) => {
            // Determine effective Color Profile for this image:
            // 1. If Lock Color Profile is enabled, keep current selected_profile and store override
            // 2. Else if user previously modified the profile on this image, restore from image_profile_overrides
            // 3. Else if image has an embedded ICC profile, select "Embedded Profile"
            // 4. Else select DEFAULT_FALLBACK_PROFILE ("sRGB IEC61966-2.1")
            if state.lock_profile {
                state
                    .image_profile_overrides
                    .insert(path.clone(), state.selected_profile.clone());
            } else if let Some(saved_prof) = state.image_profile_overrides.get(&path).cloned() {
                state.selected_profile = saved_prof;
            } else if decoded.embedded_icc_name.is_some() {
                state.selected_profile = PROFILE_EMBEDDED.to_string();
            } else {
                state.selected_profile = DEFAULT_FALLBACK_PROFILE.to_string();
            }

            ui.set_current_profile(state.selected_profile.clone().into());
            state.current_decoded = Some(decoded);
            render_current_image_and_panels(ui, state, state.is_undoing_redoing);
            ui.invoke_ensure_filmstrip_item_visible(index as i32);

            // Trigger instantaneous neighborhood preloading into RAM cache (+/- 1, 2, 3...)
            let files_clone = state.visible_files.clone();
            let ui_weak = ui.as_weak();
            let cache_ref = state.preloader.cache.clone();
            let total_cnt = files_clone.len();
            state.preloader.preload_neighborhood(index, &files_clone, move || {
                let ui_c = ui_weak.clone();
                let c_ref = cache_ref.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_c.upgrade() {
                        if let Ok(guard) = c_ref.lock() {
                            let (count, used, budget) = guard.stats();
                            let used_mb = used as f64 / (1024.0 * 1024.0);
                            let budget_mb = budget as f64 / (1024.0 * 1024.0);
                            ui.set_status_cache_text(
                                format!(
                                    "RAM Cache: {} / {} ({:.0} / {:.0} MB)",
                                    count, total_cnt, used_mb, budget_mb
                                )
                                .into(),
                            );
                        }
                    }
                });
            });
        }
        Err(err) => {
            ui.set_status_file_text(format!("Error loading {}: {}", path, err).into());
        }
    }
}

fn get_image_aspect_ratio_norm(path: &str, cache: &std::sync::Arc<std::sync::Mutex<crate::cache_manager::RamCache>>) -> Option<f32> {
    if let Ok(guard) = cache.lock() {
        if let Some(dec) = guard.get(path) {
            let w = dec.width;
            let h = dec.height;
            if w > 0 && h > 0 {
                return Some(w.max(h) as f32 / w.min(h) as f32);
            }
        }
    }
    let p = std::path::Path::new(path);
    if let Ok(sz) = imagesize::size(p) {
        if sz.width > 0 && sz.height > 0 {
            return Some(sz.width.max(sz.height) as f32 / sz.width.min(sz.height) as f32);
        }
    }
    // Fallback: extract dimensions from RAW header / decode metadata
    if let Ok(raw_img) = rawloader::decode_file(p) {
        let w = raw_img.width as u32;
        let h = raw_img.height as u32;
        if w > 0 && h > 0 {
            return Some(w.max(h) as f32 / w.min(h) as f32);
        }
    }
    None
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

fn get_file_color_profile_info(
    path: &str,
    overrides: &std::collections::HashMap<String, String>,
    cache: &std::sync::Arc<std::sync::Mutex<crate::cache_manager::RamCache>>,
) -> (String, bool) {
    if let Some(prof) = overrides.get(path) {
        return (prof.clone(), true);
    }
    if let Ok(guard) = cache.lock() {
        if let Some(dec) = guard.get(path) {
            if let Some(ref name) = dec.embedded_icc_name {
                return (name.clone(), true);
            } else if dec.embedded_icc.is_some() {
                return ("Embedded ICC".to_string(), true);
            } else {
                return ("sRGB IEC61966-2.1".to_string(), false);
            }
        }
    }
    ("sRGB IEC61966-2.1".to_string(), false)
}

fn apply_tree_filter_and_populate(ui: &AppWindow, state: &mut ViewerState) {
    let root_norm = normalize_path(&state.current_folder);
    let filter_key = state.selected_tree_filter.clone();

    state.visible_files = if filter_key == "__ALL__" {
        state.all_folder_files.clone()
    } else if filter_key == "__ROOT_ONLY__" {
        state
            .all_folder_files
            .iter()
            .filter(|f| {
                Path::new(f)
                    .parent()
                    .map(|p| normalize_path(&p.to_string_lossy()).eq_ignore_ascii_case(&root_norm))
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    } else {
        let prefix = format!("{}{}", filter_key, std::path::MAIN_SEPARATOR).to_lowercase();
        let target_low = filter_key.to_lowercase();
        state
            .all_folder_files
            .iter()
            .filter(|f| {
                let fl = f.to_lowercase();
                fl.starts_with(&prefix)
                    || Path::new(f)
                        .parent()
                        .map(|p| normalize_path(&p.to_string_lossy()).to_lowercase() == target_low)
                        .unwrap_or(false)
            })
            .cloned()
            .collect()
    };




    // Advanced Multi-Criteria Filtering (Search text, Exact Multi-Rating, Aspect Ratio, Color Profile, Modification Statuses)
    let search_low = state.filter_search_text.trim().to_lowercase();
    let r_mask = state.filter_ratings_mask;
    let ar_filt = state.filter_aspect_ratio.clone();
    let cp_filt = state.filter_color_profile.clone();
    let f_cropped = state.filter_is_cropped;
    let f_prof_mod = state.filter_is_profile_modified;

    let needs_filtering = !search_low.is_empty()
        || (r_mask != 0 && r_mask != 63)
        || (ar_filt != "ALL" && !ar_filt.is_empty())
        || (cp_filt != "ALL" && !cp_filt.is_empty())
        || f_cropped
        || f_prof_mod;

    if needs_filtering {
        let cache_ref = state.preloader.cache.clone();
        let overrides_ref = state.image_profile_overrides.clone();
        let settings_clone = state.settings.clone();
        let saved_aspects = state.settings.saved_aspect_ratios.clone();
        let cropped_set = state.cropped_files.clone();

        state.visible_files.retain(|fpath| {
            // Task 18: Search text filter matches filename including extension
            if !search_low.is_empty() {
                let fname = Path::new(fpath)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
                    .to_lowercase();
                if !fname.contains(&search_low) {
                    return false;
                }
            }

            // Task 21: Checkbox for cropped image
            if f_cropped && !cropped_set.contains(fpath) {
                return false;
            }

            // Task 21: Checkbox for modified profile
            if f_prof_mod && !overrides_ref.contains_key(fpath) {
                return false;
            }

            // Task 4: Exact Multi-Rating filter
            if r_mask != 0 && r_mask != 63 {
                let r = settings_clone.get_rating(fpath) as u8;
                if (r_mask & (1 << r)) == 0 {
                    return false;
                }
            }

            // Tasks 5, 6, 7: Aspect Ratio filter (orientation-agnostic, 99% correspondence)
            if ar_filt != "ALL" && !ar_filt.is_empty() {
                if let Some(ratio) = get_image_aspect_ratio_norm(fpath, &cache_ref) {
                    if !matches_aspect_ratio_filter(ratio, &ar_filt, &saved_aspects) {
                        return false;
                    }
                } else {
                    return false;
                }
            }

            // Task 2: Color Profile filter (support NONE = images without embedded ICC)
            if cp_filt != "ALL" && !cp_filt.is_empty() {
                let (img_prof, has_embedded) = get_file_color_profile_info(fpath, &overrides_ref, &cache_ref);
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

    refresh_folder_tree_ui(ui, state);

    // Build initial Filmstrip items
    let mut film_items = Vec::with_capacity(state.visible_files.len());
    for (i, fpath) in state.visible_files.iter().enumerate() {
        let fname = Path::new(fpath)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| fpath.clone());
        let rel = Path::new(fpath)
            .strip_prefix(&root_norm)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| fname.clone());

        let rating = state.settings.get_rating(fpath) as i32;
        film_items.push(FilmstripItemData {
            label: fname.into(),
            rel_path: rel.into(),
            full_path: fpath.clone().into(),
            thumbnail: Image::default(),
            has_thumbnail: false,
            selected: i == 0,
            badge_type: "".into(),
            badge_text: "".into(),
            rating,
            is_cropped: state.cropped_files.contains(fpath),
        });
    }
    ui.set_filmstrip_items(ModelRc::new(VecModel::from(film_items)));
    ui.set_filmstrip_offset_x(0.0);

    if !state.visible_files.is_empty() {
        select_image_by_index(ui, state, 0);
    } else {
        state.current_index = None;
        state.current_decoded = None;
        ui.set_has_image(false);
        ui.set_status_file_text("No supported images found in the selected folder filter.".into());
    }

    // Spawn high-speed prioritized, batched thumbnail loader
    spawn_filmstrip_thumbnail_loader(ui, state);
}

fn spawn_filmstrip_thumbnail_loader(ui: &AppWindow, state: &mut ViewerState) {
    let cur_idx = state.current_index.unwrap_or(0);
    let mut prioritized_indices: Vec<usize> = (0..state.visible_files.len()).collect();
    prioritized_indices.sort_by_key(|&idx| {
        let diff = (idx as isize - cur_idx as isize).abs();
        if idx >= cur_idx {
            diff * 2
        } else {
            diff * 2 + 1
        }
    });

    let (tx, rx) = crossbeam_channel::unbounded();
    let gen = state.thumbnail_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let files_to_load = state.visible_files.clone();
    let gen_arc = state.thumbnail_generation.clone();
    let cm_arc = Arc::new(state.color_manager.clone());
    let ui_weak = ui.as_weak();

    // Worker threads: Compute thumbnails in prioritized order using Rayon & shared ColorManager
    let gen_arc_worker = gen_arc.clone();
    let files_worker = files_to_load.clone();
    std::thread::spawn(move || {
        prioritized_indices.into_par_iter().for_each(|idx| {
            if gen_arc_worker.load(Ordering::SeqCst) != gen {
                return;
            }
            let file_path = &files_worker[idx];
            if let Some((tw, th, rgba)) =
                load_thumbnail_rgba(Path::new(file_path), Some(&cm_arc))
            {
                if gen_arc_worker.load(Ordering::SeqCst) != gen {
                    return;
                }
                let _ = tx.send((idx, tw, th, rgba));
            } else if gen_arc_worker.load(Ordering::SeqCst) == gen {
                // Send neutral fallback placeholder tile so "Loading..." never hangs
                let _ = tx.send((idx, 140, 98, vec![30u8; 140 * 98 * 4]));
            }
        });
    });

    // Distributor thread: Batches thumbnail updates to Slint UI without freezing the event loop
    let gen_arc_dist = gen_arc.clone();
    std::thread::spawn(move || {
        let mut count = 0;
        let mut batch = Vec::with_capacity(16);
        let mut last_flush = std::time::Instant::now();

        while let Ok((idx, tw, th, rgba)) = rx.recv() {
            if gen_arc_dist.load(Ordering::SeqCst) != gen {
                return;
            }
            batch.push((idx, tw, th, rgba));
            count += 1;

            // Immediate flush for first 10 thumbnails for instantaneous perceived responsiveness
            let should_flush = (count <= 10)
                || (batch.len() >= 16)
                || (last_flush.elapsed() >= std::time::Duration::from_millis(40));

            if should_flush {
                let to_send = std::mem::take(&mut batch);
                last_flush = std::time::Instant::now();
                let ui_copy = ui_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_copy.upgrade() {
                        let model = ui.get_filmstrip_items();
                        if let Some(vm) =
                            model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
                        {
                            for (idx, tw, th, rgba) in to_send {
                                if let Some(mut item) = vm.row_data(idx) {
                                    item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                                    item.has_thumbnail = true;
                                    vm.set_row_data(idx, item);
                                }
                            }
                        }
                    }
                });
            }
        }

        // Flush any remaining thumbnails
        if !batch.is_empty() {
            let to_send = batch;
            let ui_copy = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_copy.upgrade() {
                    let model = ui.get_filmstrip_items();
                    if let Some(vm) =
                        model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
                    {
                        for (idx, tw, th, rgba) in to_send {
                            if let Some(mut item) = vm.row_data(idx) {
                                item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                                item.has_thumbnail = true;
                                vm.set_row_data(idx, item);
                            }
                        }
                    }
                }
            });
        }
    });
}


fn refresh_current_folder_content(ui: &AppWindow, state: &mut ViewerState) {
    if state.current_folder.is_empty() {
        if state.is_files_mode && !state.visible_files.is_empty() {
            state.visible_files.retain(|f| Path::new(f).exists());
            state.all_folder_files.retain(|f| Path::new(f).exists());
            apply_tree_filter_and_populate(ui, state);
            ui.set_status_file_text(
                format!("Refreshed: {} files available", state.visible_files.len()).into(),
            );
        } else {
            ui.set_status_file_text("No folder loaded to refresh.".into());
        }
        return;
    }

    let folder = state.current_folder.clone();
    let norm = normalize_path(&folder);
    if !Path::new(&norm).is_dir() {
        ui.set_status_file_text(format!("Folder not found: {}", norm).into());
        return;
    }

    // Remember currently viewed file to preserve selection position
    let cur_file = state
        .current_index
        .and_then(|idx| state.visible_files.get(idx).cloned());

    // Rescan folder and all subfolders from disk
    let (files, subdirs) = scan_folder_and_subfolders(&norm);
    state.all_folder_files = files;
    state.subfolders = subdirs;

    // Clear preloader RAM cache to refresh decoded images
    if let Ok(mut guard) = state.preloader.cache.lock() {
        guard.clear();
    }

    // If current tree filter no longer exists among subfolders and isn't special, reset to __ALL__
    if state.selected_tree_filter != "__ALL__" && state.selected_tree_filter != "__ROOT_ONLY__" {
        let filter_still_valid = state
            .subfolders
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&state.selected_tree_filter));
        if !filter_still_valid {
            state.selected_tree_filter = "__ALL__".to_string();
        }
    }

    // Refresh UI tree & filmstrip
    apply_tree_filter_and_populate(ui, state);

    // Restore selected image if still present
    if let Some(target_file) = cur_file {
        if let Some(new_idx) = state.visible_files.iter().position(|f| f == &target_file) {
            select_image_by_index(ui, state, new_idx);
        } else if !state.visible_files.is_empty() {
            select_image_by_index(ui, state, 0);
        } else {
            state.current_index = None;
            state.current_decoded = None;
            state.rendered_rgb.clear();
            ui.set_has_image(false);
        }
    } else if !state.visible_files.is_empty() {
        select_image_by_index(ui, state, 0);
    }

    update_cache_status_ui(ui, state);

    let img_count = state.visible_files.len();
    let folder_count = state.subfolders.len();
    ui.set_status_file_text(
        format!(
            "Refreshed: {} images found across {} subfolder(s)",
            img_count, folder_count
        )
        .into(),
    );
}

fn handle_filesystem_sync(
    ui: &AppWindow,
    state: &mut ViewerState,
    actions: Vec<crate::file_watcher::FileSyncAction>,
) {
    let model = ui.get_filmstrip_items();
    let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() else {
        return;
    };

    let cur_path = state
        .current_index
        .and_then(|idx| state.visible_files.get(idx).cloned());

    let mut tree_changed = false;
    let mut folder_structure_changed = false;

    for action in actions {
        match action {
            crate::file_watcher::FileSyncAction::Removed(removed_path) => {
                let norm_rem = normalize_path(&removed_path).to_lowercase();
                for i in 0..vm.row_count() {
                    if let Some(mut item) = vm.row_data(i) {
                        if normalize_path(item.full_path.as_str()).to_lowercase() == norm_rem {
                            item.badge_type = "deleted".into();
                            item.badge_text = "🗑 Deleted".into();
                            vm.set_row_data(i, item);
                        }
                    }
                }
                if let Some(cp) = &cur_path {
                    if normalize_path(cp).to_lowercase() == norm_rem {
                        ui.set_current_file_deleted(true);
                        ui.set_current_file_renamed(false);
                        ui.set_current_file_new(false);
                        ui.set_current_file_modified(false);
                        ui.set_current_file_badge_text("⚠️ Deleted from disk".into());
                        let fname = Path::new(cp)
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| cp.clone());
                        ui.set_status_file_text(
                            format!("Current image: {} [Deleted from disk]", fname).into(),
                        );
                    }
                }
            }
            crate::file_watcher::FileSyncAction::Restored(restored_path) => {
                let norm_res = normalize_path(&restored_path).to_lowercase();
                for i in 0..vm.row_count() {
                    if let Some(mut item) = vm.row_data(i) {
                        if normalize_path(item.full_path.as_str()).to_lowercase() == norm_res {
                            if item.badge_type == "deleted" {
                                item.badge_type = "".into();
                                item.badge_text = "".into();
                                vm.set_row_data(i, item);
                            }
                        }
                    }
                }
                if let Some(cp) = &cur_path {
                    if normalize_path(cp).to_lowercase() == norm_res {
                        ui.set_current_file_deleted(false);
                        ui.set_current_file_renamed(false);
                        ui.set_current_file_new(false);
                        ui.set_current_file_modified(false);
                        ui.set_current_file_badge_text("".into());
                    }
                }
            }
            crate::file_watcher::FileSyncAction::Renamed { old_path, new_path } => {
                let norm_old = normalize_path(&old_path).to_lowercase();
                let new_fname = Path::new(&new_path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| new_path.clone());

                for i in 0..vm.row_count() {
                    if let Some(mut item) = vm.row_data(i) {
                        if normalize_path(item.full_path.as_str()).to_lowercase() == norm_old {
                            item.full_path = new_path.clone().into();
                            item.label = new_fname.clone().into();
                            item.badge_type = "renamed".into();
                            item.badge_text = "✏️ Renamed".into();
                            vm.set_row_data(i, item);
                            if let Some(vf) = state.visible_files.get_mut(i) {
                                *vf = new_path.clone();
                            }
                        }
                    }
                }

                // Update state files list
                for f in &mut state.all_folder_files {
                    if normalize_path(f).to_lowercase() == norm_old {
                        *f = new_path.clone();
                    }
                }

                // Re-key cached entry in RAM cache if already loaded
                if let Ok(mut guard) = state.preloader.cache.lock() {
                    if let Some(cached) = guard.get(&old_path) {
                        guard.insert(new_path.clone(), cached);
                    }
                }

                // If currently viewed file was renamed, update current image info in-place
                if let Some(cp) = &cur_path {
                    if normalize_path(cp).to_lowercase() == norm_old {
                        if let Some(dec) = &mut state.current_decoded {
                            let mut new_dec = (**dec).clone();
                            new_dec.path = new_path.clone();
                            state.current_decoded = Some(Arc::new(new_dec));
                        }
                        ui.set_current_file_deleted(false);
                        ui.set_current_file_renamed(true);
                        ui.set_current_file_new(false);
                        ui.set_current_file_modified(false);
                        ui.set_current_file_badge_text("✏️ Renamed".into());
                        render_current_image_and_panels(ui, state, true);
                    }
                }
            }
            crate::file_watcher::FileSyncAction::Created(created_path) => {
                if state.is_files_mode {
                    // In single-files mode, do not inject unrelated new files from the folder into the filmstrip
                    continue;
                }
                let norm_cre = normalize_path(&created_path);

                let already_exists = state
                    .all_folder_files
                    .iter()
                    .any(|f| normalize_path(f).eq_ignore_ascii_case(&norm_cre));

                if !already_exists {
                    state.all_folder_files.push(norm_cre.clone());
                    state.all_folder_files.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
                    tree_changed = true;

                    // Check if matches current tree filter
                    let root_norm = normalize_path(&state.current_folder);
                    let should_show = if state.selected_tree_filter == "__ALL__" {
                        true
                    } else if state.selected_tree_filter == "__ROOT_ONLY__" {
                        Path::new(&norm_cre)
                            .parent()
                            .map(|p| normalize_path(&p.to_string_lossy()).eq_ignore_ascii_case(&root_norm))
                            .unwrap_or(false)
                    } else {
                        let prefix = format!("{}{}", state.selected_tree_filter, std::path::MAIN_SEPARATOR).to_lowercase();
                        norm_cre.to_lowercase().starts_with(&prefix)
                    };

                    if should_show {
                        let new_fname = Path::new(&norm_cre)
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| norm_cre.clone());
                        let rel = Path::new(&norm_cre)
                            .strip_prefix(&root_norm)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| new_fname.clone());

                        let new_idx = vm.row_count();
                        state.visible_files.push(norm_cre.clone());

                        let rating = state.settings.get_rating(&norm_cre) as i32;
                        vm.push(FilmstripItemData {
                            label: new_fname.into(),
                            rel_path: rel.into(),
                            full_path: norm_cre.clone().into(),
                            thumbnail: Image::default(),
                            has_thumbnail: false,
                            selected: false,
                            badge_type: "new".into(),
                            badge_text: "✨ New".into(),
                            rating,
                            is_cropped: false,
                        });

                        // Generate thumbnail for the newly added file
                        let cm = state.color_manager.clone();
                        let ui_w = ui.as_weak();
                        let fpath = norm_cre.clone();
                        std::thread::spawn(move || {
                            if let Some((tw, th, rgba)) =
                                load_thumbnail_rgba(Path::new(&fpath), Some(&cm))
                            {
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_w.upgrade() {
                                        let model = ui.get_filmstrip_items();
                                        if let Some(vm) =
                                            model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
                                        {
                                            if let Some(mut item) = vm.row_data(new_idx) {
                                                item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                                                item.has_thumbnail = true;
                                                vm.set_row_data(new_idx, item);
                                            }
                                        }
                                    }
                                });
                            }
                        });
                    }
                }
            }
            crate::file_watcher::FileSyncAction::Modified(modified_path) => {
                let norm_mod = normalize_path(&modified_path);
                if !Path::new(&norm_mod).exists() {
                    continue;
                }
                let norm_mod_low = norm_mod.to_lowercase();

                // Invalidate cached entry in RAM cache
                if let Ok(mut guard) = state.preloader.cache.lock() {
                    guard.remove(&norm_mod);
                }

                // Update badge in filmstrip item & reload its thumbnail
                for i in 0..vm.row_count() {
                    if let Some(mut item) = vm.row_data(i) {
                        if normalize_path(item.full_path.as_str()).to_lowercase() == norm_mod_low {
                            item.badge_type = "modified".into();
                            item.badge_text = "🔄 Modified".into();
                            vm.set_row_data(i, item);

                            let cm = state.color_manager.clone();
                            let ui_w = ui.as_weak();
                            let fpath = norm_mod.clone();
                            let row_idx = i;
                            std::thread::spawn(move || {
                                if let Some((tw, th, rgba)) =
                                    load_thumbnail_rgba(Path::new(&fpath), Some(&cm))
                                {
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = ui_w.upgrade() {
                                            let model = ui.get_filmstrip_items();
                                            if let Some(vm) =
                                                model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
                                            {
                                                if let Some(mut item) = vm.row_data(row_idx) {
                                                    item.thumbnail = rgba_to_slint_image(tw, th, &rgba);
                                                    item.has_thumbnail = true;
                                                    vm.set_row_data(row_idx, item);
                                                }
                                            }
                                        }
                                    });
                                }
                            });
                        }
                    }
                }

                // If currently viewed file was modified, reload and re-render on canvas
                if let Some(cp) = &cur_path {
                    if normalize_path(cp).to_lowercase() == norm_mod_low {
                        ui.set_current_file_deleted(false);
                        ui.set_current_file_renamed(false);
                        ui.set_current_file_new(false);
                        ui.set_current_file_modified(true);
                        ui.set_current_file_badge_text("🔄 Modified".into());

                        let path_to_load = norm_mod.clone();
                        match state.preloader.get_or_load(&path_to_load) {
                            Ok(decoded) => {
                                state.current_decoded = Some(decoded);
                                render_current_image_and_panels(ui, state, true);
                            }
                            Err(err) => {
                                ui.set_status_file_text(
                                    format!("Error reloading modified image: {}", err).into(),
                                );
                            }
                        }
                    }
                }
            }
            crate::file_watcher::FileSyncAction::FolderStructureChanged => {
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

fn open_specific_files(ui: &AppWindow, state_arc: &Arc<Mutex<ViewerState>>, files: Vec<String>) {
    if files.is_empty() {
        return;
    }

    let parent_dir = Path::new(&files[0])
        .parent()
        .map(|p| normalize_path(&p.to_string_lossy()))
        .unwrap_or_default();

    let scanned_subfolders = if !parent_dir.is_empty() {
        let (_, subs) = scan_folder_and_subfolders(&parent_dir);
        subs
    } else {
        Vec::new()
    };

    {
        let mut state = state_arc.lock().unwrap();
        state.is_files_mode = true;
        state.current_folder = parent_dir.clone();
        if !parent_dir.is_empty() {
            state.settings.add_recent_folder(&parent_dir);
        }
        ui.set_folder_path(format!("{} selected file(s)", files.len()).into());
        refresh_recent_folders_ui(ui, &state);

        if let Ok(mut guard) = state.preloader.cache.lock() {
            guard.clear();
        }

        state.all_folder_files = files.clone();
        state.visible_files = files.clone();
        state.subfolders = scanned_subfolders;
        state.selected_tree_filter = "__ALL__".to_string();
        state.selected_indices.clear();
        state.selected_indices.insert(0);
        state.selection_anchor = Some(0);

        ui.set_has_subfolders(true);
        refresh_folder_tree_ui(ui, &state);

        let mut film_items = Vec::with_capacity(files.len());
        for (i, fpath) in files.iter().enumerate() {
            let fname = Path::new(fpath)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| fpath.clone());
            let rating = state.settings.get_rating(fpath) as i32;

            film_items.push(FilmstripItemData {
                label: fname.clone().into(),
                rel_path: fname.into(),
                full_path: fpath.clone().into(),
                thumbnail: Image::default(),
                has_thumbnail: false,
                selected: i == 0,
                badge_type: "".into(),
                badge_text: "".into(),
                rating,
                is_cropped: state.cropped_files.contains(fpath),
            });
        }
        ui.set_filmstrip_items(ModelRc::new(VecModel::from(film_items)));
        ui.set_filmstrip_offset_x(0.0);

        select_image_by_index(ui, &mut state, 0);

        spawn_filmstrip_thumbnail_loader(ui, &mut state);
    }

    if !parent_dir.is_empty() {
        let ui_weak = ui.as_weak();
        let state_weak = Arc::downgrade(state_arc);
        match crate::file_watcher::FolderWatcher::start(&parent_dir, move |actions| {
            let ui_c = ui_weak.clone();
            let st_w = state_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_c.upgrade() else { return };
                let Some(st_arc) = st_w.upgrade() else { return };
                let mut st = st_arc.lock().unwrap();
                handle_filesystem_sync(&ui, &mut st, actions);
            });
        }) {
            Ok(watcher) => {
                state_arc.lock().unwrap().folder_watcher = Some(watcher);
            }
            Err(err) => {
                eprintln!("Warning: Failed to start folder watcher: {}", err);
            }
        }
    }
}

fn open_folder_path(ui: &AppWindow, state_arc: &Arc<Mutex<ViewerState>>, folder: &str) {
    let norm = normalize_path(folder);
    if !Path::new(&norm).is_dir() {
        ui.set_status_file_text(format!("Folder does not exist: {}", norm).into());
        return;
    }

    {
        let mut state = state_arc.lock().unwrap();
        state.is_files_mode = false;
        state.current_folder = norm.clone();
        state.settings.add_recent_folder(&norm);
        let folder_changed = state.settings.last_folder != norm;
        state.settings.last_folder = norm.clone();
        if folder_changed {
            state.settings.last_subfolder = "__ALL__".to_string();
        }
        state.settings.save();
        ui.set_folder_path(norm.clone().into());
        refresh_recent_folders_ui(ui, &state);

        if let Ok(mut guard) = state.preloader.cache.lock() {
            guard.clear();
        }

        // Reset any existing search filters when opening a folder
        state.filter_search_text.clear();
        state.filter_rating = 0;
        ui.set_filter_search_text("".into());
        ui.invoke_clear_ratings();

        let (files, subdirs) = scan_folder_and_subfolders(&norm);
        state.all_folder_files = files;
        state.subfolders = subdirs;
        state.selected_tree_filter = "__ALL__".to_string();
        state.expanded_folders.clear();
        state.expanded_folders.insert("__ALL__".to_string());
        state.expanded_folders.insert(norm.clone());

        apply_tree_filter_and_populate(ui, &mut state);
    }

    // Start background live filesystem watcher
    let ui_weak = ui.as_weak();
    let state_weak = Arc::downgrade(state_arc);
    match crate::file_watcher::FolderWatcher::start(&norm, move |actions| {
        let ui_c = ui_weak.clone();
        let st_w = state_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_c.upgrade() else { return };
            let Some(st_arc) = st_w.upgrade() else { return };
            let mut st = st_arc.lock().unwrap();
            handle_filesystem_sync(&ui, &mut st, actions);
        });
    }) {
        Ok(watcher) => {
            state_arc.lock().unwrap().folder_watcher = Some(watcher);
        }
        Err(err) => {
            eprintln!("Warning: Failed to start folder watcher: {}", err);
        }
    }
}

/// Convert viewport mouse coordinates (mx, my) into image pixel coordinates (ix, iy).
fn viewport_to_image_coords(
    mx: f32,
    my: f32,
    vw: f32,
    vh: f32,
    state: &ViewerState,
) -> Option<(u32, u32)> {
    let decoded = state.current_decoded.as_ref()?;
    let img_w = decoded.width as f32 * state.zoom_factor;
    let img_h = decoded.height as f32 * state.zoom_factor;
    let img_x0 = (vw - img_w) * 0.5 + state.pan_x;
    let img_y0 = (vh - img_h) * 0.5 + state.pan_y;

    let rel_x = (mx - img_x0) / state.zoom_factor;
    let rel_y = (my - img_y0) / state.zoom_factor;

    if rel_x < 0.0
        || rel_y < 0.0
        || rel_x >= decoded.width as f32
        || rel_y >= decoded.height as f32
    {
        return None;
    }

    Some((
        (rel_x.floor() as u32).min(decoded.width.saturating_sub(1)),
        (rel_y.floor() as u32).min(decoded.height.saturating_sub(1)),
    ))
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


// ----------------------------------------------------------------------------
// Batch & Single File Rename Utilities (OS File Explorer Method)
// ----------------------------------------------------------------------------

/// Computes an OS-style rename plan for single or multiple files.
///
/// For 1 file:
/// - If input contains an extension, uses that extension.
/// - If input has no extension, preserves the original extension.
/// - Validates against illegal characters and existing filenames.
///
/// For multiple files (OS Windows Explorer method):
/// - Extracts the base stem from `input_base_name`.
/// - Sequentially names files: `Base (1).ext`, `Base (2).ext`, etc.
/// - Preserves each file's original extension.
/// - Safely skips counter numbers that already exist on disk outside this batch.
pub fn compute_batch_rename_plan(
    files: &[String],
    input_base_name: &str,
) -> Result<Vec<(String, String)>, String> {
    let raw_input = input_base_name.trim();
    if raw_input.is_empty() {
        return Err("File name cannot be empty.".to_string());
    }

    // Windows / cross-platform illegal filename characters: \ / : * ? " < > |
    let illegal = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
    if raw_input.chars().any(|c| illegal.contains(&c)) {
        return Err("Name contains invalid characters: \\ / : * ? \" < > |".to_string());
    }

    if files.is_empty() {
        return Ok(Vec::new());
    }

    if files.len() == 1 {
        let old_p = Path::new(&files[0]);
        let parent = old_p.parent().unwrap_or(Path::new(""));

        let new_filename = if raw_input.contains('.') {
            raw_input.to_string()
        } else if let Some(ext) = old_p.extension().and_then(|s| s.to_str()) {
            if ext.is_empty() {
                raw_input.to_string()
            } else {
                format!("{}.{}", raw_input, ext)
            }
        } else {
            raw_input.to_string()
        };

        let new_p = parent.join(&new_filename);
        let new_str = new_p.to_string_lossy().to_string();

        if new_str == files[0] {
            return Ok(Vec::new()); // No change
        }

        let is_case_change_on_windows = cfg!(windows)
            && new_str.eq_ignore_ascii_case(&files[0]);

        if new_p.exists() && !is_case_change_on_windows {
            return Err(format!("A file named '{}' already exists.", new_filename));
        }

        return Ok(vec![(files[0].clone(), new_str)]);
    }

    // Multiple files (OS batch rename style)
    let base_stem = if raw_input.contains('.') {
        Path::new(raw_input)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(raw_input)
    } else {
        raw_input
    };

    let mut plan = Vec::with_capacity(files.len());
    let mut planned_targets = std::collections::HashSet::new();

    let mut counter = 1;
    for file in files {
        let old_p = Path::new(file);
        let parent = old_p.parent().unwrap_or(Path::new(""));
        let ext = old_p.extension().and_then(|s| s.to_str()).unwrap_or("");

        loop {
            let candidate_name = if ext.is_empty() {
                format!("{} ({})", base_stem, counter)
            } else {
                format!("{} ({}).{}", base_stem, counter, ext)
            };

            let candidate_p = parent.join(&candidate_name);
            let candidate_str = candidate_p.to_string_lossy().to_string();

            let already_in_plan = planned_targets.contains(&candidate_str);
            let exists_outside = candidate_p.exists()
                && !files.iter().any(|f| f.eq_ignore_ascii_case(&candidate_str));

            counter += 1;
            if !already_in_plan && !exists_outside {
                planned_targets.insert(candidate_str.clone());
                plan.push((file.clone(), candidate_str));
                break;
            }
        }
    }

    Ok(plan)
}

/// Executes the rename plan using a collision-free two-pass rename.
pub fn execute_batch_rename(plan: &[(String, String)]) -> Result<(), String> {
    if plan.is_empty() {
        return Ok(());
    }

    // Pass 1: Rename to intermediate temporary paths in the same directory
    let mut temp_moves = Vec::with_capacity(plan.len());
    for (i, (old_path, target_path)) in plan.iter().enumerate() {
        if old_path == target_path {
            continue;
        }
        let temp_path = format!("{}.__mr_tmp_{}_{}__", old_path, std::process::id(), i);
        if let Err(e) = std::fs::rename(old_path, &temp_path) {
            // Attempt to rollback any already moved files
            for (rolled_old, rolled_tmp) in &temp_moves {
                let _ = std::fs::rename(rolled_tmp, rolled_old);
            }
            return Err(format!("Failed to rename '{}': {}", old_path, e));
        }
        temp_moves.push((old_path.clone(), temp_path));
    }

    // Pass 2: Rename from intermediate temporary paths to final destinations
    let mut completed_finals = Vec::new();
    for (i, (old_path, temp_path)) in temp_moves.iter().enumerate() {
        let target_path = &plan[i].1;
        if let Err(e) = std::fs::rename(temp_path, target_path) {
            // Rollback what we can
            let _ = std::fs::rename(temp_path, old_path);
            for (done_old, done_target) in completed_finals {
                let _ = std::fs::rename(done_target, done_old);
            }
            return Err(format!("Failed to finalize rename to '{}': {}", target_path, e));
        }
        completed_finals.push((old_path.clone(), target_path.clone()));
    }

    Ok(())
}

fn main() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "windows")]
    unsafe {
        // Enable Windows Per-Monitor V2 DPI Awareness for crisp antialiased font rendering on high-DPI displays
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
                // -4 corresponds to DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
                func(-4);
            } else {
                let set_dpi = GetProcAddress(user32, b"SetProcessDPIAware\0".as_ptr());
                if !set_dpi.is_null() {
                    let func: SetProcessDpiAwareFn = std::mem::transmute(set_dpi);
                    func();
                }
            }
        }
    }

    std::panic::set_hook(Box::new(|panic_info| {
        let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "An unexpected internal error occurred.".to_string()
        };

        let location = if let Some(loc) = panic_info.location() {
            format!("File: {}\nLine: {}, Column: {}", loc.file(), loc.line(), loc.column())
        } else {
            "Location: unknown".to_string()
        };

        let msg = format!(
            "Horama encountered an unhandled error and needs to close.\n\n\
             Error Details:\n{}\n\n\
             {}\n\n\
             If this error persists, please check your files or settings.",
            payload, location
        );

        show_fatal_error_dialog("Horama - Application Error", &msg);
    }));

    let ui = AppWindow::new()?;
    let state = Arc::new(Mutex::new(ViewerState::new()));

    let (initial_folder, initial_subfolder) = {
        let st = state.lock().unwrap();
        rebuild_profile_combobox_list(&ui, &st);
        populate_shortcuts_ui(&ui, &st.settings);
        refresh_recent_folders_ui(&ui, &st);
        let folder = if !st.settings.last_folder.is_empty() {
            let p = Path::new(&st.settings.last_folder);
            if p.is_dir() {
                Some(st.settings.last_folder.clone())
            } else {
                ui.set_folder_path(st.settings.last_folder.clone().into());
                None
            }
        } else {
            None
        };
        let sub = if !st.settings.last_subfolder.is_empty() {
            Some(st.settings.last_subfolder.clone())
        } else {
            None
        };
        (folder, sub)
    };

    // Restore persistent window size and maximized state
    {
        let (w, h, is_max) = {
            let st = state.lock().unwrap();
            (
                st.settings.window_width.max(1024),
                st.settings.window_height.max(680),
                st.settings.window_maximized,
            )
        };
        ui.window().set_size(slint::PhysicalSize::new(w, h));
        if is_max {
            ui.window().set_maximized(true);
        }
        let saved_aspects: Vec<slint::SharedString> = {
            let st = state.lock().unwrap();
            st.settings.saved_aspect_ratios.iter().map(|s| slint::SharedString::from(s.as_str())).collect()
        };
        ui.set_saved_custom_aspect_ratios(slint::ModelRc::new(slint::VecModel::from(saved_aspects)));
    }

    if let Some(folder) = initial_folder {
        open_folder_path(&ui, &state, &folder);
        if let Some(sub) = initial_subfolder {
            let mut st = state.lock().unwrap();
            let norm_sub = normalize_path(&sub);
            if sub == "__ROOT_ONLY__" || sub == "__ALL__" || st.subfolders.iter().any(|s| normalize_path(s).eq_ignore_ascii_case(&norm_sub)) {
                st.selected_tree_filter = sub.clone();
                st.settings.last_subfolder = sub.clone();
                st.settings.save();
                if sub != "__ALL__" && sub != "__ROOT_ONLY__" {
                    expand_ancestors_and_subfolder(&mut st, &sub);
                }
                apply_tree_filter_and_populate(&ui, &mut st);
            }
        }
    }

    // Deferred refinement once window layout pass is complete
    let _timer = {
        let ui_weak = ui.as_weak();
        let state_copy = state.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(60),
            move || {
                if let Some(ui) = ui_weak.upgrade() {
                    let mut st = state_copy.lock().unwrap();
                    if !st.lock_zoom {
                        if let Some(dec) = &st.current_decoded {
                            st.zoom_factor = calculate_fit_zoom(&ui, dec.width, dec.height);
                            st.pan_x = 0.0;
                            st.pan_y = 0.0;
                            update_zoom_ui(&ui, &st);
                        }
                    }
                }
            },
        );
        timer
    };

    // --- Open File(s) ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_open_files_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let start_dir = {
                let st = state.lock().unwrap();
                if !st.current_folder.is_empty() {
                    Some(PathBuf::from(&st.current_folder))
                } else if !st.settings.last_folder.is_empty() {
                    Some(PathBuf::from(&st.settings.last_folder))
                } else {
                    None
                }
            };
            let mut dlg = rfd::FileDialog::new()
                .set_title("Open Images")
                .add_filter(
                    "Supported Images & RAW",
                    &[
                        "jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "gif", "avif",
                        "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "pef", "raf", "exr", "hdr",
                    ],
                )
                .add_filter("All Files (*.*)", &["*"]);

            if let Some(dir) = start_dir {
                if dir.is_dir() {
                    dlg = dlg.set_directory(dir);
                }
            }

            if let Some(files) = dlg.pick_files() {
                let file_paths: Vec<String> = files
                    .into_iter()
                    .map(|p| normalize_path(&p.to_string_lossy()))
                    .filter(|p| crate::config::is_supported_file(Path::new(p)))
                    .collect();

                if !file_paths.is_empty() {
                    open_specific_files(&ui, &state, file_paths);
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
            let start_dir = {
                let st = state.lock().unwrap();
                if !st.current_folder.is_empty() {
                    Some(PathBuf::from(&st.current_folder))
                } else if !st.settings.last_folder.is_empty() {
                    Some(PathBuf::from(&st.settings.last_folder))
                } else {
                    None
                }
            };
            let mut dlg = rfd::FileDialog::new().set_title("Select Image Folder");
            if let Some(dir) = start_dir {
                if dir.is_dir() {
                    dlg = dlg.set_directory(dir);
                }
            }
            if let Some(folder) = dlg.pick_folder() {
                open_folder_path(&ui, &state, &folder.to_string_lossy());
            }
        });
    }
    // --- Refresh Current Folder Callback ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_refresh_current_folder(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            refresh_current_folder_content(&ui, &mut st);
        });
    }

    // --- Save / Export Image with ICC & Overwrite Confirmation ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_image_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            let Some(decoded) = st.current_decoded.clone() else {
                return;
            };

            let orig_path = Path::new(&decoded.path);
            let stem = orig_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "exported_image".to_string());

            let mut dlg = rfd::FileDialog::new()
                .set_title("Save / Export Image with Color Profile")
                .set_file_name(format!("{}.jpg", stem))
                .add_filter("JPEG Image (*.jpg, *.jpeg)", &["jpg", "jpeg"])
                .add_filter("TIFF 8-bit / 16-bit (*.tif, *.tiff)", &["tif", "tiff"])
                .add_filter("PNG Image (*.png)", &["png"])
                .add_filter("WebP Image (*.webp)", &["webp"])
                .add_filter("BMP Image (*.bmp)", &["bmp"])
                .add_filter("Digital Negative (*.dng)", &["dng"]);

            if let Some(parent) = orig_path.parent() {
                dlg = dlg.set_directory(parent);
            }

            if let Some(dest) = dlg.save_file() {
                // Overwrite confirmation warning if file exists
                if dest.exists() {
                    let confirm = rfd::MessageDialog::new()
                        .set_title("Confirm Overwrite with Color Profile")
                        .set_description(format!(
                            "The file '{}' already exists.\n\nDo you want to overwrite it with the active Color Profile ('{}') applied?",
                            dest.display(),
                            st.selected_profile
                        ))
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .show();
                    if confirm != rfd::MessageDialogResult::Yes {
                        return;
                    }
                }

                let pixels_to_save = st.color_manager.prepare_pixels_for_save(
                    &decoded.rgb_pixels,
                    &st.selected_profile,
                    decoded.embedded_icc.as_deref(),
                );
                let icc_bytes = st.color_manager.get_profile_bytes(
                    &st.selected_profile,
                    decoded.embedded_icc.as_deref(),
                );

                match save_image_with_icc(
                    &dest,
                    decoded.width,
                    decoded.height,
                    &pixels_to_save,
                    Some(&icc_bytes),
                    decoded.bit_depth >= 16,
                ) {
                    Ok(()) => {
                        st.cropped_files.remove(&decoded.path);
                        ui.set_current_image_is_cropped(false);
                        if let Some(cur_idx) = st.current_index {
                            let model = ui.get_filmstrip_items();
                            if let Some(vm) = model.as_any().downcast_ref::<VecModel<FilmstripItemData>>() {
                                if let Some(mut item) = vm.row_data(cur_idx) {
                                    item.is_cropped = false;
                                    vm.set_row_data(cur_idx, item);
                                }
                            }
                        }
                        ui.set_status_file_text(
                            format!(
                                "Saved '{}' with ICC profile '{}'",
                                dest.display(),
                                st.selected_profile
                            )
                            .into(),
                        );
                    }
                    Err(e) => {
                        rfd::MessageDialog::new()
                            .set_title("Save Error")
                            .set_description(format!("Failed to save image:\n{}", e))
                            .set_buttons(rfd::MessageButtons::Ok)
                            .show();
                    }
                }
            }
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

                let mut st = state.lock().unwrap();
                if let Some(icc_path) = picked {
                    if let Some(label) = st.color_manager.register_custom_profile(&icc_path) {
                        let p_str = icc_path.to_string_lossy().to_string();
                        if !st.settings.custom_profiles.contains(&p_str) {
                            st.settings.custom_profiles.push(p_str);
                            st.settings.save();
                        }
                        rebuild_profile_combobox_list(&ui, &st);
                        st.selected_profile = label.clone();
                        ui.set_current_profile(label.clone().into());
                        if let Some(curr) = st.current_decoded.as_ref().map(|d| d.path.clone()) {
                            st.image_profile_overrides.insert(curr, label);
                        }
                        render_current_image_and_panels(&ui, &mut st, true);
                        return;
                    }
                }
                // Restore previous profile in ComboBox if cancelled
                ui.set_current_profile(st.selected_profile.clone().into());
                return;
            }

            let mut st = state.lock().unwrap();
            let old_p = st.selected_profile.clone();
            if old_p != chosen && !st.is_undoing_redoing {
                st.push_undo(HistoryAction::ColorProfile { old_profile: old_p, new_profile: chosen.clone() });
            }
            st.selected_profile = chosen.clone();
            // Always persist per-image profile modification even when Lock Profile is unchecked
            if let Some(curr) = st.current_decoded.as_ref().map(|d| d.path.clone()) {
                st.image_profile_overrides.insert(curr, chosen);
            }
            // Preserve current zoom & pan when changing Color Profile
            render_current_image_and_panels(&ui, &mut st, true);
        });
    }

    // --- Lock Color Profile & Lock Zoom Toggles ---
    {
        let state = state.clone();
        ui.on_lock_profile_toggled(move |checked| {
            let mut st = state.lock().unwrap();
            st.lock_profile = checked;
        });
    }
    {
        let state = state.clone();
        ui.on_lock_zoom_toggled(move |checked| {
            let mut st = state.lock().unwrap();
            st.lock_zoom = checked;
        });
    }

    // --- Zoom Controls ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_in_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            st.zoom_factor = (st.zoom_factor * 1.25).clamp(0.05, 32.0);
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_out_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            st.zoom_factor = (st.zoom_factor / 1.25).clamp(0.05, 32.0);
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_zoom_100_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
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
            let mut st = state.lock().unwrap();
            st.is_fit_mode = true;
            if let Some(dec) = &st.current_decoded {
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
            let mut st = state.lock().unwrap();
            st.is_fit_mode = false;
            st.zoom_factor = factor.clamp(0.05, 32.0);
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_window_resized(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
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
            let mut st = state.lock().unwrap();
            if st.visible_files.is_empty() {
                return;
            }
            let curr = st.current_index.unwrap_or(0);
            let prev = if curr == 0 {
                st.visible_files.len() - 1
            } else {
                curr - 1
            };
            select_image_by_index(&ui, &mut st, prev);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_next_image_clicked(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            if st.visible_files.is_empty() {
                return;
            }
            let curr = st.current_index.unwrap_or(0);
            let next = (curr + 1) % st.visible_files.len();
            select_image_by_index(&ui, &mut st, next);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_filmstrip_item_clicked(move |idx, is_ctrl, is_shift| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if idx >= 0 {
                handle_filmstrip_click(&ui, &state, idx as usize, is_ctrl, is_shift);
            }
        });
    }

    // --- Filmstrip Context Menu Callbacks ---
    {
        let ui_weak = ui.as_weak();
        ui.on_request_open_with_apps(move |file_path| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let path_str = file_path.to_string();
            let handlers = crate::open_with::get_handlers_for_file(&path_str);
            let items: Vec<OpenWithAppItem> = handlers
                .into_iter()
                .map(|h| {
                    let (icon, has_icon) = if let Some(rgba) = h.icon_rgba {
                        (rgba_to_slint_image(h.icon_width, h.icon_height, &rgba), true)
                    } else {
                        (Image::default(), false)
                    };
                    OpenWithAppItem {
                        id: h.id,
                        name: h.name.into(),
                        icon,
                        has_icon,
                    }
                })
                .collect();
            ui.set_open_with_apps(ModelRc::new(VecModel::from(items)));
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_open_with_app_clicked(move |app_id, file_path| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let path_str = file_path.to_string();
            if let Err(e) = crate::open_with::launch_handler(&path_str, app_id) {
                ui.set_status_file_text(format!("Failed to open in app: {}", e).into());
            }
        });
    }
    {
        ui.on_open_with_dialog_clicked(move |file_path| {
            let path_str = file_path.to_string();
            crate::open_with::launch_open_with_dialog(&path_str);
        });
    }
    {
        ui.on_open_containing_folder_explorer(move |file_path| {
            let path_str = file_path.to_string();
            let p = Path::new(&path_str);
            if p.exists() {
                #[cfg(target_os = "windows")]
                {
                    use std::os::windows::process::CommandExt;
                    use std::process::Command;
                    extern "system" {
                        fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
                    }
                    unsafe {
                        AllowSetForegroundWindow(0xFFFFFFFF);
                    }

                    let win_path = path_str.replace('/', "\\");
                    let escaped = win_path.replace('\'', "''");
                    let script = format!(
                        "$p = '{}'; \
                         $folder = Split-Path $p; \
                         $folderName = (Get-Item $folder).Name; \
                         $proc = Start-Process explorer.exe -ArgumentList \"/select,`\"$p`\"\" -PassThru; \
                         Start-Sleep -Milliseconds 200; \
                         $sh = New-Object -ComObject WScript.Shell; \
                         $sh.AppActivate($folderName); \
                         $sh.AppActivate('File Explorer'); \
                         $sh.AppActivate('Esplora file');",
                        escaped
                    );
                    let _ = Command::new("powershell")
                        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
                        .creation_flags(0x08000000)
                        .spawn();
                }
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open")
                        .arg("-R")
                        .arg(&path_str)
                        .spawn();
                }
                #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
                {
                    if let Some(parent) = p.parent() {
                        let _ = std::process::Command::new("xdg-open")
                            .arg(parent)
                            .spawn();
                    }
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rate_selected_images(move |rating| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let r_u8 = (rating.clamp(0, 5)) as u8;
            let mut st = state.lock().unwrap();
            let sel_indices: Vec<usize> = if st.selected_indices.is_empty() {
                st.current_index.into_iter().collect()
            } else {
                st.selected_indices.iter().copied().collect()
            };

            for &idx in &sel_indices {
                if let Some(p) = st.visible_files.get(idx).cloned() {
                    let old_r = st.settings.get_rating(&p) as i32;
                    if old_r != (r_u8 as i32) && !st.is_undoing_redoing {
                        st.push_undo(HistoryAction::Rating { path: p.clone(), old_rating: old_r, new_rating: r_u8 as i32 });
                    }
                    st.settings.set_rating(&p, r_u8);
                }
            }
            let _ = st.settings.save();
            ui.set_current_rating(r_u8 as i32);

            let filmstrip_model = ui.get_filmstrip_items();
            if let Some(vec_model) =
                filmstrip_model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
            {
                for &idx in &sel_indices {
                    if let Some(mut item) = vec_model.row_data(idx) {
                        item.rating = r_u8 as i32;
                        vec_model.set_row_data(idx, item);
                    }
                }
            }

            if st.selected_indices.len() > 1 {
                refresh_multi_selection_overview(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rate_filmstrip_item(move |idx, rating| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            let idx_usize = idx as usize;
            let r_u8 = (rating.clamp(0, 5)) as u8;

            let target_indices: Vec<usize> = if st.selected_indices.contains(&idx_usize) && st.selected_indices.len() > 1 {
                st.selected_indices.iter().copied().collect()
            } else {
                vec![idx_usize]
            };

            for &ti in &target_indices {
                if let Some(p) = st.visible_files.get(ti).cloned() {
                    st.settings.set_rating(&p, r_u8);
                }
            }
            let _ = st.settings.save();
            ui.set_current_rating(r_u8 as i32);

            let filmstrip_model = ui.get_filmstrip_items();
            if let Some(vec_model) =
                filmstrip_model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
            {
                for &ti in &target_indices {
                    if let Some(mut item) = vec_model.row_data(ti) {
                        item.rating = r_u8 as i32;
                        vec_model.set_row_data(ti, item);
                    }
                }
            }

            if st.selected_indices.len() > 1 {
                refresh_multi_selection_overview(&ui, &st);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_delete_selected_images(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            let mut indices: Vec<usize> = st.selected_indices.iter().copied().collect();
            if indices.is_empty() {
                if let Some(cur) = st.current_index {
                    indices.push(cur);
                }
            }
            if indices.is_empty() {
                return;
            }

            let mut files_to_delete = Vec::new();
            for &idx in &indices {
                if let Some(p) = st.visible_files.get(idx) {
                    files_to_delete.push(p.clone());
                }
            }

            // Move files to Windows Recycle Bin via PowerShell Microsoft.VisualBasic FileSystem
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                use std::process::Command;
                for f in &files_to_delete {
                    let esc = f.replace('\'', "''");
                    let script = format!(
                        "Add-Type -AssemblyName Microsoft.VisualBasic; \
                         [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile('{}', 'OnlyErrorDialogs', 'SendToRecycleBin')",
                        esc
                    );
                    let _ = Command::new("powershell")
                        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
                        .creation_flags(0x08000000)
                        .output();
                }
            }
            #[cfg(target_os = "macos")]
            {
                for f in &files_to_delete {
                    let esc = f.replace('\\', "\\\\").replace('"', "\\\"");
                    let script = format!("tell application \"Finder\" to delete POSIX file \"{}\"", esc);
                    let res = std::process::Command::new("osascript")
                        .arg("-e")
                        .arg(&script)
                        .output();
                    if res.is_err() || !res.unwrap().status.success() {
                        let _ = std::fs::remove_file(f);
                    }
                }
            }
            #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
            {
                for f in &files_to_delete {
                    let _ = std::fs::remove_file(f);
                }
            }

            if let Ok(mut guard) = st.preloader.cache.lock() {
                for f in &files_to_delete {
                    guard.remove(f);
                }
            }

            let del_set: std::collections::HashSet<String> = files_to_delete.into_iter().collect();
            st.all_folder_files.retain(|f| !del_set.contains(f));
            st.visible_files.retain(|f| !del_set.contains(f));
            st.selected_indices.clear();

            if st.visible_files.is_empty() {
                st.current_index = None;
                st.current_decoded = None;
                st.rendered_rgb.clear();
                ui.set_has_image(false);
                ui.set_filmstrip_items(ModelRc::new(VecModel::default()));
                ui.set_is_multi_selection(false);
                ui.set_multi_selection_tiles(ModelRc::new(VecModel::default()));
            } else {
                let new_idx = indices[0].min(st.visible_files.len() - 1);
                apply_tree_filter_and_populate(&ui, &mut st);
                select_image_by_index(&ui, &mut st, new_idx);
            }
            update_cache_status_ui(&ui, &st);
        });
    }
    // --- Filmstrip Rename Callbacks ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_request_rename(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.lock().unwrap();
            let mut indices: Vec<usize> = st.selected_indices.iter().copied().collect();
            if indices.is_empty() {
                if let Some(cur) = st.current_index {
                    indices.push(cur);
                }
            }
            if indices.is_empty() {
                return;
            }

            let count = indices.len();
            if count == 1 {
                let path_str = &st.visible_files[indices[0]];
                let p = Path::new(path_str);
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{}", e)).unwrap_or_default();
                ui.set_rename_dialog_visible(true);
                ui.set_rename_file_count(1);
                ui.set_rename_input_text(stem.into());
                ui.set_rename_extension_text(ext.into());
                ui.set_rename_error_text("".into());
            } else {
                let target_path_str = st
                    .current_index
                    .and_then(|cur| if indices.contains(&cur) { st.visible_files.get(cur) } else { None })
                    .unwrap_or(&st.visible_files[indices[0]]);
                let p = Path::new(target_path_str);
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Image");
                ui.set_rename_dialog_visible(true);
                ui.set_rename_file_count(count as i32);
                ui.set_rename_input_text(stem.into());
                ui.set_rename_extension_text("Original extensions preserved".into());
                ui.set_rename_error_text("".into());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_confirm_rename(move |new_name| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            let mut indices: Vec<usize> = st.selected_indices.iter().copied().collect();
            if indices.is_empty() {
                if let Some(cur) = st.current_index {
                    indices.push(cur);
                }
            }
            if indices.is_empty() {
                ui.set_rename_dialog_visible(false);
                return;
            }

            let files_to_rename: Vec<String> = indices
                .iter()
                .filter_map(|&idx| st.visible_files.get(idx).cloned())
                .collect();

            if files_to_rename.is_empty() {
                ui.set_rename_dialog_visible(false);
                return;
            }

            let plan = match compute_batch_rename_plan(&files_to_rename, new_name.as_str()) {
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

            if let Err(e) = execute_batch_rename(&plan) {
                ui.set_rename_error_text(e.into());
                return;
            }

            // Apply path changes to state
            let mut plan_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            for (old_f, new_f) in &plan {
                plan_map.insert(old_f.clone(), new_f.clone());

                // Update ratings
                let rating = st.settings.get_rating(old_f);
                st.settings.image_ratings.remove(old_f);
                if rating > 0 {
                    st.settings.set_rating(new_f, rating);
                }

                // Transfer preloader RAM cache to new path
                if let Ok(mut guard) = st.preloader.cache.lock() {
                    if let Some(cached) = guard.get(old_f) {
                        guard.insert(new_f.clone(), cached);
                    }
                    guard.remove(old_f);
                }
            }
            let _ = st.settings.save();

            // Update all_folder_files & visible_files in place
            for f in &mut st.all_folder_files {
                if let Some(new_p) = plan_map.get(f) {
                    *f = new_p.clone();
                }
            }
            for f in &mut st.visible_files {
                if let Some(new_p) = plan_map.get(f) {
                    *f = new_p.clone();
                }
            }

            // Update Filmstrip models
            let filmstrip_model = ui.get_filmstrip_items();
            if let Some(vec_model) =
                filmstrip_model.as_any().downcast_ref::<VecModel<FilmstripItemData>>()
            {
                for &idx in &indices {
                    if let Some(new_p) = st.visible_files.get(idx) {
                        if let Some(mut item) = vec_model.row_data(idx) {
                            item.full_path = new_p.clone().into();
                            item.label = Path::new(new_p)
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| new_p.clone())
                                .into();
                            item.badge_type = "renamed".into();
                            item.badge_text = "✏️ Renamed".into();
                            vec_model.set_row_data(idx, item);
                        }
                    }
                }
            }

            // Update currently viewed image if it was renamed
            if let Some(cur) = st.current_index {
                if indices.contains(&cur) {
                    if let Some(new_p) = st.visible_files.get(cur).cloned() {
                        if let Some(dec) = &mut st.current_decoded {
                            let mut new_dec = (**dec).clone();
                            new_dec.path = new_p.clone();
                            st.current_decoded = Some(Arc::new(new_dec));
                        }
                        let fname = Path::new(&new_p)
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        ui.set_status_file_text(format!("{} - {}", fname, new_p).into());
                        ui.set_current_file_deleted(false);
                        ui.set_current_file_new(false);
                        ui.set_current_file_modified(false);
                        ui.set_current_file_renamed(true);
                        ui.set_current_file_badge_text("✏️ Renamed".into());
                    }
                }
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
            let target_path = full_path.to_string();
            let mut st = state.lock().unwrap();
            if let Some(target_idx) = st.visible_files.iter().position(|f| f == &target_path) {
                // Task 4: Become the only selected image, clear multi-selection overview, and enlarge to fit window
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

    // --- Folder Tree & Recent Folders Callbacks ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_tree_item_clicked(move |path_key| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            let key = path_key.to_string();
            st.selected_tree_filter = key.clone();
            // Task 9: persist subfolder selection
            st.settings.last_subfolder = key;
            st.settings.save();
            apply_tree_filter_and_populate(&ui, &mut st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_tree_item_toggle_expand(move |path_key| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            let key = path_key.to_string();
            let norm_root = normalize_path(&st.current_folder);
            let root_expanded = st.expanded_folders.contains("__ALL__")
                || st.expanded_folders.iter().any(|e| e.eq_ignore_ascii_case(&norm_root))
                || st.expanded_folders.is_empty();

            if key == "__ALL__" {
                if root_expanded {
                    st.expanded_folders.remove("__ALL__");
                    st.expanded_folders.remove(&norm_root);
                } else {
                    st.expanded_folders.insert("__ALL__".to_string());
                    st.expanded_folders.insert(norm_root);
                }
            } else {
                let norm_k = normalize_path(&key);
                let existing = st.expanded_folders.iter().find(|e| normalize_path(e).eq_ignore_ascii_case(&norm_k)).cloned();
                if let Some(ex) = existing {
                    st.expanded_folders.remove(&ex);
                } else {
                    st.expanded_folders.insert(norm_k);
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
            let mut st = state.lock().unwrap();
            undo_last_operation(&ui, &mut st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_redo_action(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            redo_last_operation(&ui, &mut st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_jump_forward_10(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            jump_images(&ui, &mut st, 10);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_jump_backward_10(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            jump_images(&ui, &mut st, -10);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_cycle_profile(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            cycle_color_profile(&ui, &mut st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rotate_cw(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            rotate_current_image(&ui, &mut st, true);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_rotate_ccw(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            rotate_current_image(&ui, &mut st, false);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_apply_crop(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            apply_crop(&ui, &mut st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_apply_custom_aspect_ratio(move |text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text = text.trim();
            let parse_ratio = |t: &str| -> Option<f32> {
                if let Some((w_s, h_s)) = t.split_once(':') {
                    let w: f32 = w_s.trim().parse().ok()?;
                    let h: f32 = h_s.trim().parse().ok()?;
                    if h > 0.0 { Some(w / h) } else { None }
                } else if let Some((w_s, h_s)) = t.split_once('/') {
                    let w: f32 = w_s.trim().parse().ok()?;
                    let h: f32 = h_s.trim().parse().ok()?;
                    if h > 0.0 { Some(w / h) } else { None }
                } else if let Some((w_s, h_s)) = t.split_once('x') {
                    let w: f32 = w_s.trim().parse().ok()?;
                    let h: f32 = h_s.trim().parse().ok()?;
                    if h > 0.0 { Some(w / h) } else { None }
                } else {
                    t.parse::<f32>().ok()
                }
            };

            if let Some(r) = parse_ratio(text) {
                if r > 0.01 && r < 100.0 {
                    ui.invoke_set_aspect_ratio(format!("Custom ({text})").into(), r);
                    ui.set_crop_custom_dialog_visible(false);
                }
            }
        });
    }
    // Task 4: Save Custom Aspect Ratio
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_save_custom_aspect_ratio(move |text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text_trim = text.trim().to_string();
            if parse_aspect_ratio_str(&text_trim).is_some() {
                let mut st = state.lock().unwrap();
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
    // Task 4: Delete Custom Aspect Ratio
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_delete_custom_aspect_ratio(move |text| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let text_trim = text.trim();
            let mut st = state.lock().unwrap();
            st.settings.saved_aspect_ratios.retain(|s| !s.eq_ignore_ascii_case(text_trim));
            st.settings.save();
            let model: Vec<SharedString> = st.settings.saved_aspect_ratios.iter().map(|s| SharedString::from(s.as_str())).collect();
            ui.set_saved_custom_aspect_ratios(ModelRc::new(VecModel::from(model)));
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_arc = state.clone();
        ui.on_request_exit(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state_arc.lock().unwrap();
            let has_unsaved = !st.cropped_files.is_empty();
            if has_unsaved {
                let count = st.cropped_files.len();
                ui.set_exit_confirm_text(format!("There are {} image(s) with unsaved modifications (crop).\nDo you want to save before exiting?", count).into());
                ui.set_exit_confirm_dialog_visible(true);
            } else {
                drop(st);
                ui.invoke_exit_app();
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_arc = state.clone();
        ui.on_exit_app(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_arc.lock().unwrap();
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
                st.settings.save();
            }
            std::process::exit(0);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_arc = state.clone();
        ui.on_save_all_modified_and_exit(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state_arc.lock().unwrap();

            // Collect all paths that have unsaved modifications (cropped)
            let modified_paths: std::collections::HashSet<String> = st.cropped_files.iter().cloned().collect();

            for original_str in &modified_paths {
                let orig_path = Path::new(original_str);
                // Look for modified DecodedImage in current_decoded or preloader cache
                let maybe_decoded = if let Some(ref cur) = st.current_decoded {
                    if &cur.path == original_str {
                        Some((**cur).clone())
                    } else {
                        None
                    }
                } else {
                    None
                };

                let decoded_opt = maybe_decoded.or_else(|| {
                    if let Ok(guard) = st.preloader.cache.lock() {
                        guard.get(original_str).map(|arc| (*arc).clone())
                    } else {
                        None
                    }
                });

                if let Some(decoded) = decoded_opt {
                    let dest = generate_next_sequential_path(orig_path);
                    let pixels_to_save = st.color_manager.prepare_pixels_for_save(
                        &decoded.rgb_pixels,
                        &st.selected_profile,
                        decoded.embedded_icc.as_deref(),
                    );
                    let icc_bytes = st.color_manager.get_profile_bytes(
                        &st.selected_profile,
                        decoded.embedded_icc.as_deref(),
                    );
                    let _ = save_image_with_icc(
                        &dest,
                        decoded.width,
                        decoded.height,
                        &pixels_to_save,
                        Some(&icc_bytes),
                        decoded.bit_depth >= 16,
                    );
                }
            }

            st.cropped_files.clear();
            drop(st);

            ui.invoke_exit_app();
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_recent_folder_clicked(move |folder| {
            let Some(ui) = ui_weak.upgrade() else { return };
            open_folder_path(&ui, &state, &folder);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_recent_folder_remove(move |folder| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            st.settings.remove_recent_folder(&folder);
            refresh_recent_folders_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_recent_folders_clear(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            st.settings.clear_recent_folders();
            refresh_recent_folders_ui(&ui, &st);
        });
    }
    // Task 17: Double click zoom (100% centered on cursor / toggle fit)
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_double_clicked(move |mx, my, vw, vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            if st.current_decoded.is_none() {
                return;
            }
            if (st.zoom_factor - 1.0).abs() < 0.05 && !st.is_fit_mode {
                if let Some(ref decoded) = st.current_decoded {
                    st.zoom_factor = calculate_fit_zoom(&ui, decoded.width, decoded.height);
                    st.pan_x = 0.0;
                    st.pan_y = 0.0;
                    st.is_fit_mode = true;
                    update_zoom_ui(&ui, &st);
                }
            } else {
                let old_zoom = st.zoom_factor;
                let new_zoom = 1.0f32;
                let scale_ratio = new_zoom / old_zoom;
                let cx = mx - vw * 0.5;
                let cy = my - vh * 0.5;
                st.pan_x = cx - (cx - st.pan_x) * scale_ratio;
                st.pan_y = cy - (cy - st.pan_y) * scale_ratio;
                st.zoom_factor = new_zoom;
                st.is_fit_mode = false;
                update_zoom_ui(&ui, &st);
            }
        });
    }
    // Tasks 2, 3, 4: Advanced Multi-Criteria Filter callbacks
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_filter_changed_advanced(move |search_text, ratings_mask, aspect_filter, profile_filter, f_cropped, f_prof_mod| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            st.filter_search_text = search_text.to_string();
            st.filter_ratings_mask = ratings_mask as u8;
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
            let mut st = state.lock().unwrap();
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
    // Keyboard Shortcuts Customization Callbacks
    {
        let state = state.clone();
        ui.on_get_action_for_shortcut(move |pressed| {
            let st = state.lock().unwrap();
            let mut pressed_trim = pressed.trim().to_string();

            // Normalize macOS Cmd/Meta modifier to Ctrl
            if pressed_trim.to_lowercase().starts_with("cmd+") {
                pressed_trim = format!("ctrl+{}", &pressed_trim[4..]);
            } else if pressed_trim.to_lowercase().starts_with("meta+") {
                pressed_trim = format!("ctrl+{}", &pressed_trim[5..]);
            }

            // Handle Windows ASCII control codes when Ctrl was pressed (Ctrl+A is \u{1}, Ctrl+Z is \u{26})
            if pressed_trim.starts_with("Ctrl+") && pressed_trim.len() > 5 {
                let ch = pressed_trim.chars().nth(5).unwrap_or('\0');
                if (ch as u32) >= 1 && (ch as u32) <= 26 {
                    let actual_char = ((ch as u8) - 1 + b'a') as char;
                    pressed_trim = format!("ctrl+{}", actual_char);
                }
            } else if pressed_trim.to_lowercase().starts_with("ctrl+shift+") && pressed_trim.len() > 11 {
                let ch = pressed_trim.chars().nth(11).unwrap_or('\0');
                if (ch as u32) >= 1 && (ch as u32) <= 26 {
                    let actual_char = ((ch as u8) - 1 + b'a') as char;
                    pressed_trim = format!("ctrl+shift+{}", actual_char);
                }
            }
            let p_lower = pressed_trim.to_lowercase();

            for (id, combo) in &st.settings.shortcuts {
                let c_lower = combo.trim().to_lowercase();
                if c_lower == p_lower
                    || (c_lower.len() == 1 && p_lower == format!("shift+{}", c_lower))
                    || (c_lower == "+" && (p_lower == "=" || p_lower == "+" || p_lower == "shift+=" || p_lower == "shift++"))
                    || (c_lower == "-" && (p_lower == "_" || p_lower == "-" || p_lower == "shift+_" || p_lower == "shift+-"))
                    || (c_lower == "ctrl+0" && (p_lower == "ctrl+0" || p_lower == "ctrl+ " || p_lower == "space"))
                {
                    return id.clone().into();
                }
            }
            "".into()
        });
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_update_shortcut_key(move |idx, new_key| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let model = ui.get_shortcut_items();
            if let Some(mut item) = model.row_data(idx as usize) {
                let (valid, _) = validate_shortcut_combo(&new_key);
                item.key_combo = new_key.into();
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
            let mut st = state.lock().unwrap();
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
            let mut st = state.lock().unwrap();
            let model = ui.get_shortcut_items();
            for i in 0..model.row_count() {
                if let Some(item) = model.row_data(i) {
                    let id = item.id.to_string();
                    let key = item.key_combo.to_string();
                    let (valid, canonical) = validate_shortcut_combo(&key);
                    if valid {
                        if canonical.is_empty() {
                            st.settings.shortcuts.remove(&id);
                        } else {
                            st.settings.shortcuts.insert(id, canonical);
                        }
                    }
                }
            }
            st.settings.save();
            populate_shortcuts_ui(&ui, &st.settings);
        });
    }

    // --- Canvas Wheel Zoom, Pan, Right-Click Eyedropper, and Middle-Click 4x Dynamic Loupe ---
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_wheel(move |delta_y, mx, my, vw, vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
            let old_zoom = st.zoom_factor;
            let factor = if delta_y > 0.0 { 1.18 } else { 1.0 / 1.18 };
            let new_zoom = (old_zoom * factor).clamp(0.05, 32.0);
            let scale_ratio = new_zoom / old_zoom;

            // Zoom anchored around cursor position
            let cx = mx - vw * 0.5;
            let cy = my - vh * 0.5;
            st.pan_x = cx - (cx - st.pan_x) * scale_ratio;
            st.pan_y = cy - (cy - st.pan_y) * scale_ratio;
            st.zoom_factor = new_zoom;
            update_zoom_ui(&ui, &st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_pan(move |dx, dy| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut st = state.lock().unwrap();
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
            let st = state.lock().unwrap();
            let Some(decoded) = &st.current_decoded else {
                return;
            };

            let hud_x = (mx + 22.0).clamp(8.0, (vw - 296.0).max(8.0));
            let hud_y = (my + 22.0).clamp(8.0, (vh - 126.0).max(8.0));
            ui.set_eyedropper_x(slint::LogicalPosition::new(hud_x, hud_y).x);
            ui.set_eyedropper_y(slint::LogicalPosition::new(hud_x, hud_y).y);

            if let Some((ix, iy)) = viewport_to_image_coords(mx, my, vw, vh, &st) {
                let idx = ((iy * decoded.width + ix) * 3) as usize;
                if idx + 2 < st.rendered_rgb.len() {
                    let r = st.rendered_rgb[idx];
                    let g = st.rendered_rgb[idx + 1];
                    let b = st.rendered_rgb[idx + 2];
                    let (h, s, l) = rgb_to_hsl(r, g, b);

                    ui.set_eyedropper_color(Color::from_rgb_u8(r, g, b));
                    ui.set_eyedropper_xy_text(format!("XY: ({}, {})", ix, iy).into());
                    ui.set_eyedropper_rgb_text(format!("RGB: {:3}, {:3}, {:3}", r, g, b).into());
                    ui.set_eyedropper_hsl_text(format!("HSL: {:3}°, {:3}%, {:3}%", h, s, l).into());
                    ui.set_status_eyedropper_text(
                        format!(
                            "XY:({},{}) RGB:({},{},{}) HSL:({}°,{:}%,{:}%)",
                            ix, iy, r, g, b, h, s, l
                        )
                        .into(),
                    );
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state = state.clone();
        ui.on_canvas_inspect_loupe(move |mx, my, vw, vh| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let st = state.lock().unwrap();
            let Some(decoded) = &st.current_decoded else {
                return;
            };

            // Center the 332x356 loupe overlay directly over the cursor
            let lx = (mx - 166.0).clamp(4.0, (vw - 336.0).max(4.0));
            let ly = (my - 166.0).clamp(4.0, (vh - 360.0).max(4.0));
            ui.set_loupe_x(slint::LogicalPosition::new(lx, ly).x);
            ui.set_loupe_y(slint::LogicalPosition::new(lx, ly).y);

            let img_w = decoded.width as f32 * st.zoom_factor;
            let img_h = decoded.height as f32 * st.zoom_factor;
            let img_x0 = (vw - img_w) * 0.5 + st.pan_x;
            let img_y0 = (vh - img_h) * 0.5 + st.pan_y;

            let center_x = (mx - img_x0) / st.zoom_factor;
            let center_y = (my - img_y0) / st.zoom_factor;

            // Dynamic 4x magnification relative to the current zoom level
            let effective_mag = (st.zoom_factor * 4.0).clamp(0.20, 128.0);
            let (pw, ph, patch_rgba) = render_loupe_patch_rgba(
                center_x,
                center_y,
                effective_mag,
                decoded.width,
                decoded.height,
                &st.rendered_rgb,
            );

            ui.set_loupe_image(rgba_to_slint_image(pw, ph, &patch_rgba));
            let ix = center_x.floor().clamp(0.0, decoded.width.saturating_sub(1) as f32) as u32;
            let iy = center_y.floor().clamp(0.0, decoded.height.saturating_sub(1) as f32) as u32;
            ui.set_loupe_badge_text(
                format!("4× ({:.0}%) [{}, {}]", effective_mag * 100.0, ix, iy).into(),
            );
        });
    }

    // Task 19: MCP Server Integration
    let args: Vec<String> = std::env::args().collect();
    let is_mcp_enabled = args.iter().any(|a| a == "--mcp" || a == "-mcp") || std::env::var("HORAMA_MCP").is_ok();
    let _mcp_timer = if is_mcp_enabled {
        let (mcp_tx, mcp_rx) = crossbeam_channel::unbounded::<McpCommand>();
        std::thread::spawn(move || {
            run_mcp_stdio_server(mcp_tx);
        });

        let timer = slint::Timer::default();
        let ui_weak = ui.as_weak();
        let state_arc = state.clone();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(20), move || {
            while let Ok(cmd) = mcp_rx.try_recv() {
                let Some(ui) = ui_weak.upgrade() else { return };
                match cmd {
                    McpCommand::OpenFolder { path, resp } => {
                        open_folder_path(&ui, &state_arc, &path);
                        let st = state_arc.lock().unwrap();
                        let count = st.visible_files.len();
                        let _ = resp.send(Ok(format!("Opened folder '{}' with {} visible images", path, count)));
                    }
                    McpCommand::SelectImage { filename, resp } => {
                        let mut st = state_arc.lock().unwrap();
                        let norm_name = filename.to_lowercase();
                        let found = st.visible_files.iter().position(|f| {
                            f.to_lowercase().ends_with(&norm_name)
                                || Path::new(f).file_name().map(|n| n.to_string_lossy().to_lowercase() == norm_name).unwrap_or(false)
                        });
                        if let Some(idx) = found {
                            st.selected_indices.clear();
                            st.selected_indices.insert(idx);
                            st.selection_anchor = Some(idx);
                            st.is_fit_mode = true;
                            ui.set_is_multi_selection(false);
                            select_image_by_index(&ui, &mut st, idx);
                            sync_filmstrip_selection_ui(&ui, &st);
                            let target_f = st.visible_files[idx].clone();
                            let _ = resp.send(Ok(format!("Selected image: {}", target_f)));
                        } else {
                            let _ = resp.send(Err(format!("Image '{}' not found in current folder", filename)));
                        }
                    }
                    McpCommand::RotateImage { direction, resp } => {
                        let mut st = state_arc.lock().unwrap();
                        if direction.eq_ignore_ascii_case("cw") || direction == "90" {
                            rotate_current_image(&ui, &mut st, true);
                            let _ = resp.send(Ok("Image rotated 90 degrees clockwise and saved".to_string()));
                        } else if direction.eq_ignore_ascii_case("ccw") || direction == "-90" || direction == "270" {
                            rotate_current_image(&ui, &mut st, false);
                            let _ = resp.send(Ok("Image rotated 90 degrees counter-clockwise and saved".to_string()));
                        } else if direction == "180" {
                            rotate_current_image(&ui, &mut st, true);
                            rotate_current_image(&ui, &mut st, true);
                            let _ = resp.send(Ok("Image rotated 180 degrees and saved".to_string()));
                        } else {
                            let _ = resp.send(Err(format!("Invalid rotation direction: '{}'", direction)));
                        }
                    }
                    McpCommand::SetZoom { mode, factor, resp } => {
                        let mut st = state_arc.lock().unwrap();
                        if mode == "fit" {
                            if let Some(ref decoded) = st.current_decoded {
                                st.zoom_factor = calculate_fit_zoom(&ui, decoded.width, decoded.height);
                                st.pan_x = 0.0;
                                st.pan_y = 0.0;
                                st.is_fit_mode = true;
                                update_zoom_ui(&ui, &st);
                                let _ = resp.send(Ok("Zoom set to fit window".to_string()));
                            } else {
                                let _ = resp.send(Err("No image loaded".to_string()));
                            }
                        } else if mode == "100" {
                            st.zoom_factor = 1.0;
                            st.pan_x = 0.0;
                            st.pan_y = 0.0;
                            st.is_fit_mode = false;
                            update_zoom_ui(&ui, &st);
                            let _ = resp.send(Ok("Zoom set to 100%".to_string()));
                        } else if mode == "custom" {
                            if let Some(f) = factor {
                                st.zoom_factor = f.clamp(0.05, 32.0);
                                st.is_fit_mode = false;
                                update_zoom_ui(&ui, &st);
                                let _ = resp.send(Ok(format!("Zoom set to {:.0}%", st.zoom_factor * 100.0)));
                            } else {
                                let _ = resp.send(Err("Missing 'factor' parameter for custom zoom".to_string()));
                            }
                        } else {
                            let _ = resp.send(Err(format!("Unknown zoom mode: '{}'", mode)));
                        }
                    }
                    McpCommand::SetRating { rating, resp } => {
                        let mut st = state_arc.lock().unwrap();
                        let current_path = st.current_index.and_then(|idx| st.visible_files.get(idx)).cloned();
                        if let Some(path) = current_path {
                            st.settings.set_rating(&path, rating as u8);
                            st.settings.save();
                            ui.set_current_rating(rating);
                            sync_filmstrip_selection_ui(&ui, &st);
                            let _ = resp.send(Ok(format!("Rating set to {} stars for {}", rating, path)));
                        } else {
                            let _ = resp.send(Err("No active image to rate".to_string()));
                        }
                    }
                    McpCommand::FilterImages { query, min_rating, format, resp } => {
                        let mut st = state_arc.lock().unwrap();
                        if let Some(q) = query {
                            st.filter_search_text = q.clone();
                            ui.set_filter_search_text(q.into());
                        }
                        if let Some(r) = min_rating {
                            st.filter_rating = r;
                            if r == 0 {
                                ui.invoke_clear_ratings();
                                st.filter_ratings_mask = 0;
                            } else if (1..=5).contains(&r) {
                                ui.set_filter_r0(false);
                                ui.set_filter_r1(r == 1);
                                ui.set_filter_r2(r == 2);
                                ui.set_filter_r3(r == 3);
                                ui.set_filter_r4(r == 4);
                                ui.set_filter_r5(r == 5);
                                st.filter_ratings_mask = 1 << r;
                            }
                        }
                        if let Some(fmt) = format {
                            st.filter_format = fmt.clone();
                                                    }
                        apply_tree_filter_and_populate(&ui, &mut st);
                        let count = st.visible_files.len();
                        let _ = resp.send(Ok(format!("Filters applied: {} images matching criteria", count)));
                    }
                    McpCommand::GetCurrentImageInfo { resp } => {
                        let st = state_arc.lock().unwrap();
                        let current_path = st.current_index.and_then(|idx| st.visible_files.get(idx)).cloned();
                        if let Some(ref path) = current_path {
                            let (w, h) = st.current_decoded.as_ref().map(|d| (d.width, d.height)).unwrap_or((0, 0));
                            let rating = st.settings.get_rating(path);
                            let mut meta_map = serde_json::Map::new();
                            if let Some(ref dec) = st.current_decoded {
                                let meta_pairs = crate::metadata::extract_metadata(dec);
                                for (k, v) in meta_pairs {
                                    meta_map.insert(k, Value::String(v));
                                }
                            }
                            let info = json!({
                                "path": path,
                                "filename": Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                                "width": w,
                                "height": h,
                                "rating": rating,
                                "profile": st.selected_profile,
                                "metadata": meta_map,
                            });
                            let _ = resp.send(Ok(info));
                        } else {
                            let _ = resp.send(Err("No image currently loaded".to_string()));
                        }
                    }
                    McpCommand::ListImages { resp } => {
                        let st = state_arc.lock().unwrap();
                        let _ = resp.send(Ok(st.visible_files.clone()));
                    }
                }
            }
        });
        Some(timer)
    } else {
        None
    };

    // Periodic window size tracking timer
    let _win_timer = {
        let timer = slint::Timer::default();
        let ui_weak = ui.as_weak();
        let state_arc = state.clone();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(2), move || {
            if let Some(ui) = ui_weak.upgrade() {
                let is_max = ui.window().is_maximized();
                let sz = ui.window().size();
                if let Ok(mut st) = state_arc.lock() {
                    let mut changed = false;
                    if st.settings.window_maximized != is_max {
                        st.settings.window_maximized = is_max;
                        changed = true;
                    }
                    if !is_max && sz.width >= 1024 && sz.height >= 680 {
                        if st.settings.window_width != sz.width || st.settings.window_height != sz.height {
                            st.settings.window_width = sz.width;
                            st.settings.window_height = sz.height;
                            changed = true;
                        }
                    }
                    if changed {
                        st.settings.last_subfolder = st.selected_tree_filter.clone();
                        st.settings.save();
                    }
                }
            }
        });
        timer
    };

    {
        let ui_weak = ui.as_weak();
        let state_close = state.clone();
        ui.window().on_close_requested(move || -> slint::CloseRequestResponse {
            let st = state_close.lock().unwrap();
            let has_unsaved = !st.cropped_files.is_empty();
            if has_unsaved {
                let count = st.cropped_files.len();
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_exit_confirm_text(
                        format!(
                            "There are {} image(s) with unsaved modifications (crop).\nDo you want to save before exiting?",
                            count
                        )
                        .into(),
                    );
                    ui.set_exit_confirm_dialog_visible(true);
                }
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                slint::CloseRequestResponse::HideWindow
            }
        });
    }

    let res = ui.run();
    if let Ok(mut st) = state.lock() {
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
        st.settings.save();
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

    #[test]
    fn test_compute_batch_rename_plan_single_file() {
        let files = vec![normalize_path("C:/photos/my_image.jpg")];
        
        // Single file without extension in input retains original extension
        let plan = compute_batch_rename_plan(&files, "vacation").unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].1, normalize_path("C:/photos/vacation.jpg"));

        // Single file with extension replaces extension
        let plan2 = compute_batch_rename_plan(&files, "vacation.png").unwrap();
        assert_eq!(plan2.len(), 1);
        assert_eq!(plan2[0].1, normalize_path("C:/photos/vacation.png"));

        // Invalid characters fail with error
        assert!(compute_batch_rename_plan(&files, "bad*name").is_err());
        assert!(compute_batch_rename_plan(&files, "   ").is_err());
    }

    #[test]
    fn test_compute_batch_rename_plan_multiple_files_os_style() {
        let files = vec![
            normalize_path("C:/photos/img01.jpg"),
            normalize_path("C:/photos/img02.NEF"),
            normalize_path("C:/photos/img03.png"),
        ];

        let plan = compute_batch_rename_plan(&files, "Holiday").unwrap();
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].1, normalize_path("C:/photos/Holiday (1).jpg"));
        assert_eq!(plan[1].1, normalize_path("C:/photos/Holiday (2).NEF"));
        assert_eq!(plan[2].1, normalize_path("C:/photos/Holiday (3).png"));

        // Even if input has an extension, multi-file strips it and preserves original extensions
        let plan2 = compute_batch_rename_plan(&files, "Holiday.jpg").unwrap();
        assert_eq!(plan2.len(), 3);
        assert_eq!(plan2[0].1, normalize_path("C:/photos/Holiday (1).jpg"));
        assert_eq!(plan2[1].1, normalize_path("C:/photos/Holiday (2).NEF"));
        assert_eq!(plan2[2].1, normalize_path("C:/photos/Holiday (3).png"));
    }

    
    use crate::color_manager::extract_icc_description;
    use crate::config::{PROFILE_ADOBE_RGB, PROFILE_PROPHOTO, PROFILE_SRGB};

    #[test]
    fn test_icc_generation_and_description() {
        let cm = ColorManager::new();
        let srgb_bytes = cm.get_profile_bytes(PROFILE_SRGB, None);
        assert!(srgb_bytes.len() >= 128);
        assert_eq!(
            extract_icc_description(&srgb_bytes).as_deref(),
            Some(PROFILE_SRGB)
        );

        let adobe_bytes = cm.get_profile_bytes(PROFILE_ADOBE_RGB, None);
        assert_eq!(
            extract_icc_description(&adobe_bytes).as_deref(),
            Some(PROFILE_ADOBE_RGB)
        );

        let test_px = vec![200u8, 100, 50, 50, 150, 220, 255, 255, 255, 128, 128, 128];
        let (out_srgb, info_srgb) = cm.apply_profile_with_info(&test_px, PROFILE_SRGB, None);
        println!("Rust sRGB -> Display ({}): {:?}", info_srgb.display_name, out_srgb);
        let (out_adobe, _) = cm.apply_profile_with_info(&test_px, PROFILE_ADOBE_RGB, None);
        println!("Rust Adobe -> Display: {:?}", out_adobe);
    }

    #[test]
    fn test_automatic_assign_and_convert_pipeline() {
        let cm = ColorManager::new();
        let sample_rgb = vec![200u8, 80, 40, 50, 180, 220];

        // 1. Untagged image -> Assigns default/selected profile automatically
        let (out_untagged, info_untagged) =
            cm.apply_profile_with_info(&sample_rgb, PROFILE_ADOBE_RGB, None);
        assert_eq!(out_untagged.len(), sample_rgb.len());
        assert!(!info_untagged.has_embedded);
        assert_eq!(info_untagged.source_name, PROFILE_ADOBE_RGB);

        // 2. Tagged image -> Converts from Embedded -> Selected -> Display
        let emb_bytes = cm.get_profile_bytes(PROFILE_ADOBE_RGB, None);
        let (out_conv, info_conv) =
            cm.apply_profile_with_info(&sample_rgb, PROFILE_PROPHOTO, Some(&emb_bytes));
        assert_eq!(out_conv.len(), sample_rgb.len());
        assert!(info_conv.has_embedded);
        assert!(info_conv.source_name.contains("→"));
    }

    #[test]
    fn test_rgb_to_hsl_and_loupe_patch() {
        assert_eq!(rgb_to_hsl(255, 0, 0), (0, 100, 50));
        assert_eq!(rgb_to_hsl(0, 255, 0), (120, 100, 50));
        assert_eq!(rgb_to_hsl(0, 0, 255), (240, 100, 50));

        let rgb = vec![128u8; 16 * 16 * 3];
        let (pw, ph, rgba) = render_loupe_patch_rgba(8.0, 8.0, 4.0, 16, 16, &rgb);
        assert_eq!(pw, 320);
        assert_eq!(ph, 320);
        assert_eq!(rgba.len(), 320 * 320 * 4);
    }

    #[test]
    fn test_extract_exif_thumbnail() {
        let test_path = PathBuf::from("Parigi_2025-12-22 11.09.01_MG_9855.jpg");
        let parent_test_path = PathBuf::from("../Parigi_2025-12-22 11.09.01_MG_9855.jpg");
        let target = if test_path.exists() {
            Some(test_path)
        } else if parent_test_path.exists() {
            Some(parent_test_path)
        } else {
            None
        };
        if let Some(path) = target {
            let start = std::time::Instant::now();
            let thumb = crate::image_loader::extract_exif_thumbnail(&path);
            let elapsed = start.elapsed();
            println!("EXIF thumbnail extraction took: {:?}", elapsed);
            assert!(thumb.is_some(), "Should find EXIF thumbnail in test photo");
            let bytes = thumb.unwrap();
            assert!(bytes.starts_with(&[0xFF, 0xD8]));
            println!("EXIF thumbnail byte size: {} bytes", bytes.len());
        }
    }

    #[test]
    fn test_file_sync_badge_actions() {
        let mut item = FilmstripItemData {
            label: "test.jpg".into(),
            rel_path: "test.jpg".into(),
            full_path: "C:/test/test.jpg".into(),
            thumbnail: Image::default(),
            has_thumbnail: false,
            selected: true,
            badge_type: "".into(),
            badge_text: "".into(),
            rating: 0,
            is_cropped: false,
        };

        // 1. Simulating Removed Action
        item.badge_type = "deleted".into();
        item.badge_text = "🗑 Deleted".into();
        assert_eq!(item.badge_type.as_str(), "deleted");
        assert_eq!(item.badge_text.as_str(), "🗑 Deleted");

        // 2. Simulating Restored Action
        item.badge_type = "".into();
        item.badge_text = "".into();
        assert_eq!(item.badge_type.as_str(), "");
        assert_eq!(item.badge_text.as_str(), "");

        // 3. Simulating Renamed Action
        item.full_path = "C:/test/renamed.jpg".into();
        item.label = "renamed.jpg".into();
        item.badge_type = "renamed".into();
        item.badge_text = "✏️ Renamed".into();
        assert_eq!(item.badge_type.as_str(), "renamed");
        assert_eq!(item.badge_text.as_str(), "✏️ Renamed");
        assert_eq!(item.label.as_str(), "renamed.jpg");

        // 4. Simulating New Action
        let new_item = FilmstripItemData {
            label: "new_photo.jpg".into(),
            rel_path: "new_photo.jpg".into(),
            full_path: "C:/test/new_photo.jpg".into(),
            thumbnail: Image::default(),
            has_thumbnail: false,
            selected: false,
            badge_type: "new".into(),
            badge_text: "✨ New".into(),
            rating: 0,
            is_cropped: false,
        };
        assert_eq!(new_item.badge_type.as_str(), "new");
        assert_eq!(new_item.badge_text.as_str(), "✨ New");

        // 5. Simulating Modified Action
        item.badge_type = "modified".into();
        item.badge_text = "🔄 Modified".into();
        assert_eq!(item.badge_type.as_str(), "modified");
        assert_eq!(item.badge_text.as_str(), "🔄 Modified");
    }

    #[test]
    fn test_validate_shortcut_combo() {
        // Valid single keys & case normalization
        assert_eq!(validate_shortcut_combo("c"), (true, "C".to_string()));
        assert_eq!(validate_shortcut_combo("C"), (true, "C".to_string()));
        assert_eq!(validate_shortcut_combo("p"), (true, "P".to_string()));
        assert_eq!(validate_shortcut_combo("s"), (true, "S".to_string()));
        assert_eq!(validate_shortcut_combo("up"), (true, "Up".to_string()));
        assert_eq!(validate_shortcut_combo("down"), (true, "Down".to_string()));
        assert_eq!(validate_shortcut_combo("F2"), (true, "F2".to_string()));
        assert_eq!(validate_shortcut_combo("delete"), (true, "Delete".to_string()));

        // Valid combos and '+' special case
        assert_eq!(validate_shortcut_combo("+"), (true, "+".to_string()));
        assert_eq!(validate_shortcut_combo("="), (true, "+".to_string()));
        assert_eq!(validate_shortcut_combo("ctrl++"), (true, "Ctrl++".to_string()));
        assert_eq!(validate_shortcut_combo("cmd+z"), (true, "Ctrl+Z".to_string()));
        assert_eq!(validate_shortcut_combo("ctrl+z"), (true, "Ctrl+Z".to_string()));
        assert_eq!(validate_shortcut_combo("Ctrl+Y"), (true, "Ctrl+Y".to_string()));
        assert_eq!(validate_shortcut_combo("ctrl+shift+o"), (true, "Ctrl+Shift+O".to_string()));

        // Empty combo (clearing a shortcut)
        assert_eq!(validate_shortcut_combo(""), (true, "".to_string()));
        assert_eq!(validate_shortcut_combo("   "), (true, "".to_string()));

        // Invalid combos: random character sequences like "asdfgh"
        assert_eq!(validate_shortcut_combo("asdfgh").0, false);
        assert_eq!(validate_shortcut_combo("foobar").0, false);
        assert_eq!(validate_shortcut_combo("xyz123").0, false);
        assert_eq!(validate_shortcut_combo("ctrl+").0, false);
        assert_eq!(validate_shortcut_combo("ctrl+a+b").0, false);
    }

    #[test]
    fn test_crop_tracking_and_redo_stack() {
        let mut state = ViewerState::new();
        let path = "C:/photos/test.jpg".to_string();

        assert!(!state.cropped_files.contains(&path));
        state.cropped_files.insert(path.clone());
        assert!(state.cropped_files.contains(&path));

        // Test filmstrip item reflects is_cropped
        let item = FilmstripItemData {
            label: "test.jpg".into(),
            rel_path: "test.jpg".into(),
            full_path: path.clone().into(),
            thumbnail: Image::default(),
            has_thumbnail: true,
            selected: true,
            badge_type: "".into(),
            badge_text: "".into(),
            rating: 5,
            is_cropped: state.cropped_files.contains(&path),
        };
        assert!(item.is_cropped);
    }

    #[test]
    fn test_aspect_ratio_and_multi_rating_filters() {
        let empty_customs: Vec<String> = Vec::new();
        let r_land_32 = 6000.0f32 / 4000.0f32;
        let r_port_32 = 6000.0f32.max(4000.0) / 6000.0f32.min(4000.0);
        assert!(matches_aspect_ratio_filter(r_land_32, "3x2", &empty_customs));
        assert!(matches_aspect_ratio_filter(r_port_32, "3x2", &empty_customs));
        assert!(!matches_aspect_ratio_filter(r_land_32, "16x9", &empty_customs));

        let r_169 = 1920.0f32 / 1080.0f32;
        assert!(matches_aspect_ratio_filter(r_169, "16x9", &empty_customs));

        let r_11 = 1000.0f32 / 1000.0f32;
        assert!(matches_aspect_ratio_filter(r_11, "1x1", &empty_customs));

        let r_weird = 2500.0f32 / 1200.0f32;
        assert!(matches_aspect_ratio_filter(r_weird, "OTHER", &empty_customs));
        assert!(!matches_aspect_ratio_filter(r_169, "OTHER", &empty_customs));

        let saved = vec!["18x9".to_string()];
        let r_189 = 2880.0f32 / 1440.0f32;
        assert!(matches_aspect_ratio_filter(r_189, "18x9", &saved));
        assert!(!matches_aspect_ratio_filter(r_189, "OTHER", &saved));

        let mask: u8 = (1 << 3) | (1 << 5);
        assert_eq!((mask & (1 << 3)) != 0, true);
        assert_eq!((mask & (1 << 5)) != 0, true);
        assert_eq!((mask & (1 << 4)) != 0, false);
        assert_eq!((mask & (1 << 0)) != 0, false);
    }

    #[test]
    fn test_sequential_save_filename_generation() {
        let p = Path::new("C:/photos/vacation.jpg");
        let seq1 = generate_next_sequential_path(p);
        assert_eq!(seq1, Path::new("C:/photos/vacation_1.jpg"));

        let p_seq = Path::new("C:/photos/vacation_1.jpg");
        let seq2 = generate_next_sequential_path(p_seq);
        assert_eq!(seq2, Path::new("C:/photos/vacation_2.jpg"));
    }
}
