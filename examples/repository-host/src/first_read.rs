//! Private, request-local gateway continuation protocol. No object/session cache.
use crate::http::{Request, Response};
use crate::storage::{Chunk, Manifest};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io, sync::Arc};

pub(crate) const PLAN_PATH: &str = "/_git_zero_internal/fetch-plan";
pub(crate) const CONTINUE_PATH: &str = "/_git_zero_internal/fetch-continue";
pub(crate) const AUTH_HEADER: &str = "x-git-zero-internal-fetch-auth";
pub(crate) const MAX_ORIGINAL: usize = 128 * 1024;
const MAX_METADATA: usize = 1_500_000;
const MAX_BULK: usize = 16 * 1024 * 1024 + 2060;
const HEADER: usize = 20;
pub(crate) const MAX_FRAME: usize = HEADER + MAX_METADATA + MAX_ORIGINAL + MAX_BULK;
const VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"GZFR";

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid private first read envelope",
    )
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0; N];
    for (i, v) in out.iter_mut().enumerate() {
        *v = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Key {
    key: [u8; 32],
    session: [u8; 32],
    pub(crate) receive: bool,
}
impl Key {
    pub(crate) fn from_registration(
        v: &serde_json::Value,
        requested: bool,
    ) -> io::Result<Option<Self>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Capability {
            version: u32,
            body_bytes: usize,
            frame_bytes: usize,
            key: String,
            session: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Witness {
            version: u32,
            key: String,
            session: String,
        }
        let Some(value) = v.get("gateway_first_read") else {
            return Ok(None);
        };
        if !requested {
            return Err(invalid());
        }
        let config: Capability = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
        let witness: Witness =
            serde_json::from_value(v.get("generation_witness").ok_or_else(invalid)?.clone())
                .map_err(|_| invalid())?;
        if config.version != 1
            || config.body_bytes != MAX_ORIGINAL
            || config.frame_bytes != MAX_FRAME
            || witness.version != 1
            || witness.session != config.session
            || unhex::<32>(&witness.key).is_none()
            || unhex::<16>(&config.session).is_none()
        {
            return Err(invalid());
        }
        Ok(Some(Self {
            receive: false,
            key: unhex(&config.key).ok_or_else(invalid)?,
            session: config
                .session
                .as_bytes()
                .try_into()
                .map_err(|_| invalid())?,
        }))
    }
    // Receive is independently negotiated; old first-read clients remain fetch-only.
    pub(crate) fn with_receive(
        mut key: Option<Self>,
        v: &serde_json::Value,
        requested: bool,
    ) -> io::Result<Option<Self>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Capability {
            version: u32,
            body_bytes: usize,
            frame_bytes: usize,
            session: String,
        }
        if let Some(value) = v.get("gateway_first_receive") {
            let cap: Capability = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            let config = key.as_mut().ok_or_else(invalid)?;
            if !requested
                || cap.version != 1
                || cap.body_bytes != MAX_ORIGINAL
                || cap.frame_bytes != MAX_FRAME
                || cap.session != config.session()
            {
                return Err(invalid());
            }
            config.receive = true;
        }
        Ok(key)
    }
    pub(crate) fn session(&self) -> &str {
        std::str::from_utf8(&self.session).expect("ASCII session")
    }
    pub(crate) fn verify(&self, domain: &str, request: &Request) -> io::Result<()> {
        let signature = request
            .headers
            .get(AUTH_HEADER)
            .and_then(|v| unhex::<32>(v))
            .ok_or_else(invalid)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| invalid())?;
        mac.update(domain.as_bytes());
        mac.update(&[0]);
        mac.update(&request.body);
        mac.verify_slice(&signature).map_err(|_| invalid())
    }
    pub(crate) fn response(&self, domain: &str, body: Vec<u8>) -> io::Result<Response> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| invalid())?;
        mac.update(domain.as_bytes());
        mac.update(&[0]);
        mac.update(&body);
        Ok(Response {
            status: 200,
            headers: BTreeMap::from([
                ("Content-Type".into(), "application/octet-stream".into()),
                (AUTH_HEADER.into(), hex(&mac.finalize().into_bytes())),
            ]),
            body: Arc::from(body),
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Logical {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
}
impl Logical {
    pub(crate) fn request(&self, body: Vec<u8>) -> io::Result<Request> {
        if self.method != "POST"
            || !matches!(
                self.target.as_str(),
                "/repo.git/git-upload-pack" | "/repo.git/git-receive-pack"
            )
            || body.len() > MAX_ORIGINAL
            || self.headers.len() > 256
        {
            return Err(invalid());
        }
        let mut headers = BTreeMap::new();
        let mut total = self.target.len() + 16;
        for (name, value) in &self.headers {
            total += name.len() + value.len() + 4;
            if name.is_empty()
                || !name.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
                })
                || name.starts_with("x-git-zero-internal-")
                || name == "x-git-zero-retirement-proof"
                || value.bytes().any(|b| (b < 32 && b != b'\t') || b == 127)
                || name.len() + value.len() + 4 > 8192
                || total > 65536
                || headers.insert(name.clone(), value.clone()).is_some()
            {
                return Err(invalid());
            }
        }
        let mut request = Request {
            method: self.method.clone(),
            target: self.target.clone(),
            headers,
            body,
        };
        if request.target == "/repo.git/git-receive-pack"
            && (request.headers.contains_key("transfer-encoding")
                || request
                    .headers
                    .get("content-length")
                    .is_none_or(|n| n.parse::<usize>().ok() != Some(request.body.len())))
        {
            return Err(invalid());
        }
        normalize(&mut request);
        Ok(request)
    }
}
// Framing/connection fields do not influence Git semantics; remove identically
// from ordinary and reconstructed requests before all-header cache hashing.
pub(crate) fn is_transport_header(name: &str) -> bool {
    [
        "host",
        "connection",
        "content-length",
        "transfer-encoding",
        "accept-encoding",
        "cf-worker",
        "cf-connecting-ip",
        AUTH_HEADER,
    ]
    .contains(&name)
}
pub(crate) fn normalize(request: &mut Request) {
    request.headers.retain(|name, _| !is_transport_header(name));
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub generation: u64,
    pub manifest: String,
    pub selection: String,
    #[serde(deserialize_with = "strict_chunks")]
    pub first: Vec<Chunk>,
}
fn strict_chunks<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Chunk>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictChunk {
        key: String,
        len: u64,
    }
    Ok(Vec::<StrictChunk>::deserialize(deserializer)?
        .into_iter()
        .map(|c| Chunk {
            key: c.key,
            len: c.len,
        })
        .collect())
}

