//! Bounded socket transport shared by native HTTP framing tests.
use git_zero::{Error, http::Request, io::Write as _};
use git_zero_rustix::{SocketWriter, read_request};
use rustix::{
    fd::AsFd,
    net::{self, sockopt},
};
use std::{thread, time::Duration};

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub content_type: Option<String>,
    pub git_protocol: Option<String>,
    pub body: Vec<u8>,
}

impl From<Request<'_>> for Parsed {
    fn from(request: Request<'_>) -> Self {
        Self {
            method: request.method.into(),
            path: request.path.into(),
            query: request.query.map(str::to_owned),
            content_type: request.content_type.map(str::to_owned),
            git_protocol: request.git_protocol.map(str::to_owned),
            body: request.body.into(),
        }
    }
}

pub fn receive(wire: &[u8], width: usize, storage: &mut [u8]) -> Result<Parsed, Error> {
    let (reader, writer) = net::socketpair(
        net::AddressFamily::UNIX,
        net::SocketType::STREAM,
        net::SocketFlags::empty(),
        None,
    )
    .unwrap();
    sockopt::set_socket_timeout(
        &reader,
        sockopt::Timeout::Recv,
        Some(Duration::from_secs(2)),
    )
    .unwrap();
    sockopt::set_socket_timeout(
        &writer,
        sockopt::Timeout::Send,
        Some(Duration::from_secs(2)),
    )
    .unwrap();
    let wire = wire.to_vec();
    let sender = thread::spawn(move || -> Result<(), Error> {
        for chunk in wire.chunks(width) {
            SocketWriter(writer.as_fd()).write_all(chunk)?;
        }
        // Truncated requests reach EOF immediately rather than waiting for a
        // timeout. The reader may already have closed after an early rejection.
        let _ = net::shutdown(&writer, net::Shutdown::Write);
        Ok(())
    });
    let result = read_request(&reader, storage).map(Parsed::from);
    drop(reader);
    let sent = sender.join().unwrap();
    if result.is_ok() {
        sent.unwrap();
    }
    result
}
