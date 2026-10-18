//! Deliberately small, bounded, loopback HTTP/1.1 runner shared by both backends.
use crate::diagnostics::{Counts, Observed, RequestScope, Span};
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
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

fn read_request(stream: &mut impl ReadWrite, diagnostic: &mut Span) -> io::Result<Request> {
    diagnostic.mark("request_header_read_begin");
    let mut reader = BufReader::new(stream);
    let first = line(&mut reader, 8192)?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    diagnostic.field("method", method.clone());
    diagnostic.field("path", target.split('?').next().unwrap_or("").to_string());
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
    diagnostic.mark("request_headers_complete");
    diagnostic.field(
        "framing",
        if headers.contains_key("transfer-encoding") {
            "chunked"
        } else if headers.contains_key("content-length") {
            "content-length"
        } else {
            "none"
        },
    );
    diagnostic.field("expect_present", headers.contains_key("expect"));
    if headers.contains_key("expect") {
        diagnostic.mark("continue_write_begin");
        reader
            .get_mut()
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        diagnostic.mark("continue_write_complete");
    }
    diagnostic.mark("request_body_read_begin");
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
    diagnostic.mark("request_body_complete");
    diagnostic.body("request_body", &body);
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn write_response(
    stream: &mut impl Write,
    response: &Response,
    diagnostic: &mut Span,
) -> io::Result<()> {
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
    diagnostic.mark("response_header_write_begin");
    stream.write_all(&headers)?;
    diagnostic.mark("response_headers_written");
    diagnostic.mark("response_body_write_begin");
    stream.write_all(&response.body)?;
    diagnostic.mark("response_body_written");
    Ok(())
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
        let stream = stream?;
        let _request_scope = RequestScope::enter();
        let mut diagnostic = Span::new("http_request");
        diagnostic.mark("accepted");
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let counts = Counts::default();
        diagnostic.counts(&counts);
        let mut stream = Observed::new(stream, counts.clone());
        let response = match read_request(&mut stream, &mut diagnostic) {
            Ok(request) => {
                diagnostic.mark("handler_begin");
                let response = handler(request);
                diagnostic.mark("handler_end");
                response
            }
            Err(error) => {
                diagnostic.error(&error);
                diagnostic.field("request_read_failed", true);
                Response::error(400, &error.to_string())
            }
        };
        diagnostic.field("response_status", response.status);
        diagnostic.field("response_body_bytes", response.body.len());
        // A disconnected Git client must not stop the listening server.
        let sent = write_response(&mut stream, &response, &mut diagnostic);
        diagnostic.finish(&sent);
        drop(stream);
        diagnostic.mark("response_connection_closed");
        counts.save(&mut diagnostic);
    }
    Ok(())
}

#[cfg(test)]
mod phase_framing_tests {
    use super::*;
    use std::io::Cursor;

    struct Fragments {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Fragments {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = b.len().min(3);
            self.input.read(&mut b[..n])
        }
    }
    impl Write for Fragments {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn parse(input: &[u8]) -> (io::Result<Request>, Vec<u8>) {
        let mut io = Fragments {
            input: Cursor::new(input.to_vec()),
            output: Vec::new(),
        };
        let result = read_request(&mut io, &mut Span::new("test"));
        (result, io.output)
    }
    #[test]
    fn content_length_chunked_and_continue_preserve_partial_reads() {
        let (length, continue_bytes) = parse(b"POST /repo.git/git-upload-pack HTTP/1.1\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\nhello");
        assert_eq!(length.unwrap().body, b"hello");
        assert_eq!(continue_bytes, b"HTTP/1.1 100 Continue\r\n\r\n");
        let (chunked, _) = parse(b"POST /repo.git/git-upload-pack HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhe\r\n3\r\nllo\r\n0\r\n\r\n");
        assert_eq!(chunked.unwrap().body, b"hello");
    }
    #[test]
    fn premature_eof_and_ambiguous_framing_stay_errors() {
        let (truncated, _) =
            parse(b"POST /repo.git/git-upload-pack HTTP/1.1\r\nContent-Length: 5\r\n\r\nhi");
        assert_eq!(
            truncated.err().unwrap().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let (ambiguous, _) = parse(b"POST /repo.git/git-upload-pack HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n");
        assert!(ambiguous.is_err());
    }
    #[test]
    fn disconnected_output_preserves_error_and_exact_partial_bytes() {
        struct Disconnect(usize);
        impl Write for Disconnect {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                let n = b.len().min(self.0);
                self.0 -= n;
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let response = Response::error(200, "body");
        let mut complete = Vec::new();
        write_response(&mut complete, &response, &mut Span::new("test")).unwrap();
        let headers = complete.windows(4).position(|p| p == b"\r\n\r\n").unwrap() + 4;
        for (bound, kind) in [
            (17, "disconnected_header_test"),
            (headers + 2, "disconnected_body_test"),
        ] {
            let counts = Counts::default();
            let mut stream = Observed::new(Disconnect(bound), counts.clone());
            let mut diagnostic = Span::new(kind);
            diagnostic.counts(&counts);
            let result = write_response(&mut stream, &response, &mut diagnostic);
            diagnostic.finish(&result);
            diagnostic.mark("response_connection_closed");
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
            assert_eq!(counts.written(), bound as u64);
        }
    }
}
