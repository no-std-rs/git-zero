//! Bounded blocking local WebSocket transport. Tungstenite owns RFC6455;
//! application framing never retries an attempted RPC. Large requests use
//! small RFC fragments, and replies use bounded application segments so the
//! library cannot retain maximum pack-sized input/output allocations.
use crate::http::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, io, net::TcpStream, time::Duration};
use tungstenite::{
    Message, WebSocket,
    client::{IntoClientRequest, client_with_config},
    protocol::{
        WebSocketConfig,
        frame::{
            Frame,
            coding::{Data, OpCode},
        },
    },
};

pub(crate) const PIECE: usize = 16 * 1024;
const DATA_HEADER: usize = 24;
const DATA_BYTES: usize = PIECE - DATA_HEADER;
const HEADERS: usize = 4096;
const BULK: usize = 16 * 1024 * 1024 + 2060;
const METADATA: usize = 1_500_000;
const RPCS: u32 = 16;
const PROTOCOL: &str = "git-zero-service-rpc.v1";
/// Audited conservative retained bound for small RFC-frame output, bounded
/// input/reassembly, controls and the live connection's owning state. This is
/// reserved jointly with response cache/certificates, not additional cache.
pub(crate) const RETAINED_BOUND: usize = 256 * 1024;
const _: () = {
    assert!(20 + HEADERS + BULK < 32 * 1024 * 1024);
};

pub(crate) struct Endpoint<'a> {
    pub(crate) authority: &'a str,
    pub(crate) repo: &'a str,
    pub(crate) token: &'a str,
}
pub(crate) struct Rpc<'a> {
    pub(crate) method: &'a str,
    pub(crate) route: &'a str,
    pub(crate) phase: &'a str,
    pub(crate) proof: Option<&'a str>,
    pub(crate) body: &'a [u8],
}

#[derive(Clone, Copy)]
pub(crate) struct Capability {
    session: [u8; 32],
}

impl Capability {
    pub(crate) fn parse(value: &Value, requested: bool) -> io::Result<Option<Self>> {
        let Some(capability) = value.get("service_socket") else {
            return Ok(None);
        };
        let session = capability
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or("");
        if requested
            && value.get("registered") == Some(&Value::Bool(true))
            && session.len() == 32
            && session
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && capability
                == &json!({ "version":1,"protocol":PROTOCOL,"piece_bytes":PIECE,
                "header_bytes":HEADERS,"body_bytes":BULK,"rpcs":RPCS,"session":session })
        {
            return Ok(Some(Self {
                session: session.as_bytes().try_into().unwrap(),
            }));
        }
        Err(invalid("invalid service socket registration capability"))
    }
    fn session(&self) -> &str {
        std::str::from_utf8(&self.session).expect("validated session")
    }
}

#[derive(Default)]
struct Frames {
    request_frame_attempts: u64,
    request_frames_written: u64,
    response_messages: u64,
    ack_attempts: u64,
    acks_written: u64,
}
impl Frames {
    fn add(&mut self, other: &Self) {
        self.request_frame_attempts += other.request_frame_attempts;
        self.request_frames_written += other.request_frames_written;
        self.response_messages += other.response_messages;
        self.ack_attempts += other.ack_attempts;
        self.acks_written += other.acks_written;
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) capability: Option<Capability>,
    connection: Option<Connection>,
    opens: u64,
    rpcs: u64,
    failures: u64,
    rotations: u64,
    unsupported_upgrades: u64,
    frames: Frames,
}

