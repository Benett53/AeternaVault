//! The Web tab: switching the web interface on, how it is reached and
//! protected, sign-in with LDAP / Active Directory, its users, and the audit
//! log. The interface itself runs in this process while AeternaVault is open
//! (see [`crate::web`]).
//!
//! Settings are edited as a draft and take effect with "Apply", so the
//! interface is not restarted (and everybody signed out) at every keystroke.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::audit::{self, Record};
use crate::config::{TlsMode, WebSettings};
use crate::gui::AeternaApp;
use crate::gui::theme::palette;
use crate::gui::widgets::{self, ButtonKind, NoticeKind};
use crate::i18n::Lang;
use crate::web::ldap::{self, LdapSettings};
use crate::web::users::{self, Role, UserSource, UserStore};

/// A message under a section that stays until the next action.
#[derive(Clone)]
enum Note {
    Good(String),
    Warn(String),
    Bad(String),
}

/// State of the Web tab and of the running interface.
pub struct WebUi {
    pub server: Option<crate::web::WebServer>,
    /// Why the interface did not start, for these settings.
    error: Option<(WebSettings, String)>,
    /// Settings being edited, and what they were when editing began.
    draft: Option<WebSettings>,
    draft_base: Option<WebSettings>,
    apply_note: Option<Note>,
    new_name: String,
    new_password: String,
    new_role: Role,
    user_note: Option<Note>,
    /// A password set for a user, shown once: (user, password).
    shown_password: Option<(String, String)>,
    /// A user whose removal waits for a second click.
    confirm_remove: Option<String>,
    /// The e-mail address being edited in a user's menu: (user, text).
    email_edit: Option<(String, String)>,
    ldap_test_user: String,
    ldap_test_password: String,
    ldap_test: Arc<Mutex<Option<Result<String, String>>>>,
    ldap_testing: bool,
    audit_filter: String,
    /// Audit entries shown (more after "Show more").
    audit_shown: usize,
    /// When the configuration file was last read or written by this window.
    config_mtime: Option<SystemTime>,
    last_check: Instant,
}

impl Default for WebUi {
    fn default() -> Self {
        Self {
            server: None,
            error: None,
            draft: None,
            draft_base: None,
            apply_note: None,
            new_name: String::new(),
            new_password: String::new(),
            new_role: Role::Viewer,
            user_note: None,
            shown_password: None,
            confirm_remove: None,
            email_edit: None,
            ldap_test_user: String::new(),
            ldap_test_password: String::new(),
            ldap_test: Arc::new(Mutex::new(None)),
            ldap_testing: false,
            audit_filter: String::new(),
            audit_shown: 50,
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

        let wanted = self.background.is_some()
            && self.viewer.is_none()
            && self.config.web_on()
            && self.config.web.enabled;
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
            let control = Arc::new(
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

fn show_note(ui: &mut Ui, note: &Option<Note>) {
    let p = *palette(ui);
    match note {
        Some(Note::Good(t)) => {
            ui.colored_label(p.success, t);
        }
        Some(Note::Warn(t)) => {
            ui.colored_label(p.warning, t);
        }
        Some(Note::Bad(t)) => {
            ui.colored_label(p.error, t);
        }
        None => {}
    }
}

/// A secondary text that wraps within its column (also in grid cells,
/// where text would otherwise run past the edge).
fn note(ui: &mut Ui, text: impl Into<String>) {
    let p = *palette(ui);
    let width = ui.available_width().max(160.0);
    ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
        ui.set_max_width(width);
        ui.add(
            egui::Label::new(
                RichText::new(text.into())
                    .size(13.0)
                    .color(p.text_secondary),
            )
            .wrap(),
        );
    });
}

/// A small coloured status dot (the fonts have no "●").
fn dot(ui: &mut Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

/// A label in the left column of a settings grid.
fn row_label(ui: &mut Ui, text: &str) {
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.label(RichText::new(text).strong());
    });
}

fn path_field(ui: &mut Ui, path: &mut PathBuf, choose: &str, filter: (&str, &[&str])) {
    ui.horizontal(|ui| {
        let mut text = path.display().to_string();
        if ui
            .add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width((ui.available_width() - 120.0).clamp(160.0, 320.0)),
            )
            .changed()
        {
            *path = text.trim().into();
        }
        if widgets::button(ui, ButtonKind::Quiet, choose, true).clicked()
            && let Some(file) = rfd::FileDialog::new()
                .add_filter(filter.0, filter.1)
                .pick_file()
        {
            *path = file;
        }
    });
}

/// The Web tab.
pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    let lang = app.lang;
    // Start a draft, or follow changes made elsewhere while nothing is edited.
    let unchanged = app.web.draft == app.web.draft_base;
    if app.web.draft.is_none()
        || (unchanged && app.web.draft_base.as_ref() != Some(&app.config.web))
    {
        app.web.draft = Some(app.config.web.clone());
        app.web.draft_base = Some(app.config.web.clone());
    }

    status_card(app, ui);
    apply_bar(app, ui, "top");
    ui.add_space(14.0);
    let mut draft = app.web.draft.take().expect("set above");
    let email_channels: Vec<String> = app
        .config
        .notifications
        .channels
        .iter()
        .filter(|c| c.kind == crate::notify::ChannelKind::Email && c.enabled)
        .map(|c| {
            if c.name.trim().is_empty() {
                c.smtp_host.clone()
            } else {
                c.name.clone()
            }
        })
        .collect();
    widgets::split(ui, |ui, column| {
        if column == 0 {
            connection_card(app, ui, &mut draft);
            ui.add_space(14.0);
            security_card(lang, ui, &mut draft, &email_channels);
        } else {
            ldap_card(app, ui, &mut draft);
        }
    });
    app.web.draft = Some(draft);
    apply_bar(app, ui, "bottom");
    ui.add_space(14.0);
    users_card(app, ui);
    ui.add_space(14.0);
    audit_card(app, ui);
}

