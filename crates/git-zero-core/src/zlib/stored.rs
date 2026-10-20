//! Push-driven stored-block zlib for representations whose delta base cannot
//! be reused. Sinks may change between calls and input fragmentation is inert.
use super::adler::Adler32;
use crate::{Error, Result, io::Write};

pub struct StoredEncoder {
    remaining: u64,
    block: u16,
    adler: Adler32,
    finished: bool,
    failure: Option<Error>,
}
impl StoredEncoder {
    pub fn encoded_size(size: u64) -> Result<u64> {
        let blocks = size / 65535 + u64::from(size % 65535 != 0);
        size.checked_add(blocks.checked_mul(5).ok_or(Error::LimitExceeded)?)
            .and_then(|size| size.checked_add(11))
            .ok_or(Error::LimitExceeded)
    }
    pub fn new(size: u64, output: &mut impl Write) -> Result<Self> {
        Self::encoded_size(size)?;
        output.write_all(&[0x78, 0x01])?;
        Ok(Self {
            remaining: size,
            block: 0,
            adler: Adler32::new(),
            finished: false,
            failure: None,
        })
    }
    pub fn write(&mut self, bytes: &[u8], output: &mut impl Write) -> Result<()> {
        let result = self.copy(bytes, output);
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
    fn ready(&self) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.finished {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
    fn copy(&mut self, mut bytes: &[u8], output: &mut impl Write) -> Result<()> {
        self.ready()?;
        if bytes.len() as u64 > self.remaining {
            return Err(Error::InvalidObject);
        }
        while !bytes.is_empty() {
            if self.block == 0 {
                self.block = self.remaining.min(65535) as u16;
                output.write_all(&[0])?;
                output.write_all(&self.block.to_le_bytes())?;
                output.write_all(&(!self.block).to_le_bytes())?;
            }
            let count = bytes.len().min(usize::from(self.block));
            output.write_all(&bytes[..count])?;
            self.adler.update(&bytes[..count]);
            self.block -= count as u16;
            self.remaining -= count as u64;
            bytes = &bytes[count..];
        }
        Ok(())
    }
    pub fn finish(&mut self, output: &mut impl Write) -> Result<()> {
        let result = (|| {
            self.ready()?;
            if self.remaining != 0 {
                return Err(Error::InvalidObject);
            }
            output.write_all(&[1, 0, 0, 0xff, 0xff])?;
            output.write_all(&self.adler.value().to_be_bytes())?;
            self.finished = true;
            Ok(())
        })();
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
}
