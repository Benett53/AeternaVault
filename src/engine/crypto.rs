//! Cryptography for encrypted backups. See docs/ENCRYPTION.md for the format.
//!
//! Building blocks (RustCrypto, pure Rust, widely reviewed):
//! * **Argon2id** turns a passphrase (or recovery key) into a key-encryption key.
//! * **XChaCha20-Poly1305** (default) or **AES-256-GCM** encrypts and
//!   authenticates the data; the choice is made when a vault is created.
//! * **HMAC-SHA-256** derives sub-keys and content-based blob names.
//!
//! A random 256-bit *vault key* protects all data. It is stored only in
//! wrapped form (encrypted with the passphrase-derived key and, separately,
//! with the recovery-key-derived key), so the passphrase can be changed
//! without re-encrypting any backup.
//!
//! File contents use a chunked "STREAM" construction: 1 MiB chunks, each with
//! its own nonce made of a per-stream value, a 32-bit counter and a
//! final-chunk flag. Reordering, truncating or appending chunks is detected.

use std::io::{self, Read, Write};

use aes_gcm::Aes256Gcm;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use super::CancelToken;

pub const CHUNK_SIZE: usize = 1024 * 1024;
const TAG_SIZE: usize = 16;
const PREFIX_SIZE: usize = 19;
const SALT_SIZE: usize = 32;
/// Stream encrypted with XChaCha20-Poly1305.
pub const BLOB_MAGIC: &[u8; 4] = b"AVB1";
/// Stream encrypted with AES-256-GCM.
pub const GCM_MAGIC: &[u8; 4] = b"AVG1";

/// The cipher for the data of a vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cipher {
    /// Fast on every processor, 192-bit nonces. The default.
    #[default]
    XChaCha20Poly1305,
    /// The widely standardised alternative; fast with AES hardware support.
    Aes256Gcm,
}

impl Cipher {
    pub const ALL: [Cipher; 2] = [Cipher::XChaCha20Poly1305, Cipher::Aes256Gcm];

    /// Value of `cipher` in `vault.json`.
    pub fn header_name(self) -> &'static str {
        match self {
            Cipher::XChaCha20Poly1305 => "argon2id+xchacha20poly1305-stream-1mib",
            Cipher::Aes256Gcm => "argon2id+aes256gcm-stream-1mib",
        }
    }

    pub fn from_header_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.header_name() == name)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Cipher::XChaCha20Poly1305 => "XChaCha20-Poly1305",
            Cipher::Aes256Gcm => "AES-256-GCM",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("the passphrase or recovery key is not correct")]
    WrongKey,
    #[error("the encrypted data is damaged or was modified")]
    Damaged,
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system random generator is available");
    bytes
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&(*key).into())
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