fn status_card(app: &mut AeternaApp, ui: &mut Ui) {
    let lang = app.lang;
    let p = *palette(ui);
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Web interface", "Web-Oberfläche"),
            tr(
                lang,
                "Shows jobs, backups and problems in a browser, also from other computers, and starts jobs and checks. Users sign in with their own name and password, or with their directory account; what they may do depends on their role. Restoring, deleting and changing settings stay in this window. The web interface runs while AeternaVault is open (also in the notification area), or with `aeternavault-cli web serve`.",
                "Zeigt Aufträge, Sicherungen und Probleme im Browser, auch von anderen Computern, und startet Aufträge und Prüfungen. Benutzer melden sich mit eigenem Namen und Passwort oder mit ihrem Verzeichniskonto an; was sie dürfen, hängt von ihrer Rolle ab. Wiederherstellen, Löschen und Einstellungen bleiben in diesem Fenster. Die Web-Oberfläche läuft, solange AeternaVault geöffnet ist (auch im Infobereich), oder mit `aeternavault-cli web serve`.",
            ),
        );
        let mut on = app.config.web.enabled;
        if ui
            .checkbox(
                &mut on,
                RichText::new(tr(
                    lang,
                    "Switch on the web interface",
                    "Web-Oberfläche einschalten",
                ))
                .size(16.0),
            )
            .changed()
        {
            app.config.web.enabled = on;
            if let Some(d) = app.web.draft.as_mut() {
                d.enabled = on;
            }
            if let Some(b) = app.web.draft_base.as_mut() {
                b.enabled = on;
            }
            app.web.error = None;
            app.mark_dirty();
        }
        ui.add_space(6.0);
        let users = UserStore::load(&app.paths.config_file);
        match (&app.web.server, &app.web.error) {
            (Some(server), _) => {
                let url = server.url.clone();
                ui.horizontal_wrapped(|ui| {
                    dot(ui, p.success);
                    ui.label(tr(lang, "Running at", "Läuft unter"));
                    ui.hyperlink(&url);
                    if widgets::button(ui, ButtonKind::Quiet, tr(lang, "Copy", "Kopieren"), true)
                        .clicked()
                    {
                        ui.ctx().copy_text(url.clone());
                    }
                });
                if users.users.is_empty() && !app.config.web.ldap.enabled {
                    ui.colored_label(
                        p.warning,
                        tr(
                            lang,
                            "Nobody can sign in yet: add a user below.",
                            "Noch kann sich niemand anmelden: unten einen Benutzer hinzufügen.",
                        ),
                    );
                }
            }
            (None, Some((_, err))) if app.config.web.enabled => {
                ui.horizontal_wrapped(|ui| {
                    dot(ui, p.error);
                    ui.colored_label(
                        p.error,
                        format!("{} {err}", tr(lang, "Not started:", "Nicht gestartet:")),
                    );
                });
            }
            _ if app.config.web.enabled && app.background.is_none() => {
                note(
                    ui,
                    tr(
                        lang,
                        "Starts with the next start of AeternaVault.",
                        "Startet beim nächsten Start von AeternaVault.",
                    ),
                );
            }
            _ => {
                note(
                    ui,
                    tr(
                        lang,
                        "Off. No connections are accepted.",
                        "Aus. Es werden keine Verbindungen angenommen.",
                    ),
                );
            }
        }

        // At a glance.
        let day = chrono::Utc::now() - chrono::Duration::hours(24);
        let log = audit::load(&app.paths.config_file);
        let (good, bad) = log
            .iter()
            .filter(|e| e.at > day && e.action == "sign-in")
            .fold(
                (0, 0),
                |(g, b), e| if e.ok { (g + 1, b) } else { (g, b + 1) },
            );
        let active = app
            .web
            .server
            .as_ref()
            .map_or(0, |s| s.active_sessions().len());
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            widgets::stat_tile(
                ui,
                &users.users.len().to_string(),
                tr(lang, "users", "Benutzer"),
                p.text,
            );
            widgets::stat_tile(
                ui,
                &active.to_string(),
                tr(lang, "signed in now", "jetzt angemeldet"),
                p.accent,
            );
            widgets::stat_tile(
                ui,
                &good.to_string(),
                tr(lang, "sign-ins in 24 h", "Anmeldungen in 24 h"),
                p.success,
            );
            widgets::stat_tile(
                ui,
                &bad.to_string(),
                tr(lang, "failed in 24 h", "Fehlversuche in 24 h"),
                if bad > 0 { p.error } else { p.text_secondary },
            );
        });
    });
}

