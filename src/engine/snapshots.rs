//! Finding existing backups in a destination folder. Read-only.

use std::path::{Path, PathBuf};

use super::crypto::VaultKey;
use super::manifest::{self, ENCRYPTED_FILE, FileIndex, HEADER_FILE, META_DIR, SnapshotHeader};
use super::vault;
use crate::error::{EngineError, EngineResult};

#[derive(Debug, Clone)]
pub enum Location {
    /// `<destination>\<id>\` with `snapshot.json` in its hidden `.aeternavault` folder.
    Plain { dir: PathBuf, destination: PathBuf },
    /// `<destination>\<id>\.aeternavault\encrypted.avs`.
    Encrypted { dir: PathBuf, destination: PathBuf },
}

#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub id: String,
    pub computer: String,
    pub location: Location,
    /// `None` if the backup is unfinished, damaged, or encrypted and locked.
    pub header: Option<SnapshotHeader>,
    /// The encrypted part of a partly encrypted backup (this is the plain part).
    pub companion: Option<Box<SnapshotInfo>>,
    /// Name given afterwards (`label.txt`), else the job's name.
    pub label: Option<String>,
}

impl SnapshotInfo {
    /// This backup and, if partly encrypted, its encrypted part.
    pub fn parts(&self) -> impl Iterator<Item = &SnapshotInfo> {
        std::iter::once(self).chain(self.companion.as_deref())
    }

    /// Whether the key is needed to see or restore everything.
    pub fn needs_key(&self) -> bool {
        self.parts().any(|p| p.is_encrypted())
    }

    /// Whether some part is encrypted and not unlocked.
    pub fn needs_unlock(&self) -> bool {
        self.parts().any(SnapshotInfo::is_locked)
    }

    pub fn is_split(&self) -> bool {
        self.companion.is_some()
    }

    /// Statistics of all parts together, if known.
    pub fn total_stats(&self) -> Option<super::manifest::SnapshotStats> {
        let mut total = self.header.as_ref()?.stats.clone();
        if let Some(companion) = &self.companion {
            let stats = &companion.header.as_ref()?.stats;
            total.add(stats);
        }
        Some(total)
    }

    /// Unique within a destination: the folder name.
    pub fn qualified_id(&self) -> String {
        self.id.clone()
    }

    /// The backup's folder.
    pub fn dir(&self) -> &Path {
        match &self.location {
            Location::Plain { dir, .. } | Location::Encrypted { dir, .. } => dir,
        }
    }

    /// The encrypted index of an encrypted part.
    pub fn encrypted_file(&self) -> Option<PathBuf> {
        match &self.location {
            Location::Encrypted { dir, .. } => Some(dir.join(META_DIR).join(ENCRYPTED_FILE)),
            Location::Plain { .. } => None,
        }
    }

    pub fn is_encrypted(&self) -> bool {
        matches!(self.location, Location::Encrypted { .. })
    }

    pub fn is_locked(&self) -> bool {
        self.is_encrypted() && self.header.is_none()
    }

    pub fn is_usable_base(&self) -> bool {
        self.header
            .as_ref()
            .is_some_and(|h| h.status.is_usable_base())
    }

    /// Folder that plain `blob` paths are relative to.
    pub fn blob_root(&self) -> Option<&Path> {
        match &self.location {
            Location::Plain { destination, .. } => Some(destination),
            Location::Encrypted { .. } => None,
        }
    }

    pub fn destination(&self) -> PathBuf {
        match &self.location {
            Location::Plain { destination, .. } | Location::Encrypted { destination, .. } => {
                destination.clone()
            }
        }
    }

    /// When the backup was made: from its header, otherwise from its name.
    pub fn started_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        if let Some(header) = &self.header {
            return Some(header.started_at);
        }
        let stamp = self.id.get(..16)?;
        let naive = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H-%M").ok()?;
        naive
            .and_local_timezone(chrono::Local)
            .earliest()
            .map(|t| t.with_timezone(&chrono::Utc))
    }

    pub fn load_index(&self, key: Option<&VaultKey>) -> EngineResult<FileIndex> {
        match &self.location {
            Location::Plain { dir, .. } => manifest::read_index(&dir.join(META_DIR)),
            Location::Encrypted { dir, .. } => {
                let key = key.ok_or(EngineError::Locked)?;
                let file = dir.join(META_DIR).join(ENCRYPTED_FILE);
                Ok(manifest::read_encrypted(&file, key)?.index)
            }
        }
    }

    fn sort_key(&self) -> String {
        let digits: String = self
            .id
            .chars()
            .filter(char::is_ascii_digit)
            .take(14)
            .collect();
        format!("{digits:0<14}")
    }
}

