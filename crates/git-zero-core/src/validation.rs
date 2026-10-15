//! Opt-in validation of the repository graph contract, without allocation.
use core::cmp::Ordering;

use crate::{
    BlobFilter, Error, FetchOptions, Head, Object, ObjectFormat, ObjectHasher, ObjectId,
    ObjectInfo, ObjectKind, ObjectReader, Repository, Result,
    names::{cmp_tree_names, validate_name},
    repository::validate_refs,
};

/// Cached identity and size of a validated object, in enumeration order.
#[derive(Clone, Copy, Debug)]
pub struct ValidationMetadata {
    pub id: ObjectId,
    pub info: ObjectInfo,
    reached: bool,
    checked: bool,
    excluded: bool,
    depth: u32,
    client_shallow: bool,
    shallow: bool,
}

impl ValidationMetadata {
    /// A reached commit at a requested history boundary (including root commits).
    pub fn is_shallow(&self) -> bool {
        self.reached && self.shallow
    }

    /// A client's former boundary reached inside the requested history.
    pub fn is_unshallow(&self) -> bool {
        self.reached && self.client_shallow && !self.shallow
    }
}

#[derive(Clone, Copy, Default)]
struct History {
    limit: Option<u32>,
    client: bool,
    distance: bool,
}

/// Traversal budget and annotated-tag policy for a selected fetch pack.
#[derive(Clone, Copy, Debug)]
pub struct SelectionOptions {
    /// Maximum structural bytes read across common history, wanted objects and
    /// included tags. Buffered reads and rereads consume the same budget.
    pub graph_read_limit: u64,
    /// Include advertised annotated and nested tags whose final peeled target
    /// is being sent. A target excluded as common history does not qualify.
    /// Tags use the existing object/index arrays and traversal budget; no extra
    /// collection or allocation is required. False selects only explicit wants.
    pub include_tags: bool,
}

/// Validate and index a stable repository, including streaming-only providers.
///
/// Checks repository-format IDs, unique enumeration, links and target kinds,
/// tree modes, names, Git ordering and duplicates, tag links/peeling, and exact
/// reachability from the union of HEAD and refs. Commit parents are followed;
/// gitlinks name external repositories
/// and are not followed. Branches and resolved HEAD must name commits.
///
/// This is graph validation, not a replacement for all of Git's fsck policies:
/// identity semantics, platform-specific filename aliases, and special files
/// such as .gitmodules are outside its scope.
///
/// The provider must remain stable after validation. Bodies are never retained
/// or copied in full. Enumeration/IDs/metadata use the index; body reads and refs
/// continue to use the provider.
pub struct ValidatedRepository<'index, 'repo, R: Repository + ?Sized> {
    repository: &'repo R,
    objects: &'index [Option<ValidationMetadata>],
    by_id: &'index [usize],
    head: ObjectId,
}

impl<'index, 'repo, R: Repository + ?Sized> ValidatedRepository<'index, 'repo, R> {
    /// Each metadata/index array needs object_count slots; extra slots are
    /// untouched. Initialize metadata with `[None; N]`. Read scratch needs at
    /// least one byte for nonempty bodies. Empty repositories need no scratch.
    /// Parse scratch must fit the longest commit/tag header line
    /// (excluding LF), and twice the longest tree name (excluding NUL).
    /// Neither scratch buffer is retained. Their sizes do not limit body sizes.
    ///
    /// Insufficient arrays/scratch return BufferTooSmall, absent objects/links
    /// ObjectNotFound, invalid refs InvalidReference, invalid names
    /// InvalidFileName, and ordering/duplicate names UnsortedFiles. Invalid IDs,
    /// kinds, framing, modes, duplicate IDs or unreachable objects return
    /// InvalidObject. Backend errors propagate. Discard partial arrays on error.
    ///
    /// Hashing is linear in body bytes. Lookup uses binary search. Traversal
    /// scans pending metadata without recursion, using O(n^2) metadata probes
    /// in the worst case. Directory/file duplicate detection can reread earlier
    /// tree entries, trading worst-case quadratic tree reads for bounded memory.
    pub fn new(
        repository: &'repo R,
        object_storage: &'index mut [Option<ValidationMetadata>],
        index_storage: &'index mut [usize],
        read_scratch: &mut [u8],
        parse_scratch: &mut [u8],
    ) -> Result<Self> {
        let count = repository.object_count();
        if object_storage.len() < count || index_storage.len() < count {
            return Err(Error::BufferTooSmall);
        }
        validate_refs(repository)?;
        let objects = &mut object_storage[..count];
        let by_id = &mut index_storage[..count];
        initialize_index(repository, objects, by_id, read_scratch)?;
        let head = repository.head();
        if let Some(id) = repository.head_state().id() {
            reach(objects, by_id, id, ObjectKind::Commit)?;
        }
        for index in 0..repository.reference_count() {
            validate_reference_target(
                repository,
                index,
                objects,
                by_id,
                read_scratch,
                parse_scratch,
            )?;
        }
        walk(
            repository,
            objects,
            by_id,
            (read_scratch, parse_scratch),
            None,
            History::default(),
        )?;
        if objects
            .iter()
            .any(|slot| !slot.expect("initialized metadata").reached)
        {
            return Err(Error::InvalidObject);
        }
        Ok(Self {
            repository,
            objects,
            by_id,
            head,
        })
    }

