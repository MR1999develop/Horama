//! Star ratings of the images, kept apart from the settings.
//!
//! The file is a log: one JSON line per change (`{"p":"<path>","r":3}`, `r` 0 = rating
//! removed), so a change adds one line instead of rewriting everything. At load the lines
//! are replayed; when old lines outnumber the live ratings the file is compacted. Writes go
//! through [`crate::persist`], never on the caller's thread.

use crate::config::{normalize_path, path_key};
use crate::persist;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct Line {
    p: String,
    r: u8,
}

#[derive(Debug, Clone, Default)]
pub struct Ratings {
    /// Rating by [`path_key`].
    map: HashMap<String, u8>,
    /// Where changes are written; `None` keeps them in memory only (tests).
    file: Option<PathBuf>,
}

impl Ratings {
    /// Reads the ratings from `file` (an absent file means no ratings).
    pub fn load(file: PathBuf) -> Self {
        let mut map = HashMap::new();
        let mut lines = 0usize;
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                for raw in text.lines().filter(|l| !l.trim().is_empty()) {
                    lines += 1;
                    match serde_json::from_str::<Line>(raw) {
                        Ok(l) if l.r == 0 => {
                            map.remove(&l.p);
                        }
                        Ok(l) => {
                            map.insert(l.p, l.r.min(5));
                        }
                        // A line cut by a crash is skipped; the others are still good.
                        Err(e) => log::warn!("Rating line skipped in {}: {}", file.display(), e),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::error!("Ratings not read from {}: {}", file.display(), e),
        }
        let ratings = Ratings {
            map,
            file: Some(file),
        };
        if lines > 2 * ratings.map.len() + 256 {
            ratings.compact();
        }
        ratings
    }

    /// Ratings kept in memory only.
    pub fn in_memory() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, path: &str) -> u8 {
        if let Some(r) = self.map.get(&path_key(path)) {
            return *r;
        }
        // Ratings saved by older versions used lower-case keys on every platform.
        self.map
            .get(&normalize_path(path).to_lowercase())
            .copied()
            .unwrap_or(0)
    }

    /// Sets a rating (0 removes it) and queues the change for the disk.
    pub fn set(&mut self, path: &str, rating: u8) {
        let key = path_key(path);
        if !cfg!(windows) {
            let legacy = normalize_path(path).to_lowercase();
            if legacy != key && self.map.remove(&legacy).is_some() {
                self.write_line(&legacy, 0);
            }
        }
        let rating = rating.min(5);
        let old = if rating == 0 {
            self.map.remove(&key)
        } else {
            self.map.insert(key.clone(), rating)
        };
        if old.unwrap_or(0) != rating {
            self.write_line(&key, rating);
        }
    }

    /// Moves the rating of a renamed file to its new path.
    pub fn rename(&mut self, old: &str, new: &str) {
        let r = self.get(old);
        self.set(old, 0);
        if r > 0 {
            self.set(new, r);
        }
    }

    /// Adds ratings saved by an older version (inside the settings) and rewrites the file
    /// with all of them. Ratings already here win.
    pub fn import(&mut self, legacy: HashMap<String, u8>) {
        for (k, r) in legacy {
            if r > 0 {
                self.map.entry(k).or_insert(r.min(5));
            }
        }
        self.compact();
    }

    fn write_line(&self, key: &str, rating: u8) {
        let Some(file) = &self.file else { return };
        let line = Line {
            p: key.to_string(),
            r: rating,
        };
        if let Ok(mut json) = serde_json::to_string(&line) {
            json.push('\n');
            persist::append(file.clone(), json.into_bytes());
        }
    }

    /// Rewrites the file with one line per rating.
    fn compact(&self) {
        let Some(file) = &self.file else { return };
        let mut keys: Vec<&String> = self.map.keys().collect();
        keys.sort();
        let mut text = String::new();
        for k in keys {
            let line = Line {
                p: k.clone(),
                r: self.map[k],
            };
            if let Ok(json) = serde_json::to_string(&line) {
                text.push_str(&json);
                text.push('\n');
            }
        }
        persist::replace(file.clone(), text.into_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn changes_are_logged_and_replayed() {
        let dir = std::env::temp_dir().join(format!("horama-ratings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("ratings.jsonl");

        let mut r = Ratings::load(file.clone());
        assert!(r.is_empty());
        r.set("/p/a.jpg", 4);
        r.set("/p/b.jpg", 2);
        r.set("/p/b.jpg", 2); // unchanged: no new line
        r.set("/p/a.jpg", 0);
        r.rename("/p/b.jpg", "/p/c.jpg");
        assert!(persist::flush(Duration::from_secs(10)));
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 5);

        let again = Ratings::load(file.clone());
        assert_eq!(again.len(), 1);
        assert_eq!(again.get("/p/c.jpg"), 2);
        assert_eq!(again.get("/p/a.jpg"), 0);

        // A truncated last line does not lose the others.
        std::fs::write(&file, format!("{}{{\"p\":\"/p/x", text)).unwrap();
        assert_eq!(Ratings::load(file).get("/p/c.jpg"), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_keeps_existing_ratings_and_compacts() {
        let dir = std::env::temp_dir().join(format!("horama-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("ratings.jsonl");
        let mut r = Ratings::load(file.clone());
        r.set("/p/a.jpg", 5);
        let key_a = path_key("/p/a.jpg");
        r.import(HashMap::from([
            (key_a.clone(), 1),
            (path_key("/p/b.jpg"), 3),
            (path_key("/p/c.jpg"), 0),
        ]));
        assert!(persist::flush(Duration::from_secs(10)));
        let loaded = Ratings::load(file.clone());
        assert_eq!(loaded.get("/p/a.jpg"), 5);
        assert_eq!(loaded.get("/p/b.jpg"), 3);
        assert_eq!(loaded.len(), 2);
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_only_ratings_follow_the_rules() {
        let mut r = Ratings::in_memory();
        r.set("/p/a.jpg", 9);
        assert_eq!(r.get("/p/a.jpg"), 5);
        r.rename("/p/a.jpg", "/p/b.jpg");
        assert_eq!((r.get("/p/a.jpg"), r.get("/p/b.jpg")), (0, 5));
    }
}
