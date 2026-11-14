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
use crate::web::users::{self, UserSource, UserStore};

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
    new_role: String,
    user_note: Option<Note>,
    /// A password set for a user, shown once: (user, password).
    shown_password: Option<(String, String)>,
    /// A user whose removal waits for a second click.
    confirm_remove: Option<String>,
    /// The e-mail address being edited in a user's menu: (user, text).
    email_edit: Option<(String, String)>,
    /// Address typed in to block.
    block_address: String,
    block_note: Option<Note>,
    new_role_name: String,
    role_note: Option<Note>,
    pub tab: WebTab,
    country_search: String,
    geo_test: String,
    /// The country database chosen, opened to show what it is.
    #[allow(clippy::type_complexity)]
    geo_db: Option<(
        PathBuf,
        Option<SystemTime>,
        Result<Arc<crate::web::access::Geo>, String>,
    )>,
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
            new_role: "viewer".into(),
            user_note: None,
            shown_password: None,
            confirm_remove: None,
            email_edit: None,
            block_address: String::new(),
            block_note: None,
            new_role_name: String::new(),
            role_note: None,
            tab: WebTab::default(),
            country_search: String::new(),
            geo_test: String::new(),
            geo_db: None,
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

    #[cfg(test)]
    pub fn country_search_for_test(&mut self, text: &str) {
        self.country_search = text.into();
    }

    /// Fills the "add user" fields (interface tests).
    #[cfg(test)]
    pub fn fill_new_user(&mut self, name: &str, password: &str, role: &str) {
        self.new_name = name.into();
        self.new_password = password.into();
        self.new_role = role.to_string();
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

fn path_field(ui: &mut Ui, path: &mut PathBuf, choose: &str, filter: (&str, &[&str])) {
    ui.horizontal(|ui| {
        let mut text = path.display().to_string();
        if ui
            .add(
                widgets::text_field(&mut text)
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

/// Width of the label column in the settings cards.
const LABEL: f32 = 160.0;

/// A list of networks with a field per entry, a remove button and an add
/// button; wrong entries are marked.
fn network_list(ui: &mut Ui, lang: Lang, id: &str, list: &mut Vec<String>) {
    let p = *palette(ui);
    ui.push_id(id, |ui| {
        ui.vertical(|ui| {
            let mut remove = None;
            for (i, net) in list.iter_mut().enumerate() {
                ui.push_id(i, |ui| {
                    ui.horizontal(|ui| {
                        let bad = !net.trim().is_empty()
                            && net.trim().parse::<crate::web::Network>().is_err();
                        let mut edit = widgets::text_field(net)
                            .hint_text("192.168.1.0/24")
                            .desired_width(190.0);
                        if bad {
                            edit = edit.text_color(p.error);
                        }
                        ui.add(edit).on_hover_text(if bad {
                            tr(
                                lang,
                                "Not an address or network",
                                "Keine Adresse und kein Netz",
                            )
                        } else {
                            ""
                        });
                        if widgets::button(ui, ButtonKind::Quiet, "×", true).clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            if widgets::button(
                ui,
                ButtonKind::Quiet,
                tr(lang, "+ Add network", "+ Netz hinzufügen"),
                true,
            )
            .clicked()
            {
                list.push(String::new());
            }
        });
    });
}

/// The parts of the Web page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebTab {
    #[default]
    Connection,
    SignIn,
    Access,
    Directory,
    Users,
    Roles,
    Audit,
}

/// The Web page: the state at the top, then one part at a time.
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
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (tab, en, de) in [
            (WebTab::Connection, "Connection", "Verbindung"),
            (WebTab::SignIn, "Sign-in", "Anmeldung"),
            (WebTab::Access, "Access", "Zugriff"),
            (WebTab::Directory, "LDAP / AD", "LDAP / AD"),
            (WebTab::Users, "Users", "Benutzer"),
            (WebTab::Roles, "Roles", "Rollen"),
            (WebTab::Audit, "Audit log", "Audit-Log"),
        ] {
            widgets::choice(ui, &mut app.web.tab, tab, tr(lang, en, de));
        }
    });
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
    let roles = UserStore::load(&app.paths.config_file).role_names();
    match app.web.tab {
        WebTab::Connection => {
            connection_card(app, ui, &mut draft);
            ui.add_space(14.0);
            certificate_card(app, ui, &mut draft);
        }
        WebTab::SignIn => signin_card(lang, ui, &mut draft, &email_channels),
        WebTab::Access => {
            access_card(app, ui, &mut draft);
            ui.add_space(14.0);
            countries_card(app, ui, &mut draft);
            ui.add_space(14.0);
            blocked_card(app, ui);
        }
        WebTab::Directory => ldap_card(app, ui, &mut draft, &roles),
        WebTab::Users => users_card(app, ui),
        WebTab::Roles => roles_card(app, ui),
        WebTab::Audit => audit_card(app, ui),
    }
    app.web.draft = Some(draft);
}

/// One fact in a details list: label and value, the value selectable.
fn fact(ui: &mut Ui, label: &str, value: impl Into<String>) {
    widgets::field_row(ui, LABEL, label, "", |ui| {
        ui.add(egui::Label::new(value.into()).wrap().selectable(true));
    });
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
                "Jobs, backups and problems in a browser, also from other computers. Users run jobs and checks as their role allows. Restoring, deleting and settings stay in this window. Runs while AeternaVault is open, or with `aeternavault-cli web serve`.",
                "Aufträge, Sicherungen und Probleme im Browser, auch von anderen Computern. Benutzer starten Aufträge und Prüfungen, soweit ihre Rolle es erlaubt. Wiederherstellen, Löschen und Einstellungen bleiben in diesem Fenster. Läuft, solange AeternaVault geöffnet ist, oder mit `aeternavault-cli web serve`.",
            ),
        );
        let mut on = app.config.web.enabled;
        if widgets::switch(
            ui,
            &mut on,
            RichText::new(tr(
                lang,
                "Enable web interface",
                "Web-Oberfläche aktivieren",
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
                            "Nobody can sign in yet. Add a user below.",
                            "Noch kann sich niemand anmelden. Unten einen Benutzer hinzufügen.",
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
        let blocked = crate::web::access::Blocklist::load(&app.paths.config_file)
            .entries
            .len();
        ui.add_space(12.0);
        widgets::stat_tiles(
            ui,
            &[
                (
                    users.users.len().to_string(),
                    tr(lang, "users", "Benutzer"),
                    p.text,
                ),
                (
                    active.to_string(),
                    tr(lang, "signed in now", "jetzt angemeldet"),
                    p.accent,
                ),
                (
                    good.to_string(),
                    tr(lang, "sign-ins in 24 h", "Anmeldungen in 24 h"),
                    p.success,
                ),
                (
                    bad.to_string(),
                    tr(lang, "failed in 24 h", "Fehlversuche in 24 h"),
                    if bad > 0 { p.error } else { p.text_secondary },
                ),
                (
                    blocked.to_string(),
                    tr(lang, "blocked addresses", "gesperrte Adressen"),
                    if blocked > 0 {
                        p.warning
                    } else {
                        p.text_secondary
                    },
                ),
            ],
        );
    });
}

fn connection_card(app: &mut AeternaApp, ui: &mut Ui, web: &mut WebSettings) {
    let lang = app.lang;
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Connection", "Verbindung"),
            tr(
                lang,
                "The web interface listens on every network of this computer, always over HTTPS. Other computers in the same network reach it directly; from outside it needs the port opened in the firewall or router.",
                "Die Web-Oberfläche lauscht in allen Netzen dieses Computers, immer über HTTPS. Andere Computer im selben Netz erreichen sie direkt; von außen muss der Port in Firewall oder Router freigegeben sein.",
            ),
        );
        widgets::field_row(ui, LABEL, "Port", "", |ui| {
            widgets::number(ui, &mut web.port, 1..=65535);
        });
        let url = web.url();
        widgets::field_row(ui, LABEL, tr(lang, "Address", "Adresse"), "", |ui| {
            ui.add(egui::Label::new(RichText::new(&url).monospace()).selectable(true));
            if widgets::button(ui, ButtonKind::Quiet, tr(lang, "Copy", "Kopieren"), true).clicked()
            {
                ui.ctx().copy_text(url.clone());
            }
        });
        if web.local_only() {
            widgets::field_row(
                ui,
                LABEL,
                tr(lang, "Listening on", "Lauscht auf"),
                tr(
                    lang,
                    "Set on the command line with `web set --bind`. 0.0.0.0 listens on every network.",
                    "Gesetzt auf der Kommandozeile mit `web set --bind`. 0.0.0.0 lauscht in allen Netzen.",
                ),
                |ui| {
                    ui.label(tr(lang, "this computer only", "nur diesem Computer"));
                },
            );
        }
    });
}

/// The certificate as server software shows it: what is in use, its
/// details, and making, exporting or replacing it.
fn certificate_card(app: &mut AeternaApp, ui: &mut Ui, web: &mut WebSettings) {
    let lang = app.lang;
    let p = *palette(ui);
    let config_file = app.paths.config_file.clone();
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Certificate", "Zertifikat"),
            tr(
                lang,
                "Self-signed: AeternaVault makes and signs the certificate itself. The connection is encrypted, but browsers do not know the issuer and warn until the certificate is trusted: export it and add it to the trusted certificates of the computers that connect, or compare the fingerprint once. Own certificate: PEM files from the company CA or from Let's Encrypt (certbot, win-acme); renewed files are used without a restart.",
                "Selbstsigniert: AeternaVault erstellt und signiert das Zertifikat selbst. Die Verbindung ist verschlüsselt, aber Browser kennen den Aussteller nicht und warnen, bis dem Zertifikat vertraut wird: exportieren und auf den zugreifenden Computern als vertrauenswürdig hinzufügen, oder den Fingerabdruck einmal vergleichen. Eigenes Zertifikat: PEM-Dateien der Firmen-CA oder von Let's Encrypt (certbot, win-acme); erneuerte Dateien werden ohne Neustart verwendet.",
            ),
        );
        let mut own = web.tls == TlsMode::Files;
        widgets::field_row(ui, LABEL, tr(lang, "Type", "Art"), "", |ui| {
            widgets::choice(
                ui,
                &mut own,
                false,
                tr(lang, "Self-signed", "Selbstsigniert"),
            );
            widgets::choice(
                ui,
                &mut own,
                true,
                tr(lang, "Own certificate", "Eigenes Zertifikat"),
            );
        });
        if own && web.tls != TlsMode::Files {
            web.tls = TlsMode::Files;
        } else if !own && web.tls == TlsMode::Files {
            // Auto: a self-signed certificate whenever others can connect.
            web.tls = TlsMode::Auto;
        }
        widgets::reveal(ui, "web-cert-files", own, |ui| {
            let choose = tr(lang, "Choose…", "Auswählen…");
            widgets::field_row(
                ui,
                LABEL,
                tr(lang, "Certificate file", "Zertifikatsdatei"),
                "",
                |ui| {
                    path_field(
                        ui,
                        &mut web.cert_file,
                        choose,
                        ("PEM", &["pem", "crt", "cer"]),
                    );
                },
            );
            widgets::field_row(
                ui,
                LABEL,
                tr(lang, "Private key", "Privater Schlüssel"),
                "",
                |ui| {
                    path_field(ui, &mut web.key_file, choose, ("PEM", &["pem", "key"]));
                },
            );
        });

        ui.add_space(8.0);
        match crate::web::tls::info(&config_file, web) {
            None => fact(
                ui,
                tr(lang, "In use", "In Verwendung"),
                tr(
                    lang,
                    "None yet: made when the web interface starts",
                    "Noch keines: wird beim Start der Web-Oberfläche erstellt",
                ),
            ),
            Some(Err(err)) => {
                widgets::field_row(ui, LABEL, tr(lang, "In use", "In Verwendung"), "", |ui| {
                    ui.add(egui::Label::new(RichText::new(err).color(p.error)).wrap());
                });
            }
            Some(Ok(cert)) => {
                fact(
                    ui,
                    tr(lang, "Issued to", "Ausgestellt für"),
                    cert.subject.clone(),
                );
                fact(
                    ui,
                    tr(lang, "Issued by", "Ausgestellt von"),
                    cert.issuer.clone(),
                );
                if !cert.names.is_empty() {
                    fact(
                        ui,
                        tr(lang, "Valid for", "Gültig für"),
                        cert.names.join(", "),
                    );
                }
                let days = cert.days_left();
                let until = cert
                    .not_after
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string();
                widgets::field_row(ui, LABEL, tr(lang, "Valid until", "Gültig bis"), "", |ui| {
                    let color = if days < 0 {
                        p.error
                    } else if days < 21 {
                        p.warning
                    } else {
                        p.text
                    };
                    let left = if lang == Lang::En {
                        format!("{until} · {days} days left")
                    } else {
                        format!("{until} · noch {days} Tage")
                    };
                    ui.label(RichText::new(left).color(color));
                });
                widgets::field_row(ui, LABEL, "SHA-256", "", |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&cert.fingerprint).monospace().size(12.5))
                            .wrap()
                            .selectable(true),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if widgets::button(ui, ButtonKind::Secondary, tr(lang, "Copy fingerprint", "Fingerabdruck kopieren"), true)
                        .clicked()
                    {
                        ui.ctx().copy_text(cert.fingerprint.clone());
                    }
                    if widgets::button(ui, ButtonKind::Secondary, tr(lang, "Export certificate…", "Zertifikat exportieren…"), true)
                        .on_hover_text(tr(
                            lang,
                            "Saves the certificate (without the key) to install it as trusted on other computers.",
                            "Speichert das Zertifikat (ohne Schlüssel), um es auf anderen Computern als vertrauenswürdig einzutragen.",
                        ))
                        .clicked()
                    {
                        let (cert_file, _) = crate::web::tls::files(&config_file, web);
                        if let Some(target) = rfd::FileDialog::new()
                            .set_file_name("aeternavault.crt")
                            .add_filter("Certificate", &["crt", "pem"])
                            .save_file()
                        {
                            match std::fs::copy(&cert_file, &target) {
                                Ok(_) => app.notify(
                                    NoticeKind::Info,
                                    format!("{} {}", tr(lang, "Saved:", "Gespeichert:"), target.display()),
                                ),
                                Err(err) => app.notify(NoticeKind::Error, err.to_string()),
                            }
                        }
                    }
                    if !own
                        && widgets::button(ui, ButtonKind::Quiet, tr(lang, "Make a new one", "Neu erstellen"), true)
                            .on_hover_text(tr(
                                lang,
                                "After the computer's name or address changed. Browsers warn again once.",
                                "Nachdem sich Name oder Adresse des Computers geändert haben. Browser warnen dann einmal neu.",
                            ))
                            .clicked()
                    {
                        let _ = crate::web::tls::renew(&config_file);
                        audit::record(&config_file, Record::local("web-certificate-renewed"));
                        app.web.server = None;
                    }
                });
            }
        }
    });
}

