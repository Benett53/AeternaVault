//! Notifications after backups: a summary on success, alerts on problems and
//! when the destination runs out of space.
//!
//! Channels: e-mail (SMTP), Microsoft Teams (Workflows webhook), Telegram,
//! ntfy, Slack, Discord, Gotify and a generic JSON webhook. Every channel is
//! sent to independently; a failing channel is logged and does not affect the
//! backup or the other channels.
//!
//! Titles and texts are templates with placeholders such as `{status}`,
//! `{files}` or `{free}` (see [`PLACEHOLDERS`]).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::engine::backup::BackupReport;
use crate::i18n::Lang;
use crate::platform;
use crate::state::{AutomaticOutcome, AutomaticRun};

/// Placeholders available in titles and messages.
pub const PLACEHOLDERS: &[&str] = &[
    "{job}",
    "{status}",
    "{backup}",
    "{files}",
    "{copied}",
    "{size}",
    "{failed}",
    "{duration}",
    "{computer}",
    "{destination}",
    "{free}",
    "{free_percent}",
    "{message}",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    #[default]
    Ntfy,
    Email,
    Teams,
    Telegram,
    Slack,
    Discord,
    Gotify,
    Webhook,
}

impl ChannelKind {
    pub const ALL: [ChannelKind; 8] = [
        ChannelKind::Ntfy,
        ChannelKind::Email,
        ChannelKind::Teams,
        ChannelKind::Telegram,
        ChannelKind::Slack,
        ChannelKind::Discord,
        ChannelKind::Gotify,
        ChannelKind::Webhook,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ChannelKind::Ntfy => "ntfy",
            ChannelKind::Email => "E-mail (SMTP)",
            ChannelKind::Teams => "Microsoft Teams",
            ChannelKind::Telegram => "Telegram",
            ChannelKind::Slack => "Slack",
            ChannelKind::Discord => "Discord",
            ChannelKind::Gotify => "Gotify",
            ChannelKind::Webhook => "Webhook (JSON)",
        }
    }

    /// What `url` means for this kind.
    pub fn url_hint(self) -> &'static str {
        match self {
            ChannelKind::Ntfy => "https://ntfy.sh/your-topic",
            ChannelKind::Email => "",
            ChannelKind::Teams => "Workflows webhook URL (Teams → channel → Workflows)",
            ChannelKind::Telegram => "",
            ChannelKind::Slack => "https://hooks.slack.com/services/…",
            ChannelKind::Discord => "https://discord.com/api/webhooks/…",
            ChannelKind::Gotify => "https://gotify.example.com",
            ChannelKind::Webhook => "https://example.com/hook",
        }
    }

    pub fn uses_url(self) -> bool {
        !matches!(self, ChannelKind::Email | ChannelKind::Telegram)
    }

    /// Whether `token` is used (bot token, access token, app token).
    pub fn uses_token(self) -> bool {
        matches!(
            self,
            ChannelKind::Ntfy | ChannelKind::Telegram | ChannelKind::Gotify | ChannelKind::Webhook
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SmtpSecurity {
    /// STARTTLS, usually port 587.
    #[default]
    Starttls,
    /// TLS from the start, usually port 465.
    Tls,
    /// No encryption (only inside a trusted network).
    None,
}

/// One place notifications are sent to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub name: String,
    pub kind: ChannelKind,
    pub enabled: bool,
    /// Webhook or server URL (not used by e-mail and Telegram).
    pub url: String,
    /// Telegram bot token, ntfy/Gotify access token, or a bearer token for webhooks.
    pub token: String,
    /// Telegram chat id.
    pub chat_id: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: SmtpSecurity,
    pub smtp_user: String,
    pub smtp_password: String,
    pub from: String,
    /// One or more recipients, separated by commas.
    pub to: String,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: ChannelKind::default(),
            enabled: true,
            url: String::new(),
            token: String::new(),
            chat_id: String::new(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_security: SmtpSecurity::default(),
            smtp_user: String::new(),
            smtp_password: String::new(),
            from: String::new(),
            to: String::new(),
        }
    }
}

impl Channel {
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() {
            self.kind.name().to_string()
        } else {
            self.name.trim().to_string()
        }
    }
}

/// `[notifications]` in config.toml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// A summary after every successful backup.
    pub on_success: bool,
    /// Alerts when a backup failed, was incomplete or interrupted, or needs the passphrase.
    pub on_problem: bool,
    /// Also when a backup was skipped because the destination was not connected.
    pub on_skipped: bool,
    /// Alert when the destination has less than this share free (0 = off).
    pub low_space_percent: u8,
    /// Alert when the destination has less than this many GB free (0 = off).
    pub low_space_gb: u64,
    /// Alert when an enabled job has had no successful backup for more than
    /// this many days (0 = off). Checked in the background.
    pub overdue_days: u32,
    /// A message when a new version of AeternaVault is out (once per version).
    pub on_update: bool,
    /// Templates; empty means the built-in text in the interface language.
    pub success_title: String,
    pub success_message: String,
    pub problem_title: String,
    pub problem_message: String,
    pub low_space_title: String,
    pub low_space_message: String,
    #[serde(rename = "channel", skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<Channel>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            on_success: true,
            on_problem: true,
            on_skipped: false,
            low_space_percent: 10,
            low_space_gb: 0,
            overdue_days: 3,
            on_update: true,
            success_title: String::new(),
            success_message: String::new(),
            problem_title: String::new(),
            problem_message: String::new(),
            low_space_title: String::new(),
            low_space_message: String::new(),
            channels: Vec::new(),
        }
    }
}

