//! Bounded incremental HTTP header intake and delimiter scan state.
use super::super::{header_end, receive};
use super::HeaderLimits;
use git_zero::{Error, Result, http::Request};
use rustix::fd::AsFd;

#[derive(Default)]
struct HeaderScan {
    next: usize,
    #[cfg(test)]
    candidates: usize,
}

impl HeaderScan {
    fn end(&mut self, bytes: &[u8]) -> Option<usize> {
        let suffix = &bytes[self.next..];
        #[cfg(test)]
        {
            self.candidates += suffix.len().saturating_sub(3);
        }
        if let Some(end) = header_end(suffix) {
            return Some(self.next + end);
        }
        // Only these bytes can begin a delimiter completed by the next read.
        self.next = bytes.len().saturating_sub(3);
        None
    }
}

// Keep any body bytes received with the header for the subsequent bounded read.
pub(super) fn read_header(
    socket: &impl AsFd,
    storage: &mut [u8],
    max_wire_bytes: Option<usize>,
    max_header_bytes: Option<usize>,
) -> Result<(usize, usize)> {
    let capacity = [max_wire_bytes, max_header_bytes]
        .into_iter()
        .flatten()
        .fold(storage.len(), usize::min);
    let mut length = 0;
    let mut scan = HeaderScan::default();
    let end = loop {
        if let Some(end) = scan.end(&storage[..length]) {
            break end;
        }
        if length == capacity {
            return Err(
                if max_wire_bytes == Some(length) || max_header_bytes == Some(length) {
                    Error::LimitExceeded
                } else {
                    Error::BufferTooSmall
                },
            );
        }
        let count = receive(socket, &mut storage[length..capacity])?;
        if count == 0 {
            return Err(Error::InvalidRequest);
        }
        length += count;
    };
    Ok((end, length))
}

pub(super) fn request_line(header: &str) -> Result<&str> {
    let (line, _) = header.split_once("\r\n").ok_or(Error::InvalidRequest)?;
    if line.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return Err(Error::InvalidRequest);
    }
    Ok(line)
}

#[derive(Default)]
pub(super) struct Headers<'a> {
    pub(super) content_type: Option<&'a str>,
    pub(super) git_protocol: Option<&'a str>,
    pub(super) content_encoding: Option<&'a str>,
    pub(super) content_length: Option<&'a str>,
}

impl<'a> Headers<'a> {
    pub(super) fn parse(header: &'a str, limits: HeaderLimits) -> Result<Self> {
        let mut parsed = Self::default();
        for (index, line) in header
            .split("\r\n")
            .skip(1)
            .filter(|line| !line.is_empty())
            .enumerate()
        {
            let (name, value) = line.split_once(':').ok_or(Error::InvalidRequest)?;
            if name.is_empty() || !name.bytes().all(token_byte) || !value.bytes().all(field_byte) {
                return Err(Error::InvalidRequest);
            }
            if limits.max_fields.is_some_and(|limit| index >= limit)
                || limits
                    .max_name_bytes
                    .is_some_and(|limit| name.len() > limit)
                || limits
                    .max_value_bytes
                    .is_some_and(|limit| value.len() > limit)
            {
                return Err(Error::LimitExceeded);
            }
            if name.eq_ignore_ascii_case("transfer-encoding") {
                return Err(Error::Unsupported);
            }
            let slot = if name.eq_ignore_ascii_case("content-type") {
                &mut parsed.content_type
            } else if name.eq_ignore_ascii_case("git-protocol") {
                &mut parsed.git_protocol
            } else if name.eq_ignore_ascii_case("content-encoding") {
                &mut parsed.content_encoding
            } else if name.eq_ignore_ascii_case("content-length") {
                &mut parsed.content_length
            } else {
                continue;
            };
            if slot.replace(value.trim_matches([' ', '\t'])).is_some() {
                return Err(Error::InvalidRequest);
            }
        }
        Ok(parsed)
    }
}

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn field_byte(byte: u8) -> bool {
    byte == b'\t' || (byte >= b' ' && byte != 127)
}

pub(super) fn parse_request<'a>(line: &'a str, headers: &Headers<'a>) -> Result<Request<'a>> {
    let mut line = line.split_ascii_whitespace();
    let method = line.next().ok_or(Error::InvalidRequest)?;
    let target = line.next().ok_or(Error::InvalidRequest)?;
    if !matches!(line.next(), Some("HTTP/1.0" | "HTTP/1.1")) || line.next().is_some() {
        return Err(Error::InvalidRequest);
    }
    let (path, query) = target
        .split_once('?')
        .map_or((target, None), |(path, query)| (path, Some(query)));
    if !path.starts_with('/') || target.bytes().any(|b| b <= 32 || b == 127) {
        return Err(Error::InvalidRequest);
    }
    Ok(Request {
        content_type: headers.content_type,
        git_protocol: headers.git_protocol,
        method,
        path,
        query,
        body: &[],
    })
}

#[cfg(test)]
mod header_scan_tests {
    extern crate std;
    use super::HeaderScan;

    #[test]
    fn every_split_preserves_the_first_complete_delimiter() {
        let cases: &[&[u8]] = &[
            b"\r\n\r\n",
            b"GET /repo.git/HEAD HTTP/1.0\r\n\r\n",
            b"POST /upload HTTP/1.1\r\nX: a\r\nb\r\n\r\nbody\r\n\r\n",
            b"\xff\r\n\r\nbody",
            b"GET / HTTP/1.0\r\nX: incomplete\r\n",
        ];
        for wire in cases {
            let expected = wire
                .windows(4)
                .position(|value| value == b"\r\n\r\n")
                .map(|index| index + 4);
            for split in 0..=wire.len() {
                let mut scan = HeaderScan::default();
                let actual = scan.end(&wire[..split]).or_else(|| scan.end(wire));
                assert_eq!(actual, expected, "split={split}, wire={wire:?}");
                assert!(scan.candidates <= wire.len().saturating_sub(3));
            }
        }
    }

    #[test]
    fn full_capacity_headers_have_linear_work_for_every_fragment_size() {
        let mut wire = std::vec![b'a'; 65536];
        let prefix = b"GET / HTTP/1.0\r\nX: ";
        wire[..prefix.len()].copy_from_slice(prefix);
        wire[65532..].copy_from_slice(b"\r\n\r\n");
        for width in [1, 2, 3, 7, 127, 4096, 65536] {
            let mut scan = HeaderScan::default();
            let mut length = 0;
            for chunk in wire.chunks(width) {
                length += chunk.len();
                let expected = (length == wire.len()).then_some(length);
                assert_eq!(scan.end(&wire[..length]), expected);
                assert!(
                    scan.candidates <= length.saturating_sub(3),
                    "width={width}, length={length}"
                );
            }
            assert_eq!(scan.candidates, wire.len() - 3);
        }
    }

    #[test]
    fn unchanged_incomplete_input_does_not_repeat_search_work() {
        let wire = b"GET / HTTP/1.0\r\nX: incomplete\r\n";
        let mut scan = HeaderScan::default();
        for _ in 0..16 {
            assert_eq!(scan.end(wire), None);
            assert_eq!(scan.candidates, wire.len() - 3);
        }
    }
}