// ---------------------------------------------------------------------------
// Key derivation and key slots
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl KdfParams {
    /// For passphrases: 64 MiB, 3 passes (about half a second on a typical PC).
    pub const PASSPHRASE: Self = Self {
        memory_kib: 64 * 1024,
        iterations: 3,
        parallelism: 1,
    };
    /// 256 MiB, 4 passes: about two seconds, much slower to guess.
    pub const STRONG: Self = Self {
        memory_kib: 256 * 1024,
        iterations: 4,
        parallelism: 1,
    };
    /// 1 GiB, 4 passes: for new computers; unlocking takes several seconds
    /// and needs that much free memory.
    pub const VERY_STRONG: Self = Self {
        memory_kib: 1024 * 1024,
        iterations: 4,
        parallelism: 1,
    };
    /// Recovery keys already carry 160 bits of randomness.
    pub const RECOVERY: Self = Self {
        memory_kib: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    };
    #[cfg(test)]
    pub const TEST: Self = Self {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
    };

    fn derive(&self, secret: &[u8], salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        let params = Params::new(self.memory_kib, self.iterations, self.parallelism, Some(32))
            .map_err(|e| CryptoError::Kdf(e.to_string()))?;
        let mut out = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(secret, salt, out.as_mut())
            .map_err(|e| CryptoError::Kdf(e.to_string()))?;
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlotKind {
    Passphrase,
    RecoveryKey,
}

/// The vault key, wrapped with a key derived from a passphrase or recovery key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeySlot {
    pub kind: SlotKind,
    pub kdf: KdfParams,
    pub salt: String,
    pub nonce: String,
    pub wrapped_key: String,
}

impl KeySlot {
    pub fn create(
        kind: SlotKind,
        secret: &str,
        vault: &VaultKey,
        kdf: KdfParams,
    ) -> Result<Self, CryptoError> {
        let secret = normalize_secret(kind, secret);
        let salt: [u8; 16] = random_bytes();
        let nonce: [u8; 24] = random_bytes();
        let kek = kdf.derive(secret.as_bytes(), &salt)?;
        let wrapped = cipher(&kek)
            .encrypt(&nonce.into(), &vault.master[..])
            .map_err(|_| CryptoError::Damaged)?;
        Ok(Self {
            kind,
            kdf,
            salt: hex(&salt),
            nonce: hex(&nonce),
            wrapped_key: hex(&wrapped),
        })
    }

    /// Unwraps the vault key; `data_cipher` is the vault's cipher from its header.
    pub fn open(&self, secret: &str, data_cipher: Cipher) -> Result<VaultKey, CryptoError> {
        let secret = normalize_secret(self.kind, secret);
        let salt = unhex(&self.salt).ok_or(CryptoError::Damaged)?;
        let nonce: [u8; 24] = unhex(&self.nonce)
            .and_then(|n| n.try_into().ok())
            .ok_or(CryptoError::Damaged)?;
        let wrapped = unhex(&self.wrapped_key).ok_or(CryptoError::Damaged)?;
        let kek = self.kdf.derive(secret.as_bytes(), &salt)?;
        let mut master = cipher(&kek)
            .decrypt(&nonce.into(), wrapped.as_slice())
            .map_err(|_| CryptoError::WrongKey)?;
        let key: [u8; 32] = master
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Damaged)?;
        master.zeroize();
        Ok(VaultKey::from_master(key, data_cipher))
    }
}

fn normalize_secret(kind: SlotKind, secret: &str) -> Zeroizing<String> {
    Zeroizing::new(match kind {
        // Passphrases are used exactly as typed (only Unicode-normalised by the OS input).
        SlotKind::Passphrase => secret.to_string(),
        // Recovery keys are forgiving: case, spaces and dashes do not matter.
        SlotKind::RecoveryKey => secret
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect(),
    })
}

/// A new random recovery key, e.g. `K7QM-2HXA-…` (8 groups, 160 bits).
pub fn new_recovery_key() -> String {
    // Crockford base32 without easily confused characters.
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let bytes: [u8; 20] = random_bytes();
    let mut bits = 0u32;
    let mut bit_count = 0;
    let mut chars = String::new();
    for byte in bytes {
        bits = (bits << 8) | u32::from(byte);
        bit_count += 8;
        while bit_count >= 5 {
            bit_count -= 5;
            chars.push(ALPHABET[((bits >> bit_count) & 31) as usize] as char);
        }
    }
    chars
        .as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

// ---------------------------------------------------------------------------
// Vault key and data encryption
// ---------------------------------------------------------------------------

pub struct VaultKey {
    master: Zeroizing<[u8; 32]>,
    data: Zeroizing<[u8; 32]>,
    names: Zeroizing<[u8; 32]>,
    cipher: Cipher,
}

impl Clone for VaultKey {
    fn clone(&self) -> Self {
        Self::from_master(*self.master, self.cipher)
    }
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(…)")
    }
}

/// The AEAD for one encrypted stream.
enum StreamCipher {
    /// Nonce: 19 random bytes ‖ counter (u32, big-endian) ‖ last-chunk flag.
    XChaCha {
        aead: XChaCha20Poly1305,
        prefix: [u8; PREFIX_SIZE],
    },
    /// Key: HMAC-SHA-256(data key, label ‖ random salt), so every stream has
    /// its own key. Nonce: 7 zero bytes ‖ counter (u32, big-endian) ‖ flag.
    Gcm(Box<Aes256Gcm>),
}

