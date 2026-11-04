//! Filters panel and ratings of the egui interface.
//!
//! The filter rules are those of [`crate::browse`]; this module only draws the controls and
//! keeps the facts the rules need (ratings from the settings, aspect ratios measured in the
//! background, color profiles of the images already decoded).

use super::i18n::{tr, trf};
use super::theme;
use super::{HoramaApp, Msg};
use crate::browse::{self, FileFacts};
use crate::config::{
    AppSettings, DEFAULT_FALLBACK_PROFILE, PROFILE_ADOBE_RGB, PROFILE_DISPLAY_P3, PROFILE_PROPHOTO,
    PROFILE_REC2020, PROFILE_SRGB,
};
use egui::{Pos2, Sense, Stroke, Vec2};
use std::collections::HashMap;
use std::path::Path;

/// Aspect-ratio choices: (filter value, label). Saved custom ratios are added after these.
const ASPECTS: [(&str, &str); 9] = [
    ("ALL", "All"),
    ("1x1", "1:1"),
    ("3x2", "3:2"),
    ("4x3", "4:3"),
    ("5x4", "5:4"),
    ("16x9", "16:9"),
    ("21x9", "21:9"),
    ("65x24", "65:24 (XPan)"),
    ("OTHER", "Other"),
];
const FORMATS: [(&str, &str); 6] = [
    ("ALL", "All"),
    ("RAW", "RAW"),
    ("JPG", "JPEG"),
    ("TIFF", "TIFF"),
    ("PNG", "PNG"),
    ("WEBP", "WebP"),
];
/// Profile filters match part of the profile name; "NONE" = no embedded profile.
const PROFILES: [(&str, &str); 7] = [
    ("ALL", "All"),
    ("NONE", "Without embedded profile"),
    ("sRGB", PROFILE_SRGB),
    ("Adobe RGB", PROFILE_ADOBE_RGB),
    ("Display P3", PROFILE_DISPLAY_P3),
    ("ProPhoto", PROFILE_PROPHOTO),
    ("2020", PROFILE_REC2020),
];

/// What the filters know about a file in the egui interface.
pub(super) struct Facts<'a> {
    pub settings: &'a AppSettings,
    /// Profiles chosen by the user, by path key.
    pub overrides: &'a HashMap<String, String>,
    /// Long side / short side measured so far (NaN = unreadable).
    pub aspects: &'a HashMap<String, f32>,
    /// Decoded images in the RAM cache, for their size and embedded profile.
    pub cache: Option<&'a crate::cache_manager::RamCache>,
    /// Unsaved crops, by path key.
    pub edited: &'a HashMap<String, std::sync::Arc<crate::image_loader::DecodedImage>>,
}

impl FileFacts for Facts<'_> {
    fn rating(&self, path: &str) -> u8 {
        self.settings.get_rating(path)
    }
    fn is_cropped(&self, path: &str) -> bool {
        self.edited.contains_key(&crate::config::path_key(path))
    }
    fn has_profile_override(&self, path: &str) -> bool {
        self.overrides.contains_key(&crate::config::path_key(path))
    }
    fn aspect_ratio(&self, path: &str) -> Option<f32> {
        if let Some(r) = self.aspects.get(path) {
            return Some(*r);
        }
        let d = self.cache?.peek(path)?;
        ratio(d.width, d.height)
    }
    fn color_profile(&self, path: &str) -> (String, bool) {
        // Known only for images already decoded.
        match self.cache.and_then(|c| c.peek(path)) {
            Some(d) => match &d.embedded_icc_name {
                Some(name) => (name.clone(), true),
                None if d.embedded_icc.is_some() => ("Embedded ICC".to_string(), true),
                None => (DEFAULT_FALLBACK_PROFILE.to_string(), false),
            },
            None => (DEFAULT_FALLBACK_PROFILE.to_string(), false),
        }
    }
}

/// Orientation-independent aspect ratio, `None` for an empty size.
fn ratio(w: u32, h: u32) -> Option<f32> {
    (w > 0 && h > 0).then(|| w.max(h) as f32 / w.min(h) as f32)
}

