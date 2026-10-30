//! All writing file-system operations of the engine live here.
//!
//! Files are always written to `<name>.aeterna-partial` first and renamed
//! into place once complete, so an interrupted copy never leaves a truncated
//! file under the real name.

use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use super::crypto::{self, CryptoError, VaultKey};
use super::{CancelToken, format_sha256, from_unix_nanos, vault};

const BUFFER_SIZE: usize = 1024 * 1024;

/// Whether written files are flushed to the disk before they get their real
/// name (`advanced.flush_to_disk`). Without it, a power failure shortly after
/// a backup can leave files that are empty or incomplete on the disk.
static FLUSH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_flush(on: bool) {
    FLUSH.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn finish(output: BufWriter<File>) -> io::Result<()> {
    let file = output.into_inner().map_err(|e| e.into_error())?;
    if FLUSH.load(std::sync::atomic::Ordering::Relaxed) {
        file.sync_all()?;
    }
    Ok(())
}

/// Marks a backed-up file read-only. Unchanged files of later backups are
/// hard links to the same data, so editing one in a backup folder would change
/// every backup that contains it; read-only prevents that by accident.
pub fn set_readonly(path: &Path) {
    let result = fs::metadata(path).and_then(|m| {
        let mut permissions = m.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)
    });
    if let Err(err) = result {
        tracing::debug!("could not mark {} read-only: {err}", path.display());
    }
}

#[derive(Debug)]
pub enum CopyError {
    Io(io::Error),
    ChecksumMismatch { expected: String, actual: String },
    Crypto(CryptoError),
    Cancelled,
}

impl std::fmt::Display for CopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopyError::Io(e) => write!(f, "{e}"),
            CopyError::ChecksumMismatch { expected, actual } => {
                write!(f, "checksum mismatch (expected {expected}, got {actual})")
            }
            CopyError::Crypto(e) => write!(f, "{e}"),
            CopyError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<io::Error> for CopyError {
    fn from(e: io::Error) -> Self {
        CopyError::Io(e)
    }
}

impl From<CryptoError> for CopyError {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::Cancelled => CopyError::Cancelled,
            CryptoError::Io(io) => CopyError::Io(io),
            other => CopyError::Crypto(other),
        }
    }
}

fn partial_path(dst: &Path) -> PathBuf {
    let mut name = dst
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".aeterna-partial");
    dst.with_file_name(name)
}

/// Copy `src` to `dst` while computing SHA-256. If `expected` is given, the
/// result only replaces `dst` when the checksum matches.
///
/// A streaming copy is used instead of `std::fs::copy` (CopyFileEx) because
/// the checksum is computed in the same pass — every byte is read only once.
pub fn copy_hashed(
    src: &Path,
    dst: &Path,
    expected: Option<&str>,
    cancel: &CancelToken,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<String, CopyError> {
    write_verified(dst, expected, |output| {
        let mut input = File::open(src)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; BUFFER_SIZE];
        loop {
            if cancel.is_cancelled() {
                return Err(CopyError::Cancelled);
            }
            let n = match input.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            hasher.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
            on_bytes(n as u64);
        }
        Ok(format_sha256(&hasher.finalize()))
    })
}

/// Decrypts a vault blob into `dst`, verifying the plaintext checksum.
pub fn decrypt_blob(
    key: &VaultKey,
    blob: &Path,
    dst: &Path,
    expected: Option<&str>,
    cancel: &CancelToken,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<String, CopyError> {
    write_verified(dst, expected, |output| {
        let mut input = BufReader::with_capacity(BUFFER_SIZE, File::open(blob)?);
        let digest = key.decrypt_stream(&mut input, output, cancel, on_bytes)?;
        Ok(format_sha256(&digest))
    })
}

fn write_verified(
    dst: &Path,
    expected: Option<&str>,
    produce: impl FnOnce(&mut dyn Write) -> Result<String, CopyError>,
) -> Result<String, CopyError> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = partial_path(dst);
    let result = (|| {
        let mut output = BufWriter::with_capacity(BUFFER_SIZE, File::create(&partial)?);
        let hash = produce(&mut output)?;
        output.flush()?;
        finish(output)?;
        Ok(hash)
    })();
    let hash = match result {
        Ok(hash) => hash,
        Err(err) => {
            let _ = fs::remove_file(&partial);
            return Err(err);
        }
    };

    if let Some(expected) = expected
        && !expected.eq_ignore_ascii_case(&hash)
    {
        let _ = fs::remove_file(&partial);
        return Err(CopyError::ChecksumMismatch {
            expected: expected.to_string(),
            actual: hash,
        });
    }

    // `rename` replaces an existing file on Windows (MoveFileExW with REPLACE_EXISTING).
    if let Err(err) = fs::rename(&partial, dst) {
        let _ = fs::remove_file(&partial);
        return Err(err.into());
    }
    Ok(hash)
}

