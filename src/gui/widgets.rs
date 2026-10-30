//! Small, consistent building blocks for the AeternaVault interface.

use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Galley, Layout,
    Margin, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetText, text::LayoutJob,
    text::TextWrapping,
};
use std::sync::Arc;

use super::theme::{Palette, SANS_STRONG, SERIF, palette};

/// Width of the content column. Wide screens get some air on both sides.
pub const COLUMN: f32 = 1180.0;

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

/// An on/off switch with an accessible `label`. Returns `true` when it was flipped.
pub fn toggle(ui: &mut Ui, on: &mut bool, enabled: bool, label: &str) -> bool {
    let p = *palette(ui);
    let size = egui::vec2(40.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(
        size,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = if *on { p.accent } else { p.raised };
    let alpha = if enabled { 1.0 } else { 0.5 };
    ui.painter().rect(
        rect,
        CornerRadius::same(11),
        track.gamma_multiply(alpha),
        Stroke::new(1.0, if *on { p.accent } else { p.border }),
        egui::StrokeKind::Inside,
    );
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    let knob = if *on { p.on_accent } else { p.text_secondary };
    ui.painter().circle_filled(
        Pos2::new(x, rect.center().y),
        7.5,
        knob.gamma_multiply(alpha),
    );
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
