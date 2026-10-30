//! Settings: language, appearance, destination, encryption, background,
//! exclusions, advanced options, config file and shortcuts. Every option
//! explains itself in a tooltip.

use eframe::egui::{self, Align, Layout, Ui};

use crate::config::{Appearance, BackupMode, EncryptionScope, LanguageSetting, default_excludes};
use crate::engine::scan::Excludes;
use crate::gui::widgets::{self, ButtonKind, NoticeKind};
use crate::gui::{AeternaApp, theme};
use crate::i18n::Lang;
use crate::platform;

/// How the backups are encrypted, where the key is, and how to get at the data
/// without this app.
pub fn encryption_info_card(app: &mut AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    let lang = app.lang;
    let p = *theme::palette(ui);
    let Some(header) = app.vault.header.clone() else {
        return;
    };
    let destination = app.config.destination.clone();
    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.enc_info_title);
        let cipher = header
            .data_cipher()
            .map(|c| c.display_name())
            .unwrap_or("?");
        let kdf = header
            .slot(crate::engine::crypto::SlotKind::Passphrase)
            .map(|s| s.kdf);
        egui::Grid::new("encryption-info")
            .num_columns(2)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                widgets::secondary_text(ui, t.enc_info_cipher);
                ui.label(lang.cipher_summary(cipher));
                ui.end_row();
                widgets::secondary_text(ui, t.enc_info_passphrase);
                ui.label(match kdf {
                    Some(kdf) => lang.kdf_summary(kdf.memory_kib, kdf.iterations),
                    None => "—".to_string(),
                });
                ui.end_row();
                widgets::secondary_text(ui, t.enc_info_hidden);
                ui.label(t.enc_info_hidden_value);
                ui.end_row();
                widgets::secondary_text(ui, t.enc_info_key);
                ui.label(if app.vault.remembered {
                    t.enc_info_key_remembered
                } else {
                    t.enc_info_key_not_remembered
                });
                ui.end_row();
                widgets::secondary_text(ui, t.enc_info_location);
                ui.horizontal(|ui| {
                    let dir = crate::engine::vault::vault_dir(&destination);
                    ui.label(
                        egui::RichText::new(dir.display().to_string())
                            .size(14.0)
                            .color(p.text_secondary),
                    );
                    if widgets::button(ui, ButtonKind::Quiet, "↗", true)
                        .on_hover_text(t.open_folder)
                        .clicked()
                    {
                        platform::open_in_file_manager(&dir);
                    }
                });
                ui.end_row();
            });
        ui.add_space(8.0);
        egui::CollapsingHeader::new(egui::RichText::new(t.enc_without_app_title).color(p.text))
            .id_salt("encryption-without-app")
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(t.enc_without_app_text).size(14.5)).wrap(),
                );
                ui.add_space(4.0);
                let target = if cfg!(windows) { "D:\\Restored" } else { "~/Restored" };
                let command = format!(
                    "aeternavault-cli restore latest --destination \"{}\" --to {target}",
                    destination.display()
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&command).monospace().size(14.0))
                            .wrap(),
                    );
                    if widgets::button(ui, ButtonKind::Quiet, t.copy, true).clicked() {
                        ui.ctx().copy_text(command.clone());
                    }
                });
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(t.enc_without_app_script).size(14.5))
                        .wrap(),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to(
                        "docs/ENCRYPTION.md",
                        "https://github.com/Benett53/AeternaVault/blob/main/docs/ENCRYPTION.md",
                    );
                    widgets::secondary_text(ui, "·");
                    ui.hyperlink_to(
                        "tools/aeterna-decrypt.py",
                        "https://github.com/Benett53/AeternaVault/blob/main/tools/aeterna-decrypt.py",
                    );
                });
            });
    });
}

pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();

    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.settings_general);
        egui::Grid::new("settings-general")
            .num_columns(2)
            .spacing([28.0, 10.0])
            .show(ui, |ui| {
                widgets::secondary_text(ui, t.language).on_hover_text(t.tip_language);
                ui.horizontal(|ui| {
                    let before = app.config.language;
                    ui.radio_value(
                        &mut app.config.language,
                        LanguageSetting::Auto,
                        t.language_auto,
                    );
                    ui.radio_value(&mut app.config.language, LanguageSetting::En, "English");
                    ui.radio_value(&mut app.config.language, LanguageSetting::De, "Deutsch");
                    if before != app.config.language {
                        app.lang = Lang::resolve(app.config.language);
                        app.mark_dirty();
                    }
                });
                ui.end_row();

                widgets::secondary_text(ui, t.appearance).on_hover_text(t.tip_appearance);
                ui.horizontal_wrapped(|ui| {
                    let before = app.config.appearance;
                    ui.radio_value(
                        &mut app.config.appearance,
                        Appearance::System,
                        t.appearance_system,
                    );
                    ui.radio_value(
                        &mut app.config.appearance,
                        Appearance::Light,
                        t.appearance_light,
                    );
                    ui.radio_value(
                        &mut app.config.appearance,
                        Appearance::Dark,
                        t.appearance_dark,
                    );
                    ui.radio_value(
                        &mut app.config.appearance,
                        Appearance::Black,
                        t.appearance_black,
                    )
                    .on_hover_text(t.tip_black);
                    if before != app.config.appearance {
                        theme::apply(&ctx, app.config.appearance);
                        app.mark_dirty();
                    }
                });
                ui.end_row();

                widgets::secondary_text(ui, t.interface_size).on_hover_text(t.tip_interface_size);
                ui.horizontal(|ui| {
                    let before = app.config.interface_scale;
                    egui::ComboBox::from_id_salt("interface-scale")
                        .selected_text(format!("{} %", app.config.interface_scale))
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            for scale in [90u16, 100, 110, 125, 150] {
                                ui.selectable_value(
                                    &mut app.config.interface_scale,
                                    scale,
                                    format!("{scale} %"),
                                );
                            }
                        });
                    if before != app.config.interface_scale {
                        ctx.set_zoom_factor(app.config.zoom_factor());
                        app.mark_dirty();
                    }
                });
                ui.end_row();
            });
    });

    ui.add_space(14.0);

    destination_card(app, ui);
    ui.add_space(14.0);
    encryption_card(app, ui);
    ui.add_space(14.0);

    if app.vault.header.is_some() {
        encryption_info_card(app, ui);
        ui.add_space(14.0);
    }

    background_card(app, ui);
    ui.add_space(14.0);
    updates_card(app, ui);
    ui.add_space(14.0);

    widgets::card(ui, |ui| {
        widgets::section_title_info(ui, t.exclusions_title, t.exclusions_hint);
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::multiline(&mut app.settings.exclude_text)
                .font(egui::TextStyle::Monospace)
                .desired_rows(7)
                .desired_width(f32::INFINITY),
        );
        if !app.settings.invalid_patterns.is_empty() {
            let p = *theme::palette(ui);
            ui.label(
                egui::RichText::new(app.lang.invalid_patterns(&app.settings.invalid_patterns))
                    .color(p.warning),
            );
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let patterns: Vec<String> = app
                .settings
                .exclude_text
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            let changed = patterns != app.config.exclude;
            if widgets::button(ui, ButtonKind::Secondary, t.apply, changed).clicked() {
                app.settings.invalid_patterns = Excludes::new(patterns.iter()).invalid;
                app.config.exclude = patterns;
                app.mark_dirty();
            }
            if widgets::button(ui, ButtonKind::Quiet, t.reset_defaults, true).clicked() {
                app.settings.exclude_text = default_excludes().join("\n");
            }
        });
    });

    ui.add_space(14.0);

    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.advanced);
        let before = app.config.advanced.clone();
        ui.checkbox(
            &mut app.config.advanced.skip_online_only_files,
            t.adv_skip_online,
        )
        .on_hover_text(t.tip_skip_online);
        ui.checkbox(&mut app.config.advanced.hardlink_unchanged, t.adv_hardlinks)
            .on_hover_text(t.tip_hardlinks);
        ui.checkbox(&mut app.config.advanced.confirm_before_start, t.adv_confirm)
            .on_hover_text(t.tip_confirm);
        ui.checkbox(
            &mut app.config.advanced.verify_on_restore,
            t.verify_checksums,
        )
        .on_hover_text(t.tip_verify);
        ui.checkbox(
            &mut app.config.advanced.compatibility_graphics,
            t.adv_compatibility_graphics,
        )
        .on_hover_text(t.adv_compatibility_hint);
        if before != app.config.advanced {
            app.mark_dirty();
        }
    });

    ui.add_space(14.0);

    widgets::card(ui, |ui| {
        widgets::section_title_info(ui, t.config_title, t.config_hint);
        ui.add_space(4.0);
        ui.label(egui::RichText::new(app.paths.config_file.display().to_string()).monospace());
        if app.paths.portable {
            widgets::secondary_text(ui, t.portable_mode);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, ButtonKind::Secondary, t.open_in_editor, true).clicked() {
                platform::open_in_editor(&app.paths.config_file);
            }
            if widgets::button(ui, ButtonKind::Secondary, t.reload, !app.is_busy()).clicked() {
                app.reload_config(&ctx);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Quiet, t.open_log_folder, true).clicked() {
                    if let Err(err) = std::fs::create_dir_all(&app.paths.log_dir) {
                        app.notify(NoticeKind::Warning, err.to_string());
                    }
                    platform::open_in_file_manager(&app.paths.log_dir);
                }
            });
        });
    });
    ui.add_space(14.0);
    shortcuts_card(app, ui);
    ui.add_space(12.0);
}

