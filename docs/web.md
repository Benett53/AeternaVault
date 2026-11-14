# Web interface

An overview of jobs, backups and problems in a browser, and the everyday actions: run or cancel a job, switch a job on or off, check a backup. Meant for computers that are managed from elsewhere, such as a server, so nobody has to sign in to the computer itself to see whether backups run.

Off unless switched on. Restoring, deleting backups, changing folders or settings, and entering passphrases are not possible over the web; they stay in the window and the command line.

## Switching it on

The web interface is one of the optional parts of the program: Settings → Features → Web interface (on a new installation all optional parts are off). Then:

In the window: **Settings → Web interface** → *Enable web interface*, then add the first user (an administrator) under *Users*.

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

Switched on, the web interface is reachable like other home-server software:

- on the computer itself: `http://localhost:8686/`
- from other devices in the network: `http://<address of the computer>:8686/` (shown in the Web interface settings and in `aeternavault-cli web status`)

Windows asks once whether the program may accept connections from the network (firewall).

| Setting | Default | |
|---|---|---|
| `bind` | `0.0.0.0` | Every network of the computer. `127.0.0.1`: this computer only. |
| `port` | `8686` | |
| `tls` | `off` | `off`: plain HTTP. `files`: HTTPS with `cert_file` and `key_file` (PEM). |
| `allowed_networks` | empty (any) | e.g. `["192.168.1.0/24", "10.0.0.5"]`. The computer itself may always connect. |
| `trusted_proxies` | empty | Reverse proxies in front, e.g. `["127.0.0.1", "10.0.0.2"]`. |

```sh
aeternavault-cli web set --port 8686 --allow 192.168.1.0/24
```

### HTTPS and access from the internet

Plain HTTP is meant for the local network. For HTTPS, and always for access from the internet, use one of these:

- **A reverse proxy** in front (Caddy, nginx, Traefik, Nginx Proxy Manager), which handles the certificate and forwards to `http://<computer>:8686`. Only the proxy is opened to the outside. Enter the proxy's address under *Reverse proxy* (`trusted_proxies`); AeternaVault then takes the client address from `X-Forwarded-For`, the scheme from `X-Forwarded-Proto` and the host name from `X-Forwarded-Host`, so sign-ins, blocking and the audit log name the real device and session cookies are marked secure. These headers are ignored from every other address. With the proxy on the same computer, `aeternavault-cli web set --bind 127.0.0.1 --trusted-proxies 127.0.0.1` keeps the interface itself off the network.
- **Own certificate files** (`tls = "files"`), from an internal PKI (company CA) or from Let's Encrypt (certbot on Linux, win-acme on Windows). AeternaVault speaks HTTPS itself and reads the files again when they change (checked once a minute), so a renewal needs no restart. A renewal that cannot be read yet keeps the previous certificate in use. The settings show whom the certificate is for, who issued it, until when it is valid and its SHA-256 fingerprint.

AeternaVault makes no certificates of its own.

Example for Caddy:

```text
backup.example.com {
    reverse_proxy 192.168.1.20:8686
}
```

## Access

