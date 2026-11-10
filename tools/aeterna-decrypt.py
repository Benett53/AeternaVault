#!/usr/bin/env python3
"""Independent decryptor for AeternaVault encrypted backups.

This script does not use any AeternaVault code. It implements the format
described in docs/ENCRYPTION.md, so encrypted backups stay readable even
without AeternaVault — and the documentation can be checked against it.

Requirements (Python 3.9+):

    pip install cryptography argon2-cffi

Usage:

    python aeterna-decrypt.py "E:\\AeternaVault" list
    python aeterna-decrypt.py "E:\\AeternaVault" extract latest "D:\\Restored"
    python aeterna-decrypt.py "E:\\AeternaVault" extract "2026-09-14 20-00" "D:\\Restored"

The first argument is the backup destination (the folder that contains the
hidden ".aeternavault" folder) or any backup folder inside it. The passphrase
or recovery key is read from the environment variable AETERNAVAULT_PASSPHRASE
or asked for. The plain part of a partly encrypted backup is a normal folder
and needs no tool.
"""

import getpass
import hashlib
import hmac
import json
import os
import struct
import sys
from pathlib import Path

try:
    from argon2.low_level import Type, hash_secret_raw
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM, ChaCha20Poly1305
except ImportError:  # pragma: no cover - explained to the user
    sys.exit("Please install the requirements first:  pip install cryptography argon2-cffi")

VAULT_DIR = ".aeternavault"
ENCRYPTED_INDEX = "encrypted.avs"
# AeternaVault 0.3 and 0.4 (moved into the layout above by 0.5).
OLD_VAULT_DIR = "AeternaVault Encrypted"
CHUNK = 1024 * 1024
TAG = 16
XCHACHA = "argon2id+xchacha20poly1305-stream-1mib"
AESGCM_NAME = "argon2id+aes256gcm-stream-1mib"


# --- XChaCha20-Poly1305 = HChaCha20 subkey + ChaCha20-Poly1305 -----------------

def _rotl(v, n):
    return ((v << n) & 0xFFFFFFFF) | (v >> (32 - n))


