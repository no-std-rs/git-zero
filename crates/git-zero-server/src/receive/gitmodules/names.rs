//! Section/key properties needed by gitmodules fsck, including C-string edges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Field {
    Other,
    Url,
    Path,
    Update,
}
#[derive(Clone, Copy, Default)]
pub(super) struct Key {
    bytes: [u8; 6],
    used: u8,
}
impl Key {
    pub(super) fn push(&mut self, byte: u8) {
        if self.used < 6 {
            self.bytes[usize::from(self.used)] = byte;
        }
        self.used = self.used.saturating_add(1);
    }
    fn field(self) -> Field {
        match self.bytes.get(..usize::from(self.used)) {
            Some(b"url") => Field::Url,
            Some(b"path") => Field::Path,
            Some(b"update") => Field::Update,
            _ => Field::Other,
        }
    }
}
#[derive(Clone, Copy, Default)]
struct Name {
    seen: bool,
    component: u8,
    bad: bool,
}
impl Name {
    fn push(&mut self, byte: u8) {
        self.seen = true;
        if matches!(byte, b'/' | b'\\') {
            self.bad |= self.component == 2;
            self.component = 0;
        } else if byte == b'.' && self.component < 3 {
            self.component += 1;
        } else {
            self.component = 3;
        }
    }
    fn invalid(self) -> bool {
        self.bad || !self.seen || self.component == 2
    }
}
#[derive(Clone, Copy)]
pub(super) struct Section {
    prefix: u8,
    matched: bool,
    subsection: bool,
    name: Name,
    truncated: bool,
    has_dot: bool,
    before_dot: Name,
    derived: Key,
}
impl Section {
    pub(super) fn new() -> Self {
        Self {
            prefix: 0,
            matched: true,
            subsection: false,
            name: Name::default(),
            truncated: false,
            has_dot: false,
            before_dot: Name::default(),
            derived: Key::default(),
        }
    }
    pub(super) fn base(&mut self, byte: u8) {
        let byte = byte.to_ascii_lowercase();
        if self.prefix < 9 {
            self.matched &= byte == b"submodule"[usize::from(self.prefix)];
        } else if self.prefix == 9 {
            if byte == b'.' {
                self.subsection = true;
            } else {
                self.matched = false;
            }
        } else if self.matched && self.subsection {
            self.name_byte(byte);
        }
        self.prefix = self.prefix.saturating_add(1);
    }
    pub(super) fn extended(&mut self) {
        if self.prefix == 9 {
            self.subsection = true;
        } else if self.matched && self.subsection {
            self.name_byte(b'.');
        }
    }
    pub(super) fn name_byte(&mut self, byte: u8) {
        if self.truncated {
            return;
        }
        if byte == 0 {
            self.truncated = true;
            return;
        }
        if byte == b'.' {
            self.before_dot = self.name;
            self.has_dot = true;
            self.derived = Key::default();
        } else if self.has_dot {
            self.derived.push(byte);
        }
        self.name.push(byte);
    }
    pub(super) fn entry(self, key: Key) -> Option<(bool, Field)> {
        if !self.matched || self.prefix < 9 || !self.subsection {
            return None;
        }
        if self.truncated {
            // Native config callbacks receive a NUL-terminated variable name.
            // A hidden suffix can move its last-dot key boundary into the name.
            if self.has_dot {
                Some((self.before_dot.invalid(), self.derived.field()))
            } else {
                None
            }
        } else {
            Some((self.name.invalid(), key.field()))
        }
    }
}