fn connection_card(app: &mut AeternaApp, ui: &mut Ui, web: &mut WebSettings) {
    let lang = app.lang;
    let p = *palette(ui);
    let config_file = app.paths.config_file.clone();
    widgets::card(ui, |ui| {
        widgets::section_title(ui, tr(lang, "Connection", "Verbindung"));
        egui::Grid::new("web-connection")
            .num_columns(2)
            .spacing([24.0, 12.0])
            .show(ui, |ui| {
                row_label(ui, tr(lang, "Reachable from", "Erreichbar von"));
                let mut network = !web.local_only();
                ui.vertical(|ui| {
                    widgets::choice(ui, &mut network, false, tr(lang, "this computer only", "nur diesem Computer"));
                    widgets::choice(ui, &mut network, true, tr(lang, "other computers in the network", "anderen Computern im Netzwerk"));
                });
                if network && web.local_only() {
                    web.bind = "0.0.0.0".into();
                } else if !network && !web.local_only() {
                    web.bind = "127.0.0.1".into();
                }
                ui.end_row();

                row_label(ui, "Port");
                widgets::number(ui, &mut web.port, 1..=65535);
                ui.end_row();

                row_label(ui, tr(lang, "Encryption (HTTPS)", "Verschlüsselung (HTTPS)"));
                ui.vertical(|ui| {
                    let own = tr(lang, "certificate made by AeternaVault", "von AeternaVault erstelltes Zertifikat");
                    if web.local_only() {
                        if web.tls == TlsMode::Off {
                            web.tls = TlsMode::Auto;
                        }
                        widgets::choice(ui, &mut web.tls, TlsMode::Auto, tr(lang, "none needed (this computer only)", "nicht nötig (nur dieser Computer)"));
                    } else if matches!(web.tls, TlsMode::Auto | TlsMode::Off) {
                        web.tls = TlsMode::SelfSigned;
                    }
                    widgets::choice(ui, &mut web.tls, TlsMode::SelfSigned, own);
                    widgets::choice(ui,
                        &mut web.tls,
                        TlsMode::Files,
                        tr(lang, "own certificate (e.g. from the company CA)", "eigenes Zertifikat (z. B. von der Firmen-CA)"),
                    );
                });
                ui.end_row();

                if web.tls == TlsMode::Files {
                    let choose = tr(lang, "Choose…", "Auswählen…");
                    row_label(ui, tr(lang, "Certificate (PEM)", "Zertifikat (PEM)"));
                    path_field(ui, &mut web.cert_file, choose, ("PEM", &["pem", "crt", "cer"]));
                    ui.end_row();
                    row_label(ui, tr(lang, "Private key (PEM)", "Privater Schlüssel (PEM)"));
                    path_field(ui, &mut web.key_file, choose, ("PEM", &["pem", "key"]));
                    ui.end_row();
                }

                if web.uses_tls() && web.tls != TlsMode::Files {
                    row_label(ui, tr(lang, "Fingerprint", "Fingerabdruck"));
                    ui.vertical(|ui| match crate::web::tls::fingerprint(&config_file, web) {
                        Some(fp) => {
                            ui.label(RichText::new(&fp).monospace().size(12.0));
                            ui.horizontal(|ui| {
                                if widgets::button(ui, ButtonKind::Quiet, tr(lang, "Copy", "Kopieren"), true).clicked() {
                                    ui.ctx().copy_text(fp.clone());
                                }
                                if widgets::button(ui, ButtonKind::Quiet, tr(lang, "Make a new one", "Neu erstellen"), true).clicked() {
                                    let _ = crate::web::tls::renew(&config_file);
                                    audit::record(&config_file, Record::local("web-certificate-renewed"));
                                    app.web.server = None;
                                }
                            });
                            note(
                                ui,
                                tr(lang, "Browsers warn once about this certificate. Compare this fingerprint with the one the browser shows.", "Browser warnen einmal vor diesem Zertifikat. Vergleiche diesen Fingerabdruck mit dem, den der Browser anzeigt."),
                            );
                        }
                        None => {
                            note(
                                ui,
                                tr(lang, "Made when the web interface starts.", "Wird beim Start der Web-Oberfläche erstellt."),
                            );
                        }
                    });
                    ui.end_row();
                }

                if !web.local_only() {
                    row_label(ui, tr(lang, "Allowed networks", "Erlaubte Netze"));
                    ui.vertical(|ui| {
                        let mut remove = None;
                        for (i, net) in web.allowed_networks.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                let valid = net.trim().parse::<crate::web::Network>();
                                let empty = net.trim().is_empty();
                                let mut edit = egui::TextEdit::singleline(net)
                                    .hint_text("192.168.1.0/24")
                                    .desired_width(200.0);
                                if valid.is_err() && !empty {
                                    edit = edit.text_color(p.error);
                                }
                                ui.add(edit);
                                if widgets::button(ui, ButtonKind::Quiet, "×", true).clicked() {
                                    remove = Some(i);
                                }
                                if valid.is_err() && !empty {
                                    ui.colored_label(
                                        p.error,
                                        tr(lang, "not an address or network (e.g. 192.168.1.0/24)", "keine Adresse und kein Netz (z. B. 192.168.1.0/24)"),
                                    );
                                }
                            });
                        }
                        if let Some(i) = remove {
                            web.allowed_networks.remove(i);
                        }
                        if widgets::button(ui, ButtonKind::Quiet, tr(lang, "+ Add network", "+ Netz hinzufügen"), true).clicked() {
                            web.allowed_networks.push(String::new());
                        }
                        note(
                            ui,
                            if web.allowed_networks.is_empty() {
                                tr(lang, "None: every network may connect. Example: 192.168.1.0/24 or a single address.", "Keine: jedes Netz darf sich verbinden. Beispiel: 192.168.1.0/24 oder eine einzelne Adresse.")
                            } else {
                                tr(lang, "Only these may connect; this computer always may.", "Nur diese dürfen sich verbinden; dieser Computer immer.")
                            },
                        );
                    });
                    ui.end_row();
                }
            });
    });
}

