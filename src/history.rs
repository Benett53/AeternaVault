//! The activity history: what happened, kept across sessions.
//!
//! One JSON object per line in `history.jsonl` next to the configuration.
//! Events are stored as data (not as sentences), so the Activity view shows
//! them in the current language. The window, the background service and the
//! command line all append to the same file; appending one short line at a
//! time keeps concurrent writers from mixing their entries.
//!
//! The technical log files (`logs\`) stay separate and are rotated after 14 days.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const FILE: &str = "history.jsonl";
/// Entries kept when the file is tidied up.
const KEEP: usize = 5000;
/// The file is tidied up when it has grown beyond this many entries.
const TIDY_AFTER: usize = 6000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Complete,
    Notes,
    Cancelled,
    Skipped,
    NeedsPassphrase,
    Failed,
}

impl Outcome {
    pub fn from_automatic(outcome: crate::state::AutomaticOutcome) -> Self {
        use crate::state::AutomaticOutcome as A;
        match outcome {
            A::Complete => Outcome::Complete,
            A::CompleteWithNotes => Outcome::Notes,
            A::DestinationUnavailable | A::AlreadyRunning => Outcome::Skipped,
            A::NeedsPassphrase => Outcome::NeedsPassphrase,
            A::Interrupted => Outcome::Cancelled,
            A::Failed => Outcome::Failed,
        }
    }

    pub fn from_status(status: crate::engine::manifest::SnapshotStatus) -> Self {
        use crate::engine::manifest::SnapshotStatus as S;
        match status {
            S::Complete => Outcome::Complete,
            S::CompleteWithWarnings => Outcome::Notes,
            S::Cancelled => Outcome::Cancelled,
            S::Failed => Outcome::Failed,
        }
    }
}

/// How an entry is marked in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Good,
    Neutral,
    Warning,
    Problem,
}

impl Outcome {
    pub fn severity(self) -> Severity {
        match self {
            Outcome::Complete => Severity::Good,
            Outcome::Notes | Outcome::Skipped | Outcome::Cancelled => Severity::Warning,
            Outcome::NeedsPassphrase | Outcome::Failed => Severity::Problem,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    Backup {
        /// Name of the job for automatic backups; empty when started by hand.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        job: String,
        #[serde(default)]
        command_line: bool,
        outcome: Outcome,
        #[serde(default)]
        files: u64,
        #[serde(default)]
        bytes: u64,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        snapshot: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
    },
    Restore {
        outcome: Outcome,
        snapshot: String,
        /// Empty: to the original places.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        target: String,
        #[serde(default)]
        files: u64,
        #[serde(default)]
        bytes: u64,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
    },
    BackupsDeleted {
        snapshots: Vec<String>,
        /// Removed by the rules for keeping old backups.
        #[serde(default)]
        by_rules: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
    },
    BackupMoved {
        snapshot: String,
        to: String,
    },
    FilesCopied {
        snapshot: String,
        files: u64,
        to: String,
    },
    Verified {
        snapshot: String,
        files: u64,
        damaged: u64,
        missing: u64,
    },
    JobCreated {
        job: String,
    },
    JobChanged {
        job: String,
    },
    JobRemoved {
        job: String,
    },
    JobSwitched {
        job: String,
        on: bool,
    },
    EncryptionSetUp {
        cipher: String,
    },
    EncryptionSwitched {
        on: bool,
    },
    PassphraseChanged,
    RecoveryKeyReplaced,
    DestinationChanged {
        destination: String,
    },
    StartWithWindows {
        on: bool,
    },
    /// Written by a newer version; shown as "other".
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: Event,
}

/// A backup started by hand (window or command line).
pub fn backup_event(
    result: &crate::error::EngineResult<crate::engine::backup::BackupReport>,
    command_line: bool,
) -> Event {
    match result {
        Ok(report) => {
            let stats = report.total_stats();
            Event::Backup {
                job: String::new(),
                command_line,
                outcome: Outcome::from_status(report.header.status),
                files: stats.files,
                bytes: stats.bytes,
                snapshot: report
                    .snapshot_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                message: report.warnings.first().cloned().unwrap_or_default(),
            }
        }
        Err(err) => Event::Backup {
            job: String::new(),
            command_line,
            outcome: if matches!(err, crate::error::EngineError::Cancelled) {
                Outcome::Cancelled
            } else {
                Outcome::Failed
            },
            files: 0,
            bytes: 0,
            snapshot: String::new(),
            message: err.to_string(),
        },
    }
}

/// `target` is empty for the original places.
pub fn restore_event(
    result: &crate::error::EngineResult<crate::engine::restore::RestoreReport>,
    snapshot: &str,
    target: &str,
) -> Event {
    match result {
        Ok(report) => Event::Restore {
            outcome: if report.cancelled {
                Outcome::Cancelled
            } else if report.failed > 0 {
                Outcome::Notes
            } else {
                Outcome::Complete
            },
            snapshot: snapshot.to_string(),
            target: target.to_string(),
            files: report.restored_files,
            bytes: report.restored_bytes,
            message: report.warnings.first().cloned().unwrap_or_default(),
        },
        Err(err) => Event::Restore {
            outcome: Outcome::Failed,
            snapshot: snapshot.to_string(),
            target: target.to_string(),
            files: 0,
            bytes: 0,
            message: err.to_string(),
        },
    }
}

pub fn file(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .map(|dir| dir.join(FILE))
        .unwrap_or_else(|| PathBuf::from(FILE))
}

/// Appends one event. Failures are logged, never shown: the history must not
/// get in the way of a backup.
pub fn record(config_file: &Path, event: Event) -> Entry {
    let entry = Entry {
        at: Utc::now(),
        event,
    };
    if let Err(err) = append(&file(config_file), &entry) {
        tracing::warn!("activity history could not be written: {err}");
    }
    entry
}

fn append(path: &Path, entry: &Entry) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // One write call per line, so lines of different processes do not interleave.
    file.write_all(line.as_bytes())
}

/// All entries, oldest first. Unreadable lines are skipped.
pub fn load(config_file: &Path) -> Vec<Entry> {
    let path = file(config_file);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line.trim_start_matches('\u{feff}')).ok())
        .collect();
    if entries.len() > TIDY_AFTER {
        entries.drain(..entries.len() - KEEP);
        if let Err(err) = rewrite(&path, &entries) {
            tracing::debug!("activity history could not be shortened: {err}");
        }
    }
    entries
}

