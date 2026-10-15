//! Blocking HTTP server socket and request/response framing.
use super::{
    SOCKET_FLAGS, SocketWriter, close_on_exec, content_length, header_end, receive, socket,
    timeouts,
};
use core::{
    fmt::Write as _,
    net::{Ipv4Addr, SocketAddrV4},
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

pub fn listen(port: u16) -> Result<OwnedFd> {
    let socket = socket()?;
    sockopt::set_socket_reuseaddr(&socket, true).map_err(|_| Error::Backend)?;
    net::bind(&socket, &SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map_err(|_| Error::Backend)?;
    net::listen(&socket, 128).map_err(|_| Error::Backend)?;
    Ok(socket)
}

pub fn local_port(socket: &impl AsFd) -> Result<u16> {
    let address = net::getsockname(socket).map_err(|_| Error::Backend)?;
    Ok(SocketAddrV4::try_from(address)
        .map_err(|_| Error::Backend)?
        .port())
}

pub fn accept(socket: &impl AsFd) -> Result<OwnedFd> {
    let client =
        close_on_exec(net::accept_with(socket, SOCKET_FLAGS).map_err(|_| Error::Backend)?)?;
    timeouts(&client)?;
    Ok(client)
}

/// Read one bounded request. Gzip bodies use the storage remaining after the
/// compressed wire bytes and a fixed 32 KiB stack history window. Compressed
/// plus expanded bytes and headers must fit `storage`; chunked input is rejected.
pub fn read_request<'a>(socket: &impl AsFd, storage: &'a mut [u8]) -> Result<Request<'a>> {
    let (end, mut length) = read_header(socket, storage)?;
    let header = core::str::from_utf8(&storage[..end]).map_err(|_| Error::InvalidRequest)?;
    let body_length = content_length(header)?.unwrap_or(0);
    let total = end
        .checked_add(body_length)
        .filter(|&total| total <= storage.len())
        .ok_or(Error::BufferTooSmall)?;
    while length < total {
        let count = receive(socket, &mut storage[length..total])?;
        if count == 0 {
            return Err(Error::InvalidRequest);
        }
        length += count;
    }
    let (wire, expanded) = storage.split_at_mut(total);
    let mut request = parse_request(wire, end)?;
    let header = core::str::from_utf8(&wire[..end]).map_err(|_| Error::InvalidRequest)?;
    match header_value(header, "content-encoding")? {
        None => {}
        Some(value) if value.eq_ignore_ascii_case("identity") => {}
        Some(value) if value.eq_ignore_ascii_case("gzip") => {
            use git_zero::zlib::{InflateLimits, WINDOW_SIZE, decode_gzip};
            let length = decode_gzip(
                request.body,
                expanded,
                &mut [0; WINDOW_SIZE],
                InflateLimits {
                    max_output: expanded.len() as u64,
                    max_ratio: 128,
                    ratio_slack: expanded.len() as u64,
                },
            )?;
            request.body = &expanded[..length];
        }
        _ => return Err(Error::Unsupported),
    }
    Ok(request)
}

#[derive(Default)]
struct HeaderScan {
    next: usize,
    #[cfg(test)]
    candidates: usize,
}

impl HeaderScan {
    fn end(&mut self, bytes: &[u8]) -> Option<usize> {
        let suffix = &bytes[self.next..];
        #[cfg(test)]
        {
            self.candidates += suffix.len().saturating_sub(3);
        }
        if let Some(end) = header_end(suffix) {
            return Some(self.next + end);
        }
        // Only these bytes can begin a delimiter completed by the next read.
        self.next = bytes.len().saturating_sub(3);
        None
    }
}

// Keep any body bytes received with the header for the subsequent bounded read.
fn read_header(socket: &impl AsFd, storage: &mut [u8]) -> Result<(usize, usize)> {
    let mut length = 0;
    let mut scan = HeaderScan::default();
    let end = loop {
        if let Some(end) = scan.end(&storage[..length]) {
            break end;
        }
        if length == storage.len() {
            return Err(Error::BufferTooSmall);
        }
        let count = receive(socket, &mut storage[length..])?;
        if count == 0 {
            return Err(Error::InvalidRequest);
        }
        length += count;
    };
    Ok((end, length))
}