/// One stored file of a backup, for browsing.
#[derive(Debug, Clone)]
pub struct StoredFile {
    /// Folder inside the backup (e.g. `Documents`).
    pub source: String,
    /// Path inside that folder, `/`-separated.
    pub path: String,
    pub size: u64,
    pub modified: i64,
    pub sha256: String,
    pub blob: String,
    pub encrypted: bool,
}

impl StoredFile {
    pub fn display_path(&self) -> String {
        format!("{}/{}", self.source, self.path)
    }
}

/// All files of a backup (every part), sorted by path.
pub fn contents(snapshot: &SnapshotInfo, key: Option<&VaultKey>) -> EngineResult<Vec<StoredFile>> {
    if snapshot.needs_unlock() {
        return Err(EngineError::Locked);
    }
    let mut files = Vec::new();
    for part in snapshot.parts() {
        let index = part.load_index(key)?;
        files.extend(index.files.into_iter().map(|f| StoredFile {
            source: f.source,
            path: f.path,
            size: f.size,
            modified: f.modified,
            sha256: f.sha256,
            blob: f.blob,
            encrypted: part.is_encrypted(),
        }));
    }
    files.sort_by_key(|f| f.display_path().to_lowercase());
    Ok(files)
}

/// `true` if the destination drive/share is reachable. The destination folder
/// itself does not need to exist yet.
pub fn destination_reachable(destination: &Path) -> bool {
    destination
        .ancestors()
        .any(|p| !p.as_os_str().is_empty() && p.exists())
}

/// Name of the folder created inside a freshly chosen destination.
pub const APP_FOLDER: &str = "AeternaVault";

/// The destination to use for a folder the user picked. With `app_folder`,
/// backups go into an `AeternaVault` sub-folder — unless the picked folder is
/// already such a folder or already contains backups.
pub fn chosen_destination(picked: &Path, app_folder: bool) -> PathBuf {
    if !app_folder || contains_backups(picked) {
        return picked.to_path_buf();
    }
    let already_named = picked
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(APP_FOLDER));
    if already_named {
        picked.to_path_buf()
    } else {
        picked.join(APP_FOLDER)
    }
}

/// Whether `folder` holds AeternaVault backups (or an encrypted vault, or
/// backups of AeternaVault 0.3/0.4 that are moved to the current layout).
pub fn contains_backups(folder: &Path) -> bool {
    if vault::exists(folder) || folder.join(super::layout::OLD_VAULT_DIR).is_dir() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return false;
    };
    entries.flatten().any(|entry| {
        looks_like_snapshot_id(&entry.file_name().to_string_lossy())
            && entry.path().join(META_DIR).is_dir()
    })
}

/// All snapshots below `destination`, newest first. Encrypted snapshots are
/// only described in detail when `key` unlocks them.
pub fn list(destination: &Path, key: Option<&VaultKey>) -> EngineResult<Vec<SnapshotInfo>> {
    if destination.as_os_str().is_empty() {
        return Err(EngineError::NoDestination);
    }
    if !destination_reachable(destination) {
        return Err(EngineError::DestinationUnavailable(
            destination.to_path_buf(),
        ));
    }
    let Ok(entries) = std::fs::read_dir(destination) else {
        return Ok(Vec::new());
    };

    let mut snapshots = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !looks_like_snapshot_id(&name) || !dir.is_dir() {
            continue;
        }
        let meta = dir.join(META_DIR);
        if !meta.is_dir() {
            continue;
        }
        let renamed = std::fs::read_to_string(meta.join(manifest::LABEL_FILE))
            .ok()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty());
        let plain = meta.join(HEADER_FILE).is_file();
        let encrypted = meta.join(ENCRYPTED_FILE).is_file();
        let encrypted_info = encrypted.then(|| {
            let header = key.and_then(|key| {
                manifest::read_encrypted(&meta.join(ENCRYPTED_FILE), key)
                    .inspect_err(|e| tracing::warn!("{e}"))
                    .ok()
                    .map(|s| s.header)
            });
            SnapshotInfo {
                computer: header
                    .as_ref()
                    .map(|h| h.computer.clone())
                    .or_else(|| computer_suffix(&name))
                    .unwrap_or_default(),
                id: name.clone(),
                location: Location::Encrypted {
                    dir: dir.clone(),
                    destination: destination.to_path_buf(),
                },
                label: renamed
                    .clone()
                    .or_else(|| header.as_ref().and_then(|h| h.label.clone())),
                header,
                companion: None,
            }
        });
        if plain || !encrypted {
            // A folder with neither file is an unfinished plain backup.
            let header = plain
                .then(|| {
                    manifest::read_header(&meta)
                        .inspect_err(|e| tracing::warn!("{e}"))
                        .ok()
                })
                .flatten();
            let computer = header
                .as_ref()
                .map(|h| h.computer.clone())
                .or_else(|| computer_suffix(&name))
                .unwrap_or_default();
            let label = renamed
                .clone()
                .or_else(|| header.as_ref().and_then(|h| h.label.clone()))
                .or_else(|| encrypted_info.as_ref().and_then(|e| e.label.clone()));
            snapshots.push(SnapshotInfo {
                label,
                id: name,
                computer,
                location: Location::Plain {
                    dir,
                    destination: destination.to_path_buf(),
                },
                header,
                companion: encrypted_info.map(Box::new),
            });
        } else if let Some(info) = encrypted_info {
            snapshots.push(info);
        }
    }

    snapshots.sort_by(|a, b| {
        b.sort_key()
            .cmp(&a.sort_key())
            .then_with(|| a.computer.cmp(&b.computer))
    });
    Ok(snapshots)
}

