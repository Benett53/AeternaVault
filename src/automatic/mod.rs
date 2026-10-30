//! Automatic backups.
//!
//! AeternaVault runs its schedules itself instead of registering tasks in the
//! Windows Task Scheduler (as 0.2 did):
//!
//! * several schedules, each with its own folders, managed in one place;
//! * nothing is left behind in system tools, and nothing runs when the user
//!   has quit AeternaVault;
//! * an unlocked vault can be used even if its key is not remembered.
//!
//! When the window is closed while schedules are on, AeternaVault keeps
//! running in the notification area. "Start with Windows" adds an entry to
//! the user's autostart list (`--background`).
//!
//! * [`timing`] — when a schedule is due (pure functions)
//! * [`service`] — the background thread that runs due schedules
//! * [`run_unattended`] — one backup without any window, shared with the CLI

pub mod service;
pub mod timing;

use chrono::Utc;

use crate::config::{Config, Frequency, Schedule};
use crate::engine::backup::{self, BackupReport};
use crate::engine::crypto::VaultKey;
use crate::engine::manifest::SnapshotStatus;
use crate::engine::plan::{self, BackupInput};
use crate::engine::sources::Sources;
use crate::engine::{CancelToken, Progress, vault};
use crate::error::EngineError;
use crate::i18n::Lang;
use crate::paths::AppPaths;
use crate::platform::known_paths::KnownPaths;
use crate::platform::{self, vss::LiveFiles};
use crate::state::{self, AutomaticOutcome, AutomaticRun};

pub const PASSPHRASE_ENV: &str = "AETERNAVAULT_PASSPHRASE";

/// The vault key from the remembered key file or `AETERNAVAULT_PASSPHRASE`.
pub fn unattended_key(paths: &AppPaths, config: &Config) -> Option<VaultKey> {
    let header = vault::read_header(&config.destination).ok()?;
    if let Some(key) = vault::remembered_key(&state::key_dir(&paths.config_file), &header) {
        return Some(key);
    }
    let passphrase = std::env::var(PASSPHRASE_ENV).ok()?;
    vault::unlock(&config.destination, &passphrase)
        .ok()
        .map(|(_, key)| key)
}

