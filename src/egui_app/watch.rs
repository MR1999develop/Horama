//! Keeps the open folder in step with the disk: files added, deleted, renamed or changed by
//! other programs appear in the list without reopening the folder.
//!
//! The watcher ([`FolderWatcher`]) reports batches of changes from its own thread; they come
//! back as a [`Msg`] and are applied here. Changes made by Horama itself (rotation, rename,
//! Recycle Bin, Save As) are already in the list and are recognised as such.
//!
//! Files another program added, changed or renamed carry a mark ([`DiskBadge`]) on their
//! thumbnail and over the image until another folder is opened; when the image on screen is
//! deleted, a notice says so.

use super::i18n::{tr, trf};
use super::{HoramaApp, Msg};
use crate::config::{is_supported_file, normalize_path, path_key, same_path};
use crate::file_watcher::{FileSyncAction, FolderWatcher};
use std::path::Path;

/// What another program did to a file of the open folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiskBadge {
    New,
    Modified,
    Renamed,
}

impl DiskBadge {
    /// The short mark on the thumbnail.
    pub(super) fn label(self) -> &'static str {
        match self {
            DiskBadge::New => tr("New"),
            DiskBadge::Modified => tr("Changed"),
            DiskBadge::Renamed => tr("Renamed"),
        }
    }

    /// The longer text over the image.
    pub(super) fn description(self) -> &'static str {
        match self {
            DiskBadge::New => tr("New file"),
            DiskBadge::Modified => tr("Changed on disk by another program"),
            DiskBadge::Renamed => tr("Renamed by another program"),
        }
    }
}

impl HoramaApp {
    /// The mark of `path`, if another program touched it.
    pub(super) fn disk_badge(&self, path: &str) -> Option<DiskBadge> {
        self.disk_badges.get(&path_key(path)).copied()
    }

    fn set_disk_badge(&mut self, path: &str, badge: DiskBadge) {
        self.disk_badges.insert(path_key(path), badge);
    }

    /// Starts watching `folder` (and its subfolders); the previous watcher stops.
    pub(super) fn watch_folder(&mut self, folder: &str) {
        self.watcher = None;
        self.watch_generation += 1;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let generation = self.watch_generation;
        match FolderWatcher::start(folder, move |actions| {
            let _ = tx.send(Msg::Disk {
                generation,
                actions,
            });
            ctx.request_repaint();
        }) {
            Ok(w) => self.watcher = Some(w),
            Err(e) => log::warn!("Folder not watched: {}", e),
        }
    }

    pub(super) fn disk_changed(&mut self, actions: Vec<FileSyncAction>) {
        let mut list_changed = false;
        let mut rescan = false;
        for action in actions {
            match action {
                FileSyncAction::Removed(p) if self.is_renaming(&p) => {}
                FileSyncAction::Created(p) | FileSyncAction::Restored(p)
                    if self.is_renaming(&p) => {}
                FileSyncAction::Renamed { old_path, .. } if self.is_renaming(&old_path) => {}
                FileSyncAction::Removed(p) => {
                    if Path::new(&p).exists() {
                        continue; // a case-only rename, or restored at once
                    }
                    if self.is_known_folder(&p) {
                        rescan = true;
                    } else if self.knows(&p) {
                        if self.current_path().is_some_and(|c| same_path(&c, &p)) {
                            self.notify(
                                trf(
                                    "{} was deleted by another program.",
                                    &[&crate::browse::file_name_of(&p)],
                                ),
                                false,
                            );
                        }
                        self.disk_badges.remove(&path_key(&p));
                        self.all_files.retain(|f| !same_path(f, &p));
                        if let Ok(mut cache) = self.preloader.cache.lock() {
                            cache.remove(&p);
                        }
                        self.thumbs.remove(&p);
                        list_changed = true;
                    }
                }
                FileSyncAction::Created(p) | FileSyncAction::Restored(p) => {
                    if self.add_file(&p) {
                        self.set_disk_badge(&p, DiskBadge::New);
                        list_changed = true;
                    }
                }
                FileSyncAction::Renamed { old_path, new_path } => {
                    if self.knows(&new_path) {
                        continue; // renamed by Horama: the list already has the new name
                    }
                    if self.knows(&old_path) {
                        let new_path = normalize_path(&new_path);
                        self.disk_badges.remove(&path_key(&old_path));
                        self.renamed(vec![(old_path, new_path.clone())], Ok(()));
                        self.set_disk_badge(&new_path, DiskBadge::Renamed);
                    } else if self.add_file(&new_path) {
                        self.set_disk_badge(&new_path, DiskBadge::New);
                        list_changed = true;
                    }
                }
                FileSyncAction::Modified(p) => {
                    let p = normalize_path(&p);
                    if !self.knows(&p) {
                        // An atomic write of a new file (e.g. saved by another program).
                        if self.add_file(&p) {
                            self.set_disk_badge(&p, DiskBadge::New);
                            list_changed = true;
                        }
                    } else if Path::new(&p).exists()
                        && !crate::disk_ops::is_recent_self_write(Path::new(&p))
                    {
                        // A new file keeps its mark: it is still new to the user.
                        if self.disk_badge(&p) != Some(DiskBadge::New) {
                            self.set_disk_badge(&p, DiskBadge::Modified);
                        }
                        self.aspects.remove(&p);
                        self.forget_pixels(&p);
                    }
                }
                FileSyncAction::FolderStructureChanged => rescan = true,
            }
        }
        if rescan {
            self.rescan_folder();
        } else if list_changed {
            self.all_files.sort_by_key(|a| a.to_lowercase());
            self.refilter();
        }
    }

    /// True for a file of a rename Horama is doing (its old or its new name).
    fn is_renaming(&self, path: &str) -> bool {
        self.renaming
            .iter()
            .any(|(old, new)| same_path(old, path) || same_path(new, path))
    }

    fn knows(&self, path: &str) -> bool {
        self.all_files.iter().any(|f| same_path(f, path))
    }

    fn is_known_folder(&self, path: &str) -> bool {
        self.subfolders.iter().any(|d| same_path(d, path))
    }

    /// Adds a new image file to the list; false if it is not one or already listed.
    fn add_file(&mut self, path: &str) -> bool {
        let p = normalize_path(path);
        let pending_trash = self
            .notice
            .as_ref()
            .map(|n| n.trash.iter().any(|t| same_path(t, &p)))
            .unwrap_or(false);
        // Dropped images: new files of their folder do not join the list.
        if self.files_mode || self.knows(&p) || pending_trash || !Path::new(&p).is_file() {
            return false;
        }
        if !is_supported_file(Path::new(&p)) {
            return false;
        }
        let inside = self
            .folder
            .as_deref()
            .map(|root| path_key(&p).starts_with(&crate::browse::dir_prefix_key(root)))
            .unwrap_or(false);
        if inside {
            self.all_files.push(p);
        }
        inside
    }
}