fn signin_card(lang: Lang, ui: &mut Ui, web: &mut WebSettings, email_channels: &[String]) {
    use crate::web::access::MfaPolicy;
    widgets::card(ui, |ui| {
        widgets::section_title(ui, tr(lang, "Sign-in", "Anmeldung"));
        widgets::field_row(
            ui,
            LABEL,
            tr(lang, "Sign out after", "Abmelden nach"),
            "",
            |ui| {
                widgets::number(ui, &mut web.session_minutes, 1..=1440);
                ui.label(tr(lang, "minutes idle", "Minuten Inaktivität"));
            },
        );
        ui.add_space(4.0);
        widgets::field_row(
            ui,
            LABEL,
            tr(lang, "Lock after", "Sperren nach"),
            "",
            |ui| {
                widgets::number(ui, &mut web.lockout_attempts, 1..=100);
                ui.label(tr(lang, "wrong sign-ins for", "Fehlversuchen für"));
                widgets::number(ui, &mut web.lockout_minutes, 1..=1440);
                ui.label(tr(lang, "min", "Min."));
            },
        );
        ui.add_space(4.0);
        widgets::field_row(
            ui,
            LABEL,
            tr(lang, "Password length", "Passwortlänge"),
            tr(
                lang,
                "A recommendation: shorter passwords are possible.",
                "Eine Empfehlung: Kürzere Passwörter sind möglich.",
            ),
            |ui| {
                widgets::number(ui, &mut web.recommended_password_length, 1..=128);
                ui.label(tr(lang, "characters recommended", "Zeichen empfohlen"));
            },
        );

        ui.add_space(14.0);
        widgets::field_row(
            ui,
            LABEL,
            "2FA",
            tr(
                lang,
                "A second factor after the password: a code from an authenticator app or by e-mail.",
                "Ein zweiter Faktor nach dem Passwort: ein Code aus einer Authenticator-App oder per E-Mail.",
            ),
            |ui| {
                widgets::choice(
                    ui,
                    &mut web.mfa,
                    MfaPolicy::Optional,
                    tr(lang, "Optional", "Freiwillig"),
                );
                widgets::choice(
                    ui,
                    &mut web.mfa,
                    MfaPolicy::Administrators,
                    tr(lang, "Administrators", "Administratoren"),
                );
                widgets::choice(
                    ui,
                    &mut web.mfa,
                    MfaPolicy::Everyone,
                    tr(lang, "Everyone", "Alle"),
                );
            },
        );
        ui.add_space(4.0);
        widgets::field_row(ui, LABEL, tr(lang, "Methods", "Methoden"), "", |ui| {
            ui.vertical(|ui| {
                widgets::switch(
                    ui,
                    &mut web.mfa_app,
                    tr(lang, "Authenticator app", "Authenticator-App"),
                );
                widgets::switch(
                    ui,
                    &mut web.mfa_email,
                    tr(lang, "Codes by e-mail", "Codes per E-Mail"),
                );
            });
        });
        if web.mfa != MfaPolicy::Optional && !web.mfa_app && !web.mfa_email {
            ui.colored_label(
                palette(ui).error,
                tr(
                    lang,
                    "Allow at least one method.",
                    "Mindestens eine Methode erlauben.",
                ),
            );
        }
        widgets::reveal(ui, "web-mfa-email", web.mfa_email, |ui| {
            ui.add_space(4.0);
            widgets::field_row(
                ui,
                LABEL,
                tr(lang, "Sent through", "Gesendet über"),
                "",
                |ui| {
                    if email_channels.is_empty() {
                        note(
                            ui,
                            tr(
                                lang,
                                "No e-mail channel yet. Set one up under Notifications.",
                                "Noch kein E-Mail-Kanal. Unter Benachrichtigungen einrichten.",
                            ),
                        );
                    } else {
                        let first = tr(lang, "First e-mail channel", "Erster E-Mail-Kanal");
                        let shown = if web.email_channel.trim().is_empty() {
                            first.to_string()
                        } else {
                            web.email_channel.clone()
                        };
                        egui::ComboBox::from_id_salt("web-email-channel")
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut web.email_channel, String::new(), first);
                                for name in email_channels {
                                    ui.selectable_value(&mut web.email_channel, name.clone(), name);
                                }
                            });
                    }
                },
            );
        });
    });
}

