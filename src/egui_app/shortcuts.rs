//! Keyboard shortcuts of the egui interface.
//!
//! The customizable ones are those of the settings (`shortcuts`, as text:
//! "Ctrl+Shift+O", "Right", "+"); View > Keyboard Shortcuts
//! changes them. A few keys are fixed: 0–5 ratings, F fit, I info panel, Ctrl+Shift+S Save As,
//! F6 next panel, Enter/Esc in the crop.

use super::HoramaApp;
use super::i18n::{tr, trf};
use crate::config::default_shortcuts;
use egui::{Key, KeyboardShortcut, Modifiers};

/// A command reachable from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Command {
    Undo,
    Redo,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    Zoom100,
    RotateCw,
    RotateCcw,
    ToggleCrop,
    Next,
    Prev,
    Forward10,
    Back10,
    CycleProfile,
    OpenShortcuts,
    Rename,
    Delete,
    Refresh,
    OpenFile,
    OpenFolder,
    Save,
    // Fixed keys.
    SaveAs,
    Print,
    Fit,
    ToggleInfo,
    NextPanel,
    Preferences,
}

/// The customizable commands: (command, settings id, English name), in the editor's order.
pub(super) const CUSTOM: [(Command, &str, &str); 21] = [
    (Command::OpenFile, "open_file", "Open File"),
    (Command::OpenFolder, "open_folder", "Open Folder"),
    (Command::Save, "save", "Save Crop"),
    (Command::Refresh, "refresh", "Refresh Folder"),
    (Command::Undo, "undo", "Undo"),
    (Command::Redo, "redo", "Redo"),
    (Command::Next, "next_image", "Next Image"),
    (Command::Prev, "prev_image", "Previous Image"),
    (Command::Forward10, "jump_forward_10", "10 Images Forward"),
    (Command::Back10, "jump_backward_10", "10 Images Back"),
    (Command::ZoomIn, "zoom_in", "Zoom In"),
    (Command::ZoomOut, "zoom_out", "Zoom Out"),
    (Command::ZoomFit, "zoom_fit", "Fit to Window"),
    (Command::Zoom100, "zoom_100", "Actual Size (100%)"),
    (Command::RotateCw, "rotate_cw", "Rotate Right"),
    (Command::RotateCcw, "rotate_ccw", "Rotate Left"),
    (Command::ToggleCrop, "toggle_crop", "Crop"),
    (Command::CycleProfile, "cycle_profile", "Next Color Profile"),
    (Command::Rename, "rename", "Rename…"),
    (Command::Delete, "delete", "Move to Recycle Bin"),
    (
        Command::OpenShortcuts,
        "open_shortcuts",
        "Keyboard Shortcuts",
    ),
];

/// Keys that are not customizable.
const FIXED: [(Command, KeyboardShortcut); 6] = [
    (
        Command::SaveAs,
        KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S),
    ),
    (
        Command::Print,
        KeyboardShortcut::new(Modifiers::COMMAND, Key::P),
    ),
    (Command::Fit, KeyboardShortcut::new(Modifiers::NONE, Key::F)),
    (
        Command::ToggleInfo,
        KeyboardShortcut::new(Modifiers::NONE, Key::I),
    ),
    (
        Command::NextPanel,
        KeyboardShortcut::new(Modifiers::NONE, Key::F6),
    ),
    (
        Command::Preferences,
        KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma),
    ),
];

/// Reads a shortcut written as in the settings ("Ctrl+Shift+O", "Right", "+").
pub(super) fn parse(text: &str) -> Option<KeyboardShortcut> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    // "Ctrl++" ends with the plus key itself.
    let (mods_part, key_part) = if raw == "+" {
        ("", "+")
    } else if let Some(prefix) = raw.strip_suffix("++") {
        (prefix, "+")
    } else {
        match raw.rsplit_once('+') {
            Some((m, k)) => (m, k),
            None => ("", raw),
        }
    };
    let mut modifiers = Modifiers::NONE;
    for m in mods_part
        .split('+')
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        match m.to_lowercase().as_str() {
            "ctrl" | "control" | "cmd" | "command" | "meta" => {
                modifiers = modifiers.plus(Modifiers::COMMAND)
            }
            "shift" => modifiers = modifiers.plus(Modifiers::SHIFT),
            "alt" | "opt" | "option" => modifiers = modifiers.plus(Modifiers::ALT),
            _ => return None,
        }
    }
    Some(KeyboardShortcut::new(
        modifiers,
        key_from_name(key_part.trim())?,
    ))
}

