use crate::{Object, ObjectId};

/// A stable snapshot, containing exactly the objects reachable from `head`.
///
/// `head` must refer to a commit. Objects must stay stable throughout a request
/// and across discovery/fetch requests for the same repository URL. Enumeration
/// must be complete and deterministic; each index below `object_count` must exist.
/// Implementations may borrow memory or load objects into caller-owned storage.
pub trait Repository {
    fn head(&self) -> ObjectId;
    fn object_count(&self) -> usize;
    fn object(&self, index: usize) -> Option<Object<'_>>;

    fn find(&self, id: ObjectId) -> Option<Object<'_>> {
        (0..self.object_count())
            .find_map(|index| self.object(index).filter(|object| object.id() == id))
    }
}
