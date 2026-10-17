//! Blocking HTTP server socket and request/response framing.
use super::{SOCKET_FLAGS, SocketWriter, apply_timeouts, close_on_exec, receive, socket};
use core::{
    fmt::Write as _,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};
use git_zero::{
    Error, Result,
    http::{Request, Response},
    io::{SliceWriter, Write},
};
use rustix::{
    fd::{AsFd, OwnedFd},
    net::{self, sockopt},
};

mod header;
use header::{Headers, parse_request, read_header, request_line};

/// Inclusive host admission quotas, separate from caller storage capacity.
/// `None` leaves that quota unset; storage still bounds all request bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestLimits {
    /// Request line, headers (including their delimiter), and encoded body.
    pub max_wire_bytes: Option<usize>,
    /// Decoded body bytes, including identity bodies, without HTTP headers.
    pub max_body_bytes: Option<usize>,
}

impl RequestLimits {
    // Header intake proves header <= max_wire_bytes when that policy is set.
    // Check both known quotas before the separate physical body reservation.
    fn check_declared(self, header: usize, body: usize, encoding: Encoding) -> Result<()> {
        let quotas = [
            self.max_wire_bytes.map(|limit| limit - header),
            self.max_body_bytes
                .filter(|_| encoding == Encoding::Identity),
        ];
        if quotas.into_iter().flatten().any(|limit| body > limit) {
            Err(Error::LimitExceeded)
        } else {
            Ok(())
        }
    }
}

// Streaming intake/output reaches the first capacity boundary. Policy wins a
// tie; this deliberately differs from checking a fully known declared size.
struct Capacity {
    bytes: usize,
    error: Error,
}

impl Capacity {
    fn new(physical: usize, policy: Option<usize>) -> Self {
        match policy {
            Some(bytes) if bytes <= physical => Self {
                bytes,
                error: Error::LimitExceeded,
            },
            _ => Self {
                bytes: physical,
                error: Error::BufferTooSmall,
            },
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Encoding {
    Identity,
    Gzip,
    Unsupported,
}

impl Encoding {
    fn classify(value: Option<&str>) -> Self {
        match value {
            None => Self::Identity,
            Some(value) if value.eq_ignore_ascii_case("identity") => Self::Identity,
            Some(value) if value.eq_ignore_ascii_case("gzip") => Self::Gzip,
            _ => Self::Unsupported,
        }
    }
}

/// Per-blocking-call timeouts applied only to the accepted socket. These do not
/// bound accept, a complete request, or a complete response. `None` disables the
/// corresponding timeout; zero durations are invalid. The OS may round values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptedSocketTimeouts {
    pub read: Option<Duration>,
    pub write: Option<Duration>,
}

impl Default for AcceptedSocketTimeouts {
    fn default() -> Self {
        Self {
            read: Some(Duration::from_secs(10)),
            write: Some(Duration::from_secs(10)),
        }
    }
}

pub fn listen(port: u16) -> Result<OwnedFd> {
    listen_on(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)))
}

/// Bind an explicit numeric IPv4 or IPv6 address. IPv6 listeners accept only
/// IPv6 connections; bind a separate IPv4 listener when both are required.
/// Port zero is assigned by the OS and can be read with `local_address`.
pub fn listen_on(address: SocketAddr) -> Result<OwnedFd> {
    let family = if address.is_ipv4() {
        net::AddressFamily::INET
    } else {
        net::AddressFamily::INET6
    };
    let socket = socket(family)?;
    // OpenBSD IPv6 sockets are always IPv6-only and this option is read-only.
    #[cfg(not(target_os = "openbsd"))]
    if address.is_ipv6() {
        sockopt::set_ipv6_v6only(&socket, true).map_err(|_| Error::Backend)?;
    }
    sockopt::set_socket_reuseaddr(&socket, true).map_err(|_| Error::Backend)?;
    net::bind(&socket, &address).map_err(|_| Error::Backend)?;
    net::listen(&socket, 128).map_err(|_| Error::Backend)?;
    Ok(socket)
}

pub fn local_port(socket: &impl AsFd) -> Result<u16> {
    Ok(local_address(socket)?.port())
}

/// Read the actual bound IP address, including an OS-assigned port.
pub fn local_address(socket: &impl AsFd) -> Result<SocketAddr> {
    let address = net::getsockname(socket).map_err(|_| Error::Backend)?;
    SocketAddr::try_from(address).map_err(|_| Error::Backend)
}

