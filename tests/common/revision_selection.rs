//! Core history selection, allocation and shared-budget contracts.
use super::*;

fn sorted(mut values: Vec<ObjectId>) -> Vec<ObjectId> {
    values.sort_unstable();
    values
}

struct Selection {
    wants: Vec<usize>,
    common: Vec<usize>,
    shallow: Vec<usize>,
    excluded: Vec<usize>,
    since: Option<u64>,
    filter: BlobFilter,
    tags: bool,
}
impl Selection {
    fn since(want: usize, date: u64) -> Self {
        Self {
            wants: vec![want],
            common: vec![],
            shallow: vec![],
            excluded: vec![],
            since: Some(date),
            filter: BlobFilter::All,
            tags: false,
        }
    }
}

fn run_selection<'a>(
    fixture: &Fixture,
    repository: &ValidatedRepository<'_, '_, Fixture>,
    selection: &Selection,
    arrays: (&mut [Option<git_zero::ValidationMetadata>], &'a mut [usize]),
    scratch: (&mut [u8], &mut [u8], &mut RevisionState<'_>),
    bytes: u64,
) -> git_zero::Result<&'a [usize]> {
    let mut roots = [ObjectId::Sha1([0; 20]); N];
    for (slot, &index) in roots.iter_mut().zip(&selection.wants) {
        *slot = fixture.ids[index];
    }
    repository.select_revision_fetch(
        &roots[..selection.wants.len()],
        (
            selection.common.iter().map(|&i| Ok(fixture.ids[i])),
            selection.shallow.iter().map(|&i| Ok(fixture.ids[i])),
            selection.excluded.iter().map(|&i| Ok(fixture.ids[i])),
        ),
        arrays.0,
        arrays.1,
        scratch,
        (
            SelectionOptions {
                graph_read_limit: bytes,
                include_tags: selection.tags,
            },
            FetchOptions {
                filter: selection.filter,
                ..Default::default()
            },
            RevisionOptions {
                since: selection.since,
            },
        ),
    )
}

fn select(fixture: &Fixture, selection: &Selection) -> (Vec<ObjectId>, Vec<ShallowChange>, u64) {
    let mut metadata = [None; N];
    let mut by_id = [0; N];
    let repository = validate(fixture, &mut metadata, &mut by_id);
    let mut work = [None; N];
    let mut selected = [0; N];
    let mut state = [0; N];
    let mut revision = RevisionState::new(&mut state, limits());
    fixture.bytes.set(0);
    let indices = allocations::without_allocations(|| {
        run_selection(
            fixture,
            &repository,
            selection,
            (&mut work, &mut selected),
            (&mut [0; 1], &mut [0; 256], &mut revision),
            1_000_000,
        )
    })
    .unwrap();
    assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
    let ids = sorted(
        indices
            .iter()
            .map(|&index| repository.object_id(index).unwrap())
            .collect(),
    );
    let changes = revision.changes(&work).collect();
    (ids, changes, fixture.bytes.get())
}

fn check(
    fixture: &Fixture,
    selection: &Selection,
    objects: &[usize],
    boundaries: &[usize],
    unshallow: &[usize],
) {
    let (actual, mut changes, _) = select(fixture, selection);
    assert_eq!(
        actual,
        sorted(objects.iter().map(|&index| fixture.ids[index]).collect())
    );
    let mut expected_changes: Vec<_> = boundaries
        .iter()
        .map(|&index| ShallowChange::Shallow(fixture.ids[index]))
        .chain(
            unshallow
                .iter()
                .map(|&index| ShallowChange::Unshallow(fixture.ids[index])),
        )
        .collect();
    let key = |change: &ShallowChange| match *change {
        ShallowChange::Shallow(id) => (false, id),
        ShallowChange::Unshallow(id) => (true, id),
    };
    changes.sort_unstable_by_key(key);
    expected_changes.sort_unstable_by_key(key);
    assert_eq!(changes, expected_changes);
}

