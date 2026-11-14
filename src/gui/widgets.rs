//! Small, consistent building blocks for the AeternaVault interface.

use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Galley, Layout,
    Margin, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetText, text::LayoutJob,
    text::TextWrapping,
};
use std::sync::Arc;

use super::theme::{Palette, SANS_STRONG, SERIF, palette};

/// Width of the content column. Wide screens get some air on both sides.
pub const COLUMN: f32 = 1640.0;

/// A quiet panel with a hairline border.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(30, 26))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Letter-spaced small capitals with a short gold rule, like a museum label.
pub fn section_title(ui: &mut Ui, text: &str) {
    let p = palette(ui);
    let mut job = LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: FontId::new(12.5, FontFamily::Name(SANS_STRONG.into())),
            extra_letter_spacing: 1.4,
            color: p.text_secondary,
            ..Default::default()
        },
    );
    ui.label(job);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.accent);
    ui.add_space(4.0);
}

pub fn title(ui: &mut Ui, text: &str) {
    let p = palette(ui);
    ui.label(
        egui::RichText::new(text)
            .family(FontFamily::Name(SERIF.into()))
            .size(30.0)
            .color(p.text),
    );
}

pub fn secondary_text(ui: &mut Ui, text: impl Into<String>) -> Response {
    let p = palette(ui);
    ui.label(
        egui::RichText::new(text.into())
            .size(14.0)
            .color(p.text_secondary),
    )
}

pub fn strong_text(ui: &mut Ui, text: impl Into<String>) -> Response {
    let p = palette(ui);
    ui.label(
        egui::RichText::new(text.into())
            .family(FontFamily::Name(SANS_STRONG.into()))
            .color(p.text),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Quiet,
}

/// Custom-painted button: gold for the one main action, outlined otherwise.
pub fn button(ui: &mut Ui, kind: ButtonKind, text: &str, enabled: bool) -> Response {
    let p = *palette(ui);
    let font = FontId::new(16.0, FontFamily::Name(SANS_STRONG.into()));
    let galley = WidgetText::from(egui::RichText::new(text).font(font)).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let padding = match kind {
        ButtonKind::Quiet => egui::vec2(10.0, 6.0),
        _ => egui::vec2(18.0, 8.0),
    };
    let mut size = galley.size() + 2.0 * padding;
    size.y = size.y.max(if kind == ButtonKind::Quiet {
        32.0
    } else {
        38.0
    });
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);

    if ui.is_rect_visible(rect) {
        let hovered = enabled && response.hovered();
        let pressed = enabled && response.is_pointer_button_down_on();
        let painter = ui.painter();
        let (fill, stroke, text_color) = match kind {
            ButtonKind::Primary => {
                let fill = if pressed {
                    p.accent
                } else if hovered {
                    p.accent_hover
                } else {
                    p.accent
                };
                (fill, Stroke::NONE, p.on_accent)
            }
            ButtonKind::Secondary => (
                if hovered { p.raised } else { p.panel },
                Stroke::new(1.0, if hovered { p.accent } else { p.border }),
                p.text,
            ),
            ButtonKind::Quiet => (
                if hovered {
                    p.raised
                } else {
                    Color32::TRANSPARENT
                },
                Stroke::NONE,
                if hovered { p.text } else { p.text_secondary },
            ),
        };
        let alpha = if enabled { 1.0 } else { 0.45 };
        painter.rect(
            rect,
            CornerRadius::same(4),
            fill.gamma_multiply(alpha),
            Stroke::new(stroke.width, stroke.color.gamma_multiply(alpha)),
            egui::StrokeKind::Inside,
        );
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, text_color.gamma_multiply(alpha));
    }

    // Custom-painted widgets describe themselves for screen readers (AccessKit).
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Navigation entry with a gold underline when selected.
/// A one-line text field with room around the text, as high as buttons.
pub fn text_field(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(Margin::symmetric(10, 7))
}

/// A multi-line text field with room around the text.
pub fn text_area(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::multiline(text).margin(Margin::symmetric(10, 8))
}

/// A small "i" after a label: the explanation shows on hover.
pub fn hint_icon(ui: &mut Ui, text: &str) {
    let p = *palette(ui);
    let size = 15.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    let color = if response.hovered() {
        p.accent
    } else {
        p.text_secondary
    };
    ui.painter()
        .circle_stroke(rect.center(), size / 2.0 - 1.0, Stroke::new(1.1, color));
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "i",
        FontId::proportional(10.5),
        color,
    );
    response.on_hover_text(text);
}

