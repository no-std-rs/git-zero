//! Blocking, allocation-free IPv4 HTTP/1.0 adapters for local development.
//! The sample process wrapper uses std for arguments and logging.
#![no_std]
#![forbid(unsafe_code)]

use core::{
    fmt::Write as _,
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use git_zero::{
    Error, Result,
    client::Transport,
    http::{Request, Response},
    io::{SliceWriter, Write},
};
use rustix::{
    fd::{AsFd, BorrowedFd, OwnedFd},
    net::{
        self, AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType,
        sockopt::{self, Timeout},
    },
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
    let client = net::accept_with(socket, SocketFlags::CLOEXEC).map_err(|_| Error::Backend)?;
    timeouts(&client)?;
    Ok(client)
}

fn socket() -> Result<OwnedFd> {
    net::socket_with(
        AddressFamily::INET,
        SocketType::STREAM,
        SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|_| Error::Backend)
}

fn timeouts(socket: &impl AsFd) -> Result<()> {
    for timeout in [Timeout::Recv, Timeout::Send] {
        sockopt::set_socket_timeout(socket, timeout, Some(Duration::from_secs(10)))
            .map_err(|_| Error::Backend)?;
    }
    Ok(())
}

pub struct SocketWriter<'a>(pub BorrowedFd<'a>);

impl Write for SocketWriter<'_> {
    fn write_all(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            match net::send(self.0, bytes, SendFlags::NOSIGNAL) {
                Ok(0) => return Err(Error::Backend),
                Ok(length) => bytes = &bytes[length..],
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => return Err(Error::Backend),
            }
        }
        Ok(())
    }
}

fn receive(socket: &impl AsFd, storage: &mut [u8]) -> Result<usize> {
    loop {
        match net::recv(socket, &mut *storage, RecvFlags::empty()) {
            Ok((length, _)) => return Ok(length),
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return Err(Error::Backend),
        }
    }
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn content_length(header: &str) -> Result<Option<usize>> {
    let mut result = None;
    for line in header.split("\r\n").skip(1) {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(Error::InvalidRequest)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Error::Unsupported);
        }
        if name.eq_ignore_ascii_case("content-length") {
            if result.is_some() {
                return Err(Error::InvalidRequest);
            }
            result = Some(value.trim().parse().map_err(|_| Error::InvalidRequest)?);
        }
    }
    Ok(result)
}

pub fn read_request<'a>(socket: &impl AsFd, storage: &'a mut [u8]) -> Result<Request<'a>> {
    let mut length = 0;
    let end = loop {
        if let Some(end) = header_end(&storage[..length]) {
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
    Ok(Request {
        method,
        path,
        query,
        body: &storage[end..total],
    })
}

pub fn respond(socket: &impl AsFd, response: Response, body: &[u8]) -> Result<()> {
    let mut storage = [0; 512];
    let mut header = SliceWriter::new(&mut storage);
    write!(&mut header, "HTTP/1.0 {} Response\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", response.status, response.content_type, body.len()).map_err(|_| Error::BufferTooSmall)?;
    let mut output = SocketWriter(socket.as_fd());
    output.write_all(header.as_bytes())?;
    output.write_all(body)
}

/// Numeric IPv4 addresses only. TLS, redirects, and chunked transfer decoding
/// belong in richer host transports. All exchanges use fixed caller storage.
pub struct HttpTransport<'a> {
    pub address: SocketAddrV4,
    pub repository_path: &'a str,
    pub wire_storage: &'a mut [u8],
}

impl Transport for HttpTransport<'_> {
    fn request(&mut self, request: Request<'_>, response: &mut [u8]) -> Result<(u16, usize)> {
        let socket = socket()?;
        timeouts(&socket)?;
        net::connect(&socket, &self.address).map_err(|_| Error::Backend)?;
        let mut header = SliceWriter::new(self.wire_storage);
        write!(
            &mut header,
            "{} {}/{}",
            request.method,
            self.repository_path.trim_end_matches('/'),
            request.path.trim_start_matches('/')
        )
        .map_err(|_| Error::BufferTooSmall)?;
        if let Some(query) = request.query {
            write!(&mut header, "?{query}").map_err(|_| Error::BufferTooSmall)?;
        }
        write!(&mut header, " HTTP/1.0\r\nHost: {}\r\nContent-Length: {}\r\nContent-Type: application/x-git-upload-pack-request\r\nConnection: close\r\n\r\n", self.address, request.body.len()).map_err(|_| Error::BufferTooSmall)?;
        let mut output = SocketWriter(socket.as_fd());
        output.write_all(header.as_bytes())?;
        output.write_all(request.body)?;
        let mut length = 0;
        loop {
            if length == self.wire_storage.len() {
                let mut extra = [0; 1];
                if receive(&socket, &mut extra)? != 0 {
                    return Err(Error::BufferTooSmall);
                }
                break;
            }
            let count = receive(&socket, &mut self.wire_storage[length..])?;
            if count == 0 {
                break;
            }
            length += count;
        }
        let end = header_end(&self.wire_storage[..length]).ok_or(Error::InvalidRequest)?;
        let header =
            core::str::from_utf8(&self.wire_storage[..end]).map_err(|_| Error::InvalidRequest)?;
        let status = header
            .lines()
            .next()
            .and_then(|line| line.split_ascii_whitespace().nth(1))
            .ok_or(Error::InvalidRequest)?
            .parse()
            .map_err(|_| Error::InvalidRequest)?;
        let body = &self.wire_storage[end..length];
        if content_length(header)?.is_some_and(|expected| expected != body.len()) {
            return Err(Error::InvalidRequest);
        }
        response
            .get_mut(..body.len())
            .ok_or(Error::BufferTooSmall)?
            .copy_from_slice(body);
        Ok((status, body.len()))
    }
}
