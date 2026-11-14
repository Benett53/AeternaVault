//! Certificates for HTTPS: an own certificate from files, or one made by
//! AeternaVault on first use (self-signed, kept in `web-tls` next to the
//! configuration). Browsers warn about a self-signed certificate once; its
//! SHA-256 fingerprint, shown in the program, lets you check that the
//! warning is about this computer.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};

use crate::config::{TlsMode, WebSettings};

pub const TLS_DIR: &str = "web-tls";

pub fn dir(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .map(|d| d.join(TLS_DIR))
        .unwrap_or_else(|| PathBuf::from(TLS_DIR))
}

/// The certificate and key files in use for these settings.
pub fn files(config_file: &Path, settings: &WebSettings) -> (PathBuf, PathBuf) {
    match settings.tls {
        TlsMode::Files => (settings.cert_file.clone(), settings.key_file.clone()),
        _ => {
            let dir = dir(config_file);
            (dir.join("cert.pem"), dir.join("key.pem"))
        }
    }
}

/// The TLS setup for the server, or `None` for plain HTTP. Makes a
/// self-signed certificate if one is needed and none exists yet. Own
/// certificate files are read again when they change (a renewal by certbot,
/// win-acme or a company CA needs no restart).
pub fn server_config(
    config_file: &Path,
    settings: &WebSettings,
) -> Result<Option<Arc<rustls::ServerConfig>>, String> {
    if !settings.uses_tls() {
        return Ok(None);
    }
    let (cert_file, key_file) = files(config_file, settings);
    if settings.tls != TlsMode::Files && !(cert_file.is_file() && key_file.is_file()) {
        create_self_signed(&cert_file, &key_file)?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let resolver = Reloading::new(cert_file, key_file, provider.clone())?;
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver));
    Ok(Some(Arc::new(config)))
}

fn load_key(
    cert_file: &Path,
    key_file: &Path,
    provider: &rustls::crypto::CryptoProvider,
) -> Result<CertifiedKey, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_file)
        .map_err(|e| format!("{}: {e}", cert_file.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert_file.display()))?;
    if certs.is_empty() {
        return Err(format!("{}: no certificate found", cert_file.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key_file)
        .map_err(|e| format!("{}: {e}", key_file.display()))?;
    CertifiedKey::from_der(certs, key, provider)
        .map_err(|e| format!("certificate or key not usable: {e}"))
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The certificate in use, read again when its files change (checked at most
/// once a minute). A renewal that cannot be read keeps the previous one.
struct Reloading {
    cert_file: PathBuf,
    key_file: PathBuf,
    provider: Arc<rustls::crypto::CryptoProvider>,
    state: Mutex<ReloadState>,
}

struct ReloadState {
    key: Arc<CertifiedKey>,
    stamp: (Option<SystemTime>, Option<SystemTime>),
    checked: Instant,
}

impl std::fmt::Debug for Reloading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reloading")
            .field("cert_file", &self.cert_file)
            .finish_non_exhaustive()
    }
}

impl Reloading {
    const EVERY: Duration = Duration::from_secs(60);

    fn new(
        cert_file: PathBuf,
        key_file: PathBuf,
        provider: Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<Self, String> {
        let key = load_key(&cert_file, &key_file, &provider)?;
        let stamp = (modified(&cert_file), modified(&key_file));
        Ok(Self {
            cert_file,
            key_file,
            provider,
            state: Mutex::new(ReloadState {
                key: Arc::new(key),
                stamp,
                checked: Instant::now(),
            }),
        })
    }

    fn current(&self, every: Duration) -> Arc<CertifiedKey> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.checked.elapsed() >= every {
            state.checked = Instant::now();
            let stamp = (modified(&self.cert_file), modified(&self.key_file));
            if stamp != state.stamp {
                match load_key(&self.cert_file, &self.key_file, &self.provider) {
                    Ok(key) => {
                        state.key = Arc::new(key);
                        state.stamp = stamp;
                        tracing::info!(
                            "web interface: certificate {} read again",
                            self.cert_file.display()
                        );
                    }
                    // Files may be half written during a renewal: try again
                    // at the next check, keep serving the previous one.
                    Err(err) => tracing::warn!("web interface: new certificate not used: {err}"),
                }
            }
        }
        state.key.clone()
    }
}

impl rustls::server::ResolvesServerCert for Reloading {
    fn resolve(&self, _hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current(Self::EVERY))
    }
}

/// What a certificate says about itself, as certificate dialogs show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Common name of the subject.
    pub subject: String,
    pub issuer: String,
    /// Host names and addresses it is valid for.
    pub names: Vec<String>,
    pub not_before: chrono::DateTime<chrono::Utc>,
    pub not_after: chrono::DateTime<chrono::Utc>,
    pub self_signed: bool,
    pub fingerprint: String,
}

impl CertInfo {
    pub fn days_left(&self) -> i64 {
        (self.not_after - chrono::Utc::now()).num_days()
    }
}

fn common_name(name: &x509_parser::x509::X509Name<'_>) -> String {
    name.iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}