def _quarter(s, a, b, c, d):
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = _rotl(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotl(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b]) & 0xFFFFFFFF
    s[d] = _rotl(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & 0xFFFFFFFF
    s[b] = _rotl(s[b] ^ s[c], 7)


def hchacha20(key: bytes, nonce16: bytes) -> bytes:
    """HChaCha20 (draft-irtf-cfrg-xchacha): 32-byte key, 16-byte nonce -> subkey."""
    state = list(struct.unpack("<4I", b"expand 32-byte k"))
    state += list(struct.unpack("<8I", key))
    state += list(struct.unpack("<4I", nonce16))
    for _ in range(10):
        _quarter(state, 0, 4, 8, 12)
        _quarter(state, 1, 5, 9, 13)
        _quarter(state, 2, 6, 10, 14)
        _quarter(state, 3, 7, 11, 15)
        _quarter(state, 0, 5, 10, 15)
        _quarter(state, 1, 6, 11, 12)
        _quarter(state, 2, 7, 8, 13)
        _quarter(state, 3, 4, 9, 14)
    return struct.pack("<4I", *state[0:4]) + struct.pack("<4I", *state[12:16])


def xchacha_decrypt(key: bytes, nonce24: bytes, data: bytes) -> bytes:
    subkey = hchacha20(key, nonce24[:16])
    return ChaCha20Poly1305(subkey).decrypt(b"\x00" * 4 + nonce24[16:], data, None)


# --- Keys ------------------------------------------------------------------------

def hmac_sha256(key: bytes, data: bytes) -> bytes:
    return hmac.new(key, data, hashlib.sha256).digest()


def normalize(kind: str, secret: str) -> bytes:
    if kind == "recovery-key":
        secret = "".join(c.upper() for c in secret if c.isascii() and c.isalnum())
    return secret.encode("utf-8")


# Key settings AeternaVault writes: two slots, at most 1 GiB and 4 passes. A
# header asking for more is damaged or made to exhaust the computer (the same
# limits as in the app).
MAX_HEADER_BYTES = 64 * 1024
MAX_SLOTS = 3
MAX_MEMORY_KIB = 1024 * 1024
MAX_ITERATIONS = 8
MAX_PARALLELISM = 4
MAX_TOTAL_COST = 2 * 1024 * 1024 * 4


def check_kdf(vault: dict) -> None:
    slots = vault.get("slots") or []
    ok = 0 < len(slots) <= MAX_SLOTS and all(
        isinstance(s["kdf"]["memory_kib"], int)
        and isinstance(s["kdf"]["iterations"], int)
        and isinstance(s["kdf"]["parallelism"], int)
        and 8 <= s["kdf"]["memory_kib"] <= MAX_MEMORY_KIB
        and 1 <= s["kdf"]["iterations"] <= MAX_ITERATIONS
        and 1 <= s["kdf"]["parallelism"] <= MAX_PARALLELISM
        for s in slots
    )
    if not ok or sum(s["kdf"]["memory_kib"] * s["kdf"]["iterations"] for s in slots) > MAX_TOTAL_COST:
        sys.exit("vault.json asks for implausible key settings; not opened.")


def unlock(vault: dict, secret: str) -> bytes:
    """Returns the 32-byte vault key, trying every key slot."""
    check_kdf(vault)
    for slot in vault["slots"]:
        kdf = slot["kdf"]
        kek = hash_secret_raw(
            normalize(slot["kind"], secret),
            bytes.fromhex(slot["salt"]),
            time_cost=kdf["iterations"],
            memory_cost=kdf["memory_kib"],
            parallelism=kdf["parallelism"],
            hash_len=32,
            type=Type.ID,
            version=19,
        )
        try:
            master = xchacha_decrypt(kek, bytes.fromhex(slot["nonce"]), bytes.fromhex(slot["wrapped_key"]))
        except Exception:
            continue
        if hmac_sha256(master, b"AeternaVault v1 key check")[:8].hex() == vault["key_check"]:
            return master
    sys.exit("The passphrase or recovery key is not correct.")


# --- Encrypted streams (.avs, .avb) ---------------------------------------------

def decrypt_stream(data_key: bytes, reader, writer) -> str:
    """Decrypts one stream; returns the SHA-256 of the plaintext (hex)."""
    magic = reader.read(4)
    if magic == b"AVB1":
        prefix = reader.read(19)

        def open_chunk(counter, last, sealed):
            nonce = prefix + struct.pack(">I", counter) + bytes([last])
            return xchacha_decrypt(data_key, nonce, sealed)
    elif magic == b"AVG1":
        salt = reader.read(32)
        stream_key = hmac_sha256(data_key, b"AeternaVault v1 AES-GCM stream key" + salt)
        aead = AESGCM(stream_key)

        def open_chunk(counter, last, sealed):
            nonce = b"\x00" * 7 + struct.pack(">I", counter) + bytes([last])
            return aead.decrypt(nonce, sealed, None)
    else:
        raise ValueError("not an AeternaVault encrypted stream")

    digest = hashlib.sha256()
    current = reader.read(CHUNK + TAG)
    counter = 0
    while True:
        following = reader.read(CHUNK + TAG) if len(current) == CHUNK + TAG else b""
        last = 1 if not following else 0
        plain = open_chunk(counter, last, current)
        digest.update(plain)
        writer.write(plain)
        if last:
            return digest.hexdigest()
        current = following
        counter += 1


class _Buffer:
    def __init__(self):
        self.parts = []

    def write(self, data):
        self.parts.append(data)

    def value(self):
        return b"".join(self.parts)


def read_snapshot(data_key: bytes, path: Path) -> dict:
    out = _Buffer()
    with open(path, "rb") as reader:
        decrypt_stream(data_key, reader, out)
    return json.loads(out.value())


def no_links_below(root: Path, path: Path) -> None:
    """Nothing is written through a link placed inside the target folder."""
    current = path
    while current != root and root in current.parents:
        if current.is_symlink() or getattr(os.path, "isjunction", lambda _: False)(current):
            raise ValueError(f"{current} is a link; nothing is written through links")
        current = current.parent


def create_private(path: Path):
    """A new file under an unpredictable name, never following a prepared link."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    return os.fdopen(os.open(path, flags, 0o600), "wb")


def safe_join(root: Path, *parts: str) -> Path:
    target = root
    for part in parts:
        for piece in part.replace("\\", "/").split("/"):
            if piece in ("", "."):
                continue
            if piece == ".." or ":" in piece:
                raise ValueError(f"unsafe path in the index: {part}")
            target = target / piece
    return target


# --- Layout ----------------------------------------------------------------------

def find_vault(start: Path):
    """The destination and its vault folder, from the destination or a folder inside it."""
    for folder in [start, *start.resolve().parents]:
        for name in (VAULT_DIR, OLD_VAULT_DIR):
            if (folder / name / "vault.json").is_file():
                return folder, folder / name
    sys.exit(f"No encrypted backups found at {start}.")


def encrypted_indexes(destination: Path, vault_dir: Path):
    """(backup name, path of its encrypted index) for every encrypted backup."""
    if vault_dir.name == OLD_VAULT_DIR:
        for path in sorted((vault_dir / "snapshots").glob("*.avs")):
            yield path.stem, path
        return
    for folder in sorted(p for p in destination.iterdir() if p.is_dir()):
        index = folder / VAULT_DIR / ENCRYPTED_INDEX
        if index.is_file():
            yield folder.name, index


# --- Commands --------------------------------------------------------------------

def main() -> None:
    if len(sys.argv) < 3 or sys.argv[2] not in ("list", "extract"):
        sys.exit(__doc__)
    destination, vault_dir = find_vault(Path(sys.argv[1]))
    with open(vault_dir / "vault.json", "rb") as file:
        raw = file.read(MAX_HEADER_BYTES + 1)
    if len(raw) > MAX_HEADER_BYTES:
        sys.exit("vault.json is too large.")
    vault = json.loads(raw.decode("utf-8"))
    if vault["cipher"] not in (XCHACHA, AESGCM_NAME):
        sys.exit(f"Unknown cipher {vault['cipher']}; this script may be too old.")
    secret = os.environ.get("AETERNAVAULT_PASSPHRASE") or getpass.getpass("Passphrase or recovery key: ")
    master = unlock(vault, secret)
    data_key = hmac_sha256(master, b"AeternaVault v1 data key")

    snapshots = []
    for name, path in encrypted_indexes(destination, vault_dir):
        snapshots.append((name, read_snapshot(data_key, path)))
    snapshots.sort(key=lambda s: s[1]["header"]["started_at"], reverse=True)

    if sys.argv[2] == "list":
        for name, snapshot in snapshots:
            header = snapshot["header"]
            print(f"{name:<32} {header['computer']:<16} {header['status']:<24} "
                  f"{header['stats']['files']:>8} files")
        return

    if len(sys.argv) != 5:
        sys.exit(__doc__)
    wanted, target = sys.argv[3], Path(sys.argv[4])
    chosen = next((s for s in snapshots if wanted in ("latest", s[0])), None)
    if chosen is None:
        sys.exit(f"No encrypted backup named {wanted}.")
    name, snapshot = chosen
    failed = 0
    links = []
    entries = snapshot["index"]["files"]
    # Symbolic links have no content; they are made after all files, so that
    # no file is written through a link from the index.
    for entry in (e for e in entries if e.get("link")):
        output = safe_join(target, entry["source"], entry["path"])
        partial = output.with_name(f"{output.name}.{os.urandom(6).hex()}.partial")
        try:
            if os.name == "nt":
                raise ValueError("symbolic links are restored only on Linux")
            if hashlib.sha256(entry["link"].encode("utf-8")).hexdigest() != entry["sha256"].lower():
                raise ValueError("checksum mismatch")
            links.append((entry, output, partial))
        except Exception as error:
            failed += 1
            print(f"could not restore {entry['source']}/{entry['path']}: {error}", file=sys.stderr)
    for entry in (e for e in entries if not e.get("link")):
        blob = entry["blob"]
        source = vault_dir / "blobs" / blob[:2] / f"{blob}.avb"
        output = safe_join(target, entry["source"], entry["path"])
        partial = output.with_name(f"{output.name}.{os.urandom(6).hex()}.partial")
        created = False
        try:
            no_links_below(target, output)
            output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with open(source, "rb") as reader, create_private(partial) as writer:
                created = True
                digest = decrypt_stream(data_key, reader, writer)
            if digest != entry["sha256"].lower():
                raise ValueError("checksum mismatch")
            no_links_below(target, output)
            partial.replace(output)
            if os.name != "nt" and isinstance(entry.get("mode"), int):
                os.chmod(output, entry["mode"] & 0o777)
            seconds = entry["modified"] / 1_000_000_000
            os.utime(output, (seconds, seconds))
        except Exception as error:  # report and continue with the next file
            failed += 1
            if created:
                partial.unlink(missing_ok=True)
            print(f"could not restore {entry['source']}/{entry['path']}: {error}", file=sys.stderr)
    for entry, output, partial in links:
        try:
            no_links_below(target, output.parent)
            output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            if output.is_dir() and not output.is_symlink():
                raise ValueError("a folder with this name exists")
            os.symlink(entry["link"], partial)
            try:
                os.replace(partial, output)
            except OSError:
                partial.unlink(missing_ok=True)
                raise
        except Exception as error:
            failed += 1
            print(f"could not restore {entry['source']}/{entry['path']}: {error}", file=sys.stderr)
    total = len(entries)
    print(f"{name}: {total - failed} of {total} files restored to {target}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
