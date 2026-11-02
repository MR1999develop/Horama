//! Crop, the Undo/Redo history and saving of the edited images.
//!
//! A crop never touches the file: the cropped pixels stay in memory ([`HoramaApp::edited`])
//! until the user saves them (Ctrl+S writes a new file next to the original, never over it) or
//! discards them. Undo/Redo (Ctrl+Z / Ctrl+Y) covers crops, ratings, color profiles and lossless
//! rotations. Closing the window with unsaved crops asks what to do with them.

use super::i18n::{tr, trf};
use super::{HoramaApp, browse, theme};
use crate::config::{path_key, same_path};
use crate::image_loader::DecodedImage;
use crate::image_saver::{export_image, generate_next_sequential_path};
use egui::{Color32, CursorIcon, Pos2, Rect, Stroke, Vec2};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Undo entries holding full image copies (crops) kept at most.
const MAX_PIXEL_HISTORY: usize = 8;
const MAX_HISTORY: usize = 60;
/// Size of the crop handles, in points.
const HANDLE: f32 = 10.0;

/// Fixed aspect ratios of the crop (label, width / height); `None` = free.
pub(super) const CROP_RATIOS: [(&str, Option<f32>); 8] = [
    ("Free", None),
    ("1:1", Some(1.0)),
    ("3:2", Some(1.5)),
    ("2:3", Some(2.0 / 3.0)),
    ("4:3", Some(4.0 / 3.0)),
    ("3:4", Some(0.75)),
    ("16:9", Some(16.0 / 9.0)),
    ("9:16", Some(9.0 / 16.0)),
];

/// One step of the history.
pub(super) enum Action {
    Rating {
        paths: Vec<String>,
        before: Vec<u8>,
        after: u8,
    },
    Profile {
        paths: Vec<String>,
        before: Vec<Option<String>>,
        after: Option<String>,
    },
    /// Lossless rotation on disk: undone by rotating back.
    Rotate { paths: Vec<String>, clockwise: bool },
    /// Crop in memory; `None` = the pixels of the file.
    Crop {
        path: String,
        before: Option<Arc<DecodedImage>>,
        after: Option<Arc<DecodedImage>>,
    },
}

impl Action {
    fn holds_pixels(&self) -> bool {
        matches!(self, Action::Crop { .. })
    }

    /// True for a rotation of `path`.
    pub(super) fn rotates(&self, path: &str) -> bool {
        matches!(self, Action::Rotate { paths, .. } if paths.iter().any(|p| same_path(p, path)))
    }
}

/// Which part of the crop box the mouse drags.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grip {
    Move,
    /// Corner: (moves the left edge, moves the top edge).
    Corner(bool, bool),
}

/// The crop box, in image fractions (0–1).
pub(super) struct CropBox {
    pub rect: Rect,
    pub ratio: Option<f32>,
    grip: Option<Grip>,
}

/// The window shown when the program closes with unsaved crops.
pub(super) struct ClosePrompt {
    pub count: usize,
}

impl HoramaApp {
    // ---- History ---------------------------------------------------------------------

    pub(super) fn push_history(&mut self, action: Action) {
        self.redo.clear();
        self.undo.push(action);
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
        // Full image copies are heavy: only the newest crops can be undone.
        while self.undo.iter().filter(|a| a.holds_pixels()).count() > MAX_PIXEL_HISTORY {
            if let Some(i) = self.undo.iter().position(|a| a.holds_pixels()) {
                self.undo.remove(i);
            }
        }
    }

    /// Ctrl+Z: a pending move to the Recycle Bin first, then the history.
    pub(super) fn undo_last(&mut self) {
        if self.undo_trash() {
            return;
        }
        match self.undo.pop() {
            Some(a) => {
                let back = self.apply(a, true);
                self.redo.push(back);
            }
            None => self.status = tr("Nothing to undo.").to_string(),
        }
    }

    /// Ctrl+Y.
    pub(super) fn redo_last(&mut self) {
        match self.redo.pop() {
            Some(a) => {
                let back = self.apply(a, false);
                self.undo.push(back);
            }
            None => self.status = tr("Nothing to redo.").to_string(),
        }
    }

