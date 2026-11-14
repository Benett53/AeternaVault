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
use super::users::{
    self, EmailCode, Limiter, Permission, SecondFactor, Session, Sessions, User, UserSource,
    UserStore,
};
use super::{Control, Network};
use crate::audit::{self, Record};
use crate::config::{Config, WebSettings};
use crate::engine::verify::VerifyReport;
use crate::engine::{CancelToken, snapshots};
use crate::i18n::Lang;
use crate::paths::AppPaths;

const STYLE: &str = include_str!("style.css");
/// Changes with every version and with the style sheet itself.
const ASSET_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-", env!("AV_STYLE_HASH"));
const LOGO_DARK: &[u8] = include_bytes!("../../assets/logo/logo-dark-256.png");
const LOGO_LIGHT: &[u8] = include_bytes!("../../assets/logo/logo-light-256.png");
/// The logo of the window, light or dark like the page.
const LOGO_TAG: &str = r#"<picture><source srcset="/logo-light.png" media="(prefers-color-scheme: light)"><img src="/logo.png" alt=""></picture>"#;

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
    /// Networks administrators may work from (`admin_access = "networks"`).
    admin_networks: Vec<Network>,
    /// Reverse proxies whose forwarded headers are trusted.
    proxies: Vec<Network>,
    geo: Option<super::access::Geo>,
    blocked: Mutex<super::access::BlockCache>,
    /// Connections refused by country since the start.
    geo_refused: Mutex<std::collections::BTreeMap<String, u64>>,
    failures: Mutex<super::access::FailureLog>,
    control: Arc<dyn Control>,
    sessions: Mutex<Sessions>,
    limiter: Mutex<Limiter>,
    /// Secrets shown for setting up one-time codes, until confirmed.
    pending_totp: Mutex<HashMap<String, String>>,
    /// Codes sent by e-mail for a sign-in, by session.
    email_codes: Mutex<HashMap<String, EmailCode>>,
    /// Codes sent to confirm an e-mail address, by user (lower case).
    pending_email: Mutex<HashMap<String, EmailCode>>,
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
    /// What the user's role allows from where the request comes.
    perms: Vec<Permission>,
    /// Whether the user needs a second factor (by role, wherever they are).
    mfa: bool,
    config: Config,
    en: bool,
}

impl Ctx<'_> {
    fn can(&self, permission: Permission) -> bool {
        self.perms.contains(&permission)
    }

    fn t<'s>(&self, en: &'s str, de: &'s str) -> &'s str {
        if self.en { en } else { de }
    }

    fn actor(&self) -> String {
        format!("web:{}", self.user.name)
    }

    fn record(&self, action: &str) -> Record {
        Record::new(self.actor(), super::device::describe(self.req.peer), action)
    }

    fn lang(&self) -> Lang {
        if self.en { Lang::En } else { Lang::De }
    }
}