impl HoramaApp {
    /// Measures in the background the aspect ratio of files the filter needs but that are not
    /// known yet; the list is filtered again when the results arrive.
    pub(super) fn measure_aspects(&mut self, paths: Vec<String>) {
        if paths.is_empty() || self.aspect_job_running {
            return;
        }
        self.aspect_job_running = true;
        let generation = self.scan_generation;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.status = trf("Reading the size of {} image(s)…", &[&paths.len()]);
        let spawned = std::thread::Builder::new()
            .name("egui-aspects".into())
            .spawn(move || {
                let results = paths
                    .into_iter()
                    .map(|p| {
                        let r = crate::image_loader::probe_dimensions(Path::new(&p))
                            .and_then(|(w, h)| ratio(w, h))
                            .unwrap_or(f32::NAN);
                        (p, r)
                    })
                    .collect();
                let _ = tx.send(Msg::Aspects {
                    generation,
                    results,
                });
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.aspect_job_running = false;
            self.status = trf("Could not read the image sizes: {}", &[&e]);
        }
    }

    /// Sets the rating of the selected images (or the current one), 0–5 stars.
    pub(super) fn set_rating(&mut self, stars: u8) {
        let targets: Vec<String> = if self.selected.is_empty() {
            self.current_path().into_iter().collect()
        } else {
            self.selected
                .iter()
                .filter_map(|&i| self.visible.get(i).cloned())
                .collect()
        };
        self.rate(targets, stars);
    }

    /// Sets the rating of `targets`, 0–5 stars, as one step of Undo.
    pub(super) fn rate(&mut self, targets: Vec<String>, stars: u8) {
        if targets.is_empty() {
            return;
        }
        let before = targets
            .iter()
            .map(|p| self.settings.get_rating(p))
            .collect();
        for p in &targets {
            self.settings.set_rating(p, stars.min(5));
        }
        self.push_history(super::edit::Action::Rating {
            paths: targets.clone(),
            before,
            after: stars.min(5),
        });
        self.status = match targets.len() {
            1 => trf(
                "{}: {} star(s)",
                &[&browse::file_name_of(&targets[0]), &stars],
            ),
            n => trf("{} images: {} star(s)", &[&n, &stars]),
        };
        // A rating filter may hide or show them now.
        if self.filters.ratings_mask != 0 {
            self.refilter();
        }
    }

    /// The aspect ratios offered by the filter and by the crop, as (value, label): the fixed
    /// ones, then those the user saved. Values ("3x2") do not depend on the orientation.
    pub(super) fn aspect_ratio_choices(&self) -> Vec<(String, String)> {
        ASPECTS
            .iter()
            .filter(|(value, _)| browse::parse_aspect_ratio_str(value).is_some())
            .map(|(value, label)| (value.to_string(), label.to_string()))
            .chain(
                self.settings
                    .saved_aspect_ratios
                    .iter()
                    .map(|custom| (custom.clone(), custom.replace(['x', 'X'], ":"))),
            )
            .collect()
    }

    /// Saves a typed aspect ratio ("16:10", "16x10") among the user's ones, for the filter and
    /// the crop. Its value, or `None` if the text is not a ratio.
    pub(super) fn save_aspect_ratio(&mut self, typed: &str) -> Option<String> {
        let value = typed.trim().replace([':', '/'], "x");
        browse::parse_aspect_ratio_str(&value)?;
        let known = ASPECTS.iter().any(|(fixed, _)| *fixed == value)
            || self.settings.saved_aspect_ratios.contains(&value);
        if !known {
            self.settings.saved_aspect_ratios.push(value.clone());
            self.settings.save();
        }
        Some(value)
    }

    /// Adds a custom aspect ratio ("16:10") to the saved ones, or removes the one chosen in
    /// the filter. Saved ratios are kept in the settings. True if the filter
    /// changed.
    fn custom_ratio_row(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let custom = self
            .settings
            .saved_aspect_ratios
            .iter()
            .position(|r| *r == self.filters.aspect);
        let value = self.ratio_input.trim().replace([':', '/'], "x");
        let valid = browse::parse_aspect_ratio_str(&value).is_some();
        let (mut add, mut remove) = (false, false);
        // Right to left: the buttons keep their size at the right edge and the field takes
        // the rest, so the row never makes the panel wider.
        let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        let layout = egui::Layout::right_to_left(egui::Align::Center);
        ui.allocate_ui_with_layout(row, layout, |ui| {
            if custom.is_some() {
                remove = ui
                    .button(tr("Remove"))
                    .on_hover_text(tr("Remove this aspect ratio from the filters"))
                    .clicked();
            }
            add = ui
                .add_enabled(valid, egui::Button::new(tr("Add")))
                .on_hover_text(tr("Save this aspect ratio among the filters"))
                .clicked();
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.ratio_input)
                    .hint_text(tr("e.g. 16:10"))
                    .desired_width(ui.available_width()),
            );
            add |= valid && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        if add && let Some(value) = self.save_aspect_ratio(&value) {
            self.filters.aspect = value;
            self.ratio_input.clear();
            changed = true;
        }
        if let Some(i) = custom
            && remove
        {
            self.settings.saved_aspect_ratios.remove(i);
            self.settings.save();
            self.filters.aspect = "ALL".to_string();
            changed = true;
        }
        changed
    }

    pub(super) fn filters_panel(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        // Unindented: the content lines up with the rest of the panel.
        egui::CollapsingHeader::new(super::widgets::section_text(tr("Filters")))
            .id_salt("filters")
            .default_open(false)
            .show_unindented(ui, |ui| {
                field_label(ui, tr("Rating (stars)"));
                ui.horizontal_wrapped(|ui| {
                    // Compact, so the six choices fit on one row of the default panel.
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.spacing_mut().button_padding.x = 6.0;
                    for stars in 0..=5u8 {
                        let bit = 1u8 << stars;
                        let on = self.filters.ratings_mask & bit != 0;
                        let text = if stars == 0 {
                            tr("Unrated").to_string()
                        } else {
                            stars.to_string()
                        };
                        if ui.selectable_label(on, text).clicked() {
                            self.filters.ratings_mask ^= bit;
                            changed = true;
                        }
                    }
                });
                changed |= combo(ui, tr("Format"), &mut self.filters.format, &FORMATS);
                let mut aspects = vec![("ALL".to_string(), "All".to_string())];
                aspects.extend(self.aspect_ratio_choices());
                aspects.push(("OTHER".to_string(), "Other".to_string()));
                changed |= combo_owned(ui, tr("Aspect ratio"), &mut self.filters.aspect, &aspects);
                changed |= self.custom_ratio_row(ui);
                changed |= combo(
                    ui,
                    tr("Color profile"),
                    &mut self.filters.color_profile,
                    &PROFILES,
                );
                ui.add_space(theme::SPACE_XS);
                let any = self.filters.ratings_mask != 0
                    || self.filters.format != "ALL"
                    || self.filters.aspect != "ALL"
                    || self.filters.color_profile != "ALL"
                    || !self.filters.search.is_empty();
                if ui
                    .add_enabled(any, egui::Button::new(tr("Clear filters")))
                    .clicked()
                {
                    let tree = std::mem::take(&mut self.filters.tree);
                    self.filters = browse::Filters {
                        tree,
                        ..browse::Filters::default()
                    };
                    changed = true;
                }
                if self.filters.color_profile != "ALL" {
                    ui.label(
                        egui::RichText::new(tr(
                            "The profile is known only for images already opened.",
                        ))
                        .small()
                        .weak(),
                    );
                }
            });
        if changed {
            self.refilter();
        }
    }
}

/// The name of a filter, above its control.
fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(theme::SPACE_XS);
    ui.label(egui::RichText::new(text).weak());
}

