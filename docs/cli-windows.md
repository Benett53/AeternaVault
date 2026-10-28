# Command line on Windows

The installer puts `aeternavault-cli` on `PATH` and installs the PowerShell module `AeternaVault`. Open a new terminal after installing. With the ZIP download, call `.\aeternavault-cli.exe` from the unpacked folder.

Everything the window does can be done here. The window and the command line share the same settings (`%APPDATA%\AeternaVault\config.toml`).

## First backup

```powershell
aeternavault-cli destination set E:\Backups
aeternavault-cli source add "$env:USERPROFILE\Documents"
aeternavault-cli source add "$env:USERPROFILE\Pictures"
aeternavault-cli backup
aeternavault-cli list
```

`backup` prints whether it succeeded and writes the result to the history (`aeternavault-cli history`) and to the log folder (`aeternavault-cli paths`).

## Restore

```powershell
aeternavault-cli files latest --filter letter           # find files
aeternavault-cli restore latest --to D:\Restored          # into a folder
aeternavault-cli restore latest --only Documents --yes    # to the original places
aeternavault-cli extract latest "Documents/Taxes" --to D:\Out
```

Backups are named by date, e.g. `2026-09-24 20-00`. `latest` is the newest one.

## Automatic backups

```powershell
aeternavault-cli job add --every day --at 20:00 --name Evening
aeternavault-cli job add --every week --day sunday --at 10:00 --folder Documents --verify
aeternavault-cli job list
aeternavault-cli system background on     # start with Windows, run jobs in the background
aeternavault-cli retention on             # remove old backups by the rules
aeternavault-cli retention set --keep-last 5 --days 14 --weeks 8 --months 24
```

## Encryption

```powershell
aeternavault-cli encryption setup --remember --recovery-file E:\recovery-key.txt
aeternavault-cli backup
aeternavault-cli files latest             # asks for the passphrase
```

- Keep the recovery key away from the backups. Without the passphrase or the recovery key, nothing can be read.
- `--remember` lets automatic backups run without the passphrase. Listing file names, restoring and extracting always need the passphrase.
- In scripts, pass it with `--passphrase-stdin` (one line per secret) or the variable `AETERNAVAULT_PASSPHRASE`. Without a terminal, the CLI never waits for input.

## PowerShell module

Every command returns objects and throws PowerShell errors.

```powershell
Import-Module AeternaVault            # automatic on first use
Get-Command -Module AeternaVault      # all commands
Get-Help Restore-AVBackup -Full

Set-AVDestination E:\Backups
Add-AVSource "$env:USERPROFILE\Documents"
Start-AVBackup
Get-AVBackup | Format-Table id, status, files, removal
New-AVJob -Every Day -At 20:00 -Name Evening
Set-AVRetention -Enabled $true -KeepLast 5

$pass = Read-Host 'Passphrase' -AsSecureString
Get-AVFile latest -Passphrase $pass | Where-Object path -like '*Taxes*'
Restore-AVBackup latest -To D:\Restored -Passphrase $pass
```

Destructive commands (`Restore-AVBackup`, `Remove-AVBackup`, `Invoke-AVPrune`) ask for confirmation; add `-Confirm:$false` in scripts. `-WhatIf` shows what would happen.

## Notifications

```sh
aeternavault-cli notify add --kind ntfy --url https://ntfy.sh/your-topic
aeternavault-cli notify test
```

E-mail, Teams, Telegram, Slack, Discord, Gotify and webhooks: see [notifications.md](notifications.md).

## Settings

```powershell
aeternavault-cli config show
aeternavault-cli config get advanced.verify_on_restore
aeternavault-cli config set language de
aeternavault-cli status --json
```

`--json` works with every command. Exit codes: `0` success, `1` error, `2` completed with problems or not confirmed.

## Reference

`aeternavault-cli --help` lists all commands, `aeternavault-cli <command> --help` their options.
