//! Walks a source folder and collects the files to consider. Read-only.
//!
//! Decisions made here:
//! * Symbolic links and junctions are never followed. They cause loops and
//!   duplicates (older profiles contain junctions such as `My Music` inside
//!   `Documents`). On Linux a link is collected as a link, with the target it
//!   names; on Windows it is listed as skipped so nothing disappears silently.
//! * Pipes, sockets and device files are listed as skipped. They have no
//!   content to copy, and opening a pipe waits until something writes to it.
//! * Cloud placeholders (OneDrive "online-only" files) are skipped by default,
//!   because reading them would download them.
//! * The destination folder is skipped if it lies inside a source.
//! * Individually deselected sub-folders are not entered at all; when only a
//!   few paths are selected, only those are visited.
//! * Access errors do not abort the scan; they become skipped entries.

use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use super::selection::Selection;
use super::{
    CancelToken, Phase, Progress, Reporter, path_is_within, rel_to_string, safe_relative_path,
    to_unix_nanos,
};
use crate::error::{EngineError, EngineResult};

#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// Relative path, `/`-separated.
    pub rel: String,
    pub size: u64,
    pub modified: i64,
    /// A symbolic link: the target as the link names it. `size` is 0.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Link,
    /// A pipe, socket or device file.
    Special,
    OnlineOnly,
    InsideDestination,
    SourceMissing,
    NoAccess(String),
}

#[derive(Debug, Clone)]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    pub skipped: Vec<SkippedEntry>,
    pub excluded: u64,
}

pub struct ScanOptions<'a> {
    pub excludes: &'a Excludes,
    pub selection: Selection<'a>,
    pub skip_online_only: bool,
    pub destination: &'a Path,
}

/// Compiled exclude patterns, matched against names and relative paths.
pub struct Excludes {
    set: GlobSet,
    pub invalid: Vec<String>,
}

impl Excludes {
    pub fn new<'a>(patterns: impl IntoIterator<Item = &'a String>) -> Self {
        let mut builder = GlobSetBuilder::new();
        let mut invalid = Vec::new();
        for pattern in patterns {
            let pattern = pattern.trim();
            if pattern.is_empty() || pattern.starts_with('#') {
                continue;
            }
            match GlobBuilder::new(&pattern.replace('\\', "/"))
                .case_insensitive(true)
                .literal_separator(false)
                .build()
            {
                Ok(glob) => {
                    builder.add(glob);
                }
                Err(err) => {
                    tracing::warn!("ignoring invalid exclude pattern `{pattern}`: {err}");
                    invalid.push(pattern.to_string());
                }
            }
        }
        let set = builder.build().unwrap_or_else(|_| GlobSet::empty());
        Self { set, invalid }
    }

    pub fn is_excluded(&self, name: &str, rel: &str) -> bool {
        !self.set.is_empty() && (self.set.is_match(name) || self.set.is_match(rel))
    }
}

// Win32 file attribute bits (see `GetFileAttributesW` documentation).
#[cfg(windows)]
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
#[cfg(windows)]
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

fn is_online_only(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
            & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

pub fn scan_source(
    root: &Path,
    options: &ScanOptions<'_>,
    cancel: &CancelToken,
    progress: &mut Progress,
    reporter: &mut Reporter<'_>,
) -> EngineResult<ScanResult> {
    let mut result = ScanResult::default();

    if !root.is_dir() {
        result.skipped.push(SkippedEntry {
            path: root.to_path_buf(),
            reason: SkipReason::SourceMissing,
        });
        return Ok(result);
    }

    match options.selection.scan_roots() {
        None => walk(root, root, options, cancel, progress, reporter, &mut result)?,
        Some(starts) => {
            for start in starts {
                let Some(rel) = safe_relative_path(&start) else {
                    continue;
                };
                let path = root.join(rel);
                match std::fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_dir() => walk(
                        root,
                        &path,
                        options,
                        cancel,
                        progress,
                        reporter,
                        &mut result,
                    )?,
                    Ok(meta) if meta.is_file() => {
                        consider_file(root, &path, meta, options, progress, reporter, &mut result)
                    }
                    Ok(meta) => consider_link(root, &path, &meta, options, progress, &mut result),
                    // Selected items that do not exist (yet) are simply not there.
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => result.skipped.push(SkippedEntry {
                        path,
                        reason: SkipReason::NoAccess(err.to_string()),
                    }),
                }
            }
        }
    }
    Ok(result)
}

