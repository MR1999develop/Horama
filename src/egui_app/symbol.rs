//! The icons of the interface. On Windows they come from the system icon font Segoe MDL2
//! Assets (Windows 10 and 11), as in the rest of the system; where it is missing (Linux,
//! macOS, the tests on the CI) plain characters of egui's own font stand in, because egui
//! refuses to draw a glyph that no loaded font has.

use std::sync::atomic::{AtomicBool, Ordering};

static ICON_FONT: AtomicBool = AtomicBool::new(false);

/// Called by [`super::theme::apply`] once the icon font is (or is not) loaded.
pub(super) fn set_icon_font(loaded: bool) {
    ICON_FONT.store(loaded, Ordering::Relaxed);
}

fn pick(icon: &'static str, fallback: &'static str) -> &'static str {
    if ICON_FONT.load(Ordering::Relaxed) {
        icon
    } else {
        fallback
    }
}

/// Close a notice or a panel.
pub fn close() -> &'static str {
    pick("\u{E711}", "x")
}

/// A file that cannot be read.
pub fn warning() -> &'static str {
    pick("\u{E7BA}", "!")
}

/// An image with unsaved edits (crop).
pub fn edited() -> &'static str {
    pick("\u{E70F}", "*")
}
