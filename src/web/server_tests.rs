//! The web interface end to end: a real server on this computer, requests
//! over TCP, with and without TLS.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;

use super::users::{self, UserStore};
use super::{LocalRunner, WebServer};
use crate::config::{TlsMode, WebSettings};
use crate::paths::AppPaths;

struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }

    fn cookie(&self) -> Option<String> {
        let value = self.header("set-cookie")?;
        Some(value.split(';').next()?.to_string())
    }

    fn csrf(&self) -> String {
        let at = self
            .body
            .find(r#"name="csrf" value=""#)
            .expect("a form with a token");
        let rest = &self.body[at + r#"name="csrf" value=""#.len()..];
        rest[..rest.find('"').unwrap()].to_string()
    }
}

fn parse(raw: &[u8]) -> Reply {
    let text = String::from_utf8_lossy(raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Reply {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

fn raw_request(method: &str, path: &str, host: &str, cookie: Option<&str>, body: &str) -> String {
    let cookie = cookie
        .map(|c| format!("Cookie: {c}\r\n"))
        .unwrap_or_default();
    format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{cookie}Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn send(port: u16, method: &str, path: &str, cookie: Option<&str>, body: &str) -> Reply {
    send_host(
        port,
        &format!("127.0.0.1:{port}"),
        method,
        path,
        cookie,
        body,
    )
}

fn send_host(
    port: u16,
    host: &str,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: &str,
) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(raw_request(method, path, host, cookie, body).as_bytes())
        .unwrap();
    let mut out = Vec::new();
    let _ = stream.read_to_end(&mut out);
    parse(&out)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn setup(settings: WebSettings) -> (tempfile::TempDir, AppPaths, WebServer, u16) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_file: tmp.path().join("config.toml"),
        log_dir: tmp.path().join("logs"),
        portable: false,
    };
    let mut store = UserStore::default();
    store.add("admin", "admin", "admin password 1").unwrap();
    store.add("viewer", "viewer", "viewer password 1").unwrap();
    store.save(&paths.config_file).unwrap();
    let port = free_port();
    // Plain HTTP for the tests: only this computer.
    let bind = if settings.bind == "0.0.0.0" {
        "127.0.0.1".to_string()
    } else {
        settings.bind.clone()
    };
    let settings = WebSettings {
        enabled: true,
        port,
        bind,
        ..settings
    };
    let server =
        WebServer::start(&paths, &settings, Arc::new(LocalRunner::new(paths.clone()))).unwrap();
    (tmp, paths, server, port)
}

fn sign_in(port: u16, user: &str, password: &str) -> String {
    let reply = send(
        port,
        "POST",
        "/login",
        None,
        &format!("user={user}&password={}", super::http::encode(password)),
    );
    assert_eq!(reply.status, 303, "{}", reply.body);
    reply.cookie().expect("a session cookie")
}

#[test]
fn sign_in_roles_and_forms_are_checked() {
    let (_tmp, paths, _server, port) = setup(WebSettings::default());

    // Not signed in: to the sign-in page.
    let reply = send(port, "GET", "/", None, "");
    assert_eq!(reply.status, 303);
    assert_eq!(reply.header("location").as_deref(), Some("/login"));
    let login = send(port, "GET", "/login", None, "");
    assert_eq!(login.status, 200);
    assert!(login.header("content-security-policy").is_some());

    // Wrong password: refused and written to the audit log.
    let wrong = send(port, "POST", "/login", None, "user=admin&password=nope");
    assert_eq!(wrong.status, 200);
    assert!(wrong.cookie().is_none());
    let log = crate::audit::load(&paths.config_file);
    assert!(
        log.iter()
            .any(|e| e.action == "sign-in" && !e.ok && e.actor == "web:admin")
    );

    let cookie = sign_in(port, "admin", "admin password 1");
    assert!(cookie.starts_with("av="), "{cookie}");
    let overview = send(port, "GET", "/", Some(&cookie), "");
    assert_eq!(overview.status, 200, "{}", overview.body);
    let csrf = overview.csrf();

    // A form without the token of the session is refused.
    let forged = send(port, "POST", "/jobs/run", Some(&cookie), "id=");
    assert_eq!(forged.status, 403);
    // From another site (Origin) as well, even with a token.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = format!("csrf={csrf}");
    let request = format!(
        "POST /jobs/cancel HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.example\r\nCookie: {cookie}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = stream.read_to_end(&mut out);
    assert_eq!(parse(&out).status, 403);

    // The administrator sees users and the audit log.
    assert_eq!(send(port, "GET", "/users", Some(&cookie), "").status, 200);
    let audit = send(port, "GET", "/audit", Some(&cookie), "");
    assert_eq!(audit.status, 200);
    assert!(audit.body.contains("web:admin"));

    // A viewer does not, and cannot run jobs.
    let viewer = sign_in(port, "viewer", "viewer password 1");
    assert_eq!(send(port, "GET", "/users", Some(&viewer), "").status, 403);
    assert_eq!(send(port, "GET", "/audit", Some(&viewer), "").status, 403);
    let page = send(port, "GET", "/", Some(&viewer), "");
    let token = page.csrf();
    let run = send(
        port,
        "POST",
        "/jobs/run",
        Some(&viewer),
        &format!("csrf={token}&id="),
    );
    assert_eq!(run.status, 403);

    // Signing out ends the session.
    let out = send(
        port,
        "POST",
        "/logout",
        Some(&viewer),
        &format!("csrf={token}"),
    );
    assert_eq!(out.status, 303);
    assert_eq!(send(port, "GET", "/", Some(&viewer), "").status, 303);

    // Requests naming another host are refused while only this computer may connect.
    let rebinding = send_host(port, "evil.example", "GET", "/login", None, "");
    assert_eq!(rebinding.status, 403);
}

#[test]
fn administrators_manage_users_and_the_last_one_stays() {
    let (_tmp, paths, _server, port) = setup(WebSettings::default());
    let cookie = sign_in(port, "admin", "admin password 1");
    let csrf = send(port, "GET", "/users", Some(&cookie), "").csrf();

    let add = send(
        port,
        "POST",
        "/users/add",
        Some(&cookie),
        &format!("csrf={csrf}&user=olga&password=olga+password+1&role=operator"),
    );
    assert_eq!(add.status, 303);
    let store = UserStore::load(&paths.config_file);
    let olga = store.find("olga").unwrap();
    assert_eq!(olga.role, "operator");
    assert!(
        olga.must_change_password,
        "a password set by someone else is changed first"
    );

    // A short password is a recommendation only: accepted with a note.
    let short = send(
        port,
        "POST",
        "/users/add",
        Some(&cookie),
        &format!("csrf={csrf}&user=kurt&password=abc&role=viewer"),
    );
    assert_eq!(short.status, 303);
    assert!(short.header("location").unwrap().contains("msg="));
    assert!(UserStore::load(&paths.config_file).find("kurt").is_some());

    // Olga must change her password before anything else.
    let olga_cookie = sign_in(port, "olga", "olga password 1");
    let page = send(port, "GET", "/", Some(&olga_cookie), "");
    assert_eq!(page.header("location").as_deref(), Some("/account"));

    // The only administrator cannot demote, lock or remove themselves.
    for change in ["do=save&role=viewer", "do=disable", "do=remove"] {
        let reply = send(
            port,
            "POST",
            "/users/change",
            Some(&cookie),
            &format!("csrf={csrf}&user=admin&{change}"),
        );
        assert_eq!(reply.status, 303);
        assert!(reply.header("location").unwrap().contains("msg="));
        assert!({
            let store = UserStore::load(&paths.config_file);
            store.can(
                store.find("admin").unwrap(),
                super::users::Permission::ManageUsers,
            )
        });
    }

    // Locking a user ends their access at the next request.
    let lock = send(
        port,
        "POST",
        "/users/change",
        Some(&cookie),
        &format!("csrf={csrf}&user=olga&do=disable"),
    );
    assert_eq!(lock.status, 303);
    assert_eq!(
        send(port, "GET", "/account", Some(&olga_cookie), "")
            .header("location")
            .as_deref(),
        Some("/login")
    );
    let log = crate::audit::load(&paths.config_file);
    assert!(
        log.iter()
            .any(|e| e.action == "user-added" && e.target == "olga" && e.actor == "web:admin")
    );
    assert!(
        log.iter()
            .any(|e| e.action == "user-locked" && e.target == "olga")
    );
}

#[test]
fn wrong_passwords_lock_the_name_for_a_while() {
    let settings = WebSettings {
        lockout_attempts: 3,
        ..WebSettings::default()
    };
    let (_tmp, _paths, _server, port) = setup(settings);
    for _ in 0..3 {
        let reply = send(port, "POST", "/login", None, "user=admin&password=wrong");
        assert_eq!(reply.status, 200);
    }
    // Even the right password waits now.
    let reply = send(
        port,
        "POST",
        "/login",
        None,
        "user=admin&password=admin+password+1",
    );
    assert_eq!(reply.status, 429);
    assert!(reply.cookie().is_none());
}

#[test]
fn one_time_codes_are_asked_after_the_password() {
    let (_tmp, paths, _server, port) = setup(WebSettings::default());
    let secret = users::new_totp_secret();
    let mut store = UserStore::load(&paths.config_file);
    store.find_mut("viewer").unwrap().totp = Some(secret.clone());
    store.save(&paths.config_file).unwrap();

    let cookie = sign_in(port, "viewer", "viewer password 1");
    // Password alone is not enough.
    let page = send(port, "GET", "/", Some(&cookie), "");
    assert_eq!(page.header("location").as_deref(), Some("/login/code"));
    let wrong = send(port, "POST", "/login/code", Some(&cookie), "code=000000");
    assert_eq!(wrong.status, 200);

    let step = users::current_step();
    let code = users::totp_code(&users::unbase32(&secret).unwrap(), step);
    let ok = send(
        port,
        "POST",
        "/login/code",
        Some(&cookie),
        &format!("code={code:06}"),
    );
    assert_eq!(ok.status, 303);
    assert_eq!(send(port, "GET", "/", Some(&cookie), "").status, 200);
}

#[test]
fn https_with_a_certificate_made_here() {
    let settings = WebSettings {
        tls: TlsMode::SelfSigned,
        ..WebSettings::default()
    };
    let (_tmp, paths, server, port) = setup(settings);
    assert!(server.url.starts_with("https://"));

    // Trust exactly the certificate the server made.
    let (cert_file, _) = super::tls::files(&paths.config_file, &server.settings);
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_file_iter(&cert_file).unwrap() {
        roots.add(cert.unwrap()).unwrap();
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let client = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
    let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut tls = rustls::StreamOwned::new(client, tcp);
    tls.write_all(raw_request("GET", "/login", &format!("localhost:{port}"), None, "").as_bytes())
        .unwrap();
    let mut out = Vec::new();
    let _ = tls.read_to_end(&mut out);
    let reply = parse(&out);
    assert_eq!(reply.status, 200);
    assert!(reply.header("strict-transport-security").is_some());

    // Plain HTTP on the HTTPS port gets no page.
    let plain = send(port, "GET", "/login", None, "");
    assert_ne!(plain.status, 200);
    assert!(super::tls::fingerprint(&paths.config_file, &server.settings).is_some());
}

/// A minimal SMTP receiver for one test: keeps the text of every mail.
fn fake_smtp() -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::BufRead;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mails = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = mails.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut writer = stream.try_clone().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let _ = writer.write_all(b"220 test ESMTP\r\n");
            let mut line = String::new();
            let mut data = None::<String>;
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if let Some(body) = data.as_mut() {
                    if line == ".\r\n" {
                        store.lock().unwrap().push(data.take().unwrap());
                        let _ = writer.write_all(b"250 queued\r\n");
                    } else {
                        body.push_str(&line);
                    }
                } else {
                    let upper = line.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") {
                        b"250-test\r\n250 OK\r\n"
                    } else if upper.starts_with("DATA") {
                        data = Some(String::new());
                        b"354 go on\r\n"
                    } else if upper.starts_with("QUIT") {
                        let _ = writer.write_all(b"221 bye\r\n");
                        break;
                    } else {
                        b"250 OK\r\n"
                    };
                    let _ = writer.write_all(reply);
                }
                line.clear();
            }
        }
    });
    (port, mails)
}

