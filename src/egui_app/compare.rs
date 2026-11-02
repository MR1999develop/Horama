//! Side-by-side comparison: with two or more images selected (Ctrl/Shift+click in the
//! filmstrip) the image view shows up to nine of them in a grid, each color-managed with its
//! own profile. Clicking one shows it alone.
//!
//! The previews are made on a worker thread from the decoded images (RAM cache), reduced to
//! [`PREVIEW_SIDE`]; a new selection replaces the work still to do.

use super::i18n::trf;
use super::{browse, filters, theme, HoramaApp, Msg};
use crate::config::path_key;
use egui::{Align2, Color32, ColorImage, Rect, Sense, TextureOptions, Vec2};

/// Images shown at most in the grid.
pub(super) const MAX_COMPARED: usize = 9;
/// Long side of a preview, in pixels.
const PREVIEW_SIDE: u32 = 900;

impl HoramaApp {
    /// The selected files the grid shows, in list order (empty with fewer than two).
    fn compared(&self) -> Vec<String> {
        if self.selected.len() < 2 {
            return Vec::new();
        }
        self.selected
            .iter()
            .take(MAX_COMPARED)
            .filter_map(|&i| self.visible.get(i).cloned())
            .collect()
    }

    /// True when the image view shows the comparison grid.
    pub(super) fn comparing(&self) -> bool {
        self.selected.len() >= 2
    }

    /// Asks for the previews of the selection when it changed.
    pub(super) fn update_compare(&mut self) {
        let wanted = self.compared();
        if wanted == self.compare_paths {
            return;
        }
        self.compare_paths = wanted.clone();
        self.compare_generation += 1;
        let generation = self.compare_generation;
        self.compare_textures
            .retain(|p, _| wanted.iter().any(|w| w == p));
        let missing: Vec<(String, String)> = wanted
            .into_iter()
            .filter(|p| !self.compare_textures.contains_key(p))
            .map(|p| {
                let profile = self.profile_of(&p);
                (p, profile)
            })
            .collect();
        if missing.is_empty() {
            return;
        }
        let edited: Vec<_> = missing.iter().map(|(p, _)| self.edited_pixels(p)).collect();
        let (tx, ctx, color) = (self.tx.clone(), self.ctx.clone(), self.color.clone());
        let loader = self.preloader.loader();
        let latest = self.compare_latest.clone();
        latest.store(generation, std::sync::atomic::Ordering::Relaxed);
        let spawned = std::thread::Builder::new()
            .name("egui-compare".into())
            .spawn(move || {
                for ((path, profile), edited) in missing.into_iter().zip(edited) {
                    if latest.load(std::sync::atomic::Ordering::Relaxed) != generation {
                        return; // another selection replaced this one
                    }
                    let decoded = match edited {
                        Some(d) => Ok(d),
                        None => loader.get_or_load(&path),
                    };
                    let image = decoded.ok().and_then(|d| {
                        let rendered =
                            color.render(&d.rgb_pixels, &profile, d.embedded_icc.as_deref());
                        let rgb = image::RgbImage::from_raw(d.width, d.height, rendered.display)?;
                        let small = if d.width.max(d.height) > PREVIEW_SIDE {
                            let k = PREVIEW_SIDE as f32 / d.width.max(d.height) as f32;
                            let (w, h) = (
                                ((d.width as f32 * k).round() as u32).max(1),
                                ((d.height as f32 * k).round() as u32).max(1),
                            );
                            image::imageops::thumbnail(&rgb, w, h)
                        } else {
                            rgb
                        };
                        let size = [small.width() as usize, small.height() as usize];
                        Some(ColorImage::from_rgb(size, small.as_raw()))
                    });
                    if tx
                        .send(Msg::Compared {
                            generation,
                            path,
                            image,
                        })
                        .is_err()
                    {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            log::error!("Comparison previews not started: {}", e);
        }
    }

    pub(super) fn compared_preview(&mut self, path: String, image: Option<ColorImage>) {
        let texture = image.map(|img| {
            self.ctx
                .load_texture(format!("compare:{}", path), img, TextureOptions::LINEAR)
        });
        self.compare_textures.insert(path, texture);
    }

    /// The grid of the selected images.
    pub(super) fn compare_view(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let paths = self.compare_paths.clone();
        let n = paths.len().max(1);
        let cols = (n as f32).sqrt().ceil() as usize;
        let rows = n.div_ceil(cols);
        let gap = 8.0;
        let cell = Vec2::new(
            (rect.width() - gap * (cols + 1) as f32) / cols as f32,
            (rect.height() - gap * (rows + 1) as f32) / rows as f32,
        );
        let mut clicked = None;
        let visuals = ui.visuals().clone();
        for (k, path) in paths.iter().enumerate() {
            let (c, r) = (k % cols, k / cols);
            let min = rect.min
                + Vec2::new(
                    gap + c as f32 * (cell.x + gap),
                    gap + r as f32 * (cell.y + gap),
                );
            let cell_rect = Rect::from_min_size(min, cell);
            let name = browse::file_name_of(path);
            let response = ui.interact(cell_rect, ui.id().with(("compare", k)), Sense::click());
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
            let painter = ui.painter_at(cell_rect);
            painter.rect_filled(cell_rect, 4.0, theme::cell(&visuals, response.hovered()));
            let label_h = 20.0;
            let area = Rect::from_min_max(
                cell_rect.min + Vec2::splat(4.0),
                cell_rect.max - Vec2::new(4.0, label_h + 4.0),
            );
            match self.compare_textures.get(path) {
                Some(Some(tex)) => {
                    let size = tex.size_vec2();
                    let k = (area.width() / size.x).min(area.height() / size.y);
                    let img = Rect::from_center_size(area.center(), size * k);
                    let uv = Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
                    painter.image(tex.id(), img, uv, Color32::WHITE);
                }
                Some(None) => {
                    painter.text(
                        area.center(),
                        Align2::CENTER_CENTER,
                        "⚠",
                        egui::FontId::proportional(20.0),
                        theme::canvas_text(&visuals),
                    );
                }
                None => {
                    painter.rect_filled(area.shrink(16.0), 2.0, theme::placeholder(&visuals));
                }
            }
            let stars = self.settings.get_rating(path);
            let mut label = name.clone();
            if self.edited.contains_key(&path_key(path)) {
                label.push_str(" ✂");
            }
            painter.text(
                egui::Pos2::new(cell_rect.min.x + 6.0, cell_rect.max.y - label_h / 2.0 - 2.0),
                Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(13.0),
                visuals.text_color(),
            );
            if stars > 0 {
                let size = 11.0;
                let left = egui::Pos2::new(
                    cell_rect.max.x - size * 5.0 - 6.0,
                    cell_rect.max.y - label_h / 2.0 - 2.0,
                );
                filters::paint_stars(&painter, left, size, stars);
            }
            if response.clicked() {
                clicked = Some(path.clone());
            }
        }
        if self.selected.len() > MAX_COMPARED {
            ui.painter().text(
                rect.right_bottom() - Vec2::new(8.0, 8.0),
                Align2::RIGHT_BOTTOM,
                trf(
                    "{} selected, the first {} shown",
                    &[&self.selected.len(), &MAX_COMPARED],
                ),
                egui::FontId::proportional(12.0),
                theme::canvas_text(&visuals),
            );
        }
        if let Some(path) = clicked {
            if let Some(i) = self.visible.iter().position(|v| *v == path) {
                self.select_only(i);
            }
        }
    }
}