/// An entry of a side list (settings sections): the whole width is
/// clickable, the chosen one is filled with an accent bar on the left.
pub fn side_tab(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let p = *palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 38.0), Sense::click());
    let t = ui
        .ctx()
        .animate_bool_with_time(response.id, selected || response.hovered(), 0.12);
    if t > 0.0 {
        let fill = if selected {
            p.raised
        } else {
            p.raised.gamma_multiply(0.6)
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), fill.gamma_multiply(t));
    }
    if selected {
        let bar = Rect::from_min_size(
            rect.left_top() + Vec2::new(0.0, 9.0),
            Vec2::new(3.0, rect.height() - 18.0),
        );
        ui.painter()
            .rect_filled(bar, CornerRadius::same(2), p.accent);
    }
    let font = FontId::new(15.0, FontFamily::Proportional);
    let color = if selected { p.text } else { p.text_secondary };
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let pos = Pos2::new(rect.left() + 16.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, text)
    });
    response
}

pub fn nav_tab(ui: &mut Ui, text: &str, selected: bool, enabled: bool) -> Response {
    let p = *palette(ui);
    let font = FontId::new(16.5, FontFamily::Name(SANS_STRONG.into()));
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, p.text);
    let size = egui::vec2(galley.size().x + 4.0, galley.size().y + 14.0);
    let (rect, response) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let color = if selected || (enabled && response.hovered()) {
        p.text
    } else {
        p.text_secondary
    };
    let text_pos = Pos2::new(rect.left() + 2.0, rect.top() + 3.0);
    ui.painter().galley(
        text_pos,
        galley,
        color.gamma_multiply(if enabled { 1.0 } else { 0.5 }),
    );
    if selected {
        let y = rect.bottom() - 2.0;
        ui.painter().line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(2.0, p.accent),
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, selected, text)
    });
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Draws an on/off switch into `rect`: green when on, grey when off, the
/// knob sliding between the two.
fn paint_switch(ui: &Ui, rect: Rect, id: egui::Id, on: bool, enabled: bool, hovered: bool) {
    let p = *palette(ui);
    let t = ui.ctx().animate_bool_with_time(id, on, 0.14);
    // Off: a recessed track that shows on cards and on raised frames alike.
    let off_track = p.background;
    let on_track = p.success;
    let track = lerp_color(off_track, on_track, t);
    let alpha = if enabled { 1.0 } else { 0.55 };
    let border = if on {
        p.success
    } else if hovered && enabled {
        p.text_secondary
    } else {
        p.border
    };
    let radius = (rect.height() / 2.0) as u8;
    ui.painter().rect(
        rect,
        CornerRadius::same(radius),
        track.gamma_multiply(alpha),
        Stroke::new(1.0, border.gamma_multiply(alpha)),
        egui::StrokeKind::Inside,
    );
    let r = rect.height() / 2.0 - 3.0;
    let x = egui::lerp((rect.left() + r + 3.0)..=(rect.right() - r - 3.0), t);
    let knob = if on { Color32::WHITE } else { p.text_secondary };
    ui.painter()
        .circle_filled(Pos2::new(x, rect.center().y), r, knob.gamma_multiply(alpha));
}

/// An on/off switch with an accessible `label` (not drawn). Returns `true`
/// when it was flipped.
pub fn toggle(ui: &mut Ui, on: &mut bool, enabled: bool, label: &str) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0, 22.0), sense);
    paint_switch(ui, rect, response.id, *on, enabled, response.hovered());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *on, label)
    });
    if enabled && response.clicked() {
        *on = !*on;
        return true;
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
    false
}

