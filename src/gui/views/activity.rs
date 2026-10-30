//! Activity: the history of everything AeternaVault did (kept across
//! sessions), and the technical log of this session.

use eframe::egui::{self, Align, Layout, Ui};
use tracing::Level;

use crate::gui::AeternaApp;
use crate::gui::theme::palette;
use crate::gui::widgets::{self, ButtonKind};
use crate::history::Severity;
use crate::platform;

/// Entries drawn at most; older ones are still in the file.
const SHOWN: usize = 1000;

pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let p = *palette(ui);
    let technical_id = egui::Id::new("activity-technical");
    let filter_id = egui::Id::new("activity-filter");
    let mut technical = ui
        .data(|d| d.get_temp::<bool>(technical_id))
        .unwrap_or(false);
    let mut filter = ui
        .data(|d| d.get_temp::<String>(filter_id))
        .unwrap_or_default();

    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::section_title_info(
                    ui,
                    t.nav_activity,
                    if technical {
                        t.technical_log_hint
                    } else {
                        t.history_hint
                    },
                )
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if widgets::button(ui, ButtonKind::Quiet, t.open_log_folder, true).clicked() {
                    platform::open_in_file_manager(&app.paths.log_dir);
                }
                let label = if technical {
                    t.show_history
                } else {
                    t.show_technical_log
                };
                if widgets::button(ui, ButtonKind::Quiet, label, true).clicked() {
                    technical = !technical;
                }
            });
        });
        if !technical {
            export_row(app, ui);
        }
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut filter)
                .hint_text(t.filter_activity)
                .desired_width(280.0),
        );
        ui.add_space(8.0);
        let needle = filter.trim().to_lowercase();
        let height = (ui.available_height() - 20.0).max(240.0);

        if technical {
            let lines = app.log.lines();
            if lines.is_empty() {
                widgets::secondary_text(ui, t.activity_empty);
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("activity-log")
                .max_height(height)
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in lines
                        .iter()
                        .filter(|l| needle.is_empty() || l.message.to_lowercase().contains(&needle))
                    {
                        ui.horizontal_top(|ui| {
                            ui.label(
                                egui::RichText::new(line.time.format("%H:%M:%S").to_string())
                                    .monospace()
                                    .color(p.text_secondary),
                            );
                            let (label, color) = match line.level {
                                Level::ERROR => ("ERROR", p.error),
                                Level::WARN => ("WARN ", p.warning),
                                Level::INFO => ("INFO ", p.text_secondary),
                                _ => ("DEBUG", p.text_secondary),
                            };
                            ui.label(egui::RichText::new(label).monospace().color(color));
                            ui.add(
                                egui::Label::new(egui::RichText::new(&line.message).size(13.5))
                                    .wrap(),
                            );
                        });
                    }
                });
            return;
        }

        if app.history.is_empty() {
            widgets::secondary_text(ui, t.history_empty);
            return;
        }
        let rows: Vec<(chrono::DateTime<chrono::Local>, String, String, Severity)> = app
            .history
            .iter()
            .rev()
            .map(|entry| {
                let (title, detail, severity) = lang.history_entry(&entry.event);
                (
                    entry.at.with_timezone(&chrono::Local),
                    title,
                    detail,
                    severity,
                )
            })
            .filter(|(_, title, detail, _)| {
                needle.is_empty()
                    || title.to_lowercase().contains(&needle)
                    || detail.to_lowercase().contains(&needle)
            })
            .take(SHOWN)
            .collect();

        egui::ScrollArea::vertical()
            .id_salt("activity-history")
            .max_height(height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let mut day = None;
                for (at, title, detail, severity) in &rows {
                    if day != Some(at.date_naive()) {
                        day = Some(at.date_naive());
                        ui.add_space(6.0);
                        widgets::strong_text(ui, lang.weekday_date_only(*at));
                        ui.add_space(2.0);
                    }
                    ui.horizontal_top(|ui| {
                        ui.label(
                            egui::RichText::new(at.format("%H:%M").to_string())
                                .monospace()
                                .color(p.text_secondary),
                        );
                        let color = match severity {
                            Severity::Good => p.success,
                            Severity::Neutral => p.text_secondary,
                            Severity::Warning => p.warning,
                            Severity::Problem => p.error,
                        };
                        ui.add_space(2.0);
                        ui.vertical(|ui| {
                            ui.add_space(6.0);
                            widgets::status_dot(ui, color);
                        });
                        ui.vertical(|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(title).size(14.0)).wrap());
                            if !detail.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(detail)
                                            .size(12.5)
                                            .color(p.text_secondary),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                    });
                    ui.add_space(2.0);
                }
            });
    });

    ui.data_mut(|d| {
        d.insert_temp(technical_id, technical);
        d.insert_temp(filter_id, filter);
    });
}

/// Export as TXT or CSV, or send through the notification channels.
fn export_row(app: &mut AeternaApp, ui: &mut Ui) {
    use crate::gui::widgets::NoticeKind;
    use crate::history;

    let t = app.lang.t();
    let lang = app.lang;
    let has_entries = !app.history.is_empty();
    let result_id = egui::Id::new("activity-send-result");
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let stamp = chrono::Local::now().format("%Y-%m-%d").to_string();
        for (label, extension, csv) in [(t.export_txt, "txt", false), (t.export_csv, "csv", true)] {
            if widgets::button(ui, ButtonKind::Secondary, label, has_entries).clicked()
                && let Some(file) = rfd::FileDialog::new()
                    .set_file_name(format!("aeternavault-activity-{stamp}.{extension}"))
                    .add_filter(extension.to_uppercase(), &[extension])
                    .save_file()
            {
                let rows = history::export_rows(&app.history, lang);
                let text = if csv {
                    history::to_csv(&rows)
                } else {
                    history::to_text(&rows)
                };
                match std::fs::write(&file, text) {
                    Ok(()) => app.notify(
                        NoticeKind::Success,
                        format!("{} {}", t.log_saved, file.display()),
                    ),
                    Err(err) => app.notify(NoticeKind::Error, err.to_string()),
                }
            }
        }
        let channels = app.config.notifications.active();
        let send = widgets::button(ui, ButtonKind::Secondary, t.send_log, has_entries)
            .on_hover_text(if channels {
                t.send_log_hint
            } else {
                t.log_no_channels
            });
        if send.clicked() {
            if !channels {
                app.notify(NoticeKind::Info, t.log_no_channels.to_string());
            } else {
                let rows = history::export_rows(&app.history, lang);
                let newest = &rows[rows.len().saturating_sub(200)..];
                let text = history::to_text(newest);
                let csv = history::to_csv(&rows);
                let title = t
                    .log_send_title
                    .replace("{computer}", &crate::platform::computer_name());
                let config = app.config.clone();
                let ctx = ui.ctx().clone();
                let (sent, failed) = (t.log_sent, t.log_send_failed);
                std::thread::spawn(move || {
                    let failures = crate::notify::send_log(&config, &title, &text, &csv);
                    let message = if failures.is_empty() {
                        sent.to_string()
                    } else {
                        failures
                            .iter()
                            .map(|(channel, error)| format!("{failed} {channel}: {error}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    ctx.data_mut(|d| d.insert_temp(result_id, message));
                    ctx.request_repaint();
                });
            }
        }
    });
    if let Some(message) = ui.data(|d| d.get_temp::<String>(result_id)) {
        widgets::secondary_text(ui, message);
    }
    ui.add_space(4.0);
}