    /// Applies `a` backwards (`undo`) or forwards and returns the action that reverts that.
    fn apply(&mut self, a: Action, undo: bool) -> Action {
        match a {
            Action::Rating {
                paths,
                before,
                after,
            } => {
                for (p, b) in paths.iter().zip(&before) {
                    self.settings.set_rating(p, if undo { *b } else { after });
                }
                if self.filters.ratings_mask != 0 {
                    self.refilter();
                }
                Action::Rating {
                    paths,
                    before,
                    after,
                }
            }
            Action::Profile {
                paths,
                before,
                after,
            } => {
                for (p, b) in paths.iter().zip(&before) {
                    let value = if undo { b.clone() } else { after.clone() };
                    match value {
                        Some(v) => self.profile_overrides.insert(path_key(p), v),
                        None => self.profile_overrides.remove(&path_key(p)),
                    };
                }
                self.reload_current();
                Action::Profile {
                    paths,
                    before,
                    after,
                }
            }
            Action::Rotate { paths, clockwise } => {
                self.rotate_paths(&paths, clockwise != undo, false);
                Action::Rotate { paths, clockwise }
            }
            Action::Crop {
                path,
                before,
                after,
            } => {
                let pixels = if undo { before.clone() } else { after.clone() };
                match pixels {
                    Some(d) => self.edited.insert(path_key(&path), d),
                    None => self.edited.remove(&path_key(&path)),
                };
                self.show_path(&path);
                self.reload_current();
                Action::Crop {
                    path,
                    before,
                    after,
                }
            }
        }
    }

    /// Makes `path` the current image if it is in the list.
    fn show_path(&mut self, path: &str) {
        if let Some(i) = self.visible.iter().position(|v| same_path(v, path))
            && self.current != Some(i)
        {
            self.select_only(i);
        }
    }

    /// Shows the current image again (another profile, crop or file content).
    pub(super) fn reload_current(&mut self) {
        // The comparison previews are made again too.
        self.compare_paths.clear();
        self.compare_textures.clear();
        if let Some(path) = self.current_path() {
            self.load(path);
        }
    }

    // ---- Crop ------------------------------------------------------------------------

    /// C: starts cropping the current image, or leaves the crop without applying it.
    pub(super) fn toggle_crop(&mut self) {
        if self.crop.is_some() {
            self.crop = None;
        } else if self.shown.is_some() && !self.comparing() {
            let ratio = self.crop_ratio;
            let mut b = CropBox {
                rect: Rect::from_min_max(Pos2::new(0.05, 0.05), Pos2::new(0.95, 0.95)),
                ratio,
                grip: None,
            };
            self.fit_ratio(&mut b);
            self.crop = Some(b);
        }
    }

    /// Makes the box follow its aspect ratio, keeping its centre and staying inside.
    fn fit_ratio(&self, b: &mut CropBox) {
        let (Some(r), Some(shown)) = (b.ratio, &self.shown) else {
            return;
        };
        let (w, h) = (shown.size.0 as f32, shown.size.1 as f32);
        // Box size in fractions for the ratio, as large as the current width allows.
        let mut fw = b.rect.width();
        let mut fh = fw * w / (h * r);
        if fh > 0.9 {
            fh = 0.9;
            fw = fh * h * r / w;
        }
        b.rect = Rect::from_center_size(b.rect.center(), Vec2::new(fw, fh));
        b.rect = keep_inside(b.rect);
    }

    pub(super) fn set_crop_ratio(&mut self, ratio: Option<f32>) {
        self.crop_ratio = ratio;
        if let Some(mut b) = self.crop.take() {
            b.ratio = ratio;
            self.fit_ratio(&mut b);
            self.crop = Some(b);
        }
    }

    /// Enter: crops the current image to the box (in memory).
    pub(super) fn apply_crop(&mut self) {
        let (Some(b), Some(shown)) = (self.crop.take(), &self.shown) else {
            return;
        };
        let d = shown.decoded.clone();
        let x = (b.rect.min.x * d.width as f32).round() as u32;
        let y = (b.rect.min.y * d.height as f32).round() as u32;
        let w = (b.rect.width() * d.width as f32).round().max(1.0) as u32;
        let h = (b.rect.height() * d.height as f32).round().max(1.0) as u32;
        let cropped = Arc::new(d.crop(x, y, w, h));
        let path = shown.path.clone();
        let before = self.edited.get(&path_key(&path)).cloned();
        self.edited.insert(path_key(&path), cropped.clone());
        self.push_history(Action::Crop {
            path,
            before,
            after: Some(cropped),
        });
        self.reload_current();
    }

