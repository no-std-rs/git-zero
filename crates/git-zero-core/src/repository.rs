use crate::{Error, Object, ObjectId, ObjectInfo, Result};

/// Maximum ref name size in bytes that fits a smart HTTP advertisement.
pub const MAX_REF_NAME_LENGTH: usize = crate::pktline::MAX_LENGTH - 4 - 40 - b" \n".len();

/// Maximum default branch name size in bytes, including its HEAD symref packet.
pub const MAX_HEAD_REF_LENGTH: usize = MAX_REF_NAME_LENGTH - b"HEAD\0symref=HEAD:".len();

/// Maximum unborn branch name size, including the empty advertisement marker.
pub const MAX_UNBORN_REF_LENGTH: usize =
    MAX_REF_NAME_LENGTH - b"capabilities^{}\0symref=HEAD:".len();

/// A peeled advertisement appends `^{}` to the stored ref name.
pub const MAX_PEELED_REF_LENGTH: usize = MAX_REF_NAME_LENGTH - 3;

/// One resolved advertised ref, with optional symbolic and annotated-tag data.
/// Symbolic targets must exist, resolve without cycles, and have the same ID.
/// `peeled` is the final non-tag object ID, including through nested tags.
#[derive(Clone, Copy, Debug)]
pub struct Reference<'a> {
    pub name: &'a str,
    pub id: ObjectId,
    pub symbolic_target: Option<&'a str>,
    pub peeled: Option<ObjectId>,
}

impl<'a> Reference<'a> {
    pub const fn direct(name: &'a str, id: ObjectId) -> Self {
        Self {
            name,
            id,
            symbolic_target: None,
            peeled: None,
        }
    }
}

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

/// A stable snapshot containing the union reachable from HEAD and advertised refs.
///
/// A resolved HEAD and branch tips must refer to commits. An unborn HEAD has no
/// commit, but other branches or tags may still exist.
/// Objects must stay stable throughout a request
/// and across discovery/fetch requests for the same repository URL. Enumeration
/// must be complete and deterministic; each index below `object_count` must exist.
/// Implementations may borrow memory or implement `object_info`/`read_object`
/// for file-backed or generated bodies. Their lengths and IDs must be exact.
/// Ref names must be
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

    /// Defaults preserve snapshot commit aliases. Override for independent tips.
    fn reference_id(&self, index: usize) -> Option<ObjectId> {
        self.reference_name(index)?;
        self.head_state().id()
    }

    fn reference_symbolic_target(&self, _index: usize) -> Option<&str> {
        None
    }

    fn reference_peeled(&self, _index: usize) -> Option<ObjectId> {
        None
    }

    fn find(&self, id: ObjectId) -> Option<Object<'_>> {
        self.object(self.find_index(id)?)
    }

    /// Lookup without requiring a borrowed body. Metadata providers can index it.
    fn find_index(&self, id: ObjectId) -> Option<usize> {
        (0..self.object_count()).find(|&index| self.object_id(index) == Some(id))
    }
}

/// Attach a caller-owned sorted ref manifest and HEAD to an object provider.
/// Construction validates refs and symbolic links, without reading object bodies.
/// Use `ValidatedRepository` to check IDs, target kinds, tags and the union graph.
pub struct RefRepository<'refs, 'repo, R: Repository + ?Sized> {
    source: &'repo R,
    head: Head<'refs>,
    references: &'refs [Reference<'refs>],
}

impl<'refs, 'repo, R: Repository + ?Sized> RefRepository<'refs, 'repo, R> {
    pub fn new(
        source: &'repo R,
        head: Head<'refs>,
        references: &'refs [Reference<'refs>],
    ) -> Result<Self> {
        let repository = Self {
            source,
            head,
            references,
        };
        validate_refs(&repository)?;
        Ok(repository)
    }
}

