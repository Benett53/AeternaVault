//! The Web tab: switching the web interface on, how it is reached and
//! protected, its users, and the audit log. The interface itself runs in
//! this process while AeternaVault is open (see [`crate::web`]).

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align, Layout, Ui};

use crate::audit::{self, Record};
use crate::config::TlsMode;
use crate::gui::AeternaApp;
use crate::gui::widgets::{self, ButtonKind, NoticeKind};
use crate::i18n::Lang;
use crate::web::users::{self, Role, UserStore};

/// State of the Web tab and of the running interface.
pub struct WebUi {
    pub server: Option<crate::web::WebServer>,
    /// Why the interface did not start, for these settings.
    error: Option<(crate::config::WebSettings, String)>,
    new_name: String,
    new_password: String,
    new_role: Role,
    networks: Option<String>,
    /// A password set for a user, shown once.
    shown_password: Option<(String, String)>,
    /// When the configuration file was last read or written by this window.
    config_mtime: Option<SystemTime>,
    last_check: Instant,
}

impl Default for WebUi {
    fn default() -> Self {
        Self {
            server: None,
            error: None,
            new_name: String::new(),
            new_password: String::new(),
            new_role: Role::Viewer,
            networks: None,
            shown_password: None,
            config_mtime: None,
            last_check: Instant::now(),
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl WebUi {
    /// After this window wrote the configuration: not a change from elsewhere.
    pub fn remember_config(&mut self, config_file: &Path) {
        self.config_mtime = mtime(config_file);
    }

    /// Fills the "add user" fields (interface tests).
    #[cfg(test)]
    pub fn fill_new_user(&mut self, name: &str, password: &str, role: Role) {
        self.new_name = name.into();
        self.new_password = password.into();
        self.new_role = role;
    }
}

fn tr(lang: Lang, en: &'static str, de: &'static str) -> &'static str {
    if lang == Lang::En { en } else { de }
}

impl AeternaApp {
    /// Runs before every frame: picks up settings changed over the web or by
    /// hand, and starts, restarts or stops the web interface to match them.
    pub fn web_tick(&mut self) {
        if self.web.last_check.elapsed() > Duration::from_secs(2) {
            self.web.last_check = Instant::now();
            let now = mtime(&self.paths.config_file);
            if self.web.config_mtime.is_none() {
                self.web.config_mtime = now;
            } else if now != self.web.config_mtime
                && !self.is_config_dirty()
                && self.viewer.is_none()
            {
                self.web.config_mtime = now;
                let loaded = crate::config::load_or_create(&self.paths);
                if loaded.notice.is_none() {
                    tracing::info!("settings changed outside the window; read again");
                    let label = self.config.backup_label.take();
                    self.config = loaded.config;
                    self.config.backup_label = label;
                    if let Some(background) = &self.background {
                        background.service.set_config(&self.config);
                    }
                }
            }
        }

        let wanted = self.background.is_some() && self.viewer.is_none() && self.config.web.enabled;
        let settings = &self.config.web;
        let matches = self
            .web
            .server
            .as_ref()
            .is_some_and(|s| &s.settings == settings);
        if !wanted || (self.web.server.is_some() && !matches) {
            // Dropping stops it.
            self.web.server = None;
        }
        let failed_before = self.web.error.as_ref().is_some_and(|(s, _)| s == settings);
        if wanted && self.web.server.is_none() && !failed_before {
            let control = std::sync::Arc::new(
                self.background
                    .as_ref()
                    .expect("checked above")
                    .service
                    .handle(),
            );
            match crate::web::WebServer::start(&self.paths, settings, control) {
                Ok(server) => {
                    self.web.server = Some(server);
                    self.web.error = None;
                }
                Err(err) => {
                    tracing::warn!("web interface not started: {err}");
                    self.web.error = Some((settings.clone(), err));
                }
            }
        }
    }
}

/// The Web tab.
pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    let lang = app.lang;
    let before = app.config.web.clone();
    let config_file = app.paths.config_file.clone();

    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Web interface", "Web-Oberfläche"),
            tr(
                lang,
                "Shows jobs, backups and problems in a browser, also from other computers, and starts jobs and checks. Users sign in with their own name and password; what they may do depends on their role. Restoring, deleting and changing settings stay in this window. The web interface runs while AeternaVault is open (also in the notification area), or with `aeternavault-cli web serve`.",
                "Zeigt Aufträge, Sicherungen und Probleme im Browser, auch von anderen Computern, und startet Aufträge und Prüfungen. Benutzer melden sich mit eigenem Namen und Passwort an; was sie dürfen, hängt von ihrer Rolle ab. Wiederherstellen, Löschen und Einstellungen bleiben in diesem Fenster. Die Web-Oberfläche läuft, solange AeternaVault geöffnet ist (auch im Infobereich), oder mit `aeternavault-cli web serve`.",
            ),
        );
        let web = &mut app.config.web;
        ui.checkbox(
            &mut web.enabled,
            tr(
                lang,
                "Switch on the web interface",
                "Web-Oberfläche einschalten",
            ),
        );
        ui.add_space(6.0);
        let mut network = !web.local_only();
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(
                &mut network,
                false,
                tr(lang, "Only this computer", "Nur dieser Computer"),
            );
            ui.radio_value(
                &mut network,
                true,
                tr(lang, "From the network", "Aus dem Netzwerk"),
            );
            ui.label(tr(lang, "Port", "Port"));
            ui.add(egui::DragValue::new(&mut web.port).range(1..=65535));
        });
        if network && web.local_only() {
            web.bind = "0.0.0.0".into();
        } else if !network && !web.local_only() {
            web.bind = "127.0.0.1".into();
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("HTTPS");
            let name = |mode: TlsMode| match mode {
                TlsMode::Auto => tr(
                    lang,
                    "automatic (for the network)",
                    "automatisch (fürs Netzwerk)",
                ),
                TlsMode::SelfSigned => tr(
                    lang,
                    "always, own certificate of AeternaVault",
                    "immer, eigenes Zertifikat von AeternaVault",
                ),
                TlsMode::Files => tr(lang, "certificate from files", "Zertifikat aus Dateien"),
                TlsMode::Off => tr(
                    lang,
                    "off (this computer only)",
                    "aus (nur dieser Computer)",
                ),
            };
            egui::ComboBox::from_id_salt("web-tls")
                .selected_text(name(web.tls))
                .show_ui(ui, |ui| {
                    for mode in [
                        TlsMode::Auto,
                        TlsMode::SelfSigned,
                        TlsMode::Files,
                        TlsMode::Off,
                    ] {
                        ui.selectable_value(&mut web.tls, mode, name(mode));
                    }
                });
        });
        if web.tls == TlsMode::Files {
            for (label, path) in [
                (
                    tr(lang, "Certificate (PEM)", "Zertifikat (PEM)"),
                    &mut web.cert_file,
                ),
                (tr(lang, "Key (PEM)", "Schlüssel (PEM)"), &mut web.key_file),
            ] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let mut text = path.display().to_string();
                    if ui
                        .add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY))
                        .changed()
                    {
                        *path = text.into();
                    }
                });
            }
        }
        ui.add_space(6.0);
        let networks = app
            .web
            .networks
            .get_or_insert_with(|| web.allowed_networks.join(", "));
        ui.label(tr(
            lang,
            "Only these networks may connect (e.g. 192.168.1.0/24, 10.0.0.5); empty: any",
            "Nur diese Netze dürfen sich verbinden (z. B. 192.168.1.0/24, 10.0.0.5); leer: alle",
        ));
        let response = ui.add(egui::TextEdit::singleline(networks).desired_width(f32::INFINITY));
        if response.lost_focus() {
            web.allowed_networks = networks
                .split([',', ' ', ';'])
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect();
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(tr(lang, "Sign out after", "Abmelden nach"));
            ui.add(egui::DragValue::new(&mut web.session_minutes).range(1..=1440));
            ui.label(tr(
                lang,
                "minutes without activity",
                "Minuten ohne Aktivität",
            ));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(tr(lang, "Wait", "Nach"));
            ui.add(egui::DragValue::new(&mut web.lockout_attempts).range(1..=100));
            ui.label(tr(lang, "wrong sign-ins:", "falschen Anmeldungen"));
            ui.add(egui::DragValue::new(&mut web.lockout_minutes).range(1..=1440));
            ui.label(tr(lang, "minutes", "Minuten warten"));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(tr(
                lang,
                "Passwords with at least",
                "Passwörter mit mindestens",
            ));
            ui.add(egui::DragValue::new(&mut web.min_password_length).range(8..=128));
            ui.label(tr(lang, "characters", "Zeichen"));
        });
        ui.checkbox(
            &mut web.require_two_factor,
            tr(
                lang,
                "Every user needs a one-time code from an authenticator app",
                "Jeder Benutzer braucht einen Einmalcode aus einer Authenticator-App",
            ),
        );

        ui.add_space(8.0);
        let settings = web.clone();
        match (&app.web.server, &app.web.error) {
            (Some(server), _) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr(lang, "Running at", "Läuft unter"));
                    ui.hyperlink(&server.url);
                });
                if settings.uses_tls()
                    && let Some(fp) = crate::web::tls::fingerprint(&config_file, &settings)
                {
                    widgets::secondary_text(
                        ui,
                        format!(
                            "{} SHA-256 {fp}",
                            tr(
                                lang,
                                "Browsers warn once about this certificate; it is the right one if they show",
                                "Browser warnen einmal vor diesem Zertifikat; es ist das richtige, wenn sie anzeigen:"
                            )
                        ),
                    );
                }
            }
            (None, Some((_, err))) if settings.enabled => {
                ui.colored_label(crate::gui::theme::palette(ui).error, err);
            }
            _ if settings.enabled && app.background.is_none() => {
                widgets::secondary_text(
                    ui,
                    tr(
                        lang,
                        "Starts with the next start of AeternaVault.",
                        "Startet beim nächsten Start von AeternaVault.",
                    ),
                );
            }
            _ => {}
        }
        if let Err(err) = crate::web::check_settings(&settings) {
            ui.colored_label(crate::gui::theme::palette(ui).warning, err);
        }
    });
    if app.config.web != before {
        app.web.error = None;
        app.mark_dirty();
    }

    ui.add_space(14.0);
    users_card(app, ui);
    ui.add_space(14.0);
    audit_card(app, ui);
}