fn access_card(app: &mut AeternaApp, ui: &mut Ui, web: &mut WebSettings) {
    use crate::web::access::AdminAccess;
    let lang = app.lang;
    widgets::card(ui, |ui| {
        widgets::section_title(ui, tr(lang, "Rules", "Regeln"));
        widgets::field_row(
            ui,
            LABEL,
            tr(lang, "Allowed networks", "Erlaubte Netze"),
            tr(
                lang,
                "Only these networks may connect, e.g. 192.168.1.0/24 or a single address. None: every network. This computer always may.",
                "Nur diese Netze dürfen sich verbinden, z. B. 192.168.1.0/24 oder eine einzelne Adresse. Keine: jedes Netz. Dieser Computer darf immer.",
            ),
            |ui| network_list(ui, lang, "allowed", &mut web.allowed_networks),
        );
        widgets::field_row(
            ui,
            LABEL,
            tr(lang, "Block address after", "Adresse sperren nach"),
            tr(
                lang,
                "An address with this many wrong sign-ins within 24 hours is blocked until it is allowed again. This computer is never blocked.",
                "Eine Adresse mit so vielen falschen Anmeldungen innerhalb von 24 Stunden wird gesperrt, bis sie wieder zugelassen wird. Dieser Computer wird nie gesperrt.",
            ),
            |ui| {
                widgets::number_or(
                    ui,
                    &mut web.auto_block_after,
                    0..=1000,
                    tr(lang, "off", "aus"),
                );
                ui.label(tr(lang, "wrong sign-ins in 24 h", "Fehlversuchen in 24 h"));
            },
        );
        widgets::field_row(
            ui,
            LABEL,
            tr(lang, "Administration", "Verwaltung"),
            tr(
                lang,
                "Where users may manage users and roles and block addresses. Elsewhere they keep the other permissions of their role.",
                "Von wo Benutzer und Rollen verwaltet und Adressen gesperrt werden dürfen. Anderswo behalten Benutzer die übrigen Rechte ihrer Rolle.",
            ),
            |ui| {
                widgets::choice(
                    ui,
                    &mut web.admin_access,
                    AdminAccess::ThisComputer,
                    tr(lang, "This computer", "Dieser Computer"),
                );
                widgets::choice(
                    ui,
                    &mut web.admin_access,
                    AdminAccess::Networks,
                    tr(lang, "Selected networks", "Ausgewählte Netze"),
                );
                widgets::choice(
                    ui,
                    &mut web.admin_access,
                    AdminAccess::Anywhere,
                    tr(lang, "Anywhere", "Überall"),
                );
            },
        );
        widgets::reveal(
            ui,
            "web-admin-networks",
            web.admin_access == AdminAccess::Networks,
            |ui| {
                widgets::field_row(ui, LABEL, "", "", |ui| {
                    network_list(ui, lang, "admin", &mut web.admin_networks);
                });
            },
        );
    });
}

