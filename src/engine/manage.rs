//! Managing existing backups: deleting them, moving them to another location
//! and cleaning up unused encrypted data.
//!
//! Care is needed because backups can depend on each other:
//! * On drives without hard links, an incremental plain backup may point to
//!   files stored in an older backup. Before such an older backup is deleted,
//!   those files are copied into the backups that still need them.
//! * Encrypted content is shared by all encrypted backups. Only content that
//!   no remaining backup refers to is removed, and only when every remaining
//!   encrypted backup could be read.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::crypto::VaultKey;
use super::fsops::{self, CopyError, DestinationLock};
use super::manifest::{self, HEADER_FILE, INDEX_FILE, META_DIR};
use super::snapshots::{self, Location, SnapshotInfo};
use super::{CancelToken, Phase, Progress, Reporter, paths_equal, safe_relative_path, vault};
use crate::error::{EngineError, EngineResult, IoContext};
use crate::platform;

#[derive(Debug, Clone, Default)]
pub struct DeleteReport {
    pub deleted: Vec<String>,
    /// Files copied into remaining backups that depended on a deleted one.
    pub rehomed_files: u64,
    pub pruned_blobs: u64,
    pub freed_bytes: u64,
    /// Plain backups that could not go to the recycle bin and were deleted directly.
    pub deleted_permanently: u64,
    pub warnings: Vec<String>,
    pub duration: Duration,
}

fn copy_error(context: &str, path: &Path, err: CopyError) -> EngineError {
    match err {
        CopyError::Cancelled => EngineError::Cancelled,
        CopyError::Io(io) => EngineError::io(format!("{context} {}", path.display()), io),
        other => EngineError::io(
            format!("{context} {}", path.display()),
            std::io::Error::other(other.to_string()),
        ),
    }
}

/// Where a file of `part` would be stored if it had its own copy.
fn own_blob(part: &SnapshotInfo, source: &str, path: &str) -> String {
    format!("{}/{source}/{path}", part.id)
}

fn index_file(part: &SnapshotInfo) -> Option<PathBuf> {
    match &part.location {
        Location::Plain { dir, .. } => Some(dir.join(META_DIR).join(INDEX_FILE)),
        Location::Encrypted { .. } => None,
    }
}

/// Gives a backup a name, shown in lists instead of only its date. The name is
/// stored in plain text next to the backup (`label.txt`), also for encrypted
/// backups. An empty name removes it.
pub fn set_label(snapshot: &SnapshotInfo, label: &str) -> std::io::Result<()> {
    let path = snapshot.dir().join(META_DIR).join(manifest::LABEL_FILE);
    let label = label.trim();
    if label.is_empty() {
        match fsops::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    } else {
        fsops::write_bytes_atomic(&path, label.as_bytes())
    }
}

