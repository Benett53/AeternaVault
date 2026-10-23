# Security policy

## Reporting a vulnerability

Please do not open a public issue. Use *Security → Report a vulnerability* in this repository. Include the AeternaVault version, the operating system, the steps and what you expected.

Reviews of the encryption format ([docs/ENCRYPTION.md](docs/ENCRYPTION.md)) are welcome, also as regular issues.

## In scope

- restore, extract or move writing outside the chosen folder
- planning or checking a backup changing anything on disk
- deleting or moving a backup making another one incomplete
- a backup silently missing files or storing damaged content
- unsafe handling of paths from `files.json`, `encrypted.avs`, `apps.toml` or `config.toml`
- weaknesses in the encryption format or its implementation, including keys or passphrases in logs or files
- reading encrypted file names or contents without the passphrase or recovery key

## Verifying a download

- Compare the SHA-256 checksum with `SHA256SUMS.txt` of the release.
- Check the build provenance: `gh attestation verify <file> --repo Benett53/AeternaVault`.
- `sbom.cdx.json` lists every dependency (CycloneDX).

## Limitations

- No external security audit. The encryption combines standard RustCrypto primitives (Argon2id, XChaCha20-Poly1305, AES-256-GCM, HMAC-SHA-256), is documented byte by byte, and an independent Python implementation reads it.
- Plain backups are as readable as the original files. Use encryption for cloud folders and sensitive data.
- A key remembered for automatic backups can be used by software running as your user (Windows: DPAPI; Linux: owner-only file).
- Files opened from an encrypted backup are decrypted into the temporary folder and removed at the next start.
- Releases are not code-signed; Windows SmartScreen may warn.
