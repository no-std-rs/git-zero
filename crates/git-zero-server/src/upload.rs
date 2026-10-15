//! Stateless v0 negotiation. Parse the complete bounded body before output.
use git_zero_core::{
    Error, ObjectFormat, ObjectId, ObjectKind, Repository, Result,
    io::Write,
    pktline::{self, Packet},
};

pub(crate) fn capabilities(selection: bool) -> &'static [u8] {
    if selection {
        git_zero_core::repository::UPLOAD_CAPABILITIES
    } else {
        b" multi_ack multi_ack_detailed side-band side-band-64k no-progress"
    }
}

#[derive(Clone, Copy, Default)]
struct Capabilities(u16);

const CAPABILITY_FLAGS: &[(&[u8], u16)] = &[
    (b"multi_ack", 1),
    (b"multi_ack_detailed", 2),
    (b"side-band", 4),
    (b"side-band-64k", 8),
    (b"no-progress", 16),
    (b"shallow", 64),
    (b"filter", 128),
    (b"deepen-relative", 256),
];

fn capability_flag(capability: &[u8], format: ObjectFormat) -> Result<u16> {
    if capability == format.capability() {
        return Ok(32);
    }
    if matches!(
        capability,
        b"object-format=sha1" | b"object-format=sha256" | b""
    ) {
        return Err(Error::InvalidRequest);
    }
    CAPABILITY_FLAGS
        .iter()
        .find_map(|&(name, flag)| (name == capability).then_some(flag))
        .ok_or(Error::Unsupported)
}

impl Capabilities {
    fn parse(bytes: &[u8], format: ObjectFormat) -> Result<Self> {
        let mut seen = 0_u16;
        for capability in bytes.split(|&byte| byte == b' ') {
            let flag = capability_flag(capability, format)?;
            if seen & flag != 0 {
                return Err(Error::InvalidRequest);
            }
            seen |= flag;
        }
        if seen & 3 == 3 || seen & 12 == 12 {
            return Err(Error::InvalidRequest);
        }
        Ok(Self(seen))
    }

    fn want(&mut self, value: &[u8], first: bool, format: ObjectFormat) -> Result<ObjectId> {
        let mut hex = value;
        if let Some(split) = value.iter().position(|&byte| byte == b' ') {
            if !first {
                return Err(Error::InvalidRequest);
            }
            *self = Self::parse(&value[split + 1..], format)?;
            hex = &value[..split];
        }
        id(hex, format)
    }

    fn ack(self) -> u8 {
        (self.0 & 3) as u8
    }
    fn band(self) -> u8 {
        ((self.0 >> 2) & 3) as u8
    }
}

pub(crate) struct UploadRequest<'a> {
    pub done: bool,
    capabilities: Capabilities,
    haves: &'a [u8],
    v2: bool,
    #[cfg(feature = "read")]
    shallow_body: &'a [u8],
    pub extensions: crate::fetch::Extensions,
    #[cfg(feature = "read")]
    include_tags: bool,
}

pub(crate) fn id(bytes: &[u8], format: ObjectFormat) -> Result<ObjectId> {
    let id = ObjectId::from_hex(bytes)?;
    if id.format() != format || id.is_zero() {
        return Err(Error::InvalidRequest);
    }
    Ok(id)
}