/// Deletes backups (by qualified id) from `destination`.
pub fn delete(
    destination: &Path,
    ids: &[String],
    key: Option<&VaultKey>,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> EngineResult<DeleteReport> {
    let started = Instant::now();
    let _lock = DestinationLock::acquire(destination)
        .at("could not prepare", destination)?
        .ok_or(EngineError::AlreadyRunning)?;
    let all = snapshots::list(destination, key)?;
    let (targets, keep): (Vec<SnapshotInfo>, Vec<SnapshotInfo>) = all
        .into_iter()
        .partition(|s| ids.contains(&s.qualified_id()));
    if targets.iter().any(SnapshotInfo::needs_key) && key.is_none() {
        return Err(EngineError::Locked);
    }

    let mut report = DeleteReport::default();
    let mut reporter = Reporter::new(on_progress);
    let mut progress = Progress {
        phase: Phase::Finishing,
        files_total: targets.len() as u64,
        ..Progress::default()
    };
    reporter.now(&progress);

    let doomed_plain: Vec<&SnapshotInfo> = targets
        .iter()
        .flat_map(SnapshotInfo::parts)
        .filter(|p| !p.is_encrypted())
        .collect();

    // 1. Remaining plain backups that point into a doomed one get their own copies.
    for part in keep
        .iter()
        .flat_map(SnapshotInfo::parts)
        .filter(|p| !p.is_encrypted() && p.header.is_some())
    {
        let Some(root) = part.blob_root() else {
            continue;
        };
        let prefixes: Vec<String> = doomed_plain
            .iter()
            .filter(|d| d.blob_root().is_some_and(|r| paths_equal(r, root)))
            .map(|d| format!("{}/", d.id))
            .collect();
        if prefixes.is_empty() {
            continue;
        }
        let mut index = part.load_index(None)?;
        let mut changed = false;
        for entry in &mut index.files {
            if !prefixes.iter().any(|p| entry.blob.starts_with(p.as_str())) {
                continue;
            }
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let own = own_blob(part, &entry.source, &entry.path);
            let (Some(src_rel), Some(dst_rel)) =
                (safe_relative_path(&entry.blob), safe_relative_path(&own))
            else {
                continue;
            };
            let src = root.join(src_rel);
            let dst = root.join(dst_rel);
            progress.current = dst.display().to_string();
            reporter.maybe(&progress);
            fsops::copy_hashed(&src, &dst, Some(&entry.sha256), cancel, &mut |_| {})
                .map_err(|e| copy_error("could not keep a file needed by", &dst, e))?;
            let _ = fsops::set_modified(&dst, entry.modified);
            entry.blob = own;
            report.rehomed_files += 1;
            changed = true;
        }
        if changed && let Some(path) = index_file(part) {
            fsops::write_json_atomic(&path, &index).at("could not write", &path)?;
        }
    }

    // 2. Encrypted indexes first, so a half-finished deletion never leaves an
    //    encrypted backup that points to removed content.
    let doomed_encrypted: Vec<&SnapshotInfo> = targets
        .iter()
        .flat_map(SnapshotInfo::parts)
        .filter(|p| p.is_encrypted())
        .collect();
    for part in &doomed_encrypted {
        if let Some(file) = part.encrypted_file() {
            fsops::remove_file(&file).at("could not delete", &file)?;
        }
    }

    // 3. The backup folders.
    for target in &targets {
        let dir = target.dir();
        if !dir.exists() {
            continue;
        }
        progress.current = dir.display().to_string();
        reporter.now(&progress);
        let plain = target.parts().any(|p| !p.is_encrypted());
        let recycled = plain && platform::delete_to_recycle_bin(dir).is_ok();
        if !recycled {
            fsops::remove_dir_all(dir).at("could not delete", dir)?;
            if plain {
                report.deleted_permanently += 1;
            }
        }
    }

    // 4. Encrypted content no longer used by any backup.
    if !doomed_encrypted.is_empty()
        && let Some(key) = key
    {
        let remaining: Vec<&SnapshotInfo> = keep
            .iter()
            .flat_map(SnapshotInfo::parts)
            .filter(|p| p.is_encrypted())
            .collect();
        match prune_blobs(destination, key, &remaining) {
            Ok((count, bytes)) => {
                report.pruned_blobs = count;
                report.freed_bytes = bytes;
            }
            Err(message) => report.warnings.push(message),
        }
    }

    for target in &targets {
        progress.files_done += 1;
        report.deleted.push(target.qualified_id());
        tracing::info!("backup deleted: {}", target.qualified_id());
    }
    reporter.now(&progress);
    report.duration = started.elapsed();
    Ok(report)
}

/// Removes encrypted content that none of `remaining` refers to. Returns
/// (blobs removed, bytes freed), or a message if it was not safe to clean up.
fn prune_blobs(
    destination: &Path,
    key: &VaultKey,
    remaining: &[&SnapshotInfo],
) -> Result<(u64, u64), String> {
    let mut used: HashSet<String> = HashSet::new();
    for part in remaining {
        let index = part.load_index(Some(key)).map_err(|e| {
            format!(
                "unused encrypted data was not removed because {} could not be read: {e}",
                part.id
            )
        })?;
        used.extend(index.files.into_iter().map(|f| f.blob));
    }
    let blobs = vault::vault_dir(destination).join(vault::BLOB_DIR);
    let (mut count, mut bytes) = (0, 0);
    for file in walkdir::WalkDir::new(&blobs)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
    {
        let path = file.path();
        let stale_temp = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".aeterna-partial"));
        let is_blob = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(vault::BLOB_EXT));
        let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if (is_blob && !used.contains(&id)) || stale_temp {
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            if fsops::remove_file(path).is_ok() {
                count += 1;
                bytes += size;
            }
        }
    }
    tracing::info!("removed {count} unused encrypted files ({bytes} bytes)");
    Ok((count, bytes))
}

#[derive(Debug, Clone, Default)]
pub struct ExtractReport {
    pub files: u64,
    pub bytes: u64,
    /// Where the first file was written (used to open a single file).
    pub first: Option<PathBuf>,
    pub failed: Vec<String>,
    pub cancelled: bool,
    pub duration: Duration,
}

