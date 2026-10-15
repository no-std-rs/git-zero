//! Pull-based stored-block encoding. No provider reads occur between pulls.
use super::encode::write_entry_header;
use crate::{
    Error, ObjectId, ObjectInfo, Repository, Result, io::SliceWriter, object::Hash,
    zlib::adler::Adler32,
};

/// A resumable pack encoder. Retain the repository and selected order unchanged
/// until EOF. `read` performs at most one object-body read, bounded by its output
/// slice. The cursor owns hash/framing state, never object or pack-sized storage.
pub struct Encoder<'a> {
    order: Option<&'a [usize]>,
    count: usize,
    next: usize,
    object: Option<LooseEncoder>,
    hash: Hash,
    pending: [u8; 32],
    position: usize,
    length: usize,
    checksum: Option<ObjectId>,
}

impl<'a> Encoder<'a> {
    /// Encode all objects (`None`) or the supplied unique indices, in order.
    /// Invalid selections fail before the first output byte.
    pub fn new(repository: &impl Repository, order: Option<&'a [usize]>) -> Result<Self> {
        if let Some(order) = order {
            super::encode::validate_order(repository, order)?;
        }
        let count = order.map_or(repository.object_count(), <[usize]>::len);
        let count32 = u32::try_from(count).map_err(|_| Error::Unsupported)?;
        let mut pending = [0; 32];
        pending[..4].copy_from_slice(b"PACK");
        pending[4..8].copy_from_slice(&2_u32.to_be_bytes());
        pending[8..12].copy_from_slice(&count32.to_be_bytes());
        Ok(Self {
            order,
            count,
            next: 0,
            object: None,
            hash: repository.object_format().hash(),
            pending,
            position: 0,
            length: 12,
            checksum: None,
        })
    }

    /// Return bytes to stage for transport. Preserve unsent bytes at the host;
    /// calling again advances the cursor. Zero means complete, never pending.
    /// Backend read failures leave the failed read resumable. After any failure,
    /// discard bytes in that call's output slice. Earlier successful pulls stand.
    pub fn read(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<usize> {
        loop {
            if self.position < self.length {
                let count = drain(&self.pending[..self.length], &mut self.position, output)?;
                if self.checksum.is_none() {
                    self.hash.update(&output[..count]);
                }
                return Ok(count);
            }
            if self.checksum.is_some() {
                return Ok(0);
            }
            if output.is_empty() {
                return Err(Error::BufferTooSmall);
            }
            if let Some(object) = &mut self.object {
                let count = object.read(repository, output)?;
                if count != 0 {
                    self.hash.update(&output[..count]);
                    return Ok(count);
                }
                self.object = None;
                self.next += 1;
            }
            self.next_entry(repository)?;
        }
    }

    fn next_entry(&mut self, repository: &impl Repository) -> Result<()> {
        if self.next == self.count {
            let checksum = self.hash.clone().finish();
            self.pending[..checksum.as_bytes().len()].copy_from_slice(checksum.as_bytes());
            self.position = 0;
            self.length = checksum.as_bytes().len();
            self.checksum = Some(checksum);
            return Ok(());
        }
        let index = self.order.map_or(self.next, |order| order[self.next]);
        let info = repository.object_info(index)?;
        let mut header = SliceWriter::new(&mut self.pending);
        write_entry_header(info, &mut header)?;
        self.length = header.len();
        self.position = 0;
        self.object = Some(LooseEncoder::from_info(index, info, false)?);
        Ok(())
    }

    /// Checksum becomes available after its complete trailer has been drained.
    pub fn checksum(&self) -> Option<ObjectId> {
        self.checksum.filter(|_| self.position == self.length)
    }
}

/// A resumable loose object encoder. Repository bytes and metadata must remain
/// stable. Uses the same stored blocks as `write_loose_with_scratch`.
pub struct LooseEncoder {
    index: usize,
    size: u64,
    offset: u64,
    block: u16,
    adler: Adler32,
    prefix: [u8; 32],
    prefix_length: usize,
    pending: [u8; 64],
    position: usize,
    length: usize,
    phase: Phase,
}

enum Phase {
    Prefix,
    Block,
    Body,
    End,
    Done,
}

impl LooseEncoder {
    pub fn new(repository: &impl Repository, index: usize) -> Result<Self> {
        Self::from_info(index, repository.object_info(index)?, true)
    }