    pub fn metadata(&self, index: usize) -> Option<&ValidationMetadata> {
        self.objects.get(index)?.as_ref()
    }

    /// Select the exact reachable union of explicit object IDs without allocation.
    /// Each work/index array needs object_count slots. Read/parse budgets have the
    /// same requirements as validation; graph bodies are read again, not hashed.
    /// The result borrows only `selected`, in the provider's enumeration order.
    /// Overlapping roots are deduplicated. Gitlinks remain external. A failed
    /// selection invalidates the work arrays; every retry resets all traversal
    /// state. `graph_read_limit` caps actual structural reads, including prefetch
    /// and rereads for tree duplicate checks; exhaustion returns LimitExceeded.
    /// Blob bodies and opaque commit/tag messages are not traversed. Metadata
    /// probes are bounded by the object-array capacity (quadratic worst case).
    /// Independent requests use independent caller storage. This primitive
    /// accepts any present root; HTTP's advertised-want policy is separate.
    pub fn select_objects<'selected>(
        &self,
        roots: &[ObjectId],
        work: &mut [Option<ValidationMetadata>],
        selected: &'selected mut [usize],
        read_scratch: &mut [u8],
        parse_scratch: &mut [u8],
        graph_read_limit: u64,
    ) -> Result<&'selected [usize]> {
        self.select_missing_objects(
            roots,
            core::iter::empty(),
            work,
            selected,
            (read_scratch, parse_scratch),
            graph_read_limit,
        )
    }

    /// Select the wanted closure minus the complete closure of `common` roots.
    /// Common roots must be present, and are trusted to describe complete client
    /// history (shallow clients must be rejected by the protocol layer). Both
    /// traversals share one graph-read budget and the same caller-owned arrays.
    /// Common-root iteration may fail (for example, a host metadata read); errors
    /// propagate. Unknown roots fail; retries reset exclusions and traversal state.
    pub fn select_missing_objects<'selected>(
        &self,
        roots: &[ObjectId],
        common: impl IntoIterator<Item = Result<ObjectId>>,
        work: &mut [Option<ValidationMetadata>],
        selected: &'selected mut [usize],
        scratch: (&mut [u8], &mut [u8]),
        graph_read_limit: u64,
    ) -> Result<&'selected [usize]> {
        self.select_fetch_objects(
            roots,
            common,
            work,
            selected,
            scratch,
            SelectionOptions {
                graph_read_limit,
                include_tags: false,
            },
        )
    }

    /// Select missing objects and optionally include advertised annotated tags
    /// whose final peeled targets are being sent. Tag traversal shares the
    /// graph-read budget with wanted and common history; retries reset all state.
    pub fn select_fetch_objects<'selected>(
        &self,
        roots: &[ObjectId],
        common: impl IntoIterator<Item = Result<ObjectId>>,
        work: &mut [Option<ValidationMetadata>],
        selected: &'selected mut [usize],
        scratch: (&mut [u8], &mut [u8]),
        options: SelectionOptions,
    ) -> Result<&'selected [usize]> {
        self.select_bounded_fetch(
            roots,
            (common, core::iter::empty()),
            work,
            selected,
            scratch,
            (options, FetchOptions::default()),
        )
    }

    /// Select shallow/filtered history with caller-owned arrays. Client shallow
    /// IDs must name present commits. Common traversal stops at those commits;
    /// depth requests instead resend their closure without trusting common haves.
    /// Relative requests from full clients preserve ordinary common exclusion.
    /// Relative depth measures the nearest boundary from wanted history, then
    /// extends that absolute cut (Git 2.55). Both passes share the budget.
    /// Work metadata retains shallow/unshallow markers until the next selection.
    /// All structural reads, including revisits after a shorter merge path,
    /// consume one budget. Retries reset depths, exclusions and boundary markers.
    ///
    /// `history` contains common roots followed by the client's shallow IDs.
    /// Each work/index slice needs one slot per repository object. Extra slots
    /// remain untouched. The returned indices use provider enumeration order;
    /// inspect only the first `object_count()` work slots for boundary updates.
    /// A depth request may resend common objects, while an ordinary request
    /// subtracts common history only as far as the client's declared boundaries.
    ///
    /// Empty wants, zero/oversized depth and absent or non-commit shallow IDs
    /// return `InvalidRequest`. Missing wanted/common objects return
    /// `ObjectNotFound`; short arrays or scratch return `BufferTooSmall`, and
    /// exhausted structural-read budgets return `LimitExceeded`. No result is
    /// published on failure; reuse the same arrays for a fresh attempt.
    ///
    /// This example selects a depth-one snapshot without its blob. The root
    /// commit is a shallow boundary even though it has no parents, matching Git.
    /// The source, validation index and per-request work arrays are independent.
    ///
    /// ```
    /// # #[cfg(feature = "write")]
    /// # fn main() -> git_zero_core::Result<()> {
    /// use git_zero_core::{
    ///     BlobFilter, FetchOptions, File, Repository, SelectionOptions, Snapshot,
    ///     ValidatedRepository,
    /// };
    /// let files = [File { name: "hello.txt", contents: b"hello\n" }];
    /// let (mut tree, mut commit) = ([0; 128], [0; 512]);
    /// let snapshot = Snapshot::new(&files, &mut tree, &mut commit)?;
    /// let (mut metadata, mut by_id) = ([None; 3], [0; 3]);
    /// let repository = ValidatedRepository::new(
    ///     &snapshot, &mut metadata, &mut by_id, &mut [0; 64], &mut [0; 128],
    /// )?;
    /// let (mut work, mut indices) = ([None; 3], [0; 3]);
    /// let wanted = [repository.head()];
    /// let selected = repository.select_bounded_fetch(
    ///     &wanted, (core::iter::empty(), core::iter::empty()),
    ///     &mut work, &mut indices, (&mut [0; 64], &mut [0; 128]),
    ///     (SelectionOptions { graph_read_limit: 4096, include_tags: false },
    ///      FetchOptions { depth: Some(1), filter: BlobFilter::None, ..FetchOptions::default() }),
    /// )?;
    /// assert_eq!(selected.len(), 2); // Commit and tree, with no blob.
    /// let boundary = work.iter().flatten().find(|object| object.is_shallow()).unwrap();
    /// assert_eq!(boundary.id, repository.head());
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "write"))]
    /// # fn main() {}
    /// ```
    pub fn select_bounded_fetch<'selected>(
        &self,
        roots: &[ObjectId],
        history: (
            impl IntoIterator<Item = Result<ObjectId>>,
            impl IntoIterator<Item = Result<ObjectId>>,
        ),
        work: &mut [Option<ValidationMetadata>],
        selected: &'selected mut [usize],
        scratch: (&mut [u8], &mut [u8]),
        options: (SelectionOptions, FetchOptions),
    ) -> Result<&'selected [usize]> {
        let (options, fetch) = options;
        if (fetch.deepen_relative && fetch.depth.is_none())
            || fetch
                .depth
                .is_some_and(|depth| depth == 0 || depth > i32::MAX as u32)
        {
            return Err(Error::InvalidRequest);
        }
        let SelectionOptions {
            graph_read_limit,
            include_tags,
        } = options;
        let (read_scratch, parse_scratch) = scratch;
        let (common, shallow) = history;
        if roots.is_empty() {
            return Err(Error::InvalidRequest);
        }
        let count = self.objects.len();
        if work.len() < count || selected.len() < count {
            return Err(Error::BufferTooSmall);
        }
        let work = &mut work[..count];
        work.copy_from_slice(self.objects);
        for slot in work.iter_mut() {
            let object = slot.as_mut().expect("validated metadata");
            object.reached = false;
            object.checked = false;
            object.depth = u32::MAX;
            object.client_shallow = false;
            object.shallow = false;
        }
        let mut has_shallow = false;
        for id in shallow {
            let index = find(work, self.by_id, id?).ok_or(Error::InvalidRequest)?;
            let object = work[index].as_mut().expect("validated metadata");
            if object.info.kind != ObjectKind::Commit {
                return Err(Error::InvalidRequest);
            }
            object.client_shallow = true;
            has_shallow = true;
        }
        // A relative request from a full client is an ordinary incremental
        // fetch. Its depth cannot expose missing history behind a boundary.
        let deepening = fetch.depth.is_some() && (!fetch.deepen_relative || has_shallow);
        for id in common {
            let id = id?;
            let index = find(work, self.by_id, id).ok_or(Error::ObjectNotFound)?;
            if !deepening {
                reach(work, self.by_id, id, metadata(work, index).info.kind)?;
            }
        }
        let mut remaining = graph_read_limit;
        walk(
            self,
            work,
            self.by_id,
            (read_scratch, parse_scratch),
            Some(&mut remaining),
            History {
                limit: None,
                client: true,
                distance: false,
            },
        )?;
        for slot in work.iter_mut() {
            let object = slot.as_mut().expect("validated metadata");
            object.excluded = object.reached;
            object.reached = false;
            object.checked = false;
            object.depth = u32::MAX;
            object.shallow = false;
        }
        let mut depth = fetch.depth.filter(|_| deepening);
        if fetch.deepen_relative && has_shallow {
            // Git 2.55 measures the nearest client boundary from the wants.
            // Unrelated shallow branches cannot influence or extend this cut.
            for &id in roots {
                let index = find(work, self.by_id, id).ok_or(Error::ObjectNotFound)?;
                reach(work, self.by_id, id, metadata(work, index).info.kind)?;
            }
            walk(
                self,
                work,
                self.by_id,
                (read_scratch, parse_scratch),
                Some(&mut remaining),
                History {
                    limit: None,
                    client: true,
                    distance: true,
                },
            )?;
            // Git 2.55 get_shallow_commits returns no new boundaries when none
            // match. Unlike Git 2.54, this leaves wanted history complete.
            depth = work
                .iter()
                .flatten()
                .filter(|object| object.reached && object.client_shallow)
                .map(|object| object.depth)
                .min()
                .map(|boundary| {
                    boundary
                        .checked_add(fetch.depth.expect("relative depth"))
                        .ok_or(Error::LimitExceeded)
                })
                .transpose()?;
            for slot in work.iter_mut() {
                let object = slot.as_mut().expect("validated metadata");
                object.reached = false;
                object.checked = false;
                object.depth = u32::MAX;
                object.shallow = false;
            }
        }
        for &id in roots {
            let index = find(work, self.by_id, id).ok_or(Error::ObjectNotFound)?;
            reach(work, self.by_id, id, metadata(work, index).info.kind)?;
        }
        walk(
            self,
            work,
            self.by_id,
            (read_scratch, parse_scratch),
            Some(&mut remaining),
            History {
                limit: depth,
                client: fetch.depth.is_none(),
                distance: false,
            },
        )?;
        filter_blobs(work, roots, fetch.filter);
        if include_tags {
            self.include_matching_tags(work)?;
            walk(
                self,
                work,
                self.by_id,
                (read_scratch, parse_scratch),
                Some(&mut remaining),
                History {
                    limit: depth,
                    client: fetch.depth.is_none(),
                    distance: false,
                },
            )?;
            filter_blobs(work, roots, fetch.filter);
        }
        let mut length = 0;
        for (index, slot) in work.iter().enumerate() {
            if slot.expect("initialized metadata").reached {
                selected[length] = index;
                length += 1;
            }
        }
        Ok(&selected[..length])
    }

    fn include_matching_tags(&self, work: &mut [Option<ValidationMetadata>]) -> Result<()> {
        for reference in 0..self.reference_count() {
            let Some(peeled) = self.reference_peeled(reference) else {
                continue;
            };
            let index = find(work, self.by_id, peeled).ok_or(Error::ObjectNotFound)?;
            if metadata(work, index).reached {
                let id = self.reference_id(reference).ok_or(Error::InvalidObject)?;
                reach_at(
                    work,
                    self.by_id,
                    id,
                    ObjectKind::Tag,
                    metadata(work, index).depth,
                )?;
            }
        }
        Ok(())
    }
}

