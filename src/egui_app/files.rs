//! File operations of the egui interface: lossless rotation, Recycle Bin with Undo, rename and
//! Save As with the JPEG quality window.
//!
//! The work runs on the disk queue ([`DiskQueue`], one thread, in order) and the outcome comes
//! back as a [`Msg`]; the interface shows it in a notice inside the image view. Moving to the
//! Recycle Bin has no confirmation: the files leave the list at once and reach the Recycle Bin
//! only when the notice closes, so Undo (or Ctrl+Z) can put them back until then.

use super::i18n::{tr, trf};
use super::{HoramaApp, Msg, theme};
use crate::browse;
use crate::config::{path_key, same_path};
use crate::disk_ops::{self, RotationOutcome};
use crate::image_saver::{generate_next_sequential_path, is_jpeg};
use crate::rename::{compute_batch_rename_plan, execute_batch_rename};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Outcome notices close by themselves after this time; errors stay until closed.
pub(super) const NOTICE_SECONDS: u64 = 8;
/// Quality proposed the first time a JPEG is saved at a chosen quality.
const FIRST_JPEG_QUALITY: u8 = 90;

/// A message shown at the bottom of the image view.
pub(super) struct Notice {
    pub text: String,
    pub is_error: bool,
    /// Files the Undo button puts back (a pending move to the Recycle Bin).
    pub trash: Vec<String>,
    pub shown: Instant,
}

/// The rename window.
pub(super) struct RenameDialog {
    pub files: Vec<String>,
    pub name: String,
    pub error: Option<String>,
    pub focus: bool,
}

/// The JPEG quality window, shown before saving a JPEG.
pub(super) struct QualityDialog {
    pub source: String,
    pub dest: PathBuf,
    /// Keep the compression of the original (and no recompression if nothing changed).
    pub like_original: bool,
    pub quality: u8,
}

/// Results of the disk queue.
pub(super) enum FileResult {
    Rotated {
        path: String,
        clockwise: bool,
        result: Result<RotationOutcome, String>,
    },
    Trashed {
        removed: Vec<String>,
        failed: Vec<(String, String)>,
    },
    Renamed {
        plan: Vec<(String, String)>,
        result: Result<(), String>,
    },
    Saved {
        source: String,
        dest: PathBuf,
        result: Result<(), String>,
    },
}

impl HoramaApp {
    /// Shows a notice; a pending move to the Recycle Bin of the previous one is carried out.
    pub(super) fn notify(&mut self, text: impl Into<String>, is_error: bool) {
        self.commit_trash();
        let text = text.into();
        if is_error {
            log::warn!("{}", text);
        }
        self.notice = Some(Notice {
            text,
            is_error,
            trash: Vec::new(),
            shown: Instant::now(),
        });
    }

    /// Closes the notice that timed out (outcomes only).
    pub(super) fn expire_notice(&mut self) {
        let Some(n) = &self.notice else { return };
        if n.is_error {
            return;
        }
        let left = Duration::from_secs(NOTICE_SECONDS).saturating_sub(n.shown.elapsed());
        if left.is_zero() {
            self.close_notice();
        } else {
            self.ctx.request_repaint_after(left);
        }
    }

    pub(super) fn close_notice(&mut self) {
        self.commit_trash();
        self.notice = None;
    }

    /// Files the next command acts on: the selection, or the current image.
    pub(super) fn targets(&self) -> Vec<String> {
        if self.selected.is_empty() {
            self.current_path().into_iter().collect()
        } else {
            self.selected
                .iter()
                .filter_map(|&i| self.visible.get(i).cloned())
                .collect()
        }
    }

    // ---- Rotation --------------------------------------------------------------------

    /// Rotates the selected files by 90° on disk, without quality loss.
    pub(super) fn rotate(&mut self, clockwise: bool) {
        let files = self.targets();
        self.rotate_paths(&files, clockwise, true);
    }

