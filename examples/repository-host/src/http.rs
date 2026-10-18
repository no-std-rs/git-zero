//! Deliberately small, bounded, loopback HTTP/1.1 runner shared by both backends.
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_BODY: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Arc<[u8]>,
}

impl Response {
    pub fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            headers: BTreeMap::from([("Content-Type".into(), "text/plain".into())]),
            body: Arc::from(message.as_bytes()),
        }
    }
}

fn line<R: BufRead>(reader: &mut R, max: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    reader
        .take((max + 1) as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > max || !bytes.ends_with(b"\r\n") {
        return Err(io::Error::other("invalid/oversized HTTP line"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let first = line(&mut reader, 8192)?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if !["GET", "POST"].contains(&method.as_str()) || !target.starts_with('/') {
        return Err(io::Error::other("invalid request"));
    }
    let mut headers = BTreeMap::new();
    let mut header_bytes = first.len();
    loop {
        let header = line(&mut reader, 8192)?;
        header_bytes += header.len();
        if header_bytes > 65536 {
            return Err(io::Error::other("headers too large"));
        }
        if header == "\r\n" {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid header"))?;
        let key = name.to_ascii_lowercase();
        if headers.insert(key, value.trim().to_string()).is_some() {
            return Err(io::Error::other("duplicate header"));
        }
    }
    if headers.contains_key("expect") {
        reader
            .get_mut()
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let mut body = Vec::new();
    if let Some(encoding) = headers.get("transfer-encoding") {
        if encoding != "chunked" || headers.contains_key("content-length") {
            return Err(io::Error::other("unsupported framing"));
        }
        loop {
            let length = line(&mut reader, 128)?;
            let length = usize::from_str_radix(length.trim(), 16).map_err(io::Error::other)?;
            if length == 0 {
                if line(&mut reader, 8192)? != "\r\n" {
                    return Err(io::Error::other("trailers unsupported"));
                }
                break;
            }
            if length > MAX_BODY - body.len() {
                return Err(io::Error::other("request too large"));
            }
            let start = body.len();
            body.resize(start + length, 0);
            reader.read_exact(&mut body[start..])?;
            if line(&mut reader, 2)? != "\r\n" {
                return Err(io::Error::other("invalid chunk"));
            }
        }
    } else if let Some(length) = headers.get("content-length") {
        let length: usize = length.parse().map_err(io::Error::other)?;
        if length > MAX_BODY {
            return Err(io::Error::other("request too large"));
        }
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    }
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}

pub fn serve(port: u16, mut handler: impl FnMut(Request) -> Response) -> io::Result<()> {
    serve_with_ready(port, &mut handler, |_| Ok(()))
}

pub fn serve_with_ready(
    port: u16,
    mut handler: impl FnMut(Request) -> Response,
    ready: impl FnOnce(&str) -> io::Result<()>,
) -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let origin = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    ready(&origin)?;
    println!("{origin}/repo.git");
    io::stdout().flush()?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let response = match read_request(&mut stream) {
            Ok(request) => handler(request),
            Err(error) => Response::error(400, &error.to_string()),
        };
        let send = |stream: &mut TcpStream| -> io::Result<()> {
            let mut headers = Vec::with_capacity(512);
            write!(
                headers,
                "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n",
                response.status,
                response.body.len()
            )?;
            for (name, value) in &response.headers {
                if !["content-length", "connection"].contains(&name.to_ascii_lowercase().as_str()) {
                    write!(headers, "{name}: {value}\r\n")?;
                }
            }
            headers.extend_from_slice(b"Cache-Control: no-store\r\n\r\n");
            stream.write_all(&headers)?;
            stream.write_all(&response.body)
        };
        // A disconnected Git client must not stop the listening server.
        let _ = send(&mut stream);
    }
    Ok(())
}
