//! Bounded, linear chunked transfer intake with in-place body compaction.
use git_zero::{Error, Result};

/// Extra work limits for opt-in chunked intake. All wire framing also consumes
/// the ordinary wire-byte quota; decoded identity bytes consume the body quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkedLimits {
    /// Nonzero data chunks, excluding the final zero chunk.
    pub max_chunks: usize,
    /// Hexadecimal size line including its CRLF. Extensions are unsupported.
    pub max_line_bytes: usize,
}

impl Default for ChunkedLimits {
    fn default() -> Self {
        Self {
            max_chunks: 1024,
            max_line_bytes: 32,
        }
    }
}

struct Intake<'a, 'read, F> {
    receive: &'read mut F,
    storage: &'a mut [u8],
    length: usize,
    position: usize,
    wire_limit: Option<usize>,
}

impl<F: FnMut(&mut [u8]) -> Result<usize>> Intake<'_, '_, F> {
    fn more(&mut self) -> Result<()> {
        let capacity = self
            .wire_limit
            .map_or(self.storage.len(), |limit| limit.min(self.storage.len()));
        if self.length >= capacity {
            return Err(if self.wire_limit == Some(capacity) {
                Error::LimitExceeded
            } else {
                Error::BufferTooSmall
            });
        }
        let count = (self.receive)(&mut self.storage[self.length..capacity])?;
        if count == 0 {
            return Err(Error::InvalidRequest);
        }
        if count > capacity - self.length {
            return Err(Error::Backend);
        }
        self.length += count;
        Ok(())
    }

    fn byte(&mut self) -> Result<u8> {
        if self.position == self.length {
            self.more()?;
        }
        let byte = self.storage[self.position];
        self.position += 1;
        Ok(byte)
    }

    fn size(&mut self, limit: usize) -> Result<usize> {
        let mut value = 0usize;
        let mut digits = 0usize;
        let mut consumed = 0usize;
        loop {
            if consumed >= limit {
                return Err(Error::LimitExceeded);
            }
            let byte = self.byte()?;
            consumed += 1;
            if byte == b'\r' {
                if consumed >= limit {
                    return Err(Error::LimitExceeded);
                }
                if self.byte()? != b'\n' || digits == 0 {
                    return Err(Error::InvalidRequest);
                }
                return Ok(value);
            }
            if byte == b';' {
                return Err(Error::Unsupported);
            }
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(Error::InvalidRequest),
            };
            value = value
                .checked_mul(16)
                .and_then(|value| value.checked_add(usize::from(digit)))
                .ok_or(Error::InvalidRequest)?;
            digits += 1;
        }
    }
}

pub(super) fn read(
    receive: &mut impl FnMut(&mut [u8]) -> Result<usize>,
    storage: &mut [u8],
    initial: usize,
    wire_limit: Option<usize>,
    body_limit: Option<usize>,
    limits: ChunkedLimits,
) -> Result<(usize, usize)> {
    let mut input = Intake {
        receive,
        storage,
        length: initial,
        position: 0,
        wire_limit,
    };
    let mut decoded = 0usize;
    let mut chunks = 0usize;
    loop {
        let size = input.size(limits.max_line_bytes)?;
        if size == 0 {
            // A bounded host does not interpret authentication/HTTP fields from
            // trailers. Reject them explicitly, including a second request line.
            if input.byte()? != b'\r' {
                return Err(Error::Unsupported);
            }
            if input.byte()? != b'\n' {
                return Err(Error::InvalidRequest);
            }
            if input.length != input.position {
                return Err(Error::InvalidRequest);
            }
            return Ok((input.position, decoded));
        }
        if chunks >= limits.max_chunks {
            return Err(Error::LimitExceeded);
        }
        chunks += 1;
        let end = decoded.checked_add(size).ok_or(Error::LimitExceeded)?;
        if body_limit.is_some_and(|limit| end > limit) {
            return Err(Error::LimitExceeded);
        }
        // Keep the complete encoded wire resident, including final framing.
        // Reject a declared chunk that cannot fit before trying to read it.
        let wire_end = input
            .position
            .checked_add(size)
            .and_then(|value| value.checked_add(2))
            .ok_or(Error::LimitExceeded)?;
        if wire_limit.is_some_and(|limit| wire_end > limit) {
            return Err(Error::LimitExceeded);
        }
        if wire_end > input.storage.len() {
            return Err(Error::BufferTooSmall);
        }
        let mut remaining = size;
        while remaining > 0 {
            if input.position == input.length {
                input.more()?;
            }
            let count = remaining.min(input.length - input.position);
            input
                .storage
                .copy_within(input.position..input.position + count, decoded);
            input.position += count;
            decoded += count;
            remaining -= count;
        }
        if input.byte()? != b'\r' || input.byte()? != b'\n' {
            return Err(Error::InvalidRequest);
        }
    }
}