/// A setting that is on or off: a switch followed by its label; the label
/// can be clicked too. Used for settings instead of check boxes.
pub fn switch(ui: &mut Ui, on: &mut bool, label: impl Into<WidgetText>) -> Response {
    let p = *palette(ui);
    let enabled = ui.is_enabled();
    let galley = label.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        (ui.available_width() - 52.0).max(80.0),
        egui::TextStyle::Body,
    );
    let size = Vec2::new(40.0 + 12.0 + galley.size().x, galley.size().y.max(22.0));
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    let track = Rect::from_min_size(
        Pos2::new(rect.left(), rect.top() + (rect.height() - 22.0) / 2.0),
        Vec2::new(40.0, 22.0),
    );
    paint_switch(ui, track, response.id, *on, enabled, response.hovered());
    let text_pos = Pos2::new(
        track.right() + 12.0,
        rect.center().y - galley.size().y / 2.0,
    );
    let text = galley.text().to_string();
    ui.painter().galley(
        text_pos,
        galley,
        if enabled { p.text } else { p.text_secondary },
    );
    if enabled && response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *on, &text)
    });
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Small eye symbol next to password fields; crossed out while the text is hidden.
pub fn eye_button(ui: &mut Ui, visible: bool, label: &str) -> Response {
    let p = *palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(34.0, 28.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), p.raised);
    }
    let color = if hovered || visible {
        p.text
    } else {
        p.text_secondary
    };
    let stroke = Stroke::new(1.3, color);
    let c = rect.center();
    let (w, h) = (8.5, 5.0);
    // Almond outline from two arcs.
    let arc = |sign: f32| -> Vec<Pos2> {
        (0..=16)
            .map(|i| {
                let x = -w + 2.0 * w * i as f32 / 16.0;
                let y = sign * h * (1.0 - (x / w).powi(2));
                c + egui::vec2(x, y)
            })
            .collect()
    };
    let painter = ui.painter();
    painter.add(egui::Shape::line(arc(1.0), stroke));
    painter.add(egui::Shape::line(arc(-1.0), stroke));
    painter.circle_stroke(c, 2.4, stroke);
    if !visible {
        painter.line_segment(
            [c + egui::vec2(-7.0, 6.0), c + egui::vec2(7.0, -6.0)],
            stroke,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response
        .on_hover_text(label)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// A small padlock centred at `center` (encrypted items).
pub fn lock_icon(ui: &Ui, center: Pos2, color: Color32) {
    let painter = ui.painter();
    let stroke = Stroke::new(1.3, color);
    let body = Rect::from_center_size(center + egui::vec2(0.0, 2.0), egui::vec2(10.0, 7.5));
    painter.rect_filled(body, CornerRadius::same(2), color);
    let top = center + egui::vec2(0.0, -1.5);
    let arc: Vec<Pos2> = (0..=12)
        .map(|i| {
            let a = std::f32::consts::PI + i as f32 * std::f32::consts::PI / 12.0;
            top + egui::vec2(a.cos() * 3.2, a.sin() * 3.6)
        })
        .collect();
    painter.add(egui::Shape::line(arc, stroke));
}

/// A clickable padlock for marking items as encrypted. Returns the response.
pub fn lock_toggle(
    ui: &mut Ui,
    state: crate::engine::selection::CheckState,
    label: &str,
) -> Response {
    use crate::engine::selection::CheckState;
    let p = *palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(3), p.raised);
    }
    let color = match state {
        CheckState::Checked => p.accent,
        CheckState::Mixed => p.accent.gamma_multiply(0.55),
        CheckState::Unchecked if hovered => p.text_secondary,
        CheckState::Unchecked => p.border,
    };
    lock_icon(ui, rect.center(), color);
    let marked = state == CheckState::Checked;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, marked, label)
    });
    response
        .on_hover_text(label)
        .on_hover_cursor(CursorIcon::PointingHand)
}

pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 16.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// A thin, calm progress line. `None` shows a slowly travelling segment.
pub fn progress_line(ui: &mut Ui, fraction: Option<f32>) {
    let p = *palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 4.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), p.raised);
    match fraction {
        Some(f) => {
            let mut filled = rect;
            filled.set_width(rect.width() * f.clamp(0.0, 1.0));
            painter.rect_filled(filled, CornerRadius::same(2), p.accent);
        }
        None => {
            let t = ui.input(|i| i.time) as f32;
            let phase = (t / 2.6).fract();
            let segment = rect.width() * 0.22;
            let x = rect.left() - segment + (rect.width() + segment) * phase;
            let bar = Rect::from_min_max(
                Pos2::new(x.max(rect.left()), rect.top()),
                Pos2::new((x + segment).min(rect.right()), rect.bottom()),
            );
            painter.rect_filled(bar, CornerRadius::same(2), p.accent);
            ui.ctx().request_repaint();
        }
    }
}

/// Lays out a single line that is shortened with "…" to fit `width`.
pub fn truncated(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(10.0));
    ui.painter().layout_job(job)
}

