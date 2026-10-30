//! Modal dialogs: confirmation before starting, and the encryption dialogs.

use eframe::egui::{self, Align, CornerRadius, FontFamily, Frame, Layout, Margin, Stroke, Ui};

use super::theme::{self, palette};
use super::views;
use super::widgets::{self, ButtonKind, NoticeKind};
use super::{AeternaApp, Pending, VaultDialog};
use crate::engine::passphrase::{self, Rating};
use crate::engine::plan::ItemKind;

fn modal_frame(ui: &Ui) -> Frame {
    let p = palette(ui);
    Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(26))
        .shadow(ui.visuals().window_shadow)
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .family(FontFamily::Name(theme::SERIF.into()))
            .size(26.0),
    );
    ui.add_space(8.0);
}

fn wrapped(ui: &mut Ui, text: &str, color: Option<egui::Color32>) {
    let mut rich = egui::RichText::new(text);
    if let Some(color) = color {
        rich = rich.color(color);
    }
    ui.add(egui::Label::new(rich).wrap());
}

/// A password field that fills the dialog width, with an eye button to show
/// what was typed. Whether it is shown is kept per field in egui's memory.
pub(super) fn secret_field(
    ui: &mut Ui,
    value: &mut String,
    hint: &str,
    focus: bool,
    lang: crate::i18n::Lang,
) -> egui::Response {
    let t = lang.t();
    let visible_id = ui.id().with(("secret-visible", hint));
    let mut visible = ui.data(|d| d.get_temp::<bool>(visible_id)).unwrap_or(false);
    let inner = ui.horizontal(|ui| {
        let button_width = 34.0;
        let width = ui.available_width() - button_width - ui.spacing().item_spacing.x;
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .password(!visible)
                .hint_text(hint)
                .desired_width(width.max(80.0)),
        );
        let label = if visible {
            t.hide_secret
        } else {
            t.show_secret
        };
        if widgets::eye_button(ui, visible, label).clicked() {
            visible = !visible;
        }
        response
    });
    ui.data_mut(|d| d.insert_temp(visible_id, visible));
    let response = inner.inner;
    if focus && !response.has_focus() && ui.memory(|m| m.focused().is_none()) {
        response.request_focus();
    }
    response
}

/// Shows "Copied" for a moment after `copied_at` (egui time in seconds).
fn copied_feedback(ui: &mut Ui, copied_at: Option<f64>, text: &str) {
    let Some(at) = copied_at else {
        return;
    };
    let p = *palette(ui);
    let age = ui.input(|i| i.time) - at;
    const VISIBLE: f64 = 2.5;
    if (0.0..VISIBLE).contains(&age) {
        let fade = ((VISIBLE - age) / 0.4).clamp(0.0, 1.0) as f32;
        ui.label(
            egui::RichText::new(text)
                .size(13.5)
                .color(p.success.gamma_multiply(fade)),
        );
        ui.ctx().request_repaint();
    }
}

