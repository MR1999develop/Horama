//! The file list of the open folder as the user browses it: which files are visible (folder
//! tree selection and filters) and which are selected.
//!
//! Pure logic, without UI toolkit, disk access or caches: what the filters need to know about
//! a file comes from the caller through [`FileFacts`]. Every front end (today Slint, later
//! egui) uses these functions, and they are tested without a window.

use crate::config::{path_key, same_path};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

/// Folder tree selection that shows every file of the open folder and its subfolders.
pub const TREE_ALL: &str = "__ALL__";
/// Folder tree selection that shows only the files directly in the open folder.
pub const TREE_ROOT_ONLY: &str = "__ROOT_ONLY__";
/// Ratings mask with every rating (0–5 stars) allowed.
const ALL_RATINGS: u8 = 0b11_1111;

/// What the user filters by. An empty string or `"ALL"` means "no filter" for the text
/// fields; `ratings_mask` has bit `n` set for each allowed rating `n` (0 or 63 = all).
#[derive(Clone, Debug)]
pub struct Filters {
    /// Folder tree selection: [`TREE_ALL`], [`TREE_ROOT_ONLY`] or a folder path.
    pub tree: String,
    pub search: String,
    pub ratings_mask: u8,
    /// `"1x1"`, `"3x2"`… , `"OTHER"` or a saved custom ratio such as `"18x9"`.
    pub aspect: String,
    /// Part of a profile name, or `"NONE"` for files without an embedded profile.
    pub color_profile: String,
    /// `"RAW"`, `"JPG"`, `"PNG"`, `"TIFF"`, `"WEBP"` or another extension.
    pub format: String,
    pub cropped_only: bool,
    pub profile_modified_only: bool,
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            tree: TREE_ALL.to_string(),
            search: String::new(),
            ratings_mask: 0,
            aspect: "ALL".to_string(),
            color_profile: "ALL".to_string(),
            format: "ALL".to_string(),
            cropped_only: false,
            profile_modified_only: false,
        }
    }
}

/// What the filters need to know about a file; provided by the application state.
pub trait FileFacts {
    /// Stars, 0–5.
    fn rating(&self, path: &str) -> u8;
    /// The file has an unsaved edit (crop) in memory.
    fn is_cropped(&self, path: &str) -> bool;
    /// The user chose a color profile for this file.
    fn has_profile_override(&self, path: &str) -> bool;
    /// Long side / short side if known without disk access; `None` = not measured yet,
    /// a non-finite value = unreadable.
    fn aspect_ratio(&self, path: &str) -> Option<f32>;
    /// Name of the profile in use and whether it is embedded or chosen (not the default).
    fn color_profile(&self, path: &str) -> (String, bool);
}

/// Result of [`filter_files`].
#[derive(Debug, Default, PartialEq)]
pub struct Filtered {
    /// Files to show, in the order of the folder scan.
    pub visible: Vec<String>,
    /// Files left out only because their aspect ratio is not known yet: measure them in the
    /// background, then filter again.
    pub unknown_ratio: Vec<String>,
}

/// Files of `all` that the tree selection and the filters let through. `root` is the open
/// folder, `saved_aspects` the custom ratios the user saved.
pub fn filter_files(
    all: &[String],
    root: &str,
    filters: &Filters,
    saved_aspects: &[String],
    facts: &impl FileFacts,
) -> Filtered {
    let search = filters.search.trim().to_lowercase();
    let ratings_active = filters.ratings_mask != 0 && filters.ratings_mask != ALL_RATINGS;
    let aspect_active = is_active(&filters.aspect);
    let profile_active = is_active(&filters.color_profile);
    let profile_wanted = filters.color_profile.to_lowercase();
    let mut out = Filtered::default();

    for path in all
        .iter()
        .filter(|f| in_tree_selection(f, root, &filters.tree))
    {
        if !search.is_empty() && !file_name_of(path).to_lowercase().contains(&search) {
            continue;
        }
        if !matches_format_filter(path, &filters.format) {
            continue;
        }
        if filters.cropped_only && !facts.is_cropped(path) {
            continue;
        }
        if filters.profile_modified_only && !facts.has_profile_override(path) {
            continue;
        }
        if ratings_active && filters.ratings_mask & (1 << facts.rating(path).min(5)) == 0 {
            continue;
        }
        if aspect_active {
            match facts.aspect_ratio(path) {
                Some(r) if r.is_finite() => {
                    if !matches_aspect_ratio_filter(r, &filters.aspect, saved_aspects) {
                        continue;
                    }
                }
                Some(_) => continue,
                None => {
                    out.unknown_ratio.push(path.clone());
                    continue;
                }
            }
        }
        if profile_active {
            let (name, has_profile) = facts.color_profile(path);
            let keep = if filters.color_profile.eq_ignore_ascii_case("NONE") {
                !has_profile
            } else {
                name.to_lowercase().contains(&profile_wanted)
            };
            if !keep {
                continue;
            }
        }
        out.visible.push(path.clone());
    }
    out
}

