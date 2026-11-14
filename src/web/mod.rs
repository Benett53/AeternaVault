//! The web interface: an overview of jobs, backups and problems, and the
//! everyday actions (run a job, check a backup), for computers that are
//! managed from elsewhere, e.g. a server.
//!
//! It is off unless switched on (Settings → Web, or `aeternavault-cli web`).
//! It runs inside the window process (next to the automatic backups, which it
//! starts through [`crate::automatic::service`]) or on its own with
//! `aeternavault-cli web serve`. Plain HTTP on this computer and in the
//! network; HTTPS from a reverse proxy in front or with own certificate
//! files. Users, roles and sign-in are in [`users`]; every sign-in and
//! change is written to the [`crate::audit`] log.
//!
//! Not offered here on purpose: restoring, deleting backups, changing folders
//! or settings, and entering passphrases.

pub mod access;
pub mod app;
pub mod countries;
pub mod device;
pub mod http;
pub mod ldap;
#[cfg(test)]
mod server_tests;
pub mod tls;
pub mod users;

use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::audit::{self, Record};
use crate::config::{Config, TlsMode, WebSettings};
use crate::engine::{CancelToken, Progress};
use crate::paths::AppPaths;

/// What the web interface asks of the process it runs in.
pub trait Control: Send + Sync {
    /// Starts a job now (`None`: all ticked folders).
    fn run(&self, schedule: Option<String>) -> Result<(), String>;
    /// The job running now and its progress.
    fn running(&self) -> Option<(String, Progress)>;
    fn cancel(&self);
    /// The settings were changed over the web.
    fn config_changed(&self, config: &Config);
}

/// For `aeternavault-cli web serve`: runs jobs on threads of its own. The
/// schedules themselves are run by the window or the systemd timer as usual.
pub struct LocalRunner {
    paths: AppPaths,
    running: Arc<Mutex<Option<(String, Progress, CancelToken)>>>,
}

impl LocalRunner {
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            running: Arc::new(Mutex::new(None)),
        }
    }
}

