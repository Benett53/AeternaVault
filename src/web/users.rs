//! Users of the web interface, their roles, sign-in and sessions.
//!
//! Users are kept in `web-users.json` next to the configuration, readable
//! only by the owner on Linux. Passwords are stored as Argon2id hashes;
//! secrets for one-time codes (RFC 6238, as used by authenticator apps) are
//! kept as they are, like the remembered keys of the program, because the
//! program must compute the codes itself.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use argon2::{Algorithm, Argon2, Params, Version};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};

use crate::engine::crypto::{hex, random_bytes, unhex};

pub const USERS_FILE: &str = "web-users.json";

/// What a user may do. Each role includes the ones before it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Sees the overview, jobs, backups and the activity.
    Viewer,
    /// Also runs, cancels, switches on and off jobs and checks backups.
    Operator,
    /// Also manages users and reads the audit log.
    Admin,
}

impl Role {
    pub fn name(self, en: bool) -> &'static str {
        match (self, en) {
            (Role::Viewer, true) => "Viewer",
            (Role::Viewer, false) => "Betrachter",
            (Role::Operator, true) => "Operator",
            (Role::Operator, false) => "Bediener",
            (Role::Admin, true) => "Administrator",
            (Role::Admin, false) => "Administrator",
        }
    }

    pub const ALL: [Role; 3] = [Role::Viewer, Role::Operator, Role::Admin];
}

/// Where a user signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UserSource {
    /// Name and password kept here.
    #[default]
    Local,
    /// Checked against LDAP / Active Directory; the role comes from its groups
    /// at every sign-in. Kept here for job limits, one-time codes and locking.
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    pub role: Role,
    #[serde(default)]
    pub source: UserSource,
    /// `argon2id$<memory KiB>$<passes>$<salt hex>$<hash hex>`.
    pub password: String,
    /// Secret for one-time codes (base32), once set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp: Option<String>,
    /// E-mail address for sign-in codes (directory users: from the directory).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    /// The user chose codes by e-mail as second step.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub email_codes: bool,
    /// Ids of the jobs this user may see and run. Empty: all jobs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<String>,
    #[serde(default)]
    pub disabled: bool,
    /// The user has to choose a new password after signing in.
    #[serde(default)]
    pub must_change_password: bool,
    pub created: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sign_in: Option<DateTime<Utc>>,
}

/// The second step of a sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondFactor {
    App,
    Email,
}

impl User {
    /// Which second step this user's sign-in needs. `required`: the settings
    /// ask for a second step from everybody; then a known e-mail address is
    /// used even if the user did not choose it.
    pub fn second_factor(&self, required: bool, email_possible: bool) -> Option<SecondFactor> {
        if self.totp.is_some() {
            return Some(SecondFactor::App);
        }
        let has_email = email_possible && self.email.contains('@');
        (has_email && (self.email_codes || required)).then_some(SecondFactor::Email)
    }

    pub fn may(&self, role: Role) -> bool {
        !self.disabled && self.role >= role
    }