fn security_card(lang: Lang, ui: &mut Ui, web: &mut WebSettings, email_channels: &[String]) {
    widgets::card(ui, |ui| {
        widgets::section_title(
            ui,
            tr(lang, "Sign-in and security", "Anmeldung und Sicherheit"),
        );
        egui::Grid::new("web-security")
            .num_columns(2)
            .spacing([24.0, 12.0])
            .show(ui, |ui| {
                row_label(ui, tr(lang, "Sign out after", "Abmelden nach"));
                ui.horizontal(|ui| {
                    widgets::number(ui, &mut web.session_minutes, 1..=1440);
                    ui.label(tr(lang, "minutes idle", "Min. Inaktivität"));
                });
                ui.end_row();

                row_label(ui, tr(lang, "Lock after", "Sperre nach"));
                ui.horizontal(|ui| {
                    widgets::number(ui, &mut web.lockout_attempts, 1..=100);
                    ui.label(tr(lang, "wrong sign-ins", "Fehlversuchen"));
                });
                ui.end_row();

                row_label(ui, tr(lang, "Locked for", "Sperrdauer"));
                ui.horizontal(|ui| {
                    widgets::number(ui, &mut web.lockout_minutes, 1..=1440);
                    ui.label(tr(lang, "minutes", "Minuten"));
                });
                ui.end_row();

                row_label(ui, tr(lang, "Password length", "Passwortlänge"));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        widgets::number(ui, &mut web.recommended_password_length, 1..=128);
                        ui.label(tr(lang, "characters recommended", "Zeichen empfohlen"));
                    });
                    note(
                        ui,
                        tr(lang, "Shorter passwords are accepted with a note.", "Kürzere Passwörter werden mit Hinweis angenommen."),
                    );
                });
                ui.end_row();

                row_label(ui, tr(lang, "Second step", "Zweiter Schritt"));
                ui.vertical(|ui| {
                    ui.checkbox(&mut web.require_two_factor, tr(lang, "required for everybody", "für alle verlangen"));
                    note(
                        ui,
                        tr(lang, "A code from an authenticator app or by e-mail after the password.", "Ein Code aus einer Authenticator-App oder per E-Mail nach dem Passwort."),
                    );
                });
                ui.end_row();

                row_label(ui, tr(lang, "Codes by e-mail", "Codes per E-Mail"));
                ui.vertical(|ui| {
                    if email_channels.is_empty() {
                        note(
                            ui,
                            tr(lang, "Set up an e-mail channel under Notifications to send sign-in codes by e-mail.", "Richte unter Benachrichtigungen einen E-Mail-Kanal ein, um Anmeldecodes per E-Mail zu senden."),
                        );
                    } else {
                        let first = tr(lang, "first e-mail channel", "erster E-Mail-Kanal");
                        let shown = if web.email_channel.trim().is_empty() { first.to_string() } else { web.email_channel.clone() };
                        ui.horizontal(|ui| {
                            ui.label(tr(lang, "sent through", "gesendet über"));
                            egui::ComboBox::from_id_salt("web-email-channel")
                                .selected_text(shown)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut web.email_channel, String::new(), first);
                                    for name in email_channels {
                                        ui.selectable_value(&mut web.email_channel, name.clone(), name);
                                    }
                                });
                        });
                        note(
                            ui,
                            tr(lang, "Users choose codes by e-mail under Account, or get them when a second step is required and their address is known (directory, or set here).", "Benutzer wählen Codes per E-Mail unter Konto, oder bekommen sie, wenn ein zweiter Schritt verlangt wird und ihre Adresse bekannt ist (Verzeichnis oder hier eingetragen)."),
                        );
                    }
                });
                ui.end_row();
            });
    });
}

