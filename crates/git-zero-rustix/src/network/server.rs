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

mod chunked;
pub use chunked::ChunkedLimits;
mod deadline;
use deadline::{DeadlineReader, DeadlineWriter};
pub use deadline::{RequestDeadlines, ResponseDeadlines};
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

/// Inclusive HTTP header quotas. Byte counts apply to wire bytes before UTF-8
/// decoding; values include whitespace after the colon. Unknown and repeated
/// ordinary fields consume the same budgets as recognized Git/framing fields.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderLimits {
    /// Request line plus header fields and the final CRLF delimiter.
    pub max_bytes: Option<usize>,
    /// Number of fields, excluding the request line and final empty line.
    pub max_fields: Option<usize>,
    pub max_name_bytes: Option<usize>,
    pub max_value_bytes: Option<usize>,
}

/// Independent whole-request/body and header policies. Unset defaults preserve
/// the storage-bounded parser. These quotas do not impose a total time deadline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestPolicy {
    pub request: RequestLimits,
    pub headers: HeaderLimits,
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
    read_request_with_policy(
        socket,
        storage,
        RequestPolicy {
            request: limits,
            headers: HeaderLimits::default(),
        },
    )
}

/// Apply header admission during intake and the single metadata-validation pass.
/// An incomplete header stops at its exact byte quota without reading a byte
/// beyond it. Field quotas are checked after that field's syntax, before body
/// intake/inflation and repository preparation. Failure invalidates all storage.
pub fn read_request_with_policy<'a>(
    socket: &impl AsFd,
    storage: &'a mut [u8],
    policy: RequestPolicy,
) -> Result<Request<'a>> {
    read_request_from(&mut |bytes| receive(socket, bytes), storage, policy)
}

/// Apply monotonic total-intake and idle-progress deadlines in addition to byte
/// quotas and accepted-socket read timeouts. The deadline starts on this call,
/// covers header/body intake and the final synchronous parse/inflation check,
/// and never resets when a peer drips another byte. It excludes accept, Git
/// preparation and response transmission. Close the socket on every error.
/// Expired host deadlines return `LimitExceeded`; a shorter socket timeout
/// retains `Backend`. Socket flags and timeout settings remain unchanged.
/// With both deadlines unset this is identical to `read_request_with_policy`.
pub fn read_request_with_deadlines<'a>(
    socket: &impl AsFd,
    storage: &'a mut [u8],
    policy: RequestPolicy,
    deadlines: RequestDeadlines,
) -> Result<Request<'a>> {
    read_socket_request(socket, storage, policy, deadlines, None)
}

/// Host transport boundary for authenticated TLS or other blocking streams.
/// The callback fills at most the supplied slice, returns zero on EOF, and owns
/// its transport deadlines/cancellation. Any error invalidates storage. Rust
/// owns HTTP/Git parsing; the host owns peer authentication and TLS buffering.
/// This default entry rejects Transfer-Encoding, as does the socket reader.
pub fn read_request_from<'a>(
    receive: &mut impl FnMut(&mut [u8]) -> Result<usize>,
    storage: &'a mut [u8],
    policy: RequestPolicy,
) -> Result<Request<'a>> {
    read_request_framed(receive, storage, policy, None)
}

/// Opt-in bounded chunked HTTP/1.1 intake over a host-supplied transport.
/// Chunk extensions and nonempty trailers are explicitly unsupported. Duplicate
/// transfer fields, Content-Length conflicts and invalid chunk syntax fail before
/// returning a request. Responses still close the connection; retain no pipeline.
pub fn read_request_from_chunked<'a>(
    receive: &mut impl FnMut(&mut [u8]) -> Result<usize>,
    storage: &'a mut [u8],
    policy: RequestPolicy,
    chunks: ChunkedLimits,
) -> Result<Request<'a>> {
    read_request_framed(receive, storage, policy, Some(chunks))
}

/// Bounded chunked intake with independent monotonic request deadlines.
pub fn read_request_chunked<'a>(
    socket: &impl AsFd,
    storage: &'a mut [u8],
    policy: RequestPolicy,
    deadlines: RequestDeadlines,
    chunks: ChunkedLimits,
) -> Result<Request<'a>> {
    read_socket_request(socket, storage, policy, deadlines, Some(chunks))
}

fn read_socket_request<'a>(
    socket: &impl AsFd,
    storage: &'a mut [u8],
    policy: RequestPolicy,
    deadlines: RequestDeadlines,
    chunks: Option<ChunkedLimits>,
) -> Result<Request<'a>> {
    if deadlines == RequestDeadlines::default() {
        return read_request_framed(&mut |bytes| receive(socket, bytes), storage, policy, chunks);
    }
    let mut reader = DeadlineReader::new(socket, deadlines)?;
    let result = read_request_framed(&mut |bytes| reader.receive(bytes), storage, policy, chunks);
    reader.check()?;
    result
}

