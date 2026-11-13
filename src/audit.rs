//! Who changed what, when and from where: an append-only log next to the
//! configuration (`audit.jsonl`), separate from the activity history.
//!
//! It records sign-ins to the web interface, user management, actions
//! started over the web, and every saved change of the settings, whoever
//! made it (window, command line, web), with the changed fields. Values of
//! passwords, tokens and web addresses of notification channels are never
//! written, only that they changed.
//!
//! Each line names the SHA-256 of the line before it, so an edited or
//! removed line breaks the chain and [`verify`] finds it. That shows changes
//! made later; it does not stop someone with write access to the profile
//! from rewriting the whole file.

use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const AUDIT_FILE: &str = "audit.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: DateTime<Utc>,
    /// `web:alice`, `window:bob`, `command-line:bob`, `system`.
    pub actor: String,
    /// Where it came from: an IP address for the web, else this computer.
    pub source: String,
    /// Short kebab-case name, e.g. `sign-in`, `job-run`, `settings-changed`.
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    pub ok: bool,
    /// SHA-256 of the previous line, empty for the first one.
    #[serde(default)]
    pub prev: String,
}

/// Who acts in this process when nothing more specific is known: the window
/// or the command line, with the signed-in user of the computer.
static PROCESS_ACTOR: Mutex<Option<String>> = Mutex::new(None);
static WRITE: Mutex<()> = Mutex::new(());

pub fn set_process_actor(kind: &str) {
    let user = crate::platform::user_name();
    *PROCESS_ACTOR.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{kind}:{user}"));
}

pub fn process_actor() -> String {
    PROCESS_ACTOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| format!("program:{}", crate::platform::user_name()))
}

pub fn this_computer() -> String {
    format!("this computer ({})", crate::platform::computer_name())
}

pub fn file(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .map(|d| d.join(AUDIT_FILE))
        .unwrap_or_else(|| PathBuf::from(AUDIT_FILE))
}

/// What one record says; `record` adds the time and the chain.
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub actor: String,
    pub source: String,
    pub action: String,
    pub target: String,
    pub details: Vec<String>,
    pub ok: bool,
}

impl Record {
    pub fn new(actor: impl Into<String>, source: impl Into<String>, action: &str) -> Self {
        Self {
            actor: actor.into(),
            source: source.into(),
            action: action.into(),
            ok: true,
            ..Self::default()
        }
    }

    /// By this process (window or command line) on this computer.
    pub fn local(action: &str) -> Self {
        Self::new(process_actor(), this_computer(), action)
    }

    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    pub fn details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }

    pub fn failed(mut self) -> Self {
        self.ok = false;
        self
    }
}

/// Appends one entry. Errors are logged, never passed on: a full disk must
/// not stop a backup.
pub fn record(config_file: &Path, record: Record) {
    if let Err(err) = append(&file(config_file), record) {
        tracing::warn!("audit log could not be written: {err}");
    }
}

fn append(path: &Path, record: Record) -> std::io::Result<()> {
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The window and the command line may write at the same time.
    let _lock = FileLock::acquire(path);
    let prev = last_line(path)?
        .map(|line| crate::engine::format_sha256(&Sha256::digest(line.as_bytes())))
        .unwrap_or_default();
    let entry = AuditEntry {
        at: Utc::now(),
        actor: record.actor,
        source: record.source,
        action: record.action,
        target: record.target,
        details: record.details,
        ok: record.ok,
        prev,
    };
    let mut line = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(line.as_bytes())?;
    file.sync_data()
}

/// The last non-empty line, without reading the whole file.
fn last_line(path: &Path) -> std::io::Result<Option<String>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let len = file.metadata()?.len();
    let mut start = len;
    let mut tail = Vec::new();
    // Read backwards in blocks until a line break before the last line.
    loop {
        let step = start.min(4096);
        start -= step;
        file.seek(SeekFrom::Start(start))?;
        let mut block = vec![0; step as usize];
        file.read_exact(&mut block)?;
        block.extend_from_slice(&tail);
        tail = block;
        let trimmed = tail.trim_ascii_end();
        if let Some(pos) = trimmed.iter().rposition(|&b| b == b'\n') {
            return Ok(Some(
                String::from_utf8_lossy(&trimmed[pos + 1..]).into_owned(),
            ));
        }
        if start == 0 {
            return Ok((!trimmed.is_empty()).then(|| String::from_utf8_lossy(trimmed).into_owned()));
        }
    }
}

/// Whether an entry is about signing in to the web interface (attempts,
/// codes, sign-outs, passwords and second steps).
pub fn is_sign_in(action: &str) -> bool {
    action.starts_with("sign-")
        || matches!(
            action,
            "password-changed"
                | "one-time-codes-on"
                | "one-time-codes-off"
                | "email-codes-on"
                | "email-codes-off"
        )
}

/// All entries, oldest first. Lines that cannot be read are skipped.
pub fn load(config_file: &Path) -> Vec<AuditEntry> {
    let Ok(file) = std::fs::File::open(file(config_file)) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// Result of checking the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub entries: usize,
    /// 1-based line numbers whose predecessor does not match.
    pub broken: Vec<usize>,
}