/// Confirmation before deleting, moving or copying out backups.
fn manage_confirmation(app: &mut AeternaApp, ui: &mut Ui) {
    let Some(pending) = &app.pending else {
        return;
    };
    let t = app.lang.t();
    let lang = app.lang;
    let mut start = false;
    let mut cancel = false;

    let modal = egui::Modal::new(egui::Id::new("confirm-manage"))
        .frame(modal_frame(ui))
        .show(ui.ctx(), |ui| {
            let p = *palette(ui);
            ui.set_width(dialog_width(ui, 640.0));
            let (title, body, warning, button): (&str, String, Option<String>, &str) = match pending
            {
                Pending::Delete(snapshots) => {
                    let names: Vec<String> = snapshots
                        .iter()
                        .map(|s| views::backups::snapshot_details(app, s).0)
                        .collect();
                    (
                        t.confirm_delete_title,
                        lang.confirm_delete(&names),
                        Some(if snapshots.iter().any(|s| s.needs_key()) {
                            t.confirm_delete_encrypted.to_string()
                        } else {
                            t.confirm_delete_plain.to_string()
                        }),
                        t.delete_backup,
                    )
                }
                Pending::Transfer(snapshot, target) => (
                    t.confirm_transfer_title,
                    lang.confirm_transfer(
                        &views::backups::snapshot_details(app, snapshot).0,
                        &target.display().to_string(),
                    ),
                    None,
                    t.move_backup,
                ),
                Pending::Extract {
                    snapshot,
                    files,
                    target,
                } => (
                    t.confirm_extract_title,
                    lang.confirm_extract(
                        files.as_ref().map(Vec::len),
                        &target.display().to_string(),
                    ),
                    snapshot
                        .needs_key()
                        .then(|| t.copies_are_decrypted.to_string()),
                    t.start,
                ),
                Pending::Backup(_) | Pending::Restore(_) => return,
            };
            heading(ui, title);
            wrapped(ui, &body, None);
            if let Some(warning) = warning {
                ui.add_space(4.0);
                wrapped(ui, &warning, Some(p.warning));
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, ButtonKind::Primary, button, true).clicked() {
                        start = true;
                    }
                    if widgets::button(ui, ButtonKind::Secondary, t.cancel, true).clicked() {
                        cancel = true;
                    }
                });
            });
        });
    if modal.should_close() && !start {
        cancel = true;
    }
    let ctx = ui.ctx().clone();
    if start {
        match app.pending.take() {
            Some(Pending::Delete(snapshots)) => app.start_delete(&ctx, snapshots),
            Some(Pending::Transfer(snapshot, target)) => {
                app.start_transfer(&ctx, *snapshot, target)
            }
            Some(Pending::Extract {
                snapshot,
                files,
                target,
            }) => app.start_extract(&ctx, *snapshot, files, target, false),
            other => app.pending = other,
        }
    } else if cancel {
        app.pending = None;
    }
}

pub fn confirmation(app: &mut AeternaApp, ui: &mut Ui) {
    if matches!(
        app.pending,
        Some(Pending::Delete(_) | Pending::Transfer(..) | Pending::Extract { .. })
    ) {
        manage_confirmation(app, ui);
        return;
    }
    let Some(pending) = &app.pending else {
        return;
    };
    let t = app.lang.t();
    let lang = app.lang;
    let mut action = 0; // 1 = start, 3 = cancel

    let modal = egui::Modal::new(egui::Id::new("confirm"))
        .frame(modal_frame(ui))
        .show(ui.ctx(), |ui| {
            let p = *palette(ui);
            ui.set_width(dialog_width(ui, 640.0));
            let (title, body, overwrite) = match pending {
                Pending::Backup(plan) => {
                    let (files, bytes) = plan.copy_totals();
                    (
                        t.confirm_backup_title,
                        lang.confirm_backup(files, bytes, &plan.destination.display().to_string()),
                        false,
                    )
                }
                Pending::Restore(plan) => (
                    t.confirm_restore_title,
                    lang.confirm_restore(
                        plan.summary.count(ItemKind::New),
                        plan.summary.count(ItemKind::Changed),
                    ),
                    plan.summary.count(ItemKind::Changed) > 0,
                ),
                _ => return,
            };
            heading(ui, title);
            wrapped(ui, &body, None);
            if overwrite {
                ui.add_space(4.0);
                wrapped(ui, t.confirm_overwrite, Some(p.warning));
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, ButtonKind::Primary, t.start, true).clicked() {
                        action = 1;
                    }
                    if widgets::button(ui, ButtonKind::Secondary, t.cancel, true).clicked() {
                        action = 3;
                    }
                });
            });
        });
    if modal.should_close() && action == 0 {
        action = 3;
    }

    let ctx = ui.ctx().clone();
    match action {
        1 => match app.pending.take() {
            Some(Pending::Backup(plan)) => app.start_backup(&ctx, plan),
            Some(Pending::Restore(plan)) => app.start_restore(&ctx, plan),
            other => app.pending = other,
        },
        3 => app.pending = None,
        _ => {}
    }
}

