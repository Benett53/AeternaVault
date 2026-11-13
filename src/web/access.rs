//! Who may reach the web interface besides users and passwords: blocked
//! addresses (kept in `web-blocked.json` next to the configuration and read
//! again when the file changes, so blocking needs no restart), automatic
//! blocking after many wrong sign-ins, countries (with a country database the
//! user downloads), and where administrators may work from.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::Network;
use super::http::canonical;

pub const BLOCKED_FILE: &str = "web-blocked.json";

/// One blocked address or network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocked {
    /// `203.0.113.7` or `203.0.113.0/24`.
    pub address: String,
    #[serde(default)]
    pub reason: String,
    pub since: DateTime<Utc>,
    /// Who blocked it: `web:<name>`, `window:<user>`, or `automatic`.
    #[serde(default)]
    pub by: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocklist {
    #[serde(default)]
    pub entries: Vec<Blocked>,
}

impl Blocklist {
    pub fn path(config_file: &Path) -> PathBuf {
        config_file
            .parent()
            .map(|d| d.join(BLOCKED_FILE))
            .unwrap_or_else(|| PathBuf::from(BLOCKED_FILE))
    }

    pub fn load(config_file: &Path) -> Self {
        let path = Self::path(config_file);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!("{} is not readable: {err}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, config_file: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        crate::platform::write_private_file(&Self::path(config_file), text.as_bytes())
    }

    /// Adds an address or network. `Ok(false)`: it was blocked already.
    pub fn add(&mut self, address: &str, reason: &str, by: &str) -> Result<bool, String> {
        let address = address.trim();
        let network: Network = address.parse()?;
        if network.contains(IpAddr::from([127, 0, 0, 1]))
            || network.contains(IpAddr::from(std::net::Ipv6Addr::LOCALHOST))
        {
            return Err("this computer itself cannot be blocked".into());
        }
        if self.entries.iter().any(|e| e.address == address) {
            return Ok(false);
        }
        self.entries.push(Blocked {
            address: address.to_string(),
            reason: reason.trim().to_string(),
            since: Utc::now(),
            by: by.to_string(),
        });
        Ok(true)
    }

    /// Removes an address. `false`: it was not blocked.
    pub fn remove(&mut self, address: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.address != address.trim());
        self.entries.len() != before
    }

    /// The entry that blocks `ip`, if any.
    pub fn blocking(&self, ip: IpAddr) -> Option<&Blocked> {
        self.entries
            .iter()
            .find(|e| e.address.parse::<Network>().is_ok_and(|n| n.contains(ip)))
    }
}

/// The block list as the server uses it: read again when the file changed,
/// checked for that at most every two seconds.
#[derive(Default)]
pub struct BlockCache {
    modified: Option<SystemTime>,
    checked: Option<Instant>,
    networks: Vec<Network>,
}

impl BlockCache {
    pub fn blocks(&mut self, config_file: &Path, ip: IpAddr) -> bool {
        if self
            .checked
            .is_none_or(|c| c.elapsed() >= Duration::from_secs(2))
        {
            self.checked = Some(Instant::now());
            let modified = std::fs::metadata(Blocklist::path(config_file))
                .and_then(|m| m.modified())
                .ok();
            if modified != self.modified {
                self.modified = modified;
                self.networks = Blocklist::load(config_file)
                    .entries
                    .iter()
                    .filter_map(|e| e.address.parse().ok())
                    .collect();
            }
        }
        self.networks.iter().any(|n| n.contains(ip))
    }

    /// Makes the next check read the file (after a change by this process).
    pub fn invalidate(&mut self) {
        self.checked = None;
        self.modified = None;
    }
}

/// The address at the start of a device label (`203.0.113.7 · name · mac`)
/// if it may be blocked: not this computer (`own`: its network address).
pub fn blockable(source: &str, own: Option<IpAddr>) -> Option<IpAddr> {
    let ip: IpAddr = source.split(" · ").next()?.trim().parse().ok()?;
    let ip = canonical(ip);
    (!ip.is_loopback() && Some(ip) != own).then_some(ip)
}