fn filter_blobs(
    objects: &mut [Option<ValidationMetadata>],
    roots: &[ObjectId],
    filter: BlobFilter,
) {
    let limit = match filter {
        BlobFilter::All => return,
        BlobFilter::None => 0,
        BlobFilter::Limit(limit) => limit,
    };
    for slot in objects {
        let object = slot.as_mut().expect("validated metadata");
        if object.info.kind == ObjectKind::Blob
            && !roots.contains(&object.id)
            && object.info.size >= limit
        {
            object.reached = false;
        }
    }
}

impl<R: Repository + ?Sized> Repository for ValidatedRepository<'_, '_, R> {
    fn object_format(&self) -> ObjectFormat {
        self.repository.object_format()
    }
    fn head(&self) -> ObjectId {
        self.head
    }
    fn head_state(&self) -> Head<'_> {
        self.repository.head_state()
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.metadata(index)?;
        self.repository.object(index)
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.metadata(index)
            .map(|object| object.info)
            .ok_or(Error::ObjectNotFound)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.metadata(index).map(|object| object.id)
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        self.object_info(index)?;
        self.repository.read_object(index, offset, storage)
    }
    fn find_index(&self, id: ObjectId) -> Option<usize> {
        find(self.objects, self.by_id, id)
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
}

// Hash and index the stable enumeration before traversing any graph edges.
fn initialize_index(
    repository: &(impl Repository + ?Sized),
    objects: &mut [Option<ValidationMetadata>],
    by_id: &mut [usize],
    read_scratch: &mut [u8],
) -> Result<()> {
    for (index, slot) in objects.iter_mut().enumerate() {
        let info = repository.object_info(index)?;
        let id = repository.object_id(index).ok_or(Error::ObjectNotFound)?;
        if ObjectHasher::new(repository, index)?.finish(read_scratch)? != id
            || repository.object(index).is_some_and(|object| {
                object.info() != info || object.id_with_format(repository.object_format()) != id
            })
        {
            return Err(Error::InvalidObject);
        }
        *slot = Some(ValidationMetadata {
            id,
            info,
            reached: false,
            checked: false,
            excluded: false,
            depth: u32::MAX,
            client_shallow: false,
            shallow: false,
        });
        by_id[index] = index;
    }
    by_id.sort_unstable_by_key(|&index| metadata(objects, index).id);
    if by_id
        .windows(2)
        .any(|pair| metadata(objects, pair[0]).id == metadata(objects, pair[1]).id)
    {
        return Err(Error::InvalidObject);
    }
    Ok(())
}

