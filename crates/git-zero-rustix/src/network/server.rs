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
    parse_request(&storage[..total], end)
}

// Keep any body bytes received with the header for the subsequent bounded read.
fn read_header(socket: &impl AsFd, storage: &mut [u8]) -> Result<(usize, usize)> {
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
    Ok(Request {
        method,
        path,
        query,
        body: &storage[end..],
    })
}

pub fn respond(socket: &impl AsFd, response: Response, body: &[u8]) -> Result<()> {
    respond_headers(socket, response, body.len() as u64)?;
    SocketWriter(socket.as_fd()).write_all(body)
}

/// Write framing for a body streamed separately by the host. The host must
/// then write exactly `body_length` bytes and close the connection on failure.
pub fn respond_headers(socket: &impl AsFd, response: Response, body_length: u64) -> Result<()> {
    let mut storage = [0; 512];
    let mut header = SliceWriter::new(&mut storage);
    write!(&mut header, "HTTP/1.0 {} Response\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", response.status, response.content_type, body_length).map_err(|_| Error::BufferTooSmall)?;
    SocketWriter(socket.as_fd()).write_all(header.as_bytes())
}
