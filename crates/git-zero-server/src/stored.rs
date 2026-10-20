//! Smart HTTP protocol over an external immutable object/ref catalog. Parsing
//! never grants access: the host authorizes wants, checks common commit IDs,
//! selects a closure and supplies authenticated pack representations separately.
pub use crate::upload::{Acknowledgments, ShallowUpdates};
use crate::{route::Version, upload::UploadRequest, v2};
use git_zero_core::{
    Error, FetchOptions, ObjectFormat, ObjectId, Result,
    io::Write,
    pktline::{self, Packet},
    repository::validate_ref_name_bytes,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Noop,
    Refs,
    Fetch,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectRequest {
    Want(ObjectId),
    Have(ObjectId),
    Shallow(ObjectId),
}

enum Parsed<'a> {
    Noop,
    Refs(v2::Refs<'a>),
    Fetch(UploadRequest<'a>),
}

/// A complete validated control body; storage is borrowed, not copied. Revisit
/// its objects without an object-count array. Bound bytes before parsing it.
pub struct Request<'a> {
    parsed: Parsed<'a>,
    objects: &'a [u8],
    format: ObjectFormat,
}
impl<'a> Request<'a> {
    pub fn parse(body: &'a [u8], format: ObjectFormat, version: u8) -> Result<Self> {
        let version = version_value(version)?;
        let (parsed, objects) = if version == Version::V2 {
            match v2::Command::parse(body, format)? {
                None => (Parsed::Noop, &body[..0]),
                Some(command) if command.name == b"ls-refs" => {
                    (Parsed::Refs(v2::Refs::parse(command.args)?), &body[..0])
                }
                Some(command) => (
                    Parsed::Fetch(UploadRequest::parse(body, format, version, |_| Ok(()))?),
                    command.args,
                ),
            }
        } else {
            (
                Parsed::Fetch(UploadRequest::parse(body, format, version, |_| Ok(()))?),
                body,
            )
        };
        Ok(Self {
            parsed,
            objects,
            format,
        })
    }
    pub fn command(&self) -> Command {
        match self.parsed {
            Parsed::Noop => Command::Noop,
            Parsed::Refs(_) => Command::Refs,
            Parsed::Fetch(_) => Command::Fetch,
        }
    }
    pub fn done(&self) -> bool {
        matches!(&self.parsed, Parsed::Fetch(request) if request.done)
    }
    pub fn options(&self) -> Option<FetchOptions> {
        match &self.parsed {
            Parsed::Fetch(request) => Some(request.extensions.options),
            _ => None,
        }
    }
    pub fn require_complete(&self) -> Result<()> {
        match &self.parsed {
            Parsed::Fetch(request) => request.require_complete(),
            _ => Ok(()),
        }
    }
    pub fn include_tags(&self) -> bool {
        matches!(&self.parsed, Parsed::Fetch(request) if request.include_tags())
    }
    pub fn band(&self) -> u8 {
        match &self.parsed {
            Parsed::Fetch(request) => request.band(),
            _ => 0,
        }
    }
    pub fn objects(&self) -> Objects<'a> {
        Objects {
            bytes: self.objects,
        }
    }
    pub fn acknowledgments(&self) -> Result<Acknowledgments> {
        let Parsed::Fetch(request) = &self.parsed else {
            return Err(Error::InvalidRequest);
        };
        Ok(Acknowledgments::new(request))
    }
    pub fn shallow_updates(&self) -> Result<ShallowUpdates> {
        let Parsed::Fetch(request) = &self.parsed else {
            return Err(Error::InvalidRequest);
        };
        Ok(ShallowUpdates::new(request, self.format))
    }
    pub fn reference(
        &self,
        id: Option<ObjectId>,
        name: &[u8],
        symbolic: Option<&[u8]>,
        peeled: Option<ObjectId>,
        output: &mut impl Write,
    ) -> Result<()> {
        let Parsed::Refs(request) = &self.parsed else {
            return Err(Error::InvalidRequest);
        };
        if name != b"HEAD" {
            validate_ref_name_bytes(name)?;
        }
        if let Some(target) = symbolic {
            validate_ref_name_bytes(target)?;
        }
        if id
            .into_iter()
            .chain(peeled)
            .any(|id| id.format() != self.format || id.is_zero())
        {
            return Err(Error::InvalidObjectId);
        }
        request.write_ref(id, name, symbolic, peeled, output)
    }
}