impl StreamCipher {
    fn seal(&self, counter: u32, last: bool, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        match self {
            StreamCipher::XChaCha { aead, prefix } => aead
                .encrypt(&chunk_nonce(prefix, counter, last).into(), data)
                .map_err(|_| CryptoError::Damaged),
            StreamCipher::Gcm(aead) => aead
                .encrypt(&gcm_nonce(counter, last).into(), data)
                .map_err(|_| CryptoError::Damaged),
        }
    }

    fn open(&self, counter: u32, last: bool, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        match self {
            StreamCipher::XChaCha { aead, prefix } => aead
                .decrypt(&chunk_nonce(prefix, counter, last).into(), data)
                .map_err(|_| CryptoError::Damaged),
            StreamCipher::Gcm(aead) => aead
                .decrypt(&gcm_nonce(counter, last).into(), data)
                .map_err(|_| CryptoError::Damaged),
        }
    }
}

impl VaultKey {
    pub fn generate(cipher: Cipher) -> Self {
        Self::from_master(random_bytes(), cipher)
    }

    pub fn from_master(master: [u8; 32], cipher: Cipher) -> Self {
        Self {
            data: Zeroizing::new(hmac(&master, b"AeternaVault v1 data key")),
            names: Zeroizing::new(hmac(&master, b"AeternaVault v1 name key")),
            master: Zeroizing::new(master),
            cipher,
        }
    }

    pub fn master_bytes(&self) -> &[u8; 32] {
        &self.master
    }

    #[cfg(test)]
    pub fn cipher(&self) -> Cipher {
        self.cipher
    }

    /// Blob name for a file content: unlinkable to the content without the key,
    /// identical for identical content (deduplication).
    pub fn blob_id(&self, content_sha256: &[u8]) -> String {
        hex(&hmac(self.names.as_ref(), content_sha256))
    }

    /// Checks that a key belongs to a vault without decrypting any data.
    pub fn fingerprint(&self) -> String {
        hex(&hmac(self.master.as_ref(), b"AeternaVault v1 key check")[..8])
    }

    fn gcm_stream(&self, salt: &[u8; SALT_SIZE]) -> StreamCipher {
        let mut input = Vec::with_capacity(40 + SALT_SIZE);
        input.extend_from_slice(b"AeternaVault v1 AES-GCM stream key");
        input.extend_from_slice(salt);
        let key = Zeroizing::new(hmac(self.data.as_ref(), &input));
        StreamCipher::Gcm(Box::new(Aes256Gcm::new(&(*key).into())))
    }

    /// Writes the stream header for this key's cipher.
    fn start_stream(&self, output: &mut dyn Write) -> Result<StreamCipher, CryptoError> {
        match self.cipher {
            Cipher::XChaCha20Poly1305 => {
                let prefix: [u8; PREFIX_SIZE] = random_bytes();
                output.write_all(BLOB_MAGIC)?;
                output.write_all(&prefix)?;
                Ok(StreamCipher::XChaCha {
                    aead: cipher(&self.data),
                    prefix,
                })
            }
            Cipher::Aes256Gcm => {
                let salt: [u8; SALT_SIZE] = random_bytes();
                output.write_all(GCM_MAGIC)?;
                output.write_all(&salt)?;
                Ok(self.gcm_stream(&salt))
            }
        }
    }

    /// Reads a stream header. Both ciphers can be read with any key of the
    /// vault, because the header names the cipher.
    fn resume_stream(&self, input: &mut dyn Read) -> Result<StreamCipher, CryptoError> {
        let mut magic = [0u8; 4];
        input
            .read_exact(&mut magic)
            .map_err(|_| CryptoError::Damaged)?;
        if &magic == BLOB_MAGIC {
            let mut prefix = [0u8; PREFIX_SIZE];
            input
                .read_exact(&mut prefix)
                .map_err(|_| CryptoError::Damaged)?;
            Ok(StreamCipher::XChaCha {
                aead: cipher(&self.data),
                prefix,
            })
        } else if &magic == GCM_MAGIC {
            let mut salt = [0u8; SALT_SIZE];
            input
                .read_exact(&mut salt)
                .map_err(|_| CryptoError::Damaged)?;
            Ok(self.gcm_stream(&salt))
        } else {
            Err(CryptoError::Damaged)
        }
    }