#[test]
fn codes_by_e_mail_when_a_second_step_is_required() {
    let settings = WebSettings {
        mfa: crate::web::access::MfaPolicy::Everyone,
        ..WebSettings::default()
    };
    let (_tmp, paths, _server, port) = setup(settings);
    let (smtp_port, mails) = fake_smtp();
    let mut config = crate::config::Config::default();
    config.notifications.channels.push(crate::notify::Channel {
        kind: crate::notify::ChannelKind::Email,
        enabled: true,
        smtp_host: "127.0.0.1".into(),
        smtp_port,
        smtp_security: crate::notify::SmtpSecurity::None,
        from: "backup@firma.local".into(),
        to: "team@firma.local".into(),
        ..Default::default()
    });
    config.save(&paths.config_file).unwrap();
    let mut store = UserStore::load(&paths.config_file);
    store.find_mut("viewer").unwrap().email = "viewer@firma.local".into();
    store.save(&paths.config_file).unwrap();

    let cookie = sign_in(port, "viewer", "viewer password 1");
    assert_eq!(
        send(port, "GET", "/", Some(&cookie), "")
            .header("location")
            .as_deref(),
        Some("/login/code"),
        "the password alone is not enough"
    );
    let page = send(port, "GET", "/login/code", Some(&cookie), "");
    assert!(page.body.contains("v***@firma.local"), "{}", page.body);

    let mail = mails
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("a code was mailed");
    assert!(mail.contains("viewer@firma.local"));
    let code: String = mail
        .split(|c: char| !c.is_ascii_digit())
        .find(|w| w.len() == 6)
        .expect("a six-digit code")
        .to_string();
    let wrong = send(port, "POST", "/login/code", Some(&cookie), "code=000000");
    assert_eq!(wrong.status, 200);
    let ok = send(
        port,
        "POST",
        "/login/code",
        Some(&cookie),
        &format!("code={code}"),
    );
    assert_eq!(ok.status, 303, "{}", ok.body);
    assert_eq!(send(port, "GET", "/", Some(&cookie), "").status, 200);
    // Used once only.
    let again = send(
        port,
        "POST",
        "/login/code",
        Some(&cookie),
        &format!("code={code}"),
    );
    assert_eq!(again.header("location").as_deref(), Some("/login"));

    let log = crate::audit::load(&paths.config_file);
    assert!(log.iter().any(|e| e.action == "sign-in-code-sent" && e.ok));
    assert!(
        log.iter()
            .any(|e| e.action == "sign-in" && e.details.iter().any(|d| d.contains("e-mail")))
    );

    // The administrator sees it live under Activity → Sign-ins.
    let admin = sign_in(port, "admin", "admin password 1");
    // Admin has no second step yet: required, so the account page comes first.
    assert_eq!(
        send(port, "GET", "/activity?tab=sign-ins", Some(&admin), "")
            .header("location")
            .as_deref(),
        Some("/account")
    );
}

