//! Protocol-v0 discovery, bounded fetch negotiation and raw-pack responses.
use super::{Protocol, Reference, References};
use git_zero_core::{
    Error, ObjectFormat, ObjectId, Result,
    io::{SliceWriter, Write},
    pack,
    pktline::{self, Packet},
};

/// A validated smart-HTTP advertisement. Empty repositories are valid but have
/// no HEAD. Capabilities and reference names borrow the caller's response.
pub struct SmartAdvertisement<'a> {
    bytes: &'a [u8],
    body_offset: usize,
    head: Option<ObjectId>,
    capabilities: &'a [u8],
    format: Option<ObjectFormat>,
}

impl<'a> SmartAdvertisement<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let body = advertisement_body(data)?;
        let mut first = body;
        let capabilities = match pktline::read(&mut first)? {
            Some(Packet::Data(line)) => line.splitn(2, |&b| b == 0).nth(1).unwrap_or_default(),
            _ => &[],
        };
        let mut head = None;
        let mut refs = References {
            protocol: Protocol::Smart,
            bytes: body,
            failed: false,
            format: None,
        };
        for entry in &mut refs {
            let entry = entry?;
            if entry.name == "HEAD" && head.replace(entry.id).is_some() {
                return Err(Error::InvalidReference);
            }
        }
        Ok(Self {
            bytes: data,
            body_offset: data.len() - body.len(),
            head,
            capabilities,
            format: refs.format,
        })
    }

    pub fn head(&self) -> Result<ObjectId> {
        self.head.ok_or(Error::ObjectNotFound)
    }
    pub fn object_format(&self) -> Option<ObjectFormat> {
        self.format
    }
    pub fn supports_shallow(&self) -> bool {
        self.supports("shallow")
    }
    /// Test an exact advertised capability token, without accepting prefixes.
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities
            .split(|b| b.is_ascii_whitespace())
            .any(|flag| flag == capability.as_bytes())
    }
    /// Request only capabilities advertised by this origin. An origin without
    /// shallow support falls back to a full fetch within the host's budgets.
    pub fn fetch_options(&self, shallow_depth: Option<u32>) -> FetchOptions {
        FetchOptions {
            shallow_depth: shallow_depth.filter(|_| self.supports_shallow()),
            ofs_delta: self.supports("ofs-delta"),
            no_progress: self.supports("no-progress"),
        }
    }
    /// Validate protocol permission to request an ID. Unadvertised IDs require
    /// an allow-*-sha1-in-want capability; the server still checks whether the ID
    /// is an allowed hidden tip or reachable object under that capability.
    pub fn validate_want(&self, id: ObjectId) -> Result<()> {
        if self.format.is_some_and(|format| format != id.format()) {
            return Err(Error::InvalidRequest);
        }
        if self.supports("allow-tip-sha1-in-want") || self.supports("allow-reachable-sha1-in-want")
        {
            return Ok(());
        }
        for reference in self.references() {
            if reference?.id == id {
                return Ok(());
            }
        }
        Err(Error::InvalidRequest)
    }
    pub fn references(&self) -> References<'a> {
        References {
            protocol: Protocol::Smart,
            bytes: &self.bytes[self.body_offset..],
            failed: false,
            format: self.format,
        }
    }
}

pub(super) fn advertisement_body(mut data: &[u8]) -> Result<&[u8]> {
    if !matches!(
        pktline::read(&mut data)?,
        Some(Packet::Data(b"# service=git-upload-pack\n"))
    ) || !matches!(pktline::read(&mut data)?, Some(Packet::Flush))
    {
        return Err(Error::InvalidPacket);
    }
    Ok(data)
}

pub(super) fn next_reference<'a>(
    bytes: &mut &'a [u8],
    format: &mut Option<ObjectFormat>,
) -> Result<Option<Reference<'a>>> {
    loop {
        match pktline::read(bytes)? {
            Some(Packet::Data(line)) => {
                let mut parser = References {
                    protocol: Protocol::Smart,
                    bytes: &[],
                    failed: false,
                    format: *format,
                };
                let reference = parser.parse(line)?;
                *format = parser.format;
                if reference.id.is_zero() && reference.name == "capabilities^{}" {
                    continue;
                }
                if reference.name != "HEAD" {
                    git_zero_core::repository::validate_ref_name(
                        reference.name.strip_suffix("^{}").unwrap_or(reference.name),
                    )?;
                }
                return Ok(Some(reference));
            }
            Some(Packet::Flush) if bytes.is_empty() => return Ok(None),
            _ => return Err(Error::InvalidPacket),
        }
    }
}

