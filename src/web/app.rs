//! Pages and actions of the web interface. Pages are rendered on the server
//! as plain HTML with one style sheet and no scripts; the overview refreshes
//! itself every 15 seconds.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Local, Utc};

use super::http::{Request, Response, encode};
use super::users::{self, Limiter, Role, Session, Sessions, User, UserStore};
use super::{Control, Network};
use crate::audit::{self, Record};
use crate::config::{Config, WebSettings};
use crate::engine::verify::VerifyReport;
use crate::engine::{CancelToken, snapshots};
use crate::i18n::Lang;
use crate::paths::AppPaths;

const STYLE: &str = include_str!("style.css");

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Escapes text for HTML.
pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A check of a backup started over the web.
struct Check {
    backup: String,
    destination: PathBuf,
    by: String,
    started: DateTime<Utc>,
    result: Option<Result<VerifyReport, String>>,
}

pub struct WebApp {
    paths: AppPaths,
    settings: WebSettings,
    networks: Vec<Network>,
    control: Arc<dyn Control>,
    sessions: Mutex<Sessions>,
    limiter: Mutex<Limiter>,
    /// Secrets shown for setting up one-time codes, until confirmed.
    pending_totp: Mutex<HashMap<String, String>>,
    checks: Arc<Mutex<Vec<Check>>>,
    /// Passwords set by an administrator, shown once on the next page.
    flash: Mutex<HashMap<String, String>>,
}

/// One request with its session.
struct Ctx<'a> {
    req: &'a Request,
    token: String,
    session: Session,
    user: User,
    config: Config,
    en: bool,
}

impl Ctx<'_> {
    fn t<'s>(&self, en: &'s str, de: &'s str) -> &'s str {
        if self.en { en } else { de }
    }

    fn actor(&self) -> String {
        format!("web:{}", self.user.name)
    }

    fn record(&self, action: &str) -> Record {
        Record::new(self.actor(), self.req.peer.to_string(), action)
    }

    fn lang(&self) -> Lang {
        if self.en { Lang::En } else { Lang::De }
    }
}

fn csrf_field(session: &Session) -> String {
    format!(
        r#"<input type="hidden" name="csrf" value="{}">"#,
        esc(&session.csrf)
    )
}

impl WebApp {
    pub fn new(
        paths: AppPaths,
        settings: WebSettings,
        networks: Vec<Network>,
        control: Arc<dyn Control>,
    ) -> Self {
        Self {
            paths,
            settings,
            networks,
            control,
            sessions: Mutex::new(Sessions::default()),
            limiter: Mutex::new(Limiter::default()),
            pending_totp: Mutex::new(HashMap::new()),
            checks: Arc::new(Mutex::new(Vec::new())),
            flash: Mutex::new(HashMap::new()),
        }
    }

    fn config(&self) -> Config {
        crate::config::load_or_create(&self.paths).config
    }