fn check_history_cuts(fixture: &Fixture) {
    let dates: &[(usize, u64, &[usize], &[usize])] = &[
        (D, 1700000100, &[A, B, C, D, TREE, BLOB], &[]),
        (D, 1700000200, &[B, C, D, TREE, BLOB], &[B]),
        (D, 1700000250, &[D, TREE, BLOB], &[D]),
        (M, 1700000250, &[M, D, E, B, TREE, BLOB], &[D, B]),
        (M, 1700000400, &[M, TREE, BLOB], &[M, D]),
    ];
    for &(want, date, objects, boundaries) in dates {
        check(
            fixture,
            &Selection::since(want, date),
            objects,
            boundaries,
            &[],
        );
    }
    let mut explicit = Selection::since(D, 1700000400);
    explicit.wants.push(B);
    check(fixture, &explicit, &[D, B, A, TREE, BLOB], &[D], &[]);
    explicit = Selection::since(D, 0);
    explicit.excluded.push(B);
    explicit.wants.push(B);
    check(fixture, &explicit, &[D, C, B, A, TREE, BLOB], &[C], &[]);
    let exclusions: &[(&[usize], &[usize], &[usize])] = &[
        (&[B], &[D, C, TREE, BLOB], &[C]),
        (&[B, B], &[D, C, TREE, BLOB], &[C]),
        (&[TAG], &[D, TREE, BLOB], &[D]),
        (&[B, TAG], &[D, TREE, BLOB], &[D]),
        (&[BLOB], &[D, C, B, A, TREE, BLOB], &[]),
        (&[BLOB_TAG], &[D, C, B, A, TREE, BLOB], &[]),
    ];
    for &(excluded, objects, boundaries) in exclusions {
        let mut selection = Selection::since(D, 0);
        selection.excluded = excluded.to_vec();
        check(fixture, &selection, objects, boundaries, &[]);
    }
    let mut selection = Selection::since(M, 0);
    selection.excluded = vec![D];
    check(fixture, &selection, &[M, TREE, BLOB], &[M, E], &[]);
}

#[test]
fn dates_skew_roots_merges_and_exclusions_match_independent_git_oracles() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for reverse in [false, true] {
            let mut fixture = Fixture::new(format);
            if reverse {
                fixture.objects.reverse();
            }
            check_history_cuts(&fixture);
        }
    }
}

fn check_known_history(fixture: &Fixture) {
    let mut selection = Selection::since(D, 1700000200);
    selection.shallow = vec![D];
    let common_cases: &[(&[usize], &[usize])] = &[
        (&[], &[B, C, TREE, BLOB]),
        (&[D], &[B, C]),
        (&[C], &[]),
        (&[B], &[C]),
        (&[A], &[B, C, TREE, BLOB]),
    ];
    for &(common, objects) in common_cases {
        selection.common = common.to_vec();
        check(fixture, &selection, objects, &[B], &[D]);
    }
    selection.common = vec![M, D];
    selection.shallow = vec![M, D];
    check(fixture, &selection, &[B, C], &[B], &[D]);
    selection = Selection::since(D, 1700000100);
    selection.shallow = vec![C];
    check(fixture, &selection, &[A, B, D, TREE, BLOB], &[], &[C]);
    selection = Selection::since(D, 1700000350);
    selection.shallow = vec![C];
    check(fixture, &selection, &[D, TREE, BLOB], &[D], &[]);
    selection = Selection::since(A, 0);
    selection.shallow = vec![A];
    check(fixture, &selection, &[], &[], &[A]);
}

#[test]
fn unshallow_seeds_missing_parents_and_safe_haves_keep_known_closures() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        check_known_history(&fixture);
    }
}

#[test]
fn explicit_noncommit_wants_and_blob_filters_keep_their_existing_contract() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut selection = Selection::since(TAG, 1700000200);
    check(&fixture, &selection, &[TAG, C, B, TREE, BLOB], &[B], &[]);
    selection = Selection::since(D, 1700000200);
    selection.filter = BlobFilter::None;
    check(&fixture, &selection, &[D, C, B, TREE], &[B], &[]);
    selection.wants.push(BLOB);
    check(&fixture, &selection, &[D, C, B, TREE, BLOB], &[B], &[]);
}

#[derive(Clone, Copy, Debug)]
struct Capacity {
    bytes: u64,
    states: usize,
    objects: usize,
    indices: usize,
    read: usize,
    parse: usize,
}

