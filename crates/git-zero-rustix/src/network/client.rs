//! Blocking IPv4 HTTP/1.0 client transport.
use super::{SocketWriter, content_length, header_end, receive, socket, timeouts};
use core::{fmt::Write as _, net::SocketAddrV4};
use git_zero::{
    Error, Result,
    client::Transport,
    http::Request,
    io::{SliceWriter, Write},
};
use rustix::{fd::AsFd, net};

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
        self.send_request(&socket, request)?;
        self.read_response(&socket, response)
    }
}

impl HttpTransport<'_> {
    fn send_request(&mut self, socket: &impl AsFd, request: Request<'_>) -> Result<()> {
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
        Ok(())
    }

    fn read_response(&mut self, socket: &impl AsFd, response: &mut [u8]) -> Result<(u16, usize)> {
        let mut length = 0;
        loop {
            if length == self.wire_storage.len() {
                let mut extra = [0; 1];
                if receive(socket, &mut extra)? != 0 {
                    return Err(Error::BufferTooSmall);
                }
                break;
            }
            let count = receive(socket, &mut self.wire_storage[length..])?;
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