fn walk(
    repository: &(impl Repository + ?Sized),
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
    scratch: (&mut [u8], &mut [u8]),
    mut budget: Option<&mut u64>,
    history: History,
) -> Result<()> {
    let (read_scratch, parse_scratch) = scratch;
    while let Some(index) = objects.iter().position(|slot| {
        let object = slot.expect("initialized metadata");
        object.reached && !object.checked
    }) {
        objects[index]
            .as_mut()
            .expect("initialized metadata")
            .checked = true;
        let mut body = Body::budgeted(repository, index, read_scratch, budget.as_deref_mut())?;
        let object = *metadata(objects, index);
        match object.info.kind {
            ObjectKind::Commit => {
                let stop = history.limit.is_some_and(|limit| object.depth >= limit)
                    || (history.client && object.client_shallow);
                validate_commit(
                    &mut body,
                    parse_scratch,
                    objects,
                    by_id,
                    (!stop).then_some(if history.limit.is_some() || history.distance {
                        object.depth.saturating_add(1)
                    } else {
                        1
                    }),
                )?;
                objects[index]
                    .as_mut()
                    .expect("initialized metadata")
                    .shallow = stop;
            }
            ObjectKind::Tree => validate_tree(&mut body, parse_scratch, objects, by_id)?,
            ObjectKind::Blob => {}
            ObjectKind::Tag => {
                validate_tag(&mut body, parse_scratch, objects, by_id, object.depth)?
            }
        }
    }
    Ok(())
}