fn is_active(filter: &str) -> bool {
    !filter.is_empty() && filter != "ALL"
}

/// Whether `file` belongs to the folder tree selection `tree` of the open folder `root`.
/// A folder selection includes its subfolders.
pub fn in_tree_selection(file: &str, root: &str, tree: &str) -> bool {
    let parent_is = |dir: &str| {
        Path::new(file)
            .parent()
            .map(|p| same_path(&p.to_string_lossy(), dir))
            .unwrap_or(false)
    };
    match tree {
        TREE_ALL => true,
        TREE_ROOT_ONLY => parent_is(root),
        folder => path_key(file).starts_with(&dir_prefix_key(folder)) || parent_is(folder),
    }
}

/// Comparison key for "is inside this folder" checks (`normalize_path` drops trailing
/// separators, so the separator is appended after normalizing).
pub fn dir_prefix_key(dir: &str) -> String {
    let k = path_key(dir);
    if k.ends_with(std::path::MAIN_SEPARATOR) {
        k // drive root such as "C:\"
    } else {
        format!("{}{}", k, std::path::MAIN_SEPARATOR)
    }
}

pub fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

pub fn matches_format_filter(path: &str, filter: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
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

/// `"18x9"` → 2.0 (long side / short side). Only `x` separates the two numbers.
pub fn parse_aspect_ratio_str(s: &str) -> Option<f32> {
    let clean = s.trim();
    if clean.is_empty() || clean.contains(':') || clean.contains('/') {
        return None;
    }
    let (w, h) = clean.split_once(['x', 'X'])?;
    let (w, h) = (w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?);
    (w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()).then(|| w.max(h) / w.min(h))
}

/// Ratios within 1% count as equal (sensor sizes and crops are never exact).
fn ratio_matches_target(ratio: f32, target: f32) -> bool {
    let (min_r, max_r) = (ratio.min(target), ratio.max(target));
    max_r > 0.0 && (min_r / max_r) >= 0.99
}

const STANDARD_RATIOS: [(&str, f32); 7] = [
    ("1x1", 1.0),
    ("3x2", 1.5),
    ("4x3", 4.0 / 3.0),
    ("5x4", 1.25),
    ("16x9", 16.0 / 9.0),
    ("21x9", 21.0 / 9.0),
    ("65x24", 65.0 / 24.0),
];

/// Whether an orientation-independent `ratio` passes the aspect-ratio filter `filter`.
/// `"OTHER"` keeps ratios that match neither a standard nor a saved custom ratio; an
/// unrecognised filter keeps everything.
pub fn matches_aspect_ratio_filter(ratio: f32, filter: &str, saved_customs: &[String]) -> bool {
    let named = filter.replace(':', "x");
    if let Some((_, target)) = STANDARD_RATIOS.iter().find(|(n, _)| *n == named) {
        return ratio_matches_target(ratio, *target);
    }
    match filter {
        "ALL" | "" => true,
        "CUSTOM" | "OTHER" => {
            !STANDARD_RATIOS
                .iter()
                .any(|(_, t)| ratio_matches_target(ratio, *t))
                && !saved_customs.iter().any(|s| {
                    parse_aspect_ratio_str(s)
                        .map(|t| ratio_matches_target(ratio, t))
                        .unwrap_or(false)
                })
        }
        saved => parse_aspect_ratio_str(saved)
            .map(|t| ratio_matches_target(ratio, t))
            .unwrap_or(true),
    }
}

/// One row of the folder tree, in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// Folder name, or `"[Direct Files]"` for [`TREE_ROOT_ONLY`].
    pub name: String,
    /// Number of files in the folder and its subfolders (direct files only for the
    /// [`TREE_ROOT_ONLY`] row).
    pub count: usize,
    /// Value for [`Filters::tree`]: [`TREE_ALL`] (the open folder), [`TREE_ROOT_ONLY`] or the
    /// folder path.
    pub key: String,
    /// 0 for the open folder, 1 for its direct subfolders, and so on.
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
    pub selected: bool,
}