/// Creating or changing one automatic backup.
pub fn schedule_dialog(app: &mut AeternaApp, ui: &mut Ui) {
    use crate::config::{Frequency, Weekday};

    let Some(mut editor) = app.schedule.editor.take() else {
        return;
    };
    let t = app.lang.t();
    let lang = app.lang;
    let ctx = ui.ctx().clone();
    let mut keep_open = true;
    let mut save = false;

    let title = if editor.is_new {
        t.schedule_new_title
    } else {
        t.schedule_edit_title
    };
    let sources: Vec<(std::path::PathBuf, String)> = app
        .config
        .sources
        .iter()
        .map(|s| (s.path.clone(), s.display_name()))
        .collect();

    let modal = egui::Modal::new(egui::Id::new("schedule-editor"))
        .frame(modal_frame(ui))
        .show(&ctx, |ui| {
            let p = *palette(ui);
            ui.set_width(dialog_width(ui, 700.0));
            heading(ui, title);
            let schedule = &mut editor.schedule;

            widgets::secondary_text(ui, t.schedule_when);
            ui.horizontal_wrapped(|ui| {
                let label = |f: Frequency| match f {
                    Frequency::Daily => t.freq_daily,
                    Frequency::Weekly => t.freq_weekly,
                    Frequency::Hourly => t.freq_hourly,
                    Frequency::AtStart => t.freq_at_start,
                };
                egui::ComboBox::from_id_salt("schedule-frequency")
                    .selected_text(label(schedule.frequency))
                    .width(210.0)
                    .show_ui(ui, |ui| {
                        for f in [
                            Frequency::Daily,
                            Frequency::Weekly,
                            Frequency::Hourly,
                            Frequency::AtStart,
                        ] {
                            ui.selectable_value(&mut schedule.frequency, f, label(f));
                        }
                    });
                if schedule.frequency == Frequency::Weekly {
                    ui.label(t.on_day);
                    egui::ComboBox::from_id_salt("schedule-weekday")
                        .selected_text(t.weekdays[schedule.weekday as usize])
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            for day in Weekday::ALL {
                                ui.selectable_value(
                                    &mut schedule.weekday,
                                    day,
                                    t.weekdays[day as usize],
                                );
                            }
                        });
                }
                if schedule.frequency == Frequency::Hourly {
                    egui::ComboBox::from_id_salt("schedule-hours")
                        .selected_text(lang.every_hours(schedule.every_hours))
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for hours in [1u8, 2, 3, 4, 6, 8, 12] {
                                ui.selectable_value(
                                    &mut schedule.every_hours,
                                    hours,
                                    lang.every_hours(hours),
                                );
                            }
                        });
                    ui.label(t.starting_at);
                }
                if schedule.frequency != Frequency::AtStart {
                    if schedule.frequency != Frequency::Hourly {
                        ui.label(t.at_time);
                    }
                    time_picker(ui, &mut schedule.time);
                }
            });
            if schedule.frequency == Frequency::AtStart {
                widgets::secondary_text(ui, t.at_start_hint);
            }
            ui.add_space(4.0);
            ui.checkbox(&mut schedule.catch_up, t.catch_up);
            ui.checkbox(&mut schedule.verify_after, t.verify_after)
                .on_hover_text(t.verify_after_hint);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                widgets::secondary_text(ui, t.job_destination);
                let shown = schedule
                    .destination
                    .as_ref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|| t.job_destination_general.to_string());
                ui.label(shown);
                if widgets::button(ui, ButtonKind::Quiet, t.choose, true).clicked()
                    && let Some(folder) = rfd::FileDialog::new().pick_folder()
                {
                    schedule.destination = Some(folder);
                }
                if schedule.destination.is_some()
                    && widgets::button(ui, ButtonKind::Quiet, t.job_destination_general, true)
                        .clicked()
                {
                    schedule.destination = None;
                }
                widgets::info_icon(ui, "job-destination", t.job_destination_hint);
            });
            ui.horizontal(|ui| {
                widgets::secondary_text(ui, t.job_before_command);
                ui.add(
                    egui::TextEdit::singleline(&mut schedule.before_command)
                        .hint_text("pg_dump …")
                        .desired_width(420.0),
                );
                widgets::info_icon(ui, "job-before", t.job_before_command_hint);
            });

            ui.add_space(12.0);
            widgets::secondary_text(ui, t.schedule_what);
            ui.radio_value(&mut schedule.all_folders, true, t.scope_radio_everything);
            ui.radio_value(&mut schedule.all_folders, false, t.scope_radio_only);
            if !schedule.all_folders {
                ui.indent("schedule-folders", |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (path, name) in &sources {
                            let mut on = schedule
                                .folders
                                .iter()
                                .any(|f| crate::engine::paths_equal(f, path));
                            if ui.checkbox(&mut on, name).changed() {
                                if on {
                                    schedule.folders.push(path.clone());
                                } else {
                                    schedule
                                        .folders
                                        .retain(|f| !crate::engine::paths_equal(f, path));
                                }
                            }
                        }
                    });
                });
            }

            ui.add_space(12.0);
            widgets::secondary_text(ui, t.schedule_name);
            let hint = crate::automatic::when_text(schedule, lang);
            ui.add(
                egui::TextEdit::singleline(&mut schedule.name)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            );

            let nothing = !schedule.all_folders
                && !schedule.folders.iter().any(|f| {
                    sources
                        .iter()
                        .any(|(path, _)| crate::engine::paths_equal(f, path))
                });
            if nothing {
                ui.add_space(4.0);
                wrapped(ui, t.scope_nothing, Some(p.warning));
            }

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, ButtonKind::Primary, t.save, !nothing).clicked() {
                        save = true;
                    }
                    if widgets::button(ui, ButtonKind::Secondary, t.cancel, true).clicked() {
                        keep_open = false;
                    }
                });
            });
        });
    if modal.should_close() {
        keep_open = false;
    }
    if save {
        let mut schedule = editor.schedule;
        if schedule.all_folders {
            schedule.folders.clear();
        }
        schedule.name = schedule.name.trim().to_string();
        app.save_schedule(schedule);
    } else if keep_open {
        app.schedule.editor = Some(editor);
    }
}