/// Geoblocking, transparent: the rule, the countries by name, what the
/// database is, a lookup to try, and what was refused.
fn countries_card(app: &mut AeternaApp, ui: &mut Ui, web: &mut WebSettings) {
    use crate::web::access::{Geo, GeoMode};
    use crate::web::countries;
    let lang = app.lang;
    let en = lang == Lang::En;
    let p = *palette(ui);
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Countries", "Länder"),
            tr(
                lang,
                "Every connection is looked up in a country database before anything else is done. Refused connections get 403 Forbidden and are counted below. Addresses of this computer and of private networks (10.x, 172.16–31.x, 192.168.x, fc00::/7, fe80::/10) are never checked. An address the database does not know counts as not listed.",
                "Jede Verbindung wird vor allem anderen in einer Länderdatenbank nachgeschlagen. Abgewiesene Verbindungen erhalten 403 Forbidden und werden unten gezählt. Adressen dieses Computers und privater Netze (10.x, 172.16–31.x, 192.168.x, fc00::/7, fe80::/10) werden nie geprüft. Eine Adresse, die die Datenbank nicht kennt, gilt als nicht gelistet.",
            ),
        );
        widgets::field_row(ui, LABEL, tr(lang, "Rule", "Regel"), "", |ui| {
            widgets::choice(ui, &mut web.geo.mode, GeoMode::Off, tr(lang, "Off", "Aus"));
            widgets::choice(
                ui,
                &mut web.geo.mode,
                GeoMode::Allow,
                tr(lang, "Allow only", "Nur zulassen"),
            );
            widgets::choice(
                ui,
                &mut web.geo.mode,
                GeoMode::Block,
                tr(lang, "Block", "Sperren"),
            );
        });
        let on = web.geo.mode != GeoMode::Off;
        widgets::reveal(ui, "web-geo", on, |ui| {
            // The chosen countries as removable chips, then a search to add.
            widgets::field_row(
                ui,
                LABEL,
                match web.geo.mode {
                    GeoMode::Block => tr(lang, "Blocked countries", "Gesperrte Länder"),
                    _ => tr(lang, "Allowed countries", "Erlaubte Länder"),
                },
                "",
                |ui| {
                    let mut remove = None;
                    for (i, code) in web.geo.countries.iter().enumerate() {
                        let label = format!(
                            "{} {} ×",
                            code.to_ascii_uppercase(),
                            countries::name(code, en)
                        );
                        if widgets::chip(ui, &label)
                            .on_hover_text(tr(lang, "Remove", "Entfernen"))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    }
                    if let Some(i) = remove {
                        web.geo.countries.remove(i);
                    }
                    if web.geo.countries.is_empty() {
                        ui.label(
                            RichText::new(tr(lang, "None yet", "Noch keine")).color(p.warning),
                        );
                    }
                },
            );
            widgets::field_row(
                ui,
                LABEL,
                tr(lang, "Add country", "Land hinzufügen"),
                "",
                |ui| {
                    ui.add(
                        widgets::text_field(&mut app.web.country_search)
                            .hint_text(tr(lang, "Name or code", "Name oder Code"))
                            .desired_width(220.0),
                    );
                },
            );
            let needle = app.web.country_search.trim().to_lowercase();
            if !needle.is_empty() {
                widgets::field_row(ui, LABEL, "", "", |ui| {
                    let mut shown = 0;
                    for (code, name_en, name_de) in countries::all() {
                        let name = if en { name_en } else { name_de };
                        let chosen = web
                            .geo
                            .countries
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(code));
                        let hit = code.eq_ignore_ascii_case(&needle)
                            || name.to_lowercase().contains(&needle)
                            || name_en.to_lowercase().contains(&needle);
                        if chosen || !hit {
                            continue;
                        }
                        if widgets::chip(ui, &format!("+ {code} {name}")).clicked() {
                            web.geo.countries.push(code.to_string());
                            app.web.country_search.clear();
                        }
                        shown += 1;
                        if shown == 12 {
                            break;
                        }
                    }
                });
            }
            widgets::field_row(
                ui,
                LABEL,
                tr(lang, "Database", "Datenbank"),
                tr(
                    lang,
                    "A country database in MaxMind format (.mmdb): DB-IP IP to Country Lite or MaxMind GeoLite2 Country, both free. Update it monthly; it is read when the web interface starts.",
                    "Eine Länderdatenbank im MaxMind-Format (.mmdb): DB-IP IP to Country Lite oder MaxMind GeoLite2 Country, beide kostenlos. Monatlich aktualisieren; sie wird beim Start der Web-Oberfläche gelesen.",
                ),
                |ui| {
                    path_field(
                        ui,
                        &mut web.geo.database,
                        tr(lang, "Choose…", "Auswählen…"),
                        ("MaxMind DB", &["mmdb"]),
                    );
                },
            );
            widgets::field_row(ui, LABEL, "", "", |ui| {
                ui.hyperlink_to(
                    "DB-IP Lite",
                    "https://db-ip.com/db/download/ip-to-country-lite",
                );
                widgets::secondary_text(ui, "·");
                ui.hyperlink_to(
                    "MaxMind GeoLite2",
                    "https://dev.maxmind.com/geoip/geolite2-free-geolocation-data",
                );
            });

            // What the database is, read again when the file changes.
            let path = web.geo.database.clone();
            let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let fresh = app
                .web
                .geo_db
                .as_ref()
                .is_some_and(|(p, s, _)| *p == path && *s == stamp);
            if !fresh && !path.as_os_str().is_empty() {
                app.web.geo_db = Some((
                    path.clone(),
                    stamp,
                    Geo::open_database(&path).map(std::sync::Arc::new),
                ));
            }
            if path.as_os_str().is_empty() {
                app.web.geo_db = None;
            }
            if let Some((_, _, db)) = &app.web.geo_db {
                match db {
                    Ok(geo) => {
                        let info = geo.info();
                        let built = info
                            .built
                            .map(|b| b.format("%Y-%m-%d").to_string())
                            .unwrap_or_else(|| "?".into());
                        fact(
                            ui,
                            tr(lang, "Database type", "Datenbanktyp"),
                            format!(
                                "{} · IPv{} · {} {built}",
                                info.database_type,
                                info.ip_version,
                                tr(lang, "built", "erstellt")
                            ),
                        );
                        widgets::field_row(
                            ui,
                            LABEL,
                            tr(lang, "Look up", "Nachschlagen"),
                            "",
                            |ui| {
                                ui.add(
                                    widgets::text_field(&mut app.web.geo_test)
                                        .hint_text("203.0.113.7")
                                        .desired_width(180.0),
                                );
                                let text = app.web.geo_test.trim();
                                if let Ok(ip) = text.parse::<std::net::IpAddr>() {
                                    let codes = web.geo.codes().unwrap_or_default();
                                    let country = geo.country(ip);
                                    let name = country
                                        .as_deref()
                                        .map(|c| format!("{c} {}", countries::name(c, en)))
                                        .unwrap_or_else(|| tr(lang, "unknown", "unbekannt").into());
                                    let (allowed, why) = if crate::web::access::is_local(ip) {
                                        (
                                            true,
                                            tr(
                                                lang,
                                                "private network, not checked",
                                                "privates Netz, nicht geprüft",
                                            ),
                                        )
                                    } else if geo.allows_with(ip, web.geo.mode, &codes) {
                                        (true, tr(lang, "allowed", "zugelassen"))
                                    } else {
                                        (false, tr(lang, "refused", "abgewiesen"))
                                    };
                                    ui.label(
                                        RichText::new(format!("{name} · {why}"))
                                            .color(if allowed { p.success } else { p.error }),
                                    );
                                }
                            },
                        );
                    }
                    Err(err) => {
                        widgets::field_row(
                            ui,
                            LABEL,
                            tr(lang, "Database", "Datenbank"),
                            "",
                            |ui| {
                                ui.add(egui::Label::new(RichText::new(err).color(p.error)).wrap());
                            },
                        );
                    }
                }
            } else {
                fact(
                    ui,
                    tr(lang, "Database type", "Datenbanktyp"),
                    tr(lang, "No database chosen", "Keine Datenbank gewählt"),
                );
            }
            let refused = app
                .web
                .server
                .as_ref()
                .map(|s| s.geo_refusals())
                .unwrap_or_default();
            let text = if refused.is_empty() {
                tr(lang, "None since the start", "Keine seit dem Start").to_string()
            } else {
                refused
                    .iter()
                    .map(|(c, n)| format!("{c} {n}"))
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            fact(ui, tr(lang, "Refused", "Abgewiesen"), text);
        });
    });
}

