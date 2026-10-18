//! Local-only HTTP bridge to real Wrangler R2/Durable Object bindings.
//! Native Git remains a separate local codec, not code running inside a Worker.
use crate::http::Response;
use crate::storage::{
    BlobStore, Chunk, CloudflareBindings, DurableStateStore, LocalBlobStore, LocalStateStore,
    Manifest, R2BlobStore, StateStore,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_RESPONSE: usize = 128 * 1024 * 1024;
const MAX_BULK_PAYLOAD: usize = 16 * 1024 * 1024;
const MAX_BULK_KEYS: usize = 256;
const MAX_MANIFEST_BYTES: usize = 1_500_000;
const BULK_HEADER_BYTES: usize = 12;
const BULK_LENGTH_BYTES: usize = 8;
const MAX_BULK_RESPONSE: usize =
    MAX_BULK_PAYLOAD + BULK_HEADER_BYTES + BULK_LENGTH_BYTES * MAX_BULK_KEYS;
const BULK_MAGIC: &[u8; 4] = b"GZBR";

fn validate_bulk_chunks(chunks: &[Chunk]) -> io::Result<()> {
    let mut lengths = BTreeMap::new();
    for chunk in chunks {
        if chunk.len == 0
            || chunk.len > MAX_RESPONSE as u64
            || chunk.key.len() != 71
            || !chunk.key.starts_with("chunks/")
            || !chunk.key.as_bytes()[7..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
            || lengths
                .insert(&chunk.key, chunk.len)
                .is_some_and(|len| len != chunk.len)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid bulk read chunk",
            ));
        }
    }
    Ok(())
}

/// Larger legacy chunks retain the scalar route. Payload and framing have
/// separate bounds so two complete 8 MiB chunks fit in one bulk response.
fn bulk_batch_len(chunks: &[Chunk]) -> usize {
    let mut payload = 0usize;
    let mut wire = BULK_HEADER_BYTES;
    let mut count = 0;
    for chunk in chunks.iter().take(MAX_BULK_KEYS) {
        if chunk.len == 0 || chunk.len > crate::CHUNK_BYTES as u64 {
            break;
        }
        let Ok(len) = usize::try_from(chunk.len) else {
            break;
        };
        let Some(next_payload) = payload
            .checked_add(len)
            .filter(|payload| *payload <= MAX_BULK_PAYLOAD)
        else {
            break;
        };
        let Some(next_wire) = wire
            .checked_add(BULK_LENGTH_BYTES)
            .and_then(|wire| wire.checked_add(len))
            .filter(|wire| *wire <= MAX_BULK_RESPONSE)
        else {
            break;
        };
        payload = next_payload;
        wire = next_wire;
        count += 1;
    }
    count
}

