# Architecture

One Rust crate: a library and two programs.

| Program | Source | Purpose |
|---|---|---|
| `aeternavault` | `src/main.rs` | the window (feature `gui`, egui/eframe) |
| `aeternavault-cli` | `src/bin/aeternavault-cli.rs` → `src/cli.rs` | everything on the command line, `--json` for scripts |
| PowerShell module | `powershell/AeternaVault/` | wraps `aeternavault-cli --json` |

`cargo build --no-default-features --bin aeternavault-cli` builds the command line without any window libraries (used for the `aeternavault-cli` Linux packages).

## Modules

```text
src/
├── engine/          backups; knows nothing about the window
│   ├── sources.rs     configured folders → concrete folders (read-only)
│   ├── scan.rs        walks a folder, applies exclusions (read-only)
│   ├── plan.rs        what a backup or restore will do (read-only)
│   ├── backup.rs      executes a backup plan
│   ├── restore.rs     executes a restore plan
│   ├── fsops.rs       all writing file operations of the engine
│   ├── snapshots.rs   finds backups at a destination (read-only)
│   ├── manifest.rs    on-disk format of a backup
│   ├── vault.rs       encrypted vault: keys, blobs, remembered key
│   ├── crypto.rs      Argon2id, XChaCha20-Poly1305, AES-256-GCM (docs/ENCRYPTION.md)
│   ├── layout.rs      moves 0.3/0.4 encrypted backups into the current layout
│   ├── manage.rs      delete, move
│   ├── verify.rs      check a backup against its checksums (read-only)
│   ├── retention.rs   which backups the rules remove (read-only)
│   ├── selection.rs   included/excluded and encrypted sub-folders
│   └── passphrase.rs  passphrase rating (zxcvbn + public guidance)
├── automatic/       backup jobs: timing, running a job, the background service
├── platform/        operating system: paths, tray, autostart, systemd, Explorer menu,
│                    file association, application catalog (apps.toml)
├── gui/             window: views/, dialogs, theme, input (shortcuts, autoscroll)
├── cli.rs           command line
├── config.rs        config.toml (hand-editable)
├── history.rs       activity history (history.jsonl), shared by window, jobs and CLI
├── i18n.rs          English and German texts
└── paths.rs         where settings and logs are kept
```

## Plan, then execute

Every backup and restore is first planned by read-only code (`sources`, `scan`, `plan`), then executed by `backup`/`restore` through `fsops`. A test (`planning_modules_are_read_only` in `engine/mod.rs`) scans the read-only modules for writing APIs, so planning can never change anything.

## Backup layout

```text
<destination>/
  2026-09-24 20-00/                 one folder per backup
    Documents/…                     plain copies (unchanged files are hard links)
    .aeternavault/snapshot.json     header: date, sources, status, statistics
    .aeternavault/files.json        index: path, size, time, SHA-256 per file
  .aeternavault/                    only with encryption (see ENCRYPTION.md)
```

Each file is written as `<name>.aeterna-partial` and renamed once it is complete. The header records whether a backup completed, was cancelled or failed; only completed backups serve as the base of the next incremental one.

## Integrity

- Every file is written as `<name>.aeterna-partial`, flushed to the disk (`advanced.flush_to_disk`), checked against its SHA-256 where known, and only then renamed.
- An incremental backup reuses an earlier copy only if its size and modification time match the index (plain), or its size matches the exact encrypted size (encrypted). Anything else is copied again.
- Backed-up files are read-only, because hard-linked files share their data between backups.
- `verify` reads every file again and compares it with its checksum; jobs can do this after each backup.

## Security rule for encrypted backups

The key remembered on a computer (DPAPI on Windows, owner-only file on Linux) is used to write, list, check, move and delete. Reading names or contents (browse, open, extract, restore) always requires the passphrase or recovery key in the current session: in the window for 15 minutes or until it is hidden, on the command line per call.

## Platforms

| | Windows | Linux |
|---|---|---|
| Settings | `%APPDATA%\AeternaVault` | `~/.config/aeternavault` |
| Logs | `%LOCALAPPDATA%\AeternaVault\logs` | `~/.local/share/aeternavault/logs` |
| Background jobs | autostart + notification area icon | systemd user timer (`aeternavault-cli job run-due`) |
| Files in use | read as they are (Volume Shadow Copy is planned, `platform/vss.rs`) | read as they are |
| Default destination | fixed drive with the most free space other than the system drive, else `%USERPROFILE%\AeternaVault` | largest mounted data partition other than `/`, else `~/AeternaVault` |
| Install | Inno Setup (`installer/windows`) | `.deb` via cargo-deb, tarball with `install.sh` |

Environment variables for tests and demos: `AETERNAVAULT_HOME` (all own files in one folder), `AETERNAVAULT_PROFILE_ROOT` (fake user profile; also blocks every system change), `AETERNAVAULT_LOG=debug`.

## Main dependencies

| Purpose | Crate |
|---|---|
| Window | `eframe`/`egui` (wgpu, glow fallback), `rfd` |
| Command line | `clap` |
| Encryption | RustCrypto: `argon2`, `chacha20poly1305`, `aes-gcm`, `hmac`, `sha2` |
| Passphrase rating | `zxcvbn` |
| Settings | `serde`, `toml`, `serde_json` |
| Logging | `tracing` |
| Windows | `windows-sys`, `winreg` |
