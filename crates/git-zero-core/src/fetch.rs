/// Partial-clone object policies. Explicitly wanted objects are retained.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlobFilter {
    /// Keep the complete selected closure.
    #[default]
    All,
    /// Omit blobs reached through trees or tags. An explicit blob want wins.
    None,
    /// Omit blobs of this size or larger, in bytes.
    Limit(u64),
    /// Omit every other kind, retaining explicitly wanted objects.
    Type(ObjectKind),
}

impl BlobFilter {
    /// Keep explicitly wanted objects; apply the requested policy to the rest.
    pub const fn includes(self, info: ObjectInfo, explicitly_wanted: bool) -> bool {
        if explicitly_wanted {
            return true;
        }
        match self {
            Self::All => true,
            Self::None => !matches!(info.kind, ObjectKind::Blob),
            Self::Limit(limit) => !matches!(info.kind, ObjectKind::Blob) || info.size < limit,
            Self::Type(kind) => info.kind as u8 == kind as u8,
        }
    }
}

/// Bounded history and object selection, sharing the ordinary graph-read budget.
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
    /// Apply the object policy after reachability and common-history exclusion.
    /// Filtering does not change commit parents or tree bodies. The receiving
    /// host must retain Git's shallow/promisor metadata when importing the pack.
    pub filter: BlobFilter,
}
use crate::{Error, ObjectInfo, ObjectKind, Result};

/// History decisions for an external, host-owned graph frontier. The host
/// supplies the shortest commit distance from wanted roots (starting at one)
/// and persists deduplication, exclusions and work budgets outside this value.
#[derive(Clone, Copy, Debug, Default)]
pub struct HistoryPolicy {
    pub(crate) limit: Option<u32>,
    pub(crate) client: bool,
    pub(crate) distance: bool,
}
impl HistoryPolicy {
    /// Measure distances to existing client boundaries without crossing them.
    /// A host only needs commit-parent and tag-target edges during this probe.
    pub const fn boundary_probe() -> Self {
        Self {
            limit: None,
            client: true,
            distance: true,
        }
    }
    pub const fn follows_parents(self, depth: u32, client_boundary: bool) -> bool {
        !((self.client && client_boundary)
            || match self.limit {
                Some(limit) => depth >= limit,
                None => false,
            })
    }
    pub const fn parent_depth(self, depth: u32) -> u32 {
        if self.limit.is_some() || self.distance {
            depth.saturating_add(1)
        } else {
            1
        }
    }
}

impl FetchOptions {
    /// Shallow haves cannot exclude the newly requested history. A relative
    /// request from a complete client preserves ordinary common exclusion.
    pub const fn deepens_history(self, has_client_boundaries: bool) -> bool {
        self.depth.is_some() && (!self.deepen_relative || has_client_boundaries)
    }
    /// Derive the wanted-history cut after a relative boundary probe. None
    /// means no client boundary occurs in wanted history, leaving it complete.
    pub fn history(
        self,
        has_client_boundaries: bool,
        nearest_boundary_depth: Option<u32>,
    ) -> Result<HistoryPolicy> {
        if self
            .depth
            .is_some_and(|depth| depth == 0 || depth > i32::MAX as u32)
            || nearest_boundary_depth == Some(0)
        {
            return Err(Error::InvalidRequest);
        }
        let limit = match self
            .depth
            .filter(|_| self.deepens_history(has_client_boundaries))
        {
            Some(depth) if self.deepen_relative => nearest_boundary_depth
                .map(|boundary| boundary.checked_add(depth).ok_or(Error::LimitExceeded))
                .transpose()?,
            depth => depth,
        };
        Ok(HistoryPolicy {
            limit,
            client: self.depth.is_none(),
            distance: false,
        })
    }
}
