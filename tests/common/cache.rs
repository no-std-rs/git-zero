use git_zero::{
    Error, Result,
    cache::{Key, PackCache},
};
use std::cell::Cell;

/// A single complete pack in fixed storage. Insertion replaces the old key only
/// after capacity/error checks; a failed insertion leaves the old entry usable.
pub struct Cache {
    pub key: Option<Key>,
    pub bytes: [u8; 4096],
    pub length: usize,
    pub gets: Cell<usize>,
    pub puts: usize,
    pub fail_get: bool,
    pub fail_put: bool,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            key: None,
            bytes: [0; 4096],
            length: 0,
            gets: Cell::new(0),
            puts: 0,
            fail_get: false,
            fail_put: false,
        }
    }
}

impl PackCache for Cache {
    fn get(&self, key: Key) -> Result<Option<&[u8]>> {
        self.gets.set(self.gets.get() + 1);
        if self.fail_get {
            return Err(Error::Backend);
        }
        Ok((self.key == Some(key)).then_some(&self.bytes[..self.length]))
    }

    fn put(&mut self, key: Key, bytes: &[u8]) -> Result<()> {
        if self.fail_put {
            return Err(Error::Backend);
        }
        let destination = self
            .bytes
            .get_mut(..bytes.len())
            .ok_or(Error::BufferTooSmall)?;
        destination.copy_from_slice(bytes);
        self.length = bytes.len();
        self.key = Some(key);
        self.puts += 1;
        Ok(())
    }
}
