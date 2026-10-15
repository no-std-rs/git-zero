//! Request-local shallow/filter policy; packet storage remains owned by the host.
use git_zero_core::{BlobFilter, Error, FetchOptions, ObjectFormat, Result};
#[derive(Default)]
pub(crate) struct Extensions {
    pub options: FetchOptions,
    has_shallow: bool,
    filtered: bool,
}

impl Extensions {
    pub fn argument(&mut self, line: &[u8], format: ObjectFormat) -> Result<()> {
        match argument_parts(line) {
            (b"shallow", Some(value)) => {
                crate::upload::id(value, format)?;
                self.has_shallow = true;
            }
            (b"deepen", Some(value)) => {
                if self.options.depth.is_some() {
                    return Err(Error::InvalidRequest);
                }
                let depth = number(value)?;
                if depth == 0 || depth > i32::MAX as u64 {
                    return Err(Error::InvalidRequest);
                }
                self.options.depth = Some(depth as u32);
            }
            (b"deepen-relative", None) if !self.options.deepen_relative => {
                self.options.deepen_relative = true;
            }
            (b"filter", Some(value)) => {
                if self.filtered {
                    return Err(Error::InvalidRequest);
                }
                self.options.filter = filter(value)?;
                self.filtered = true;
            }
            _ => return Err(Error::Unsupported),
        }
        Ok(())
    }

    pub fn require_filter_capability(&self, enabled: bool) -> Result<()> {
        if self.options.deepen_relative && self.options.depth.is_none() {
            return Err(Error::InvalidRequest);
        }
        if self.filtered && !enabled {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.has_shallow || self.options.depth.is_some() || self.filtered
    }
}

pub(crate) fn argument_parts(line: &[u8]) -> (&[u8], Option<&[u8]>) {
    let mut parts = line.splitn(2, |&byte| byte == b' ');
    (parts.next().unwrap_or_default(), parts.next())
}

fn number(value: &[u8]) -> Result<u64> {
    let text = core::str::from_utf8(value).map_err(|_| Error::InvalidRequest)?;
    if text.starts_with('+') {
        return Err(Error::InvalidRequest);
    }
    text.parse().map_err(|_| Error::InvalidRequest)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_limits_preserve_full_width_and_scaled_integer_boundaries() {
        let cases = [
            ("blob:limit=0", 0),
            ("blob:limit=0006", 6),
            ("blob:limit=1024", 1024),
            ("blob:limit=1k", 1024),
            ("blob:limit=2k", 2048),
            ("blob:limit=1m", 1_048_576),
            ("blob:limit=1g", 1_073_741_824),
            ("blob:limit=18446744073709551615", u64::MAX),
            ("blob:limit=18014398509481983k", 18_446_744_073_709_550_592),
            ("blob:limit=17592186044415m", 18_446_744_073_708_503_040),
            ("blob:limit=17179869183g", 18_446_744_072_635_809_792),
        ];
        for (wire, expected) in cases {
            assert_eq!(
                filter(wire.as_bytes()),
                Ok(BlobFilter::Limit(expected)),
                "{wire}"
            );
        }
        assert_eq!(filter(b"blob:none"), Ok(BlobFilter::None));
    }

    #[test]
    fn scaled_limits_reject_overflow_at_the_first_unrepresentable_integer() {
        for wire in [
            "blob:limit=18446744073709551616",
            "blob:limit=18014398509481984k",
            "blob:limit=17592186044416m",
            "blob:limit=17179869184g",
            "blob:limit=+1",
            "blob:limit=-1",
            "blob:limit=1.5",
            "blob:limit=1K",
            "blob:limit=1M",
            "blob:limit=1G",
            "blob:limit=1t",
            "blob:limit=k",
            "blob:limit=1k ",
            "blob:limit= 1k",
            "blob:limit=",
            "blob:limit=1\0",
        ] {
            assert_eq!(
                filter(wire.as_bytes()),
                Err(Error::InvalidRequest),
                "{wire}"
            );
        }
    }
}