/// Plans and runs one backup without asking anything. `key` is an already
/// unlocked vault key (from the window); otherwise the remembered key is used.
pub fn run_unattended(
    paths: &AppPaths,
    config: &Config,
    key: Option<VaultKey>,
    label: &str,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> (AutomaticRun, Option<BackupReport>) {
    let sources = Sources::collect(config, &KnownPaths::current());
    let key = if config.encryption.enabled {
        key.or_else(|| unattended_key(paths, config))
    } else {
        None
    };
    let computer = platform::computer_name();
    let finished = |outcome, message: String, files, bytes| AutomaticRun {
        at: Utc::now(),
        outcome,
        message,
        files,
        bytes,
        schedule: label.to_string(),
    };

    let input = BackupInput {
        config,
        sources: &sources,
        computer: &computer,
        key: key.as_ref(),
    };
    let plan = match plan::plan_backup(&input, cancel, on_progress) {
        Ok(plan) => plan,
        Err(err) => {
            let outcome = outcome_for(&err);
            if outcome == AutomaticOutcome::DestinationUnavailable {
                // An unplugged external drive is expected, not an error.
                tracing::info!("automatic backup skipped: {err}");
            } else {
                tracing::warn!("automatic backup could not be planned: {err}");
            }
            return (finished(outcome, err.to_string(), 0, 0), None);
        }
    };

    match backup::run_backup(&plan, key.as_ref(), &LiveFiles, cancel, on_progress) {
        Ok(report) if report.header.status == SnapshotStatus::Cancelled => {
            // Not a success: the job is made up later, and old backups stay.
            tracing::info!("automatic backup interrupted");
            let total = report.total_stats();
            (
                finished(
                    AutomaticOutcome::Interrupted,
                    String::new(),
                    total.files,
                    total.bytes,
                ),
                Some(report),
            )
        }
        Ok(report) => {
            let outcome = if report.header.status == SnapshotStatus::Complete {
                AutomaticOutcome::Complete
            } else {
                AutomaticOutcome::CompleteWithNotes
            };
            let total = report.total_stats();
            let run = finished(
                outcome,
                report.warnings.first().cloned().unwrap_or_default(),
                total.files,
                total.bytes,
            );
            match apply_retention(
                &config.destination,
                &config.retention,
                &computer,
                key.as_ref(),
                cancel,
            ) {
                Some(Err(err)) => tracing::warn!("old backups could not be removed: {err}"),
                Some(Ok(removed)) => {
                    tracing::info!("{} old backups removed", removed.deleted.len());
                    record_retention(&paths.config_file, &removed);
                }
                None => {}
            }
            (run, Some(report))
        }
        Err(err) => {
            tracing::error!("automatic backup failed: {err}");
            (finished(outcome_for(&err), err.to_string(), 0, 0), None)
        }
    }
}

/// Removes old backups of this computer by the retention rules. `None` if the
/// rules are off or nothing is to be removed. Encrypted backups are only
/// removed with a key.
pub fn apply_retention(
    destination: &std::path::Path,
    policy: &crate::config::Retention,
    computer: &str,
    key: Option<&VaultKey>,
    cancel: &CancelToken,
) -> Option<Result<crate::engine::manage::DeleteReport, EngineError>> {
    if !policy.enabled {
        return None;
    }
    let all = crate::engine::snapshots::list(destination, key).ok()?;
    let ids: Vec<String> = crate::engine::retention::to_remove(&all, policy, computer)
        .into_iter()
        .filter(|id| {
            key.is_some()
                || all
                    .iter()
                    .find(|s| &s.qualified_id() == id)
                    .is_some_and(|s| !s.needs_key())
        })
        .collect();
    if ids.is_empty() {
        return None;
    }
    tracing::info!("removing {} old backups by the retention rules", ids.len());
    Some(crate::engine::manage::delete(
        destination,
        &ids,
        key,
        cancel,
        &mut |_| {},
    ))
}

/// Notes removed old backups in the activity history.
pub fn record_retention(
    config_file: &std::path::Path,
    report: &crate::engine::manage::DeleteReport,
) {
    if report.deleted.is_empty() {
        return;
    }
    crate::history::record(
        config_file,
        crate::history::Event::BackupsDeleted {
            snapshots: report.deleted.clone(),
            by_rules: true,
            message: String::new(),
        },
    );
}

/// Notes an automatic backup in the activity history.
pub fn record_run(
    config_file: &std::path::Path,
    run: &AutomaticRun,
    report: Option<&BackupReport>,
    command_line: bool,
) {
    crate::history::record(
        config_file,
        crate::history::Event::Backup {
            job: run.schedule.clone(),
            command_line,
            outcome: crate::history::Outcome::from_automatic(run.outcome),
            files: run.files,
            bytes: run.bytes,
            snapshot: report
                .and_then(|r| r.snapshot_dir.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            message: run.message.clone(),
        },
    );
}

/// The name shown for a job run: the job's name or description, or "Back up now".
pub fn job_label(schedule: Option<&Schedule>, config: &Config, lang: Lang) -> String {
    match schedule {
        Some(s) => describe(s, config, lang),
        None => lang.t().back_up_now.to_string(),
    }
}

/// Runs one job (or, with `None`, everything that is ticked) without asking
/// anything, checks the result if the job asks for it, and records the outcome
/// in `state.json`, the activity history and the log. Used by the background
/// service of the window and by `aeternavault-cli`.
#[allow(clippy::too_many_arguments)]
pub fn run_job(
    paths: &AppPaths,
    config: &Config,
    schedule: Option<&Schedule>,
    key: Option<VaultKey>,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
    command_line: bool,
) -> (AutomaticRun, Option<BackupReport>) {
    let lang = Lang::resolve(config.language);
    let label = job_label(schedule, config, lang);
    let run_config = match schedule {
        Some(s) => config.for_schedule(s),
        None => config.clone(),
    };
    if let Some(s) = schedule {
        let id = s.id.clone();
        state::State::update(&paths.config_file, |state| {
            state.schedules.entry(id).or_default().last_attempt = Some(Utc::now());
        });
    }
    tracing::info!("backup job started: {label}");

    let key_for_check = key.clone();
    let before = schedule
        .map(|s| s.before_command.trim())
        .unwrap_or_default();
    let (mut run, report) = match run_before_command(before, cancel) {
        Err(message) => (
            AutomaticRun {
                at: Utc::now(),
                outcome: AutomaticOutcome::Failed,
                message,
                files: 0,
                bytes: 0,
                schedule: label.clone(),
            },
            None,
        ),
        Ok(()) => run_unattended(paths, &run_config, key, &label, cancel, on_progress),
    };
    if let (Some(s), Some(report)) = (schedule, &report)
        && s.verify_after
        && run.outcome.is_success()
        && !cancel.is_cancelled()
    {
        let key = key_for_check.or_else(|| unattended_key(paths, &run_config));
        check_backup(paths, &run_config, report, key.as_ref(), cancel, &mut run);
    }

    let schedule_id = schedule.map(|s| s.id.clone());
    let mut record = run.outcome != AutomaticOutcome::AlreadyRunning;
    state::State::update(&paths.config_file, |state| {
        let repeated_skip = run.outcome == AutomaticOutcome::DestinationUnavailable
            && state
                .last_automatic
                .as_ref()
                .is_some_and(|last| last.outcome == run.outcome);
        // An unplugged drive is retried every few minutes; note it only once.
        record &= !repeated_skip;
        if !repeated_skip {
            state.last_automatic = Some(run.clone());
        }
        if let Some(id) = schedule_id {
            state.schedules.entry(id).or_default().last_run = Some(run.clone());
        }
    });
    if record {
        record_run(&paths.config_file, &run, report.as_ref(), command_line);
        crate::notify::after_run(config, &run, report.as_ref());
    }
    match run.outcome {
        AutomaticOutcome::Complete => tracing::info!(
            files = run.files,
            bytes = run.bytes,
            "backup job completed successfully: {label}"
        ),
        AutomaticOutcome::CompleteWithNotes => tracing::warn!(
            files = run.files,
            "backup job completed with notes: {label}: {}",
            run.message
        ),
        AutomaticOutcome::DestinationUnavailable => {
            tracing::info!("backup job skipped, destination not connected: {label}")
        }
        other => tracing::error!(outcome = ?other, "backup job failed: {label}: {}", run.message),
    }
    (run, report)
}

/// The jobs that are due now. `since_start` is when "at start" jobs count from
/// (the start of the window, or of the computer for timers).
pub fn due_jobs(
    config: &Config,
    state: &state::State,
    since_start: chrono::DateTime<chrono::Local>,
) -> Vec<Schedule> {
    let now = chrono::Local::now();
    config
        .schedules
        .iter()
        .filter(|s| {
            let entry = state.schedules.get(&s.id).cloned().unwrap_or_default();
            timing::is_due(s, &entry, now, since_start)
        })
        .cloned()
        .collect()
}

/// Runs every due job once, one after the other (used by timers and
/// `aeternavault-cli jobs run-due`). Returns the results.
pub fn run_due(
    paths: &AppPaths,
    config: &Config,
    key: Option<VaultKey>,
    since_start: chrono::DateTime<chrono::Local>,
) -> Vec<AutomaticRun> {
    // Jobs without bookkeeping start counting from now (as in the window).
    let state = state::State::update(&paths.config_file, |state| {
        for schedule in &config.schedules {
            let entry = state.schedules.entry(schedule.id.clone()).or_default();
            if entry.armed_at.is_none() {
                entry.armed_at = Some(Utc::now());
            }
        }
    });
    let due = due_jobs(config, &state, since_start);
    if due.is_empty() {
        tracing::debug!("no backup job is due");
        return Vec::new();
    }
    if config.background.only_on_ac_power && platform::on_battery() {
        tracing::info!("backup jobs wait for mains power");
        return Vec::new();
    }
    let cancel = CancelToken::default();
    due.iter()
        .map(|s| {
            run_job(
                paths,
                config,
                Some(s),
                key.clone(),
                &cancel,
                &mut |_| {},
                true,
            )
            .0
        })
        .collect()
}

/// How far the retention forecast looks ahead.
const FORECAST_YEARS: i64 = 2;
const FORECAST_MAX_BACKUPS: usize = 4000;

/// Times of future backups by the switched-on jobs, oldest first. Without such
/// jobs (or with "at start" jobs only), one backup a day is assumed; the flag
/// says so.
pub fn future_backup_times(config: &Config) -> (Vec<chrono::DateTime<Utc>>, bool) {
    use chrono::{Duration, Local};
    let now = Local::now();
    let end = now + Duration::days(365 * FORECAST_YEARS);
    let mut times = Vec::new();
    let mut daily = false;
    for schedule in config.schedules.iter().filter(|s| s.enabled) {
        if schedule.frequency == Frequency::AtStart {
            daily = true;
            continue;
        }
        let mut cursor = now;
        while let Some(next) = timing::next_occurrence(schedule, cursor) {
            if next > end || times.len() > FORECAST_MAX_BACKUPS {
                break;
            }
            times.push(next.with_timezone(&Utc));
            cursor = next + Duration::minutes(1);
        }
    }
    let assumed = times.is_empty() && !daily;
    if times.is_empty() {
        daily = true;
    }
    if daily {
        let mut day = now + Duration::days(1);
        while day <= end {
            times.push(day.with_timezone(&Utc));
            day += Duration::days(1);
        }
    }
    times.sort();
    times.dedup();
    times.truncate(FORECAST_MAX_BACKUPS);
    (times, assumed)
}

/// When the retention rules will remove each backup (only while they are on).
pub fn removal_dates(
    config: &Config,
    snapshots: &[crate::engine::snapshots::SnapshotInfo],
    computer: &str,
) -> Vec<(String, crate::engine::retention::Removal)> {
    if !config.retention.enabled {
        return Vec::new();
    }
    let (future, _) = future_backup_times(config);
    crate::engine::retention::forecast(snapshots, &config.retention, computer, &future)
}

/// Reads the new backup again and compares every file with its checksum.
/// Problems turn the run into "completed with notes".
fn check_backup(
    paths: &AppPaths,
    config: &Config,
    report: &BackupReport,
    key: Option<&VaultKey>,
    cancel: &CancelToken,
    run: &mut AutomaticRun,
) {
    let Some(id) = report
        .snapshot_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return;
    };
    let computer = platform::computer_name();
    let result = crate::engine::snapshots::find(&config.destination, &id, &computer, key).and_then(
        |snapshot| crate::engine::verify::verify(&snapshot, key, false, cancel, &mut |_| {}),
    );
    match result {
        Ok(check) if !check.cancelled => {
            tracing::info!(
                damaged = check.damaged.len(),
                missing = check.missing.len(),
                "backup checked: {id}"
            );
            crate::history::record(
                &paths.config_file,
                crate::history::Event::Verified {
                    snapshot: id,
                    files: check.files,
                    damaged: check.damaged.len() as u64,
                    missing: check.missing.len() as u64,
                },
            );
            if !check.is_ok() {
                run.outcome = AutomaticOutcome::CompleteWithNotes;
                run.message = format!(
                    "check found {} damaged and {} missing files",
                    check.damaged.len(),
                    check.missing.len()
                );
            }
        }
        Ok(_) => {}
        Err(err) => tracing::warn!("the backup could not be checked: {err}"),
    }
}

fn outcome_for(err: &EngineError) -> AutomaticOutcome {
    match err {
        EngineError::DestinationUnavailable(_) => AutomaticOutcome::DestinationUnavailable,
        EngineError::Locked | EngineError::EncryptionNotSetUp => AutomaticOutcome::NeedsPassphrase,
        EngineError::AlreadyRunning => AutomaticOutcome::AlreadyRunning,
        _ => AutomaticOutcome::Failed,
    }
}

/// A short description such as "Every day at 20:00 · Documents, Pictures".
pub fn describe(schedule: &Schedule, config: &Config, lang: Lang) -> String {
    if !schedule.name.trim().is_empty() {
        return schedule.name.trim().to_string();
    }
    format!(
        "{} · {}",
        when_text(schedule, lang),
        scope_text(schedule, config, lang)
    )
}

pub fn when_text(schedule: &Schedule, lang: Lang) -> String {
    let t = lang.t();
    let time = schedule.time.trim();
    match schedule.frequency {
        Frequency::Daily => format!("{} {} {time}", t.freq_daily, t.at_time),
        Frequency::Weekly => format!(
            "{} {} {time}",
            t.weekdays_every[schedule.weekday as usize], t.at_time
        ),
        Frequency::Hourly => lang.every_hours_capital(schedule.every_hours),
        Frequency::AtStart => t.freq_at_start.to_string(),
    }
}

pub fn scope_text(schedule: &Schedule, config: &Config, lang: Lang) -> String {
    let t = lang.t();
    if schedule.is_everything() {
        return t.scope_everything.to_string();
    }
    let mut parts: Vec<String> = config
        .sources
        .iter()
        .filter(|s| {
            schedule.all_folders
                || schedule
                    .folders
                    .iter()
                    .any(|f| crate::engine::paths_equal(f, &s.path))
        })
        .map(|s| s.display_name())
        .collect();
    if schedule.all_folders {
        parts = vec![t.scope_all_folders.to_string()];
    }
    if parts.is_empty() {
        t.scope_nothing.to_string()
    } else {
        lang.list_names(&parts)
    }
}

/// Runs a job's command before its backup (`cmd /C` on Windows, `sh -c`
/// elsewhere) and waits for it. A failure stops the job. Cancelling the job
/// stops the command. The passphrase variable is not passed on, and the
/// command's error output goes to a file so a talkative command cannot block.
pub fn run_before_command(command: &str, cancel: &CancelToken) -> Result<(), String> {
    use std::process::{Command, Stdio};
    if command.is_empty() {
        return Ok(());
    }
    tracing::info!("running the command before the backup: {command}");
    let mut process = if cfg!(windows) {
        let mut p = Command::new("cmd");
        p.args(["/D", "/C", command]);
        p
    } else {
        let mut p = Command::new("sh");
        p.args(["-c", command]);
        p
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up for background jobs.
        process.creation_flags(0x0800_0000);
    }
    let errors =
        std::env::temp_dir().join(format!("aeternavault-before-{}.log", std::process::id()));
    let fail = |message: String| {
        tracing::error!("{message}");
        Err(message)
    };
    let stderr = match std::fs::File::create(&errors) {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
    };
    let mut child = match process
        .env_remove(PASSPHRASE_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return fail(format!(
                "the command before the backup could not start: {err}"
            ));
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if cancel.is_cancelled() => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&errors);
                return fail("the command before the backup was stopped".into());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(err) => return fail(format!("the command before the backup failed: {err}")),
        }
    };
    let output = std::fs::read(&errors).unwrap_or_default();
    let _ = std::fs::remove_file(&errors);
    if status.success() {
        tracing::info!("the command before the backup finished");
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output);
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    fail(format!(
        "the command before the backup failed ({status}): {}",
        last.trim()
    ))
}

#[cfg(test)]
mod before_command_tests {
    #[test]
    fn failing_command_stops_the_job() {
        let cancel = crate::engine::CancelToken::default();
        assert!(super::run_before_command("", &cancel).is_ok());
        assert!(super::run_before_command("exit 0", &cancel).is_ok());
        let failing = if cfg!(windows) {
            "echo broken 1>&2 & exit 3"
        } else {
            "echo broken 1>&2; exit 3"
        };
        let err = super::run_before_command(failing, &cancel).unwrap_err();
        assert!(err.contains("broken"), "{err}");
        assert!(err.contains("failed"), "{err}");
    }
}