    /// Whether the user may see and run a job (`None`: a backup of all folders).
    pub fn may_use_job(&self, id: Option<&str>) -> bool {
        match id {
            _ if self.jobs.is_empty() => true,
            Some(id) => self.jobs.iter().any(|j| j == id),
            None => false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserStore {
    pub users: Vec<User>,
}

pub fn file(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .map(|d| d.join(USERS_FILE))
        .unwrap_or_else(|| PathBuf::from(USERS_FILE))
}

impl UserStore {
    pub fn load(config_file: &Path) -> Self {
        std::fs::read_to_string(file(config_file))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, config_file: &Path) -> std::io::Result<()> {
        let text = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let path = file(config_file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::platform::write_private_file(&path, &text)
    }

    pub fn find(&self, name: &str) -> Option<&User> {
        self.users
            .iter()
            .find(|u| u.name.eq_ignore_ascii_case(name))
    }

    pub fn find_mut(&mut self, name: &str) -> Option<&mut User> {
        self.users
            .iter_mut()
            .find(|u| u.name.eq_ignore_ascii_case(name))
    }

    pub fn admins(&self) -> usize {
        self.users.iter().filter(|u| u.may(Role::Admin)).count()
    }

    /// Adds a user; the name must be new and the password not empty.
    pub fn add(&mut self, name: &str, role: Role, password: &str) -> Result<(), String> {
        let name = name.trim();
        valid_name(name)?;
        if self.find(name).is_some() {
            return Err(format!("a user named {name} already exists"));
        }
        check_password(password)?;
        self.users.push(User {
            name: name.to_string(),
            role,
            source: UserSource::Local,
            password: hash_password(password),
            totp: None,
            email: String::new(),
            email_codes: false,
            jobs: Vec::new(),
            disabled: false,
            must_change_password: false,
            created: Utc::now(),
            last_sign_in: None,
        });
        Ok(())
    }

    /// Whether removing or demoting `name` would leave no administrator.
    pub fn is_last_admin(&self, name: &str) -> bool {
        self.admins() == 1 && self.find(name).is_some_and(|u| u.may(Role::Admin))
    }
}

pub fn valid_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'));
    if ok {
        Ok(())
    } else {
        Err("a user name has 1 to 64 letters, digits, '.', '-', '_' or '@'".into())
    }
}

/// Only what cannot work is refused: an empty password, or one so long it
/// would only slow the server down. Length and strength are advice, see
/// [`password_advice`].
pub fn check_password(password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("the password is empty".into());
    }
    if password.chars().count() > 1024 {
        return Err("the password is too long (more than 1024 characters)".into());
    }
    Ok(())
}

/// Advice about a password, or `None` if it looks fine: shorter than
/// recommended, or easy to guess. Never a reason to refuse it.
pub fn password_advice(password: &str, recommended: u32, en: bool) -> Option<String> {
    if password.is_empty() {
        return None;
    }
    let mut advice = Vec::new();
    let length = password.chars().count();
    if length < recommended as usize {
        advice.push(if en {
            format!("shorter than the recommended {recommended} characters ({length})")
        } else {
            format!("kürzer als die empfohlenen {recommended} Zeichen ({length})")
        });
    }
    let personal = crate::engine::passphrase::personal_details();
    let personal: Vec<&str> = personal.iter().map(String::as_str).collect();
    let rating = crate::engine::passphrase::assess(password, &personal).rating();
    if rating == crate::engine::passphrase::Rating::Weak {
        advice.push(if en {
            "easy to guess".into()
        } else {
            "leicht zu erraten".into()
        });
    }
    if advice.is_empty() {
        return None;
    }
    let text = advice.join(if en { " and " } else { " und " });
    Some(if en {
        format!("Note: the password is {text}. It is accepted anyway.")
    } else {
        format!("Hinweis: Das Passwort ist {text}. Es wird trotzdem übernommen.")
    })
}

// --- passwords ------------------------------------------------------------------

/// OWASP recommendation for Argon2id: 19 MiB, 2 passes.
const PASSWORD_MEMORY_KIB: u32 = 19 * 1024;
const PASSWORD_PASSES: u32 = 2;

fn argon2(password: &str, salt: &[u8], memory: u32, passes: u32) -> Option<[u8; 32]> {
    let params = Params::new(memory, passes, 1, Some(32)).ok()?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .ok()?;
    Some(out)
}

pub fn hash_password(password: &str) -> String {
    let salt = random_bytes::<16>();
    let hash = argon2(password, &salt, PASSWORD_MEMORY_KIB, PASSWORD_PASSES)
        .expect("fixed Argon2 parameters are valid");
    format!(
        "argon2id${PASSWORD_MEMORY_KIB}${PASSWORD_PASSES}${}${}",
        hex(&salt),
        hex(&hash)
    )
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    let [kind, memory, passes, salt, hash] = parts.as_slice() else {
        return false;
    };
    let (Ok(memory), Ok(passes), Some(salt), Some(hash)) = (
        memory.parse::<u32>(),
        passes.parse::<u32>(),
        unhex(salt),
        unhex(hash),
    ) else {
        return false;
    };
    // Settings below these are refused, so an edited file cannot make
    // guessing cheap without also replacing the hash.
    if *kind != "argon2id" || memory < 8 * 1024 || !(1..=16).contains(&passes) || memory > 1 << 21 {
        return false;
    }
    argon2(password, &salt, memory, passes).is_some_and(|computed| equal(&computed, &hash))
}

/// Compares in constant time.
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A password that is hard to guess, for resets by an administrator.
pub fn random_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    random_bytes::<18>()
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect::<String>()
        .as_bytes()
        .chunks(6)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

// --- one-time codes (RFC 6238) ---------------------------------------------------

const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn unbase32(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let value = BASE32
            .iter()
            .position(|&b| b as char == c.to_ascii_uppercase())? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

pub fn new_totp_secret() -> String {
    base32(&random_bytes::<20>())
}

/// The 6-digit code for one 30-second step.
pub fn totp_code(secret: &[u8], step: u64) -> u32 {
    let mut mac = <Hmac<sha1::Sha1> as hmac::KeyInit>::new_from_slice(secret)
        .expect("HMAC accepts any key length");
    mac.update(&step.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[hash.len() - 1] & 0x0f) as usize;
    let value = u32::from_be_bytes([
        hash[offset] & 0x7f,
        hash[offset + 1],
        hash[offset + 2],
        hash[offset + 3],
    ]);
    value % 1_000_000
}

pub fn current_step() -> u64 {
    (Utc::now().timestamp().max(0) as u64) / 30
}

/// The step a code belongs to (this one, the one before or after, for clocks
/// that are a little off), or `None`.
pub fn check_totp(secret_base32: &str, code: &str, now_step: u64) -> Option<u64> {
    let code: u32 = code.trim().replace(' ', "").parse().ok()?;
    let secret = unbase32(secret_base32)?;
    [now_step.saturating_sub(1), now_step, now_step + 1]
        .into_iter()
        .find(|&step| totp_code(&secret, step) == code)
}

/// `otpauth://` link for authenticator apps.
pub fn totp_uri(secret: &str, user: &str) -> String {
    let issuer = format!("AeternaVault {}", crate::platform::computer_name());
    let encode = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    format!(
        "otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period=30",
        encode(&issuer),
        encode(user),
        encode(&issuer)
    )
}

// --- codes by e-mail -------------------------------------------------------------

/// How long a code sent by e-mail is valid.
pub const EMAIL_CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// Wrong codes before a sent code is void.
pub const EMAIL_CODE_TRIES: u32 = 5;

/// A code sent by e-mail and not used yet.
#[derive(Debug, Clone)]
pub struct EmailCode {
    pub code: String,
    /// The address it went to (when an address is being confirmed).
    pub address: String,
    pub sent: Instant,
    pub tries: u32,
}

impl EmailCode {
    pub fn new(address: &str) -> Self {
        let n = u32::from_be_bytes(random_bytes::<4>()) % 1_000_000;
        Self {
            code: format!("{n:06}"),
            address: address.to_string(),
            sent: Instant::now(),
            tries: 0,
        }
    }

    /// Checks a typed code; every wrong one counts, and an old or used-up
    /// code never matches.
    pub fn check(&mut self, typed: &str) -> bool {
        let typed: String = typed.chars().filter(|c| c.is_ascii_digit()).collect();
        if self.sent.elapsed() > EMAIL_CODE_TTL || self.tries >= EMAIL_CODE_TRIES {
            return false;
        }
        let ok = equal(typed.as_bytes(), self.code.as_bytes());
        if !ok {
            self.tries += 1;
        }
        ok
    }
}

/// `anna@example.org` → `a***@example.org`, for pages that say where a code went.
pub fn masked(address: &str) -> String {
    match address.split_once('@') {
        Some((local, domain)) => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".into(),
    }
}

pub fn plausible_email(address: &str) -> bool {
    let address = address.trim();
    address.len() <= 254
        && !address.contains(char::is_whitespace)
        && address.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')
        })
}

// --- sessions and sign-in limits -----------------------------------------------------

#[derive(Debug, Clone)]
pub struct Session {
    pub user: String,
    /// Sent with every form; a request from another site does not know it.
    pub csrf: String,
    pub ip: IpAddr,
    pub started: Instant,
    pub last_seen: Instant,
    /// Signed in with password but the one-time code is still missing.
    pub pending_code: bool,
}

/// A session ends after `idle` without requests, and after 12 hours at most.
const MAX_SESSION: Duration = Duration::from_secs(12 * 3600);

#[derive(Default)]
pub struct Sessions {
    sessions: HashMap<String, Session>,
}

impl Sessions {
    pub fn create(&mut self, user: &str, ip: IpAddr, pending_code: bool) -> (String, Session) {
        let token = hex(&random_bytes::<32>());
        let session = Session {
            user: user.to_string(),
            csrf: hex(&random_bytes::<16>()),
            ip,
            started: Instant::now(),
            last_seen: Instant::now(),
            pending_code,
        };
        self.sessions.insert(token.clone(), session.clone());
        (token, session)
    }

