//! Notifications after backups: when, where to, and with which texts.

use eframe::egui::{self, Align, Layout, Ui};

use crate::gui::AeternaApp;
use crate::gui::widgets::{self, ButtonKind};
use crate::i18n::Lang;

/// The Notifications tab: rules, channels with a test button, texts.
pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    use crate::notify::{self, Channel, ChannelKind};

    let t = app.lang.t();
    let lang = app.lang;
    let ctx = ui.ctx().clone();
    let before = app.config.notifications.clone();
    widgets::split(ui, |ui, column| {
        if column == 0 {
            widgets::card(ui, |ui| {
                let open = ui
                    .horizontal(|ui| {
                        widgets::section_title(ui, t.notifications_title);
                        widgets::info_icon(ui, "notifications", t.tip_notifications)
                    })
                    .inner;
                widgets::info_text(ui, open, t.tip_notifications);
                let n = &mut app.config.notifications;
                widgets::switch(ui, &mut n.on_success, t.notify_on_success);
                widgets::switch(ui, &mut n.on_problem, t.notify_on_problem);
                widgets::switch(ui, &mut n.on_skipped, t.notify_on_skipped);
                ui.add_space(6.0);
                widgets::field_row(ui, 230.0, t.notify_low_space, "", |ui| {
                    ui.horizontal(|ui| {
                        widgets::number_or(ui, &mut n.low_space_percent, 0..=99, t.zero_off);
                        ui.label(t.notify_low_space_or);
                        widgets::number_or(ui, &mut n.low_space_gb, 0..=100_000, t.zero_off);
                        ui.label(t.notify_low_space_gb);
                    });
                });
                ui.add_space(4.0);
                let open = widgets::field_row(ui, 230.0, t.notify_overdue, "", |ui| {
                    ui.horizontal(|ui| {
                        widgets::number_or(ui, &mut n.overdue_days, 0..=365, t.zero_off);
                        ui.label(t.notify_overdue_days);
                        widgets::info_icon(ui, "overdue", t.tip_overdue)
                    })
                    .inner
                });
                widgets::info_text(ui, open, t.tip_overdue);
            });
        } else {
            widgets::card(ui, |ui| {
                let n = &mut app.config.notifications;
                widgets::section_title(ui, t.notify_channels);
                if n.channels.is_empty() {
                    widgets::secondary_text(ui, t.notify_no_channels);
                }
                let mut remove = None;
                for (i, channel) in n.channels.iter_mut().enumerate() {
                    let result_id = egui::Id::new(("notify-test", i));
                    let mut test = None;
                    ui.push_id(("channel", i), |ui| {
                        egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    widgets::switch(ui, &mut channel.enabled, "");
                                    ui.add(
                                        widgets::text_field(&mut channel.name)
                                            .hint_text(channel.kind.name())
                                            .desired_width(180.0),
                                    );
                                    egui::ComboBox::from_id_salt("kind")
                                        .selected_text(channel.kind.name())
                                        .show_ui(ui, |ui| {
                                            for kind in ChannelKind::ALL {
                                                ui.selectable_value(
                                                    &mut channel.kind,
                                                    kind,
                                                    kind.name(),
                                                );
                                            }
                                        });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if widgets::button(
                                            ui,
                                            ButtonKind::Quiet,
                                            t.notify_remove,
                                            true,
                                        )
                                        .clicked()
                                        {
                                            remove = Some(i);
                                        }
                                        if widgets::button(
                                            ui,
                                            ButtonKind::Secondary,
                                            t.notify_test,
                                            true,
                                        )
                                        .clicked()
                                        {
                                            test = Some(channel.clone());
                                        }
                                    });
                                });
                                channel_fields(ui, channel, lang);
                                if let Some(result) = ctx.data(|d| d.get_temp::<String>(result_id))
                                {
                                    widgets::secondary_text(ui, result);
                                }
                            });
                    });
                    ui.add_space(4.0);
                    if let Some(channel) = test {
                        let ctx = ctx.clone();
                        let sent = t.notify_test_sent;
                        std::thread::spawn(move || {
                            let text = match notify::send(&channel, &notify::test_message(lang)) {
                                Ok(()) => sent.to_string(),
                                Err(err) => format!("{err:#}"),
                            };
                            ctx.data_mut(|d| d.insert_temp(result_id, text));
                            ctx.request_repaint();
                        });
                    }
                }
                if let Some(i) = remove {
                    n.channels.remove(i);
                }
                if widgets::button(ui, ButtonKind::Secondary, t.notify_add, true).clicked() {
                    n.channels.push(Channel::default());
                }
            });
        }
    });
    ui.add_space(14.0);
    message_texts(app, ui);
    if app.config.notifications != before {
        app.mark_dirty();
    }
}

/// Which message the text editor shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Success,
    Problem,
    LowSpace,
}

