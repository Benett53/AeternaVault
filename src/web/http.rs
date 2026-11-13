//! A small HTTP/1.1 server for the web interface: one request per
//! connection, plain or over TLS (rustls), with strict limits. It serves a
//! handful of pages to a few administrators, not the internet; simplicity
//! and predictable limits matter more than speed.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 32;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path without the query, e.g. `/backups`.
    pub path: String,
    pub query: HashMap<String, String>,
    /// Header names in lower case.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub peer: IpAddr,
    pub tls: bool,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    pub fn cookie(&self, name: &str) -> Option<String> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then(|| value.to_string())
        })
    }

    /// Fields of a submitted form (`application/x-www-form-urlencoded`).
    pub fn form(&self) -> HashMap<String, String> {
        parse_query(&String::from_utf8_lossy(&self.body))
    }

    /// All values of a field that occurs several times (checkboxes).
    pub fn form_all(&self, name: &str) -> Vec<String> {
        String::from_utf8_lossy(&self.body)
            .split('&')
            .filter_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                (decode(k) == name).then(|| decode(v))
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn html(body: String) -> Self {
        Self::with_type(200, "text/html; charset=utf-8", body.into_bytes())
    }

    pub fn with_type(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
        }
    }

    pub fn text(status: u16, text: &str) -> Self {
        Self::with_type(
            status,
            "text/plain; charset=utf-8",
            text.as_bytes().to_vec(),
        )
    }

    /// After a form: back to a page (303 See Other, so a reload does not send it again).
    pub fn redirect(location: &str) -> Self {
        let mut response = Self::with_type(303, "text/plain; charset=utf-8", Vec::new());
        response.headers.push(("Location".into(), location.into()));
        response
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let digits = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()));
                match digits.and_then(|d| u8::from_str_radix(d, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn parse_query(text: &str) -> HashMap<String, String> {
    text.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect()
}

pub type Handler = Arc<dyn Fn(Request) -> Response + Send + Sync>;

/// Accepts connections until `stop` is set. Blocks; run it on its own thread.
pub fn serve(
    listener: TcpListener,
    tls: Option<Arc<rustls::ServerConfig>>,
    handler: Handler,
    stop: Arc<AtomicBool>,
) {
    if let Err(err) = listener.set_nonblocking(true) {
        tracing::error!("web interface: {err}");
        return;
    }
    let open = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        let (stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(err) => {
                tracing::debug!("web interface: connection not accepted: {err}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if open.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            drop(stream);
            continue;
        }
        open.fetch_add(1, Ordering::Relaxed);
        let (handler, tls, counter) = (handler.clone(), tls.clone(), open.clone());
        let spawned = std::thread::Builder::new()
            .name("aeterna-web".into())
            .spawn(move || {
                connection(stream, peer, tls, &handler);
                counter.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            open.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn connection(
    stream: TcpStream,
    peer: SocketAddr,
    tls: Option<Arc<rustls::ServerConfig>>,
    handler: &Handler,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let _ = stream.set_nodelay(true);
    let peer = canonical(peer.ip());
    match tls {
        Some(config) => {
            let Ok(server) = rustls::ServerConnection::new(config) else {
                return;
            };
            let mut stream = rustls::StreamOwned::new(server, stream);
            exchange(&mut stream, peer, true, handler);
            stream.conn.send_close_notify();
            let _ = stream.flush();
        }
        None => {
            let mut stream = stream;
            exchange(&mut stream, peer, false, handler);
        }
    }
}

/// `::ffff:192.168.1.5` → `192.168.1.5`.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn exchange(stream: &mut (impl Read + Write), peer: IpAddr, tls: bool, handler: &Handler) {
    let response = match read_request(stream, peer, tls) {
        Ok(request) => handler(request),
        Err(Some(status)) => Response::text(status, reason(status)),
        // Closed, or not HTTP(S) at all.
        Err(None) => return,
    };
    let _ = write_response(stream, response, tls);
}

fn read_request(stream: &mut impl Read, peer: IpAddr, tls: bool) -> Result<Request, Option<u16>> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        if buffer.len() > MAX_HEAD {
            return Err(Some(431));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(None),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err((!buffer.is_empty()).then_some(408));
            }
            Err(_) => return Err(None),
        }
    };

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    match parsed.parse(&buffer[..head_end]) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(Some(400)),
    }
    let method = parsed.method.unwrap_or_default().to_string();
    let target = parsed.path.unwrap_or("/").to_string();
    let mut header_map = HashMap::new();
    for header in parsed.headers.iter() {
        let value = String::from_utf8_lossy(header.value).into_owned();
        header_map.insert(header.name.to_ascii_lowercase(), value);
    }
    if header_map.contains_key("transfer-encoding") {
        return Err(Some(400));
    }
    let length: usize = match header_map.get("content-length") {
        Some(v) => v.trim().parse().map_err(|_| Some(400))?,
        None => 0,
    };
    if length > MAX_BODY {
        return Err(Some(413));
    }
    let mut body = buffer[head_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) => return Err(Some(400)),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return Err(Some(408)),
        }
    }
    body.truncate(length);

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target, HashMap::new()),
    };
    if !path.starts_with('/') {
        return Err(Some(400));
    }
    Ok(Request {
        method,
        path,
        query,
        headers: header_map,
        body,
        peer,
        tls,
    })
}

