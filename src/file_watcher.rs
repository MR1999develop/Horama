//! Real-time filesystem watcher using native Windows ReadDirectoryChangesW (via `notify`).
//! Detects deleted, renamed, restored, and newly created image files with debouncing.

use crate::config::{is_supported_file, normalize_path};
use crossbeam_channel::unbounded;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum FileSyncAction {
    /// An existing file in the folder was deleted on disk.
    Removed(String),
    /// A previously removed file reappeared or was restored (e.g. via Undo / Ctrl+Z).
    #[allow(dead_code)]
    Restored(String),
    /// A file was renamed on disk.
    Renamed {
        old_path: String,
        new_path: String,
    },
    /// A new supported image file was added to the folder.
    Created(String),
    /// An existing file in the folder was modified on disk.
    Modified(String),
    /// A directory/folder was added, moved, renamed, or deleted within the watched tree.
    FolderStructureChanged,
}

pub struct FolderWatcher {
    _watcher: RecommendedWatcher,
    active: Arc<AtomicBool>,
}

impl FolderWatcher {
    /// Start watching the given root directory recursively.
    /// Debounced events are passed to the `callback` closure.
    pub fn start<F>(folder_path: &str, callback: F) -> Result<Self, String>
    where
        F: Fn(Vec<FileSyncAction>) + Send + 'static,
    {
        let root = PathBuf::from(folder_path);
        if !root.is_dir() {
            return Err(format!("Directory does not exist: {}", folder_path));
        }

        let (raw_tx, raw_rx) = unbounded::<Event>();
        let active = Arc::new(AtomicBool::new(true));
        let active_clone = active.clone();

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(ev) = res {
                    let _ = raw_tx.send(ev);
                }
            },
            Config::default(),
        )
        .map_err(|e| format!("Failed to create filesystem watcher: {}", e))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| format!("Failed to watch folder: {}", e))?;

        // Background debouncer thread (150ms aggregation window)
        std::thread::spawn(move || {
            let debounce_delay = Duration::from_millis(150);
            let mut last_rename_from: Option<PathBuf> = None;
            let mut explicit_rename_pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
            let mut pending_removes: HashSet<PathBuf> = HashSet::new();
            let mut pending_creates: HashSet<PathBuf> = HashSet::new();
            let mut pending_modifies: HashSet<PathBuf> = HashSet::new();
            let mut last_event_time: Option<Instant> = None;

            loop {
                if !active_clone.load(Ordering::SeqCst) {
                    break;
                }

                let timeout = if let Some(last_time) = last_event_time {
                    let elapsed = last_time.elapsed();
                    if elapsed >= debounce_delay {
                        Duration::from_millis(0)
                    } else {
                        debounce_delay - elapsed
                    }
                } else {
                    Duration::from_millis(100)
                };

                match raw_rx.recv_timeout(timeout) {
                    Ok(event) => {
                        last_event_time = Some(Instant::now());

                        match event.kind {
                            EventKind::Modify(notify::event::ModifyKind::Name(mode)) => {
                                match mode {
                                    notify::event::RenameMode::Both => {
                                        if event.paths.len() >= 2 {
                                            explicit_rename_pairs.push((
                                                event.paths[0].clone(),
                                                event.paths[1].clone(),
                                            ));
                                        }
                                    }
                                    notify::event::RenameMode::From => {
                                        if let Some(p) = event.paths.first() {
                                            last_rename_from = Some(p.clone());
                                            pending_removes.insert(p.clone());
                                        }
                                    }
                                    notify::event::RenameMode::To => {
                                        if let Some(new_p) = event.paths.first() {
                                            if let Some(old_p) = last_rename_from.take() {
                                                pending_removes.remove(&old_p);
                                                explicit_rename_pairs.push((old_p, new_p.clone()));
                                            } else {
                                                pending_creates.insert(new_p.clone());
                                            }
                                        }
                                    }
                                    _ => {
                                        for p in event.paths {
                                            if p.exists() {
                                                pending_creates.insert(p);
                                            } else {
                                                pending_removes.insert(p);
                                            }
                                        }
                                    }
                                }
                            }
                            EventKind::Remove(_) => {
                                for p in event.paths {
                                    pending_removes.insert(p);
                                }
                            }
                            EventKind::Create(_) => {
                                for p in event.paths {
                                    pending_creates.insert(p);
                                }
                            }
                            EventKind::Modify(_) => {
                                for p in event.paths {
                                    if p.exists() {
                                        pending_modifies.insert(p);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if let Some(last_time) = last_event_time {
                            if last_time.elapsed() >= debounce_delay {
                                last_event_time = None;
                                last_rename_from = None;

                                let mut actions = Vec::new();

                                // 1. Process explicit rename pairs
                                for (old_p, new_p) in explicit_rename_pairs.drain(..) {
                                    pending_removes.remove(&old_p);
                                    pending_creates.remove(&new_p);
                                    pending_modifies.remove(&old_p);
                                    pending_modifies.remove(&new_p);
                                    let old_norm = normalize_path(&old_p.to_string_lossy());
                                    let new_norm = normalize_path(&new_p.to_string_lossy());
                                    if new_p.is_dir() || (old_p.extension().is_none() && new_p.extension().is_none()) {
                                        actions.push(FileSyncAction::FolderStructureChanged);
                                    } else if is_supported_file(&new_p) {
                                        actions.push(FileSyncAction::Renamed {
                                            old_path: old_norm,
                                            new_path: new_norm,
                                        });
                                    } else {
                                        actions.push(FileSyncAction::Removed(old_norm));
                                    }
                                }

                                // 2. Heuristic rename correlation:
                                // If a file was removed and another file created in the same directory,
                                // correlate them as a rename!
                                let mut removes_list: Vec<PathBuf> = pending_removes.drain().collect();
                                let mut creates_list: Vec<PathBuf> = pending_creates.drain().collect();

                                removes_list.retain(|old_p| {
                                    if !old_p.exists() {
                                        if let Some(idx) = creates_list.iter().position(|new_p| {
                                            new_p.exists()
                                                && is_supported_file(new_p)
                                                && new_p.parent() == old_p.parent()
                                        }) {
                                            let new_p = creates_list.remove(idx);
                                            pending_modifies.remove(old_p);
                                            pending_modifies.remove(&new_p);
                                            actions.push(FileSyncAction::Renamed {
                                                old_path: normalize_path(&old_p.to_string_lossy()),
                                                new_path: normalize_path(&new_p.to_string_lossy()),
                                            });
                                            return false; // Paired as rename!
                                        }
                                    }
                                    true
                                });

                                // 3. Remaining genuine removes
                                for old_p in removes_list {
                                    pending_modifies.remove(&old_p);
                                    let norm = normalize_path(&old_p.to_string_lossy());
                                    if !old_p.exists() {
                                        if old_p.extension().is_none() {
                                            actions.push(FileSyncAction::FolderStructureChanged);
                                        } else {
                                            actions.push(FileSyncAction::Removed(norm));
                                        }
                                    }
                                }

                                // 4. Remaining genuine creates
                                for new_p in creates_list {
                                    pending_modifies.remove(&new_p);
                                    let norm = normalize_path(&new_p.to_string_lossy());
                                    if new_p.exists() {
                                        if new_p.is_dir() {
                                            actions.push(FileSyncAction::FolderStructureChanged);
                                        } else if is_supported_file(&new_p) {
                                            actions.push(FileSyncAction::Created(norm));
                                        }
                                    }
                                }

                                // 5. Remaining genuine modifications
                                for mod_p in pending_modifies.drain() {
                                    let norm = normalize_path(&mod_p.to_string_lossy());
                                    if mod_p.exists() && is_supported_file(&mod_p) {
                                        actions.push(FileSyncAction::Modified(norm));
                                    }
                                }

                                if !actions.is_empty() {
                                    callback(actions);
                                }
                            }
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            active,
        })
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
    }
}
