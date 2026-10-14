//! Bounded HTTP fetch primitives. The host supplies transport and storage.
//! Smart fetch returns a validated raw pack; dumb fetch returns a zlib loose
//! object. Bounded loose-object decoding validates headers, lengths and IDs;
//! `crate::zlib` also provides standalone inflation, and `crate::pack` provides
//! entry/delta decoding and bounded materialized indexes. The host owns storage
//! and checkout policy.
use crate::{
    Error, Object, ObjectId, Result,
    http::Request,
    io::{SliceWriter, Write},
    zlib::{InflateLimits, InflateStatus, Inflater, WINDOW_SIZE},
};

mod smart;
pub use smart::{
    FetchOptions, SmartAdvertisement, read_pack_response, read_smart_reference, write_fetch_request,
};

pub trait Transport {
    /// Paths are URL paths relative to a fixed repository URL, with reserved
    /// bytes percent-encoded. Return status and body length.
    /// Decode HTTP transfer framing before returning. Do not enable protocol v2.
    fn request(&mut self, request: Request<'_>, response: &mut [u8]) -> Result<(u16, usize)>;
}

/// Wire capacity for a default-zlib loose object at the given body limit,
/// including up to 32 bytes of canonical Git header. Non-default encoders may
/// need a separate wire budget; the inflater's output budget is independent.
pub fn loose_wire_capacity(body_limit: usize) -> Result<usize> {
    let source = body_limit.checked_add(32).ok_or(Error::LimitExceeded)?;
    usize::try_from(crate::zlib::compress_bound(source as u64)?).map_err(|_| Error::LimitExceeded)
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
    /// Parse a complete response without performing transport I/O.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let (protocol, bytes) = if data
            .get(4..)
            .is_some_and(|tail| tail.starts_with(b"# service="))
        {
            SmartAdvertisement::parse(data)?;
            (Protocol::Smart, smart::advertisement_body(data)?)
        } else {
            (Protocol::Dumb, data)
        };
        let discovery = Self { protocol, bytes };
        for reference in discovery.references() {
            reference?;
        }
        Ok(discovery)
    }
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
            Protocol::Smart => {
                let result = smart::next_reference(&mut self.bytes);
                if !matches!(result, Ok(Some(_))) {
                    self.failed = true;
                }
                return result.transpose();
            }
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
    Discovery::parse(storage.get(..length).ok_or(Error::BufferTooSmall)?)
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
    fetch_pack_with_options(
        transport,
        &[want],
        FetchOptions::default(),
        request_storage,
        response_storage,
    )
}

/// Fetch several wanted tips using optional shallow and offset-delta support.
/// Caller storage and the transport's limits bound the request and response.
pub fn fetch_pack_with_options<'a>(
    transport: &mut impl Transport,
    wants: &[ObjectId],
    options: FetchOptions,
    request_storage: &mut [u8],
    response_storage: &'a mut [u8],
) -> Result<&'a [u8]> {
    let mut request = SliceWriter::new(request_storage);
    write_fetch_request(wants, options, &mut request)?;
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
    let bytes = response_storage
        .get(..length)
        .ok_or(Error::BufferTooSmall)?;
    read_pack_response(bytes)
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
