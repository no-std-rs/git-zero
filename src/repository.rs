use crate::{Error, Object, ObjectId, Result};

/// Maximum ref name size in bytes that fits a smart HTTP advertisement.
pub const MAX_REF_NAME_LENGTH: usize = crate::pktline::MAX_LENGTH - 4 - 40 - b" \n".len();

/// Maximum default branch name size in bytes, including its HEAD symref packet.
pub const MAX_HEAD_REF_LENGTH: usize = MAX_REF_NAME_LENGTH - b"HEAD\0symref=HEAD:".len();

/// A stable snapshot, containing exactly the objects reachable from `head`.
///
/// `head` must refer to a commit. Objects must stay stable throughout a request
/// and across discovery/fetch requests for the same repository URL. Enumeration
/// must be complete and deterministic; each index below `object_count` must exist.
/// Implementations may borrow memory or load objects into caller-owned storage.
/// All advertised refs are aliases for this single commit. Ref names must be
/// valid, unique, byte-sorted, and include `head_ref`, a branch under `refs/heads/`.
/// Names must fit `MAX_REF_NAME_LENGTH`; `head_ref` must fit `MAX_HEAD_REF_LENGTH`.
pub trait Repository {
    fn head(&self) -> ObjectId;
    fn object_count(&self) -> usize;
    fn object(&self, index: usize) -> Option<Object<'_>>;

    /// Return an object's ID. Implementations with metadata can avoid hashing.
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
        (0..self.object_count())
            .find_map(|index| self.object(index).filter(|object| object.id() == id))
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
/// contract. This wrapper does not validate object graphs or deduplicate them.
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

    fn find(&self, id: ObjectId) -> Option<Object<'_>> {
        let position = self
            .by_id
            .binary_search_by_key(&id, |&index| {
                self.objects[index].expect("initialized metadata").id
            })
            .ok()?;
        self.object(self.by_id[position])
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
