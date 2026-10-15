//! Request-local shallow/filter policy; packet storage remains owned by the host.
use git_zero_core::{BlobFilter, Error, FetchOptions, ObjectFormat, Result};
#[cfg(feature = "read")]
use git_zero_core::{
    ObjectId,
    pktline::{self, Packet},
};

pub(crate) struct Extensions<'a> {
    #[cfg(feature = "read")]
    body: &'a [u8],
    marker: core::marker::PhantomData<&'a [u8]>,
    pub options: FetchOptions,
    has_shallow: bool,
    filtered: bool,
}

impl<'a> Extensions<'a> {
    pub fn new(_body: &'a [u8]) -> Self {
        Self {
            #[cfg(feature = "read")]
            body: _body,
            marker: core::marker::PhantomData,
            options: FetchOptions::default(),
            has_shallow: false,
            filtered: false,
        }
    }

    pub fn argument(&mut self, line: &[u8], format: ObjectFormat) -> Result<()> {
        if let Some(value) = line.strip_prefix(b"shallow ") {
            crate::upload::id(value, format)?;
            self.has_shallow = true;
        } else if let Some(value) = line.strip_prefix(b"deepen ") {
            if self.options.depth.is_some() {
                return Err(Error::InvalidRequest);
            }
            let depth = number(value)?;
            if depth == 0 || depth > i32::MAX as u64 {
                return Err(Error::InvalidRequest);
            }
            self.options.depth = Some(depth as u32);
        } else if let Some(value) = line.strip_prefix(b"filter ") {
            if self.filtered {
                return Err(Error::InvalidRequest);
            }
            self.options.filter = filter(value)?;
            self.filtered = true;
        } else {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    pub fn validate(&self, shallow: bool, filter: bool) -> Result<()> {
        if ((self.has_shallow || self.options.depth.is_some()) && !shallow)
            || (self.filtered && !filter)
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.has_shallow || self.options.depth.is_some() || self.filtered
    }

    #[cfg(feature = "read")]
    pub fn shallow(&self) -> impl Iterator<Item = Result<ObjectId>> + '_ {
        let mut bytes = self.body;
        core::iter::from_fn(move || pktline::read(&mut bytes).ok().flatten()).filter_map(|packet| {
            let Packet::Data(line) = packet else {
                return None;
            };
            let hex = line.strip_prefix(b"shallow ")?;
            Some(ObjectId::from_hex(hex.strip_suffix(b"\n").unwrap_or(hex)))
        })
    }
}

fn number(value: &[u8]) -> Result<u64> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return Err(Error::InvalidRequest);
    }
    value.iter().try_fold(0_u64, |n, byte| {
        n.checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(byte - b'0')))
            .ok_or(Error::InvalidRequest)
    })
}

fn filter(value: &[u8]) -> Result<BlobFilter> {
    if value == b"blob:none" {
        return Ok(BlobFilter::None);
    }
    let value = value
        .strip_prefix(b"blob:limit=")
        .ok_or(Error::Unsupported)?;
    let (digits, multiplier) = match value.last() {
        Some(b'k') => (&value[..value.len() - 1], 1024),
        Some(b'm') => (&value[..value.len() - 1], 1024 * 1024),
        Some(b'g') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    Ok(BlobFilter::Limit(
        number(digits)?
            .checked_mul(multiplier)
            .ok_or(Error::InvalidRequest)?,
    ))
}
