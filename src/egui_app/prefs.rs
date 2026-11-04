//! The Preferences window (File > Preferences, Ctrl+,) and the About window (Help menu):
//! secondary windows centred on the main one, with section titles.

use super::i18n::{self, tr, trf};
use super::{HoramaApp, theme, widgets};
use crate::config::{APP_NAME, ThemeChoice};

/// Where the source code lives (shown in About).
const REPOSITORY: &str = "https://github.com/MR1999develop/Horama";

impl HoramaApp {
    pub(super) fn preferences_window(&mut self, ctx: &egui::Context) {
        if !self.show_prefs {
            return;
        }
        let mut open = true;
        let mut theme_choice = None;
        let mut language = None;
        widgets::dialog(ctx, tr("Preferences"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                widgets::section_title(ui, tr("Theme"));
                for choice in ThemeChoice::ALL {
                    if ui
                        .radio(self.settings.theme == choice, tr(choice.label()))
                        .clicked()
                    {
                        theme_choice = Some(choice);
                    }
                }
                ui.add_space(theme::SPACE_M);
                widgets::section_title(ui, tr("Language"));
                for (value, name) in i18n::CHOICES {
                    let label = if value == i18n::SYSTEM_LANGUAGE {
                        tr(name)
                    } else {
                        name
                    };
                    // No choice saved yet means English.
                    let chosen = self.settings.language.as_str();
                    let on = chosen == value || (chosen.is_empty() && value == "en");
                    if ui.radio(on, label).clicked() {
                        language = Some(value);
                    }
                }
                ui.add_space(theme::SPACE_S);
                ui.label(
                    egui::RichText::new(tr(
                        "Settings, ratings and shortcuts are saved as you change them.",
                    ))
                    .small()
                    .weak(),
                );
            });
        if let Some(choice) = theme_choice {
            self.settings.theme = choice;
            self.settings.save();
            ctx.set_theme(theme::preference(choice));
        }
        if let Some(value) = language {
            self.settings.language = value.to_string();
            self.settings.save();
            i18n::set_language(value);
        }
        if !open {
            self.show_prefs = false;
        }
    }

    pub(super) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = true;
        widgets::dialog(ctx, trf("About {}", &[&APP_NAME]))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                ui.heading(APP_NAME);
                ui.label(trf("Version {}", &[&env!("CARGO_PKG_VERSION")]));
                ui.label(tr("Color-managed RAW and image viewer."));
                ui.add_space(theme::SPACE_S);
                ui.label(egui::RichText::new("Copyright (C) 2026 MR").small().weak());
                ui.label(
                    egui::RichText::new(tr(
                        "Free software under the GNU General Public License v3.0 only (GPL-3.0-only), without any warranty.",
                    ))
                    .small()
                    .weak(),
                );
                ui.hyperlink(REPOSITORY);
            });
        if !open {
            self.show_about = false;
        }
    }
}
