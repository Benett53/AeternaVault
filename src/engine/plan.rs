//! Planning: computes what a backup or restore will do, before anything is
//! written. Backups and restores always run a plan first.
//!
//! This module is strictly read-only (enforced by a test in `engine/mod.rs`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::crypto::VaultKey;
use super::manifest::SourceRecord;
use super::manifest::{self, FileEntry, SnapshotHeader};
use super::scan::{self, Excludes, ScanOptions, SkipReason};
use super::selection::Selection;
use super::snapshots::{self, SnapshotInfo};
use super::sources::{Sources, sanitize_segment};
use super::{
    CancelToken, Phase, Progress, Reporter, path_is_within, safe_relative_path, to_unix_nanos,
    vault,
};
use crate::config::{BackupMode, Config, ConflictPolicy};
use crate::error::{EngineError, EngineResult};
use crate::platform::known_paths::KnownPaths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// Backup: not in the previous backup. Restore: target does not exist.
    New,
    /// Backup: differs from the previous backup. Restore: target will be replaced.
    Changed,
    /// Nothing to do.
    Unchanged,
    /// Backup only: was in the previous backup, no longer in the source.
    /// Nothing is deleted — older backups keep the file.
    Removed,
    /// Left out, see [`Note`].
    Skipped,
}

impl ItemKind {
    pub const ALL: [ItemKind; 5] = [
        Self::New,
        Self::Changed,
        Self::Unchanged,
        Self::Removed,
        Self::Skipped,
    ];

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Link,
    /// Backup: a pipe, socket or device file.
    SpecialFile,
    OnlineOnly,
    InsideDestination,
    SourceMissing,
    NoAccess(String),
    KeepExisting,
    TargetNewer,
    TargetIsFolder,
    BackupFileMissing,
    UnsafePath,
    /// Restore: a symbolic link, on a system where links are not restored.
    LinkNotRestored,
}

impl From<SkipReason> for Note {
    fn from(reason: SkipReason) -> Self {
        match reason {
            SkipReason::Link => Note::Link,
            SkipReason::Special => Note::SpecialFile,
            SkipReason::OnlineOnly => Note::OnlineOnly,
            SkipReason::InsideDestination => Note::InsideDestination,
            SkipReason::SourceMissing => Note::SourceMissing,
            SkipReason::NoAccess(e) => Note::NoAccess(e),
        }
    }
}

