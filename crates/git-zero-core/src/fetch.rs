/// Supported partial-clone blob policies. Explicitly wanted blobs are retained.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlobFilter {
    #[default]
    All,
    None,
    /// Omit blobs of this size or larger, in bytes.
    Limit(u64),
}

/// Bounded history and blob selection, sharing the ordinary graph-read budget.
#[derive(Clone, Copy, Debug, Default)]
pub struct FetchOptions {
    /// Absolute commit depth from the wanted tips (1..=2147483647).
    /// None preserves client shallow boundaries. A depth request retransmits
    /// its bounded closure so shallow haves cannot hide newly requested history.
    pub depth: Option<u32>,
    pub filter: BlobFilter,
}
