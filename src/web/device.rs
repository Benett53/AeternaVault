//! Which device a request came from, as precisely as the network allows:
//! its address, its name (reverse DNS) and its MAC address (from the
//! neighbour table of this computer, so only for devices in the same network
//! segment; behind a router only the router's would be known, so none is
//! shown). A request from this computer itself is shown with the computer's
//! network address, name and MAC instead of 127.0.0.1.
//!
//! Lookups are kept for ten minutes.

use std::collections::HashMap;
use std::net::IpAddr;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Device {
    /// The address other computers know this device by.
    pub ip: String,
    pub name: Option<String>,
    pub mac: Option<String>,
    /// The request came from this computer itself.
    pub this_computer: bool,
}

impl Device {
    /// `192.168.1.23 · pc-anna.example.local · 3c:7c:3f:12:34:56`.
    pub fn label(&self) -> String {
        let mut parts = vec![self.ip.clone()];
        if let Some(name) = &self.name {
            parts.push(name.clone());
        }
        if let Some(mac) = &self.mac {
            parts.push(mac.clone());
        }
        parts.join(" · ")
    }
}

const KEEP: Duration = Duration::from_secs(600);
static CACHE: Mutex<Option<HashMap<IpAddr, (Instant, Device)>>> = Mutex::new(None);

/// The device behind an address (cached).
pub fn identify(ip: IpAddr) -> Device {
    let ip = super::http::canonical(ip);
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, device)) = cache.as_ref().and_then(|c| c.get(&ip))
            && at.elapsed() < KEEP
        {
            return device.clone();
        }
    }
    let device = lookup(ip);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    map.retain(|_, (at, _)| at.elapsed() < KEEP);
    map.insert(ip, (Instant::now(), device.clone()));
    device
}

/// Short form for logs and the audit log.
pub fn describe(ip: IpAddr) -> String {
    identify(ip).label()
}

fn lookup(ip: IpAddr) -> Device {
    if ip.is_loopback() {
        let own = super::tls::primary_address();
        return Device {
            ip: own.map_or_else(|| ip.to_string(), |a| a.to_string()),
            name: Some(crate::platform::computer_name()),
            mac: own.and_then(own_mac),
            this_computer: true,
        };
    }
    Device {
        ip: ip.to_string(),
        name: reverse_dns(ip),
        mac: neighbour_mac(ip),
        this_computer: false,
    }
}

/// `AA-BB-CC-DD-EE-FF` or `aa:bb:…` → `aa:bb:cc:dd:ee:ff`; `None` for
/// empty or all-zero addresses.
fn normalise_mac(text: &str) -> Option<String> {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 || hex.chars().all(|c| c == '0') {
        return None;
    }
    let lower = hex.to_ascii_lowercase();
    Some(
        lower
            .as_bytes()
            .chunks(2)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn quiet(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up.
        command.creation_flags(0x0800_0000);
    }
    command
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = quiet(Command::new(program).args(args)).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The MAC address of a device in the same network, from the neighbour
/// (ARP) table this computer already has; nothing is sent.
fn neighbour_mac(ip: IpAddr) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let IpAddr::V4(v4) = ip
            && let Ok(table) = std::fs::read_to_string("/proc/net/arp")
        {
            return parse_proc_arp(&table, &v4.to_string());
        }
        // IPv6 neighbours.
        let text = run("ip", &["neigh", "show", &ip.to_string()])?;
        return text
            .split_whitespace()
            .skip_while(|w| *w != "lladdr")
            .nth(1)
            .and_then(normalise_mac);
    }
    #[cfg(windows)]
    {
        let text = run("arp", &["-a", &ip.to_string()])?;
        return parse_windows_arp(&text, &ip.to_string());
    }
    #[allow(unreachable_code)]
    {
        let _ = ip;
        None
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_arp(table: &str, ip: &str) -> Option<String> {
    table
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.first() == Some(&ip))
        .and_then(|f| f.get(3).and_then(|m| normalise_mac(m)))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn parse_windows_arp(text: &str, ip: &str) -> Option<String> {
    text.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|f| f.first() == Some(&ip))
        .and_then(|f| f.get(1).and_then(|m| normalise_mac(m)))
}

/// The MAC address of this computer's network card that has `ip`.
fn own_mac(ip: IpAddr) -> Option<String> {
    #[cfg(unix)]
    {
        let name = interface_of(ip)?;
        let text = std::fs::read_to_string(format!("/sys/class/net/{name}/address")).ok()?;
        return normalise_mac(text.trim());
    }
    #[cfg(windows)]
    {
        let script = format!("(Get-NetIPAddress -IPAddress '{ip}' | Get-NetAdapter).MacAddress");
        let text = run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
        )?;
        return normalise_mac(text.trim());
    }
    #[allow(unreachable_code)]
    {
        let _ = ip;
        None
    }
}

