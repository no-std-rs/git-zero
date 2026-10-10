//! Bounded HTTP fetch primitives. The host supplies transport and storage.
//! Smart fetch returns a validated raw pack; dumb fetch returns a zlib loose
//! object. Checkout, inflation, delta resolution and pack indexing are not provided.
use crate::{
    Error, ObjectId, Result,
    http::Request,
    io::{SliceWriter, Write},
    pack,
    pktline::{self, Packet},
};

pub trait Transport {
    /// Paths are relative to a fixed repository URL. Return status and body length.
    /// Decode HTTP transfer framing before returning. Do not enable protocol v2.
    fn request(&mut self, request: Request<'_>, response: &mut [u8]) -> Result<(u16, usize)>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Smart,
    Dumb,
}

#[derive(Clone, Copy, Debug)]
pub struct Reference<'a> {
    pub id: ObjectId,
    pub name: &'a str,
}

pub struct Discovery<'a> {
    pub protocol: Protocol,
    bytes: &'a [u8],
}

impl<'a> Discovery<'a> {
    pub fn references(&self) -> References<'a> {
        References {
            protocol: self.protocol,
            bytes: self.bytes,
            failed: false,
        }
    }

    /// Smart HEAD when present; otherwise the first advertised branch.
    /// Dumb callers can use `read_head` to resolve the actual default branch.
    pub fn head(&self) -> Result<ObjectId> {
        let mut first = None;
        for reference in self.references() {
            let reference = reference?;
            if reference.name == "HEAD" {
                return Ok(reference.id);
            }
            first.get_or_insert(reference.id);
        }
        first.ok_or(Error::ObjectNotFound)
    }
}

pub struct References<'a> {
    protocol: Protocol,
    bytes: &'a [u8],
    failed: bool,
}

impl<'a> Iterator for References<'a> {
    type Item = Result<Reference<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let line = match self.protocol {
            Protocol::Smart => loop {
                match pktline::read(&mut self.bytes) {
                    Ok(Some(Packet::Data(line))) => break line,
                    Ok(Some(Packet::Flush)) => continue,
                    Ok(None) => return None,
                    Err(error) => {
                        self.failed = true;
                        return Some(Err(error));
                    }
                }
            },
            Protocol::Dumb => {
                if self.bytes.is_empty() {
                    return None;
                }
                let end = self
                    .bytes
                    .iter()
                    .position(|&byte| byte == b'\n')
                    .unwrap_or(self.bytes.len());
                let line = &self.bytes[..end];
                self.bytes = self.bytes.get(end + 1..).unwrap_or_default();
                line
            }
        };
        let result = (|| {
            let line = line
                .split(|&byte| byte == 0)
                .next()
                .ok_or(Error::InvalidRequest)?;
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            if line.len() < 42 || !matches!(line[40], b' ' | b'\t') {
                return Err(Error::InvalidRequest);
            }
            Ok(Reference {
                id: ObjectId::from_hex(&line[..40])?,
                name: core::str::from_utf8(&line[41..]).map_err(|_| Error::InvalidRequest)?,
            })
        })();
        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }
}

pub fn discover<'a>(
    transport: &mut impl Transport,
    storage: &'a mut [u8],
) -> Result<Discovery<'a>> {
    let (status, length) = transport.request(
        Request {
            method: "GET",
            path: "info/refs",
            query: Some("service=git-upload-pack"),
            body: &[],
        },
        storage,
    )?;
    if status != 200 {
        return Err(Error::Backend);
    }
    let mut bytes = storage.get(..length).ok_or(Error::BufferTooSmall)?;
    let protocol = if bytes.starts_with(b"001e# service=git-upload-pack\n0000") {
        bytes = &bytes[34..];
        Protocol::Smart
    } else {
        Protocol::Dumb
    };
    let discovery = Discovery { protocol, bytes };
    // Validate every advertised ref now, rather than silently accepting truncation.
    for reference in discovery.references() {
        reference?;
    }
    Ok(discovery)
}

pub fn read_head(
    transport: &mut impl Transport,
    discovery: &Discovery<'_>,
    storage: &mut [u8],
) -> Result<ObjectId> {
    if discovery.protocol == Protocol::Smart {
        return discovery.head();
    }
    let (status, length) = transport.request(
        Request {
            method: "GET",
            path: "HEAD",
            query: None,
            body: &[],
        },
        storage,
    )?;
    if status != 200 {
        return Err(Error::Backend);
    }
    let bytes = storage.get(..length).ok_or(Error::BufferTooSmall)?;
    let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if let Some(name) = line.strip_prefix(b"ref: ") {
        for reference in discovery.references() {
            let reference = reference?;
            if reference.name.as_bytes() == name {
                return Ok(reference.id);
            }
        }
        Err(Error::ObjectNotFound)
    } else {
        ObjectId::from_hex(line)
    }
}

/// Ask for a full reachable pack, without thin packs or sideband capabilities.
pub fn fetch_pack<'a>(
    transport: &mut impl Transport,
    want: ObjectId,
    request_storage: &mut [u8],
    response_storage: &'a mut [u8],
) -> Result<&'a [u8]> {
    let mut request = SliceWriter::new(request_storage);
    let mut line_storage = [0; 64];
    let mut line = SliceWriter::new(&mut line_storage);
    line.write_all(b"want ")?;
    line.write_all(&want.to_hex())?;
    line.write_all(b"\n")?;
    pktline::write(&mut request, line.as_bytes())?;
    request.write_all(b"0000")?;
    pktline::write(&mut request, b"done\n")?;
    let (status, length) = transport.request(
        Request {
            method: "POST",
            path: "git-upload-pack",
            query: None,
            body: request.as_bytes(),
        },
        response_storage,
    )?;
    if status != 200 {
        return Err(Error::Backend);
    }
    let mut bytes = response_storage
        .get(..length)
        .ok_or(Error::BufferTooSmall)?;
    match pktline::read(&mut bytes)? {
        Some(Packet::Data(b"NAK\n")) => {}
        _ => return Err(Error::InvalidPacket),
    }
    pack::validate(bytes)?;
    Ok(bytes)
}

/// Fetch a loose object's zlib bytes from an ordinary HTTP repository.
pub fn fetch_loose<'a>(
    transport: &mut impl Transport,
    id: ObjectId,
    response_storage: &'a mut [u8],
) -> Result<&'a [u8]> {
    let hex = id.to_hex();
    let mut path_storage = [0; 49];
    let mut path = SliceWriter::new(&mut path_storage);
    path.write_all(b"objects/")?;
    path.write_all(&hex[..2])?;
    path.write_all(b"/")?;
    path.write_all(&hex[2..])?;
    let path = core::str::from_utf8(path.as_bytes()).map_err(|_| Error::InvalidRequest)?;
    let (status, length) = transport.request(
        Request {
            method: "GET",
            path,
            query: None,
            body: &[],
        },
        response_storage,
    )?;
    if status == 404 {
        return Err(Error::ObjectNotFound);
    }
    if status != 200 {
        return Err(Error::Backend);
    }
    response_storage.get(..length).ok_or(Error::BufferTooSmall)
}
