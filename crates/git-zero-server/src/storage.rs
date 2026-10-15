//! Caller-owned fetch storage, with a borrowed view for each request.
use git_zero_core::{ObjectId, ValidationMetadata};

/// Independent per-request storage for bounded reachable-object selection.
/// `wants` bounds distinct wanted IDs. `objects` and `indices` each need the
/// repository's complete object count; their extra slots remain untouched.
/// Read/parse scratch budgets match `ValidatedRepository::select_objects`.
pub struct SelectionStorage<'a> {
    pub wants: &'a mut [ObjectId],
    pub objects: &'a mut [Option<ValidationMetadata>],
    pub indices: &'a mut [usize],
    pub read: &'a mut [u8],
    pub parse: &'a mut [u8],
    /// Maximum structural bytes read while traversing a request's graph.
    pub graph_read_limit: u64,
}

impl SelectionStorage<'_> {
    /// Borrow the same arrays for one request without moving the host descriptor.
    pub fn reborrow(&mut self) -> SelectionStorage<'_> {
        SelectionStorage {
            wants: self.wants,
            objects: self.objects,
            indices: self.indices,
            read: self.read,
            parse: self.parse,
            graph_read_limit: self.graph_read_limit,
        }
    }
}

/// Fixed-capacity backing storage for one independent selected-fetch session.
/// All capacities are explicit const parameters; construction allocates nothing.
/// Keep separate values for overlapping responses. Only the borrowed descriptor
/// reaches the protocol encoder, so applications may also keep their own arrays
/// and construct `SelectionStorage` directly.
pub struct SelectionBuffers<
    const WANTS: usize,
    const OBJECTS: usize,
    const READ: usize,
    const PARSE: usize,
> {
    wants: [ObjectId; WANTS],
    objects: [Option<ValidationMetadata>; OBJECTS],
    indices: [usize; OBJECTS],
    read: [u8; READ],
    parse: [u8; PARSE],
}

impl<const WANTS: usize, const OBJECTS: usize, const READ: usize, const PARSE: usize>
    SelectionBuffers<WANTS, OBJECTS, READ, PARSE>
{
    pub const fn new() -> Self {
        Self {
            wants: [ObjectId::Sha1([0; 20]); WANTS],
            objects: [None; OBJECTS],
            indices: [0; OBJECTS],
            read: [0; READ],
            parse: [0; PARSE],
        }
    }

    /// Borrow a fresh request descriptor. Traversal resets the work arrays;
    /// no clearing or reallocation is needed between successful/failed requests.
    pub fn storage(&mut self, graph_read_limit: u64) -> SelectionStorage<'_> {
        SelectionStorage {
            wants: &mut self.wants,
            objects: &mut self.objects,
            indices: &mut self.indices,
            read: &mut self.read,
            parse: &mut self.parse,
            graph_read_limit,
        }
    }

    pub const fn want_capacity(&self) -> usize {
        WANTS
    }
    pub const fn object_capacity(&self) -> usize {
        OBJECTS
    }
    pub const fn read_capacity(&self) -> usize {
        READ
    }
    pub const fn parse_capacity(&self) -> usize {
        PARSE
    }
    /// Bytes owned by this value, including metadata and alignment padding.
    pub const fn storage_bytes(&self) -> usize {
        core::mem::size_of::<Self>()
    }
}

impl<const WANTS: usize, const OBJECTS: usize, const READ: usize, const PARSE: usize> Default
    for SelectionBuffers<WANTS, OBJECTS, READ, PARSE>
{
    fn default() -> Self {
        Self::new()
    }
}