/// Where backups are kept and how.
fn destination_card(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.destination_title);
        ui.horizontal(|ui| {
            let destination = app.config.destination.display().to_string();
            if destination.is_empty() {
                widgets::secondary_text(ui, t.destination_not_set);
            } else {
                widgets::strong_text(ui, destination);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Secondary, t.choose, !app.is_busy()).clicked() {
                    app.choose_destination(&ctx);
                }
            });
        });
        crate::gui::views::backup::destination_status(app, ui);
        ui.add_space(4.0);
        let mut app_folder = app.config.advanced.destination_app_folder;
        if ui
            .add_enabled(
                !app.is_busy(),
                egui::Checkbox::new(&mut app_folder, t.destination_app_folder),
            )
            .on_hover_text(t.tip_app_folder)
            .changed()
        {
            app.set_destination_app_folder(&ctx, app_folder);
        }

        ui.add_space(12.0);
        widgets::secondary_text(ui, t.mode_title);
        let before = app.config.mode;
        ui.radio_value(
            &mut app.config.mode,
            BackupMode::Incremental,
            t.mode_incremental,
        )
        .on_hover_text(t.mode_incremental_hint);
        ui.radio_value(&mut app.config.mode, BackupMode::Full, t.mode_full)
            .on_hover_text(t.mode_full_hint);
        if app.config.mode != before {
            app.mark_dirty();
        }
    });
}

