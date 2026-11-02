//! Colors and sizes of the egui interface, in one place.
//!
//! The theme follows the user's choice (System, Light, Dark; Dark by default, as suits an
//! image viewer). Colors that egui does not provide are derived here from the active visuals,
//! so every panel changes together.

pub use crate::config::ThemeChoice;
use egui::{Color32, Visuals};

/// Accent of the selection (the same blue as the Slint interface).
pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0x7A, 0xCC);
/// Filled rating stars.
pub const STAR: Color32 = Color32::from_rgb(0xF2, 0xC0, 0x3A);

/// The egui preference for the user's choice.
pub fn preference(choice: ThemeChoice) -> egui::ThemePreference {
    match choice {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
    }
}

/// The interface font of the system and its icon font: Segoe UI and Segoe MDL2 Assets on
/// Windows, read from the Windows fonts folder (never shipped: their licence does not allow
/// it). Elsewhere, or when they are missing, egui's own font is used and the icons fall back
/// to plain characters ([`super::symbol`]).
pub fn apply_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let mut icons = false;
    if let Some((regular, icon_font)) = system_fonts() {
        defs.font_data.insert(
            "ui".into(),
            std::sync::Arc::new(egui::FontData::from_owned(regular)),
        );
        let family = defs
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default();
        family.insert(0, "ui".into());
        if let Some(bytes) = icon_font {
            defs.font_data.insert(
                "icons".into(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            family.insert(1, "icons".into());
            icons = true;
        }
    }
    ctx.set_fonts(defs);
    super::symbol::set_icon_font(icons);
}

#[cfg(windows)]
fn system_fonts() -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let dir = std::env::var_os("WINDIR")
        .map_or_else(|| r"C:\Windows".into(), std::path::PathBuf::from)
        .join("Fonts");
    let regular = std::fs::read(dir.join("segoeui.ttf")).ok()?;
    // Windows 11 also has Segoe Fluent Icons, with other glyphs at some code points: one
    // font everywhere keeps the icons as checked.
    let icons = std::fs::read(dir.join("segmdl2.ttf")).ok();
    Some((regular, icons))
}

#[cfg(not(windows))]
fn system_fonts() -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    None
}

/// Neutral grey behind the image: dark in the dark theme, mid-light in the light one, never
/// tinted, so it does not shift how the colors of the photo are perceived.
pub fn canvas(v: &Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_gray(0x12)
    } else {
        Color32::from_gray(0xC8)
    }
}

/// Text drawn directly on the canvas (hints, placeholders).
pub fn canvas_text(v: &Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_gray(0x99)
    } else {
        Color32::from_gray(0x40)
    }
}

/// Background of a filmstrip cell, normal or under the mouse.
pub fn cell(v: &Visuals, hovered: bool) -> Color32 {
    match (v.dark_mode, hovered) {
        (true, false) => Color32::from_gray(0x24),
        (true, true) => Color32::from_gray(0x33),
        (false, false) => Color32::from_gray(0xE4),
        (false, true) => Color32::from_gray(0xD4),
    }
}

/// Placeholder of a thumbnail not made yet.
pub fn placeholder(v: &Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_gray(0x2E)
    } else {
        Color32::from_gray(0xD0)
    }
}

/// Outline of an empty star and of other faint marks.
pub fn faint(v: &Visuals) -> Color32 {
    if v.dark_mode {
        Color32::from_gray(0x70)
    } else {
        Color32::from_gray(0x80)
    }
}

/// Background of a notice: outcome or error.
pub fn notice(v: &Visuals, is_error: bool) -> Color32 {
    match (v.dark_mode, is_error) {
        (true, false) => Color32::from_gray(0x2A),
        (true, true) => Color32::from_rgb(0x5A, 0x1E, 0x1E),
        (false, false) => Color32::from_gray(0xF4),
        (false, true) => Color32::from_rgb(0xFB, 0xE0, 0xE0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an sRGB color.
    fn luminance(c: Color32) -> f32 {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn text_on_canvas_and_notices_meets_wcag_aa() {
        for v in [Visuals::dark(), Visuals::light()] {
            assert!(contrast(canvas_text(&v), canvas(&v)) >= 4.5);
            for err in [false, true] {
                assert!(contrast(v.strong_text_color(), notice(&v, err)) >= 4.5);
            }
            // Non-text marks (selection outline, empty stars) need 3:1.
            assert!(contrast(ACCENT, cell(&v, false)) >= 3.0);
            assert!(contrast(faint(&v), cell(&v, false)) >= 3.0);
        }
    }
}