fn validate_reference_target(
    repository: &(impl Repository + ?Sized),
    index: usize,
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
    read_scratch: &mut [u8],
    parse_scratch: &mut [u8],
) -> Result<()> {
    let id = repository.reference_id(index).ok_or(Error::InvalidObject)?;
    let position = find(objects, by_id, id).ok_or(Error::ObjectNotFound)?;
    let kind = metadata(objects, position).info.kind;
    let name = repository
        .reference_name(index)
        .ok_or(Error::InvalidReference)?;
    if name.starts_with("refs/heads/") && kind != ObjectKind::Commit {
        return Err(Error::InvalidObject);
    }
    reach(objects, by_id, id, kind)?;
    let expected = if kind == ObjectKind::Tag {
        Some(peel(
            repository,
            objects,
            by_id,
            id,
            read_scratch,
            parse_scratch,
        )?)
    } else {
        None
    };
    if repository.reference_peeled(index) != expected {
        return Err(Error::InvalidObject);
    }
    Ok(())
}

fn peel(
    repository: &(impl Repository + ?Sized),
    objects: &[Option<ValidationMetadata>],
    by_id: &[usize],
    mut id: ObjectId,
    read_scratch: &mut [u8],
    parse_scratch: &mut [u8],
) -> Result<ObjectId> {
    for _ in 0..objects.len() {
        let index = find(objects, by_id, id).ok_or(Error::ObjectNotFound)?;
        if metadata(objects, index).info.kind != ObjectKind::Tag {
            return Ok(id);
        }
        let (target, kind) = tag_target(
            &mut Body::new(repository, index, read_scratch)?,
            parse_scratch,
        )?;
        let target_index = find(objects, by_id, target).ok_or(Error::ObjectNotFound)?;
        if metadata(objects, target_index).info.kind != kind {
            return Err(Error::InvalidObject);
        }
        id = target;
    }
    Err(Error::InvalidObject)
}

