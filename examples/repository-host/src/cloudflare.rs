//! Local-only HTTP bridge to real Wrangler R2/Durable Object bindings.
//! Native Git remains a separate local codec, not code running inside a Worker.
use crate::http::Response;
use crate::storage::{
    BlobStore, CloudflareBindings, DurableStateStore, LocalBlobStore, LocalStateStore, Manifest,
    R2BlobStore, StateStore,
};
use crate::wire::{self, Policy};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Write},
    net::TcpStream,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_RESPONSE: usize = 128 * 1024 * 1024;

fn read_response(reader: &mut impl BufRead, method: &str) -> io::Result<Response> {
    let first = wire::line(reader, 8192, Policy::Response)?;
    if !first.starts_with("HTTP/1.") || !first.ends_with("\r\n") {
        return Err(io::Error::other("invalid emulator status line"));
    }
    let status = first
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| io::Error::other("missing HTTP status"))?
        .parse()
        .map_err(io::Error::other)?;
    let headers = wire::headers(reader, first.len(), Policy::Response)?;
    let body = if status == 204 || status == 304 || method == "HEAD" {
        // These statuses have no message body, even on a persistent socket.
        Vec::new()
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        wire::chunked(reader, MAX_RESPONSE, Policy::Response)?
    } else if let Some(length) = headers.get("content-length") {
        let length: usize = length.parse().map_err(io::Error::other)?;
        wire::exact(reader, length, MAX_RESPONSE)?
    } else {
        wire::to_end(reader, MAX_RESPONSE)?
    };
    Ok(Response {
        status,
        headers,
        body: body.into(),
    })
}

pub struct HttpServices {
    authority: String,
    repo: String,
    token: String,
    phase: Mutex<String>,
}

impl HttpServices {
    pub fn new(origin: &str, repo: &str, token: &str) -> io::Result<Self> {
        let authority = origin
            .strip_prefix("http://127.0.0.1:")
            .and_then(|port| port.strip_suffix('/').or(Some(port)))
            .filter(|port| port.parse::<u16>().is_ok_and(|port| port > 0))
            .map(|port| format!("127.0.0.1:{port}"))
            .ok_or_else(|| {
                io::Error::other("Cloudflare emulator origin must be http://127.0.0.1:PORT")
            })?;
        if repo.is_empty()
            || repo.len() > 80
            || !repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(io::Error::other("invalid emulator repository name"));
        }
        if token.is_empty() || token.bytes().any(|byte| byte < 32 || byte == 127) {
            return Err(io::Error::other("invalid local service token"));
        }
        Ok(Self {
            authority,
            repo: repo.into(),
            token: token.into(),
            phase: Mutex::new("startup".into()),
        })
    }

    pub(crate) fn set_phase(&self, phase: &str) -> io::Result<()> {
        *self
            .phase
            .lock()
            .map_err(|_| io::Error::other("phase lock poisoned"))? = phase.into();
        Ok(())
    }

    fn call(
        &self,
        method: &str,
        route: &str,
        body: &[u8],
        phase: Option<&str>,
    ) -> io::Result<Response> {
        let current_phase = self
            .phase
            .lock()
            .map_err(|_| io::Error::other("phase lock poisoned"))?
            .clone();
        let phase = phase.unwrap_or(&current_phase);
        let mut stream = TcpStream::connect(&self.authority)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        stream.set_write_timeout(Some(Duration::from_secs(60)))?;
        write!(
            stream,
            "{method} /_services/{route} HTTP/1.1\r\nHost: {}\r\nx-hosting-token: {}\r\nx-hosting-repo: {}\r\nx-hosting-phase: {phase}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.authority,
            self.token,
            self.repo,
            body.len()
        )?;
        stream.write_all(body)?;
        let response = read_response(&mut BufReader::new(stream), method)?;
        if !(200..300).contains(&response.status) {
            return Err(io::Error::new(
                match response.status {
                    404 => io::ErrorKind::NotFound,
                    409 => io::ErrorKind::AlreadyExists,
                    401 | 403 => io::ErrorKind::PermissionDenied,
                    _ => io::ErrorKind::Other,
                },
                format!(
                    "emulator {method} {route}: {}: {}",
                    response.status,
                    String::from_utf8_lossy(&response.body[..response.body.len().min(1024)])
                ),
            ));
        }
        Ok(response)
    }

    fn json(
        &self,
        method: &str,
        route: &str,
        value: Option<&Value>,
        phase: Option<&str>,
    ) -> io::Result<Value> {
        let body = value
            .map(serde_json::to_vec)
            .transpose()?
            .unwrap_or_default();
        let response = self.call(method, route, &body, phase)?;
        serde_json::from_slice(&response.body).map_err(io::Error::other)
    }

    pub fn register(&self, codec_origin: &str) -> io::Result<()> {
        // Reuse strict loopback validation so registration cannot enable SSRF.
        Self::new(codec_origin, &self.repo, &self.token)?;
        self.json(
            "POST",
            "register",
            Some(&json!({"codec_origin": codec_origin})),
            Some("stats"),
        )?;
        Ok(())
    }

    pub fn unregister(&self) -> io::Result<()> {
        self.json("POST", "unregister", Some(&json!({})), Some("stats"))?;
        Ok(())
    }

    pub fn usage(&self) -> io::Result<Value> {
        self.json("GET", "metrics", None, Some("stats"))
    }

    fn reclaim(&self, manifest: &Manifest) -> io::Result<usize> {
        let live_keys: Vec<_> = manifest
            .packs
            .iter()
            .flat_map(|p| p.pack.iter().chain(&p.index))
            .map(|chunk| &chunk.key)
            .collect();
        let value = self.json(
            "POST",
            "gc",
            Some(&json!({"live_keys": live_keys, "expected_generation": manifest.generation})),
            None,
        )?;
        value
            .get("deleted")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .ok_or_else(|| io::Error::other("invalid emulator GC result"))
    }
}

