use crate::{Error, Result};

/// A sink that writes every byte or returns an error. Errors may leave partial output.
pub trait Write {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;
}

pub struct SliceWriter<'a> {
    buffer: &'a mut [u8],
    length: usize,
}

impl<'a> SliceWriter<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, length: 0 }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer[..self.length]
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn finish(self) -> &'a [u8] {
        &self.buffer[..self.length]
    }
}

impl Write for SliceWriter<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self
            .length
            .checked_add(bytes.len())
            .ok_or(Error::BufferTooSmall)?;
        let destination = self
            .buffer
            .get_mut(self.length..end)
            .ok_or(Error::BufferTooSmall)?;
        destination.copy_from_slice(bytes);
        self.length = end;
        Ok(())
    }
}

impl core::fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        self.write_all(value.as_bytes())
            .map_err(|_| core::fmt::Error)
    }
}