    /// The session for a cookie, if it is still valid; it counts as used now.
    pub fn touch(&mut self, token: &str, idle: Duration) -> Option<Session> {
        self.sessions
            .retain(|_, s| s.last_seen.elapsed() < idle && s.started.elapsed() < MAX_SESSION);
        let session = self.sessions.get_mut(token)?;
        session.last_seen = Instant::now();
        Some(session.clone())
    }

    pub fn finish_code(&mut self, token: &str) {
        if let Some(s) = self.sessions.get_mut(token) {
            s.pending_code = false;
        }
    }

    pub fn end(&mut self, token: &str) {
        self.sessions.remove(token);
    }

    /// Signs a user out everywhere (password changed, user disabled or removed).
    pub fn end_user(&mut self, user: &str) {
        self.sessions
            .retain(|_, s| !s.user.eq_ignore_ascii_case(user));
    }

    pub fn count(&self) -> usize {
        self.sessions.len()
    }

    /// Signed-in sessions (user, address, signed in for, idle for), for the
    /// live view of sign-ins.
    pub fn active(&self) -> Vec<(String, IpAddr, Duration, Duration)> {
        let mut list: Vec<_> = self
            .sessions
            .values()
            .filter(|s| !s.pending_code)
            .map(|s| {
                (
                    s.user.clone(),
                    s.ip,
                    s.started.elapsed(),
                    s.last_seen.elapsed(),
                )
            })
            .collect();
        list.sort_by_key(|(_, _, started, _)| *started);
        list
    }
}

/// Wrong sign-ins per address and per user name.
#[derive(Default)]
pub struct Limiter {
    failures: HashMap<String, (u32, Instant)>,
    /// Codes already used, so a code seen by someone else cannot be reused.
    used_codes: HashMap<String, u64>,
}

impl Limiter {
    /// Whether `key` (an address or a user name) must wait, and how long.
    pub fn locked(&mut self, key: &str, attempts: u32, wait: Duration) -> Option<Duration> {
        let (count, last) = *self.failures.get(key)?;
        if last.elapsed() >= wait {
            self.failures.remove(key);
            return None;
        }
        (attempts > 0 && count >= attempts).then(|| wait - last.elapsed())
    }

