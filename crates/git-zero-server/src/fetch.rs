//! Request-local shallow/filter policy; packet storage remains owned by the host.
use git_zero_core::{BlobFilter, Error, FetchOptions, ObjectFormat, ObjectKind, Result};
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
        if self.filtered && !enabled {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.has_shallow
            || self.options.depth.is_some()
            || self.options.deepen_relative
            || self.filtered
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
    if let Some(kind) = value.strip_prefix(b"object:type=") {
        return Ok(BlobFilter::Type(match kind {
            b"commit" => ObjectKind::Commit,
            b"tree" => ObjectKind::Tree,
            b"blob" => ObjectKind::Blob,
            b"tag" => ObjectKind::Tag,
            _ => return Err(Error::InvalidRequest),
        }));
    }
    if let Some(value) = value.strip_prefix(b"tree:") {
        return scaled_filter_number(value).map(BlobFilter::TreeDepth);
    }
    let value = value
        .strip_prefix(b"blob:limit=")
        .ok_or(Error::Unsupported)?;
    scaled_filter_number(value).map(BlobFilter::Limit)
}

fn scaled_filter_number(value: &[u8]) -> Result<u64> {
    let (digits, multiplier) = match value.last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    filter_number(digits)?
        .checked_mul(multiplier)
        .ok_or(Error::InvalidRequest)
}

fn filter_number(mut value: &[u8]) -> Result<u64> {
    // Native Git uses its base-0 unsigned parser, allowing leading C whitespace
    // and one plus sign. Keep this separate from decimal protocol depths.
    while value
        .first()
        .is_some_and(|&byte| matches!(byte, b' ' | b'\t'..=b'\r'))
    {
        value = &value[1..];
    }
    let value = value.strip_prefix(b"+").unwrap_or(value);
    let (digits, radix) = match value {
        [b'0', b'x' | b'X', digits @ ..] => (digits, 16),
        [b'0', b'b' | b'B', digits @ ..] => (digits, 2),
        [b'0', ..] => (value, 8),
        _ => (value, 10),
    };
    // from_str_radix also accepts '+', but a second sign or one after a base
    // prefix is invalid in Git's numeric syntax.
    if !digits.first().is_some_and(u8::is_ascii_hexdigit) {
        return Err(Error::InvalidRequest);
    }
    let text = core::str::from_utf8(digits).map_err(|_| Error::InvalidRequest)?;
    u64::from_str_radix(text, radix).map_err(|_| Error::InvalidRequest)
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
            ("blob:limit=1K", 1024),
            ("blob:limit=1M", 1_048_576),
            ("blob:limit=1G", 1_073_741_824),
            ("blob:limit=016", 14),
            ("blob:limit=0xe", 14),
            ("blob:limit=0Xf", 15),
            ("blob:limit=0b1110", 14),
            ("blob:limit=0B1111", 15),
            ("blob:limit=+1", 1),
            ("blob:limit= 1", 1),
            ("blob:limit=\t1", 1),
            ("blob:limit=\x0b1", 1),
            ("blob:limit=\x0c1", 1),
            ("blob:limit=\r1", 1),
            ("blob:limit=\n1", 1),
            ("blob:limit= +017", 15),
            ("blob:limit=+0x10K", 16384),
            ("blob:limit=18446744073709551615", u64::MAX),
            ("blob:limit=0xffffffffffffffff", u64::MAX),
            ("blob:limit=01777777777777777777777", u64::MAX),
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
            let tree = wire.replace("blob:limit=", "tree:");
            assert_eq!(
                filter(tree.as_bytes()),
                Ok(BlobFilter::TreeDepth(expected)),
                "{tree}"
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
            "blob:limit=-1",
            "blob:limit=-0",
            "blob:limit=++1",
            "blob:limit=0x+1",
            "blob:limit=0b+1",
            "blob:limit=0x",
            "blob:limit=0b",
            "blob:limit=08",
            "blob:limit=09",
            "blob:limit=0b2",
            "blob:limit=0x10000000000000000",
            "blob:limit=02000000000000000000000",
            "blob:limit=1.5",
            "blob:limit=1t",
            "blob:limit=k",
            "blob:limit=1k ",
            "blob:limit=+ 1",
            "blob:limit=\u{a0}1",
            "blob:limit=",
            "blob:limit=1\0",
        ] {
            assert_eq!(
                filter(wire.as_bytes()),
                Err(Error::InvalidRequest),
                "{wire}"
            );
            let tree = wire.replace("blob:limit=", "tree:");
            assert_eq!(
                filter(tree.as_bytes()),
                Err(Error::InvalidRequest),
                "{tree}"
            );
        }
    }
}