    pub(super) fn from_info(index: usize, info: ObjectInfo, loose: bool) -> Result<Self> {
        let mut prefix = [0; 32];
        let prefix_length = if loose {
            info.header(&mut prefix)?.len()
        } else {
            0
        };
        let mut pending = [0; 64];
        pending[..2].copy_from_slice(&[0x78, 0x01]);
        Ok(Self {
            index,
            size: info.size,
            offset: 0,
            block: 0,
            adler: Adler32::new(),
            prefix,
            prefix_length,
            pending,
            position: 0,
            length: 2,
            phase: Phase::Prefix,
        })
    }

    pub fn read(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<usize> {
        loop {
            if self.position < self.length {
                return drain(&self.pending[..self.length], &mut self.position, output);
            }
            if matches!(self.phase, Phase::Done) {
                return Ok(0);
            }
            if output.is_empty() {
                return Err(Error::BufferTooSmall);
            }
            self.position = 0;
            self.length = 0;
            if let Some(count) = self.step(repository, output)? {
                return Ok(count);
            }
        }
    }

    fn step(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<Option<usize>> {
        match self.phase {
            Phase::Prefix => self.start_prefix(),
            Phase::Block => self.start_block(),
            Phase::Body => return self.read_body(repository, output).map(Some),
            Phase::End => self.end(),
            Phase::Done => unreachable!(),
        }
        Ok(None)
    }

    fn start_prefix(&mut self) {
        self.phase = Phase::Block;
        if self.prefix_length == 0 {
            return;
        }
        self.block_header(self.prefix_length as u16);
        self.pending[5..5 + self.prefix_length].copy_from_slice(&self.prefix[..self.prefix_length]);
        self.length += self.prefix_length;
        self.adler.update(&self.prefix[..self.prefix_length]);
    }

    fn start_block(&mut self) {
        if self.offset == self.size {
            self.phase = Phase::End;
            return;
        }
        self.block = (self.size - self.offset).min(u64::from(u16::MAX)) as u16;
        self.block_header(self.block);
        self.phase = Phase::Body;
    }

    fn read_body(&mut self, repository: &impl Repository, output: &mut [u8]) -> Result<usize> {
        let capacity = usize::from(self.block).min(output.len());
        let count = repository.read_object(self.index, self.offset, &mut output[..capacity])?;
        if count == 0 || count > capacity {
            return Err(Error::InvalidObject);
        }
        self.offset += count as u64;
        self.block -= count as u16;
        self.adler.update(&output[..count]);
        if self.block == 0 {
            self.phase = Phase::Block;
        }
        Ok(count)
    }

    fn end(&mut self) {
        self.pending[..5].copy_from_slice(&[1, 0, 0, 0xff, 0xff]);
        self.pending[5..9].copy_from_slice(&self.adler.value().to_be_bytes());
        self.length = 9;
        self.phase = Phase::Done;
    }

    fn block_header(&mut self, length: u16) {
        self.pending[0] = 0;
        self.pending[1..3].copy_from_slice(&length.to_le_bytes());
        self.pending[3..5].copy_from_slice(&(!length).to_le_bytes());
        self.length = 5;
    }
}

pub(crate) fn drain(bytes: &[u8], position: &mut usize, output: &mut [u8]) -> Result<usize> {
    if output.is_empty() {
        return Err(Error::BufferTooSmall);
    }
    let count = output.len().min(bytes.len() - *position);
    output[..count].copy_from_slice(&bytes[*position..*position + count]);
    *position += count;
    Ok(count)
}