/// Resume a smart advertisement at a byte cursor. Start at zero; successful
/// reads advance past one ref, including peeled tag pseudo-refs. EOF is stable.
/// Validate the complete advertisement with SmartAdvertisement::parse first
/// when exposing refs incrementally to callers.
pub fn read_smart_reference<'a>(
    data: &'a [u8],
    offset: &mut usize,
) -> Result<Option<Reference<'a>>> {
    if *offset == data.len() && *offset != 0 {
        return Ok(None);
    }
    let body = advertisement_body(data)?;
    let mut first = body;
    let mut parser = References {
        protocol: Protocol::Smart,
        bytes: &[],
        failed: false,
        format: None,
    };
    if let Some(Packet::Data(line)) = pktline::read(&mut first)? {
        parser.parse(line)?;
    }
    let mut bytes = if *offset == 0 {
        body
    } else {
        data.get(*offset..).ok_or(Error::InvalidPacket)?
    };
    let entry = next_reference(&mut bytes, &mut parser.format)?;
    *offset = data.len() - bytes.len();
    Ok(entry)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FetchOptions {
    /// None fetches full history; Some(1) fetches each wanted tip's snapshot.
    /// The host must check the advertisement's shallow capability first.
    pub shallow_depth: Option<u32>,
    pub ofs_delta: bool,
    pub no_progress: bool,
}

/// Encode a protocol-v0 request without thin packs or sideband. Duplicate
/// wants are omitted using bounded quadratic probes over caller-owned IDs.
/// Invalid inputs emit nothing; discard partial output on storage/I/O error.
pub fn write_fetch_request(
    wants: &[ObjectId],
    options: FetchOptions,
    output: &mut impl Write,
) -> Result<()> {
    write_fetch_request_iter(wants.iter().copied(), options, output)
}

/// Encode a cloneable iterator of IDs, avoiding temporary ID arrays in adapters.
/// Duplicate probes reread the iterator; it must yield the same IDs on every
/// clone. All wants use one format. Validation precedes any output.
pub fn write_fetch_request_iter(
    wants: impl Iterator<Item = ObjectId> + Clone,
    options: FetchOptions,
    output: &mut impl Write,
) -> Result<()> {
    let format = wants.clone().next().ok_or(Error::InvalidRequest)?.format();
    if options.shallow_depth == Some(0) || wants.clone().any(|want| want.format() != format) {
        return Err(Error::InvalidRequest);
    }
    for (position, want) in wants.clone().enumerate() {
        if wants
            .clone()
            .take(position)
            .any(|previous| previous == want)
        {
            continue;
        }
        let mut storage = [0; 128];
        let mut line = SliceWriter::new(&mut storage);
        line.write_all(b"want ")?;
        line.write_all(&want.to_hex())?;
        if position == 0 {
            if format == ObjectFormat::Sha256 {
                line.write_all(b" ")?;
                line.write_all(format.capability())?;
            }
            if options.ofs_delta {
                line.write_all(b" ofs-delta")?;
            }
            if options.no_progress {
                line.write_all(b" no-progress")?;
            }
            if options.shallow_depth.is_some() {
                line.write_all(b" shallow")?;
            }
        }
        line.write_all(b"\n")?;
        pktline::write(output, line.as_bytes())?;
    }
    if let Some(depth) = options.shallow_depth {
        use core::fmt::Write as _;
        let mut storage = [0; 32];
        let mut line = SliceWriter::new(&mut storage);
        writeln!(&mut line, "deepen {depth}").map_err(|_| Error::BufferTooSmall)?;
        pktline::write(output, line.as_bytes())?;
    }
    output.write_all(b"0000")?;
    pktline::write(output, b"done\n")
}

/// Extract a checksummed, non-sideband pack after optional shallow negotiation.
/// ACK/have negotiation and protocol v2 are intentionally outside this parser.
pub fn read_pack_response(bytes: &[u8]) -> Result<&[u8]> {
    read_pack_response_with_format(bytes, ObjectFormat::Sha1)
}

/// Validate a response in the repository's negotiated object format, including
/// each shallow boundary ID and the full-width pack checksum.
pub fn read_pack_response_with_format(mut bytes: &[u8], format: ObjectFormat) -> Result<&[u8]> {
    loop {
        match pktline::read(&mut bytes)? {
            Some(Packet::Data(b"NAK\n")) => break,
            Some(Packet::Data(line)) if line.starts_with(b"shallow ") => {
                if ObjectId::from_hex(&line.strip_suffix(b"\n").unwrap_or(line)[8..])?.format()
                    != format
                {
                    return Err(Error::InvalidRequest);
                }
            }
            Some(Packet::Flush) => {}
            _ => return Err(Error::InvalidPacket),
        }
    }
    pack::validate_with_format(bytes, format)?;
    Ok(bytes)
}