fn write_response(stream: &mut impl Write, response: Response, tls: bool) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    let mut headers = response.headers;
    // Pages are private and never framed, guessed or cached.
    let fixed = [
        (
            "Content-Security-Policy",
            "default-src 'none'; style-src 'self'; img-src 'self' data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        ("Cache-Control", "no-store"),
        ("Connection", "close"),
    ];
    for (name, value) in fixed {
        if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
            headers.push((name.into(), value.into()));
        }
    }
    if tls {
        headers.push((
            "Strict-Transport-Security".into(),
            "max-age=31536000".into(),
        ));
    }
    headers.push(("Content-Length".into(), response.body.len().to_string()));
    for (name, value) in headers {
        // Never let a value end the header block.
        let value = value.replace(['\r', '\n'], " ");
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(raw: &[u8]) -> Result<Request, Option<u16>> {
        let mut input = raw;
        read_request(&mut input, "127.0.0.1".parse().unwrap(), false)
    }

    #[test]
    fn requests_are_parsed_with_limits() {
        let request = read(
            b"POST /jobs/run?x=1 HTTP/1.1\r\nHost: a\r\nCookie: s=abc; t=1\r\nContent-Length: 13\r\n\r\nid=a%2Fb&z=+1",
        )
        .unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/jobs/run");
        assert_eq!(request.query["x"], "1");
        assert_eq!(request.cookie("s").as_deref(), Some("abc"));
        assert_eq!(request.form()["id"], "a/b");
        assert_eq!(request.form()["z"], " 1");

        assert_eq!(read(b"GARBAGE\r\n\r\n").unwrap_err(), Some(400));
        let chunked = b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(read(chunked).unwrap_err(), Some(400));
        let big = b"POST / HTTP/1.1\r\nContent-Length: 999999\r\n\r\n";
        assert_eq!(read(big).unwrap_err(), Some(413));
        let mut huge = b"GET / HTTP/1.1\r\nX: ".to_vec();
        huge.extend(std::iter::repeat_n(b'a', MAX_HEAD + 10));
        assert_eq!(read(&huge).unwrap_err(), Some(431));
    }

    #[test]
    fn header_values_cannot_inject_headers() {
        let mut out = Vec::new();
        let response = Response::redirect("/x\r\nSet-Cookie: evil=1");
        write_response(&mut out, response, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("\r\nSet-Cookie"), "{text}");
        assert!(text.contains("Content-Security-Policy"));
    }

    #[test]
    fn decoding_survives_broken_escapes() {
        assert_eq!(decode("a%2"), "a%2");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%C3%A4"), "ä");
        assert_eq!(encode("a b/ä"), "a%20b%2F%C3%A4");
    }
}
