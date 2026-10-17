//! Keep canonical file fixtures and validation storage scoped to one test phase.
use super::fixture::{self, Fixture, core_result};
use git_zero::{RefRepository, ValidatedRepository};
use git_zero_rustix::FileRepository;
use std::path::Path;

/// Keep file descriptors, borrowed refs and validation storage alive together.
pub fn with_fixture<T>(
    root: &Path,
    use_repository: impl FnOnce(
        &ValidatedRepository<'_, '_, RefRepository<'_, '_, FileRepository<'_>>>,
    ) -> fixture::Result<T>,
) -> fixture::Result<T> {
    let fixture = Fixture::load(root)?;
    let (id, head) = fixture.head()?;
    let references = fixture.references()?;
    let source = FileRepository::new(id, fixture.objects());
    let source = core_result(RefRepository::new(&source, head, &references), "references")?;
    let (mut metadata, mut index) = ([None; 256], [0; 256]);
    let repository = core_result(
        ValidatedRepository::new(
            &source,
            &mut metadata,
            &mut index,
            &mut [0; 512],
            &mut [0; 512],
        ),
        "validation",
    )?;
    use_repository(&repository)
}