fn key_from_name(name: &str) -> Option<Key> {
    let key = match name.to_lowercase().as_str() {
        "+" | "plus" => Key::Plus,
        "=" | "equals" => Key::Equals,
        "-" | "minus" => Key::Minus,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "del" | "delete" => Key::Delete,
        "esc" | "escape" => Key::Escape,
        "return" | "enter" => Key::Enter,
        "pgup" | "pageup" => Key::PageUp,
        "pgdown" | "pagedown" => Key::PageDown,
        _ => return Key::from_name(name).or_else(|| Key::from_name(&name.to_uppercase())),
    };
    Some(key)
}

/// Writes a shortcut in the settings format.
pub(super) fn format(s: &KeyboardShortcut) -> String {
    let mut out = String::new();
    if s.modifiers.command || s.modifiers.ctrl {
        out.push_str("Ctrl+");
    }
    if s.modifiers.shift {
        out.push_str("Shift+");
    }
    if s.modifiers.alt {
        out.push_str("Alt+");
    }
    let key = match s.logical_key {
        Key::Plus => "+".to_string(),
        Key::Minus => "-".to_string(),
        Key::Equals => "=".to_string(),
        Key::ArrowLeft => "Left".to_string(),
        Key::ArrowRight => "Right".to_string(),
        Key::ArrowUp => "Up".to_string(),
        Key::ArrowDown => "Down".to_string(),
        k => k.name().to_string(),
    };
    out.push_str(&key);
    out
}

fn modifier_count(m: Modifiers) -> usize {
    [m.command || m.ctrl, m.shift, m.alt]
        .iter()
        .filter(|b| **b)
        .count()
}

/// The command editor: which command waits for its new key.
pub(super) struct ShortcutEditor {
    pub capturing: Option<Command>,
    pub error: Option<String>,
}

impl HoramaApp {
    /// The shortcut of a customizable command, from the settings or the default.
    pub(super) fn shortcut_of(&self, cmd: Command) -> Option<KeyboardShortcut> {
        if let Some((_, s)) = FIXED.iter().find(|(c, _)| *c == cmd) {
            return Some(*s);
        }
        let (_, id, _) = CUSTOM.iter().find(|(c, _, _)| *c == cmd)?;
        let text = self
            .settings
            .shortcuts
            .get(*id)
            .cloned()
            .or_else(|| default_shortcuts().get(*id).cloned())?;
        parse(&text)
    }

    /// Text for a menu entry ("Ctrl+O"), empty if the command has no key.
    pub(super) fn shortcut_text(&self, cmd: Command) -> String {
        self.shortcut_of(cmd)
            .map(|s| format(&s))
            .unwrap_or_default()
    }

    /// The commands whose keys were pressed in this frame (their key events are consumed).
    pub(super) fn pressed_commands(&self, ui: &egui::Ui) -> Vec<Command> {
        let mut bindings: Vec<(Command, KeyboardShortcut)> = CUSTOM
            .iter()
            .filter_map(|(c, _, _)| self.shortcut_of(*c).map(|s| (*c, s)))
            .chain(FIXED.iter().copied())
            .collect();
        // Ctrl+Shift+Z before Ctrl+Z, Ctrl+1 before 1: a key press goes to one command only.
        bindings.sort_by_key(|(_, s)| std::cmp::Reverse(modifier_count(s.modifiers)));
        ui.input_mut(|i| {
            let mut fired = Vec::new();
            for (cmd, s) in bindings {
                let plus_alias = s.logical_key == Key::Plus
                    && i.consume_shortcut(&KeyboardShortcut::new(s.modifiers, Key::Equals));
                if i.consume_shortcut(&s) || plus_alias {
                    fired.push(cmd);
                }
            }
            fired
        })
    }