    /// Draws the crop box over the image and lets the mouse move it or pull its corners.
    pub(super) fn crop_overlay(&mut self, ui: &egui::Ui, response: &egui::Response, image: Rect) {
        let Some(mut b) = self.crop.take() else {
            return;
        };
        let to_screen = |p: Pos2| image.min + Vec2::new(p.x * image.width(), p.y * image.height());
        let screen = Rect::from_min_max(to_screen(b.rect.min), to_screen(b.rect.max));
        let corners = [
            (screen.left_top(), Grip::Corner(true, true)),
            (screen.right_top(), Grip::Corner(false, true)),
            (screen.left_bottom(), Grip::Corner(true, false)),
            (screen.right_bottom(), Grip::Corner(false, false)),
        ];
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let under = pointer.and_then(|p| {
            corners
                .iter()
                .find(|(c, _)| c.distance(p) <= HANDLE)
                .map(|(_, g)| *g)
                .or_else(|| screen.contains(p).then_some(Grip::Move))
        });
        if response.drag_started() {
            b.grip = under;
        }
        if response.dragged()
            && let Some(grip) = b.grip
        {
            let d = response.drag_delta();
            let delta = Vec2::new(d.x / image.width(), d.y / image.height());
            b.rect = self.drag_box(&b, grip, delta);
        }
        if response.drag_stopped() {
            b.grip = None;
        }
        match b.grip.or(under) {
            Some(Grip::Move) => ui.ctx().set_cursor_icon(CursorIcon::Move),
            Some(Grip::Corner(l, t)) if l == t => ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe),
            Some(Grip::Corner(..)) => ui.ctx().set_cursor_icon(CursorIcon::ResizeNeSw),
            None => {}
        }

        let painter = ui.painter_at(image.intersect(ui.clip_rect()));
        let shade = Color32::from_black_alpha(150);
        for r in [
            Rect::from_min_max(image.min, Pos2::new(image.max.x, screen.min.y)),
            Rect::from_min_max(Pos2::new(image.min.x, screen.max.y), image.max),
            Rect::from_min_max(
                Pos2::new(image.min.x, screen.min.y),
                Pos2::new(screen.min.x, screen.max.y),
            ),
            Rect::from_min_max(
                Pos2::new(screen.max.x, screen.min.y),
                Pos2::new(image.max.x, screen.max.y),
            ),
        ] {
            painter.rect_filled(r, 0.0, shade);
        }
        painter.rect_stroke(
            screen,
            0.0,
            Stroke::new(1.5, Color32::WHITE),
            egui::StrokeKind::Middle,
        );
        // Rule of thirds.
        for k in [1.0 / 3.0, 2.0 / 3.0] {
            let x = screen.min.x + screen.width() * k;
            let y = screen.min.y + screen.height() * k;
            let line = Stroke::new(0.5, Color32::from_white_alpha(120));
            painter.line_segment(
                [Pos2::new(x, screen.min.y), Pos2::new(x, screen.max.y)],
                line,
            );
            painter.line_segment(
                [Pos2::new(screen.min.x, y), Pos2::new(screen.max.x, y)],
                line,
            );
        }
        for (c, _) in corners {
            painter.rect_filled(
                Rect::from_center_size(c, Vec2::splat(HANDLE)),
                1.0,
                theme::accent(ui.visuals()),
            );
        }
        if let Some(shown) = &self.shown {
            let px = (
                (b.rect.width() * shown.size.0 as f32).round(),
                (b.rect.height() * shown.size.1 as f32).round(),
            );
            painter.text(
                screen.left_top() + Vec2::new(4.0, 4.0),
                egui::Align2::LEFT_TOP,
                format!("{} × {}", px.0, px.1),
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
        }
        self.crop = Some(b);
    }