fn metadata(objects: &[Option<ValidationMetadata>], index: usize) -> &ValidationMetadata {
    objects[index].as_ref().expect("initialized metadata")
}

fn find(objects: &[Option<ValidationMetadata>], by_id: &[usize], id: ObjectId) -> Option<usize> {
    let position = by_id
        .binary_search_by_key(&id, |&index| metadata(objects, index).id)
        .ok()?;
    Some(by_id[position])
}

fn reach(
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
    id: ObjectId,
    kind: ObjectKind,
) -> Result<()> {
    reach_at(objects, by_id, id, kind, 1)
}

fn reach_at(
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
    id: ObjectId,
    kind: ObjectKind,
    depth: u32,
) -> Result<()> {
    if id.is_zero() {
        return Err(Error::InvalidObject);
    }
    let index = find(objects, by_id, id).ok_or(Error::ObjectNotFound)?;
    let object = objects[index].as_mut().expect("initialized metadata");
    if object.info.kind != kind {
        return Err(Error::InvalidObject);
    }
    if !object.excluded {
        object.reached = true;
        if depth < object.depth {
            object.depth = depth;
            object.checked = false;
        }
    }
    Ok(())
}

/// Buffered streaming cursor. Seek discards buffered bytes; logical offsets
/// exclude unread bytes and remain 64-bit independently of the host usize.
struct Body<'scratch, 'repo, R: Repository + ?Sized> {
    reader: ObjectReader<'repo, R>,
    scratch: &'scratch mut [u8],
    position: usize,
    length: usize,
    budget: Option<&'scratch mut u64>,
}