fn combo(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[(&str, &str)]) -> bool {
    let owned: Vec<(String, String)> = options
        .iter()
        .map(|(v, l)| (v.to_string(), l.to_string()))
        .collect();
    combo_owned(ui, label, value, &owned)
}

fn combo_owned(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    options: &[(String, String)],
) -> bool {
    let shown = options
        .iter()
        .find(|(v, _)| v == value)
        .map(|(_, l)| tr(l).to_string())
        .unwrap_or_else(|| value.clone());
    let mut changed = false;
    field_label(ui, label);
    egui::ComboBox::from_id_salt(label)
        .selected_text(shown)
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for (v, l) in options {
                if ui.selectable_label(value == v, tr(l)).clicked() && value != v {
                    *value = v.clone();
                    changed = true;
                }
            }
        })
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, label));
    changed
}

/// Draws `stars` filled stars out of five, `size` points each, starting at `left_center`.
pub(super) fn paint_stars(painter: &egui::Painter, left_center: Pos2, size: f32, stars: u8) {
    for i in 0..5u8 {
        let c = left_center + Vec2::new(size * (i as f32 + 0.5), 0.0);
        let points: Vec<Pos2> = (0..10)
            .map(|k| {
                let r = if k % 2 == 0 { size * 0.48 } else { size * 0.2 };
                let a = std::f32::consts::PI * (k as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
                c + Vec2::new(r * a.cos(), r * a.sin())
            })
            .collect();
        if i < stars {
            // Concave shape: drawn as a fan of triangles around the centre.
            let mut mesh = egui::Mesh::default();
            let centre = mesh.vertices.len() as u32;
            mesh.colored_vertex(c, theme::STAR);
            for p in &points {
                mesh.colored_vertex(*p, theme::STAR);
            }
            for k in 0..10u32 {
                mesh.add_triangle(centre, centre + 1 + k, centre + 1 + (k + 1) % 10);
            }
            painter.add(egui::Shape::mesh(mesh));
        } else {
            let mut closed = points.clone();
            closed.push(points[0]);
            painter.add(egui::Shape::line(
                closed,
                Stroke::new(1.0, theme::faint(&painter.ctx().global_style().visuals)),
            ));
        }
    }
}

/// Clickable row of five stars: returns the clicked rating (clicking the current rating
/// clears it).
pub(super) fn star_picker(ui: &mut egui::Ui, current: u8) -> Option<u8> {
    let size = 16.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(size * 5.0, size), Sense::click());
    let hover = response
        .hover_pos()
        .map(|p| star_under(rect.min.x, size, p));
    let shown = hover.unwrap_or(current);
    paint_stars(ui.painter(), rect.left_center(), size, shown);
    if response.hovered() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            3.0,
            Stroke::new(1.0, theme::accent(ui.visuals())),
            egui::StrokeKind::Outside,
        );
    }
    let clicked = response.clicked().then_some(hover).flatten();
    // Screen readers hear "Rating, 3 of 5" instead of an unnamed drawing.
    response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(current), tr("Rating")));
    response.on_hover_text(tr("Rating (keys 0–5)"));
    clicked.map(|s| if s == current { 0 } else { s })
}

/// The star (1–5) under `pos` in a row of five stars of `size` starting at `left`.
pub(super) fn star_under(left: f32, size: f32, pos: Pos2) -> u8 {
    (((pos.x - left) / size).floor() as i32 + 1).clamp(1, 5) as u8
}