impl State {
    pub(crate) fn disconnect(&mut self) {
        self.connection = None;
    }
    pub(crate) fn reset(&mut self) {
        self.connection = None;
        self.capability = None;
    }
    pub(crate) fn diagnostics(&self) -> Value {
        json!({"upgrades_attempted":self.opens,"rpcs_completed":self.rpcs,"failures":self.failures,
            "rotations":self.rotations,"retained_reservation_bytes":RETAINED_BOUND,
            "maximum_rfc_fragment_payload":PIECE,"maximum_response_message":PIECE,
            "unsupported_upgrades":self.unsupported_upgrades,"connected":self.connection.is_some(),
            "request_rfc_frame_attempts":self.frames.request_frame_attempts,
            "request_rfc_frames_written":self.frames.request_frames_written,
            "response_binary_messages_read":self.frames.response_messages,
            "ack_frame_attempts":self.frames.ack_attempts,"ack_frames_written":self.frames.acks_written})
    }
    /// None chooses ordinary HTTP before application RPC/storage I/O. A known
    /// unsupported/capacity Upgrade is still a real counted Worker request.
    /// Once a
    /// frame might have reached the Worker, every failure drops the socket and
    /// returns the error; no resend or HTTP fallback is possible here.
    pub(crate) fn call(
        &mut self,
        endpoint: Endpoint<'_>,
        rpc: Rpc<'_>,
    ) -> Option<io::Result<Response>> {
        let Endpoint {
            authority,
            repo,
            token,
        } = endpoint;
        let Rpc {
            method,
            route,
            phase,
            proof,
            body,
        } = rpc;
        let capability = self.capability?;
        let (request_bound, response_bound) = bounds(method, route)?;
        if body.len() > request_bound || token.len() + repo.len() > HEADERS / 2 || !token.is_ascii()
        {
            return None;
        }
        let mut diagnostic = crate::diagnostics::Span::new("service_socket");
        diagnostic.field("method", method);
        diagnostic.field("route", route);
        diagnostic.field("request_body_bytes", body.len());
        diagnostic.field("reused_connection", self.connection.is_some());
        diagnostic.mark("socket_begin");
        if self.connection.is_none() {
            self.opens += 1;
            match Connection::open(authority, repo, token, phase, capability, &mut diagnostic) {
                Ok(Some(connection)) => self.connection = Some(connection),
                Ok(None) => {
                    self.unsupported_upgrades += 1;
                    diagnostic.field("fallback_before_rpc", true);
                    diagnostic.finish(&Ok::<_, io::Error>(()));
                    return None;
                }
                Err(error) => {
                    self.failures += 1;
                    diagnostic.finish(&Err::<(), _>(io::Error::new(
                        error.kind(),
                        "socket open failed",
                    )));
                    diagnostic.error(&error);
                    return Some(Err(error));
                }
            }
        }
        let result = (|| {
            let connection = self.connection.as_mut().expect("opened socket");
            let result = connection.call(
                method,
                route,
                phase,
                proof,
                body,
                response_bound,
                &mut diagnostic,
            );
            diagnostic.field(
                "request_frames_written",
                connection.frames.request_frames_written,
            );
            diagnostic.field(
                "response_messages_read",
                connection.frames.response_messages,
            );
            diagnostic.field("ack_frames_written", connection.frames.acks_written);
            self.frames.add(&connection.frames);
            connection.frames = Frames::default();
            let response = result?;
            self.rpcs += 1;
            if connection.sequence == RPCS {
                self.rotations += 1;
                self.connection = None;
            } else if !connection.usable {
                self.connection = None;
            }
            if !(200..300).contains(&response.status) {
                self.connection = None;
            }
            Ok(response)
        })();
        if result.is_err() {
            self.failures += 1;
            self.connection = None;
        }
        diagnostic.finish(&result);
        Some(result)
    }
}

fn bounds(method: &str, route: &str) -> Option<(usize, usize)> {
    match (method, route) {
        ("POST", "blobs/read") => Some((METADATA, BULK)),
        ("POST", "manifest/upload-retire") => Some((BULK, 32 * 1024)),
        ("PUT", route)
            if route.strip_prefix("blob/chunks/").is_some_and(|key| {
                key.len() == 64
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }) =>
        {
            Some((16 * 1024 * 1024, 32 * 1024))
        }
        _ => None,
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn socket_io_error(diagnostic: &mut crate::diagnostics::Span, e: &io::Error) {
    diagnostic.field("socket_io_error_kind", format!("{:?}", e.kind()));
    diagnostic.field("socket_io_raw_os_error", json!(e.raw_os_error()));
}
fn socket_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("service socket: {error}"))
}
fn number(bytes: &[u8], offset: usize) -> io::Result<u32> {
    bytes
        .get(offset..offset + 4)
        .and_then(|x| x.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| invalid("truncated service socket field"))
}
fn frame(magic: &[u8; 4], values: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + values.len() * 4);
    bytes.extend_from_slice(magic);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

