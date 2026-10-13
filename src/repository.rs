use crate::{Error, Object, ObjectId, ObjectInfo, Result};

/// Maximum ref name size in bytes that fits a smart HTTP advertisement.
pub const MAX_REF_NAME_LENGTH: usize = crate::pktline::MAX_LENGTH - 4 - 40 - b" \n".len();

/// Maximum default branch name size in bytes, including its HEAD symref packet.
pub const MAX_HEAD_REF_LENGTH: usize = MAX_REF_NAME_LENGTH - b"HEAD\0symref=HEAD:".len();

/// Maximum unborn branch name size, including the empty advertisement marker.
pub const MAX_UNBORN_REF_LENGTH: usize =
    MAX_REF_NAME_LENGTH - b"capabilities^{}\0symref=HEAD:".len();

/// HEAD's resolved commit and optional symbolic branch target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Head<'a> {
    Symbolic {
        target: &'a str,
        id: ObjectId,
    },
    Detached(ObjectId),
    /// A symbolic branch that has no commit or advertised ref yet.
    Unborn(&'a str),
}

impl<'a> Head<'a> {
    pub fn id(self) -> Option<ObjectId> {
        match self {
            Self::Symbolic { id, .. } | Self::Detached(id) => Some(id),
            Self::Unborn(_) => None,
        }
    }

    pub fn target(self) -> Option<&'a str> {
        match self {
            Self::Symbolic { target, .. } | Self::Unborn(target) => Some(target),
            Self::Detached(_) => None,
        }
    }
}

/// An empty repository whose HEAD points at an unborn branch.
#[derive(Clone, Copy, Debug)]
pub struct EmptyRepository<'a> {
    target: &'a str,
}

impl<'a> EmptyRepository<'a> {
    pub fn new(target: &'a str) -> Result<Self> {
        let repository = Self { target };
        validate_refs(&repository)?;
        Ok(repository)
    }
}

impl Repository for EmptyRepository<'_> {
    fn head(&self) -> ObjectId {
        ObjectId([0; 20])
    }
    fn head_state(&self) -> Head<'_> {
        Head::Unborn(self.target)
    }
    fn head_ref(&self) -> &str {
        self.target
    }
    fn object_count(&self) -> usize {
        0
    }
    fn reference_count(&self) -> usize {
        0
    }
    fn reference_name(&self, _index: usize) -> Option<&str> {
        None
    }
}

/// A stable snapshot, containing exactly the objects reachable from HEAD.
///
/// A resolved HEAD must refer to a commit; an unborn HEAD has no objects or refs.
/// Objects must stay stable throughout a request
/// and across discovery/fetch requests for the same repository URL. Enumeration
/// must be complete and deterministic; each index below `object_count` must exist.
/// Implementations may borrow memory or implement `object_info`/`read_object`
/// for file-backed or generated bodies. Their lengths and IDs must be exact.
/// All advertised refs are aliases for this single commit. Ref names must be
/// valid, unique, byte-sorted, and include the symbolic HEAD branch, if resolved.
/// Names must fit `MAX_REF_NAME_LENGTH`; symbolic and unborn HEAD targets must
/// fit `MAX_HEAD_REF_LENGTH` and `MAX_UNBORN_REF_LENGTH`, respectively.
pub trait Repository {
    /// Resolved commit ID, or the all-zero sentinel for an unborn HEAD.
    /// Use `head_state().id()` when absence needs to be distinguished.
    fn head(&self) -> ObjectId;
    fn head_state(&self) -> Head<'_> {
        Head::Symbolic {
            target: self.head_ref(),
            id: self.head(),
        }
    }
    fn object_count(&self) -> usize;
    /// Optional borrowed-body fast path. Streaming providers can leave this as
    /// `None` and implement `object_info`, `read_object`, and `object_id` instead.
    fn object(&self, _index: usize) -> Option<Object<'_>> {
        None
    }

    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.object(index)
            .map(|object| object.info())
            .ok_or(Error::ObjectNotFound)
    }

    /// Read at most `storage.len()` body bytes starting at a byte offset.
    /// Short reads are allowed. Return zero only at EOF or for empty storage.
    /// Calls must be independent of earlier reads, enabling interleaved readers
    /// and retries. Errors may modify storage but must not change the object.
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        let object = self.object(index).ok_or(Error::ObjectNotFound)?;
        let Some(bytes) = usize::try_from(offset)
            .ok()
            .and_then(|offset| object.data.get(offset..))
        else {
            return Ok(0);
        };
        let length = bytes.len().min(storage.len());
        storage[..length].copy_from_slice(&bytes[..length]);
        Ok(length)
    }

    /// Return an object's ID. Streaming providers must supply precomputed IDs;
    /// `ObjectHasher` computes them incrementally with caller-owned scratch.
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.object(index).map(|object| object.id())
    }

    fn head_ref(&self) -> &str {
        "refs/heads/main"
    }

    fn reference_count(&self) -> usize {
        1
    }

    fn reference_name(&self, index: usize) -> Option<&str> {
        (index == 0).then(|| self.head_ref())
    }

    fn find(&self, id: ObjectId) -> Option<Object<'_>> {
        self.object(self.find_index(id)?)
    }

    /// Lookup without requiring a borrowed body. Metadata providers can index it.
    fn find_index(&self, id: ObjectId) -> Option<usize> {
        (0..self.object_count()).find(|&index| self.object_id(index) == Some(id))
    }
}