    pub fn failed(&mut self, key: &str) {
        let entry = self
            .failures
            .entry(key.to_string())
            .or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
    }

    pub fn succeeded(&mut self, key: &str) {
        self.failures.remove(key);
    }

    /// Accepts a code step once per user.
    pub fn use_code(&mut self, user: &str, step: u64) -> bool {
        let key = user.to_lowercase();
        if self.used_codes.get(&key).is_some_and(|&used| used >= step) {
            return false;
        }
        self.used_codes.insert(key, step);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_hashed_and_checked() {
        let stored = hash_password("correct horse battery");
        assert!(stored.starts_with("argon2id$"));
        assert!(!stored.contains("horse"));
        assert!(verify_password("correct horse battery", &stored));
        assert!(!verify_password("correct horse batterY", &stored));
        assert!(
            !verify_password("x", "argon2id$8$1$00$00"),
            "weak settings refused"
        );
        assert!(!verify_password("x", "garbage"));
    }

    /// Test vector of RFC 6238 (SHA-1, T = 59 s): 94287082, last 6 digits.
    #[test]
    fn one_time_codes_match_rfc_6238() {
        let secret = b"12345678901234567890";
        assert_eq!(totp_code(secret, 59 / 30), 287_082);
        assert_eq!(totp_code(secret, 1_111_111_109 / 30), 81_804);
        let encoded = base32(secret);
        assert_eq!(encoded, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(unbase32(&encoded).unwrap(), secret);
        assert_eq!(check_totp(&encoded, "287082", 1), Some(1));
        assert_eq!(
            check_totp(&encoded, "287 082", 2),
            Some(1),
            "one step late is fine"
        );
        assert_eq!(check_totp(&encoded, "287082", 5), None);
    }

    #[test]
    fn roles_and_jobs_limit_what_a_user_may_do() {
        let mut store = UserStore::default();
        store
            .add("anna", Role::Operator, "long enough pass")
            .unwrap();
        assert!(store.add("Anna", Role::Viewer, "long enough pass").is_err());
        assert!(
            store.add("bob", Role::Viewer, "").is_err(),
            "only empty is refused"
        );
        store.add("carl", Role::Viewer, "short").unwrap();
        assert!(
            store
                .add("bad name", Role::Viewer, "long enough pass")
                .is_err()
        );
        let anna = store.find_mut("ANNA").unwrap();
        assert!(anna.may(Role::Operator) && !anna.may(Role::Admin));
        anna.jobs = vec!["nightly".into()];
        assert!(anna.may_use_job(Some("nightly")));
        assert!(!anna.may_use_job(Some("other")));
        assert!(!anna.may_use_job(None));
        anna.disabled = true;
        assert!(!anna.may(Role::Viewer));
    }

    #[test]
    fn email_codes_expire_and_allow_few_tries() {
        let mut code = EmailCode::new("anna@example.org");
        let right = code.code.clone();
        assert_eq!(right.len(), 6);
        assert!(!code.check("000000x") || right == "000000");
        assert!(code.check(&format!("{} {}", &right[..3], &right[3..])));
        let mut used = EmailCode::new("a@b.de");
        let right = used.code.clone();
        for _ in 0..EMAIL_CODE_TRIES {
            used.check("wrong");
        }
        assert!(!used.check(&right), "too many wrong codes void it");
        assert_eq!(masked("anna@example.org"), "a***@example.org");
        assert!(plausible_email("anna@example.org"));
        assert!(!plausible_email("anna@localhost") && !plausible_email("a b@x.de"));
    }

    #[test]
    fn second_factor_is_app_then_email() {
        let mut store = UserStore::default();
        store.add("anna", Role::Viewer, "pw").unwrap();
        let anna = store.find_mut("anna").unwrap();
        assert_eq!(anna.second_factor(true, true), None, "no way known yet");
        anna.email = "anna@example.org".into();
        assert_eq!(
            anna.second_factor(false, true),
            None,
            "not chosen, not required"
        );
        assert_eq!(anna.second_factor(true, true), Some(SecondFactor::Email));
        assert_eq!(anna.second_factor(true, false), None, "no e-mail channel");
        anna.totp = Some(new_totp_secret());
        assert_eq!(anna.second_factor(false, true), Some(SecondFactor::App));
    }

    #[test]
    fn short_passwords_get_advice_not_refused() {
        assert!(check_password("abc").is_ok());
        let advice = password_advice("abc", 12, true).unwrap();
        assert!(
            advice.contains("recommended 12") && advice.contains("accepted"),
            "{advice}"
        );
        assert!(password_advice("Tannenbaum-Kaffee-Wolke-73", 12, true).is_none());
        assert!(password_advice("", 12, true).is_none());
    }

    #[test]
    fn repeated_failures_lock_and_codes_are_used_once() {
        let mut limiter = Limiter::default();
        let wait = Duration::from_secs(60);
        for _ in 0..3 {
            assert!(limiter.locked("10.0.0.9", 3, wait).is_none());
            limiter.failed("10.0.0.9");
        }
        assert!(limiter.locked("10.0.0.9", 3, wait).is_some());
        limiter.succeeded("10.0.0.9");
        assert!(limiter.locked("10.0.0.9", 3, wait).is_none());
        assert!(limiter.use_code("anna", 100));
        assert!(!limiter.use_code("ANNA", 100));
        assert!(limiter.use_code("anna", 101));
    }
}