// Registration preserves optional legacy fields, but duplicate fields at any
// depth must not select a different private capability/session across parsers.
pub(crate) fn strict_json(bytes: &[u8]) -> io::Result<serde_json::Value> {
    struct Unique(serde_json::Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON with unique object fields")
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Unique(n.into()))
                        .ok_or_else(|| E::custom("invalid JSON number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                    Ok(Unique(serde_json::Value::Null))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> Result<Unique, A::Error> {
                    let mut out = Vec::new();
                    while let Some(v) = a.next_element::<Unique>()? {
                        out.push(v.0);
                    }
                    Ok(Unique(out.into()))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> Result<Unique, A::Error> {
                    let mut out = serde_json::Map::new();
                    while let Some(k) = a.next_key::<String>()? {
                        if out.contains_key(&k) {
                            return Err(serde::de::Error::custom("duplicate JSON field"));
                        }
                        out.insert(k, a.next_value::<Unique>()?.0);
                    }
                    Ok(Unique(out.into()))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    serde_json::from_slice::<Unique>(bytes)
        .map(|v| v.0)
        .map_err(io::Error::other)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Planning {
    pub version: u32,
    pub session: String,
    pub nonce: String,
    pub repo: String,
    pub request: Logical,
}
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Planned {
    pub version: u32,
    pub session: String,
    pub nonce: String,
    pub request_hash: String,
    pub status: &'static str,
    pub plan: Option<Plan>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Continuing {
    pub version: u32,
    pub session: String,
    pub nonce: String,
    pub repo: String,
    pub request: Logical,
    pub request_hash: String,
    pub plan: Plan,
    pub generation: u64,
    pub matched: bool,
    pub witness: String,
}

pub(crate) fn identity(
    version: u32,
    session: &str,
    nonce: &str,
    repo: &str,
    key: &Key,
    actual_repo: &str,
) -> io::Result<()> {
    if version != VERSION
        || session != key.session()
        || unhex::<16>(nonce).is_none()
        || repo != actual_repo
    {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn decode<T: for<'a> Deserialize<'a>>(
    mut data: Vec<u8>,
) -> io::Result<(T, Vec<u8>, Vec<u8>)> {
    if data.len() < HEADER || data.len() > MAX_FRAME || &data[..4] != MAGIC {
        return Err(invalid());
    }
    let u = |p| u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
    if u(4) != VERSION as usize {
        return Err(invalid());
    }
    let (meta, body, bulk) = (u(8), u(12), u(16));
    if meta > MAX_METADATA
        || body > MAX_ORIGINAL
        || bulk > MAX_BULK
        || HEADER
            .checked_add(meta)
            .and_then(|n| n.checked_add(body))
            .and_then(|n| n.checked_add(bulk))
            != Some(data.len())
    {
        return Err(invalid());
    }
    let metadata = serde_json::from_slice(&data[HEADER..HEADER + meta]).map_err(|_| invalid())?;
    let bulk_data = data.split_off(HEADER + meta + body);
    let body_data = data.split_off(HEADER + meta);
    Ok((metadata, body_data, bulk_data))
}
pub(crate) fn encode<T: Serialize>(meta: &T) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(meta).map_err(io::Error::other)?;
    if bytes.len() > MAX_METADATA {
        return Err(invalid());
    }
    let mut out = Vec::with_capacity(HEADER + bytes.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&bytes);
    Ok(out)
}
fn field(h: &mut Sha256, b: &[u8]) {
    h.update((b.len() as u64).to_le_bytes());
    h.update(b);
}
pub(crate) fn request_hash(request: &Request) -> String {
    let mut h = Sha256::new();
    h.update(b"git-zero-fetch-request-v1");
    field(&mut h, request.method.as_bytes());
    field(&mut h, request.target.as_bytes());
    h.update((request.headers.len() as u64).to_le_bytes());
    for (name, value) in &request.headers {
        field(&mut h, name.as_bytes());
        field(&mut h, value.as_bytes());
    }
    field(&mut h, &request.body);
    hex(&h.finalize())
}
#[cfg(test)]
pub(crate) fn manifest_hash(manifest: &Manifest) -> String {
    hex(&manifest_digest(manifest))
}

pub(crate) fn manifest_digest(manifest: &Manifest) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"git-zero-fetch-manifest-v1");
    h.update(manifest.generation.to_le_bytes());
    h.update((manifest.packs.len() as u64).to_le_bytes());
    for p in &manifest.packs {
        field(&mut h, p.id.as_bytes());
        h.update(p.pack_bytes.to_le_bytes());
        h.update(p.index_bytes.to_le_bytes());
        h.update(p.level.to_le_bytes());
        for chunks in [&p.pack, &p.index, &p.bundle] {
            h.update((chunks.len() as u64).to_le_bytes());
            for c in chunks {
                field(&mut h, c.key.as_bytes());
                h.update(c.len.to_le_bytes());
            }
        }
    }
    h.update((manifest.refs.len() as u64).to_le_bytes());
    for (n, v) in &manifest.refs {
        field(&mut h, n.as_bytes());
        field(&mut h, v.as_bytes());
    }
    h.update((manifest.garbage.len() as u64).to_le_bytes());
    for k in &manifest.garbage {
        field(&mut h, k.as_bytes());
    }
    h.finalize().into()
}
pub(crate) fn selection_hash(
    selected: &[usize],
    projection: Option<(u64, [u8; 32])>,
    first: &[Chunk],
) -> String {
    let mut h = Sha256::new();
    h.update(b"git-zero-fetch-selection-v1");
    h.update([u8::from(projection.is_some())]);
    if let Some((cutoff, binding)) = projection {
        h.update(cutoff.to_le_bytes());
        h.update(binding);
    }
    h.update((selected.len() as u64).to_le_bytes());
    for p in selected {
        h.update((*p as u64).to_le_bytes());
    }
    h.update((first.len() as u64).to_le_bytes());
    for c in first {
        field(&mut h, c.key.as_bytes());
        h.update(c.len.to_le_bytes());
    }
    hex(&h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key() -> Key {
        Key {
            receive: false,
            key: [7; 32],
            session: *b"00000000000000000000000000000000",
        }
    }
    fn request(body: Vec<u8>) -> Request {
        Request {
            method: "POST".into(),
            target: PLAN_PATH.into(),
            headers: BTreeMap::new(),
            body,
        }
    }
    #[test]
    fn private_auth_binds_domain_every_byte_and_fixed_registration() {
        let key = key();
        let bytes = encode(&serde_json::json!({"test":"bounded"})).unwrap();
        let signed = key.response("plan", bytes.clone()).unwrap();
        let mut req = request(bytes);
        req.headers = signed.headers;
        key.verify("plan", &req).unwrap();
        assert!(key.verify("continue", &req).is_err());
        req.body.push(0);
        assert!(key.verify("plan", &req).is_err());
        req.body.pop();
        req.body[0] ^= 1;
        assert!(key.verify("plan", &req).is_err());
        assert!(
            identity(
                1,
                key.session(),
                "00000000000000000000000000000000",
                "one",
                &key,
                "two"
            )
            .is_err()
        );
        assert!(
            identity(
                2,
                key.session(),
                "00000000000000000000000000000000",
                "one",
                &key,
                "one"
            )
            .is_err()
        );
    }
    #[test]
    fn private_frames_reject_duplicate_unknown_lengths_and_original_header_conflicts() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Only {
            a: u32,
        }
        let good = encode(&serde_json::json!({"a":1})).unwrap();
        assert_eq!(decode::<Only>(good.clone()).unwrap().0.a, 1);
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(decode::<Only>(trailing).is_err());
        let mut oversize = good;
        oversize[12..16].copy_from_slice(&((MAX_ORIGINAL + 1) as u32).to_le_bytes());
        assert!(decode::<Only>(oversize).is_err());
        for json in [
            br#"{"a":1,"a":2}"#.as_slice(),
            br#"{"a":1,"extra":2}"#.as_slice(),
        ] {
            let mut raw = Vec::from(&b"GZFR"[..]);
            raw.extend_from_slice(&1u32.to_le_bytes());
            raw.extend_from_slice(&(json.len() as u32).to_le_bytes());
            raw.extend_from_slice(&[0; 8]);
            raw.extend_from_slice(json);
            assert!(decode::<Only>(raw).is_err());
        }
        let logical = Logical {
            method: "POST".into(),
            target: "/repo.git/git-upload-pack".into(),
            headers: vec![
                ("cookie".into(), "one".into()),
                ("cookie".into(), "two".into()),
            ],
        };
        assert!(logical.request(vec![]).is_err());
        let mut logical = logical;
        logical.headers = vec![(AUTH_HEADER.into(), "spoof".into())];
        assert!(logical.request(vec![]).is_err());
    }
    #[test]
    fn exact_original_and_bulk_frame_limits_fit_without_raising_incoming_limit() {
        let mut data = encode(&serde_json::json!({"bounded":true})).unwrap();
        data[12..16].copy_from_slice(&(MAX_ORIGINAL as u32).to_le_bytes());
        data[16..20].copy_from_slice(&(MAX_BULK as u32).to_le_bytes());
        data.resize(data.len() + MAX_ORIGINAL + MAX_BULK, 0);
        let (_, body, bulk) = decode::<serde_json::Value>(data.clone()).unwrap();
        assert_eq!(body.len(), MAX_ORIGINAL);
        assert_eq!(bulk.len(), MAX_BULK);
        const {
            assert!(MAX_FRAME < crate::http::MAX_BODY);
        }
        data[12..16].copy_from_slice(&((MAX_ORIGINAL + 1) as u32).to_le_bytes());
        data.push(0);
        assert!(decode::<serde_json::Value>(data).is_err());
    }

    #[test]
    fn first_read_registration_is_optional_but_present_capability_is_strict_and_paired() {
        let key = key();
        let valid = serde_json::json!({"gateway_first_read":{"version":1,"body_bytes":MAX_ORIGINAL,"frame_bytes":MAX_FRAME,"key":"07".repeat(32),"session":key.session()},"generation_witness":{"version":1,"key":"08".repeat(32),"session":key.session()}});
        assert!(
            Key::from_registration(&serde_json::json!({}), true)
                .unwrap()
                .is_none()
        );
        assert!(Key::from_registration(&valid, true).unwrap().is_some());
        assert!(Key::from_registration(&valid, false).is_err());
        for field in ["version", "body_bytes", "frame_bytes", "key", "session"] {
            let mut invalid = valid.clone();
            invalid["gateway_first_read"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(Key::from_registration(&invalid, true).is_err());
        }
        for path in ["extra", "body_bytes", "key", "session"] {
            let mut invalid = valid.clone();
            invalid["gateway_first_read"][path] = serde_json::json!("bad");
            assert!(Key::from_registration(&invalid, true).is_err());
        }
        let mut invalid = valid.clone();
        invalid["generation_witness"]["session"] = serde_json::json!("1".repeat(32));
        assert!(Key::from_registration(&invalid, true).is_err());
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .remove("generation_witness");
        assert!(Key::from_registration(&invalid, true).is_err());
        assert!(
            Key::from_registration(&serde_json::json!({"gateway_first_read":null}), true).is_err()
        );
        for json in [
            br#"{"gateway_first_read":null,"gateway_first_read":null}"#.as_slice(),
            br#"{"gateway_first_read":{"version":1,"version":1}}"#.as_slice(),
        ] {
            assert!(strict_json(json).is_err());
        }
    }

    #[test]
    fn receive_capability_requires_explicit_request_exact_bounds_and_first_read_session() {
        let cap = serde_json::json!({"version":1,"body_bytes":MAX_ORIGINAL,"frame_bytes":MAX_FRAME,"session":key().session()});
        let valid = serde_json::json!({"gateway_first_receive":cap});
        assert!(
            Key::with_receive(Some(key()), &valid, true)
                .unwrap()
                .unwrap()
                .receive
        );
        assert!(
            !Key::with_receive(Some(key()), &serde_json::json!({}), true)
                .unwrap()
                .unwrap()
                .receive
        );
        assert!(Key::with_receive(None, &valid, true).is_err());
        assert!(Key::with_receive(Some(key()), &valid, false).is_err());
        for field in ["version", "body_bytes", "frame_bytes", "session"] {
            let mut invalid = valid.clone();
            invalid["gateway_first_receive"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(Key::with_receive(Some(key()), &invalid, true).is_err());
        }
        for (field, value) in [
            ("extra", serde_json::json!(1)),
            ("session", serde_json::json!("f".repeat(32))),
            ("body_bytes", serde_json::json!(MAX_ORIGINAL + 1)),
        ] {
            let mut invalid = valid.clone();
            invalid["gateway_first_receive"][field] = value;
            assert!(Key::with_receive(Some(key()), &invalid, true).is_err());
        }
        assert!(
            Key::with_receive(
                Some(key()),
                &serde_json::json!({"gateway_first_receive":null}),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn full_manifest_identity_includes_debt_order_lengths_unicode_and_representations() {
        let mut m = Manifest {
            generation: 3,
            refs: BTreeMap::from([
                ("refs/heads/\u{e000}".into(), "a".into()),
                ("refs/heads/\u{10000}".into(), "b".into()),
            ]),
            garbage: vec!["chunks/a".into(), "chunks/b".into()],
            ..Manifest::default()
        };
        let original = manifest_hash(&m);
        assert_eq!(
            original,
            "3679df2c35f192f8832484fb1d25d20a482f0819994a96b6202e87f9601f374a"
        );
        m.garbage.reverse();
        assert_ne!(original, manifest_hash(&m));
        m.garbage.reverse();
        m.generation += 1;
        assert_ne!(original, manifest_hash(&m));
        let mut request = request(vec![1, 2]);
        request.target = "/repo.git/git-upload-pack".into();
        request.headers.insert("authorization".into(), "one".into());
        let original = request_hash(&request);
        request.headers.insert("authorization".into(), "two".into());
        assert_ne!(original, request_hash(&request));
    }
}
