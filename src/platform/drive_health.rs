//! Health of the drive a folder is on (S.M.A.R.T.), read from what the
//! system already reports: UDisks under Linux (no root needed), `smartctl`
//! as a fallback, and the Storage module of PowerShell under Windows.
//! Network shares have no drive here; their health is known on the server.
//!
//! Reading takes a moment (external programs), so the window and the web
//! interface use [`cached`], which reads in the background.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The overall verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Good,
    /// Still working, but showing wear or errors.
    Warning,
    /// The drive reports that it is failing: copy the backups elsewhere.
    Failing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveHealth {
    pub model: String,
    pub status: Status,
    pub temperature_c: Option<i64>,
    pub power_on_hours: Option<u64>,
    /// What led to a warning or failure, in English.
    pub findings: Vec<String>,
    /// Where the values came from: `udisks`, `smartctl` or `windows`.
    pub source: String,
}

impl DriveHealth {
    /// One line: `Good · 46 °C · 708 h`.
    pub fn summary(&self, en: bool) -> String {
        let mut parts = vec![self.status_name(en).to_string()];
        if let Some(t) = self.temperature_c {
            parts.push(format!("{t} °C"));
        }
        if let Some(h) = self.power_on_hours {
            parts.push(format!("{h} h"));
        }
        parts.join(" · ")
    }

    pub fn status_name(&self, en: bool) -> &'static str {
        match (self.status, en) {
            (Status::Good, true) => "Good",
            (Status::Good, false) => "Gut",
            (Status::Warning, true) => "Warning",
            (Status::Warning, false) => "Warnung",
            (Status::Failing, true) => "Failing",
            (Status::Failing, false) => "Fällt aus",
        }
    }
}

/// Why no health could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// A network share: the drive is in another computer.
    NetworkShare,
    /// The folder does not exist (drive not connected).
    Missing,
    /// The system does not report it (no UDisks, no rights, virtual disk …).
    NotReported(String),
}

impl Unavailable {
    pub fn text(&self, en: bool) -> String {
        match (self, en) {
            (Self::NetworkShare, true) => "Network share: known on the server".into(),
            (Self::NetworkShare, false) => "Netzwerkfreigabe: am Server bekannt".into(),
            (Self::Missing, true) => "Drive not connected".into(),
            (Self::Missing, false) => "Laufwerk nicht verbunden".into(),
            (Self::NotReported(why), true) => format!("Not reported by the system: {why}"),
            (Self::NotReported(why), false) => format!("Vom System nicht gemeldet: {why}"),
        }
    }
}

pub type Reading = Result<DriveHealth, Unavailable>;

fn quiet(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up.
        command.creation_flags(0x0800_0000);
    }
    command
}

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = quiet(Command::new(program).args(args))
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() || !text.trim().is_empty() {
        Ok(text)
    } else {
        Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Reads the health of the drive `path` is on. Blocking (runs programs).
pub fn read(path: &Path) -> Reading {
    if !path.exists() {
        return Err(Unavailable::Missing);
    }
    read_platform(path)
}

#[cfg(target_os = "linux")]
fn read_platform(path: &Path) -> Reading {
    let disk = linux_disk(path)?;
    match udisks(&disk) {
        Ok(health) => Ok(health),
        Err(first) => {
            smartctl(&disk).map_err(|second| Unavailable::NotReported(format!("{first}; {second}")))
        }
    }
}

/// The whole disk (`/dev/nvme0n1`, `/dev/sda`) a folder is on.
#[cfg(target_os = "linux")]
fn linux_disk(path: &Path) -> Result<String, Unavailable> {
    let path = path.to_string_lossy();
    let line = output("findmnt", &["-n", "-o", "SOURCE,FSTYPE", "--target", &path])
        .map_err(Unavailable::NotReported)?;
    let mut fields = line.split_whitespace();
    let source = fields.next().unwrap_or_default();
    let fstype = fields.next().unwrap_or_default();
    if is_network_fs(fstype) || source.starts_with("//") || source.contains(":/") {
        return Err(Unavailable::NetworkShare);
    }
    // btrfs subvolumes: `/dev/nvme1n1p3[/@home]`.
    let device = source.split('[').next().unwrap_or_default();
    if !device.starts_with("/dev/") {
        return Err(Unavailable::NotReported(format!("{source} is no drive")));
    }
    // Walk up from a partition, LVM or LUKS device to the disk.
    let tree = output(
        "lsblk",
        &["-s", "-n", "-p", "-r", "-o", "NAME,TYPE", device],
    )
    .map_err(Unavailable::NotReported)?;
    tree.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let name = f.next()?;
            (f.next()? == "disk").then(|| name.to_string())
        })
        .next()
        .ok_or_else(|| Unavailable::NotReported(format!("no disk found for {device}")))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_network_fs(fstype: &str) -> bool {
    matches!(
        fstype,
        "cifs" | "smb3" | "smbfs" | "nfs" | "nfs4" | "9p" | "afs" | "ceph" | "glusterfs"
    ) || fstype.starts_with("fuse.sshfs")
        || fstype.starts_with("fuse.rclone")
}

