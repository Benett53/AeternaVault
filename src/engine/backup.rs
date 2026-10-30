//! Executes a confirmed [`BackupPlan`].
//!
//! A backup has a plain part (a folder), an encrypted part (in the vault), or
//! both. Each plan item says where it goes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{Local, Utc};

use super::crypto::VaultKey;
use super::fsops::{self, CopyError, DestinationLock};
use super::manifest::{
    EncryptedSnapshot, FORMAT_VERSION, FileEntry, FileIndex, HEADER_FILE, INDEX_FILE, META_DIR,
    SnapshotHeader, SnapshotStats, SnapshotStatus, SourceRecord,
};
use super::plan::{BackupPlan, ItemKind};
use super::sources::sanitize_segment;
use super::{
    CancelToken, Phase, Progress, Reporter, safe_relative_path, snapshots, to_unix_nanos, vault,
};
use crate::config::BackupMode;
use crate::error::{EngineError, EngineResult, IoContext};
use crate::platform::known_paths::KnownPaths;
use crate::platform::{self, vss::FileReader};

#[derive(Debug, Clone)]
pub struct BackupReport {
    /// Folder of the new backup.
    pub snapshot_dir: PathBuf,
    /// Header of the plain part, or of the encrypted part if there is no plain one.
    pub header: SnapshotHeader,
    /// Header of the encrypted part of a partly encrypted backup.
    pub encrypted_part: Option<SnapshotHeader>,
    pub warnings: Vec<String>,
    pub duration: Duration,
}

impl BackupReport {
    pub fn total_stats(&self) -> SnapshotStats {
        let mut stats = self.header.stats.clone();
        if let Some(part) = &self.encrypted_part {
            stats.add(&part.stats);
        }
        stats
    }
}

const PLAIN: usize = 0;
const ENCRYPTED: usize = 1;

/// What is collected for one part while copying.
#[derive(Default)]
struct PartData {
    entries: Vec<FileEntry>,
    stats: SnapshotStats,
}