/// The values a message can contain, as (placeholder, English, German).
const PLACEHOLDERS: [(&str, &str, &str); 13] = [
    ("{job}", "Job", "Auftrag"),
    ("{status}", "Status", "Status"),
    ("{backup}", "Backup", "Sicherung"),
    ("{files}", "Files", "Dateien"),
    ("{copied}", "New or changed", "Neu oder geändert"),
    ("{size}", "Size", "Größe"),
    ("{failed}", "Failed", "Fehlgeschlagen"),
    ("{duration}", "Duration", "Dauer"),
    ("{computer}", "Computer", "Computer"),
    ("{destination}", "Destination", "Ziel"),
    ("{free}", "Free space", "Freier Platz"),
    ("{free_percent}", "Free %", "Frei %"),
    ("{message}", "Details", "Details"),
];

/// Sample values for the preview.
fn sample_facts(kind: Kind, lang: Lang) -> crate::notify::Facts {
    let de = matches!(lang, Lang::De);
    let problem = kind == Kind::Problem;
    crate::notify::Facts {
        job: if de { "Dokumente" } else { "Documents" }.into(),
        status: match (problem, de) {
            (true, true) => "mit Hinweisen abgeschlossen",
            (true, false) => "complete with notes",
            (false, true) => "abgeschlossen",
            (false, false) => "complete",
        }
        .into(),
        backup: "2026-11-14 20-00".into(),
        files: 12_480,
        copied: 37,
        size: 8_804_682_752,
        failed: if problem { 3 } else { 0 },
        duration: std::time::Duration::from_secs(252),
        computer: "OFFICE-PC".into(),
        destination: if cfg!(windows) {
            "E:\\Backups"
        } else {
            "/mnt/backup"
        }
        .into(),
        free: Some(if kind == Kind::LowSpace {
            18_253_611_008
        } else {
            442_381_631_488
        }),
        free_percent: Some(if kind == Kind::LowSpace { 4.0 } else { 41.0 }),
        message: if problem {
            if de {
                "3 Dateien konnten nicht gelesen werden."
            } else {
                "3 files could not be read."
            }
        } else {
            ""
        }
        .into(),
    }
}

/// Inserts `text` into the field `id` where its cursor is (at the end if the
/// field never had the cursor) and keeps the cursor behind the insertion.
fn insert_at_cursor(ui: &Ui, id: egui::Id, field: &mut String, text: &str) {
    let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
    let chars = field.chars().count();
    let at = state
        .cursor
        .char_range()
        .map(|r| r.primary.index.0.min(chars))
        .unwrap_or(chars);
    let byte = field
        .char_indices()
        .nth(at)
        .map(|(b, _)| b)
        .unwrap_or(field.len());
    field.insert_str(byte, text);
    let after = egui::text::CCursor::new(at + text.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(after)));
    state.store(ui.ctx(), id);
    ui.ctx().memory_mut(|m| m.request_focus(id));
}