    /// The box after dragging `grip` by `delta` (fractions), with its ratio kept.
    fn drag_box(&self, b: &CropBox, grip: Grip, delta: Vec2) -> Rect {
        const MIN: f32 = 0.02;
        let r = b.rect;
        match grip {
            Grip::Move => {
                let dx = delta.x.clamp(-r.min.x, 1.0 - r.max.x);
                let dy = delta.y.clamp(-r.min.y, 1.0 - r.max.y);
                r.translate(Vec2::new(dx, dy))
            }
            Grip::Corner(left, top) => {
                let mut n = r;
                if left {
                    n.min.x = (r.min.x + delta.x).clamp(0.0, r.max.x - MIN);
                } else {
                    n.max.x = (r.max.x + delta.x).clamp(r.min.x + MIN, 1.0);
                }
                if top {
                    n.min.y = (r.min.y + delta.y).clamp(0.0, r.max.y - MIN);
                } else {
                    n.max.y = (r.max.y + delta.y).clamp(r.min.y + MIN, 1.0);
                }
                if let (Some(ratio), Some(shown)) = (b.ratio, &self.shown) {
                    // The height follows the width; the fixed corner stays where it is.
                    let (w, h) = (shown.size.0 as f32, shown.size.1 as f32);
                    let mut fh = n.width() * w / (h * ratio);
                    let room = if top { r.max.y } else { 1.0 - r.min.y };
                    if fh > room {
                        fh = room;
                        let fw = fh * h * ratio / w;
                        if left {
                            n.min.x = n.max.x - fw;
                        } else {
                            n.max.x = n.min.x + fw;
                        }
                    }
                    if top {
                        n.min.y = n.max.y - fh;
                    } else {
                        n.max.y = n.min.y + fh;
                    }
                }
                n
            }
        }
    }