fn decode_bulk_response(chunks: &[Chunk], bytes: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid bulk blob response");
    if chunks.is_empty()
        || chunks.len() > MAX_BULK_KEYS
        || bulk_batch_len(chunks) != chunks.len()
        || bytes.len() > MAX_BULK_RESPONSE
        || bytes.len() < BULK_HEADER_BYTES
        || &bytes[..4] != BULK_MAGIC
        || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != 1
        || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize != chunks.len()
    {
        return Err(invalid());
    }
    let mut offset = BULK_HEADER_BYTES;
    let mut result = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let end = offset
            .checked_add(BULK_LENGTH_BYTES)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(invalid)?;
        let len = u64::from_le_bytes(bytes[offset..end].try_into().unwrap());
        if len != chunk.len {
            return Err(invalid());
        }
        offset = end;
        let end = usize::try_from(len)
            .ok()
            .and_then(|len| offset.checked_add(len))
            .filter(|end| *end <= bytes.len())
            .ok_or_else(invalid)?;
        let data = &bytes[offset..end];
        crate::verify_chunk(chunk, data)?;
        result.push(data.to_vec());
        offset = end;
    }
    if offset != bytes.len() {
        return Err(invalid());
    }
    Ok(result)
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
        let maximum = if route == "blobs/read" {
            MAX_BULK_RESPONSE
        } else {
            MAX_RESPONSE
        };
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
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.by_ref().take(8193).read_line(&mut line)?;
        if line.len() > 8192 || !line.starts_with("HTTP/1.") || !line.ends_with("\r\n") {
            return Err(io::Error::other("invalid emulator status line"));
        }
        let status = line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| io::Error::other("missing HTTP status"))?
            .parse()
            .map_err(io::Error::other)?;
        let mut headers = BTreeMap::new();
        let mut total_headers = line.len();
        loop {
            line.clear();
            let read = reader.by_ref().take(8193).read_line(&mut line)?;
            total_headers += read;
            if read == 0 || read > 8192 || total_headers > 65536 {
                return Err(io::Error::other("invalid emulator HTTP headers"));
            }
            if line == "\r\n" {
                break;
            }
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| io::Error::other("invalid emulator header"))?;
            headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        }
        let mut bytes = Vec::new();
        if status == 204 || status == 304 || method == "HEAD" {
            // These statuses have no message body, even on a persistent socket.
        } else if headers
            .get("transfer-encoding")
            .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
        {
            loop {
                line.clear();
                reader.by_ref().take(129).read_line(&mut line)?;
                if line.len() > 128 {
                    return Err(io::Error::other("invalid HTTP chunk"));
                }
                let length = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                    .map_err(io::Error::other)?;
                if length == 0 {
                    break;
                }
                if length > maximum - bytes.len() {
                    return Err(io::Error::other("emulator response too large"));
                }
                let start = bytes.len();
                bytes.resize(start + length, 0);
                reader.read_exact(&mut bytes[start..])?;
                let mut ending = [0u8; 2];
                reader.read_exact(&mut ending)?;
                if ending != *b"\r\n" {
                    return Err(io::Error::other("invalid HTTP chunk ending"));
                }
            }
        } else if let Some(length) = headers.get("content-length") {
            let length: usize = length.parse().map_err(io::Error::other)?;
            if length > maximum {
                return Err(io::Error::other("emulator response too large"));
            }
            bytes.resize(length, 0);
            reader.read_exact(&mut bytes)?;
        } else {
            reader.take((maximum + 1) as u64).read_to_end(&mut bytes)?;
            if bytes.len() > maximum {
                return Err(io::Error::other("emulator response too large"));
            }
            if let Some(length) = headers.get("content-length") {
                let length: usize = length.parse().map_err(io::Error::other)?;
                if bytes.len() != length {
                    return Err(io::Error::other("truncated emulator response"));
                }
            }
        }
        if !(200..300).contains(&status) {
            return Err(io::Error::new(
                match status {
                    404 => io::ErrorKind::NotFound,
                    409 => io::ErrorKind::AlreadyExists,
                    401 | 403 => io::ErrorKind::PermissionDenied,
                    _ => io::ErrorKind::Other,
                },
                format!(
                    "emulator {method} {route}: {status}: {}",
                    String::from_utf8_lossy(&bytes[..bytes.len().min(1024)])
                ),
            ));
        }
        Ok(Response {
            status,
            headers,
            body: bytes.into(),
        })
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
            .flat_map(crate::storage::StoredPack::chunks)
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
    fn r2_get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
        // Validate the complete request before issuing even its first read.
        validate_bulk_chunks(chunks)?;
        let mut result = Vec::with_capacity(chunks.len());
        let mut offset = 0;
        while offset < chunks.len() {
            let count = bulk_batch_len(&chunks[offset..]);
            if count == 0 {
                let chunk = &chunks[offset];
                let data = self.r2_get(&chunk.key)?;
                crate::verify_chunk(chunk, &data)?;
                result.push(data);
                offset += 1;
                continue;
            }
            let batch = &chunks[offset..offset + count];
            let body = serde_json::to_vec(&json!({"chunks": batch})).map_err(io::Error::other)?;
            if body.len() > MAX_MANIFEST_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "bulk read request too large",
                ));
            }
            let response = self.call("POST", "blobs/read", &body, None)?;
            result.extend(decode_bulk_response(batch, &response.body)?);
            offset += count;
        }
        Ok(result)
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
            Self::Cloudflare(..) => OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?
                .write_all(verified),
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
        match self {
            Self::Local(b) => b.get(key),
            Self::Cloudflare(b, _) => b.get(key),
        }
    }
    fn get_many(&self, chunks: &[Chunk]) -> io::Result<Vec<Vec<u8>>> {
        match self {
            Self::Local(b) => b.get_many(chunks),
            Self::Cloudflare(b, _) => b.get_many(chunks),
        }
    }
    fn put(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Local(b) => b.put(key, bytes),
            Self::Cloudflare(b, _) => b.put(key, bytes),
        }
    }
    fn delete(&self, key: &str) -> io::Result<()> {
        match self {
            Self::Local(b) => b.delete(key),
            Self::Cloudflare(b, _) => b.delete(key),
        }
    }
}
pub(crate) enum HostState {
    Local(LocalStateStore),
    Cloudflare(DurableStateStore<HttpServices>),
}
impl HostState {
    pub(crate) fn stabilize(&self) -> io::Result<()> {
        match self {
            Self::Local(state) => state.stabilize(),
            Self::Cloudflare(_) => Ok(()),
        }
    }
}
impl StateStore for HostState {
    fn load(&self) -> io::Result<Manifest> {
        match self {
            Self::Local(s) => s.load(),
            Self::Cloudflare(s) => s.load(),
        }
    }
    fn compare_and_swap(&self, expected: u64, next: &Manifest) -> io::Result<bool> {
        match self {
            Self::Local(s) => s.compare_and_swap(expected, next),
            Self::Cloudflare(s) => s.compare_and_swap(expected, next),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(data: &[u8]) -> Chunk {
        Chunk {
            key: format!("chunks/{}", crate::hash(data)),
            len: data.len() as u64,
        }
    }

    fn frame(parts: &[&[u8]]) -> Vec<u8> {
        let mut bytes = BULK_MAGIC.to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(parts.len() as u32).to_le_bytes());
        for part in parts {
            bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
            bytes.extend_from_slice(part);
        }
        bytes
    }

    #[test]
    fn bulk_planning_bounds_payload_framing_keys_and_legacy_fallback() {
        let mut chunks = vec![chunk(b"a"); 3];
        chunks[0].len = crate::CHUNK_BYTES as u64;
        chunks[1].len = crate::CHUNK_BYTES as u64;
        assert_eq!(bulk_batch_len(&chunks[..2]), 2);
        assert_eq!(bulk_batch_len(&chunks), 2);
        assert_eq!(bulk_batch_len(&chunks[2..]), 1);
        assert_eq!(MAX_BULK_RESPONSE, 16_779_276);
        let mut exact = vec![chunk(b"a"); MAX_BULK_KEYS];
        for chunk in &mut exact {
            chunk.len = (MAX_BULK_PAYLOAD / MAX_BULK_KEYS) as u64;
        }
        assert_eq!(bulk_batch_len(&exact), MAX_BULK_KEYS);
        exact[MAX_BULK_KEYS - 1].len += 1;
        assert_eq!(bulk_batch_len(&exact), MAX_BULK_KEYS - 1);
        assert_eq!(
            bulk_batch_len(&vec![chunk(b"a"); MAX_BULK_KEYS + 1]),
            MAX_BULK_KEYS
        );
        assert!(
            serde_json::to_vec(&json!({"chunks": vec![chunk(b"a"); MAX_BULK_KEYS]}))
                .unwrap()
                .len()
                <= MAX_MANIFEST_BYTES
        );
        for len in [0, crate::CHUNK_BYTES as u64 + 1, u64::MAX] {
            chunks[0].len = len;
            assert_eq!(bulk_batch_len(&chunks), 0);
        }
        let services = HttpServices::new("http://127.0.0.1:1", "unused", "token").unwrap();
        assert!(services.r2_get_many(&[]).unwrap().is_empty());
        for bad in [
            "chunks/../escape".into(),
            format!("chunks/{}", "A".repeat(64)),
        ] {
            assert_eq!(
                services
                    .r2_get_many(&[Chunk { key: bad, len: 1 }])
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        let mut conflict = chunk(b"a");
        conflict.len += 1;
        assert_eq!(
            services
                .r2_get_many(&[chunk(b"a"), conflict])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn bulk_decoder_accepts_full_payload_and_rejects_excess_within_wire_bound() {
        let full = vec![0x61; crate::CHUNK_BYTES];
        let chunks = [chunk(&full), chunk(&full)];
        let valid = frame(&[&full, &full]);
        assert!(valid.len() > MAX_BULK_PAYLOAD);
        assert!(valid.len() <= MAX_BULK_RESPONSE);
        assert_eq!(
            decode_bulk_response(&chunks, &valid).unwrap(),
            vec![full.clone(), full.clone()]
        );

        let excessive = frame(&[&full, &full, b"z"]);
        assert!(excessive.len() <= MAX_BULK_RESPONSE);
        assert!(
            decode_bulk_response(
                &[chunks[0].clone(), chunks[1].clone(), chunk(b"z")],
                &excessive
            )
            .is_err()
        );
    }

    #[test]
    fn bulk_decoder_rejects_count_length_hash_truncation_trailing_and_overflow() {
        let chunks = [chunk(b"abc"), chunk(b"defgh"), chunk(b"abc")];
        let valid = frame(&[b"abc", b"defgh", b"abc"]);
        assert_eq!(
            decode_bulk_response(&chunks, &valid).unwrap(),
            vec![b"abc".to_vec(), b"defgh".to_vec(), b"abc".to_vec()]
        );
        for end in 0..valid.len() {
            assert!(
                decode_bulk_response(&chunks, &valid[..end]).is_err(),
                "accepted truncation at {end}"
            );
        }
        let mut invalid = Vec::new();
        for offset in [0, 4, 8, 12, BULK_HEADER_BYTES + BULK_LENGTH_BYTES] {
            let mut changed = valid.clone();
            changed[offset] ^= 1;
            invalid.push(changed);
        }
        let mut overflow = valid.clone();
        overflow[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        invalid.push(overflow);
        let mut trailing = valid.clone();
        trailing.push(0);
        invalid.push(trailing);
        invalid.push(vec![0; MAX_BULK_RESPONSE + 1]);
        for bytes in invalid {
            assert!(decode_bulk_response(&chunks, &bytes).is_err());
        }
        assert!(decode_bulk_response(&[], &frame(&[])).is_err());
    }

    fn fake_services(
        expected: Vec<(String, Vec<Chunk>, u16, Vec<u8>)>,
    ) -> (HttpServices, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (method, chunks, status, response) in expected {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(&method), "{line}");
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.strip_prefix("Content-Length: ") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if method.starts_with("POST") {
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(
                        serde_json::from_value::<Vec<Chunk>>(value["chunks"].clone()).unwrap(),
                        chunks
                    );
                } else {
                    assert!(body.is_empty());
                }
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                )
                .unwrap();
                reader.get_mut().write_all(&response).unwrap();
            }
        });
        (HttpServices::new(&origin, "test", "token").unwrap(), server)
    }

    #[test]
    fn bulk_bridge_batches_two_full_chunks_before_the_next_byte() {
        let first = vec![0x61; crate::CHUNK_BYTES];
        let second = vec![0x62; crate::CHUNK_BYTES];
        let chunks = vec![chunk(&first), chunk(&second), chunk(b"c")];
        let (services, server) = fake_services(vec![
            (
                "POST /_services/blobs/read ".into(),
                chunks[..2].to_vec(),
                200,
                frame(&[&first, &second]),
            ),
            (
                "POST /_services/blobs/read ".into(),
                chunks[2..].to_vec(),
                200,
                frame(&[b"c"]),
            ),
        ]);
        assert_eq!(
            services.r2_get_many(&chunks).unwrap(),
            vec![first, second, b"c".to_vec()]
        );
        server.join().unwrap();
    }

    #[test]
    fn bulk_bridge_batches_repeated_keys_and_preserves_large_legacy_scalar_reads() {
        let chunks = vec![chunk(b"a"); MAX_BULK_KEYS + 1];
        let legacy = vec![0x62; crate::CHUNK_BYTES + 1];
        let legacy_chunk = chunk(&legacy);
        let expected = vec![
            (
                "POST /_services/blobs/read ".into(),
                chunks[..MAX_BULK_KEYS].to_vec(),
                200,
                frame(&vec![b"a".as_slice(); MAX_BULK_KEYS]),
            ),
            (
                "POST /_services/blobs/read ".into(),
                vec![chunk(b"a")],
                200,
                frame(&[b"a"]),
            ),
            (
                format!("GET /_services/blob/{} ", legacy_chunk.key),
                vec![],
                200,
                legacy.clone(),
            ),
            (
                "POST /_services/blobs/read ".into(),
                vec![chunk(b"a")],
                200,
                frame(&[b"a"]),
            ),
        ];
        let (services, server) = fake_services(expected);
        assert!(services.r2_get_many(&[]).unwrap().is_empty());
        let mut request = chunks.clone();
        request.push(legacy_chunk);
        assert_eq!(
            services.r2_get_many(&request).unwrap(),
            [vec![b"a".to_vec(); chunks.len()], vec![legacy]].concat()
        );
        // A later call performs another RPC; no chunk cache survives the call.
        assert_eq!(
            services.r2_get_many(&[chunk(b"a")]).unwrap(),
            vec![b"a".to_vec()]
        );
        server.join().unwrap();
    }

    #[test]
    fn bulk_bridge_fails_closed_on_malformed_frame_and_missing_object() {
        let mut trailing = frame(&[b"a"]);
        trailing.push(0);
        let (services, server) = fake_services(vec![
            (
                "POST /_services/blobs/read ".into(),
                vec![chunk(b"a")],
                200,
                trailing,
            ),
            (
                "POST /_services/blobs/read ".into(),
                vec![chunk(b"a")],
                404,
                b"missing".to_vec(),
            ),
        ]);
        assert!(services.r2_get_many(&[chunk(b"a")]).is_err());
        assert_eq!(
            services.r2_get_many(&[chunk(b"a")]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        server.join().unwrap();
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
