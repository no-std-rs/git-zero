#[path = "common/allocations.rs"]
mod allocations;
#[path = "common/revision_fixture.rs"]
mod fixture;
use fixture::*;
use git_zero::{
    BlobFilter, Error, FetchOptions, ObjectFormat, ObjectId, Repository, RevisionLimits,
    RevisionOptions, RevisionState, SelectionOptions, ShallowChange, ValidatedRepository,
    http::{self, Request, RevisionBuffers},
    io::SliceWriter,
};

fn validate<'a>(
    fixture: &'a Fixture,
    metadata: &'a mut [Option<git_zero::ValidationMetadata>],
    index: &'a mut [usize],
) -> ValidatedRepository<'a, 'a, Fixture> {
    ValidatedRepository::new(fixture, metadata, index, &mut [0; 1], &mut [0; 256]).unwrap()
}

fn limits() -> RevisionLimits {
    RevisionLimits { max_exclusions: 8 }
}

#[path = "common/pull.rs"]
mod pull;

#[path = "common/revision_selection.rs"]
mod selection;
#[path = "common/revision_http.rs"]
mod wire;
