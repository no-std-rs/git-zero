//! Bounded incremental HTTP header intake and delimiter scan state.
use super::super::{header_end, receive};
use git_zero::{Error, Result};
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
pub(super) fn read_header(socket: &impl AsFd, storage: &mut [u8]) -> Result<(usize, usize)> {
    let mut length = 0;
    let mut scan = HeaderScan::default();
    let end = loop {
        if let Some(end) = scan.end(&storage[..length]) {
            break end;
        }
        if length == storage.len() {
            return Err(Error::BufferTooSmall);
        }
        let count = receive(socket, &mut storage[length..])?;
        if count == 0 {
            return Err(Error::InvalidRequest);
        }
        length += count;
    };
    Ok((end, length))
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
