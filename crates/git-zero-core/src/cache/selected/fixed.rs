use super::{Key, PackCache};
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Statistics {
    pub hits: u64,
    pub misses: u64,
    pub insertions: u64,
    pub evictions: u64,
}

/// One immutable pack in caller-owned storage. Capacity failures preserve the
/// previous entry. Disabling bypasses reads and insertions, retaining the entry
/// until explicit eviction or replacement. Counters saturate at u64::MAX.
pub struct FixedCache<'a> {
    storage: &'a mut [u8],
    entry: Option<(Key, usize)>,
    enabled: bool,
    statistics: Statistics,
}

impl<'a> FixedCache<'a> {
    pub fn new(storage: &'a mut [u8]) -> Self {
        Self {
            storage,
            entry: None,
            enabled: true,
            statistics: Statistics::default(),
        }
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    pub fn statistics(&self) -> Statistics {
        self.statistics
    }
    pub fn evict(&mut self) {
        if self.entry.take().is_some() {
            self.statistics.evictions = self.statistics.evictions.saturating_add(1);
        }
    }
}

impl PackCache for FixedCache<'_> {
    fn get(&mut self, key: Key) -> Result<Option<&[u8]>> {
        if let Some((_, length)) = self
            .entry
            .filter(|(stored, _)| self.enabled && *stored == key)
        {
            self.statistics.hits = self.statistics.hits.saturating_add(1);
            return Ok(Some(&self.storage[..length]));
        }
        self.statistics.misses = self.statistics.misses.saturating_add(1);
        Ok(None)
    }
    fn put(&mut self, key: Key, pack: &[u8]) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let destination = self
            .storage
            .get_mut(..pack.len())
            .ok_or(Error::BufferTooSmall)?;
        destination.copy_from_slice(pack);
        self.entry = Some((key, pack.len()));
        self.statistics.insertions = self.statistics.insertions.saturating_add(1);
        Ok(())
    }
}