/// What is encrypted, the method, the key and the recovery key.
fn encryption_card(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    let lang = app.lang;
    let p = *theme::palette(ui);

    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.enc_settings_title);
        if app.config.encryption.everything() {
            widgets::strong_text(ui, t.enc_status_on);
        } else if app.config.encryption.selected() {
            widgets::strong_text(ui, t.enc_status_selected);
        } else {
            widgets::secondary_text(ui, t.enc_status_off_settings);
        }

        ui.add_space(10.0);
        widgets::secondary_text(ui, t.enc_scope_title);
        let before = app.config.encryption.clone();
        ui.radio_value(
            &mut app.config.encryption.scope,
            EncryptionScope::Everything,
            t.scope_encrypt_everything,
        )
        .on_hover_text(t.tip_scope_everything);
        ui.radio_value(
            &mut app.config.encryption.scope,
            EncryptionScope::Selected,
            t.scope_encrypt_selected,
        )
        .on_hover_text(t.tip_scope_selected);
        if app.config.encryption.scope == EncryptionScope::Selected {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(26.0);
                widgets::lock_icon(
                    ui,
                    ui.cursor().left_center() + egui::vec2(6.0, 9.0),
                    p.accent,
                );
                ui.add_space(16.0);
                widgets::secondary_text(ui, t.scope_selected_hint);
            });
        }

        ui.add_space(10.0);
        widgets::secondary_text(ui, t.enc_method_title);
        match &app.vault.header {
            Some(header) => {
                let cipher = header
                    .data_cipher()
                    .map(|c| c.display_name())
                    .unwrap_or("?");
                ui.add(
                    egui::Label::new(egui::RichText::new(lang.method_fixed(cipher)).size(14.5))
                        .wrap(),
                );
            }
            None => {
                let current = app.config.encryption.vault_options();
                egui::CollapsingHeader::new(
                    egui::RichText::new(lang.method_summary(current.cipher, current.kdf))
                        .size(14.5),
                )
                .id_salt("encryption-method")
                .show(ui, |ui| {
                    crate::gui::dialogs::encryption_method(ui, &mut app.config.encryption, lang);
                });
            }
        }
        if app.config.encryption != before {
            app.mark_dirty();
        }

        if app.vault.header.is_some() {
            ui.add_space(10.0);
            let mut remembered = app.vault.remembered;
            if ui
                .checkbox(&mut remembered, t.remember_on_computer)
                .on_hover_text(t.tip_remember)
                .changed()
            {
                app.set_remembered(remembered);
            }
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if widgets::button(
                    ui,
                    ButtonKind::Secondary,
                    t.change_passphrase,
                    !app.is_busy(),
                )
                .clicked()
                {
                    if app.vault.key.is_some() {
                        app.vault.dialog = Some(crate::gui::VaultDialog::Change {
                            passphrase: String::new(),
                            repeat: String::new(),
                            error: None,
                        });
                    } else {
                        app.open_unlock(crate::gui::AfterUnlock::ChangePassphrase);
                    }
                }
                if widgets::button(ui, ButtonKind::Secondary, t.test_recovery, true).clicked() {
                    app.vault.dialog = Some(crate::gui::VaultDialog::TestRecovery {
                        secret: String::new(),
                        result: None,
                    });
                }
                if widgets::button(ui, ButtonKind::Quiet, t.replace_recovery, !app.is_busy())
                    .on_hover_text(t.replace_recovery_hint)
                    .clicked()
                {
                    app.replace_recovery_key();
                }
                if app.vault.key.is_some()
                    && !app.vault.remembered
                    && widgets::button(ui, ButtonKind::Quiet, t.lock_now, true).clicked()
                {
                    app.lock_vault(&ctx);
                }
            });
        }
        if platform::file_association::supported() {
            ui.add_space(8.0);
            let mut on = app.config.advanced.open_by_double_click;
            if ui
                .checkbox(&mut on, t.open_by_double_click)
                .on_hover_text(t.open_by_double_click_hint)
                .changed()
            {
                app.set_open_by_double_click(on);
            }
        }
    });
}

