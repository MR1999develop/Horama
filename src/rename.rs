//! Single and batch file renaming (Windows Explorer style), with collision-safe execution.

use std::path::Path;

/// Names that Windows refuses as file names (with or without extension).
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn validate_name(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Err("File name cannot be empty.".to_string());
    }
    let illegal = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
    if raw.chars().any(|c| illegal.contains(&c) || (c as u32) < 32) {
        return Err("Name contains invalid characters: \\ / : * ? \" < > |".to_string());
    }
    if raw.ends_with('.') || raw.ends_with(' ') {
        return Err("Name cannot end with a dot or a space.".to_string());
    }
    let stem = raw.split('.').next().unwrap_or("").to_lowercase();
    if WINDOWS_RESERVED.contains(&stem.as_str()) {
        return Err(format!("'{}' is a reserved name on Windows.", raw));
    }
    Ok(())
}

fn same_file_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Computes an OS-style rename plan for single or multiple files.
///
/// For 1 file:
/// - The original extension is always kept; the dialog edits the name only. If the user
///   types the same extension again (e.g. "beach.jpg" for a .jpg) it is not duplicated.
/// - Validates against illegal characters, reserved names and existing files.
///
/// For multiple files (Windows Explorer method):
/// - Sequentially names files: `Base (1).ext`, `Base (2).ext`, …, preserving each extension.
/// - Skips counter values that already exist on disk outside this batch.
pub fn compute_batch_rename_plan(
    files: &[String],
    input_base_name: &str,
) -> Result<Vec<(String, String)>, String> {
    let raw_input = input_base_name.trim();
    validate_name(raw_input)?;

    if files.is_empty() {
        return Ok(Vec::new());
    }

    if files.len() == 1 {
        let old_p = Path::new(&files[0]);
        let parent = old_p.parent().unwrap_or(Path::new(""));
        let new_filename = match old_p
            .extension()
            .and_then(|s| s.to_str())
            .filter(|e| !e.is_empty())
        {
            Some(ext) => {
                let typed_same_ext = raw_input
                    .rsplit_once('.')
                    .map(|(_, e)| e.eq_ignore_ascii_case(ext))
                    .unwrap_or(false);
                if typed_same_ext {
                    raw_input.to_string()
                } else {
                    format!("{}.{}", raw_input, ext)
                }
            }
            None => raw_input.to_string(),
        };

        let new_p = parent.join(&new_filename);
        let new_str = new_p.to_string_lossy().to_string();

        if new_str == files[0] {
            return Ok(Vec::new()); // No change
        }

        let is_case_change_on_windows = cfg!(windows) && new_str.eq_ignore_ascii_case(&files[0]);
        if new_p.exists() && !is_case_change_on_windows {
            return Err(format!("A file named '{}' already exists.", new_filename));
        }

        return Ok(vec![(files[0].clone(), new_str)]);
    }

    let base_stem = raw_input;
    let mut plan = Vec::with_capacity(files.len());
    let mut planned_targets: Vec<String> = Vec::new();

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
            counter += 1;

            let candidate_p = parent.join(&candidate_name);
            let candidate_str = candidate_p.to_string_lossy().to_string();

            let already_in_plan = planned_targets
                .iter()
                .any(|t| same_file_name(t, &candidate_str));
            let exists_outside =
                candidate_p.exists() && !files.iter().any(|f| same_file_name(f, &candidate_str));
            if !already_in_plan && !exists_outside {
                planned_targets.push(candidate_str.clone());
                plan.push((file.clone(), candidate_str));
                break;
            }
        }
    }

    Ok(plan)
}

