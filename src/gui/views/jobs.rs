//! Backup jobs: automatic backups, each with its own time and contents.

use eframe::egui::{self, Align, CornerRadius, FontFamily, Frame, Layout, Margin, Stroke, Ui};

use crate::gui::theme::{SANS_STRONG, palette};
use crate::gui::widgets::{self, ButtonKind};
use crate::gui::{AeternaApp, View};

pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let p = *palette(ui);

    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| widgets::section_title_info(ui, t.jobs_title, t.schedule_hint));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if widgets::button(ui, ButtonKind::Primary, t.add_schedule, true).clicked() {
                    app.new_schedule();
                }
            });
        });

        // A backup that runs right now in the background.
        let running = app.background.as_ref().and_then(|b| b.service.running());
        if let Some(running) = &running {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                widgets::strong_text(ui, lang.running_automatic(&running.label));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, ButtonKind::Quiet, t.stop, true).clicked()
                        && let Some(background) = &app.background
                    {
                        background.service.cancel_running();
                    }
                });
            });
            widgets::progress_line(ui, running.progress.fraction());
        }

        if app.config.schedules.is_empty() {
            ui.add_space(10.0);
            widgets::secondary_text(ui, t.schedules_empty);
        }
    });

    let now = chrono::Local::now();
    let mut toggled: Option<(String, bool)> = None;
    let mut edit = None;
    let mut remove = None;
    let mut run_now = None;
    let running = app
        .background
        .as_ref()
        .is_some_and(|b| b.service.running().is_some());

    for schedule in &app.config.schedules {
        ui.add_space(10.0);
        let label = if schedule.name.trim().is_empty() {
            crate::automatic::when_text(schedule, lang)
        } else {
            schedule.name.trim().to_string()
        };
        let state = app.state.schedules.get(&schedule.id);
        let last = state.and_then(|s| s.last_run.as_ref());
        Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(18, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let mut on = schedule.enabled;
                    if widgets::toggle(ui, &mut on, true, &label) {
                        toggled = Some((schedule.id.clone(), on));
                    }
                    ui.add_space(6.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2((ui.available_width() - 250.0).max(160.0), 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            let color = if schedule.enabled {
                                p.text
                            } else {
                                p.text_secondary
                            };
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&label)
                                        .family(FontFamily::Name(SANS_STRONG.into()))
                                        .size(15.0)
                                        .color(color),
                                )
                                .truncate(),
                            );
                            let mut lines = Vec::new();
                            if !schedule.name.trim().is_empty() {
                                lines.push(crate::automatic::when_text(schedule, lang));
                            }
                            lines.push(format!(
                                "{}: {}",
                                t.schedule_what,
                                crate::automatic::scope_text(schedule, &app.config, lang)
                            ));
                            for line in lines {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(line)
                                            .size(12.5)
                                            .color(p.text_secondary),
                                    )
                                    .truncate(),
                                );
                            }
                            let mut status = Vec::new();
                            if schedule.enabled
                                && let Some(next) =
                                    crate::automatic::timing::next_occurrence(schedule, now)
                            {
                                status.push(lang.next_backup(next));
                            }
                            if let Some(run) = last {
                                status.push(lang.schedule_last_run(run));
                            }
                            if !status.is_empty() {
                                let failed = last.is_some_and(|r| !r.outcome.is_success());
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(status.join(" · ")).size(12.5).color(
                                            if failed { p.warning } else { p.text_secondary },
                                        ),
                                    )
                                    .wrap(),
                                );
                            }
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::button(ui, ButtonKind::Quiet, "×", true)
                            .on_hover_text(t.remove_schedule)
                            .clicked()
                        {
                            remove = Some(schedule.id.clone());
                        }
                        if widgets::button(ui, ButtonKind::Secondary, t.edit, true).clicked() {
                            edit = Some(schedule.id.clone());
                        }
                        let can_run = app.background.is_some() && !running && !app.is_busy();
                        if widgets::button(ui, ButtonKind::Quiet, t.run_now, can_run).clicked() {
                            run_now = Some(schedule.id.clone());
                        }
                    });
                });
            });
    }
    if let Some((id, on)) = toggled {
        app.set_schedule_enabled(&id, on);
    }
    if let Some(id) = edit {
        app.edit_schedule(&id);
    }
    if let Some(id) = remove {
        app.remove_schedule(&id);
    }
    if let Some(id) = run_now {
        app.run_schedule_now(&id);
    }

    // What automatic backups depend on.
    if app.config.any_schedule_enabled() {
        let autostart = app.background.as_ref().is_some_and(|b| b.autostart);
        let needs_key =
            app.config.encryption.enabled && !app.vault.remembered && app.vault.key.is_none();
        let incomplete = !autostart || !app.config.background.keep_running;
        if incomplete || needs_key {
            ui.add_space(14.0);
            widgets::card(ui, |ui| {
                if incomplete {
                    ui.horizontal_wrapped(|ui| {
                        widgets::secondary_text(ui, t.schedule_only_while_running);
                        if widgets::button(ui, ButtonKind::Quiet, t.open_settings, true).clicked() {
                            app.view = View::Settings;
                        }
                    });
                }
                if needs_key {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(t.schedule_needs_key).color(p.warning));
                        if widgets::button(ui, ButtonKind::Quiet, t.remember_now, true).clicked() {
                            app.set_remembered(true);
                        }
                    });
                }
            });
        }
    }
    ui.add_space(12.0);
}
