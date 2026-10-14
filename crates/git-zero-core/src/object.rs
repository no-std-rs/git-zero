use core::fmt;
use sha1::{Digest, Sha1};

use crate::{Error, Result, io::SliceWriter};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct ObjectId(pub [u8; 20]);

impl ObjectId {
    pub fn from_hex(hex: &[u8]) -> Result<Self> {
        if hex.len() != 40 {
            return Err(Error::InvalidObjectId);
        }
        let mut bytes = [0; 20];
        for (destination, pair) in bytes.iter_mut().zip(hex.chunks_exact(2)) {
            *destination = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(Self(bytes))
    }

    pub fn to_hex(self) -> [u8; 40] {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut result = [0; 40];
        for (i, byte) in self.0.iter().enumerate() {
            result[i * 2] = HEX[(byte >> 4) as usize];
            result[i * 2 + 1] = HEX[(byte & 15) as usize];
        }
        result
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        let text = core::str::from_utf8(&hex).map_err(|_| fmt::Error)?;
        output.write_str(text)
    }
}

fn nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(Error::InvalidObjectId),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ObjectKind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
}

impl ObjectKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
            Self::Blob => "blob",
            Self::Tag => "tag",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Object<'a> {
    pub kind: ObjectKind,
    pub data: &'a [u8],
}

/// Object metadata independent of how its body is stored. Lengths are 64-bit
/// so generated objects need not fit in one slice, including on 32-bit hosts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectInfo {
    pub kind: ObjectKind,
    pub size: u64,
}

impl ObjectInfo {
    /// Parse one complete canonical Git header, including its terminating NUL.
    /// Sizes use unsigned decimal without leading zeros and fit in 64 bits.
    #[cfg(feature = "read")]
    pub fn from_header(header: &[u8]) -> Result<Self> {
        let header = header.strip_suffix(&[0]).ok_or(Error::InvalidObject)?;
        let space = header
            .iter()
            .position(|&byte| byte == b' ')
            .ok_or(Error::InvalidObject)?;
        let kind = match &header[..space] {
            b"commit" => ObjectKind::Commit,
            b"tree" => ObjectKind::Tree,
            b"blob" => ObjectKind::Blob,
            b"tag" => ObjectKind::Tag,
            _ => return Err(Error::InvalidObject),
        };
        let decimal = &header[space + 1..];
        if decimal.is_empty()
            || decimal.len() > 20
            || !decimal[0].is_ascii_digit()
            || (decimal.len() > 1 && decimal[0] == b'0')
        {
            return Err(Error::InvalidObject);
        }
        let size = core::str::from_utf8(decimal)
            .map_err(|_| Error::InvalidObject)?
            .parse()
            .map_err(|_| Error::InvalidObject)?;
        Ok(Self { kind, size })
    }

    /// Write the loose-object/hash header. A 32-byte buffer fits every header.
    pub fn header<'header>(&self, storage: &'header mut [u8]) -> Result<&'header [u8]> {
        use core::fmt::Write;
        let mut header = SliceWriter::new(storage);
        write!(&mut header, "{} {}\0", self.kind.name(), self.size)
            .map_err(|_| Error::BufferTooSmall)?;
        Ok(header.finish())
    }
}

impl<'a> Object<'a> {
    /// Validate already inflated loose bytes before exposing a borrowed body.
    /// Header syntax, exact body length and the expected SHA-1 must all match.
    /// This checks the object's envelope, not its commit/tree/tag semantics.
    #[cfg(feature = "read")]
    pub fn from_loose(bytes: &'a [u8], expected: ObjectId) -> Result<Self> {
        let end = bytes
            .iter()
            .take(32)
            .position(|&byte| byte == 0)
            .ok_or(Error::InvalidObject)?
            + 1;
        let info = ObjectInfo::from_header(&bytes[..end])?;
        let data = &bytes[end..];
        if data.len() as u64 != info.size || Sha1::digest(bytes)[..] != expected.0 {
            return Err(Error::InvalidObject);
        }
        Ok(Self {
            kind: info.kind,
            data,
        })
    }

    pub fn info(&self) -> ObjectInfo {
        ObjectInfo {
            kind: self.kind,
            size: self.data.len() as u64,
        }
    }

    pub fn header<'header>(&self, storage: &'header mut [u8]) -> Result<&'header [u8]> {
        self.info().header(storage)
    }

    pub fn id(&self) -> ObjectId {
        // Largest header: "commit " + 20 decimal digits + NUL on a 64-bit target.
        let mut storage = [0; 32];
        let header = self
            .header(&mut storage)
            .expect("object header fits in 32 bytes");
        let mut hash = Sha1::new();
        hash.update(header);
        hash.update(self.data);
        ObjectId(hash.finalize().into())
    }
}