/// Wrong sign-ins per address over the last 24 hours, for automatic blocking.
#[derive(Default)]
pub struct FailureLog {
    failures: HashMap<IpAddr, Vec<Instant>>,
}

impl FailureLog {
    const WINDOW: Duration = Duration::from_secs(24 * 3600);

    /// Counts a wrong sign-in; returns how many there were in 24 hours.
    pub fn failed(&mut self, ip: IpAddr) -> usize {
        let ip = canonical(ip);
        let list = self.failures.entry(ip).or_default();
        list.retain(|t| t.elapsed() < Self::WINDOW);
        list.push(Instant::now());
        let count = list.len();
        // Keep the map small when many addresses try once.
        if self.failures.len() > 10_000 {
            self.failures.retain(|_, l| {
                l.retain(|t| t.elapsed() < Self::WINDOW);
                !l.is_empty()
            });
        }
        count
    }

    pub fn forget(&mut self, ip: IpAddr) {
        self.failures.remove(&canonical(ip));
    }
}

/// Whether an address is on this computer or in a private network, where a
/// country means nothing.
pub fn is_local(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Countries that may connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum GeoMode {
    /// Every country.
    #[default]
    Off,
    /// Only the listed countries.
    Allow,
    /// Every country except the listed ones.
    Block,
}

/// Geoblocking: a country database (`.mmdb`, MaxMind GeoLite2 Country or
/// DB-IP IP to Country Lite) and the countries by ISO code (`DE`, `AT`).
/// Addresses of this computer and of private networks are never checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeoSettings {
    pub mode: GeoMode,
    pub database: PathBuf,
    pub countries: Vec<String>,
}

impl GeoSettings {
    /// The countries as upper-case codes; an error names a wrong entry.
    pub fn codes(&self) -> Result<Vec<String>, String> {
        self.countries
            .iter()
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty())
            .map(|c| {
                if c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()) {
                    Ok(c)
                } else {
                    Err(format!(
                        "{c} is not a country code (two letters, e.g. DE or AT)"
                    ))
                }
            })
            .collect()
    }
}

/// An opened country database with the rule.
pub struct Geo {
    reader: maxminddb::Reader<Vec<u8>>,
    mode: GeoMode,
    countries: Vec<String>,
}

#[derive(Deserialize)]
struct Record<'a> {
    #[serde(borrow, default)]
    country: Option<Code<'a>>,
}

#[derive(Deserialize)]
struct Code<'a> {
    #[serde(borrow, default)]
    iso_code: Option<&'a str>,
}

impl Geo {
    /// Opens the database. `Ok(None)` when geoblocking is off.
    pub fn open(settings: &GeoSettings) -> Result<Option<Self>, String> {
        if settings.mode == GeoMode::Off {
            return Ok(None);
        }
        let countries = settings.codes()?;
        if countries.is_empty() {
            return Err("geoblocking: list at least one country".into());
        }
        if settings.database.as_os_str().is_empty() {
            return Err("geoblocking: choose the country database (.mmdb)".into());
        }
        let reader = maxminddb::Reader::open_readfile(&settings.database)
            .map_err(|e| format!("{}: {e}", settings.database.display()))?;
        Ok(Some(Self {
            reader,
            mode: settings.mode,
            countries,
        }))
    }

    /// The ISO code of the country of `ip`, if the database knows it.
    pub fn country(&self, ip: IpAddr) -> Option<String> {
        let found = self.reader.lookup(canonical(ip)).ok()?;
        let record: Record<'_> = found.decode().ok()??;
        record.country?.iso_code.map(|c| c.to_ascii_uppercase())
    }

