//! The look of the window, in one place:
//! colour tokens for the light and dark themes, the type scale, spacing and
//! corners. The rest of the interface uses only these values.
//!
//! Colours follow Windows 11 (neutral greys, one blue accent for what can be clicked); text
//! and controls keep the WCAG contrasts checked by the tests below. The image itself sits on
//! a neutral grey canvas, never tinted, so it does not shift how the photo's colours look.

pub use crate::config::ThemeChoice;
use egui::{
    Color32, Context, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme,
    Visuals, vec2,
};

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_S: f32 = 8.0;
pub const SPACE_M: f32 = 16.0;
pub const SIDE_PANEL_WIDTH: f32 = 260.0;
/// Corners of controls, list items and thumbnails.
pub const RADIUS: u8 = 4;
/// Corners of windows, menus and popups.
pub const RADIUS_WINDOW: u8 = 8;

/// Body text, captions (hints, details), page titles.
pub const FONT_BODY: f32 = 14.0;
pub const FONT_SMALL: f32 = 12.0;
pub const FONT_TITLE: f32 = 20.0;

/// Filled rating stars (the same in both themes: they sit on the thumbnails too).
pub const STAR: Color32 = Color32::from_rgb(0xF2, 0xC0, 0x3A);

/// The semibold face: section and page titles.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// The colours of one theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    /// Window and panel background.
    pub bg: Color32,
    /// Raised surfaces: text fields, cards, windows.
    pub surface: Color32,
    pub text: Color32,
    /// Hints, details, dates.
    pub text_weak: Color32,
    /// Borders of fields and controls.
    pub stroke: Color32,
    /// Separators and the outline of cards.
    pub stroke_weak: Color32,
    /// Fill of controls at rest, and of hovered list items.
    pub control: Color32,
    pub control_hover: Color32,
    /// Clickable: links, primary button, selection, ticks.
    pub accent: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    /// Background of the selected list item.
    pub selected: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
    /// Behind the image.
    pub canvas: Color32,
    /// Text drawn on the canvas.
    pub canvas_text: Color32,
}

impl Tokens {
    pub const LIGHT: Self = Self {
        bg: Color32::from_rgb(0xF3, 0xF3, 0xF3),
        surface: Color32::from_rgb(0xFB, 0xFB, 0xFB),
        text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
        text_weak: Color32::from_rgb(0x5C, 0x5C, 0x5C),
        stroke: Color32::from_rgb(0x8A, 0x8A, 0x8A),
        stroke_weak: Color32::from_rgb(0xE0, 0xE0, 0xE0),
        control: Color32::from_rgb(0xFE, 0xFE, 0xFE),
        control_hover: Color32::from_rgb(0xEA, 0xEA, 0xEA),
        accent: Color32::from_rgb(0x00, 0x5F, 0xB8),
        on_accent: Color32::WHITE,
        selected: Color32::from_rgb(0xDD, 0xE9, 0xF5),
        error: Color32::from_rgb(0xC4, 0x2B, 0x1C),
        warning: Color32::from_rgb(0x9D, 0x5D, 0x00),
        success: Color32::from_rgb(0x0F, 0x7B, 0x0F),
        canvas: Color32::from_gray(0xC8),
        canvas_text: Color32::from_gray(0x40),
    };

    pub const DARK: Self = Self {
        bg: Color32::from_rgb(0x20, 0x20, 0x20),
        surface: Color32::from_rgb(0x2B, 0x2B, 0x2B),
        text: Color32::from_rgb(0xF5, 0xF5, 0xF5),
        text_weak: Color32::from_rgb(0xB8, 0xB8, 0xB8),
        stroke: Color32::from_rgb(0x8C, 0x8C, 0x8C),
        stroke_weak: Color32::from_rgb(0x3D, 0x3D, 0x3D),
        control: Color32::from_rgb(0x37, 0x37, 0x37),
        control_hover: Color32::from_rgb(0x42, 0x42, 0x42),
        accent: Color32::from_rgb(0x60, 0xCD, 0xFF),
        on_accent: Color32::BLACK,
        selected: Color32::from_rgb(0x1F, 0x3B, 0x52),
        error: Color32::from_rgb(0xFF, 0x99, 0xA4),
        warning: Color32::from_rgb(0xFC, 0xE1, 0x00),
        success: Color32::from_rgb(0x6C, 0xCB, 0x5F),
        canvas: Color32::from_gray(0x12),
        canvas_text: Color32::from_gray(0x99),
    };