/// Reference to existing content inside the backup.
#[derive(Debug, Clone)]
pub struct BlobRef {
    pub blob: String,
    pub sha256: String,
    /// The content lives in the encrypted vault (a blob id), not in a plain folder.
    pub encrypted: bool,
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub kind: ItemKind,
    /// Index into the plan's `sources`.
    pub source: usize,
    /// Relative path (`/`-separated); for skipped entries possibly a full path.
    pub rel: String,
    pub size: u64,
    pub modified: i64,
    pub note: Option<Note>,
    pub blob: Option<BlobRef>,
    /// Backup: stored in the encrypted part. Restore: comes from it.
    pub encrypted: bool,
    /// Restore: Linux permission bits of the original.
    pub mode: Option<u32>,
    /// A symbolic link: its target. Nothing is copied for it.
    pub link: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlannedSource {
    pub name: String,
    /// Backup: the source folder. Restore: the folder files are written to.
    pub root: PathBuf,
    /// Folder inside the snapshot.
    pub key: String,
    pub portable: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Summary {
    pub counts: [u64; 5],
    pub bytes: [u64; 5],
}

impl Summary {
    fn add(&mut self, item: &PlanItem) {
        self.counts[item.kind.index()] += 1;
        self.bytes[item.kind.index()] += item.size;
    }

    pub fn count(&self, kind: ItemKind) -> u64 {
        self.counts[kind.index()]
    }

    pub fn bytes(&self, kind: ItemKind) -> u64 {
        self.bytes[kind.index()]
    }
}

fn push(items: &mut Vec<PlanItem>, summary: &mut Summary, item: PlanItem) {
    summary.add(&item);
    items.push(item);
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BackupPlan {
    pub mode: BackupMode,
    /// Name stored with the backup (the job's name).
    pub label: Option<String>,
    pub flush_to_disk: bool,
    pub destination: PathBuf,
    pub computer: String,
    /// The backup gets a plain part (a folder) …
    pub plain_part: bool,
    /// … and/or an encrypted part (in the vault). Both: partly encrypted.
    pub encrypted_part: bool,
    /// Previous plain backup that unchanged plain files are compared against.
    pub base: Option<SnapshotInfo>,
    /// Previous encrypted backup (or encrypted part).
    pub encrypted_base: Option<SnapshotInfo>,
    pub hardlink_unchanged: bool,
    /// Health check: every stored copy that would be reused is read first and
    /// stored again from the original if it is damaged.
    pub health_check: bool,
    pub sources: Vec<PlannedSource>,
    pub items: Vec<PlanItem>,
    pub summary: Summary,
    pub excluded: u64,
    pub warnings: Vec<String>,
}

impl BackupPlan {
    /// Whether an item's content is copied (as opposed to linked/referenced).
    pub fn copies(&self, item: &PlanItem) -> bool {
        match item.kind {
            // A link is only an entry in the index.
            _ if item.link.is_some() => false,
            ItemKind::New | ItemKind::Changed => true,
            ItemKind::Unchanged => self.mode == BackupMode::Full || item.blob.is_none(),
            ItemKind::Removed | ItemKind::Skipped => false,
        }
    }

    /// Files that end up in the new snapshot.
    pub fn stored_files(&self) -> impl Iterator<Item = &PlanItem> {
        self.items.iter().filter(|i| {
            matches!(
                i.kind,
                ItemKind::New | ItemKind::Changed | ItemKind::Unchanged
            )
        })
    }

    /// (files, bytes) that will be copied.
    pub fn copy_totals(&self) -> (u64, u64) {
        self.items
            .iter()
            .filter(|i| self.copies(i))
            .fold((0, 0), |(f, b), i| (f + 1, b + i.size))
    }
}

pub struct BackupInput<'a> {
    pub config: &'a Config,
    pub sources: &'a Sources,
    pub computer: &'a str,
    pub key: Option<&'a VaultKey>,
}

pub fn plan_backup(
    input: &BackupInput<'_>,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> EngineResult<BackupPlan> {
    let config = input.config;
    if input.sources.is_empty() {
        return Err(EngineError::NoSources);
    }
    let destination = config.destination.clone();
    if destination.as_os_str().is_empty() {
        return Err(EngineError::NoDestination);
    }
    if !snapshots::destination_reachable(&destination) {
        return Err(EngineError::DestinationUnavailable(destination));
    }
    for source in &input.sources.folders {
        if path_is_within(&source.root, &destination) {
            return Err(EngineError::SourceInsideDestination {
                source_path: source.root.clone(),
                destination,
            });
        }
    }

    let may_encrypt = input.sources.may_encrypt();
    if may_encrypt {
        if !vault::exists(&destination) {
            return Err(EngineError::EncryptionNotSetUp);
        }
        if input.key.is_none() {
            return Err(EngineError::Locked);
        }
    }

    let mut warnings = Vec::new();

    // The previous backups to compare against: one per part.
    let mut load_base = |encrypted: bool| -> Option<(SnapshotInfo, manifest::FileIndex)> {
        if encrypted && input.key.is_none() {
            return None;
        }
        let info = snapshots::latest_usable(&destination, input.computer, encrypted, input.key)?;
        match info.load_index(input.key) {
            Ok(index) => Some((info, index)),
            Err(err) => {
                warnings.push(format!(
                    "previous backup could not be read, all files count as new: {err}"
                ));
                None
            }
        }
    };
    let plain_base = load_base(false);
    let encrypted_base = if may_encrypt { load_base(true) } else { None };

    // Lookup per part: source path -> relative path -> entry (see `path_key`).
    type Lookup<'a> = HashMap<String, HashMap<String, &'a FileEntry>>;
    let mut lookups: [Lookup<'_>; 2] = [HashMap::new(), HashMap::new()];
    for (part, base) in [(0usize, &plain_base), (1, &encrypted_base)] {
        let Some((info, index)) = base else {
            continue;
        };
        let Some(header) = &info.header else {
            continue;
        };
        let key_to_path: HashMap<&str, String> = header
            .sources
            .iter()
            .map(|s| (s.key.as_str(), path_key(&s.path)))
            .collect();
        for entry in &index.files {
            if let Some(path) = key_to_path.get(entry.source.as_str()) {
                lookups[part]
                    .entry(path.clone())
                    .or_default()
                    .insert(rel_key(&entry.path), entry);
            }
        }
    }

    let mut plan = BackupPlan {
        mode: config.mode,
        destination: destination.clone(),
        computer: input.computer.to_string(),
        plain_part: false,
        encrypted_part: false,
        base: plain_base.as_ref().map(|(info, _)| info.clone()),
        encrypted_base: encrypted_base.as_ref().map(|(info, _)| info.clone()),
        hardlink_unchanged: config.advanced.hardlink_unchanged,
        health_check: config.mode == BackupMode::Incremental
            && config.advanced.health_check_days > 0
            && !snapshots::checked_recently(
                &destination,
                input.computer,
                input.key,
                config.advanced.health_check_days,
            ),
        label: config.backup_label.clone(),
        flush_to_disk: config.advanced.flush_to_disk,
        sources: Vec::new(),
        items: Vec::new(),
        summary: Summary::default(),
        excluded: 0,
        warnings,
    };

    let mut progress = Progress::default();
    let mut reporter = Reporter::new(on_progress);

    for source in &input.sources.folders {
        let source_index = plan.sources.len();
        plan.sources.push(PlannedSource {
            name: source.name.clone(),
            root: source.root.clone(),
            key: source.key.clone(),
            portable: source.portable.clone(),
        });

        let excludes = Excludes::new(config.exclude.iter().chain(source.excludes.iter()));
        let options = ScanOptions {
            excludes: &excludes,
            selection: Selection::new(&source.include_paths, &source.exclude_paths),
            skip_online_only: config.advanced.skip_online_only_files,
            destination: &destination,
        };
        progress.current = source.root.display().to_string();
        reporter.now(&progress);
        let scanned =
            scan::scan_source(&source.root, &options, cancel, &mut progress, &mut reporter)?;
        plan.excluded += scanned.excluded;

        let source_path = path_key(&source.root);
        let previous = [lookups[0].get(&source_path), lookups[1].get(&source_path)];
        let mut seen = HashSet::new();

        for file in scanned.files {
            let lower = rel_key(&file.rel);
            let encrypted = source.encrypt.applies(&file.rel);
            let part = usize::from(encrypted);
            let same_part = previous[part].and_then(|m| m.get(&lower));
            let any_part = same_part.or_else(|| previous[1 - part].and_then(|m| m.get(&lower)));
            let kind = match (any_part, &file.link) {
                (None, _) => ItemKind::New,
                // A link is unchanged while it names the same target.
                (Some(e), Some(target)) if e.link.as_ref() == Some(target) => ItemKind::Unchanged,
                (Some(e), None)
                    if !e.is_link() && e.size == file.size && e.modified == file.modified =>
                {
                    ItemKind::Unchanged
                }
                (Some(_), _) => ItemKind::Changed,
            };
            // Content can only be reused from the part it is stored in now.
            // A link has no content.
            let blob = same_part
                .filter(|_| kind == ItemKind::Unchanged && file.link.is_none())
                .map(|e| BlobRef {
                    blob: e.blob.clone(),
                    sha256: e.sha256.clone(),
                    encrypted,
                });
            seen.insert(lower);
            if encrypted {
                plan.encrypted_part = true;
            } else {
                plan.plain_part = true;
            }
            push(
                &mut plan.items,
                &mut plan.summary,
                PlanItem {
                    kind,
                    source: source_index,
                    rel: file.rel,
                    size: file.size,
                    modified: file.modified,
                    note: None,
                    blob,
                    encrypted,
                    mode: None,
                    link: file.link,
                },
            );
        }

        for skipped in scanned.skipped {
            let rel = skipped
                .path
                .strip_prefix(&source.root)
                .map(super::rel_to_string)
                .unwrap_or_default();
            push(
                &mut plan.items,
                &mut plan.summary,
                PlanItem {
                    kind: ItemKind::Skipped,
                    source: source_index,
                    rel: if rel.is_empty() {
                        skipped.path.display().to_string()
                    } else {
                        rel
                    },
                    size: 0,
                    modified: 0,
                    note: Some(skipped.reason.into()),
                    blob: None,
                    encrypted: false,
                    mode: None,
                    link: None,
                },
            );
        }

        let mut removed: Vec<&FileEntry> = Vec::new();
        for map in previous.into_iter().flatten() {
            for (lower, entry) in map {
                if seen.insert(lower.clone()) {
                    removed.push(entry);
                }
            }
        }
        removed.sort_by(|a, b| a.path.cmp(&b.path));
        for entry in removed {
            push(
                &mut plan.items,
                &mut plan.summary,
                PlanItem {
                    kind: ItemKind::Removed,
                    source: source_index,
                    rel: entry.path.clone(),
                    size: entry.size,
                    modified: entry.modified,
                    note: None,
                    blob: None,
                    encrypted: false,
                    mode: None,
                    link: None,
                },
            );
        }
    }

    // A backup without any content still gets a (plain) folder.
    if !plan.plain_part && !plan.encrypted_part {
        if input.sources.may_encrypt() && config.encryption.everything() {
            plan.encrypted_part = true;
        } else {
            plan.plain_part = true;
        }
    }

    progress.phase = Phase::Finishing;
    reporter.now(&progress);
    Ok(plan)
}

/// Windows file names ignore case, Linux file names do not: `A.txt` and
/// `a.txt` are two files there and must never be matched with each other.
fn path_key(path: &Path) -> String {
    path.components()
        .map(|c| {
            let part = c.as_os_str().to_string_lossy();
            if cfg!(windows) {
                part.to_lowercase()
            } else {
                part.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\\")
}

/// The same rule for a `/`-separated path inside a backed-up folder.
fn rel_key(rel: &str) -> String {
    if cfg!(windows) {
        rel.to_lowercase()
    } else {
        rel.to_string()
    }
}

// ---------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreTarget {
    /// Back to where the files came from (resolved for the current user).
    Original,
    /// Into `<folder>\<the snapshot's folder structure>`.
    Folder(PathBuf),
}

#[derive(Debug, Clone)]
pub struct RestoreOptions {
    pub target: RestoreTarget,
    pub conflict: ConflictPolicy,
    pub verify: bool,
    /// Source keys (folders of the backup) to leave out.
    pub skip: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub snapshot: SnapshotInfo,
    pub options: RestoreOptions,
    pub sources: Vec<PlannedSource>,
    pub items: Vec<PlanItem>,
    pub summary: Summary,
}

impl RestorePlan {
    /// (files and links, bytes) that will be written.
    pub fn write_totals(&self) -> (u64, u64) {
        self.items
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::New | ItemKind::Changed))
            .fold((0, 0), |(f, b), i| (f + 1, b + i.size))
    }
}

pub fn is_skipped(skip: &HashSet<String>, key: &str) -> bool {
    skip.contains(key)
}

/// Refuses a readable part that the encrypted part does not vouch for, before
/// anything is written.
pub(crate) fn check_parts(parts: &[(&SnapshotInfo, manifest::FileIndex)]) -> EngineResult<()> {
    let [(plain, index), (encrypted, _)] = parts else {
        return Ok(());
    };
    let (Some(plain_header), Some(encrypted_header)) = (&plain.header, &encrypted.header) else {
        return Err(EngineError::Locked);
    };
    manifest::check_plain_part(plain_header, index, encrypted_header).map_err(|message| {
        EngineError::Manifest {
            path: plain.dir().to_path_buf(),
            message: message.into(),
        }
    })
}

/// Where the files of one backed-up folder go when restoring.
fn planned_source(
    record: &SourceRecord,
    target: &RestoreTarget,
    here: &KnownPaths,
) -> PlannedSource {
    let root = match target {
        RestoreTarget::Original => record
            .portable
            .as_deref()
            .and_then(|p| here.resolve(p))
            .unwrap_or_else(|| record.path.clone()),
        RestoreTarget::Folder(folder) => record
            .key
            .split('/')
            .fold(folder.clone(), |acc, part| acc.join(sanitize_segment(part))),
    };
    PlannedSource {
        name: record.name.clone(),
        root,
        key: record.key.clone(),
        portable: record.portable.clone(),
    }
}

pub fn plan_restore(
    snapshot: &SnapshotInfo,
    options: RestoreOptions,
    key: Option<&VaultKey>,
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&Progress),
) -> EngineResult<RestorePlan> {
    let header: SnapshotHeader = match &snapshot.header {
        Some(header) => header.clone(),
        None if snapshot.is_locked() => return Err(EngineError::Locked),
        None => return Err(EngineError::SnapshotNotFound(snapshot.qualified_id())),
    };
    if snapshot.needs_unlock() {
        return Err(EngineError::Locked);
    }
    // Every part with its index: the plain part first, then the encrypted one.
    let mut parts: Vec<(&SnapshotInfo, manifest::FileIndex)> = Vec::new();
    for part in snapshot.parts() {
        parts.push((part, part.load_index(key)?));
    }
    let here = KnownPaths::current();
    let destination = snapshot.destination();
    check_parts(&parts)?;

    // Every part names its own folders. The folders of the encrypted part come
    // only from its own, authenticated header, never from the readable plain
    // header next to it, which anyone with access to the storage could edit.
    let mut sources: Vec<PlannedSource> = Vec::new();
    let mut part_sources: Vec<HashMap<String, usize>> = Vec::new();
    for (part, _) in &parts {
        let part_header = part.header.as_ref().ok_or(EngineError::Locked)?;
        if part_header.id != header.id || (part.is_encrypted() != part_header.encrypted) {
            return Err(EngineError::Manifest {
                path: part.dir().to_path_buf(),
                message: "the parts of this backup do not belong together".into(),
            });
        }
        let mut map = HashMap::new();
        for record in &part_header.sources {
            map.insert(record.key.clone(), sources.len());
            sources.push(planned_source(record, &options.target, &here));
        }
        part_sources.push(map);
    }

    let mut plan = RestorePlan {
        snapshot: snapshot.clone(),
        options,
        sources: sources.clone(),
        items: Vec::with_capacity(parts.iter().map(|(_, i)| i.files.len()).sum()),
        summary: Summary::default(),
    };

    let mut progress = Progress {
        phase: Phase::Scanning,
        files_total: parts.iter().map(|(_, i)| i.files.len() as u64).sum(),
        ..Progress::default()
    };
    let mut reporter = Reporter::new(on_progress);

    let entries = parts
        .iter()
        .enumerate()
        .flat_map(|(n, (part, index))| index.files.iter().map(move |entry| (n, *part, entry)));
    for (n, part, entry) in entries {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let Some(&source) = part_sources[n].get(entry.source.as_str()) else {
            continue;
        };
        let planned = &sources[source];
        if is_skipped(&plan.options.skip, &planned.key) {
            continue;
        }
        progress.files_done += 1;

        let mut item = PlanItem {
            kind: ItemKind::New,
            source,
            rel: entry.path.clone(),
            size: entry.size,
            modified: entry.modified,
            note: None,
            blob: Some(BlobRef {
                blob: entry.blob.clone(),
                sha256: entry.sha256.clone(),
                encrypted: part.is_encrypted(),
            }),
            encrypted: part.is_encrypted(),
            mode: entry.mode,
            link: entry.link.clone(),
        };

        let Some(rel) = safe_relative_path(&entry.path) else {
            item.kind = ItemKind::Skipped;
            item.note = Some(Note::UnsafePath);
            push(&mut plan.items, &mut plan.summary, item);
            continue;
        };
        if let Some(link) = &entry.link {
            plan_link(
                &mut item,
                link,
                &planned.root.join(&rel),
                plan.options.conflict,
            );
            push(&mut plan.items, &mut plan.summary, item);
            continue;
        }
        let blob_path = match part.blob_root() {
            Some(root) => safe_relative_path(&entry.blob).map(|b| root.join(b)),
            None => entry
                .blob
                .chars()
                .all(|c| c.is_ascii_hexdigit())
                .then(|| vault::blob_path(&destination, &entry.blob)),
        };
        let Some(blob_path) = blob_path else {
            item.kind = ItemKind::Skipped;
            item.note = Some(Note::UnsafePath);
            push(&mut plan.items, &mut plan.summary, item);
            continue;
        };

        let target = planned.root.join(&rel);
        if progress.files_done.is_multiple_of(64) {
            progress.current = target.display().to_string();
            reporter.maybe(&progress);
        }

        if !blob_path.is_file() {
            item.kind = ItemKind::Skipped;
            item.note = Some(Note::BackupFileMissing);
        } else {
            match std::fs::symlink_metadata(&target) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => item.kind = ItemKind::New,
                Err(err) => {
                    item.kind = ItemKind::Skipped;
                    item.note = Some(Note::NoAccess(err.to_string()));
                }
                Ok(meta) if meta.is_dir() => {
                    item.kind = ItemKind::Skipped;
                    item.note = Some(Note::TargetIsFolder);
                }
                Ok(meta) => {
                    let existing_modified = meta.modified().map(to_unix_nanos).unwrap_or(0);
                    if meta.len() == entry.size && existing_modified == entry.modified {
                        item.kind = ItemKind::Unchanged;
                    } else {
                        apply_conflict_policy(
                            &mut item,
                            plan.options.conflict,
                            existing_modified > entry.modified,
                        );
                    }
                }
            }
        }
        push(&mut plan.items, &mut plan.summary, item);
    }

    progress.phase = Phase::Finishing;
    reporter.now(&progress);
    Ok(plan)
}

/// What restoring a link will do. A link at the target that names the same
/// target is unchanged; a folder is never replaced.
fn plan_link(item: &mut PlanItem, link: &str, target: &Path, conflict: ConflictPolicy) {
    if !cfg!(unix) {
        item.kind = ItemKind::Skipped;
        item.note = Some(Note::LinkNotRestored);
        return;
    }
    match std::fs::symlink_metadata(target) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => item.kind = ItemKind::New,
        Err(err) => {
            item.kind = ItemKind::Skipped;
            item.note = Some(Note::NoAccess(err.to_string()));
        }
        Ok(meta) if meta.is_dir() => {
            item.kind = ItemKind::Skipped;
            item.note = Some(Note::TargetIsFolder);
        }
        Ok(meta) => {
            let same = meta.file_type().is_symlink()
                && std::fs::read_link(target).is_ok_and(|t| t.to_str() == Some(link));
            if same {
                item.kind = ItemKind::Unchanged;
            } else {
                let existing_modified = meta.modified().map(to_unix_nanos).unwrap_or(0);
                apply_conflict_policy(item, conflict, existing_modified > item.modified);
            }
        }
    }
}

fn apply_conflict_policy(item: &mut PlanItem, policy: ConflictPolicy, existing_is_newer: bool) {
    match policy {
        ConflictPolicy::ReplaceChanged => item.kind = ItemKind::Changed,
        ConflictPolicy::KeepExisting => {
            item.kind = ItemKind::Skipped;
            item.note = Some(Note::KeepExisting);
        }
        ConflictPolicy::KeepNewer if existing_is_newer => {
            item.kind = ItemKind::Skipped;
            item.note = Some(Note::TargetNewer);
        }
        ConflictPolicy::KeepNewer => item.kind = ItemKind::Changed,
    }
}