fn users_card(app: &mut AeternaApp, ui: &mut Ui) {
    let lang = app.lang;
    let config_file = app.paths.config_file.clone();
    let min = app.config.web.min_password_length;
    let mut store = UserStore::load(&config_file);
    let mut changed: Option<Record> = None;
    let mut error: Option<String> = None;
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Users", "Benutzer"),
            tr(
                lang,
                "Viewer: overview, backups, activity. Operator: also runs, cancels and switches jobs and checks backups. Administrator: also users and the audit log. A user limited to jobs sees only those jobs and their destinations.",
                "Betrachter: Übersicht, Sicherungen, Verlauf. Bediener: startet, stoppt und schaltet außerdem Aufträge und prüft Sicherungen. Administrator: außerdem Benutzer und Audit-Log. Ein auf Aufträge beschränkter Benutzer sieht nur diese Aufträge und ihre Ziele.",
            ),
        );
        if let Some((name, password)) = &app.web.shown_password {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{} {name}:",
                    tr(lang, "New password for", "Neues Passwort für")
                ));
                ui.add(egui::TextEdit::singleline(&mut password.clone()).desired_width(220.0));
            });
            widgets::secondary_text(
                ui,
                tr(
                    lang,
                    "Shown only now. The user must change it after signing in.",
                    "Wird nur jetzt angezeigt. Der Benutzer muss es nach der Anmeldung ändern.",
                ),
            );
            ui.add_space(6.0);
        }
        if store.users.is_empty() {
            widgets::secondary_text(
                ui,
                tr(
                    lang,
                    "No users yet. The first one should be an administrator.",
                    "Noch keine Benutzer. Der erste sollte ein Administrator sein.",
                ),
            );
        }
        let schedules: Vec<(String, String)> = app
            .config
            .schedules
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    crate::automatic::describe(s, &app.config, lang),
                )
            })
            .collect();
        let mut remove = None;
        let admins = store.admins();
        let total = store.users.len();
        for user in store.users.iter_mut() {
            ui.push_id(("web-user", user.name.clone()), |ui| {
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let last_admin = admins == 1 && user.may(Role::Admin);
                        ui.horizontal(|ui| {
                            ui.strong(&user.name);
                            let before = user.role;
                            egui::ComboBox::from_id_salt("role")
                                .selected_text(user.role.name(lang == Lang::En))
                                .show_ui(ui, |ui| {
                                    for role in Role::ALL {
                                        ui.selectable_value(
                                            &mut user.role,
                                            role,
                                            role.name(lang == Lang::En),
                                        );
                                    }
                                });
                            if user.role != before {
                                if last_admin {
                                    user.role = before;
                                    error = Some(
                                        tr(
                                            lang,
                                            "The last administrator stays administrator.",
                                            "Der letzte Administrator bleibt Administrator.",
                                        )
                                        .into(),
                                    );
                                } else {
                                    changed = Some(
                                        Record::local("user-changed")
                                            .target(user.name.clone())
                                            .details(vec![format!(
                                                "role: {} → {}",
                                                before.name(true),
                                                user.role.name(true)
                                            )]),
                                    );
                                }
                            }
                            widgets::secondary_text(
                                ui,
                                if user.totp.is_some() {
                                    tr(lang, "one-time codes on", "Einmalcodes an")
                                } else {
                                    tr(lang, "no one-time codes", "keine Einmalcodes")
                                },
                            );
                            if user.disabled {
                                ui.colored_label(
                                    crate::gui::theme::palette(ui).error,
                                    tr(lang, "locked", "gesperrt"),
                                );
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::button(
                                    ui,
                                    ButtonKind::Quiet,
                                    tr(lang, "Remove", "Entfernen"),
                                    !last_admin || total == 1,
                                )
                                .clicked()
                                {
                                    remove = Some(user.name.clone());
                                }
                                let lock_label = if user.disabled {
                                    tr(lang, "Unlock", "Entsperren")
                                } else {
                                    tr(lang, "Lock", "Sperren")
                                };
                                if widgets::button(
                                    ui,
                                    ButtonKind::Quiet,
                                    lock_label,
                                    !last_admin || user.disabled,
                                )
                                .clicked()
                                {
                                    user.disabled = !user.disabled;
                                    changed = Some(
                                        Record::local(if user.disabled {
                                            "user-locked"
                                        } else {
                                            "user-unlocked"
                                        })
                                        .target(user.name.clone()),
                                    );
                                }
                                if user.totp.is_some()
                                    && widgets::button(
                                        ui,
                                        ButtonKind::Quiet,
                                        tr(
                                            lang,
                                            "Reset one-time codes",
                                            "Einmalcodes zurücksetzen",
                                        ),
                                        true,
                                    )
                                    .clicked()
                                {
                                    user.totp = None;
                                    changed = Some(
                                        Record::local("user-one-time-codes-reset")
                                            .target(user.name.clone()),
                                    );
                                }
                                if widgets::button(
                                    ui,
                                    ButtonKind::Secondary,
                                    tr(lang, "New password", "Neues Passwort"),
                                    true,
                                )
                                .clicked()
                                {
                                    let password = users::random_password();
                                    user.password = users::hash_password(&password);
                                    user.must_change_password = true;
                                    app.web.shown_password = Some((user.name.clone(), password));
                                    changed = Some(
                                        Record::local("user-password-reset")
                                            .target(user.name.clone()),
                                    );
                                }
                            });
                        });
                        if !schedules.is_empty() {
                            let before = user.jobs.clone();
                            egui::CollapsingHeader::new(if user.jobs.is_empty() {
                                tr(lang, "All jobs", "Alle Aufträge").to_string()
                            } else {
                                format!("{} {}", user.jobs.len(), tr(lang, "jobs", "Aufträge"))
                            })
                            .id_salt("jobs")
                            .show(ui, |ui| {
                                widgets::secondary_text(
                                    ui,
                                    tr(
                                        lang,
                                        "None ticked: all jobs and all destinations.",
                                        "Keiner angehakt: alle Aufträge und Ziele.",
                                    ),
                                );
                                for (id, name) in &schedules {
                                    let mut on = user.jobs.contains(id);
                                    if ui.checkbox(&mut on, name).changed() {
                                        if on {
                                            user.jobs.push(id.clone());
                                        } else {
                                            user.jobs.retain(|j| j != id);
                                        }
                                    }
                                }
                            });
                            if user.jobs != before {
                                changed = Some(
                                    Record::local("user-changed")
                                        .target(user.name.clone())
                                        .details(vec![format!(
                                            "jobs: [{}] → [{}]",
                                            before.join(", "),
                                            user.jobs.join(", ")
                                        )]),
                                );
                            }
                        }
                    });
            });
            ui.add_space(4.0);
        }
        if let Some(name) = remove {
            store.users.retain(|u| u.name != name);
            changed = Some(Record::local("user-removed").target(name));
        }

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.web.new_name)
                    .hint_text(tr(lang, "User name", "Benutzername"))
                    .desired_width(160.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut app.web.new_password)
                    .password(true)
                    .hint_text(tr(lang, "Password", "Passwort"))
                    .desired_width(180.0),
            );
            egui::ComboBox::from_id_salt("new-role")
                .selected_text(app.web.new_role.name(lang == Lang::En))
                .show_ui(ui, |ui| {
                    for role in Role::ALL {
                        ui.selectable_value(
                            &mut app.web.new_role,
                            role,
                            role.name(lang == Lang::En),
                        );
                    }
                });
            if widgets::button(
                ui,
                ButtonKind::Secondary,
                tr(lang, "Add", "Hinzufügen"),
                !app.web.new_name.trim().is_empty(),
            )
            .clicked()
            {
                match store.add(
                    &app.web.new_name,
                    app.web.new_role,
                    &app.web.new_password,
                    min,
                ) {
                    Ok(()) => {
                        changed = Some(
                            Record::local("user-added")
                                .target(app.web.new_name.trim())
                                .details(vec![format!("role: {}", app.web.new_role.name(true))]),
                        );
                        app.web.new_name.clear();
                        app.web.new_password.clear();
                    }
                    Err(err) => error = Some(err),
                }
            }
        });
        if let Some(err) = &error {
            ui.colored_label(crate::gui::theme::palette(ui).error, err);
        }
    });
    if let Some(record) = changed {
        match store.save(&config_file) {
            Ok(()) => audit::record(&config_file, record),
            Err(err) => app.notify(NoticeKind::Error, err.to_string()),
        }
    }
}

