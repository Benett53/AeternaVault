//! Sign-in with LDAP or Active Directory for the web interface.
//!
//! A user name and password are checked against the directory: the user is
//! found with a search (as a service account, anonymously, or as the user
//! themselves), the password is checked by binding as that user, and the
//! role comes from the user's groups. Users not in any of the configured
//! groups cannot sign in. Connections are encrypted (LDAPS or StartTLS)
//! unless explicitly allowed otherwise; certificates are checked against the
//! certificates trusted by the system and an optional own CA file.

use std::sync::Arc;
use std::time::Duration;

use ldap3::{LdapConn, LdapConnSettings, Scope, SearchEntry, dn_escape, ldap_escape};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde::{Deserialize, Serialize};

use super::users::Role;

/// Directory settings (part of the web settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LdapSettings {
    pub enabled: bool,
    /// `ldaps://dc1.example.local` or `ldap://ldap.example.local:389`.
    pub url: String,
    /// For `ldap://`: switch to TLS with StartTLS.
    pub starttls: bool,
    /// For `ldap://` without StartTLS: send passwords unencrypted (test setups only).
    pub allow_unencrypted: bool,
    /// An own certificate authority (PEM), in addition to the system's.
    pub ca_file: std::path::PathBuf,
    /// Account for the search. Empty: anonymous. With `{user}`: the user
    /// themselves, e.g. `{user}@example.local` (Active Directory).
    pub bind_dn: String,
    pub bind_password: String,
    /// Where users are searched, e.g. `DC=example,DC=local`.
    pub base_dn: String,
    /// Finds one user; `{user}` is the name typed at sign-in.
    pub user_filter: String,
    /// Members of these groups (full DN) get the role. Empty: not used.
    pub admin_group: String,
    pub operator_group: String,
    pub viewer_group: String,
    /// Active Directory: also count membership through nested groups.
    pub nested_groups: bool,
    pub timeout_seconds: u32,
}

pub const AD_FILTER: &str = "(&(objectClass=user)(sAMAccountName={user}))";
pub const OPENLDAP_FILTER: &str = "(&(objectClass=inetOrgPerson)(uid={user}))";

impl Default for LdapSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            starttls: false,
            allow_unencrypted: false,
            ca_file: std::path::PathBuf::new(),
            bind_dn: String::new(),
            bind_password: String::new(),
            base_dn: String::new(),
            user_filter: AD_FILTER.into(),
            admin_group: String::new(),
            operator_group: String::new(),
            viewer_group: String::new(),
            nested_groups: true,
            timeout_seconds: 10,
        }
    }
}

impl LdapSettings {
    fn encrypted(&self) -> bool {
        self.url.to_ascii_lowercase().starts_with("ldaps://") || self.starttls
    }

    /// Whether the search runs as the user signing in.
    pub fn binds_as_user(&self) -> bool {
        self.bind_dn.contains("{user}")
    }

    /// What is missing or unsafe; `Ok` if these settings can be used.
    pub fn check(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let url = self.url.trim().to_ascii_lowercase();
        if !(url.starts_with("ldap://") || url.starts_with("ldaps://")) {
            return Err("LDAP: the server address starts with ldaps:// or ldap://".into());
        }
        if !self.encrypted() && !self.allow_unencrypted {
            return Err(
                "LDAP: passwords would be sent unencrypted; use ldaps:// or StartTLS".into(),
            );
        }
        if self.base_dn.trim().is_empty() {
            return Err("LDAP: enter where users are searched (base DN)".into());
        }
        if !self.user_filter.contains("{user}") {
            return Err("LDAP: the user filter must contain {user}".into());
        }
        if [&self.admin_group, &self.operator_group, &self.viewer_group]
            .iter()
            .all(|g| g.trim().is_empty())
        {
            return Err("LDAP: name at least one group that gives a role".into());
        }
        Ok(())
    }
}

/// A user the directory accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryUser {
    pub dn: String,
    pub role: Role,
    /// The `mail` attribute, for sign-in codes by e-mail.
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LdapError {
    /// Wrong name or password, or not found.
    Rejected,
    /// Signed in, but in none of the groups.
    NoGroup,
    /// The directory could not be asked (connection, certificate, settings).
    Unavailable(String),
}

