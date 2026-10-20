//! Stream structural links into host-owned indexes without retaining a graph.
use crate::{Error, ObjectFormat, ObjectId, ObjectInfo, ObjectKind, Result, names::cmp_tree_names};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkLimits {
    pub max_body_bytes: u64,
    pub max_links: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkRelation {
    RootTree,
    Parent,
    TagTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkStatus<'name> {
    NeedInput,
    Link {
        id: ObjectId,
        kind: ObjectKind,
        relation: LinkRelation,
    },
    TreeEntry {
        id: ObjectId,
        mode: u32,
        name: &'name [u8],
        kind: Option<ObjectKind>,
        canonical_mode: bool,
    },
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkProgress<'name> {
    pub consumed: usize,
    pub status: LinkStatus<'name>,
}

#[derive(Clone, Copy)]
enum Phase {
    Blob,
    CommitTree,
    Parents,
    Headers,
    TagObject,
    TagType,
    TagName,
    TagHeaders,
    Message,
    Mode,
    Name,
    Id,
    Done,
}

struct Names<'a> {
    previous: &'a mut [u8],
    current: &'a mut [u8],
}
struct State {
    format: ObjectFormat,
    info: ObjectInfo,
    limits: LinkLimits,
    phase: Phase,
    position: u64,
    links: u64,
    header: [u8; 80],
    header_len: usize,
    header_overflow: bool,
    target: ObjectId,
    mode: u32,
    mode_digits: u32,
    padded_mode: bool,
    previous_mode: u32,
    previous_len: usize,
    current_len: usize,
    has_previous: bool,
    id: [u8; 32],
    id_len: usize,
    failure: Option<Error>,
}

/// Scan one canonical object body across arbitrary chunks. Blobs/messages are
/// skipped without retaining them; commit parents and tag targets are emitted
/// with their expected kinds. Tree names borrow caller scratch until resumption.
/// Scratch is divided equally into current/previous names; other kinds need none.
///
/// This scans syntax and adjacent tree order, not complete graph validity.
/// Authenticate the body separately with `ObjectDigest`. The host MUST check
/// target existence/kinds and tree-name uniqueness (including file/directory
/// aliases separated in Git order), persist parent edges and enforce ref policy.
/// Gitlinks name external repositories and emit `kind: None`.
///
/// Historical noncanonical tree modes with recognized type bits, including
/// zero padding and old regular-file permissions, are preserved and flagged.
/// They are not silently rewritten or rejected as graph errors.
pub struct LinkDecoder<'scratch> {
    state: State,
    names: Names<'scratch>,
}

impl<'scratch> LinkDecoder<'scratch> {
    pub fn new(
        info: ObjectInfo,
        format: ObjectFormat,
        names: &'scratch mut [u8],
        limits: LinkLimits,
    ) -> Result<Self> {
        if info.size > limits.max_body_bytes {
            return Err(Error::LimitExceeded);
        }
        let midpoint = names.len() / 2;
        let (previous, current) = names[..midpoint * 2].split_at_mut(midpoint);
        let phase = match info.kind {
            ObjectKind::Blob => Phase::Blob,
            ObjectKind::Tree => Phase::Mode,
            ObjectKind::Commit => Phase::CommitTree,
            ObjectKind::Tag => Phase::TagObject,
        };
        Ok(Self {
            state: State {
                format,
                info,
                limits,
                phase,
                position: 0,
                links: 0,
                header: [0; 80],
                header_len: 0,
                header_overflow: false,
                target: format.zero_id(),
                mode: 0,
                mode_digits: 0,
                padded_mode: false,
                previous_mode: 0,
                previous_len: 0,
                current_len: 0,
                has_previous: false,
                id: [0; 32],
                id_len: 0,
                failure: None,
            },
            names: Names { previous, current },
        })
    }

    pub fn position(&self) -> u64 {
        self.state.position
    }