fn audit_card(app: &mut AeternaApp, ui: &mut Ui) {
    let lang = app.lang;
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Audit log", "Audit-Log"),
            tr(
                lang,
                "Who changed what, when and from where: sign-ins to the web interface, users, actions over the web, and every change of the settings from this window, the command line or the web. Each entry names a checksum of the one before, so later changes are found.",
                "Wer wann was von wo geändert hat: Anmeldungen an der Web-Oberfläche, Benutzer, Aktionen über das Web und jede Einstellungsänderung aus diesem Fenster, der Kommandozeile oder dem Web. Jeder Eintrag nennt eine Prüfsumme des vorherigen, so fallen spätere Änderungen auf.",
            ),
        );
        match audit::verify(&app.paths.config_file) {
            Ok(v) if v.broken.is_empty() => {
                widgets::secondary_text(
                    ui,
                    format!(
                        "{} {}",
                        v.entries,
                        tr(
                            lang,
                            "entries, none changed afterwards.",
                            "Einträge, keiner nachträglich verändert."
                        )
                    ),
                );
            }
            Ok(v) => {
                ui.colored_label(
                    crate::gui::theme::palette(ui).error,
                    format!(
                        "{} {:?}",
                        tr(
                            lang,
                            "Changed or removed afterwards before line:",
                            "Nachträglich verändert oder entfernt vor Zeile:"
                        ),
                        v.broken
                    ),
                );
            }
            Err(err) => {
                ui.colored_label(crate::gui::theme::palette(ui).error, err.to_string());
            }
        }
        let entries = audit::load(&app.paths.config_file);
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .id_salt("audit")
            .show(ui, |ui| {
                for e in entries.iter().rev().take(200) {
                    let when =
                        e.at.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M:%S");
                    let text = format!(
                        "{when}  {}  {}  {}{} {}",
                        e.actor,
                        e.source,
                        e.action,
                        if e.ok { "" } else { " ✗" },
                        e.target
                    );
                    if e.ok {
                        ui.label(text);
                    } else {
                        ui.colored_label(crate::gui::theme::palette(ui).error, text);
                    }
                    for d in &e.details {
                        widgets::secondary_text(ui, format!("      {d}"));
                    }
                }
            });
    });
}
