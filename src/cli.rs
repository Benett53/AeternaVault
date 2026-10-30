//! `aeternavault-cli`: everything AeternaVault does, from a terminal or a
//! script. With `--json`, results are printed as JSON (used by the PowerShell
//! module in `powershell/`).
//!
//! Encrypted backups: listing, backing up and checking may use the key that is
//! remembered on this computer. Anything that shows file names or contents
//! (`files`, `restore`, `extract`) always needs the passphrase or the recovery
//! key: from `--passphrase-stdin`, the `AETERNAVAULT_PASSPHRASE` variable, or a
//! prompt.
//!
//! Exit codes: 0 success, 1 error, 2 completed with problems or not confirmed.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::automatic;
use crate::config::{self, Config, ConflictPolicy, EncryptionScope, Frequency, Schedule, Source};
use crate::engine::crypto::{Cipher, KdfParams, SlotKind, VaultKey};
use crate::engine::plan::{self, ItemKind, RestoreOptions, RestoreTarget};
use crate::engine::retention::Removal;
use crate::engine::snapshots::{self, SnapshotInfo};
use crate::engine::{CancelToken, Progress, layout, manage, restore, selection, vault, verify};
use crate::history;
use crate::i18n::Lang;
use crate::paths::AppPaths;
use crate::platform::{self, apps::Catalog, known_paths::KnownPaths};
use crate::state::{self, AutomaticOutcome, State};

pub const PASSPHRASE_ENV: &str = automatic::PASSPHRASE_ENV;

