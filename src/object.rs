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
        for byte in self.0 {
            write!(output, "{byte:02x}")?;
        }
        Ok(())
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
    /// Write the loose-object/hash header. A 32-byte buffer fits every header.
    pub fn header<'a>(&self, storage: &'a mut [u8]) -> Result<&'a [u8]> {
        use core::fmt::Write;
        let mut header = SliceWriter::new(storage);
        write!(&mut header, "{} {}\0", self.kind.name(), self.size)
            .map_err(|_| Error::BufferTooSmall)?;
        Ok(header.finish())
    }
}

impl Object<'_> {
    pub fn info(&self) -> ObjectInfo {
        ObjectInfo {
            kind: self.kind,
            size: self.data.len() as u64,
        }
    }

    pub fn header<'a>(&self, storage: &'a mut [u8]) -> Result<&'a [u8]> {
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