fn rewrite(path: &Path, entries: &[Entry]) -> std::io::Result<()> {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&serde_json::to_string(entry).map_err(std::io::Error::other)?);
        text.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_survive_a_restart_and_unknown_lines_are_tolerated() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.toml");
        record(
            &config,
            Event::Backup {
                job: String::new(),
                command_line: false,
                outcome: Outcome::Complete,
                files: 3,
                bytes: 1200,
                snapshot: "2026-09-16 20-00".into(),
                message: String::new(),
            },
        );
        record(
            &config,
            Event::JobCreated {
                job: "Every day at 20:00".into(),
            },
        );
        // A later version may write events this one does not know, or a line may be cut off.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(file(&config))
            .unwrap();
        writeln!(
            file,
            r#"{{"at":"2026-09-16T20:00:00Z","event":"from-the-future"}}"#
        )
        .unwrap();
        writeln!(file, r#"{{"at":"2026-09-16T20:00"#).unwrap();

        let entries = load(&config);
        assert_eq!(entries.len(), 3);
        assert!(matches!(entries[0].event, Event::Backup { files: 3, .. }));
        assert_eq!(entries[2].event, Event::Other);
    }
}

// ---------------------------------------------------------------------------
// Export (activity tab, `aeternavault-cli history --format`, notifications)
// ---------------------------------------------------------------------------

/// One line of an exported history.
#[derive(Debug, Clone)]
pub struct ExportRow {
    pub at: chrono::DateTime<chrono::Local>,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
}

impl Severity {
    pub fn code(self) -> &'static str {
        match self {
            Severity::Good => "ok",
            Severity::Neutral => "info",
            Severity::Warning => "warning",
            Severity::Problem => "error",
        }
    }
}

/// The entries as rows in the given language, oldest first.
pub fn export_rows(entries: &[Entry], lang: crate::i18n::Lang) -> Vec<ExportRow> {
    entries
        .iter()
        .map(|e| {
            let (title, detail, severity) = lang.history_entry(&e.event);
            ExportRow {
                at: e.at.with_timezone(&chrono::Local),
                severity,
                title,
                detail,
            }
        })
        .collect()
}

/// Plain text, one entry per line (detail indented below).
pub fn to_text(rows: &[ExportRow]) -> String {
    let mut out = String::new();
    for row in rows {
        out += &format!(
            "{}  {:<7}  {}\n",
            row.at.format("%Y-%m-%d %H:%M:%S"),
            row.severity.code(),
            row.title
        );
        if !row.detail.is_empty() {
            for line in row.detail.lines() {
                out += &format!("                              {line}\n");
            }
        }
    }
    out
}

/// CSV (RFC 4180) with a header line; opens in Excel and LibreOffice.
pub fn to_csv(rows: &[ExportRow]) -> String {
    fn field(text: &str) -> String {
        if text.contains([',', '"', '\n', '\r', ';']) {
            format!("\"{}\"", text.replace('"', "\"\""))
        } else {
            text.to_string()
        }
    }
    let mut out = String::from("time,severity,event,detail\r\n");
    for row in rows {
        out += &format!(
            "{},{},{},{}\r\n",
            row.at.format("%Y-%m-%d %H:%M:%S"),
            row.severity.code(),
            field(&row.title),
            field(&row.detail)
        );
    }
    out
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn csv_quotes_fields() {
        let rows = vec![ExportRow {
            at: chrono::Local::now(),
            severity: Severity::Problem,
            title: "Backup \"A\" failed".into(),
            detail: "disk full, 3 files".into(),
        }];
        let csv = to_csv(&rows);
        assert!(csv.starts_with("time,severity,event,detail\r\n"));
        assert!(csv.contains(",error,\"Backup \"\"A\"\" failed\",\"disk full, 3 files\"\r\n"));
        assert!(to_text(&rows).contains("error"));
    }
}
