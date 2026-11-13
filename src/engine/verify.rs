//! Checking a backup without restoring it. Read-only.
//!
//! Every stored file is read again: plain files are hashed and compared with
//! the SHA-256 in the index; encrypted blobs are decrypted into nothing, which
//! also checks their authentication tags. A link has no stored copy; its
//! entry is compared with the checksum of the target it names.

use std::collections::HashMap;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::crypto::VaultKey;
use super::manifest::link_digest;
use super::snapshots::SnapshotInfo;
use super::{CancelToken, Phase, Progress, Reporter, format_sha256, safe_relative_path, vault};
use crate::error::{EngineError, EngineResult};

#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub files: u64,
    pub bytes: u64,
    /// `source/path` of files whose content does not match.
    pub damaged: Vec<String>,
    /// `source/path` of files whose stored copy is missing.
    pub missing: Vec<String>,
    /// `source/path: error` of stored copies that could not be read (often
    /// locked by a virus scanner or a sync program at the destination). Their
    /// content is unknown, not known to be damaged.
    pub unreadable: Vec<String>,
    pub cancelled: bool,
    pub duration: Duration,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.damaged.is_empty()
            && self.missing.is_empty()
            && self.unreadable.is_empty()
            && !self.cancelled
    }
}

/// `show_encrypted_names`: whether names of encrypted files may appear in the
/// report, the progress and the log. Only when the passphrase was entered for
/// this check; with the key remembered for automatic backups, encrypted files
/// are reported by number and content id.
pub fn verify(
    snapshot: &SnapshotInfo,
    key: Option<&VaultKey>,
    show_encrypted_names: bool,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> EngineResult<VerifyReport> {
    let started = Instant::now();
    let destination = snapshot.destination();
    let parts = super::snapshots::load_parts(snapshot, key)?;

    let mut report = VerifyReport::default();
    let mut progress = Progress {
        phase: Phase::Scanning,
        files_total: parts.iter().map(|(_, i)| i.files.len() as u64).sum(),
        bytes_total: parts
            .iter()
            .flat_map(|(_, i)| i.files.iter().map(|f| f.size))
            .sum(),
        ..Progress::default()
    };
    let mut reporter = Reporter::new(on_progress);
    // Encrypted content is shared between files; each blob is checked once.
    let mut checked_blobs: HashMap<String, Result<bool, String>> = HashMap::new();

    let mut hidden = 0u64;
    for (part, index) in &parts {
        for entry in &index.files {
            if cancel.is_cancelled() {
                report.cancelled = true;
                report.duration = started.elapsed();
                return Ok(report);
            }
            let label = if part.is_encrypted() && !show_encrypted_names {
                hidden += 1;
                let id: String = entry.blob.chars().take(12).collect();
                format!("encrypted file {hidden} ({id}…)")
            } else {
                format!("{}/{}", entry.source, entry.path)
            };
            progress.current = label.clone();
            progress.files_done += 1;
            report.files += 1;

            if let Some(link) = &entry.link {
                if !entry.sha256.eq_ignore_ascii_case(&link_digest(link)) {
                    tracing::warn!("damaged in backup {}: {label}", snapshot.qualified_id());
                    report.damaged.push(label);
                }
                continue;
            }

            let path: Option<PathBuf> = match part.blob_root() {
                Some(root) => safe_relative_path(&entry.blob).map(|rel| root.join(rel)),
                None => entry
                    .blob
                    .chars()
                    .all(|c| c.is_ascii_hexdigit())
                    .then(|| vault::blob_path(&destination, &entry.blob)),
            };
            let Some(path) = path.filter(|p| p.is_file()) else {
                tracing::warn!(
                    "missing in backup {}: {label}: the stored copy is not there",
                    snapshot.qualified_id()
                );
                report.missing.push(label);
                progress.bytes_done += entry.size;
                continue;
            };

            // `Err(error)`: the stored copy could not be read at all.
            let result: Result<bool, String> = if part.is_encrypted() {
                let key = key.ok_or(EngineError::Locked)?;
                match checked_blobs.get(&entry.blob) {
                    Some(result) => result.clone(),
                    None => {
                        let result = match decrypted_hash(key, &path, cancel, &mut |n| {
                            progress.bytes_done += n;
                            reporter.maybe(&progress);
                        }) {
                            Ok(hash) => Ok(hash.eq_ignore_ascii_case(&entry.sha256)),
                            // A cut-off copy reads to its end: damaged, not locked.
                            Err(super::crypto::CryptoError::Io(err))
                                if err.kind() != io::ErrorKind::UnexpectedEof =>
                            {
                                Err(err.to_string())
                            }
                            Err(_) => Ok(false),
                        };
                        checked_blobs.insert(entry.blob.clone(), result.clone());
                        result
                    }
                }
            } else {
                file_hash(&path, cancel, &mut |n| {
                    progress.bytes_done += n;
                    reporter.maybe(&progress);
                })
                .map(|hash| hash.eq_ignore_ascii_case(&entry.sha256))
                .map_err(|err| err.to_string())
            };
            if cancel.is_cancelled() {
                report.cancelled = true;
                break;
            }
            report.bytes += entry.size;
            match result {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(
                        "damaged in backup {}: {label}: the content does not match its checksum",
                        snapshot.qualified_id()
                    );
                    report.damaged.push(label);
                }
                Err(err) => {
                    tracing::warn!(
                        "could not be read in backup {}: {label}: {err}",
                        snapshot.qualified_id()
                    );
                    report.unreadable.push(format!("{label}: {err}"));
                }
            }
            reporter.maybe(&progress);
        }
    }

    progress.phase = Phase::Finishing;
    reporter.now(&progress);
    report.duration = started.elapsed();
    tracing::info!(
        snapshot = %snapshot.qualified_id(),
        files = report.files,
        damaged = report.damaged.len(),
        missing = report.missing.len(),
        unreadable = report.unreadable.len(),
        "backup checked"
    );
    Ok(report)
}

pub(crate) fn file_hash(
    path: &std::path::Path,
    cancel: &CancelToken,
    on_bytes: &mut dyn FnMut(u64),
) -> io::Result<String> {
    let mut input = BufReader::with_capacity(1024 * 1024, std::fs::File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        on_bytes(n as u64);
    }
    Ok(format_sha256(&hasher.finalize()))
}

pub(crate) fn decrypted_hash(
    key: &VaultKey,
    path: &std::path::Path,
    cancel: &CancelToken,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<String, super::crypto::CryptoError> {
    let mut input = BufReader::with_capacity(1024 * 1024, std::fs::File::open(path)?);
    let digest = key.decrypt_stream(&mut input, &mut io::sink(), cancel, on_bytes)?;
    Ok(format_sha256(&digest))
}
