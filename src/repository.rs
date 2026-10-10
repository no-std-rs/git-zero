use crate::{Object, ObjectId};

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
