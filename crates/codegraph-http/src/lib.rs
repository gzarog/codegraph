//! A small HTTP/1.1 server: one request per connection, `Content-Length`
//! bodies only. Enough for an ingest endpoint and a dashboard behind a
//! TLS-terminating proxy; peer addresses are never read.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 64;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Lower-cased names.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            (k == name).then_some(v)
        })
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Response {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body: body.into(),
        }
    }

    pub fn text(status: u16, body: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", body)
    }

    pub fn json(value: &serde_json::Value) -> Self {
        Self::new(200, "application/json", value.to_string())
    }

    pub fn empty(status: u16) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_form(s: &str) -> HashMap<String, String> {
    s.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (!k.is_empty()).then(|| (percent_decode(k), percent_decode(v)))
        })
        .collect()
}

enum ReadError {
    Bad,
    TooLarge,
    LengthRequired,
}

fn read_request(stream: &mut TcpStream, max_body: usize) -> Result<Request, ReadError> {
    let mut reader = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take((MAX_HEADER_BYTES - head.len().min(MAX_HEADER_BYTES)) as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| ReadError::Bad)?;
        if n == 0 {
            return Err(ReadError::Bad);
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEADER_BYTES {
            return Err(ReadError::TooLarge);
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let head = String::from_utf8(head).map_err(|_| ReadError::Bad)?;
    let mut lines = head.lines();
    let mut first = lines.next().ok_or(ReadError::Bad)?.split_whitespace();
    let method = first.next().ok_or(ReadError::Bad)?.to_string();
    let target = first.next().ok_or(ReadError::Bad)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if matches!(method.as_str(), "POST" | "PUT") {
        let len: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .ok_or(ReadError::LengthRequired)?;
        if len > max_body {
            return Err(ReadError::TooLarge);
        }
        body.resize(len, 0);
        reader.read_exact(&mut body).map_err(|_| ReadError::Bad)?;
    }
    Ok(Request {
        method,
        path: percent_decode(path),
        query: parse_form(query),
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, r: &Response) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", r.status, reason(r.status));
    for (k, v) in &r.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\nx-content-type-options: nosniff\r\n\r\n",
        r.body.len()
    ));
    stream.write_all(head.as_bytes())?;
    stream.write_all(&r.body)?;
    stream.flush()
}

/// Serve forever, one thread per connection (bounded).
pub fn serve(
    listener: TcpListener,
    max_body: usize,
    handler: Arc<dyn Fn(Request) -> Response + Send + Sync>,
) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            let _ = write_response(&mut stream, &Response::text(503, "busy\n"));
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let (handler, active) = (handler.clone(), active.clone());
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
            let response = match read_request(&mut stream, max_body) {
                Ok(req) => handler(req),
                Err(ReadError::TooLarge) => Response::text(413, "payload too large\n"),
                Err(ReadError::LengthRequired) => Response::text(411, "length required\n"),
                Err(ReadError::Bad) => Response::text(400, "bad request\n"),
            };
            let _ = write_response(&mut stream, &response);
            active.fetch_sub(1, Ordering::Relaxed);
        });
    }
}
