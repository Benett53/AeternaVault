//! The encrypted vault of a backup destination: the keys and the encrypted
//! file contents shared by all encrypted backups.
//!
//! ```text
//! <destination>\
//!   .aeternavault\                    hidden
//!     vault.json                       format, vault id, wrapped keys (no secrets)
//!     blobs\3f\3fa9…c1.avb             encrypted file contents, named by keyed hash
//!   2026-09-14 20-00\
//!     Open with AeternaVault.avault    double-click to browse
//!     .aeternavault\encrypted.avs      encrypted backup index (see `manifest`)
//! ```
//!
//! `vault.json` is required: it holds the vault key, encrypted with the
//! passphrase and with the recovery key. Identical content is stored once;
//! unchanged files are never uploaded again, which keeps cloud synchronisation
//! small. File names, folder structure and sizes of individual files stay
//! hidden inside the encrypted index.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::crypto::{self, Cipher, CryptoError, KdfParams, KeySlot, SlotKind, VaultKey};
use crate::platform;

/// Hidden folder with `vault.json` and the blobs (same name as a backup's meta folder).
pub const VAULT_DIR: &str = super::manifest::META_DIR;
pub const VAULT_FILE: &str = "vault.json";
pub const BLOB_DIR: &str = "blobs";
pub const BLOB_EXT: &str = "avb";
/// The small file that opens encrypted backups by double-click (see
/// `platform::file_association`). Written into every encrypted backup folder.
pub const OPEN_FILE: &str = "Open with AeternaVault.avault";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultHeader {
    pub format: u32,
    pub vault_id: String,
    pub created_at: DateTime<Utc>,
    pub cipher: String,
    pub key_check: String,
    pub slots: Vec<KeySlot>,
}

impl VaultHeader {
    /// The data cipher; an unknown name means a newer, unsupported format.
    pub fn data_cipher(&self) -> Result<Cipher, CryptoError> {
        Cipher::from_header_name(&self.cipher).ok_or_else(|| {
            CryptoError::Io(io::Error::other(format!(
                "this vault uses a newer format ({}); please update AeternaVault",
                self.cipher
            )))
        })
    }

    pub fn slot(&self, kind: SlotKind) -> Option<&KeySlot> {
        self.slots.iter().find(|s| s.kind == kind)
    }
}

/// Choices made when a vault is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultOptions {
    pub cipher: Cipher,
    /// Key derivation for the passphrase.
    pub kdf: KdfParams,
}

impl Default for VaultOptions {
    fn default() -> Self {
        Self {
            cipher: Cipher::default(),
            kdf: KdfParams::PASSPHRASE,
        }
    }
}

pub fn vault_dir(destination: &Path) -> PathBuf {
    destination.join(VAULT_DIR)
}

pub fn exists(destination: &Path) -> bool {
    vault_dir(destination).join(VAULT_FILE).is_file()
}

pub fn read_header(destination: &Path) -> io::Result<VaultHeader> {
    let text = fs::read_to_string(vault_dir(destination).join(VAULT_FILE))?;
    serde_json::from_str(&text).map_err(io::Error::other)
}

/// The destination that belongs to a path someone opened: the destination
/// itself, a backup folder, or a file inside one (such as the `.avault` file).
pub fn destination_for_opened(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| exists(candidate) || super::snapshots::contains_backups(candidate))
        .map(Path::to_path_buf)
}

/// Unlocks with a passphrase or a recovery key (tries every slot).
pub fn unlock(destination: &Path, secret: &str) -> Result<(VaultHeader, VaultKey), CryptoError> {
    let header = read_header(destination)?;
    let (key, _) = open_slots(&header, secret)?;
    Ok((header, key))
}

/// Which kind of secret was entered, without keeping the key (used to test a
/// recovery key).
pub fn check_secret(destination: &Path, secret: &str) -> Result<SlotKind, CryptoError> {
    let header = read_header(destination)?;
    open_slots(&header, secret).map(|(_, kind)| kind)
}

fn open_slots(header: &VaultHeader, secret: &str) -> Result<(VaultKey, SlotKind), CryptoError> {
    let cipher = header.data_cipher()?;
    let mut last_error = CryptoError::WrongKey;
    for slot in &header.slots {
        match slot.open(secret, cipher) {
            Ok(key) if key.fingerprint() == header.key_check => return Ok((key, slot.kind)),
            Ok(_) => last_error = CryptoError::Damaged,
            Err(CryptoError::WrongKey) => {}
            Err(err) => last_error = err,
        }
    }
    Err(last_error)
}

/// What the user needs to keep after creating a vault.
pub struct NewVault {
    pub header: VaultHeader,
    pub key: VaultKey,
    pub recovery_key: String,
}