/// The newest finished snapshot of `computer` of the requested kind, used as
/// incremental base.
pub fn latest_usable(
    destination: &Path,
    computer: &str,
    encrypted: bool,
    key: Option<&VaultKey>,
) -> Option<SnapshotInfo> {
    list(destination, key)
        .ok()?
        .into_iter()
        .flat_map(|mut s| {
            let companion = s.companion.take().map(|c| *c);
            std::iter::once(s).chain(companion)
        })
        .find(|s| {
            s.computer.eq_ignore_ascii_case(computer)
                && s.is_usable_base()
                && s.is_encrypted() == encrypted
        })
}

/// Whether another computer already writes backups into `destination`.
pub fn has_other_computers(destination: &Path, computer: &str) -> bool {
    list(destination, None).is_ok_and(|all| {
        all.iter()
            .any(|s| !s.computer.is_empty() && !s.computer.eq_ignore_ascii_case(computer))
    })
}

/// Resolve `latest` or a backup name. Names prefer this computer.
pub fn find(
    destination: &Path,
    wanted: &str,
    computer: &str,
    key: Option<&VaultKey>,
) -> EngineResult<SnapshotInfo> {
    let all = list(destination, key)?;
    let found = if wanted.eq_ignore_ascii_case("latest") {
        // This computer's newest backup; otherwise the newest one at all
        // (e.g. restoring on a new computer). Locked backups count as usable.
        let usable = |s: &SnapshotInfo| s.is_usable_base() || s.needs_unlock();
        let own = all
            .iter()
            .position(|s| s.computer.eq_ignore_ascii_case(computer) && usable(s))
            .or_else(|| all.iter().position(usable));
        own.map(|i| all[i].clone())
    } else {
        let mut matches: Vec<_> = all.into_iter().filter(|s| s.id == wanted).collect();
        matches.sort_by_key(|s| !s.computer.eq_ignore_ascii_case(computer));
        matches.into_iter().next()
    };
    found.ok_or_else(|| EngineError::SnapshotNotFound(wanted.to_string()))
}

fn computer_suffix(name: &str) -> Option<String> {
    let start = name.rfind(" (")?;
    name.ends_with(')')
        .then(|| name[start + 2..name.len() - 1].to_string())
}

/// Snapshot names: `2026-09-14 14-32`, optionally with `-2` and/or ` (PC)`.
pub fn looks_like_snapshot_id(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 16 {
        return false;
    }
    let digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    (0..4).all(digit)
        && bytes[4] == b'-'
        && digit(5)
        && digit(6)
        && bytes[7] == b'-'
        && digit(8)
        && digit(9)
        && bytes[10] == b' '
        && digit(11)
        && digit(12)
        && bytes[13] == b'-'
        && digit(14)
        && digit(15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_ids() {
        assert!(looks_like_snapshot_id("2026-09-14 14-32"));
        assert!(looks_like_snapshot_id("2026-09-14 14-32-2"));
        assert!(looks_like_snapshot_id("2026-09-14 14-32 (LAPTOP)"));
        assert!(!looks_like_snapshot_id("Documents"));
        assert!(!looks_like_snapshot_id("2026-09-14"));
        assert!(!looks_like_snapshot_id("2026-09-14_143205"));
        assert_eq!(
            computer_suffix("2026-09-14 14-32 (LAPTOP)").as_deref(),
            Some("LAPTOP")
        );
    }
}
