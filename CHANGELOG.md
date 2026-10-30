# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [0.8.0] - 2026-10-31

### Added

- Notifications have their own tab.
- Activity: export as TXT or CSV, and "Send via notifications" (e-mail with the complete list as CSV attachment, chat services with the newest entries). `aeternavault-cli history --format csv|text|json --output FILE`, PowerShell `Export-AVHistory`.
- Backup jobs can run a command before the backup (for example `pg_dump` into a folder that the job backs up). If it fails, the backup does not run and a problem is reported. `--before`, PowerShell `-Before`.
- Backup jobs can use their own destination. `--destination`, PowerShell `-Destination`.
- Backups carry the name of their job and can be renamed (Backups → Rename…, `aeternavault-cli rename`, `Rename-AVBackup`).
- Administration guide: silent installation, database servers, monitoring (`docs/administration.md`).

### Changed

- The backup list shows how many files of an incremental backup are new or changed, so incremental backups no longer look like copies of each other.
- Explanations moved behind (i) symbols (pointing or clicking shows them); more room in cards and between controls. The tab row wraps in narrow windows.

## [0.7.0] - 2026-10-28

### Added

- Notifications after backups: a summary on success, alerts when a backup failed, was incomplete or interrupted, and when the destination runs low on space. Channels: e-mail (SMTP), Microsoft Teams, Telegram, ntfy, Slack, Discord, Gotify and webhook. Thresholds (percent or GB) and all texts with placeholders are configurable. Settings → Notifications, `aeternavault-cli notify`, see `docs/notifications.md`.
- A "Send test" button per channel and `aeternavault-cli notify test`.

## [0.6.0] - 2026-10-27

### Added

- Updates from the settings: the current version, "Check for updates" and "Download and install". A small dot on the Settings tab shows when a new version is out; there are no pop-ups. Installing waits until a running backup or restore has finished.
- Automatic check at most once a day, switchable in the settings. This is the only network connection.
- `aeternavault-cli update` installs the latest release; `--check` only reports it.
- Releases carry a minisign signature of `SHA256SUMS.txt`. An update is installed only if the signature and the checksum match.
- A note when the destination is on the same drive as folders that are backed up (window and `aeternavault-cli status`).

### Fixed

- Checking an encrypted backup with the remembered key showed the names of damaged or missing files, in the window, the command line and the log. Names now appear only after entering the passphrase; otherwise files are counted.
- In partly encrypted backups, warnings about encrypted files were also stored in the readable index.
- An interrupted automatic backup counted as completed and was not made up. It is now retried like a backup whose drive was not connected.
- A backup lock left by a crashed process blocked the destination for up to 12 hours, and a backup running longer than 12 hours could be joined by a second one. Locks of this computer now end with their process.
- Entries of a damaged backup index were skipped silently during a restore; they are now reported as failed.
- The PowerShell module sent passphrases in the console code page; passphrases with umlauts did not match when typed later.
- `--passphrase-stdin` is now used even when a key is remembered.

## [0.5.0] - 2026-09-24

### Added

- Linux support: window and command line, `.deb` packages and tarballs; background jobs through a systemd user timer.
- `aeternavault-cli`: a separate command-line program that covers everything the window does (backups, restore, folders, applications, jobs, retention, destination, encryption, settings, history), with `--json` and exit codes for scripts.
- PowerShell module `AeternaVault` (Windows PowerShell 5.1 and PowerShell 7).
- Windows installer: Program Files, `aeternavault-cli` on `PATH`, updates keep settings, uninstall removes autostart and all registry entries.
- True black appearance for OLED screens.
- Tooltips for every setting; a list of keyboard and mouse shortcuts in the settings.
- Keyboard: Ctrl+1…7 and Ctrl+Tab switch tabs, Alt+←/→ and the mouse side buttons go back and forward, Ctrl+B backs up, Esc goes back. Middle-click scrolling.
- The backup list shows when each backup will be removed by the retention rules.
- Passphrase rating that recognises repetitions, dictionary words and keyboard patterns (zxcvbn).
- Default destination: the drive with the most free space other than the system drive.

### Changed

- All backups use one layout: a folder per date. Encrypted backups keep their index in that folder; the shared keys and contents are in a hidden `.aeternavault` folder. Encrypted backups of earlier versions are moved into this layout automatically.
- Applications: only well-known data folders (browsers, mail, notes, keys, games, OBS) are offered, and only when they exist. Choosing one adds its folders to the folders to back up, where their contents can be adjusted. Sign-ins and saved passwords are left out.
- Larger interface and dialogs; the tabs can be used from every screen.
- The window program is `aeternavault.exe`; command-line use moved to `aeternavault-cli`.

### Removed

- Preview (dry run) and its export.
- Registry settings, lists of installed programs and the winget export.
- `README.txt` and the application list files inside backups.

### Fixed

- Encrypted backups could be browsed and opened without entering the passphrase when the key was remembered. Reading encrypted names or contents now always requires the passphrase or recovery key.
- The passphrase criteria were shown only when pointing at the strength bar; the (?) button now shows them too and keeps them open when clicked.

[0.6.0]: https://github.com/Benett53/AeternaVault/releases/tag/v0.6.0
[0.5.0]: https://github.com/Benett53/AeternaVault/releases/tag/v0.5.0
