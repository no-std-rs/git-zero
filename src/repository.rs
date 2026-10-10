use crate::{Object, ObjectId};

/// A stable snapshot, containing exactly the objects reachable from `head`.
///
/// `head` must refer to a commit. Objects must stay stable throughout a request
/// and across discovery/fetch requests for the same repository URL. Enumeration
/// must be complete and deterministic; each index below `object_count` must exist.
/// Implementations may borrow memory or load objects into caller-owned storage.
/// All advertised refs are aliases for this single commit. Ref names must be
/// valid, unique, byte-sorted, and include `head_ref`, a branch under `refs/heads/`.
pub trait Repository {
    fn head(&self) -> ObjectId;
    fn object_count(&self) -> usize;
    fn object(&self, index: usize) -> Option<Object<'_>>;

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

/// Validate a fully qualified ref name without normalizing it.
pub fn validate_ref_name(name: &str) -> crate::Result<()> {
    if !name.starts_with("refs/")
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
