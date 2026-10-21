//! Bounded submodule URL policy; no normalized URL string is retained.
#[derive(Clone, Copy)]
enum Mode {
    Prefix,
    Other,
    Relative,
    Curl,
}
#[derive(Clone, Copy)]
pub(super) struct Url {
    mode: Mode,
    head: [u8; 8],
    used: usize,
    bad: bool,
    relative: Relative,
    curl: Curl,
}
impl Url {
    pub(super) fn new() -> Self {
        Self {
            mode: Mode::Prefix,
            head: [0; 8],
            used: 0,
            bad: false,
            relative: Relative::new(false, false),
            curl: Curl::new(false),
        }
    }
    pub(super) fn push(&mut self, byte: u8) {
        match self.mode {
            Mode::Other => {}
            Mode::Relative => self.relative.push(byte),
            Mode::Curl => self.curl.push(byte),
            Mode::Prefix => {
                if self.used == 0 && byte == b'-' {
                    self.bad = true;
                    self.mode = Mode::Other;
                    return;
                }
                self.head[self.used] = byte;
                self.used += 1;
                let prefix = &self.head[..self.used];
                let choices: [(&[u8], u8); 11] = [
                    (b"./", 1),
                    (b"../", 2),
                    (b"git://", 3),
                    (b"http://", 4),
                    (b"https://", 4),
                    (b"ftp://", 4),
                    (b"ftps://", 4),
                    (b"http::", 5),
                    (b"https::", 5),
                    (b"ftp::", 5),
                    (b"ftps::", 5),
                ];
                let mut possible = false;
                for (needle, kind) in choices {
                    if !needle.starts_with(prefix) {
                        continue;
                    }
                    possible = true;
                    if prefix.len() != needle.len() {
                        continue;
                    }
                    if kind <= 3 {
                        self.relative = Relative::new(kind == 2, kind == 3);
                        self.mode = Mode::Relative;
                    } else {
                        self.curl = Curl::new(kind == 4);
                        self.mode = Mode::Curl;
                    }
                    return;
                }
                if !possible {
                    self.mode = Mode::Other;
                }
            }
        }
    }
    pub(super) fn invalid(&self) -> bool {
        self.bad
            || match self.mode {
                Mode::Prefix | Mode::Other => false,
                Mode::Relative => self.relative.invalid(),
                Mode::Curl => self.curl.invalid(),
            }
    }
}

#[derive(Clone, Copy, Default)]
struct Percent {
    phase: u8,
    high: u8,
    bad: bool,
}
impl Percent {
    fn push(&mut self, byte: u8) -> Option<(u8, bool)> {
        match self.phase {
            0 => {
                if byte == b'%' {
                    self.phase = 1;
                    None
                } else {
                    Some((byte, false))
                }
            }
            1 => match hex(byte) {
                Some(value) => {
                    self.high = value;
                    self.phase = 2;
                    None
                }
                None => {
                    self.bad = true;
                    self.phase = u8::from(byte == b'%');
                    Some((byte, false))
                }
            },
            _ => match hex(byte) {
                Some(value) => {
                    self.phase = 0;
                    Some((self.high * 16 + value, true))
                }
                None => {
                    self.bad = true;
                    self.phase = u8::from(byte == b'%');
                    Some((byte, false))
                }
            },
        }
    }
}
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Relative {
    leading: bool,
    up: bool,
    dots: u8,
    colon: bool,
    raw_lf: bool,
    escaped_lf: bool,
    bad: bool,
    percent: Percent,
}
impl Relative {
    fn new(up: bool, git: bool) -> Self {
        Self {
            leading: !git,
            up,
            dots: 0,
            colon: git,
            raw_lf: false,
            escaped_lf: false,
            bad: false,
            percent: Percent::default(),
        }
    }
    fn push(&mut self, byte: u8) {
        self.raw_lf |= byte == b'\n';
        // url_decode() treats everything before the first colon as protocol,
        // even for relative URLs. Escaped newlines there remain undecoded.
        if byte == b':' && !self.colon {
            self.colon = true;
            self.escaped_lf = false;
            self.percent = Percent::default();
        }
        if let Some((b, true)) = self.percent.push(byte) {
            self.escaped_lf |= b == b'\n';
        }
        if !self.leading {
            return;
        }
        match (self.dots, byte) {
            (0, b'.') => self.dots = 1,
            (1, b'.') => self.dots = 2,
            (1, b'/') => self.dots = 0,
            (2, b'/') => {
                self.up = true;
                self.dots = 0;
            }
            (0, _) => {
                self.bad |= self.up && matches!(byte, b':' | b'/');
                self.leading = false;
            }
            _ => self.leading = false,
        }
    }
    fn invalid(&self) -> bool {
        self.bad || self.raw_lf || self.escaped_lf
    }
}