/// Rows of the folder tree of the open folder `root`: the folder itself, then (if expanded and
/// it has subfolders) a row for its direct files and the subfolders whose ancestors are all
/// expanded. `subfolders` is in scan order (parents before children); `expanded` holds
/// [`path_key`]s of expanded folders ([`TREE_ALL`] for the root); `selected` is the current
/// [`Filters::tree`].
pub fn tree_rows(
    root: &str,
    subfolders: &[String],
    files: &[String],
    expanded: &HashSet<String>,
    selected: &str,
) -> Vec<TreeRow> {
    let root_key = path_key(root);
    let is_expanded = |key: &str| expanded.contains(key);
    // Files per folder, counted once for every ancestor up to the root.
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut direct = 0;
    for f in files {
        let mut dir = Path::new(f).parent();
        if dir
            .map(|d| path_key(&d.to_string_lossy()) == root_key)
            .unwrap_or(false)
        {
            direct += 1;
        }
        while let Some(d) = dir {
            let key = path_key(&d.to_string_lossy());
            if key == root_key || key.len() < root_key.len() {
                break;
            }
            *counts.entry(key).or_default() += 1;
            dir = d.parent();
        }
    }
    let parent_key = |p: &str| {
        Path::new(p)
            .parent()
            .map(|d| path_key(&d.to_string_lossy()))
    };
    let with_children: HashSet<String> = subfolders.iter().filter_map(|s| parent_key(s)).collect();

    let root_expanded = expanded.is_empty() || is_expanded(TREE_ALL) || is_expanded(&root_key);
    let mut rows = vec![TreeRow {
        name: file_name_of(root),
        count: files.len(),
        key: TREE_ALL.to_string(),
        depth: 0,
        has_children: !subfolders.is_empty(),
        expanded: root_expanded,
        selected: selected == TREE_ALL,
    }];
    if !root_expanded || subfolders.is_empty() {
        return rows;
    }
    rows.push(TreeRow {
        name: "[Direct Files]".to_string(),
        count: direct,
        key: TREE_ROOT_ONLY.to_string(),
        depth: 1,
        has_children: false,
        expanded: false,
        selected: selected == TREE_ROOT_ONLY,
    });
    for sub in subfolders {
        let key = path_key(sub);
        // Every folder between the root and this one must be expanded.
        let mut visible = true;
        let mut depth = 1;
        let mut dir = Path::new(sub).parent();
        while let Some(d) = dir {
            let k = path_key(&d.to_string_lossy());
            if k == root_key || k.len() < root_key.len() {
                break;
            }
            visible &= is_expanded(&k);
            depth += 1;
            dir = d.parent();
        }
        if !visible {
            continue;
        }
        rows.push(TreeRow {
            name: file_name_of(sub),
            count: counts.get(&key).copied().unwrap_or(0),
            key: sub.clone(),
            depth,
            has_children: with_children.contains(&key),
            expanded: is_expanded(&key),
            selected: selected != TREE_ALL
                && selected != TREE_ROOT_ONLY
                && same_path(selected, sub),
        });
    }
    rows
}

/// Marks `folder` and every folder between it and `root` as expanded, so the folder is
/// visible in the tree.
pub fn expand_to(expanded: &mut HashSet<String>, root: &str, folder: &str) {
    let root_key = path_key(root);
    expanded.insert(TREE_ALL.to_string());
    let mut dir = Some(Path::new(folder));
    while let Some(d) = dir {
        let k = path_key(&d.to_string_lossy());
        if k.len() < root_key.len() {
            break;
        }
        expanded.insert(k.clone());
        if k == root_key {
            break;
        }
        dir = d.parent();
    }
}