/// Creates a new vault. Fails if one already exists at the destination.
pub fn create(
    destination: &Path,
    passphrase: &str,
    options: VaultOptions,
) -> Result<NewVault, CryptoError> {
    create_with(destination, passphrase, options, KdfParams::RECOVERY)
}

pub(crate) fn create_with(
    destination: &Path,
    passphrase: &str,
    options: VaultOptions,
    recovery_kdf: KdfParams,
) -> Result<NewVault, CryptoError> {
    let dir = vault_dir(destination);
    if dir.join(VAULT_FILE).exists() {
        return Err(CryptoError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "an encrypted vault already exists at this destination",
        )));
    }
    fs::create_dir_all(dir.join(BLOB_DIR))?;
    platform::set_hidden(&dir);

    let key = VaultKey::generate(options.cipher);
    let recovery_key = crypto::new_recovery_key();
    let header = VaultHeader {
        format: 1,
        vault_id: crypto::hex(&crypto::random_bytes::<8>()),
        created_at: Utc::now(),
        cipher: options.cipher.header_name().into(),
        key_check: key.fingerprint(),
        slots: vec![
            KeySlot::create(SlotKind::Passphrase, passphrase, &key, options.kdf)?,
            KeySlot::create(SlotKind::RecoveryKey, &recovery_key, &key, recovery_kdf)?,
        ],
    };
    write_header(destination, &header)?;
    Ok(NewVault {
        header,
        key,
        recovery_key,
    })
}

fn write_header(destination: &Path, header: &VaultHeader) -> io::Result<()> {
    let dir = vault_dir(destination);
    let tmp = dir.join("vault.json.tmp");
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(header).map_err(io::Error::other)?,
    )?;
    fs::rename(tmp, dir.join(VAULT_FILE))
}

/// Writes the double-click file into an encrypted backup's folder.
pub fn write_open_file(backup_dir: &Path) {
    let path = backup_dir.join(OPEN_FILE);
    if !path.exists()
        && let Err(err) = fs::write(&path, OPEN_FILE_CONTENT)
    {
        tracing::debug!("could not write {}: {err}", path.display());
    }
}

fn checked_header(destination: &Path, key: &VaultKey) -> Result<VaultHeader, CryptoError> {
    let header = read_header(destination)?;
    if header.key_check != key.fingerprint() {
        return Err(CryptoError::WrongKey);
    }
    Ok(header)
}

/// Replaces the passphrase slot. The recovery key and all data stay valid; the
/// key derivation strength stays as chosen.
pub fn change_passphrase(
    destination: &Path,
    key: &VaultKey,
    new_passphrase: &str,
) -> Result<(), CryptoError> {
    let mut header = checked_header(destination, key)?;
    let kdf = header
        .slot(SlotKind::Passphrase)
        .map(|s| s.kdf)
        .unwrap_or(KdfParams::PASSPHRASE);
    header.slots.retain(|s| s.kind != SlotKind::Passphrase);
    header.slots.insert(
        0,
        KeySlot::create(SlotKind::Passphrase, new_passphrase, key, kdf)?,
    );
    write_header(destination, &header)?;
    Ok(())
}

/// Creates a new recovery key; the old one stops working. Returns the new key.
pub fn replace_recovery_key(destination: &Path, key: &VaultKey) -> Result<String, CryptoError> {
    let mut header = checked_header(destination, key)?;
    let recovery_key = crypto::new_recovery_key();
    let kdf = header
        .slot(SlotKind::RecoveryKey)
        .map(|s| s.kdf)
        .unwrap_or(KdfParams::RECOVERY);
    header.slots.retain(|s| s.kind != SlotKind::RecoveryKey);
    header.slots.push(KeySlot::create(
        SlotKind::RecoveryKey,
        &recovery_key,
        key,
        kdf,
    )?);
    write_header(destination, &header)?;
    Ok(recovery_key)
}

pub fn blob_path(destination: &Path, id: &str) -> PathBuf {
    let shard = id.get(..2).unwrap_or("00");
    vault_dir(destination)
        .join(BLOB_DIR)
        .join(shard)
        .join(format!("{id}.{BLOB_EXT}"))
}

/// The encrypted index of backup `id`.
pub fn snapshot_path(destination: &Path, id: &str) -> PathBuf {
    destination
        .join(id)
        .join(super::manifest::META_DIR)
        .join(super::manifest::ENCRYPTED_FILE)
}

// ---------------------------------------------------------------------------
// Remembering the key on this computer (for automatic backups)
// ---------------------------------------------------------------------------

fn remembered_key_file(key_dir: &Path, vault_id: &str) -> PathBuf {
    key_dir.join(format!("vault-{vault_id}.key"))
}