fn request_line(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

fn unexpected_command(line: &[u8]) -> Error {
    if line.starts_with(b"want ") || line.starts_with(b"have ") || line == b"done" {
        Error::InvalidRequest
    } else {
        Error::Unsupported
    }
}

fn parse_wants(
    bytes: &mut &[u8],
    format: ObjectFormat,
    mut want: impl FnMut(ObjectId) -> Result<()>,
    extensions: &mut crate::fetch::Extensions,
) -> Result<Capabilities> {
    let mut wanted = false;
    let mut extended = false;
    let mut capabilities = Capabilities::default();
    while let Some(packet) = pktline::read(bytes)? {
        let Packet::Data(line) = packet else {
            if packet != Packet::Flush {
                return Err(Error::InvalidRequest);
            }
            // Shallow commands use the server's advertisement; Git does not
            // echo `shallow` in its want capabilities. Filter is negotiated.
            extensions.options.deepen_relative |= capabilities.0 & 256 != 0;
            extensions.require_filter_capability(capabilities.0 & 128 != 0)?;
            return wanted.then_some(capabilities).ok_or(Error::InvalidRequest);
        };
        let line = request_line(line);
        if let Some(value) = line.strip_prefix(b"want ") {
            if extended {
                return Err(Error::InvalidRequest);
            }
            want(capabilities.want(value, !wanted, format)?)?;
            wanted = true;
        } else if wanted {
            extensions.argument(line, format)?;
            extended = true;
        } else {
            return Err(unexpected_command(line));
        }
    }
    Err(Error::InvalidRequest)
}

fn parse_haves(mut bytes: &[u8], format: ObjectFormat) -> Result<bool> {
    let empty = bytes.is_empty();
    while let Some(packet) = pktline::read(&mut bytes)? {
        let Packet::Data(line) = packet else {
            return bytes
                .is_empty()
                .then_some(false)
                .ok_or(Error::InvalidRequest);
        };
        let line = request_line(line);
        if line == b"done" && bytes.is_empty() {
            return Ok(true);
        }
        let value = line
            .strip_prefix(b"have ")
            .ok_or_else(|| unexpected_command(line))?;
        id(value, format)?;
    }
    empty.then_some(false).ok_or(Error::InvalidRequest)
}

impl<'a> UploadRequest<'a> {
    pub fn parse(
        mut bytes: &'a [u8],
        format: ObjectFormat,
        version: crate::route::Version,
        want: impl FnMut(ObjectId) -> Result<()>,
    ) -> Result<Self> {
        let mut extensions = crate::fetch::Extensions::default();
        #[cfg(feature = "read")]
        let shallow_body = bytes;
        if version == crate::route::Version::V2 {
            let command = crate::v2::Command::parse(bytes, format)?.ok_or(Error::InvalidRequest)?;
            if command.name != b"fetch" {
                return Err(Error::InvalidRequest);
            }
            let fetch = crate::v2::Fetch::parse(command.args, format, want, &mut extensions)?;
            return Ok(Self {
                done: fetch.done,
                capabilities: Capabilities::default(),
                haves: command.args,
                v2: true,
                #[cfg(feature = "read")]
                shallow_body: command.args,
                extensions,
                #[cfg(feature = "read")]
                include_tags: fetch.include_tags,
            });
        }
        let capabilities = parse_wants(&mut bytes, format, want, &mut extensions)?;
        Ok(Self {
            done: parse_haves(bytes, format)?,
            capabilities,
            haves: bytes,
            v2: false,
            #[cfg(feature = "read")]
            shallow_body,
            extensions,
            #[cfg(feature = "read")]
            include_tags: false,
        })
    }

    pub fn require_complete(&self) -> Result<()> {
        if self.extensions.active() || self.capabilities.0 & 192 != 0 {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    #[cfg(feature = "read")]
    pub fn shallow_info(
        &self,
        objects: &[Option<git_zero_core::ValidationMetadata>],
        output: &mut impl Write,
    ) -> Result<()> {
        if self.extensions.options.depth.is_none() || (self.v2 && !self.done) {
            return Ok(());
        }
        let mut changes = objects
            .iter()
            .flatten()
            .filter_map(shallow_change)
            .peekable();
        if self.v2 && changes.peek().is_none() {
            return Ok(());
        }
        if self.v2 {
            pktline::write(output, b"shallow-info\n")?;
        }
        for (marker, id) in changes {
            pktline::write_parts(output, &[marker, &id.to_hex(), b"\n"])?;
        }
        output.write_all(if self.v2 { b"0001" } else { b"0000" })
    }

    #[cfg(feature = "read")]
    pub(crate) fn parse_selected(
        repository: &impl Repository,
        body: &'a [u8],
        version: crate::route::Version,
        wants: &mut [ObjectId],
    ) -> Result<(Self, usize)> {
        let mut count = 0;
        let upload = Self::parse(body, repository.object_format(), version, |id| {
            if !crate::advertised(repository, id) {
                return Err(Error::InvalidRequest);
            }
            if !wants[..count].contains(&id) {
                *wants.get_mut(count).ok_or(Error::BufferTooSmall)? = id;
                count += 1;
            }
            Ok(())
        })?;
        Ok((upload, count))
    }

    /// Revisit validated packets without retaining an array of have IDs. Only
    /// present commits are acknowledged or used to exclude complete history.
    pub fn common<'r>(
        &'r self,
        repository: &'r impl Repository,
    ) -> impl Iterator<Item = Result<ObjectId>> + 'r {
        let mut bytes = self.haves;
        core::iter::from_fn(move || pktline::read(&mut bytes).ok().flatten())
            .filter_map(have_id)
            .filter_map(move |id| common_commit(repository, id).transpose())
    }

    #[cfg(feature = "read")]
    pub fn shallow(&self) -> impl Iterator<Item = Result<ObjectId>> + '_ {
        let mut bytes = self.shallow_body;
        core::iter::from_fn(move || pktline::read(&mut bytes).ok().flatten()).filter_map(|packet| {
            let Packet::Data(line) = packet else {
                return None;
            };
            let hex = line.strip_prefix(b"shallow ")?;
            Some(ObjectId::from_hex(hex.strip_suffix(b"\n").unwrap_or(hex)))
        })
    }

    #[cfg(feature = "read")]
    pub fn validate_shallow(&self, repository: &impl Repository) -> Result<()> {
        for id in self.shallow() {
            if common_commit(repository, id?)?.is_none() {
                return Err(Error::InvalidRequest);
            }
        }
        Ok(())
    }

    #[cfg(feature = "read")]
    pub fn include_tags(&self) -> bool {
        self.include_tags
    }

    pub fn acknowledge(&self, repository: &impl Repository, output: &mut impl Write) -> Result<()> {
        if self.v2 {
            return crate::v2::acknowledge(self.done, self.common(repository), output);
        }
        if self.extensions.options.depth.is_some() && self.haves.is_empty() {
            // The v0 shallow-list probe precedes any have/done round. A NAK
            // here would be consumed as the next replay's shallow-list header.
            return Ok(());
        }
        let mut last = None;
        for id in self.common(repository) {
            let id = id?;
            if self.capabilities.ack() == 0 {
                pktline::write_parts(output, &[b"ACK ", &id.to_hex(), b"\n"])?;
                return Ok(());
            }
            let status: &[u8] = if self.capabilities.ack() == 1 {
                b" continue\n"
            } else {
                b" common\n"
            };
            pktline::write_parts(output, &[b"ACK ", &id.to_hex(), status])?;
            last = Some(id);
        }
        if let Some(id) = last.filter(|_| self.done) {
            pktline::write_parts(output, &[b"ACK ", &id.to_hex(), b"\n"])
        } else {
            pktline::write(output, b"NAK\n")
        }
    }

    pub fn output<'w, W: Write>(&self, output: &'w mut W) -> PackOutput<'w, W> {
        PackOutput {
            output,
            band: self.band(),
            buffer: [0; 1024],
            length: 0,
        }
    }

    pub(crate) fn band(&self) -> u8 {
        // V2 always frames its packfile section on sideband channel 1.
        if self.v2 { 2 } else { self.capabilities.band() }
    }
}

