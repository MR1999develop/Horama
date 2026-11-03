//! The pieces every view draws the same way: titles, buttons by importance,
//! icon buttons, list items, dialog windows. Built on the tokens of [`super::theme`].

use super::theme::{self, Tokens};
use egui::{Button, Response, RichText, Sense, Stroke, Ui, WidgetInfo, accesskit::Role};

/// Title of a group of controls: semibold body text.
pub fn section_title(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.label(section_text(text))
}

/// The text of a section title, for headers drawn by egui.
pub fn section_text(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .family(theme::semibold())
        .size(theme::FONT_BODY)
}

/// The one main action of a window: accent fill.
pub fn primary_button(ui: &Ui, text: impl Into<String>) -> Button<'static> {
    let t = Tokens::current(ui);
    Button::new(RichText::new(text).color(t.on_accent))
        .fill(t.accent)
        .stroke(Stroke::NONE)
}

/// An action that destroys or discards something, in its confirmation: error fill.
pub fn danger_button(ui: &Ui, text: impl Into<String>) -> Button<'static> {
    let t = Tokens::current(ui);
    Button::new(RichText::new(text).color(t.on_accent))
        .fill(t.error)
        .stroke(Stroke::NONE)
}

/// A secondary window (Preferences, Help, About) centred on the main one the first time.
pub fn dialog(ctx: &egui::Context, title: impl Into<egui::WidgetText>) -> egui::Window<'static> {
    egui::Window::new(title)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .collapsible(false)
}

/// A modal dialog with the margins of a window.
pub fn modal(ctx: &egui::Context, id: egui::Id) -> egui::Modal {
    egui::Modal::new(id).frame(egui::Frame::window(&ctx.global_style()))
}

/// A quiet button with one icon: no border or fill until hovered. `hint` names the action
/// for the tooltip and screen readers.
pub fn icon_button(ui: &mut Ui, icon: &str, hint: impl Into<String>) -> Response {
    let hint = hint.into();
    let button = Button::new(RichText::new(icon).size(theme::FONT_BODY)).frame_when_inactive(false);
    ui.add(button).on_hover_text(&hint).accessible_name(hint)
}

/// An item of a list as wide as the list, selected and hovered as one, with the accent mark
/// of Windows 11 lists. `indent` moves the text right (tree levels).
pub fn list_item(ui: &mut Ui, selected: bool, title: RichText, indent: f32) -> Response {
    let t = Tokens::current(ui);
    let pad = egui::vec2(theme::SPACE_S, theme::SPACE_XS);
    let width = (ui.available_width() - 2.0 * pad.x - indent).max(1.0);
    let galley = egui::WidgetText::from(title.clone()).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        width,
        egui::TextStyle::Body,
    );
    let height = (galley.size().y + 2.0 * pad.y).max(ui.spacing().interact_size.y - 4.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, title.text()));
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            Some(t.selected)
        } else {
            resp.hovered().then_some(t.control_hover)
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(rect, theme::RADIUS, fill);
        }
        if selected {
            let mark = egui::Rect::from_center_size(
                egui::pos2(rect.left() + 1.5, rect.center().y),
                egui::vec2(3.0, (height * 0.5).min(16.0)),
            );
            ui.painter().rect_filled(mark, 1.5, t.accent);
        }
        let top = egui::pos2(
            rect.left() + pad.x + indent,
            rect.center().y - galley.size().y / 2.0,
        );
        ui.painter().galley(top, galley, t.text);
    }
    resp
}

/// An icon button that stays highlighted while its option is on (locks, panels).
pub fn toggle_button(ui: &mut Ui, on: bool, icon: &str, hint: impl Into<String>) -> Response {
    let t = Tokens::current(ui);
    let hint = hint.into();
    let text = RichText::new(icon)
        .size(theme::FONT_BODY)
        .color(if on { t.accent } else { t.text });
    let mut button = Button::new(text).frame_when_inactive(on);
    if on {
        button = button.fill(t.selected);
    }
    let resp = ui.add(button).on_hover_text(&hint);
    resp.widget_info(|| WidgetInfo::selected(Role::Switch, true, on, &hint));
    resp
}

/// A short mark over an image (a thumbnail, the canvas): text on a dark rounded fill, readable
/// over any picture. `anchor` says which corner of the mark is at `pos`.
pub fn badge(ui: &Ui, pos: egui::Pos2, anchor: egui::Align2, text: &str, size: f32) -> egui::Rect {
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::new(size, theme::semibold()),
        egui::Color32::WHITE,
    );
    let pad = egui::vec2(size * 0.5, size * 0.2);
    let rect = anchor.anchor_size(pos, galley.size() + 2.0 * pad);
    painter.rect_filled(rect, theme::RADIUS, egui::Color32::from_black_alpha(180));
    painter.galley(rect.min + pad, galley, egui::Color32::WHITE);
    rect
}

/// An icon button whose icon is drawn by `paint` (for icons no system font has), with the
/// size and behaviour of [`icon_button`].
pub fn painted_icon_button(
    ui: &mut Ui,
    hint: impl Into<String>,
    paint: impl FnOnce(&egui::Painter, egui::Rect, egui::Color32),
) -> Response {
    let hint = hint.into();
    let size = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), &hint));
    if ui.is_rect_visible(rect) {
        let t = Tokens::current(ui);
        if resp.hovered() && ui.is_enabled() {
            ui.painter()
                .rect_filled(rect, theme::RADIUS, t.control_hover);
        }
        let color = if ui.is_enabled() { t.text } else { t.text_weak };
        paint(ui.painter(), rect.shrink(size * 0.28), color);
    }
    resp.on_hover_text(hint)
}

/// A circular arrow, turning clockwise or counter-clockwise, inside `r`.
pub fn paint_rotate(p: &egui::Painter, r: egui::Rect, color: egui::Color32, clockwise: bool) {
    use std::f32::consts::PI;
    let c = r.center();
    let radius = r.width().min(r.height()) / 2.0;
    let dir = if clockwise { 1.0 } else { -1.0 };
    // A 270° arc open at the top, ending in an arrow head.
    let (start, end) = (-PI / 2.0 + dir * 0.6, -PI / 2.0 + dir * (0.6 + 1.5 * PI));
    let n = 24;
    let point = |a: f32| c + egui::vec2(a.cos(), a.sin()) * radius;
    let arc: Vec<egui::Pos2> = (0..=n)
        .map(|k| point(start + (end - start) * k as f32 / n as f32))
        .collect();
    p.add(egui::Shape::line(arc, Stroke::new(1.5, color)));
    let tip = point(end);
    let along = egui::vec2(-end.sin(), end.cos()) * dir;
    let out = egui::vec2(end.cos(), end.sin());
    let head = radius * 0.6;
    p.add(egui::Shape::convex_polygon(
        vec![
            tip + along * head,
            tip + out * head * 0.6,
            tip - out * head * 0.6,
        ],
        color,
        Stroke::NONE,
    ));
}