    /// Rotates `files` on disk; `record` adds the step to the Undo history.
    pub(super) fn rotate_paths(&mut self, files: &[String], clockwise: bool, record: bool) {
        if files.is_empty() {
            return;
        }
        let turns = if clockwise { 1 } else { 3 };
        let mut on_screen = Vec::new();
        let mut cropped = Vec::new();
        let mut rotated = Vec::new();
        for path in files.iter().cloned() {
            // The crop was drawn on the image as it is: rotating would misplace it.
            if self.edited_pixels(&path).is_some() {
                cropped.push(browse::file_name_of(&path));
                continue;
            }
            rotated.push(path.clone());
            if !disk_ops::rotation_format_supported(Path::new(&path)) {
                on_screen.push(path);
                continue;
            }
            let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
            self.disk.submit(move || {
                let result = disk_ops::persist_rotation(Path::new(&path), turns);
                let _ = tx.send(Msg::File(FileResult::Rotated {
                    path,
                    clockwise,
                    result,
                }));
                ctx.request_repaint();
            });
        }
        if !on_screen.is_empty() {
            self.rotate_on_screen(&on_screen, clockwise);
        }
        if record && !rotated.is_empty() {
            self.push_history(super::edit::Action::Rotate {
                paths: rotated,
                clockwise,
            });
        }
        if !cropped.is_empty() {
            self.notify(
                trf(
                    "Not rotated: {} (save or undo the crop first).",
                    &[&cropped.join(", ")],
                ),
                false,
            );
        }
    }

    fn rotated(&mut self, path: String, clockwise: bool, result: Result<RotationOutcome, String>) {
        let name = browse::file_name_of(&path);
        match result {
            Ok(RotationOutcome::Saved(_)) => {
                self.forget_pixels(&path);
                self.notify(trf("Rotated: {}", &[&name]), false);
            }
            Ok(RotationOutcome::ShownOnly(reason)) => {
                // Left untouched on purpose (e.g. animated): turned on screen instead.
                log::info!("Rotation shown only for {}: {}", path, reason);
                self.rotate_on_screen(std::slice::from_ref(&path), clockwise);
            }
            Err(e) => {
                // The history no longer matches the file.
                self.undo.retain(|a| !a.rotates(&path));
                self.redo.retain(|a| !a.rotates(&path));
                self.notify(trf("Rotation not saved for '{}': {}", &[&name, &e]), true)
            }
        }
    }

    /// Drops the cached pixels and thumbnail of a file changed on disk, and reloads it if it
    /// is on screen.
    pub(super) fn forget_pixels(&mut self, path: &str) {
        if let Ok(mut cache) = self.preloader.cache.lock() {
            cache.remove(path);
        }
        self.thumbs.remove(path);
        self.request_thumbs();
        if self.shown.as_ref().map(|s| s.path == path).unwrap_or(false) {
            self.load(path.to_string());
        }
    }

    // ---- Recycle Bin -----------------------------------------------------------------

    /// Takes the selected files out of the list and offers Undo; they reach the Recycle Bin
    /// when the notice closes.
    pub(super) fn trash_selected(&mut self) {
        let files = self.targets();
        if files.is_empty() {
            return;
        }
        let text = match files.len() {
            1 => trf(
                "'{}' moved to the Recycle Bin.",
                &[&browse::file_name_of(&files[0])],
            ),
            n => trf("{} files moved to the Recycle Bin.", &[&n]),
        };
        self.notify(text, false);
        let keys: Vec<String> = files.iter().map(|f| path_key(f)).collect();
        self.all_files.retain(|f| !keys.contains(&path_key(f)));
        if let Some(n) = &mut self.notice {
            n.trash = files;
        }
        self.refilter();
        if self.visible.is_empty() {
            self.shown = None;
        }
    }

