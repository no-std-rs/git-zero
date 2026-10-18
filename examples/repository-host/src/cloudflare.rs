//! Local-only HTTP bridge to real Wrangler R2/Durable Object bindings.
//! Native Git remains a separate local codec, not code running inside a Worker.
use crate::http::{Request, Response};
use crate::storage::{
    BlobStore, Chunk, CloudflareBindings, DurableStateStore, LocalBlobStore, LocalStateStore,
    Manifest, PendingChunk, R2BlobStore, RetirementReceipt, RetiringCas, StateStore, UploadReceipt,
    UploadRetiringCas, VerifiedChunks,
};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const MAX_RESPONSE: usize = 128 * 1024 * 1024;
const MAX_BULK_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const MAX_BULK_PAYLOAD: usize = 16 * 1024 * 1024;
const MAX_BULK_KEYS: usize = 256;
const MAX_MANIFEST_BYTES: usize = 1_500_000;
const MAX_RETIREMENT_KEYS: usize = 256;
const MAX_RETIREMENT_RESPONSE: usize = 32 * 1024;
const BULK_HEADER_BYTES: usize = 12;
const BULK_LENGTH_BYTES: usize = 8;
const MAX_BULK_RESPONSE: usize =
    MAX_BULK_PAYLOAD + BULK_HEADER_BYTES + BULK_LENGTH_BYTES * MAX_BULK_KEYS;
const UPLOAD_MAGIC: &[u8; 4] = b"GZUP";
const BULK_MAGIC: &[u8; 4] = b"GZBR";

fn upload_supported(value: &Value) -> bool {
    value.get("manifest_upload_retire")
        == Some(&json!({
            "version": 1, "wire_bytes": MAX_BULK_RESPONSE,
            "metadata_bytes": MAX_MANIFEST_BYTES, "chunk_bytes": MAX_BULK_CHUNK_BYTES,
            "retirement_keys": MAX_RETIREMENT_KEYS,
        }))
}

/// None is a bounds/capability fallback decided before sending any request.
fn upload_frame(
    expected: u64,
    next: &Manifest,
    pending: &PendingChunk,
) -> io::Result<Option<Vec<u8>>> {
    if next.garbage.len() > MAX_RETIREMENT_KEYS {
        return Ok(None);
    }
    if expected.checked_add(1) != Some(next.generation) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid upload generation",
        ));
    }
    validate_retiring_manifest(next)?;
    crate::validate_staged_manifest(next, pending)?;
    let metadata = serde_json::to_vec(
        &json!({"version": 1, "expected": expected, "next": next, "chunk": pending.chunk()}),
    )?;
    let Some(size) = BULK_HEADER_BYTES
        .checked_add(metadata.len())
        .and_then(|n| n.checked_add(pending.bytes().len()))
        .filter(|size| {
            *size <= MAX_BULK_RESPONSE
                && metadata.len() <= MAX_MANIFEST_BYTES
                && pending.bytes().len() <= MAX_BULK_CHUNK_BYTES
        })
    else {
        return Ok(None);
    };
    let mut frame = Vec::with_capacity(size);
    frame.extend_from_slice(UPLOAD_MAGIC);
    frame.extend_from_slice(&1u32.to_le_bytes());
    frame.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    frame.extend_from_slice(&metadata);
    frame.extend_from_slice(pending.bytes());
    Ok(Some(frame))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadResponse {
    version: u32,
    uploaded: UploadReceipt,
    published: bool,
    #[serde(deserialize_with = "deserialize_retirement")]
    retirement: Option<RetirementReceipt>,
}

fn decode_upload_response(
    next: &Manifest,
    pending: &PendingChunk,
    bytes: &[u8],
) -> io::Result<UploadRetiringCas> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid upload response");
    if bytes.len() > MAX_RETIREMENT_RESPONSE {
        return Err(invalid());
    }
    let reply: UploadResponse = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if reply.version != 1
        || reply.uploaded.key != pending.chunk().key
        || reply.uploaded.len != pending.chunk().len
    {
        return Err(invalid());
    }
    match (reply.published, reply.retirement) {
        (false, None) => Ok(UploadRetiringCas::Stale(reply.uploaded)),
        (true, Some(retirement)) => {
            let keys = next.retirement_keys();
            if retirement.generation != next.generation
                || retirement.outcomes.len() != keys.len()
                || retirement
                    .outcomes
                    .iter()
                    .zip(keys)
                    .any(|(outcome, key)| outcome.key != key)
            {
                return Err(invalid());
            }
            Ok(UploadRetiringCas::Published {
                upload: reply.uploaded,
                retirement,
            })
        }
        _ => Err(invalid()),
    }
}

fn retirement_supported(value: &Value) -> bool {
    value
        .get("manifest_retire")
        .and_then(|config| config.get("version"))
        .and_then(Value::as_u64)
        == Some(1)
}