impl std::fmt::Display for LdapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LdapError::Rejected => write!(f, "user name or password not accepted by the directory"),
            LdapError::NoGroup => write!(f, "not in any group that gives a role"),
            LdapError::Unavailable(e) => write!(f, "directory not reachable: {e}"),
        }
    }
}

fn tls_config(settings: &LdapSettings) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if !settings.ca_file.as_os_str().is_empty() {
        let certs = CertificateDer::pem_file_iter(&settings.ca_file)
            .map_err(|e| format!("{}: {e}", settings.ca_file.display()))?;
        for cert in certs {
            roots
                .add(cert.map_err(|e| format!("{}: {e}", settings.ca_file.display()))?)
                .map_err(|e| format!("{}: {e}", settings.ca_file.display()))?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn connect(settings: &LdapSettings) -> Result<LdapConn, LdapError> {
    let timeout = Duration::from_secs(u64::from(settings.timeout_seconds.clamp(2, 120)));
    let config = tls_config(settings).map_err(LdapError::Unavailable)?;
    let conn_settings = LdapConnSettings::new()
        .set_conn_timeout(timeout)
        .set_starttls(
            settings.starttls && !settings.url.to_ascii_lowercase().starts_with("ldaps://"),
        )
        .set_config(config);
    let mut conn = LdapConn::with_settings(conn_settings, settings.url.trim())
        .map_err(|e| LdapError::Unavailable(e.to_string()))?;
    conn.with_timeout(timeout);
    Ok(conn)
}

/// The text typed as user name, checked before it goes into a filter or DN.
fn plausible_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '*' | '(' | ')' | '\\' | '\0'))
}

/// Checks a user name and password against the directory.
pub fn authenticate(
    settings: &LdapSettings,
    name: &str,
    password: &str,
) -> Result<DirectoryUser, LdapError> {
    // An empty password is an anonymous bind in LDAP, which succeeds: never
    // let it count as a sign-in.
    if password.is_empty() || !plausible_name(name) {
        return Err(LdapError::Rejected);
    }
    settings.check().map_err(LdapError::Unavailable)?;
    let mut conn = connect(settings)?;
    let result = sign_in(&mut conn, settings, name, password);
    let _ = conn.unbind();
    result
}

fn sign_in(
    conn: &mut LdapConn,
    settings: &LdapSettings,
    name: &str,
    password: &str,
) -> Result<DirectoryUser, LdapError> {
    let unavailable = |e: ldap3::LdapError| LdapError::Unavailable(e.to_string());
    // 1. Who searches.
    if settings.binds_as_user() {
        // In a DN (`CN={user},OU=…`) the name is escaped; in `{user}@domain`
        // or `DOMAIN\{user}` it is used as typed.
        let value = if settings.bind_dn.contains('=') {
            dn_escape(name)
        } else {
            name.into()
        };
        let dn = settings.bind_dn.replace("{user}", &value);
        if conn
            .simple_bind(&dn, password)
            .map_err(unavailable)?
            .success()
            .is_err()
        {
            return Err(LdapError::Rejected);
        }
    } else if !settings.bind_dn.trim().is_empty() {
        conn.simple_bind(settings.bind_dn.trim(), &settings.bind_password)
            .map_err(unavailable)?
            .success()
            .map_err(|e| {
                LdapError::Unavailable(format!("the search account was not accepted: {e}"))
            })?;
    }

    // 2. Find exactly one user.
    let filter = settings.user_filter.replace("{user}", &ldap_escape(name));
    let (entries, _) = conn
        .search(
            settings.base_dn.trim(),
            Scope::Subtree,
            &filter,
            vec!["memberOf", "mail"],
        )
        .map_err(unavailable)?
        .success()
        .map_err(unavailable)?;
    let mut entries: Vec<SearchEntry> = entries.into_iter().map(SearchEntry::construct).collect();
    // Referrals and other entries without a DN do not count.
    entries.retain(|e| !e.dn.is_empty());
    if entries.len() != 1 {
        return Err(LdapError::Rejected);
    }
    let user = entries.remove(0);

    // 3. The role, while still bound as the searching account.
    let member_of: Vec<String> = user
        .attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("memberOf"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let mut role = None;
    for (group, r) in [
        (&settings.viewer_group, Role::Viewer),
        (&settings.operator_group, Role::Operator),
        (&settings.admin_group, Role::Admin),
    ] {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        let direct = member_of
            .iter()
            .any(|g| g.trim().eq_ignore_ascii_case(group));
        let member = direct
            || (settings.nested_groups && in_chain(conn, &user.dn, group).map_err(unavailable)?);
        if member {
            role = Some(r);
        }
    }

    // 4. The password, by binding as the user (unless that was step 1).
    if !settings.binds_as_user()
        && conn
            .simple_bind(&user.dn, password)
            .map_err(unavailable)?
            .success()
            .is_err()
    {
        return Err(LdapError::Rejected);
    }
    let role = role.ok_or(LdapError::NoGroup)?;
    let email = user
        .attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("mail"))
        .and_then(|(_, v)| v.first().cloned())
        .filter(|m| super::users::plausible_email(m));
    Ok(DirectoryUser {
        dn: user.dn,
        role,
        email,
    })
}