/// Details of the certificate in use (`None`: none made yet).
pub fn info(config_file: &Path, settings: &WebSettings) -> Option<Result<CertInfo, String>> {
    let (cert_file, _) = files(config_file, settings);
    if !cert_file.is_file() {
        return None;
    }
    Some(read_info(&cert_file))
}

fn read_info(cert_file: &Path) -> Result<CertInfo, String> {
    let der = CertificateDer::pem_file_iter(cert_file)
        .map_err(|e| format!("{}: {e}", cert_file.display()))?
        .next()
        .ok_or_else(|| format!("{}: no certificate found", cert_file.display()))?
        .map_err(|e| format!("{}: {e}", cert_file.display()))?;
    let (_, cert) = x509_parser::parse_x509_certificate(der.as_ref())
        .map_err(|e| format!("{}: {e}", cert_file.display()))?;
    let names = cert
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|san| {
            san.value
                .general_names
                .iter()
                .filter_map(|n| match n {
                    x509_parser::extensions::GeneralName::DNSName(d) => Some(d.to_string()),
                    x509_parser::extensions::GeneralName::IPAddress(ip) => match ip.len() {
                        4 => Some(
                            std::net::Ipv4Addr::from(<[u8; 4]>::try_from(*ip).ok()?).to_string(),
                        ),
                        16 => Some(
                            std::net::Ipv6Addr::from(<[u8; 16]>::try_from(*ip).ok()?).to_string(),
                        ),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let time = |t: &x509_parser::time::ASN1Time| {
        chrono::DateTime::from_timestamp(t.timestamp(), 0).unwrap_or_default()
    };
    let digest = Sha256::digest(der.as_ref());
    Ok(CertInfo {
        subject: common_name(cert.subject()),
        issuer: common_name(cert.issuer()),
        names,
        not_before: time(&cert.validity().not_before),
        not_after: time(&cert.validity().not_after),
        self_signed: cert.subject() == cert.issuer(),
        fingerprint: digest
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
    })
}

/// SHA-256 of the certificate as browsers show it (`AB:CD:…`).
pub fn fingerprint(config_file: &Path, settings: &WebSettings) -> Option<String> {
    let (cert_file, _) = files(config_file, settings);
    let cert = CertificateDer::pem_file_iter(&cert_file)
        .ok()?
        .next()?
        .ok()?;
    let digest = Sha256::digest(cert.as_ref());
    Some(
        digest
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// Removes the certificate made by AeternaVault; a new one is made at the
/// next start (for a new computer name or address).
pub fn renew(config_file: &Path) -> std::io::Result<()> {
    let dir = dir(config_file);
    for name in ["cert.pem", "key.pem"] {
        match std::fs::remove_file(dir.join(name)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    Ok(())
}

fn create_self_signed(cert_file: &Path, key_file: &Path) -> Result<(), String> {
    let computer = crate::platform::computer_name().to_lowercase();
    let mut names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if !computer.is_empty() {
        names.push(computer.clone());
    }
    if let Some(ip) = primary_address() {
        names.push(ip.to_string());
    }
    let mut params = rcgen::CertificateParams::new(names).map_err(|e| e.to_string())?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("AeternaVault {computer}"),
    );
    // Valid for ten years from the start of this year.
    let year = chrono::Datelike::year(&chrono::Utc::now());
    params.not_before = rcgen::date_time_ymd(year, 1, 1);
    params.not_after = rcgen::date_time_ymd(year + 10, 1, 1);
    let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    if let Some(parent) = cert_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::platform::write_private_file(key_file, key.serialize_pem().as_bytes())
        .map_err(|e| format!("{}: {e}", key_file.display()))?;
    std::fs::write(cert_file, cert.pem()).map_err(|e| format!("{}: {e}", cert_file.display()))?;
    tracing::info!("web interface: certificate made for {computer}");
    Ok(())
}

/// The address other computers most likely use to reach this one: the one
/// the system would send from to the internet (nothing is sent).
pub fn primary_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewed_certificate_files_are_used_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        create_self_signed(&cert, &key).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let reloading = Reloading::new(cert.clone(), key.clone(), provider).unwrap();
        let first = reloading.current(Duration::ZERO).cert[0].clone();
        assert_eq!(
            reloading.current(Duration::ZERO).cert[0],
            first,
            "unchanged"
        );

        // A half-written renewal keeps the previous certificate.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&cert, "-----BEGIN CERTIFICATE-----\n").unwrap();
        assert_eq!(reloading.current(Duration::ZERO).cert[0], first);

        std::fs::remove_file(&cert).unwrap();
        std::fs::remove_file(&key).unwrap();
        create_self_signed(&cert, &key).unwrap();
        let second = reloading.current(Duration::ZERO).cert[0].clone();
        assert_ne!(second, first, "the new certificate is served");

        let info = read_info(&cert).unwrap();
        assert!(info.self_signed);
        assert!(info.subject.starts_with("AeternaVault"));
        assert!(info.names.iter().any(|n| n == "localhost"));
        assert!(info.names.iter().any(|n| n == "127.0.0.1"));
        assert!(info.days_left() > 300);
        assert_eq!(info.fingerprint.len(), 32 * 3 - 1);
    }
}
