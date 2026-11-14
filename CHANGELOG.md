# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Settings in sections with a list beside the page; notifications and the web interface are sections there instead of top tabs.
- Web interface page in parts: connection, sign-in, access, LDAP / AD, users, roles, audit log.
- The web certificate is shown like in server software: issued to and by, names, validity, SHA-256, and buttons to export it (to trust it on other computers) or make a new one.
- Countries for geoblocking are chosen from a list; the page shows the database type and date, looks up an address with the current rule, and counts refusals per country.
- Settings → Drive health: what the drive reports, its source and a button to check now; drive health in the web overview and in every backup report (`{drive}`).
- Notifications when a new version is available (`on_update`, once per version).

### Changed

- The web interface listens on the network over HTTPS by default; the choice between this computer and the network is gone (`web set --bind 127.0.0.1` still limits it). Addresses are blocked after 5 wrong sign-ins instead of 20, and administration is allowed from this computer only by default. Settings from 0.14 are moved once.
- Explanations are behind info icons instead of between settings; text fields have room around the text; sections slide open smoothly instead of jumping.
- LDAP: “Enable LDAP authentication”.

### Fixed

- Opening the Web page counted as an unapplied change and moved the page.
- The web overview showed “reading…” for the drive health until the next reload.
- Web audit log: space between the search field and the Filter button.

## [0.14.0] - 2026-11-14

Contains the changes of 0.13.0, which was withdrawn.

### Added

