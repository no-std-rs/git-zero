use core::fmt;
use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::{Error, Result, io::SliceWriter};

/// One repository's object/hash encoding. Object formats cannot be mixed.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub enum ObjectFormat {
    #[default]
    Sha1,
    Sha256,
}

impl ObjectFormat {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }
    pub const fn id_length(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
    pub const fn hex_length(self) -> usize {
        self.id_length() * 2
    }
    pub const fn zero_id(self) -> ObjectId {
        match self {
            Self::Sha1 => ObjectId::Sha1([0; 20]),
            Self::Sha256 => ObjectId::Sha256([0; 32]),
        }
    }
    pub const fn capability(self) -> &'static [u8] {
        match self {
            Self::Sha1 => b"object-format=sha1",
            Self::Sha256 => b"object-format=sha256",
        }
    }
    pub(crate) fn hash(self) -> Hash {
        match self {
            Self::Sha1 => Hash::Sha1(Sha1::new()),
            Self::Sha256 => Hash::Sha256(Sha256::new()),
        }
    }
    #[cfg(feature = "read")]
    pub(crate) fn digest(self, bytes: &[u8]) -> ObjectId {
        let mut hash = self.hash();
        hash.update(bytes);
        hash.finish()
    }
}

#[derive(Clone)]
pub(crate) enum Hash {
    Sha1(Sha1),
    Sha256(Sha256),
}
impl Hash {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hash) => hash.update(bytes),
            Self::Sha256(hash) => hash.update(bytes),
        }
    }
    pub(crate) fn finish(self) -> ObjectId {
        match self {
            Self::Sha1(hash) => ObjectId::Sha1(hash.finalize().into()),
            Self::Sha256(hash) => ObjectId::Sha256(hash.finalize().into()),
        }
    }
}

/// A format-tagged object ID; no truncation or implicit format conversion.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub enum ObjectId {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}

/// Fixed-memory hex storage, exposing only the active 40 or 64 bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HexId {
    bytes: [u8; 64],
    length: usize,
}
impl HexId {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
    pub fn as_slice(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl core::ops::Deref for HexId {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl ObjectId {
    pub fn from_hex(hex: &[u8]) -> Result<Self> {
        let format = match hex.len() {
            40 => ObjectFormat::Sha1,
            64 => ObjectFormat::Sha256,
            _ => return Err(Error::InvalidObjectId),
        };
        let mut bytes = [0; 32];
        for (destination, pair) in bytes[..format.id_length()]
            .iter_mut()
            .zip(hex.chunks_exact(2))
        {
            *destination = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Self::from_bytes(format, &bytes[..format.id_length()])
    }

    pub fn from_bytes(format: ObjectFormat, bytes: &[u8]) -> Result<Self> {
        Ok(match format {
            ObjectFormat::Sha1 => Self::Sha1(bytes.try_into().map_err(|_| Error::InvalidObjectId)?),
            ObjectFormat::Sha256 => {
                Self::Sha256(bytes.try_into().map_err(|_| Error::InvalidObjectId)?)
            }
        })
    }
    pub const fn format(self) -> ObjectFormat {
        match self {
            Self::Sha1(_) => ObjectFormat::Sha1,
            Self::Sha256(_) => ObjectFormat::Sha256,
        }
    }
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Sha1(bytes) => bytes,
            Self::Sha256(bytes) => bytes,
        }
    }
    pub fn is_zero(self) -> bool {
        self.as_bytes().iter().all(|&byte| byte == 0)
    }
    pub fn to_hex(self) -> HexId {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut result = HexId {
            bytes: [0; 64],
            length: self.format().hex_length(),
        };
        for (i, byte) in self.as_bytes().iter().enumerate() {
            result.bytes[i * 2] = HEX[(byte >> 4) as usize];
            result.bytes[i * 2 + 1] = HEX[(byte & 15) as usize];
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
    /// Header syntax, exact body length and the expected ID's hash must all match.
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
        if data.len() as u64 != info.size || expected.format().digest(bytes) != expected {
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
        self.id_with_format(ObjectFormat::Sha1)
    }

    pub fn id_with_format(&self, format: ObjectFormat) -> ObjectId {
        // Largest header: "commit " + 20 decimal digits + NUL on a 64-bit target.
        let mut storage = [0; 32];
        let header = self
            .header(&mut storage)
            .expect("object header fits in 32 bytes");
        let mut hash = format.hash();
        hash.update(header);
        hash.update(self.data);
        hash.finish()
    }
}