fn ldap_card(app: &mut AeternaApp, ui: &mut Ui, web: &mut WebSettings) {
    let lang = app.lang;
    let p = *palette(ui);
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            "LDAP / Active Directory",
            tr(
                lang,
                "Users sign in with their directory account. Their role comes from the groups below and is read again at every sign-in; users in none of them cannot sign in. Users created here keep working besides (e.g. as an emergency administrator).",
                "Benutzer melden sich mit ihrem Verzeichniskonto an. Ihre Rolle kommt aus den Gruppen unten und wird bei jeder Anmeldung neu gelesen; wer in keiner davon ist, kann sich nicht anmelden. Hier angelegte Benutzer funktionieren weiterhin (z. B. als Notfall-Administrator).",
            ),
        );
        let l = &mut web.ldap;
        ui.checkbox(
            &mut l.enabled,
            tr(
                lang,
                "Allow sign-in with the directory",
                "Anmeldung über das Verzeichnis erlauben",
            ),
        );
        if !l.enabled {
            return;
        }
        // Label above field: directory names are long, and so they fit any width.
        let width = ui.available_width().min(560.0);
        let field = |ui: &mut Ui, label: &str| {
            ui.add_space(10.0);
            ui.label(RichText::new(label).strong());
            ui.add_space(2.0);
        };

        field(ui, tr(lang, "Directory", "Verzeichnis"));
        let mut kind = if l.user_filter == ldap::AD_FILTER {
            0
        } else if l.user_filter == ldap::OPENLDAP_FILTER {
            1
        } else {
            2
        };
        let before = kind;
        ui.horizontal_wrapped(|ui| {
            widgets::choice(ui, &mut kind, 0, "Active Directory");
            widgets::choice(ui, &mut kind, 1, "OpenLDAP");
            if kind == 2 {
                widgets::choice(ui, &mut kind, 2, tr(lang, "own filter", "eigener Filter"));
            }
        });
        if kind != before {
            match kind {
                0 => {
                    l.user_filter = ldap::AD_FILTER.into();
                    l.nested_groups = true;
                }
                1 => {
                    l.user_filter = ldap::OPENLDAP_FILTER.into();
                    l.nested_groups = false;
                }
                _ => {}
            }
        }

        field(ui, "Server");
        ui.add(
            egui::TextEdit::singleline(&mut l.url)
                .hint_text("ldaps://dc1.firma.local")
                .desired_width(width),
        );
        if l.url.trim().to_ascii_lowercase().starts_with("ldap://") {
            ui.checkbox(
                &mut l.starttls,
                tr(lang, "encrypt with StartTLS", "mit StartTLS verschlüsseln"),
            );
            if !l.starttls {
                ui.checkbox(
                    &mut l.allow_unencrypted,
                    tr(
                        lang,
                        "allow unencrypted (tests only)",
                        "unverschlüsselt erlauben (nur für Tests)",
                    ),
                );
                note(
                    ui,
                    tr(
                        lang,
                        "Without encryption, passwords can be read in the network.",
                        "Ohne Verschlüsselung sind Passwörter im Netz lesbar.",
                    ),
                );
            }
        } else {
            note(
                ui,
                tr(
                    lang,
                    "ldaps:// is encrypted. Certificates trusted by this computer are accepted.",
                    "ldaps:// ist verschlüsselt. Zertifikate, denen dieser Computer vertraut, werden akzeptiert.",
                ),
            );
        }

        field(
            ui,
            tr(
                lang,
                "Own certificate authority (optional)",
                "Eigene Zertifizierungsstelle (optional)",
            ),
        );
        path_field(
            ui,
            &mut l.ca_file,
            tr(lang, "Choose…", "Auswählen…"),
            ("PEM", &["pem", "crt", "cer"]),
        );

        field(ui, tr(lang, "Search account", "Suchkonto"));
        ui.add(
            egui::TextEdit::singleline(&mut l.bind_dn)
                .hint_text("CN=svc-backup,OU=Service,DC=firma,DC=local")
                .desired_width(width),
        );
        if !l.bind_dn.trim().is_empty() && !l.binds_as_user() {
            ui.add(
                egui::TextEdit::singleline(&mut l.bind_password)
                    .password(true)
                    .hint_text(tr(
                        lang,
                        "Password of the search account",
                        "Passwort des Suchkontos",
                    ))
                    .desired_width(width.min(300.0)),
            );
        }
        note(
            ui,
            tr(
                lang,
                "Empty: anonymous. With {user}, e.g. {user}@firma.local: users search with their own account.",
                "Leer: anonym. Mit {user}, z. B. {user}@firma.local: Benutzer suchen mit ihrem eigenen Konto.",
            ),
        );

        field(ui, tr(lang, "Search base", "Suchbasis"));
        ui.add(
            egui::TextEdit::singleline(&mut l.base_dn)
                .hint_text("DC=firma,DC=local")
                .desired_width(width),
        );

        field(ui, tr(lang, "User filter", "Benutzerfilter"));
        ui.add(
            egui::TextEdit::singleline(&mut l.user_filter)
                .desired_width(width)
                .font(egui::TextStyle::Monospace),
        );

        field(ui, tr(lang, "Groups and roles", "Gruppen und Rollen"));
        egui::Grid::new("web-ldap-groups")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                for (label, group, hint) in [
                    (
                        tr(lang, "Administrators", "Administratoren"),
                        &mut l.admin_group,
                        "CN=Backup-Admins,OU=Gruppen,DC=firma,DC=local",
                    ),
                    (
                        tr(lang, "Operators", "Bediener"),
                        &mut l.operator_group,
                        "CN=Backup-Operatoren,OU=Gruppen,DC=firma,DC=local",
                    ),
                    (
                        tr(lang, "Viewers", "Betrachter"),
                        &mut l.viewer_group,
                        "CN=Backup-Leser,OU=Gruppen,DC=firma,DC=local",
                    ),
                ] {
                    ui.label(label);
                    let w = (ui.available_width() - 4.0).clamp(160.0, 440.0);
                    ui.add(
                        egui::TextEdit::singleline(group)
                            .hint_text(hint)
                            .desired_width(w),
                    );
                    ui.end_row();
                }
            });
        ui.checkbox(
            &mut l.nested_groups,
            tr(
                lang,
                "count nested groups (Active Directory)",
                "verschachtelte Gruppen zählen (Active Directory)",
            ),
        );
        note(
            ui,
            tr(
                lang,
                "At least one group. Members of several get the highest role; members of none cannot sign in.",
                "Mindestens eine Gruppe. Wer in mehreren ist, bekommt die höchste Rolle; wer in keiner ist, kann sich nicht anmelden.",
            ),
        );

        ui.add_space(12.0);
        ui.separator();
        field(ui, tr(lang, "Test", "Testen"));
        ui.horizontal_wrapped(|ui| {
            let w = ((width - 16.0) / 2.0).clamp(140.0, 220.0);
            ui.add(
                egui::TextEdit::singleline(&mut app.web.ldap_test_user)
                    .hint_text(tr(lang, "User name (optional)", "Benutzername (optional)"))
                    .desired_width(w),
            );
            ui.add(
                egui::TextEdit::singleline(&mut app.web.ldap_test_password)
                    .password(true)
                    .hint_text(tr(lang, "Password (optional)", "Passwort (optional)"))
                    .desired_width(w),
            );
        });
        ui.add_space(6.0);
        let label = if app.web.ldap_testing {
            tr(lang, "Testing…", "Teste…")
        } else {
            tr(lang, "Test connection", "Verbindung testen")
        };
        if widgets::button(ui, ButtonKind::Secondary, label, !app.web.ldap_testing).clicked() {
            let mut settings: LdapSettings = l.clone();
            settings.enabled = true;
            let (user, password) = (
                app.web.ldap_test_user.clone(),
                app.web.ldap_test_password.clone(),
            );
            let slot = app.web.ldap_test.clone();
            let ctx = ui.ctx().clone();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
            app.web.ldap_testing = true;
            std::thread::spawn(move || {
                let result = ldap::test(&settings, &user, &password);
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                ctx.request_repaint();
            });
        }
        let result = app
            .web
            .ldap_test
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(result) = result {
            app.web.ldap_testing = false;
            let (color, text) = match result {
                Ok(text) => (p.success, text),
                Err(text) => (p.error, text),
            };
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
        }
        note(
            ui,
            tr(
                lang,
                "Without a password the connection and the search are checked; with a user name and password the whole sign-in including the role. Tests use the values above, also before they are applied.",
                "Ohne Passwort werden Verbindung und Suche geprüft; mit Benutzername und Passwort die ganze Anmeldung einschließlich Rolle. Getestet wird mit den Werten oben, auch vor dem Übernehmen.",
            ),
        );
    });
}

/// "Apply" and "Discard" while the draft differs from the saved settings.
fn apply_bar(app: &mut AeternaApp, ui: &mut Ui, place: &str) {
    let changed = app.web.draft.as_ref() != Some(&app.config.web);
    if !changed && app.web.apply_note.is_none() {
        return;
    }
    ui.add_space(10.0);
    ui.push_id(place, |ui| apply_card(app, ui, changed));
}