/// Message texts: one message at a time, the built-in text or an own one,
/// values to click in, and a preview with sample values.
fn message_texts(app: &mut AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let de = matches!(lang, Lang::De);
    let kind_id = egui::Id::new("notify-text-kind");
    let mut kind = ui
        .data(|d| d.get_temp::<Kind>(kind_id))
        .unwrap_or(Kind::Success);
    widgets::card(ui, |ui| {
        let open = ui
            .horizontal(|ui| {
                widgets::section_title(ui, t.notify_messages);
                widgets::info_icon(ui, "notify-texts", t.notify_placeholders)
            })
            .inner;
        widgets::info_text(ui, open, t.notify_placeholders);
        ui.horizontal_wrapped(|ui| {
            widgets::choice(ui, &mut kind, Kind::Success, t.notify_success_label);
            widgets::choice(ui, &mut kind, Kind::Problem, t.notify_problem_label);
            widgets::choice(ui, &mut kind, Kind::LowSpace, t.notify_low_space_label);
        });
        ui.data_mut(|d| d.insert_temp(kind_id, kind));
        ui.add_space(12.0);

        let n = &mut app.config.notifications;
        let (title, body, default_title, default_body) = match kind {
            Kind::Success => (
                &mut n.success_title,
                &mut n.success_message,
                t.notify_success_title,
                t.notify_success_message,
            ),
            Kind::Problem => (
                &mut n.problem_title,
                &mut n.problem_message,
                t.notify_problem_title,
                t.notify_problem_message,
            ),
            Kind::LowSpace => (
                &mut n.low_space_title,
                &mut n.low_space_message,
                t.notify_low_space_title,
                t.notify_low_space_message,
            ),
        };
        let mut own = !title.is_empty() || !body.is_empty();
        let title_id = egui::Id::new(("notify-text-title", kind as u8));
        let body_id = egui::Id::new(("notify-text-body", kind as u8));
        let last_id = egui::Id::new("notify-text-last-field");

        widgets::split(ui, |ui, column| {
            if column == 0 {
                if widgets::switch(ui, &mut own, if de { "Eigener Text" } else { "Own text" })
                    .changed()
                {
                    if own {
                        *title = default_title.to_owned();
                        *body = default_body.to_owned();
                    } else {
                        title.clear();
                        body.clear();
                    }
                }
                widgets::reveal(ui, "notify-text-edit", own, |ui| {
                    ui.add_space(8.0);
                    widgets::secondary_text(ui, if de { "Betreff" } else { "Subject" });
                    let r = ui.add(
                        widgets::text_field(title)
                            .id(title_id)
                            .hint_text(default_title)
                            .desired_width(f32::INFINITY),
                    );
                    if r.has_focus() {
                        ui.data_mut(|d| d.insert_temp(last_id, false));
                    }
                    ui.add_space(6.0);
                    widgets::secondary_text(ui, if de { "Nachricht" } else { "Message" });
                    let r = ui.add(
                        widgets::text_area(body)
                            .id(body_id)
                            .hint_text(default_body)
                            .desired_rows(5)
                            .desired_width(f32::INFINITY),
                    );
                    if r.has_focus() {
                        ui.data_mut(|d| d.insert_temp(last_id, true));
                    }
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        for (token, en, german) in PLACEHOLDERS {
                            if widgets::chip(ui, if de { german } else { en })
                                .on_hover_text(token)
                                .clicked()
                            {
                                let into_body =
                                    ui.data(|d| d.get_temp::<bool>(last_id)).unwrap_or(true);
                                if into_body {
                                    insert_at_cursor(ui, body_id, body, token);
                                } else {
                                    insert_at_cursor(ui, title_id, title, token);
                                }
                            }
                        }
                    });
                    ui.add_space(8.0);
                    if widgets::button(
                        ui,
                        ButtonKind::Quiet,
                        if de {
                            "Eingebauten Text wiederherstellen"
                        } else {
                            "Restore the built-in text"
                        },
                        *title != default_title || *body != default_body,
                    )
                    .clicked()
                    {
                        *title = default_title.to_owned();
                        *body = default_body.to_owned();
                    }
                });
            } else {
                let facts = sample_facts(kind, lang);
                let shown_title = if title.is_empty() {
                    default_title
                } else {
                    title.as_str()
                };
                let shown_body = if body.is_empty() {
                    default_body
                } else {
                    body.as_str()
                };
                widgets::secondary_text(ui, if de { "Vorschau" } else { "Preview" });
                ui.add_space(4.0);
                widgets::preview_message(
                    ui,
                    &facts.render(shown_title, lang),
                    &facts.render(shown_body, lang),
                    kind != Kind::Success,
                );
            }
        });
    });
}

/// The settings a channel kind needs.
fn channel_fields(ui: &mut Ui, channel: &mut crate::notify::Channel, lang: Lang) {
    use crate::notify::{ChannelKind, SmtpSecurity};
    let t = lang.t();
    let row = |ui: &mut Ui, label: &str, value: &mut String, hint: &str, secret: bool| {
        widgets::secondary_text(ui, label);
        ui.add(
            widgets::text_field(value)
                .hint_text(hint)
                .password(secret)
                .desired_width(f32::INFINITY),
        );
        ui.end_row();
    };
    egui::Grid::new("fields")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            if channel.kind.uses_url() {
                let hint = channel.kind.url_hint();
                row(ui, t.notify_field_url, &mut channel.url, hint, false);
            }
            if channel.kind.uses_token() {
                row(ui, t.notify_field_token, &mut channel.token, "", true);
            }
            if channel.kind == ChannelKind::Telegram {
                row(
                    ui,
                    t.notify_field_chat,
                    &mut channel.chat_id,
                    "123456789",
                    false,
                );
            }
            if channel.kind == ChannelKind::Email {
                row(
                    ui,
                    t.notify_field_server,
                    &mut channel.smtp_host,
                    "smtp.example.com",
                    false,
                );
                widgets::secondary_text(ui, t.notify_field_port);
                ui.horizontal(|ui| {
                    widgets::number(ui, &mut channel.smtp_port, 1..=65535);
                    ui.add_space(12.0);
                    widgets::secondary_text(ui, t.notify_field_security);
                    for (value, name) in [
                        (SmtpSecurity::Starttls, "STARTTLS"),
                        (SmtpSecurity::Tls, "TLS"),
                        (SmtpSecurity::None, "-"),
                    ] {
                        widgets::choice(ui, &mut channel.smtp_security, value, name);
                    }
                });
                ui.end_row();
                row(ui, t.notify_field_user, &mut channel.smtp_user, "", false);
                row(
                    ui,
                    t.notify_field_password,
                    &mut channel.smtp_password,
                    "",
                    true,
                );
                row(
                    ui,
                    t.notify_field_from,
                    &mut channel.from,
                    "backup@example.com",
                    false,
                );
                row(
                    ui,
                    t.notify_field_to,
                    &mut channel.to,
                    "it@example.com",
                    false,
                );
            }
        });
}
