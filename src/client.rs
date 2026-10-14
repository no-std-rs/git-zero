//! Bounded HTTP fetch primitives. The host supplies transport and storage.
//! Smart fetch returns a validated raw pack; dumb fetch returns a zlib loose
//! object. Bounded loose-object decoding validates headers, lengths and IDs;
//! `crate::zlib` also provides standalone inflation, and `crate::pack` provides
//! entry/delta decoding. Graph traversal, checkout and pack indexing remain work.
use crate::{
    Error, Object, ObjectFormat, ObjectId, Result,
    http::Request,
    io::{SliceWriter, Write},
    pack,
    pktline::{self, Packet},
    zlib::{InflateLimits, InflateStatus, Inflater, WINDOW_SIZE},
};

pub trait Transport {
    /// Paths are URL paths relative to a fixed repository URL, with reserved
    /// bytes percent-encoded. Return status and body length.
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
            format: None,
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
    format: Option<ObjectFormat>,
}

impl<'a> References<'a> {
    fn parse(&mut self, line: &'a [u8]) -> Result<Reference<'a>> {
        let mut parts = line.splitn(2, |&byte| byte == 0);
        let reference = parts.next().ok_or(Error::InvalidRequest)?;
        let reference = reference.strip_suffix(b"\n").unwrap_or(reference);
        let separator = reference
            .iter()
            .position(|&byte| matches!(byte, b' ' | b'\t'))
            .ok_or(Error::InvalidRequest)?;
        if reference.len() < separator + 2 {
            return Err(Error::InvalidRequest);
        }
        let id = ObjectId::from_hex(&reference[..separator])?;
        let advertised = if self.protocol == Protocol::Smart {
            parts.next().and_then(|capabilities| {
                capabilities
                    .strip_suffix(b"\n")
                    .unwrap_or(capabilities)
                    .split(|&byte| byte == b' ')
                    .find_map(|capability| capability.strip_prefix(b"object-format="))
            })
        } else {
            None
        };
        let format = match advertised {
            Some(b"sha1") => ObjectFormat::Sha1,
            Some(b"sha256") => ObjectFormat::Sha256,
            Some(_) => return Err(Error::Unsupported),
            None => id.format(),
        };
        // The first advertised algorithm encodes the refs. Without a capability,
        // infer it from the first ID (including the empty capabilities marker).
        let expected = *self.format.get_or_insert(format);
        if format != expected || id.format() != expected {
            return Err(Error::InvalidRequest);
        }
        Ok(Reference {
            id,
            name: core::str::from_utf8(&reference[separator + 1..])
                .map_err(|_| Error::InvalidRequest)?,
        })
    }
}

impl<'a> Iterator for References<'a> {
    type Item = Result<Reference<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            let line = match self.protocol {
                Protocol::Smart => match pktline::read(&mut self.bytes) {
                    Ok(Some(Packet::Data(line))) => line,
                    Ok(Some(Packet::Flush)) => continue,
                    Ok(None) => return None,
                    Err(error) => {
                        self.failed = true;
                        return Some(Err(error));
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
            let result = self.parse(line);
            if let Ok(reference) = &result {
                if self.protocol == Protocol::Smart
                    && reference.id.is_zero()
                    && reference.name == "capabilities^{}"
                {
                    continue;
                }
            }
            if result.is_err() {
                self.failed = true;
            }
            return Some(result);
        }
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
    let mut line_storage = [0; 128];
    let mut line = SliceWriter::new(&mut line_storage);
    line.write_all(b"want ")?;
    line.write_all(&want.to_hex())?;
    if want.format() == ObjectFormat::Sha256 {
        line.write_all(b" ")?;
        line.write_all(want.format().capability())?;
    }
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
    pack::validate_with_format(bytes, want.format())?;
    Ok(bytes)
}

/// Fetch a loose object's zlib bytes from an ordinary HTTP repository.
pub fn fetch_loose<'a>(
    transport: &mut impl Transport,
    id: ObjectId,
    response_storage: &'a mut [u8],
) -> Result<&'a [u8]> {
    let hex = id.to_hex();
    let mut path_storage = [0; 73];
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

/// Decode exactly one loose response into caller-owned staging storage.
/// Storage/limits include the Git header (at most 28 bytes) and body. A borrowed
/// body is returned only after zlib, canonical header, length and ID validation.
/// Discard staging bytes on error; concatenated streams/trailing bytes fail.
pub fn decode_loose<'a>(
    compressed: &[u8],
    expected: ObjectId,
    storage: &'a mut [u8],
    window: &mut [u8; WINDOW_SIZE],
    limits: InflateLimits,
) -> Result<Object<'a>> {
    let mut decoder = Inflater::new(window, limits);
    let progress = decoder.update(compressed, storage, true)?;
    if progress.status == InflateStatus::NeedOutput {
        return Err(Error::BufferTooSmall);
    }
    if progress.status != InflateStatus::Finished || progress.consumed != compressed.len() {
        return Err(Error::InvalidObject);
    }
    Object::from_loose(&storage[..progress.written], expected)
}

/// Fetch and validate a dumb-HTTP loose object using independent wire and
/// inflated buffers. The transport remains responsible for HTTP framing.
pub fn fetch_loose_object<'a>(
    transport: &mut impl Transport,
    expected: ObjectId,
    response_storage: &mut [u8],
    object_storage: &'a mut [u8],
    window: &mut [u8; WINDOW_SIZE],
    limits: InflateLimits,
) -> Result<Object<'a>> {
    let compressed = fetch_loose(transport, expected, response_storage)?;
    decode_loose(compressed, expected, object_storage, window, limits)
}
