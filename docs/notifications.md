# Notifications

AeternaVault can report every backup: a summary on success, an alert when a backup failed, was incomplete or interrupted, and an alert when the destination runs out of space. Backups from the window, from backup jobs and from `aeternavault-cli` are reported alike.

Set it up under **Settings → Notifications**, or on the command line.

## Channels

| Kind | What is needed |
|---|---|
| ntfy | topic URL, e.g. `https://ntfy.sh/company-backups`; token for protected topics |
| E-mail | SMTP server, port, STARTTLS / TLS, user, password, sender, recipients |
| Microsoft Teams | webhook URL of a Workflows flow (channel → … → Workflows → "Post to a channel when a webhook request is received") |
| Telegram | bot token (from @BotFather) and chat id |
| Slack | incoming webhook URL |
| Discord | channel webhook URL |
| Gotify | server URL and app token |
| Webhook | any URL; receives JSON `{source, version, computer, severity, title, message}`, optional bearer token |

```sh
aeternavault-cli notify add --kind teams --name "IT" --url "https://…"
aeternavault-cli notify add --kind email --name "Admins" --smtp-host smtp.example.com --smtp-user backup@example.com --to "it@example.com, ops@example.com" --passphrase-stdin
aeternavault-cli notify add --kind telegram --token 123456:ABC… --chat-id 987654321
aeternavault-cli notify list
aeternavault-cli notify test            # all enabled channels
aeternavault-cli notify test "IT"
aeternavault-cli notify disable "IT"
```

With `--passphrase-stdin`, the SMTP password is read from standard input instead of the command line.

## Rules and thresholds

| Setting (`config set notifications.…`) | Default |
|---|---|
| `on_success` | `true` |
| `on_problem` (failed, incomplete, interrupted, passphrase needed) | `true` |
| `on_skipped` (destination not connected) | `false` |
| `low_space_percent` (alert below this share free, 0 = off) | `10` |
| `low_space_gb` (alert below this many GB free, 0 = off) | `0` |

## Texts

`success_title`, `success_message`, `problem_title`, `problem_message`, `low_space_title` and `low_space_message` replace the built-in texts. Placeholders:

`{job}` `{status}` `{backup}` `{files}` `{copied}` `{size}` `{failed}` `{duration}` `{computer}` `{destination}` `{free}` `{free_percent}` `{message}`

```sh
aeternavault-cli config set notifications.problem_title "[BACKUP] {computer}: {status}"
```

Passwords and tokens are stored in `config.toml` in the user profile, readable only by that user account.

## Databases (PostgreSQL)

Database files must not be copied while the server runs; the copy would be inconsistent. Let PostgreSQL write a dump into a folder and back up that folder, e.g. with a scheduled task shortly before the backup job:

```powershell
pg_dump --format=custom --file "D:\DB-Dumps\company.dump" company
```

(DBeaver is a client; its connection settings live in `%APPDATA%\DBeaverData`.)
