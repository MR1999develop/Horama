//! The toolbar under the menus: open, save, undo and redo, previous and next, rotation and crop, zoom (with its percentage),
//! color profile with its lock, zoom lock, info panel.

use super::i18n::tr;
use super::shortcuts::Command;
use super::{HoramaApp, symbol, view, widgets};

/// Zoom range of the percentage field, as the view allows.
const ZOOM_PERCENT: std::ops::RangeInclusive<f32> = 1.0..=3200.0;

/// The zoom levels of the menu next to the percentage, in percent.
const ZOOM_PRESETS: [u16; 12] = [800, 700, 600, 500, 400, 300, 200, 100, 75, 50, 25, 10];

impl HoramaApp {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        let has_image = self.current.is_some();
        let shown = self.shown.is_some();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            self.tool(
                ui,
                true,
                symbol::open_folder(),
                "Open Folder",
                Command::OpenFolder,
            );
            let edited = self
                .current_path()
                .is_some_and(|p| self.edited_pixels(&p).is_some());
            self.tool(ui, edited, symbol::save(), "Save Crop", Command::Save);
            ui.separator();
            let can_undo =
                !self.undo.is_empty() || self.notice.as_ref().is_some_and(|n| !n.trash.is_empty());
            self.tool(ui, can_undo, symbol::undo(), "Undo", Command::Undo);
            self.tool(
                ui,
                !self.redo.is_empty(),
                symbol::redo(),
                "Redo",
                Command::Redo,
            );
            ui.separator();
            self.tool(
                ui,
                has_image,
                symbol::previous(),
                "Previous Image",
                Command::Prev,
            );
            self.tool(ui, has_image, symbol::next(), "Next Image", Command::Next);
            ui.separator();
            for (cw, name, cmd) in [
                (false, "Rotate Left", Command::RotateCcw),
                (true, "Rotate Right", Command::RotateCw),
            ] {
                let hint = self.tool_hint(name, cmd);
                let clicked = ui
                    .add_enabled_ui(has_image, |ui| {
                        widgets::painted_icon_button(ui, hint, |p, r, c| {
                            widgets::paint_rotate(p, r, c, cw)
                        })
                    })
                    .inner
                    .clicked();
                if clicked {
                    self.run(cmd);
                }
            }
            self.tool(ui, has_image, symbol::crop(), "Crop", Command::ToggleCrop);
            ui.separator();
            self.tool(ui, shown, symbol::zoom_out(), "Zoom Out", Command::ZoomOut);
            let mut percent = self.view.zoom * 100.0;
            let field = ui.add_enabled(
                shown,
                egui::DragValue::new(&mut percent)
                    .range(ZOOM_PERCENT)
                    .speed(1.0)
                    .max_decimals(0)
                    .suffix("%"),
            );
            if field.changed() {
                self.view.zoom = view::clamp_zoom(percent / 100.0);
                self.view.fit = false;
            }
            field.on_hover_text(tr("Zoom"));
            self.zoom_presets(ui, shown);
            self.tool(ui, shown, symbol::zoom_in(), "Zoom In", Command::ZoomIn);
            self.tool(ui, shown, symbol::fit(), "Fit to Window", Command::Fit);
            self.tool(
                ui,
                shown,
                symbol::actual_size(),
                "Actual Size (100%)",
                Command::Zoom100,
            );
            let lock = if self.lock_zoom {
                symbol::locked()
            } else {
                symbol::unlocked()
            };
            if widgets::toggle_button(
                ui,
                self.lock_zoom,
                lock,
                tr("Keep the zoom when changing image"),
            )
            .clicked()
            {
                self.lock_zoom = !self.lock_zoom;
            }
            ui.separator();
            self.profile_combo(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::toggle_button(
                    ui,
                    self.show_info,
                    symbol::info(),
                    tr("Histogram and metadata (I)"),
                )
                .clicked()
                {
                    self.show_info = !self.show_info;
                }
            });
        });
    }

    /// The menu of the usual zoom levels, next to the percentage.
    fn zoom_presets(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let mut chosen = None;
        ui.add_enabled_ui(enabled, |ui| {
            let menu = ui.menu_button(symbol::chevron_down(), |ui| {
                for percent in ZOOM_PRESETS {
                    let level = f32::from(percent) / 100.0;
                    let on = !self.view.fit && (self.view.zoom - level).abs() < 0.005;
                    if ui.selectable_label(on, format!("{percent}%")).clicked() {
                        chosen = Some(level);
                        ui.close();
                    }
                }
            });
            menu.response
                .on_hover_text(tr("Zoom Levels"))
                .accessible_name(tr("Zoom Levels"));
        });
        if let Some(level) = chosen {
            self.view.zoom = view::clamp_zoom(level);
            self.view.fit = false;
        }
    }

    /// An icon button that runs `cmd`; its tooltip names the action and its key.
    fn tool(&mut self, ui: &mut egui::Ui, enabled: bool, icon: &str, name: &str, cmd: Command) {
        let hint = self.tool_hint(name, cmd);
        let clicked = ui
            .add_enabled_ui(enabled, |ui| widgets::icon_button(ui, icon, hint))
            .inner
            .clicked();
        if clicked {
            self.run(cmd);
        }
    }

    /// The tooltip of a tool: its name and key.
    fn tool_hint(&self, name: &str, cmd: Command) -> String {
        let key = self.shortcut_text(cmd);
        if key.is_empty() {
            tr(name).to_string()
        } else {
            format!("{} ({})", tr(name), key)
        }
    }

    /// The color profile of the current image, and the lock that keeps it for the next ones.
    fn profile_combo(&mut self, ui: &mut egui::Ui) {
        let current = self.current_path().map(|p| self.profile_of(&p));
        let mut chosen = None;
        ui.add_enabled_ui(current.is_some(), |ui| {
            egui::ComboBox::from_id_salt("toolbar-profile")
                .width(210.0)
                .selected_text(current.as_deref().map(tr).unwrap_or_default())
                .show_ui(ui, |ui| {
                    for profile in self.profile_choices() {
                        let on = current.as_deref() == Some(profile.as_str());
                        if ui.selectable_label(on, tr(&profile)).clicked() {
                            chosen = Some(profile);
                        }
                    }
                })
                .response
                .on_hover_text(tr("Color Profile"));
        });
        if let Some(p) = chosen {
            self.set_profile(&p);
            if self.locked_profile.is_some() {
                self.locked_profile = Some(p);
            }
        }
        let locked = self.locked_profile.is_some();
        let icon = if locked {
            symbol::locked()
        } else {
            symbol::unlocked()
        };
        if widgets::toggle_button(
            ui,
            locked,
            icon,
            tr("Keep this profile for the other images"),
        )
        .clicked()
        {
            self.locked_profile = if locked { None } else { current };
            self.reload_current();
        }
    }
}