    pub fn of(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::LIGHT,
            Theme::Dark => Self::DARK,
        }
    }

    /// The tokens of the visuals something is drawn with.
    pub fn of_visuals(v: &Visuals) -> Self {
        Self::of(if v.dark_mode {
            Theme::Dark
        } else {
            Theme::Light
        })
    }

    /// The tokens of the theme `ui` is drawn with.
    pub fn current(ui: &egui::Ui) -> Self {
        Self::of_visuals(ui.visuals())
    }

    fn visuals(self, theme: Theme) -> Visuals {
        let mut v = match theme {
            Theme::Light => Visuals::light(),
            Theme::Dark => Visuals::dark(),
        };
        let control = CornerRadius::same(RADIUS);
        v.panel_fill = self.bg;
        v.window_fill = self.surface;
        v.extreme_bg_color = self.surface;
        v.faint_bg_color = self.control_hover;
        v.text_edit_bg_color = Some(self.surface);
        v.override_text_color = None;
        v.weak_text_color = Some(self.text_weak);
        v.hyperlink_color = self.accent;
        v.error_fg_color = self.error;
        v.warn_fg_color = self.warning;
        v.selection.bg_fill = self.selected;
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.window_corner_radius = CornerRadius::same(RADIUS_WINDOW);
        v.menu_corner_radius = CornerRadius::same(RADIUS_WINDOW);
        v.window_stroke = Stroke::new(1.0, self.stroke_weak);
        let shadow = Color32::from_black_alpha(if theme == Theme::Dark { 90 } else { 40 });
        v.window_shadow = Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: shadow,
        };
        v.popup_shadow = Shadow {
            offset: [0, 4],
            blur: 12,
            spread: 0,
            color: shadow,
        };
        v.indent_has_left_vline = false;
        v.collapsing_header_frame = false;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.bg;
        w.noninteractive.weak_bg_fill = self.bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.stroke_weak);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        w.noninteractive.corner_radius = control;
        for (state, fill) in [
            (&mut w.inactive, self.control),
            (&mut w.hovered, self.control_hover),
            (&mut w.active, self.control_hover),
            (&mut w.open, self.control_hover),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, self.stroke_weak);
            state.fg_stroke = Stroke::new(1.0, self.text);
            state.corner_radius = control;
            state.expansion = 0.0;
        }
        // Fields and boxes need a visible edge (3:1) to be found.
        w.inactive.bg_stroke = Stroke::new(1.0, self.stroke);
        w.hovered.bg_stroke = Stroke::new(1.0, self.stroke);
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        v
    }
}

/// The egui preference for the user's choice.
pub fn preference(choice: ThemeChoice) -> egui::ThemePreference {
    match choice {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
    }
}

/// Applies fonts, sizes, spacing and the colours of both themes.
pub fn apply(ctx: &Context) {
    apply_fonts(ctx);
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (
                TextStyle::Small,
                FontId::new(FONT_SMALL, FontFamily::Proportional),
            ),
            (
                TextStyle::Body,
                FontId::new(FONT_BODY, FontFamily::Proportional),
            ),
            (
                TextStyle::Button,
                FontId::new(FONT_BODY, FontFamily::Proportional),
            ),
            (TextStyle::Heading, FontId::new(FONT_TITLE, semibold())),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();
        s.spacing.item_spacing = vec2(SPACE_S, SPACE_S);
        s.spacing.button_padding = vec2(12.0, 5.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.window_margin = Margin::same(16);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.indent = 18.0;
    });
    for theme in [Theme::Light, Theme::Dark] {
        ctx.set_visuals_of(theme, Tokens::of(theme).visuals(theme));
    }
}

