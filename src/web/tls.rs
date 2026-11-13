//! Certificates for HTTPS: an own certificate from files, or one made by
//! AeternaVault on first use (self-signed, kept in `web-tls` next to the
//! configuration). Browsers warn about a self-signed certificate once; its
//! SHA-256 fingerprint, shown in the program, lets you check that the
//! warning is about this computer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
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
/// self-signed certificate if one is needed and none exists yet.
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
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&cert_file)
        .map_err(|e| format!("{}: {e}", cert_file.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert_file.display()))?;
    if certs.is_empty() {
        return Err(format!("{}: no certificate found", cert_file.display()));
    }
    let key = PrivateKeyDer::from_pem_file(&key_file)
        .map_err(|e| format!("{}: {e}", key_file.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate or key not usable: {e}"))?;
    Ok(Some(Arc::new(config)))
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
