use super::Capabilities;
use git_zero_core::{
    Error, ObjectFormat, ObjectId, Result, pktline, repository::validate_ref_name_bytes,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Command<'a> {
    pub old: ObjectId,
    pub new: ObjectId,
    pub name: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_commands: u32,
    pub max_shallow: u32,
    pub max_options: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlEvent<'a> {
    NeedInput,
    Shallow(ObjectId),
    Command(Command<'a>),
    /// The command flush was read; negotiated push options follow next.
    Options,
    PushOption(&'a [u8]),
    /// The input suffix is untouched. Pass it to pack decoding when required.
    End {
        pack_required: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlProgress<'a> {
    pub consumed: usize,
    pub event: ControlEvent<'a>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Commands,
    Options,
    End,
}

/// Incremental receive-pack control decoding with one caller-owned packet
/// buffer. Persist each borrowed command/option before calling `read` again.
/// No pack bytes are consumed. Duplicate refs, old-value CAS, fast-forwards,
/// authorization and shallow-update policy are host validation responsibilities.
pub struct ControlDecoder<'a> {
    frame: Frame<'a>,
    state: ControlState,
    failure: Option<Error>,
}

struct ControlState {
    format: ObjectFormat,
    offered: Capabilities,
    selected: Capabilities,
    limits: Limits,
    phase: Phase,
    commands: u32,
    shallow: u32,
    options: u32,
    pack_required: bool,
}

impl<'a> ControlDecoder<'a> {
    pub fn new(
        storage: &'a mut [u8],
        format: ObjectFormat,
        offered: Capabilities,
        limits: Limits,
    ) -> Result<Self> {
        if storage.len() < 4 {
            return Err(Error::BufferTooSmall);
        }
        Ok(Self {
            frame: Frame {
                storage,
                filled: 0,
                target: 4,
                ready: false,
                total: 0,
                limit: limits.max_bytes,
            },
            state: ControlState {
                format,
                offered,
                selected: Capabilities::default(),
                limits,
                phase: Phase::Commands,
                commands: 0,
                shallow: 0,
                options: 0,
                pack_required: false,
            },
            failure: None,
        })
    }

    pub fn capabilities(&self) -> Capabilities {
        self.state.selected
    }

    /// EOF before the last control flush is a terminal error. `End` refers only
    /// to the control section; the host must still require and validate the pack
    /// or require transport EOF for a deletion-only/no-op request.
    pub fn read(&mut self, input: &[u8], end_of_input: bool) -> Result<ControlProgress<'_>> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.state.phase == Phase::End {
            return Ok(ControlProgress {
                consumed: 0,
                event: ControlEvent::End {
                    pack_required: self.state.pack_required,
                },
            });
        }
        let frame = match self.frame.read(input) {
            Ok(frame) => frame,
            Err(error) => {
                self.failure = Some(error);
                return Err(error);
            }
        };
        let event = match frame.event {
            FrameEvent::NeedInput if end_of_input => Err(Error::InvalidPacket),
            event => self.state.accept(event),
        };
        match event {
            Ok(event) => Ok(ControlProgress {
                consumed: frame.consumed,
                event,
            }),
            Err(error) => {
                self.failure = Some(error);
                Err(error)
            }
        }
    }
}