    /// View > Keyboard Shortcuts: every customizable command with its key; "Change" waits
    /// for the next key combination.
    pub(super) fn shortcuts_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = &self.shortcut_editor else {
            return;
        };
        let capturing = editor.capturing;
        let error = editor.error.clone();
        // A key pressed while capturing becomes the new shortcut (Esc cancels).
        let mut captured: Option<KeyboardShortcut> = None;
        let mut cancel_capture = false;
        if let Some(_cmd) = capturing {
            ctx.input_mut(|i| {
                let found = i.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                });
                if let Some((key, mods)) = found {
                    if key == Key::Escape && !mods.any() {
                        cancel_capture = true;
                    } else {
                        captured = Some(KeyboardShortcut::new(mods, key));
                    }
                    i.events.clear();
                }
            });
        }
        let mut open = true;
        let mut change: Option<Command> = None;
        let mut reset = false;
        super::widgets::dialog(ctx, tr("Keyboard Shortcuts"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    egui::Grid::new("shortcuts").num_columns(3).striped(true).show(ui, |ui| {
                        for (cmd, _, name) in CUSTOM {
                            ui.label(tr(name));
                            if capturing == Some(cmd) {
                                ui.label(egui::RichText::new(tr("Press the new keys… (Esc cancels)")).italics());
                            } else {
                                ui.monospace(self.shortcut_text(cmd));
                            }
                            if ui.button(tr("Change")).clicked() {
                                change = Some(cmd);
                            }
                            ui.end_row();
                        }
                    });
                });
                if let Some(e) = &error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(tr(
                        "Fixed keys: 0–5 rating, F fit, I info panel, Ctrl+Shift+S save a copy, Ctrl+P print, F6 next panel.",
                    ))
                    .small()
                    .weak(),
                );
                if ui.button(tr("Reset to Defaults")).clicked() {
                    reset = true;
                }
            });
        let editor = self.shortcut_editor.as_mut().expect("checked above");
        if let Some(cmd) = change {
            editor.capturing = Some(cmd);
            editor.error = None;
        }
        if cancel_capture {
            editor.capturing = None;
        }
        if let (Some(cmd), Some(s)) = (capturing, captured) {
            editor.capturing = None;
            let taken = CUSTOM
                .iter()
                .map(|(c, _, n)| (*c, *n))
                .chain([
                    (Command::SaveAs, "Save a Copy As…"),
                    (Command::Print, "Print…"),
                    (Command::Fit, "Fit to Window"),
                    (Command::ToggleInfo, "Image Info"),
                    (Command::NextPanel, "Next Panel"),
                ])
                .find(|(c, _)| *c != cmd && self_shortcut(&self.settings, *c) == Some(s));
            match taken {
                Some((_, name)) => {
                    self.shortcut_editor.as_mut().expect("checked above").error = Some(trf(
                        "{} is already used by \"{}\".",
                        &[&format(&s), &tr(name)],
                    ));
                }
                None => {
                    if let Some((_, id, _)) = CUSTOM.iter().find(|(c, _, _)| *c == cmd) {
                        self.settings.shortcuts.insert(id.to_string(), format(&s));
                        self.settings.save();
                    }
                }
            }
        }
        if reset {
            self.settings.shortcuts = default_shortcuts();
            self.settings.save();
        }
        if !open {
            self.shortcut_editor = None;
        }
    }
}

/// The shortcut of `cmd` as the settings say (also for fixed keys), for conflict checks.
fn self_shortcut(settings: &crate::config::AppSettings, cmd: Command) -> Option<KeyboardShortcut> {
    if let Some((_, s)) = FIXED.iter().find(|(c, _)| *c == cmd) {
        return Some(*s);
    }
    let (_, id, _) = CUSTOM.iter().find(|(c, _, _)| *c == cmd)?;
    settings
        .shortcuts
        .get(*id)
        .cloned()
        .or_else(|| default_shortcuts().get(*id).cloned())
        .and_then(|t| parse(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_shortcuts_are_read_and_written_back_the_same() {
        for text in default_shortcuts().values() {
            let s = parse(text).unwrap_or_else(|| panic!("not read: {}", text));
            assert_eq!(format(&s), *text, "{}", text);
        }
        assert_eq!(
            parse("ctrl+shift+o"),
            Some(KeyboardShortcut::new(
                Modifiers::COMMAND.plus(Modifiers::SHIFT),
                Key::O
            ))
        );
        assert_eq!(parse("Ctrl++").map(|s| s.logical_key), Some(Key::Plus));
        assert_eq!(parse("Hyper+X"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn every_customizable_command_has_a_default_key() {
        let defaults = default_shortcuts();
        for (_, id, _) in CUSTOM {
            assert!(defaults.contains_key(id), "{}", id);
        }
    }
}