/// Paints a truncated line of text into `rect`.
pub fn paint_cell(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32, align: Align) {
    let galley = truncated(ui, text, font, color, rect.width());
    let y = rect.center().y - galley.size().y / 2.0;
    let x = match align {
        Align::Max => rect.right() - galley.size().x,
        Align::Center => rect.center().x - galley.size().x / 2.0,
        Align::Min => rect.left(),
    };
    ui.painter()
        .with_clip_rect(rect)
        .galley(Pos2::new(x, y), galley, color);
}

/// Stretches its contents to the full width and centers a column of at most `max_width`.
pub fn centered_column<R>(ui: &mut Ui, max_width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let available = ui.available_width();
    let width = available.min(max_width);
    let margin = ((available - width) / 2.0).max(0.0);
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.add_space(margin);
        ui.allocate_ui_with_layout(
            egui::vec2(width, height),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(width);
                add(ui)
            },
        )
        .inner
    })
    .inner
}

pub enum NoticeKind {
    Info,
    Success,
    Warning,
    Error,
}

/// A calm message strip with a colored edge. Returns `true` when dismissed.
/// Returns whether × was clicked, and the notice's response (for hovering).
pub fn notice(ui: &mut Ui, kind: &NoticeKind, text: &str) -> (bool, Response) {
    let p = *palette(ui);
    let edge = kind_color(&p, kind);
    let mut dismissed = false;
    let response = Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin {
            left: 18,
            right: 10,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let close_width = 36.0;
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width() - close_width, 0.0),
                    Layout::left_to_right(Align::Center).with_main_wrap(true),
                    |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(text).color(p.text)).wrap());
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if button(ui, ButtonKind::Quiet, "×", true).clicked() {
                        dismissed = true;
                    }
                });
            });
        });
    let r = response.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(r.min, Pos2::new(r.min.x + 3.0, r.max.y)),
        CornerRadius {
            nw: 6,
            sw: 6,
            ne: 0,
            se: 0,
        },
        edge,
    );
    (dismissed, response.response)
}

pub fn kind_color(p: &Palette, kind: &NoticeKind) -> Color32 {
    match kind {
        NoticeKind::Info => p.accent,
        NoticeKind::Success => p.success,
        NoticeKind::Warning => p.warning,
        NoticeKind::Error => p.error,
    }
}

/// The logo, in the gold variant that fits the current appearance.
pub fn logo(ui: &mut Ui, size: f32) -> Response {
    let dark = ui.visuals().dark_mode;
    let id = egui::Id::new(("aeternavault-logo", dark));
    let texture = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id));
    let texture = match texture {
        Some(texture) => texture,
        None => {
            let bytes: &[u8] = if dark {
                include_bytes!("../../assets/logo/logo-dark-256.png")
            } else {
                include_bytes!("../../assets/logo/logo-light-256.png")
            };
            let image = match eframe::icon_data::from_png_bytes(bytes) {
                Ok(icon) => egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                ),
                Err(_) => egui::ColorImage::filled([1, 1], Color32::TRANSPARENT),
            };
            let texture =
                ui.ctx()
                    .load_texture("aeternavault-logo", image, egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, texture.clone()));
            texture
        }
    };
    ui.add(
        egui::Image::new(&texture)
            .fit_to_exact_size(Vec2::splat(size))
            .alt_text("AeternaVault"),
    )
}

/// A small (i) after a label: the explanation appears when pointing at it,
/// and stays open below the row after a click. Returns whether it is open;
/// show the text with [`info_text`] after the row.
pub fn info_icon(ui: &mut Ui, id_salt: &str, text: &str) -> bool {
    let p = *palette(ui);
    let id = ui.id().with(("info", id_salt));
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let size = 16.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let color = if response.hovered() || open {
        p.accent
    } else {
        p.text_secondary
    };
    let painter = ui.painter();
    painter.circle_stroke(
        rect.center(),
        size / 2.0 - 1.0,
        egui::Stroke::new(1.2, color),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(11.5),
        color,
    );
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(text);
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    open
}

/// The explanation of an open [`info_icon`].
pub fn info_text(ui: &mut Ui, open: bool, text: &str) {
    if open {
        let p = *palette(ui);
        ui.add(
            egui::Label::new(egui::RichText::new(text).size(13.5).color(p.text_secondary)).wrap(),
        );
    }
}