    /// The crop bar at the top of the image view: ratio, Apply, Cancel.
    pub(super) fn crop_bar(&mut self, ui: &mut egui::Ui, view: Rect) {
        if self.crop.is_none() {
            return;
        }
        let mut apply = false;
        let mut cancel = false;
        let mut ratio = None;
        egui::Area::new(ui.scope_id().with("crop-bar"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(view.center_top() + Vec2::new(0.0, 12.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("Crop"));
                        for (label, r) in CROP_RATIOS {
                            let on = self.crop_ratio == r;
                            if ui.selectable_label(on, tr(label)).clicked() {
                                ratio = Some(r);
                            }
                        }
                        ui.separator();
                        if ui
                            .add(super::widgets::primary_button(ui, tr("Apply")))
                            .on_hover_text("Enter")
                            .clicked()
                        {
                            apply = true;
                        }
                        if ui.button(tr("Cancel")).on_hover_text("Esc").clicked() {
                            cancel = true;
                        }
                    });
                });
            });
        if let Some(r) = ratio {
            self.set_crop_ratio(r);
        }
        if apply {
            self.apply_crop();
        } else if cancel {
            self.crop = None;
        }
    }

    // ---- Saving edits ----------------------------------------------------------------

    /// The pixels to save for `path`: the edited ones, if any.
    pub(super) fn edited_pixels(&self, path: &str) -> Option<Arc<DecodedImage>> {
        self.edited.get(&path_key(path)).cloned()
    }

    /// Ctrl+S: writes the cropped images of the selection as new files next to the
    /// originals (`name_1.jpg`…), which are never overwritten.
    pub(super) fn save_edits(&mut self, paths: Vec<String>) {
        let paths: Vec<String> = paths
            .into_iter()
            .filter(|p| self.edited.contains_key(&path_key(p)))
            .collect();
        if paths.is_empty() {
            self.notify(tr("Nothing to save: only cropped images are saved."), false);
            return;
        }
        let quality = self.settings.jpeg_quality;
        for path in paths {
            let dest = generate_next_sequential_path(Path::new(&path));
            self.save_copy(path, dest, quality);
        }
    }

    /// Writes `source` (edited pixels if any) to `dest` on the disk queue; when the copy of an
    /// edited image is written, the edit is done.
    pub(super) fn save_copy(&mut self, source: String, dest: PathBuf, jpeg_quality: u8) {
        let edited = self.edited_pixels(&source);
        let turns = self.view_turns_of(&source);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let (loader, color) = (self.preloader.loader(), self.color.clone());
        let profile = self.profile_of(&source);
        self.status = trf(
            "Saving {}…",
            &[&browse::file_name_of(&dest.to_string_lossy())],
        );
        self.disk.submit(move || {
            let is_edit = edited.is_some() || turns != 0;
            let pixels = match edited {
                Some(d) => Ok(d),
                None => loader
                    .get_or_load(&source)
                    .map(|d| super::view_rotation::turned(d, turns)),
            };
            let result = pixels.and_then(|decoded| {
                export_image(&color, &decoded, &profile, &dest, is_edit, jpeg_quality)
            });
            let _ = tx.send(super::Msg::File(super::files::FileResult::Saved {
                source,
                dest,
                result,
            }));
            ctx.request_repaint();
        });
    }

    /// After a successful save the crop of `source` is no longer pending.
    pub(super) fn edit_saved(&mut self, source: &str) {
        self.edited.remove(&path_key(source));
        self.undo
            .retain(|a| !matches!(a, Action::Crop { path, .. } if same_path(path, source)));
        self.redo
            .retain(|a| !matches!(a, Action::Crop { path, .. } if same_path(path, source)));
    }

    // ---- Closing with unsaved crops --------------------------------------------------

    /// Called every frame: a close request with unsaved crops is held back and asked about.
    pub(super) fn check_close(&mut self, ctx: &egui::Context) {
        if self.edited.is_empty() || self.closing {
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_prompt = Some(ClosePrompt {
                count: self.edited.len(),
            });
        }
    }

    pub(super) fn close_window(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &self.close_prompt else {
            return;
        };
        let count = prompt.count;
        let mut choice = None;
        super::widgets::modal(ctx, egui::Id::unique("close-prompt")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.heading(tr("Unsaved crops"));
            ui.label(trf(
                "{} cropped image(s) are not saved. Save them as new files next to the originals?",
                &[&count],
            ));
            ui.horizontal(|ui| {
                if ui
                    .add(super::widgets::primary_button(ui, tr("Save and Close")))
                    .clicked()
                {
                    choice = Some(0);
                }
                if ui
                    .add(super::widgets::danger_button(
                        ui,
                        tr("Close Without Saving"),
                    ))
                    .clicked()
                {
                    choice = Some(1);
                }
                if ui.button(tr("Cancel")).clicked() {
                    choice = Some(2);
                }
            });
        });
        match choice {
            Some(0) => {
                let paths: Vec<String> = self.edited_paths();
                self.save_edits(paths);
                self.finish_closing(ctx);
            }
            Some(1) => {
                self.edited.clear();
                self.finish_closing(ctx);
            }
            Some(2) => self.close_prompt = None,
            _ => {}
        }
    }

    fn finish_closing(&mut self, ctx: &egui::Context) {
        self.close_prompt = None;
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Files with an unsaved crop, as listed in the folder.
    fn edited_paths(&self) -> Vec<String> {
        self.all_files
            .iter()
            .filter(|f| self.edited.contains_key(&path_key(f)))
            .cloned()
            .collect()
    }
}

/// Moves `r` (fractions) back inside the image, shrinking it only if it is larger.
fn keep_inside(r: Rect) -> Rect {
    let size = Vec2::new(r.width().min(1.0), r.height().min(1.0));
    let min = Pos2::new(
        r.min.x.clamp(0.0, 1.0 - size.x),
        r.min.y.clamp(0.0, 1.0 - size.y),
    );
    Rect::from_min_size(min, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_moved_or_too_large_stays_inside_the_image() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        let r = keep_inside(Rect::from_min_size(
            Pos2::new(0.8, -0.1),
            Vec2::new(0.5, 0.3),
        ));
        assert!(close(r.min.x, 0.5) && close(r.min.y, 0.0), "{:?}", r);
        assert!(close(r.width(), 0.5) && close(r.height(), 0.3), "{:?}", r);
        let big = keep_inside(Rect::from_min_size(
            Pos2::new(-0.2, 0.0),
            Vec2::new(1.4, 0.5),
        ));
        assert!(
            close(big.min.x, 0.0) && close(big.width(), 1.0),
            "{:?}",
            big
        );
    }
}
