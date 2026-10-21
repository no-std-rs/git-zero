//! Allocation-free, streamed config policy for a designated gitmodules blob.
use git_zero_core::{Error, Result};
mod names;
mod url;
use names::{Field, Key, Section};
use url::Url;

#[derive(Clone, Copy)]
enum Phase {
    Main,
    Comment,
    Base,
    SectionSpace,
    Name,
    NameEscape,
    SectionEnd,
    Key,
    KeySpace,
    Value,
    Halted,
}
enum ValueStep {
    More,
    Complete,
    Malformed,
}

/// Validate the settings that strict receive fsck checks in gitmodules.
/// Config syntax errors are warning-only and stop parsing subsequent entries;
/// an unsafe setting parsed before that error remains fatal. Names, values and
/// URL paths are reduced to bounded properties rather than retained strings.
pub struct FsckModules {
    remaining: u64,
    normalized: u64,
    pending_cr: bool,
    eof_seen: bool,
    limited: bool,
    phase: Phase,
    base_seen: bool,
    section: Section,
    key: Key,
    value: Value,
    done: bool,
    failure: Option<Error>,
}
impl FsckModules {
    pub fn new(size: u64) -> Self {
        Self {
            remaining: size,
            normalized: 0,
            pending_cr: false,
            eof_seen: false,
            limited: false,
            phase: Phase::Main,
            base_seen: false,
            section: Section::new(),
            key: Key::default(),
            value: Value::new(None),
            done: false,
            failure: None,
        }
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
        self.remaining = self
            .remaining
            .checked_sub(bytes.len() as u64)
            .ok_or(Error::InvalidObject)?;
        for &byte in bytes {
            if self.limited || matches!(self.phase, Phase::Halted) {
                break;
            }
            if self.pending_cr {
                self.pending_cr = false;
                if byte == b'\n' {
                    self.character(b'\n')?;
                    continue;
                }
                self.character(b'\r')?;
                if byte == 0xff {
                    continue;
                }
            }
            if byte == b'\r' {
                self.pending_cr = true;
            } else {
                self.character(byte)?;
            }
        }
        Ok(())
    }
    fn character(&mut self, byte: u8) -> Result<()> {
        if self.limited {
            return Ok(());
        }
        // Match Git's memory-backed config source: on the reference targets
        // 0xff is returned as EOF, and non-ASCII leading bytes (including a
        // UTF-8 BOM) are syntax warnings rather than file-style BOM handling.
        if byte == 0xff {
            self.eof_seen = true;
            return self.consume(b'\n');
        }
        self.normalized += 1;
        // Native config supplies NUL after INT_MAX until EOF. An unquoted
        // value still invokes its callback with the visible prefix; discarding
        // it here could suppress an unsafe setting parsed before the limit.
        if self.normalized > i32::MAX as u64 {
            self.limited = true;
            self.eof_seen = true;
            if matches!(self.phase, Phase::Value) {
                self.consume(0)?;
            } else {
                self.phase = Phase::Halted;
            }
            return Ok(());
        }
        self.consume(byte)
    }
    fn boolean(&mut self) -> Result<()> {
        if self.section.entry(self.key).is_some_and(|(bad, _)| bad) {
            return Err(Error::InvalidObject);
        }
        self.phase = Phase::Main;
        Ok(())
    }
    fn start_value(&mut self) {
        self.value = Value::new(self.section.entry(self.key));
        self.phase = Phase::Value;
    }
    fn consume(&mut self, byte: u8) -> Result<()> {
        match self.phase {
            Phase::Main => self.main(byte),
            Phase::Comment => {
                if byte == b'\n' {
                    self.phase = Phase::Main;
                }
            }
            Phase::Base => self.base(byte),
            Phase::SectionSpace => self.section_space(byte),
            Phase::Name => match byte {
                b'\n' => self.phase = Phase::Halted,
                b'\\' => self.phase = Phase::NameEscape,
                b'"' => self.phase = Phase::SectionEnd,
                _ => self.section.name_byte(byte),
            },
            Phase::NameEscape => {
                if byte == b'\n' {
                    self.phase = Phase::Halted;
                } else {
                    self.section.name_byte(byte);
                    self.phase = Phase::Name;
                }
            }
            Phase::SectionEnd => {
                self.phase = if byte == b']' {
                    Phase::Main
                } else {
                    Phase::Halted
                }
            }
            Phase::Key | Phase::KeySpace => self.key(byte)?,
            Phase::Value => self.value(byte)?,
            Phase::Halted => {}
        }
        Ok(())
    }
    fn main(&mut self, byte: u8) {
        match byte {
            b'\n' if self.eof_seen => self.phase = Phase::Halted,
            b'#' | b';' => self.phase = Phase::Comment,
            b'[' => {
                self.section = Section::new();
                self.base_seen = false;
                self.phase = Phase::Base;
            }
            _ if space(byte) => {}
            _ if byte.is_ascii_alphabetic() => {
                self.key = Key::default();
                self.key.push(byte.to_ascii_lowercase());
                self.phase = Phase::Key;
            }
            _ => self.phase = Phase::Halted,
        }
    }
    fn base(&mut self, byte: u8) {
        if self.eof_seen {
            self.phase = Phase::Halted;
            return;
        }
        match byte {
            b']' if self.base_seen => self.phase = Phase::Main,
            b' ' | b'\t' | b'\r' => self.phase = Phase::SectionSpace,
            _ if key_char(byte) || byte == b'.' => {
                self.base_seen = true;
                self.section.base(byte);
            }
            _ => self.phase = Phase::Halted,
        }
    }
    fn section_space(&mut self, byte: u8) {
        match byte {
            b' ' | b'\t' | b'\r' => {}
            b'"' if self.base_seen => {
                self.section.extended();
                self.phase = Phase::Name;
            }
            _ => self.phase = Phase::Halted,
        }
    }
    fn key(&mut self, byte: u8) -> Result<()> {
        if matches!(self.phase, Phase::Key) && key_char(byte) && !self.eof_seen {
            self.key.push(byte.to_ascii_lowercase());
            return Ok(());
        }
        match byte {
            b' ' | b'\t' => self.phase = Phase::KeySpace,
            b'=' => self.start_value(),
            b'\n' => self.boolean()?,
            _ => self.phase = Phase::Halted,
        }
        Ok(())
    }
    fn value(&mut self, byte: u8) -> Result<()> {
        match self.value.push(byte) {
            ValueStep::More => {}
            ValueStep::Malformed => self.phase = Phase::Halted,
            ValueStep::Complete => {
                if self.value.invalid() {
                    return Err(Error::InvalidObject);
                }
                self.phase = Phase::Main;
            }
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.done {
            return Ok(());
        }
        let result = self.end();
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
    fn end(&mut self) -> Result<()> {
        if self.remaining != 0 {
            return Err(Error::InvalidObject);
        }
        if self.pending_cr {
            self.pending_cr = false;
            self.character(b'\r')?;
        }
        // Native config EOF supplies a newline; a final backslash can consume
        // it as a continuation and require the next EOF newline to finish.
        self.consume(b'\n')?;
        self.consume(b'\n')?;
        self.done = true;
        Ok(())
    }
}
fn key_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}
fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

struct Value {
    field: Field,
    bad_name: bool,
    bad: bool,
    actual: bool,
    pending_space: bool,
    quote: bool,
    escape: bool,
    comment: bool,
    url: Url,
}
impl Value {
    fn new(entry: Option<(bool, Field)>) -> Self {
        let (bad_name, field) = entry.unwrap_or((false, Field::Other));
        Self {
            field,
            bad_name,
            bad: false,
            actual: false,
            pending_space: false,
            quote: false,
            escape: false,
            comment: false,
            url: Url::new(),
        }
    }
    fn emit(&mut self, byte: u8) {
        self.actual = true;
        if self.field == Field::Other {
            return;
        }
        if byte == 0 {
            self.bad |= self.field == Field::Url && self.url.invalid();
            self.field = Field::Other;
            return;
        }
        match self.field {
            Field::Path => {
                self.bad |= byte == b'-';
                self.field = Field::Other;
            }
            Field::Update => {
                self.bad |= byte == b'!';
                self.field = Field::Other;
            }
            Field::Url => {
                self.url.push(byte);
                if let Url::Other(bad) = self.url {
                    self.bad |= bad;
                    self.field = Field::Other;
                }
            }
            Field::Other => {}
        }
    }
    fn flush_space(&mut self) {
        // For these policies all non-newline whitespace has the same effect:
        // it breaks URL tokens/hostnames, or makes a non-dot path segment.
        if self.pending_space && self.actual {
            self.emit(b' ');
        }
        self.pending_space = false;
    }
    fn push(&mut self, byte: u8) -> ValueStep {
        if self.comment {
            return if byte == b'\n' {
                ValueStep::Complete
            } else {
                ValueStep::More
            };
        }
        if self.escape {
            self.escape = false;
            let decoded = match byte {
                b'\n' => return ValueStep::More,
                b't' => b'\t',
                b'b' => 8,
                b'n' => b'\n',
                b'\\' | b'"' => byte,
                _ => return ValueStep::Malformed,
            };
            self.emit(decoded);
            return ValueStep::More;
        }
        if byte == b'\n' {
            return if self.quote {
                ValueStep::Malformed
            } else {
                ValueStep::Complete
            };
        }
        if !self.quote && space(byte) {
            self.pending_space = true;
            return ValueStep::More;
        }
        if !self.quote && matches!(byte, b'#' | b';') {
            self.comment = true;
            return ValueStep::More;
        }
        self.flush_space();
        match byte {
            b'\\' => self.escape = true,
            b'"' => self.quote = !self.quote,
            _ => self.emit(byte),
        }
        ValueStep::More
    }
    fn invalid(&self) -> bool {
        self.bad_name || self.bad || (self.field == Field::Url && self.url.invalid())
    }
}