/// Blocked addresses: they apply at once, not with "Apply".
fn blocked_card(app: &mut AeternaApp, ui: &mut Ui) {
    use crate::web::access::Blocklist;
    let lang = app.lang;
    let config_file = app.paths.config_file.clone();
    widgets::card(ui, |ui| {
        let mut list = Blocklist::load(&config_file);
        widgets::section_title_info(
            ui,
            &format!(
                "{} ({})",
                tr(lang, "Blocked addresses", "Gesperrte Adressen"),
                list.entries.len()
            ),
            tr(
                lang,
                "Kept in web-blocked.json next to the configuration. A change applies at once, also to a running web interface, without signing anybody out.",
                "Gespeichert in web-blocked.json neben der Konfiguration. Eine Änderung gilt sofort, auch für eine laufende Web-Oberfläche, ohne dass jemand abgemeldet wird.",
            ),
        );
        let mut changed: Option<Record> = None;
        let mut unblock = None;
        if !list.entries.is_empty() {
            egui::Grid::new("web-blocked")
                .num_columns(4)
                .striped(true)
                .spacing([18.0, 8.0])
                .show(ui, |ui| {
                    for entry in list.entries.iter().rev() {
                        ui.label(RichText::new(&entry.address).monospace());
                        let by = if entry.by == "automatic" {
                            tr(lang, "automatic", "automatisch").to_string()
                        } else {
                            entry.by.clone()
                        };
                        widgets::secondary_text(
                            ui,
                            format!(
                                "{} · {by}",
                                lang.relative_time(entry.since.with_timezone(&chrono::Local))
                            ),
                        );
                        widgets::secondary_text(ui, &entry.reason);
                        if widgets::button(
                            ui,
                            ButtonKind::Quiet,
                            tr(lang, "Allow again", "Wieder zulassen"),
                            true,
                        )
                        .clicked()
                        {
                            unblock = Some(entry.address.clone());
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
        }
        if let Some(address) = unblock {
            list.remove(&address);
            changed = Some(Record::local("address-unblocked").target(address));
        }
        ui.horizontal(|ui| {
            ui.add(
                widgets::text_field(&mut app.web.block_address)
                    .hint_text(tr(lang, "Address or network", "Adresse oder Netz"))
                    .desired_width(220.0),
            );
            let ready = !app.web.block_address.trim().is_empty();
            if widgets::button(
                ui,
                ButtonKind::Secondary,
                tr(lang, "Block", "Sperren"),
                ready,
            )
            .clicked()
            {
                let address = app.web.block_address.trim().to_string();
                match list.add(&address, "", &audit::process_actor()) {
                    Ok(true) => {
                        changed = Some(Record::local("address-blocked").target(address));
                        app.web.block_address.clear();
                        app.web.block_note = None;
                    }
                    Ok(false) => {
                        app.web.block_note = Some(Note::Warn(
                            tr(lang, "Blocked already.", "Schon gesperrt.").into(),
                        ));
                    }
                    Err(err) => app.web.block_note = Some(Note::Bad(err)),
                }
            }
        });
        show_note(ui, &app.web.block_note);
        if let Some(record) = changed {
            match list.save(&config_file) {
                Ok(()) => audit::record(&config_file, record),
                Err(err) => app.notify(NoticeKind::Error, err.to_string()),
            }
        }
    });
}

fn ldap_card(app: &mut AeternaApp, ui: &mut Ui, web: &mut WebSettings, roles: &[String]) {
    use crate::web::ldap::{GroupRole, RoleSource};
    let lang = app.lang;
    let en = lang == Lang::En;
    let p = *palette(ui);
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            "LDAP / Active Directory",
            tr(
                lang,
                "Users sign in with their directory account. Users created here keep working besides, for example as an emergency administrator.",
                "Benutzer melden sich mit ihrem Verzeichniskonto an. Hier angelegte Benutzer funktionieren weiterhin, zum Beispiel als Notfall-Administrator.",
            ),
        );
        let l = &mut web.ldap;
        widgets::switch(
            ui,
            &mut l.enabled,
            tr(
                lang,
                "Enable LDAP authentication",
                "LDAP-Authentifizierung aktivieren",
            ),
        );
        let open = l.enabled;
        widgets::reveal(ui, "web-ldap", open, |ui| {
            let width = ui.available_width().min(560.0);
            let field = |ui: &mut Ui, label: &str| {
                ui.add_space(10.0);
                widgets::secondary_text(ui, label);
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
                    widgets::choice(ui, &mut kind, 2, tr(lang, "Own filter", "Eigener Filter"));
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
                widgets::text_field(&mut l.url)
                    .hint_text("ldaps://dc1.firma.local")
                    .desired_width(width),
            );
            let plain = l.url.trim().to_ascii_lowercase().starts_with("ldap://");
            if plain {
                widgets::switch(ui, &mut l.starttls, "StartTLS");
                if !l.starttls {
                    widgets::switch(
                        ui,
                        &mut l.allow_unencrypted,
                        tr(lang, "Allow unencrypted", "Unverschlüsselt erlauben"),
                    )
                    .on_hover_text(tr(
                        lang,
                        "For tests only: passwords can be read in the network.",
                        "Nur für Tests: Passwörter sind im Netz lesbar.",
                    ));
                }
            }

            field(
                ui,
                tr(
                    lang,
                    "Own certificate authority",
                    "Eigene Zertifizierungsstelle",
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
                widgets::text_field(&mut l.bind_dn)
                    .hint_text("CN=svc-backup,OU=Service,DC=firma,DC=local")
                    .desired_width(width),
            )
            .on_hover_text(tr(
                lang,
                "Empty: anonymous. With {user}, e.g. {user}@firma.local, users search with their own account.",
                "Leer: anonym. Mit {user}, z. B. {user}@firma.local, suchen Benutzer mit ihrem eigenen Konto.",
            ));
            if !l.bind_dn.trim().is_empty() && !l.binds_as_user() {
                ui.add(
                    widgets::text_field(&mut l.bind_password)
                        .password(true)
                        .hint_text(tr(lang, "Password", "Passwort"))
                        .desired_width(width.min(300.0)),
                );
            }

            field(ui, tr(lang, "Search base", "Suchbasis"));
            ui.add(
                widgets::text_field(&mut l.base_dn)
                    .hint_text("DC=firma,DC=local")
                    .desired_width(width),
            );

            field(ui, tr(lang, "User filter", "Benutzerfilter"));
            ui.add(
                widgets::text_field(&mut l.user_filter)
                    .desired_width(width)
                    .font(egui::TextStyle::Monospace),
            );

            field(ui, tr(lang, "Roles from", "Rollen aus"));
            ui.horizontal_wrapped(|ui| {
                widgets::choice(
                    ui,
                    &mut l.role_source,
                    RoleSource::Directory,
                    tr(lang, "Directory groups", "Verzeichnisgruppen"),
                )
                .on_hover_text(tr(
                    lang,
                    "Read again at every sign-in. Members of several groups get the role with the most permissions; members of none cannot sign in.",
                    "Bei jeder Anmeldung neu gelesen. Wer in mehreren Gruppen ist, bekommt die Rolle mit den meisten Rechten; wer in keiner ist, kann sich nicht anmelden.",
                ));
                widgets::choice(ui, &mut l.role_source, RoleSource::AeternaVault, "AeternaVault")
                    .on_hover_text(tr(
                        lang,
                        "The directory checks only the password. Roles are given and changed under Users.",
                        "Das Verzeichnis prüft nur das Passwort. Rollen werden unter Benutzer vergeben und geändert.",
                    ));
            });
            let by_groups = l.role_source == RoleSource::Directory;
            widgets::reveal(ui, "web-ldap-groups", by_groups, |ui| {
                ui.add_space(6.0);
                ui.add_space(4.0);
                let group_width = (width - 190.0).clamp(160.0, 380.0);
                for (label, group, hint) in [
                    (
                        users::Role::Admin,
                        &mut l.admin_group,
                        "CN=Backup-Admins,OU=Gruppen,DC=firma,DC=local",
                    ),
                    (
                        users::Role::Operator,
                        &mut l.operator_group,
                        "CN=Backup-Operatoren,OU=Gruppen,DC=firma,DC=local",
                    ),
                    (
                        users::Role::Viewer,
                        &mut l.viewer_group,
                        "CN=Backup-Leser,OU=Gruppen,DC=firma,DC=local",
                    ),
                ] {
                    ui.horizontal(|ui| {
                        ui.add(
                            widgets::text_field(group)
                                .hint_text(hint)
                                .desired_width(group_width),
                        );
                        ui.label(label.name(en));
                    });
                }
                let mut remove = None;
                for (i, mapping) in l.group_roles.iter_mut().enumerate() {
                    ui.push_id(("group-role", i), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                widgets::text_field(&mut mapping.group)
                                    .hint_text("CN=…,OU=Gruppen,DC=firma,DC=local")
                                    .desired_width(group_width),
                            );
                            egui::ComboBox::from_id_salt("role")
                                .selected_text(UserStore::role_label(&mapping.role, en))
                                .show_ui(ui, |ui| {
                                    for role in roles {
                                        ui.selectable_value(
                                            &mut mapping.role,
                                            role.clone(),
                                            UserStore::role_label(role, en),
                                        );
                                    }
                                });
                            if widgets::button(ui, ButtonKind::Quiet, "×", true).clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                }
                if let Some(i) = remove {
                    l.group_roles.remove(i);
                }
                if widgets::button(
                    ui,
                    ButtonKind::Quiet,
                    tr(lang, "+ Add group", "+ Gruppe hinzufügen"),
                    true,
                )
                .clicked()
                {
                    l.group_roles.push(GroupRole {
                        group: String::new(),
                        role: "viewer".into(),
                    });
                }
                widgets::switch(
                    ui,
                    &mut l.nested_groups,
                    tr(lang, "Nested groups", "Verschachtelte Gruppen"),
                )
                .on_hover_text(tr(
                    lang,
                    "Active Directory: membership through groups inside groups counts too.",
                    "Active Directory: Mitgliedschaft über Gruppen in Gruppen zählt mit.",
                ));
            });
            widgets::reveal(ui, "web-ldap-own-roles", !by_groups, |ui| {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    widgets::secondary_text(
                        ui,
                        tr(
                            lang,
                            "Role at the first sign-in",
                            "Rolle bei der ersten Anmeldung",
                        ),
                    );
                    egui::ComboBox::from_id_salt("ldap-default-role")
                        .selected_text(if l.default_role.is_empty() {
                            tr(lang, "None", "Keine").to_string()
                        } else {
                            UserStore::role_label(&l.default_role, en)
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut l.default_role,
                                String::new(),
                                tr(lang, "None", "Keine"),
                            );
                            for role in roles {
                                ui.selectable_value(
                                    &mut l.default_role,
                                    role.clone(),
                                    UserStore::role_label(role, en),
                                );
                            }
                        });
                });
            });

            ui.add_space(12.0);
            ui.separator();
            field(ui, tr(lang, "Test", "Testen"));
            ui.horizontal_wrapped(|ui| {
                let w = ((width - 16.0) / 2.0).clamp(140.0, 220.0);
                ui.add(
                    widgets::text_field(&mut app.web.ldap_test_user)
                        .hint_text(tr(lang, "User name", "Benutzername"))
                        .desired_width(w),
                );
                ui.add(
                    widgets::text_field(&mut app.web.ldap_test_password)
                        .password(true)
                        .hint_text(tr(lang, "Password", "Passwort"))
                        .desired_width(w),
                );
            });
            ui.add_space(6.0);
            let label = if app.web.ldap_testing {
                tr(lang, "Testing…", "Teste…")
            } else {
                tr(lang, "Test connection", "Verbindung testen")
            };
            if widgets::button(ui, ButtonKind::Secondary, label, !app.web.ldap_testing)
                .on_hover_text(tr(
                    lang,
                    "Without a password: connection and search. With user name and password: the whole sign-in including the role. Uses the values above, also before they are applied.",
                    "Ohne Passwort: Verbindung und Suche. Mit Benutzername und Passwort: die ganze Anmeldung einschließlich Rolle. Nutzt die Werte oben, auch vor dem Übernehmen.",
                ))
                .clicked()
            {
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
        });
    });
}