fn apply_card(app: &mut AeternaApp, ui: &mut Ui, changed: bool) {
    let lang = app.lang;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            if changed {
                ui.label(
                    RichText::new(tr(lang, "Changed settings are not in use yet.", "Geänderte Einstellungen sind noch nicht aktiv.")).strong(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !changed {
                    if widgets::button(ui, ButtonKind::Quiet, "OK", true).clicked() {
                        app.web.apply_note = None;
                    }
                    return;
                }
                if widgets::button(ui, ButtonKind::Primary, tr(lang, "Apply", "Übernehmen"), true).clicked() {
                    let mut draft = app.web.draft.clone().expect("set in show");
                    draft.allowed_networks.retain(|n| !n.trim().is_empty());
                    draft.allowed_networks.iter_mut().for_each(|n| *n = n.trim().to_string());
                    match crate::web::check_settings(&draft) {
                        Ok(_) => {
                            draft.enabled = app.config.web.enabled;
                            app.config.web = draft.clone();
                            app.web.draft = Some(draft.clone());
                            app.web.draft_base = Some(draft);
                            app.web.error = None;
                            app.mark_dirty();
                            app.web.apply_note = Some(Note::Good(
                                if app.config.web.enabled {
                                    tr(lang, "Applied. The web interface was started again; users sign in again.", "Übernommen. Die Web-Oberfläche wurde neu gestartet; Benutzer melden sich neu an.")
                                } else {
                                    tr(lang, "Applied.", "Übernommen.")
                                }
                                .into(),
                            ));
                        }
                        Err(err) => app.web.apply_note = Some(Note::Bad(err)),
                    }
                }
                if widgets::button(ui, ButtonKind::Quiet, tr(lang, "Discard", "Verwerfen"), true).clicked() {
                    app.web.draft = Some(app.config.web.clone());
                    app.web.draft_base = Some(app.config.web.clone());
                    app.web.apply_note = None;
                }
            });
        });
        show_note(ui, &app.web.apply_note);
    });
}

