//! Optional requested-closure path for the existing paired HTTP workloads.
use git_zero::{
    ObjectId, Repository, ValidatedRepository, ValidationMetadata,
    http::{self, Request, Response, SelectionStorage},
    io::Write,
};
use std::{fmt, mem::size_of};

pub const GRAPH_READ_LIMIT: u64 = 64 * 1024 * 1024;

pub struct Selection {
    wants: Vec<ObjectId>,
    objects: Vec<Option<ValidationMetadata>>,
    indices: Vec<usize>,
    parse: Vec<u8>,
}

impl Selection {
    pub fn new(objects: usize) -> Self {
        Self {
            wants: vec![ObjectId::Sha1([0; 20]); 64],
            objects: vec![None; objects],
            indices: vec![0; objects],
            parse: vec![0; 4096],
        }
    }

    pub fn serve<R: Repository + ?Sized>(
        &mut self,
        repository: &ValidatedRepository<'_, '_, R>,
        request: Request<'_>,
        read: &mut [u8],
        output: &mut impl Write,
    ) -> git_zero::Result<Response> {
        http::serve_with_selection(
            repository,
            request,
            &mut SelectionStorage {
                wants: &mut self.wants,
                objects: &mut self.objects,
                indices: &mut self.indices,
                read,
                parse: &mut self.parse,
                graph_read_limit: GRAPH_READ_LIMIT,
            },
            output,
        )
    }

    pub fn budget(&self) -> Budget {
        Budget {
            wants: self.wants.capacity() * size_of::<ObjectId>(),
            objects: self.objects.capacity() * size_of::<Option<ValidationMetadata>>(),
            indices: self.indices.capacity() * size_of::<usize>(),
            parse: self.parse.capacity(),
            graph_read_limit: GRAPH_READ_LIMIT,
        }
    }
}

/// Reserved heap bytes, excluding the read scratch shared with ordinary serving.
#[derive(Default)]
pub struct Budget {
    wants: usize,
    objects: usize,
    indices: usize,
    parse: usize,
    graph_read_limit: u64,
}

impl fmt::Display for Budget {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            output,
            "{{\"want_bytes\":{},\"object_metadata_bytes\":{},\"index_bytes\":{},\"parse_bytes\":{},\"graph_read_limit\":{}}}",
            self.wants, self.objects, self.indices, self.parse, self.graph_read_limit
        )
    }
}
