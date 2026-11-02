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
use sha2::{Digest, Sha256};

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
    /// Every reused stored copy was read and checked (see
    /// `Advanced::health_check_days`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub health_check: bool,
    /// Encrypted part of a partly encrypted backup: digest of the readable
    /// part (see [`plain_digest`]), so that it cannot be edited or added later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_digest: Option<String>,
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
    /// Linux permission bits of the original (`0o640` …), restored with the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
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

/// What decides where the files of a readable part go and what they contain:
/// its folders and, per file, name, size, time, permissions and SHA-256. Where a plain
/// file is stored (`blob`) is left out, as it changes when older backups are
/// deleted.
pub fn plain_digest(header: &SnapshotHeader, index: &FileIndex) -> String {
    let sources: Vec<_> = header
        .sources
        .iter()
        .map(|s| (&s.key, &s.path, &s.portable))
        .collect();
    let files: Vec<_> = index
        .files
        .iter()
        .map(|f| (&f.source, &f.path, f.size, f.modified, &f.sha256, f.mode))
        .collect();
    let bytes = serde_json::to_vec(&(&header.id, sources, files)).unwrap_or_default();
    super::format_sha256(&Sha256::digest(bytes))
}

/// Whether the readable part of a backup belongs to its encrypted part. The
/// readable part is not authenticated by itself: anyone who can write to the
/// storage could add or edit one. The encrypted header decides.
pub fn check_plain_part(
    plain: &SnapshotHeader,
    index: &FileIndex,
    encrypted: &SnapshotHeader,
) -> Result<(), &'static str> {
    if !encrypted.split {
        return Err("readable files were added to a fully encrypted backup");
    }
    if plain.id != encrypted.id {
        return Err("the parts of this backup do not belong together");
    }
    match &encrypted.plain_digest {
        Some(digest) if *digest != plain_digest(plain, index) => {
            Err("the readable part of this backup was changed")
        }
        Some(_) => Ok(()),
        // Backups of version 0.8.1 and older: the readable part may only use
        // folders that the encrypted header names.
        None => {
            let known = |r: &SourceRecord| encrypted.sources.iter().any(|e| e == r);
            if plain.sources.iter().all(known) {
                Ok(())
            } else {
                Err("the readable part of this backup names other folders")
            }
        }
    }
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