pub fn run_backup(
    plan: &BackupPlan,
    key: Option<&VaultKey>,
    reader: &dyn FileReader,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> EngineResult<BackupReport> {
    let started = Instant::now();
    let started_at = Utc::now();
    let (files_total, bytes_total) = plan.copy_totals();

    let _lock = DestinationLock::acquire(&plan.destination)
        .at("could not prepare", &plan.destination)?
        .ok_or(EngineError::AlreadyRunning)?;

    if let Some(free) = platform::free_space(&plan.destination)
        && free < bytes_total
    {
        return Err(EngineError::NotEnoughSpace {
            needed: bytes_total,
            available: free,
        });
    }

    let key = if plan.encrypted_part {
        Some(key.ok_or(EngineError::Locked)?)
    } else {
        None
    };
    let id = new_snapshot_id(plan)?;
    let snapshot_dir = plan.destination.join(&id);
    let meta = snapshot_dir.join(META_DIR);
    fsops::create_dir_all(&meta).at("could not create", &meta)?;
    platform::set_hidden(&meta);
    let plain_dir = plan.plain_part.then(|| snapshot_dir.clone());
    if key.is_some() {
        vault::write_open_file(&snapshot_dir);
    }

    tracing::info!(
        snapshot = %id,
        plain = plan.plain_part,
        encrypted = plan.encrypted_part,
        mode = ?plan.mode,
        reader = reader.describe(),
        files = files_total,
        bytes = bytes_total,
        "backup started"
    );

    let mut reporter = Reporter::new(on_progress);
    let mut progress = Progress {
        phase: Phase::Copying,
        files_total: plan.stored_files().count() as u64,
        bytes_total,
        ..Progress::default()
    };
    reporter.now(&progress);

    let mut parts = [PartData::default(), PartData::default()];
    let primary = if plain_dir.is_some() {
        PLAIN
    } else {
        ENCRYPTED
    };
    parts[primary].stats.skipped = plan.summary.count(ItemKind::Skipped);
    // Warnings per part: in a partly encrypted backup, names of encrypted
    // files must not end up in the readable index. Warnings from scanning
    // cannot be assigned to a part and go to the encrypted one there.
    let split_plan = plan.plain_part && plan.encrypted_part;
    let mut warnings: [Vec<String>; 2] = Default::default();
    warnings[if split_plan { ENCRYPTED } else { primary }] = plan.warnings.clone();
    let mut cancelled = false;

    for item in plan.stored_files() {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        let source = &plan.sources[item.source];
        let Some(rel) = safe_relative_path(&item.rel) else {
            continue;
        };
        let src = source.root.join(&rel);
        progress.current = src.display().to_string();
        let part = if item.encrypted { ENCRYPTED } else { PLAIN };
        let stats = &mut parts[part].stats;

        // Re-check the file: it may have changed since the preview.
        let read_path = reader.read_path(&src);
        let meta = match std::fs::metadata(&read_path) {
            Ok(meta) => meta,
            Err(err) => {
                warnings[part].push(format!("{}: {err}", src.display()));
                stats.failed += 1;
                progress.files_done += 1;
                continue;
            }
        };
        let size = meta.len();
        let modified = meta.modified().map(to_unix_nanos).unwrap_or(0);
        let unchanged_since_plan = size == item.size && modified == item.modified;
        let reuse_base = plan.mode == BackupMode::Incremental
            && item.kind == ItemKind::Unchanged
            && unchanged_since_plan;

        let outcome: Result<(String, String), CopyError> = match (item.encrypted, key, &plain_dir) {
            (false, _, Some(snapshot_dir)) => {
                let dst = snapshot_dir.join(key_path(&source.key)).join(&rel);
                let own_blob = format!("{id}/{}/{}", source.key, item.rel);
                let base_path = item
                    .blob
                    .as_ref()
                    .filter(|b| reuse_base && !b.encrypted)
                    .and_then(|b| {
                        let root = plan.base.as_ref()?.blob_root()?;
                        Some((root.join(safe_relative_path(&b.blob)?), b))
                    })
                    .filter(|(path, _)| path.is_file());
                match base_path {
                    Some((base_path, base)) => {
                        if plan.hardlink_unchanged && fsops::hard_link(&base_path, &dst).is_ok() {
                            stats.linked_files += 1;
                            Ok((own_blob, base.sha256.clone()))
                        } else {
                            // File systems without hard links (FAT32/exFAT, many NAS
                            // shares): point to the existing copy instead.
                            stats.referenced_files += 1;
                            Ok((base.blob.clone(), base.sha256.clone()))
                        }
                    }
                    None => fsops::copy_hashed(&read_path, &dst, None, cancel, &mut |n| {
                        progress.bytes_done += n;
                        reporter.maybe(&progress);
                    })
                    .map(|sha| {
                        if let Err(err) = fsops::set_modified(&dst, modified) {
                            tracing::debug!(
                                "could not set modification time on {}: {err}",
                                dst.display()
                            );
                        }
                        stats.copied_files += 1;
                        stats.copied_bytes += size;
                        (own_blob, sha)
                    }),
                }
            }
            (true, Some(key), _) => {
                let existing = item.blob.as_ref().filter(|b| {
                    reuse_base
                        && b.encrypted
                        && vault::blob_path(&plan.destination, &b.blob).is_file()
                });
                match existing {
                    Some(base) => {
                        stats.referenced_files += 1;
                        Ok((base.blob.clone(), base.sha256.clone()))
                    }
                    None => fsops::encrypt_into_vault(
                        key,
                        &plan.destination,
                        &read_path,
                        cancel,
                        &mut |n| {
                            progress.bytes_done += n;
                            reporter.maybe(&progress);
                        },
                    )
                    .map(|(blob, sha, stored_new)| {
                        if stored_new {
                            stats.copied_files += 1;
                            stats.copied_bytes += size;
                        } else {
                            stats.referenced_files += 1;
                        }
                        (blob, sha)
                    }),
                }
            }
            // The plan and the prepared parts disagree; cannot happen for plans
            // made by `plan_backup`.
            _ => Err(CopyError::Io(std::io::Error::other(
                "no storage prepared for this file",
            ))),
        };

        match outcome {
            Ok((blob, sha256)) => {
                parts[part].entries.push(FileEntry {
                    source: source.key.clone(),
                    path: item.rel.clone(),
                    size,
                    modified,
                    sha256,
                    blob,
                });
                parts[part].stats.files += 1;
                parts[part].stats.bytes += size;
            }
            Err(CopyError::Cancelled) => {
                cancelled = true;
                break;
            }
            Err(err) => {
                tracing::warn!("could not back up {}: {err}", src.display());
                warnings[part].push(format!("{}: {err}", src.display()));
                parts[part].stats.failed += 1;
            }
        }
        progress.files_done += 1;
        reporter.maybe(&progress);
    }

    progress.phase = Phase::Finishing;
    progress.current.clear();
    reporter.now(&progress);

    let source_records: Vec<SourceRecord> = plan
        .sources
        .iter()
        .map(|s| SourceRecord {
            key: s.key.clone(),
            name: s.name.clone(),
            path: s.root.clone(),
            portable: s.portable.clone(),
        })
        .collect();

    let failed: u64 = parts.iter().map(|p| p.stats.failed).sum();
    let status = if cancelled {
        SnapshotStatus::Cancelled
    } else if failed > 0 {
        SnapshotStatus::CompleteWithWarnings
    } else {
        SnapshotStatus::Complete
    };
    let split = plain_dir.is_some() && key.is_some();
    let finished_at = Some(Utc::now());
    let known_paths = KnownPaths::current();

    let [plain_data, encrypted_data] = parts;
    let make_header = |encrypted: bool, data: &PartData, base: Option<String>| SnapshotHeader {
        format: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        id: id.clone(),
        label: plan.label.clone(),
        computer: plan.computer.clone(),
        user: platform::user_name(),
        mode: plan.mode,
        status,
        started_at,
        finished_at,
        base,
        encrypted,
        split,
        sources: source_records.clone(),
        known_paths: known_paths.clone(),
        stats: data.stats.clone(),
    };

    let mut plain_header = None;
    if let Some(snapshot_dir) = &plain_dir {
        let header = make_header(false, &plain_data, plan.base.as_ref().map(|b| b.id.clone()));
        let index = FileIndex {
            format: FORMAT_VERSION,
            files: plain_data.entries,
            warnings: warnings[PLAIN].clone(),
        };
        // The index is written first and the header last: a folder without a
        // header is recognisable as an unfinished backup.
        let meta = snapshot_dir.join(META_DIR);
        let index_path = meta.join(INDEX_FILE);
        fsops::write_json_atomic(&index_path, &index).at("could not write", &index_path)?;
        let header_path = meta.join(HEADER_FILE);
        fsops::write_json_atomic(&header_path, &header).at("could not write", &header_path)?;
        plain_header = Some(header);
    }

    let mut encrypted_header = None;
    if let Some(key) = key {
        let header = make_header(
            true,
            &encrypted_data,
            plan.encrypted_base.as_ref().map(|b| b.id.clone()),
        );
        let sealed = serde_json::to_vec(&EncryptedSnapshot {
            header: header.clone(),
            index: FileIndex {
                format: FORMAT_VERSION,
                files: encrypted_data.entries,
                warnings: warnings[ENCRYPTED].clone(),
            },
        })
        .map_err(|e| {
            EngineError::io(
                "could not encode the backup index",
                std::io::Error::other(e),
            )
        })?;
        let path = vault::snapshot_path(&plan.destination, &id);
        fsops::write_bytes_atomic(&path, &key.encrypt_bytes(&sealed))
            .at("could not write", &path)?;
        encrypted_header = Some(header);
    }

    let (header, encrypted_part) = match (plain_header, encrypted_header) {
        (Some(plain), encrypted) => (plain, encrypted),
        (None, Some(encrypted)) => (encrypted, None),
        (None, None) => {
            return Err(EngineError::io(
                "nothing was written",
                std::io::Error::other("the plan has no parts"),
            ));
        }
    };

    let report = BackupReport {
        snapshot_dir,
        header,
        encrypted_part,
        warnings: warnings.concat(),
        duration: started.elapsed(),
    };
    let total = report.total_stats();
    tracing::info!(
        status = ?report.header.status,
        split,
        files = total.files,
        copied = total.copied_files,
        linked = total.linked_files,
        referenced = total.referenced_files,
        failed = total.failed,
        "backup finished"
    );
    Ok(report)
}

/// `Documents/Letters` → `Documents\Letters`.
pub(crate) fn key_path(key: &str) -> PathBuf {
    key.split('/').collect()
}

/// `2026-09-14 20-00`, with ` (COMPUTER)` if other computers use the same
/// destination and `-2`, `-3` … if the minute is already taken (in either part).
fn new_snapshot_id(plan: &BackupPlan) -> EngineResult<String> {
    let base = Local::now().format("%Y-%m-%d %H-%M").to_string();
    let suffix = if snapshots::has_other_computers(&plan.destination, &plan.computer) {
        format!(" ({})", sanitize_segment(&plan.computer))
    } else {
        String::new()
    };
    for n in 1..1000 {
        let id = if n == 1 {
            format!("{base}{suffix}")
        } else {
            format!("{base}-{n}{suffix}")
        };
        let dir = plan.destination.join(&id);
        if dir.exists() {
            continue;
        }
        match fsops::create_new_dir(&dir) {
            Ok(()) => return Ok(id),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(EngineError::io(
                    format!("could not create {}", dir.display()),
                    err,
                ));
            }
        }
    }
    Err(EngineError::io(
        "could not find a free backup name",
        std::io::Error::from(std::io::ErrorKind::AlreadyExists),
    ))
}