/// Hours and minutes (in 15-minute steps) as two small drop-downs.
fn time_picker(ui: &mut Ui, time: &mut String) {
    let (mut hour, mut minute) = time
        .split_once(':')
        .and_then(|(h, m)| Some((h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?)))
        .unwrap_or((20, 0));
    hour = hour.min(23);
    minute = (minute / 15) * 15;
    let before = (hour, minute);
    egui::ComboBox::from_id_salt("schedule-hour")
        .selected_text(format!("{hour:02}"))
        .width(56.0)
        .show_ui(ui, |ui| {
            for h in 0..24 {
                ui.selectable_value(&mut hour, h, format!("{h:02}"));
            }
        });
    ui.label(":");
    egui::ComboBox::from_id_salt("schedule-minute")
        .selected_text(format!("{minute:02}"))
        .width(56.0)
        .show_ui(ui, |ui| {
            for m in [0, 15, 30, 45] {
                ui.selectable_value(&mut minute, m, format!("{m:02}"));
            }
        });
    if (hour, minute) != before || !time.contains(':') {
        *time = format!("{hour:02}:{minute:02}");
    }
}

/// Choice of cipher and key derivation strength for a vault set up later.
pub(super) fn encryption_method(
    ui: &mut Ui,
    encryption: &mut crate::config::Encryption,
    lang: crate::i18n::Lang,
) {
    use crate::config::{CipherSetting, KeyStrength};
    use crate::engine::crypto::Cipher;
    let t = lang.t();
    for (setting, cipher) in [
        (CipherSetting::XChaCha20Poly1305, Cipher::XChaCha20Poly1305),
        (CipherSetting::Aes256Gcm, Cipher::Aes256Gcm),
    ] {
        ui.radio_value(
            &mut encryption.cipher,
            setting,
            format!(
                "{}{}",
                cipher.display_name(),
                if cipher == Cipher::default() {
                    t.recommended_suffix
                } else {
                    ""
                }
            ),
        );
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            widgets::secondary_text(ui, lang.cipher_hint(cipher));
        });
    }
    ui.add_space(8.0);
    widgets::secondary_text(ui, t.kdf_title);
    for (strength, label) in [
        (KeyStrength::Standard, t.kdf_standard),
        (KeyStrength::Strong, t.kdf_strong),
        (KeyStrength::VeryStrong, t.kdf_very_strong),
    ] {
        ui.radio_value(&mut encryption.key_strength, strength, label);
    }
}

