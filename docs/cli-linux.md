# Command line on Linux

## Install

| Download | Contents |
|---|---|
| `aeternavault_<version>-1_amd64.deb` | window and command line (Debian, Ubuntu, Mint) |
| `aeternavault-cli_<version>-1_amd64.deb` | command line only, no desktop libraries (servers) |
| `aeternavault-<version>-linux-x64.tar.gz` | window and command line, any distribution |
| `aeternavault-cli-<version>-linux-x64.tar.gz` | command line only, any distribution |

```sh
sudo apt install ./aeternavault_0.5.0-1_amd64.deb
# or, without root, into ~/.local/bin:
tar xzf aeternavault-cli-0.5.0-linux-x64.tar.gz && ./aeternavault-cli-0.5.0-linux-x64/install.sh
```

Installing a newer package or running `install.sh` again updates in place; settings in `~/.config/aeternavault/` are kept.

## First backup

```sh
aeternavault-cli destination set /mnt/backup
aeternavault-cli source add ~/Documents
aeternavault-cli source add ~/Pictures
aeternavault-cli backup
aeternavault-cli list
```

`backup` prints whether it succeeded and writes the result to the history (`aeternavault-cli history`) and to the log folder (`aeternavault-cli paths`).

## Restore

```sh
aeternavault-cli files latest --filter letter           # find files
aeternavault-cli restore latest --to ~/Restored           # into a folder
aeternavault-cli restore latest --only Documents --yes    # to the original places
aeternavault-cli extract latest "Documents/Taxes" --to ~/Out
```

Backups are named by date, e.g. `2026-09-24 20-00`. `latest` is the newest one.

## Automatic backups

```sh
aeternavault-cli job add --every day --at 20:00 --name Evening
aeternavault-cli job add --every hours --hours 4 --folder Documents
aeternavault-cli system background on     # systemd user timer, checks every 5 minutes
aeternavault-cli retention on
```

The timer runs `aeternavault-cli job run-due` and works without a desktop session. Check it with `systemctl --user list-timers aeternavault*`. On servers, `loginctl enable-linger $USER` keeps it running after logout. Without systemd, a cron entry does the same:

```sh
*/5 * * * * aeternavault-cli job run-due
```

## Encryption

```sh
aeternavault-cli encryption setup --remember --recovery-file ~/recovery-key.txt
aeternavault-cli backup
aeternavault-cli files latest             # asks for the passphrase
```

- Keep the recovery key away from the backups. Without the passphrase or the recovery key, nothing can be read.
- `--remember` stores the key in `~/.config/aeternavault/` (readable only by you) so jobs run unattended. Listing file names, restoring and extracting always need the passphrase.
- In scripts: `printf '%s\n' "$PASS" | aeternavault-cli --passphrase-stdin files latest`, or the variable `AETERNAVAULT_PASSPHRASE`. Without a terminal, the CLI never waits for input.

## Settings

```sh
aeternavault-cli config show
aeternavault-cli config set language de
aeternavault-cli status --json | jq .
```

`--json` works with every command. Exit codes: `0` success, `1` error, `2` completed with problems or not confirmed.

PowerShell 7 users can load the module from the package: `Import-Module /usr/share/aeternavault/powershell/AeternaVault` (see [cli-windows.md](cli-windows.md#powershell-module)).

## Update

```sh
aeternavault-cli update --check           # is a newer version out?
aeternavault-cli update                   # download, verify and install it
```

A `.deb` installation is updated through apt and asks for the password; a tarball installation runs its `install.sh` again.

## Uninstall

```sh
aeternavault-cli system cleanup           # removes the background timer
sudo apt remove aeternavault              # or: ./install.sh --uninstall [--purge]
```

## Reference

`aeternavault-cli --help` lists all commands, `aeternavault-cli <command> --help` their options.
