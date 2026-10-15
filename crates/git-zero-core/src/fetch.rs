/// Supported partial-clone blob policies. Explicitly wanted blobs are retained.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlobFilter {
    /// Keep the complete selected closure.
    #[default]
    All,
    /// Omit blobs reached through trees or tags. An explicit blob want wins.
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
    /// A relative request without client boundaries keeps common-history pruning.
    pub depth: Option<u32>,
    /// Add `depth` to the nearest client boundary measured from wanted tips
    /// (Git 2.55). Inert without a depth; unrelated client boundaries are preserved.
    /// Without a boundary in wanted history, select its complete closure.
    pub deepen_relative: bool,
    /// Apply the blob policy after reachability and common-history exclusion.
    /// Filtering does not change commit parents or tree bodies. The receiving
    /// host must retain Git's shallow/promisor metadata when importing the pack.
    pub filter: BlobFilter,
}