/// A section title with an (i) that explains it (hover, or click to keep open).
pub fn section_title_info(ui: &mut Ui, title: &str, info: &str) {
    let open = ui
        .horizontal(|ui| {
            section_title(ui, title);
            info_icon(ui, title, info)
        })
        .inner;
    info_text(ui, open, info);
}

/// A whole number: type it after a click into the field, or step it by one
/// with the arrows beside it (held down, it keeps counting). Dragging does
/// not change it, so a slip of the mouse never changes a setting.
pub fn number<N: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
) -> Response {
    number_with_zero(ui, value, range, None)
}

/// Like [`number`], but 0 is shown as a word (such as “off”) and typing that
/// word sets 0.
pub fn number_or<N: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
    zero: &str,
) -> Response {
    number_with_zero(ui, value, range, Some(zero))
}

fn number_with_zero<N: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
    zero: Option<&str>,
) -> Response {
    let p = *palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let mut drag = egui::DragValue::new(value)
            .range(range.clone())
            .speed(0.0)
            .max_decimals(0);
        if let Some(zero) = zero {
            let shown = zero.to_owned();
            let typed = zero.to_lowercase();
            drag = drag
                .custom_formatter(move |v, _| {
                    if v == 0.0 {
                        shown.clone()
                    } else {
                        format!("{v:.0}")
                    }
                })
                .custom_parser(move |text| {
                    let text = text.trim();
                    if text.to_lowercase() == typed {
                        Some(0.0)
                    } else {
                        text.parse().ok()
                    }
                });
        }
        let mut response = ui.add(drag);
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
        }

        let height = response.rect.height().max(20.0);
        let (rect, arrows) =
            ui.allocate_exact_size(Vec2::new(18.0, height), Sense::click_and_drag());
        let center = rect.center().y;
        let up = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, center));
        let down = Rect::from_min_max(Pos2::new(rect.min.x, center), rect.max);
        let pointer = arrows.interact_pointer_pos().or(arrows.hover_pos());
        let direction = |pos: Pos2| if pos.y < center { 1.0 } else { -1.0 };

        // One step per click; after holding for a moment, several per second.
        let now = ui.input(|i| i.time);
        let state = arrows.id.with("repeat");
        let mut delta = 0.0;
        if arrows.is_pointer_button_down_on() {
            let (start, last, repeated) = ui
                .data(|d| d.get_temp::<(f64, f64, bool)>(state))
                .unwrap_or((now, now, false));
            let mut next = (start, last, repeated);
            if now - start > 0.45
                && now - last > 0.07
                && let Some(pos) = pointer
            {
                delta = direction(pos);
                next = (start, now, true);
            }
            ui.data_mut(|d| d.insert_temp(state, next));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        if arrows.clicked() {
            let repeated = ui
                .data(|d| d.get_temp::<(f64, f64, bool)>(state))
                .is_some_and(|s| s.2);
            if !repeated && let Some(pos) = pointer {
                delta = direction(pos);
            }
        }
        if !arrows.is_pointer_button_down_on() {
            ui.data_mut(|d| d.remove::<(f64, f64, bool)>(state));
        }
        if delta != 0.0 {
            let stepped =
                (value.to_f64() + delta).clamp(range.start().to_f64(), range.end().to_f64());
            if stepped != value.to_f64() {
                *value = N::from_f64(stepped);
                response.mark_changed();
            }
        }

        // Two small buttons with a triangle each.
        let painter = ui.painter();
        let visuals = ui.visuals().widgets.inactive;
        painter.rect_filled(rect, CornerRadius::same(3), visuals.weak_bg_fill);
        painter.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        painter.line_segment(
            [
                Pos2::new(rect.min.x + 2.0, center),
                Pos2::new(rect.max.x - 2.0, center),
            ],
            Stroke::new(1.0, p.border),
        );
        for (half, pointing_up) in [(up, true), (down, false)] {
            let hot = pointer.is_some_and(|pos| half.contains(pos)) && arrows.hovered();
            if hot {
                painter.rect_filled(half.shrink(1.0), CornerRadius::same(2), visuals.bg_fill);
            }
            let c = half.center();
            let (w, h) = (4.0, 3.0);
            let points = if pointing_up {
                vec![
                    Pos2::new(c.x - w, c.y + h / 2.0),
                    Pos2::new(c.x + w, c.y + h / 2.0),
                    Pos2::new(c.x, c.y - h),
                ]
            } else {
                vec![
                    Pos2::new(c.x - w, c.y - h / 2.0),
                    Pos2::new(c.x + w, c.y - h / 2.0),
                    Pos2::new(c.x, c.y + h),
                ]
            };
            let color = if hot { p.text } else { p.text_secondary };
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        let arrows = arrows.on_hover_cursor(CursorIcon::PointingHand);
        response | arrows
    })
    .inner
}

