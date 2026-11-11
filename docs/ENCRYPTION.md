# Encrypted backups

This document describes exactly how AeternaVault protects encrypted backups and
how they are stored, so the format can be checked and re-implemented without
AeternaVault. [`tools/aeterna-decrypt.py`](../tools/aeterna-decrypt.py) is such
an independent implementation (about 250 lines of Python using the
`cryptography` and `argon2-cffi` packages); it is tested against vaults written
by AeternaVault with both ciphers.

> **Status:** AeternaVault has not had an external security audit. The design
> only combines standard, widely reviewed primitives from the RustCrypto
> project, but the way they are combined is AeternaVault's own. Please read the
> threat model below and keep a second copy of important data.

## Goals

- Nobody without the passphrase or the recovery key can read file contents,
  file names or the folder structure.
- Damaged or modified data is detected; it is never restored silently.
- Unchanged files are stored once, so synchronising a vault to a cloud folder
  only uploads what is new.
- Changing the passphrase or the recovery key does not re-encrypt anything.
- Automatic backups can run without typing the passphrase, without storing it.
- The data stays readable without AeternaVault.

Not goals: hiding *that* backups exist, their dates, their total size, the
number of stored contents or the size of each one (a blob is about as large as
the file it holds; identical content is stored once, which is visible too).

## Choices when setting up

| Choice | Options | Default |
|---|---|---|
| Cipher | XChaCha20-Poly1305 · AES-256-GCM | XChaCha20-Poly1305 |
| Passphrase key derivation | Argon2id 64 MiB / 3 passes · 256 MiB / 4 · 1 GiB / 4 (parallelism 1) | 64 MiB / 3 |
| What is encrypted | everything · only marked folders and files | everything |

Both ciphers are authenticated encryption with 256-bit keys. XChaCha20-Poly1305
is fast on every processor and has 192-bit nonces; AES-256-GCM is the more widely
standardised choice and fast on processors with AES instructions. There is no
known reason to prefer one for security.

## Building blocks

| Purpose | Algorithm | Crate |
|---|---|---|
| Passphrase → key | Argon2id, version 0x13, 32-byte output | `argon2` |
| Key wrapping | XChaCha20-Poly1305 | `chacha20poly1305` |
| Data encryption | XChaCha20-Poly1305 or AES-256-GCM | `chacha20poly1305`, `aes-gcm` |
| Sub-keys, content names | HMAC-SHA-256 | `hmac`, `sha2` |
| Randomness | the operating system's secure random generator | `getrandom` |
| Key for automatic backups | Windows: DPAPI (current user). Linux: file readable by the owner only | `windows-sys`, `libc` |

## Keys

```text
passphrase ──Argon2id(salt₁, kdf₁)──► KEK₁ ─┐
                                              ├─ XChaCha20-Poly1305 wraps ─► vault key (32 random bytes)
recovery key ─Argon2id(salt₂, kdf₂)─► KEK₂ ─┘

data key  = HMAC-SHA-256(key = vault key, message = "AeternaVault v1 data key")
name key  = HMAC-SHA-256(key = vault key, message = "AeternaVault v1 name key")
key check = HMAC-SHA-256(key = vault key, message = "AeternaVault v1 key check")[0..8], hex
```

All strings are ASCII without a terminating zero.

- **Passphrase** bytes are the UTF-8 encoding exactly as typed.
- **Recovery key**: 20 random bytes written as 32 Crockford Base32 characters
  (alphabet `0123456789ABCDEFGHJKMNPQRSTVWXYZ`, 5 bits per character,
  most significant bit first) in eight groups of four joined by `-`. For key
  derivation, every character that is not an ASCII letter or digit is removed and
  letters are upper-cased (so `k7qm 2hxa …` works too).