    /// Puts back the files of the pending move to the Recycle Bin. False if there is none.
    pub(super) fn undo_trash(&mut self) -> bool {
        let Some(n) = self.notice.as_mut().filter(|n| !n.trash.is_empty()) else {
            return false;
        };
        let files = std::mem::take(&mut n.trash);
        self.notice = None;
        self.restore_files(&files);
        self.notify(trf("Restored {} file(s).", &[&files.len()]), false);
        true
    }

    /// Adds files back to the list (sorted like a folder scan) and shows the first one.
    fn restore_files(&mut self, files: &[String]) {
        for f in files {
            if !self.all_files.iter().any(|g| same_path(g, f)) {
                self.all_files.push(f.clone());
            }
        }
        self.all_files.sort_by_key(|a| a.to_lowercase());
        self.refilter();
        if let Some(i) = files
            .first()
            .and_then(|f| self.visible.iter().position(|g| same_path(g, f)))
        {
            self.select_only(i);
        }
    }

    /// Carries out the pending move to the Recycle Bin, on the disk queue.
    pub(super) fn commit_trash(&mut self) {
        let Some(n) = self.notice.as_mut() else {
            return;
        };
        let files = std::mem::take(&mut n.trash);
        if files.is_empty() {
            return;
        }
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.disk.submit(move || {
            let mut removed = Vec::new();
            let mut failed = Vec::new();
            for f in files {
                match disk_ops::move_to_trash(Path::new(&f)) {
                    Ok(()) => removed.push(f),
                    Err(e) => failed.push((f, e)),
                }
            }
            if !removed.is_empty() {
                log::info!("Moved {} file(s) to the Recycle Bin", removed.len());
            }
            let _ = tx.send(Msg::File(FileResult::Trashed { removed, failed }));
            ctx.request_repaint();
        });
    }

    fn trashed(&mut self, removed: Vec<String>, failed: Vec<(String, String)>) {
        if let Ok(mut cache) = self.preloader.cache.lock() {
            for f in &removed {
                cache.remove(f);
            }
        }
        for f in &removed {
            self.thumbs.remove(f);
            self.settings.set_rating(f, 0);
            self.edited.remove(&path_key(f));
            self.view_turns.remove(&path_key(f));
        }
        if failed.is_empty() {
            return;
        }
        let back: Vec<String> = failed
            .iter()
            .map(|(f, _)| f.clone())
            .filter(|f| Path::new(f).exists())
            .collect();
        let details: Vec<String> = failed
            .iter()
            .map(|(f, e)| format!("{}: {}", browse::file_name_of(f), e))
            .collect();
        self.restore_files(&back);
        self.notify(
            trf(
                "{} file(s) could not be moved to the Recycle Bin and are back in the list. {}",
                &[&failed.len(), &details.join("; ")],
            ),
            true,
        );
    }

    // ---- Rename ----------------------------------------------------------------------