#[derive(Clone, Copy)]
enum CurlPhase {
    Scheme,
    Slash1,
    Slash2,
    Authority,
    Path,
    Query,
}
#[derive(Clone, Copy)]
struct Curl {
    phase: CurlPhase,
    scheme: [u8; 4],
    scheme_len: usize,
    scheme_ok: bool,
    file: bool,
    host: Host,
    at: bool,
    percent: Percent,
    path: Path,
    permanent_lf: bool,
    bad: bool,
}
impl Curl {
    fn new(direct: bool) -> Self {
        Self {
            phase: if direct {
                CurlPhase::Authority
            } else {
                CurlPhase::Scheme
            },
            scheme: [0; 4],
            scheme_len: 0,
            scheme_ok: true,
            file: false,
            host: Host::default(),
            at: false,
            percent: Percent::default(),
            path: Path::default(),
            permanent_lf: false,
            bad: false,
        }
    }
    fn push(&mut self, byte: u8) {
        match self.phase {
            CurlPhase::Scheme => {
                if byte == b':' {
                    self.bad |= self.scheme_len == 0 || !self.scheme_ok;
                    self.file = self.scheme_len == 4 && self.scheme.eq_ignore_ascii_case(b"file");
                    self.phase = CurlPhase::Slash1;
                } else {
                    self.scheme_ok &= if self.scheme_len == 0 {
                        byte.is_ascii_alphabetic()
                    } else {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')
                    };
                    if self.scheme_len < 4 {
                        self.scheme[self.scheme_len] = byte;
                    }
                    self.scheme_len = self.scheme_len.saturating_add(1);
                }
            }
            CurlPhase::Slash1 => {
                self.bad |= byte != b'/';
                self.phase = CurlPhase::Slash2;
            }
            CurlPhase::Slash2 => {
                self.bad |= byte != b'/';
                self.phase = CurlPhase::Authority;
            }
            CurlPhase::Authority => {
                if matches!(byte, b'/' | b'?' | b'#') {
                    self.bad |=
                        self.host.invalid(self.file) || self.percent.bad || self.percent.phase != 0;
                    self.percent = Percent::default();
                    self.phase = CurlPhase::Path;
                    if byte != b'/' {
                        self.path.end();
                        self.phase = CurlPhase::Query;
                    }
                    return;
                }
                if byte == b'@' {
                    self.bad |= self.at || self.percent.bad || self.percent.phase != 0;
                    self.at = true;
                    self.host = Host::default();
                    self.percent = Percent::default();
                    return;
                }
                if let Some((b, _)) = self.percent.push(byte) {
                    self.permanent_lf |= b == b'\n';
                }
                self.host.push(byte);
            }
            CurlPhase::Path => {
                if matches!(byte, b'/' | b'?' | b'#') {
                    self.bad |= self.percent.bad || self.percent.phase != 0;
                    self.percent = Percent::default();
                    self.path.end();
                    if byte != b'/' {
                        self.phase = CurlPhase::Query;
                    }
                } else if let Some((b, _)) = self.percent.push(byte) {
                    self.path.push(b);
                }
            }
            CurlPhase::Query => {
                if let Some((b, _)) = self.percent.push(byte) {
                    self.permanent_lf |= b == b'\n';
                }
            }
        }
    }
    fn invalid(mut self) -> bool {
        match self.phase {
            CurlPhase::Scheme | CurlPhase::Slash1 | CurlPhase::Slash2 => return true,
            CurlPhase::Authority => self.bad |= self.host.invalid(self.file),
            CurlPhase::Path => self.path.end(),
            CurlPhase::Query => {}
        }
        self.bad
            || self.percent.bad
            || self.percent.phase != 0
            || self.permanent_lf
            || self.path.bad
            || self.path.first_lf != 0
    }
}

#[derive(Clone, Copy, Default)]
struct Host {
    seen: bool,
    starts_colon: bool,
    invalid_char: bool,
    colon: bool,
    before_colon_invalid: bool,
    port_digits: u8,
    port: u32,
    port_bad: bool,
}
impl Host {
    fn push(&mut self, byte: u8) {
        if !self.seen {
            self.starts_colon = byte == b':';
            self.seen = true;
        }
        if byte == b':' {
            self.colon = true;
            self.before_colon_invalid = self.invalid_char;
            self.port_digits = 0;
            self.port = 0;
            self.port_bad = false;
        } else if byte == b']' {
            self.colon = false;
        } else if self.colon {
            self.port_bad |= !byte.is_ascii_digit();
            self.port_digits = self.port_digits.saturating_add(1);
            if byte.is_ascii_digit() {
                self.port = self
                    .port
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'));
            }
        }
        self.invalid_char |= !(byte.is_ascii_alphanumeric() || b".-_[:]".contains(&byte));
    }
    fn invalid(self, file: bool) -> bool {
        let missing = !self.seen || self.starts_colon;
        if missing && !file {
            return true;
        }
        if self.colon {
            self.before_colon_invalid
                || self.port_bad
                || (missing && self.port_digits != 0)
                || (self.port_digits != 0 && (self.port == 0 || self.port > 65535))
        } else {
            self.invalid_char
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Path {
    depth: u64,
    kind: u8,
    lf: bool,
    first_lf: u64,
    bad: bool,
}
impl Path {
    fn push(&mut self, byte: u8) {
        self.lf |= byte == b'\n';
        if byte == b'.' {
            self.kind = if self.kind < 2 { self.kind + 1 } else { 3 };
        } else {
            self.kind = 3;
        }
    }
    fn end(&mut self) {
        match self.kind {
            1 => {}
            2 => {
                if self.depth == 0 {
                    self.bad = true;
                } else {
                    self.depth -= 1;
                    if self.first_lf > self.depth {
                        self.first_lf = 0;
                    }
                }
            }
            _ => {
                self.depth = self.depth.saturating_add(1);
                if self.lf && self.first_lf == 0 {
                    self.first_lf = self.depth;
                }
            }
        }
        self.kind = 0;
        self.lf = false;
    }
}
