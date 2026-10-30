//! On-disk format of a backup ("snapshot"). Read-only helpers only.
//!
//! Every backup is one folder named after the time it was made:
//!
//! ```text
//! <destination>\
//!   2026-09-14 20-00\                  plain backup
//!     Documents\…                      your folders, directly browsable
//!     .aeternavault\                   hidden: snapshot.json, files.json
//!   2026-09-15 20-00\                  encrypted backup
//!     Open with AeternaVault.avault    double-click to browse
//!     .aeternavault\encrypted.avs      encrypted header and file index
//!   .aeternavault\                     hidden, shared by all encrypted backups:
//!     vault.json                       wrapped keys (see `vault`)
//!     blobs\3f\3fa9…c1.avb             encrypted file contents
//! ```
//!
//! A partly encrypted backup has both: plain folders plus `snapshot.json`
//! and `encrypted.avs` in the same folder, each marked with `split: true`.
//! If a destination is shared by several computers, backups of other
//! computers get the computer name appended: `2026-09-14 20-00 (LAPTOP)`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::crypto::VaultKey;
use crate::config::BackupMode;
use crate::error::{EngineError, EngineResult};
use crate::platform::known_paths::KnownPaths;

pub const FORMAT_VERSION: u32 = 3;
pub const HEADER_FILE: &str = "snapshot.json";
pub const INDEX_FILE: &str = "files.json";
pub const ENCRYPTED_FILE: &str = "encrypted.avs";
pub const META_DIR: &str = ".aeternavault";
/// A name given to a backup afterwards (plain text, also for encrypted backups).
pub const LABEL_FILE: &str = "label.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotStatus {
    Complete,
    CompleteWithWarnings,
    Cancelled,
    Failed,
}

impl SnapshotStatus {
    /// Whether this snapshot may serve as the base of an incremental backup.
    pub fn is_usable_base(self) -> bool {
        matches!(self, Self::Complete | Self::CompleteWithWarnings)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// Folder inside the snapshot, `/`-separated.
    pub key: String,
    pub name: String,
    /// Absolute path on the computer that made the backup.
    pub path: PathBuf,
    /// Portable form (`{DOCUMENTS}\…`) used to restore for another user name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotStats {
    pub files: u64,
    pub bytes: u64,
    pub copied_files: u64,
    pub copied_bytes: u64,
    pub linked_files: u64,
    pub referenced_files: u64,
    pub skipped: u64,
    pub failed: u64,
}

impl SnapshotStats {
    pub fn add(&mut self, other: &SnapshotStats) {
        self.files += other.files;
        self.bytes += other.bytes;
        self.copied_files += other.copied_files;
        self.copied_bytes += other.copied_bytes;
        self.linked_files += other.linked_files;
        self.referenced_files += other.referenced_files;
        self.skipped += other.skipped;
        self.failed += other.failed;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub format: u32,
    pub app_version: String,
    pub id: String,
    /// Name of the backup job that made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub computer: String,
    pub user: String,
    pub mode: BackupMode,
    pub status: SnapshotStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Snapshot that unchanged files were compared against.
    pub base: Option<String>,
    #[serde(default)]
    pub encrypted: bool,
    /// Partly encrypted backup: a plain and an encrypted part share the folder.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub split: bool,
    pub sources: Vec<SourceRecord>,
    /// Known folders of the backed-up user, for remapping on restore.
    #[serde(default)]
    pub known_paths: KnownPaths,
    pub stats: SnapshotStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Source key (see [`SourceRecord::key`]).
    pub source: String,
    /// Path relative to the source folder, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Last-modified time in nanoseconds since the Unix epoch.
    pub modified: i64,
    pub sha256: String,
    /// Where the content lives:
    /// * plain: relative to the destination, e.g. `2026-09-12 10-10/Documents/a.txt`
    ///   (may point into an older backup on file systems without hard links),
    /// * encrypted: the blob id.
    pub blob: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileIndex {
    pub format: u32,
    pub files: Vec<FileEntry>,
    pub warnings: Vec<String>,
}

/// Content of an encrypted `encrypted.avs` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedSnapshot {
    pub header: SnapshotHeader,
    pub index: FileIndex,
}

pub fn read_header(meta_dir: &Path) -> EngineResult<SnapshotHeader> {
    read_json(&meta_dir.join(HEADER_FILE))
}

pub fn read_index(meta_dir: &Path) -> EngineResult<FileIndex> {
    read_json(&meta_dir.join(INDEX_FILE))
}

pub fn read_encrypted(path: &Path, key: &VaultKey) -> EngineResult<EncryptedSnapshot> {
    let manifest_error = |message: String| EngineError::Manifest {
        path: path.to_path_buf(),
        message,
    };
    let sealed = std::fs::read(path).map_err(|e| manifest_error(e.to_string()))?;
    let plain = key
        .decrypt_bytes(&sealed)
        .map_err(|e| manifest_error(e.to_string()))?;
    serde_json::from_slice(&plain).map_err(|e| manifest_error(e.to_string()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> EngineResult<T> {
    let file = std::fs::File::open(path).map_err(|e| EngineError::Manifest {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    serde_json::from_reader(std::io::BufReader::new(file)).map_err(|e| EngineError::Manifest {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}