#[derive(Parser, Debug)]
#[command(
    name = "aeternavault-cli",
    version,
    about = "AeternaVault on the command line",
    after_help = "Run `aeternavault-cli <command> --help` for details. Documentation: docs/cli-windows.md, docs/cli-linux.md"
)]
pub struct Cli {
    /// Print results as JSON.
    #[arg(long, global = true)]
    pub json: bool,
    /// Read the passphrase (or recovery key) from standard input, one per line.
    #[arg(long, global = true)]
    pub passphrase_stdin: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Destination, encryption, folders, jobs and the last backup at a glance.
    Status,
    /// Back up now: all ticked folders, or the folders of one job.
    Backup {
        /// Copy every file again instead of only new and changed ones.
        #[arg(long)]
        full: bool,
        /// Back up the folders of this job (name or id) and record it as its run.
        #[arg(long, value_name = "JOB")]
        job: Option<String>,
    },
    /// List the backups at the destination.
    #[command(alias = "snapshots")]
    List {
        #[command(flatten)]
        at: At,
    },
    /// List the files in a backup.
    Files {
        /// `latest` or a backup name as shown by `list`.
        backup: String,
        /// Only paths containing this text.
        #[arg(long)]
        filter: Option<String>,
        #[command(flatten)]
        at: At,
    },
    /// Restore a backup to the original places or into a folder.
    Restore {
        backup: String,
        /// Restore into this folder instead of the original places.
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
        /// Only these folders of the backup (as shown by `files`), e.g. `Documents`.
        #[arg(long, value_name = "FOLDER")]
        only: Vec<String>,
        /// What to do with files that already exist.
        #[arg(long, value_enum, default_value_t = ConflictArg::ReplaceChanged)]
        conflict: ConflictArg,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
        #[command(flatten)]
        at: At,
    },
    /// Copy files out of a backup into a folder (decrypted).
    Extract {
        backup: String,
        /// Files or folders inside the backup, e.g. `Documents/Letters`. Empty: everything.
        paths: Vec<String>,
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
        #[command(flatten)]
        at: At,
    },
    /// Read every file of a backup again and compare it with its checksum.
    Verify {
        backup: String,
        #[command(flatten)]
        at: At,
    },
    /// Delete backups.
    Delete {
        #[arg(required = true)]
        backups: Vec<String>,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Move a backup to another folder or drive.
    Move {
        backup: String,
        #[arg(long, value_name = "DIR")]
        to: PathBuf,
    },
    /// Give a backup a name (shown next to its date); an empty name removes it.
    Rename {
        backup: String,
        name: String,
        #[command(flatten)]
        at: At,
    },
    /// Remove old backups by the retention rules now.
    Prune {
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// The folders to back up.
    #[command(subcommand, alias = "sources")]
    Source(SourceCommand),
    /// Applications whose data folders can be added.
    #[command(subcommand, alias = "apps")]
    App(AppCommand),
    /// Automatic backups.
    #[command(subcommand, alias = "jobs")]
    Job(JobCommand),
    /// Rules for removing old backups.
    #[command(subcommand)]
    Retention(RetentionCommand),
    /// Where backups are kept.
    #[command(subcommand)]
    Destination(DestinationCommand),
    /// Encrypted backups.
    #[command(subcommand)]
    Encryption(EncryptionCommand),
    /// Read and change settings.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// What AeternaVault did (kept across sessions).
    History {
        /// Only the newest entries (0 = all).
        #[arg(long, default_value_t = 50)]
        last: usize,
        /// `text` (default), `csv` or `json` (same as --json).
        #[arg(long, value_enum, default_value_t = HistoryFormat::Text)]
        format: HistoryFormat,
        /// Write to this file instead of the screen.
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Where configuration, history and logs are stored.
    Paths,
    /// Notifications after backups (e-mail, Teams, Telegram, ntfy, Slack,
    /// Discord, Gotify, webhook). Thresholds and texts: `config set notifications.…`.
    #[command(subcommand, alias = "notifications")]
    Notify(NotifyCommand),
    /// Integration with the operating system.
    #[command(subcommand)]
    System(SystemCommand),
    /// Install the latest release if it is newer.
    Update {
        /// Only report whether a newer version is available.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Args, Debug, Clone, Default)]
pub struct At {
    /// Use the backups in this folder instead of the configured destination.
    #[arg(long, value_name = "DIR")]
    destination: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum SourceCommand {
    List,
    /// Add a folder.
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a folder from the list (nothing is deleted).
    Remove {
        source: String,
    },
    /// Tick a folder again.
    Enable {
        source: String,
    },
    /// Keep a folder in the list but leave it out of backups.
    Disable {
        source: String,
    },
    /// Leave out a sub-folder or file, e.g. `exclude Documents "Old stuff"`.
    Exclude {
        source: String,
        path: String,
    },
    /// Include a sub-folder or file again.
    Include {
        source: String,
        path: String,
    },
    /// With "encrypt marked folders and files": encrypt a folder or a part of it.
    Encrypt {
        source: String,
        path: Option<String>,
    },
    /// Do not encrypt a folder or a part of it.
    Plain {
        source: String,
        path: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum AppCommand {
    /// Applications found on this computer, and whether they are added.
    List,
    /// Add an application's data folders to the folders to back up.
    Add { id: String },
    /// Remove an application's folders from the list (nothing is deleted).
    Remove { id: String },
}

#[derive(Subcommand, Debug)]
pub enum JobCommand {
    List,
    /// Create a job.
    Add(JobSpec),
    /// Change a job (only the given options change).
    Edit {
        job: String,
        #[command(flatten)]
        spec: JobSpec,
    },
    Remove {
        job: String,
    },
    Enable {
        job: String,
    },
    Disable {
        job: String,
    },
    /// Run a job now.
    Run {
        job: String,
    },
    /// Run every job that is due, then exit (used by timers).
    RunDue,
}

#[derive(Args, Debug, Clone, Default)]
pub struct JobSpec {
    /// How often.
    #[arg(long, value_enum)]
    every: Option<EveryArg>,
    /// Time of day (HH:MM); for `hours`, the first run of the day.
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// For `week`.
    #[arg(long, value_enum)]
    day: Option<DayArg>,
    /// For `hours`: every how many hours (1–23).
    #[arg(long)]
    hours: Option<u8>,
    /// Only these folders (names or paths); repeat for several. Default: all ticked folders.
    #[arg(long = "folder", value_name = "FOLDER")]
    folders: Vec<String>,
    /// Back up all ticked folders.
    #[arg(long, conflicts_with = "folders")]
    all_folders: bool,
    #[arg(long)]
    name: Option<String>,
    /// Do not make up a missed backup later.
    #[arg(long)]
    no_catch_up: bool,
    /// Check the backup afterwards.
    #[arg(long)]
    verify: bool,
    /// Do not check the backup afterwards.
    #[arg(long, conflicts_with = "verify")]
    no_verify: bool,
    /// Run this command before the backup (e.g. a database dump); `-` removes it.
    #[arg(long, value_name = "COMMAND")]
    before: Option<String>,
    /// Back up to this folder instead of the general destination; `-` for the general one.
    #[arg(long, value_name = "DIR")]
    destination: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum EveryArg {
    Day,
    Week,
    Hours,
    /// A few minutes after AeternaVault starts.
    Start,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DayArg {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Subcommand, Debug)]
pub enum RetentionCommand {
    Show,
    On,
    Off,
    Set {
        #[arg(long)]
        keep_last: Option<u32>,
        #[arg(long)]
        days: Option<u32>,
        #[arg(long)]
        weeks: Option<u32>,
        #[arg(long)]
        months: Option<u32>,
    },
}

#[derive(Subcommand, Debug)]
pub enum DestinationCommand {
    Show,
    /// Use this folder. With `--app-folder`, backups go into an `AeternaVault` folder inside it.
    Set {
        path: PathBuf,
        #[arg(long)]
        app_folder: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum EncryptionCommand {
    Status,
    /// Create the encrypted vault at the destination and turn encryption on.
    Setup {
        #[arg(long, value_enum, default_value_t = CipherArg::Xchacha20)]
        cipher: CipherArg,
        #[arg(long, value_enum, default_value_t = StrengthArg::Standard)]
        strength: StrengthArg,
        /// Remember the key on this computer (for automatic backups).
        #[arg(long)]
        remember: bool,
        /// Also write the recovery key into this text file.
        #[arg(long, value_name = "FILE")]
        recovery_file: Option<PathBuf>,
    },
    /// Encrypt new backups.
    On,
    /// Do not encrypt new backups (existing ones stay encrypted).
    Off,
    /// Encrypt everything, or only marked folders and files.
    Scope {
        #[arg(value_enum)]
        scope: ScopeArg,
    },
    /// Replace the passphrase (asks for the current one first).
    ChangePassphrase,
    /// Check that a recovery key works.
    TestRecovery,
    /// Create a new recovery key; the old one stops working.
    NewRecovery {
        #[arg(long, value_name = "FILE")]
        recovery_file: Option<PathBuf>,
    },
    /// Remember the key on this computer for automatic backups.
    Remember,
    /// Forget the remembered key.
    Forget,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CipherArg {
    Xchacha20,
    Aes256,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StrengthArg {
    Standard,
    Strong,
    VeryStrong,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ScopeArg {
    Everything,
    Selected,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// The whole configuration.
    Show,
    /// One value, e.g. `advanced.hardlink_unchanged`.
    Get { key: String },
    /// Change one value, e.g. `config set language de`.
    Set { key: String, value: String },
    /// Path of the configuration file.
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HistoryFormat {
    Text,
    Csv,
    Json,
}

#[derive(Subcommand, Debug)]
pub enum NotifyCommand {
    /// The channels and rules.
    List,
    /// Send a test message to all enabled channels, or to one.
    Test {
        channel: Option<String>,
    },
    /// Add a channel.
    Add(Box<ChannelArgs>),
    /// Remove a channel.
    Remove {
        channel: String,
    },
    Enable {
        channel: String,
    },
    Disable {
        channel: String,
    },
}

#[derive(Args, Debug)]
pub struct ChannelArgs {
    #[arg(long, value_enum)]
    kind: crate::notify::ChannelKind,
    /// A name to tell channels apart, e.g. `Ops team`.
    #[arg(long)]
    name: Option<String>,
    /// Webhook or server URL (ntfy topic URL, Teams Workflows URL, …).
    #[arg(long)]
    url: Option<String>,
    /// Telegram bot token, ntfy/Gotify token, or bearer token for webhooks.
    #[arg(long)]
    token: Option<String>,
    /// Telegram chat id.
    #[arg(long)]
    chat_id: Option<String>,
    #[arg(long)]
    smtp_host: Option<String>,
    #[arg(long)]
    smtp_port: Option<u16>,
    #[arg(long, value_enum)]
    smtp_security: Option<crate::notify::SmtpSecurity>,
    #[arg(long)]
    smtp_user: Option<String>,
    /// SMTP password; with `--passphrase-stdin` it is read from standard input instead.
    #[arg(long)]
    smtp_password: Option<String>,
    #[arg(long)]
    from: Option<String>,
    /// Recipients, separated by commas.
    #[arg(long)]
    to: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum SystemCommand {
    /// Run backup jobs in the background: on Windows by starting AeternaVault
    /// with Windows, on Linux with a systemd user timer.
    Background {
        #[arg(value_enum)]
        state: Option<OnOff>,
    },
    /// Windows: open encrypted backups by double-click.
    DoubleClick {
        #[arg(value_enum)]
        state: Option<OnOff>,
    },
    /// Windows: "Back up with AeternaVault" in the Explorer menu of folders.
    ExplorerMenu {
        #[arg(value_enum)]
        state: Option<OnOff>,
    },
    /// Remove everything AeternaVault registered in the system (used when uninstalling).
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConflictArg {
    ReplaceChanged,
    KeepExisting,
    KeepNewer,
}

impl From<ConflictArg> for ConflictPolicy {
    fn from(value: ConflictArg) -> Self {
        match value {
            ConflictArg::ReplaceChanged => ConflictPolicy::ReplaceChanged,
            ConflictArg::KeepExisting => ConflictPolicy::KeepExisting,
            ConflictArg::KeepNewer => ConflictPolicy::KeepNewer,
        }
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// A finished command: its exit code.
enum Outcome {
    Ok,
    /// Completed with problems, or not confirmed.
    Partly,
}

struct Ctx {
    paths: AppPaths,
    config: Config,
    json: bool,
    secret_stdin: bool,
    stdin_lines: Option<VecDeque<String>>,
    computer: String,
    /// The passphrase or recovery key was entered in this call (not the
    /// remembered key): names of encrypted files may be shown.
    key_typed: bool,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let paths = AppPaths::resolve();
    let _log = crate::logging::init(&paths, true);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        command = ?std::env::args().nth(1).unwrap_or_default(),
        "aeternavault-cli started"
    );
    let loaded = config::load_or_create(&paths);
    let mut ctx = Ctx {
        paths,
        config: loaded.config,
        json: cli.json,
        secret_stdin: cli.passphrase_stdin,
        stdin_lines: None,
        key_typed: false,
        computer: platform::computer_name(),
    };
    match run(cli.command, &mut ctx) {
        Ok(Outcome::Ok) => ExitCode::SUCCESS,
        Ok(Outcome::Partly) => ExitCode::from(2),
        Err(err) => {
            tracing::error!("{err:#}");
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command, ctx: &mut Ctx) -> anyhow::Result<Outcome> {
    match command {
        Command::Status => status(ctx),
        Command::Backup { full, job } => backup(ctx, full, job),
        Command::List { at } => list(ctx, &at),
        Command::Files { backup, filter, at } => files(ctx, &backup, filter, &at),
        Command::Restore {
            backup,
            to,
            only,
            conflict,
            yes,
            at,
        } => restore_backup(ctx, &backup, to, only, conflict.into(), yes, &at),
        Command::Extract {
            backup,
            paths,
            to,
            at,
        } => extract(ctx, &backup, &paths, &to, &at),
        Command::Verify { backup, at } => verify_backup(ctx, &backup, &at),
        Command::Delete { backups, yes } => delete(ctx, &backups, yes),
        Command::Move { backup, to } => move_backup(ctx, &backup, &to),
        Command::Prune { yes } => prune(ctx, yes),
        Command::Rename { backup, name, at } => {
            let destination = ctx.destination(&at)?;
            let key = ctx.remembered_key(&destination);
            let snapshot = ctx.find(&destination, &backup, key.as_ref())?;
            manage::set_label(&snapshot, &name)
                .with_context(|| format!("could not name {}", snapshot.id))?;
            ctx.print(
                &serde_json::json!({ "backup": snapshot.id, "name": name.trim() }),
                || format!("Backup {} is now named {:?}.", snapshot.id, name.trim()),
            );
            Ok(Outcome::Ok)
        }
        Command::Source(command) => source(ctx, command),
        Command::App(command) => app(ctx, command),
        Command::Job(command) => job(ctx, command),
        Command::Retention(command) => retention(ctx, command),
        Command::Destination(command) => destination(ctx, command),
        Command::Encryption(command) => encryption(ctx, command),
        Command::Config(command) => config_command(ctx, command),
        Command::History {
            last,
            format,
            output,
        } => history_command(ctx, last, format, output),
        Command::Paths => paths_command(ctx),
        Command::Notify(command) => notify_command(ctx, command),
        Command::System(command) => system(ctx, command),
        Command::Update { check } => match crate::update::run_cli(check) {
            0 => Ok(Outcome::Ok),
            _ => anyhow::bail!("the update did not complete"),
        },
    }
}

impl Ctx {
    fn save(&self) -> anyhow::Result<()> {
        self.config
            .save(&self.paths.config_file)
            .with_context(|| format!("could not save {}", self.paths.config_file.display()))
    }

    fn destination(&self, at: &At) -> anyhow::Result<PathBuf> {
        let destination = at
            .destination
            .clone()
            .unwrap_or_else(|| self.config.destination.clone());
        if destination.as_os_str().is_empty() {
            bail!("no destination is set; use `aeternavault-cli destination set <folder>`");
        }
        // Backups of AeternaVault 0.3 and 0.4 move to the current layout once.
        if let Err(err) = layout::migrate(&destination) {
            tracing::warn!("older backups could not be moved to the current layout: {err}");
        }
        Ok(destination)
    }

    /// Prints `value` as JSON, or `text` for people.
    fn print<T: Serialize>(&self, value: &T, text: impl FnOnce() -> String) {
        if self.json {
            match serde_json::to_string_pretty(value) {
                Ok(json) => println!("{json}"),
                Err(err) => eprintln!("error: {err}"),
            }
        } else {
            let text = text();
            if !text.is_empty() {
                println!("{text}");
            }
        }
    }

    /// A secret from standard input (one per line), the environment, or a prompt.
    fn secret(&mut self, prompt: &str) -> Option<String> {
        if self.secret_stdin {
            let lines = self.stdin_lines.get_or_insert_with(|| {
                std::io::stdin()
                    .lock()
                    .lines()
                    .map_while(Result::ok)
                    // Windows PowerShell 5.1 puts a byte order mark in front of
                    // piped text; it is not part of the passphrase.
                    .map(|l| {
                        l.trim_start_matches('\u{feff}')
                            .trim_end_matches(['\r', '\n'])
                            .to_string()
                    })
                    .collect()
            });
            return lines.pop_front();
        }
        if let Ok(secret) = std::env::var(PASSPHRASE_ENV)
            && !secret.is_empty()
        {
            return Some(secret);
        }
        // Scripts and scheduled runs have no one to answer a prompt; waiting
        // for one would hang them.
        if !std::io::stdin().is_terminal() {
            return None;
        }
        platform::read_secret(prompt)
    }

    fn interactive(&self) -> bool {
        !self.secret_stdin && std::io::stdin().is_terminal()
    }

    /// The key for reading file names and contents: always the passphrase or
    /// the recovery key, never the remembered key. `None` without a vault.
    fn read_key(&mut self, destination: &Path) -> anyhow::Result<Option<VaultKey>> {
        if !vault::exists(destination) {
            return Ok(None);
        }
        let attempts = if self.interactive() && std::env::var(PASSPHRASE_ENV).is_err() {
            3
        } else {
            1
        };
        for _ in 0..attempts {
            let Some(secret) = self.secret("Passphrase or recovery key: ") else {
                bail!(
                    "these backups are encrypted: enter the passphrase, set {PASSPHRASE_ENV}, or use --passphrase-stdin"
                );
            };
            match vault::unlock(destination, &secret) {
                Ok((_, key)) => {
                    self.key_typed = true;
                    return Ok(Some(key));
                }
                Err(err) if attempts > 1 => eprintln!("{err}"),
                Err(err) => return Err(err.into()),
            }
        }
        bail!("the passphrase or recovery key is not correct")
    }

    /// The key for backing up, listing and checking: the remembered one if
    /// there is one, otherwise the passphrase. A passphrase given with
    /// `--passphrase-stdin` is used in any case (and allows showing names).
    fn write_key(&mut self, destination: &Path) -> anyhow::Result<Option<VaultKey>> {
        let Ok(header) = vault::read_header(destination) else {
            return Ok(None);
        };
        if self.secret_stdin {
            return self.read_key(destination);
        }
        if let Some(key) = vault::remembered_key(&state::key_dir(&self.paths.config_file), &header)
        {
            return Ok(Some(key));
        }
        self.read_key(destination)
    }

    /// The remembered key only (no prompt): for listing.
    fn remembered_key(&self, destination: &Path) -> Option<VaultKey> {
        let header = vault::read_header(destination).ok()?;
        vault::remembered_key(&state::key_dir(&self.paths.config_file), &header)
    }

    fn find(
        &mut self,
        destination: &Path,
        wanted: &str,
        key: Option<&VaultKey>,
    ) -> anyhow::Result<SnapshotInfo> {
        Ok(snapshots::find(destination, wanted, &self.computer, key)?)
    }

    fn confirm(&self, question: &str) -> bool {
        if !self.interactive() {
            return false;
        }
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).is_ok()
            && matches!(
                answer.trim().to_lowercase().as_str(),
                "y" | "yes" | "j" | "ja"
            )
    }
}

/// Progress on the terminal (stderr), about four times a second.
fn progress_printer(enabled: bool) -> impl FnMut(&Progress) {
    let mut last = std::time::Instant::now();
    move |progress: &Progress| {
        if !enabled || last.elapsed().as_millis() < 250 {
            return;
        }
        last = std::time::Instant::now();
        let percent = progress
            .fraction()
            .map(|f| format!("{:>3.0}%", f * 100.0))
            .unwrap_or_else(|| "   ".into());
        let mut current = progress.current.clone();
        if current.chars().count() > 60 {
            let tail: String = current.chars().rev().take(57).collect();
            current = format!("...{}", tail.chars().rev().collect::<String>());
        }
        eprint!(
            "\r{percent}  {} files  {:<60}",
            progress.files_done, current
        );
        let _ = std::io::stderr().flush();
    }
}

fn end_progress(enabled: bool) {
    if enabled {
        eprint!("\r{:<80}\r", "");
    }
}

fn bytes(n: u64) -> String {
    Lang::En.bytes(n)
}

fn local(time: chrono::DateTime<chrono::Utc>) -> String {
    time.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusView {
    version: &'static str,
    destination: PathBuf,
    destination_reachable: bool,
    free_bytes: Option<u64>,
    /// The destination is on the same drive as a ticked folder.
    same_drive_as_folders: bool,
    encryption: &'static str,
    vault: bool,
    key_remembered: bool,
    folders: Vec<SourceView>,
    jobs: Vec<JobView>,
    last_backup: Option<String>,
    backups: usize,
    background: bool,
}

fn status(ctx: &mut Ctx) -> anyhow::Result<Outcome> {
    let destination = ctx.config.destination.clone();
    let reachable = snapshots::destination_reachable(&destination);
    let key = ctx.remembered_key(&destination);
    let list = if reachable {
        let _ = layout::migrate(&destination);
        snapshots::list(&destination, key.as_ref()).unwrap_or_default()
    } else {
        Vec::new()
    };
    let header = vault::read_header(&destination).ok();
    let view = StatusView {
        version: env!("CARGO_PKG_VERSION"),
        destination: destination.clone(),
        destination_reachable: reachable,
        free_bytes: platform::free_space(&destination),
        encryption: encryption_word(&ctx.config),
        vault: header.is_some(),
        key_remembered: header
            .as_ref()
            .is_some_and(|h| vault::is_remembered(&state::key_dir(&ctx.paths.config_file), h)),
        folders: source_views(&ctx.config),
        jobs: job_views(ctx),
        last_backup: list.first().map(|s| s.id.clone()),
        backups: list.len(),
        background: background_on(),
        same_drive_as_folders: ctx
            .config
            .enabled_sources()
            .any(|s| platform::same_drive(&destination, &s.path)),
    };
    ctx.print(&view, || {
        let mut out = String::new();
        out += &format!("AeternaVault {}\n", view.version);
        out += &format!(
            "Destination:  {}{}\n",
            view.destination.display(),
            match (view.destination_reachable, view.free_bytes) {
                (false, _) => "  (not reachable)".to_string(),
                (true, Some(free)) => format!("  ({} free)", bytes(free)),
                _ => String::new(),
            }
        );
        if view.same_drive_as_folders {
            out += "              Note: on the same drive as folders that are backed up;
              a failing drive would take the backups with it.
";
        }
        out += &format!(
            "Encryption:   {}{}\n",
            view.encryption,
            if view.key_remembered {
                ", key remembered"
            } else {
                ""
            }
        );
        out += &format!(
            "Folders:      {} ({} ticked)\n",
            view.folders.len(),
            view.folders.iter().filter(|f| f.enabled).count()
        );
        out += &format!(
            "Jobs:         {} ({} on){}\n",
            view.jobs.len(),
            view.jobs.iter().filter(|j| j.enabled).count(),
            if view.background {
                ", running in the background"
            } else {
                ""
            }
        );
        out += &format!(
            "Backups:      {}{}",
            view.backups,
            view.last_backup
                .as_ref()
                .map(|l| format!(", newest {l}"))
                .unwrap_or_default()
        );
        out
    });
    Ok(Outcome::Ok)
}

fn encryption_word(config: &Config) -> &'static str {
    if config.encryption.everything() {
        "on (everything)"
    } else if config.encryption.selected() {
        "on (marked folders and files)"
    } else {
        "off"
    }
}

#[derive(Serialize)]
struct BackupResult {
    outcome: AutomaticOutcome,
    backup: Option<String>,
    files: u64,
    bytes: u64,
    message: String,
}

fn backup(ctx: &mut Ctx, full: bool, job: Option<String>) -> anyhow::Result<Outcome> {
    let mut config = ctx.config.clone();
    if full {
        config.mode = config::BackupMode::Full;
    }
    let schedule = match &job {
        Some(wanted) => Some(find_job(&ctx.config, wanted)?.clone()),
        None => None,
    };
    let destination = ctx.destination(&At::default())?;
    let key = if config.encryption.enabled {
        ctx.write_key(&destination)?
    } else {
        None
    };
    let show = !ctx.json && std::io::stderr().is_terminal();
    let cancel = CancelToken::default();
    let (run, report) = automatic::run_job(
        &ctx.paths,
        &config,
        schedule.as_ref(),
        key,
        &cancel,
        &mut progress_printer(show),
        true,
    );
    end_progress(show);
    let result = BackupResult {
        outcome: run.outcome,
        backup: report
            .as_ref()
            .and_then(|r| r.snapshot_dir.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        files: run.files,
        bytes: run.bytes,
        message: run.message.clone(),
    };
    ctx.print(&result, || match run.outcome {
        AutomaticOutcome::Complete | AutomaticOutcome::CompleteWithNotes => {
            let mut text = format!(
                "Backup {} completed: {} files, {}.",
                result.backup.clone().unwrap_or_default(),
                Lang::En.count(run.files),
                bytes(run.bytes)
            );
            if let Some(report) = &report {
                for warning in &report.warnings {
                    text += &format!("\n  note: {warning}");
                }
            }
            text
        }
        AutomaticOutcome::DestinationUnavailable => {
            format!("Skipped: the destination is not reachable. {}", run.message)
        }
        AutomaticOutcome::Interrupted => {
            "Interrupted: the backup was stopped before it finished.".to_string()
        }
        _ => String::new(),
    });
    match run.outcome {
        AutomaticOutcome::Complete => Ok(Outcome::Ok),
        AutomaticOutcome::CompleteWithNotes
        | AutomaticOutcome::DestinationUnavailable
        | AutomaticOutcome::Interrupted => Ok(Outcome::Partly),
        _ => Err(anyhow!("the backup failed: {}", run.message)),
    }
}

#[derive(Serialize)]
struct BackupView {
    id: String,
    /// Name given with `rename`, or the job's name.
    name: Option<String>,
    date: Option<String>,
    computer: String,
    status: String,
    encrypted: &'static str,
    locked: bool,
    files: Option<u64>,
    bytes: Option<u64>,
    /// When the retention rules will remove it: `next-backup`, a date, `later`.
    removal: Option<String>,
}

fn backup_views(ctx: &Ctx, list: &[SnapshotInfo]) -> Vec<BackupView> {
    let removals = automatic::removal_dates(&ctx.config, list, &ctx.computer);
    list.iter()
        .map(|s| {
            let stats = s.total_stats();
            let removal = removals
                .iter()
                .find(|(id, _)| id == &s.qualified_id())
                .and_then(|(_, r)| match r {
                    Removal::NextBackup => Some("next-backup".to_string()),
                    Removal::After(at) => Some(
                        at.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d")
                            .to_string(),
                    ),
                    Removal::NotWithin => Some("later".to_string()),
                    Removal::NotAffected => None,
                });
            BackupView {
                id: s.id.clone(),
                name: s.label.clone(),
                date: s.started_at().map(local),
                computer: s.computer.clone(),
                status: match &s.header {
                    Some(h) => Lang::En.status(Some(h.status)).to_string(),
                    None if s.is_locked() => "encrypted (locked)".into(),
                    None => Lang::En.status(None).to_string(),
                },
                encrypted: if s.is_split() {
                    "partly"
                } else if s.needs_key() {
                    "yes"
                } else {
                    "no"
                },
                locked: s.needs_unlock(),
                files: stats.as_ref().map(|s| s.files),
                bytes: stats.as_ref().map(|s| s.bytes),
                removal,
            }
        })
        .collect()
}

fn list(ctx: &mut Ctx, at: &At) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(at)?;
    let key = if ctx.secret_stdin {
        ctx.write_key(&destination)?
    } else {
        ctx.remembered_key(&destination)
    };
    let list = snapshots::list(&destination, key.as_ref())?;
    let views = backup_views(ctx, &list);
    ctx.print(&views, || {
        if views.is_empty() {
            return format!("No backups in {}", destination.display());
        }
        let mut out = format!(
            "{:<28} {:<24} {:>9} {:>10}  {:<9} {}\n",
            "BACKUP", "STATUS", "FILES", "SIZE", "ENCRYPTED", "REMOVED"
        );
        for v in &views {
            out += &format!(
                "{:<28} {:<24} {:>9} {:>10}  {:<9} {}\n",
                v.name
                    .as_ref()
                    .map_or_else(|| v.id.clone(), |n| format!("{} ({n})", v.id)),
                v.status,
                v.files.map(|f| Lang::En.count(f)).unwrap_or_default(),
                v.bytes.map(bytes).unwrap_or_default(),
                v.encrypted,
                v.removal.as_deref().unwrap_or("")
            );
        }
        out.trim_end().to_string()
    });
    Ok(Outcome::Ok)
}

#[derive(Serialize)]
struct FileView {
    path: String,
    size: u64,
    modified: String,
    encrypted: bool,
}

fn files(ctx: &mut Ctx, wanted: &str, filter: Option<String>, at: &At) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(at)?;
    let key = ctx.read_key(&destination)?;
    let snapshot = ctx.find(&destination, wanted, key.as_ref())?;
    let needle = filter.map(|f| f.to_lowercase());
    let views: Vec<FileView> = snapshots::contents(&snapshot, key.as_ref())?
        .into_iter()
        .filter(|f| {
            needle
                .as_ref()
                .is_none_or(|n| f.display_path().to_lowercase().contains(n))
        })
        .map(|f| FileView {
            path: f.display_path(),
            size: f.size,
            modified: local(crate::engine::from_unix_nanos(f.modified).into()),
            encrypted: f.encrypted,
        })
        .collect();
    ctx.print(&views, || {
        views
            .iter()
            .map(|f| format!("{:>10}  {}  {}", bytes(f.size), f.modified, f.path))
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(Outcome::Ok)
}

#[derive(Serialize)]
struct RestoreResult {
    backup: String,
    restored_files: u64,
    restored_bytes: u64,
    skipped: u64,
    failed: u64,
    warnings: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn restore_backup(
    ctx: &mut Ctx,
    wanted: &str,
    to: Option<PathBuf>,
    only: Vec<String>,
    conflict: ConflictPolicy,
    yes: bool,
    at: &At,
) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(at)?;
    let key = ctx.read_key(&destination)?;
    let snapshot = ctx.find(&destination, wanted, key.as_ref())?;
    let header = snapshot
        .header
        .clone()
        .ok_or_else(|| anyhow!("{} cannot be read", snapshot.id))?;
    let skip: HashSet<String> = if only.is_empty() {
        HashSet::new()
    } else {
        for folder in &only {
            if !header
                .sources
                .iter()
                .any(|s| s.key.eq_ignore_ascii_case(folder))
            {
                bail!("the backup has no folder named {folder}");
            }
        }
        header
            .sources
            .iter()
            .filter(|s| !only.iter().any(|o| o.eq_ignore_ascii_case(&s.key)))
            .map(|s| s.key.clone())
            .collect()
    };
    let target_text = to
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let options = RestoreOptions {
        target: to
            .map(RestoreTarget::Folder)
            .unwrap_or(RestoreTarget::Original),
        conflict,
        verify: ctx.config.advanced.verify_on_restore,
        skip,
    };
    let cancel = CancelToken::default();
    let plan = plan::plan_restore(&snapshot, options, key.as_ref(), &cancel, &mut |_| {})?;
    let (create, replace) = (
        plan.summary.count(ItemKind::New),
        plan.summary.count(ItemKind::Changed),
    );
    if !ctx.json {
        eprintln!(
            "Backup {}: {} files to create, {} to replace, {} unchanged or skipped.",
            snapshot.id,
            create,
            replace,
            plan.summary.count(ItemKind::Unchanged) + plan.summary.count(ItemKind::Skipped)
        );
    }
    if !yes
        && !ctx.confirm(if target_text.is_empty() {
            "Restore to the original places?"
        } else {
            "Restore into the folder?"
        })
    {
        eprintln!("Nothing was changed. Add --yes to restore without asking.");
        return Ok(Outcome::Partly);
    }
    let show = !ctx.json && std::io::stderr().is_terminal();
    let result = restore::run_restore(&plan, key.as_ref(), &cancel, &mut progress_printer(show));
    end_progress(show);
    history::record(
        &ctx.paths.config_file,
        history::restore_event(&result, &snapshot.id, &target_text),
    );
    let report = result?;
    let view = RestoreResult {
        backup: snapshot.id.clone(),
        restored_files: report.restored_files,
        restored_bytes: report.restored_bytes,
        skipped: report.skipped,
        failed: report.failed,
        warnings: report.warnings.clone(),
    };
    ctx.print(&view, || {
        let mut text = Lang::En.restore_result(
            report.restored_files,
            report.restored_bytes,
            report.skipped,
            report.duration,
        );
        for warning in &report.warnings {
            text += &format!("\n  note: {warning}");
        }
        text
    });
    Ok(if report.failed == 0 && !report.cancelled {
        Outcome::Ok
    } else {
        Outcome::Partly
    })
}

#[derive(Serialize)]
struct ExtractResult {
    target: PathBuf,
    files: u64,
    bytes: u64,
    failed: Vec<String>,
}

fn extract(
    ctx: &mut Ctx,
    wanted: &str,
    paths: &[String],
    to: &Path,
    at: &At,
) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(at)?;
    let key = ctx.read_key(&destination)?;
    let snapshot = ctx.find(&destination, wanted, key.as_ref())?;
    let normalize = |p: &str| p.replace('\\', "/").trim_matches('/').to_lowercase();
    let wanted_paths: Vec<String> = paths.iter().map(|p| normalize(p)).collect();
    let files: Vec<_> = snapshots::contents(&snapshot, key.as_ref())?
        .into_iter()
        .filter(|f| {
            let path = f.display_path().to_lowercase();
            wanted_paths.is_empty()
                || wanted_paths
                    .iter()
                    .any(|w| path == *w || path.starts_with(&format!("{w}/")))
        })
        .collect();
    if files.is_empty() {
        bail!("nothing in {} matches", snapshot.id);
    }
    let show = !ctx.json && std::io::stderr().is_terminal();
    let cancel = CancelToken::default();
    let report = manage::extract(
        &snapshot,
        &files,
        to,
        key.as_ref(),
        &cancel,
        &mut progress_printer(show),
    )?;
    end_progress(show);
    history::record(
        &ctx.paths.config_file,
        history::Event::FilesCopied {
            snapshot: snapshot.id.clone(),
            files: report.files,
            to: to.display().to_string(),
        },
    );
    let view = ExtractResult {
        target: to.to_path_buf(),
        files: report.files,
        bytes: report.bytes,
        failed: report.failed.clone(),
    };
    ctx.print(&view, || {
        let mut text = format!(
            "{} files ({}) copied to {}.",
            Lang::En.count(report.files),
            bytes(report.bytes),
            to.display()
        );
        for failed in &report.failed {
            text += &format!("\n  failed: {failed}");
        }
        text
    });
    Ok(if report.failed.is_empty() {
        Outcome::Ok
    } else {
        Outcome::Partly
    })
}

#[derive(Serialize)]
struct VerifyResult {
    backup: String,
    files: u64,
    bytes: u64,
    damaged: Vec<String>,
    missing: Vec<String>,
}

fn verify_backup(ctx: &mut Ctx, wanted: &str, at: &At) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(at)?;
    let key = ctx.write_key(&destination)?;
    let snapshot = ctx.find(&destination, wanted, key.as_ref())?;
    let show = !ctx.json && std::io::stderr().is_terminal();
    let cancel = CancelToken::default();
    let report = verify::verify(
        &snapshot,
        key.as_ref(),
        ctx.key_typed,
        &cancel,
        &mut progress_printer(show),
    )?;
    end_progress(show);
    history::record(
        &ctx.paths.config_file,
        history::Event::Verified {
            snapshot: snapshot.id.clone(),
            files: report.files,
            damaged: report.damaged.len() as u64,
            missing: report.missing.len() as u64,
        },
    );
    let ok = report.is_ok();
    tracing::info!(
        damaged = report.damaged.len(),
        missing = report.missing.len(),
        "backup {} checked",
        snapshot.id
    );
    let view = VerifyResult {
        backup: snapshot.id.clone(),
        files: report.files,
        bytes: report.bytes,
        damaged: report.damaged.clone(),
        missing: report.missing.clone(),
    };
    ctx.print(&view, || {
        let mut text = if ok {
            format!(
                "Backup {} is complete: {} files ({}) read and checked.",
                snapshot.id,
                Lang::En.count(report.files),
                bytes(report.bytes)
            )
        } else {
            format!(
                "Backup {}: {} damaged, {} missing.",
                snapshot.id,
                report.damaged.len(),
                report.missing.len()
            )
        };
        for damaged in &report.damaged {
            text += &format!("\n  damaged: {damaged}");
        }
        for missing in &report.missing {
            text += &format!("\n  missing: {missing}");
        }
        text
    });
    Ok(if ok { Outcome::Ok } else { Outcome::Partly })
}

#[derive(Serialize)]
struct DeleteResult {
    deleted: Vec<String>,
    deleted_permanently: u64,
    freed_bytes: u64,
    warnings: Vec<String>,
}

fn delete(ctx: &mut Ctx, wanted: &[String], yes: bool) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(&At::default())?;
    let key = ctx.write_key(&destination)?;
    let mut ids = Vec::new();
    for w in wanted {
        ids.push(ctx.find(&destination, w, key.as_ref())?.qualified_id());
    }
    if !yes && !ctx.confirm(&format!("Delete {}?", ids.join(", "))) {
        eprintln!("Nothing was deleted. Add --yes to delete without asking.");
        return Ok(Outcome::Partly);
    }
    let cancel = CancelToken::default();
    let report = manage::delete(&destination, &ids, key.as_ref(), &cancel, &mut |_| {})?;
    history::record(
        &ctx.paths.config_file,
        history::Event::BackupsDeleted {
            snapshots: report.deleted.clone(),
            by_rules: false,
            message: report.warnings.first().cloned().unwrap_or_default(),
        },
    );
    let view = DeleteResult {
        deleted: report.deleted.clone(),
        deleted_permanently: report.deleted_permanently,
        freed_bytes: report.freed_bytes,
        warnings: report.warnings.clone(),
    };
    ctx.print(&view, || format!("Deleted: {}", view.deleted.join(", ")));
    Ok(Outcome::Ok)
}

fn move_backup(ctx: &mut Ctx, wanted: &str, to: &Path) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(&At::default())?;
    let key = ctx.write_key(&destination)?;
    let snapshot = ctx.find(&destination, wanted, key.as_ref())?;
    let cancel = CancelToken::default();
    let show = !ctx.json && std::io::stderr().is_terminal();
    let report = manage::transfer(
        &snapshot,
        to,
        key.as_ref(),
        &cancel,
        &mut progress_printer(show),
    )?;
    end_progress(show);
    history::record(
        &ctx.paths.config_file,
        history::Event::BackupMoved {
            snapshot: snapshot.id.clone(),
            to: to.display().to_string(),
        },
    );
    ctx.print(
        &serde_json::json!({"backup": snapshot.id, "to": to, "files": report.files, "bytes": report.bytes}),
        || format!("{} moved to {}.", snapshot.id, to.display()),
    );
    Ok(Outcome::Ok)
}

fn prune(ctx: &mut Ctx, yes: bool) -> anyhow::Result<Outcome> {
    if !ctx.config.retention.enabled {
        bail!("the rules for removing old backups are off; use `aeternavault-cli retention on`");
    }
    let destination = ctx.destination(&At::default())?;
    let key = ctx.write_key(&destination)?;
    let list = snapshots::list(&destination, key.as_ref())?;
    let ids = crate::engine::retention::to_remove(&list, &ctx.config.retention, &ctx.computer);
    if ids.is_empty() {
        ctx.print(&serde_json::json!({"deleted": []}), || {
            "No backup is due for removal.".into()
        });
        return Ok(Outcome::Ok);
    }
    if !yes && !ctx.confirm(&format!("Remove {}?", ids.join(", "))) {
        eprintln!("Nothing was removed. Add --yes to remove without asking.");
        return Ok(Outcome::Partly);
    }
    let cancel = CancelToken::default();
    let report = manage::delete(&destination, &ids, key.as_ref(), &cancel, &mut |_| {})?;
    automatic::record_retention(&ctx.paths.config_file, &report);
    ctx.print(&serde_json::json!({"deleted": report.deleted}), || {
        format!("Removed: {}", report.deleted.join(", "))
    });
    Ok(Outcome::Ok)
}

// ---------------------------------------------------------------------------
// Folders and applications
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SourceView {
    name: String,
    path: PathBuf,
    enabled: bool,
    excluded: Vec<String>,
    included: Vec<String>,
    encrypted: Vec<String>,
    exclude_patterns: Vec<String>,
}

fn source_views(config: &Config) -> Vec<SourceView> {
    config
        .sources
        .iter()
        .map(|s| SourceView {
            name: s.display_name(),
            path: s.path.clone(),
            enabled: s.enabled,
            excluded: s.exclude_paths.clone(),
            included: s.include_paths.clone(),
            encrypted: s.encrypt_paths.clone(),
            exclude_patterns: s.exclude.clone(),
        })
        .collect()
}

fn find_source<'a>(config: &'a mut Config, wanted: &str) -> anyhow::Result<&'a mut Source> {
    let path = PathBuf::from(wanted);
    config
        .sources
        .iter_mut()
        .find(|s| {
            s.display_name().eq_ignore_ascii_case(wanted)
                || crate::engine::paths_equal(&s.path, &path)
        })
        .ok_or_else(|| anyhow!("no folder named {wanted}; see `aeternavault-cli source list`"))
}

fn relative(path: &str) -> String {
    let trimmed = path.replace('\\', "/");
    let trimmed = trimmed.trim_matches('/');
    if trimmed.is_empty() {
        selection::ROOT.to_string()
    } else {
        trimmed.to_string()
    }
}

fn source(ctx: &mut Ctx, command: SourceCommand) -> anyhow::Result<Outcome> {
    let message = match command {
        SourceCommand::List => {
            let views = source_views(&ctx.config);
            ctx.print(&views, || {
                views
                    .iter()
                    .map(|v| {
                        let mut line = format!(
                            "[{}] {:<20} {}",
                            if v.enabled { "x" } else { " " },
                            v.name,
                            v.path.display()
                        );
                        if !v.excluded.is_empty() {
                            line += &format!("\n      left out: {}", v.excluded.join(", "));
                        }
                        if !v.encrypted.is_empty() {
                            line += &format!("\n      encrypted: {}", v.encrypted.join(", "));
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            return Ok(Outcome::Ok);
        }
        SourceCommand::Add { path, name } => {
            let path = std::path::absolute(&path)?;
            if !path.is_dir() {
                bail!("{} is not a folder", path.display());
            }
            if ctx.config.has_source_path(&path) {
                bail!("{} is already in the list", path.display());
            }
            if crate::engine::path_is_within(&path, &ctx.config.destination)
                || crate::engine::path_is_within(&ctx.config.destination, &path)
            {
                bail!("the folder contains the destination or lies inside it");
            }
            let source = match name {
                Some(name) => Source::new(name, path.clone(), true),
                None => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    Source::new(name, path.clone(), true)
                }
            };
            let text = format!("Added {}", path.display());
            ctx.config.sources.push(source);
            text
        }
        SourceCommand::Remove { source } => {
            let path = find_source(&mut ctx.config, &source)?.path.clone();
            ctx.config
                .sources
                .retain(|s| !crate::engine::paths_equal(&s.path, &path));
            format!("Removed {} from the list", path.display())
        }
        SourceCommand::Enable { source } => {
            find_source(&mut ctx.config, &source)?.enabled = true;
            format!("{source} is backed up")
        }
        SourceCommand::Disable { source } => {
            find_source(&mut ctx.config, &source)?.enabled = false;
            format!("{source} is left out")
        }
        SourceCommand::Exclude { source, path } => {
            let s = find_source(&mut ctx.config, &source)?;
            selection::set_included(
                &mut s.include_paths,
                &mut s.exclude_paths,
                &relative(&path),
                false,
            );
            format!("{source}/{path} is left out")
        }
        SourceCommand::Include { source, path } => {
            let s = find_source(&mut ctx.config, &source)?;
            selection::set_included(
                &mut s.include_paths,
                &mut s.exclude_paths,
                &relative(&path),
                true,
            );
            format!("{source}/{path} is backed up")
        }
        SourceCommand::Encrypt { source, path } => {
            let s = find_source(&mut ctx.config, &source)?;
            let rel = relative(path.as_deref().unwrap_or(""));
            selection::set_marked(&mut s.encrypt_paths, &mut s.plain_paths, &rel, true);
            let mut text = format!("{source} {rel} is encrypted");
            if !ctx.config.encryption.selected() {
                text += " (takes effect with `encryption scope selected`)";
            }
            text
        }
        SourceCommand::Plain { source, path } => {
            let s = find_source(&mut ctx.config, &source)?;
            let rel = relative(path.as_deref().unwrap_or(""));
            selection::set_marked(&mut s.encrypt_paths, &mut s.plain_paths, &rel, false);
            format!("{source} {rel} is not encrypted")
        }
    };
    ctx.save()?;
    ctx.print(&serde_json::json!({"ok": true, "message": message}), || {
        message.clone()
    });
    Ok(Outcome::Ok)
}

#[derive(Serialize)]
struct AppView {
    id: String,
    name: String,
    folders: Vec<PathBuf>,
    added: bool,
    note: Option<String>,
}

fn app(ctx: &mut Ctx, command: AppCommand) -> anyhow::Result<Outcome> {
    let config_dir = ctx
        .paths
        .config_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let catalog = Catalog::load(&config_dir);
    let known = KnownPaths::current();
    match command {
        AppCommand::List => {
            let views: Vec<AppView> = catalog
                .detected(&known)
                .into_iter()
                .map(|(app, folders)| AppView {
                    id: app.id.clone(),
                    name: app.name.clone(),
                    added: folders.iter().all(|f| ctx.config.has_source_path(f)),
                    folders,
                    note: app.note_en.clone(),
                })
                .collect();
            ctx.print(&views, || {
                if views.is_empty() {
                    return "No known application was found.".into();
                }
                views
                    .iter()
                    .map(|v| {
                        format!(
                            "[{}] {:<20} {:<28} {}",
                            if v.added { "x" } else { " " },
                            v.id,
                            v.name,
                            v.folders
                                .iter()
                                .map(|f| f.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(Outcome::Ok)
        }
        AppCommand::Add { id } => {
            let app = catalog.get(&id).ok_or_else(|| {
                anyhow!("unknown application {id}; see `aeternavault-cli app list`")
            })?;
            let folders = app.present_folders(&known);
            if folders.is_empty() {
                bail!("{} was not found on this computer", app.name);
            }
            let mut added = Vec::new();
            for folder in folders {
                if ctx.config.has_source_path(&folder) {
                    continue;
                }
                let mut source = Source::new(app.name.clone(), folder.clone(), true);
                source.exclude = app.exclude.clone();
                ctx.config.sources.push(source);
                added.push(folder);
            }
            ctx.save()?;
            ctx.print(&serde_json::json!({"added": added}), || {
                let mut text = added
                    .iter()
                    .map(|f| format!("Added {}", f.display()))
                    .collect::<Vec<_>>()
                    .join("\n");
                if app.sensitive && !ctx.config.encryption.enabled {
                    text += "\nThis folder holds sensitive data; consider `aeternavault-cli encryption setup`.";
                }
                text
            });
            Ok(Outcome::Ok)
        }
        AppCommand::Remove { id } => {
            let app = catalog
                .get(&id)
                .ok_or_else(|| anyhow!("unknown application {id}"))?;
            let folders = app.present_folders(&known);
            ctx.config.sources.retain(|s| {
                !folders
                    .iter()
                    .any(|f| crate::engine::paths_equal(f, &s.path))
            });
            ctx.save()?;
            ctx.print(&serde_json::json!({"removed": folders}), || {
                format!("Removed the folders of {} from the list", app.name)
            });
            Ok(Outcome::Ok)
        }
    }
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct JobView {
    id: String,
    name: String,
    enabled: bool,
    when: String,
    folders: String,
    verify_after: bool,
    next: Option<String>,
    last: Option<String>,
    last_outcome: Option<AutomaticOutcome>,
}

fn job_views(ctx: &Ctx) -> Vec<JobView> {
    let state = State::load(&ctx.paths.config_file);
    let now = chrono::Local::now();
    ctx.config
        .schedules
        .iter()
        .map(|s| {
            let last = state.schedules.get(&s.id).and_then(|e| e.last_run.clone());
            JobView {
                id: s.id.clone(),
                name: automatic::describe(s, &ctx.config, Lang::En),
                enabled: s.enabled,
                when: automatic::when_text(s, Lang::En),
                folders: automatic::scope_text(s, &ctx.config, Lang::En),
                verify_after: s.verify_after,
                next: s
                    .enabled
                    .then(|| automatic::timing::next_occurrence(s, now))
                    .flatten()
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string()),
                last: last.as_ref().map(|r| local(r.at)),
                last_outcome: last.map(|r| r.outcome),
            }
        })
        .collect()
}

fn find_job<'a>(config: &'a Config, wanted: &str) -> anyhow::Result<&'a Schedule> {
    config
        .schedules
        .iter()
        .find(|s| s.id == wanted || s.name.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| anyhow!("no job {wanted}; see `aeternavault-cli job list`"))
}

fn apply_spec(config: &Config, schedule: &mut Schedule, spec: &JobSpec) -> anyhow::Result<()> {
    if let Some(every) = spec.every {
        schedule.frequency = match every {
            EveryArg::Day => Frequency::Daily,
            EveryArg::Week => Frequency::Weekly,
            EveryArg::Hours => Frequency::Hourly,
            EveryArg::Start => Frequency::AtStart,
        };
    }
    if let Some(at) = &spec.at {
        chrono::NaiveTime::parse_from_str(at, "%H:%M")
            .map_err(|_| anyhow!("--at needs a time like 20:00"))?;
        schedule.time = at.clone();
    }
    if let Some(day) = spec.day {
        use config::Weekday as W;
        schedule.weekday = match day {
            DayArg::Monday => W::Monday,
            DayArg::Tuesday => W::Tuesday,
            DayArg::Wednesday => W::Wednesday,
            DayArg::Thursday => W::Thursday,
            DayArg::Friday => W::Friday,
            DayArg::Saturday => W::Saturday,
            DayArg::Sunday => W::Sunday,
        };
    }
    if let Some(hours) = spec.hours {
        if !(1..=23).contains(&hours) {
            bail!("--hours must be between 1 and 23");
        }
        schedule.every_hours = hours;
    }
    if spec.all_folders {
        schedule.all_folders = true;
        schedule.folders.clear();
    }
    if !spec.folders.is_empty() {
        let mut folders = Vec::new();
        for wanted in &spec.folders {
            let path = PathBuf::from(wanted);
            let source = config
                .sources
                .iter()
                .find(|s| {
                    s.display_name().eq_ignore_ascii_case(wanted)
                        || crate::engine::paths_equal(&s.path, &path)
                })
                .ok_or_else(|| anyhow!("no folder named {wanted}"))?;
            folders.push(source.path.clone());
        }
        schedule.all_folders = false;
        schedule.folders = folders;
    }
    if let Some(name) = &spec.name {
        schedule.name = name.trim().to_string();
    }
    if spec.no_catch_up {
        schedule.catch_up = false;
    }
    if spec.verify {
        schedule.verify_after = true;
    }
    if spec.no_verify {
        schedule.verify_after = false;
    }
    if let Some(command) = &spec.before {
        schedule.before_command = if command.trim() == "-" {
            String::new()
        } else {
            command.trim().to_string()
        };
    }
    if let Some(dir) = &spec.destination {
        schedule.destination = if dir.as_os_str() == "-" {
            None
        } else {
            Some(std::path::absolute(dir)?)
        };
    }
    Ok(())
}

fn job(ctx: &mut Ctx, command: JobCommand) -> anyhow::Result<Outcome> {
    let arm = |paths: &AppPaths, id: &str| {
        let id = id.to_string();
        State::update(&paths.config_file, |state| {
            state.schedules.entry(id).or_default().armed_at = Some(chrono::Utc::now());
        });
    };
    let message = match command {
        JobCommand::List => {
            let views = job_views(ctx);
            ctx.print(&views, || {
                if views.is_empty() {
                    return "No jobs. Create one with `aeternavault-cli job add --every day --at 20:00`.".into();
                }
                views
                    .iter()
                    .map(|v| {
                        format!(
                            "[{}] {}  {}\n      next: {}  last: {}",
                            if v.enabled { "on " } else { "off" },
                            v.id,
                            v.name,
                            v.next.as_deref().unwrap_or("-"),
                            match (&v.last, v.last_outcome) {
                                (Some(at), Some(outcome)) => format!("{at} ({outcome:?})"),
                                _ => "-".into(),
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            return Ok(Outcome::Ok);
        }
        JobCommand::Add(spec) => {
            let mut schedule = Schedule {
                id: Schedule::new_id(),
                ..Schedule::default()
            };
            apply_spec(&ctx.config, &mut schedule, &spec)?;
            let label = automatic::describe(&schedule, &ctx.config, Lang::En);
            let id = schedule.id.clone();
            ctx.config.schedules.push(schedule);
            ctx.save()?;
            arm(&ctx.paths, &id);
            history::record(
                &ctx.paths.config_file,
                history::Event::JobCreated { job: label.clone() },
            );
            let mut text = format!("Created job {id}: {label}");
            if !background_on() {
                text += "\nJobs run while AeternaVault runs. Use `aeternavault-cli system background on` so they also run after closing it and after a restart.";
            }
            ctx.print(&serde_json::json!({"id": id, "name": label}), || {
                text.clone()
            });
            return Ok(Outcome::Ok);
        }
        JobCommand::Edit { job, spec } => {
            let id = find_job(&ctx.config, &job)?.id.clone();
            let config = ctx.config.clone();
            let schedule = ctx
                .config
                .schedules
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow!("no job {job}"))?;
            apply_spec(&config, schedule, &spec)?;
            let label = automatic::describe(schedule, &config, Lang::En);
            history::record(
                &ctx.paths.config_file,
                history::Event::JobChanged { job: label.clone() },
            );
            format!("Changed job {id}: {label}")
        }
        JobCommand::Remove { job } => {
            let schedule = find_job(&ctx.config, &job)?.clone();
            ctx.config.schedules.retain(|s| s.id != schedule.id);
            State::update(&ctx.paths.config_file, |state| {
                state.schedules.remove(&schedule.id);
            });
            let label = automatic::describe(&schedule, &ctx.config, Lang::En);
            history::record(
                &ctx.paths.config_file,
                history::Event::JobRemoved { job: label.clone() },
            );
            format!("Removed job {}: {label}", schedule.id)
        }
        JobCommand::Enable { job } => set_job(ctx, &job, true)?,
        JobCommand::Disable { job } => set_job(ctx, &job, false)?,
        JobCommand::Run { job } => return backup(ctx, false, Some(job)),
        JobCommand::RunDue => {
            let destination = ctx.config.destination.clone();
            let key = if ctx.config.encryption.enabled {
                ctx.remembered_key(&destination)
            } else {
                None
            };
            let runs = automatic::run_due(&ctx.paths, &ctx.config, key, boot_time());
            let failed = runs.iter().any(|r| !r.outcome.is_success());
            ctx.print(&runs, || {
                if runs.is_empty() {
                    return "No job is due.".into();
                }
                runs.iter()
                    .map(|r| format!("{}: {:?} ({} files)", r.schedule, r.outcome, r.files))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            return Ok(if failed { Outcome::Partly } else { Outcome::Ok });
        }
    };
    ctx.save()?;
    ctx.print(&serde_json::json!({"ok": true, "message": message}), || {
        message.clone()
    });
    Ok(Outcome::Ok)
}

fn set_job(ctx: &mut Ctx, wanted: &str, on: bool) -> anyhow::Result<String> {
    let id = find_job(&ctx.config, wanted)?.id.clone();
    let config = ctx.config.clone();
    let schedule = ctx
        .config
        .schedules
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow!("no job {wanted}"))?;
    let was = schedule.enabled;
    schedule.enabled = on;
    let label = automatic::describe(schedule, &config, Lang::En);
    if on && !was {
        State::update(&ctx.paths.config_file, |state| {
            state.schedules.entry(id.clone()).or_default().armed_at = Some(chrono::Utc::now());
        });
    }
    history::record(
        &ctx.paths.config_file,
        history::Event::JobSwitched {
            job: label.clone(),
            on,
        },
    );
    Ok(format!(
        "Job {id} is {}: {label}",
        if on { "on" } else { "off" }
    ))
}

/// When the computer started ("at start" jobs run once after that).
fn boot_time() -> chrono::DateTime<chrono::Local> {
    let uptime = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok());
    match uptime {
        Some(seconds) => chrono::Local::now() - chrono::Duration::seconds(seconds as i64),
        None => chrono::Local::now(),
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn retention(ctx: &mut Ctx, command: RetentionCommand) -> anyhow::Result<Outcome> {
    match command {
        RetentionCommand::Show => {}
        RetentionCommand::On => ctx.config.retention.enabled = true,
        RetentionCommand::Off => ctx.config.retention.enabled = false,
        RetentionCommand::Set {
            keep_last,
            days,
            weeks,
            months,
        } => {
            let r = &mut ctx.config.retention;
            if let Some(v) = keep_last {
                r.keep_last = v;
            }
            if let Some(v) = days {
                r.keep_daily = v;
            }
            if let Some(v) = weeks {
                r.keep_weekly = v;
            }
            if let Some(v) = months {
                r.keep_monthly = v;
            }
        }
    }
    ctx.save()?;
    let r = ctx.config.retention.clone();
    ctx.print(&r, || {
        format!(
            "Removing old backups: {}\nKeep the newest {}, one per day for {} days, per week for {} weeks, per month for {} months.",
            if r.enabled { "on" } else { "off" },
            r.keep_last,
            r.keep_daily,
            r.keep_weekly,
            r.keep_monthly
        )
    });
    Ok(Outcome::Ok)
}

fn destination(ctx: &mut Ctx, command: DestinationCommand) -> anyhow::Result<Outcome> {
    if let DestinationCommand::Set { path, app_folder } = command {
        let path = std::path::absolute(&path)?;
        let chosen = snapshots::chosen_destination(&path, app_folder);
        if ctx
            .config
            .enabled_sources()
            .any(|s| crate::engine::path_is_within(&chosen, &s.path))
        {
            bail!("the destination may not lie inside a folder that is backed up");
        }
        ctx.config.destination = chosen.clone();
        ctx.config.advanced.destination_app_folder = app_folder;
        ctx.save()?;
        history::record(
            &ctx.paths.config_file,
            history::Event::DestinationChanged {
                destination: chosen.display().to_string(),
            },
        );
    }
    let destination = ctx.config.destination.clone();
    let free = platform::free_space(&destination);
    ctx.print(
        &serde_json::json!({"destination": destination, "reachable": snapshots::destination_reachable(&destination), "free_bytes": free}),
        || {
            format!(
                "{}{}",
                destination.display(),
                free.map(|f| format!("  ({} free)", bytes(f)))
                    .unwrap_or_default()
            )
        },
    );
    Ok(Outcome::Ok)
}

fn encryption(ctx: &mut Ctx, command: EncryptionCommand) -> anyhow::Result<Outcome> {
    let destination = ctx.destination(&At::default())?;
    let key_dir = state::key_dir(&ctx.paths.config_file);
    let message = match command {
        EncryptionCommand::Status => {
            let header = vault::read_header(&destination).ok();
            let view = serde_json::json!({
                "encryption": encryption_word(&ctx.config),
                "vault": header.is_some(),
                "cipher": header.as_ref().and_then(|h| h.data_cipher().ok()).map(|c| c.display_name()),
                "key_remembered": header.as_ref().is_some_and(|h| vault::is_remembered(&key_dir, h)),
            });
            ctx.print(&view, || {
                format!(
                    "Encryption: {}\nVault: {}\nKey remembered on this computer: {}",
                    encryption_word(&ctx.config),
                    header
                        .as_ref()
                        .and_then(|h| h.data_cipher().ok())
                        .map(|c| c.display_name().to_string())
                        .unwrap_or_else(|| "not set up".into()),
                    if view["key_remembered"] == true {
                        "yes"
                    } else {
                        "no"
                    }
                )
            });
            return Ok(Outcome::Ok);
        }
        EncryptionCommand::Setup {
            cipher,
            strength,
            remember,
            recovery_file,
        } => {
            if vault::exists(&destination) {
                bail!("{} already has an encrypted vault", destination.display());
            }
            let passphrase = new_passphrase(ctx)?;
            let options = vault::VaultOptions {
                cipher: match cipher {
                    CipherArg::Xchacha20 => Cipher::XChaCha20Poly1305,
                    CipherArg::Aes256 => Cipher::Aes256Gcm,
                },
                kdf: match strength {
                    StrengthArg::Standard => KdfParams::PASSPHRASE,
                    StrengthArg::Strong => KdfParams::STRONG,
                    StrengthArg::VeryStrong => KdfParams::VERY_STRONG,
                },
            };
            std::fs::create_dir_all(&destination)?;
            let created = vault::create(&destination, &passphrase, options)?;
            if remember {
                vault::remember_key(&key_dir, &created.header, &created.key)?;
            }
            ctx.config.encryption.enabled = true;
            ctx.config.encryption.cipher = match cipher {
                CipherArg::Xchacha20 => config::CipherSetting::XChaCha20Poly1305,
                CipherArg::Aes256 => config::CipherSetting::Aes256Gcm,
            };
            ctx.save()?;
            history::record(
                &ctx.paths.config_file,
                history::Event::EncryptionSetUp {
                    cipher: options.cipher.display_name().into(),
                },
            );
            if let Some(file) = &recovery_file {
                write_recovery_file(file, &created.recovery_key, &destination)?;
            }
            ctx.print(
                &serde_json::json!({"recovery_key": created.recovery_key, "vault_id": created.header.vault_id}),
                || {
                    format!(
                        "Encryption is set up.\n\nRecovery key (shown once; keep it in a safe place):\n\n    {}\n",
                        created.recovery_key
                    )
                },
            );
            return Ok(Outcome::Ok);
        }
        EncryptionCommand::On => {
            if !vault::exists(&destination) {
                bail!("encryption is not set up yet; use `aeternavault-cli encryption setup`");
            }
            ctx.config.encryption.enabled = true;
            history::record(
                &ctx.paths.config_file,
                history::Event::EncryptionSwitched { on: true },
            );
            "New backups are encrypted".to_string()
        }
        EncryptionCommand::Off => {
            ctx.config.encryption.enabled = false;
            history::record(
                &ctx.paths.config_file,
                history::Event::EncryptionSwitched { on: false },
            );
            "New backups are not encrypted".to_string()
        }
        EncryptionCommand::Scope { scope } => {
            ctx.config.encryption.scope = match scope {
                ScopeArg::Everything => EncryptionScope::Everything,
                ScopeArg::Selected => EncryptionScope::Selected,
            };
            format!("Encrypted: {}", encryption_word(&ctx.config))
        }
        EncryptionCommand::ChangePassphrase => {
            let key = ctx
                .read_key(&destination)?
                .ok_or_else(|| anyhow!("encryption is not set up"))?;
            let passphrase = new_passphrase(ctx)?;
            vault::change_passphrase(&destination, &key, &passphrase)?;
            history::record(&ctx.paths.config_file, history::Event::PassphraseChanged);
            "The passphrase was changed".to_string()
        }
        EncryptionCommand::TestRecovery => {
            let Some(secret) = ctx.secret("Recovery key: ") else {
                bail!("no recovery key was entered");
            };
            match vault::check_secret(&destination, &secret)? {
                SlotKind::RecoveryKey => "The recovery key works".to_string(),
                SlotKind::Passphrase => {
                    bail!("that is the passphrase, not the recovery key")
                }
            }
        }
        EncryptionCommand::NewRecovery { recovery_file } => {
            let key = ctx
                .read_key(&destination)?
                .ok_or_else(|| anyhow!("encryption is not set up"))?;
            let recovery = vault::replace_recovery_key(&destination, &key)?;
            history::record(&ctx.paths.config_file, history::Event::RecoveryKeyReplaced);
            if let Some(file) = &recovery_file {
                write_recovery_file(file, &recovery, &destination)?;
            }
            ctx.print(&serde_json::json!({"recovery_key": recovery}), || {
                format!("New recovery key (the old one no longer works):\n\n    {recovery}\n")
            });
            return Ok(Outcome::Ok);
        }
        EncryptionCommand::Remember => {
            let key = ctx
                .read_key(&destination)?
                .ok_or_else(|| anyhow!("encryption is not set up"))?;
            let header = vault::read_header(&destination)?;
            vault::remember_key(&key_dir, &header, &key)?;
            "The key is remembered on this computer".to_string()
        }
        EncryptionCommand::Forget => {
            let header = vault::read_header(&destination)?;
            vault::forget_key(&key_dir, &header)?;
            "The key is no longer remembered".to_string()
        }
    };
    ctx.save()?;
    ctx.print(&serde_json::json!({"ok": true, "message": message}), || {
        message.clone()
    });
    Ok(Outcome::Ok)
}

/// A new passphrase: one line from standard input, or typed twice.
fn new_passphrase(ctx: &mut Ctx) -> anyhow::Result<String> {
    let first = ctx
        .secret("New passphrase: ")
        .ok_or_else(|| anyhow!("no passphrase was entered"))?;
    if first.is_empty() {
        bail!("the passphrase is empty");
    }
    if ctx.interactive() && std::env::var(PASSPHRASE_ENV).is_err() {
        let second = ctx.secret("Repeat the passphrase: ").unwrap_or_default();
        if second != first {
            bail!("the two passphrases differ");
        }
    }
    let assessment = crate::engine::passphrase::assess(&first, &[]);
    if assessment.rating() == crate::engine::passphrase::Rating::Weak && !ctx.json {
        eprintln!("Note: this passphrase is weak; a longer one is harder to guess.");
    }
    Ok(first)
}

fn write_recovery_file(file: &Path, key: &str, destination: &Path) -> anyhow::Result<()> {
    let text = Lang::En.recovery_file_text(key, "", &destination.display().to_string());
    std::fs::write(file, text.replace('\n', "\r\n"))
        .with_context(|| format!("could not write {}", file.display()))
}

fn config_command(ctx: &mut Ctx, command: ConfigCommand) -> anyhow::Result<Outcome> {
    let value = toml::Value::try_from(&ctx.config)?;
    match command {
        ConfigCommand::Show => {
            let json = serde_json::to_value(&ctx.config)?;
            ctx.print(&json, || ctx.config.to_toml().unwrap_or_default());
        }
        ConfigCommand::Path => {
            let path = ctx.paths.config_file.clone();
            ctx.print(&serde_json::json!({"path": path}), || {
                path.display().to_string()
            });
        }
        ConfigCommand::Get { key } => {
            let mut current = &value;
            for part in key.split('.') {
                current = current
                    .get(part)
                    .ok_or_else(|| anyhow!("unknown setting {key}"))?;
            }
            let json = serde_json::to_value(current)?;
            ctx.print(&json, || match current {
                toml::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
        ConfigCommand::Set { key, value: text } => {
            let mut root = value;
            let parts: Vec<&str> = key.split('.').collect();
            let (last, parents) = parts
                .split_last()
                .ok_or_else(|| anyhow!("no setting given"))?;
            let mut table = root
                .as_table_mut()
                .ok_or_else(|| anyhow!("unexpected configuration"))?;
            for part in parents {
                table = table
                    .get_mut(*part)
                    .and_then(toml::Value::as_table_mut)
                    .ok_or_else(|| anyhow!("unknown setting {key}"))?;
            }
            let old = table
                .get(*last)
                .ok_or_else(|| anyhow!("unknown setting {key}"))?;
            let new = match old {
                toml::Value::Boolean(_) => {
                    toml::Value::Boolean(match text.to_lowercase().as_str() {
                        "true" | "on" | "yes" | "1" => true,
                        "false" | "off" | "no" | "0" => false,
                        _ => bail!("{key} needs true or false"),
                    })
                }
                toml::Value::Integer(_) => toml::Value::Integer(
                    text.parse()
                        .with_context(|| format!("{key} needs a number"))?,
                ),
                toml::Value::Float(_) => toml::Value::Float(
                    text.parse()
                        .with_context(|| format!("{key} needs a number"))?,
                ),
                toml::Value::Array(_) => toml::Value::Array(
                    text.split(',')
                        .map(|s| toml::Value::String(s.trim().to_string()))
                        .filter(|v| v.as_str() != Some(""))
                        .collect(),
                ),
                toml::Value::Table(_) => {
                    bail!("{key} is a group of settings; set one of its values")
                }
                _ => toml::Value::String(text.clone()),
            };
            table.insert(last.to_string(), new);
            let mut config: Config = root.try_into().map_err(|e: toml::de::Error| {
                anyhow!("invalid value for {key}: {}", e.message())
            })?;
            config.normalize();
            ctx.config = config;
            ctx.save()?;
            ctx.print(&serde_json::json!({"ok": true}), || {
                format!("{key} = {text}")
            });
        }
    }
    Ok(Outcome::Ok)
}

fn history_command(
    ctx: &mut Ctx,
    last: usize,
    format: HistoryFormat,
    output: Option<PathBuf>,
) -> anyhow::Result<Outcome> {
    let entries = history::load(&ctx.paths.config_file);
    let start = if last == 0 {
        0
    } else {
        entries.len().saturating_sub(last)
    };
    let entries = &entries[start..];
    if format != HistoryFormat::Text || output.is_some() {
        let lang = Lang::resolve(ctx.config.language);
        let rows = history::export_rows(entries, lang);
        let text = match format {
            HistoryFormat::Csv => history::to_csv(&rows),
            HistoryFormat::Json => serde_json::to_string_pretty(&entries)?,
            HistoryFormat::Text => history::to_text(&rows),
        };
        match output {
            Some(file) => {
                std::fs::write(&file, text)
                    .with_context(|| format!("could not write {}", file.display()))?;
                if !ctx.json {
                    println!("{} entries written to {}", rows.len(), file.display());
                }
            }
            None => print!("{text}"),
        }
        return Ok(Outcome::Ok);
    }
    ctx.print(&entries, || {
        entries
            .iter()
            .map(|e| {
                let (title, detail, _) = Lang::En.history_entry(&e.event);
                if detail.is_empty() {
                    format!("{}  {title}", local(e.at))
                } else {
                    format!("{}  {title}\n                  {detail}", local(e.at))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(Outcome::Ok)
}

fn paths_command(ctx: &mut Ctx) -> anyhow::Result<Outcome> {
    let view = serde_json::json!({
        "configuration": ctx.paths.config_file,
        "history": history::file(&ctx.paths.config_file),
        "logs": ctx.paths.log_dir,
        "portable": ctx.paths.portable,
    });
    ctx.print(&view, || {
        format!(
            "configuration: {}\nhistory:       {}\nlogs:          {}",
            ctx.paths.config_file.display(),
            history::file(&ctx.paths.config_file).display(),
            ctx.paths.log_dir.display()
        )
    });
    Ok(Outcome::Ok)
}

// ---------------------------------------------------------------------------
// System integration
// ---------------------------------------------------------------------------

fn background_on() -> bool {
    if cfg!(windows) {
        platform::autostart::is_enabled()
    } else {
        platform::systemd::is_installed()
    }
}

fn system(ctx: &mut Ctx, command: SystemCommand) -> anyhow::Result<Outcome> {
    let (name, on) = match command {
        SystemCommand::Background { state } => {
            if let Some(state) = state {
                let on = state == OnOff::On;
                if cfg!(windows) {
                    platform::autostart::set_enabled(on)?;
                    ctx.config.background.keep_running |= on;
                    ctx.save()?;
                } else if on {
                    let cli = platform::cli_executable()?;
                    platform::systemd::install(&cli, 5)?;
                } else {
                    platform::systemd::remove()?;
                }
                history::record(
                    &ctx.paths.config_file,
                    history::Event::StartWithWindows { on },
                );
            }
            ("background", background_on())
        }
        SystemCommand::DoubleClick { state } => {
            if !platform::file_association::supported() {
                bail!("only available on Windows");
            }
            if let Some(state) = state {
                let on = state == OnOff::On;
                platform::file_association::set_registered(on)?;
                ctx.config.advanced.open_by_double_click = on;
                ctx.save()?;
            }
            ("double-click", platform::file_association::is_registered())
        }
        SystemCommand::ExplorerMenu { state } => {
            if !cfg!(windows) {
                bail!("only available on Windows");
            }
            if let Some(state) = state {
                let on = state == OnOff::On;
                platform::context_menu::set_registered(
                    on,
                    Lang::resolve(ctx.config.language).t().explorer_menu_label,
                )?;
                ctx.config.advanced.explorer_menu = on;
                ctx.save()?;
            }
            ("explorer-menu", platform::context_menu::is_registered())
        }
        SystemCommand::Cleanup => {
            let mut problems = Vec::new();
            if cfg!(windows) {
                if let Err(err) = platform::autostart::unregister() {
                    problems.push(format!("autostart: {err}"));
                }
                if let Err(err) = platform::file_association::set_registered(false) {
                    problems.push(format!("double-click: {err}"));
                }
                if let Err(err) = platform::context_menu::set_registered(false, "") {
                    problems.push(format!("Explorer menu: {err}"));
                }
            } else if platform::systemd::is_installed()
                && let Err(err) = platform::systemd::remove()
            {
                problems.push(format!("timer: {err}"));
            }
            ctx.print(&serde_json::json!({"problems": problems}), || {
                if problems.is_empty() {
                    "Removed everything AeternaVault registered in the system.".into()
                } else {
                    problems.join("\n")
                }
            });
            return Ok(if problems.is_empty() {
                Outcome::Ok
            } else {
                Outcome::Partly
            });
        }
    };
    let mut view = serde_json::Map::new();
    view.insert(name.to_string(), serde_json::Value::Bool(on));
    ctx.print(&view, || {
        format!("{name}: {}", if on { "on" } else { "off" })
    });
    Ok(Outcome::Ok)
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

fn find_channel(config: &Config, wanted: &str) -> anyhow::Result<usize> {
    let wanted = wanted.trim();
    config
        .notifications
        .channels
        .iter()
        .position(|c| c.label().eq_ignore_ascii_case(wanted))
        .or_else(|| {
            wanted
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=config.notifications.channels.len()).contains(n))
                .map(|n| n - 1)
        })
        .ok_or_else(|| anyhow!("no channel named {wanted:?}; see `aeternavault-cli notify list`"))
}

#[derive(Serialize)]
struct ChannelView {
    number: usize,
    name: String,
    kind: crate::notify::ChannelKind,
    enabled: bool,
    target: String,
}

fn notify_command(ctx: &mut Ctx, command: NotifyCommand) -> anyhow::Result<Outcome> {
    use crate::notify::{self, Channel, ChannelKind};
    let command_kind = match &command {
        NotifyCommand::Enable { .. } => "enable",
        _ => "",
    };
    match command {
        NotifyCommand::List => {
            let n = &ctx.config.notifications;
            let views: Vec<ChannelView> = n
                .channels
                .iter()
                .enumerate()
                .map(|(i, c)| ChannelView {
                    number: i + 1,
                    name: c.label(),
                    kind: c.kind,
                    enabled: c.enabled,
                    target: match c.kind {
                        ChannelKind::Email => c.to.clone(),
                        ChannelKind::Telegram => format!("chat {}", c.chat_id),
                        _ => c.url.clone(),
                    },
                })
                .collect();
            let rules = serde_json::json!({
                "on_success": n.on_success,
                "on_problem": n.on_problem,
                "on_skipped": n.on_skipped,
                "low_space_percent": n.low_space_percent,
                "low_space_gb": n.low_space_gb,
                "channels": views,
            });
            ctx.print(&rules, || {
                let mut out = format!(
                    "On success: {}  On problems: {}  When skipped: {}  Low space below: {} % / {} GB\n",
                    n.on_success, n.on_problem, n.on_skipped, n.low_space_percent, n.low_space_gb
                );
                if views.is_empty() {
                    out += "No channels. Add one with `aeternavault-cli notify add --kind …`.";
                }
                for v in &views {
                    out += &format!(
                        "{:>2}  {:<20} {:<16} {:<4} {}\n",
                        v.number,
                        v.name,
                        v.kind.name(),
                        if v.enabled { "on" } else { "off" },
                        v.target
                    );
                }
                out.trim_end().to_string()
            });
            Ok(Outcome::Ok)
        }
        NotifyCommand::Test { channel } => {
            let lang = Lang::resolve(ctx.config.language);
            let message = notify::test_message(lang);
            let targets: Vec<Channel> = match channel {
                Some(wanted) => {
                    vec![
                        ctx.config.notifications.channels[find_channel(&ctx.config, &wanted)?]
                            .clone(),
                    ]
                }
                None => ctx
                    .config
                    .notifications
                    .channels
                    .iter()
                    .filter(|c| c.enabled)
                    .cloned()
                    .collect(),
            };
            if targets.is_empty() {
                bail!("no enabled channel; add one with `aeternavault-cli notify add`");
            }
            let mut results = Vec::new();
            for c in &targets {
                let result = notify::send(c, &message);
                results.push(serde_json::json!({
                    "channel": c.label(),
                    "ok": result.is_ok(),
                    "error": result.as_ref().err().map(|e| format!("{e:#}")),
                }));
            }
            let failed = results.iter().filter(|r| r["ok"] == false).count();
            ctx.print(&results, || {
                results
                    .iter()
                    .map(|r| match r["error"].as_str() {
                        None => format!("sent:   {}", r["channel"].as_str().unwrap_or_default()),
                        Some(e) => {
                            format!("failed: {}: {e}", r["channel"].as_str().unwrap_or_default())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            Ok(if failed == 0 {
                Outcome::Ok
            } else {
                Outcome::Partly
            })
        }
        NotifyCommand::Add(args) => {
            let args = *args;
            let mut channel = Channel {
                name: args.name.unwrap_or_default(),
                kind: args.kind,
                url: args.url.unwrap_or_default(),
                token: args.token.unwrap_or_default(),
                chat_id: args.chat_id.unwrap_or_default(),
                smtp_host: args.smtp_host.unwrap_or_default(),
                smtp_security: args.smtp_security.unwrap_or_default(),
                smtp_user: args.smtp_user.unwrap_or_default(),
                smtp_password: args.smtp_password.unwrap_or_default(),
                from: args.from.unwrap_or_default(),
                to: args.to.unwrap_or_default(),
                ..Channel::default()
            };
            if let Some(port) = args.smtp_port {
                channel.smtp_port = port;
            } else if channel.smtp_security == notify::SmtpSecurity::Tls {
                channel.smtp_port = 465;
            }
            if channel.kind == ChannelKind::Email
                && channel.smtp_password.is_empty()
                && ctx.secret_stdin
            {
                channel.smtp_password = ctx.secret("SMTP password: ").unwrap_or_default();
            }
            let label = channel.label();
            ctx.config.notifications.channels.push(channel);
            ctx.save()?;
            ctx.print(&serde_json::json!({ "added": label }), || {
                format!(
                    "Channel {label} added. Try it with `aeternavault-cli notify test \"{label}\"`."
                )
            });
            Ok(Outcome::Ok)
        }
        NotifyCommand::Remove { channel } => {
            let i = find_channel(&ctx.config, &channel)?;
            let removed = ctx.config.notifications.channels.remove(i);
            ctx.save()?;
            let label = removed.label();
            ctx.print(&serde_json::json!({ "removed": label }), || {
                format!("Channel {label} removed.")
            });
            Ok(Outcome::Ok)
        }
        NotifyCommand::Enable { channel } | NotifyCommand::Disable { channel } => {
            let on = matches!(command_kind, "enable");
            let i = find_channel(&ctx.config, &channel)?;
            ctx.config.notifications.channels[i].enabled = on;
            ctx.save()?;
            let label = ctx.config.notifications.channels[i].label();
            ctx.print(
                &serde_json::json!({ "channel": label, "enabled": on }),
                || {
                    format!(
                        "Channel {label} {}.",
                        if on { "enabled" } else { "disabled" }
                    )
                },
            );
            Ok(Outcome::Ok)
        }
    }
}