    /// Encrypts a stream. Returns the SHA-256 of the plaintext.
    pub fn encrypt_stream(
        &self,
        input: &mut dyn Read,
        output: &mut dyn Write,
        cancel: &CancelToken,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<[u8; 32], CryptoError> {
        let stream = self.start_stream(output)?;
        let mut hasher = Sha256::new();
        let mut current = read_up_to(input, CHUNK_SIZE)?;
        let mut counter: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(CryptoError::Cancelled);
            }
            let next = if current.len() == CHUNK_SIZE {
                read_up_to(input, CHUNK_SIZE)?
            } else {
                Vec::new()
            };
            let last = next.is_empty();
            hasher.update(&current);
            let sealed = stream.seal(counter, last, &current)?;
            output.write_all(&sealed)?;
            on_bytes(current.len() as u64);
            current.zeroize();
            if last {
                break;
            }
            current = next;
            counter = counter.checked_add(1).ok_or(CryptoError::Damaged)?;
        }
        output.flush()?;
        Ok(hasher.finalize().into())
    }

    /// Decrypts a stream written by [`encrypt_stream`]. Returns the SHA-256 of
    /// the plaintext. Output written before an error must be discarded by the
    /// caller.
    pub fn decrypt_stream(
        &self,
        input: &mut dyn Read,
        output: &mut dyn Write,
        cancel: &CancelToken,
        on_bytes: &mut dyn FnMut(u64),
    ) -> Result<[u8; 32], CryptoError> {
        let stream = self.resume_stream(input)?;
        let sealed_size = CHUNK_SIZE + TAG_SIZE;
        let mut hasher = Sha256::new();
        let mut current = read_up_to(input, sealed_size)?;
        if current.len() < TAG_SIZE {
            return Err(CryptoError::Damaged);
        }
        let mut counter: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(CryptoError::Cancelled);
            }
            let next = if current.len() == sealed_size {
                read_up_to(input, sealed_size)?
            } else {
                Vec::new()
            };
            let last = next.is_empty();
            let mut plain = stream.open(counter, last, &current)?;
            hasher.update(&plain);
            output.write_all(&plain)?;
            on_bytes(plain.len() as u64);
            plain.zeroize();
            if last {
                break;
            }
            if next.len() < TAG_SIZE {
                return Err(CryptoError::Damaged);
            }
            current = next;
            counter = counter.checked_add(1).ok_or(CryptoError::Damaged)?;
        }
        output.flush()?;
        Ok(hasher.finalize().into())
    }

    pub fn encrypt_bytes(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 64);
        self.encrypt_stream(
            &mut &data[..],
            &mut out,
            &CancelToken::default(),
            &mut |_| {},
        )
        .expect("encrypting into memory cannot fail");
        out
    }

    pub fn decrypt_bytes(&self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = Vec::new();
        self.decrypt_stream(
            &mut &data[..],
            &mut out,
            &CancelToken::default(),
            &mut |_| {},
        )?;
        Ok(out)
    }
}

fn chunk_nonce(prefix: &[u8; PREFIX_SIZE], counter: u32, last: bool) -> [u8; 24] {
    let mut nonce = [0u8; 24];
    nonce[..PREFIX_SIZE].copy_from_slice(prefix);
    nonce[PREFIX_SIZE..PREFIX_SIZE + 4].copy_from_slice(&counter.to_be_bytes());
    nonce[23] = u8::from(last);
    nonce
}

fn gcm_nonce(counter: u32, last: bool) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[7..11].copy_from_slice(&counter.to_be_bytes());
    nonce[11] = u8::from(last);
    nonce
}

fn read_up_to(input: &mut dyn Read, size: usize) -> io::Result<Vec<u8>> {
    let mut buffer = Vec::with_capacity(size.min(CHUNK_SIZE + TAG_SIZE));
    input.take(size as u64).read_to_end(&mut buffer)?;
    Ok(buffer)
}