    fn idle(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.session_minutes.max(1)) * 60)
    }

    fn cookie_name(&self, req: &Request) -> &'static str {
        // `__Host-` cookies are only accepted over HTTPS and only for this host.
        if req.tls { "__Host-av" } else { "av" }
    }

    fn session_cookie(&self, req: &Request, token: &str) -> String {
        let secure = if req.tls { "; Secure" } else { "" };
        format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Strict{secure}",
            self.cookie_name(req)
        )
    }

    fn clear_cookie(&self, req: &Request) -> String {
        let secure = if req.tls { "; Secure" } else { "" };
        format!(
            "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}",
            self.cookie_name(req)
        )
    }

    fn allowed(&self, ip: IpAddr) -> bool {
        let ip = super::http::canonical(ip);
        ip.is_loopback() || self.networks.is_empty() || self.networks.iter().any(|n| n.contains(ip))
    }

    /// Requests for this computer only must name it: a web page elsewhere
    /// cannot use a made-up host name that points here (DNS rebinding).
    fn host_ok(&self, req: &Request) -> bool {
        if !self.settings.local_only() {
            return true;
        }
        let Some(host) = req.header("host") else {
            return false;
        };
        let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
        matches!(name, "localhost" | "127.0.0.1" | "[::1]")
    }

    /// A form must come from a page of this interface.
    fn same_origin(req: &Request) -> bool {
        let Some(host) = req.header("host") else {
            return false;
        };
        match req.header("origin").or_else(|| req.header("referer")) {
            Some(origin) if origin != "null" => origin
                .split_once("://")
                .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
                .is_some_and(|h| h.eq_ignore_ascii_case(host)),
            // Browsers send Origin with every form; tools may send neither.
            _ => true,
        }
    }

    pub fn handle(&self, req: Request) -> Response {
        if !self.allowed(req.peer) {
            tracing::warn!(
                "web interface: refused {} (not an allowed network)",
                req.peer
            );
            return Response::text(403, "Forbidden");
        }
        if !self.host_ok(&req) {
            return Response::text(403, "Forbidden");
        }
        if req.method == "GET" && req.path == "/style.css" {
            return Response::with_type(200, "text/css; charset=utf-8", STYLE.as_bytes().to_vec())
                .header("Cache-Control", "max-age=3600");
        }
        if req.method == "POST" && !Self::same_origin(&req) {
            return Response::text(403, "Forbidden");
        }
        let config = self.config();
        let en = Lang::resolve(config.language) == Lang::En;
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/login") => return self.login_page(&req, en, None),
            ("POST", "/login") => return self.login(&req, en),
            ("GET", "/login/code") => return self.code_page(&req, en, None),
            ("POST", "/login/code") => return self.code(&req, en),
            _ => {}
        }

        let Some((token, session)) = req
            .cookie(self.cookie_name(&req))
            .and_then(|t| lock(&self.sessions).touch(&t, self.idle()).map(|s| (t, s)))
        else {
            return Response::redirect("/login");
        };
        if session.pending_code {
            return Response::redirect("/login/code");
        }
        let store = UserStore::load(&self.paths.config_file);
        let Some(user) = store.find(&session.user).filter(|u| !u.disabled).cloned() else {
            lock(&self.sessions).end(&token);
            return Response::redirect("/login");
        };
        let ctx = Ctx {
            req: &req,
            token,
            session,
            user,
            config,
            en,
        };
        if req.method == "POST" {
            let form = req.form();
            let sent = form.get("csrf").map(String::as_str).unwrap_or_default();
            if !users::equal(sent.as_bytes(), ctx.session.csrf.as_bytes()) {
                return Response::text(
                    403,
                    "Forbidden: the form has expired, please reload the page",
                );
            }
        }

        // A new password or the set-up of one-time codes comes first.
        let needs_code = self.settings.require_two_factor && ctx.user.totp.is_none();
        let account_only = ctx.user.must_change_password || needs_code;
        if account_only && !ctx.req.path.starts_with("/account") && ctx.req.path != "/logout" {
            return Response::redirect("/account");
        }

        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/logout") => self.logout(&ctx),
            ("GET", "/") => self.dashboard(&ctx),
            ("GET", "/backups") => self.backups(&ctx),
            ("GET", "/activity") => self.activity(&ctx),
            ("GET", "/account") => self.account(&ctx, None),
            ("POST", "/account/password") => self.change_password(&ctx),
            ("POST", "/account/2fa/start") => self.totp_start(&ctx),
            ("POST", "/account/2fa/confirm") => self.totp_confirm(&ctx),
            ("POST", "/account/2fa/off") => self.totp_off(&ctx),
            ("POST", path) if ctx.user.may(Role::Operator) => match path {
                "/jobs/run" => self.job_run(&ctx),
                "/jobs/cancel" => self.job_cancel(&ctx),
                "/jobs/switch" => self.job_switch(&ctx),
                "/backups/check" => self.check_start(&ctx),
                _ if ctx.user.may(Role::Admin) => match path {
                    "/users/add" => self.user_add(&ctx),
                    "/users/change" => self.user_change(&ctx),
                    _ => self.not_found(&ctx),
                },
                _ => self.forbidden(&ctx),
            },
            ("GET", "/audit") if ctx.user.may(Role::Admin) => self.audit_page(&ctx),
            ("GET", "/users") if ctx.user.may(Role::Admin) => self.users_page(&ctx),
            ("GET", "/audit" | "/users") | ("POST", _) => self.forbidden(&ctx),
            _ => self.not_found(&ctx),
        }
    }

    // --- frame ------------------------------------------------------------------------

    fn page(&self, ctx: &Ctx<'_>, title: &str, body: &str, refresh: bool) -> Response {
        let mut nav = vec![
            ("/", ctx.t("Overview", "Übersicht")),
            ("/backups", ctx.t("Backups", "Sicherungen")),
            ("/activity", ctx.t("Activity", "Verlauf")),
        ];
        if ctx.user.may(Role::Admin) {
            nav.push(("/users", ctx.t("Users", "Benutzer")));
            nav.push(("/audit", ctx.t("Audit log", "Audit-Log")));
        }
        nav.push(("/account", ctx.t("Account", "Konto")));
        let links: String = nav
            .iter()
            .map(|(href, label)| {
                let current = if *href == ctx.req.path {
                    r#" aria-current="page""#
                } else {
                    ""
                };
                format!(r#"<a href="{href}"{current}>{}</a>"#, esc(label))
            })
            .collect();
        let refresh = if refresh {
            r#"<meta http-equiv="refresh" content="15">"#
        } else {
            ""
        };
        Response::html(format!(
            r#"<!DOCTYPE html><html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">{refresh}<title>{title} · AeternaVault</title><link rel="stylesheet" href="/style.css"></head><body><header><strong>AeternaVault</strong><span class="computer">{computer}</span><nav>{links}</nav><form method="post" action="/logout" class="inline">{csrf}<button class="link">{logout} ({user})</button></form></header><main><h1>{title}</h1>{body}</main></body></html>"#,
            lang = if ctx.en { "en" } else { "de" },
            title = esc(title),
            computer = esc(&crate::platform::computer_name()),
            csrf = csrf_field(&ctx.session),
            logout = ctx.t("Sign out", "Abmelden"),
            user = esc(&ctx.user.name),
        ))
    }

    fn bare(en: bool, title: &str, body: &str) -> Response {
        Response::html(format!(
            r#"<!DOCTYPE html><html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title} · AeternaVault</title><link rel="stylesheet" href="/style.css"></head><body class="bare"><main class="narrow"><h1>AeternaVault</h1><p class="muted">{computer}</p>{body}</main></body></html>"#,
            lang = if en { "en" } else { "de" },
            title = esc(title),
            computer = esc(&crate::platform::computer_name()),
        ))
    }

    fn forbidden(&self, ctx: &Ctx<'_>) -> Response {
        let mut r = self.page(
            ctx,
            ctx.t("Not allowed", "Nicht erlaubt"),
            &format!(
                "<p>{}</p>",
                ctx.t(
                    "Your role does not allow this.",
                    "Deine Rolle erlaubt das nicht."
                )
            ),
            false,
        );
        r.status = 403;
        r
    }

    fn not_found(&self, ctx: &Ctx<'_>) -> Response {
        let mut r = self.page(ctx, ctx.t("Not found", "Nicht gefunden"), "", false);
        r.status = 404;
        r
    }

    fn back(path: &str, message: &str) -> Response {
        Response::redirect(&format!("{path}?msg={}", encode(message)))
    }

    fn message(ctx: &Ctx<'_>) -> String {
        ctx.req
            .query
            .get("msg")
            .filter(|m| !m.is_empty())
            .map(|m| format!(r#"<p class="notice">{}</p>"#, esc(m)))
            .unwrap_or_default()
    }

    // --- sign-in ------------------------------------------------------------------------

    fn login_page(&self, req: &Request, en: bool, error: Option<&str>) -> Response {
        let t = |a: &'static str, b: &'static str| if en { a } else { b };
        let store = UserStore::load(&self.paths.config_file);
        if store.users.is_empty() {
            return Self::bare(
                en,
                t("Sign in", "Anmelden"),
                &format!(
                    "<p>{}</p><pre>aeternavault-cli web user add NAME --role admin</pre>",
                    t(
                        "No user has been created yet. Create the first administrator on this computer: in AeternaVault under Web → Users, or on the command line:",
                        "Es gibt noch keinen Benutzer. Lege den ersten Administrator auf diesem Computer an: in AeternaVault unter Web → Benutzer oder in der Kommandozeile:"
                    )
                ),
            );
        }
        let error = error
            .map(|e| format!(r#"<p class="error">{}</p>"#, esc(e)))
            .unwrap_or_default();
        let _ = req;
        Self::bare(
            en,
            t("Sign in", "Anmelden"),
            &format!(
                r#"{error}<form method="post" action="/login" class="stack"><label>{user}<input name="user" autocomplete="username" required autofocus></label><label>{pass}<input name="password" type="password" autocomplete="current-password" required></label><button>{go}</button></form>"#,
                user = t("User name", "Benutzername"),
                pass = t("Password", "Passwort"),
                go = t("Sign in", "Anmelden"),
            ),
        )
    }

    fn lockout(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.lockout_minutes.max(1)) * 60)
    }

    fn login(&self, req: &Request, en: bool) -> Response {
        let t = |a: &'static str, b: &'static str| if en { a } else { b };
        let form = req.form();
        let name = form
            .get("user")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let password = form.get("password").cloned().unwrap_or_default();
        let ip = req.peer.to_string();
        let user_key = format!("user:{}", name.to_lowercase());
        let wrong = t(
            "User name or password is not correct.",
            "Benutzername oder Passwort ist nicht korrekt.",
        );
        let failed = |reason: &str| {
            audit::record(
                &self.paths.config_file,
                Record::new(format!("web:{name}"), ip.clone(), "sign-in")
                    .details(vec![reason.to_string()])
                    .failed(),
            );
        };

        {
            let mut limiter = lock(&self.limiter);
            let attempts = self.settings.lockout_attempts;
            let wait = self.lockout();
            if let Some(left) = limiter
                .locked(&ip, attempts.saturating_mul(3), wait)
                .or_else(|| limiter.locked(&user_key, attempts, wait))
            {
                failed("locked after too many wrong attempts");
                let minutes = left.as_secs().div_ceil(60);
                let message = if en {
                    format!("Too many wrong attempts. Please wait {minutes} minutes.")
                } else {
                    format!("Zu viele falsche Versuche. Bitte {minutes} Minuten warten.")
                };
                let mut r = self.login_page(req, en, Some(&message));
                r.status = 429;
                return r;
            }
        }

        let mut store = UserStore::load(&self.paths.config_file);
        let user = store.find(&name).cloned();
        // The hash is computed even for unknown names, so the answer takes
        // as long and does not tell which names exist.
        let stored = user.as_ref().map(|u| u.password.clone()).unwrap_or_else(|| {
            "argon2id$19456$2$00000000000000000000000000000000$0000000000000000000000000000000000000000000000000000000000000000".into()
        });
        let password_ok = users::verify_password(&password, &stored);
        let Some(user) = user.filter(|_| password_ok) else {
            let mut limiter = lock(&self.limiter);
            limiter.failed(&ip);
            limiter.failed(&user_key);
            drop(limiter);
            failed(if store.find(&name).is_some() {
                "wrong password"
            } else {
                "unknown user"
            });
            return self.login_page(req, en, Some(wrong));
        };
        if user.disabled {
            failed("user is disabled");
            return self.login_page(req, en, Some(wrong));
        }
        {
            let mut limiter = lock(&self.limiter);
            limiter.succeeded(&user_key);
            limiter.succeeded(&ip);
        }
        let pending = user.totp.is_some();
        let (token, _) = lock(&self.sessions).create(&user.name, req.peer, pending);
        if !pending {
            if let Some(u) = store.find_mut(&user.name) {
                u.last_sign_in = Some(Utc::now());
            }
            let _ = store.save(&self.paths.config_file);
            audit::record(
                &self.paths.config_file,
                Record::new(format!("web:{}", user.name), ip, "sign-in"),
            );
        }
        Response::redirect(if pending { "/login/code" } else { "/" })
            .header("Set-Cookie", &self.session_cookie(req, &token))
    }

    fn pending_session(&self, req: &Request) -> Option<(String, Session)> {
        let token = req.cookie(self.cookie_name(req))?;
        let session = lock(&self.sessions).touch(&token, Duration::from_secs(300))?;
        session.pending_code.then_some((token, session))
    }

    fn code_page(&self, req: &Request, en: bool, error: Option<&str>) -> Response {
        let t = |a: &'static str, b: &'static str| if en { a } else { b };
        if self.pending_session(req).is_none() {
            return Response::redirect("/login");
        }
        let error = error
            .map(|e| format!(r#"<p class="error">{}</p>"#, esc(e)))
            .unwrap_or_default();
        Self::bare(
            en,
            t("One-time code", "Einmalcode"),
            &format!(
                r#"{error}<form method="post" action="/login/code" class="stack"><label>{label}<input name="code" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9 ]*" required autofocus></label><button>{go}</button></form>"#,
                label = t(
                    "Code from your authenticator app",
                    "Code aus deiner Authenticator-App"
                ),
                go = t("Continue", "Weiter"),
            ),
        )
    }

    fn code(&self, req: &Request, en: bool) -> Response {
        let t = |a: &'static str, b: &'static str| if en { a } else { b };
        let Some((token, session)) = self.pending_session(req) else {
            return Response::redirect("/login");
        };
        let mut store = UserStore::load(&self.paths.config_file);
        let Some(secret) = store.find(&session.user).and_then(|u| u.totp.clone()) else {
            return Response::redirect("/login");
        };
        let user_key = format!("code:{}", session.user.to_lowercase());
        let ip = req.peer.to_string();
        let code = req.form().get("code").cloned().unwrap_or_default();
        let step = users::check_totp(&secret, &code, users::current_step());
        let mut limiter = lock(&self.limiter);
        let ok = step.is_some_and(|s| limiter.use_code(&session.user, s))
            && limiter
                .locked(&user_key, self.settings.lockout_attempts, self.lockout())
                .is_none();
        if !ok {
            limiter.failed(&user_key);
            let locked = limiter
                .locked(&user_key, self.settings.lockout_attempts, self.lockout())
                .is_some();
            drop(limiter);
            audit::record(
                &self.paths.config_file,
                Record::new(format!("web:{}", session.user), ip, "sign-in")
                    .details(vec!["wrong one-time code".into()])
                    .failed(),
            );
            if locked {
                lock(&self.sessions).end(&token);
                return Response::redirect("/login");
            }
            return self.code_page(
                req,
                en,
                Some(t("The code is not correct.", "Der Code ist nicht korrekt.")),
            );
        }
        limiter.succeeded(&user_key);
        drop(limiter);
        lock(&self.sessions).finish_code(&token);
        if let Some(u) = store.find_mut(&session.user) {
            u.last_sign_in = Some(Utc::now());
        }
        let _ = store.save(&self.paths.config_file);
        audit::record(
            &self.paths.config_file,
            Record::new(format!("web:{}", session.user), ip, "sign-in")
                .details(vec!["with one-time code".into()]),
        );
        Response::redirect("/")
    }

    fn logout(&self, ctx: &Ctx<'_>) -> Response {
        lock(&self.sessions).end(&ctx.token);
        audit::record(&self.paths.config_file, ctx.record("sign-out"));
        Response::redirect("/login").header("Set-Cookie", &self.clear_cookie(ctx.req))
    }

    // --- overview -----------------------------------------------------------------------

    fn outcome_class(outcome: crate::state::AutomaticOutcome) -> &'static str {
        use crate::state::AutomaticOutcome as O;
        match outcome {
            O::Complete => "good",
            O::CompleteWithNotes | O::DestinationUnavailable | O::Interrupted => "warn",
            O::AlreadyRunning => "muted",
            O::NeedsPassphrase | O::Failed => "bad",
        }
    }

    fn when(ctx: &Ctx<'_>, at: DateTime<Utc>) -> String {
        let local = at.with_timezone(&Local);
        format!(
            r#"<span title="{}">{}</span>"#,
            local.format("%Y-%m-%d %H:%M:%S"),
            esc(&ctx.lang().relative_time(local))
        )
    }

    fn dashboard(&self, ctx: &Ctx<'_>) -> Response {
        let lang = ctx.lang();
        let config = &ctx.config;
        let state = crate::state::State::load(&self.paths.config_file);
        let operator = ctx.user.may(Role::Operator);
        let mut body = Self::message(ctx);

        // Running now.
        if let Some((label, progress)) = self.control.running() {
            let percent = (progress.bytes_done * 100)
                .checked_div(progress.bytes_total)
                .unwrap_or(0);
            let cancel = if operator {
                format!(
                    r#"<form method="post" action="/jobs/cancel" class="inline">{}<button class="secondary">{}</button></form>"#,
                    csrf_field(&ctx.session),
                    ctx.t("Cancel", "Abbrechen")
                )
            } else {
                String::new()
            };
            body.push_str(&format!(
                r#"<section class="card running"><h2>{now}: {label}</h2><progress max="100" value="{percent}"></progress><p>{files} · {bytes} {cancel}</p></section>"#,
                now = ctx.t("Running", "Läuft"),
                label = esc(&label),
                files = esc(&format!("{} / {}", lang.count(progress.files_done), lang.count(progress.files_total))),
                bytes = esc(&lang.bytes_of(progress.bytes_done, progress.bytes_total)),
            ));
        }

        // Overdue.
        let overdue = crate::notify::overdue_jobs(config, &state, Utc::now());
        for (schedule, last) in overdue
            .iter()
            .filter(|(s, _)| ctx.user.may_use_job(Some(&s.id)))
        {
            let since = last
                .map(|l| Self::when(ctx, l))
                .unwrap_or_else(|| ctx.t("never", "noch nie").into());
            body.push_str(&format!(
                r#"<p class="error">{}: {} ({} {})</p>"#,
                ctx.t("Overdue", "Überfällig"),
                esc(&crate::automatic::describe(schedule, config, lang)),
                ctx.t("last successful backup", "letzte erfolgreiche Sicherung"),
                since
            ));
        }

        // Jobs.
        let mut rows = String::new();
        let now = Local::now();
        for schedule in config
            .schedules
            .iter()
            .filter(|s| ctx.user.may_use_job(Some(&s.id)))
        {
            let entry = state.schedules.get(&schedule.id);
            let last = entry.and_then(|e| e.last_run.as_ref());
            let last_text = match last {
                Some(run) => format!(
                    r#"<span class="badge {}">{}</span> {} · {}"#,
                    Self::outcome_class(run.outcome),
                    esc(lang.notify_status(run.outcome)),
                    Self::when(ctx, run.at),
                    esc(&lang.files(run.files)),
                ),
                None => ctx.t("not yet", "noch nicht").into(),
            };
            let problems = last
                .filter(|r| !r.problems.is_empty())
                .map(|r| {
                    format!(
                        r#"<details><summary>{}</summary><pre>{}</pre></details>"#,
                        ctx.t(
                            "What went wrong and what to do",
                            "Was schiefging und was zu tun ist"
                        ),
                        esc(&lang.problem_report(&r.problems, false))
                    )
                })
                .or_else(|| {
                    last.filter(|r| !r.message.is_empty() && !r.outcome.is_success())
                        .map(|r| format!("<pre>{}</pre>", esc(&r.message)))
                })
                .unwrap_or_default();
            let next = if schedule.enabled {
                crate::automatic::timing::next_occurrence(schedule, now)
                    .map(|n| esc(&n.format("%Y-%m-%d %H:%M").to_string()))
                    .unwrap_or_else(|| ctx.t("at start", "beim Start").into())
            } else {
                format!(
                    r#"<span class="badge muted">{}</span>"#,
                    ctx.t("off", "aus")
                )
            };
            let destination = config
                .for_schedule(schedule)
                .destination
                .display()
                .to_string();
            let actions = if operator {
                let csrf = csrf_field(&ctx.session);
                format!(
                    r#"<form method="post" action="/jobs/run" class="inline">{csrf}<input type="hidden" name="id" value="{id}"><button>{run}</button></form> <form method="post" action="/jobs/switch" class="inline">{csrf}<input type="hidden" name="id" value="{id}"><input type="hidden" name="on" value="{on}"><button class="secondary">{switch}</button></form>"#,
                    id = esc(&schedule.id),
                    run = ctx.t("Run now", "Jetzt ausführen"),
                    on = if schedule.enabled { "0" } else { "1" },
                    switch = if schedule.enabled {
                        ctx.t("Switch off", "Ausschalten")
                    } else {
                        ctx.t("Switch on", "Einschalten")
                    },
                )
            } else {
                String::new()
            };
            rows.push_str(&format!(
                r#"<tr><td><strong>{name}</strong><br><span class="muted">{dest}</span></td><td>{last_text}{problems}</td><td class="nowrap">{next}</td><td class="actions">{actions}</td></tr>"#,
                name = esc(&crate::automatic::describe(schedule, config, lang)),
                dest = esc(&destination),
            ));
        }
        let all_now = if operator && ctx.user.jobs.is_empty() {
            format!(
                r#"<form method="post" action="/jobs/run" class="inline">{}<button>{}</button></form>"#,
                csrf_field(&ctx.session),
                ctx.t(
                    "Back up all ticked folders now",
                    "Alle ausgewählten Ordner jetzt sichern"
                )
            )
        } else {
            String::new()
        };
        body.push_str(&format!(
            r#"<section class="card"><h2>{jobs}</h2>{table}{all_now}</section>"#,
            jobs = ctx.t("Jobs", "Aufträge"),
            table = if rows.is_empty() {
                format!("<p class=\"muted\">{}</p>", ctx.t("No jobs.", "Keine Aufträge."))
            } else {
                format!(
                    r#"<table><thead><tr><th>{}</th><th>{}</th><th>{}</th><th></th></tr></thead><tbody>{rows}</tbody></table>"#,
                    ctx.t("Job", "Auftrag"),
                    ctx.t("Last run", "Letzter Lauf"),
                    ctx.t("Next run", "Nächster Lauf"),
                )
            },
        ));

        // Destinations.
        let mut destinations: Vec<PathBuf> = Vec::new();
        if ctx.user.jobs.is_empty() && !config.destination.as_os_str().is_empty() {
            destinations.push(config.destination.clone());
        }
        for s in config
            .schedules
            .iter()
            .filter(|s| ctx.user.may_use_job(Some(&s.id)))
        {
            let d = config.for_schedule(s).destination;
            if !d.as_os_str().is_empty() && !destinations.contains(&d) {
                destinations.push(d);
            }
        }
        let mut dest_rows = String::new();
        for (n, d) in destinations.iter().enumerate() {
            let reachable = snapshots::destination_reachable(d);
            let space = crate::platform::disk_space(d)
                .map(|(free, total)| format!("{} / {}", lang.bytes(free), lang.bytes(total)))
                .unwrap_or_else(|| "?".into());
            let list = snapshots::list(d, None).unwrap_or_default();
            let newest = list
                .first()
                .map(|s| {
                    format!(
                        "{} · {}",
                        esc(&s.id),
                        esc(lang.status(s.header.as_ref().map(|h| h.status)))
                    )
                })
                .unwrap_or_else(|| "–".into());
            dest_rows.push_str(&format!(
                r#"<tr><td><a href="/backups?d={n}">{path}</a></td><td>{state}</td><td>{space}</td><td>{count}</td><td>{newest}</td></tr>"#,
                path = esc(&d.display().to_string()),
                state = if reachable {
                    format!(r#"<span class="badge good">{}</span>"#, ctx.t("reachable", "erreichbar"))
                } else {
                    format!(r#"<span class="badge bad">{}</span>"#, ctx.t("not reachable", "nicht erreichbar"))
                },
                space = esc(&space),
                count = list.len(),
            ));
        }
        body.push_str(&format!(
            r#"<section class="card"><h2>{}</h2><table><thead><tr><th>{}</th><th></th><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>{dest_rows}</tbody></table></section>"#,
            ctx.t("Destinations", "Ziele"),
            ctx.t("Folder", "Ordner"),
            ctx.t("Free / size", "Frei / Größe"),
            ctx.t("Backups", "Sicherungen"),
            ctx.t("Newest", "Neueste"),
        ));

        // Recent activity.
        body.push_str(&format!(
            r#"<section class="card"><h2>{}</h2>{}<p><a href="/activity">{}</a></p></section>"#,
            ctx.t("Recent activity", "Letzte Aktivität"),
            self.activity_table(ctx, 8),
            ctx.t("All activity", "Gesamter Verlauf")
        ));
        self.page(ctx, ctx.t("Overview", "Übersicht"), &body, true)
    }

    fn activity_table(&self, ctx: &Ctx<'_>, limit: usize) -> String {
        let lang = ctx.lang();
        let entries = crate::history::load(&self.paths.config_file);
        let rows: String = entries
            .iter()
            .rev()
            .take(limit)
            .map(|entry| {
                let (title, detail, severity) = lang.history_entry(&entry.event);
                let class = match severity {
                    crate::history::Severity::Good => "good",
                    crate::history::Severity::Neutral => "muted",
                    crate::history::Severity::Warning => "warn",
                    crate::history::Severity::Problem => "bad",
                };
                format!(
                    r#"<tr><td class="nowrap">{}</td><td><span class="dot {class}"></span>{}</td><td>{}</td></tr>"#,
                    Self::when(ctx, entry.at),
                    esc(&title),
                    esc(&detail)
                )
            })
            .collect();
        if rows.is_empty() {
            format!(
                r#"<p class="muted">{}</p>"#,
                ctx.t("Nothing yet.", "Noch nichts.")
            )
        } else {
            format!("<table><tbody>{rows}</tbody></table>")
        }
    }

    fn activity(&self, ctx: &Ctx<'_>) -> Response {
        let body = self.activity_table(ctx, 300);
        self.page(ctx, ctx.t("Activity", "Verlauf"), &body, false)
    }

    // --- jobs -------------------------------------------------------------------------

    fn job_run(&self, ctx: &Ctx<'_>) -> Response {
        let id = ctx.req.form().get("id").cloned().filter(|i| !i.is_empty());
        if !ctx.user.may_use_job(id.as_deref()) {
            return self.forbidden(ctx);
        }
        let name = match &id {
            Some(id) => match ctx.config.schedules.iter().find(|s| &s.id == id) {
                Some(s) => crate::automatic::describe(s, &ctx.config, ctx.lang()),
                None => {
                    return Self::back(
                        "/",
                        ctx.t("This job does not exist.", "Diesen Auftrag gibt es nicht."),
                    );
                }
            },
            None => ctx
                .t("all ticked folders", "alle ausgewählten Ordner")
                .to_string(),
        };
        let result = self.control.run(id);
        let record = ctx.record("job-run").target(name.clone());
        match result {
            Ok(()) => {
                audit::record(&self.paths.config_file, record);
                Self::back("/", &format!("{}: {name}", ctx.t("Started", "Gestartet")))
            }
            Err(err) => {
                audit::record(
                    &self.paths.config_file,
                    record.details(vec![err.clone()]).failed(),
                );
                Self::back("/", &err)
            }
        }
    }

    fn job_cancel(&self, ctx: &Ctx<'_>) -> Response {
        let Some((label, _)) = self.control.running() else {
            return Response::redirect("/");
        };
        self.control.cancel();
        audit::record(
            &self.paths.config_file,
            ctx.record("job-cancel").target(label),
        );
        Self::back("/", ctx.t("Cancelled.", "Abgebrochen."))
    }

    fn job_switch(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let id = form.get("id").cloned().unwrap_or_default();
        let on = form.get("on").is_some_and(|v| v == "1");
        if !ctx.user.may_use_job(Some(&id)) {
            return self.forbidden(ctx);
        }
        let mut config = self.config();
        let Some(schedule) = config.schedules.iter_mut().find(|s| s.id == id) else {
            return Self::back(
                "/",
                ctx.t("This job does not exist.", "Diesen Auftrag gibt es nicht."),
            );
        };
        schedule.enabled = on;
        if on {
            crate::state::State::update(&self.paths.config_file, |state| {
                state.schedules.entry(id.clone()).or_default().armed_at = Some(Utc::now());
            });
        }
        let record = ctx.record(if on {
            "job-switched-on"
        } else {
            "job-switched-off"
        });
        if let Err(err) = config.save_as(&self.paths.config_file, record) {
            return Self::back("/", &err.to_string());
        }
        self.control.config_changed(&config);
        Response::redirect("/")
    }

    // --- backups ----------------------------------------------------------------------

    /// Destinations this user may see: the general one (all jobs only) and
    /// those of the user's jobs, with the settings to read them.
    fn destinations(&self, ctx: &Ctx<'_>) -> Vec<Config> {
        let mut out: Vec<Config> = Vec::new();
        if ctx.user.jobs.is_empty() && !ctx.config.destination.as_os_str().is_empty() {
            out.push(ctx.config.clone());
        }
        for s in ctx
            .config
            .schedules
            .iter()
            .filter(|s| ctx.user.may_use_job(Some(&s.id)))
        {
            let c = ctx.config.for_schedule(s);
            if !c.destination.as_os_str().is_empty()
                && !out.iter().any(|o| o.destination == c.destination)
            {
                out.push(c);
            }
        }
        out
    }

    fn backups(&self, ctx: &Ctx<'_>) -> Response {
        let lang = ctx.lang();
        let destinations = self.destinations(ctx);
        let chosen: usize = ctx
            .req
            .query
            .get("d")
            .and_then(|d| d.parse().ok())
            .filter(|d| *d < destinations.len())
            .unwrap_or(0);
        let mut body = Self::message(ctx);
        if destinations.len() > 1 {
            let tabs: String = destinations
                .iter()
                .enumerate()
                .map(|(n, c)| {
                    let current = if n == chosen {
                        r#" aria-current="page""#
                    } else {
                        ""
                    };
                    format!(
                        r#"<a href="/backups?d={n}"{current}>{}</a>"#,
                        esc(&c.destination.display().to_string())
                    )
                })
                .collect();
            body.push_str(&format!(r#"<nav class="tabs">{tabs}</nav>"#));
        }

        // Checks started over the web.
        let checks = lock(&self.checks);
        if !checks.is_empty() {
            let rows: String = checks
                .iter()
                .rev()
                .take(10)
                .map(|c| {
                    let result = match &c.result {
                        None => format!(
                            r#"<span class="badge muted">{}</span>"#,
                            ctx.t("running", "läuft")
                        ),
                        Some(Ok(r)) if r.is_ok() => format!(
                            r#"<span class="badge good">{}</span> {}"#,
                            ctx.t("complete", "vollständig"),
                            esc(&lang.files(r.files))
                        ),
                        Some(Ok(r)) => {
                            let problems = crate::state::Problems {
                                check_unreadable: r.unreadable.clone(),
                                check_damaged: r.damaged.clone(),
                                check_missing: r.missing.clone(),
                                ..Default::default()
                            };
                            format!(
                                r#"<span class="badge bad">{}</span><pre>{}</pre>"#,
                                ctx.t("problems", "Probleme"),
                                esc(&lang.problem_report(&problems, false))
                            )
                        }
                        Some(Err(e)) => format!(
                            r#"<span class="badge bad">{}</span> {}"#,
                            ctx.t("failed", "fehlgeschlagen"),
                            esc(e)
                        ),
                    };
                    format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                        esc(&c.backup),
                        esc(&c.destination.display().to_string()),
                        esc(&format!(
                            "{} · {}",
                            c.by,
                            c.started.with_timezone(&Local).format("%H:%M")
                        )),
                        result
                    )
                })
                .collect();
            body.push_str(&format!(
                r#"<section class="card"><h2>{}</h2><table><tbody>{rows}</tbody></table></section>"#,
                ctx.t("Checks", "Prüfungen")
            ));
        }
        drop(checks);

        let Some(config) = destinations.get(chosen) else {
            body.push_str(&format!(
                "<p class=\"muted\">{}</p>",
                ctx.t("No destination is set.", "Kein Ziel festgelegt.")
            ));
            return self.page(ctx, ctx.t("Backups", "Sicherungen"), &body, false);
        };
        let key = crate::automatic::unattended_key(&self.paths, config);
        let list = match snapshots::list(&config.destination, key.as_ref()) {
            Ok(list) => list,
            Err(err) => {
                body.push_str(&format!(
                    r#"<p class="error">{}</p>"#,
                    esc(&lang.error_message(&err))
                ));
                return self.page(ctx, ctx.t("Backups", "Sicherungen"), &body, false);
            }
        };
        let operator = ctx.user.may(Role::Operator);
        let rows: String = list
            .iter()
            .map(|s| {
                let stats = s.total_stats();
                let check = if operator && !s.needs_unlock() {
                    format!(
                        r#"<form method="post" action="/backups/check" class="inline">{}<input type="hidden" name="d" value="{chosen}"><input type="hidden" name="id" value="{}"><button class="secondary">{}</button></form>"#,
                        csrf_field(&ctx.session),
                        esc(&s.id),
                        ctx.t("Check", "Prüfen")
                    )
                } else {
                    String::new()
                };
                let encrypted = if s.needs_unlock() {
                    ctx.t("locked", "gesperrt")
                } else if s.needs_key() {
                    ctx.t("yes", "ja")
                } else {
                    ctx.t("no", "nein")
                };
                format!(
                    r#"<tr><td><strong>{id}</strong>{label}</td><td>{computer}</td><td>{status}</td><td>{files}</td><td>{size}</td><td>{encrypted}</td><td>{check}</td></tr>"#,
                    id = esc(&s.id),
                    label = s.label.as_ref().map(|l| format!("<br><span class=\"muted\">{}</span>", esc(l))).unwrap_or_default(),
                    computer = esc(&s.computer),
                    status = esc(lang.status(s.header.as_ref().map(|h| h.status))),
                    files = stats.as_ref().map(|st| lang.count(st.files)).unwrap_or_default(),
                    size = stats.as_ref().map(|st| lang.bytes(st.bytes)).unwrap_or_default(),
                )
            })
            .collect();
        body.push_str(&format!(
            r#"<section class="card"><h2>{dest}</h2><table><thead><tr><th>{b}</th><th>{c}</th><th>{st}</th><th>{f}</th><th>{sz}</th><th>{e}</th><th></th></tr></thead><tbody>{rows}</tbody></table></section>"#,
            dest = esc(&config.destination.display().to_string()),
            b = ctx.t("Backup", "Sicherung"),
            c = ctx.t("Computer", "Computer"),
            st = ctx.t("Status", "Status"),
            f = ctx.t("Files", "Dateien"),
            sz = ctx.t("Size", "Größe"),
            e = ctx.t("Encrypted", "Verschlüsselt"),
        ));
        if list.iter().any(|s| s.needs_unlock()) {
            body.push_str(&format!(
                r#"<p class="muted">{}</p>"#,
                ctx.t(
                    "Locked backups can only be checked when the key is remembered on this computer for automatic backups. Passphrases are never entered over the web.",
                    "Gesperrte Sicherungen lassen sich nur prüfen, wenn der Schlüssel für automatische Sicherungen auf diesem Computer gemerkt ist. Passphrasen werden nie über das Web eingegeben."
                )
            ));
        }
        self.page(ctx, ctx.t("Backups", "Sicherungen"), &body, false)
    }

    fn check_start(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let destinations = self.destinations(ctx);
        let Some(config) = form
            .get("d")
            .and_then(|d| d.parse::<usize>().ok())
            .and_then(|d| destinations.get(d))
            .cloned()
        else {
            return self.forbidden(ctx);
        };
        let id = form.get("id").cloned().unwrap_or_default();
        let key = crate::automatic::unattended_key(&self.paths, &config);
        let Some(snapshot) = snapshots::list(&config.destination, key.as_ref())
            .ok()
            .and_then(|l| l.into_iter().find(|s| s.id == id))
        else {
            return Self::back(
                "/backups",
                ctx.t(
                    "This backup was not found.",
                    "Diese Sicherung wurde nicht gefunden.",
                ),
            );
        };
        {
            let mut checks = lock(&self.checks);
            if checks.iter().any(|c| c.result.is_none()) {
                return Self::back(
                    "/backups",
                    ctx.t(
                        "A check is already running.",
                        "Es läuft bereits eine Prüfung.",
                    ),
                );
            }
            checks.push(Check {
                backup: id.clone(),
                destination: config.destination.clone(),
                by: ctx.user.name.clone(),
                started: Utc::now(),
                result: None,
            });
            let len = checks.len();
            if len > 20 {
                checks.drain(..len - 20);
            }
        }
        audit::record(
            &self.paths.config_file,
            ctx.record("backup-check")
                .target(format!("{} ({})", id, config.destination.display())),
        );
        let (checks, paths) = (self.checks.clone(), self.paths.clone());
        let _ = std::thread::Builder::new()
            .name("aeterna-web-check".into())
            .spawn(move || {
                let _background = crate::platform::BackgroundThread::enter();
                let result = crate::engine::verify::verify(
                    &snapshot,
                    key.as_ref(),
                    false,
                    &CancelToken::default(),
                    &mut |_| {},
                )
                .map_err(|e| e.to_string());
                if let Ok(report) = &result {
                    crate::history::record(
                        &paths.config_file,
                        crate::history::Event::Verified {
                            snapshot: snapshot.id.clone(),
                            files: report.files,
                            damaged: report.damaged.len() as u64,
                            missing: report.missing.len() as u64,
                            unreadable: report.unreadable.len() as u64,
                        },
                    );
                }
                if let Some(check) = lock(&checks).iter_mut().rev().find(|c| c.result.is_none()) {
                    check.result = Some(result);
                }
            });
        Self::back(
            "/backups",
            ctx.t(
                "The check runs; reload this page to see the result.",
                "Die Prüfung läuft; lade die Seite neu, um das Ergebnis zu sehen.",
            ),
        )
    }

    // --- audit and users ----------------------------------------------------------------

    fn audit_page(&self, ctx: &Ctx<'_>) -> Response {
        let filter = ctx
            .req
            .query
            .get("q")
            .cloned()
            .unwrap_or_default()
            .to_lowercase();
        let verified = audit::verify(&self.paths.config_file);
        let mut body = match verified {
            Ok(v) if v.broken.is_empty() => format!(
                r#"<p><span class="badge good">{}</span> {} {}</p>"#,
                ctx.t("intact", "intakt"),
                v.entries,
                ctx.t(
                    "entries, none changed afterwards.",
                    "Einträge, keiner nachträglich verändert."
                )
            ),
            Ok(v) => format!(
                r#"<p class="error">{} {:?}</p>"#,
                ctx.t(
                    "These lines were changed or removed afterwards:",
                    "Diese Zeilen wurden nachträglich verändert oder entfernt:"
                ),
                v.broken
            ),
            Err(e) => format!(r#"<p class="error">{}</p>"#, esc(&e.to_string())),
        };
        body.push_str(&format!(
            r#"<form method="get" action="/audit" class="inline"><input name="q" value="{}" placeholder="{}"><button class="secondary">{}</button></form>"#,
            esc(&filter),
            ctx.t("user, action, address …", "Benutzer, Aktion, Adresse …"),
            ctx.t("Filter", "Filtern")
        ));
        let rows: String = audit::load(&self.paths.config_file)
            .iter()
            .rev()
            .filter(|e| {
                filter.is_empty()
                    || [&e.actor, &e.source, &e.action, &e.target]
                        .iter()
                        .any(|f| f.to_lowercase().contains(&filter))
                    || e.details.iter().any(|d| d.to_lowercase().contains(&filter))
            })
            .take(500)
            .map(|e| {
                format!(
                    r#"<tr><td class="nowrap">{}</td><td class="nowrap">{}</td><td>{}</td><td class="nowrap"><span class="dot {}"></span>{}</td><td>{}{}</td></tr>"#,
                    esc(&e.at.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()),
                    esc(&e.actor),
                    esc(&e.source),
                    if e.ok { "good" } else { "bad" },
                    esc(&e.action),
                    esc(&e.target),
                    if e.details.is_empty() {
                        String::new()
                    } else {
                        format!("<ul class=\"details\">{}</ul>", e.details.iter().map(|d| format!("<li>{}</li>", esc(d))).collect::<String>())
                    }
                )
            })
            .collect();
        body.push_str(&format!(
            r#"<table><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>{rows}</tbody></table>"#,
            ctx.t("When", "Wann"),
            ctx.t("Who", "Wer"),
            ctx.t("From", "Von wo"),
            ctx.t("What", "Was"),
            ctx.t("Details", "Details"),
        ));
        self.page(ctx, ctx.t("Audit log", "Audit-Log"), &body, false)
    }

    fn role_select(ctx: &Ctx<'_>, current: Role) -> String {
        let options: String = Role::ALL
            .iter()
            .map(|r| {
                let selected = if *r == current { " selected" } else { "" };
                format!(
                    r#"<option value="{}"{selected}>{}</option>"#,
                    serde_json::to_value(r)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default(),
                    r.name(ctx.en)
                )
            })
            .collect();
        format!(r#"<select name="role">{options}</select>"#)
    }

    fn job_boxes(ctx: &Ctx<'_>, chosen: &[String]) -> String {
        if ctx.config.schedules.is_empty() {
            return String::new();
        }
        let boxes: String = ctx
            .config
            .schedules
            .iter()
            .map(|s| {
                let checked = if chosen.contains(&s.id) { " checked" } else { "" };
                format!(
                    r#"<label class="check"><input type="checkbox" name="jobs" value="{}"{checked}> {}</label>"#,
                    esc(&s.id),
                    esc(&crate::automatic::describe(s, &ctx.config, ctx.lang()))
                )
            })
            .collect();
        format!(
            r#"<fieldset><legend>{}</legend>{boxes}</fieldset>"#,
            ctx.t(
                "Only these jobs (none ticked: all jobs and all destinations)",
                "Nur diese Aufträge (keiner angehakt: alle Aufträge und Ziele)"
            )
        )
    }

    fn users_page(&self, ctx: &Ctx<'_>) -> Response {
        let store = UserStore::load(&self.paths.config_file);
        let mut body = Self::message(ctx);
        if let Some(password) = lock(&self.flash).remove(&ctx.token) {
            body.push_str(&format!(
                r#"<p class="notice">{} <code>{}</code></p>"#,
                ctx.t(
                    "New password, shown only now. The user must change it after signing in:",
                    "Neues Passwort, wird nur jetzt angezeigt. Der Benutzer muss es nach der Anmeldung ändern:"
                ),
                esc(&password)
            ));
        }
        let csrf = csrf_field(&ctx.session);
        for user in &store.users {
            let last = user
                .last_sign_in
                .map(|t| Self::when(ctx, t))
                .unwrap_or_else(|| ctx.t("never", "noch nie").into());
            body.push_str(&format!(
                r#"<section class="card"><h2>{name} {disabled}</h2><p class="muted">{role_label}: {role} · {twofa} · {last_label}: {last}</p>
<form method="post" action="/users/change" class="stack">{csrf}<input type="hidden" name="user" value="{name}">
<label>{role_label} {select}</label>{jobs}
<div class="row"><button name="do" value="save">{save}</button>
<button name="do" value="{toggle}" class="secondary">{toggle_label}</button>
<button name="do" value="password" class="secondary">{reset}</button>
<button name="do" value="2fa" class="secondary">{reset2fa}</button>
<button name="do" value="remove" class="danger">{remove}</button></div></form></section>"#,
                name = esc(&user.name),
                disabled = if user.disabled { format!(r#"<span class="badge bad">{}</span>"#, ctx.t("disabled", "gesperrt")) } else { String::new() },
                role_label = ctx.t("Role", "Rolle"),
                role = user.role.name(ctx.en),
                twofa = if user.totp.is_some() { ctx.t("one-time codes on", "Einmalcodes an") } else { ctx.t("no one-time codes", "keine Einmalcodes") },
                last_label = ctx.t("last sign-in", "letzte Anmeldung"),
                select = Self::role_select(ctx, user.role),
                jobs = Self::job_boxes(ctx, &user.jobs),
                save = ctx.t("Save", "Speichern"),
                toggle = if user.disabled { "enable" } else { "disable" },
                toggle_label = if user.disabled { ctx.t("Unlock", "Entsperren") } else { ctx.t("Lock", "Sperren") },
                reset = ctx.t("New password", "Neues Passwort"),
                reset2fa = ctx.t("Reset one-time codes", "Einmalcodes zurücksetzen"),
                remove = ctx.t("Remove", "Entfernen"),
            ));
        }
        body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2><form method="post" action="/users/add" class="stack">{csrf}
<label>{name}<input name="user" required></label><label>{pass}<input name="password" type="password" autocomplete="new-password" required minlength="{min}"></label>
<label>{role_label} {select}</label>{jobs}<button>{add}</button></form>
<p class="muted">{roles}</p></section>"#,
            title = ctx.t("Add user", "Benutzer hinzufügen"),
            name = ctx.t("User name", "Benutzername"),
            pass = ctx.t("Password (the user must change it after the first sign-in)", "Passwort (muss nach der ersten Anmeldung geändert werden)"),
            min = self.settings.min_password_length.max(8),
            role_label = ctx.t("Role", "Rolle"),
            select = Self::role_select(ctx, Role::Viewer),
            jobs = Self::job_boxes(ctx, &[]),
            add = ctx.t("Add", "Hinzufügen"),
            roles = ctx.t(
                "Viewer: overview, backups, activity. Operator: also runs, cancels and switches jobs and checks backups. Administrator: also users and the audit log.",
                "Betrachter: Übersicht, Sicherungen, Verlauf. Bediener: startet, stoppt und schaltet außerdem Aufträge und prüft Sicherungen. Administrator: außerdem Benutzer und Audit-Log."
            ),
        ));
        self.page(ctx, ctx.t("Users", "Benutzer"), &body, false)
    }

    fn parse_role(text: &str) -> Option<Role> {
        serde_json::from_value(serde_json::Value::String(text.to_string())).ok()
    }

    fn user_add(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let name = form.get("user").cloned().unwrap_or_default();
        let password = form.get("password").cloned().unwrap_or_default();
        let Some(role) = form.get("role").and_then(|r| Self::parse_role(r)) else {
            return Self::back("/users", "?");
        };
        let jobs = ctx.req.form_all("jobs");
        let mut store = UserStore::load(&self.paths.config_file);
        if let Err(err) = store.add(&name, role, &password, self.settings.min_password_length) {
            return Self::back("/users", &err);
        }
        if let Some(user) = store.find_mut(&name) {
            user.jobs = jobs.clone();
            user.must_change_password = true;
        }
        if let Err(err) = store.save(&self.paths.config_file) {
            return Self::back("/users", &err.to_string());
        }
        let mut details = vec![format!("role: {}", role.name(true))];
        if !jobs.is_empty() {
            details.push(format!("jobs: {}", jobs.join(", ")));
        }
        audit::record(
            &self.paths.config_file,
            ctx.record("user-added")
                .target(name.trim())
                .details(details),
        );
        Self::back("/users", ctx.t("User added.", "Benutzer hinzugefügt."))
    }

    fn user_change(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let name = form.get("user").cloned().unwrap_or_default();
        let action = form.get("do").cloned().unwrap_or_default();
        let mut store = UserStore::load(&self.paths.config_file);
        let Some(before) = store.find(&name).cloned() else {
            return Self::back(
                "/users",
                ctx.t(
                    "This user does not exist.",
                    "Diesen Benutzer gibt es nicht.",
                ),
            );
        };
        let last_admin = store.is_last_admin(&name);
        let refuse_last_admin = ctx.t(
            "The last administrator cannot be removed, locked or demoted.",
            "Der letzte Administrator kann nicht entfernt, gesperrt oder herabgestuft werden.",
        );
        let mut details = Vec::new();
        let mut end_sessions = false;
        let mut new_password = None;
        let audit_action;
        match action.as_str() {
            "save" => {
                let role = form
                    .get("role")
                    .and_then(|r| Self::parse_role(r))
                    .unwrap_or(before.role);
                if last_admin && role != Role::Admin {
                    return Self::back("/users", refuse_last_admin);
                }
                let jobs = ctx.req.form_all("jobs");
                let user = store.find_mut(&name).expect("checked above");
                if user.role != role {
                    details.push(format!(
                        "role: {} → {}",
                        user.role.name(true),
                        role.name(true)
                    ));
                }
                if user.jobs != jobs {
                    details.push(format!(
                        "jobs: [{}] → [{}]",
                        user.jobs.join(", "),
                        jobs.join(", ")
                    ));
                }
                user.role = role;
                user.jobs = jobs;
                audit_action = "user-changed";
            }
            "disable" | "enable" => {
                if action == "disable" && last_admin {
                    return Self::back("/users", refuse_last_admin);
                }
                store.find_mut(&name).expect("checked above").disabled = action == "disable";
                end_sessions = action == "disable";
                audit_action = if action == "disable" {
                    "user-locked"
                } else {
                    "user-unlocked"
                };
            }
            "password" => {
                let password = users::random_password();
                let user = store.find_mut(&name).expect("checked above");
                user.password = users::hash_password(&password);
                user.must_change_password = true;
                new_password = Some(password);
                end_sessions = true;
                audit_action = "user-password-reset";
            }
            "2fa" => {
                store.find_mut(&name).expect("checked above").totp = None;
                end_sessions = true;
                audit_action = "user-one-time-codes-reset";
            }
            "remove" => {
                if last_admin {
                    return Self::back("/users", refuse_last_admin);
                }
                store.users.retain(|u| !u.name.eq_ignore_ascii_case(&name));
                end_sessions = true;
                audit_action = "user-removed";
            }
            _ => return Self::back("/users", "?"),
        }
        if let Err(err) = store.save(&self.paths.config_file) {
            return Self::back("/users", &err.to_string());
        }
        if end_sessions {
            lock(&self.sessions).end_user(&name);
        }
        if let Some(password) = new_password {
            lock(&self.flash).insert(ctx.token.clone(), password);
        }
        audit::record(
            &self.paths.config_file,
            ctx.record(audit_action)
                .target(before.name)
                .details(details),
        );
        Self::back("/users", ctx.t("Saved.", "Gespeichert."))
    }

    // --- own account ------------------------------------------------------------------

    fn account(&self, ctx: &Ctx<'_>, error: Option<&str>) -> Response {
        let mut body = Self::message(ctx);
        if let Some(e) = error {
            body.push_str(&format!(r#"<p class="error">{}</p>"#, esc(e)));
        }
        if ctx.user.must_change_password {
            body.push_str(&format!(
                r#"<p class="notice">{}</p>"#,
                ctx.t(
                    "Please choose a new password first.",
                    "Bitte wähle zuerst ein neues Passwort."
                )
            ));
        } else if self.settings.require_two_factor && ctx.user.totp.is_none() {
            body.push_str(&format!(
                r#"<p class="notice">{}</p>"#,
                ctx.t(
                    "One-time codes are required here. Please set them up first.",
                    "Einmalcodes sind hier vorgeschrieben. Bitte richte sie zuerst ein."
                )
            ));
        }
        let csrf = csrf_field(&ctx.session);
        body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2><p class="muted">{role}: {r}</p><form method="post" action="/account/password" class="stack">{csrf}
<label>{current}<input name="current" type="password" autocomplete="current-password" required></label>
<label>{new}<input name="new" type="password" autocomplete="new-password" required minlength="{min}"></label>
<label>{again}<input name="again" type="password" autocomplete="new-password" required></label><button>{save}</button></form></section>"#,
            title = ctx.t("Password", "Passwort"),
            role = ctx.t("Role", "Rolle"),
            r = ctx.user.role.name(ctx.en),
            current = ctx.t("Current password", "Aktuelles Passwort"),
            new = ctx.t("New password", "Neues Passwort"),
            again = ctx.t("New password again", "Neues Passwort wiederholen"),
            min = self.settings.min_password_length.max(8),
            save = ctx.t("Change password", "Passwort ändern"),
        ));
        let pending = lock(&self.pending_totp)
            .get(&ctx.user.name.to_lowercase())
            .cloned();
        let twofa = match (&ctx.user.totp, pending) {
            (Some(_), _) => format!(
                r#"<p><span class="badge good">{on}</span></p>{off}"#,
                on = ctx.t("on", "an"),
                off = if self.settings.require_two_factor {
                    String::new()
                } else {
                    format!(
                        r#"<form method="post" action="/account/2fa/off" class="stack">{csrf}<label>{pw}<input name="current" type="password" required></label><button class="secondary">{label}</button></form>"#,
                        pw = ctx.t("Password", "Passwort"),
                        label = ctx.t("Switch off", "Ausschalten")
                    )
                }
            ),
            (None, Some(secret)) => format!(
                r#"<p>{explain}</p><p><code class="secret">{secret}</code></p><p class="muted small">{uri}</p>
<form method="post" action="/account/2fa/confirm" class="stack">{csrf}<label>{code}<input name="code" inputmode="numeric" autocomplete="one-time-code" required></label><button>{confirm}</button></form>"#,
                explain = ctx.t(
                    "Add this key in your authenticator app (e.g. Aegis, Google Authenticator, Microsoft Authenticator, 1Password), as a time-based key, then enter the code it shows:",
                    "Trage diesen Schlüssel in deiner Authenticator-App ein (z. B. Aegis, Google Authenticator, Microsoft Authenticator, 1Password), zeitbasiert, und gib dann den angezeigten Code ein:"
                ),
                secret = esc(&secret),
                uri = esc(&users::totp_uri(&secret, &ctx.user.name)),
                code = ctx.t("Code", "Code"),
                confirm = ctx.t("Confirm", "Bestätigen"),
            ),
            (None, None) => format!(
                r#"<p>{explain}</p><form method="post" action="/account/2fa/start">{csrf}<button>{label}</button></form>"#,
                explain = ctx.t(
                    "Sign-in then also needs a code from an authenticator app.",
                    "Die Anmeldung braucht dann zusätzlich einen Code aus einer Authenticator-App."
                ),
                label = ctx.t("Set up", "Einrichten")
            ),
        };
        body.push_str(&format!(
            r#"<section class="card"><h2>{}</h2>{twofa}</section>"#,
            ctx.t(
                "One-time codes (two-factor sign-in)",
                "Einmalcodes (Zwei-Faktor-Anmeldung)"
            )
        ));
        self.page(ctx, ctx.t("Account", "Konto"), &body, false)
    }

    fn change_password(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let current = form.get("current").cloned().unwrap_or_default();
        let new = form.get("new").cloned().unwrap_or_default();
        if Some(&new) != form.get("again") {
            return self.account(
                ctx,
                Some(ctx.t(
                    "The new passwords differ.",
                    "Die neuen Passwörter stimmen nicht überein.",
                )),
            );
        }
        if !users::verify_password(&current, &ctx.user.password) {
            audit::record(
                &self.paths.config_file,
                ctx.record("password-changed")
                    .details(vec!["wrong current password".into()])
                    .failed(),
            );
            return self.account(
                ctx,
                Some(ctx.t(
                    "The current password is not correct.",
                    "Das aktuelle Passwort ist nicht korrekt.",
                )),
            );
        }
        if new == current {
            return self.account(
                ctx,
                Some(ctx.t(
                    "Please choose a different password.",
                    "Bitte wähle ein anderes Passwort.",
                )),
            );
        }
        if let Err(err) = users::check_password(&new, self.settings.min_password_length) {
            return self.account(ctx, Some(&err));
        }
        let mut store = UserStore::load(&self.paths.config_file);
        if let Some(user) = store.find_mut(&ctx.user.name) {
            user.password = users::hash_password(&new);
            user.must_change_password = false;
        }
        if let Err(err) = store.save(&self.paths.config_file) {
            return self.account(ctx, Some(&err.to_string()));
        }
        // Other sessions of this user end; this one stays.
        {
            let mut sessions = lock(&self.sessions);
            let keep = sessions.touch(&ctx.token, self.idle());
            sessions.end_user(&ctx.user.name);
            if keep.is_some() {
                let (token, _) = sessions.create(&ctx.user.name, ctx.req.peer, false);
                audit::record(&self.paths.config_file, ctx.record("password-changed"));
                return Self::back("/account", ctx.t("Password changed.", "Passwort geändert."))
                    .header("Set-Cookie", &self.session_cookie(ctx.req, &token));
            }
        }
        Response::redirect("/login")
    }

    fn totp_start(&self, ctx: &Ctx<'_>) -> Response {
        if ctx.user.totp.is_none() {
            lock(&self.pending_totp).insert(ctx.user.name.to_lowercase(), users::new_totp_secret());
        }
        Response::redirect("/account")
    }

    fn totp_confirm(&self, ctx: &Ctx<'_>) -> Response {
        let key = ctx.user.name.to_lowercase();
        let Some(secret) = lock(&self.pending_totp).get(&key).cloned() else {
            return Response::redirect("/account");
        };
        let code = ctx.req.form().get("code").cloned().unwrap_or_default();
        let Some(step) = users::check_totp(&secret, &code, users::current_step()) else {
            return self.account(
                ctx,
                Some(ctx.t(
                    "The code is not correct. Is the time of both devices right?",
                    "Der Code ist nicht korrekt. Stimmt die Uhrzeit beider Geräte?",
                )),
            );
        };
        lock(&self.limiter).use_code(&ctx.user.name, step);
        let mut store = UserStore::load(&self.paths.config_file);
        if let Some(user) = store.find_mut(&ctx.user.name) {
            user.totp = Some(secret);
        }
        if let Err(err) = store.save(&self.paths.config_file) {
            return self.account(ctx, Some(&err.to_string()));
        }
        lock(&self.pending_totp).remove(&key);
        audit::record(&self.paths.config_file, ctx.record("one-time-codes-on"));
        Self::back(
            "/account",
            ctx.t("One-time codes are on.", "Einmalcodes sind an."),
        )
    }

    fn totp_off(&self, ctx: &Ctx<'_>) -> Response {
        if self.settings.require_two_factor {
            return self.forbidden(ctx);
        }
        let current = ctx.req.form().get("current").cloned().unwrap_or_default();
        if !users::verify_password(&current, &ctx.user.password) {
            return self.account(
                ctx,
                Some(ctx.t(
                    "The password is not correct.",
                    "Das Passwort ist nicht korrekt.",
                )),
            );
        }
        let mut store = UserStore::load(&self.paths.config_file);
        if let Some(user) = store.find_mut(&ctx.user.name) {
            user.totp = None;
        }
        let _ = store.save(&self.paths.config_file);
        audit::record(&self.paths.config_file, ctx.record("one-time-codes-off"));
        Self::back(
            "/account",
            ctx.t("One-time codes are off.", "Einmalcodes sind aus."),
        )
    }
}