/// `Key: value` lines of `udisksctl info`, by interface.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn udisks_fields(text: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.trim().split_once(':')
            && !key.contains('.')
            && !key.starts_with('/')
        {
            fields
                .entry(key.trim().to_string())
                .or_insert_with(|| value.trim().trim_matches('\'').to_string());
        }
    }
    fields
}

#[cfg(target_os = "linux")]
fn udisks(disk: &str) -> Result<DriveHealth, String> {
    let block = output("udisksctl", &["info", "-b", disk])?;
    let drive = udisks_fields(&block)
        .get("Drive")
        .map(|d| d.rsplit('/').next().unwrap_or_default().to_string())
        .filter(|d| !d.is_empty())
        .ok_or("udisks knows no drive for it")?;
    let info = output("udisksctl", &["info", "-d", &drive])?;
    parse_udisks(&info)
}

/// The health from `udisksctl info -d`: ATA (`Drive.Ata`) or NVMe.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_udisks(text: &str) -> Result<DriveHealth, String> {
    let f = udisks_fields(text);
    let get = |k: &str| f.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let number = |k: &str| get(k).and_then(|v| v.parse::<f64>().ok());
    let kelvin = |k: &str| {
        number(k)
            .filter(|t| *t > 0.0)
            .map(|t| (t - 273.15).round() as i64)
    };
    let model = get("Model").unwrap_or("?").to_string();
    let mut findings = Vec::new();
    let (mut status, temperature_c, power_on_hours) =
        if text.contains("org.freedesktop.UDisks2.NVMe.Controller") {
            let warnings: Vec<&str> = get("SmartCriticalWarning")
                .map(|w| {
                    w.split([',', ' '])
                        .map(|s| s.trim_matches(|c| c == '[' || c == ']' || c == '\''))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            for w in &warnings {
                findings.push(format!("critical warning: {w}"));
            }
            let status = if warnings
                .iter()
                .any(|w| matches!(*w, "degraded" | "readonly" | "spare" | "volatile_mem"))
            {
                Status::Failing
            } else if warnings.is_empty() {
                Status::Good
            } else {
                Status::Warning
            };
            (
                status,
                kelvin("SmartTemperature"),
                number("SmartPowerOnHours").map(|h| h as u64),
            )
        } else if text.contains("org.freedesktop.UDisks2.Drive.Ata") {
            if get("SmartSupported") == Some("false") {
                return Err("the drive has no S.M.A.R.T.".into());
            }
            if get("SmartEnabled") == Some("false") {
                return Err("S.M.A.R.T. is switched off on the drive".into());
            }
            let failing = get("SmartFailing") == Some("true");
            let bad_sectors = number("SmartNumBadSectors").unwrap_or(0.0) as u64;
            let failing_now = number("SmartNumAttributesFailing").unwrap_or(0.0) as u64;
            let failed_before = number("SmartNumAttributesFailedInThePast").unwrap_or(0.0) as u64;
            if failing {
                findings.push("the drive predicts its failure".into());
            }
            if failing_now > 0 {
                findings.push(format!("{failing_now} attributes below threshold"));
            }
            if bad_sectors > 0 {
                findings.push(format!("{bad_sectors} bad sectors"));
            }
            if failed_before > 0 {
                findings.push(format!("{failed_before} attributes failed in the past"));
            }
            let status = if failing || failing_now > 0 {
                Status::Failing
            } else if bad_sectors > 0 || failed_before > 0 {
                Status::Warning
            } else {
                Status::Good
            };
            (
                status,
                kelvin("SmartTemperature"),
                number("SmartPowerOnSeconds").map(|s| (s / 3600.0) as u64),
            )
        } else {
            return Err("udisks reports no S.M.A.R.T. values for this drive".into());
        };
    if let Some(selftest) = get("SmartSelftestStatus")
        && ["fatal", "error", "electrical", "servo", "read", "handling"]
            .iter()
            .any(|p| selftest.starts_with(p))
    {
        findings.push(format!("last self-test: {selftest}"));
        status = status.max(Status::Warning);
    }
    Ok(DriveHealth {
        model,
        status,
        temperature_c,
        power_on_hours,
        findings,
        source: "udisks".into(),
    })
}

#[cfg(target_os = "linux")]
fn smartctl(disk: &str) -> Result<DriveHealth, String> {
    let text = output("smartctl", &["-j", "-H", "-A", "-i", disk])?;
    parse_smartctl(&text)
}

/// The health from `smartctl -j -H -A -i`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_smartctl(text: &str) -> Result<DriveHealth, String> {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Some(passed) = json["smart_status"]["passed"].as_bool() else {
        let message = json["smartctl"]["messages"][0]["string"]
            .as_str()
            .unwrap_or("smartctl reports no health")
            .to_string();
        return Err(message);
    };
    let mut findings = Vec::new();
    let mut status = if passed {
        Status::Good
    } else {
        findings.push("the drive predicts its failure".into());
        Status::Failing
    };
    for attribute in json["ata_smart_attributes"]["table"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let id = attribute["id"].as_u64().unwrap_or(0);
        let raw = attribute["raw"]["value"].as_u64().unwrap_or(0);
        let name = attribute["name"].as_str().unwrap_or("?");
        if matches!(id, 5 | 187 | 197 | 198) && raw > 0 {
            findings.push(format!("{name}: {raw}"));
            status = status.max(Status::Warning);
        }
    }
    let nvme = &json["nvme_smart_health_information_log"];
    if let Some(warning) = nvme["critical_warning"].as_u64()
        && warning != 0
    {
        findings.push(format!("critical warning 0x{warning:02x}"));
        status = status.max(Status::Warning);
    }
    if let Some(used) = nvme["percentage_used"].as_u64()
        && used >= 90
    {
        findings.push(format!("{used} % of the rated wear used"));
        status = status.max(Status::Warning);
    }
    Ok(DriveHealth {
        model: json["model_name"].as_str().unwrap_or("?").to_string(),
        status,
        temperature_c: json["temperature"]["current"].as_i64(),
        power_on_hours: json["power_on_time"]["hours"].as_u64(),
        findings,
        source: "smartctl".into(),
    })
}