/// The name of the network interface that has `ip` (Linux).
#[cfg(unix)]
fn interface_of(ip: IpAddr) -> Option<String> {
    // SAFETY: getifaddrs fills a linked list that is only read here and
    // freed with freeifaddrs before returning.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut entry = list;
        while !entry.is_null() {
            let item = &*entry;
            if !item.ifa_addr.is_null() {
                let family = i32::from((*item.ifa_addr).sa_family);
                let address = match family {
                    libc::AF_INET => {
                        let a = &*(item.ifa_addr as *const libc::sockaddr_in);
                        Some(IpAddr::from(u32::from_be(a.sin_addr.s_addr).to_be_bytes()))
                    }
                    libc::AF_INET6 => {
                        let a = &*(item.ifa_addr as *const libc::sockaddr_in6);
                        Some(IpAddr::from(a.sin6_addr.s6_addr))
                    }
                    _ => None,
                };
                if address == Some(ip) {
                    found = Some(
                        std::ffi::CStr::from_ptr(item.ifa_name)
                            .to_string_lossy()
                            .into_owned(),
                    );
                    break;
                }
            }
            entry = item.ifa_next;
        }
        libc::freeifaddrs(list);
        found
    }
}

/// The name a DNS server knows for an address, if any.
fn reverse_dns(ip: IpAddr) -> Option<String> {
    #[cfg(unix)]
    {
        use std::net::SocketAddr;
        let addr = SocketAddr::new(ip, 0);
        let mut host = [0 as libc::c_char; 256];
        // SAFETY: the socket address is built from a valid SocketAddr with
        // the matching length; the buffer is large enough and NUL-terminated
        // by getnameinfo on success.
        let result = unsafe {
            match addr {
                SocketAddr::V4(v4) => {
                    let mut sa: libc::sockaddr_in = std::mem::zeroed();
                    sa.sin_family = libc::AF_INET as libc::sa_family_t;
                    sa.sin_addr.s_addr = u32::from_ne_bytes(v4.ip().octets());
                    libc::getnameinfo(
                        &sa as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                        host.as_mut_ptr(),
                        host.len() as libc::socklen_t,
                        std::ptr::null_mut(),
                        0,
                        libc::NI_NAMEREQD,
                    )
                }
                SocketAddr::V6(v6) => {
                    let mut sa: libc::sockaddr_in6 = std::mem::zeroed();
                    sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                    sa.sin6_addr.s6_addr = v6.ip().octets();
                    libc::getnameinfo(
                        &sa as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                        host.as_mut_ptr(),
                        host.len() as libc::socklen_t,
                        std::ptr::null_mut(),
                        0,
                        libc::NI_NAMEREQD,
                    )
                }
            }
        };
        if result != 0 {
            return None;
        }
        // SAFETY: getnameinfo succeeded, so `host` holds a NUL-terminated name.
        let name = unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        return (!name.is_empty() && name != ip.to_string()).then_some(name);
    }
    #[cfg(windows)]
    {
        let script =
            format!("try {{ [System.Net.Dns]::GetHostEntry('{ip}').HostName }} catch {{ }}");
        let text = run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
        )?;
        let name = text.trim().to_string();
        return (!name.is_empty() && name != ip.to_string()).then_some(name);
    }
    #[allow(unreachable_code)]
    {
        let _ = ip;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_addresses_are_read_from_neighbour_tables() {
        let linux = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.168.1.23     0x1         0x2         3c:7c:3f:12:34:56     *        enp4s0\n\
                     192.168.1.99     0x1         0x0         00:00:00:00:00:00     *        enp4s0\n";
        assert_eq!(
            parse_proc_arp(linux, "192.168.1.23").as_deref(),
            Some("3c:7c:3f:12:34:56")
        );
        assert_eq!(
            parse_proc_arp(linux, "192.168.1.99"),
            None,
            "incomplete entry"
        );
        assert_eq!(parse_proc_arp(linux, "192.168.1.2"), None);

        let windows = "\nInterface: 192.168.1.10 --- 0x5\n  Internet Address      Physical Address      Type\n  192.168.1.23          3C-7C-3F-12-34-56     dynamic\n";
        assert_eq!(
            parse_windows_arp(windows, "192.168.1.23").as_deref(),
            Some("3c:7c:3f:12:34:56")
        );
    }

    #[test]
    fn this_computer_is_shown_with_its_network_address() {
        let device = identify("127.0.0.1".parse().unwrap());
        assert!(device.this_computer);
        assert_eq!(
            device.name.as_deref(),
            Some(crate::platform::computer_name().as_str())
        );
        if let Some(own) = super::super::tls::primary_address() {
            assert_eq!(device.ip, own.to_string());
        }
        assert!(device.label().starts_with(&device.ip));
    }
}
