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
    widgets::card(ui, |ui| {
        let open = ui
            .horizontal(|ui| {
                widgets::section_title(ui, t.notifications_title);
                widgets::info_icon(ui, "notifications", t.tip_notifications)
            })
            .inner;
        widgets::info_text(ui, open, t.tip_notifications);
        let n = &mut app.config.notifications;
        ui.checkbox(&mut n.on_success, t.notify_on_success);
        ui.checkbox(&mut n.on_problem, t.notify_on_problem);
        ui.checkbox(&mut n.on_skipped, t.notify_on_skipped);
        ui.horizontal_wrapped(|ui| {
            ui.label(t.notify_low_space);
            ui.add(egui::DragValue::new(&mut n.low_space_percent).range(0..=99));
            ui.label(t.notify_low_space_or);
            ui.add(egui::DragValue::new(&mut n.low_space_gb).range(0..=100_000));
            ui.label(t.notify_low_space_gb);
        });
    });
    ui.add_space(14.0);
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
                            ui.checkbox(&mut channel.enabled, "");
                            ui.add(
                                egui::TextEdit::singleline(&mut channel.name)
                                    .hint_text(channel.kind.name())
                                    .desired_width(180.0),
                            );
                            egui::ComboBox::from_id_salt("kind")
                                .selected_text(channel.kind.name())
                                .show_ui(ui, |ui| {
                                    for kind in ChannelKind::ALL {
                                        ui.selectable_value(&mut channel.kind, kind, kind.name());
                                    }
                                });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::button(ui, ButtonKind::Quiet, t.notify_remove, true)
                                    .clicked()
                                {
                                    remove = Some(i);
                                }
                                if widgets::button(ui, ButtonKind::Secondary, t.notify_test, true)
                                    .clicked()
                                {
                                    test = Some(channel.clone());
                                }
                            });
                        });
                        channel_fields(ui, channel, lang);
                        if let Some(result) = ctx.data(|d| d.get_temp::<String>(result_id)) {
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
    ui.add_space(14.0);
    widgets::card(ui, |ui| {
        let n = &mut app.config.notifications;
        let open = ui
            .horizontal(|ui| {
                widgets::section_title(ui, t.notify_messages);
                widgets::info_icon(ui, "notify-texts", t.notify_placeholders)
            })
            .inner;
        widgets::info_text(ui, open, t.notify_placeholders);
        {
            {
                let rows = [
                    (
                        t.notify_success_label,
                        &mut n.success_title,
                        &mut n.success_message,
                        t.notify_success_title,
                        t.notify_success_message,
                    ),
                    (
                        t.notify_problem_label,
                        &mut n.problem_title,
                        &mut n.problem_message,
                        t.notify_problem_title,
                        t.notify_problem_message,
                    ),
                    (
                        t.notify_low_space_label,
                        &mut n.low_space_title,
                        &mut n.low_space_message,
                        t.notify_low_space_title,
                        t.notify_low_space_message,
                    ),
                ];
                for (label, title, body, default_title, default_body) in rows {
                    ui.add_space(6.0);
                    ui.label(label);
                    ui.add(
                        egui::TextEdit::singleline(title)
                            .hint_text(default_title)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add(
                        egui::TextEdit::multiline(body)
                            .hint_text(default_body)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                }
            }
        }
    });
    if app.config.notifications != before {
        app.mark_dirty();
    }
}

/// The settings a channel kind needs.
fn channel_fields(ui: &mut Ui, channel: &mut crate::notify::Channel, lang: Lang) {
    use crate::notify::{ChannelKind, SmtpSecurity};
    let t = lang.t();
    let row = |ui: &mut Ui, label: &str, value: &mut String, hint: &str, secret: bool| {
        widgets::secondary_text(ui, label);
        ui.add(
            egui::TextEdit::singleline(value)
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
                    ui.add(egui::DragValue::new(&mut channel.smtp_port).range(1..=65535));
                    ui.add_space(12.0);
                    widgets::secondary_text(ui, t.notify_field_security);
                    for (value, name) in [
                        (SmtpSecurity::Starttls, "STARTTLS"),
                        (SmtpSecurity::Tls, "TLS"),
                        (SmtpSecurity::None, "-"),
                    ] {
                        ui.radio_value(&mut channel.smtp_security, value, name);
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