pub fn verify(config_file: &Path) -> std::io::Result<Verified> {
    let text = match std::fs::read_to_string(file(config_file)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let mut previous: Option<&str> = None;
    let mut result = Verified {
        entries: 0,
        broken: Vec::new(),
    };
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        result.entries += 1;
        let expected = previous
            .map(|p| crate::engine::format_sha256(&Sha256::digest(p.as_bytes())))
            .unwrap_or_default();
        let ok = serde_json::from_str::<AuditEntry>(line).is_ok_and(|e| e.prev == expected);
        if !ok {
            result.broken.push(n + 1);
        }
        previous = Some(line);
    }
    Ok(result)
}

/// A lock file next to the log, given up after a few seconds (a stale lock
/// of a crashed process must not stop the log for good).
struct FileLock(Option<PathBuf>);

impl FileLock {
    fn acquire(path: &Path) -> Self {
        let lock = path.with_extension("jsonl.lock");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => return Self(Some(lock)),
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => {
                    let stale = std::fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t.elapsed().is_ok_and(|e| e > Duration::from_secs(30)));
                    if stale {
                        let _ = std::fs::remove_file(&lock);
                    }
                    return Self(None);
                }
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(lock) = &self.0 {
            let _ = std::fs::remove_file(lock);
        }
    }
}

/// The settings that differ between `old` and `new`, one line each:
/// `retention.keep_last: 7 → 14`. Secrets show only that they changed.
pub fn config_changes(old: &crate::config::Config, new: &crate::config::Config) -> Vec<String> {
    let (Ok(old), Ok(new)) = (serde_json::to_value(old), serde_json::to_value(new)) else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    diff("", &old, &new, &mut changes);
    changes
}

fn is_secret(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    ["password", "token", "secret", "url", "webhook", "key"]
        .iter()
        .any(|word| lower.contains(word))
}

fn short(value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(s) => format!("\"{s}\""),
        serde_json::Value::Null => "none".into(),
        other => other.to_string(),
    };
    if text.chars().count() > 120 {
        format!("{}…", text.chars().take(120).collect::<String>())
    } else {
        text
    }
}

fn diff(path: &str, old: &serde_json::Value, new: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    if old == new {
        return;
    }
    let join = |key: &str| {
        if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        }
    };
    // An empty list is not written to the file; it reads back as nothing.
    let empty = Value::Array(Vec::new());
    let (old, new) = match (old, new) {
        (Value::Null, Value::Array(_)) => (&empty, new),
        (Value::Array(_), Value::Null) => (old, &empty),
        _ => (old, new),
    };
    if old == new {
        return;
    }
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let null = Value::Null;
                diff(
                    &join(key),
                    a.get(key).unwrap_or(&null),
                    b.get(key).unwrap_or(&null),
                    out,
                );
            }
        }
        // Lists of the same length are compared item by item (jobs, folders);
        // otherwise what was added and removed is named.
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (n, (x, y)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}[{n}]"), x, y, out);
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            let added = b.iter().filter(|v| !a.contains(v)).count();
            let removed = a.iter().filter(|v| !b.contains(v)).count();
            out.push(format!(
                "{path}: {} → {} entries (+{added}, -{removed})",
                a.len(),
                b.len()
            ));
            if !is_secret(path) {
                for value in b.iter().filter(|v| !a.contains(v)) {
                    out.push(format!("{path} +{}", short(&describe(value))));
                }
                for value in a.iter().filter(|v| !b.contains(v)) {
                    out.push(format!("{path} -{}", short(&describe(value))));
                }
            }
        }
        _ if is_secret(path) => out.push(format!("{path}: changed")),
        _ => out.push(format!("{path}: {} → {}", short(old), short(new))),
    }
}

/// A list item named by its most telling field (a folder by its path, a job
/// by its name), without the secrets a channel holds.
fn describe(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => ["name", "path", "label", "id", "kind"]
            .iter()
            .find_map(|k| {
                map.get(*k)
                    .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            })
            .cloned()
            .unwrap_or_else(|| serde_json::Value::String("(entry)".into())),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_shows_edited_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.toml");
        for n in 0..3 {
            record(
                &config,
                Record::new("web:alice", "10.0.0.5", "job-run").target(format!("job {n}")),
            );
        }
        let entries = load(&config);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].prev, "");
        assert_eq!(verify(&config).unwrap().broken, Vec::<usize>::new());

        let path = file(&config);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen("job 1", "job 9", 1)).unwrap();
        assert_eq!(verify(&config).unwrap().broken, vec![3]);
    }

    #[test]
    fn settings_changes_name_fields_but_hide_secrets() {
        use crate::config::Config;
        let mut old = Config::default();
        old.notifications.channels.push(crate::notify::Channel {
            smtp_password: "old-secret".into(),
            url: "https://example.org/hook/old-token".into(),
            ..Default::default()
        });
        let mut new = old.clone();
        new.retention.keep_last = 99;
        new.notifications.channels[0].smtp_password = "hunter2".into();
        new.notifications.channels[0].url = "https://example.org/hook/new-token".into();
        let changes = config_changes(&old, &new);
        assert!(
            changes
                .iter()
                .any(|c| c.starts_with("retention.keep_last:") && c.contains("99")),
            "{changes:?}"
        );
        for secret in ["hunter2", "old-secret", "token"] {
            assert!(changes.iter().all(|c| !c.contains(secret)), "{changes:?}");
        }
        assert!(
            changes
                .iter()
                .any(|c| c.ends_with("smtp_password: changed")),
            "{changes:?}"
        );
    }
}