impl Control for LocalRunner {
    fn run(&self, schedule: Option<String>) -> Result<(), String> {
        let config = crate::config::load_or_create(&self.paths).config;
        let schedule = match schedule {
            Some(id) => Some(
                config
                    .schedules
                    .iter()
                    .find(|s| s.id == id)
                    .cloned()
                    .ok_or("this job does not exist any more")?,
            ),
            None => None,
        };
        let mut slot = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err("a backup is already running".into());
        }
        let lang = crate::i18n::Lang::resolve(config.language);
        let label = crate::automatic::job_label(schedule.as_ref(), &config, lang);
        let cancel = CancelToken::default();
        *slot = Some((label, Progress::default(), cancel.clone()));
        drop(slot);
        let (paths, running) = (self.paths.clone(), self.running.clone());
        std::thread::Builder::new()
            .name("aeterna-web-job".into())
            .spawn(move || {
                let _ = crate::automatic::run_job(
                    &paths,
                    &config,
                    schedule.as_ref(),
                    None,
                    &cancel,
                    &mut |progress| {
                        if let Some(slot) =
                            running.lock().unwrap_or_else(|e| e.into_inner()).as_mut()
                        {
                            slot.1 = progress.clone();
                        }
                    },
                    false,
                );
                *running.lock().unwrap_or_else(|e| e.into_inner()) = None;
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn running(&self) -> Option<(String, Progress)> {
        let slot = self.running.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_ref()
            .map(|(label, progress, _)| (label.clone(), progress.clone()))
    }

    fn cancel(&self) {
        if let Some((_, _, cancel)) = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
    }

    fn config_changed(&self, _config: &Config) {}
}

/// The e-mail channel that sends sign-in codes: the one named in the web
/// settings, or the first switched-on e-mail channel.
pub fn email_channel(config: &Config) -> Option<crate::notify::Channel> {
    use crate::notify::ChannelKind;
    let wanted = config.web.email_channel.trim();
    config
        .notifications
        .channels
        .iter()
        .filter(|c| c.kind == ChannelKind::Email && c.enabled)
        .find(|c| wanted.is_empty() || c.name.trim().eq_ignore_ascii_case(wanted))
        .cloned()
}

/// Sends a sign-in code to `address` through the e-mail channel.
pub fn send_code(config: &Config, address: &str, code: &str, confirm: bool) -> Result<(), String> {
    let mut channel = email_channel(config).ok_or("no e-mail channel is set up (Notifications)")?;
    channel.to = address.to_string();
    let en = crate::i18n::Lang::resolve(config.language) == crate::i18n::Lang::En;
    let computer = crate::platform::computer_name();
    let (title, body) = match (en, confirm) {
        (true, false) => (
            format!("AeternaVault {computer}: sign-in code {code}"),
            format!(
                "Your code for signing in to the AeternaVault web interface on {computer}: {code}\n\nIt is valid for 10 minutes. If you did not try to sign in, someone knows your password: change it and tell your administrator."
            ),
        ),
        (false, false) => (
            format!("AeternaVault {computer}: Anmeldecode {code}"),
            format!(
                "Dein Code für die Anmeldung an der AeternaVault-Web-Oberfläche auf {computer}: {code}\n\nEr gilt 10 Minuten. Wenn du dich nicht anmelden wolltest, kennt jemand dein Passwort: Ändere es und informiere deinen Administrator."
            ),
        ),
        (true, true) => (
            format!("AeternaVault {computer}: confirm your address"),
            format!(
                "Code to confirm this address for sign-in codes: {code}\n\nValid for 10 minutes."
            ),
        ),
        (false, true) => (
            format!("AeternaVault {computer}: Adresse bestätigen"),
            format!(
                "Code zum Bestätigen dieser Adresse für Anmeldecodes: {code}\n\nGültig für 10 Minuten."
            ),
        ),
    };
    let message = crate::notify::Message {
        title,
        body,
        severity: crate::notify::Severity::Info,
        attachment: None,
    };
    crate::notify::send(&channel, &message).map_err(|e| e.to_string())
}

/// A network clients may connect from: `192.168.1.0/24`, `10.0.0.5`, `fd00::/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl std::str::FromStr for Network {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (addr, prefix) = match text.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (text, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("{text} is not an address or network"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("{text}: the number after / must be 0 to {max}"))?,
            None => max,
        };
        Ok(Self { addr, prefix })
    }
}