pub struct Objects<'a> {
    bytes: &'a [u8],
}
impl Iterator for Objects<'_> {
    type Item = ObjectRequest;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(packet) = pktline::read(&mut self.bytes).ok().flatten() {
            let Packet::Data(line) = packet else {
                continue;
            };
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            let (marker, value) = crate::fetch::argument_parts(line);
            let Some(value) = value else {
                continue;
            };
            let value = value.split(|&byte| byte == b' ').next()?;
            let constructor: fn(ObjectId) -> ObjectRequest = match marker {
                b"want" => ObjectRequest::Want,
                b"have" => ObjectRequest::Have,
                b"shallow" => ObjectRequest::Shallow,
                _ => continue,
            };
            // The complete request parser has already checked every ID.
            return ObjectId::from_hex(value).ok().map(constructor);
        }
        None
    }
}

fn version_value(version: u8) -> Result<Version> {
    match version {
        0 => Ok(Version::V0),
        1 => Ok(Version::V1),
        2 => Ok(Version::V2),
        _ => Err(Error::Unsupported),
    }
}

pub fn protocol_version(header: Option<&str>) -> Result<u8> {
    Ok(match crate::route::version(header)? {
        Version::V0 => 0,
        Version::V1 => 1,
        Version::V2 => 2,
    })
}

pub fn advertise(
    format: ObjectFormat,
    version: u8,
    selection: bool,
    output: &mut impl Write,
) -> Result<()> {
    let version = version_value(version)?;
    pktline::write(output, b"# service=git-upload-pack\n")?;
    output.write_all(b"0000")?;
    if version == Version::V2 {
        v2::advertise(format, output, selection)?;
    } else if version == Version::V1 {
        pktline::write(output, b"version 1\n")?;
    }
    Ok(())
}

/// v0/v1 ref projection. The host supplies a stable sorted projection, HEAD
/// first when resolved; `first` carries capabilities once. None is the empty
/// repository carrier. Symbolic HEAD targets are raw validated ref bytes.
pub fn reference(
    format: ObjectFormat,
    id: Option<ObjectId>,
    name: &[u8],
    first: bool,
    head: Option<&[u8]>,
    selection: bool,
    output: &mut impl Write,
) -> Result<()> {
    if name != b"HEAD" && id.is_some() {
        validate_ref_name_bytes(name)?;
    }
    if id.is_some_and(|id| id.format() != format || id.is_zero()) {
        return Err(Error::InvalidObjectId);
    }
    if let Some(target) = head {
        validate_ref_name_bytes(target)?;
    }
    let id = id.unwrap_or_else(|| format.zero_id()).to_hex();
    let name = if name.is_empty() {
        b"capabilities^{}".as_slice()
    } else {
        name
    };
    if first {
        let target: &[u8] = head.unwrap_or_default();
        let symref: &[u8] = if head.is_some() {
            b" symref=HEAD:"
        } else {
            b""
        };
        pktline::write_parts(
            output,
            &[
                id.as_bytes(),
                b" ",
                name,
                b"\0",
                format.capability(),
                crate::upload::capabilities(selection),
                symref,
                target,
                b"\n",
            ],
        )
    } else {
        pktline::write_parts(output, &[id.as_bytes(), b" ", name, b"\n"])
    }
}

/// Frame authenticated raw pack bytes, preserving both negotiated sideband
/// limits. The host emits a flush after the pack checksum when band != 0.
pub fn pack_chunk(band: u8, bytes: &[u8], output: &mut impl Write) -> Result<()> {
    let capacity = match band {
        0 => return output.write_all(bytes),
        1 => 995,
        2 => pktline::MAX_LENGTH - 5,
        _ => return Err(Error::InvalidRequest),
    };
    for bytes in bytes.chunks(capacity) {
        pktline::write_parts(output, &[b"\x01", bytes])?;
    }
    Ok(())
}

pub fn peeled_reference(
    format: ObjectFormat,
    id: ObjectId,
    name: &[u8],
    output: &mut impl Write,
) -> Result<()> {
    validate_ref_name_bytes(name)?;
    if id.format() != format || id.is_zero() {
        return Err(Error::InvalidObjectId);
    }
    pktline::write_parts(output, &[id.to_hex().as_bytes(), b" ", name, b"^{}\n"])
}