/// Encrypts a file into the vault. Returns `(blob id, plaintext SHA-256, stored new)`.
/// Content that already exists in the vault is not stored twice.
pub fn encrypt_into_vault(
    key: &VaultKey,
    destination: &Path,
    src: &Path,
    cancel: &CancelToken,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(String, String, bool), CopyError> {
    let blobs = vault::vault_dir(destination).join(vault::BLOB_DIR);
    fs::create_dir_all(&blobs)?;
    let temp = blobs.join(format!(
        ".tmp-{}.aeterna-partial",
        crypto::hex(&crypto::random_bytes::<8>())
    ));
    let result = (|| {
        let mut input = BufReader::with_capacity(BUFFER_SIZE, File::open(src)?);
        let mut output = BufWriter::with_capacity(BUFFER_SIZE, File::create(&temp)?);
        let digest = key.encrypt_stream(&mut input, &mut output, cancel, on_bytes)?;
        output.flush()?;
        finish(output)?;
        Ok::<_, CopyError>(digest)
    })();
    let digest = match result {
        Ok(digest) => digest,
        Err(err) => {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
    };

    let id = key.blob_id(&digest);
    let target = vault::blob_path(destination, &id);
    // The same content is stored once. An existing copy is reused only if it
    // is complete (same size as the new one); a damaged one is replaced.
    let new_len = fs::metadata(&temp).map(|m| m.len()).ok();
    if target.is_file() && fs::metadata(&target).map(|m| m.len()).ok() == new_len {
        let _ = fs::remove_file(&temp);
        return Ok((id, format_sha256(&digest), false));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(err) = fs::rename(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return Err(err.into());
    }
    Ok((id, format_sha256(&digest), true))
}

/// Restore the original modification time, so later comparisons (and the
/// user's view in Explorer) stay meaningful.
pub fn set_modified(path: &Path, modified: i64) -> io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_times(FileTimes::new().set_modified(from_unix_nanos(modified)))
}

pub fn hard_link(existing: &Path, new: &Path) -> io::Result<()> {
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::hard_link(existing, new)
}

pub fn create_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Create a new, not yet existing directory.
pub fn create_new_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = partial_path(path);
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

pub fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_bytes_atomic(path, &bytes)
}

pub fn remove_file(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

/// Only used for backup folders the user chose to delete (when the recycle
/// bin is not available) and for half-copied folders of AeternaVault itself.
pub fn remove_dir_all(path: &Path) -> io::Result<()> {
    fs::remove_dir_all(path)
}

/// Prevents two backups from writing into the same destination at once, e.g.
/// an automatic backup starting while one runs from the window.
pub struct DestinationLock {
    path: PathBuf,
}

impl DestinationLock {
    const FILE: &'static str = ".aeternavault.lock";
    /// A lock of another computer older than this is considered left over from a crash.
    const STALE_AFTER: Duration = Duration::from_secs(12 * 3600);

    pub fn acquire(destination: &Path) -> io::Result<Option<Self>> {
        fs::create_dir_all(destination)?;
        let path = destination.join(Self::FILE);
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let _ = writeln!(
                        file,
                        "{} {}",
                        std::process::id(),
                        crate::platform::computer_name()
                    );
                    return Ok(Some(Self { path }));
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if !Self::is_stale(&path) {
                        return Ok(None);
                    }
                    tracing::info!("removing a lock left over from an ended process");
                    let _ = fs::remove_file(&path);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }
}

impl DestinationLock {
    /// A lock of this computer is stale once its process has ended, however
    /// long a backup takes. A lock of another computer (shared drive) is
    /// stale after `STALE_AFTER`.
    fn is_stale(path: &Path) -> bool {
        let content = fs::read_to_string(path).unwrap_or_default();
        let mut fields = content.trim().splitn(2, ' ');
        let pid = fields.next().and_then(|p| p.parse::<u32>().ok());
        let computer = fields.next().unwrap_or_default();
        if let Some(pid) = pid
            && computer.eq_ignore_ascii_case(&crate::platform::computer_name())
            && let Some(running) = crate::platform::process_running(pid)
        {
            return !running;
        }
        fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() > Self::STALE_AFTER)
            .unwrap_or(true)
    }
}

impl Drop for DestinationLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