pub fn accept(socket: &impl AsFd) -> Result<OwnedFd> {
    accept_with_timeouts(socket, AcceptedSocketTimeouts::default())
}

/// Accept with host-selected child socket timeouts, preserving close-on-exec.
/// Invalid zero durations are rejected before accepting; configuration failures
/// close the child before returning `Backend`. The listener is unchanged.
pub fn accept_with_timeouts(
    socket: &impl AsFd,
    timeouts: AcceptedSocketTimeouts,
) -> Result<OwnedFd> {
    if [timeouts.read, timeouts.write]
        .into_iter()
        .flatten()
        .any(|value| value.is_zero())
    {
        return Err(Error::InvalidRequest);
    }
    accept_configured(socket, |client| {
        apply_timeouts(client, timeouts.read, timeouts.write)
    })
}

// Own the accepted descriptor throughout configuration, including failure.
fn accept_configured(
    socket: &impl AsFd,
    configure: impl FnOnce(&OwnedFd) -> Result<()>,
) -> Result<OwnedFd> {
    let client =
        close_on_exec(net::accept_with(socket, SOCKET_FLAGS).map_err(|_| Error::Backend)?)?;
    configure(&client)?;
    Ok(client)
}

/// Read one bounded request. Gzip bodies use the storage remaining after the
/// compressed wire bytes and a fixed 32 KiB stack history window. Compressed
/// plus expanded bytes and headers must fit `storage`; chunked input is rejected.
pub fn read_request<'a>(socket: &impl AsFd, storage: &'a mut [u8]) -> Result<Request<'a>> {
    read_request_with_limits(socket, storage, RequestLimits::default())
}

/// Read one request with inclusive logical quotas. Known policy exhaustion
/// returns `LimitExceeded`; insufficient physical storage returns
/// `BufferTooSmall`. A tied policy/capacity boundary is a policy failure.
/// Gzip retains the complete wire body and original physical expansion slack.
/// All storage is provisional on failure; discard it and close the socket.
pub fn read_request_with_limits<'a>(
    socket: &impl AsFd,
    storage: &'a mut [u8],
    limits: RequestLimits,
) -> Result<Request<'a>> {
    let (end, length) = read_header(socket, storage, limits.max_wire_bytes)?;
    let (header, storage) = storage.split_at_mut(end);
    let mut length = length - end;
    let header = core::str::from_utf8(header).map_err(|_| Error::InvalidRequest)?;
    let line = request_line(header)?;
    let headers = Headers::parse(header)?;
    let mut request = parse_request(line, &headers)?;
    let total = headers
        .content_length
        .map(super::parse_length)
        .transpose()?
        .unwrap_or(0);
    let encoding = Encoding::classify(headers.content_encoding);
    limits.check_declared(end, total, encoding)?;
    if total > storage.len() {
        return Err(Error::BufferTooSmall);
    }
    while length < total {
        let count = receive(socket, &mut storage[length..total])?;
        if count == 0 {
            return Err(Error::InvalidRequest);
        }
        length += count;
    }
    let (wire, expanded) = storage.split_at_mut(total);
    request.body = wire;
    match encoding {
        Encoding::Identity => {}
        Encoding::Gzip => {
            use git_zero::zlib::{InflateLimits, WINDOW_SIZE, decode_gzip};
            let capacity = Capacity::new(expanded.len(), limits.max_body_bytes);
            let length = decode_gzip(
                request.body,
                expanded,
                &mut [0; WINDOW_SIZE],
                InflateLimits {
                    max_output: capacity.bytes as u64,
                    max_ratio: 128,
                    ratio_slack: expanded.len() as u64,
                },
            )
            .map_err(|error| {
                // decode_gzip's only BufferTooSmall path is incomplete output.
                // Input exhaustion is InvalidZlib because input is complete.
                if error == Error::BufferTooSmall {
                    capacity.error
                } else {
                    error
                }
            })?;
            request.body = &expanded[..length];
        }
        Encoding::Unsupported => return Err(Error::Unsupported),
    }
    Ok(request)
}

pub fn respond(socket: &impl AsFd, response: Response, body: &[u8]) -> Result<()> {
    respond_headers(socket, response, body.len() as u64)?;
    SocketWriter(socket.as_fd()).write_all(body)
}