#[cfg(windows)]
fn read_platform(path: &Path) -> Reading {
    let text = path.to_string_lossy();
    if text.starts_with(r"\\") && !text.starts_with(r"\\?\") {
        return Err(Unavailable::NetworkShare);
    }
    let letter = text
        .trim_start_matches(r"\\?\")
        .chars()
        .next()
        .filter(char::is_ascii_alphabetic)
        .ok_or_else(|| Unavailable::NotReported("no drive letter".into()))?;
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $v = Get-Volume -DriveLetter {letter}; \
         if ($v.DriveType -eq 'Network') {{ 'NETWORK'; exit }}; \
         $d = Get-Partition -DriveLetter {letter} | Get-Disk; \
         $p = Get-PhysicalDisk | Where-Object {{ $_.DeviceId -eq [string]$d.Number }} | Select-Object -First 1; \
         $r = $null; try {{ $r = $p | Get-StorageReliabilityCounter }} catch {{}}; \
         [pscustomobject]@{{ Model = $p.FriendlyName; Health = [string]$p.HealthStatus; \
           Operational = (@($p.OperationalStatus) | ForEach-Object {{ [string]$_ }}) -join ','; \
           Temperature = $r.Temperature; Hours = $r.PowerOnHours; \
           ReadErrors = $r.ReadErrorsUncorrected; Wear = $r.Wear }} | ConvertTo-Json -Compress"
    );
    let out = output(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )
    .map_err(Unavailable::NotReported)?;
    if out.trim() == "NETWORK" {
        return Err(Unavailable::NetworkShare);
    }
    parse_windows(&out).map_err(Unavailable::NotReported)
}