#[test]
fn administrators_see_sign_ins_live() {
    let (_tmp, _paths, _server, port) = setup(WebSettings::default());
    let _ = send(port, "POST", "/login", None, "user=admin&password=wrong");
    let admin = sign_in(port, "admin", "admin password 1");
    let page = send(port, "GET", "/activity?tab=sign-ins", Some(&admin), "");
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("http-equiv=\"refresh\""),
        "refreshes itself"
    );
    assert!(page.body.contains("wrong password"), "{}", page.body);
    let viewer = sign_in(port, "viewer", "viewer password 1");
    let page = send(port, "GET", "/activity?tab=sign-ins", Some(&viewer), "");
    assert!(
        !page.body.contains("wrong password"),
        "viewers do not see sign-ins"
    );
}

/// Requests as if they came from other computers (the test itself can only
/// connect from this one), straight to the request handler.
mod from_elsewhere {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::sync::Arc;

    use super::super::access::{AdminAccess, Blocklist};
    use super::super::app::WebApp;
    use super::super::http::{Request, Response};
    use super::*;

    fn app(settings: WebSettings) -> (tempfile::TempDir, AppPaths, WebApp) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths {
            config_file: tmp.path().join("config.toml"),
            log_dir: tmp.path().join("logs"),
            portable: false,
        };
        let mut store = UserStore::default();
        store.add("admin", "admin", "admin password 1").unwrap();
        store.save(&paths.config_file).unwrap();
        let settings = WebSettings {
            bind: "0.0.0.0".into(),
            ..settings
        };
        let app = WebApp::new(
            paths.clone(),
            settings,
            Vec::new(),
            Arc::new(LocalRunner::new(paths.clone())),
        )
        .unwrap();
        (tmp, paths, app)
    }

    fn request(method: &str, path: &str, from: &str, cookie: Option<&str>, body: &str) -> Request {
        let mut headers = HashMap::new();
        headers.insert("host".to_string(), "server:8686".to_string());
        if let Some(cookie) = cookie {
            headers.insert("cookie".to_string(), cookie.to_string());
        }
        Request {
            method: method.into(),
            path: path.into(),
            query: HashMap::new(),
            headers,
            body: body.as_bytes().to_vec(),
            peer: from.parse::<IpAddr>().unwrap(),
            tls: false,
        }
    }

    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn sign_in(app: &WebApp, from: &str) -> String {
        let r = app.handle(request(
            "POST",
            "/login",
            from,
            None,
            "user=admin&password=admin+password+1",
        ));
        assert_eq!(r.status, 303);
        header(&r, "set-cookie")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    #[test]
    fn an_address_with_many_wrong_sign_ins_is_blocked() {
        let (_tmp, paths, app) = app(WebSettings {
            auto_block_after: 3,
            lockout_attempts: 0,
            ..WebSettings::default()
        });
        let wrong = "user=admin&password=wrong";
        for _ in 0..3 {
            let r = app.handle(request("POST", "/login", "203.0.113.7", None, wrong));
            assert_eq!(r.status, 200, "the sign-in page again");
        }
        let list = Blocklist::load(&paths.config_file);
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].address, "203.0.113.7");
        assert_eq!(list.entries[0].by, "automatic");
        assert_eq!(
            app.handle(request("GET", "/login", "203.0.113.7", None, ""))
                .status,
            403
        );
        assert_eq!(
            app.handle(request("GET", "/login", "203.0.113.8", None, ""))
                .status,
            200,
            "other addresses are not affected"
        );
        // This computer is never blocked.
        for _ in 0..4 {
            app.handle(request("POST", "/login", "127.0.0.1", None, wrong));
        }
        assert_eq!(Blocklist::load(&paths.config_file).entries.len(), 1);
        assert_eq!(
            app.handle(request("GET", "/login", "127.0.0.1", None, ""))
                .status,
            200
        );
    }

    #[test]
    fn administrators_manage_only_from_where_allowed() {
        let (_tmp, paths, app) = app(WebSettings {
            admin_access: AdminAccess::Networks,
            admin_networks: vec!["192.168.1.0/24".into()],
            ..WebSettings::default()
        });
        let away = sign_in(&app, "203.0.113.7");
        assert_eq!(
            app.handle(request("GET", "/", "203.0.113.7", Some(&away), ""))
                .status,
            200,
            "signed in, with what the role allows besides administration"
        );
        assert_eq!(
            app.handle(request("GET", "/users", "203.0.113.7", Some(&away), ""))
                .status,
            403
        );
        assert_eq!(
            app.handle(request("GET", "/access", "203.0.113.7", Some(&away), ""))
                .status,
            403
        );
        let office = sign_in(&app, "192.168.1.20");
        assert_eq!(
            app.handle(request("GET", "/users", "192.168.1.20", Some(&office), ""))
                .status,
            200
        );

        // One click in the access page blocks an address; the next request
        // from there is refused, without a restart.
        let page = app.handle(request("GET", "/access", "192.168.1.20", Some(&office), ""));
        assert_eq!(page.status, 200);
        let html = String::from_utf8_lossy(&page.body);
        let csrf = html
            .split(r#"name="csrf" value=""#)
            .nth(1)
            .and_then(|r| r.split('"').next())
            .unwrap()
            .to_string();
        let r = app.handle(request(
            "POST",
            "/access/block",
            "192.168.1.20",
            Some(&office),
            &format!("csrf={csrf}&address=203.0.113.7&reason=test"),
        ));
        assert_eq!(r.status, 303);
        assert_eq!(
            Blocklist::load(&paths.config_file).entries[0].by,
            "web:admin"
        );
        assert_eq!(
            app.handle(request("GET", "/", "203.0.113.7", Some(&away), ""))
                .status,
            403
        );
    }
}
