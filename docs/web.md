# Web interface

An overview of jobs, backups and problems in a browser, and the everyday actions: run or cancel a job, switch a job on or off, check a backup. Meant for computers that are managed from elsewhere, such as a server, so nobody has to sign in to the computer itself to see whether backups run.

Off unless switched on. Restoring, deleting backups, changing folders or settings, and entering passphrases are not possible over the web; they stay in the window and the command line.

## Switching it on

The web interface is one of the optional parts of the program: Settings → Features → Web interface (on a new installation all optional parts are off). Then:

In the window: **Web** tab → *Switch on the web interface*, then add the first user (an administrator) under *Users*.

On the command line:

```sh
aeternavault-cli web user add admin --role admin
aeternavault-cli web on
aeternavault-cli web status
```

The web interface runs while AeternaVault is open, also when it waits in the notification area. Without the window (a server with only the command line, or as a service):

```sh
aeternavault-cli web serve
```

`web serve` runs jobs that are started over the web itself. Scheduled jobs keep running through the window or the systemd timer as before. Do not run `web serve` and a window with the web interface switched on at the same time; only one of them can use the port.

## Reaching it

| Setting | Default | |
|---|---|---|
| `bind` | `127.0.0.1` | This computer only. `0.0.0.0`: every network of the computer. |
| `port` | `8686` | |
| `tls` | `auto` | `auto`: plain HTTP for this computer only, HTTPS otherwise. `self-signed`: always HTTPS. `files`: HTTPS with `cert_file` and `key_file` (PEM). `off`: plain HTTP, only allowed for this computer. |
| `allowed_networks` | empty (any) | e.g. `["192.168.1.0/24", "10.0.0.5"]`. The computer itself may always connect. |

```sh
aeternavault-cli web set --bind 0.0.0.0 --allow 192.168.1.0/24
```

From the network, connections are always encrypted. Without own certificate files, AeternaVault makes a self-signed certificate (in `web-tls` next to the configuration) for the computer name, `localhost` and the computer's address. Browsers warn about it once. Compare the SHA-256 fingerprint the browser shows with the one in the Web tab or in `aeternavault-cli web status`; if they match, the warning is about this computer. After a change of the computer name or address: `aeternavault-cli web renew-certificate`.

Windows asks once whether the program may accept connections from the network (firewall).

## Users and roles

| Role | May |
|---|---|
| Viewer | see the overview, jobs, destinations, backups and the activity |
| Operator | also run, cancel and switch jobs on and off, and check backups |
| Administrator | also manage users and read the audit log |

A user can be limited to some jobs; they then see only those jobs and their destinations.

Users are kept in `web-users.json` next to the configuration (readable by its owner only on Linux), with passwords as Argon2id hashes. The first administrator can only be created on the computer itself (window or command line), never over the network. A user added by an administrator over the web, or given a new password, has to choose a new password after signing in. The last administrator cannot be removed, locked or demoted.

| Setting | Default | |
|---|---|---|
| `session_minutes` | `30` | Signed out after this many minutes without activity (at most 12 hours in total). |
| `require_two_factor` | `false` | Every user needs a second step after the password: a code from an authenticator app (RFC 6238) or a code by e-mail. Users set it up under *Account*. |
| `lockout_attempts`, `lockout_minutes` | `5`, `15` | After this many wrong sign-ins for a name, it has to wait. An address waits after three times as many. |
| `recommended_password_length` | `12` | A recommendation: shorter passwords are accepted with a note, as are easily guessed ones. Only an empty password is refused. |
| `email_channel` | empty | The e-mail notification channel that sends sign-in codes; empty: the first switched-on one. |

A lost phone: `aeternavault-cli web user reset-two-factor NAME`, or *Reset one-time codes* in the Web tab.

### Codes by e-mail

Instead of an authenticator app, the second step can be a six-digit code sent by e-mail. It needs an e-mail channel under Notifications (the same SMTP account as for backup reports). A code is valid for 10 minutes, once, and for five tries; a new one can be sent after a minute.

- A user switches it on under *Account* and confirms the address with a code.
- Directory users have the address of their directory account (`mail`).
- An administrator can enter the address of a user in the Web tab (*Actions*). With `require_two_factor`, users with a known address and no authenticator app then get codes by e-mail without setting anything up.

