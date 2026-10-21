//! Streaming identity/header checks used by strict receive hosting.
use git_zero_core::{Error, ObjectKind, Result};

#[derive(Clone, Copy)]
enum Phase {
    Tree,
    Parents,
    Committer,
    CommitExtra,
    TagObject,
    TagType,
    TagName,
    Tagger,
    TagExtra,
    Message,
}

enum Line {
    Prefix,
    Parent,
    Skip,
    Identity(Identity),
}

/// Additional fsck header policy, independent of structural link decoding.
/// Feed exactly the canonical bytes accepted by the link decoder, then finish.
/// Identity names/emails can be arbitrarily long; this retains only ten prefix
/// bytes and a fixed-size identity state. Missing historical taggers are allowed.
/// This is not a complete fsck implementation; tree and designated-blob policy
/// must be checked separately by the host before publication.
pub struct FsckHeaders {
    phase: Phase,
    line: Line,
    prefix: [u8; 10],
    used: usize,
    nonempty: bool,
    commit: bool,
    failure: Option<Error>,
}

impl FsckHeaders {
    pub fn new(kind: ObjectKind) -> Result<Self> {
        let phase = match kind {
            ObjectKind::Commit => Phase::Tree,
            ObjectKind::Tag => Phase::TagObject,
            _ => return Err(Error::InvalidObject),
        };
        Ok(Self {
            phase,
            line: Line::Prefix,
            prefix: [0; 10],
            used: 0,
            nonempty: false,
            commit: kind == ObjectKind::Commit,
            failure: None,
        })
    }

    pub fn update(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = self.feed(bytes);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        for (index, &byte) in bytes.iter().enumerate() {
            if matches!(self.phase, Phase::Message) {
                return if self.commit && bytes[index..].contains(&0) {
                    Err(Error::InvalidObject)
                } else {
                    Ok(())
                };
            }
            if byte == 0 {
                return Err(Error::InvalidObject);
            }
            if byte == b'\n' {
                self.end_line()?;
            } else {
                self.nonempty = true;
                self.byte(byte)?;
            }
        }
        Ok(())
    }

    fn byte(&mut self, byte: u8) -> Result<()> {
        match &mut self.line {
            Line::Identity(identity) => return identity.byte(byte),
            Line::Skip | Line::Parent => return Ok(()),
            Line::Prefix => {}
        }
        let expected: &[u8] = match self.phase {
            Phase::Parents => b"author ",
            Phase::Committer => b"committer ",
            Phase::Tagger => b"tagger ",
            _ => {
                self.line = Line::Skip;
                return Ok(());
            }
        };
        self.prefix[self.used] = byte;
        self.used += 1;
        let prefix = &self.prefix[..self.used];
        if matches!(self.phase, Phase::Parents) && b"parent ".starts_with(prefix) {
            if self.used == 7 {
                self.line = Line::Parent;
            }
            return Ok(());
        }
        if expected.starts_with(prefix) {
            if self.used == expected.len() {
                self.line = Line::Identity(Identity::new());
            }
            return Ok(());
        }
        if !matches!(self.phase, Phase::Tagger) {
            return Err(Error::InvalidObject);
        }
        self.line = Line::Skip;
        Ok(())
    }

    fn end_line(&mut self) -> Result<()> {
        if let Line::Identity(identity) = &self.line {
            identity.finish()?;
        }
        self.phase = match self.phase {
            Phase::Tree => Phase::Parents,
            Phase::Parents => match self.line {
                Line::Parent => Phase::Parents,
                Line::Identity(_) => Phase::Committer,
                _ => return Err(Error::InvalidObject),
            },
            Phase::Committer if matches!(self.line, Line::Identity(_)) => Phase::CommitExtra,
            Phase::Committer => return Err(Error::InvalidObject),
            Phase::TagObject => Phase::TagType,
            Phase::TagType => Phase::TagName,
            Phase::TagName => Phase::Tagger,
            Phase::CommitExtra | Phase::Tagger | Phase::TagExtra if !self.nonempty => {
                Phase::Message
            }
            Phase::CommitExtra => Phase::CommitExtra,
            Phase::Tagger | Phase::TagExtra => Phase::TagExtra,
            Phase::Message => Phase::Message,
        };
        self.line = Line::Prefix;
        self.used = 0;
        self.nonempty = false;
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if matches!(self.phase, Phase::Message)
            || (!self.nonempty
                && matches!(
                    self.phase,
                    Phase::CommitExtra | Phase::Tagger | Phase::TagExtra
                ))
        {
            Ok(())
        } else {
            Err(Error::InvalidObject)
        }
    }
}

#[derive(Clone, Copy)]
enum IdentPhase {
    Name,
    Email,
    Space,
    DateStart,
    Date,
    Sign,
    Zone(u8),
    End,
}
struct Identity {
    phase: IdentPhase,
    previous: Option<u8>,
    date: u64,
    zero: bool,
}
impl Identity {
    fn new() -> Self {
        Self {
            phase: IdentPhase::Name,
            previous: None,
            date: 0,
            zero: false,
        }
    }
    fn byte(&mut self, byte: u8) -> Result<()> {
        self.phase = match self.phase {
            IdentPhase::Name if byte == b'<' && self.previous == Some(b' ') => IdentPhase::Email,
            IdentPhase::Name if byte != b'<' && byte != b'>' => IdentPhase::Name,
            IdentPhase::Email if byte == b'>' => IdentPhase::Space,
            IdentPhase::Email if byte != b'<' => IdentPhase::Email,
            IdentPhase::Space if byte == b' ' => IdentPhase::DateStart,
            IdentPhase::DateStart if byte == b' ' || byte == b'\t' => IdentPhase::DateStart,
            IdentPhase::DateStart if byte.is_ascii_digit() => {
                self.date = u64::from(byte - b'0');
                self.zero = byte == b'0';
                IdentPhase::Date
            }
            IdentPhase::Date if byte == b' ' => IdentPhase::Sign,
            IdentPhase::Date if byte.is_ascii_digit() && !self.zero => {
                self.date = self
                    .date
                    .checked_mul(10)
                    .and_then(|date| date.checked_add(u64::from(byte - b'0')))
                    .filter(|&date| date <= i64::MAX as u64)
                    .ok_or(Error::InvalidObject)?;
                IdentPhase::Date
            }
            IdentPhase::Sign if byte == b'+' || byte == b'-' => IdentPhase::Zone(0),
            IdentPhase::Zone(digits) if byte.is_ascii_digit() => {
                if digits == 3 {
                    IdentPhase::End
                } else {
                    IdentPhase::Zone(digits + 1)
                }
            }
            _ => return Err(Error::InvalidObject),
        };
        self.previous = Some(byte);
        Ok(())
    }
    fn finish(&self) -> Result<()> {
        if matches!(self.phase, IdentPhase::End) {
            Ok(())
        } else {
            Err(Error::InvalidObject)
        }
    }
}