/// The health from `Get-PhysicalDisk` and `Get-StorageReliabilityCounter`.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_windows(text: &str) -> Result<DriveHealth, String> {
    let json: serde_json::Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("{e}: {}", text.trim()))?;
    let health = json["Health"].as_str().unwrap_or_default();
    let operational = json["Operational"].as_str().unwrap_or_default();
    let mut findings = Vec::new();
    let mut status = match health {
        "Healthy" => Status::Good,
        "Warning" => Status::Warning,
        "Unhealthy" => Status::Failing,
        "" => return Err("Windows reports no health for this drive".into()),
        other => {
            findings.push(format!("health: {other}"));
            Status::Warning
        }
    };
    if !operational.is_empty() && operational != "OK" {
        findings.push(format!("state: {operational}"));
        status = status.max(Status::Warning);
    }
    if let Some(errors) = json["ReadErrors"].as_u64()
        && errors > 0
    {
        findings.push(format!("{errors} uncorrected read errors"));
        status = status.max(Status::Warning);
    }
    if let Some(wear) = json["Wear"].as_u64()
        && wear >= 90
    {
        findings.push(format!("{wear} % of the rated wear used"));
        status = status.max(Status::Warning);
    }
    if status != Status::Good && findings.is_empty() {
        findings.push(format!("Windows reports: {health}"));
    }
    Ok(DriveHealth {
        model: json["Model"].as_str().unwrap_or("?").trim().to_string(),
        status,
        temperature_c: json["Temperature"].as_i64().filter(|t| *t > 0),
        power_on_hours: json["Hours"].as_u64(),
        findings,
        source: "windows".into(),
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn read_platform(_path: &Path) -> Reading {
    Err(Unavailable::NotReported(
        "not supported on this system".into(),
    ))
}

const KEEP: Duration = Duration::from_secs(600);

struct Entry {
    at: Option<Instant>,
    reading: Option<Reading>,
    reading_now: bool,
}

static CACHE: Mutex<Option<HashMap<PathBuf, Entry>>> = Mutex::new(None);

/// The last reading for `path` (`None` until the first one is done), read
/// again in the background every ten minutes. `repaint` is called when a
/// new reading arrives.
pub fn cached(path: &Path, repaint: impl FnOnce() + Send + 'static) -> Option<Reading> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    let entry = map.entry(path.to_path_buf()).or_insert(Entry {
        at: None,
        reading: None,
        reading_now: false,
    });
    let stale = entry.at.is_none_or(|at| at.elapsed() >= KEEP);
    if stale && !entry.reading_now {
        entry.reading_now = true;
        let path = path.to_path_buf();
        let _ = std::thread::Builder::new()
            .name("aeterna-drive-health".into())
            .spawn(move || {
                let reading = read(&path);
                let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = cache.as_mut().and_then(|m| m.get_mut(&path)) {
                    entry.reading = Some(reading);
                    entry.at = Some(Instant::now());
                    entry.reading_now = false;
                }
                drop(cache);
                repaint();
            });
    }
    entry.reading.clone()
}

/// Makes the next [`cached`] call read again (a "Check now" button).
pub fn refresh(path: &Path) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = cache.as_mut().and_then(|m| m.get_mut(path))
        && !entry.reading_now
    {
        entry.at = None;
    }
}