/// Roles: the built-in ones for reference, own ones with their permissions.
fn roles_card(app: &mut AeternaApp, ui: &mut Ui) {
    let lang = app.lang;
    let en = lang == Lang::En;
    let p = *palette(ui);
    let config_file = app.paths.config_file.clone();
    let mut store = UserStore::load(&config_file);
    let mut changed: Option<Record> = None;
    widgets::card(ui, |ui| {
        widgets::section_title_info(
            ui,
            tr(lang, "Roles", "Rollen"),
            tr(
                lang,
                "What users may do in the web interface. Viewer, Operator and Administrator are built in; own roles combine permissions freely.",
                "Was Benutzer in der Web-Oberfläche dürfen. Betrachter, Bediener und Administrator sind eingebaut; eigene Rollen kombinieren Rechte frei.",
            ),
        );
        let columns = (((ui.available_width() + 16.0) / 236.0) as usize).clamp(1, 4);
        let cell = (ui.available_width() - 16.0 * (columns - 1) as f32) / columns as f32;
        let names = store.role_names();
        let mut remove = None;
        for chunk in names.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                for name in chunk {
                    let builtin = users::Role::from_id(name).is_some();
                    let perms = store.role_permissions(name).unwrap_or_default();
                    ui.allocate_ui_with_layout(
                        egui::vec2(cell, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(cell);
                            egui::Frame::new()
                                .fill(p.raised)
                                .stroke(egui::Stroke::new(1.0, p.border))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::same(12))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(UserStore::role_label(name, en)).strong(),
                                        );
                                        if builtin {
                                            widgets::secondary_text(
                                                ui,
                                                tr(lang, "built in", "eingebaut"),
                                            );
                                        } else {
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    if widgets::button(
                                                        ui,
                                                        ButtonKind::Quiet,
                                                        "×",
                                                        true,
                                                    )
                                                    .on_hover_text(tr(
                                                        lang,
                                                        "Remove role",
                                                        "Rolle entfernen",
                                                    ))
                                                    .clicked()
                                                    {
                                                        remove = Some(name.clone());
                                                    }
                                                },
                                            );
                                        }
                                    });
                                    ui.add_space(4.0);
                                    let mut edited = perms.clone();
                                    for permission in users::Permission::ALL {
                                        let mut on = edited.contains(&permission);
                                        let response = ui
                                            .add_enabled_ui(!builtin, |ui| {
                                                ui.push_id((name, permission as u8), |ui| {
                                                    widgets::switch(
                                                        ui,
                                                        &mut on,
                                                        permission.name(en),
                                                    )
                                                })
                                                .inner
                                            })
                                            .inner;
                                        if response.changed() {
                                            if on {
                                                edited.push(permission);
                                            } else {
                                                edited.retain(|p| *p != permission);
                                            }
                                        }
                                    }
                                    if edited != perms {
                                        match store.set_role(name, edited.clone()) {
                                            Ok(()) => {
                                                changed = Some(
                                                    Record::local("role-changed")
                                                        .target(name.clone())
                                                        .details(vec![format!("{edited:?}")]),
                                                );
                                            }
                                            Err(err) => app.web.role_note = Some(Note::Bad(err)),
                                        }
                                    }
                                });
                        },
                    );
                }
            });
            ui.add_space(12.0);
        }
        if let Some(name) = remove {
            match store.remove_role(&name) {
                Ok(()) => changed = Some(Record::local("role-removed").target(name)),
                Err(err) => app.web.role_note = Some(Note::Bad(err)),
            }
        }
        ui.horizontal(|ui| {
            ui.add(
                widgets::text_field(&mut app.web.new_role_name)
                    .hint_text(tr(lang, "Name of a new role", "Name einer neuen Rolle"))
                    .desired_width(220.0),
            );
            let ready = !app.web.new_role_name.trim().is_empty();
            if widgets::button(
                ui,
                ButtonKind::Secondary,
                tr(lang, "Add role", "Rolle hinzufügen"),
                ready,
            )
            .clicked()
            {
                let name = app.web.new_role_name.trim().to_string();
                if store.role_permissions(&name).is_some() {
                    app.web.role_note = Some(Note::Bad(
                        tr(
                            lang,
                            "This role exists already.",
                            "Diese Rolle gibt es schon.",
                        )
                        .into(),
                    ));
                } else {
                    match store.set_role(&name, vec![users::Permission::ViewActivity]) {
                        Ok(()) => {
                            changed = Some(Record::local("role-added").target(name));
                            app.web.new_role_name.clear();
                            app.web.role_note = None;
                        }
                        Err(err) => app.web.role_note = Some(Note::Bad(err)),
                    }
                }
            }
        });
        show_note(ui, &app.web.role_note);
    });
    if let Some(record) = changed {
        match store.save(&config_file) {
            Ok(()) => audit::record(&config_file, record),
            Err(err) => app.notify(NoticeKind::Error, err.to_string()),
        }
    }
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
                "What a user may do comes from the role. A user limited to some jobs sees only those and their destinations. Directory users appear here after their first sign-in.",
                "Was ein Benutzer darf, bestimmt die Rolle. Ein auf einige Aufträge beschränkter Benutzer sieht nur diese und ihre Ziele. Verzeichnisbenutzer erscheinen hier nach ihrer ersten Anmeldung.",
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
                    widgets::hint_icon(
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
                    let role_names = store.role_names();
                    let admin_names: Vec<String> = store
                        .users
                        .iter()
                        .filter(|u| store.can(u, users::Permission::ManageUsers))
                        .map(|u| u.name.clone())
                        .collect();
                    let manages = |role: &str| {
                        users::Role::from_id(role).is_some_and(|r| r == users::Role::Admin)
                            || store.roles.iter().any(|d| {
                                d.name.eq_ignore_ascii_case(role)
                                    && d.permissions.contains(&users::Permission::ManageUsers)
                            })
                    };
                    let manages: Vec<String> =
                        role_names.iter().filter(|r| manages(r)).cloned().collect();
                    for user in store.users.iter_mut() {
                        let last_admin = admins == 1 && admin_names.contains(&user.name);
                        let directory = user.source == UserSource::Directory;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&user.name).strong());
                            if user.disabled {
                                ui.colored_label(p.error, tr(lang, "locked", "gesperrt"));
                            }
                        });
                        if directory {
                            ui.label(UserStore::role_label(&user.role, en))
                                .on_hover_text(tr(
                                    lang,
                                    "From the directory groups",
                                    "Aus den Verzeichnisgruppen",
                                ));
                        } else {
                            let before = user.role.clone();
                            egui::ComboBox::from_id_salt(("role", user.name.clone()))
                                .selected_text(UserStore::role_label(&user.role, en))
                                .show_ui(ui, |ui| {
                                    for role in &role_names {
                                        ui.selectable_value(
                                            &mut user.role,
                                            role.clone(),
                                            UserStore::role_label(role, en),
                                        );
                                    }
                                });
                            if user.role != before {
                                if last_admin && !manages.contains(&user.role) {
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
                                                "role: {before} → {}",
                                                user.role
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
                                                widgets::text_field(text)
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
                                                "Only these jobs",
                                                "Nur diese Aufträge",
                                            ))
                                            .color(p.text_secondary),
                                        );
                                        let before = user.jobs.clone();
                                        for (id, name) in &schedules {
                                            let mut on = user.jobs.contains(id);
                                            if widgets::switch(ui, &mut on, name).changed() {
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

        ui.add_space(12.0);
        ui.label(RichText::new(tr(lang, "Add user", "Benutzer hinzufügen")).strong());
        ui.add_space(4.0);
        let mut add = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
            ui.add(
                widgets::text_field(&mut app.web.new_name)
                    .hint_text(tr(lang, "User name", "Benutzername"))
                    .desired_width(200.0),
            );
            ui.add(
                widgets::text_field(&mut app.web.new_password)
                    .password(true)
                    .hint_text(tr(lang, "Initial password", "Startpasswort"))
                    .desired_width(200.0),
            );
            let names = store.role_names();
            egui::ComboBox::from_id_salt("new-role")
                .selected_text(UserStore::role_label(&app.web.new_role, en))
                .width(160.0)
                .show_ui(ui, |ui| {
                    for role in &names {
                        ui.selectable_value(
                            &mut app.web.new_role,
                            role.clone(),
                            UserStore::role_label(role, en),
                        );
                    }
                });
            let ready = !app.web.new_name.trim().is_empty() && !app.web.new_password.is_empty();
            add = widgets::button(
                ui,
                ButtonKind::Secondary,
                tr(lang, "Add", "Hinzufügen"),
                ready,
            )
            .clicked();
        });
        if !app.web.new_password.is_empty() {
            ui.add_space(4.0);
            ui.allocate_ui(egui::vec2(420.0_f32.min(ui.available_width()), 0.0), |ui| {
                crate::gui::dialogs::strength_meter(ui, &app.web.new_password, lang);
            });
            let length = app.web.new_password.chars().count();
            if length < recommended as usize {
                note(
                    ui,
                    if en {
                        format!("{length} of {recommended} recommended characters")
                    } else {
                        format!("{length} von {recommended} empfohlenen Zeichen")
                    },
                );
            }
        }
        if add {
            let name = app.web.new_name.trim().to_string();
            match store.add(&name, &app.web.new_role, &app.web.new_password) {
                Ok(()) => {
                    changed = Some(
                        Record::local("user-added")
                            .target(name.clone())
                            .details(vec![format!("role: {}", app.web.new_role)]),
                    );
                    let text = format!("{} {name}.", tr(lang, "Added:", "Hinzugefügt:"));
                    app.web.user_note = Some(
                        match users::password_advice(&app.web.new_password, recommended, en) {
                            Some(advice) => Note::Warn(format!("{text} {advice}")),
                            None => Note::Good(text),
                        },
                    );
                    app.web.new_name.clear();
                    app.web.new_password.clear();
                }
                Err(err) => app.web.user_note = Some(Note::Bad(err)),
            }
        }
        show_note(ui, &app.web.user_note);
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
                    widgets::text_field(&mut app.web.audit_filter)
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