/// Starting with the system, background jobs and power.
fn background_card(app: &mut AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    let has_tray = app.has_tray();
    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.background_title);
        if cfg!(windows) {
            let autostart = app.background.as_ref().map(|b| b.autostart);
            let mut on = autostart.unwrap_or(false);
            if ui
                .add_enabled(
                    autostart.is_some(),
                    egui::Checkbox::new(&mut on, t.start_with_windows),
                )
                .on_hover_text(t.start_with_windows_hint)
                .changed()
            {
                app.set_autostart(on);
            }
        }
        if platform::systemd::supported() {
            let mut on = platform::systemd::is_installed();
            if ui
                .checkbox(&mut on, t.systemd_timer)
                .on_hover_text(t.systemd_timer_hint)
                .changed()
            {
                app.set_systemd_timer(on);
            }
        }
        let before = app.config.background.clone();
        if has_tray {
            ui.checkbox(&mut app.config.background.keep_running, t.keep_running)
                .on_hover_text(t.keep_running_hint);
        }
        ui.checkbox(&mut app.config.background.only_on_ac_power, t.only_ac)
            .on_hover_text(t.only_ac_hint);
        if app.config.background != before {
            app.mark_dirty();
        }
        if platform::file_association::supported() {
            let mut on = app.config.advanced.explorer_menu;
            if ui
                .checkbox(&mut on, t.explorer_menu)
                .on_hover_text(t.explorer_menu_hint)
                .changed()
            {
                app.set_explorer_menu(on);
            }
        }
    });
}

/// Version, update state and the button that acts on it.
fn updates_card(app: &mut AeternaApp, ui: &mut Ui) {
    use crate::update::{CURRENT_VERSION, Status};

    let t = app.lang.t();
    let status = app.updater.status();
    let busy_task = app.is_busy();
    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.updates_title);
        let p = *theme::palette(ui);
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("AeternaVault {CURRENT_VERSION}"));
            let line = match &status {
                Status::Idle => None,
                Status::Checking => Some((t.update_checking.to_string(), p.text_secondary)),
                Status::UpToDate => Some((t.update_up_to_date.to_string(), p.text_secondary)),
                Status::Available(release) => Some((
                    format!("{} {}", t.update_available, release.version),
                    p.accent,
                )),
                Status::Downloading(received, Some(total)) if *total > 0 => Some((
                    format!("{} {} %", t.update_downloading, received * 100 / total),
                    p.text_secondary,
                )),
                Status::Downloading(..) => {
                    Some((t.update_downloading.to_string(), p.text_secondary))
                }
                Status::Installing => Some((t.update_installing.to_string(), p.text_secondary)),
                Status::Failed(error) => Some((error.clone(), p.warning)),
            };
            if let Some((text, color)) = line {
                ui.label(egui::RichText::new(text).color(color));
            }
        });
        ui.add_space(6.0);
        let working = matches!(
            status,
            Status::Checking | Status::Downloading(..) | Status::Installing
        );
        let ctx = ui.ctx().clone();
        let wake = move || ctx.request_repaint();
        ui.horizontal(|ui| match &status {
            Status::Available(release) => {
                let enabled = !working && !busy_task;
                let label = format!("{} {}", t.update_install, release.version);
                if widgets::button(ui, ButtonKind::Secondary, &label, enabled).clicked() {
                    app.updater.install(release.clone(), wake);
                }
                if busy_task {
                    widgets::secondary_text(ui, t.update_wait_for_task);
                }
            }
            _ => {
                if widgets::button(ui, ButtonKind::Secondary, t.update_check, !working).clicked() {
                    app.updater.check(wake);
                }
            }
        });
        if working {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        ui.add_space(4.0);
        let before = app.config.updates.check_automatically;
        ui.checkbox(
            &mut app.config.updates.check_automatically,
            t.update_automatic,
        );
        if before != app.config.updates.check_automatically {
            app.updater
                .set_automatic(app.config.updates.check_automatically);
            app.mark_dirty();
        }
    });
}

/// The keyboard and mouse shortcuts, for reference.
fn shortcuts_card(app: &AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.shortcuts_title);
        egui::Grid::new("shortcuts")
            .num_columns(2)
            .spacing([28.0, 6.0])
            .show(ui, |ui| {
                for (keys, what) in t.shortcuts {
                    ui.label(egui::RichText::new(*keys).monospace());
                    widgets::secondary_text(ui, *what);
                    ui.end_row();
                }
            });
    });
}