/// A borrowed object and its precomputed ID, stored in caller-owned memory.
#[derive(Clone, Copy, Debug)]
pub struct ObjectMetadata<'a> {
    pub id: ObjectId,
    pub object: Object<'a>,
}

/// Index a stable repository without allocating or copying object bodies.
///
/// Construction reads the source count once, reads and hashes each object once,
/// and sorts an ID index in place. Subsequent object enumeration and ID access
/// are O(1); lookup by ID is O(log n). Object order, HEAD, and refs are preserved.
/// The source still owns the bodies and must satisfy [`Repository`]'s stability
/// contract. It must expose borrowed bodies through `object`; this wrapper does
/// not index streaming-only providers, validate object graphs, or deduplicate them.
pub struct IndexedRepository<'index, 'objects, R: Repository + ?Sized> {
    repository: &'objects R,
    objects: &'index [Option<ObjectMetadata<'objects>>],
    by_id: &'index [usize],
}

impl<'index, 'objects, R: Repository + ?Sized> IndexedRepository<'index, 'objects, R> {
    /// Both arrays need at least `repository.object_count()` elements. Extra
    /// slots are untouched. Initialize metadata with `[None; N]`.
    /// Insufficient capacity fails before reading any object. A missing source
    /// object returns `ObjectNotFound`; discard partial arrays after an error.
    ///
    /// ```
    /// use git_zero::{File, IndexedRepository, Repository, Snapshot};
    ///
    /// let files = [File { name: "hello", contents: b"hello\n" }];
    /// let mut tree = [0; 128];
    /// let mut commit = [0; 256];
    /// let snapshot = Snapshot::new(&files, &mut tree, &mut commit)?;
    /// let mut objects = [None; 3]; // Commit, tree, and blob.
    /// let mut by_id = [0; 3];
    /// let indexed = IndexedRepository::new(&snapshot, &mut objects, &mut by_id)?;
    /// assert_eq!(indexed.find(indexed.head()).unwrap().kind,
    ///            git_zero::ObjectKind::Commit);
    /// # Ok::<(), git_zero::Error>(())
    /// ```
    pub fn new(
        repository: &'objects R,
        object_storage: &'index mut [Option<ObjectMetadata<'objects>>],
        index_storage: &'index mut [usize],
    ) -> Result<Self> {
        let count = repository.object_count();
        if object_storage.len() < count || index_storage.len() < count {
            return Err(Error::BufferTooSmall);
        }
        let objects = &mut object_storage[..count];
        let by_id = &mut index_storage[..count];
        for (index, slot) in objects.iter_mut().enumerate() {
            let object = repository.object(index).ok_or(Error::ObjectNotFound)?;
            *slot = Some(ObjectMetadata {
                id: object.id(),
                object,
            });
            by_id[index] = index;
        }
        // sort_unstable uses no heap storage. Every used metadata slot is Some.
        by_id.sort_unstable_by_key(|&index| objects[index].expect("initialized metadata").id);
        Ok(Self {
            repository,
            objects,
            by_id,
        })
    }

    /// Metadata in the source repository's enumeration order.
    pub fn metadata(&self, index: usize) -> Option<&ObjectMetadata<'objects>> {
        self.objects.get(index)?.as_ref()
    }
}

