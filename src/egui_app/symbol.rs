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

/// An outcome to read.
pub fn info() -> &'static str {
    pick("\u{E946}", "i")
}

/// A menu that opens downwards.
pub fn chevron_down() -> &'static str {
    pick("\u{E70D}", "v")
}

/// `icon` and `text` with the gap the interface uses between them.
pub fn with(icon: &str, text: impl std::fmt::Display) -> String {
    format!("{icon}  {text}")
}

/// An image with unsaved edits (crop).
pub fn edited() -> &'static str {
    pick("\u{E70F}", "*")
}

/// Open a folder.
pub fn open_folder() -> &'static str {
    pick("\u{F12B}", "Open")
}

/// Save.
pub fn save() -> &'static str {
    pick("\u{E74E}", "Save")
}

/// Undo.
pub fn undo() -> &'static str {
    pick("\u{E7A7}", "<-")
}

/// Redo.
pub fn redo() -> &'static str {
    pick("\u{E7A6}", "->")
}

/// Previous image.
pub fn previous() -> &'static str {
    pick("\u{E72B}", "<")
}

/// Next image.
pub fn next() -> &'static str {
    pick("\u{E72A}", ">")
}

/// Crop.
pub fn crop() -> &'static str {
    pick("\u{E7A8}", "Crop")
}

/// Zoom out.
pub fn zoom_out() -> &'static str {
    pick("\u{E71F}", "-")
}

/// Zoom in.
pub fn zoom_in() -> &'static str {
    pick("\u{E8A3}", "+")
}

/// Fit to the window.
pub fn fit() -> &'static str {
    pick("\u{E9A6}", "Fit")
}

/// Locked (kept for the next images).
pub fn locked() -> &'static str {
    pick("\u{E72E}", "[x]")
}

/// Not locked.
pub fn unlocked() -> &'static str {
    pick("\u{E785}", "[ ]")
}

/// Actual size (100%): the same text everywhere, no icon reads better.
pub fn actual_size() -> &'static str {
    "1:1"
}