fn read_request_framed<'a>(
    receive: &mut impl FnMut(&mut [u8]) -> Result<usize>,
    storage: &'a mut [u8],
    policy: RequestPolicy,
    chunks: Option<ChunkedLimits>,
) -> Result<Request<'a>> {
    let limits = policy.request;
    let (end, length) = read_header(
        receive,
        storage,
        limits.max_wire_bytes,
        policy.headers.max_bytes,
    )?;
    let (header, storage) = storage.split_at_mut(end);
    let length = length - end;
    let header = core::str::from_utf8(header).map_err(|_| Error::InvalidRequest)?;
    let line = request_line(header)?;
    let headers = if chunks.is_some() {
        Headers::parse_framed(header, policy.headers, true)?
    } else {
        Headers::parse(header, policy.headers)?
    };
    let mut request = parse_request(line, &headers)?;
    let identity = headers
        .content_encoding
        .is_none_or(|value| value.eq_ignore_ascii_case("identity"));
    let (wire_length, body_length) = if headers.chunked {
        if !line.ends_with(" HTTP/1.1") {
            return Err(Error::InvalidRequest);
        }
        chunked::read(
            receive,
            storage,
            length,
            limits.max_wire_bytes.map(|limit| limit - end),
            if identity {
                limits.max_body_bytes
            } else {
                None
            },
            chunks.unwrap(),
        )?
    } else {
        let total = headers
            .content_length
            .map(super::parse_length)
            .transpose()?
            .unwrap_or(0);
        let total = read_fixed_body(
            receive,
            storage,
            length,
            total,
            RequestLimits {
                max_wire_bytes: limits.max_wire_bytes.map(|limit| limit - end),
                max_body_bytes: if identity {
                    limits.max_body_bytes
                } else {
                    None
                },
            },
        )?;
        (total, total)
    };
    let (wire, expanded) = storage.split_at_mut(wire_length);
    request.body = decode_body(
        &wire[..body_length],
        expanded,
        headers.content_encoding,
        limits.max_body_bytes,
    )?;
    Ok(request)
}

fn read_fixed_body(
    receive: &mut impl FnMut(&mut [u8]) -> Result<usize>,
    storage: &mut [u8],
    mut length: usize,
    total: usize,
    limits: RequestLimits,
) -> Result<usize> {
    if limits.max_wire_bytes.is_some_and(|limit| total > limit)
        || limits.max_body_bytes.is_some_and(|limit| total > limit)
    {
        return Err(Error::LimitExceeded);
    }
    if total > storage.len() {
        return Err(Error::BufferTooSmall);
    }
    while length < total {
        let count = receive(&mut storage[length..total])?;
        if count == 0 {
            return Err(Error::InvalidRequest);
        }
        if count > total - length {
            return Err(Error::Backend);
        }
        length += count;
    }
    Ok(total)
}

fn decode_body<'a>(
    body: &'a [u8],
    expanded: &'a mut [u8],
    encoding: Option<&str>,
    max_body_bytes: Option<usize>,
) -> Result<&'a [u8]> {
    match encoding {
        None => Ok(body),
        Some(value) if value.eq_ignore_ascii_case("identity") => Ok(body),
        Some(value) if value.eq_ignore_ascii_case("gzip") => {
            use git_zero::zlib::{InflateLimits, WINDOW_SIZE, decode_gzip};
            let length = decode_gzip(
                body,
                expanded,
                &mut [0; WINDOW_SIZE],
                InflateLimits {
                    max_output: max_body_bytes
                        .map_or(expanded.len(), |limit| limit.min(expanded.len()))
                        as u64,
                    max_ratio: 128,
                    ratio_slack: expanded.len() as u64,
                },
            )
            .map_err(|error| {
                // decode_gzip's only BufferTooSmall path is incomplete output.
                // Input exhaustion is InvalidZlib because input is complete.
                if error == Error::BufferTooSmall
                    && max_body_bytes.is_some_and(|limit| limit <= expanded.len())
                {
                    Error::LimitExceeded
                } else {
                    error
                }
            })?;
            Ok(&expanded[..length])
        }
        _ => Err(Error::Unsupported),
    }
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
    respond_stream_using(
        &mut SocketWriter(socket.as_fd()),
        repository,
        encoder,
        staging,
    )
}