impl<'scratch, 'repo, R: Repository + ?Sized> Body<'scratch, 'repo, R> {
    fn new(repository: &'repo R, index: usize, scratch: &'scratch mut [u8]) -> Result<Self> {
        Self::budgeted(repository, index, scratch, None)
    }
    fn budgeted(
        repository: &'repo R,
        index: usize,
        scratch: &'scratch mut [u8],
        budget: Option<&'scratch mut u64>,
    ) -> Result<Self> {
        Ok(Self {
            reader: ObjectReader::new(repository, index)?,
            scratch,
            position: 0,
            length: 0,
            budget,
        })
    }
    fn offset(&self) -> u64 {
        self.reader.position() - (self.length - self.position) as u64
    }
    fn seek(&mut self, offset: u64) -> Result<()> {
        self.reader.seek(offset)?;
        self.position = 0;
        self.length = 0;
        Ok(())
    }
    fn byte(&mut self) -> Result<Option<u8>> {
        if self.position == self.length {
            if self.budget.as_deref() == Some(&0) && self.reader.remaining() != 0 {
                return Err(Error::LimitExceeded);
            }
            let capacity = self
                .budget
                .as_deref()
                .map_or(self.scratch.len(), |remaining| {
                    self.scratch
                        .len()
                        .min(usize::try_from(*remaining).unwrap_or(usize::MAX))
                });
            self.length = self.reader.read(&mut self.scratch[..capacity])?;
            if let Some(remaining) = self.budget.as_deref_mut() {
                *remaining -= self.length as u64;
            }
            self.position = 0;
            if self.length == 0 {
                return Ok(None);
            }
        }
        let byte = self.scratch[self.position];
        self.position += 1;
        Ok(Some(byte))
    }
    fn until<'a>(&mut self, delimiter: u8, output: &'a mut [u8]) -> Result<&'a [u8]> {
        let mut length = 0;
        loop {
            let byte = self.byte()?.ok_or(Error::InvalidObject)?;
            if byte == delimiter {
                return Ok(&output[..length]);
            }
            *output.get_mut(length).ok_or(Error::BufferTooSmall)? = byte;
            length += 1;
        }
    }
    fn id(&mut self) -> Result<ObjectId> {
        let format = self.reader.object_format();
        let mut bytes = [0; 32];
        for byte in &mut bytes[..format.id_length()] {
            *byte = self.byte()?.ok_or(Error::InvalidObject)?;
        }
        let id = ObjectId::from_bytes(format, &bytes[..format.id_length()])?;
        if id.is_zero() {
            return Err(Error::InvalidObject);
        }
        Ok(id)
    }
    fn line<'a>(&mut self, scratch: &'a mut [u8]) -> Result<&'a [u8]> {
        let line = self.until(b'\n', scratch)?;
        if line.contains(&0) {
            return Err(Error::InvalidObject);
        }
        Ok(line)
    }
}

fn link(line: &[u8], prefix: &[u8], format: ObjectFormat) -> Result<ObjectId> {
    let id = ObjectId::from_hex(line.strip_prefix(prefix).ok_or(Error::InvalidObject)?)
        .map_err(|_| Error::InvalidObject)?;
    if id.format() != format {
        return Err(Error::InvalidObject);
    }
    Ok(id)
}

fn validate_commit(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &mut [u8],
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
    parent_depth: Option<u32>,
) -> Result<()> {
    let format = body.reader.object_format();
    reach(
        objects,
        by_id,
        link(body.line(scratch)?, b"tree ", format)?,
        ObjectKind::Tree,
    )?;
    let mut line = body.line(scratch)?;
    while line.starts_with(b"parent ") {
        let id = link(line, b"parent ", format)?;
        if let Some(depth) = parent_depth {
            reach_at(objects, by_id, id, ObjectKind::Commit, depth)?;
        }
        line = body.line(scratch)?;
    }
    if !line.starts_with(b"author ") || line.len() == b"author ".len() {
        return Err(Error::InvalidObject);
    }
    let line = body.line(scratch)?;
    if !line.starts_with(b"committer ") || line.len() == b"committer ".len() {
        return Err(Error::InvalidObject);
    }
    validate_extra_headers(
        body,
        scratch,
        &[b"tree", b"parent", b"author", b"committer"],
    )
}