pub fn hex(bytes: &[u8]) -> String {
    super::format_sha256(bytes)
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// The exact size of an encrypted stream of `plain` bytes (format from its
/// first four bytes). `None` for an unknown format.
pub fn stream_size(magic: &[u8; 4], plain: u64) -> Option<u64> {
    let header = if magic == BLOB_MAGIC {
        4 + PREFIX_SIZE as u64
    } else if magic == GCM_MAGIC {
        4 + 32
    } else {
        return None;
    };
    let chunks = plain.div_ceil(CHUNK_SIZE as u64).max(1);
    Some(header + plain + chunks * TAG_SIZE as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(cipher: Cipher, size: usize) {
        let key = VaultKey::generate(cipher);
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let sealed = key.encrypt_bytes(&data);
        assert_eq!(key.decrypt_bytes(&sealed).unwrap(), data);
    }

    #[test]
    fn roundtrips_various_sizes_with_both_ciphers() {
        for cipher in Cipher::ALL {
            for size in [
                0,
                1,
                100,
                CHUNK_SIZE - 1,
                CHUNK_SIZE,
                CHUNK_SIZE + 1,
                2 * CHUNK_SIZE + 17,
            ] {
                roundtrip(cipher, size);
            }
        }
    }

    #[test]
    fn a_key_reads_streams_of_either_cipher() {
        let master = random_bytes();
        let chacha = VaultKey::from_master(master, Cipher::XChaCha20Poly1305);
        let gcm = VaultKey::from_master(master, Cipher::Aes256Gcm);
        let sealed = gcm.encrypt_bytes(b"archive");
        assert_eq!(&sealed[..4], GCM_MAGIC);
        assert_eq!(chacha.decrypt_bytes(&sealed).unwrap(), b"archive");
        // The same plaintext never gives the same ciphertext.
        assert_ne!(sealed, gcm.encrypt_bytes(b"archive"));
    }

    #[test]
    fn detects_tampering_truncation_and_wrong_key() {
        for cipher in Cipher::ALL {
            let key = VaultKey::generate(cipher);
            let data = vec![7u8; CHUNK_SIZE + 5];
            let sealed = key.encrypt_bytes(&data);
            let header = match cipher {
                Cipher::XChaCha20Poly1305 => 4 + PREFIX_SIZE,
                Cipher::Aes256Gcm => 4 + SALT_SIZE,
            };

            let mut flipped = sealed.clone();
            flipped[header + 3] ^= 1;
            assert!(matches!(
                key.decrypt_bytes(&flipped),
                Err(CryptoError::Damaged)
            ));

            // Cutting off the final chunk leaves a chunk that is not marked as last.
            let truncated = &sealed[..header + CHUNK_SIZE + TAG_SIZE];
            assert!(key.decrypt_bytes(truncated).is_err());

            let other = VaultKey::generate(cipher);
            assert!(other.decrypt_bytes(&sealed).is_err());
        }
    }

    #[test]
    fn key_slots_unlock_only_with_the_right_secret() {
        let key = VaultKey::generate(Cipher::default());
        let slot =
            KeySlot::create(SlotKind::Passphrase, "correct horse", &key, KdfParams::TEST).unwrap();
        assert!(matches!(
            slot.open("wrong", Cipher::default()),
            Err(CryptoError::WrongKey)
        ));
        let opened = slot.open("correct horse", Cipher::default()).unwrap();
        assert_eq!(opened.master_bytes(), key.master_bytes());

        let recovery = new_recovery_key();
        assert_eq!(recovery.len(), 32 + 7);
        let slot =
            KeySlot::create(SlotKind::RecoveryKey, &recovery, &key, KdfParams::TEST).unwrap();
        let sloppy = recovery.to_lowercase().replace('-', " ");
        assert_eq!(
            slot.open(&sloppy, Cipher::default())
                .unwrap()
                .master_bytes(),
            key.master_bytes()
        );
    }

    #[test]
    fn blob_ids_are_deterministic_per_key() {
        let a = VaultKey::generate(Cipher::default());
        let b = VaultKey::generate(Cipher::default());
        let digest = Sha256::digest(b"content");
        assert_eq!(a.blob_id(&digest), a.blob_id(&digest));
        assert_ne!(a.blob_id(&digest), b.blob_id(&digest));
    }
}