impl CloudflareBindings for HttpServices {
    fn r2_get(&self, key: &str) -> io::Result<Vec<u8>> {
        Ok(self
            .call("GET", &format!("blob/{key}"), &[], None)?
            .body
            .to_vec())
    }
    fn r2_put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.call("PUT", &format!("blob/{key}"), bytes, None)?;
        Ok(())
    }
    fn r2_delete(&self, key: &str) -> io::Result<()> {
        self.call("DELETE", &format!("blob/{key}"), &[], None)?;
        Ok(())
    }
    fn manifest_load(&self) -> io::Result<Manifest> {
        serde_json::from_value(self.json("GET", "manifest", None, None)?).map_err(io::Error::other)
    }
    fn manifest_cas(&self, expected: u64, next: &Manifest) -> io::Result<bool> {
        self.json(
            "POST",
            "manifest",
            Some(&json!({"expected": expected,"next": next})),
            None,
        )?["published"]
            .as_bool()
            .ok_or_else(|| io::Error::other("invalid emulator CAS result"))
    }
}

pub(crate) enum HostBlobs {
    Local(LocalBlobStore),
    Cloudflare(R2BlobStore<HttpServices>, Arc<HttpServices>),
}
impl HostBlobs {
    fn as_store(&self) -> &dyn BlobStore {
        match self {
            Self::Local(blobs) => blobs,
            Self::Cloudflare(blobs, _) => blobs,
        }
    }

    pub(crate) fn put_file(&self, key: &str, source: &Path, verified: &[u8]) -> io::Result<()> {
        match self {
            Self::Local(blobs) => blobs.put_file(key, source, verified),
            Self::Cloudflare(..) => self.put(key, verified),
        }
    }
    pub(crate) fn materialize_file(
        &self,
        key: &str,
        destination: &Path,
        verified: &[u8],
    ) -> io::Result<()> {
        match self {
            Self::Local(blobs) => blobs.materialize_file(key, destination, verified),
            Self::Cloudflare(..) => crate::storage::write_native_file(destination, verified),
        }
    }
    pub(crate) fn reclaim_unreferenced(&self, manifest: &Manifest) -> io::Result<usize> {
        match self {
            Self::Local(blobs) => blobs.reclaim_unreferenced(manifest),
            Self::Cloudflare(_, services) => services.reclaim(manifest),
        }
    }
}
impl BlobStore for HostBlobs {
    fn get(&self, key: &str) -> io::Result<Vec<u8>> {
        self.as_store().get(key)
    }
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.as_store().put(key, bytes)
    }
    fn delete(&self, key: &str) -> io::Result<()> {
        self.as_store().delete(key)
    }
}
pub(crate) enum HostState {
    Local(LocalStateStore),
    Cloudflare(DurableStateStore<HttpServices>),
}
impl HostState {
    fn as_store(&self) -> &dyn StateStore {
        match self {
            Self::Local(state) => state,
            Self::Cloudflare(state) => state,
        }
    }

    pub(crate) fn stabilize(&self) -> io::Result<()> {
        match self {
            Self::Local(state) => state.stabilize(),
            Self::Cloudflare(_) => Ok(()),
        }
    }
}
impl StateStore for HostState {
    fn load(&self) -> io::Result<Manifest> {
        self.as_store().load()
    }
    fn compare_and_swap(&self, expected: u64, next: &Manifest) -> io::Result<bool> {
        self.as_store().compare_and_swap(expected, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_framing_handles_content_length_chunk_extensions_and_eof() {
        use std::io::Cursor;

        for (framing, payload, remaining) in [
            ("Content-Length: 4\r\n", "datafollowing", "following"),
            (
                "Transfer-Encoding: Chunked\r\nContent-Length: 999\r\n",
                "2;name=value\r\nda\r\n2\r\nta\r\n0\r\n\r\n",
                "\r\n",
            ),
            ("", "data", ""),
        ] {
            let mut reader = Cursor::new(format!("HTTP/1.1 200 OK\r\n{framing}\r\n{payload}"));
            let response = read_response(&mut reader, "GET").unwrap();
            assert_eq!(&*response.body, b"data");
            assert_eq!(reader.fill_buf().unwrap(), remaining.as_bytes());
        }
        let mut truncated = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\ndata");
        assert_eq!(
            read_response(&mut truncated, "GET").err().unwrap().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn bodyless_responses_do_not_parse_framing_or_consume_body_bytes() {
        use std::io::Cursor;

        for (method, status) in [("HEAD", 200), ("GET", 204), ("GET", 304)] {
            let mut reader = Cursor::new(format!(
                "HTTP/1.1 {status} No body\r\nContent-Length: invalid\r\nTransfer-Encoding: chunked\r\n\r\nuntouched"
            ));
            let response = read_response(&mut reader, method).unwrap();
            assert_eq!(response.status, status);
            assert!(response.body.is_empty());
            assert_eq!(reader.fill_buf().unwrap(), b"untouched");
        }
    }

    #[test]
    fn service_bridge_rejects_nonlocal_origins_and_header_injection() {
        assert!(HttpServices::new("http://127.0.0.1:8787", "demo", "local-token").is_ok());
        for origin in [
            "https://example.com",
            "http://localhost:8787",
            "http://127.0.0.1:0",
            "http://127.0.0.1:8787/path",
        ] {
            assert!(HttpServices::new(origin, "demo", "token").is_err());
        }
        assert!(HttpServices::new("http://127.0.0.1:8787", "../demo", "token").is_err());
        assert!(HttpServices::new("http://127.0.0.1:8787", "demo", "bad\r\ntoken").is_err());
    }
}