// Parsing borrows only the complete request, after all socket reads have finished.
fn parse_request(storage: &[u8], end: usize) -> Result<Request<'_>> {
    let header = core::str::from_utf8(&storage[..end]).map_err(|_| Error::InvalidRequest)?;
    let mut line = header
        .lines()
        .next()
        .ok_or(Error::InvalidRequest)?
        .split_ascii_whitespace();
    let method = line.next().ok_or(Error::InvalidRequest)?;
    let target = line.next().ok_or(Error::InvalidRequest)?;
    if !matches!(line.next(), Some("HTTP/1.0" | "HTTP/1.1")) || line.next().is_some() {
        return Err(Error::InvalidRequest);
    }
    let (path, query) = target
        .split_once('?')
        .map_or((target, None), |(path, query)| (path, Some(query)));
    if !path.starts_with('/') || target.bytes().any(|b| b <= 32 || b == 127) {
        return Err(Error::InvalidRequest);
    }
    Ok(Request {
        content_type: header_value(header, "content-type")?,
        git_protocol: header_value(header, "git-protocol")?,
        method,
        path,
        query,
        body: &storage[end..],
    })
}

fn header_value<'a>(header: &'a str, key: &str) -> Result<Option<&'a str>> {
    let mut result = None;
    for line in header.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(Error::InvalidRequest)?;
        if name.eq_ignore_ascii_case(key) && result.replace(value.trim()).is_some() {
            return Err(Error::InvalidRequest);
        }
    }
    Ok(result)
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
    fn identity_coding_is_case_insensitive_and_preserves_git_metadata() {
        for encoding in ["identity", "Identity", "IDENTITY", "iDeNtItY"] {
            check_identity_coding(encoding);
        }
    }

    fn check_identity_coding(encoding: &str) {
        let (reader, writer) = net::socketpair(
            net::AddressFamily::UNIX,
            net::SocketType::STREAM,
            SOCKET_FLAGS,
            None,
        )
        .unwrap();
        let wire = std::format!(
            "POST /repo.git/git-upload-pack HTTP/1.1\r\nContent-Length: 4\r\nContent-Encoding: {encoding}\r\nContent-Type: application/x-git-upload-pack-request\r\nGit-Protocol: version=2\r\n\r\n0000"
        );
        SocketWriter(writer.as_fd())
            .write_all(wire.as_bytes())
            .unwrap();
        let mut storage = [0; 1024];
        let request = read_request(&reader, &mut storage).unwrap();
        assert_eq!(request.body, b"0000");
        assert_eq!(
            request.content_type,
            Some("application/x-git-upload-pack-request")
        );
        assert_eq!(request.git_protocol, Some("version=2"));
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
}

#[cfg(test)]
mod header_scan_tests {
    extern crate std;
    use super::HeaderScan;

    #[test]
    fn every_split_preserves_the_first_complete_delimiter() {
        let cases: &[&[u8]] = &[
            b"\r\n\r\n",
            b"GET /repo.git/HEAD HTTP/1.0\r\n\r\n",
            b"POST /upload HTTP/1.1\r\nX: a\r\nb\r\n\r\nbody\r\n\r\n",
            b"\xff\r\n\r\nbody",
            b"GET / HTTP/1.0\r\nX: incomplete\r\n",
        ];
        for wire in cases {
            let expected = wire
                .windows(4)
                .position(|value| value == b"\r\n\r\n")
                .map(|index| index + 4);
            for split in 0..=wire.len() {
                let mut scan = HeaderScan::default();
                let actual = scan.end(&wire[..split]).or_else(|| scan.end(wire));
                assert_eq!(actual, expected, "split={split}, wire={wire:?}");
                assert!(scan.candidates <= wire.len().saturating_sub(3));
            }
        }
    }

    #[test]
    fn full_capacity_headers_have_linear_work_for_every_fragment_size() {
        let mut wire = std::vec![b'a'; 65536];
        let prefix = b"GET / HTTP/1.0\r\nX: ";
        wire[..prefix.len()].copy_from_slice(prefix);
        wire[65532..].copy_from_slice(b"\r\n\r\n");
        for width in [1, 2, 3, 7, 127, 4096, 65536] {
            let mut scan = HeaderScan::default();
            let mut length = 0;
            for chunk in wire.chunks(width) {
                length += chunk.len();
                let expected = (length == wire.len()).then_some(length);
                assert_eq!(scan.end(&wire[..length]), expected);
                assert!(
                    scan.candidates <= length.saturating_sub(3),
                    "width={width}, length={length}"
                );
            }
            assert_eq!(scan.candidates, wire.len() - 3);
        }
    }

    #[test]
    fn unchanged_incomplete_input_does_not_repeat_search_work() {
        let wire = b"GET / HTTP/1.0\r\nX: incomplete\r\n";
        let mut scan = HeaderScan::default();
        for _ in 0..16 {
            assert_eq!(scan.end(wire), None);
            assert_eq!(scan.candidates, wire.len() - 3);
        }
    }
}