/// Copies (and decrypts) chosen files of a backup into `target`, keeping the
/// backup's folder structure: `<target>\Documents\Letters\a.txt`. The backup
/// itself is not changed.
pub fn extract(
    snapshot: &SnapshotInfo,
    files: &[snapshots::StoredFile],
    target: &Path,
    key: Option<&VaultKey>,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> EngineResult<ExtractReport> {
    let started = Instant::now();
    if files.iter().any(|f| f.encrypted) && key.is_none() {
        return Err(EngineError::Locked);
    }
    let destination = snapshot.destination();
    let plain_root = snapshot.parts().find_map(|p| p.blob_root());
    let mut report = ExtractReport::default();
    let mut reporter = Reporter::new(on_progress);
    let mut progress = Progress {
        phase: Phase::Restoring,
        files_total: files.len() as u64,
        bytes_total: files.iter().map(|f| f.size).sum(),
        ..Progress::default()
    };
    reporter.now(&progress);

    for file in files {
        if cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let dst_rel = file
            .source
            .split('/')
            .map(super::sources::sanitize_segment)
            .collect::<PathBuf>()
            .join(safe_relative_path(&file.path).unwrap_or_default());
        let dst = target.join(dst_rel);
        progress.current = dst.display().to_string();
        let mut on_bytes = |n| {
            progress.bytes_done += n;
            reporter.maybe(&progress);
        };
        let result = match (file.encrypted, key, plain_root) {
            (true, Some(key), _) => {
                let blob = vault::blob_path(&destination, &file.blob);
                fsops::decrypt_blob(key, &blob, &dst, Some(&file.sha256), cancel, &mut on_bytes)
            }
            (false, _, Some(root)) => match safe_relative_path(&file.blob) {
                Some(rel) => fsops::copy_hashed(
                    &root.join(rel),
                    &dst,
                    Some(&file.sha256),
                    cancel,
                    &mut on_bytes,
                ),
                None => Err(CopyError::Io(std::io::Error::other(
                    "invalid path in the index",
                ))),
            },
            _ => Err(CopyError::Io(std::io::Error::other("the key is missing"))),
        };
        match result {
            Ok(_) => {
                let _ = fsops::set_modified(&dst, file.modified);
                report.first.get_or_insert(dst);
                report.files += 1;
                report.bytes += file.size;
            }
            Err(CopyError::Cancelled) => {
                report.cancelled = true;
                break;
            }
            Err(err) => {
                tracing::warn!("could not copy {}: {err}", file.display_path());
                report
                    .failed
                    .push(format!("{}: {err}", file.display_path()));
            }
        }
        progress.files_done += 1;
        reporter.maybe(&progress);
    }
    report.duration = started.elapsed();
    Ok(report)
}

#[derive(Debug, Clone, Default)]
pub struct TransferReport {
    pub files: u64,
    pub bytes: u64,
    pub delete: DeleteReport,
    pub duration: Duration,
}

/// Moves a backup (all its parts) to another destination: copies and checks
/// everything first, then deletes the original.
pub fn transfer(
    snapshot: &SnapshotInfo,
    target: &Path,
    key: Option<&VaultKey>,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> EngineResult<TransferReport> {
    let started = Instant::now();
    if snapshot.needs_unlock() || (snapshot.needs_key() && key.is_none()) {
        return Err(EngineError::Locked);
    }
    let source_destination = snapshot.destination();
    if paths_equal(&source_destination, target) {
        return Err(EngineError::io(
            "the backup is already there",
            std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        ));
    }

    let mut report = TransferReport::default();
    {
        let _source_lock = DestinationLock::acquire(&source_destination)
            .at("could not prepare", &source_destination)?
            .ok_or(EngineError::AlreadyRunning)?;
        let _target_lock = DestinationLock::acquire(target)
            .at("could not prepare", target)?
            .ok_or(EngineError::AlreadyRunning)?;
        let new_dir = target.join(&snapshot.id);
        if new_dir.exists() {
            return Err(EngineError::io(
                format!("{} already exists", new_dir.display()),
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        let result = copy_parts(snapshot, target, key, cancel, on_progress, &mut report);
        if result.is_err()
            && new_dir.is_dir()
            && !new_dir.join(META_DIR).join(HEADER_FILE).is_file()
        {
            // Our own half-copied folder; nothing of the user's is in it.
            let _ = fsops::remove_dir_all(&new_dir);
        }
        result?;
    }

    report.delete = delete(
        &source_destination,
        &[snapshot.qualified_id()],
        key,
        cancel,
        &mut |_| {},
    )?;
    report.duration = started.elapsed();
    tracing::info!(
        "backup {} moved to {}",
        snapshot.qualified_id(),
        target.display()
    );
    Ok(report)
}

fn copy_parts(
    snapshot: &SnapshotInfo,
    target: &Path,
    key: Option<&VaultKey>,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
    report: &mut TransferReport,
) -> EngineResult<()> {
    let source_destination = snapshot.destination();
    let mut parts = Vec::new();
    for part in snapshot.parts() {
        parts.push((part, part.load_index(key)?));
    }
    let mut reporter = Reporter::new(on_progress);
    let mut progress = Progress {
        phase: Phase::Copying,
        files_total: parts.iter().map(|(_, i)| i.files.len() as u64).sum(),
        bytes_total: parts
            .iter()
            .flat_map(|(_, i)| i.files.iter().map(|f| f.size))
            .sum(),
        ..Progress::default()
    };
    reporter.now(&progress);

    for (part, index) in parts {
        let Some(header) = part.header.clone() else {
            return Err(EngineError::Locked);
        };
        let new_dir = target.join(&part.id);
        match &part.location {
            Location::Plain { destination, .. } => {
                let meta = new_dir.join(META_DIR);
                fsops::create_dir_all(&meta).at("could not create", &meta)?;
                platform::set_hidden(&meta);
                let mut index = index;
                for entry in &mut index.files {
                    if cancel.is_cancelled() {
                        return Err(EngineError::Cancelled);
                    }
                    let own = own_blob(part, &entry.source, &entry.path);
                    let (Some(src_rel), Some(dst_rel)) =
                        (safe_relative_path(&entry.blob), safe_relative_path(&own))
                    else {
                        continue;
                    };
                    let src = destination.join(src_rel);
                    let dst = target.join(dst_rel);
                    progress.current = dst.display().to_string();
                    fsops::copy_hashed(&src, &dst, Some(&entry.sha256), cancel, &mut |n| {
                        progress.bytes_done += n;
                        reporter.maybe(&progress);
                    })
                    .map_err(|e| copy_error("could not copy", &src, e))?;
                    let _ = fsops::set_modified(&dst, entry.modified);
                    entry.blob = own;
                    progress.files_done += 1;
                    report.files += 1;
                    report.bytes += entry.size;
                }
                let mut header = header;
                header.base = None;
                fsops::write_json_atomic(&meta.join(INDEX_FILE), &index)
                    .at("could not write", &meta)?;
                fsops::write_json_atomic(&meta.join(HEADER_FILE), &header)
                    .at("could not write", &meta)?;
            }
            Location::Encrypted { .. } => {
                let key = key.ok_or(EngineError::Locked)?;
                let file = part.encrypted_file().unwrap_or_default();
                let source_vault = vault::read_header(&source_destination)
                    .at("could not read", &vault::vault_dir(&source_destination))?;
                if vault::exists(target) {
                    let target_vault = vault::read_header(target)
                        .at("could not read", &vault::vault_dir(target))?;
                    if target_vault.vault_id != source_vault.vault_id {
                        return Err(EngineError::io(
                            "the new location already holds other encrypted backups",
                            std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                        ));
                    }
                } else {
                    let from = vault::vault_dir(&source_destination).join(vault::VAULT_FILE);
                    let to = vault::vault_dir(target).join(vault::VAULT_FILE);
                    fsops::copy_hashed(&from, &to, None, cancel, &mut |_| {})
                        .map_err(|e| copy_error("could not copy", &from, e))?;
                    platform::set_hidden(&vault::vault_dir(target));
                }
                let new_file = vault::snapshot_path(target, &part.id);
                if new_file.exists() {
                    return Err(EngineError::io(
                        format!("{} already exists", new_file.display()),
                        std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                    ));
                }
                let mut copied: HashSet<&str> = HashSet::new();
                for entry in &index.files {
                    if cancel.is_cancelled() {
                        return Err(EngineError::Cancelled);
                    }
                    progress.files_done += 1;
                    report.files += 1;
                    report.bytes += entry.size;
                    if !copied.insert(entry.blob.as_str()) {
                        continue;
                    }
                    let dst = vault::blob_path(target, &entry.blob);
                    if !dst.is_file() {
                        let src = vault::blob_path(&source_destination, &entry.blob);
                        fsops::copy_hashed(&src, &dst, None, cancel, &mut |n| {
                            progress.bytes_done += n;
                            reporter.maybe(&progress);
                        })
                        .map_err(|e| copy_error("could not copy", &src, e))?;
                    }
                }
                fsops::copy_hashed(&file, &new_file, None, cancel, &mut |_| {})
                    .map_err(|e| copy_error("could not copy", &file, e))?;
                if let Some(meta) = new_file.parent() {
                    platform::set_hidden(meta);
                }
                vault::write_open_file(&new_dir);
                // The copied index must be readable before the original goes.
                manifest::read_encrypted(&new_file, key)?;
            }
        }
    }
    Ok(())
}