fn validate_retiring_manifest(next: &Manifest) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid retiring manifest");
    if next.generation > MAX_SAFE_GENERATION || next.garbage.len() > MAX_RETIREMENT_KEYS {
        return Err(invalid());
    }
    let mut debt = std::collections::BTreeSet::new();
    for key in &next.garbage {
        validate_bulk_chunks(&[Chunk {
            key: key.clone(),
            len: 1,
        }])?;
        if !debt.insert(key) {
            return Err(invalid());
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut lengths = BTreeMap::new();
    let mut snapshot_bytes = 0u64;
    for pack in &next.packs {
        if !ids.insert(&pack.id) {
            return Err(invalid());
        }
        let total = crate::pack_total(pack.pack_bytes, pack.index_bytes)?;
        snapshot_bytes = snapshot_bytes
            .checked_add(total)
            .filter(|total| *total <= MAX_RESPONSE as u64)
            .ok_or_else(invalid)?;
        if pack.bundle.is_empty() {
            crate::validate_chunks(&pack.pack, pack.pack_bytes)?;
            crate::validate_chunks(&pack.index, pack.index_bytes)?;
        } else {
            if !pack.pack.is_empty() || !pack.index.is_empty() {
                return Err(invalid());
            }
            crate::validate_chunks(&pack.bundle, total)?;
        }
        for chunk in pack.chunks() {
            validate_bulk_chunks(std::slice::from_ref(chunk))?;
            if lengths
                .insert(&chunk.key, chunk.len)
                .is_some_and(|len| len != chunk.len)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetirementResponse {
    version: u32,
    published: bool,
    // deserialize_with makes this field required even when its value is null.
    #[serde(deserialize_with = "deserialize_retirement")]
    retirement: Option<RetirementReceipt>,
}

fn deserialize_retirement<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<RetirementReceipt>, D::Error> {
    serde::Deserialize::deserialize(deserializer)
}

fn decode_retirement_response(next: &Manifest, bytes: &[u8]) -> io::Result<RetiringCas> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid retirement response");
    if bytes.len() > MAX_RETIREMENT_RESPONSE {
        return Err(invalid());
    }
    let reply: RetirementResponse = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if reply.version != 1 {
        return Err(invalid());
    }
    match (reply.published, reply.retirement) {
        (false, None) => Ok(RetiringCas::Stale),
        (true, Some(receipt)) => {
            let keys = next.retirement_keys();
            if receipt.generation != next.generation
                || receipt.outcomes.len() != keys.len()
                || receipt
                    .outcomes
                    .iter()
                    .zip(keys)
                    .any(|(outcome, key)| outcome.key != key)
            {
                return Err(invalid());
            }
            Ok(RetiringCas::Published(receipt))
        }
        _ => Err(invalid()),
    }
}

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
/// separate bounds so one complete 16 MiB chunk or two 8 MiB chunks fit.
fn bulk_batch_len(chunks: &[Chunk]) -> usize {
    let mut payload = 0usize;
    let mut wire = BULK_HEADER_BYTES;
    let mut count = 0;
    for chunk in chunks.iter().take(MAX_BULK_KEYS) {
        if chunk.len == 0 || chunk.len > MAX_BULK_CHUNK_BYTES as u64 {
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

fn decode_bulk_response(chunks: &[Chunk], bytes: &[u8]) -> io::Result<VerifiedChunks> {
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
        result.push(data.to_vec());
        offset = end;
    }
    if offset != bytes.len() {
        return Err(invalid());
    }
    VerifiedChunks::verify(chunks, result)
}

const WITNESS_HEADER: &str = "x-git-zero-internal-generation-witness";
const WITNESS_DOMAIN: &str = "git-zero-generation-witness";
const MAX_SAFE_GENERATION: u64 = 9_007_199_254_740_991;

// Private per-registration capability. Never serialize or include it in metrics.
struct GenerationWitness {
    key: [u8; 32],
    session: String,
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut result = [0; N];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(result)
}

impl GenerationWitness {
    fn from_registration(value: &Value) -> Option<Self> {
        let config = value.get("generation_witness")?;
        if config.get("version")?.as_u64()? != 1 {
            return None;
        }
        let session = config.get("session")?.as_str()?;
        decode_hex::<16>(session)?;
        Some(Self {
            key: decode_hex(config.get("key")?.as_str()?)?,
            session: session.into(),
        })
    }

    fn message(&self, repo: &str, request: &Request, generation: u64) -> Option<Vec<u8>> {
        // URL paths forwarded by workerd are ASCII, including percent escapes.
        if !request.method.is_ascii() || !request.target.is_ascii() {
            return None;
        }
        serde_json::to_vec(&(
            WITNESS_DOMAIN,
            1,
            &self.session,
            repo,
            &request.method,
            &request.target,
            generation,
        ))
        .ok()
    }

    fn verify(&self, repo: &str, request: &Request) -> Option<u64> {
        let header = request.headers.get(WITNESS_HEADER)?;
        if header.len() > 256 {
            return None;
        }
        let fields: Vec<_> = header.split(':').collect();
        if fields.len() != 4 || fields[0] != "1" || fields[1] != self.session {
            return None;
        }
        let generation: u64 = fields[2].parse().ok()?;
        if generation > MAX_SAFE_GENERATION || generation.to_string() != fields[2] {
            return None;
        }
        let signature = decode_hex::<32>(fields[3])?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
        mac.update(&self.message(repo, request, generation)?);
        mac.verify_slice(&signature).ok()?;
        Some(generation)
    }
}

pub struct HttpServices {
    authority: String,
    repo: String,
    token: String,
    phase: Mutex<String>,
    generation_witness: Mutex<Option<GenerationWitness>>,
    manifest_retire: AtomicBool,
    manifest_upload_retire: AtomicBool,
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
            generation_witness: Mutex::new(None),
            manifest_retire: AtomicBool::new(false),
            manifest_upload_retire: AtomicBool::new(false),
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
        } else if matches!(route, "manifest/retire" | "manifest/upload-retire") {
            MAX_RETIREMENT_RESPONSE
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
        // Reset before all validation/I/O: failed registration cannot retain
        // capabilities or witnesses from an earlier session.
        self.manifest_retire.store(false, Ordering::Release);
        self.manifest_upload_retire.store(false, Ordering::Release);
        *self
            .generation_witness
            .lock()
            .map_err(|_| io::Error::other("witness config lock poisoned"))? = None;
        // Reuse strict loopback validation so registration cannot enable SSRF.
        Self::new(codec_origin, &self.repo, &self.token)?;
        let response = self.json(
            "POST",
            "register",
            Some(&json!({"codec_origin": codec_origin, "generation_witness": 1, "manifest_retire": 1, "manifest_upload_retire": 1, "catalog_peak": 1})),
            Some("stats"),
        )?;
        *self
            .generation_witness
            .lock()
            .map_err(|_| io::Error::other("witness config lock poisoned"))? =
            GenerationWitness::from_registration(&response);
        self.manifest_retire
            .store(retirement_supported(&response), Ordering::Release);
        self.manifest_upload_retire
            .store(upload_supported(&response), Ordering::Release);
        Ok(())
    }

    pub(crate) fn generation_witness_matches(
        &self,
        request: &Request,
        generation: u64,
    ) -> io::Result<bool> {
        Ok(self
            .generation_witness
            .lock()
            .map_err(|_| io::Error::other("witness config lock poisoned"))?
            .as_ref()
            .and_then(|config| config.verify(&self.repo, request))
            == Some(generation))
    }

    pub fn unregister(&self) -> io::Result<()> {
        self.manifest_retire.store(false, Ordering::Release);
        self.manifest_upload_retire.store(false, Ordering::Release);
        *self
            .generation_witness
            .lock()
            .map_err(|_| io::Error::other("witness config lock poisoned"))? = None;
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
        self.r2_get_many_verified(chunks)?.into_data(chunks)
    }
    fn r2_get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
        // Validate the complete request before issuing even its first read.
        validate_bulk_chunks(chunks)?;
        let mut result = VerifiedChunks::verify(&[], Vec::with_capacity(chunks.len()))?;
        let mut offset = 0;
        while offset < chunks.len() {
            let count = bulk_batch_len(&chunks[offset..]);
            if count == 0 {
                let chunk = &chunks[offset];
                let data = self.r2_get(&chunk.key)?;
                result.append(VerifiedChunks::verify(
                    std::slice::from_ref(chunk),
                    vec![data],
                )?);
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
            result.append(decode_bulk_response(batch, &response.body)?);
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
    fn try_compare_and_swap_retiring(
        &self,
        expected: u64,
        next: &Manifest,
    ) -> io::Result<RetiringCas> {
        // Capability and debt bounds are resolved before any mutation. An old
        // Worker is never probed with a request that could already publish.
        if !self.manifest_retire.load(Ordering::Acquire) || next.garbage.len() > MAX_RETIREMENT_KEYS
        {
            return Ok(RetiringCas::Unsupported);
        }
        if expected.checked_add(1) != Some(next.generation) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid retiring generation",
            ));
        }
        validate_retiring_manifest(next)?;
        let body = serde_json::to_vec(&json!({"version": 1, "expected": expected, "next": next}))?;
        if body.len() > MAX_MANIFEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "retiring request too large",
            ));
        }
        let response = self.call("POST", "manifest/retire", &body, None)?;
        // Errors here remain errors: 404, malformed and lost replies may all
        // follow a committed CAS, and must never trigger scalar fallback.
        decode_retirement_response(next, &response.body)
    }

    fn try_compare_and_swap_upload_retiring(
        &self,
        expected: u64,
        next: &Manifest,
        pending: &PendingChunk,
    ) -> io::Result<UploadRetiringCas> {
        if !self.manifest_upload_retire.load(Ordering::Acquire) {
            return Ok(UploadRetiringCas::Unsupported);
        }
        let Some(body) = upload_frame(expected, next, pending)? else {
            return Ok(UploadRetiringCas::Unsupported);
        };
        let response = self.call("POST", "manifest/upload-retire", &body, None)?;
        decode_upload_response(next, pending, &response.body)
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
    fn get_many_verified(&self, chunks: &[Chunk]) -> io::Result<VerifiedChunks> {
        match self {
            Self::Local(b) => b.get_many_verified(chunks),
            Self::Cloudflare(b, _) => b.get_many_verified(chunks),
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
    fn try_compare_and_swap_retiring(
        &self,
        expected: u64,
        next: &Manifest,
    ) -> io::Result<RetiringCas> {
        match self {
            Self::Local(s) => s.try_compare_and_swap_retiring(expected, next),
            Self::Cloudflare(s) => s.try_compare_and_swap_retiring(expected, next),
        }
    }

    fn try_compare_and_swap_upload_retiring(
        &self,
        expected: u64,
        next: &Manifest,
        pending: &PendingChunk,
    ) -> io::Result<UploadRetiringCas> {
        match self {
            Self::Local(s) => s.try_compare_and_swap_upload_retiring(expected, next, pending),
            Self::Cloudflare(s) => s.try_compare_and_swap_upload_retiring(expected, next, pending),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn witness() -> GenerationWitness {
        GenerationWitness {
            key: [11; 32],
            session: "01".repeat(16),
        }
    }

    fn witness_request() -> Request {
        Request {
            method: "GET".into(),
            target: "/repo.git/info/refs?service=git-upload-pack&x=%2F".into(),
            headers: BTreeMap::new(),
            body: vec![],
        }
    }

    #[test]
    fn generation_witness_matches_standard_hmac_and_binds_authority_and_exact_request() {
        let config = witness();
        let mut request = witness_request();
        // Independent Python stdlib hmac/hashlib SHA-256 vector for this exact
        // canonical JSON, also exercised against WebCrypto by workerd tests.
        let signature = "d54a7a042252a66fc453bf0dab6970062fb04e23af0b4f05411aa347c54d8a45";
        let proof = format!("1:{}:7:{signature}", config.session);
        request.headers.insert(WITNESS_HEADER.into(), proof.clone());
        assert_eq!(config.verify("demo", &request), Some(7));
        assert_eq!(config.verify("other", &request), None);
        assert_eq!(
            GenerationWitness {
                session: "02".repeat(16),
                ..witness()
            }
            .verify("demo", &request),
            None
        );
        assert_eq!(
            GenerationWitness {
                key: [12; 32],
                ..witness()
            }
            .verify("demo", &request),
            None
        );
        for target in [
            "/repo.git/info/refs?service=git-upload-pack&x=%2f",
            "/repo.git/info/refs?x=%2F&service=git-upload-pack",
            "/repo.git/info/refs?service=git-receive-pack&x=%2F",
            "/repo.git/git-upload-pack",
        ] {
            let mut changed = request.clone();
            changed.target = target.into();
            assert_eq!(config.verify("demo", &changed), None);
        }
        let mut changed = request.clone();
        changed.method = "POST".into();
        assert_eq!(config.verify("demo", &changed), None);
        for invalid in [
            proof.replacen(":7:", ":8:", 1),
            proof.replacen("1:", "2:", 1),
            proof.replacen(":7:", ":07:", 1),
            proof.replacen(":7:", ":9007199254740992:", 1),
            format!("1:{}:7:{}", config.session, "0".repeat(64)),
            format!("{proof}:extra"),
            proof[..proof.len() - 1].into(),
            "x".repeat(257),
            String::new(),
        ] {
            changed.headers.insert(WITNESS_HEADER.into(), invalid);
            assert_eq!(config.verify("demo", &changed), None);
        }
        changed.headers.clear();
        assert_eq!(config.verify("demo", &changed), None);
    }

    #[test]
    fn generation_witness_registration_compatibility_defaults_to_callbacks() {
        let valid = json!({"generation_witness": {
            "version": 1, "key": "0b".repeat(32), "session": "01".repeat(16)
        }});
        assert!(GenerationWitness::from_registration(&valid).is_some());
        for response in [
            json!({"registered": true}),
            json!({"generation_witness": null}),
            json!({"generation_witness": {"version": 2, "key": "0b".repeat(32), "session": "01".repeat(16)}}),
            json!({"generation_witness": {"version": 1, "key": "0b".repeat(31), "session": "01".repeat(16)}}),
            json!({"generation_witness": {"version": 1, "key": "0b".repeat(32), "session": "bad"}}),
        ] {
            assert!(GenerationWitness::from_registration(&response).is_none());
        }
        let services = HttpServices::new("http://127.0.0.1:1", "demo", "token").unwrap();
        let mut request = witness_request();
        request.headers.insert(
            WITNESS_HEADER.into(),
            format!(
                "1:{}:7:{}",
                witness().session,
                "d54a7a042252a66fc453bf0dab6970062fb04e23af0b4f05411aa347c54d8a45"
            ),
        );
        assert!(!services.generation_witness_matches(&request, 7).unwrap());
        *services.generation_witness.lock().unwrap() = Some(witness());
        assert!(services.generation_witness_matches(&request, 7).unwrap());
        assert!(!services.generation_witness_matches(&request, 8).unwrap());
    }

    fn chunk(data: &[u8]) -> Chunk {
        Chunk {
            key: format!("chunks/{}", crate::hash(data)),
            len: data.len() as u64,
        }
    }

    fn staged_upload(bytes: Vec<u8>) -> (Manifest, PendingChunk) {
        let pending = PendingChunk::new(bytes).unwrap();
        (
            Manifest {
                generation: 1,
                packs: vec![crate::storage::StoredPack {
                    id: "upload".into(),
                    bundle: vec![pending.chunk().clone()],
                    pack_bytes: pending.chunk().len,
                    ..crate::storage::StoredPack::default()
                }],
                ..Manifest::default()
            },
            pending,
        )
    }

    #[test]
    fn upload_frame_respects_exact_wire_metadata_and_chunk_bounds_before_io() {
        let (mut next, pending) = staged_upload(vec![b'x'; MAX_BULK_CHUNK_BYTES]);
        let body = upload_frame(0, &next, &pending).unwrap().unwrap();
        assert_eq!(&body[..4], b"GZUP");
        assert_eq!(u32::from_le_bytes(body[4..8].try_into().unwrap()), 1);
        let metadata_len = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
        let metadata: Value = serde_json::from_slice(&body[12..12 + metadata_len]).unwrap();
        assert_eq!(metadata["chunk"], json!(pending.chunk()));
        assert_eq!(&body[12 + metadata_len..], pending.bytes());
        // Find the exact whole-wire ceiling with valid, bounded debt. The
        // metadata JSON grows one byte at a time with its string value.
        next.refs.insert("refs/tags/padding".into(), "a".repeat(40));
        let baseline = upload_frame(0, &next, &pending).unwrap().unwrap().len();
        let padding = MAX_BULK_RESPONSE - baseline;
        // Pack IDs are opaque immutable codec identifiers at this boundary.
        next.packs[0].id.push_str(&"p".repeat(padding));
        assert_eq!(
            upload_frame(0, &next, &pending).unwrap().unwrap().len(),
            MAX_BULK_RESPONSE
        );
        next.packs[0].id.push('p');
        assert!(upload_frame(0, &next, &pending).unwrap().is_none());
        let (mut next, pending) = staged_upload(vec![b'x']);
        next.packs[0].id = "x".repeat(MAX_MANIFEST_BYTES);
        assert!(upload_frame(0, &next, &pending).unwrap().is_none());
        next.packs[0].id.clear();
        next.garbage = (0..=MAX_RETIREMENT_KEYS)
            .map(|i| format!("chunks/{i:064x}"))
            .collect();
        assert!(upload_frame(0, &next, &pending).unwrap().is_none());
        assert!(PendingChunk::new(vec![0; MAX_BULK_CHUNK_BYTES + 1]).is_err());
        assert!(PendingChunk::new(Vec::new()).is_err());
    }

    #[test]
    fn upload_receipts_require_exact_uploaded_member_and_complete_ordered_retirement() {
        let (mut next, pending) = staged_upload(b"tail".to_vec());
        next.garbage = vec![chunk(b"a").key, chunk(b"b").key];
        let valid = json!({"version":1, "uploaded":pending.chunk(), "published":true,
            "retirement":{"generation":1,"outcomes":[{"key":next.garbage[0],"deleted":true},{"key":next.garbage[1],"deleted":false}]}});
        assert!(matches!(
            decode_upload_response(&next, &pending, &serde_json::to_vec(&valid).unwrap()).unwrap(),
            UploadRetiringCas::Published { .. }
        ));
        let stale =
            json!({"version":1,"uploaded":pending.chunk(),"published":false,"retirement":null});
        assert!(matches!(
            decode_upload_response(&next, &pending, &serde_json::to_vec(&stale).unwrap()).unwrap(),
            UploadRetiringCas::Stale(_)
        ));
        for (pointer, value) in [
            ("/version", json!(2)),
            ("/uploaded", Value::Null),
            ("/uploaded/key", json!(chunk(b"different").key)),
            ("/uploaded/len", json!(5)),
            ("/published", json!(false)),
            ("/retirement", Value::Null),
            ("/retirement/generation", json!(2)),
            ("/retirement/outcomes", json!([])),
            ("/retirement/outcomes/0/key", json!(next.garbage[1])),
            ("/retirement/outcomes/1/key", json!(next.garbage[0])),
            ("/retirement/outcomes/0/deleted", json!(1)),
        ] {
            let mut changed = valid.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                decode_upload_response(&next, &pending, &serde_json::to_vec(&changed).unwrap())
                    .is_err(),
                "{changed}"
            );
        }
        for field in ["version", "uploaded", "published", "retirement"] {
            let mut changed = stale.clone();
            changed.as_object_mut().unwrap().remove(field);
            assert!(
                decode_upload_response(&next, &pending, &serde_json::to_vec(&changed).unwrap())
                    .is_err()
            );
        }
        for location in ["", "/uploaded", "/retirement", "/retirement/outcomes/0"] {
            let mut changed = valid.clone();
            changed.pointer_mut(location).unwrap()["extra"] = json!(1);
            assert!(
                decode_upload_response(&next, &pending, &serde_json::to_vec(&changed).unwrap())
                    .is_err()
            );
        }
        let encoded = serde_json::to_string(&stale).unwrap();
        for bytes in [
            encoded
                .replacen("\"version\":1", "\"version\":1,\"version\":1", 1)
                .into_bytes(),
            encoded
                .replacen("\"len\":4", "\"len\":4,\"len\":4", 1)
                .into_bytes(),
            format!("{encoded}x").into_bytes(),
            encoded.as_bytes()[..encoded.len() - 1].to_vec(),
            vec![b' '; MAX_RETIREMENT_RESPONSE + 1],
        ] {
            assert!(decode_upload_response(&next, &pending, &bytes).is_err());
        }
    }

    #[test]
    fn unknown_upload_capability_is_side_effect_free_and_attempted_errors_never_fallback() {
        let (next, pending) = staged_upload(b"tail".to_vec());
        let services = HttpServices::new("http://127.0.0.1:1", "unused", "token").unwrap();
        for value in [
            json!({}),
            json!({"manifest_upload_retire":null}),
            json!({"manifest_upload_retire":{"version":1}}),
            json!({"manifest_upload_retire":{"version":2,"wire_bytes":MAX_BULK_RESPONSE,"metadata_bytes":MAX_MANIFEST_BYTES,"chunk_bytes":MAX_BULK_CHUNK_BYTES,"retirement_keys":MAX_RETIREMENT_KEYS}}),
        ] {
            assert!(!upload_supported(&value));
            assert_eq!(
                services
                    .try_compare_and_swap_upload_retiring(0, &next, &pending)
                    .unwrap(),
                UploadRetiringCas::Unsupported
            );
        }
        let capability = json!({"manifest_upload_retire":{"version":1,"wire_bytes":MAX_BULK_RESPONSE,"metadata_bytes":MAX_MANIFEST_BYTES,"chunk_bytes":MAX_BULK_CHUNK_BYTES,"retirement_keys":MAX_RETIREMENT_KEYS}});
        assert!(upload_supported(&capability));
        for field in [
            "wire_bytes",
            "metadata_bytes",
            "chunk_bytes",
            "retirement_keys",
        ] {
            let mut bad = capability.clone();
            bad["manifest_upload_retire"][field] = json!(1);
            assert!(!upload_supported(&bad));
        }
        for (status, body) in [
            (404, b"missing".to_vec()),
            (500, b"lost committed response".to_vec()),
            (200, b"{}".to_vec()),
            (200, vec![0; MAX_RETIREMENT_RESPONSE + 1]),
        ] {
            let (services, server) = fake_services(vec![(
                "POST /_services/manifest/upload-retire ".into(),
                Vec::new(),
                status,
                body,
            )]);
            services
                .manifest_upload_retire
                .store(true, Ordering::Release);
            assert!(
                services
                    .try_compare_and_swap_upload_retiring(0, &next, &pending)
                    .is_err()
            );
            server.join().unwrap();
        }
        // Invalid re-registration and unregister reset even previously enabled
        // state before the failed validation/network exchange.
        services
            .manifest_upload_retire
            .store(true, Ordering::Release);
        assert!(services.register("https://outside.invalid").is_err());
        assert!(!services.manifest_upload_retire.load(Ordering::Acquire));
        services
            .manifest_upload_retire
            .store(true, Ordering::Release);
        assert!(services.unregister().is_err());
        assert!(!services.manifest_upload_retire.load(Ordering::Acquire));
    }

    #[test]
    fn retirement_decoder_requires_exact_complete_ordered_receipts() {
        let next = Manifest {
            generation: 7,
            garbage: vec![chunk(b"a").key, chunk(b"b").key],
            ..Manifest::default()
        };
        let valid = json!({"version":1, "published":true, "retirement": {
            "generation":7, "outcomes":[
                {"key":next.garbage[0], "deleted":true},
                {"key":next.garbage[1], "deleted":false}
            ]
        }});
        assert!(matches!(
            decode_retirement_response(&next, &serde_json::to_vec(&valid).unwrap()).unwrap(),
            RetiringCas::Published(_)
        ));
        assert_eq!(
            decode_retirement_response(
                &next,
                br#"{"version":1,"published":false,"retirement":null}"#
            )
            .unwrap(),
            RetiringCas::Stale
        );
        let mut invalid = vec![
            json!({"version":1,"published":false}),
            json!({"version":1,"published":true,"retirement":null}),
            json!({"version":1,"published":false,"retirement":valid["retirement"]}),
        ];
        for (pointer, value) in [
            ("/version", json!(2)),
            ("/published", json!("true")),
            ("/retirement/generation", json!(8)),
            ("/retirement/outcomes/0/deleted", json!(1)),
            ("/retirement/outcomes/0/key", json!(next.garbage[1])),
            ("/retirement/outcomes/1/key", json!(next.garbage[0])),
            ("/retirement/outcomes", json!([])),
            (
                "/retirement/outcomes",
                json!([valid["retirement"]["outcomes"][0].clone()]),
            ),
            (
                "/retirement/outcomes",
                json!([
                    valid["retirement"]["outcomes"][1].clone(),
                    valid["retirement"]["outcomes"][0].clone()
                ]),
            ),
        ] {
            let mut changed = valid.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            invalid.push(changed);
        }
        let mut extra = valid.clone();
        extra["retirement"]["outcomes"][0]["extra"] = json!(true);
        invalid.push(extra);
        for value in invalid {
            assert!(
                decode_retirement_response(&next, &serde_json::to_vec(&value).unwrap()).is_err(),
                "{value}"
            );
        }
        for bytes in [
            br#"{"version":1,"version":1,"published":false,"retirement":null}"#.to_vec(),
            br#"{"version":1,"published":false,"retirement":null,"retirement":null}"#.to_vec(),
            vec![b' '; MAX_RETIREMENT_RESPONSE + 1],
            b"truncated".to_vec(),
        ] {
            assert!(decode_retirement_response(&next, &bytes).is_err());
        }
    }

    #[test]
    fn retirement_capability_and_bounds_choose_fallback_before_io() {
        let services = HttpServices::new("http://127.0.0.1:1", "unused", "token").unwrap();
        let mut next = Manifest {
            generation: 1,
            ..Manifest::default()
        };
        for value in [
            json!({}),
            json!({"manifest_retire":null}),
            json!({"manifest_retire":{"version":2}}),
            json!({"manifest_retire":1}),
        ] {
            assert!(!retirement_supported(&value));
            services
                .manifest_retire
                .store(retirement_supported(&value), Ordering::Release);
            assert_eq!(
                services.try_compare_and_swap_retiring(0, &next).unwrap(),
                RetiringCas::Unsupported
            );
        }
        assert!(retirement_supported(
            &json!({"manifest_retire":{"version":1}})
        ));
        services.manifest_retire.store(true, Ordering::Release);
        next.garbage = (0..=MAX_RETIREMENT_KEYS)
            .map(|i| format!("chunks/{i:064x}"))
            .collect();
        assert_eq!(
            services.try_compare_and_swap_retiring(0, &next).unwrap(),
            RetiringCas::Unsupported
        );
        next.garbage = vec![chunk(b"a").key; 2];
        assert_eq!(
            services
                .try_compare_and_swap_retiring(0, &next)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        next.garbage = vec!["chunks/../bad".into()];
        assert_eq!(
            services
                .try_compare_and_swap_retiring(0, &next)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn retirement_http_errors_never_return_unsupported() {
        for (status, body) in [(404, b"missing".to_vec()), (200, b"malformed".to_vec())] {
            let (services, server) = fake_services(vec![(
                "POST /_services/manifest/retire ".into(),
                vec![],
                status,
                body,
            )]);
            services.manifest_retire.store(true, Ordering::Release);
            let next = Manifest {
                generation: 1,
                ..Manifest::default()
            };
            assert!(services.try_compare_and_swap_retiring(0, &next).is_err());
            server.join().unwrap();
        }
    }

    #[test]
    fn unknown_registration_capability_uses_scalar_cas_without_probing_fused_route() {
        for capability in [
            json!({"registered":true}),
            json!({"registered":true,"manifest_retire":{"version":2}}),
        ] {
            let (services, server) = fake_services(vec![
                (
                    "POST /_services/register ".into(),
                    vec![],
                    200,
                    serde_json::to_vec(&capability).unwrap(),
                ),
                (
                    "POST /_services/manifest ".into(),
                    vec![],
                    200,
                    br#"{"published":true}"#.to_vec(),
                ),
            ]);
            services.register("http://127.0.0.1:1234").unwrap();
            let services = Arc::new(services);
            let repository = crate::Repository::new(
                R2BlobStore::new(services.clone()),
                DurableStateStore::new(services),
            );
            assert!(matches!(
                repository
                    .publish_retiring(&Manifest::default(), Manifest::default())
                    .unwrap(),
                RetiringCas::Published(_)
            ));
            server.join().unwrap();
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
        let mut full = chunk(b"a");
        full.len = MAX_BULK_CHUNK_BYTES as u64;
        assert_eq!(bulk_batch_len(std::slice::from_ref(&full)), 1);
        assert_eq!(bulk_batch_len(&[full, chunk(b"a")]), 1);
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
        for len in [0, MAX_BULK_CHUNK_BYTES as u64 + 1, u64::MAX] {
            chunks[0].len = len;
            assert_eq!(bulk_batch_len(&chunks), 0);
        }
        let services = HttpServices::new("http://127.0.0.1:1", "unused", "token").unwrap();
        assert!(services.r2_get_many(&[]).unwrap().is_empty());
        assert!(
            services
                .r2_get_many_verified(&[])
                .unwrap()
                .into_data(&[])
                .unwrap()
                .is_empty()
        );
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
            decode_bulk_response(&chunks, &valid)
                .unwrap()
                .into_data(&chunks)
                .unwrap(),
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
            decode_bulk_response(&chunks, &valid)
                .unwrap()
                .into_data(&chunks)
                .unwrap(),
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
                if method.starts_with("POST /_services/blobs/read ") {
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(
                        serde_json::from_value::<Vec<Chunk>>(value["chunks"].clone()).unwrap(),
                        chunks
                    );
                } else if method.starts_with("POST /_services/register ") {
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["catalog_peak"], 1);
                } else if !method.starts_with("POST") {
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
    fn bulk_bridge_reads_one_full_cloud_chunk_before_the_next_byte() {
        let full = vec![0x63; MAX_BULK_CHUNK_BYTES];
        let chunks = vec![chunk(&full), chunk(b"d")];
        let (services, server) = fake_services(vec![
            (
                "POST /_services/blobs/read ".into(),
                chunks[..1].to_vec(),
                200,
                frame(&[&full]),
            ),
            (
                "POST /_services/blobs/read ".into(),
                chunks[1..].to_vec(),
                200,
                frame(&[b"d"]),
            ),
        ]);
        assert_eq!(
            services
                .r2_get_many_verified(&chunks)
                .unwrap()
                .into_data(&chunks)
                .unwrap(),
            vec![full, b"d".to_vec()]
        );
        server.join().unwrap();
    }

    #[test]
    fn bulk_bridge_batches_repeated_keys_and_preserves_large_legacy_scalar_reads() {
        let chunks = vec![chunk(b"a"); MAX_BULK_KEYS + 1];
        let legacy = vec![0x62; MAX_BULK_CHUNK_BYTES + 1];
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
        request.push(chunk(b"a"));
        assert_eq!(
            services
                .r2_get_many_verified(&request)
                .unwrap()
                .into_data(&request)
                .unwrap(),
            [
                vec![b"a".to_vec(); chunks.len()],
                vec![legacy, b"a".to_vec()]
            ]
            .concat()
        );
        // A later call performs another RPC; no chunk cache survives the call.
        assert_eq!(
            services.r2_get_many(&[chunk(b"a")]).unwrap(),
            vec![b"a".to_vec()]
        );
        server.join().unwrap();
    }

    #[test]
    fn bulk_bridge_rejects_corrupt_first_batch_before_any_later_read() {
        let chunks = vec![chunk(b"a"); MAX_BULK_KEYS + 1];
        let mut corrupt = frame(&vec![b"a".as_slice(); MAX_BULK_KEYS]);
        *corrupt.last_mut().unwrap() ^= 1;
        for verified in [false, true] {
            let (services, server) = fake_services(vec![
                (
                    "POST /_services/blobs/read ".into(),
                    chunks[..MAX_BULK_KEYS].to_vec(),
                    200,
                    corrupt.clone(),
                ),
                (
                    "POST /_services/blobs/read ".into(),
                    vec![chunk(b"z")],
                    200,
                    frame(&[b"z"]),
                ),
            ]);
            let result = if verified {
                services
                    .r2_get_many_verified(&chunks)
                    .and_then(|proof| proof.into_data(&chunks))
            } else {
                services.r2_get_many(&chunks)
            };
            assert!(result.unwrap_err().to_string().contains("corrupt chunk"));
            // The server's next expected request is this independent read,
            // proving no later batch was requested after the corrupt first one.
            assert_eq!(
                services.r2_get_many(&[chunk(b"z")]).unwrap(),
                vec![b"z".to_vec()]
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn bulk_bridge_rejects_corrupt_scalar_fallback_before_resuming_bulk_reads() {
        let mut corrupt = vec![0x62; MAX_BULK_CHUNK_BYTES + 1];
        let legacy = chunk(&corrupt);
        corrupt[0] ^= 1;
        let chunks = [legacy.clone(), chunk(b"a")];
        for verified in [false, true] {
            let (services, server) = fake_services(vec![
                (
                    format!("GET /_services/blob/{} ", legacy.key),
                    vec![],
                    200,
                    corrupt.clone(),
                ),
                (
                    "POST /_services/blobs/read ".into(),
                    vec![chunk(b"z")],
                    200,
                    frame(&[b"z"]),
                ),
            ]);
            let result = if verified {
                services
                    .r2_get_many_verified(&chunks)
                    .and_then(|proof| proof.into_data(&chunks))
            } else {
                services.r2_get_many(&chunks)
            };
            assert!(result.unwrap_err().to_string().contains("corrupt chunk"));
            assert_eq!(
                services.r2_get_many(&[chunk(b"z")]).unwrap(),
                vec![b"z".to_vec()]
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn host_blob_wrapper_reads_verified_http_snapshot_in_order() {
        let chunks = vec![chunk(b"abc"), chunk(b"def")];
        let (services, server) = fake_services(vec![(
            "POST /_services/blobs/read ".into(),
            chunks.clone(),
            200,
            frame(&[b"abc", b"def"]),
        )]);
        let services = Arc::new(services);
        let repository = crate::Repository::new(
            HostBlobs::Cloudflare(R2BlobStore::new(services.clone()), services.clone()),
            DurableStateStore::new(services),
        );
        let manifest = Manifest {
            packs: vec![crate::storage::StoredPack {
                bundle: chunks,
                pack_bytes: 3,
                index_bytes: 3,
                ..crate::storage::StoredPack::default()
            }],
            ..Manifest::default()
        };
        assert_eq!(
            repository.read_snapshot_files(&manifest).unwrap(),
            vec![(b"abc".to_vec(), b"def".to_vec())]
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