## LDAP / Active Directory

Users can sign in with their directory account. In the Web tab: *LDAP / Active Directory*, or on the command line:

```sh
printf '%s\n' 'password of the search account' | aeternavault-cli --passphrase-stdin web ldap set \
  --preset active-directory --url ldaps://dc1.example.local \
  --bind-dn 'CN=svc-backup,OU=Service,DC=example,DC=local' --base-dn 'DC=example,DC=local' \
  --admin-group 'CN=Backup-Admins,OU=Groups,DC=example,DC=local' \
  --viewer-group 'CN=Backup-Readers,OU=Groups,DC=example,DC=local'
aeternavault-cli web ldap on
aeternavault-cli web ldap test --user anna
```

| Setting | |
|---|---|
| `url` | `ldaps://…` (encrypted) or `ldap://…` with `starttls`. Unencrypted `ldap://` only with `allow_unencrypted` (test setups). |
| `ca_file` | An own certificate authority (PEM). Certificates trusted by the computer are always accepted, on Windows also company CAs from group policy. |
| `bind_dn`, `bind_password` | The account that searches. Empty: anonymous. With `{user}`, e.g. `{user}@example.local`: the user signing in searches with their own account; no service account is needed. |
| `base_dn`, `user_filter` | Where and how the user is found; `{user}` is the name typed at sign-in. Presets: Active Directory `(&(objectClass=user)(sAMAccountName={user}))`, OpenLDAP `(&(objectClass=inetOrgPerson)(uid={user}))`. |
| `admin_group`, `operator_group`, `viewer_group` | Members get the role (the highest if several). Members of none cannot sign in. |
| `nested_groups` | Active Directory: membership through nested groups counts. |

The role is read again at every sign-in. Directory users appear in the user list after their first sign-in; there they can be limited to jobs, locked, and their one-time codes reset. Their password is checked by the directory, never stored. Users created here keep working besides, e.g. as an emergency administrator when the directory cannot be reached. A directory that cannot be reached does not count as a wrong password.

## Which device

Sign-ins, sessions and the audit log name the device a request came from: its address, its name (reverse DNS, e.g. `pc-anna.example.local`) and its MAC address. The MAC address comes from the neighbour table of the computer running AeternaVault, so it is known only for devices in the same network segment; behind a router it would be the router's, so none is shown. A browser on the computer itself appears with the computer's network address, name and MAC address instead of `127.0.0.1`.

## Sign-ins, live

Administrators see under *Activity → Sign-ins (live)* who is signed in now, how many sign-ins succeeded and failed in the last 24 hours, and every attempt with user, address and reason (wrong password, unknown user, wrong code, locked, not in a group). The page updates itself. In the window, the same is under *Activity → Sign-ins (web)*.

## Audit log

`audit.jsonl` next to the configuration lists who did what, when and from where:

- sign-ins (also failed ones, with the reason), sign-outs, codes sent by e-mail;
- users added, changed, locked, removed, passwords and one-time codes reset;
- jobs run, cancelled and switched over the web, backups checked over the web;
- every saved change of the settings, from the window, the command line or the web, with the changed fields. Passwords, tokens and web addresses of notification channels are only noted as changed;
- the web interface started and stopped.

Each line names the SHA-256 of the line before it. `aeternavault-cli web audit --verify` (or the Web tab) reports lines that were changed or removed afterwards. This shows later changes; it does not stop someone who may write to the user profile from rewriting the whole file.

```sh
aeternavault-cli web audit --last 100
aeternavault-cli web audit --json
```

## Protection in detail

- HTTPS with rustls (TLS 1.2 and 1.3) whenever other computers can connect.
- Session cookies are `HttpOnly`, `SameSite=Strict`, and over HTTPS `Secure` with the `__Host-` prefix. Every form carries a token of the session; forms from other sites are refused.
- While only this computer may connect, requests must name `localhost` or `127.0.0.1` as host, so a web page elsewhere cannot reach the interface through a made-up name.
- Pages contain no scripts; a strict content security policy forbids scripts, frames and outside resources.
- One request per connection, at most 32 connections, 16 KiB of headers and 64 KiB of form data, 15 seconds per request.
- Unknown user names take as long to refuse as wrong passwords.