struct Connection {
    socket: WebSocket<TcpStream>,
    sequence: u32,
    usable: bool,
    frames: Frames,
}
impl Connection {
    fn open(
        authority: &str,
        repo: &str,
        token: &str,
        phase: &str,
        capability: Capability,
        diagnostic: &mut crate::diagnostics::Span,
    ) -> io::Result<Option<Self>> {
        diagnostic.mark("socket_connect_begin");
        let stream = TcpStream::connect(authority)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        stream.set_write_timeout(Some(Duration::from_secs(60)))?;
        diagnostic.mark("socket_connected");
        let mut request = format!("ws://{authority}/_services/socket")
            .into_client_request()
            .map_err(socket_error)?;
        for (name, value) in [
            ("x-hosting-token", token),
            ("x-hosting-repo", repo),
            ("x-hosting-phase", phase),
            ("x-hosting-socket-session", capability.session()),
            ("sec-websocket-protocol", PROTOCOL),
        ] {
            request.headers_mut().insert(
                tungstenite::http::HeaderName::from_bytes(name.as_bytes()).map_err(socket_error)?,
                value.parse().map_err(socket_error)?,
            );
        }
        let config = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(0)
            .max_write_buffer_size(2 * PIECE)
            .max_message_size(Some(PIECE))
            .max_frame_size(Some(PIECE));
        diagnostic.mark("socket_upgrade_begin");
        let (socket, response) = match client_with_config(request, stream, Some(config)) {
            Ok(result) => result,
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)))
                if matches!(response.status().as_u16(), 404 | 426 | 429) =>
            {
                return Ok(None);
            }
            Err(error) => {
                if let tungstenite::HandshakeError::Failure(tungstenite::Error::Io(ref e)) = error {
                    socket_io_error(diagnostic, e);
                }
                return Err(socket_error(error));
            }
        };
        diagnostic.mark("socket_upgrade_complete");
        if response.headers().contains_key("sec-websocket-extensions")
            || response
                .headers()
                .get("sec-websocket-protocol")
                .is_none_or(|v| v != PROTOCOL)
        {
            return Err(invalid("unexpected service socket extension or protocol"));
        }
        Ok(Some(Self {
            socket,
            sequence: 0,
            usable: true,
            frames: Frames::default(),
        }))
    }
    fn binary(&mut self, diagnostic: &mut crate::diagnostics::Span) -> io::Result<Vec<u8>> {
        for _ in 0..16 {
            match self
                .socket
                .read()
                .inspect_err(|e| {
                    if let tungstenite::Error::Io(e) = e {
                        socket_io_error(diagnostic, e);
                    }
                })
                .map_err(socket_error)?
            {
                Message::Binary(bytes) if bytes.len() <= PIECE => {
                    self.frames.response_messages += 1;
                    return Ok(bytes.to_vec());
                }
                Message::Ping(_) | Message::Pong(_) => {
                    self.socket
                        .flush()
                        .inspect_err(|e| {
                            if let tungstenite::Error::Io(e) = e {
                                socket_io_error(diagnostic, e);
                            }
                        })
                        .map_err(socket_error)?;
                }
                _ => return Err(invalid("unexpected service socket message")),
            }
        }
        Err(invalid("too many service socket controls"))
    }
    fn call(
        &mut self,
        method: &str,
        route: &str,
        phase: &str,
        proof: Option<&str>,
        body: &[u8],
        maximum: usize,
        diagnostic: &mut crate::diagnostics::Span,
    ) -> io::Result<Response> {
        let metadata = serde_json::to_vec(
            &json!({"method":method,"route":route,"phase":phase,"proof":proof}),
        )?;
        if metadata.len() > HEADERS {
            return Err(invalid("service socket metadata too large"));
        }
        self.sequence += 1;
        let id = self.sequence;
        let mut prefix = frame(b"GZSQ", &[1, id, metadata.len() as u32, body.len() as u32]);
        prefix.extend_from_slice(&metadata);
        // Small RFC frames bound Tungstenite's retained output Vec capacity.
        // The runtime still receives exactly one complete application request.
        diagnostic.mark("socket_request_send_begin");
        self.frames.request_frame_attempts += 1;
        self.socket
            .send(Message::Frame(Frame::message(
                prefix,
                OpCode::Data(Data::Binary),
                body.is_empty(),
            )))
            .inspect_err(|e| {
                if let tungstenite::Error::Io(e) = e {
                    socket_io_error(diagnostic, e);
                }
            })
            .map_err(socket_error)?;
        self.frames.request_frames_written += 1;
        let chunks = body.chunks(PIECE);
        let count = chunks.len();
        for (index, part) in chunks.enumerate() {
            self.frames.request_frame_attempts += 1;
            self.socket
                .send(Message::Frame(Frame::message(
                    part.to_vec(),
                    OpCode::Data(Data::Continue),
                    index + 1 == count,
                )))
                .inspect_err(|e| {
                    if let tungstenite::Error::Io(e) = e {
                        socket_io_error(diagnostic, e);
                    }
                })
                .map_err(socket_error)?;
            self.frames.request_frames_written += 1;
        }
        diagnostic.mark("socket_request_sent");
        diagnostic.mark("socket_response_header_read_begin");
        let header = self.binary(diagnostic)?;
        if !header.starts_with(b"GZSR")
            || header.len() < 24
            || number(&header, 4)? != 1
            || number(&header, 8)? != id
        {
            return Err(invalid("invalid service socket response identity"));
        }
        let header_length = number(&header, 12)? as usize;
        let length = number(&header, 16)? as usize;
        let count = number(&header, 20)? as usize;
        if header_length > HEADERS
            || header.len() != 24 + header_length
            || length > maximum
            || count != length.div_ceil(DATA_BYTES)
        {
            return Err(invalid("invalid service socket response lengths"));
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Metadata {
            status: u16,
            headers: BTreeMap<String, String>,
        }
        let value = crate::first_read::strict_json(&header[24..])?;
        let metadata: Metadata = serde_json::from_value(value).map_err(socket_error)?;
        if !(100..600).contains(&metadata.status)
            || metadata.headers.iter().any(|(name, value)| {
                name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    || value.bytes().any(|b| b < 32 || b == 127)
                    || name == "x-git-zero-service-usage"
                    || name.starts_with("x-git-zero-internal-")
            })
        {
            return Err(invalid("invalid service socket response metadata"));
        }
        diagnostic.field("response_status", metadata.status);
        diagnostic.field("expected_response_body_bytes", length);
        diagnostic.mark("socket_response_headers_complete");
        diagnostic.mark("socket_response_body_read_begin");
        let mut body = Vec::with_capacity(length);
        for index in 0..count {
            let piece = self.binary(diagnostic)?;
            if index == 0 {
                diagnostic.mark("socket_first_body_segment");
            }
            let part_length = number(&piece, 20)? as usize;
            if !piece.starts_with(b"GZSD")
                || piece.len() < DATA_HEADER
                || number(&piece, 4)? != id
                || number(&piece, 8)? as usize != index
                || number(&piece, 12)? as usize != body.len()
                || number(&piece, 16)? as usize != length
                || part_length == 0
                || part_length > DATA_BYTES
                || piece.len() != DATA_HEADER + part_length
                || part_length != (length - body.len()).min(DATA_BYTES)
            {
                return Err(invalid("invalid service socket response segment"));
            }
            body.extend_from_slice(&piece[DATA_HEADER..]);
            diagnostic.field("response_body_bytes", body.len());
        }
        diagnostic.mark("socket_terminal_read_begin");
        let terminal = self.binary(diagnostic)?;
        if terminal != frame(b"GZSE", &[1, id, length as u32, count as u32]) || body.len() != length
        {
            return Err(invalid("invalid service socket response terminal"));
        }
        // The terminal proves this complete response, not a mutation replay.
        // ACK merely releases the Worker's bounded response gate. If this
        // transport-only write fails, the verified response still wins and the
        // caller drops this connection before its next distinct RPC.
        diagnostic.mark("socket_terminal_received");
        diagnostic.mark("socket_ack_write_begin");
        self.frames.ack_attempts += 1;
        if self
            .socket
            .send(Message::Binary(frame(b"GZSA", &[1, id]).into()))
            .inspect_err(|e| {
                if let tungstenite::Error::Io(e) = e {
                    socket_io_error(diagnostic, e);
                }
            })
            .is_err()
        {
            self.usable = false;
            diagnostic.field("ack_write_failed", true);
        } else {
            diagnostic.mark("socket_ack_written");
            self.frames.acks_written += 1;
        }
        Ok(Response {
            status: metadata.status,
            headers: metadata.headers,
            body: body.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_known_bounded_routes_select_socket_before_io() {
        assert_eq!(bounds("POST", "blobs/read"), Some((METADATA, BULK)));
        assert_eq!(
            bounds("POST", "manifest/upload-retire"),
            Some((BULK, 32768))
        );
        assert!(bounds("GET", &format!("blob/chunks/{}", "a".repeat(64))).is_none());
        assert!(bounds("PUT", &format!("blob/chunks/{}", "a".repeat(64))).is_some());
        assert!(bounds("PUT", "blob/chunks/invalid").is_none());
        assert!(bounds("POST", "manifest/first-read").is_none());
        assert_eq!(BULK, 16_779_276);
    }
    #[test]
    fn socket_capability_is_exact_optional_and_independent_of_first_read() {
        assert!(
            Capability::parse(&json!({"registered":true}), true)
                .unwrap()
                .is_none()
        );
        let valid = json!({"registered":true,"service_socket":{"version":1,"protocol":PROTOCOL,
            "piece_bytes":PIECE,"header_bytes":HEADERS,"body_bytes":BULK,"rpcs":RPCS,"session":"11".repeat(16)}});
        assert!(Capability::parse(&valid, true).unwrap().is_some());
        assert!(Capability::parse(&valid, false).is_err());
        for field in [
            "version",
            "protocol",
            "piece_bytes",
            "header_bytes",
            "body_bytes",
            "rpcs",
            "session",
        ] {
            let mut changed = valid.clone();
            changed["service_socket"][field] = Value::Null;
            assert!(Capability::parse(&changed, true).is_err(), "{field}");
        }
        let mut changed = valid;
        changed["service_socket"]["extra"] = Value::Bool(true);
        assert!(Capability::parse(&changed, true).is_err());
    }
}
