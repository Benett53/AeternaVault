//! Progress and result screens.

use eframe::egui::{self, Align, FontFamily, Layout, Ui};

use crate::engine::Phase;
use crate::engine::manifest::SnapshotStatus;
use crate::gui::tasks::TaskKind;
use crate::gui::theme::{SERIF, palette};
use crate::gui::widgets::{self, ButtonKind};
use crate::gui::{AeternaApp, Done, Screen};
use crate::platform;

fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .family(FontFamily::Name(SERIF.into()))
            .size(26.0),
    );
}

/// What a task does, in a few words.
pub fn task_title(lang: crate::i18n::Lang, kind: TaskKind) -> &'static str {
    let t = lang.t();
    match kind {
        TaskKind::PlanBackup => t.working_scan_backup,
        TaskKind::PlanRestore => t.working_scan_restore,
        TaskKind::Backup => t.working_backup,
        TaskKind::Restore => t.working_restore,
        TaskKind::Verify => t.working_verify,
        TaskKind::Delete => t.working_delete,
        TaskKind::Transfer => t.working_transfer,
        TaskKind::Extract { .. } => t.working_extract,
        TaskKind::LoadContents => t.loading,
    }
}

/// Whether a result is good, and its headline (for a notice when the user
/// did not wait on the progress page).
pub fn done_title(lang: crate::i18n::Lang, done: &Done) -> (bool, &'static str) {
    let t = lang.t();
    match done {
        Done::Backup(Ok(report)) => match report.header.status {
            SnapshotStatus::Complete => (true, t.done_backup),
            SnapshotStatus::CompleteWithWarnings => (false, t.done_backup_notes),
            _ => (false, t.done_backup_cancelled),
        },
        Done::Restore(Ok(report)) if report.cancelled => (false, t.done_restore_cancelled),
        Done::Restore(Ok(report)) if report.failed > 0 => (false, t.done_restore_notes),
        Done::Restore(Ok(_)) => (true, t.done_restore),
        Done::Verify(Ok(report)) if report.is_ok() => (true, t.done_verify_ok),
        Done::Verify(Ok(_)) => (false, t.done_verify_problems),
        Done::Delete(Ok(_)) => (true, t.done_delete),
        Done::Transfer(Ok(_)) => (true, t.done_transfer),
        Done::Extract(Ok((report, _))) if report.failed.is_empty() => (true, t.done_extract),
        Done::Extract(Ok(_)) => (false, t.done_extract_notes),
        _ => (false, t.done_failed),
    }
}

pub fn show_working(app: &mut AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let Some(task) = &app.task else {
        return;
    };
    let progress = task.progress.clone();
    let cancelling = task.cancel.is_cancelled();
    let kind = task.kind;

    widgets::centered_column(ui, 860.0, |ui| {
        ui.add_space((ui.available_height() * 0.18).clamp(20.0, 140.0));
        widgets::card(ui, |ui| {
            ui.add_space(6.0);
            let title = match (kind, progress.phase) {
                (_, Phase::Finishing) if matches!(kind, TaskKind::Backup | TaskKind::Restore) => {
                    t.working_finishing
                }
                _ => task_title(lang, kind),
            };
            heading(ui, title);
            ui.add_space(14.0);

            match kind {
                TaskKind::Delete | TaskKind::LoadContents => {
                    widgets::progress_line(ui, None);
                }
                TaskKind::PlanBackup => {
                    widgets::progress_line(ui, None);
                    ui.add_space(8.0);
                    widgets::secondary_text(
                        ui,
                        lang.files_found(progress.files_done, progress.bytes_done),
                    );
                }
                TaskKind::PlanRestore => {
                    let fraction = (progress.files_total > 0)
                        .then(|| progress.files_done as f32 / progress.files_total as f32);
                    widgets::progress_line(ui, fraction);
                    ui.add_space(8.0);
                    widgets::secondary_text(
                        ui,
                        lang.files_of(progress.files_done, progress.files_total),
                    );
                }
                TaskKind::Backup
                | TaskKind::Restore
                | TaskKind::Verify
                | TaskKind::Transfer
                | TaskKind::Extract { .. } => {
                    widgets::progress_line(ui, progress.fraction());
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        widgets::secondary_text(
                            ui,
                            lang.files_of(progress.files_done, progress.files_total),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            widgets::secondary_text(
                                ui,
                                lang.bytes_of(
                                    progress.bytes_done.min(progress.bytes_total),
                                    progress.bytes_total,
                                ),
                            );
                        });
                    });
                }
            }

            ui.add_space(4.0);
            let p = *palette(ui);
            let width = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::hover());
            widgets::paint_cell(
                ui,
                rect,
                &progress.current,
                egui::FontId::proportional(12.0),
                p.text_secondary,
                Align::Min,
            );

            ui.add_space(18.0);
            // `horizontal` keeps the right-aligned row as tall as its buttons.
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if cancelling { t.stopping } else { t.cancel };
                    if widgets::button(ui, ButtonKind::Secondary, label, !cancelling).clicked()
                        && let Some(task) = &app.task
                    {
                        task.cancel.cancel();
                        tracing::info!("cancellation requested");
                    }
                });
            });
        });
    });
}

