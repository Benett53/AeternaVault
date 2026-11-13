//! Small persistent state next to the configuration: when each automatic
//! backup last ran and how it went, so the window can report it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const STATE_FILE: &str = "state.json";

/// The window and the background service both update the file.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutomaticOutcome {
    Complete,
    CompleteWithNotes,
    /// The destination (e.g. an external drive) was not connected.
    DestinationUnavailable,
    /// Encrypted backups are on, but the key is not available.
    NeedsPassphrase,
    AlreadyRunning,
    /// Stopped before it finished (window closed, computer shut down); made
    /// up later like a skipped run.
    Interrupted,
    Failed,
}

impl AutomaticOutcome {
    pub fn is_success(self) -> bool {
        matches!(self, Self::Complete | Self::CompleteWithNotes)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomaticRun {
    pub at: DateTime<Utc>,
    pub outcome: AutomaticOutcome,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub bytes: u64,
    /// Name or description of the schedule that ran.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schedule: String,
    /// What went wrong, item by item, for notifications and the log.
    #[serde(default, skip_serializing_if = "Problems::is_empty")]
    pub problems: Problems,
}

/// The problems of one run, each entry `item: reason`, sorted by kind so that
/// a message can say what to do about them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Problems {
    /// Why the run stopped, explained in the user's language. It names no
    /// file of a backed-up folder, so it is also sent with encryption on.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Folders to back up that could not be read at all.
    pub folders_missing: Vec<String>,
    /// Everything the backup could not read or store, folders included.
    pub not_saved: Vec<String>,
    /// Check after the backup: stored copies that could not be read.
    pub check_unreadable: Vec<String>,
    /// … whose content does not match the checksum taken when they were saved.
    pub check_damaged: Vec<String>,
    /// … that are not there.
    pub check_missing: Vec<String>,
}

impl Problems {
    pub fn is_empty(&self) -> bool {
        self.error.is_empty()
            && self.folders_missing.is_empty()
            && self.not_saved.is_empty()
            && self.check_unreadable.is_empty()
            && self.check_damaged.is_empty()
            && self.check_missing.is_empty()
    }

    pub fn from_error(error: &crate::error::EngineError, lang: crate::i18n::Lang) -> Self {
        Self {
            error: lang.error_message(error),
            ..Self::default()
        }
    }

    pub fn from_backup(report: &crate::engine::backup::BackupReport) -> Self {
        Self {
            folders_missing: report.unread_folders.clone(),
            not_saved: report.warnings.clone(),
            ..Self::default()
        }
    }

    pub fn add_check(&mut self, check: &crate::engine::verify::VerifyReport) {
        self.check_unreadable = check.unreadable.clone();
        self.check_damaged = check.damaged.clone();
        self.check_missing = check.missing.clone();
    }
}

/// Bookkeeping for one schedule (keyed by its id).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleState {
    /// When the schedule was switched on; occurrences before this are not caught up.
    pub armed_at: Option<DateTime<Utc>>,
    /// Last time a run was started (whatever the outcome).
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_run: Option<AutomaticRun>,
    /// Last run that completed (with or without notes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success: Option<DateTime<Utc>>,
    /// Last alert that this schedule is overdue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overdue_alerted: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub last_automatic: Option<AutomaticRun>,
    /// When the user last saw the result in the window.
    pub acknowledged_at: Option<DateTime<Utc>>,
    /// Executable path the scheduled task of AeternaVault 0.2 was created for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_executable: Option<PathBuf>,
    pub schedules: BTreeMap<String, ScheduleState>,
}

impl State {
    pub fn path(config_file: &Path) -> PathBuf {
        config_file
            .parent()
            .map(|p| p.join(STATE_FILE))
            .unwrap_or_else(|| PathBuf::from(STATE_FILE))
    }

    pub fn load(config_file: &Path) -> Self {
        std::fs::read_to_string(Self::path(config_file))
            .ok()
            .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default()
    }

    /// Loads the current file, applies `change` and saves it again, so changes
    /// from the window and the background service do not overwrite each other.
    pub fn update(config_file: &Path, change: impl FnOnce(&mut State)) -> State {
        let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = Self::load(config_file);
        change(&mut state);
        state.write(config_file);
        state
    }

    fn write(&self, config_file: &Path) {
        let path = Self::path(config_file);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                let tmp = path.with_extension("json.tmp");
                let result =
                    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path));
                if let Err(err) = result {
                    tracing::warn!("state could not be saved: {err}");
                }
            }
            Err(err) => tracing::warn!("state could not be encoded: {err}"),
        }
    }

    /// An automatic backup result the user has not seen yet.
    pub fn unseen_automatic(&self) -> Option<&AutomaticRun> {
        let run = self.last_automatic.as_ref()?;
        match self.acknowledged_at {
            Some(seen) if seen >= run.at => None,
            _ => Some(run),
        }
    }
}

/// Folder for remembered vault keys (protected with DPAPI).
pub fn key_dir(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .map(|p| p.join("keys"))
        .unwrap_or_else(|| PathBuf::from("keys"))
}