/// One option of a choice, drawn as a button: the chosen one is filled with
/// the accent colour and carries a check mark, the others are outlined. Used
/// instead of radio circles, so it is obvious what is chosen.
pub fn choice<V: PartialEq>(
    ui: &mut Ui,
    current: &mut V,
    value: V,
    label: impl Into<WidgetText>,
) -> Response {
    let p = *palette(ui);
    let selected = *current == value;
    let font = FontId::proportional(15.0);
    let galley =
        label
            .into()
            .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, font);
    let padding = Vec2::new(16.0, 7.0);
    let size = Vec2::new(
        galley.size().x + padding.x * 2.0,
        (galley.size().y + padding.y * 2.0).max(32.0),
    );
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() && !selected {
        *current = value;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::RadioButton,
            ui.is_enabled(),
            selected,
            galley.text(),
        )
    });
    if ui.is_rect_visible(rect) {
        // The chosen option fades to the accent colour.
        let t = ui.ctx().animate_bool_with_time(response.id, selected, 0.12);
        let hovered = response.hovered();
        let fill = lerp_color(p.raised, p.accent, t);
        let text = lerp_color(p.text, p.on_accent, t);
        let border = if selected || hovered {
            p.accent
        } else {
            p.border
        };
        let radius = CornerRadius::same((size.y / 2.0).min(16.0) as u8);
        ui.painter().rect_filled(rect, radius, fill);
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let pos = Pos2::new(
            rect.min.x + padding.x,
            rect.center().y - galley.size().y / 2.0,
        );
        ui.painter().galley(pos, galley, text);
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| egui::lerp(f32::from(x)..=f32::from(y), t) as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// Two columns side by side when the window is wide enough, else one below
/// the other. `add` is called once per column with its index.
pub fn split(ui: &mut Ui, mut add: impl FnMut(&mut Ui, usize)) {
    if ui.available_width() < 1100.0 {
        add(ui, 0);
        ui.add_space(14.0);
        add(ui, 1);
        return;
    }
    // Each column gets exactly half; content that would be wider is clipped
    // instead of widening everything below (cards then keep one width).
    let gap = 16.0;
    let width = ui.available_width();
    let column = ((width - gap) / 2.0).floor();
    let top = ui.cursor().min;
    let mut bottom = top.y;
    for i in 0..2 {
        let rect = Rect::from_min_size(
            Pos2::new(top.x + i as f32 * (column + gap), top.y),
            Vec2::new(column, 1.0e6),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        child.set_width(column);
        // 2 px beyond the column, so the frame of a card is not cut off.
        child.set_clip_rect(child.clip_rect().intersect(Rect::from_min_size(
            rect.min - Vec2::new(0.0, 1.0e6),
            Vec2::new(column + 2.0, 2.0e6),
        )));
        add(&mut child, i);
        bottom = bottom.max(child.min_rect().bottom());
    }
    ui.allocate_rect(
        Rect::from_min_max(top, Pos2::new(top.x + width, bottom)),
        Sense::hover(),
    );
}

/// A bar showing how full something is, e.g. the destination drive.
pub fn usage_bar(ui: &mut Ui, used_fraction: f32, label: &str) {
    let p = *palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 10.0), Sense::hover());
    let fraction = used_fraction.clamp(0.0, 1.0);
    ui.painter()
        .rect_filled(rect, CornerRadius::same(5), p.raised);
    let mut used = rect;
    used.set_width(rect.width() * fraction);
    let color = if fraction > 0.9 {
        p.error
    } else if fraction > 0.75 {
        p.warning
    } else {
        p.accent
    };
    ui.painter().rect_filled(used, CornerRadius::same(5), color);
    ui.add_space(2.0);
    secondary_text(ui, label);
}

