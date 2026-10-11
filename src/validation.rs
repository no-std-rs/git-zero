//! Opt-in validation of the single-HEAD repository contract, without allocation.
use core::cmp::Ordering;

use crate::{
    Error, Object, ObjectHasher, ObjectId, ObjectInfo, ObjectKind, ObjectReader, Repository,
    Result,
    repository::{MAX_HEAD_REF_LENGTH, validate_ref_name},
};

/// Cached identity and size of a validated object, in enumeration order.
#[derive(Clone, Copy, Debug)]
pub struct ValidationMetadata {
    pub id: ObjectId,
    pub info: ObjectInfo,
    reached: bool,
    checked: bool,
}

/// Validate and index a stable repository, including streaming-only providers.
///
/// Checks SHA-1 IDs, unique enumeration, commit/tree links and target kinds,
/// tree modes, names, Git ordering and duplicates, and exact reachability from a
/// commit HEAD. Commit parents are followed; gitlinks name external repositories
/// and are not followed. Refs retain the single-commit alias contract.
///
/// This is graph validation, not a replacement for all of Git's fsck policies:
/// identity semantics, platform-specific filename aliases, and special files
/// such as .gitmodules are outside its scope. Annotated tags cannot be reachable
/// under the current ref contract and are rejected as extra objects.
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
    /// least one byte. Parse scratch must fit the longest commit header line
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
        for (index, slot) in objects.iter_mut().enumerate() {
            let info = repository.object_info(index)?;
            let id = repository.object_id(index).ok_or(Error::ObjectNotFound)?;
            if ObjectHasher::new(repository, index)?.finish(read_scratch)? != id
                || repository
                    .object(index)
                    .is_some_and(|object| object.info() != info || object.id() != id)
            {
                return Err(Error::InvalidObject);
            }
            *slot = Some(ValidationMetadata {
                id,
                info,
                reached: false,
                checked: false,
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
        let head = repository.head();
        reach(objects, by_id, head, ObjectKind::Commit)?;
        while let Some(index) = objects.iter().position(|slot| {
            let object = slot.expect("initialized metadata");
            object.reached && !object.checked
        }) {
            objects[index]
                .as_mut()
                .expect("initialized metadata")
                .checked = true;
            let kind = metadata(objects, index).info.kind;
            let mut body = Body::new(repository, index, read_scratch)?;
            match kind {
                ObjectKind::Commit => validate_commit(&mut body, parse_scratch, objects, by_id)?,
                ObjectKind::Tree => validate_tree(&mut body, parse_scratch, objects, by_id)?,
                ObjectKind::Blob => {}
                ObjectKind::Tag => return Err(Error::InvalidObject),
            }
        }
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
}

impl<R: Repository + ?Sized> Repository for ValidatedRepository<'_, '_, R> {
    fn head(&self) -> ObjectId {
        self.head
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
}

fn metadata(objects: &[Option<ValidationMetadata>], index: usize) -> ValidationMetadata {
    objects[index].expect("initialized metadata")
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
    if id.0 == [0; 20] {
        return Err(Error::InvalidObject);
    }
    let index = find(objects, by_id, id).ok_or(Error::ObjectNotFound)?;
    let object = objects[index].as_mut().expect("initialized metadata");
    if object.info.kind != kind {
        return Err(Error::InvalidObject);
    }
    object.reached = true;
    Ok(())
}

fn validate_refs(repository: &(impl Repository + ?Sized)) -> Result<()> {
    let head = repository.head_ref();
    validate_ref_name(head)?;
    if !head.starts_with("refs/heads/") || head.len() > MAX_HEAD_REF_LENGTH {
        return Err(Error::InvalidReference);
    }
    let mut found = false;
    for index in 0..repository.reference_count() {
        let name = repository
            .reference_name(index)
            .ok_or(Error::InvalidReference)?;
        validate_ref_name(name)?;
        found |= name == head;
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
    if !found {
        return Err(Error::InvalidReference);
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
}

impl<'scratch, 'repo, R: Repository + ?Sized> Body<'scratch, 'repo, R> {
    fn new(repository: &'repo R, index: usize, scratch: &'scratch mut [u8]) -> Result<Self> {
        Ok(Self {
            reader: ObjectReader::new(repository, index)?,
            scratch,
            position: 0,
            length: 0,
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
            self.length = self.reader.read(self.scratch)?;
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
        let mut id = [0; 20];
        for byte in &mut id {
            *byte = self.byte()?.ok_or(Error::InvalidObject)?;
        }
        if id == [0; 20] {
            return Err(Error::InvalidObject);
        }
        Ok(ObjectId(id))
    }
    fn line<'a>(&mut self, scratch: &'a mut [u8]) -> Result<&'a [u8]> {
        let line = self.until(b'\n', scratch)?;
        if line.contains(&0) {
            return Err(Error::InvalidObject);
        }
        Ok(line)
    }
}

fn link(line: &[u8], prefix: &[u8]) -> Result<ObjectId> {
    ObjectId::from_hex(line.strip_prefix(prefix).ok_or(Error::InvalidObject)?)
        .map_err(|_| Error::InvalidObject)
}

fn validate_commit(
    body: &mut Body<'_, '_, impl Repository + ?Sized>,
    scratch: &mut [u8],
    objects: &mut [Option<ValidationMetadata>],
    by_id: &[usize],
) -> Result<()> {
    reach(
        objects,
        by_id,
        link(body.line(scratch)?, b"tree ")?,
        ObjectKind::Tree,
    )?;
    let mut line = body.line(scratch)?;
    while line.starts_with(b"parent ") {
        reach(objects, by_id, link(line, b"parent ")?, ObjectKind::Commit)?;
        line = body.line(scratch)?;
    }
    if !line.starts_with(b"author ") || line.len() == b"author ".len() {
        return Err(Error::InvalidObject);
    }
    let line = body.line(scratch)?;
    if !line.starts_with(b"committer ") || line.len() == b"committer ".len() {
        return Err(Error::InvalidObject);
    }
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
            || matches!(name, b"tree" | b"parent" | b"author" | b"committer")
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
    if name.is_empty()
        || matches!(name, b"." | b"..")
        || name.eq_ignore_ascii_case(b".git")
        || name.contains(&b'/')
        || name.contains(&b'\\')
    {
        return Err(Error::InvalidFileName);
    }
    Ok((kind, name, body.id()?))
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
            let terminator = |kind| if kind == ObjectKind::Tree { b'/' } else { 0 };
            if old == name
                || old
                    .iter()
                    .copied()
                    .chain([terminator(previous_kind)])
                    .cmp(name.iter().copied().chain([terminator(kind)]))
                    != Ordering::Less
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
            let resume = body.offset();
            body.seek(0)?;
            while body.offset() < start {
                let (_, old, _) = tree_entry(body, previous)?;
                if old == name {
                    return Err(Error::UnsortedFiles);
                }
            }
            body.seek(resume)?;
        }
        previous[..name.len()].copy_from_slice(name);
        previous_length = name.len();
        previous_kind = kind;
    }
    Ok(())
}