/// The interface font of the system, its semibold face and its icon font: Segoe UI and Segoe
/// MDL2 Assets on Windows, read from the Windows fonts folder (never shipped: their licence
/// does not allow it). Elsewhere, or when they are missing, egui's own font is used in both
/// weights and the icons fall back to plain characters ([`super::symbol`]).
fn apply_fonts(ctx: &Context) {
    let mut defs = egui::FontDefinitions::default();
    let mut icons = false;
    let fallbacks = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if let Some(fonts) = system_fonts() {
        let mut add = |name: &str, bytes: Vec<u8>| {
            defs.font_data.insert(
                name.into(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
        };
        add("ui", fonts.regular);
        add("ui-semibold", fonts.semibold);
        let mut rest = fallbacks.clone();
        if let Some(bytes) = fonts.icons {
            add("icons", bytes);
            rest.insert(0, "icons".into());
            icons = true;
        }
        let with = |first: &str| {
            std::iter::once(first.to_owned())
                .chain(rest.iter().cloned())
                .collect()
        };
        defs.families.insert(FontFamily::Proportional, with("ui"));
        defs.families.insert(semibold(), with("ui-semibold"));
    } else {
        defs.families.insert(semibold(), fallbacks);
    }
    // Chinese (the interface in Chinese, and file names in any script egui's fonts lack):
    // a font of the system, tried last in every family.
    if let Some(bytes) = cjk_font() {
        defs.font_data.insert(
            "cjk".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in defs.families.values_mut() {
            family.push("cjk".into());
        }
    }
    ctx.set_fonts(defs);
    super::symbol::set_icon_font(icons);
}

struct SystemFonts {
    regular: Vec<u8>,
    semibold: Vec<u8>,
    icons: Option<Vec<u8>>,
}

#[cfg(windows)]
fn system_fonts() -> Option<SystemFonts> {
    let dir = std::env::var_os("WINDIR")
        .map_or_else(|| r"C:\Windows".into(), std::path::PathBuf::from)
        .join("Fonts");
    let regular = std::fs::read(dir.join("segoeui.ttf")).ok()?;
    let semibold = std::fs::read(dir.join("seguisb.ttf")).unwrap_or_else(|_| regular.clone());
    // Windows 11 also has Segoe Fluent Icons, with other glyphs at some code points: one
    // font everywhere keeps the icons as checked.
    let icons = std::fs::read(dir.join("segmdl2.ttf")).ok();
    Some(SystemFonts {
        regular,
        semibold,
        icons,
    })
}

#[cfg(not(windows))]
fn system_fonts() -> Option<SystemFonts> {
    None
}

/// A system font with the Chinese characters: Microsoft YaHei on Windows, PingFang on macOS,
/// Noto Sans CJK or WenQuanYi on Linux. In a collection (.ttc) the first face is used.
fn cjk_font() -> Option<Vec<u8>> {
    #[cfg(windows)]
    let candidates: Vec<std::path::PathBuf> = {
        let dir = std::env::var_os("WINDIR")
            .map_or_else(|| r"C:\Windows".into(), std::path::PathBuf::from)
            .join("Fonts");
        ["msyh.ttc", "msyh.ttf", "simsun.ttc"]
            .iter()
            .map(|name| dir.join(name))
            .collect()
    };
    #[cfg(target_os = "macos")]
    let candidates: Vec<std::path::PathBuf> = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect();
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: Vec<std::path::PathBuf> = [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect();
    candidates.iter().find_map(|path| std::fs::read(path).ok())
}

/// Behind the image.
pub fn canvas(v: &Visuals) -> Color32 {
    Tokens::of_visuals(v).canvas
}

/// Text drawn on the canvas (hints, placeholders).
pub fn canvas_text(v: &Visuals) -> Color32 {
    Tokens::of_visuals(v).canvas_text
}

/// A filmstrip or comparison cell, at rest or under the mouse.
pub fn cell(v: &Visuals, hovered: bool) -> Color32 {
    let t = Tokens::of_visuals(v);
    if hovered { t.control_hover } else { t.control }
}

/// A thumbnail not made yet.
pub fn placeholder(v: &Visuals) -> Color32 {
    Tokens::of_visuals(v).stroke_weak
}

/// The outline of an empty star.
pub fn faint(v: &Visuals) -> Color32 {
    Tokens::of_visuals(v).stroke
}

/// Selection, handles, what can be clicked.
pub fn accent(v: &Visuals) -> Color32 {
    Tokens::of_visuals(v).accent
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2 contrast ratio of two opaque colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let ch = |v: u8| {
                let s = f32::from(v) / 255.0;
                if s <= 0.040_45 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_and_controls_keep_their_contrast_in_both_themes() {
        for t in [Tokens::LIGHT, Tokens::DARK] {
            for bg in [t.bg, t.surface, t.control, t.control_hover, t.selected] {
                assert!(contrast(t.text, bg) >= 4.5, "text on {bg:?}");
                assert!(contrast(t.text_weak, bg) >= 4.5, "weak text on {bg:?}");
            }
            for bg in [t.bg, t.surface] {
                assert!(contrast(t.accent, bg) >= 4.5, "links on {bg:?}");
                assert!(contrast(t.error, bg) >= 4.5, "errors on {bg:?}");
                assert!(contrast(t.warning, bg) >= 4.5, "warnings on {bg:?}");
                assert!(contrast(t.stroke, bg) >= 3.0, "field borders on {bg:?}");
            }
            assert!(
                contrast(t.on_accent, t.accent) >= 4.5,
                "primary button text"
            );
            assert!(
                contrast(t.canvas_text, t.canvas) >= 4.5,
                "text on the canvas"
            );
            // The selection outline of a thumbnail is a non-text mark: 3:1.
            assert!(
                contrast(t.accent, t.control_hover) >= 3.0,
                "selection outline"
            );
        }
    }
}