#[cfg(feature = "read")]
fn shallow_change(object: &git_zero_core::ValidationMetadata) -> Option<(&'static [u8], ObjectId)> {
    if object.is_shallow() {
        Some((b"shallow ", object.id))
    } else if object.is_unshallow() {
        Some((b"unshallow ", object.id))
    } else {
        None
    }
}

// The request parser already checked packet framing and ID syntax. These
// helpers keep packet filtering separate from fallible repository metadata.
fn have_id(packet: Packet<'_>) -> Option<ObjectId> {
    let Packet::Data(line) = packet else {
        return None;
    };
    let hex = line.strip_prefix(b"have ")?;
    ObjectId::from_hex(hex.strip_suffix(b"\n").unwrap_or(hex)).ok()
}

pub(crate) fn common_commit(
    repository: &impl Repository,
    id: ObjectId,
) -> Result<Option<ObjectId>> {
    let Some(index) = repository.find_index(id) else {
        return Ok(None);
    };
    Ok((repository.object_info(index)?.kind == ObjectKind::Commit).then_some(id))
}

/// Complete-pack sink passed to host pack writers. It forwards raw pack bytes
/// or frames channel 1 according to the client's negotiated sideband mode.
/// Uses a fixed 1 KiB buffer; packet sizes stay within both protocol limits.
/// The server flushes the final packet and terminator after the writer succeeds.
pub struct PackOutput<'a, W: Write> {
    output: &'a mut W,
    band: u8,
    buffer: [u8; 1024],
    length: usize,
}

impl<W: Write> PackOutput<'_, W> {
    fn flush(&mut self) -> Result<()> {
        if self.length != 0 {
            pktline::write_parts(self.output, &[b"\x01", &self.buffer[..self.length]])?;
            self.length = 0;
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if self.band != 0 {
            self.flush()?;
            self.output.write_all(b"0000")?;
        }
        Ok(())
    }
}

impl<W: Write> Write for PackOutput<'_, W> {
    fn write_all(&mut self, mut bytes: &[u8]) -> Result<()> {
        if self.band == 0 {
            return self.output.write_all(bytes);
        }
        let capacity = if self.band == 1 {
            995
        } else {
            self.buffer.len()
        };
        while !bytes.is_empty() {
            let count = bytes.len().min(capacity - self.length);
            self.buffer[self.length..self.length + count].copy_from_slice(&bytes[..count]);
            self.length += count;
            bytes = &bytes[count..];
            if self.length == capacity {
                self.flush()?;
            }
        }
        Ok(())
    }
}

/// Write a terminal v0 ERR packet as a replacement for a failed buffered upload
/// response. Discard the failed response first. No backend details are exposed.
/// Hosts own the HTTP status; never append this to a partial raw pack.
pub fn write_upload_error(error: Error, output: &mut impl Write) -> Result<()> {
    let message: &[u8] = match error {
        Error::BufferTooSmall | Error::LimitExceeded => b"request exceeds resource limits",
        Error::Backend => b"repository unavailable",
        Error::Unsupported => b"unsupported upload request",
        _ => b"invalid upload request",
    };
    pktline::write_parts(output, &[b"ERR ", message, b"\n"])
}
