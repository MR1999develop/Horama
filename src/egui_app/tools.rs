//! Tools of the image view and of the filmstrip:
//!
//! - round 6× magnifier while the middle mouse button is held;
//! - color picker (pixel position, RGB and HSL of the file's pixels) while the right button
//!   is held on the image;
//! - context menu of a filmstrip cell, with "Open With" and the registered applications.

use super::i18n::{tr, trf};
use super::{HoramaApp, browse};
use crate::color_manager::rgb_to_hsl;
use crate::open_with::{self, OpenWithItem};
use egui::{Color32, PointerButton, Pos2, Rect, Stroke, Vec2};

/// Diameter of the magnifier, in points.
const LENS: f32 = 260.0;
/// Magnification of the lens over the image as shown.
const LENS_ZOOM: f32 = 6.0;
/// Sides of the polygon that draws the lens: enough for a smooth circle.
const LENS_SIDES: usize = 96;
/// Side of the color swatch of the picker, in points.
const SWATCH: f32 = 56.0;

/// The lens: a disc around `center` showing the texture around `uv`, `half` UV units on each
/// side. Past the edges of the image the disc is cut, instead of smearing the edge pixels: each
/// rim point is moved back inside the image, which keeps the shape convex for the fan.
fn lens_mesh(
    texture: egui::TextureId,
    center: Pos2,
    radius: f32,
    uv: Pos2,
    half: Vec2,
) -> egui::Mesh {
    let mut mesh = egui::Mesh::with_texture(texture);
    let to_screen =
        |t: Pos2| center + Vec2::new((t.x - uv.x) / half.x, (t.y - uv.y) / half.y) * radius;
    let mut vertex = |t: Pos2| {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: to_screen(t),
            uv: t,
            color: Color32::WHITE,
        });
    };
    vertex(uv);
    for k in 0..LENS_SIDES {
        let a = k as f32 / LENS_SIDES as f32 * std::f32::consts::TAU;
        let t = Pos2::new(uv.x + a.cos() * half.x, uv.y + a.sin() * half.y);
        vertex(Pos2::new(t.x.clamp(0.0, 1.0), t.y.clamp(0.0, 1.0)));
    }
    let sides = LENS_SIDES as u32;
    for k in 0..sides {
        mesh.add_triangle(0, 1 + k, 1 + (k + 1) % sides);
    }
    mesh
}

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
            let radius = LENS / 2.0;
            let half = Vec2::new(
                radius / (LENS_ZOOM * image.width()),
                radius / (LENS_ZOOM * image.height()),
            );
            painter.circle_filled(pos, radius, Color32::BLACK);
            painter.add(lens_mesh(shown.texture.id(), pos, radius, uv, half));
            painter.circle_stroke(pos, radius, Stroke::new(2.0, Color32::WHITE));
            painter.circle_stroke(
                pos,
                radius + 2.0,
                Stroke::new(1.0, Color32::from_black_alpha(160)),
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
                egui::FontId::monospace(14.0),
                ui.visuals().strong_text_color(),
            );
            const PAD: f32 = 8.0;
            let inner = Vec2::new(SWATCH + PAD + galley.size().x, SWATCH.max(galley.size().y));
            let at = pos + Vec2::new(20.0, 20.0);
            let frame = Rect::from_min_size(at, inner + Vec2::splat(2.0 * PAD));
            painter.rect_filled(frame, 6.0, ui.visuals().extreme_bg_color);
            painter.rect_stroke(
                frame,
                6.0,
                ui.visuals().window_stroke(),
                egui::StrokeKind::Inside,
            );
            let swatch = Rect::from_min_size(at + Vec2::splat(PAD), Vec2::splat(SWATCH));
            painter.rect_filled(swatch, 4.0, Color32::from_rgb(px[0], px[1], px[2]));
            painter.rect_stroke(
                swatch,
                4.0,
                ui.visuals().window_stroke(),
                egui::StrokeKind::Inside,
            );
            let text_at = Pos2::new(
                swatch.max.x + PAD,
                swatch.center().y - galley.size().y / 2.0,
            );
            painter.galley(text_at, galley, Color32::WHITE);
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