    /// Whether `ip` may connect. An address the database does not know
    /// counts as an unlisted country.
    pub fn allows(&self, ip: IpAddr) -> bool {
        if is_local(ip) {
            return true;
        }
        let listed = self
            .country(ip)
            .is_some_and(|c| self.countries.contains(&c));
        match self.mode {
            GeoMode::Off => true,
            GeoMode::Allow => listed,
            GeoMode::Block => !listed,
        }
    }
}

/// Where administrators may work from. Elsewhere they sign in with what the
/// other permissions of their role allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum AdminAccess {
    #[default]
    Anywhere,
    /// Only from this computer.
    ThisComputer,
    /// From this computer and the admin networks.
    Networks,
}

/// Whether administrative permissions apply for a request from `ip`.
pub fn admin_allowed(access: AdminAccess, networks: &[Network], ip: IpAddr) -> bool {
    let ip = canonical(ip);
    match access {
        AdminAccess::Anywhere => true,
        AdminAccess::ThisComputer => ip.is_loopback(),
        AdminAccess::Networks => ip.is_loopback() || networks.iter().any(|n| n.contains(ip)),
    }
}

/// Who needs a second factor (2FA) after the password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MfaPolicy {
    /// Each user decides.
    #[default]
    Optional,
    /// Users who may manage users or block addresses.
    Administrators,
    Everyone,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_list_matches_addresses_and_networks() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let mut list = Blocklist::default();
        assert!(
            list.add("203.0.113.7", "tries passwords", "automatic")
                .unwrap()
        );
        assert!(
            !list.add("203.0.113.7", "", "web:anna").unwrap(),
            "only once"
        );
        assert!(list.add("198.51.100.0/24", "", "web:anna").unwrap());
        assert!(list.add("127.0.0.1", "", "x").is_err(), "not this computer");
        assert!(list.add("0.0.0.0/0", "", "x").is_err(), "not everything");
        assert!(list.add("nonsense", "", "x").is_err());
        list.save(&config).unwrap();

        let mut cache = BlockCache::default();
        assert!(cache.blocks(&config, "203.0.113.7".parse().unwrap()));
        assert!(cache.blocks(&config, "::ffff:198.51.100.9".parse().unwrap()));
        assert!(!cache.blocks(&config, "203.0.113.8".parse().unwrap()));

        let mut list = Blocklist::load(&config);
        assert!(list.remove("203.0.113.7"));
        assert!(!list.remove("203.0.113.7"));
        list.save(&config).unwrap();
        cache.invalidate();
        assert!(!cache.blocks(&config, "203.0.113.7".parse().unwrap()));
    }

    #[test]
    fn failures_are_counted_per_address() {
        let mut log = FailureLog::default();
        let a: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(log.failed(a), 1);
        assert_eq!(log.failed("::ffff:203.0.113.7".parse().unwrap()), 2);
        assert_eq!(log.failed("203.0.113.8".parse().unwrap()), 1);
        log.forget(a);
        assert_eq!(log.failed(a), 1);
    }

    #[test]
    fn administrators_only_where_allowed() {
        let lan: Vec<Network> = vec!["192.168.1.0/24".parse().unwrap()];
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        let office: IpAddr = "192.168.1.20".parse().unwrap();
        let away: IpAddr = "203.0.113.7".parse().unwrap();
        assert!(admin_allowed(AdminAccess::Anywhere, &[], away));
        assert!(admin_allowed(AdminAccess::ThisComputer, &lan, local));
        assert!(!admin_allowed(AdminAccess::ThisComputer, &lan, office));
        assert!(admin_allowed(AdminAccess::Networks, &lan, office));
        assert!(!admin_allowed(AdminAccess::Networks, &lan, away));
    }

    #[test]
    fn countries_need_codes_and_a_database() {
        let mut geo = GeoSettings {
            mode: GeoMode::Allow,
            ..GeoSettings::default()
        };
        assert!(Geo::open(&geo).is_err(), "no countries");
        geo.countries = vec!["de".into(), " at ".into()];
        assert_eq!(geo.codes().unwrap(), vec!["DE", "AT"]);
        assert!(Geo::open(&geo).is_err(), "no database");
        geo.database = "/no/such/file.mmdb".into();
        assert!(Geo::open(&geo).is_err());
        geo.countries = vec!["Germany".into()];
        assert!(geo.codes().is_err());
        geo.mode = GeoMode::Off;
        assert!(Geo::open(&geo).unwrap().is_none());
        assert!(is_local("10.1.2.3".parse().unwrap()));
        assert!(is_local("fd00::1".parse().unwrap()));
        assert!(!is_local("8.8.8.8".parse().unwrap()));
    }

    /// A country database by hand (MaxMind DB format 2.0, IPv4, 24-bit
    /// records): 203.0.113.0/24 is in `DE`, nothing else is known.
    fn tiny_database(path: &Path) {
        let net: u32 = u32::from(std::net::Ipv4Addr::new(203, 0, 113, 0));
        let nodes = 24u32;
        let mut file = Vec::new();
        for i in 0..nodes {
            let bit = (net >> (31 - i)) & 1;
            let next = if i + 1 < nodes { i + 1 } else { nodes + 16 };
            let (left, right) = if bit == 0 {
                (next, nodes)
            } else {
                (nodes, next)
            };
            file.extend_from_slice(&left.to_be_bytes()[1..]);
            file.extend_from_slice(&right.to_be_bytes()[1..]);
        }
        file.extend_from_slice(&[0; 16]);
        let string = |out: &mut Vec<u8>, s: &str| {
            out.push(0x40 | s.len() as u8);
            out.extend_from_slice(s.as_bytes());
        };
        // {"country": {"iso_code": "DE"}}
        file.push(0xE1);
        string(&mut file, "country");
        file.push(0xE1);
        string(&mut file, "iso_code");
        string(&mut file, "DE");
        file.extend_from_slice(b"\xAB\xCD\xEFMaxMind.com");
        file.push(0xE9);
        string(&mut file, "node_count");
        file.extend_from_slice(&[0xC1, nodes as u8]);
        string(&mut file, "record_size");
        file.extend_from_slice(&[0xA1, 24]);
        string(&mut file, "ip_version");
        file.extend_from_slice(&[0xA1, 4]);
        string(&mut file, "database_type");
        string(&mut file, "Test-Country");
        string(&mut file, "languages");
        file.extend_from_slice(&[0x00, 0x04]);
        string(&mut file, "binary_format_major_version");
        file.extend_from_slice(&[0xA1, 2]);
        string(&mut file, "binary_format_minor_version");
        file.push(0xA0);
        string(&mut file, "build_epoch");
        file.extend_from_slice(&[0x00, 0x02]);
        string(&mut file, "description");
        file.push(0xE0);
        std::fs::write(path, file).unwrap();
    }

    #[test]
    fn countries_are_allowed_or_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("country.mmdb");
        tiny_database(&database);
        let german: IpAddr = "203.0.113.50".parse().unwrap();
        let unknown: IpAddr = "8.8.8.8".parse().unwrap();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();

        let mut settings = GeoSettings {
            mode: GeoMode::Allow,
            database,
            countries: vec!["DE".into(), "AT".into()],
        };
        let geo = Geo::open(&settings).unwrap().unwrap();
        assert_eq!(geo.country(german).as_deref(), Some("DE"));
        assert_eq!(geo.country(unknown), None);
        assert!(geo.allows(german));
        assert!(geo.allows("::ffff:203.0.113.50".parse().unwrap()));
        assert!(!geo.allows(unknown), "unknown counts as not listed");
        assert!(geo.allows(lan), "private networks are never checked");

        settings.mode = GeoMode::Block;
        settings.countries = vec!["de".into()];
        let geo = Geo::open(&settings).unwrap().unwrap();
        assert!(!geo.allows(german));
        assert!(geo.allows(unknown));
    }
}