impl Network {
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = http::canonical(ip);
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// Checks the settings before starting; the message says what to change.
pub fn check_settings(settings: &WebSettings) -> Result<Vec<Network>, String> {
    if settings.tls == TlsMode::Files
        && (settings.cert_file.as_os_str().is_empty() || settings.key_file.as_os_str().is_empty())
    {
        return Err(
            "choose the certificate and key files, or switch HTTPS off and use a reverse proxy"
                .into(),
        );
    }
    if settings.port == 0 {
        return Err("choose a port between 1 and 65535".into());
    }
    settings.ldap.check()?;
    bind_address(settings)?;
    if settings.mfa != access::MfaPolicy::Optional && !settings.mfa_app && !settings.mfa_email {
        return Err(
            "2FA is required, but neither an authenticator app nor e-mail codes are allowed".into(),
        );
    }
    if settings.admin_access == access::AdminAccess::Networks {
        for n in settings
            .admin_networks
            .iter()
            .filter(|n| !n.trim().is_empty())
        {
            n.parse::<Network>()?;
        }
    }
    settings.geo.codes()?;
    for proxy in settings
        .trusted_proxies
        .iter()
        .filter(|n| !n.trim().is_empty())
    {
        proxy.parse::<Network>()?;
    }
    settings
        .allowed_networks
        .iter()
        .filter(|n| !n.trim().is_empty())
        .map(|n| n.parse())
        .collect()
}

fn bind_address(settings: &WebSettings) -> Result<IpAddr, String> {
    let bind = settings.bind.trim();
    if bind.eq_ignore_ascii_case("localhost") {
        return Ok(IpAddr::from([127, 0, 0, 1]));
    }
    bind.parse().map_err(|_| {
        format!("{bind} is not an address of this computer (e.g. 127.0.0.1 or 0.0.0.0)")
    })
}

/// A running web interface. Stops when dropped.
pub struct WebServer {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub url: String,
    pub settings: WebSettings,
    config_file: std::path::PathBuf,
    app: Arc<app::WebApp>,
}

impl WebServer {
    pub fn start(
        paths: &AppPaths,
        settings: &WebSettings,
        control: Arc<dyn Control>,
    ) -> Result<Self, String> {
        let networks = check_settings(settings)?;
        let ip = bind_address(settings)?;
        let listener = TcpListener::bind((ip, settings.port))
            .map_err(|e| format!("port {} on {ip} could not be opened: {e}", settings.port))?;
        let tls = tls::server_config(settings)?;
        let app = Arc::new(app::WebApp::new(
            paths.clone(),
            settings.clone(),
            networks,
            control,
        )?);
        let handling = app.clone();
        let handler: http::Handler = Arc::new(move |request| handling.handle(request));
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("aeterna-web-listen".into())
            .spawn(move || http::serve(listener, tls, handler, stop_flag))
            .map_err(|e| e.to_string())?;
        let url = settings.url();
        tracing::info!("web interface started: {url}");
        audit::record(
            &paths.config_file,
            Record::local("web-started").target(url.clone()),
        );
        Ok(Self {
            stop,
            thread: Some(thread),
            url,
            settings: settings.clone(),
            config_file: paths.config_file.clone(),
            app,
        })
    }

    /// Connections refused by country since the start, by ISO code
    /// (`??`: not in the database).
    pub fn geo_refusals(&self) -> Vec<(String, u64)> {
        self.app.geo_refusals()
    }

    /// Who is signed in now: (user, address, signed in for, idle for).
    pub fn active_sessions(
        &self,
    ) -> Vec<(String, IpAddr, std::time::Duration, std::time::Duration)> {
        self.app.active_sessions()
    }

    pub fn stop(&mut self) {
        if self.stop.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        tracing::info!("web interface stopped");
        audit::record(
            &self.config_file,
            Record::local("web-stopped").target(self.url.clone()),
        );
    }
}

impl Drop for WebServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_match_addresses() {
        let lan: Network = "192.168.1.0/24".parse().unwrap();
        assert!(lan.contains("192.168.1.77".parse().unwrap()));
        assert!(lan.contains("::ffff:192.168.1.77".parse().unwrap()));
        assert!(!lan.contains("192.168.2.1".parse().unwrap()));
        let one: Network = "10.0.0.5".parse().unwrap();
        assert!(one.contains("10.0.0.5".parse().unwrap()));
        assert!(!one.contains("10.0.0.6".parse().unwrap()));
        let all: Network = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("8.8.8.8".parse().unwrap()));
        let v6: Network = "fd00::/8".parse().unwrap();
        assert!(v6.contains("fd12::1".parse().unwrap()));
        assert!("192.168.1.0/33".parse::<Network>().is_err());
        assert!("server".parse::<Network>().is_err());
    }

    #[test]
    fn plain_http_in_the_network_https_only_with_own_files() {
        let mut settings = WebSettings::default();
        assert_eq!(settings.bind, "0.0.0.0");
        assert!(
            check_settings(&settings).is_ok(),
            "plain HTTP in the network"
        );
        assert!(!settings.uses_tls());
        settings.tls = TlsMode::Files;
        assert!(check_settings(&settings).is_err(), "files must be chosen");
        settings.trusted_proxies = vec!["proxy.example".into()];
        settings.tls = TlsMode::Off;
        assert!(check_settings(&settings).is_err(), "proxies are addresses");
    }
}