    /// Events may consume a prefix. Resubmit the suffix with the same EOF flag.
    /// Every error is terminal; all emitted metadata remains provisional.
    pub fn update<'name>(
        &'name mut self,
        input: &[u8],
        end_of_input: bool,
    ) -> Result<LinkProgress<'name>> {
        let result = self.state.advance(input, end_of_input, &mut self.names);
        if let Err(error) = result {
            self.state.failure = Some(error);
        }
        result
    }

    pub fn finish(&self) -> Result<()> {
        if let Some(error) = self.state.failure {
            return Err(error);
        }
        if matches!(self.state.phase, Phase::Done) {
            Ok(())
        } else {
            Err(Error::InvalidObject)
        }
    }
}

impl State {
    fn advance<'name>(
        &mut self,
        input: &[u8],
        eof: bool,
        names: &'name mut Names<'_>,
    ) -> Result<LinkProgress<'name>> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let mut consumed = 0;
        loop {
            if self.position == self.info.size {
                if consumed != input.len() {
                    return Err(Error::InvalidObject);
                }
                let status = self.end(eof)?;
                return Ok(LinkProgress { consumed, status });
            }
            if consumed == input.len() {
                if eof {
                    return Err(Error::InvalidObject);
                }
                return Ok(LinkProgress {
                    consumed,
                    status: LinkStatus::NeedInput,
                });
            }
            if matches!(self.phase, Phase::Blob | Phase::Message) {
                let count =
                    (self.info.size - self.position).min((input.len() - consumed) as u64) as usize;
                self.position += count as u64;
                consumed += count;
                continue;
            }
            let byte = input[consumed];
            consumed += 1;
            self.position += 1;
            match self.phase {
                Phase::Mode => self.mode_byte(byte)?,
                Phase::Name => {
                    if byte == 0 {
                        self.phase = Phase::Id;
                        self.id_len = 0;
                    } else {
                        *names
                            .current
                            .get_mut(self.current_len)
                            .ok_or(Error::BufferTooSmall)? = byte;
                        self.current_len += 1;
                    }
                }
                Phase::Id => {
                    self.id[self.id_len] = byte;
                    self.id_len += 1;
                    if self.id_len == self.format.id_length() {
                        let id = ObjectId::from_bytes(self.format, &self.id[..self.id_len])?;
                        if id.is_zero() {
                            return Err(Error::InvalidObject);
                        }
                        self.tree_entry(names)?;
                        return Ok(LinkProgress {
                            consumed,
                            status: LinkStatus::TreeEntry {
                                id,
                                mode: self.previous_mode,
                                name: &names.previous[..self.previous_len],
                                kind: tree_kind(self.previous_mode)?,
                                canonical_mode: !self.padded_mode
                                    && canonical_mode(self.previous_mode),
                            },
                        });
                    }
                }
                _ => {
                    if byte == b'\n' {
                        let status = self.line()?;
                        self.header_len = 0;
                        self.header_overflow = false;
                        if let Some(status) = status {
                            self.charge()?;
                            return Ok(LinkProgress { consumed, status });
                        }
                    } else {
                        if byte == 0 {
                            return Err(Error::InvalidObject);
                        }
                        if self.header_len == self.header.len() {
                            self.header_overflow = true;
                        } else {
                            self.header[self.header_len] = byte;
                            self.header_len += 1;
                        }
                    }
                }
            }
        }
    }

    fn end(&mut self, eof: bool) -> Result<LinkStatus<'static>> {
        if matches!(self.phase, Phase::Done) {
            return Ok(LinkStatus::Finished);
        }
        if !matches!(self.phase, Phase::Blob | Phase::Message)
            && !(matches!(self.phase, Phase::Mode) && self.mode_digits == 0)
        {
            return Err(Error::InvalidObject);
        }
        if !eof {
            return Ok(LinkStatus::NeedInput);
        }
        self.phase = Phase::Done;
        Ok(LinkStatus::Finished)
    }

    fn charge(&mut self) -> Result<()> {
        if self.links == self.limits.max_links {
            return Err(Error::LimitExceeded);
        }
        self.links += 1;
        Ok(())
    }

    fn mode_byte(&mut self, byte: u8) -> Result<()> {
        match byte {
            b'0'..=b'7' => {
                if self.mode_digits == 0 {
                    self.padded_mode = byte == b'0';
                }
                self.mode = self
                    .mode
                    .checked_mul(8)
                    .and_then(|mode| mode.checked_add(u32::from(byte - b'0')))
                    .ok_or(Error::InvalidObject)?;
                self.mode_digits = self.mode_digits.saturating_add(1);
            }
            b' ' if self.mode_digits != 0 => {
                tree_kind(self.mode)?;
                self.phase = Phase::Name;
            }
            _ => return Err(Error::InvalidObject),
        }
        Ok(())
    }

    fn tree_entry(&mut self, names: &mut Names<'_>) -> Result<()> {
        let name = &names.current[..self.current_len];
        if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
            return Err(Error::InvalidFileName);
        }
        if self.has_previous
            && cmp_tree_names(
                &names.previous[..self.previous_len],
                self.previous_mode & 0o170000 == 0o040000,
                name,
                self.mode & 0o170000 == 0o040000,
            ) != core::cmp::Ordering::Less
        {
            return Err(Error::UnsortedFiles);
        }
        self.charge()?;
        core::mem::swap(&mut names.previous, &mut names.current);
        self.previous_len = self.current_len;
        self.previous_mode = self.mode;
        self.has_previous = true;
        self.current_len = 0;
        self.mode = 0;
        self.mode_digits = 0;
        self.phase = Phase::Mode;
        Ok(())
    }

    fn line(&mut self) -> Result<Option<LinkStatus<'static>>> {
        let line = &self.header[..self.header_len];
        let edge = match self.phase {
            Phase::CommitTree => {
                let id = link(line, b"tree ", self.format, self.header_overflow)?;
                self.phase = Phase::Parents;
                Some(LinkStatus::Link {
                    id,
                    kind: ObjectKind::Tree,
                    relation: LinkRelation::RootTree,
                })
            }
            Phase::Parents if line.starts_with(b"parent ") => Some(LinkStatus::Link {
                id: link(line, b"parent ", self.format, self.header_overflow)?,
                kind: ObjectKind::Commit,
                relation: LinkRelation::Parent,
            }),
            Phase::TagObject => {
                self.target = link(line, b"object ", self.format, self.header_overflow)?;
                self.phase = Phase::TagType;
                None
            }
            Phase::TagType => {
                let kind = match line {
                    b"type commit" => ObjectKind::Commit,
                    b"type tree" => ObjectKind::Tree,
                    b"type blob" => ObjectKind::Blob,
                    b"type tag" => ObjectKind::Tag,
                    _ => return Err(Error::InvalidObject),
                };
                if self.header_overflow {
                    return Err(Error::InvalidObject);
                }
                self.phase = Phase::TagName;
                Some(LinkStatus::Link {
                    id: self.target,
                    kind,
                    relation: LinkRelation::TagTarget,
                })
            }
            Phase::TagName => {
                if !line.starts_with(b"tag ") || line.len() == 4 {
                    return Err(Error::InvalidObject);
                }
                self.phase = Phase::TagHeaders;
                None
            }
            _ => {
                self.phase = if line.is_empty() {
                    Phase::Message
                } else if matches!(self.phase, Phase::TagHeaders) {
                    Phase::TagHeaders
                } else {
                    Phase::Headers
                };
                None
            }
        };
        Ok(edge)
    }
}

fn link(line: &[u8], prefix: &[u8], format: ObjectFormat, overflow: bool) -> Result<ObjectId> {
    if overflow {
        return Err(Error::InvalidObject);
    }
    let hex = line
        .strip_prefix(prefix)
        .filter(|hex| hex.len() == format.hex_length())
        .ok_or(Error::InvalidObject)?;
    ObjectId::from_hex(hex).map_err(|_| Error::InvalidObject)
}

fn tree_kind(mode: u32) -> Result<Option<ObjectKind>> {
    Ok(match mode & 0o170000 {
        0o040000 => Some(ObjectKind::Tree),
        0o100000 | 0o120000 => Some(ObjectKind::Blob),
        0o160000 => None,
        _ => return Err(Error::InvalidObject),
    })
}

fn canonical_mode(mode: u32) -> bool {
    matches!(mode, 0o040000 | 0o100644 | 0o100755 | 0o120000 | 0o160000)
}
