//! Files and folders dragged from the file manager onto the window.
//!
//! Slint has no API for external drops; the winit backend delivers them as
//! `WindowEvent::DroppedFile`, one event per dropped item.

use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor};
use std::path::PathBuf;

/// Calls `on_drop` with the path of every item dropped on `window`.
pub fn on_dropped_path(window: &slint::Window, mut on_drop: impl FnMut(PathBuf) + 'static) {
    window.on_winit_window_event(move |_window, event| match event {
        WindowEvent::DroppedFile(path) => {
            on_drop(path.clone());
            EventResult::PreventDefault
        }
        _ => EventResult::Propagate,
    });
}