/// Active Directory: whether `user_dn` is in `group`, also through nested groups.
fn in_chain(conn: &mut LdapConn, user_dn: &str, group: &str) -> ldap3::result::Result<bool> {
    let filter = format!("(memberOf:1.2.840.113556.1.4.1941:={})", ldap_escape(group));
    let (entries, _) = conn
        .search(user_dn, Scope::Base, &filter, vec!["1.1"])?
        .success()?;
    Ok(!entries.is_empty())
}

/// For the "Test" button: connects, signs in the search account and counts
/// what the filter finds for `name` (or checks the given user completely).
pub fn test(settings: &LdapSettings, name: &str, password: &str) -> Result<String, String> {
    if !password.is_empty() {
        return authenticate(settings, name, password)
            .map(|u| format!("signed in as {} with the role {}", u.dn, u.role.name(true)))
            .map_err(|e| e.to_string());
    }
    settings.check()?;
    if settings.binds_as_user() {
        return Err(
            "with {user} in the search account, enter a user name and password to test".into(),
        );
    }
    let mut conn = connect(settings).map_err(|e| e.to_string())?;
    if !settings.bind_dn.trim().is_empty() {
        conn.simple_bind(settings.bind_dn.trim(), &settings.bind_password)
            .and_then(|r| r.success())
            .map_err(|e| format!("the search account was not accepted: {e}"))?;
    }
    let who = if name.trim().is_empty() {
        "*"
    } else {
        name.trim()
    };
    let filter = if who == "*" {
        settings.user_filter.replace("{user}", "*")
    } else {
        settings.user_filter.replace("{user}", &ldap_escape(who))
    };
    let found = conn
        .search(
            settings.base_dn.trim(),
            Scope::Subtree,
            &filter,
            vec!["1.1"],
        )
        .and_then(|r| r.success())
        .map(|(entries, _)| entries.len())
        .map_err(|e| e.to_string());
    let _ = conn.unbind();
    found.map(|n| format!("connected; the filter finds {n} users"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_settings_and_input_are_refused() {
        let mut s = LdapSettings {
            enabled: true,
            url: "ldap://dc.example.local".into(),
            base_dn: "DC=example,DC=local".into(),
            admin_group: "CN=Backup Admins,DC=example,DC=local".into(),
            ..LdapSettings::default()
        };
        assert!(s.check().unwrap_err().contains("unencrypted"));
        s.starttls = true;
        assert!(s.check().is_ok());
        s.url = "ldaps://dc.example.local".into();
        s.starttls = false;
        assert!(s.check().is_ok());
        s.admin_group.clear();
        assert!(s.check().is_err());

        assert!(!plausible_name("a*"));
        assert!(!plausible_name("x)(uid=*"));
        assert!(plausible_name("anna.mueller@example.local"));
        // An empty password never reaches the directory.
        assert_eq!(authenticate(&s, "anna", ""), Err(LdapError::Rejected));
    }
}