fn tag_target(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &mut [u8],
) -> Result<(ObjectId, ObjectKind)> {
    let format = body.reader.object_format();
    let id = link(body.line(scratch)?, b"object ", format)?;
    if id.is_zero() {
        return Err(Error::InvalidObject);
    }
    let kind = match body.line(scratch)? {
        b"type commit" => ObjectKind::Commit,
        b"type tree" => ObjectKind::Tree,
        b"type blob" => ObjectKind::Blob,
        b"type tag" => ObjectKind::Tag,
        _ => return Err(Error::InvalidObject),
    };
    Ok((id, kind))
}

fn validate_tag(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &mut [u8],
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
    depth: u32,
) -> Result<()> {
    let (id, kind) = tag_target(body, scratch)?;
    reach_at(objects, by_id, id, kind, depth)?;
    let line = body.line(scratch)?;
    if !line.starts_with(b"tag ") || line.len() == b"tag ".len() {
        return Err(Error::InvalidObject);
    }
    validate_extra_headers(body, scratch, &[b"object", b"type", b"tag"])
}

fn validate_extra_headers(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &mut [u8],
    reserved: &[&[u8]],
) -> Result<()> {
    loop {
        let line = body.line(scratch)?;
        if line.is_empty() {
            return Ok(());
        }
        if line.starts_with(b" ") {
            continue;
        }
        let space = line
            .iter()
            .position(|&byte| byte == b' ')
            .ok_or(Error::InvalidObject)?;
        let name = &line[..space];
        if name.is_empty()
            || name.iter().any(|byte| byte.is_ascii_control())
            || reserved.contains(&name)
        {
            return Err(Error::InvalidObject);
        }
    }
}

fn tree_entry<'a>(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &'a mut [u8],
) -> Result<(ObjectKind, &'a [u8], ObjectId)> {
    let mut mode = [0; 6];
    let kind = match body.until(b' ', &mut mode).map_err(|error| {
        if error == Error::BufferTooSmall {
            Error::InvalidObject
        } else {
            error
        }
    })? {
        b"40000" => ObjectKind::Tree,
        b"100644" | b"100755" | b"120000" => ObjectKind::Blob,
        b"160000" => ObjectKind::Commit, // External gitlink, not a graph edge.
        _ => return Err(Error::InvalidObject),
    };
    let name = body.until(0, scratch)?;
    validate_name(name)?;
    Ok((kind, name, body.id()?))
}

fn check_duplicate_name(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &mut [u8],
    name: &[u8],
    before: u64,
) -> Result<()> {
    let resume = body.offset();
    body.seek(0)?;
    while body.offset() < before {
        let (_, previous, _) = tree_entry(body, scratch)?;
        if previous == name {
            return Err(Error::UnsortedFiles);
        }
    }
    body.seek(resume)
}

fn validate_tree(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &mut [u8],
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
) -> Result<()> {
    let capacity = scratch.len() / 2;
    let (previous, rest) = scratch.split_at_mut(capacity);
    let current = &mut rest[..capacity];
    let mut previous_length = 0;
    let mut previous_kind = ObjectKind::Blob;
    while body.offset() < body.reader.info().size {
        let start = body.offset();
        let (kind, name, id) = tree_entry(body, current)?;
        if start != 0 {
            let old = &previous[..previous_length];
            if old == name
                || cmp_tree_names(
                    old,
                    previous_kind == ObjectKind::Tree,
                    name,
                    kind == ObjectKind::Tree,
                ) != Ordering::Less
            {
                return Err(Error::UnsortedFiles);
            }
        }
        if kind != ObjectKind::Commit {
            reach(objects, by_id, id, kind)?;
        }
        // A file "foo", file "foo.bar", directory "foo" is sorted but
        // still duplicates a name. Reread earlier entries instead of keeping
        // an unbounded set of names or recursively visiting child trees.
        if kind == ObjectKind::Tree && start != 0 {
            check_duplicate_name(body, previous, name, start)?;
        }
        previous[..name.len()].copy_from_slice(name);
        previous_length = name.len();
        previous_kind = kind;
    }
    Ok(())
}