/// When the last reading of `path` was made.
pub fn read_at(path: &Path) -> Option<Instant> {
    let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.as_ref()?.get(path)?.at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvme_from_udisks() {
        let text = "/org/freedesktop/UDisks2/drives/Samsung_SSD:\n  org.freedesktop.UDisks2.Drive:\n    Model:                      Samsung SSD 980 PRO 1TB\n  org.freedesktop.UDisks2.NVMe.Controller:\n    SmartCriticalWarning:               \n    SmartPowerOnHours:                  708\n    SmartSelftestStatus:                success\n    SmartTemperature:                   319\n";
        let health = parse_udisks(text).unwrap();
        assert_eq!(health.model, "Samsung SSD 980 PRO 1TB");
        assert_eq!(health.status, Status::Good);
        assert_eq!(health.temperature_c, Some(46));
        assert_eq!(health.power_on_hours, Some(708));
        assert_eq!(health.summary(true), "Good · 46 °C · 708 h");

        let worn = text.replace(
            "SmartCriticalWarning:               ",
            "SmartCriticalWarning:               ['spare', 'temperature']",
        );
        let health = parse_udisks(&worn).unwrap();
        assert_eq!(health.status, Status::Failing);
        assert_eq!(health.findings.len(), 2);
    }

    #[test]
    fn ata_from_udisks() {
        let text = "  org.freedesktop.UDisks2.Drive:\n    Model: WDC WD40EFRX\n  org.freedesktop.UDisks2.Drive.Ata:\n    SmartEnabled: true\n    SmartFailing: false\n    SmartNumAttributesFailedInThePast: 0\n    SmartNumAttributesFailing: 0\n    SmartNumBadSectors: 0\n    SmartPowerOnSeconds: 36000000\n    SmartSupported: true\n    SmartTemperature: 308.15\n";
        let health = parse_udisks(text).unwrap();
        assert_eq!(health.status, Status::Good);
        assert_eq!(health.temperature_c, Some(35));
        assert_eq!(health.power_on_hours, Some(10_000));

        let bad = text.replace("SmartNumBadSectors: 0", "SmartNumBadSectors: 8");
        assert_eq!(parse_udisks(&bad).unwrap().status, Status::Warning);
        let failing = text.replace("SmartFailing: false", "SmartFailing: true");
        assert_eq!(parse_udisks(&failing).unwrap().status, Status::Failing);
        let off = text.replace("SmartEnabled: true", "SmartEnabled: false");
        assert!(parse_udisks(&off).is_err());
        assert!(parse_udisks("  org.freedesktop.UDisks2.Drive:\n    Model: USB stick\n").is_err());
    }

    #[test]
    fn smartctl_json() {
        let text = r#"{"model_name":"ST4000","smart_status":{"passed":true},"temperature":{"current":31},"power_on_time":{"hours":1200},
            "ata_smart_attributes":{"table":[{"id":5,"name":"Reallocated_Sector_Ct","raw":{"value":12}},{"id":9,"name":"Power_On_Hours","raw":{"value":1200}}]}}"#;
        let health = parse_smartctl(text).unwrap();
        assert_eq!(health.status, Status::Warning);
        assert_eq!(health.findings, vec!["Reallocated_Sector_Ct: 12"]);
        assert_eq!(health.temperature_c, Some(31));
        let denied = r#"{"smartctl":{"messages":[{"string":"Smartctl open device: /dev/sda failed: Permission denied"}]}}"#;
        assert!(
            parse_smartctl(denied)
                .unwrap_err()
                .contains("Permission denied")
        );
    }

    #[test]
    fn windows_storage_module() {
        let good = r#"{"Model":"Samsung SSD 870 ","Health":"Healthy","Operational":"OK","Temperature":38,"Hours":5000,"ReadErrors":0,"Wear":3}"#;
        let health = parse_windows(good).unwrap();
        assert_eq!(health.status, Status::Good);
        assert_eq!(health.model, "Samsung SSD 870");
        let bad = r#"{"Model":"X","Health":"Unhealthy","Operational":"Predictive Failure","Temperature":null,"Hours":null,"ReadErrors":null,"Wear":null}"#;
        let health = parse_windows(bad).unwrap();
        assert_eq!(health.status, Status::Failing);
        assert_eq!(health.findings, vec!["state: Predictive Failure"]);
    }

    #[test]
    fn missing_folders_and_shares() {
        assert_eq!(
            read(Path::new("/no/such/folder")),
            Err(Unavailable::Missing)
        );
        assert!(is_network_fs("cifs"));
        assert!(is_network_fs("fuse.sshfs"));
        assert!(!is_network_fs("ext4"));
    }
}
