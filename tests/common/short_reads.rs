use git_zero::{ObjectFormat, ObjectId, ObjectInfo, Repository, Result};

/// Suppress borrowed bodies and deliberately return short reads.
pub struct Streamed<'a> {
    pub source: &'a dyn Repository,
    pub limit: usize,
}
impl Repository for Streamed<'_> {
    fn object_format(&self) -> ObjectFormat {
        self.source.object_format()
    }
    fn head(&self) -> ObjectId {
        self.source.head()
    }
    fn object_count(&self) -> usize {
        self.source.object_count()
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.source.object_info(index)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.source.object_id(index)
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        let capacity = self.limit.min(storage.len());
        self.source
            .read_object(index, offset, &mut storage[..capacity])
    }
    fn head_ref(&self) -> &str {
        self.source.head_ref()
    }
    fn reference_count(&self) -> usize {
        self.source.reference_count()
    }
    fn reference_name(&self, index: usize) -> Option<&str> {
        self.source.reference_name(index)
    }
}