/// Executes a rename plan with a collision-free two-pass rename (originals → temporary
/// names → targets). Entries whose source equals the target are skipped. On failure every
/// file that was already moved is put back under its original name.
pub fn execute_batch_rename(plan: &[(String, String)]) -> Result<(), String> {
    let moves: Vec<&(String, String)> = plan.iter().filter(|(old, new)| old != new).collect();
    if moves.is_empty() {
        return Ok(());
    }

    // Pass 1: originals -> temporary names in the same folder.
    let mut temps: Vec<(String, String, String)> = Vec::with_capacity(moves.len()); // (old, temp, target)
    for (i, (old_path, target_path)) in moves.iter().enumerate() {
        let temp_path = format!("{}.__mr_tmp_{}_{}__", old_path, std::process::id(), i);
        if let Err(e) = std::fs::rename(old_path, &temp_path) {
            for (old, tmp, _) in temps.iter().rev() {
                let _ = std::fs::rename(tmp, old);
            }
            return Err(format!("Failed to rename '{}': {}", old_path, e));
        }
        temps.push((old_path.clone(), temp_path, target_path.clone()));
    }

    // Pass 2: temporary names -> targets. Never overwrite an existing file.
    let mut done: Vec<(String, String)> = Vec::new(); // (old, target)
    for idx in 0..temps.len() {
        let (old, tmp, target) = &temps[idx];
        let target_taken = Path::new(target).exists();
        let result = if target_taken {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a file with this name already exists",
            ))
        } else {
            std::fs::rename(tmp, target)
        };
        if let Err(e) = result {
            // Roll back: finished targets and all remaining temporaries go back to their originals.
            for (o, t) in done.iter().rev() {
                let _ = std::fs::rename(t, o);
            }
            for (o, t, _) in &temps[idx..] {
                let _ = std::fs::rename(t, o);
            }
            return Err(format!("Failed to rename '{}' to '{}': {}", old, target, e));
        }
        done.push((old.clone(), target.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::normalize_path;

    #[test]
    fn single_file_keeps_extension() {
        let files = vec![normalize_path("C:/photos/my_image.jpg")];
        let plan = compute_batch_rename_plan(&files, "vacation").unwrap();
        assert_eq!(plan[0].1, normalize_path("C:/photos/vacation.jpg"));
        // A dot in the name must not drop the extension.
        let plan = compute_batch_rename_plan(&files, "Vacanza.2024").unwrap();
        assert_eq!(plan[0].1, normalize_path("C:/photos/Vacanza.2024.jpg"));
        // Typing the same extension does not duplicate it.
        let plan = compute_batch_rename_plan(&files, "beach.JPG").unwrap();
        assert_eq!(plan[0].1, normalize_path("C:/photos/beach.JPG"));
        assert!(compute_batch_rename_plan(&files, "bad*name").is_err());
        assert!(compute_batch_rename_plan(&files, "   ").is_err());
        assert!(compute_batch_rename_plan(&files, "con").is_err());
        assert!(compute_batch_rename_plan(&files, "trailing.").is_err());
    }

    #[test]
    fn multiple_files_os_style() {
        let files = vec![
            normalize_path("C:/photos/a.jpg"),
            normalize_path("C:/photos/b.NEF"),
            normalize_path("C:/photos/c.png"),
        ];
        let plan = compute_batch_rename_plan(&files, "Holiday").unwrap();
        assert_eq!(plan[0].1, normalize_path("C:/photos/Holiday (1).jpg"));
        assert_eq!(plan[1].1, normalize_path("C:/photos/Holiday (2).NEF"));
        assert_eq!(plan[2].1, normalize_path("C:/photos/Holiday (3).png"));
    }

    fn tmpdir(n: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mriv-ren-{}-{}", n, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rerunning_batch_rename_never_overwrites() {
        // Two files already named like the target pattern plus one new file.
        let d = tmpdir("rerun");
        let f1 = d.join("Holiday (1).jpg");
        let f2 = d.join("Holiday (2).jpg");
        let f3 = d.join("new.jpg");
        std::fs::write(&f1, b"one").unwrap();
        std::fs::write(&f2, b"two").unwrap();
        std::fs::write(&f3, b"three").unwrap();
        let files: Vec<String> = [&f1, &f2, &f3]
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let plan = compute_batch_rename_plan(&files, "Holiday").unwrap();
        execute_batch_rename(&plan).unwrap();
        let mut contents: Vec<Vec<u8>> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| std::fs::read(e.unwrap().path()).unwrap())
            .collect();
        contents.sort();
        assert_eq!(
            contents,
            vec![b"one".to_vec(), b"three".to_vec(), b"two".to_vec()],
            "no file may be lost"
        );
        assert_eq!(std::fs::read(d.join("Holiday (3).jpg")).unwrap(), b"three");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn failed_batch_rolls_back_everything() {
        let d = tmpdir("rollback");
        let a = d.join("a.jpg");
        let b = d.join("b.jpg");
        std::fs::write(&a, b"A").unwrap();
        std::fs::write(&b, b"B").unwrap();
        // Second target already exists (created after planning) -> must roll back.
        let plan = vec![
            (
                a.to_string_lossy().to_string(),
                d.join("x.jpg").to_string_lossy().to_string(),
            ),
            (
                b.to_string_lossy().to_string(),
                d.join("y.jpg").to_string_lossy().to_string(),
            ),
        ];
        std::fs::write(d.join("y.jpg"), b"Y").unwrap();
        assert!(execute_batch_rename(&plan).is_err());
        assert_eq!(std::fs::read(&a).unwrap(), b"A");
        assert_eq!(std::fs::read(&b).unwrap(), b"B");
        assert_eq!(std::fs::read(d.join("y.jpg")).unwrap(), b"Y");
        assert!(!d.join("x.jpg").exists());
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            3,
            "no temporary files left"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