| Setting | Default | |
|---|---|---|
| `auto_block_after` | `5` | An address with this many wrong sign-ins within 24 hours is blocked. `0`: never. This computer is never blocked. |
| `geo.mode` | `off` | `allow`: only the listed countries may connect. `block`: all but the listed ones. |
| `geo.countries` | empty | ISO codes, e.g. `["DE", "AT", "CH"]`. |
| `geo.database` | empty | A country database (`.mmdb`): [DB-IP IP to Country Lite](https://db-ip.com/db/download/ip-to-country-lite) or MaxMind GeoLite2 Country, both free. Addresses of private networks and of this computer are not checked; an address the database does not know counts as not listed. |
| `admin_access` | `this-computer` | Where administrators may manage users and block addresses: `anywhere`, `this-computer`, or `networks` (this computer and `admin_networks`). Elsewhere they sign in with the other permissions of their role. |
| `admin_networks` | empty | e.g. `["192.168.1.0/24"]`. |

Settings from 0.14 are moved once: the interface listens on the network, addresses are blocked after 5 wrong sign-ins, and administration is limited to this computer (unless other networks were chosen).

In the window the countries are chosen from a list. The page shows the type and date of the database, can look up an address with the current rule, and counts refused connections per country since the start.

Blocked addresses and networks are kept in `web-blocked.json` next to the configuration. A change applies at once, also to a running web interface, without signing anybody out. Block an address in the Web tab (*Access*), with *Block* next to a failed sign-in (web: *Activity → Sign-ins*, window: *Activity → Sign-ins (web)*), on the web page *Access*, or on the command line:

```sh
aeternavault-cli web block 203.0.113.7 --reason "password guessing"
aeternavault-cli web block 198.51.100.0/24
aeternavault-cli web blocked
aeternavault-cli web unblock 203.0.113.7
aeternavault-cli web set --geo allow --geo-countries DE,AT --geo-database /var/lib/geo/dbip-country-lite.mmdb
aeternavault-cli web set --admin-access networks --admin-networks 192.168.1.0/24
```

## Users and roles

| Role | May |
|---|---|
| Viewer | see the overview, jobs, destinations, backups and the activity |
| Operator | also run, cancel and switch jobs on and off, and check backups |
| Administrator | also see sign-ins, block addresses, read the audit log, and manage users and roles |

Own roles combine these permissions freely: `view-activity`, `run-jobs`, `switch-jobs`, `check-backups`, `view-sign-ins`, `block-addresses`, `view-audit`, `manage-users`. In the Web tab under *Roles*, on the web page *Users*, or:

```sh
aeternavault-cli web role set auditor --permissions view-activity,view-sign-ins,view-audit
aeternavault-cli web role list
aeternavault-cli web user role anna auditor
```

A user can be limited to some jobs; they then see only those jobs and their destinations.

Users are kept in `web-users.json` next to the configuration (readable by its owner only on Linux), with passwords as Argon2id hashes. The first administrator can only be created on the computer itself (window or command line), never over the network. A user added by an administrator over the web, or given a new password, has to choose a new password after signing in. The last administrator cannot be removed, locked or demoted.

| Setting | Default | |
|---|---|---|
| `session_minutes` | `30` | Signed out after this many minutes without activity (at most 12 hours in total). |
| `mfa` | `optional` | Who needs a second factor (2FA) after the password: `optional`, `administrators` (roles that manage users or block addresses), or `everyone`. Users set it up under *Account*. `require_two_factor = true` from older versions becomes `everyone`. |
| `mfa_app`, `mfa_email` | `true`, `true` | Which second factors may be used: an authenticator app (RFC 6238), codes by e-mail. |
| `lockout_attempts`, `lockout_minutes` | `5`, `15` | After this many wrong sign-ins for a name, it has to wait. An address waits after three times as many. |
| `recommended_password_length` | `12` | A recommendation: shorter passwords are accepted with a note, as are easily guessed ones. Only an empty password is refused. |
| `email_channel` | empty | The e-mail notification channel that sends sign-in codes; empty: the first switched-on one. |

A lost phone: `aeternavault-cli web user reset-two-factor NAME`, or *Reset one-time codes* in the Web tab.

### Codes by e-mail

Instead of an authenticator app, the second factor can be a six-digit code sent by e-mail. It needs an e-mail channel under Notifications (the same SMTP account as for backup reports). A code is valid for 10 minutes, once, and for five tries; a new one can be sent after a minute.

- A user switches it on under *Account* and confirms the address with a code.
- Directory users have the address of their directory account (`mail`).
- An administrator can enter the address of a user in the Web tab (*Actions*). When 2FA is required for them, users with a known address and no authenticator app then get codes by e-mail without setting anything up.

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
| `role_source` | `directory`: the role comes from directory groups. `aeternavault`: the directory checks only the password; roles are given in AeternaVault. |
| `admin_group`, `operator_group`, `viewer_group`, `group_roles` | With `role_source = "directory"`: members get the role; `group_roles` maps any further group to any role, own roles included. Members of several groups get the role with the most permissions; members of none cannot sign in. |
| `default_role` | With `role_source = "aeternavault"`: the role at the first sign-in. Empty: no access until an administrator gives one. |
| `nested_groups` | Active Directory: membership through nested groups counts. |

With roles from the directory, the role is read again at every sign-in. Directory users appear in the user list after their first sign-in; there they can be limited to jobs, locked, and their one-time codes reset. Their password is checked by the directory, never stored. Users created here keep working besides, e.g. as an emergency administrator when the directory cannot be reached. A directory that cannot be reached does not count as a wrong password.

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

- HTTPS with rustls (TLS 1.2 and 1.3) with own certificate files, or from a reverse proxy.
- Session cookies are `HttpOnly`, `SameSite=Strict`, and over HTTPS `Secure` with the `__Host-` prefix. Every form carries a token of the session; forms from other sites are refused.
- While only this computer may connect, requests must name `localhost` or `127.0.0.1` as host, so a web page elsewhere cannot reach the interface through a made-up name.
- Pages contain no scripts; a strict content security policy forbids scripts, frames and outside resources.
- One request per connection, at most 32 connections, 16 KiB of headers and 64 KiB of form data, 15 seconds per request.
- Unknown user names take as long to refuse as wrong passwords.
