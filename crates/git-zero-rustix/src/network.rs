//! Shared blocking IPv4 sockets and HTTP/1.0 framing primitives.
use core::time::Duration;
use git_zero::{Error, Result, io::Write};
use rustix::{
    fd::{AsFd, BorrowedFd, OwnedFd},
    net::{
        self, AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType,
        sockopt::{self, Timeout},
    },
};

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
pub use client::HttpTransport;
#[cfg(feature = "server")]
mod server;
#[cfg(feature = "server")]
pub use server::{accept, listen, local_port, read_request, respond, respond_headers};

#[cfg(not(target_vendor = "apple"))]
const SOCKET_FLAGS: SocketFlags = SocketFlags::CLOEXEC;
#[cfg(target_vendor = "apple")]
const SOCKET_FLAGS: SocketFlags = SocketFlags::empty();

fn close_on_exec(socket: OwnedFd) -> Result<OwnedFd> {
    // Darwin has no SOCK_CLOEXEC/accept4 flags; set the descriptor flag after
    // creation. Other platforms set it atomically through SOCKET_FLAGS.
    #[cfg(target_vendor = "apple")]
    rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC).map_err(|_| Error::Backend)?;
    Ok(socket)
}

fn socket() -> Result<OwnedFd> {
    close_on_exec(
        net::socket_with(AddressFamily::INET, SocketType::STREAM, SOCKET_FLAGS, None)
            .map_err(|_| Error::Backend)?,
    )
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
        #[cfg(target_vendor = "apple")]
        let flags = {
            // Cover borrowed host sockets as well as sockets opened here.
            sockopt::set_socket_nosigpipe(self.0, true).map_err(|_| Error::Backend)?;
            SendFlags::empty()
        };
        #[cfg(not(target_vendor = "apple"))]
        let flags = SendFlags::NOSIGNAL;
        while !bytes.is_empty() {
            match net::send(self.0, bytes, flags) {
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