impl Notifications {
    pub fn active(&self) -> bool {
        self.channels.iter().any(|c| c.enabled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
}

/// A rendered notification.
#[derive(Debug, Clone)]
pub struct Message {
    pub title: String,
    pub body: String,
    pub severity: Severity,
    /// A file for e-mail (name, content); other channels get only the text.
    pub attachment: Option<(String, Vec<u8>)>,
}

/// Facts about one backup run, for the templates.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub job: String,
    pub status: String,
    pub backup: String,
    pub files: u64,
    pub copied: u64,
    pub size: u64,
    pub failed: u64,
    pub duration: Duration,
    pub computer: String,
    pub destination: String,
    pub free: Option<u64>,
    pub free_percent: Option<f64>,
    pub message: String,
    /// Health of the destination drive (`Good · 46 °C · 708 h`), when
    /// drive health is switched on and known.
    pub drive: String,
}

impl Facts {
    pub fn render(&self, template: &str, lang: Lang) -> String {
        let free = self
            .free
            .map(|f| lang.bytes(f))
            .unwrap_or_else(|| "?".into());
        let percent = self
            .free_percent
            .map(|p| format!("{p:.0} %"))
            .unwrap_or_else(|| "?".into());
        let secs = self.duration.as_secs();
        let duration = if secs >= 3600 {
            format!("{} h {} min", secs / 3600, secs % 3600 / 60)
        } else if secs >= 60 {
            format!("{} min {} s", secs / 60, secs % 60)
        } else {
            format!("{secs} s")
        };
        template
            .replace("{job}", &self.job)
            .replace("{status}", &self.status)
            .replace("{backup}", &self.backup)
            .replace("{files}", &lang.count(self.files))
            .replace("{copied}", &lang.count(self.copied))
            .replace("{size}", &lang.bytes(self.size))
            .replace("{failed}", &lang.count(self.failed))
            .replace("{duration}", &duration)
            .replace("{computer}", &self.computer)
            .replace("{destination}", &self.destination)
            .replace("{free}", &free)
            .replace("{free_percent}", &percent)
            .replace("{message}", &self.message)
            .replace("{drive}", &self.drive)
    }
}

fn pick<'a>(custom: &'a str, default: &'a str) -> &'a str {
    if custom.trim().is_empty() {
        default
    } else {
        custom
    }
}

/// Facts from a finished job (window, background, command line).
pub fn facts_for_run(
    config: &Config,
    run: &AutomaticRun,
    report: Option<&BackupReport>,
    lang: Lang,
) -> Facts {
    let destination = &config.destination;
    let (free, total) = platform::disk_space(destination).unzip();
    let stats = report.map(BackupReport::total_stats).unwrap_or_default();
    Facts {
        job: run.schedule.clone(),
        status: lang.notify_status(run.outcome).to_string(),
        backup: report
            .and_then(|r| r.snapshot_dir.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        files: run.files,
        copied: stats.copied_files,
        size: run.bytes,
        failed: stats.failed,
        duration: report.map(|r| r.duration).unwrap_or_default(),
        computer: platform::computer_name(),
        destination: destination.display().to_string(),
        free,
        free_percent: match (free, total) {
            (Some(f), Some(t)) if t > 0 => Some(f as f64 * 100.0 / t as f64),
            _ => None,
        },
        message: run_message(config, run, lang),
        drive: if config.drive_health_on() {
            crate::platform::drive_health::read(destination)
                .map(|h| h.summary(lang == Lang::En))
                .unwrap_or_default()
        } else {
            String::new()
        },
    }
}

/// What went wrong, for `{message}`: item by item with the reason and what
/// to do. With encryption on, a file or folder named by the backup may belong
/// to an encrypted folder; it must not leave the computer, so those are only
/// counted. An error that stopped the run is explained in the user's language.
fn run_message(config: &Config, run: &AutomaticRun, lang: Lang) -> String {
    let hide = config.encryption.enabled;
    if !run.problems.is_empty() {
        return lang.problem_report(&run.problems, hide);
    }
    if run.message.is_empty() {
        String::new()
    } else if hide {
        lang.t().notify_message_hidden.to_string()
    } else {
        run.message.clone()
    }
}

/// The messages a finished run causes, by the configured rules.
pub fn messages_for_run(
    settings: &Notifications,
    facts: &Facts,
    outcome: AutomaticOutcome,
    lang: Lang,
) -> Vec<Message> {
    let t = lang.t();
    let mut out = Vec::new();
    let problem = matches!(
        outcome,
        AutomaticOutcome::CompleteWithNotes
            | AutomaticOutcome::NeedsPassphrase
            | AutomaticOutcome::Interrupted
            | AutomaticOutcome::Failed
    );
    let wanted = match outcome {
        AutomaticOutcome::Complete => settings.on_success,
        AutomaticOutcome::DestinationUnavailable => settings.on_skipped,
        AutomaticOutcome::AlreadyRunning => false,
        _ => settings.on_problem,
    };
    if wanted {
        let (title, body) = if outcome == AutomaticOutcome::Complete {
            (
                pick(&settings.success_title, t.notify_success_title),
                pick(&settings.success_message, t.notify_success_message),
            )
        } else {
            (
                pick(&settings.problem_title, t.notify_problem_title),
                pick(&settings.problem_message, t.notify_problem_message),
            )
        };
        let mut text = facts.render(body, lang);
        // The drive's health goes with every report while it is watched.
        if !facts.drive.is_empty() && !body.contains("{drive}") {
            let label = if lang == Lang::En {
                "Drive"
            } else {
                "Laufwerk"
            };
            text.push_str(&format!("\n{label}: {}", facts.drive));
        }
        out.push(Message {
            title: facts.render(title, lang),
            body: text,
            severity: if problem {
                Severity::Warning
            } else {
                Severity::Info
            },
            attachment: None,
        });
    }
    if low_space(settings, facts) {
        out.push(Message {
            title: facts.render(
                pick(&settings.low_space_title, t.notify_low_space_title),
                lang,
            ),
            body: facts.render(
                pick(&settings.low_space_message, t.notify_low_space_message),
                lang,
            ),
            severity: Severity::Warning,
            attachment: None,
        });
    }
    out
}

/// Whether the destination is below one of the configured thresholds.
pub fn low_space(settings: &Notifications, facts: &Facts) -> bool {
    let by_percent = settings.low_space_percent > 0
        && facts
            .free_percent
            .is_some_and(|p| p < f64::from(settings.low_space_percent));
    let by_size = settings.low_space_gb > 0
        && facts
            .free
            .is_some_and(|f| f < settings.low_space_gb * 1024 * 1024 * 1024);
    by_percent || by_size
}

/// Sends the notifications for a finished run to every enabled channel.
/// Blocking (network); call it from a background thread in the window.
pub fn after_run(config: &Config, run: &AutomaticRun, report: Option<&BackupReport>) {
    let settings = &config.notifications;
    if !config.notifications_on() || !settings.active() {
        return;
    }
    let lang = Lang::resolve(config.language);
    let facts = facts_for_run(config, run, report, lang);
    for message in messages_for_run(settings, &facts, run.outcome, lang) {
        for channel in settings.channels.iter().filter(|c| c.enabled) {
            match send(channel, &message) {
                Ok(()) => tracing::info!("notification sent via {}", channel.label()),
                Err(err) => tracing::warn!("notification via {} failed: {err}", channel.label()),
            }
        }
    }
}

/// The message about the destination drive when its health became worse
/// than last reported; `None` when it is good, unchanged or not readable.
pub fn drive_health_message(
    previous: Option<crate::platform::drive_health::Status>,
    health: &crate::platform::drive_health::DriveHealth,
    destination: &std::path::Path,
    lang: Lang,
) -> Option<Message> {
    use crate::platform::drive_health::Status;
    if health.status == Status::Good || previous.is_some_and(|p| p >= health.status) {
        return None;
    }
    let en = lang == Lang::En;
    let computer = platform::computer_name();
    let findings = if health.findings.is_empty() {
        String::new()
    } else {
        format!("\n- {}", health.findings.join("\n- "))
    };
    let advice = match (health.status, en) {
        (Status::Failing, true) => "Copy the backups to another drive soon and replace this one.",
        (Status::Failing, false) => {
            "Kopiere die Sicherungen bald auf ein anderes Laufwerk und ersetze dieses."
        }
        (_, true) => "Keep an eye on it and keep a second copy of the backups.",
        (_, false) => "Behalte es im Blick und halte eine zweite Kopie der Sicherungen bereit.",
    };
    let (title, body) = if en {
        (
            format!(
                "Drive health: {} {}",
                health.model,
                health.status_name(true)
            ),
            format!(
                "{computer}: the drive with the backups at {} reports: {}.{findings}\n{advice}",
                destination.display(),
                health.summary(true)
            ),
        )
    } else {
        (
            format!(
                "Laufwerkszustand: {} {}",
                health.model,
                health.status_name(false)
            ),
            format!(
                "{computer}: Das Laufwerk mit den Sicherungen unter {} meldet: {}.{findings}\n{advice}",
                destination.display(),
                health.summary(false)
            ),
        )
    };
    Some(Message {
        title,
        body,
        severity: Severity::Warning,
        attachment: None,
    })
}

/// The message that a new version is out.
pub fn update_message(version: &str, lang: Lang) -> Message {
    let computer = platform::computer_name();
    let current = crate::update::CURRENT_VERSION;
    let (title, body) = if lang == Lang::En {
        (
            format!("AeternaVault {version} is available"),
            format!(
                "{computer}: AeternaVault {version} is available; {current} is installed.\nInstall it in Settings → Updates or with `aeternavault-cli update`. Settings and backups are kept.\nhttps://github.com/Benett53/AeternaVault/releases/tag/v{version}"
            ),
        )
    } else {
        (
            format!("AeternaVault {version} ist verfügbar"),
            format!(
                "{computer}: AeternaVault {version} ist verfügbar; installiert ist {current}.\nInstallieren unter Einstellungen → Updates oder mit `aeternavault-cli update`. Einstellungen und Sicherungen bleiben erhalten.\nhttps://github.com/Benett53/AeternaVault/releases/tag/v{version}"
            ),
        )
    };
    Message {
        title,
        body,
        severity: Severity::Info,
        attachment: None,
    }
}

/// Sends the message about a new version once per version (remembered in
/// `state.json`). Blocking; call it from a background thread.
pub fn update_available(config_file: &std::path::Path, config: &Config, version: &str) {
    let settings = &config.notifications;
    if !config.notifications_on() || !settings.active() || !settings.on_update {
        return;
    }
    if crate::state::State::load(config_file)
        .update_notified
        .as_deref()
        == Some(version)
    {
        return;
    }
    let message = update_message(version, Lang::resolve(config.language));
    for channel in settings.channels.iter().filter(|c| c.enabled) {
        match send(channel, &message) {
            Ok(()) => tracing::info!("update notification sent via {}", channel.label()),
            Err(err) => tracing::warn!("notification via {} failed: {err}", channel.label()),
        }
    }
    crate::state::State::update(config_file, |s| {
        s.update_notified = Some(version.to_string());
    });
}

/// After a backup: reads the health of the destination drive and sends a
/// message when it became worse. Blocking; call it from a background thread.
pub fn check_drive_health(config_file: &std::path::Path, config: &Config) {
    if !config.drive_health_on() || config.destination.as_os_str().is_empty() {
        return;
    }
    let Ok(health) = crate::platform::drive_health::read(&config.destination) else {
        return;
    };
    let key = config.destination.display().to_string();
    let previous = crate::state::State::load(config_file)
        .drive_alerted
        .get(&key)
        .copied();
    if previous == Some(health.status) {
        return;
    }
    let lang = Lang::resolve(config.language);
    if config.notifications_on()
        && config.notifications.active()
        && let Some(message) = drive_health_message(previous, &health, &config.destination, lang)
    {
        for channel in config.notifications.channels.iter().filter(|c| c.enabled) {
            if let Err(err) = send(channel, &message) {
                tracing::warn!("notification via {} failed: {err}", channel.label());
            }
        }
    }
    if health.status != crate::platform::drive_health::Status::Good {
        tracing::warn!(
            "drive health of {}: {}",
            config.destination.display(),
            health.summary(true)
        );
    }
    crate::state::State::update(config_file, |s| {
        s.drive_alerted.insert(key, health.status);
    });
}

/// Enabled jobs without a successful backup for longer than allowed, with the
/// time of their last success. A weekly job gets at least eight days.
pub fn overdue_jobs<'a>(
    config: &'a Config,
    state: &crate::state::State,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(
    &'a crate::config::Schedule,
    Option<chrono::DateTime<chrono::Utc>>,
)> {
    let days = config.notifications.overdue_days;
    if days == 0 {
        return Vec::new();
    }
    config
        .schedules
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|schedule| {
            let entry = state
                .schedules
                .get(&schedule.id)
                .cloned()
                .unwrap_or_default();
            let last = entry.last_success.or_else(|| {
                entry
                    .last_run
                    .as_ref()
                    .filter(|r| r.outcome.is_success())
                    .map(|r| r.at)
            });
            // Counted from the last success, or from when the job was switched on.
            let since = last.or(entry.armed_at)?;
            let allowed = match schedule.frequency {
                crate::config::Frequency::Weekly => days.max(8),
                _ => days,
            };
            (now - since > chrono::Duration::days(i64::from(allowed))).then_some((schedule, last))
        })
        .collect()
}

/// Sends an alert for every overdue job, at most once a day per job.
/// Blocking (network); called by the background service.
pub fn check_overdue(config_file: &std::path::Path, config: &Config) {
    let settings = &config.notifications;
    if !config.notifications_on() || !settings.active() || settings.overdue_days == 0 {
        return;
    }
    let now = chrono::Utc::now();
    let state = crate::state::State::load(config_file);
    let due: Vec<_> = overdue_jobs(config, &state, now)
        .into_iter()
        .filter(|(schedule, _)| {
            state
                .schedules
                .get(&schedule.id)
                .and_then(|s| s.overdue_alerted)
                .is_none_or(|at| now - at > chrono::Duration::hours(23))
        })
        .collect();
    if due.is_empty() {
        return;
    }
    let lang = Lang::resolve(config.language);
    let t = lang.t();
    let computer = platform::computer_name();
    for (schedule, last) in &due {
        let entry = state.schedules.get(&schedule.id);
        let status = entry
            .and_then(|e| e.last_run.as_ref())
            .map(|r| lang.notify_status(r.outcome).to_string())
            .unwrap_or_else(|| t.notify_never.to_string());
        let last = last
            .map(|l| lang.datetime(l.with_timezone(&chrono::Local)))
            .unwrap_or_else(|| t.notify_never.to_string());
        let job = crate::automatic::describe(schedule, config, lang);
        let fill = |text: &str| {
            text.replace("{job}", &job)
                .replace("{computer}", &computer)
                .replace("{days}", &settings.overdue_days.to_string())
                .replace("{last}", &last)
                .replace("{status}", &status)
        };
        let message = Message {
            title: fill(t.notify_overdue_title),
            body: fill(t.notify_overdue_message),
            severity: Severity::Warning,
            attachment: None,
        };
        for channel in settings.channels.iter().filter(|c| c.enabled) {
            match send(channel, &message) {
                Ok(()) => tracing::info!("overdue alert sent via {}", channel.label()),
                Err(err) => tracing::warn!("overdue alert via {} failed: {err}", channel.label()),
            }
        }
    }
    crate::state::State::update(config_file, |state| {
        for (schedule, _) in &due {
            state
                .schedules
                .entry(schedule.id.clone())
                .or_default()
                .overdue_alerted = Some(now);
        }
    });
}

/// Notifications for a backup started in the window. Sent from a background
/// thread; a backup the user cancelled is not reported.
pub fn after_manual_backup(
    config: &Config,
    result: &crate::error::EngineResult<BackupReport>,
    label: &str,
) {
    use crate::engine::manifest::SnapshotStatus;
    use crate::error::EngineError;
    if !config.notifications_on() || !config.notifications.active() {
        return;
    }
    let error = result
        .as_ref()
        .err()
        .map(|err| crate::state::Problems::from_error(err, Lang::resolve(config.language)));
    let (outcome, report, message) = match result {
        Ok(r) => match r.header.status {
            SnapshotStatus::Complete => (AutomaticOutcome::Complete, Some(r), String::new()),
            SnapshotStatus::CompleteWithWarnings => (
                AutomaticOutcome::CompleteWithNotes,
                Some(r),
                crate::automatic::problems_summary(r.warnings.len()),
            ),
            SnapshotStatus::Failed => (AutomaticOutcome::Failed, Some(r), String::new()),
            SnapshotStatus::Cancelled => return,
        },
        Err(EngineError::Cancelled) => return,
        Err(err @ EngineError::DestinationUnavailable(_)) => (
            AutomaticOutcome::DestinationUnavailable,
            None,
            err.to_string(),
        ),
        Err(err) => (AutomaticOutcome::Failed, None, err.to_string()),
    };
    let stats = report.map(BackupReport::total_stats).unwrap_or_default();
    let run = AutomaticRun {
        at: chrono::Utc::now(),
        outcome,
        message,
        files: stats.files,
        bytes: stats.bytes,
        schedule: label.to_string(),
        problems: error
            .or_else(|| report.map(crate::state::Problems::from_backup))
            .unwrap_or_default(),
    };
    let config = config.clone();
    let report = report.cloned();
    std::thread::spawn(move || after_run(&config, &run, report.as_ref()));
}

/// Most chat services limit messages to a few thousand characters.
const CHAT_LIMIT: usize = 3500;

/// Sends an exported log to all enabled channels: e-mail with the CSV file
/// attached, chat services with the newest lines of the text. Returns
/// (channel, error) for every channel that failed.
pub fn send_log(config: &Config, title: &str, text: &str, csv: &str) -> Vec<(String, String)> {
    let mut failures = Vec::new();
    for channel in config.notifications.channels.iter().filter(|c| c.enabled) {
        let message = if channel.kind == ChannelKind::Email {
            Message {
                title: title.to_string(),
                body: text.to_string(),
                severity: Severity::Info,
                attachment: Some((
                    format!(
                        "aeternavault-activity-{}.csv",
                        chrono::Local::now().format("%Y-%m-%d")
                    ),
                    csv.as_bytes().to_vec(),
                )),
            }
        } else {
            Message {
                title: title.to_string(),
                body: newest_lines(text, CHAT_LIMIT),
                severity: Severity::Info,
                attachment: None,
            }
        };
        if let Err(err) = send(channel, &message) {
            tracing::warn!("activity log via {} failed: {err:#}", channel.label());
            failures.push((channel.label(), format!("{err:#}")));
        }
    }
    failures
}

/// The end of `text` that fits into `limit` bytes, cut at a line.
fn newest_lines(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let cut = text[start..].find('\n').map_or(start, |i| start + i + 1);
    format!("…\n{}", &text[cut..])
}

/// A test message for one channel.
pub fn test_message(lang: Lang) -> Message {
    let t = lang.t();
    Message {
        title: t.notify_test_title.to_string(),
        body: t
            .notify_test_message
            .replace("{computer}", &platform::computer_name()),
        severity: Severity::Info,
        attachment: None,
    }
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .http_status_as_error(true)
        .build()
        .into()
}

fn post_json(url: &str, bearer: Option<&str>, body: &serde_json::Value) -> anyhow::Result<()> {
    if url.trim().is_empty() {
        anyhow::bail!("no URL is set");
    }
    let mut request = agent().post(url.trim());
    if let Some(token) = bearer.filter(|t| !t.trim().is_empty()) {
        request = request.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    request.send_json(body)?;
    Ok(())
}

/// Sends one message through one channel.
pub fn send(channel: &Channel, message: &Message) -> anyhow::Result<()> {
    check_transport(channel)?;
    send_unchecked(channel, message)
        .map_err(|err| anyhow::anyhow!(redact(&format!("{err:#}"), channel)))
}

/// Only `localhost` and real loopback addresses; a name like
/// `127.example.com` is an ordinary host.
fn is_loopback(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The host of an `http://…` URL.
fn http_host(url: &str) -> Option<&str> {
    let rest = url.trim().strip_prefix("http://").or_else(|| {
        let lower = url.trim().get(..7)?;
        lower
            .eq_ignore_ascii_case("http://")
            .then(|| &url.trim()[7..])
    })?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    Some(if authority.starts_with('[') {
        authority.split(']').next().map_or(authority, |h| &h[1..])
    } else {
        authority.split(':').next().unwrap_or_default()
    })
}

/// Passwords and tokens are only sent encrypted, except to this computer.
pub fn check_transport(channel: &Channel) -> anyhow::Result<()> {
    // Webhook addresses are secrets themselves (Teams, Slack, Discord, ntfy
    // topics), so every address must use https, except one on this computer.
    if channel.kind.uses_url()
        && let Some(host) = http_host(&channel.url)
        && !is_loopback(host)
    {
        anyhow::bail!("unencrypted http:// addresses are not used; use https://");
    }
    if channel.kind == ChannelKind::Email
        && channel.smtp_security == SmtpSecurity::None
        && !channel.smtp_user.trim().is_empty()
        && !is_loopback(&channel.smtp_host)
    {
        anyhow::bail!("the SMTP password would be sent unencrypted; choose STARTTLS or TLS");
    }
    Ok(())
}

/// Error texts can contain URLs; tokens and passwords never reach the log.
/// Every address in the text keeps only its scheme and host, in whatever
/// form the HTTP library prints it.
pub fn redact(text: &str, channel: &Channel) -> String {
    let mut out = text.to_string();
    let url = channel.url.trim();
    if !url.is_empty() {
        out = out.replace(url, &shorten_url(url));
    }
    out = shorten_urls(&out);
    for secret in [channel.token.trim(), channel.smtp_password.as_str()] {
        if !secret.is_empty() {
            out = out.replace(secret, "***");
        }
    }
    out
}

/// `https://user:pass@host:8443/path?q#f` -> `https://host:8443/***`.
fn shorten_url(url: &str) -> String {
    let Some(start) = url.find("://").map(|i| i + 3) else {
        return "***".into();
    };
    let rest = &url[start..];
    let authority = &rest[..rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len())];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{}{host}/***", &url[..start])
}

fn shorten_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let scheme = rest[..i]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || "+.-".contains(c)))
            .map_or(0, |j| j + 1);
        let end = rest[i..]
            .find(|c: char| c.is_whitespace() || "\"'<>(){}`|".contains(c))
            .map_or(rest.len(), |j| i + j);
        out.push_str(&rest[..scheme]);
        out.push_str(&shorten_url(&rest[scheme..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn send_unchecked(channel: &Channel, message: &Message) -> anyhow::Result<()> {
    let text = format!("{}\n\n{}", message.title, message.body);
    match channel.kind {
        ChannelKind::Ntfy => {
            if channel.url.trim().is_empty() {
                anyhow::bail!("no URL is set");
            }
            let mut request = agent()
                .post(channel.url.trim())
                .header("Title", &ascii_header(&message.title))
                .header(
                    "Tags",
                    if message.severity == Severity::Warning {
                        "warning"
                    } else {
                        "floppy_disk"
                    },
                )
                .header(
                    "Priority",
                    if message.severity == Severity::Warning {
                        "high"
                    } else {
                        "default"
                    },
                );
            if !channel.token.trim().is_empty() {
                request =
                    request.header("Authorization", &format!("Bearer {}", channel.token.trim()));
            }
            request.send(message.body.as_bytes())?;
            Ok(())
        }
        ChannelKind::Telegram => {
            if channel.token.trim().is_empty() || channel.chat_id.trim().is_empty() {
                anyhow::bail!("bot token and chat id are needed");
            }
            let url = format!(
                "https://api.telegram.org/bot{}/sendMessage",
                channel.token.trim()
            );
            agent().post(&url).send_json(serde_json::json!({
                "chat_id": channel.chat_id.trim(),
                "text": text,
                "disable_web_page_preview": true,
            }))?;
            Ok(())
        }
        ChannelKind::Teams => post_json(
            &channel.url,
            None,
            &serde_json::json!({
                "type": "message",
                "attachments": [{
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "content": {
                        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                        "type": "AdaptiveCard",
                        "version": "1.4",
                        "body": [
                            {
                                "type": "TextBlock",
                                "text": message.title,
                                "weight": "Bolder",
                                "size": "Medium",
                                "wrap": true,
                                "color": if message.severity == Severity::Warning { "Attention" } else { "Default" },
                            },
                            { "type": "TextBlock", "text": message.body, "wrap": true },
                        ],
                    },
                }],
            }),
        ),
        ChannelKind::Slack => post_json(&channel.url, None, &serde_json::json!({ "text": text })),
        ChannelKind::Discord => {
            post_json(&channel.url, None, &serde_json::json!({ "content": text }))
        }
        ChannelKind::Gotify => {
            let base = channel.url.trim().trim_end_matches('/');
            if base.is_empty() || channel.token.trim().is_empty() {
                anyhow::bail!("server URL and app token are needed");
            }
            let url = format!("{base}/message?token={}", channel.token.trim());
            agent().post(&url).send_json(serde_json::json!({
                "title": message.title,
                "message": message.body,
                "priority": if message.severity == Severity::Warning { 8 } else { 4 },
            }))?;
            Ok(())
        }
        ChannelKind::Webhook => post_json(
            &channel.url,
            Some(&channel.token),
            &serde_json::json!({
                "source": "AeternaVault",
                "version": env!("CARGO_PKG_VERSION"),
                "computer": platform::computer_name(),
                "severity": if message.severity == Severity::Warning { "warning" } else { "info" },
                "title": message.title,
                "message": message.body,
            }),
        ),
        ChannelKind::Email => send_email(channel, message),
    }
}

/// HTTP headers are ASCII; ntfy also accepts the title in the body otherwise.
fn ascii_header(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn send_email(channel: &Channel, message: &Message) -> anyhow::Result<()> {
    use lettre::message::{Mailbox, header::ContentType};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{SmtpTransport, Transport};

    if channel.smtp_host.trim().is_empty() || channel.to.trim().is_empty() {
        anyhow::bail!("SMTP server and recipient are needed");
    }
    let from_text = if channel.from.trim().is_empty() {
        channel.smtp_user.trim()
    } else {
        channel.from.trim()
    };
    let from: Mailbox = from_text
        .parse()
        .map_err(|e| anyhow::anyhow!("sender address {from_text:?}: {e}"))?;
    let mut builder = lettre::Message::builder()
        .from(from)
        .subject(message.title.clone());
    if message.attachment.is_none() {
        builder = builder.header(ContentType::TEXT_PLAIN);
    }
    for to in channel
        .to
        .split([',', ';'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        builder = builder.to(to
            .parse()
            .map_err(|e| anyhow::anyhow!("recipient {to:?}: {e}"))?);
    }
    let email = match &message.attachment {
        None => builder.body(message.body.clone())?,
        Some((name, content)) => {
            use lettre::message::{Attachment, MultiPart, SinglePart};
            builder.multipart(
                MultiPart::mixed()
                    .singlepart(SinglePart::plain(message.body.clone()))
                    .singlepart(
                        Attachment::new(name.clone())
                            .body(content.clone(), ContentType::parse("text/csv")?),
                    ),
            )?
        }
    };

    let host = channel.smtp_host.trim();
    let mut transport = match channel.smtp_security {
        SmtpSecurity::Starttls => SmtpTransport::starttls_relay(host)?,
        SmtpSecurity::Tls => SmtpTransport::relay(host)?,
        SmtpSecurity::None => SmtpTransport::builder_dangerous(host),
    }
    .port(channel.smtp_port)
    .timeout(Some(Duration::from_secs(30)));
    if !channel.smtp_user.trim().is_empty() {
        transport = transport.credentials(Credentials::new(
            channel.smtp_user.trim().to_string(),
            channel.smtp_password.clone(),
        ));
    }
    transport.build().send(&email)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(problems: crate::state::Problems) -> AutomaticRun {
        AutomaticRun {
            at: chrono::Utc::now(),
            outcome: AutomaticOutcome::CompleteWithNotes,
            message: "short summary".into(),
            files: 338,
            bytes: 0,
            schedule: "Database 20:00".into(),
            problems,
        }
    }

    /// A problem names each affected file with its reason and says what to
    /// do (feedback from a company: "an error, but not which one").
    #[test]
    fn problem_message_names_files_reasons_and_what_to_do() {
        let problems = crate::state::Problems {
            check_unreadable: vec![
                "Data/tasks.db: The process cannot access the file because it is being used by another process. (os error 32)".into(),
            ],
            check_damaged: vec!["Data/orders.db".into()],
            ..Default::default()
        };
        let config = Config::default();
        let text = run_message(&config, &run_with(problems.clone()), Lang::De);
        assert!(
            text.contains("Data/tasks.db: The process cannot access"),
            "{text}"
        );
        assert!(text.contains("• Data/orders.db"), "{text}");
        assert!(
            text.contains("1 beschädigt, 0 fehlen, 1 nicht lesbar"),
            "{text}"
        );
        assert!(text.contains("Virenscanner"), "{text}");
        assert!(text.contains("vollständige Sicherung"), "{text}");
        let english = run_message(&config, &run_with(problems), Lang::En);
        assert!(english.contains("What to do"), "{english}");
    }

    /// With encryption on, files named by the backup itself are only
    /// counted; the reason and what to do are still sent.
    #[test]
    fn encrypted_names_stay_on_the_computer_but_the_advice_is_sent() {
        let problems = crate::state::Problems {
            folders_missing: vec!["Y:\\Data".into()],
            not_saved: vec![
                "Y:\\Data: the folder was not found or is not connected".into(),
                "C:\\Secret\\salaries.xlsx: locked".into(),
            ],
            ..Default::default()
        };
        let mut config = Config::default();
        config.encryption.enabled = true;
        let text = run_message(&config, &run_with(problems), Lang::De);
        assert!(!text.contains("salaries"), "{text}");
        assert!(text.contains("(2)"), "{text}");
        assert!(text.contains("UNC-Pfad"), "{text}");
        assert!(
            text.contains("gesperrt") || text.contains("sperrt"),
            "{text}"
        );
    }

    /// A long list is cut; the rest is in the log.
    #[test]
    fn long_problem_lists_are_cut() {
        let problems = crate::state::Problems {
            check_missing: (0..25).map(|n| format!("Data/{n}.txt")).collect(),
            ..Default::default()
        };
        let text = Lang::En.problem_report(&problems, false);
        assert!(
            text.contains("Data/9.txt") && !text.contains("Data/10.txt"),
            "{text}"
        );
        assert!(text.contains("15 more"), "{text}");
    }

    /// An error that stopped the run is explained, also with encryption on.
    #[test]
    fn stopping_error_is_explained() {
        let error = crate::error::EngineError::SourcesUnavailable("Y:\\Data".into());
        let mut config = Config::default();
        config.encryption.enabled = true;
        let mut run = run_with(crate::state::Problems::from_error(&error, Lang::De));
        run.outcome = AutomaticOutcome::Failed;
        let text = run_message(&config, &run, Lang::De);
        assert!(text.contains("Netzwerkfreigabe"), "{text}");
    }

    fn facts() -> Facts {
        Facts {
            job: "Every evening".into(),
            status: "completed".into(),
            backup: "2026-10-28 20-00".into(),
            files: 1234,
            copied: 12,
            size: 5_000_000_000,
            duration: Duration::from_secs(125),
            computer: "PC".into(),
            destination: "E:\\Backups".into(),
            free: Some(40_000_000_000),
            free_percent: Some(8.0),
            ..Facts::default()
        }
    }

    #[test]
    fn templates_are_filled() {
        let text = facts().render(
            "{job}: {status}, {files} files ({copied} new), {duration}, {free} free",
            Lang::En,
        );
        assert_eq!(
            text,
            "Every evening: completed, 1,234 files (12 new), 2 min 5 s, 37.3 GB free"
        );
    }

    #[test]
    fn rules_decide_what_is_sent() {
        let mut settings = Notifications::default();
        let lang = Lang::En;
        // Success summary plus low-space alert (8 % < 10 %).
        let sent = messages_for_run(&settings, &facts(), AutomaticOutcome::Complete, lang);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].severity, Severity::Warning);
        // Without success messages and with enough space: nothing.
        settings.on_success = false;
        settings.low_space_percent = 5;
        assert!(messages_for_run(&settings, &facts(), AutomaticOutcome::Complete, lang).is_empty());
        // Problems are reported by default, skipped runs are not.
        assert_eq!(
            messages_for_run(&settings, &facts(), AutomaticOutcome::Failed, lang).len(),
            1
        );
        assert!(
            messages_for_run(
                &settings,
                &facts(),
                AutomaticOutcome::DestinationUnavailable,
                lang
            )
            .is_empty()
        );
        // Custom template.
        settings.problem_title = "ALERT {computer}".into();
        let sent = messages_for_run(&settings, &facts(), AutomaticOutcome::Interrupted, lang);
        assert_eq!(sent[0].title, "ALERT PC");
        // Threshold in GB.
        settings.low_space_gb = 50;
        assert!(low_space(&settings, &facts()));
    }

    #[test]
    fn reports_name_the_drive_health_and_new_versions() {
        let settings = Notifications::default();
        let mut f = facts();
        f.drive = "Good · 38 °C · 9120 h".into();
        let sent = messages_for_run(&settings, &f, AutomaticOutcome::Complete, Lang::En);
        assert!(
            sent[0].body.ends_with("\nDrive: Good · 38 °C · 9120 h"),
            "{}",
            sent[0].body
        );
        let quiet = messages_for_run(&settings, &facts(), AutomaticOutcome::Complete, Lang::En);
        assert!(!quiet[0].body.contains("Drive:"));

        let update = update_message("9.9.9", Lang::De);
        assert_eq!(update.title, "AeternaVault 9.9.9 ist verfügbar");
        assert!(update.body.contains("aeternavault-cli update"));
        assert!(Notifications::default().on_update);
    }

    #[test]
    fn secrets_only_travel_encrypted() {
        let mut c = Channel {
            kind: ChannelKind::Webhook,
            url: "http://hooks.example.com/x".into(),
            token: "secret-token".into(),
            ..Channel::default()
        };
        assert!(check_transport(&c).is_err());
        c.url = "https://hooks.example.com/x".into();
        assert!(check_transport(&c).is_ok());
        c.url = "http://127.0.0.1:8080/x".into();
        assert!(check_transport(&c).is_ok());
        c.url = "http://127.example.test/x".into();
        assert!(check_transport(&c).is_err());
        c.url = "http://[::1]:8080/x".into();
        assert!(check_transport(&c).is_ok());
        let secret_url = Channel {
            kind: ChannelKind::Slack,
            url: "http://hooks.slack.com/services/T0/B0/XXXX".into(),
            ..Channel::default()
        };
        assert!(check_transport(&secret_url).is_err());
        c.url = "http://user@evil.example:80/x".into();
        assert!(check_transport(&c).is_err());
        let mail = Channel {
            kind: ChannelKind::Email,
            smtp_host: "smtp.example.com".into(),
            smtp_security: SmtpSecurity::None,
            smtp_user: "a@example.com".into(),
            smtp_password: "pw123456".into(),
            ..Channel::default()
        };
        assert!(check_transport(&mail).is_err());
        assert_eq!(
            redact(
                "failed: https://api.telegram.org/botsecret-token/send pw123456",
                &Channel {
                    token: "secret-token".into(),
                    smtp_password: "pw123456".into(),
                    ..Channel::default()
                }
            ),
            "failed: https://api.telegram.org/*** ***"
        );
    }

    #[test]
    fn addresses_in_errors_lose_their_secrets() {
        let channel = |url: &str| Channel {
            kind: ChannelKind::Webhook,
            url: url.into(),
            ..Channel::default()
        };
        for (url, printed, expected) in [
            (
                "https://notify.invalid?token=DUMMY_TOKEN",
                "https://notify.invalid/?token=DUMMY_TOKEN",
                "https://notify.invalid/***",
            ),
            (
                "https://u:DUMMY_PASS@notify.invalid/hook",
                "https://u:DUMMY_PASS@notify.invalid/hook",
                "https://notify.invalid/***",
            ),
            (
                "https://Notify.Invalid/a/DUMMY_PATH?x=DUMMY_QUERY#DUMMY_FRAGMENT",
                "https://notify.invalid/a/DUMMY_PATH?x=DUMMY_QUERY#DUMMY_FRAGMENT",
                "https://notify.invalid/***",
            ),
            (
                "https://notify.invalid/hook?key=DUMMY%20KEY",
                "https://notify.invalid/hook?key=DUMMY%20KEY",
                "https://notify.invalid/***",
            ),
            (
                "http://[::1]:8080/DUMMY_PATH",
                "http://[::1]:8080/DUMMY_PATH",
                "http://[::1]:8080/***",
            ),
        ] {
            let text = format!("https://notify.invalid: Connection refused for url ({printed}): x");
            let out = redact(&text, &channel(url));
            assert!(!out.contains("DUMMY"), "{url}: {out}");
            assert!(out.contains(expected), "{url}: {out}");
        }
    }

    #[test]
    fn overdue_jobs_are_found() {
        use crate::config::{Frequency, Schedule};
        use crate::state::{ScheduleState, State};
        let now = chrono::Utc::now();
        let days = |n: i64| now - chrono::Duration::days(n);
        let mut config = Config::default();
        for (id, frequency) in [
            ("daily", Frequency::Daily),
            ("weekly", Frequency::Weekly),
            ("fresh", Frequency::Daily),
            ("off", Frequency::Daily),
        ] {
            config.schedules.push(Schedule {
                id: id.into(),
                frequency,
                enabled: id != "off",
                ..Schedule::default()
            });
        }
        let mut state = State::default();
        let ok = |at| AutomaticRun {
            at,
            outcome: AutomaticOutcome::Complete,
            message: String::new(),
            files: 0,
            bytes: 0,
            schedule: String::new(),
            problems: crate::state::Problems::default(),
        };
        let failed = AutomaticRun {
            outcome: AutomaticOutcome::Failed,
            ..ok(days(0))
        };
        state.schedules.insert(
            "daily".into(),
            ScheduleState {
                armed_at: Some(days(30)),
                last_success: Some(days(4)),
                last_run: Some(failed),
                ..ScheduleState::default()
            },
        );
        state.schedules.insert(
            "weekly".into(),
            ScheduleState {
                armed_at: Some(days(30)),
                last_run: Some(ok(days(5))),
                ..ScheduleState::default()
            },
        );
        state.schedules.insert(
            "fresh".into(),
            ScheduleState {
                armed_at: Some(days(1)),
                ..ScheduleState::default()
            },
        );
        state.schedules.insert(
            "off".into(),
            ScheduleState {
                armed_at: Some(days(30)),
                ..ScheduleState::default()
            },
        );
        let ids = |config: &Config| -> Vec<String> {
            overdue_jobs(config, &state, now)
                .into_iter()
                .map(|(s, _)| s.id.clone())
                .collect()
        };
        assert_eq!(ids(&config), vec!["daily".to_string()]);
        config.notifications.overdue_days = 0;
        assert!(ids(&config).is_empty());
    }

    #[test]
    fn channels_roundtrip_in_toml() {
        let mut config = Config::default();
        config.notifications.channels.push(Channel {
            name: "Ops".into(),
            kind: ChannelKind::Teams,
            url: "https://example.com/hook".into(),
            ..Channel::default()
        });
        let text = config.to_toml().unwrap();
        assert!(text.contains("[[notifications.channel]]"));
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.notifications, config.notifications);
    }

    #[test]
    fn drive_health_is_reported_when_it_gets_worse() {
        use crate::platform::drive_health::{DriveHealth, Status};
        let mut health = DriveHealth {
            model: "WDC WD40EFRX".into(),
            status: Status::Good,
            temperature_c: Some(35),
            power_on_hours: Some(10_000),
            findings: Vec::new(),
            source: "udisks".into(),
        };
        let at = std::path::Path::new("/mnt/backup");
        assert!(drive_health_message(None, &health, at, Lang::En).is_none());
        health.status = Status::Warning;
        health.findings = vec!["8 bad sectors".into()];
        let message = drive_health_message(Some(Status::Good), &health, at, Lang::En).unwrap();
        assert!(
            message.title.contains("WDC WD40EFRX Warning"),
            "{}",
            message.title
        );
        assert!(message.body.contains("8 bad sectors"));
        assert!(message.body.contains("/mnt/backup"));
        assert!(
            drive_health_message(Some(Status::Warning), &health, at, Lang::En).is_none(),
            "told already"
        );
        health.status = Status::Failing;
        let message = drive_health_message(Some(Status::Warning), &health, at, Lang::De).unwrap();
        assert!(message.body.contains("ersetze"));
    }
}