    pub(super) fn start_rename(&mut self) {
        let files = self.targets();
        let Some(first) = files.first() else { return };
        let name = if files.len() == 1 {
            Path::new(first)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.rename = Some(RenameDialog {
            files,
            name,
            error: None,
            focus: true,
        });
    }

    pub(super) fn rename_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.rename else {
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        let title = match dialog.files.len() {
            1 => tr("Rename").to_string(),
            n => trf("Rename {} files", &[&n]),
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                if dialog.files.len() > 1 {
                    ui.label(tr(
                        "The files become \"Name (1)\", \"Name (2)\"… keeping their extension.",
                    ));
                }
                let field = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(320.0)
                        .hint_text(tr("New name")),
                );
                if std::mem::take(&mut dialog.focus) {
                    field.request_focus();
                }
                if field.changed() {
                    dialog.error = None;
                }
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    apply = true;
                }
                if let Some(e) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.horizontal(|ui| {
                    if ui.button(tr("Rename")).clicked() {
                        apply = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if cancel {
            self.rename = None;
            return;
        }
        if !apply {
            return;
        }
        match compute_batch_rename_plan(&dialog.files, &dialog.name) {
            Ok(plan) => {
                self.rename = None;
                let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
                self.disk.submit(move || {
                    let result = execute_batch_rename(&plan);
                    let _ = tx.send(Msg::File(FileResult::Renamed { plan, result }));
                    ctx.request_repaint();
                });
            }
            Err(e) => {
                dialog.error = Some(e);
                dialog.focus = true;
            }
        }
    }

    pub(super) fn renamed(&mut self, plan: Vec<(String, String)>, result: Result<(), String>) {
        if let Err(e) = result {
            self.notify(trf("Not renamed: {}", &[&e]), true);
            return;
        }
        let current = self.current_path();
        let mut new_current = None;
        for (old, new) in &plan {
            for f in self.all_files.iter_mut().filter(|f| same_path(f, old)) {
                *f = new.clone();
            }
            self.settings.rename_rating(old, new);
            if let Some(t) = self.view_turns.remove(&path_key(old)) {
                self.view_turns.insert(path_key(new), t);
            }
            if let Some(d) = self.edited.remove(&path_key(old)) {
                self.edited.insert(path_key(new), d);
            }
            if let Some(p) = self.profile_overrides.remove(&path_key(old)) {
                self.profile_overrides.insert(path_key(new), p);
            }
            if let Ok(mut cache) = self.preloader.cache.lock() {
                cache.rename_key(old, new);
            }
            if let Some(t) = self.thumbs.remove(old) {
                self.thumbs.insert(new.clone(), t);
            }
            if let Some(shown) = self.shown.as_mut().filter(|s| same_path(&s.path, old)) {
                shown.path = new.clone();
            }
            if current
                .as_deref()
                .map(|c| same_path(c, old))
                .unwrap_or(false)
            {
                new_current = Some(new.clone());
            }
        }
        self.all_files.sort_by_key(|a| a.to_lowercase());
        self.refilter();
        if let Some(i) = new_current.and_then(|c| self.visible.iter().position(|g| *g == c)) {
            self.select_only(i);
        }
        let text = match plan.len() {
            1 => trf("Renamed to {}", &[&browse::file_name_of(&plan[0].1)]),
            n => trf("{} files renamed", &[&n]),
        };
        self.notify(text, false);
    }

    // ---- Save As ---------------------------------------------------------------------

    /// Asks where to save a copy of the current image, then its JPEG quality if needed.
    pub(super) fn save_as(&mut self) {
        let Some(source) = self.current_path() else {
            return;
        };
        let suggested = generate_next_sequential_path(Path::new(&source));
        let mut dialog = rfd::FileDialog::new()
            .set_title(tr("Save a Copy As"))
            .add_filter("JPEG", &["jpg", "jpeg"])
            .add_filter("TIFF", &["tif", "tiff"])
            .add_filter("PNG", &["png"])
            .add_filter("WebP", &["webp"]);
        if let Some(dir) = suggested.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        let Some(dest) = dialog.save_file() else {
            return;
        };
        if same_path(&dest.to_string_lossy(), &source) {
            self.notify(
                tr("Choose another name: the original is never overwritten."),
                true,
            );
            return;
        }
        if is_jpeg(&dest) {
            let last = self.settings.jpeg_quality;
            self.quality = Some(QualityDialog {
                like_original: last == 0 && is_jpeg(Path::new(&source)),
                source,
                dest,
                quality: if last == 0 { FIRST_JPEG_QUALITY } else { last },
            });
        } else {
            self.save_copy(source, dest, 0);
        }
    }

    pub(super) fn quality_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.quality else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        let source_is_jpeg = is_jpeg(Path::new(&dialog.source));
        egui::Window::new(tr("JPEG Quality"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(browse::file_name_of(&dialog.dest.to_string_lossy()));
                if source_is_jpeg {
                    ui.checkbox(
                        &mut dialog.like_original,
                        tr("Same compression as the original"),
                    )
                    .on_hover_text(tr("Reuses the tables of the original: repeated saves do not wear the image, and an unchanged image is copied without recompression."));
                }
                ui.add_enabled(
                    !(source_is_jpeg && dialog.like_original),
                    egui::Slider::new(&mut dialog.quality, 1..=100).text(tr("Quality")),
                );
                ui.label(
                    egui::RichText::new(tr("From 90 the color keeps full resolution (4:4:4)."))
                        .small()
                        .weak(),
                );
                ui.horizontal(|ui| {
                    if ui.button(tr("Save")).clicked() {
                        save = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if cancel {
            self.quality = None;
        } else if save {
            let quality = if source_is_jpeg && dialog.like_original {
                0
            } else {
                dialog.quality.clamp(1, 100)
            };
            let (source, dest) = (dialog.source.clone(), dialog.dest.clone());
            self.quality = None;
            self.settings.jpeg_quality = quality;
            self.settings.save();
            self.save_copy(source, dest, quality);
        }
    }

    fn saved(&mut self, source: String, dest: PathBuf, result: Result<(), String>) {
        let name = browse::file_name_of(&dest.to_string_lossy());
        match result {
            Ok(()) => {
                log::info!("Saved {}", dest.display());
                self.edit_saved(&source);
                self.notify(trf("Saved '{}'.", &[&name]), false);
                // A copy saved inside the open folder joins the list.
                let dest_s = crate::config::normalize_path(&dest.to_string_lossy());
                let inside = self
                    .folder
                    .as_deref()
                    .map(|root| path_key(&dest_s).starts_with(&browse::dir_prefix_key(root)))
                    .unwrap_or(false);
                if inside && !self.all_files.iter().any(|f| same_path(f, &dest_s)) {
                    self.all_files.push(dest_s);
                    self.all_files.sort_by_key(|a| a.to_lowercase());
                    self.refilter();
                }
            }
            Err(e) => self.notify(trf("'{}' not saved: {}", &[&name, &e]), true),
        }
    }

    // ---- Results ---------------------------------------------------------------------

    pub(super) fn file_result(&mut self, result: FileResult) {
        match result {
            FileResult::Rotated {
                path,
                clockwise,
                result,
            } => self.rotated(path, clockwise, result),
            FileResult::Trashed { removed, failed } => self.trashed(removed, failed),
            FileResult::Renamed { plan, result } => self.renamed(plan, result),
            FileResult::Saved {
                source,
                dest,
                result,
            } => self.saved(source, dest, result),
        }
    }

    /// Draws the notice at the bottom of the image view.
    pub(super) fn notice_overlay(&mut self, ui: &mut egui::Ui, view: egui::Rect) {
        let Some(n) = &self.notice else { return };
        let mut undo = false;
        let mut close = false;
        let fill = theme::notice(ui.visuals(), n.is_error);
        let width = (view.width() - 32.0).clamp(160.0, 560.0);
        let area = egui::Area::new(ui.id().with("notice"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(view.center_bottom() - egui::vec2(0.0, 16.0));
        area.show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(fill)
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_max_width(width);
                    ui.horizontal(|ui| {
                        let text =
                            egui::RichText::new(&n.text).color(ui.visuals().strong_text_color());
                        ui.add(egui::Label::new(text).wrap());
                        if !n.trash.is_empty() && ui.button(tr("Undo")).clicked() {
                            undo = true;
                        }
                        if ui.small_button("✕").on_hover_text(tr("Close")).clicked() {
                            close = true;
                        }
                    });
                });
        });
        if undo {
            self.undo_trash();
        } else if close {
            self.close_notice();
        }
    }

    /// Called when the window closes: a pending move to the Recycle Bin is carried out and
    /// the file operations still queued are given time to finish.
    pub fn on_exit(&mut self) {
        self.commit_trash();
        if !self.disk.flush(Duration::from_secs(10)) {
            log::warn!("Some file operations did not finish before exit");
        }
    }
}
