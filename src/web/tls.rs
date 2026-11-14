//! HTTPS with own certificate files (an internal PKI, Let's Encrypt). By
//! default the web interface speaks plain HTTP like home-server software;
//! HTTPS for the network or the internet comes from a reverse proxy in
//! front or from these files. AeternaVault makes no certificates itself.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};

use crate::config::WebSettings;

/// The certificate and key files (`None` without HTTPS of its own).
pub fn files(settings: &WebSettings) -> Option<(PathBuf, PathBuf)> {
    settings
        .uses_tls()
        .then(|| (settings.cert_file.clone(), settings.key_file.clone()))
}

/// The TLS setup for the server, or `None` for plain HTTP. The files are
/// read again when they change (a renewal by certbot, win-acme or a company
/// CA needs no restart).
pub fn server_config(settings: &WebSettings) -> Result<Option<Arc<rustls::ServerConfig>>, String> {
    let Some((cert_file, key_file)) = files(settings) else {
        return Ok(None);
    };
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

/// Details of the certificate in use (`None`: no HTTPS of its own, or no
/// file chosen yet).
pub fn info(settings: &WebSettings) -> Option<Result<CertInfo, String>> {
    let (cert_file, _) = files(settings)?;
    if cert_file.as_os_str().is_empty() {
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
pub fn fingerprint(settings: &WebSettings) -> Option<String> {
    info(settings)?.ok().map(|i| i.fingerprint)
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
pub(crate) mod tests {
    use super::*;

    /// A certificate for `localhost`, as an own PKI would issue it.
    pub fn test_certificate(cert_file: &Path, key_file: &Path) {
        let mut params =
            rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "AeternaVault test");
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        std::fs::write(key_file, key.serialize_pem()).unwrap();
        std::fs::write(cert_file, cert.pem()).unwrap();
    }

    #[test]
    fn renewed_certificate_files_are_used_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        test_certificate(&cert, &key);
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
        test_certificate(&cert, &key);
        let second = reloading.current(Duration::ZERO).cert[0].clone();
        assert_ne!(second, first, "the new certificate is served");

        let info = read_info(&cert).unwrap();
        assert!(info.self_signed);
        assert_eq!(info.subject, "AeternaVault test");
        assert!(info.names.iter().any(|n| n == "localhost"));
        assert!(info.names.iter().any(|n| n == "127.0.0.1"));
        assert!(info.days_left() > 300);
        assert_eq!(info.fingerprint.len(), 32 * 3 - 1);
    }
}