/// A labelled row: the label in a fixed column, the content beside it. The
/// content wraps within the row, and the row grows with it, so nothing below
/// is overlapped (a grid keeps last frame's row height).
pub fn field_row<R>(
    ui: &mut Ui,
    label_width: f32,
    label: &str,
    hint: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let label_width = label_width.min(ui.available_width() * 0.4);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(label_width, 36.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_size(Vec2::new(label_width, 36.0));
                ui.spacing_mut().item_spacing.x = 6.0;
                secondary_text(ui, label);
                if !hint.is_empty() {
                    hint_icon(ui, hint);
                }
            },
        );
        let rest = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(rest, 32.0),
            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
            |ui| {
                // Rows as tall as the tallest control, so everything in a row
                // is centred on the same line.
                ui.spacing_mut().interact_size.y = 36.0;
                add(ui)
            },
        )
        .inner
    })
    .inner
}

/// Shows `add` when `open`: it slides open and fades in over a fifth of a
/// second (and back), so the content below moves smoothly instead of jumping.
pub fn reveal<R>(ui: &mut Ui, id: &str, open: bool, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let id = ui.make_persistent_id(("reveal", id));
    let linear = ui.ctx().animate_bool_with_time(id, open, 0.22);
    if linear <= 0.0 {
        return None;
    }
    // Ease out: fast at first, settling gently.
    let t = 1.0 - (1.0 - linear).powi(3);
    let height_id = id.with("height");
    let known: Option<f32> = ui.data(|d| d.get_temp(height_id));
    let area = ui.available_rect_before_wrap();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(*ui.layout()));
    let animating = linear < 1.0;
    if animating && let Some(full) = known {
        let mut clip = child.clip_rect();
        clip.max.y = clip.max.y.min(area.top() + full * t);
        child.set_clip_rect(clip);
    }
    child.multiply_opacity(t);
    let result = add(&mut child);
    let used = child.min_rect();
    ui.data_mut(|d| d.insert_temp(height_id, used.height()));
    let height = if animating {
        (used.height() * t).round()
    } else {
        used.height()
    };
    ui.advance_cursor_after_rect(Rect::from_min_size(
        used.min,
        Vec2::new(used.width(), height),
    ));
    Some(result)
}

/// A small rounded button for inserting a value, filter tags and the like.
pub fn chip(ui: &mut Ui, label: &str) -> Response {
    let p = *palette(ui);
    let response = ui.add(
        egui::Button::new(egui::RichText::new(label).size(13.0).color(p.text))
            .fill(p.raised)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(12))
            .min_size(Vec2::new(0.0, 24.0)),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    response
}

/// A notification as it arrives: a coloured edge, the subject in bold and the
/// message below.
pub fn preview_message(ui: &mut Ui, title: &str, body: &str, warning: bool) {
    let p = *palette(ui);
    let response = Frame::new()
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin {
            left: 18,
            right: 14,
            top: 12,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(title)
                        .family(egui::FontFamily::Name(SANS_STRONG.into()))
                        .size(15.0)
                        .color(p.text),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(body).size(14.0)).wrap());
        })
        .response;
    let mut edge = response.rect;
    edge.set_width(4.0);
    ui.painter().rect_filled(
        edge,
        CornerRadius {
            nw: 8,
            sw: 8,
            ne: 0,
            se: 0,
        },
        if warning { p.warning } else { p.success },
    );
}

/// A tile with a big number and a caption, for overviews.
fn stat_tile_sized(ui: &mut Ui, value: &str, caption: &str, color: Color32, width: f32) {
    let p = *palette(ui);
    Frame::new()
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(18, 12))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(value)
                        .size(26.0)
                        .color(color)
                        .family(FontFamily::Name(SERIF.into())),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(caption)
                            .size(13.0)
                            .color(p.text_secondary),
                    )
                    .truncate(),
                );
            });
        });
}

/// Tiles in rows that fill the width: as many per row as fit at about 170
/// points each (frames do not wrap by themselves).
pub fn stat_tiles(ui: &mut Ui, tiles: &[(String, &str, Color32)]) {
    const GAP: f32 = 12.0;
    const MARGIN: f32 = 2.0 * 18.0 + 2.0;
    let width = ui.available_width();
    let per_row = (((width + GAP) / (170.0 + MARGIN + GAP)) as usize).clamp(1, tiles.len().max(1));
    let inner = ((width - GAP * (per_row - 1) as f32) / per_row as f32 - MARGIN).floor();
    for (r, row) in tiles.chunks(per_row).enumerate() {
        if r > 0 {
            ui.add_space(GAP);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for (value, caption, color) in row {
                stat_tile_sized(ui, value, caption, *color, inner);
            }
        });
    }
}