- Web interface: own roles as named sets of permissions (view activity, run jobs, switch jobs, check backups, view sign-ins, block addresses, view the audit log, manage users). Viewer, operator and administrator stay as built-in roles; existing users keep theirs. `aeternavault-cli web role list|set|remove`.
- LDAP: roles from directory groups (any group to any role, the highest wins) or assigned in AeternaVault, with an optional default role for new directory users.
- Help (the ? at the top right): keyboard shortcuts, version and links to the documentation and the issue tracker.
- Drive health (Settings → Features): the S.M.A.R.T. status of the destination drive in Settings and in the web overview, read from UDisks (Linux, no root needed; `smartctl` as fallback) or the Storage module (Windows), and a message when it gets worse. `aeternavault-cli drive-health [PATH]`.
- Settings → Features: backup jobs and drive health can be switched like the other parts; all five sit in one row of switches.
- Web interface access: blocked addresses and networks (Access page, a Block button next to failed sign-ins, `aeternavault-cli web block|unblock|blocked`), applied at once without a restart; automatic blocking of an address after 20 wrong sign-ins within 24 hours (`auto_block_after`, never this computer); geoblocking by country with a downloaded country database (MaxMind GeoLite2 Country or DB-IP Lite, `.mmdb`; private networks are not checked).
- Web interface: administrators can be limited to this computer or to given networks (`admin_access`, `admin_networks`); elsewhere they keep the other permissions of their role.
- 2FA policy: optional, required for administrators, or required for everyone (`mfa`), and which methods are allowed (`mfa_app`, `mfa_email`). `require_two_factor = true` from older files becomes `mfa = "everyone"`.
- Own certificate files are read again when they change, so a renewal (for example Let's Encrypt with certbot or win-acme) needs no restart.
- Notifications → Message texts: one message at a time, starting from the built-in text, values to click in at the cursor, a preview with sample values and a button to restore the built-in text.

### Changed

- The Web tab is rebuilt: connection, sign-in with the 2FA policy, access (networks, automatic blocking, countries, administrators, blocked addresses) and LDAP in balanced columns; roles with their permissions as cards; LDAP roles from directory groups (any group to any role) or given in AeternaVault. The self-signed certificate is explained where it is chosen. Failed sign-ins in Activity → Sign-ins (web) can be blocked with one click.
- Switches instead of check boxes for settings that are on or off (green when on, grey when off, animated); chosen options are marked by their colour only, without a check mark. The same in the web interface.
- Settings are placed in two balanced columns; cards no longer leave gaps or overlap when options wrap.
- Shorter labels; number fields show “off” or “never” for 0.
- Pages before sign-in name nothing about the computer, and the sign-in box is centred; the style sheet address carries the version, so a browser never mixes old and new.

## [0.13.0] - 2026-11-13 (withdrawn)

### Added

- Settings → Features: notifications, applications and the web interface are switched on there; a part that is off disappears with its tab and keeps its settings. A new installation starts with all three off; existing settings keep what they use (notifications with channels, the applications tab, a web interface that was on). Adding a notification channel on the command line switches notifications on.
- Settings → Logging: how much goes to the technical log (problems only, normal, detailed; from the next start) and whether changes of the settings go to the audit log.
- Web interface: the overview shows tiles (jobs, last backup, share of complete backups, failures, free space), a chart of the backups of the last 30 days and a ring of the results, and jobs and destinations as cards.
- Web interface: sign-ins, sessions and the audit log name the device with its network address, name (reverse DNS) and MAC address; a browser on the computer itself appears with the computer's address instead of 127.0.0.1.

### Changed

- The web interface looks like the window: the same colours (dark, or light when the browser prefers it), the logo, serif headings, gold buttons, sub-tabs as buttons, and layouts that adapt from phone to wide screen.
- The window uses wide screens: content up to 1640 px wide, settings, notifications and the Web tab in two columns side by side when there is room, one below the other otherwise. Choices are buttons (the chosen one gold with a check mark) instead of radio circles. The destination shows how full the drive is. The Web tab shows tiles for users and sign-ins, its LDAP settings with the label above each field, and the audit log as a full table instead of a box of one line.

## [0.12.1] - 2026-11-13

### Changed

- Number fields in the window (retention, health check, notifications, Web tab) have arrows beside them: a click steps by one, held down it keeps counting, a click into the field lets you type the number. Dragging no longer changes a value, so a slip of the mouse cannot change a setting.

## [0.12.0] - 2026-11-13

### Added

- Web interface: sign-in with LDAP or Active Directory. The role comes from directory groups (nested groups in Active Directory), read again at every sign-in; LDAPS or StartTLS, certificates trusted by the system plus an optional own CA; search with a service account, anonymously, or as the user. `aeternavault-cli web ldap on|off|set|test`, and a test button in the Web tab.
- Web interface: codes by e-mail as second step, sent through an e-mail notification channel. Users switch them on under Account; directory users use the address of their account; administrators can enter addresses. With a second step required, either an authenticator app or e-mail counts.
- Sign-ins, live: who is signed in now, the last 24 hours, and every attempt with its reason, in the web interface (Activity → Sign-ins, administrators) and in the window (Activity → Sign-ins (web)).

### Changed

- The Web tab is laid out again: settings in sections with labels, edited as a draft and used with Apply (before, every change restarted the web interface), allowed networks as a list with a check per entry, file choosers for certificates, a user table with an actions menu, and a filterable audit log.
- The password length is a recommendation (`recommended_password_length`, before `min_password_length`): shorter or easily guessed passwords are accepted with a note. Only an empty password is refused.

### Fixed

- Web tab: the message for a refused new user (for example a password that was too short) disappeared at once and could not be read.

## [0.11.0] - 2026-11-13

### Added

- Web interface, off by default ([docs/web.md](docs/web.md)): an overview of jobs, last runs with their problems, destinations, backups and the activity in a browser, and the everyday actions (run, cancel, switch jobs on and off, check backups). Only this computer can connect by default; from the network only over HTTPS, with a certificate made by AeternaVault or own certificate files, and optionally only from chosen networks. Users with the roles viewer, operator and administrator, optionally limited to some jobs; Argon2id passwords, optional or required one-time codes from authenticator apps, lockout after wrong sign-ins, sessions that end after inactivity. Runs while AeternaVault is open, or with `aeternavault-cli web serve`. New Web tab in the window and `aeternavault-cli web` commands. Restoring, deleting and changing settings stay in the window and the command line.
- Audit log (`audit.jsonl`): sign-ins, user management, actions over the web, and every saved change of the settings from the window, the command line or the web, with the changed fields (secrets only as "changed"). Entries are hash-chained; `aeternavault-cli web audit --verify` finds lines changed afterwards.

### Changed

- The window reads the settings again when they were changed outside it (over the web, by the command line or by hand) and it has no unsaved changes of its own.

## [0.10.1] - 2026-11-13

### Changed

- Notifications and the activity log now say what went wrong and what to do: every file or folder that was not saved and every file the check after the backup found damaged, missing or unreadable, each with its reason, followed by advice. Before, a notification named only the first problem or a count ("check found 6 damaged and 0 missing files"). Errors that stop a run are explained in the language of the program. With encryption on, file names from the backup stay on the computer; their number, the reason and the advice are sent. In the window, the details appear when hovering over the last run of a job.
- The check of a backup tells stored copies that could not be read (often locked for a moment by a virus scanner, an indexing service or a sync program at the destination) apart from damaged ones, and names the error reported by the system. Before, both counted as damaged.

### Fixed

- A folder to back up that was not there or not readable (network share not connected, drive unplugged, no access) was counted as skipped, and the backup was reported as completed. Such a folder, and every sub-folder or file that could not be read while scanning, now counts as failed: the backup completes with notes, `aeternavault-cli backup` ends with exit code 2, and the folder is not named in the new backup. Retention therefore keeps the newest backup that contains it. If no folder can be read at all, no backup is made and the run fails.
- Restore left out index entries that name a folder the backup does not have, without a message and with exit code 0. They now count as failed and are named.
- Update `rustix` to 1.1.6 (RUSTSEC-2026-0486; the affected functions are not used by AeternaVault).

### Security

Findings of an external review (12.11.2026):

- Browsing, opening and extracting files read a backup's parts without checking that they belong together, as restore and check do. A readable part placed next to an encrypted backup by someone with write access to the backup storage was listed next to the authenticated files and could be extracted or opened. All of these now read backups through the same check, and extracting copies only files that the backup, read again, contains.
- A partly encrypted backup whose encrypted part was missing (deleted, damaged, not fully synchronised) was restored as if it were complete: only its readable files came back, without any message. It is now refused with "the encrypted part of this partly encrypted backup is missing", and the same applies to a missing readable part.
- Linux: saving a recovery key over an existing file, or remembering the key, kept the permissions of the existing file; a key file written by version 0.9.0 (0644) stayed readable for other users. Such files are now always written as a new file for the owner only (0600) that replaces the old one; a link at that name is replaced, not followed.

## [0.10.0] - 2026-11-12

### Added

- Linux: symbolic links in backed-up folders are backed up as links and restored as links. The target is stored as the link names it (relative, absolute, also if it does not exist); links are never followed. Before, links were left out. A link is only an entry in the backup index, in encrypted backups in the encrypted index. Version 0.9.0 and older skip these entries when restoring such a backup and report them as missing when checking it. On Windows, links and junctions are still skipped.

### Fixed

- Linux: a named pipe in a backed-up folder (for example `~/.steam/steam.pipe`) made the backup wait forever, because opening a pipe waits for a writer. Pipes, sockets and device files are now listed as skipped and never opened.
- Retention: with several jobs backing up different folders into the same destination, the backups of the job that runs more often pushed out every backup of the other one, leaving a folder without any backup. The newest backup of every folder is now always kept.
- Backups made within the same minute were listed in the wrong order when the destination is shared by several computers and the computer name contains a digit; `latest` then meant the first backup of that minute.
- Notifications for a job with its own destination named the general destination and its free space, and the low-space warning watched the wrong drive. They now refer to the job's destination.
- `aeternavault-cli backup --job` for a job with its own encrypted destination asked for the passphrase of the general destination and then failed. It now unlocks the job's destination.
- Restore: files whose content is missing in the backup, or whose index entry is invalid, were counted as "left as they were", and `aeternavault-cli restore` ended with exit code 0. They now count as failed, are named in the report, and the exit code is 2.
- Linux: with MangoHud or vkBasalt enabled for the whole session, the overlay was drawn inside the AeternaVault window and kept it redrawing, about 4 % CPU while idle. Both are now switched off for the window.

### Security

- `vault.json`: the vault id is used in the name of the key file remembered on this computer. An id that is not made of letters, digits, `-` and `_` is now refused; on Windows an edited `vault.json` on the backup storage could otherwise name a file outside the key folder.
- Changing the passphrase reused the key derivation settings found in `vault.json`, which are not authenticated. Settings lowered by someone with write access to the backup storage would have made the new passphrase much cheaper to guess. A new passphrase is now derived with at least the default (64 MiB, 3 passes).
- Linux: the error output of a job's command before the backup was written to `/tmp/aeternavault-before-<process id>.log`. The file is now created under an unpredictable name, exclusively and readable by its owner only, so a file or link prepared under the old name is never written through.
- Linux: a recovery key saved to a file (`encryption setup --recovery-file`, or "Save as text file…" in the window) was readable by other users of the computer (mode 0644). The file is now created for its owner only (0600).

## [0.9.0] - 2026-11-02

### Added

- Health check: once every 30 days (Settings → Advanced, `advanced.health_check_days`), an incremental backup reads every stored copy it reuses and compares its checksum. A damaged copy is stored again from the original. A full backup counts as a health check.
- Overdue alert: a notification when an enabled job has had no successful backup for more than 3 days (`notifications.overdue_days`, weekly jobs at least 8). Checked in the background, also when backups do not run at all; repeated once a day.
- Linux: the permission bits of each file are stored and restored. Backed-up copies are never more readable than the original; new folders are private (0700).

### Changed

- Unlocking in the window runs in the background; the window stays responsive while the key is derived.
- Retention never removes the newest backup in which every file was backed up. A newer backup with unreadable files no longer replaces it.

### Security

Findings of a second external source review (01.11.2026):

- A readable part placed next to a fully encrypted backup, or an edited readable part of a partly encrypted backup, is refused before a restore or check writes or reports anything. New partly encrypted backups store a checksum of their readable part in the encrypted header.
- Restoring to the original place no longer writes through symbolic links or junctions placed inside a backed-up folder.
- `vault.json` is written under an unpredictable temporary name. The Python decryptor creates its temporary files exclusively and does not write through links.
- Tighter limits for key settings in `vault.json` (3 slots, 1 GiB, 8 passes, a total budget, 64 KiB file size), also when a key slot is replaced and in the Python decryptor.
- Linux: files that differ only in upper and lower case are kept apart in incremental backups (0.8.1 fixed only the folder names).
- Addresses in notification errors are reduced to scheme and host in every form, including user names, passwords, queries and fragments.
- Documentation: the size of each stored content is visible in encrypted backups; the README no longer says otherwise.

## [0.8.1] - 2026-10-31

### Fixed

- After a power failure, a backed-up file could remain empty or incomplete, and later incremental backups linked to it instead of copying it again. Files are now flushed to the disk before they count as backed up (Settings → Advanced), and an earlier copy is reused only if its size and time match the index; otherwise it is copied again.
- The same for encrypted backups: stored content is reused only if its size matches exactly; damaged content is stored again.
- Backed-up files are read-only. Unchanged files of several backups share the same data, so editing one in a backup folder changed all of them.
- `config.toml` holds notification passwords and tokens: on Linux it is now readable only by its owner, and it is flushed to the disk when saved.
- Notifications refuse to send tokens over unencrypted `http://` or SMTP passwords without TLS (except to this computer). Tokens and passwords are removed from error messages and the log.
- With encryption on, notifications no longer include warning texts, which can name files of encrypted folders.
- CSV export: cells that start with `=`, `+`, `-` or `@` are no longer run as formulas in spreadsheet programs.
- The command before a backup no longer receives `AETERNAVAULT_PASSPHRASE`, is stopped when the job is cancelled, and cannot block on large error output.

### Security

Findings of an external source audit (31.10.2026):

- Restoring a partly encrypted backup took the folders of the encrypted files from the readable header, which anyone with access to the storage could edit. Encrypted files now go only where their authenticated header says, and both parts must belong together.
- A job with its own destination could encrypt with the key of the vault open in the window. Each destination now uses only its own key; a backup with a key that does not match the destination's vault is refused before anything is written.
- Restoring or copying into a chosen folder no longer writes through symbolic links or junctions inside it. Temporary files get unpredictable names and are only created new.
- Moving a backup removes the old copy only after every file at the new place has been read again and matched its checksum, and only within the same vault and key.
- Encrypted content is always stored freshly encrypted, which also repairs a stored copy damaged without changing its size. Automatic jobs remove old backups only after the new backup completed and, with checking on, passed the check.
- Changing the passphrase or the recovery key in the window needs the passphrase entered in this session; switching to another vault ends read access.
- `vault.json` with implausible key settings is refused before any key derivation.
- Notifications: every `http://` address is refused except on this computer (webhook addresses are secrets), loopback is checked by address instead of name prefix, and addresses are shortened in error messages.
- Opened encrypted files and downloaded updates go into new folders only the user can open (Linux 0700).
- Linux: file names that differ only in case are no longer mixed up in incremental backups.
- The browser profile notes no longer promise more than the exclusions do; browser profiles are marked sensitive.
- `ctutils` updated to 0.4.7 (RUSTSEC-2026-0385). CI checks dependencies with `cargo audit`.

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