pub fn show_done(app: &mut AeternaApp, ui: &mut Ui) {
    let Screen::Done(done) = &app.screen else {
        return;
    };
    let t = app.lang.t();
    let lang = app.lang;
    let p = *palette(ui);
    let mut back = false;
    let mut make_job = false;
    // After a successful backup from the window, offer to repeat it automatically
    // (not in the viewer, and not when a job already covers everything).
    let offer_job = matches!(done.as_ref(), Done::Backup(Ok(report))
            if report.header.status != SnapshotStatus::Failed
                && report.header.status != SnapshotStatus::Cancelled)
        && app.viewer.is_none()
        && !app
            .config
            .schedules
            .iter()
            .any(|s| s.enabled && s.is_everything());

    widgets::centered_column(ui, 960.0, |ui| {
        ui.add_space(24.0);
        widgets::card(ui, |ui| {
            ui.add_space(4.0);
            let mut warnings: &[String] = &[];
            let mut extra_notes: Vec<String> = Vec::new();
            let mut open_folder = None;
            match done.as_ref() {
                Done::Backup(Ok(report)) => {
                    let h = &report.header;
                    let stats = report.total_stats();
                    heading(
                        ui,
                        match h.status {
                            SnapshotStatus::Complete => t.done_backup,
                            SnapshotStatus::CompleteWithWarnings => t.done_backup_notes,
                            SnapshotStatus::Cancelled | SnapshotStatus::Failed => {
                                t.done_backup_cancelled
                            }
                        },
                    );
                    ui.add_space(8.0);
                    ui.label(lang.backup_result(
                        stats.copied_files,
                        stats.linked_files + stats.referenced_files,
                        stats.bytes,
                        report.duration,
                    ));
                    widgets::secondary_text(ui, report.snapshot_dir.display().to_string());
                    if let Some(part) = &report.encrypted_part {
                        widgets::secondary_text(ui, lang.encrypted_part_files(part.stats.files));
                    }
                    if stats.failed > 0 {
                        ui.label(
                            egui::RichText::new(lang.failed_files(stats.failed)).color(p.warning),
                        );
                    }
                    warnings = &report.warnings;
                    open_folder = Some(report.snapshot_dir.clone());
                }
                Done::Verify(Ok(report)) => {
                    heading(
                        ui,
                        if report.cancelled {
                            t.done_verify_cancelled
                        } else if report.is_ok() {
                            t.done_verify_ok
                        } else {
                            t.done_verify_problems
                        },
                    );
                    ui.add_space(8.0);
                    ui.label(lang.verify_result(report.files, report.bytes, report.duration));
                    if !report.is_ok() {
                        ui.label(
                            egui::RichText::new(lang.verify_problems(
                                report.damaged.len(),
                                report.missing.len(),
                                report.unreadable.len(),
                            ))
                            .color(p.warning),
                        );
                        extra_notes = report
                            .damaged
                            .iter()
                            .map(|d| format!("{}: {d}", t.damaged_label))
                            .chain(
                                report
                                    .missing
                                    .iter()
                                    .map(|m| format!("{}: {m}", t.missing_label)),
                            )
                            .chain(
                                report
                                    .unreadable
                                    .iter()
                                    .map(|u| format!("{}: {u}", t.unreadable_label)),
                            )
                            .collect();
                    }
                }
                Done::Delete(Ok(report)) => {
                    heading(ui, t.done_delete);
                    ui.add_space(8.0);
                    ui.label(lang.delete_result(report));
                    warnings = &report.warnings;
                }
                Done::Transfer(Ok(report)) => {
                    heading(ui, t.done_transfer);
                    ui.add_space(8.0);
                    ui.label(lang.transfer_result(report.files, report.bytes, report.duration));
                    if report.delete.rehomed_files > 0 {
                        widgets::secondary_text(ui, lang.rehomed_note(report.delete.rehomed_files));
                    }
                }
                Done::Extract(Ok((report, target))) => {
                    heading(
                        ui,
                        if report.failed.is_empty() && !report.cancelled {
                            t.done_extract
                        } else {
                            t.done_extract_notes
                        },
                    );
                    ui.add_space(8.0);
                    ui.label(lang.extract_result(report.files, report.bytes, report.duration));
                    widgets::secondary_text(ui, target.display().to_string());
                    warnings = &report.failed;
                    open_folder = Some(target.clone());
                }
                Done::Verify(Err(message))
                | Done::Delete(Err(message))
                | Done::Transfer(Err(message))
                | Done::Extract(Err(message)) => {
                    heading(ui, t.done_failed);
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(egui::RichText::new(message).color(p.warning)).wrap());
                }
                Done::Restore(Ok(report)) => {
                    heading(
                        ui,
                        if report.cancelled {
                            t.done_restore_cancelled
                        } else if report.failed > 0 {
                            t.done_restore_notes
                        } else {
                            t.done_restore
                        },
                    );
                    ui.add_space(8.0);
                    ui.label(lang.restore_result(
                        report.restored_files,
                        report.restored_bytes,
                        report.skipped,
                        report.duration,
                    ));
                    if report.failed > 0 {
                        ui.label(
                            egui::RichText::new(lang.failed_files(report.failed)).color(p.warning),
                        );
                    }
                    warnings = &report.warnings;
                }
                Done::Backup(Err(message)) | Done::Restore(Err(message)) => {
                    heading(ui, t.done_failed);
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(egui::RichText::new(message).color(p.warning)).wrap());
                }
            }

            if !extra_notes.is_empty() {
                warnings = &extra_notes;
            }
            if !warnings.is_empty() {
                ui.add_space(10.0);
                egui::CollapsingHeader::new(
                    egui::RichText::new(t.notes_title).color(p.text_secondary),
                )
                .id_salt("done-notes")
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for warning in warnings.iter().take(500) {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(warning).size(12.5))
                                        .wrap(),
                                );
                            }
                        });
                });
            }

            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, ButtonKind::Primary, t.to_overview, true).clicked() {
                        back = true;
                    }
                    if offer_job
                        && widgets::button(ui, ButtonKind::Secondary, t.make_automatic, true)
                            .on_hover_text(t.make_automatic_hint)
                            .clicked()
                    {
                        make_job = true;
                    }
                    if let Some(folder) = &open_folder
                        && widgets::button(ui, ButtonKind::Secondary, t.open_backup_folder, true)
                            .clicked()
                    {
                        platform::open_in_file_manager(folder);
                    }
                });
            });
        });
    });

    if back || make_job {
        app.screen = Screen::Main;
    }
    if make_job {
        let ctx = ui.ctx().clone();
        app.navigate(&ctx, crate::gui::View::Jobs);
        app.new_schedule();
    }
}