impl<R: Repository + ?Sized> Repository for RefRepository<'_, '_, R> {
    fn head(&self) -> ObjectId {
        self.head.id().unwrap_or(ObjectId([0; 20]))
    }
    fn head_state(&self) -> Head<'_> {
        self.head
    }
    fn head_ref(&self) -> &str {
        self.head.target().unwrap_or("refs/heads/main")
    }
    fn object_count(&self) -> usize {
        self.source.object_count()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.source.object(index)
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.source.object_info(index)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.source.object_id(index)
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        self.source.read_object(index, offset, storage)
    }
    fn find_index(&self, id: ObjectId) -> Option<usize> {
        self.source.find_index(id)
    }
    fn reference_count(&self) -> usize {
        self.references.len()
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        self.references.get(index).map(|reference| reference.name)
    }
    fn reference_id(&self, index: usize) -> Option<ObjectId> {
        self.references.get(index).map(|reference| reference.id)
    }
    fn reference_symbolic_target(&self, index: usize) -> Option<&str> {
        self.references.get(index)?.symbolic_target
    }
    fn reference_peeled(&self, index: usize) -> Option<ObjectId> {
        self.references.get(index)?.peeled
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
    /// # #[cfg(feature = "write")]
    /// # fn example() -> git_zero_core::Result<()> {
    /// use git_zero_core::{File, IndexedRepository, Repository, Snapshot};
    ///
    /// let files = [File { name: "hello", contents: b"hello\n" }];
    /// let mut tree = [0; 128];
    /// let mut commit = [0; 256];
    /// let snapshot = Snapshot::new(&files, &mut tree, &mut commit)?;
    /// let mut objects = [None; 3]; // Commit, tree, and blob.
    /// let mut by_id = [0; 3];
    /// let indexed = IndexedRepository::new(&snapshot, &mut objects, &mut by_id)?;
    /// assert_eq!(indexed.find(indexed.head()).unwrap().kind,
    ///            git_zero_core::ObjectKind::Commit);
    /// # Ok(())
    /// # }
    /// # #[cfg(feature = "write")]
    /// # example().unwrap();
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

    fn reference_id(&self, index: usize) -> Option<ObjectId> {
        self.repository.reference_id(index)
    }
    fn reference_symbolic_target(&self, index: usize) -> Option<&str> {
        self.repository.reference_symbolic_target(index)
    }
    fn reference_peeled(&self, index: usize) -> Option<ObjectId> {
        self.repository.reference_peeled(index)
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

/// Validate a repository's advertised refs, symbolic links and HEAD state.
/// Names must be valid, sorted, unique, free of path conflicts, and include a
/// resolved symbolic HEAD's branch. Unborn HEAD's target must be absent.
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
            || (repository.reference_count() == 0 && repository.object_count() != 0) =>
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
        let id = repository.reference_id(index).ok_or(Error::InvalidObject)?;
        if id.0 == [0; 20]
            || repository
                .reference_peeled(index)
                .is_some_and(|peeled| peeled.0 == [0; 20])
        {
            return Err(Error::InvalidObject);
        }
        if repository.reference_peeled(index).is_some() && name.len() > MAX_PEELED_REF_LENGTH {
            return Err(Error::InvalidReference);
        }
        if head.id().is_none() && index == 0 && name.len() == MAX_REF_NAME_LENGTH {
            return Err(Error::InvalidReference);
        }
        resolve_reference(repository, name)?;
        if Some(name) == head.target() && head.id() != Some(id) {
            return Err(Error::InvalidReference);
        }
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
    if let Head::Symbolic { target, .. } = head {
        let (target, _) = resolve_reference(repository, target)?;
        if target.len() > MAX_HEAD_REF_LENGTH {
            return Err(Error::InvalidReference);
        }
    }
    Ok(())
}

/// Resolve a named symbolic ref using a bounded number of manifest probes.
pub fn resolve_reference<'a>(
    repository: &'a (impl Repository + ?Sized),
    name: &str,
) -> Result<(&'a str, ObjectId)> {
    let mut index = reference_index(repository, name).ok_or(Error::InvalidReference)?;
    let id = repository.reference_id(index).ok_or(Error::InvalidObject)?;
    if id.0 == [0; 20] {
        return Err(Error::InvalidObject);
    }
    for _ in 0..repository.reference_count() {
        let name = repository
            .reference_name(index)
            .ok_or(Error::InvalidReference)?;
        let Some(target) = repository.reference_symbolic_target(index) else {
            return Ok((name, id));
        };
        validate_ref_name(target)?;
        index = reference_index(repository, target).ok_or(Error::InvalidReference)?;
        if repository.reference_id(index) != Some(id) {
            return Err(Error::InvalidReference);
        }
    }
    Err(Error::InvalidReference)
}

fn reference_index(repository: &(impl Repository + ?Sized), name: &str) -> Option<usize> {
    (0..repository.reference_count()).find(|&index| repository.reference_name(index) == Some(name))
}

/// Complete-pack HEAD keys are safe only when every ref aliases that HEAD.
pub fn uses_only_head(repository: &(impl Repository + ?Sized)) -> bool {
    (0..repository.reference_count())
        .all(|index| repository.reference_id(index) == repository.head_state().id())
}