impl ControlState {
    fn accept<'a>(&mut self, event: FrameEvent<'a>) -> Result<ControlEvent<'a>> {
        match event {
            FrameEvent::NeedInput => Ok(ControlEvent::NeedInput),
            FrameEvent::Flush => Ok(self.flush()),
            FrameEvent::Data(bytes) if self.phase == Phase::Options => self.option(bytes),
            FrameEvent::Data(bytes) => {
                let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
                if let Some(hex) = bytes.strip_prefix(b"shallow ") {
                    self.boundary(hex)
                } else {
                    self.command(bytes)
                }
            }
        }
    }

    fn flush(&mut self) -> ControlEvent<'static> {
        if self.phase == Phase::Commands && self.selected.contains(Capabilities::PUSH_OPTIONS) {
            self.phase = Phase::Options;
            ControlEvent::Options
        } else {
            self.phase = Phase::End;
            ControlEvent::End {
                pack_required: self.pack_required,
            }
        }
    }

    fn option<'a>(&mut self, bytes: &'a [u8]) -> Result<ControlEvent<'a>> {
        if self.options == self.limits.max_options {
            return Err(Error::LimitExceeded);
        }
        if bytes.iter().any(|byte| matches!(byte, 0 | b'\n' | b'\r')) {
            return Err(Error::InvalidRequest);
        }
        self.options += 1;
        Ok(ControlEvent::PushOption(bytes))
    }

    fn boundary(&mut self, hex: &[u8]) -> Result<ControlEvent<'static>> {
        if self.commands != 0 {
            return Err(Error::InvalidRequest);
        }
        if self.shallow == self.limits.max_shallow {
            return Err(Error::LimitExceeded);
        }
        let id = object_id(hex, self.format)?;
        if id.is_zero() {
            return Err(Error::InvalidRequest);
        }
        self.shallow += 1;
        Ok(ControlEvent::Shallow(id))
    }

    fn command<'a>(&mut self, bytes: &'a [u8]) -> Result<ControlEvent<'a>> {
        if self.commands == self.limits.max_commands {
            return Err(Error::LimitExceeded);
        }
        let (command, capabilities) =
            command(bytes, self.format, self.commands == 0, self.offered)?;
        if command.new.is_zero() && !self.offered.contains(Capabilities::DELETE_REFS) {
            return Err(Error::Unsupported);
        }
        if self.commands == 0 {
            self.selected = capabilities;
        }
        self.pack_required |= !command.new.is_zero();
        self.commands += 1;
        Ok(ControlEvent::Command(command))
    }
}

fn object_id(bytes: &[u8], format: ObjectFormat) -> Result<ObjectId> {
    let id = ObjectId::from_hex(bytes).map_err(|_| Error::InvalidRequest)?;
    if id.format() != format {
        return Err(Error::InvalidRequest);
    }
    Ok(id)
}

fn command(
    bytes: &[u8],
    format: ObjectFormat,
    first: bool,
    offered: Capabilities,
) -> Result<(Command<'_>, Capabilities)> {
    let (line, selected) = match bytes.iter().position(|&byte| byte == 0) {
        Some(at) if first => (
            &bytes[..at],
            Capabilities::parse(&bytes[at + 1..], format, offered)?,
        ),
        Some(_) => return Err(Error::InvalidRequest),
        None if first && format == ObjectFormat::Sha256 => return Err(Error::InvalidRequest),
        None => (bytes, Capabilities::default()),
    };
    let width = format.hex_length();
    if line.get(width) != Some(&b' ') || line.get(width * 2 + 1) != Some(&b' ') {
        return Err(Error::InvalidRequest);
    }
    let old = object_id(&line[..width], format)?;
    let new = object_id(&line[width + 1..width * 2 + 1], format)?;
    let name = &line[width * 2 + 2..];
    validate_ref_name_bytes(name)?;
    if !new.is_zero() {
        super::advertisement::validate_first_name(format, offered, name)?;
    }
    Ok((Command { old, new, name }, selected))
}

enum FrameEvent<'a> {
    NeedInput,
    Flush,
    Data(&'a [u8]),
}
struct FrameProgress<'a> {
    consumed: usize,
    event: FrameEvent<'a>,
}
struct Frame<'a> {
    storage: &'a mut [u8],
    filled: usize,
    target: usize,
    ready: bool,
    total: u64,
    limit: u64,
}

impl Frame<'_> {
    fn read(&mut self, input: &[u8]) -> Result<FrameProgress<'_>> {
        if self.ready {
            self.filled = 0;
            self.target = 4;
            self.ready = false;
        }
        let mut consumed = 0;
        while self.filled < self.target {
            let count = (self.target - self.filled).min(input.len() - consumed);
            if count == 0 {
                return Ok(FrameProgress {
                    consumed,
                    event: FrameEvent::NeedInput,
                });
            }
            let total = self
                .total
                .checked_add(count as u64)
                .filter(|&n| n <= self.limit)
                .ok_or(Error::LimitExceeded)?;
            self.storage[self.filled..self.filled + count]
                .copy_from_slice(&input[consumed..consumed + count]);
            self.filled += count;
            consumed += count;
            self.total = total;
            if self.filled == 4 && self.target == 4 {
                let length = pktline::length(&self.storage[..4])?;
                if length == 0 {
                    self.ready = true;
                    return Ok(FrameProgress {
                        consumed,
                        event: FrameEvent::Flush,
                    });
                }
                if length > self.storage.len() {
                    return Err(Error::BufferTooSmall);
                }
                self.target = length;
            }
        }
        self.ready = true;
        Ok(FrameProgress {
            consumed,
            event: FrameEvent::Data(&self.storage[4..self.target]),
        })
    }
}