fn users_card(app: &mut AeternaApp, ui: &mut Ui) {
    let lang = app.lang;
    let en = lang == Lang::En;
    let p = *palette(ui);
    let config_file = app.paths.config_file.clone();
    let recommended = app.config.web.recommended_password_length;
    let mut store = UserStore::load(&config_file);
    let mut changed: Option<Record> = None;
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
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Users", "Benutzer"),
            tr(
                lang,
                "Viewer: overview, backups, activity. Operator: also runs, cancels and switches jobs and checks backups. Administrator: also users and the audit log. A user limited to jobs sees only those jobs and their destinations. Directory users appear here after their first sign-in.",
                "Betrachter: Übersicht, Sicherungen, Verlauf. Bediener: startet, stoppt und schaltet außerdem Aufträge und prüft Sicherungen. Administrator: außerdem Benutzer und Audit-Log. Ein auf Aufträge beschränkter Benutzer sieht nur diese Aufträge und ihre Ziele. Verzeichnisbenutzer erscheinen hier nach ihrer ersten Anmeldung.",
            ),
        );

        if let Some((name, password)) = app.web.shown_password.clone() {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{} {name}:", tr(lang, "New password for", "Neues Passwort für")));
                        ui.label(RichText::new(&password).monospace().strong());
                        if widgets::button(ui, ButtonKind::Quiet, tr(lang, "Copy", "Kopieren"), true).clicked() {
                            ui.ctx().copy_text(password.clone());
                        }
                        if widgets::button(ui, ButtonKind::Quiet, tr(lang, "Done", "Fertig"), true).clicked() {
                            app.web.shown_password = None;
                        }
                    });
                    note(
                        ui,
                        tr(lang, "Shown only now. The user chooses a new one after signing in.", "Wird nur jetzt angezeigt. Der Benutzer wählt nach der Anmeldung ein neues."),
                    );
                });
            ui.add_space(6.0);
        }

        if store.users.is_empty() {
            note(
                ui,
                tr(
                    lang,
                    "No users yet. The first one should be an administrator.",
                    "Noch keine Benutzer. Der erste sollte ein Administrator sein.",
                ),
            );
        } else {
            let admins = store.admins();
            let total = store.users.len();
            let mut remove = None;
            egui::Grid::new("web-users")
                .num_columns(5)
                .striped(true)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    for h in [
                        tr(lang, "Name", "Name"),
                        tr(lang, "Role", "Rolle"),
                        tr(lang, "Sign-in", "Anmeldung"),
                        tr(lang, "Last sign-in", "Letzte Anmeldung"),
                        "",
                    ] {
                        ui.label(RichText::new(h).color(p.text_secondary).size(13.0));
                    }
                    ui.end_row();
                    for user in store.users.iter_mut() {
                        let last_admin = admins == 1 && user.may(Role::Admin);
                        let directory = user.source == UserSource::Directory;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&user.name).strong());
                            if user.disabled {
                                ui.colored_label(p.error, tr(lang, "locked", "gesperrt"));
                            }
                        });
                        if directory {
                            ui.label(user.role.name(en)).on_hover_text(tr(
                                lang,
                                "From the directory groups",
                                "Aus den Verzeichnisgruppen",
                            ));
                        } else {
                            let before = user.role;
                            egui::ComboBox::from_id_salt(("role", user.name.clone()))
                                .selected_text(user.role.name(en))
                                .show_ui(ui, |ui| {
                                    for role in Role::ALL {
                                        ui.selectable_value(&mut user.role, role, role.name(en));
                                    }
                                });
                            if user.role != before {
                                if last_admin {
                                    user.role = before;
                                    app.web.user_note = Some(Note::Bad(
                                        tr(
                                            lang,
                                            "The last administrator stays administrator.",
                                            "Der letzte Administrator bleibt Administrator.",
                                        )
                                        .into(),
                                    ));
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
                        }
                        ui.label(format!(
                            "{}{}",
                            if directory {
                                "LDAP"
                            } else {
                                tr(lang, "password", "Passwort")
                            },
                            if user.totp.is_some() {
                                tr(lang, " + app", " + App")
                            } else if user.email_codes && user.email.contains('@') {
                                tr(lang, " + e-mail", " + E-Mail")
                            } else {
                                ""
                            }
                        ));
                        ui.label(
                            user.last_sign_in
                                .map(|t| lang.relative_time(t.with_timezone(&chrono::Local)))
                                .unwrap_or_else(|| tr(lang, "never", "noch nie").into()),
                        );
                        ui.horizontal(|ui| {
                            ui.push_id(("actions", user.name.clone()), |ui| {
                                ui.menu_button(tr(lang, "Actions…", "Aktionen…"), |ui| {
                                    let lock_label = if user.disabled {
                                        tr(lang, "Unlock", "Entsperren")
                                    } else {
                                        tr(lang, "Lock", "Sperren")
                                    };
                                    if ui
                                        .add_enabled(
                                            !last_admin || user.disabled,
                                            egui::Button::new(lock_label),
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
                                        ui.close();
                                    }
                                    if !directory
                                        && ui
                                            .button(tr(
                                                lang,
                                                "Set a new password",
                                                "Neues Passwort setzen",
                                            ))
                                            .clicked()
                                    {
                                        let password = users::random_password();
                                        user.password = users::hash_password(&password);
                                        user.must_change_password = true;
                                        app.web.shown_password =
                                            Some((user.name.clone(), password));
                                        changed = Some(
                                            Record::local("user-password-reset")
                                                .target(user.name.clone()),
                                        );
                                        ui.close();
                                    }
                                    if !directory {
                                        ui.separator();
                                        if app
                                            .web
                                            .email_edit
                                            .as_ref()
                                            .is_none_or(|(n, _)| n != &user.name)
                                        {
                                            app.web.email_edit =
                                                Some((user.name.clone(), user.email.clone()));
                                        }
                                        if let Some((_, text)) = app.web.email_edit.as_mut() {
                                            ui.label(
                                                RichText::new(tr(
                                                    lang,
                                                    "E-mail address (for sign-in codes)",
                                                    "E-Mail-Adresse (für Anmeldecodes)",
                                                ))
                                                .color(p.text_secondary),
                                            );
                                            ui.add(
                                                egui::TextEdit::singleline(text)
                                                    .hint_text("name@firma.de")
                                                    .desired_width(220.0),
                                            );
                                            if ui
                                                .button(tr(
                                                    lang,
                                                    "Save address",
                                                    "Adresse speichern",
                                                ))
                                                .clicked()
                                            {
                                                let address = text.trim().to_string();
                                                if address.is_empty()
                                                    || users::plausible_email(&address)
                                                {
                                                    user.email = address.clone();
                                                    changed = Some(
                                                        Record::local("user-changed")
                                                            .target(user.name.clone())
                                                            .details(vec![format!(
                                                                "e-mail: {}",
                                                                if address.is_empty() {
                                                                    "none".into()
                                                                } else {
                                                                    users::masked(&address)
                                                                }
                                                            )]),
                                                    );
                                                    ui.close();
                                                } else {
                                                    app.web.user_note = Some(Note::Bad(
                                                        tr(
                                                            lang,
                                                            "This is not an e-mail address.",
                                                            "Das ist keine E-Mail-Adresse.",
                                                        )
                                                        .into(),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                    if user.totp.is_some()
                                        && ui
                                            .button(tr(
                                                lang,
                                                "Reset one-time codes",
                                                "Einmalcodes zurücksetzen",
                                            ))
                                            .clicked()
                                    {
                                        user.totp = None;
                                        changed = Some(
                                            Record::local("user-one-time-codes-reset")
                                                .target(user.name.clone()),
                                        );
                                        ui.close();
                                    }
                                    if !schedules.is_empty() {
                                        ui.separator();
                                        ui.label(
                                            RichText::new(tr(
                                                lang,
                                                "Only these jobs (none: all)",
                                                "Nur diese Aufträge (keiner: alle)",
                                            ))
                                            .color(p.text_secondary),
                                        );
                                        let before = user.jobs.clone();
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
                                    ui.separator();
                                    let confirm = app.web.confirm_remove.as_deref()
                                        == Some(user.name.as_str());
                                    let label = if confirm {
                                        tr(lang, "Really remove?", "Wirklich entfernen?")
                                    } else {
                                        tr(lang, "Remove…", "Entfernen…")
                                    };
                                    if ui
                                        .add_enabled(
                                            !last_admin || total == 1,
                                            egui::Button::new(RichText::new(label).color(p.error)),
                                        )
                                        .clicked()
                                    {
                                        if confirm {
                                            remove = Some(user.name.clone());
                                            app.web.confirm_remove = None;
                                            ui.close();
                                        } else {
                                            app.web.confirm_remove = Some(user.name.clone());
                                        }
                                    }
                                });
                            });
                            if !user.jobs.is_empty() {
                                note(
                                    ui,
                                    format!("{} {}", user.jobs.len(), tr(lang, "jobs", "Aufträge")),
                                );
                            }
                        });
                        ui.end_row();
                    }
                });
            if let Some(name) = remove {
                store.users.retain(|u| u.name != name);
                changed = Some(Record::local("user-removed").target(name));
            }
        }

        ui.add_space(10.0);
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tr(lang, "Add user", "Benutzer hinzufügen")).strong());
                ui.add_space(4.0);
                egui::Grid::new("web-add-user")
                    .num_columns(2)
                    .spacing([24.0, 8.0])
                    .show(ui, |ui| {
                        row_label(ui, tr(lang, "User name", "Benutzername"));
                        ui.add(egui::TextEdit::singleline(&mut app.web.new_name).desired_width(220.0));
                        ui.end_row();
                        row_label(ui, tr(lang, "Password", "Passwort"));
                        ui.vertical(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut app.web.new_password)
                                    .password(true)
                                    .desired_width(220.0),
                            );
                            if !app.web.new_password.is_empty() {
                                crate::gui::dialogs::strength_meter(ui, &app.web.new_password, lang);
                                let length = app.web.new_password.chars().count();
                                if length < recommended as usize {
                                    ui.colored_label(
                                        p.warning,
                                        if en {
                                            format!("{length} of the recommended {recommended} characters. Allowed, but easier to guess.")
                                        } else {
                                            format!("{length} von empfohlenen {recommended} Zeichen. Erlaubt, aber leichter zu erraten.")
                                        },
                                    );
                                }
                            }
                        });
                        ui.end_row();
                        row_label(ui, tr(lang, "Role", "Rolle"));
                        egui::ComboBox::from_id_salt("new-role")
                            .selected_text(app.web.new_role.name(en))
                            .show_ui(ui, |ui| {
                                for role in Role::ALL {
                                    ui.selectable_value(&mut app.web.new_role, role, role.name(en));
                                }
                            });
                        ui.end_row();
                    });
                ui.add_space(4.0);
                let ready = !app.web.new_name.trim().is_empty() && !app.web.new_password.is_empty();
                if widgets::button(ui, ButtonKind::Secondary, tr(lang, "Add", "Hinzufügen"), ready).clicked() {
                    let name = app.web.new_name.trim().to_string();
                    match store.add(&name, app.web.new_role, &app.web.new_password) {
                        Ok(()) => {
                            changed = Some(
                                Record::local("user-added")
                                    .target(name.clone())
                                    .details(vec![format!("role: {}", app.web.new_role.name(true))]),
                            );
                            let text = format!("{} {name}.", tr(lang, "Added:", "Hinzugefügt:"));
                            app.web.user_note = Some(match users::password_advice(&app.web.new_password, recommended, en) {
                                Some(advice) => Note::Warn(format!("{text} {advice}")),
                                None => Note::Good(text),
                            });
                            app.web.new_name.clear();
                            app.web.new_password.clear();
                        }
                        Err(err) => app.web.user_note = Some(Note::Bad(err)),
                    }
                }
                show_note(ui, &app.web.user_note);
            });
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
    let p = *palette(ui);
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
        ui.horizontal(|ui| {
            match audit::verify(&app.paths.config_file) {
                Ok(v) if v.broken.is_empty() => {
                    dot(ui, p.success);
                    note(
                        ui,
                        format!(
                            "{} {}",
                            v.entries,
                            tr(
                                lang,
                                "entries, none changed afterwards",
                                "Einträge, keiner nachträglich verändert"
                            )
                        ),
                    );
                }
                Ok(v) => {
                    ui.colored_label(
                        p.error,
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
                    ui.colored_label(p.error, err.to_string());
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut app.web.audit_filter)
                        .hint_text(tr(
                            lang,
                            "Filter: user, action, address…",
                            "Filtern: Benutzer, Aktion, Adresse…",
                        ))
                        .desired_width(240.0),
                );
            });
        });
        ui.add_space(6.0);
        let filter = app.web.audit_filter.to_lowercase();
        let entries: Vec<audit::AuditEntry> = audit::load(&app.paths.config_file)
            .into_iter()
            .rev()
            .filter(|e| {
                filter.is_empty()
                    || [&e.actor, &e.source, &e.action, &e.target]
                        .iter()
                        .any(|f| f.to_lowercase().contains(&filter))
                    || e.details.iter().any(|d| d.to_lowercase().contains(&filter))
            })
            .collect();
        let shown = app.web.audit_shown.max(50);
        // A plain table in the page (no scroll area of its own, which would
        // shrink to a single line inside the scrolling page).
        egui::Grid::new("audit-grid")
            .num_columns(5)
            .striped(true)
            .spacing([20.0, 6.0])
            .min_col_width(80.0)
            .show(ui, |ui| {
                for h in [
                    tr(lang, "When", "Wann"),
                    tr(lang, "Who", "Wer"),
                    tr(lang, "From", "Von wo"),
                    tr(lang, "What", "Was"),
                    tr(lang, "Details", "Details"),
                ] {
                    ui.label(RichText::new(h).color(p.text_secondary).size(13.0));
                }
                ui.end_row();
                for e in entries.iter().take(shown) {
                    ui.label(
                        RichText::new(
                            e.at.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d %H:%M:%S")
                                .to_string(),
                        )
                        .size(13.5),
                    );
                    ui.label(RichText::new(&e.actor).size(13.5));
                    ui.label(RichText::new(&e.source).size(13.5).color(p.text_secondary));
                    ui.horizontal(|ui| {
                        dot(ui, if e.ok { p.success } else { p.error });
                        let what = format!("{} {}", e.action, e.target);
                        ui.label(RichText::new(what).size(13.5).color(if e.ok {
                            p.text
                        } else {
                            p.error
                        }));
                    });
                    ui.add(
                        egui::Label::new(
                            RichText::new(e.details.join(" · "))
                                .size(13.0)
                                .color(p.text_secondary),
                        )
                        .wrap(),
                    );
                    ui.end_row();
                }
            });
        if entries.len() > shown {
            ui.add_space(6.0);
            if widgets::button(
                ui,
                ButtonKind::Quiet,
                &format!(
                    "{} ({})",
                    tr(lang, "Show more", "Mehr anzeigen"),
                    entries.len() - shown
                ),
                true,
            )
            .clicked()
            {
                app.web.audit_shown = shown + 100;
            }
        }
    });
}