fn walk(
    root: &Path,
    start: &Path,
    options: &ScanOptions<'_>,
    cancel: &CancelToken,
    progress: &mut Progress,
    reporter: &mut Reporter<'_>,
    result: &mut ScanResult,
) -> EngineResult<()> {
    let mut walker = WalkDir::new(start).follow_links(false).into_iter();
    while let Some(next) = walker.next() {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }

        let entry = match next {
            Ok(entry) => entry,
            Err(err) => {
                let path = err
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| start.to_path_buf());
                result.skipped.push(SkippedEntry {
                    path,
                    reason: SkipReason::NoAccess(err.to_string()),
                });
                continue;
            }
        };

        let path = entry.path();
        let Ok(rel_path) = path.strip_prefix(root) else {
            continue;
        };
        if rel_path.as_os_str().is_empty() {
            continue;
        }
        let rel = rel_to_string(rel_path);
        let name = entry.file_name().to_string_lossy();
        let file_type = entry.file_type();

        if file_type.is_symlink() {
            if options.selection.is_included(&rel) {
                match entry.metadata() {
                    Ok(meta) => consider_link(root, path, &meta, options, progress, result),
                    Err(err) => result.skipped.push(SkippedEntry {
                        path: path.to_path_buf(),
                        reason: SkipReason::NoAccess(err.to_string()),
                    }),
                }
            }
            continue;
        }

        if file_type.is_dir() {
            if path_is_within(path, options.destination) {
                result.skipped.push(SkippedEntry {
                    path: path.to_path_buf(),
                    reason: SkipReason::InsideDestination,
                });
                walker.skip_current_dir();
            } else if options.excludes.is_excluded(&name, &rel)
                || !options.selection.should_enter(&rel)
            {
                result.excluded += 1;
                walker.skip_current_dir();
            }
            continue;
        }

        if options.excludes.is_excluded(&name, &rel) || !options.selection.is_included(&rel) {
            result.excluded += 1;
            continue;
        }

        // Neither a file nor a folder nor a link: never opened.
        if !file_type.is_file() {
            result.skipped.push(SkippedEntry {
                path: path.to_path_buf(),
                reason: SkipReason::Special,
            });
            continue;
        }

        match entry.metadata() {
            Ok(meta) => consider_file(root, path, meta, options, progress, reporter, result),
            Err(err) => result.skipped.push(SkippedEntry {
                path: path.to_path_buf(),
                reason: SkipReason::NoAccess(err.to_string()),
            }),
        }
    }
    Ok(())
}

fn consider_file(
    root: &Path,
    path: &Path,
    meta: std::fs::Metadata,
    options: &ScanOptions<'_>,
    progress: &mut Progress,
    reporter: &mut Reporter<'_>,
    result: &mut ScanResult,
) {
    let Ok(rel_path) = path.strip_prefix(root) else {
        return;
    };
    let rel = rel_to_string(rel_path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if options.excludes.is_excluded(&name, &rel) {
        result.excluded += 1;
        return;
    }
    if options.skip_online_only && is_online_only(&meta) {
        result.skipped.push(SkippedEntry {
            path: path.to_path_buf(),
            reason: SkipReason::OnlineOnly,
        });
        return;
    }

    let modified = meta.modified().map(to_unix_nanos).unwrap_or(0);
    result.files.push(ScannedFile {
        rel,
        size: meta.len(),
        modified,
        link: None,
    });

    progress.phase = Phase::Scanning;
    progress.files_done += 1;
    progress.bytes_done += meta.len();
    if progress.files_done.is_multiple_of(64) {
        progress.current = path.display().to_string();
        reporter.maybe(progress);
    }
}

/// A symbolic link, junction or other entry that is neither a file nor a
/// folder. It is never followed or opened. On Linux a symbolic link is
/// collected with its target; everything else is listed as skipped.
fn consider_link(
    root: &Path,
    path: &Path,
    meta: &std::fs::Metadata,
    options: &ScanOptions<'_>,
    progress: &mut Progress,
    result: &mut ScanResult,
) {
    let skip = |result: &mut ScanResult, reason| {
        result.skipped.push(SkippedEntry {
            path: path.to_path_buf(),
            reason,
        })
    };
    if cfg!(unix) && !meta.file_type().is_symlink() {
        return skip(result, SkipReason::Special);
    }
    if !cfg!(unix) {
        return skip(result, SkipReason::Link);
    }
    let Ok(rel_path) = path.strip_prefix(root) else {
        return;
    };
    let rel = rel_to_string(rel_path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if options.excludes.is_excluded(&name, &rel) {
        result.excluded += 1;
        return;
    }
    match std::fs::read_link(path) {
        Ok(target) => match target.to_str() {
            Some(target) if !target.is_empty() => {
                result.files.push(ScannedFile {
                    rel,
                    size: 0,
                    modified: meta.modified().map(to_unix_nanos).unwrap_or(0),
                    link: Some(target.to_string()),
                });
                progress.phase = Phase::Scanning;
                progress.files_done += 1;
            }
            // A target that is not valid text cannot be stored in the index.
            _ => skip(result, SkipReason::Link),
        },
        Err(err) => skip(result, SkipReason::NoAccess(err.to_string())),
    }
}

/// Quick size estimate of a folder for the source list (errors ignored).
pub fn folder_size(
    root: &Path,
    selection: &Selection<'_>,
    cancel: &CancelToken,
) -> Option<(u64, u64)> {
    if !root.is_dir() {
        return None;
    }
    let mut bytes = 0u64;
    let mut files = 0u64;
    let starts: Vec<PathBuf> = match selection.scan_roots() {
        None => vec![root.to_path_buf()],
        Some(roots) => roots
            .iter()
            .filter_map(|r| safe_relative_path(r))
            .map(|r| root.join(r))
            .collect(),
    };
    for start in starts {
        let mut walker = WalkDir::new(&start).follow_links(false).into_iter();
        while let Some(next) = walker.next() {
            let Ok(entry) = next else {
                continue;
            };
            if cancel.is_cancelled() {
                return None;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let rel = rel_to_string(rel);
            if entry.file_type().is_dir() {
                if !rel.is_empty() && !selection.should_enter(&rel) {
                    walker.skip_current_dir();
                }
            } else if entry.file_type().is_file()
                && selection.is_included(&rel)
                && let Ok(meta) = entry.metadata()
            {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    Some((bytes, files))
}