- A **key slot** is opened by deriving the KEK with the slot's salt and
  parameters, then decrypting `wrapped_key` with XChaCha20-Poly1305
  (key = KEK, nonce = the slot's 24-byte nonce, no associated data). The result
  is accepted only if its key check equals `key_check` in `vault.json`.
- **Remember on this computer** stores the 32-byte vault key (not the passphrase)
  so automatic backups run unattended:
  - Windows: encrypted with DPAPI in `%APPDATA%\AeternaVault\keys\vault-<vault id>.key`.
    Only the same Windows user on the same computer can decrypt it.
  - Linux: in `~/.config/aeternavault/keys/vault-<vault id>.key` with mode `0600`.
    Anyone who can read your home folder as you (or as root) can use it.

  AeternaVault uses the remembered key only to write, list, check, move and delete
  backups. Browsing, opening, extracting and restoring always require the
  passphrase or the recovery key to be entered. This is a rule of the program,
  not of the cryptography: software running as your user could read the key file.

### Replacing keys

- *Change passphrase* replaces the passphrase slot (same Argon2id strength).
- *New recovery key* replaces the recovery-key slot; the old key stops working.
- The **vault key itself cannot be replaced** without re-encrypting every backup.
  If you believe the vault key was exposed (for example, malware ran as your
  Windows user while the key was remembered), set up encryption at a new
  destination and make a fresh backup there.

## Layout

```text
<destination>/
  .aeternavault/                      hidden; shared by all encrypted backups
    vault.json                        public header (see below)
    blobs/3f/3fa9…c1.avb              encrypted file contents
  2026-09-14 20-00/                   one folder per backup, named by its date
    Open with AeternaVault.avault     double-click target (plain text, no secrets)
    .aeternavault/encrypted.avs       encrypted index of this backup
```

`vault.json` is required: it holds the vault key, wrapped with the passphrase and
with the recovery key. Without it, the backups cannot be decrypted even with the
right passphrase.

Backups written by AeternaVault 0.3 and 0.4 kept these files in a visible
`AeternaVault Encrypted` folder. AeternaVault 0.5 moves them into the layout above
the first time it opens the destination (renames only, nothing is re-encrypted).

### `vault.json`

```json
{
  "format": 1,
  "vault_id": "9c1e0f6a2b7d4e31",
  "created_at": "2026-09-14T18:00:00Z",
  "cipher": "argon2id+xchacha20poly1305-stream-1mib",
  "key_check": "5a0c93d1e2f47b86",
  "slots": [
    {
      "kind": "passphrase",
      "kdf": { "memory_kib": 65536, "iterations": 3, "parallelism": 1 },
      "salt": "<16 bytes, hex>",
      "nonce": "<24 bytes, hex>",
      "wrapped_key": "<48 bytes: 32 ciphertext + 16 tag, hex>"
    },
    { "kind": "recovery-key", "kdf": { "memory_kib": 19456, "iterations": 2, "parallelism": 1 }, "…": "…" }
  ]
}
```

`cipher` is `argon2id+xchacha20poly1305-stream-1mib` or
`argon2id+aes256gcm-stream-1mib` and names the cipher used for new streams. Hex is
lower-case.

`vault_id` is 16 hex characters. AeternaVault uses it in the name of the
remembered key file and refuses a header whose id is empty, longer than 64
characters or contains anything but ASCII letters, digits, `-` and `_`.

A reader must refuse a header that asks for more than AeternaVault writes, before
deriving any key: more than 64 KiB, more than 3 slots, more than 1 GiB memory,
8 iterations or 4 lanes per slot, or more than 8 GiB × iterations for all slots
together (two slots of the strongest setting). Otherwise a manipulated header
could make every unlock attempt exhaust the computer.

## Encrypted streams (`.avb`, `.avs`)

Plaintext is split into chunks of exactly 1 048 576 bytes; the last chunk may be
shorter, and an empty plaintext is one empty chunk. Each chunk is sealed
separately (ciphertext followed by its 16-byte tag). A reader knows a chunk is the
last one when no further bytes follow it; the nonce includes that flag, so
truncation at a chunk boundary, reordering, duplication and appending are all
detected.

Both formats can be read with any key of the vault: the first four bytes name
the format.

### `AVB1` — XChaCha20-Poly1305

```text
offset 0   "AVB1"                           4 bytes
offset 4   prefix                           19 random bytes
offset 23  chunk₀ ‖ chunk₁ ‖ …              each: ciphertext ‖ 16-byte tag

key   = data key
nonce = prefix (19) ‖ chunk index as u32 big-endian (4) ‖ last (1: 0x01 or 0x00)   → 24 bytes
```

### `AVG1` — AES-256-GCM

```text
offset 0   "AVG1"                           4 bytes
offset 4   salt                             32 random bytes
offset 36  chunk₀ ‖ chunk₁ ‖ …              each: ciphertext ‖ 16-byte tag

key   = HMAC-SHA-256(key = data key, message = "AeternaVault v1 AES-GCM stream key" ‖ salt)
nonce = 7 zero bytes ‖ chunk index as u32 big-endian (4) ‖ last (1)               → 12 bytes
```

Every AES-GCM stream has its own key, so the short nonce never repeats under one key.

No associated data is used in either format.

## Blobs

- A file's content is one stream, stored at
  `blobs/<first two characters of id>/<id>.avb`.
- `id = hex(HMAC-SHA-256(key = name key, message = SHA-256(plaintext)))`.
- Identical content produces the same id, so it is stored once and reused by later
  backups. Without the name key, ids reveal nothing about the content.

## Snapshot index (`.avs`)

One stream per backup; its plaintext is UTF-8 JSON:

```json
{
  "header": {
    "format": 3,
    "app_version": "0.5.0",
    "id": "2026-09-14 20-00",
    "computer": "ANNA-PC",
    "user": "anna",
    "mode": "incremental",
    "status": "complete",
    "started_at": "2026-09-14T18:00:00Z",
    "finished_at": "2026-09-14T18:02:10Z",
    "base": "2026-09-13 20-00",
    "encrypted": true,
    "split": false,
    "sources": [
      { "key": "Documents", "name": "Documents", "path": "C:\\Users\\anna\\Documents", "portable": "{DOCUMENTS}" }
    ],
    "known_paths": { "values": [ ["APPDATA", "C:\\Users\\anna\\AppData\\Roaming"] ] },
    "stats": { "files": 1234, "bytes": 5678, "copied_files": 3, "copied_bytes": 10, "linked_files": 0,
               "referenced_files": 1231, "skipped": 0, "failed": 0 },
    "health_check": true,
    "plain_digest": "<hex, only in a partly encrypted backup>"
  },
  "index": {
    "format": 3,
    "files": [
      { "source": "Documents", "path": "Letters/2026/a.txt", "size": 13, "modified": 1757872800000000000,
        "sha256": "<hex of the plaintext>", "mode": 420, "blob": "<blob id>" },
      { "source": "Documents", "path": "current", "size": 0, "modified": 1757872800000000000,
        "sha256": "<hex of the target text>", "blob": "", "link": "Letters/2026" }
    ],
    "warnings": []
  }
}
```

- `source` is the folder inside the backup (`/`-separated, e.g. `Documents`);
  `path` is relative to it, `/`-separated.
- `modified` is nanoseconds since 1970-01-01 UTC.
- `mode` (optional, Linux) is the permission bits of the original; only the
  read, write and execute bits are restored.
- `link` (optional, Linux): the entry is a symbolic link and `link` is the
  target exactly as the link names it. A link is never followed and has no
  blob: `blob` is empty, `size` is 0 and `sha256` is the SHA-256 of the target
  text (UTF-8). A link whose target is not valid UTF-8 is not backed up.
  The format number is unchanged. Version 0.9.0 and older do not know `link`:
  they leave these entries out when restoring and report them as missing when
  checking.
- `health_check` (optional): every reused stored content was read and checked
  during this backup.
- A restore must reject paths containing `..`, drive letters or absolute paths.

The name of the backup folder (its date) is not encrypted.

## Partly encrypted backups

When only marked items are encrypted, one backup folder holds both parts:

```text
2026-09-14 20-00/
  Documents/…                         unmarked files, readable in any file manager
  .aeternavault/snapshot.json         plain header and
  .aeternavault/files.json            index of the unmarked files
  .aeternavault/encrypted.avs         index of the marked files (encrypted)
  Open with AeternaVault.avault
```

Both headers have `"split": true`. Each index lists only the files stored in its
part. AeternaVault lists, restores, checks, moves and deletes both parts together.

The readable part is not authenticated by itself: anyone who can write to the
storage could edit it or put one next to a fully encrypted backup. The encrypted
header therefore decides:

- `"split": false` in the encrypted header: a readable part next to it is refused.
- `plain_digest` (since 0.9.0) is SHA-256 over the JSON array
  `[id, [[key, path, portable], …], [[source, path, size, modified, sha256, mode], …]]`
  of the readable header and index. A mismatch is refused. `blob` is left out,
  because it changes when older plain backups are deleted. If the readable
  index has link entries, a fourth element `[[source, path, link], …]` (links
  only, in index order) is appended to the array.
- Without `plain_digest` (0.8.1 and older), every folder of the readable header
  must be listed identically in the encrypted header.

A refused readable part stops a restore or check before anything is written.

## Opening without restoring

- In AeternaVault: *Backups → Browse*, double-click
  `Open with AeternaVault.avault` (optional association on Windows), or
  `aeternavault open <folder>`. Browsing, opening and restoring always ask for the
  passphrase or the recovery key, also when the key is remembered on this
  computer. Access ends after 15 minutes or when the window is hidden. Opening a
  single file decrypts it into the temporary folder; those copies are removed at
  the next start.
- Command line on any computer:
  `aeternavault-cli restore latest --destination <folder> --to <folder> --yes`
  (asks for the passphrase, or reads `--passphrase-stdin` or `AETERNAVAULT_PASSPHRASE`).
- Without AeternaVault: `python tools/aeterna-decrypt.py <destination> extract latest <folder>`.

## Restoring

1. Unlock: try each key slot with the entered secret; compare the key check.
2. Decrypt the chosen `.avs` index.
3. For each file, decrypt its blob into a new file with an unpredictable name
   (`<target>.<random>.aeterna-partial`, created exclusively), compute SHA-256
   of the plaintext, compare with the index, then rename into place. Any
   authentication or checksum failure leaves the target untouched and is
   reported. Nothing is written through a symbolic link or junction below the
   target folder.
4. Linux: create the symbolic links of the index after all files, each under
   a new name that is renamed into place. An entry whose `sha256` does not
   match its target is not restored. A link replaces a file or link of the
   same name, never a folder.

## Threat model in short

| Situation | Protected? |
|---|---|
| Cloud provider or someone with the backup drive reads the files | Yes |
| Someone modifies or deletes files in the vault | Detected on restore and by *Check* (deleted data cannot be recovered) |
| Someone swaps whole `.avs` or blob files between backups of the same vault | Mostly detected by checksums; an old but authentic backup can be put back in place of a newer one |
| Someone adds or edits the readable part of a partly encrypted backup | Detected; restore and check are refused |
| Someone learns how large each stored file is | Not protected (see Goals) |
| Weak passphrase and a determined attacker | Argon2id slows guessing; a strong passphrase is still essential |
| Malware running as your user | No — it can read your files directly, and a remembered key |
| Opened files in `%TEMP%` | Decrypted copies exist until the next start of AeternaVault |
| Passphrase **and** recovery key lost | Data cannot be recovered by anyone |