/// Dialog width that still fits into small windows.
fn dialog_width(ui: &Ui, wanted: f32) -> f32 {
    wanted
        .min(ui.ctx().content_rect().width() - 80.0)
        .max(320.0)
}

/// Asks for a file name and writes the recovery key with a short explanation.
/// `None` if the user cancelled the file dialog.
fn save_recovery_file(
    lang: crate::i18n::Lang,
    key: &str,
    vault_id: &str,
    destination: &std::path::Path,
) -> Option<Result<(), String>> {
    let path = rfd::FileDialog::new()
        .set_file_name(lang.t().recovery_file_name)
        .add_filter("Text", &["txt"])
        .save_file()?;
    let text = lang.recovery_file_text(key, vault_id, &destination.display().to_string());
    // Notepad-friendly line endings.
    let text = text.replace('\n', "\r\n");
    Some(std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display())))
}

/// Weak / fair / good. The criteria appear when pointing at the bar or at
/// (?), and stay open after a click on (?).
fn strength_meter(ui: &mut Ui, passphrase: &str, lang: crate::i18n::Lang) {
    let p = *palette(ui);
    let t = lang.t();
    let personal = passphrase::personal_details();
    let personal: Vec<&str> = personal.iter().map(String::as_str).collect();
    let assessment = passphrase::assess(passphrase, &personal);
    let (label, color, filled) = match assessment.rating() {
        Rating::Weak => (t.strength[0], p.error, 1.0),
        Rating::Fair => (t.strength[1], p.warning, 2.0),
        Rating::Good => (t.strength[2], p.success, 3.0),
    };
    let criteria = lang.passphrase_criteria(&assessment);
    let open_id = ui.id().with("strength-criteria-open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    ui.horizontal(|ui| {
        let width = 200.0;
        let (rect, bar) = ui.allocate_exact_size(egui::vec2(width, 16.0), egui::Sense::hover());
        let track = egui::Rect::from_center_size(rect.center(), egui::vec2(width, 5.0));
        ui.painter()
            .rect_filled(track, CornerRadius::same(2), p.raised);
        if !passphrase.is_empty() {
            let mut part = track;
            part.set_width(width * filled / 3.0);
            ui.painter().rect_filled(part, CornerRadius::same(2), color);
            bar.on_hover_text(&criteria);
            ui.add_space(6.0);
            ui.label(egui::RichText::new(label).color(color))
                .on_hover_text(&criteria);
        }
        let help = ui
            .add(egui::Button::new(egui::RichText::new("(?)").color(p.text_secondary)).frame(false))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if help.clicked() {
            open = !open;
        }
        if !open {
            help.on_hover_text(&criteria);
        }
    });
    if open {
        Frame::new()
            .fill(p.raised)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(egui::RichText::new(&criteria).size(14.0));
            });
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
}