impl<R: Repository + ?Sized> Repository for IndexedRepository<'_, '_, R> {
    fn head(&self) -> ObjectId {
        self.repository.head()
    }

    fn head_state(&self) -> Head<'_> {
        self.repository.head_state()
    }

    fn object_count(&self) -> usize {
        self.objects.len()
    }

    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.metadata(index).map(|metadata| metadata.object)
    }

    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.metadata(index).map(|metadata| metadata.id)
    }

    fn head_ref(&self) -> &str {
        self.repository.head_ref()
    }

    fn reference_count(&self) -> usize {
        self.repository.reference_count()
    }

    fn reference_name(&self, index: usize) -> Option<&str> {
        self.repository.reference_name(index)
    }

    fn find_index(&self, id: ObjectId) -> Option<usize> {
        let position = self
            .by_id
            .binary_search_by_key(&id, |&index| {
                self.objects[index].expect("initialized metadata").id
            })
            .ok()?;
        Some(self.by_id[position])
    }
}

/// Validate a fully qualified ref name that fits a smart advertisement.
/// The default branch additionally needs to fit `MAX_HEAD_REF_LENGTH`.
pub fn validate_ref_name(name: &str) -> crate::Result<()> {
    if name.len() > MAX_REF_NAME_LENGTH
        || !name.starts_with("refs/")
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("@{")
        || name.bytes().any(|byte| {
            byte <= b' ' || matches!(byte, 127 | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
        || name
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err(crate::Error::InvalidReference);
    }
    Ok(())
}

/// Validate a repository's advertised commit aliases and HEAD state.
/// Names must be valid, sorted, unique, free of path conflicts, and include a
/// resolved symbolic HEAD's branch. Unborn repositories have no objects or refs.
/// Object identities and graph closure are checked separately by the validator.
pub fn validate_refs(repository: &(impl Repository + ?Sized)) -> Result<()> {
    let head = repository.head_state();
    if let Some(target) = head.target() {
        validate_ref_name(target)?;
        let maximum = if head.id().is_some() {
            MAX_HEAD_REF_LENGTH
        } else {
            MAX_UNBORN_REF_LENGTH
        };
        if !target.starts_with("refs/heads/") || target.len() > maximum {
            return Err(Error::InvalidReference);
        }
    }
    match head.id() {
        Some(id) if id != repository.head() || id.0 == [0; 20] => {
            return Err(Error::InvalidObject);
        }
        None if repository.head().0 != [0; 20]
            || repository.object_count() != 0
            || repository.reference_count() != 0 =>
        {
            return Err(Error::InvalidObject);
        }
        _ => {}
    }
    let mut found = false;
    for index in 0..repository.reference_count() {
        let name = repository
            .reference_name(index)
            .ok_or(Error::InvalidReference)?;
        validate_ref_name(name)?;
        found |= Some(name) == head.target();
        for previous in 0..index {
            let previous = repository
                .reference_name(previous)
                .ok_or(Error::InvalidReference)?;
            if previous >= name
                || name
                    .strip_prefix(previous)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            {
                return Err(Error::InvalidReference);
            }
        }
    }
    if matches!(head, Head::Symbolic { .. }) && !found {
        return Err(Error::InvalidReference);
    }
    Ok(())
}