fn check_selection_storage(format: ObjectFormat) {
    let fixture = Fixture::new(format);
    let selection = Selection::since(D, 1700000200);
    let (_, _, bytes) = select(&fixture, &selection);
    let mut metadata = [None; N];
    let mut by_id = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut by_id);
    let count = fixture.object_count();
    let mut objects = [None; N];
    let mut indices = [usize::MAX; N];
    let mut states = [0xa5; N];
    let mut read = [0; 1];
    let mut parse = [0; 256];
    let full = Capacity {
        bytes,
        states: count,
        objects: count,
        indices: count,
        read: 1,
        parse: 256,
    };
    for (capacity, expected) in [
        (full, Ok(())),
        (
            Capacity {
                bytes: bytes - 1,
                ..full
            },
            Err(Error::LimitExceeded),
        ),
        (
            Capacity {
                states: count - 1,
                ..full
            },
            Err(Error::BufferTooSmall),
        ),
        (
            Capacity {
                objects: count - 1,
                ..full
            },
            Err(Error::BufferTooSmall),
        ),
        (
            Capacity {
                indices: count - 1,
                ..full
            },
            Err(Error::BufferTooSmall),
        ),
        (Capacity { read: 0, ..full }, Err(Error::BufferTooSmall)),
        (Capacity { parse: 0, ..full }, Err(Error::BufferTooSmall)),
        (full, Ok(())),
    ] {
        let mut revision = RevisionState::new(&mut states[..capacity.states], limits());
        let result = allocations::without_allocations(|| {
            run_selection(
                &fixture,
                &repository,
                &selection,
                (
                    &mut objects[..capacity.objects],
                    &mut indices[..capacity.indices],
                ),
                (
                    &mut read[..capacity.read],
                    &mut parse[..capacity.parse],
                    &mut revision,
                ),
                capacity.bytes,
            )
        });
        assert_eq!(result.map(|_| ()), expected, "{capacity:?}");
        if expected.is_err() {
            assert_eq!(revision.changes(&objects).count(), 0);
        }
        assert!(states[count..].iter().all(|&byte| byte == 0xa5));
        assert!(objects[count..].iter().all(Option::is_none));
        assert!(indices[count..].iter().all(|&index| index == usize::MAX));
    }
    fixture.failed.set(true);
    let mut revision = RevisionState::new(&mut states, limits());
    assert_eq!(
        run_selection(
            &fixture,
            &repository,
            &selection,
            (&mut objects, &mut indices),
            (&mut read, &mut parse, &mut revision),
            bytes
        )
        .map(|_| ()),
        Err(Error::Backend)
    );
    assert_eq!(revision.changes(&objects).count(), 0);
    fixture.failed.set(false);
    check(&fixture, &selection, &[B, C, D, TREE, BLOB], &[B], &[]);
}

#[test]
fn shared_byte_budget_capacity_failures_and_fresh_retries_are_bounded() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        check_selection_storage(format);
    }
}

#[test]
fn matching_tags_use_selected_targets_after_blob_filtering() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let mut selection = Selection::since(D, 1700000200);
        selection.filter = BlobFilter::None;
        selection.tags = true;
        let (selected, _, _) = select(&fixture, &selection);
        assert!(selected.contains(&fixture.ids[TAG]));
        assert!(!selected.contains(&fixture.ids[BLOB_TAG]));
    }
}

#[test]
fn exclusions_dates_common_history_unshallow_and_tags_share_one_byte_budget() {
    let fixture = Fixture::new(ObjectFormat::Sha256);
    let mut selection = Selection::since(D, 1700000200);
    selection.excluded = vec![B];
    selection.shallow = vec![D];
    selection.common = vec![D];
    selection.tags = true;
    selection.filter = BlobFilter::None;
    let (_, _, bytes) = select(&fixture, &selection);
    let mut metadata = [None; N];
    let mut by_id = [0; N];
    let repository = validate(&fixture, &mut metadata, &mut by_id);
    for (limit, expected) in [(bytes, Ok(())), (bytes - 1, Err(Error::LimitExceeded))] {
        let mut objects = [None; N];
        let mut indices = [0; N];
        let mut states = [0; N];
        let mut revision = RevisionState::new(&mut states, limits());
        fixture.bytes.set(0);
        let result = allocations::without_allocations(|| {
            run_selection(
                &fixture,
                &repository,
                &selection,
                (&mut objects, &mut indices),
                (&mut [0; 1], &mut [0; 256], &mut revision),
                limit,
            )
        });
        assert_eq!(result.map(|_| ()), expected);
        assert!(fixture.bytes.get() <= limit);
    }
    assert_eq!(
        RevisionBuffers::<64, 64, 512, 512>::new().storage_bytes(),
        http::SelectionBuffers::<64, 64, 512, 512>::new().storage_bytes() + 64
    );
}
