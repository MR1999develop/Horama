//! Tools of the image view and of the filmstrip:
//!
//! - 4× magnifier while the middle mouse button is held;
//! - color picker (pixel position, RGB and HSL of the file's pixels) while the right button
//!   is held on the image;
//! - context menu of a filmstrip cell, with "Open With" and the registered applications.

use super::i18n::{tr, trf};
use super::{HoramaApp, browse};
use crate::color_manager::rgb_to_hsl;
use crate::open_with::{self, OpenWithItem};
use egui::{Color32, PointerButton, Pos2, Rect, Stroke, Vec2};

/// Side of the magnifier, in points.
const LENS: f32 = 220.0;
const LENS_ZOOM: f32 = 4.0;

/// What the context menu of a filmstrip cell asked for.
pub(super) enum CellAction {
    OpenWith(i32),
    ChooseApp,
    Rename,
    Trash,
    Rotate(bool),
    ShowInFolder,
    Rating(u8),
}

impl HoramaApp {
    /// Magnifier (middle button) and color picker (right button) over the image.
    pub(super) fn image_tools(&self, ui: &egui::Ui, image: Rect) {
        let Some(shown) = &self.shown else { return };
        let (pos, middle, right) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_down(PointerButton::Middle),
                i.pointer.button_down(PointerButton::Secondary),
            )
        });
        let Some(pos) = pos.filter(|p| image.contains(*p)) else {
            return;
        };
        let uv = Pos2::new(
            (pos.x - image.min.x) / image.width(),
            (pos.y - image.min.y) / image.height(),
        );
        let painter = ui.painter();
        if middle {
            let half = Vec2::new(
                LENS / (2.0 * LENS_ZOOM * image.width()),
                LENS / (2.0 * LENS_ZOOM * image.height()),
            );
            let lens = Rect::from_center_size(pos, Vec2::splat(LENS));
            painter.rect_filled(lens, 0.0, Color32::BLACK);
            painter.image(
                shown.texture.id(),
                lens,
                Rect::from_min_max(uv - half, uv + half),
                Color32::WHITE,
            );
            painter.rect_stroke(
                lens,
                0.0,
                Stroke::new(2.0, Color32::WHITE),
                egui::StrokeKind::Outside,
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        } else if right {
            let d = &shown.decoded;
            let x = ((uv.x * d.width as f32) as u32).min(d.width.saturating_sub(1));
            let y = ((uv.y * d.height as f32) as u32).min(d.height.saturating_sub(1));
            let i = (y as usize * d.width as usize + x as usize) * 3;
            let Some(px) = d.rgb_pixels.get(i..i + 3) else {
                return;
            };
            let (h, s, l) = rgb_to_hsl(px[0], px[1], px[2]);
            let text = format!(
                "{} {}, {}\nRGB {} {} {}\nHSL {}° {}% {}%",
                tr("Pixel"),
                x,
                y,
                px[0],
                px[1],
                px[2],
                h,
                s,
                l
            );
            let galley = painter.layout_no_wrap(
                text,
                egui::FontId::monospace(12.0),
                ui.visuals().strong_text_color(),
            );
            let at = pos + Vec2::new(18.0, 18.0);
            let frame = Rect::from_min_size(at, galley.size() + Vec2::new(36.0, 12.0));
            painter.rect_filled(frame, 4.0, ui.visuals().extreme_bg_color);
            painter.rect_stroke(
                frame,
                4.0,
                ui.visuals().window_stroke(),
                egui::StrokeKind::Inside,
            );
            let swatch = Rect::from_min_size(at + Vec2::new(6.0, 6.0), Vec2::splat(18.0));
            painter.rect_filled(swatch, 2.0, Color32::from_rgb(px[0], px[1], px[2]));
            painter.galley(at + Vec2::new(30.0, 6.0), galley, Color32::WHITE);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }

    /// Applications registered for `path`, read once per file (it asks the system).
    pub(super) fn open_with_handlers(&mut self, path: &str) -> &[OpenWithItem] {
        if self
            .open_with
            .as_ref()
            .map(|(p, _)| p != path)
            .unwrap_or(true)
        {
            self.open_with = Some((path.to_string(), open_with::get_handlers_for_file(path)));
        }
        &self.open_with.as_ref().expect("filled above").1
    }

    /// The context menu of a filmstrip cell; returns what was chosen.
    pub(super) fn cell_menu(
        ui: &mut egui::Ui,
        handlers: &[OpenWithItem],
        has_handlers_for: bool,
    ) -> Option<CellAction> {
        let mut action = None;
        ui.menu_button(tr("Open With"), |ui| {
            if !has_handlers_for {
                ui.label(egui::RichText::new(tr("Loading…")).weak());
            }
            for h in handlers {
                if ui.button(&h.name).clicked() {
                    action = Some(CellAction::OpenWith(h.id));
                }
            }
            ui.separator();
            if ui.button(tr("Choose Another App…")).clicked() {
                action = Some(CellAction::ChooseApp);
            }
        });
        if ui.button(tr("Show in Folder")).clicked() {
            action = Some(CellAction::ShowInFolder);
        }
        ui.separator();
        ui.menu_button(tr("Rating"), |ui| {
            for stars in 0..=5u8 {
                let label = if stars == 0 {
                    tr("Unrated").to_string()
                } else {
                    trf("{} star(s)", &[&stars])
                };
                if ui.button(label).clicked() {
                    action = Some(CellAction::Rating(stars));
                }
            }
        });
        if ui.button(tr("Rotate Right")).clicked() {
            action = Some(CellAction::Rotate(true));
        }
        if ui.button(tr("Rotate Left")).clicked() {
            action = Some(CellAction::Rotate(false));
        }
        ui.separator();
        if ui.button(tr("Rename…")).clicked() {
            action = Some(CellAction::Rename);
        }
        if ui.button(tr("Move to Recycle Bin")).clicked() {
            action = Some(CellAction::Trash);
        }
        action
    }

    /// Carries out the action chosen in the context menu of the cell of `path`.
    pub(super) fn cell_action(&mut self, path: &str, action: CellAction) {
        match action {
            CellAction::OpenWith(id) => {
                if let Err(e) = open_with::launch_handler(path, id) {
                    self.notify(
                        trf("'{}' not opened: {}", &[&browse::file_name_of(path), &e]),
                        true,
                    );
                }
            }
            CellAction::ChooseApp => open_with::launch_open_with_dialog(path),
            CellAction::Rename => self.start_rename(),
            CellAction::Trash => self.trash_selected(),
            CellAction::Rotate(cw) => self.rotate(cw),
            CellAction::ShowInFolder => open_with::reveal_in_file_manager(path),
            CellAction::Rating(stars) => self.set_rating(stars),
        }
    }
}