/// Stores the vault key for automatic backups. On Windows it is protected by
/// DPAPI (only this Windows user on this computer can read it); on Linux the
/// file is readable by the owner only. The passphrase itself is never stored.
pub fn remember_key(key_dir: &Path, header: &VaultHeader, key: &VaultKey) -> io::Result<()> {
    fs::create_dir_all(key_dir)?;
    let protected = platform::protect_for_user(key.master_bytes())?;
    platform::write_private_file(&remembered_key_file(key_dir, &header.vault_id), &protected)
}

pub fn remembered_key(key_dir: &Path, header: &VaultHeader) -> Option<VaultKey> {
    let protected = fs::read(remembered_key_file(key_dir, &header.vault_id)).ok()?;
    let bytes = platform::unprotect_for_user(&protected).ok()?;
    let master: [u8; 32] = bytes.as_slice().try_into().ok()?;
    let key = VaultKey::from_master(master, header.data_cipher().ok()?);
    (key.fingerprint() == header.key_check).then_some(key)
}

pub fn is_remembered(key_dir: &Path, header: &VaultHeader) -> bool {
    remembered_key_file(key_dir, &header.vault_id).is_file()
}

pub fn forget_key(key_dir: &Path, header: &VaultHeader) -> io::Result<()> {
    match fs::remove_file(remembered_key_file(key_dir, &header.vault_id)) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

const OPEN_FILE_CONTENT: &str = "\
AeternaVault encrypted backup
Double-click this file to open the encrypted backups in this destination with
AeternaVault, or run: aeternavault-cli list --destination \"<this folder's parent>\"
https://github.com/Benett53/AeternaVault
";

/// Whether an encrypted blob has exactly the size its content needs. A blob
/// cut short (e.g. by a power failure) is stored again instead of reused.
pub fn blob_intact(path: &Path, plain_size: u64) -> bool {
    use std::io::Read;
    let Ok(len) = fs::metadata(path).map(|m| m.len()) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_err()
    {
        return false;
    }
    crypto::stream_size(&magic, plain_size) == Some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_options(cipher: Cipher) -> VaultOptions {
        VaultOptions {
            cipher,
            kdf: KdfParams::TEST,
        }
    }

    #[test]
    fn create_unlock_change_passphrase_and_recovery_key() {
        for cipher in Cipher::ALL {
            let tmp = tempfile::tempdir().unwrap();
            let created = create_with(
                tmp.path(),
                "first passphrase",
                test_options(cipher),
                KdfParams::TEST,
            )
            .unwrap();
            assert!(exists(tmp.path()));
            assert!(matches!(
                unlock(tmp.path(), "nope"),
                Err(CryptoError::WrongKey)
            ));

            let (header, key) = unlock(tmp.path(), "first passphrase").unwrap();
            assert_eq!(header.data_cipher().unwrap(), cipher);
            assert_eq!(key.cipher(), cipher);
            assert_eq!(key.master_bytes(), created.key.master_bytes());
            assert_eq!(
                check_secret(tmp.path(), &created.recovery_key).unwrap(),
                SlotKind::RecoveryKey
            );

            change_passphrase(tmp.path(), &key, "second passphrase").unwrap();
            assert!(unlock(tmp.path(), "first passphrase").is_err());
            assert!(unlock(tmp.path(), "second passphrase").is_ok());
            assert!(unlock(tmp.path(), &created.recovery_key).is_ok());

            let new_recovery = replace_recovery_key(tmp.path(), &key).unwrap();
            assert!(unlock(tmp.path(), &created.recovery_key).is_err());
            assert!(unlock(tmp.path(), &new_recovery).is_ok());
            assert!(unlock(tmp.path(), "second passphrase").is_ok());

            // Opening any path inside finds the destination again.
            let avs = snapshot_path(tmp.path(), "2026-09-14 20-00");
            assert_eq!(destination_for_opened(&avs).as_deref(), Some(tmp.path()));
        }
    }

    #[test]
    fn remembered_key_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let created = create_with(
            tmp.path(),
            "pass",
            test_options(Cipher::Aes256Gcm),
            KdfParams::TEST,
        )
        .unwrap();
        let keys = tmp.path().join("keys");
        remember_key(&keys, &created.header, &created.key).unwrap();
        let loaded = remembered_key(&keys, &created.header).unwrap();
        assert_eq!(loaded.master_bytes(), created.key.master_bytes());
        assert_eq!(loaded.cipher(), Cipher::Aes256Gcm);
        forget_key(&keys, &created.header).unwrap();
        assert!(remembered_key(&keys, &created.header).is_none());
    }
}
