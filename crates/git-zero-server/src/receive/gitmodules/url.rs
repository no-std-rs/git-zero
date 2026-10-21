//! Bounded submodule URL policy; no normalized URL string is retained.
#[derive(Clone, Copy)]
pub(super) enum Url {
    Prefix(Prefix),
    Other(bool),
    Relative(Relative),
    Curl(Curl),
}
impl Url {
    pub(super) fn new() -> Self {
        Self::Prefix(Prefix::default())
    }
    pub(super) fn push(&mut self, byte: u8) {
        match self {
            Self::Prefix(prefix) => {
                if let Some(next) = prefix.push(byte) {
                    *self = next;
                }
            }
            Self::Other(_) => {}
            Self::Relative(relative) => relative.push(byte),
            Self::Curl(curl) => curl.push(byte),
        }
    }
    pub(super) fn invalid(&self) -> bool {
        match self {
            Self::Prefix(_) => false,
            Self::Other(bad) => *bad,
            Self::Relative(relative) => relative.invalid(),
            Self::Curl(curl) => curl.invalid(),
        }
    }
}
#[derive(Clone, Copy, Default)]
pub(super) struct Prefix {
    head: [u8; 8],
    used: usize,
}
#[derive(Clone, Copy)]
enum Transport {
    Relative,
    Parent,
    Git,
    Curl,
    RemoteCurl,
}
impl Transport {
    fn start(self) -> Url {
        match self {
            Self::Relative => Url::Relative(Relative::new(false, false)),
            Self::Parent => Url::Relative(Relative::new(true, false)),
            Self::Git => Url::Relative(Relative::new(false, true)),
            Self::Curl => Url::Curl(Curl::new(true)),
            Self::RemoteCurl => Url::Curl(Curl::new(false)),
        }
    }
}
impl Prefix {
    fn push(&mut self, byte: u8) -> Option<Url> {
        if self.used == 0 && byte == b'-' {
            return Some(Url::Other(true));
        }
        self.head[self.used] = byte;
        self.used += 1;
        let prefix = &self.head[..self.used];
        let choices: [(&[u8], Transport); 11] = [
            (b"./", Transport::Relative),
            (b"../", Transport::Parent),
            (b"git://", Transport::Git),
            (b"http://", Transport::Curl),
            (b"https://", Transport::Curl),
            (b"ftp://", Transport::Curl),
            (b"ftps://", Transport::Curl),
            (b"http::", Transport::RemoteCurl),
            (b"https::", Transport::RemoteCurl),
            (b"ftp::", Transport::RemoteCurl),
            (b"ftps::", Transport::RemoteCurl),
        ];
        let mut possible = false;
        for (needle, next) in choices {
            if !needle.starts_with(prefix) {
                continue;
            }
            possible = true;
            if prefix.len() == needle.len() {
                return Some(next.start());
            }
        }
        if possible {
            None
        } else {
            Some(Url::Other(false))
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
        if self.phase == 0 {
            if byte == b'%' {
                self.phase = 1;
                return None;
            }
            return Some((byte, false));
        }
        let Some(value) = hex(byte) else {
            self.bad = true;
            self.phase = u8::from(byte == b'%');
            return Some((byte, false));
        };
        if self.phase == 1 {
            self.high = value;
            self.phase = 2;
            None
        } else {
            self.phase = 0;
            Some((self.high * 16 + value, true))
        }
    }
    fn invalid(self) -> bool {
        self.bad || self.phase != 0
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
pub(super) struct Relative {
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
pub(super) struct Curl {
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
            CurlPhase::Scheme => self.scheme(byte),
            CurlPhase::Slash1 => {
                self.bad |= byte != b'/';
                self.phase = CurlPhase::Slash2;
            }
            CurlPhase::Slash2 => {
                self.bad |= byte != b'/';
                self.phase = CurlPhase::Authority;
            }
            CurlPhase::Authority => self.authority(byte),
            CurlPhase::Path => self.path(byte),
            CurlPhase::Query => {
                if let Some((b, _)) = self.percent.push(byte) {
                    self.permanent_lf |= b == b'\n';
                }
            }
        }
    }
    fn scheme(&mut self, byte: u8) {
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
    fn authority(&mut self, byte: u8) {
        if matches!(byte, b'/' | b'?' | b'#') {
            self.bad |= self.host.invalid(self.file) || self.percent.invalid();
            self.percent = Percent::default();
            self.phase = CurlPhase::Path;
            if byte != b'/' {
                self.path.end();
                self.phase = CurlPhase::Query;
            }
            return;
        }
        if byte == b'@' {
            self.bad |= self.at || self.percent.invalid();
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
    fn path(&mut self, byte: u8) {
        if matches!(byte, b'/' | b'?' | b'#') {
            self.bad |= self.percent.invalid();
            self.percent = Percent::default();
            self.path.end();
            if byte != b'/' {
                self.phase = CurlPhase::Query;
            }
        } else if let Some((b, _)) = self.percent.push(byte) {
            self.path.push(b);
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
