# Administration

For running AeternaVault on company computers and servers.

## Installation and updates

| Task | Command |
|---|---|
| Silent installation (Windows) | `AeternaVault-<version>-setup-x64.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART` |
| Silent uninstallation | `"C:\Program Files\AeternaVault\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES` (settings are kept) |
| Update | `aeternavault-cli update`, or the same setup over the installed version |
| Servers without a desktop (Linux) | `aeternavault-cli_<version>-1_amd64.deb` |

Settings are per user: `%APPDATA%\AeternaVault\config.toml` (Linux `~/.config/aeternavault/config.toml`). A prepared `config.toml` can be copied there before the first start. `aeternavault-cli paths` shows all locations.

## Typical setup for a database server

```powershell
aeternavault-cli destination set \\nas\backups\db01
aeternavault-cli source add D:\DB-Dumps --name "Database dumps"
aeternavault-cli job add --every day --at 22:00 --name "PostgreSQL nightly" `
    --folder "Database dumps" --verify `
    --before "pg_dump --format=custom --file D:\DB-Dumps\company.dump company"
aeternavault-cli retention on
aeternavault-cli retention set --keep-last 7 --days 14 --weeks 8 --months 12
aeternavault-cli notify add --kind teams --name "IT" --url "https://…"
aeternavault-cli system background on
```

- **Command before the backup** (`--before`): runs and is waited for; if it fails, the backup does not run and a problem is reported. Use it for database dumps (`pg_dump`, `mysqldump`) or to stop and start services. The password of `pg_dump` belongs in `%APPDATA%\postgresql\pgpass.conf`, not on the command line.
- **Destination per job** (`--destination`): e.g. a daily job to a local disk and a weekly job to a NAS. Encrypted backups need encryption set up at each destination. The Backups tab shows the general destination; others are listed with `aeternavault-cli list --destination <folder>`.
- **Names**: a job's backups carry the job's name; any backup can be renamed (`aeternavault-cli rename latest "Before migration"`).

## Monitoring

- **Notifications** after every backup and when space runs low: e-mail, Teams, Telegram, ntfy, Slack, Discord, Gotify, webhook ([notifications.md](notifications.md)).
- **Exit codes** of `aeternavault-cli`: `0` success, `1` error, `2` completed with problems. `aeternavault-cli status --json` for monitoring systems.
- **Activity history**: tab Activity (export as TXT or CSV, or send through the notification channels), or `aeternavault-cli history --format csv --last 0 --output activity.csv`.
- **Logs**: `%LOCALAPPDATA%\AeternaVault\logs` (14 days, one file per day). `AETERNAVAULT_LOG=debug` for more detail.

## Checking backups

`aeternavault-cli verify latest` reads every file again and compares it with its checksum; a job does the same after each backup with `--verify`. Test a restore regularly into a separate folder: `aeternavault-cli restore latest --to D:\RestoreTest --yes`.
