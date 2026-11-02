//! Color profile of each image: the embedded one by default, or one the user chooses
//! (Image > Color Profile, or P to go to the next one).
//!
//! A chosen profile is *assigned* to an image without one, and the image is *converted* to it
//! when it has an embedded profile ([`crate::color_manager::ColorManager::render`]). Custom
//! ICC/ICM files are remembered in the settings, shared with the Slint interface.

use super::HoramaApp;
use super::i18n::{tr, trf};
use crate::color_manager::ColorManager;
use crate::config::{AppSettings, BUILTIN_PROFILES, PROFILE_EMBEDDED, path_key};
use std::path::Path;

/// Registers the custom profiles saved in the settings; files that disappeared or are no
/// longer valid profiles are dropped from the list.
pub(super) fn register_saved(color: &mut ColorManager, settings: &mut AppSettings) {
    let before = settings.custom_profiles.len();
    settings.custom_profiles.retain(|p| {
        Path::new(p).is_file() && color.register_custom_profile(Path::new(p)).is_some()
    });
    if settings.custom_profiles.len() != before {
        settings.save();
    }
}

impl HoramaApp {
    /// The profile of `path`: the one chosen for it, the locked one (toolbar), or the
    /// embedded one.
    pub(super) fn profile_of(&self, path: &str) -> String {
        self.profile_overrides
            .get(&path_key(path))
            .or(self.locked_profile.as_ref())
            .cloned()
            .unwrap_or_else(|| PROFILE_EMBEDDED.to_string())
    }

    /// Every profile the user can choose, in menu order.
    pub(super) fn profile_choices(&self) -> Vec<String> {
        let mut custom: Vec<String> = self
            .color
            .get_custom_profiles()
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        custom.sort();
        BUILTIN_PROFILES
            .iter()
            .map(|p| p.to_string())
            .chain(custom)
            .collect()
    }

    /// Gives `profile` to the selected images and shows the current one again with it.
    pub(super) fn set_profile(&mut self, profile: &str) {
        let paths = self.targets();
        let before = paths
            .iter()
            .map(|p| self.profile_overrides.get(&path_key(p)).cloned())
            .collect();
        self.push_history(super::edit::Action::Profile {
            paths: paths.clone(),
            before,
            after: (profile != PROFILE_EMBEDDED).then(|| profile.to_string()),
        });
        for path in paths {
            if profile == PROFILE_EMBEDDED {
                self.profile_overrides.remove(&path_key(&path));
            } else {
                self.profile_overrides
                    .insert(path_key(&path), profile.to_string());
            }
        }
        self.reload_current();
        if self.filters.color_profile != "ALL" {
            self.refilter();
        }
    }

    /// Moves the current image to the next profile of the list (P).
    pub(super) fn cycle_profile(&mut self) {
        let Some(path) = self.current_path() else {
            return;
        };
        let choices = self.profile_choices();
        let now = self.profile_of(&path);
        let next = choices
            .iter()
            .position(|p| *p == now)
            .map(|i| (i + 1) % choices.len())
            .unwrap_or(0);
        let profile = choices[next].clone();
        self.set_profile(&profile);
        self.status = trf("Color profile: {}", &[&tr(&profile)]);
    }

    /// Asks for an ICC/ICM file, registers it and gives it to the selected images.
    fn load_custom_profile(&mut self) {
        let Some(file) = rfd::FileDialog::new()
            .set_title(tr("Load Custom ICC/ICM Profile"))
            .add_filter("ICC", &["icc", "icm"])
            .pick_file()
        else {
            return;
        };
        match self.color.register_custom_profile(&file) {
            Some(label) => {
                let saved = file.to_string_lossy().to_string();
                if !self.settings.custom_profiles.contains(&saved) {
                    self.settings.custom_profiles.push(saved);
                    self.settings.save();
                }
                self.set_profile(&label);
            }
            None => self.notify(
                trf("'{}' is not a valid RGB ICC profile.", &[&file.display()]),
                true,
            ),
        }
    }

    /// The Color Profile submenu of the Image menu.
    pub(super) fn profile_menu(&mut self, ui: &mut egui::Ui) {
        let current = self.current_path().map(|p| self.profile_of(&p));
        ui.add_enabled_ui(current.is_some(), |ui| {
            ui.menu_button(tr("Color Profile"), |ui| {
                let mut chosen = None;
                for profile in self.profile_choices() {
                    let on = current.as_deref() == Some(profile.as_str());
                    if ui.radio(on, tr(&profile)).clicked() {
                        chosen = Some(profile);
                    }
                }
                ui.separator();
                let load = ui.button(tr("Load Custom ICC/ICM Profile…")).clicked();
                ui.label(
                    egui::RichText::new(tr(
                        "Images with an embedded profile are converted; the others get it assigned.",
                    ))
                    .small()
                    .weak(),
                );
                if let Some(p) = chosen {
                    self.set_profile(&p);
                }
                if load {
                    self.load_custom_profile();
                }
            });
        });
    }
}