/// Expands a collapsed folder of the tree or collapses an expanded one. `key` is a
/// [`TreeRow::key`].
pub fn toggle_expanded(expanded: &mut HashSet<String>, root: &str, key: &str) {
    let root_key = path_key(root);
    if key == TREE_ALL {
        let open =
            expanded.is_empty() || expanded.contains(TREE_ALL) || expanded.contains(&root_key);
        if open {
            expanded.remove(TREE_ALL);
            expanded.remove(&root_key);
            // An empty set means "root expanded": keep a marker so it stays collapsed.
            expanded.insert(String::new());
        } else {
            expanded.insert(TREE_ALL.to_string());
            expanded.insert(root_key);
        }
    } else {
        let k = path_key(key);
        if !expanded.remove(&k) {
            expanded.insert(k);
        }
    }
}

/// Position to select after the list was filtered again: the previously shown file if it is
/// still visible, otherwise the first one.
pub fn index_after_refilter(previous: Option<&str>, visible: &[String]) -> usize {
    previous
        .and_then(|p| visible.iter().position(|f| f == p))
        .unwrap_or(0)
}

/// Modifier keys held during a click in the file list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClickMods {
    /// Ctrl (Cmd on macOS): add or remove one file.
    pub toggle: bool,
    /// Shift: select the range from the anchor.
    pub range: bool,
}