/// Bound an entire streamed response and gaps in successful socket writes.
/// Clock checks bracket every synchronous provider pull and EOF; a blocking
/// provider cannot be preempted. Any expiry cancels the response without a
/// successful protocol terminator. Close the transport on every error.
pub fn respond_stream_with_deadlines(
    socket: &impl AsFd,
    repository: &impl git_zero::Repository,
    encoder: &mut git_zero::http::stream::ResponseEncoder<'_>,
    staging: &mut [u8],
    deadlines: ResponseDeadlines,
) -> Result<()> {
    if deadlines == ResponseDeadlines::default() {
        return respond_stream(socket, repository, encoder, staging);
    }
    let mut writer = match DeadlineWriter::new(socket, deadlines) {
        Ok(writer) => writer,
        Err(error) => {
            encoder.cancel();
            return Err(error);
        }
    };
    respond_stream_using(&mut writer, repository, encoder, staging)
}

fn respond_stream_using(
    writer: &mut impl Write,
    repository: &impl git_zero::Repository,
    encoder: &mut git_zero::http::stream::ResponseEncoder<'_>,
    staging: &mut [u8],
) -> Result<()> {
    let result = respond_body(writer, encoder.response(), None, staging, |bytes| {
        encoder.read(repository, bytes)
    });
    if result.is_err() {
        encoder.cancel();
    }
    result
}

fn respond_body(
    writer: &mut impl Write,
    response: Response,
    mut remaining: Option<u64>,
    staging: &mut [u8],
    mut read: impl FnMut(&mut [u8]) -> Result<usize>,
) -> Result<()> {
    if staging.is_empty() {
        return Err(Error::BufferTooSmall);
    }
    let mut storage = [0; 512];
    let mut header = SliceWriter::new(&mut storage);
    write!(
        &mut header,
        "HTTP/1.0 {} Response\r\nContent-Type: {}\r\n",
        response.status, response.content_type
    )
    .map_err(|_| Error::BufferTooSmall)?;
    if let Some(size) = remaining {
        write!(&mut header, "Content-Length: {size}\r\n").map_err(|_| Error::BufferTooSmall)?;
    }
    header.write_all(b"Cache-Control: no-store\r\nConnection: close\r\n\r\n")?;
    writer.write_all(header.as_bytes())?;
    loop {
        writer.write_all(&[])?; // Deadline check before entering the provider.
        let count = read(staging)?;
        if let Some(left) = &mut remaining {
            *left = left.checked_sub(count as u64).ok_or(Error::Backend)?;
        }
        writer.write_all(&staging[..count])?; // Also check EOF/late provider work.
        if count == 0 {
            return if remaining.unwrap_or(0) == 0 {
                Ok(())
            } else {
                Err(Error::Backend)
            };
        }
    }
}

/// Stream a publication response with its known complete length. Each cursor
/// pins its provider; failed writes cancel only that response. Close the socket
/// on errors, including a provider that was changed/truncated after publication.
pub fn respond_published<R: git_zero::Repository>(
    socket: &impl AsFd,
    encoder: &mut git_zero::http::dumb::PublishedResponse<'_, R>,
    staging: &mut [u8],
) -> Result<()> {
    let result = respond_known(
        socket,
        encoder.response(),
        encoder.size(),
        staging,
        |bytes| encoder.read(bytes),
    );
    if result.is_err() {
        encoder.cancel();
    }
    result
}

/// Known-length publication responses with the same total/idle write budget.
/// Deadline configuration errors and late provider failures cancel this cursor.
pub fn respond_published_with_deadlines<R: git_zero::Repository>(
    socket: &impl AsFd,
    encoder: &mut git_zero::http::dumb::PublishedResponse<'_, R>,
    staging: &mut [u8],
    deadlines: ResponseDeadlines,
) -> Result<()> {
    if deadlines == ResponseDeadlines::default() {
        return respond_published(socket, encoder, staging);
    }
    let result = DeadlineWriter::new(socket, deadlines).and_then(|mut writer| {
        respond_body(
            &mut writer,
            encoder.response(),
            Some(encoder.size()),
            staging,
            |bytes| encoder.read(bytes),
        )
    });
    if result.is_err() {
        encoder.cancel();
    }
    result
}

/// Send a previously verified on-disk publication, with independent file reads
/// and response-local cancellation on any provider or socket error.
#[cfg(feature = "storage")]
pub fn respond_file_published<R: git_zero::Repository>(
    socket: &impl AsFd,
    encoder: &mut crate::FilePublishedResponse<'_, R>,
    staging: &mut [u8],
) -> Result<()> {
    let result = respond_known(
        socket,
        encoder.response(),
        encoder.size(),
        staging,
        |bytes| encoder.read(bytes),
    );
    if result.is_err() {
        encoder.cancel();
    }
    result
}

fn respond_known(
    socket: &impl AsFd,
    response: Response,
    size: u64,
    staging: &mut [u8],
    read: impl FnMut(&mut [u8]) -> Result<usize>,
) -> Result<()> {
    respond_body(
        &mut SocketWriter(socket.as_fd()),
        response,
        Some(size),
        staging,
        read,
    )
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