/// An address in `X-Forwarded-For`: `203.0.113.7`, `[2001:db8::1]:443`
/// or `203.0.113.7:51234`.
fn parse_forwarded_ip(text: &str) -> Option<IpAddr> {
    if let Ok(ip) = text.parse() {
        return Some(ip);
    }
    if let Ok(addr) = text.parse::<std::net::SocketAddr>() {
        return Some(addr.ip());
    }
    text.trim_start_matches('[')
        .split(']')
        .next()
        .and_then(|t| t.parse().ok())
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
    ) -> Result<Self, String> {
        let admin_networks = settings
            .admin_networks
            .iter()
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.parse())
            .collect::<Result<_, _>>()?;
        let proxies = settings
            .trusted_proxies
            .iter()
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.parse())
            .collect::<Result<_, _>>()?;
        let geo = super::access::Geo::open(&settings.geo)?;
        Ok(Self {
            paths,
            settings,
            networks,
            admin_networks,
            proxies,
            geo,
            blocked: Mutex::new(super::access::BlockCache::default()),
            geo_refused: Mutex::new(std::collections::BTreeMap::new()),
            failures: Mutex::new(super::access::FailureLog::default()),
            control,
            sessions: Mutex::new(Sessions::default()),
            limiter: Mutex::new(Limiter::default()),
            pending_totp: Mutex::new(HashMap::new()),
            email_codes: Mutex::new(HashMap::new()),
            pending_email: Mutex::new(HashMap::new()),
            checks: Arc::new(Mutex::new(Vec::new())),
            flash: Mutex::new(HashMap::new()),
        })
    }

    pub fn geo_refusals(&self) -> Vec<(String, u64)> {
        lock(&self.geo_refused)
            .iter()
            .map(|(c, n)| (c.clone(), *n))
            .collect()
    }

    /// Whether a user with these permissions needs a second factor.
    fn mfa_required(&self, perms: &[Permission]) -> bool {
        use super::access::MfaPolicy;
        match self.settings.mfa {
            MfaPolicy::Optional => false,
            MfaPolicy::Everyone => true,
            MfaPolicy::Administrators => perms.iter().any(|p| p.administrative()),
        }
    }

    /// Whether codes by e-mail can be used at all (allowed and a channel set up).
    fn email_possible(&self, config: &Config) -> bool {
        self.settings.mfa_email && super::email_channel(config).is_some()
    }

    /// The permissions of a user for a request from `ip`: administrative ones
    /// only where administrators may work from.
    fn effective(&self, mut perms: Vec<Permission>, ip: IpAddr) -> Vec<Permission> {
        if !super::access::admin_allowed(self.settings.admin_access, &self.admin_networks, ip) {
            perms.retain(|p| !p.administrative());
        }
        perms
    }

    /// Blocks an address after too many wrong sign-ins (never this computer).
    fn count_failure(&self, ip: IpAddr) {
        let limit = self.settings.auto_block_after;
        if limit == 0 || super::http::canonical(ip).is_loopback() {
            return;
        }
        let count = lock(&self.failures).failed(ip);
        if count < limit as usize {
            return;
        }
        let address = super::http::canonical(ip).to_string();
        let mut list = super::access::Blocklist::load(&self.paths.config_file);
        let reason = format!("{count} wrong sign-ins within 24 hours");
        if let Ok(true) = list.add(&address, &reason, "automatic") {
            match list.save(&self.paths.config_file) {
                Ok(()) => {
                    tracing::warn!("web interface: blocked {address}: {reason}");
                    audit::record(
                        &self.paths.config_file,
                        Record::local("address-blocked")
                            .target(address)
                            .details(vec![reason]),
                    );
                }
                Err(err) => tracing::warn!("web interface: block list not saved: {err}"),
            }
            lock(&self.blocked).invalidate();
            lock(&self.failures).forget(ip);
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

    /// Behind a trusted reverse proxy: the device it forwards for, whether
    /// the browser used HTTPS, and the host name it asked for. Headers from
    /// anyone else are ignored, so nobody can claim another address.
    fn forwarded(&self, mut req: Request) -> Request {
        let trusted = |ip: IpAddr| self.proxies.iter().any(|n| n.contains(ip));
        if !trusted(req.peer) {
            return req;
        }
        if let Some(chain) = req.header("x-forwarded-for") {
            // The rightmost address no trusted proxy added is the client.
            let client = chain
                .rsplit(',')
                .filter_map(|part| parse_forwarded_ip(part.trim()))
                .find(|ip| !trusted(*ip));
            if let Some(ip) = client {
                req.peer = ip;
            }
        }
        if req
            .header("x-forwarded-proto")
            .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
        {
            req.tls = true;
        }
        if let Some(host) = req.header("x-forwarded-host").map(|h| h.trim().to_string())
            && !host.is_empty()
        {
            req.headers.insert("host".into(), host);
        }
        req
    }

    pub fn handle(&self, req: Request) -> Response {
        let req = self.forwarded(req);
        if lock(&self.blocked).blocks(&self.paths.config_file, req.peer) {
            tracing::debug!("web interface: refused {} (blocked)", req.peer);
            return Response::text(403, "Forbidden");
        }
        if let Some(geo) = &self.geo
            && !geo.allows(req.peer)
        {
            let country = geo.country(req.peer).unwrap_or_else(|| "??".into());
            tracing::info!("web interface: refused {} (country {country})", req.peer);
            *lock(&self.geo_refused).entry(country).or_default() += 1;
            return Response::text(403, "Forbidden");
        }
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
        if req.method == "GET" && (req.path == "/logo.png" || req.path == "/logo-light.png") {
            let bytes = if req.path == "/logo.png" {
                LOGO_DARK
            } else {
                LOGO_LIGHT
            };
            return Response::with_type(200, "image/png", bytes.to_vec())
                .header("Cache-Control", "max-age=86400");
        }
        if req.method == "GET" && req.path == "/style.css" {
            // The address names the version of the program, so a browser
            // never combines a new page with a style sheet it kept from an
            // older version.
            return Response::with_type(200, "text/css; charset=utf-8", STYLE.as_bytes().to_vec())
                .header("Cache-Control", "no-cache");
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
            ("POST", "/login/code/resend") => return self.code_resend(&req, en),
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
        let role_perms = store.permissions(&user);
        let mfa = self.mfa_required(&role_perms);
        let perms = self.effective(role_perms, req.peer);
        let ctx = Ctx {
            req: &req,
            token,
            session,
            user,
            perms,
            mfa,
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
        let email_possible = self.email_possible(&ctx.config);
        let needs_code = ctx.mfa
            && ctx
                .user
                .second_factor(true, self.settings.mfa_app, email_possible)
                .is_none();
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
            ("POST", "/account/email/start") => self.email_start(&ctx),
            ("POST", "/account/email/confirm") => self.email_confirm(&ctx),
            ("POST", "/account/email/use") => self.email_use(&ctx),
            ("POST", "/account/email/off") => self.email_off(&ctx),
            ("POST", "/jobs/run") if ctx.can(Permission::RunJobs) => self.job_run(&ctx),
            ("POST", "/jobs/cancel") if ctx.can(Permission::RunJobs) => self.job_cancel(&ctx),
            ("POST", "/jobs/switch") if ctx.can(Permission::SwitchJobs) => self.job_switch(&ctx),
            ("POST", "/backups/check") if ctx.can(Permission::CheckBackups) => {
                self.check_start(&ctx)
            }
            ("POST", "/users/add") if ctx.can(Permission::ManageUsers) => self.user_add(&ctx),
            ("POST", "/users/change") if ctx.can(Permission::ManageUsers) => self.user_change(&ctx),
            ("POST", "/roles/save") if ctx.can(Permission::ManageUsers) => self.role_save(&ctx),
            ("GET", "/access") if ctx.can(Permission::BlockAddresses) => self.access_page(&ctx),
            ("POST", "/access/block") if ctx.can(Permission::BlockAddresses) => {
                self.access_block(&ctx)
            }
            ("POST", "/access/unblock") if ctx.can(Permission::BlockAddresses) => {
                self.access_unblock(&ctx)
            }
            ("GET", "/audit") if ctx.can(Permission::ViewAudit) => self.audit_page(&ctx),
            ("GET", "/users") if ctx.can(Permission::ManageUsers) => self.users_page(&ctx),
            ("GET", "/audit" | "/users" | "/access") | ("POST", _) => self.forbidden(&ctx),
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
        if ctx.can(Permission::BlockAddresses) {
            nav.push(("/access", ctx.t("Access", "Zugriff")));
        }
        if ctx.can(Permission::ManageUsers) {
            nav.push(("/users", ctx.t("Users", "Benutzer")));
        }
        if ctx.can(Permission::ViewAudit) {
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
            r#"<!DOCTYPE html><html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">{refresh}<title>{title} · AeternaVault</title><link rel="stylesheet" href="/style.css?v={asset_version}"><link rel="icon" href="/logo.png"></head><body><header><div class="bar"><a class="brand" href="/">{logo}<span>AeternaVault</span></a><span class="computer">{computer}</span><div class="account">{user}<form method="post" action="/logout" class="inline">{csrf}<button class="link">{logout}</button></form></div><nav class="main">{links}</nav></div></header><main><h1>{title}</h1>{body}</main></body></html>"#,
            lang = if ctx.en { "en" } else { "de" },
            title = esc(title),
            logo = LOGO_TAG,
            asset_version = ASSET_VERSION,
            computer = esc(&crate::platform::computer_name()),
            csrf = csrf_field(&ctx.session),
            logout = ctx.t("Sign out", "Abmelden"),
            user = esc(&ctx.user.name),
        ))
    }

    /// Pages before sign-in. They name nothing about the computer or the
    /// system, so nobody learns anything without an account.
    fn bare(en: bool, title: &str, body: &str) -> Response {
        Response::html(format!(
            r#"<!DOCTYPE html><html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="robots" content="noindex"><title>{title}</title><link rel="stylesheet" href="/style.css?v={asset_version}"><link rel="icon" href="/logo.png"></head><body class="bare"><main class="login"><div class="brand">{logo}<span>AeternaVault</span></div>{body}</main></body></html>"#,
            lang = if en { "en" } else { "de" },
            title = esc(title),
            logo = LOGO_TAG,
            asset_version = ASSET_VERSION,
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
        // Address, name and MAC of the device, for the audit log.
        let from = super::device::describe(req.peer);
        let user_key = format!("user:{}", name.to_lowercase());
        let wrong = t(
            "User name or password is not correct.",
            "Benutzername oder Passwort ist nicht korrekt.",
        );
        let failed = |reason: &str| {
            audit::record(
                &self.paths.config_file,
                Record::new(format!("web:{name}"), from.clone(), "sign-in")
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
        let local = store
            .find(&name)
            .filter(|u| u.source == UserSource::Local)
            .cloned();
        let ldap = &self.settings.ldap;
        // Who signs in, or why not (`None`: wrong name or password).
        let checked: Result<User, &str> = match local {
            Some(user) => {
                if users::verify_password(&password, &user.password) {
                    Ok(user)
                } else {
                    Err("wrong password")
                }
            }
            None if ldap.enabled && users::valid_name(&name).is_ok() => {
                let rank = |role: &str| store.role_permissions(role).map_or(0, |p| p.len());
                match super::ldap::authenticate(ldap, &name, &password, &rank) {
                    Ok(found) => Ok(self.directory_user(&mut store, &name, &found)),
                    Err(super::ldap::LdapError::Rejected) => {
                        Err("wrong password or unknown user (directory)")
                    }
                    Err(super::ldap::LdapError::NoGroup) => {
                        failed("directory user in none of the groups that give a role");
                        return self.login_page(
                            req,
                            en,
                            Some(t(
                                "Your account is not in any group that may use this web interface.",
                                "Dein Konto ist in keiner Gruppe, die diese Web-Oberfläche nutzen darf.",
                            )),
                        );
                    }
                    Err(super::ldap::LdapError::Unavailable(err)) => {
                        // Not counted as a wrong attempt: a server that is
                        // down must not lock everybody out.
                        tracing::warn!("web interface: directory not reachable: {err}");
                        failed(&format!("directory not reachable: {err}"));
                        return self.login_page(
                            req,
                            en,
                            Some(t(
                                "The directory (LDAP) cannot be reached at the moment. Details are in the log of the server.",
                                "Das Verzeichnis (LDAP) ist gerade nicht erreichbar. Details stehen im Protokoll des Servers.",
                            )),
                        );
                    }
                }
            }
            None => {
                // The hash is computed even for unknown names, so the answer
                // takes as long and does not tell which names exist.
                let _ = users::verify_password(
                    &password,
                    "argon2id$19456$2$00000000000000000000000000000000$0000000000000000000000000000000000000000000000000000000000000000",
                );
                Err("unknown user")
            }
        };
        let user = match checked {
            Ok(user) => user,
            Err(reason) => {
                let mut limiter = lock(&self.limiter);
                limiter.failed(&ip);
                limiter.failed(&user_key);
                drop(limiter);
                failed(reason);
                self.count_failure(req.peer);
                return self.login_page(req, en, Some(wrong));
            }
        };
        if user.source == UserSource::Directory {
            let _ = store.save(&self.paths.config_file);
        }
        if user.disabled {
            failed("user is disabled");
            return self.login_page(req, en, Some(wrong));
        }
        if store.role_permissions(&user.role).is_none() {
            failed("no role given yet");
            return self.login_page(
                req,
                en,
                Some(t(
                    "Your account has no role yet. An administrator has to give you one.",
                    "Dein Konto hat noch keine Rolle. Ein Administrator muss dir eine zuweisen.",
                )),
            );
        }
        {
            let mut limiter = lock(&self.limiter);
            limiter.succeeded(&user_key);
            limiter.succeeded(&ip);
        }
        let config = self.config();
        let email_possible = self.email_possible(&config);
        let mfa = self.mfa_required(&store.permissions(&user));
        let factor = user.second_factor(mfa, self.settings.mfa_app, email_possible);
        let pending = factor.is_some();
        let (token, _) = lock(&self.sessions).create(&user.name, req.peer, pending);
        if factor == Some(SecondFactor::Email)
            && let Err(err) = self.send_sign_in_code(&config, &token, &user, &from)
        {
            lock(&self.sessions).end(&token);
            tracing::warn!(
                "web interface: sign-in code not sent to {}: {err}",
                user.name
            );
            return self.login_page(
                req,
                en,
                Some(t(
                    "The sign-in code could not be sent by e-mail. Details are in the log of the server.",
                    "Der Anmeldecode konnte nicht per E-Mail gesendet werden. Details stehen im Protokoll des Servers.",
                )),
            );
        }
        if !pending {
            if let Some(u) = store.find_mut(&user.name) {
                u.last_sign_in = Some(Utc::now());
            }
            let _ = store.save(&self.paths.config_file);
            audit::record(
                &self.paths.config_file,
                Record::new(format!("web:{}", user.name), from, "sign-in"),
            );
        }
        Response::redirect(if pending { "/login/code" } else { "/" })
            .header("Set-Cookie", &self.session_cookie(req, &token))
    }

    /// The local record of a directory user, made at the first sign-in. The
    /// role comes from the directory groups, or is kept here (settings).
    fn directory_user(
        &self,
        store: &mut UserStore,
        name: &str,
        found: &super::ldap::DirectoryUser,
    ) -> User {
        let name = name.trim();
        match store.find_mut(name) {
            Some(user) => {
                if let Some(role) = &found.role
                    && &user.role != role
                {
                    audit::record(
                        &self.paths.config_file,
                        Record::new("system", "directory", "user-changed")
                            .target(user.name.clone())
                            .details(vec![format!(
                                "role from directory groups: {} → {role}",
                                user.role
                            )]),
                    );
                    user.role = role.clone();
                }
                if let Some(email) = &found.email {
                    user.email = email.clone();
                }
                user.clone()
            }
            None => {
                let role = found
                    .role
                    .clone()
                    .unwrap_or_else(|| self.settings.ldap.default_role.trim().to_string());
                let user = User {
                    name: name.to_string(),
                    role: role.clone(),
                    source: UserSource::Directory,
                    password: String::new(),
                    totp: None,
                    email: found.email.clone().unwrap_or_default(),
                    email_codes: false,
                    jobs: Vec::new(),
                    disabled: false,
                    must_change_password: false,
                    created: Utc::now(),
                    last_sign_in: None,
                };
                store.users.push(user.clone());
                audit::record(
                    &self.paths.config_file,
                    Record::new("system", "directory", "user-added")
                        .target(name.to_string())
                        .details(vec![
                            format!("from the directory: {}", found.dn),
                            format!("role: {}", if role.is_empty() { "none yet" } else { &role }),
                        ]),
                );
                user
            }
        }
    }

    /// Sends a new sign-in code to the user's address and keeps it for the session.
    fn send_sign_in_code(
        &self,
        config: &Config,
        token: &str,
        user: &User,
        ip: &str,
    ) -> Result<(), String> {
        let code = EmailCode::new(&user.email);
        let result = super::send_code(config, &user.email, &code.code, false);
        let record = Record::new(
            format!("web:{}", user.name),
            ip.to_string(),
            "sign-in-code-sent",
        )
        .details(vec![format!("by e-mail to {}", users::masked(&user.email))]);
        match &result {
            Ok(()) => {
                audit::record(&self.paths.config_file, record);
                let mut codes = lock(&self.email_codes);
                codes.retain(|_, c| c.sent.elapsed() < users::EMAIL_CODE_TTL);
                codes.insert(token.to_string(), code);
            }
            Err(err) => audit::record(
                &self.paths.config_file,
                record.details(vec![err.clone()]).failed(),
            ),
        }
        result
    }

    fn pending_session(&self, req: &Request) -> Option<(String, Session)> {
        let token = req.cookie(self.cookie_name(req))?;
        let session = lock(&self.sessions).touch(&token, Duration::from_secs(300))?;
        session.pending_code.then_some((token, session))
    }

    fn code_page(&self, req: &Request, en: bool, error: Option<&str>) -> Response {
        let t = |a: &'static str, b: &'static str| if en { a } else { b };
        let Some((token, _)) = self.pending_session(req) else {
            return Response::redirect("/login");
        };
        let by_mail = lock(&self.email_codes)
            .get(&token)
            .map(|c| users::masked(&c.address));
        let error = error
            .map(|e| format!(r#"<p class="error">{}</p>"#, esc(e)))
            .unwrap_or_default();
        let resend = if by_mail.is_some() {
            format!(
                r#"<form method="post" action="/login/code/resend" class="stack"><button class="secondary">{}</button></form><p class="muted">{}</p>"#,
                t("Send a new code", "Neuen Code senden"),
                t(
                    "The code is valid for 10 minutes. Also look in the spam folder.",
                    "Der Code gilt 10 Minuten. Schau auch im Spam-Ordner nach."
                )
            )
        } else {
            String::new()
        };
        Self::bare(
            en,
            t("One-time code", "Einmalcode"),
            &format!(
                r#"{error}<form method="post" action="/login/code" class="stack"><label>{label}<input name="code" inputmode="numeric" autocomplete="one-time-code" pattern="[0-9 ]*" required autofocus></label><button>{go}</button></form>{resend}"#,
                label = match &by_mail {
                    Some(address) if en => format!("Code from the e-mail to {address}"),
                    Some(address) => format!("Code aus der E-Mail an {address}"),
                    None => t(
                        "Code from your authenticator app",
                        "Code aus deiner Authenticator-App"
                    )
                    .into(),
                },
                go = t("Continue", "Weiter"),
            ),
        )
    }

    fn code_resend(&self, req: &Request, en: bool) -> Response {
        let t = |a: &'static str, b: &'static str| if en { a } else { b };
        let Some((token, session)) = self.pending_session(req) else {
            return Response::redirect("/login");
        };
        let recent = lock(&self.email_codes)
            .get(&token)
            .is_some_and(|c| c.sent.elapsed() < Duration::from_secs(60));
        if recent {
            return self.code_page(
                req,
                en,
                Some(t(
                    "A code was sent less than a minute ago.",
                    "Vor weniger als einer Minute wurde schon ein Code gesendet.",
                )),
            );
        }
        let store = UserStore::load(&self.paths.config_file);
        let Some(user) = store.find(&session.user).cloned() else {
            return Response::redirect("/login");
        };
        if !lock(&self.email_codes).contains_key(&token) {
            return Response::redirect("/login/code");
        }
        match self.send_sign_in_code(
            &self.config(),
            &token,
            &user,
            &super::device::describe(req.peer),
        ) {
            Ok(()) => self.code_page(req, en, None),
            Err(_) => self.code_page(
                req,
                en,
                Some(t(
                    "The code could not be sent.",
                    "Der Code konnte nicht gesendet werden.",
                )),
            ),
        }
    }

    fn code(&self, req: &Request, en: bool) -> Response {
        let t = |a: &'static str, b: &'static str| if en { a } else { b };
        let Some((token, session)) = self.pending_session(req) else {
            return Response::redirect("/login");
        };
        let mut store = UserStore::load(&self.paths.config_file);
        let user_key = format!("code:{}", session.user.to_lowercase());
        let ip = super::device::describe(req.peer);
        let code = req.form().get("code").cloned().unwrap_or_default();
        let by_mail = lock(&self.email_codes)
            .get_mut(&token)
            .map(|c| c.check(&code));
        let secret = store.find(&session.user).and_then(|u| u.totp.clone());
        if by_mail.is_none() && secret.is_none() {
            return Response::redirect("/login");
        }
        let mut limiter = lock(&self.limiter);
        let code_ok = match by_mail {
            Some(ok) => ok,
            None => users::check_totp(
                secret.as_deref().unwrap_or_default(),
                &code,
                users::current_step(),
            )
            .is_some_and(|s| limiter.use_code(&session.user, s)),
        };
        let ok = code_ok
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
        lock(&self.email_codes).remove(&token);
        lock(&self.sessions).finish_code(&token);
        if let Some(u) = store.find_mut(&session.user) {
            u.last_sign_in = Some(Utc::now());
        }
        let _ = store.save(&self.paths.config_file);
        audit::record(
            &self.paths.config_file,
            Record::new(format!("web:{}", session.user), ip, "sign-in").details(vec![
                if by_mail.is_some() {
                    "with code by e-mail"
                } else {
                    "with one-time code"
                }
                .into(),
            ]),
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
        let operator = ctx.can(Permission::RunJobs);
        let switcher = ctx.can(Permission::SwitchJobs);
        let csrf = csrf_field(&ctx.session);
        let mut body = Self::message(ctx);

        // Running now.
        if let Some((label, progress)) = self.control.running() {
            let percent = (progress.bytes_done * 100)
                .checked_div(progress.bytes_total)
                .unwrap_or(0);
            let cancel = if operator {
                format!(
                    r#"<form method="post" action="/jobs/cancel" class="inline">{csrf}<button class="secondary">{}</button></form>"#,
                    ctx.t("Cancel", "Abbrechen")
                )
            } else {
                String::new()
            };
            body.push_str(&format!(
                r#"<section class="card"><h2>{now}</h2><h3>{label}</h3><p class="muted">{files} · {bytes} · {percent} %</p><progress max="100" value="{percent}"></progress><div class="row">{cancel}</div></section>"#,
                now = ctx.t("Running now", "Läuft gerade"),
                label = esc(&label),
                files = esc(&format!("{} / {}", lang.count(progress.files_done), lang.count(progress.files_total))),
                bytes = esc(&lang.bytes_of(progress.bytes_done, progress.bytes_total)),
            ));
        }

        // Overdue.
        for (schedule, last) in crate::notify::overdue_jobs(config, &state, Utc::now())
            .iter()
            .filter(|(s, _)| ctx.user.may_use_job(Some(&s.id)))
        {
            let since = last
                .map(|l| Self::when(ctx, l))
                .unwrap_or_else(|| ctx.t("never", "noch nie").into());
            body.push_str(&format!(
                r#"<p class="error"><strong>{}:</strong> {} · {} {}</p>"#,
                ctx.t("Overdue", "Überfällig"),
                esc(&crate::automatic::describe(schedule, config, lang)),
                ctx.t("last successful backup", "letzte erfolgreiche Sicherung"),
                since
            ));
        }

        // Backups of the last 30 days, from the history (only the user's jobs).
        let jobs: Vec<&crate::config::Schedule> = config
            .schedules
            .iter()
            .filter(|s| ctx.user.may_use_job(Some(&s.id)))
            .collect();
        let labels: Option<std::collections::HashSet<String>> =
            (!ctx.user.jobs.is_empty()).then(|| {
                jobs.iter()
                    .map(|s| crate::automatic::job_label(Some(s), config, lang))
                    .collect()
            });
        const DAYS: usize = 30;
        let today = Local::now().date_naive();
        let mut per_day = [(0u32, 0u32, 0u32); DAYS];
        let mut last_backup: Option<(DateTime<Utc>, crate::history::Outcome)> = None;
        for entry in crate::history::load(&self.paths.config_file) {
            let crate::history::Event::Backup { job, outcome, .. } = &entry.event else {
                continue;
            };
            if labels.as_ref().is_some_and(|l| !l.contains(job)) {
                continue;
            }
            use crate::history::Outcome as O;
            if matches!(
                outcome,
                O::Complete | O::Notes | O::Failed | O::NeedsPassphrase
            ) {
                last_backup = Some((entry.at, *outcome));
            }
            let age = (today - entry.at.with_timezone(&Local).date_naive()).num_days();
            if !(0..DAYS as i64).contains(&age) {
                continue;
            }
            let day = &mut per_day[DAYS - 1 - age as usize];
            match outcome {
                O::Complete => day.0 += 1,
                O::Notes => day.1 += 1,
                O::Failed | O::NeedsPassphrase => day.2 += 1,
                O::Cancelled | O::Skipped => {}
            }
        }
        let (good, notes, bad) = per_day
            .iter()
            .fold((0, 0, 0), |a, d| (a.0 + d.0, a.1 + d.1, a.2 + d.2));
        let total = good + notes + bad;

        // Tiles.
        let problem_jobs = jobs
            .iter()
            .filter(|s| {
                state
                    .schedules
                    .get(&s.id)
                    .and_then(|e| e.last_run.as_ref())
                    .is_some_and(|r| r.outcome != crate::state::AutomaticOutcome::Complete)
            })
            .count();
        let destinations = self.destinations(ctx);
        let lowest_free = destinations
            .iter()
            .filter_map(|c| crate::platform::disk_space(&c.destination))
            .min_by_key(|(free, _)| *free);
        let tile = |class: &str, value: String, caption: String| {
            format!(
                r#"<div class="tile {class}"><span class="value">{}</span><span class="caption">{}</span></div>"#,
                esc(&value),
                esc(&caption)
            )
        };
        let mut tiles = String::new();
        tiles.push_str(&tile(
            if problem_jobs > 0 { "warn" } else { "accent" },
            jobs.len().to_string(),
            if problem_jobs > 0 {
                format!(
                    "{} · {problem_jobs} {}",
                    ctx.t("jobs", "Aufträge"),
                    ctx.t("with problems", "mit Problemen")
                )
            } else {
                ctx.t("jobs, all fine", "Aufträge, alle in Ordnung").into()
            },
        ));
        tiles.push_str(&match last_backup {
            Some((at, outcome)) => tile(
                match outcome {
                    crate::history::Outcome::Complete => "good",
                    crate::history::Outcome::Notes => "warn",
                    _ => "bad",
                },
                lang.relative_time(at.with_timezone(&Local)),
                ctx.t("last backup", "letzte Sicherung").into(),
            ),
            None => tile(
                "",
                "–".into(),
                ctx.t("no backup yet", "noch keine Sicherung").into(),
            ),
        });
        tiles.push_str(&tile(
            if total == 0 {
                ""
            } else if bad > 0 {
                "warn"
            } else {
                "good"
            },
            (good * 100)
                .checked_div(total)
                .map_or_else(|| "–".to_string(), |p| format!("{p} %")),
            ctx.t("complete in 30 days", "vollständig in 30 Tagen")
                .into(),
        ));
        tiles.push_str(&tile(
            if bad > 0 { "bad" } else { "" },
            bad.to_string(),
            ctx.t("failed in 30 days", "fehlgeschlagen in 30 Tagen")
                .into(),
        ));
        if let Some((free, size)) = lowest_free {
            let percent = (free * 100).checked_div(size).unwrap_or(0);
            tiles.push_str(&tile(
                if percent < 10 {
                    "bad"
                } else if percent < 20 {
                    "warn"
                } else {
                    ""
                },
                lang.bytes(free),
                format!(
                    "{} ({percent} %)",
                    ctx.t("free at the destination", "frei am Ziel")
                ),
            ));
        }
        if config.drive_health_on() && !config.destination.as_os_str().is_empty() {
            use crate::platform::drive_health::{self, Status};
            match drive_health::cached_wait(&config.destination, Duration::from_secs(3)) {
                Some(Ok(h)) => tiles.push_str(&tile(
                    match h.status {
                        Status::Good => "good",
                        Status::Warning => "warn",
                        Status::Failing => "bad",
                    },
                    h.status_name(ctx.en).into(),
                    format!(
                        "{} · {}",
                        ctx.t("drive health", "Laufwerkszustand"),
                        h.summary(ctx.en)
                            .split(" · ")
                            .skip(1)
                            .collect::<Vec<_>>()
                            .join(" · ")
                    ),
                )),
                // The reason is on the destination card below.
                Some(Err(_)) => tiles.push_str(&tile(
                    "",
                    "–".into(),
                    ctx.t(
                        "drive health not reported",
                        "Laufwerkszustand nicht gemeldet",
                    )
                    .into(),
                )),
                None => {}
            }
        }
        body.push_str(&format!(r#"<div class="tiles">{tiles}</div>"#));

        // Chart and ring.
        let chart = Self::bar_chart(&per_day, today, ctx);
        let ring = Self::ring(good, notes, bad, ctx);
        body.push_str(&format!(
            r#"<div class="split"><section class="card"><h2>{t1}</h2>{chart}<div class="legend"><span><span class="dot good"></span>{g}</span><span><span class="dot warn"></span>{n}</span><span><span class="dot bad"></span>{b}</span></div></section><section class="card"><h2>{t2}</h2><div class="summary">{ring}<div><p><span class="dot good"></span>{good} {g}</p><p><span class="dot warn"></span>{notes} {n}</p><p><span class="dot bad"></span>{bad} {b}</p></div></div></section></div>"#,
            t1 = ctx.t("Backups, last 30 days", "Sicherungen, letzte 30 Tage"),
            t2 = ctx.t("Result", "Ergebnis"),
            g = ctx.t("complete", "vollständig"),
            n = ctx.t("with notes", "mit Hinweisen"),
            b = ctx.t("failed", "fehlgeschlagen"),
        ));

        // Jobs as cards.
        let now = Local::now();
        let mut cards = String::new();
        for schedule in &jobs {
            let last = state
                .schedules
                .get(&schedule.id)
                .and_then(|e| e.last_run.as_ref());
            let badge = match last {
                Some(run) => format!(
                    r#"<span class="badge {}">{}</span>"#,
                    Self::outcome_class(run.outcome),
                    esc(lang.notify_status(run.outcome))
                ),
                None => format!(
                    r#"<span class="badge muted">{}</span>"#,
                    ctx.t("not run yet", "noch nicht gelaufen")
                ),
            };
            let last_text = last
                .map(|run| {
                    format!(
                        "{} · {}",
                        Self::when(ctx, run.at),
                        esc(&lang.files(run.files))
                    )
                })
                .unwrap_or_else(|| "–".into());
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
                    ctx.t("switched off", "ausgeschaltet")
                )
            };
            let mut buttons = String::new();
            if operator {
                buttons.push_str(&format!(
                    r#"<form method="post" action="/jobs/run" class="inline">{csrf}<input type="hidden" name="id" value="{id}"><button>{run}</button></form>"#,
                    id = esc(&schedule.id),
                    run = ctx.t("Run now", "Jetzt ausführen"),
                ));
            }
            if switcher {
                buttons.push_str(&format!(
                    r#"<form method="post" action="/jobs/switch" class="inline">{csrf}<input type="hidden" name="id" value="{id}"><input type="hidden" name="on" value="{on}"><button class="secondary">{switch}</button></form>"#,
                    id = esc(&schedule.id),
                    on = if schedule.enabled { "0" } else { "1" },
                    switch = if schedule.enabled { ctx.t("Switch off", "Ausschalten") } else { ctx.t("Switch on", "Einschalten") },
                ));
            }
            let actions = if buttons.is_empty() {
                String::new()
            } else {
                format!(r#"<div class="actions">{buttons}</div>"#)
            };
            cards.push_str(&format!(
                r#"<section class="card job"><div class="head"><h3>{name}</h3>{badge}</div><dl class="meta"><dt>{l1}</dt><dd>{last_text}</dd><dt>{l2}</dt><dd>{next}</dd><dt>{l3}</dt><dd>{dest}</dd></dl>{problems}{actions}</section>"#,
                name = esc(&crate::automatic::describe(schedule, config, lang)),
                l1 = ctx.t("Last run", "Letzter Lauf"),
                l2 = ctx.t("Next run", "Nächster Lauf"),
                l3 = ctx.t("Destination", "Ziel"),
                dest = esc(&config.for_schedule(schedule).destination.display().to_string()),
            ));
        }
        let all_now = if operator && ctx.user.jobs.is_empty() {
            format!(
                r#"<form method="post" action="/jobs/run" class="inline">{csrf}<button class="secondary">{}</button></form>"#,
                ctx.t(
                    "Back up all ticked folders now",
                    "Alle ausgewählten Ordner jetzt sichern"
                )
            )
        } else {
            String::new()
        };
        body.push_str(&format!(r#"<h2>{}</h2>"#, ctx.t("Jobs", "Aufträge")));
        if cards.is_empty() {
            body.push_str(&format!(
                r#"<p class="muted">{}</p>"#,
                ctx.t("No jobs.", "Keine Aufträge.")
            ));
        } else {
            body.push_str(&format!(r#"<div class="grid">{cards}</div>"#));
        }
        if !all_now.is_empty() {
            body.push_str(&format!(r#"<p>{all_now}</p>"#));
        }

        // Destinations as cards.
        let mut dest_cards = String::new();
        for (n, c) in destinations.iter().enumerate() {
            let d = &c.destination;
            let reachable = snapshots::destination_reachable(d);
            let space = crate::platform::disk_space(d);
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
            let meter = space
                .map(|(free, size)| {
                    let used = (size - free.min(size)) * 100 / size.max(1);
                    format!(
                        r#"<meter min="0" max="100" low="75" high="90" optimum="10" value="{used}"></meter><p class="muted small">{} {} · {used} % {}</p>"#,
                        esc(&lang.bytes(free)),
                        ctx.t("free", "frei"),
                        ctx.t("used", "belegt"),
                    )
                })
                .unwrap_or_default();
            let size_note = space
                .map(|(_, size)| esc(&lang.bytes(size)))
                .unwrap_or_default();
            let health = if config.drive_health_on() {
                use crate::platform::drive_health::{self, Status};
                let text = match drive_health::cached_wait(d, Duration::from_secs(3)) {
                    None => ctx.t("reading…", "wird gelesen…").to_string(),
                    Some(Ok(h)) => {
                        let class = match h.status {
                            Status::Good => "good",
                            Status::Warning => "warn",
                            Status::Failing => "bad",
                        };
                        format!(
                            r#"<span class="badge {class}" title="{title}">{summary}</span>"#,
                            title = esc(&h.findings.join("; ")),
                            summary = esc(&h.summary(ctx.en)),
                        )
                    }
                    Some(Err(why)) => esc(&why.text(ctx.en)),
                };
                format!(
                    "<dt>{}</dt><dd>{text}</dd>",
                    ctx.t("Drive health", "Laufwerkszustand")
                )
            } else {
                String::new()
            };
            dest_cards.push_str(&format!(
                r#"<section class="card job"><div class="head"><h3><a href="/backups?d={n}">{path}</a></h3>{state}</div>{meter}<dl class="meta"><dt>{l1}</dt><dd>{size_note}</dd><dt>{l2}</dt><dd>{count}</dd><dt>{l3}</dt><dd>{newest}</dd>{health}</dl></section>"#,
                path = esc(&d.display().to_string()),
                state = if reachable {
                    format!(r#"<span class="badge good">{}</span>"#, ctx.t("reachable", "erreichbar"))
                } else {
                    format!(r#"<span class="badge bad">{}</span>"#, ctx.t("not reachable", "nicht erreichbar"))
                },
                l1 = ctx.t("Size", "Größe"),
                l2 = ctx.t("Backups", "Sicherungen"),
                l3 = ctx.t("Newest", "Neueste"),
                count = list.len(),
            ));
        }
        if !dest_cards.is_empty() {
            body.push_str(&format!(
                r#"<h2>{}</h2><div class="grid">{dest_cards}</div>"#,
                ctx.t("Destinations", "Ziele")
            ));
        }

        // Recent activity.
        body.push_str(&format!(
            r#"<section class="card"><h2>{}</h2><div class="scroll">{}</div><p><a href="/activity">{} →</a></p></section>"#,
            ctx.t("Recent activity", "Letzte Aktivität"),
            self.activity_table(ctx, 8),
            ctx.t("All activity", "Gesamter Verlauf")
        ));
        self.page(ctx, ctx.t("Overview", "Übersicht"), &body, true)
    }

    /// Stacked bars per day: complete, with notes, failed.
    fn bar_chart(days: &[(u32, u32, u32)], today: chrono::NaiveDate, ctx: &Ctx<'_>) -> String {
        let (width, height, left, bottom, top) = (900.0, 230.0, 34.0, 26.0, 10.0);
        let plot_w = width - left - 6.0;
        let plot_h = height - bottom - top;
        let max = days
            .iter()
            .map(|d| d.0 + d.1 + d.2)
            .max()
            .unwrap_or(0)
            .max(1);
        let slot = plot_w / days.len() as f64;
        let bar = (slot * 0.62).max(2.0);
        let mut svg = format!(
            r#"<svg class="chart" viewBox="0 0 {width} {height}" role="img" aria-label="{}">"#,
            ctx.t("Backups per day", "Sicherungen pro Tag")
        );
        for step in [0.0, 0.5, 1.0] {
            let y = top + plot_h * (1.0 - step);
            let value = (f64::from(max) * step).round();
            svg.push_str(&format!(
                r#"<line class="axis" x1="{left}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}"/><text x="{:.1}" y="{:.1}" text-anchor="end">{value}</text>"#,
                width - 6.0,
                left - 6.0,
                y + 4.0
            ));
        }
        for (i, (good, notes, bad)) in days.iter().enumerate() {
            let x = left + slot * i as f64 + (slot - bar) / 2.0;
            let mut y = top + plot_h;
            for (count, class) in [(good, "good"), (notes, "warn"), (bad, "bad")] {
                if *count == 0 {
                    continue;
                }
                let h = plot_h * f64::from(*count) / f64::from(max);
                y -= h;
                svg.push_str(&format!(
                    r#"<rect class="{class}" x="{x:.1}" y="{y:.1}" width="{bar:.1}" height="{h:.1}" rx="2"/>"#
                ));
            }
            let age = days.len() - 1 - i;
            if age.is_multiple_of(5) {
                let date = today - chrono::Duration::days(age as i64);
                svg.push_str(&format!(
                    r#"<text x="{:.1}" y="{:.1}" text-anchor="middle">{}</text>"#,
                    x + bar / 2.0,
                    height - 8.0,
                    date.format("%d.%m.")
                ));
            }
        }
        svg.push_str("</svg>");
        svg
    }

    /// A ring of the shares, with the share of complete backups in the middle.
    fn ring(good: u32, notes: u32, bad: u32, ctx: &Ctx<'_>) -> String {
        let total = good + notes + bad;
        let r = 60.0_f64;
        let circumference = 2.0 * std::f64::consts::PI * r;
        let mut svg = format!(
            r#"<svg class="ring" viewBox="0 0 150 150" role="img" aria-label="{}"><circle class="track" cx="75" cy="75" r="{r}"/><g transform="rotate(-90 75 75)">"#,
            ctx.t("Share of results", "Anteile der Ergebnisse")
        );
        let mut offset = 0.0;
        for (count, class) in [(good, "good"), (notes, "warn"), (bad, "bad")] {
            if count == 0 || total == 0 {
                continue;
            }
            let length = circumference * f64::from(count) / f64::from(total);
            svg.push_str(&format!(
                r#"<circle class="{class}" cx="75" cy="75" r="{r}" stroke-dasharray="{length:.2} {:.2}" stroke-dashoffset="{:.2}"/>"#,
                circumference - length,
                -offset
            ));
            offset += length;
        }
        let share = (good * 100)
            .checked_div(total)
            .map_or_else(|| "–".to_string(), |p| format!("{p} %"));
        svg.push_str(&format!(
            r#"</g><text x="75" y="80" text-anchor="middle">{share}</text><text class="sub" x="75" y="100" text-anchor="middle">{}</text></svg>"#,
            ctx.t("complete", "vollständig")
        ));
        svg
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

    pub fn active_sessions(&self) -> Vec<(String, IpAddr, Duration, Duration)> {
        lock(&self.sessions).active()
    }

    fn activity(&self, ctx: &Ctx<'_>) -> Response {
        let sign_ins = ctx.req.query.get("tab").is_some_and(|t| t == "sign-ins");
        let mut body = String::new();
        if ctx.can(Permission::ViewSignIns) {
            let current = |on: bool| if on { r#" aria-current="page""# } else { "" };
            body.push_str(&format!(
                r#"<nav class="tabs"><a href="/activity"{}>{}</a><a href="/activity?tab=sign-ins"{}>{}</a></nav>"#,
                current(!sign_ins),
                ctx.t("Backups and checks", "Sicherungen und Prüfungen"),
                current(sign_ins),
                ctx.t("Sign-ins (live)", "Anmeldungen (live)"),
            ));
        }
        if sign_ins && ctx.can(Permission::ViewSignIns) {
            body.push_str(&Self::message(ctx));
            body.push_str(&self.sign_ins(ctx));
            return self.page(ctx, ctx.t("Activity", "Verlauf"), &body, true);
        }
        body.push_str(&self.activity_table(ctx, 300));
        self.page(ctx, ctx.t("Activity", "Verlauf"), &body, false)
    }

    /// Who is signed in now, and every sign-in attempt, newest first.
    fn sign_ins(&self, ctx: &Ctx<'_>) -> String {
        let lang = ctx.lang();
        let mut html = String::new();
        let active = self.active_sessions();
        let rows: String = active
            .iter()
            .map(|(user, ip, since, idle)| {
                format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    esc(user),
                    esc(&super::device::describe(*ip)),
                    esc(&lang.duration(*since)),
                    esc(&lang.duration(*idle)),
                )
            })
            .collect();
        html.push_str(&format!(
            r#"<section class="card"><h2>{} ({})</h2>{}</section>"#,
            ctx.t("Signed in now", "Jetzt angemeldet"),
            active.len(),
            if rows.is_empty() {
                String::new()
            } else {
                format!(
                    "<table><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>{rows}</tbody></table>",
                    ctx.t("User", "Benutzer"),
                    ctx.t("Address", "Adresse"),
                    ctx.t("Signed in for", "Angemeldet seit"),
                    ctx.t("Idle for", "Inaktiv seit"),
                )
            }
        ));

        let entries: Vec<_> = audit::load(&self.paths.config_file)
            .into_iter()
            .filter(|e| audit::is_sign_in(&e.action))
            .collect();
        let day = Utc::now() - chrono::Duration::hours(24);
        let recent = entries
            .iter()
            .filter(|e| e.at > day && e.action == "sign-in");
        let (good, bad) = recent.fold(
            (0, 0),
            |(g, b), e| if e.ok { (g + 1, b) } else { (g, b + 1) },
        );
        let addresses: std::collections::HashSet<&str> = entries
            .iter()
            .filter(|e| e.at > day && e.action == "sign-in" && !e.ok)
            .map(|e| e.source.as_str())
            .collect();
        html.push_str(&format!(
            r#"<p><span class="badge good">{good} {ok}</span> <span class="badge {cls}">{bad} {failed}</span> <span class="muted">{from} {n} {addr} · {last24}</span></p>"#,
            ok = ctx.t("successful", "erfolgreich"),
            cls = if bad > 0 { "bad" } else { "muted" },
            failed = ctx.t("failed", "fehlgeschlagen"),
            from = ctx.t("from", "von"),
            n = addresses.len(),
            addr = ctx.t("addresses", "Adressen"),
            last24 = ctx.t("last 24 hours", "letzte 24 Stunden"),
        ));
        let may_block = ctx.can(Permission::BlockAddresses);
        let blocked = super::access::Blocklist::load(&self.paths.config_file);
        let own = super::tls::primary_address();
        let csrf = csrf_field(&ctx.session);
        let rows: String = entries
            .iter()
            .rev()
            .take(300)
            .map(|e| {
                let action = match super::access::blockable(&e.source, own) {
                    Some(ip) if may_block && !e.ok => {
                        if blocked.blocking(ip).is_some() {
                            format!(
                                r#"<span class="badge bad">{}</span>"#,
                                ctx.t("blocked", "gesperrt")
                            )
                        } else {
                            format!(
                                r#"<form method="post" action="/access/block" class="inline">{csrf}<input type="hidden" name="address" value="{ip}"><input type="hidden" name="back" value="sign-ins"><button class="secondary">{label}</button></form>"#,
                                label = ctx.t("Block", "Sperren"),
                            )
                        }
                    }
                    _ => String::new(),
                };
                format!(
                    r#"<tr><td class="nowrap">{}</td><td class="nowrap">{}</td><td>{}</td><td class="nowrap"><span class="dot {}"></span>{}</td><td>{}</td><td>{action}</td></tr>"#,
                    esc(&e.at.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()),
                    esc(e.actor.trim_start_matches("web:")),
                    esc(&e.source),
                    if e.ok { "good" } else { "bad" },
                    esc(&e.action),
                    esc(&e.details.join("; ")),
                )
            })
            .collect();
        html.push_str(&format!(
            r#"<section class="card"><div class="scroll"><table><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th></th></tr></thead><tbody>{rows}</tbody></table></div></section>"#,
            ctx.t("When", "Wann"),
            ctx.t("User", "Benutzer"),
            ctx.t("Address", "Adresse"),
            ctx.t("What", "Was"),
            ctx.t("Details", "Details"),
        ));
        html
    }

    // --- access -----------------------------------------------------------------------

    fn access_page(&self, ctx: &Ctx<'_>) -> Response {
        use super::access::{AdminAccess, GeoMode};
        let csrf = csrf_field(&ctx.session);
        let list = super::access::Blocklist::load(&self.paths.config_file);
        let mut body = Self::message(ctx);
        let rows: String = list
            .entries
            .iter()
            .rev()
            .map(|e| {
                let by = if e.by == "automatic" {
                    ctx.t("automatic", "automatisch").to_string()
                } else {
                    e.by.clone()
                };
                format!(
                    r#"<tr><td class="nowrap"><strong>{address}</strong></td><td class="nowrap">{since}</td><td>{by}</td><td>{reason}</td><td><form method="post" action="/access/unblock" class="inline">{csrf}<input type="hidden" name="address" value="{address}"><button class="secondary">{label}</button></form></td></tr>"#,
                    address = esc(&e.address),
                    // Already HTML with the exact time as title.
                    since = Self::when(ctx, e.since),
                    by = esc(&by),
                    reason = esc(&e.reason),
                    label = ctx.t("Allow again", "Wieder zulassen"),
                )
            })
            .collect();
        let table = if rows.is_empty() {
            format!(
                r#"<p class="muted">{}</p>"#,
                ctx.t("No address is blocked.", "Keine Adresse ist gesperrt.")
            )
        } else {
            format!(
                r#"<div class="scroll"><table><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th></th></tr></thead><tbody>{rows}</tbody></table></div>"#,
                ctx.t("Address", "Adresse"),
                ctx.t("Since", "Seit"),
                ctx.t("By", "Von"),
                ctx.t("Reason", "Grund"),
            )
        };
        body.push_str(&format!(
            r#"<section class="card"><h2>{title} ({n})</h2>{table}</section>"#,
            title = ctx.t("Blocked addresses", "Gesperrte Adressen"),
            n = list.entries.len(),
        ));

        let w = &self.settings;
        let networks = if w.allowed_networks.is_empty() {
            ctx.t("any", "alle").to_string()
        } else {
            w.allowed_networks.join(", ")
        };
        let auto = if w.auto_block_after == 0 {
            ctx.t("off", "aus").to_string()
        } else if ctx.en {
            format!(
                "after {} wrong sign-ins within 24 hours",
                w.auto_block_after
            )
        } else {
            format!(
                "nach {} falschen Anmeldungen in 24 Stunden",
                w.auto_block_after
            )
        };
        let countries = match w.geo.mode {
            GeoMode::Off => ctx.t("any", "alle").to_string(),
            GeoMode::Allow => format!("{} {}", ctx.t("only", "nur"), w.geo.countries.join(", ")),
            GeoMode::Block => format!(
                "{} {}",
                ctx.t("all except", "alle außer"),
                w.geo.countries.join(", ")
            ),
        };
        let admins = match w.admin_access {
            AdminAccess::Anywhere => ctx.t("anywhere", "überall").to_string(),
            AdminAccess::ThisComputer => ctx
                .t("this computer only", "nur dieser Computer")
                .to_string(),
            AdminAccess::Networks => format!(
                "{} {}",
                ctx.t("this computer and", "dieser Computer und"),
                w.admin_networks.join(", ")
            ),
        };
        body.push_str(r#"<div class="grid">"#);
        body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2><form method="post" action="/access/block" class="stack">{csrf}<label>{address}<input name="address" required placeholder="203.0.113.7 / 203.0.113.0/24" autocomplete="off"></label><label>{reason}<input name="reason" autocomplete="off"></label><button>{label}</button></form></section>"#,
            title = ctx.t("Block an address", "Adresse sperren"),
            address = ctx.t("Address or network", "Adresse oder Netz"),
            reason = ctx.t("Reason", "Grund"),
            label = ctx.t("Block", "Sperren"),
        ));
        body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2><dl class="facts"><dt>{n}</dt><dd>{networks}</dd><dt>{c}</dt><dd>{countries}</dd><dt>{a}</dt><dd>{auto}</dd><dt>{ad}</dt><dd>{admins}</dd></dl><p class="muted small">{hint}</p></section>"#,
            title = ctx.t("Rules", "Regeln"),
            n = ctx.t("Networks", "Netze"),
            networks = esc(&networks),
            c = ctx.t("Countries", "Länder"),
            countries = esc(&countries),
            a = ctx.t("Automatic blocking", "Automatisch sperren"),
            auto = esc(&auto),
            ad = ctx.t("Administrators", "Administratoren"),
            admins = esc(&admins),
            hint = ctx.t(
                "Changed in AeternaVault on the server: Web tab, Access.",
                "Geändert in AeternaVault auf dem Server: Tab Web, Zugriff."
            ),
        ));
        body.push_str("</div>");
        self.page(ctx, ctx.t("Access", "Zugriff"), &body, false)
    }

    fn access_block(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let address = form.get("address").cloned().unwrap_or_default();
        let reason = form.get("reason").cloned().unwrap_or_default();
        let back = if form.get("back").is_some_and(|b| b == "sign-ins") {
            "/activity?tab=sign-ins"
        } else {
            "/access"
        };
        let reason = if reason.trim().is_empty() && back != "/access" {
            ctx.t(
                "blocked from the sign-in log",
                "aus dem Anmeldeprotokoll gesperrt",
            )
            .to_string()
        } else {
            reason
        };
        let mut list = super::access::Blocklist::load(&self.paths.config_file);
        let message = match list.add(&address, &reason, &ctx.actor()) {
            Ok(true) => match list.save(&self.paths.config_file) {
                Ok(()) => {
                    lock(&self.blocked).invalidate();
                    let mut record = ctx
                        .record("address-blocked")
                        .target(address.trim().to_string());
                    if !reason.trim().is_empty() {
                        record = record.details(vec![reason.trim().to_string()]);
                    }
                    audit::record(&self.paths.config_file, record);
                    format!("{}: {}", ctx.t("Blocked", "Gesperrt"), address.trim())
                }
                Err(err) => err.to_string(),
            },
            Ok(false) => ctx
                .t(
                    "This address was blocked already.",
                    "Diese Adresse war schon gesperrt.",
                )
                .to_string(),
            Err(err) => err,
        };
        let separator = if back.contains('?') { "&" } else { "?" };
        Response::redirect(&format!("{back}{separator}msg={}", encode(&message)))
    }

    fn access_unblock(&self, ctx: &Ctx<'_>) -> Response {
        let address = ctx.req.form().get("address").cloned().unwrap_or_default();
        let mut list = super::access::Blocklist::load(&self.paths.config_file);
        if !list.remove(&address) {
            return Self::back(
                "/access",
                ctx.t(
                    "This address is not blocked.",
                    "Diese Adresse ist nicht gesperrt.",
                ),
            );
        }
        if let Err(err) = list.save(&self.paths.config_file) {
            return Self::back("/access", &err.to_string());
        }
        lock(&self.blocked).invalidate();
        audit::record(
            &self.paths.config_file,
            ctx.record("address-unblocked")
                .target(address.trim().to_string()),
        );
        Self::back(
            "/access",
            &format!(
                "{}: {}",
                ctx.t("Allowed again", "Wieder zugelassen"),
                address.trim()
            ),
        )
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
        let operator = ctx.can(Permission::CheckBackups);
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
            r#"<form method="get" action="/audit" class="row search"><input name="q" value="{}" placeholder="{}"><button class="secondary">{}</button></form>"#,
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

    fn role_select(ctx: &Ctx<'_>, store: &UserStore, current: &str) -> String {
        let options: String = store
            .role_names()
            .iter()
            .map(|r| {
                let selected = if r.eq_ignore_ascii_case(current) {
                    " selected"
                } else {
                    ""
                };
                format!(
                    r#"<option value="{}"{selected}>{}</option>"#,
                    esc(r),
                    esc(&UserStore::role_label(r, ctx.en))
                )
            })
            .collect();
        let none = if current.is_empty() {
            format!(
                r#"<option value="" selected>{}</option>"#,
                ctx.t("no role", "keine Rolle")
            )
        } else {
            String::new()
        };
        format!(r#"<select name="role">{none}{options}</select>"#)
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
                "Limited to jobs (none: all)",
                "Auf Aufträge beschränkt (keiner: alle)"
            )
        )
    }

    fn users_page(&self, ctx: &Ctx<'_>) -> Response {
        let store = UserStore::load(&self.paths.config_file);
        let csrf = csrf_field(&ctx.session);
        let mut body = Self::message(ctx);
        if let Some(password) = lock(&self.flash).remove(&ctx.token) {
            body.push_str(&format!(
                r#"<p class="notice">{} <code>{}</code></p>"#,
                ctx.t(
                    "New password, shown only now:",
                    "Neues Passwort, nur jetzt sichtbar:"
                ),
                esc(&password)
            ));
        }

        // Users as a table; each row opens its own form.
        let rows: String = store
            .users
            .iter()
            .map(|user| {
                let source = if user.source == UserSource::Directory { "LDAP" } else { ctx.t("Local", "Lokal") };
                let mfa = if user.totp.is_some() {
                    ctx.t("App", "App")
                } else if user.email_codes && user.email.contains('@') {
                    ctx.t("E-mail", "E-Mail")
                } else {
                    "–"
                };
                let last = user
                    .last_sign_in
                    .map(|t| Self::when(ctx, t))
                    .unwrap_or_else(|| ctx.t("never", "noch nie").into());
                let state = if user.disabled {
                    format!(r#"<span class="badge bad">{}</span>"#, ctx.t("locked", "gesperrt"))
                } else if user.role.is_empty() {
                    format!(r#"<span class="badge warn">{}</span>"#, ctx.t("no role", "keine Rolle"))
                } else {
                    format!(r#"<span class="badge good">{}</span>"#, ctx.t("active", "aktiv"))
                };
                let password = if user.source == UserSource::Directory {
                    String::new()
                } else {
                    format!(r#"<button name="do" value="password" class="secondary">{}</button>"#, ctx.t("New password", "Neues Passwort"))
                };
                format!(
                    r#"<tr><td><strong>{name}</strong><div class="muted small">{source} · MFA: {mfa} · {last}</div></td><td>{state}</td><td><form method="post" action="/users/change" class="row">{csrf}<input type="hidden" name="user" value="{name}">{select}<button name="do" value="save" class="secondary">{save}</button><button name="do" value="{toggle}" class="secondary">{toggle_label}</button>{password}<button name="do" value="2fa" class="secondary">{reset2fa}</button><button name="do" value="remove" class="danger">{remove}</button></form></td></tr>"#,
                    name = esc(&user.name),
                    select = Self::role_select(ctx, &store, &user.role),
                    save = ctx.t("Save role", "Rolle speichern"),
                    toggle = if user.disabled { "enable" } else { "disable" },
                    toggle_label = if user.disabled { ctx.t("Unlock", "Entsperren") } else { ctx.t("Lock", "Sperren") },
                    reset2fa = ctx.t("Reset MFA", "MFA zurücksetzen"),
                    remove = ctx.t("Remove", "Entfernen"),
                )
            })
            .collect();
        body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2><div class="scroll"><table><thead><tr><th>{n}</th><th>{st}</th><th>{r}</th></tr></thead><tbody>{rows}</tbody></table></div></section>"#,
            title = ctx.t("Users", "Benutzer"),
            n = ctx.t("User", "Benutzer"),
            st = ctx.t("Status", "Status"),
            r = ctx.t("Role and actions", "Rolle und Aktionen"),
        ));

        body.push_str(r#"<div class="grid">"#);
        body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2><form method="post" action="/users/add" class="stack">{csrf}
<label>{name}<input name="user" required autocomplete="off"></label><label>{pass}<input name="password" type="password" autocomplete="new-password" required></label>
<label>{role_label}{select}</label>{jobs}<button>{add}</button></form></section>"#,
            title = ctx.t("Add user", "Benutzer hinzufügen"),
            name = ctx.t("User name", "Benutzername"),
            pass = ctx.t("Initial password", "Startpasswort"),
            role_label = ctx.t("Role", "Rolle"),
            select = Self::role_select(ctx, &store, "viewer"),
            jobs = Self::job_boxes(ctx, &[]),
            add = ctx.t("Add", "Hinzufügen"),
        ));

        // Roles: the built-in ones as reference, own ones editable.
        let mut roles = String::new();
        for name in store.role_names() {
            let perms = store.role_permissions(&name).unwrap_or_default();
            let builtin = users::Role::from_id(&name).is_some();
            let list: String = Permission::ALL
                .iter()
                .map(|p| {
                    let on = perms.contains(p);
                    if builtin {
                        format!(
                            r#"<li class="{}">{}</li>"#,
                            if on { "good" } else { "muted" },
                            esc(p.name(ctx.en))
                        )
                    } else {
                        format!(
                            r#"<label class="check"><input type="checkbox" name="perm" value="{}"{}> {}</label>"#,
                            serde_json::to_value(p).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
                            if on { " checked" } else { "" },
                            esc(p.name(ctx.en))
                        )
                    }
                })
                .collect();
            if builtin {
                roles.push_str(&format!(
                    r#"<details><summary>{}</summary><ul class="perms">{list}</ul></details>"#,
                    esc(&UserStore::role_label(&name, ctx.en))
                ));
            } else {
                roles.push_str(&format!(
                    r#"<details><summary>{n}</summary><form method="post" action="/roles/save" class="stack">{csrf}<input type="hidden" name="name" value="{n}">{list}<div class="row"><button name="do" value="save" class="secondary">{save}</button><button name="do" value="remove" class="danger">{remove}</button></div></form></details>"#,
                    n = esc(&name),
                    save = ctx.t("Save", "Speichern"),
                    remove = ctx.t("Remove", "Entfernen"),
                ));
            }
        }
        let new_perms: String = Permission::ALL
            .iter()
            .map(|p| {
                format!(
                    r#"<label class="check"><input type="checkbox" name="perm" value="{}"> {}</label>"#,
                    serde_json::to_value(p).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
                    esc(p.name(ctx.en))
                )
            })
            .collect();
        body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2>{roles}<h3>{new}</h3><form method="post" action="/roles/save" class="stack">{csrf}<label>{name}<input name="name" required></label>{new_perms}<button name="do" value="save">{create}</button></form></section>"#,
            title = ctx.t("Roles", "Rollen"),
            new = ctx.t("New role", "Neue Rolle"),
            name = ctx.t("Name", "Name"),
            create = ctx.t("Create role", "Rolle anlegen"),
        ));
        body.push_str("</div>");
        self.page(ctx, ctx.t("Users", "Benutzer"), &body, false)
    }

    fn role_save(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let name = form.get("name").cloned().unwrap_or_default();
        let perms: Vec<Permission> = ctx
            .req
            .form_all("perm")
            .iter()
            .filter_map(|p| serde_json::from_value(serde_json::Value::String(p.clone())).ok())
            .collect();
        let mut store = UserStore::load(&self.paths.config_file);
        let removing = form.get("do").is_some_and(|d| d == "remove");
        let result = if removing {
            store.remove_role(&name)
        } else {
            store.set_role(&name, perms.clone())
        };
        if let Err(err) = result {
            return Self::back("/users", &err);
        }
        // Somebody must still be able to manage users.
        if store.admins() == 0 {
            return Self::back(
                "/users",
                ctx.t(
                    "At least one user must keep the right to manage users.",
                    "Mindestens ein Benutzer muss Benutzer verwalten dürfen.",
                ),
            );
        }
        if let Err(err) = store.save(&self.paths.config_file) {
            return Self::back("/users", &err.to_string());
        }
        let details = if removing {
            Vec::new()
        } else {
            vec![format!(
                "permissions: {}",
                perms
                    .iter()
                    .map(|p| p.name(true))
                    .collect::<Vec<_>>()
                    .join(", ")
            )]
        };
        audit::record(
            &self.paths.config_file,
            ctx.record(if removing {
                "role-removed"
            } else {
                "role-saved"
            })
            .target(name.trim())
            .details(details),
        );
        Self::back("/users", ctx.t("Saved.", "Gespeichert."))
    }

    fn user_add(&self, ctx: &Ctx<'_>) -> Response {
        let form = ctx.req.form();
        let name = form.get("user").cloned().unwrap_or_default();
        let password = form.get("password").cloned().unwrap_or_default();
        let role = form.get("role").cloned().unwrap_or_default();
        let jobs = ctx.req.form_all("jobs");
        let mut store = UserStore::load(&self.paths.config_file);
        if let Err(err) = store.add(&name, &role, &password) {
            return Self::back("/users", &err);
        }
        if let Some(user) = store.find_mut(&name) {
            user.jobs = jobs.clone();
            user.must_change_password = true;
        }
        if let Err(err) = store.save(&self.paths.config_file) {
            return Self::back("/users", &err.to_string());
        }
        let mut details = vec![format!("role: {role}")];
        if !jobs.is_empty() {
            details.push(format!("jobs: {}", jobs.join(", ")));
        }
        audit::record(
            &self.paths.config_file,
            ctx.record("user-added")
                .target(name.trim())
                .details(details),
        );
        let mut message = ctx.t("User added.", "Benutzer hinzugefügt.").to_string();
        if let Some(advice) =
            users::password_advice(&password, self.settings.recommended_password_length, ctx.en)
        {
            message = format!("{message} {advice}");
        }
        Self::back("/users", &message)
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
            "The last user who manages users cannot be removed, locked or lose that right.",
            "Der letzte Benutzer mit Benutzerverwaltung kann nicht entfernt, gesperrt oder herabgestuft werden.",
        );
        let mut details = Vec::new();
        let mut end_sessions = false;
        let mut new_password = None;
        let audit_action;
        match action.as_str() {
            "save" => {
                let role = form
                    .get("role")
                    .cloned()
                    .unwrap_or_else(|| before.role.clone());
                if store.role_permissions(&role).is_none() {
                    return Self::back(
                        "/users",
                        ctx.t("There is no such role.", "Diese Rolle gibt es nicht."),
                    );
                }
                let keeps_admin = store
                    .role_permissions(&role)
                    .is_some_and(|p| p.contains(&Permission::ManageUsers));
                if last_admin && !keeps_admin {
                    return Self::back("/users", refuse_last_admin);
                }
                let jobs = ctx.req.form_all("jobs");
                let user = store.find_mut(&name).expect("checked above");
                if user.role != role {
                    details.push(format!("role: {} → {role}", user.role));
                }
                if form.contains_key("jobs") && user.jobs != jobs {
                    details.push(format!(
                        "jobs: [{}] → [{}]",
                        user.jobs.join(", "),
                        jobs.join(", ")
                    ));
                    user.jobs = jobs;
                }
                user.role = role;
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
            "password" if before.source == UserSource::Local => {
                let password = users::random_password();
                let user = store.find_mut(&name).expect("checked above");
                user.password = users::hash_password(&password);
                user.must_change_password = true;
                new_password = Some(password);
                end_sessions = true;
                audit_action = "user-password-reset";
            }
            "2fa" => {
                let user = store.find_mut(&name).expect("checked above");
                user.totp = None;
                user.email_codes = false;
                end_sessions = true;
                audit_action = "user-mfa-reset";
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
        } else if ctx.mfa && ctx.user.totp.is_none() && !self.email_factor(ctx) {
            body.push_str(&format!(
                r#"<p class="notice">{}</p>"#,
                ctx.t(
                    "One-time codes are required here. Please set them up first.",
                    "Einmalcodes sind hier vorgeschrieben. Bitte richte sie zuerst ein."
                )
            ));
        }
        let csrf = csrf_field(&ctx.session);
        if ctx.user.source == UserSource::Directory {
            body.push_str(&format!(
                r#"<section class="card"><h2>{}</h2><p class="muted">{}: {}</p><p>{}</p></section>"#,
                ctx.t("Password", "Passwort"),
                ctx.t("Role", "Rolle"),
                UserStore::role_label(&ctx.user.role, ctx.en),
                ctx.t(
                    "You sign in with your directory account (LDAP / Active Directory). Its password and your role are managed there.",
                    "Du meldest dich mit deinem Verzeichniskonto an (LDAP / Active Directory). Passwort und Rolle werden dort verwaltet."
                )
            ));
        } else {
            body.push_str(&format!(
            r#"<section class="card"><h2>{title}</h2><p class="muted">{role}: {r}</p><form method="post" action="/account/password" class="stack">{csrf}
<label>{current}<input name="current" type="password" autocomplete="current-password" required></label>
<label>{new}<input name="new" type="password" autocomplete="new-password" required></label>
<label>{again}<input name="again" type="password" autocomplete="new-password" required></label><button>{save}</button></form></section>"#,
            title = ctx.t("Password", "Passwort"),
            role = ctx.t("Role", "Rolle"),
            r = UserStore::role_label(&ctx.user.role, ctx.en),
            current = ctx.t("Current password", "Aktuelles Passwort"),
            new = if ctx.en {
                format!("New password (recommended: at least {} characters)", self.settings.recommended_password_length)
            } else {
                format!("Neues Passwort (empfohlen: mindestens {} Zeichen)", self.settings.recommended_password_length)
            },
            again = ctx.t("New password again", "Neues Passwort wiederholen"),
            save = ctx.t("Change password", "Passwort ändern"),
        ));
        }
        let pending = lock(&self.pending_totp)
            .get(&ctx.user.name.to_lowercase())
            .cloned();
        let twofa = match (&ctx.user.totp, pending) {
            (Some(_), _) => format!(
                r#"<p><span class="badge good">{on}</span></p>{off}"#,
                on = ctx.t("on", "an"),
                off = if ctx.mfa && !self.email_factor(ctx) {
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
        if self.settings.mfa_app || ctx.user.totp.is_some() {
            body.push_str(&format!(
                r#"<section class="card"><h2>{}</h2>{twofa}</section>"#,
                ctx.t("Authenticator app", "Authenticator-App")
            ));
        }
        if self.settings.mfa_email {
            body.push_str(&self.email_section(ctx));
        }
        self.page(ctx, ctx.t("Account", "Konto"), &body, false)
    }

    /// Whether codes by e-mail work for this user now (channel, address, chosen or required).
    fn email_factor(&self, ctx: &Ctx<'_>) -> bool {
        self.email_possible(&ctx.config)
            && ctx.user.email.contains('@')
            && (ctx.user.email_codes || ctx.mfa)
    }

    fn email_section(&self, ctx: &Ctx<'_>) -> String {
        let csrf = csrf_field(&ctx.session);
        let title = ctx.t("Codes by e-mail", "Codes per E-Mail");
        if super::email_channel(&ctx.config).is_none() {
            return format!(
                r#"<section class="card"><h2>{title}</h2><p class="muted">{}</p></section>"#,
                ctx.t(
                    "Not available: no e-mail channel is set up on the server (AeternaVault → Notifications).",
                    "Nicht verfügbar: Auf dem Server ist kein E-Mail-Kanal eingerichtet (AeternaVault → Benachrichtigungen)."
                )
            );
        }
        let directory = ctx.user.source == UserSource::Directory;
        let address = esc(&ctx.user.email);
        let pending = lock(&self.pending_email)
            .get(&ctx.user.name.to_lowercase())
            .cloned();
        let mut html = String::new();
        if self.email_factor(ctx) {
            html.push_str(&format!(
                r#"<p><span class="badge good">{on}</span> {to} <strong>{address}</strong></p>"#,
                on = ctx.t("on", "an"),
                to = ctx.t("Codes go to", "Codes gehen an"),
            ));
            let can_switch_off = !ctx.mfa || ctx.user.totp.is_some();
            if can_switch_off && ctx.user.email_codes {
                html.push_str(&format!(
                    r#"<form method="post" action="/account/email/off" class="stack">{csrf}<label>{pw}<input name="current" type="password" required></label><button class="secondary">{label}</button></form>"#,
                    pw = ctx.t("Password", "Passwort"),
                    label = ctx.t("Switch off", "Ausschalten")
                ));
            }
        } else if directory && ctx.user.email.contains('@') {
            html.push_str(&format!(
                r#"<p>{text} <strong>{address}</strong></p><form method="post" action="/account/email/use">{csrf}<button>{label}</button></form>"#,
                text = ctx.t("Your directory account has the address", "Dein Verzeichniskonto hat die Adresse"),
                label = ctx.t("Use codes by e-mail", "Codes per E-Mail verwenden"),
            ));
        } else if let Some(code) = pending {
            html.push_str(&format!(
                r#"<p>{text} <strong>{to}</strong>.</p><form method="post" action="/account/email/confirm" class="stack">{csrf}<label>{label}<input name="code" inputmode="numeric" autocomplete="one-time-code" required></label><button>{confirm}</button></form>"#,
                text = ctx.t("A code was sent to", "Ein Code wurde gesendet an"),
                to = esc(&code.address),
                label = ctx.t("Code from the e-mail", "Code aus der E-Mail"),
                confirm = ctx.t("Confirm", "Bestätigen"),
            ));
        } else if directory {
            html.push_str(&format!(
                r#"<p class="muted">{}</p>"#,
                ctx.t(
                    "Your directory account has no e-mail address (attribute mail).",
                    "Dein Verzeichniskonto hat keine E-Mail-Adresse (Attribut mail)."
                )
            ));
        } else {
            html.push_str(&format!(
                r#"<p>{explain}</p><form method="post" action="/account/email/start" class="stack">{csrf}<label>{label}<input name="email" type="email" autocomplete="email" value="{address}" required></label><button>{send}</button></form>"#,
                explain = ctx.t(
                    "After the password, sign-in then needs a code sent to this address.",
                    "Nach dem Passwort braucht die Anmeldung dann einen Code, der an diese Adresse geht."
                ),
                label = ctx.t("E-mail address", "E-Mail-Adresse"),
                send = ctx.t("Send a code to confirm", "Code zum Bestätigen senden"),
            ));
        }
        format!(r#"<section class="card"><h2>{title}</h2>{html}</section>"#)
    }

    /// The user's own password: from the hash here, or checked by the directory.
    fn own_password_ok(&self, ctx: &Ctx<'_>, password: &str) -> bool {
        match ctx.user.source {
            UserSource::Local => users::verify_password(password, &ctx.user.password),
            UserSource::Directory => {
                super::ldap::authenticate(&self.settings.ldap, &ctx.user.name, password, &|_| 0)
                    .is_ok()
            }
        }
    }

    fn email_start(&self, ctx: &Ctx<'_>) -> Response {
        if ctx.user.source == UserSource::Directory || !self.settings.mfa_email {
            return self.forbidden(ctx);
        }
        let address = ctx
            .req
            .form()
            .get("email")
            .map(|e| e.trim().to_string())
            .unwrap_or_default();
        if !users::plausible_email(&address) {
            return self.account(
                ctx,
                Some(ctx.t(
                    "This is not an e-mail address.",
                    "Das ist keine E-Mail-Adresse.",
                )),
            );
        }
        let key = ctx.user.name.to_lowercase();
        if lock(&self.pending_email)
            .get(&key)
            .is_some_and(|c| c.sent.elapsed() < Duration::from_secs(60))
        {
            return self.account(
                ctx,
                Some(ctx.t(
                    "A code was sent less than a minute ago.",
                    "Vor weniger als einer Minute wurde schon ein Code gesendet.",
                )),
            );
        }
        let code = EmailCode::new(&address);
        if let Err(err) = super::send_code(&ctx.config, &address, &code.code, true) {
            tracing::warn!("web interface: confirmation code not sent: {err}");
            return self.account(ctx, Some(ctx.t("The code could not be sent. Details are in the log of the server.", "Der Code konnte nicht gesendet werden. Details stehen im Protokoll des Servers.")));
        }
        lock(&self.pending_email).insert(key, code);
        Response::redirect("/account")
    }

    fn email_confirm(&self, ctx: &Ctx<'_>) -> Response {
        let key = ctx.user.name.to_lowercase();
        let typed = ctx.req.form().get("code").cloned().unwrap_or_default();
        let checked = lock(&self.pending_email)
            .get_mut(&key)
            .map(|c| (c.check(&typed), c.address.clone()));
        let Some((true, address)) = checked else {
            return self.account(
                ctx,
                Some(ctx.t(
                    "The code is not correct or no longer valid.",
                    "Der Code ist nicht korrekt oder nicht mehr gültig.",
                )),
            );
        };
        lock(&self.pending_email).remove(&key);
        let mut store = UserStore::load(&self.paths.config_file);
        if let Some(user) = store.find_mut(&ctx.user.name) {
            user.email = address.clone();
            user.email_codes = true;
        }
        if let Err(err) = store.save(&self.paths.config_file) {
            return self.account(ctx, Some(&err.to_string()));
        }
        audit::record(
            &self.paths.config_file,
            ctx.record("email-codes-on")
                .details(vec![format!("address: {}", users::masked(&address))]),
        );
        Self::back(
            "/account",
            ctx.t("Codes by e-mail are on.", "Codes per E-Mail sind an."),
        )
    }

    fn email_use(&self, ctx: &Ctx<'_>) -> Response {
        if !ctx.user.email.contains('@') {
            return Response::redirect("/account");
        }
        let mut store = UserStore::load(&self.paths.config_file);
        if let Some(user) = store.find_mut(&ctx.user.name) {
            user.email_codes = true;
        }
        let _ = store.save(&self.paths.config_file);
        audit::record(
            &self.paths.config_file,
            ctx.record("email-codes-on")
                .details(vec![format!("address: {}", users::masked(&ctx.user.email))]),
        );
        Self::back(
            "/account",
            ctx.t("Codes by e-mail are on.", "Codes per E-Mail sind an."),
        )
    }

    fn email_off(&self, ctx: &Ctx<'_>) -> Response {
        if ctx.mfa && ctx.user.totp.is_none() {
            return self.forbidden(ctx);
        }
        let current = ctx.req.form().get("current").cloned().unwrap_or_default();
        if !self.own_password_ok(ctx, &current) {
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
            user.email_codes = false;
        }
        let _ = store.save(&self.paths.config_file);
        audit::record(&self.paths.config_file, ctx.record("email-codes-off"));
        Self::back(
            "/account",
            ctx.t("Codes by e-mail are off.", "Codes per E-Mail sind aus."),
        )
    }

    fn change_password(&self, ctx: &Ctx<'_>) -> Response {
        if ctx.user.source == UserSource::Directory {
            return self.forbidden(ctx);
        }
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
        if let Err(err) = users::check_password(&new) {
            return self.account(ctx, Some(&err));
        }
        let advice =
            users::password_advice(&new, self.settings.recommended_password_length, ctx.en);
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
                let mut message = ctx.t("Password changed.", "Passwort geändert.").to_string();
                if let Some(advice) = advice {
                    message = format!("{message} {advice}");
                }
                return Self::back("/account", &message)
                    .header("Set-Cookie", &self.session_cookie(ctx.req, &token));
            }
        }
        Response::redirect("/login")
    }

    fn totp_start(&self, ctx: &Ctx<'_>) -> Response {
        if !self.settings.mfa_app {
            return self.forbidden(ctx);
        }
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
        if ctx.mfa && !self.email_factor(ctx) {
            return self.forbidden(ctx);
        }
        let current = ctx.req.form().get("current").cloned().unwrap_or_default();
        if !self.own_password_ok(ctx, &current) {
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