/// The encryption dialogs. Each frame draws the active one, if any.
pub fn vault_dialog(app: &mut AeternaApp, ui: &mut Ui) {
    let Some(mut dialog) = app.vault.dialog.take() else {
        return;
    };
    let ctx = ui.ctx().clone();
    let lang = app.lang;
    let t = lang.t();
    let frame = modal_frame(ui);
    let mut keep_open = true;
    let mut next: Option<VaultDialog> = None;

    match &mut dialog {
        VaultDialog::Create {
            passphrase,
            repeat,
            remember,
            options,
            error,
        } => {
            let mut submit = false;
            let modal = egui::Modal::new(egui::Id::new("vault-create"))
                .frame(frame)
                .show(&ctx, |ui| {
                    let p = *palette(ui);
                    ui.set_width(dialog_width(ui, 660.0));
                    heading(ui, t.enc_create_title);
                    wrapped(ui, t.enc_create_hint, Some(p.text_secondary));
                    ui.add_space(10.0);
                    secret_field(ui, passphrase, t.passphrase, true, lang);
                    strength_meter(ui, passphrase, lang);
                    let repeat_field = secret_field(ui, repeat, t.passphrase_repeat, false, lang);
                    if repeat_field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.add_space(4.0);
                    ui.checkbox(remember, t.remember_on_computer);
                    ui.add_space(8.0);
                    wrapped(
                        ui,
                        &lang.method_for_new_vault(options.cipher, options.kdf),
                        Some(p.text_secondary),
                    );
                    ui.add_space(8.0);
                    wrapped(ui, t.enc_warning, Some(p.warning));
                    if let Some(error) = error.as_ref() {
                        ui.add_space(4.0);
                        wrapped(ui, error, Some(p.error));
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, ButtonKind::Primary, t.set_up, true).clicked() {
                                submit = true;
                            }
                            if widgets::button(ui, ButtonKind::Secondary, t.cancel, true).clicked()
                            {
                                keep_open = false;
                            }
                        });
                    });
                });
            if modal.should_close() {
                keep_open = false;
            }
            if submit {
                if let Some(problem) = app.passphrase_problem(passphrase, repeat) {
                    *error = Some(problem);
                } else {
                    match app.create_vault(passphrase, *remember, *options) {
                        Ok(key) => {
                            next = Some(VaultDialog::ShowRecovery {
                                key,
                                confirmed: false,
                                copied_at: None,
                            });
                        }
                        Err(message) => *error = Some(message),
                    }
                }
            }
        }

        VaultDialog::ShowRecovery {
            key,
            confirmed,
            copied_at,
        } => {
            let destination = app.config.destination.clone();
            let vault_id = app
                .vault
                .header
                .as_ref()
                .map(|h| h.vault_id.clone())
                .unwrap_or_default();
            let mut saved: Option<Result<(), String>> = None;
            let modal = egui::Modal::new(egui::Id::new("vault-recovery"))
                .frame(frame)
                .show(&ctx, |ui| {
                    let p = *palette(ui);
                    ui.set_width(dialog_width(ui, 720.0));
                    heading(ui, t.recovery_title);
                    wrapped(ui, t.recovery_hint, Some(p.text_secondary));
                    ui.add_space(12.0);
                    // The key gets the full width of its own frame; the buttons sit below,
                    // so nothing can cover it.
                    Frame::new()
                        .fill(p.raised)
                        .stroke(Stroke::new(1.0, p.accent))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(16, 14))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical_centered(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(key.as_str())
                                            .monospace()
                                            .size(17.0)
                                            .color(p.text),
                                    )
                                    .wrap(),
                                );
                            });
                        });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if widgets::button(ui, ButtonKind::Secondary, t.copy, true).clicked() {
                            ui.ctx().copy_text(key.clone());
                            *copied_at = Some(ui.input(|i| i.time));
                        }
                        if widgets::button(ui, ButtonKind::Quiet, t.save_as_file, true).clicked() {
                            saved = save_recovery_file(lang, key, &vault_id, &destination);
                            if saved.is_some() {
                                *copied_at = None;
                            }
                        }
                        copied_feedback(ui, *copied_at, t.copied);
                    });
                    ui.add_space(10.0);
                    ui.checkbox(confirmed, t.recovery_confirm);
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, ButtonKind::Primary, t.done, *confirmed)
                                .clicked()
                            {
                                keep_open = false;
                            }
                        });
                    });
                });
            // Closing by Escape or clicking outside is not allowed before confirming.
            let _ = modal;
            match saved {
                Some(Ok(())) => app.notify(NoticeKind::Success, t.recovery_file_saved),
                Some(Err(message)) => app.notify(NoticeKind::Error, message),
                None => {}
            }
        }

        VaultDialog::Unlock {
            secret,
            remember,
            error,
            then,
        } => {
            let mut submit = false;
            let then = *then;
            let modal = egui::Modal::new(egui::Id::new("vault-unlock"))
                .frame(frame)
                .show(&ctx, |ui| {
                    let p = *palette(ui);
                    ui.set_width(dialog_width(ui, 620.0));
                    heading(ui, t.unlock_title);
                    wrapped(ui, t.unlock_hint, Some(p.text_secondary));
                    ui.add_space(10.0);
                    let field = secret_field(ui, secret, t.passphrase, true, lang);
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.add_space(4.0);
                    ui.checkbox(remember, t.remember_on_computer);
                    if let Some(error) = error.as_ref() {
                        ui.add_space(4.0);
                        wrapped(ui, error, Some(p.error));
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let label = t.unlock.trim_end_matches('…');
                            if widgets::button(ui, ButtonKind::Primary, label, !secret.is_empty())
                                .clicked()
                            {
                                submit = true;
                            }
                            if widgets::button(ui, ButtonKind::Secondary, t.cancel, true).clicked()
                            {
                                keep_open = false;
                            }
                        });
                    });
                });
            if modal.should_close() {
                keep_open = false;
            }
            if submit && !secret.is_empty() {
                let secret_value = std::mem::take(secret);
                match app.unlock_vault(&ctx, &secret_value, *remember, then) {
                    Ok(()) => {
                        // The follow-up step may have opened the next dialog.
                        return;
                    }
                    Err(message) => *error = Some(message),
                }
            }
        }

        VaultDialog::TestRecovery { secret, result } => {
            let mut submit = false;
            let modal = egui::Modal::new(egui::Id::new("vault-test-recovery"))
                .frame(frame)
                .show(&ctx, |ui| {
                    let p = *palette(ui);
                    ui.set_width(dialog_width(ui, 660.0));
                    heading(ui, t.test_recovery_title);
                    wrapped(ui, t.test_recovery_hint, Some(p.text_secondary));
                    ui.add_space(10.0);
                    let field = ui.add(
                        egui::TextEdit::singleline(secret)
                            .hint_text("XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX")
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    match result.as_ref() {
                        Some(Ok(())) => wrapped(ui, t.test_recovery_ok, Some(p.success)),
                        Some(Err(message)) => wrapped(ui, message, Some(p.error)),
                        None => {}
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, ButtonKind::Secondary, t.done, true).clicked() {
                                keep_open = false;
                            }
                            if widgets::button(
                                ui,
                                ButtonKind::Primary,
                                t.test_now,
                                !secret.trim().is_empty(),
                            )
                            .clicked()
                            {
                                submit = true;
                            }
                        });
                    });
                });
            if modal.should_close() {
                keep_open = false;
            }
            if submit && !secret.trim().is_empty() {
                *result = Some(app.test_recovery_key(secret));
            }
        }

        VaultDialog::Change {
            passphrase,
            repeat,
            error,
        } => {
            let mut submit = false;
            let modal = egui::Modal::new(egui::Id::new("vault-change"))
                .frame(frame)
                .show(&ctx, |ui| {
                    let p = *palette(ui);
                    ui.set_width(dialog_width(ui, 620.0));
                    heading(ui, t.change_title);
                    wrapped(ui, t.change_hint, Some(p.text_secondary));
                    ui.add_space(10.0);
                    secret_field(ui, passphrase, t.new_passphrase, true, lang);
                    strength_meter(ui, passphrase, lang);
                    secret_field(ui, repeat, t.passphrase_repeat, false, lang);
                    if let Some(error) = error.as_ref() {
                        ui.add_space(4.0);
                        wrapped(ui, error, Some(p.error));
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, ButtonKind::Primary, t.save, true).clicked() {
                                submit = true;
                            }
                            if widgets::button(ui, ButtonKind::Secondary, t.cancel, true).clicked()
                            {
                                keep_open = false;
                            }
                        });
                    });
                });
            if modal.should_close() {
                keep_open = false;
            }
            if submit {
                if let Some(problem) = app.passphrase_problem(passphrase, repeat) {
                    *error = Some(problem);
                } else {
                    match app.change_passphrase(passphrase) {
                        Ok(()) => {
                            app.notify(NoticeKind::Success, t.passphrase_changed);
                            keep_open = false;
                        }
                        Err(message) => *error = Some(message),
                    }
                }
            }
        }
    }

    if let Some(next) = next {
        dialog = next;
    }
    // A dialog opened by a follow-up step takes precedence.
    if keep_open && app.vault.dialog.is_none() {
        app.vault.dialog = Some(dialog);
    }
}