/// Applies a click on item `idx` of a list of `len` items to the selection, following the
/// file manager conventions: plain click selects one item, Ctrl toggles one, Shift selects the
/// range from the anchor (Ctrl+Shift adds the range). The last selected item can't be removed
/// with Ctrl.
///
/// Returns the item to show, or `None` when the image on screen stays the same.
pub fn apply_click(
    selected: &mut BTreeSet<usize>,
    anchor: &mut Option<usize>,
    current: Option<usize>,
    idx: usize,
    len: usize,
    mods: ClickMods,
) -> Option<usize> {
    if idx >= len {
        return None;
    }
    if mods.range {
        let from = anchor.or(current).unwrap_or(0).min(len - 1);
        if !mods.toggle {
            selected.clear();
        }
        selected.extend(from.min(idx)..=from.max(idx));
        Some(idx)
    } else if mods.toggle {
        if selected.contains(&idx) {
            if selected.len() > 1 {
                selected.remove(&idx);
            }
            None
        } else {
            selected.insert(idx);
            *anchor = Some(idx);
            Some(idx)
        }
    } else {
        selected.clear();
        selected.insert(idx);
        *anchor = Some(idx);
        Some(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Facts {
        ratings: HashMap<String, u8>,
        cropped: Vec<String>,
        overrides: Vec<String>,
        ratios: HashMap<String, f32>,
        profiles: HashMap<String, String>,
    }

    impl FileFacts for Facts {
        fn rating(&self, p: &str) -> u8 {
            self.ratings.get(p).copied().unwrap_or(0)
        }
        fn is_cropped(&self, p: &str) -> bool {
            self.cropped.iter().any(|c| c == p)
        }
        fn has_profile_override(&self, p: &str) -> bool {
            self.overrides.iter().any(|c| c == p)
        }
        fn aspect_ratio(&self, p: &str) -> Option<f32> {
            self.ratios.get(p).copied()
        }
        fn color_profile(&self, p: &str) -> (String, bool) {
            match self.profiles.get(p) {
                Some(n) => (n.clone(), true),
                None => ("sRGB".to_string(), false),
            }
        }
    }

    fn p(parts: &[&str]) -> String {
        let mut b = std::path::PathBuf::from(std::path::MAIN_SEPARATOR_STR);
        for x in parts {
            b.push(x);
        }
        b.to_string_lossy().to_string()
    }

    fn files() -> (String, Vec<String>) {
        let root = p(&["photos"]);
        let all = vec![
            p(&["photos", "a.jpg"]),
            p(&["photos", "b.CR3"]),
            p(&["photos", "trip", "c.png"]),
            p(&["photos", "trip", "day2", "d.jpg"]),
            p(&["photos", "tripod", "e.jpg"]),
        ];
        (root, all)
    }

    fn visible(all: &[String], root: &str, f: &Filters, facts: &Facts) -> Vec<String> {
        filter_files(all, root, f, &[], facts)
            .visible
            .iter()
            .map(|v| file_name_of(v))
            .collect()
    }

    #[test]
    fn tree_selection_includes_subfolders_but_not_similar_names() {
        let (root, all) = files();
        let facts = Facts::default();
        let mut f = Filters::default();
        assert_eq!(visible(&all, &root, &f, &facts).len(), 5);
        f.tree = TREE_ROOT_ONLY.into();
        assert_eq!(visible(&all, &root, &f, &facts), ["a.jpg", "b.CR3"]);
        f.tree = p(&["photos", "trip"]);
        // "tripod" starts with "trip" but is another folder.
        assert_eq!(visible(&all, &root, &f, &facts), ["c.png", "d.jpg"]);
    }

    #[test]
    fn search_format_and_ratings_combine() {
        let (root, all) = files();
        let mut facts = Facts::default();
        facts.ratings.insert(all[0].clone(), 5);
        facts.ratings.insert(all[3].clone(), 3);
        let mut f = Filters {
            format: "JPG".into(),
            ..Filters::default()
        };
        assert_eq!(
            visible(&all, &root, &f, &facts),
            ["a.jpg", "d.jpg", "e.jpg"]
        );
        f.ratings_mask = 1 << 5 | 1 << 3;
        assert_eq!(visible(&all, &root, &f, &facts), ["a.jpg", "d.jpg"]);
        f.search = "  D.J ".into();
        assert_eq!(visible(&all, &root, &f, &facts), ["d.jpg"]);
        f.format = "RAW".into();
        f.search.clear();
        f.ratings_mask = 0;
        assert_eq!(visible(&all, &root, &f, &facts), ["b.CR3"]);
    }

    #[test]
    fn unknown_aspect_ratios_are_reported_not_shown() {
        let (root, all) = files();
        let mut facts = Facts::default();
        facts.ratios.insert(all[0].clone(), 1.5);
        facts.ratios.insert(all[1].clone(), 16.0 / 9.0);
        facts.ratios.insert(all[2].clone(), f32::NAN);
        let f = Filters {
            aspect: "3x2".into(),
            ..Filters::default()
        };
        let r = filter_files(&all, &root, &f, &[], &facts);
        assert_eq!(r.visible, vec![all[0].clone()]);
        assert_eq!(r.unknown_ratio, vec![all[3].clone(), all[4].clone()]);
    }

    #[test]
    fn crop_profile_and_override_filters() {
        let (root, all) = files();
        let mut facts = Facts::default();
        facts.cropped.push(all[2].clone());
        facts.overrides.push(all[4].clone());
        facts
            .profiles
            .insert(all[1].clone(), "Adobe RGB (1998)".into());
        let base = Filters::default();
        let only = |f: Filters| visible(&all, &root, &f, &facts);
        assert_eq!(
            only(Filters {
                cropped_only: true,
                ..base.clone()
            }),
            ["c.png"]
        );
        assert_eq!(
            only(Filters {
                profile_modified_only: true,
                ..base.clone()
            }),
            ["e.jpg"]
        );
        assert_eq!(
            only(Filters {
                color_profile: "adobe".into(),
                ..base.clone()
            }),
            ["b.CR3"]
        );
        assert_eq!(
            only(Filters {
                color_profile: "NONE".into(),
                ..base
            })
            .len(),
            4
        );
    }

    #[test]
    fn aspect_ratio_filters() {
        let empty: Vec<String> = Vec::new();
        assert!(matches_aspect_ratio_filter(6000.0 / 4000.0, "3x2", &empty));
        assert!(matches_aspect_ratio_filter(6000.0 / 4000.0, "3:2", &empty));
        assert!(!matches_aspect_ratio_filter(
            6000.0 / 4000.0,
            "16x9",
            &empty
        ));
        assert!(matches_aspect_ratio_filter(1920.0 / 1080.0, "16x9", &empty));
        assert!(matches_aspect_ratio_filter(1.0, "1x1", &empty));
        assert!(matches_aspect_ratio_filter(
            2500.0 / 1200.0,
            "OTHER",
            &empty
        ));
        let saved = vec!["18x9".to_string()];
        assert!(matches_aspect_ratio_filter(2.0, "18x9", &saved));
        assert!(!matches_aspect_ratio_filter(2.0, "OTHER", &saved));
        assert!(matches_aspect_ratio_filter(2.0, "nonsense", &saved));
        assert_eq!(parse_aspect_ratio_str("9 x 18"), Some(2.0));
        assert_eq!(parse_aspect_ratio_str("18:9"), None);
        assert_eq!(parse_aspect_ratio_str("0x9"), None);
        assert_eq!(parse_aspect_ratio_str("x9"), None);
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
    fn clicks_follow_file_manager_conventions() {
        let mut sel = BTreeSet::new();
        let mut anchor = None;
        let plain = ClickMods::default();
        let ctrl = ClickMods {
            toggle: true,
            range: false,
        };
        let shift = ClickMods {
            toggle: false,
            range: true,
        };
        let ctrl_shift = ClickMods {
            toggle: true,
            range: true,
        };

        assert_eq!(
            apply_click(&mut sel, &mut anchor, None, 2, 10, plain),
            Some(2)
        );
        assert_eq!(
            (sel.iter().copied().collect::<Vec<_>>(), anchor),
            (vec![2], Some(2))
        );

        assert_eq!(
            apply_click(&mut sel, &mut anchor, Some(2), 5, 10, shift),
            Some(5)
        );
        assert_eq!(sel.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4, 5]);
        assert_eq!(anchor, Some(2), "a range keeps its anchor");

        assert_eq!(
            apply_click(&mut sel, &mut anchor, Some(5), 3, 10, ctrl),
            None
        );
        assert_eq!(sel.iter().copied().collect::<Vec<_>>(), vec![2, 4, 5]);

        assert_eq!(
            apply_click(&mut sel, &mut anchor, Some(5), 8, 10, ctrl),
            Some(8)
        );
        assert_eq!(anchor, Some(8));
        assert_eq!(
            apply_click(&mut sel, &mut anchor, Some(8), 9, 10, ctrl_shift),
            Some(9)
        );
        assert_eq!(sel.iter().copied().collect::<Vec<_>>(), vec![2, 4, 5, 8, 9]);

        // The last selected item stays selected.
        let mut one = BTreeSet::from([4]);
        assert_eq!(
            apply_click(&mut one, &mut anchor, Some(4), 4, 10, ctrl),
            None
        );
        assert_eq!(one.len(), 1);
        // Out of range: nothing happens.
        assert_eq!(
            apply_click(&mut one, &mut anchor, Some(4), 10, 10, plain),
            None
        );
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn tree_rows_follow_expansion_and_count_nested_files() {
        let (root, all) = files();
        let subs = vec![
            p(&["photos", "trip"]),
            p(&["photos", "trip", "day2"]),
            p(&["photos", "tripod"]),
        ];
        let mut expanded = HashSet::new();
        let rows = tree_rows(&root, &subs, &all, &expanded, TREE_ALL);
        let names: Vec<_> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.count, r.depth))
            .collect();
        // Empty expansion set = root expanded; "day2" hidden until "trip" is expanded.
        assert_eq!(
            names,
            [
                ("photos", 5, 0),
                ("[Direct Files]", 2, 1),
                ("trip", 2, 1),
                ("tripod", 1, 1)
            ]
        );
        assert!(rows[0].selected && rows[2].has_children && !rows[3].has_children);

        expand_to(&mut expanded, &root, &p(&["photos", "trip", "day2"]));
        let rows = tree_rows(&root, &subs, &all, &expanded, &subs[1]);
        let day2 = rows
            .iter()
            .find(|r| r.name == "day2")
            .expect("day2 visible");
        assert_eq!((day2.count, day2.depth, day2.selected), (1, 2, true));
        assert!(!rows[0].selected);

        toggle_expanded(&mut expanded, &root, TREE_ALL);
        assert_eq!(tree_rows(&root, &subs, &all, &expanded, TREE_ALL).len(), 1);
        toggle_expanded(&mut expanded, &root, TREE_ALL);
        assert!(tree_rows(&root, &subs, &all, &expanded, TREE_ALL).len() > 1);
        toggle_expanded(&mut expanded, &root, &subs[0]);
        assert!(!tree_rows(&root, &subs, &all, &expanded, TREE_ALL)
            .iter()
            .any(|r| r.name == "day2"));

        // Root collapsed: only the root row.
        let collapsed: HashSet<String> = HashSet::from(["something-else".to_string()]);
        assert_eq!(tree_rows(&root, &subs, &all, &collapsed, TREE_ALL).len(), 1);
    }

    #[test]
    fn refilter_keeps_the_current_file() {
        let v = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(index_after_refilter(Some("c"), &v), 2);
        assert_eq!(index_after_refilter(Some("x"), &v), 0);
        assert_eq!(index_after_refilter(None, &v), 0);
    }
}
