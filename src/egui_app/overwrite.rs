//! File > Overwrite Original: writes the image as shown (crop, color profile, rotation shown
//! only) over its own file. Never for RAW files, which Horama cannot write, nor for formats
//! it does not save; it always asks first, because the old file cannot be brought back.
//!
//! JPEGs are compressed like the original (its quantization tables), so the loss of a new
//! compression is as small as it can be.

use super::i18n::{tr, trf};
use super::{HoramaApp, browse, widgets};
use crate::config::{PROFILE_EMBEDDED, is_raw_file, path_key, same_path};
use crate::image_saver::is_jpeg;
use std::path::{Path, PathBuf};

/// Extensions Horama writes back ([`crate::image_saver::save_image_with_icc`]).
const WRITABLE: [&str; 8] = ["jpg", "jpeg", "jpe", "png", "tif", "tiff", "webp", "bmp"];

/// The question asked before overwriting.
pub(super) struct OverwritePrompt {
    path: String,
    /// The file has more than 8 bits per channel, or a profile that is not RGB: it comes back
    /// as 8-bit RGB.
    reduced: bool,
}

impl HoramaApp {
    /// Why `path` cannot be overwritten, or `None` if it can.
    pub(super) fn overwrite_blocker(&self, path: &str) -> Option<&'static str> {
        let p = Path::new(path);
        if is_raw_file(p) {
            return Some(tr("RAW files are never overwritten: save a copy instead."));
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !WRITABLE.contains(&ext.as_str()) {
            return Some(tr("Horama cannot write this format: save a copy instead."));
        }
        if !self.has_changes(path) {
            return Some(tr(
                "Nothing to save: crop, rotate or change the color profile first.",
            ));
        }
        None
    }

    /// The image on screen differs from its file: crop, profile, rotation shown only.
    fn has_changes(&self, path: &str) -> bool {
        self.edited.contains_key(&path_key(path))
            || self.view_turns_of(path) != 0
            || self.profile_of(path) != PROFILE_EMBEDDED
    }

    /// File > Overwrite Original: asks before writing over the current image.
    pub(super) fn ask_overwrite(&mut self) {
        let Some(path) = self.current_path() else {
            return;
        };
        if let Some(reason) = self.overwrite_blocker(&path) {
            self.notify(reason, false);
            return;
        }
        let reduced = self
            .shown
            .as_ref()
            .filter(|s| same_path(&s.path, &path))
            .is_some_and(|s| s.decoded.bit_depth > 8 || s.decoded.ignored_icc_note.is_some());
        self.overwrite = Some(OverwritePrompt { path, reduced });
    }

    pub(super) fn overwrite_window(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &self.overwrite else {
            return;
        };
        let name = browse::file_name_of(&prompt.path);
        let jpeg = is_jpeg(Path::new(&prompt.path));
        let reduced = prompt.reduced;
        let mut choice = None;
        widgets::modal(ctx, egui::Id::unique("overwrite-prompt")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading(tr("Overwrite the original?"));
            ui.label(trf(
                "'{}' will be replaced by the image as shown, with its crop, rotation and color profile. The original cannot be recovered.",
                &[&name],
            ));
            if jpeg {
                ui.label(
                    egui::RichText::new(tr(
                        "JPEG: compressed like the original, to lose as little as possible.",
                    ))
                    .small()
                    .weak(),
                );
            }
            if reduced {
                ui.label(
                    egui::RichText::new(tr(
                        "This file has more than 8 bits per channel or a profile that is not RGB: it will be saved as 8-bit RGB.",
                    ))
                    .color(ui.visuals().warn_fg_color),
                );
            }
            ui.add_space(super::theme::SPACE_S);
            ui.horizontal(|ui| {
                if ui
                    .add(widgets::danger_button(ui, tr("Overwrite")))
                    .clicked()
                {
                    choice = Some(Choice::Overwrite);
                }
                if ui.button(tr("Save a Copy Instead…")).clicked() {
                    choice = Some(Choice::Copy);
                }
                if ui.button(tr("Cancel")).clicked() {
                    choice = Some(Choice::Cancel);
                }
            });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            choice = Some(Choice::Cancel);
        }
        let Some(choice) = choice else { return };
        let Some(prompt) = self.overwrite.take() else {
            return;
        };
        match choice {
            // Quality 0: a JPEG keeps the compression of the original.
            Choice::Overwrite => {
                self.save_copy(prompt.path.clone(), PathBuf::from(&prompt.path), 0)
            }
            Choice::Copy => self.save_as(),
            Choice::Cancel => {}
        }
    }

    /// After the original was overwritten: what was shown on top of the file is now in it.
    pub(super) fn overwritten(&mut self, path: &str) {
        let key = path_key(path);
        self.profile_overrides.remove(&key);
        self.view_turns.remove(&key);
        // Undoing a profile change would now apply it to the new file a second time.
        self.undo.retain(|a| !a.sets_profile_of(path));
        self.redo.retain(|a| !a.sets_profile_of(path));
        self.aspects.remove(path);
        self.forget_pixels(path);
        self.notify(
            trf("'{}' overwritten.", &[&browse::file_name_of(path)]),
            false,
        );
    }
}

enum Choice {
    Overwrite,
    Copy,
    Cancel,
}