/// Send a prepared pull response with HTTP/1.0 close-delimited framing and
/// caller-owned staging. Each successful socket write precedes the next pull.
/// On any failure cancel the cursor and close the socket; never retry headers.
pub fn respond_stream(
    socket: &impl AsFd,
    repository: &impl git_zero::Repository,
    encoder: &mut git_zero::http::stream::ResponseEncoder<'_>,
    staging: &mut [u8],
) -> Result<()> {
    let result = (|| {
        if staging.is_empty() {
            return Err(Error::BufferTooSmall);
        }
        let response = encoder.response();
        let mut storage = [0; 512];
        let mut header = SliceWriter::new(&mut storage);
        write!(&mut header, "HTTP/1.0 {} Response\r\nContent-Type: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", response.status, response.content_type)
            .map_err(|_| Error::BufferTooSmall)?;
        let mut writer = SocketWriter(socket.as_fd());
        writer.write_all(header.as_bytes())?;
        loop {
            let count = encoder.read(repository, staging)?;
            if count == 0 {
                return Ok(());
            }
            writer.write_all(&staging[..count])?;
        }
    })();
    if result.is_err() {
        encoder.cancel();
    }
    result
}

/// Write framing for a body streamed separately by the host. The host must
/// then write exactly `body_length` bytes and close the connection on failure.
pub fn respond_headers(socket: &impl AsFd, response: Response, body_length: u64) -> Result<()> {
    let mut storage = [0; 512];
    let mut header = SliceWriter::new(&mut storage);
    write!(&mut header, "HTTP/1.0 {} Response\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", response.status, response.content_type, body_length).map_err(|_| Error::BufferTooSmall)?;
    SocketWriter(socket.as_fd()).write_all(header.as_bytes())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn explicit_ip_listeners_report_bound_addresses_and_isolate_ipv6() {
        for value in ["127.0.0.1:0", "[::1]:0"] {
            let address: SocketAddr = value.parse().unwrap();
            let listener = listen_on(address).unwrap();
            let bound = local_address(&listener).unwrap();
            assert_eq!(bound.ip(), address.ip());
            assert_ne!(bound.port(), 0);
            assert_eq!(local_port(&listener).unwrap(), bound.port());
            assert!(
                rustix::io::fcntl_getfd(&listener)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            let family = if address.is_ipv4() {
                net::AddressFamily::INET
            } else {
                assert!(sockopt::ipv6_v6only(&listener).unwrap());
                net::AddressFamily::INET6
            };
            let client = socket(family).unwrap();
            net::connect(&client, &bound).unwrap();
            let accepted = accept(&listener).unwrap();
            assert!(
                rustix::io::fcntl_getfd(&accepted)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            for timeout in [sockopt::Timeout::Recv, sockopt::Timeout::Send] {
                assert_eq!(
                    sockopt::socket_timeout(&accepted, timeout).unwrap(),
                    Some(core::time::Duration::from_secs(10))
                );
            }
        }
    }

    #[test]
    fn disconnected_stream_cancels_before_any_more_output() {
        let (socket, peer) = net::socketpair(
            net::AddressFamily::UNIX,
            net::SocketType::STREAM,
            SOCKET_FLAGS,
            None,
        )
        .unwrap();
        let repository = git_zero::EmptyRepository::new("refs/heads/main").unwrap();
        let request = Request {
            method: "GET",
            path: "HEAD",
            query: None,
            content_type: None,
            git_protocol: None,
            body: &[],
        };
        let mut control = [0; 128];
        let mut encoder =
            git_zero::http::stream::ResponseEncoder::new(&repository, request, &mut control)
                .unwrap();
        drop(peer);
        assert_eq!(
            respond_stream(&socket, &repository, &mut encoder, &mut [0; 3]),
            Err(Error::Backend)
        );
        assert_eq!(
            encoder.read(&repository, &mut [0; 3]),
            Err(Error::InvalidRequest)
        );
    }

    #[test]
    fn failed_accepted_configuration_closes_the_child() {
        use std::io::Read;
        let listener = listen(0).unwrap();
        let mut peer = std::net::TcpStream::connect(local_address(&listener).unwrap()).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(
            accept_configured(&listener, |_| Err(Error::Backend)),
            Err(Error::Backend)
        ));
        assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
        // The listener remains usable after the failed child is discarded.
        let _peer = std::net::TcpStream::connect(local_address(&listener).unwrap()).unwrap();
        assert!(accept(&listener).is_ok());
    }
}
